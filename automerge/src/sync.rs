use itertools::Itertools;
use std::collections::{HashMap, HashSet};

#[cfg(not(feature = "storage-v2"))]
use std::{borrow::Cow, io, io::Write};

#[cfg(feature = "storage-v2")]
use crate::storage::{parse, Change as StoredChange, ReadChangeOpError};
#[cfg(not(feature = "storage-v2"))]
use crate::{decoding, decoding::Decoder, encoding::Encodable};
use crate::{ApplyOptions, Automerge, AutomergeError, Change, ChangeHash, OpObserver};

mod bloom;
mod state;

pub use bloom::BloomFilter;
pub use state::{Have, State};

#[cfg(not(feature = "storage-v2"))]
const HASH_SIZE: usize = 32; // 256 bits = 32 bytes
const MESSAGE_TYPE_SYNC: u8 = 0x42; // first byte of a sync message, for identification

impl Automerge {
    pub fn generate_sync_message(&self, sync_state: &mut State) -> Option<Message> {
        let our_heads = self.get_heads();

        let our_need = self.get_missing_deps(sync_state.their_heads.as_ref().unwrap_or(&vec![]));

        let their_heads_set = if let Some(ref heads) = sync_state.their_heads {
            heads.iter().collect::<HashSet<_>>()
        } else {
            HashSet::new()
        };
        let our_have = if our_need.iter().all(|hash| their_heads_set.contains(hash)) {
            vec![self.make_bloom_filter(sync_state.shared_heads.clone())]
        } else {
            Vec::new()
        };

        if let Some(ref their_have) = sync_state.their_have {
            if let Some(first_have) = their_have.first().as_ref() {
                if !first_have
                    .last_sync
                    .iter()
                    .all(|hash| self.get_change_by_hash(hash).is_some())
                {
                    let reset_msg = Message {
                        heads: our_heads,
                        need: Vec::new(),
                        have: vec![Have::default()],
                        changes: Vec::new(),
                    };
                    return Some(reset_msg);
                }
            }
        }

        let mut changes_to_send = if let (Some(their_have), Some(their_need)) = (
            sync_state.their_have.as_ref(),
            sync_state.their_need.as_ref(),
        ) {
            self.get_changes_to_send(their_have.clone(), their_need)
        } else {
            Vec::new()
        };

        let heads_unchanged = sync_state.last_sent_heads == our_heads;

        let heads_equal = if let Some(their_heads) = sync_state.their_heads.as_ref() {
            their_heads == &our_heads
        } else {
            false
        };

        if heads_unchanged && heads_equal && changes_to_send.is_empty() {
            return None;
        }

        // deduplicate the changes to send with those we have already sent
        changes_to_send.retain(|change| !sync_state.sent_hashes.contains(&change.hash()));

        sync_state.last_sent_heads = our_heads.clone();
        sync_state
            .sent_hashes
            .extend(changes_to_send.iter().map(|c| c.hash()));

        let sync_message = Message {
            heads: our_heads,
            have: our_have,
            need: our_need,
            changes: changes_to_send.into_iter().cloned().collect(),
        };

        Some(sync_message)
    }

    pub fn receive_sync_message(
        &mut self,
        sync_state: &mut State,
        message: Message,
    ) -> Result<(), AutomergeError> {
        self.receive_sync_message_with::<()>(sync_state, message, ApplyOptions::default())
    }

    pub fn receive_sync_message_with<'a, Obs: OpObserver>(
        &mut self,
        sync_state: &mut State,
        message: Message,
        options: ApplyOptions<'a, Obs>,
    ) -> Result<(), AutomergeError> {
        let before_heads = self.get_heads();

        let Message {
            heads: message_heads,
            changes: message_changes,
            need: message_need,
            have: message_have,
        } = message;

        let changes_is_empty = message_changes.is_empty();
        if !changes_is_empty {
            self.apply_changes_with(message_changes, options)?;
            sync_state.shared_heads = advance_heads(
                &before_heads.iter().collect(),
                &self.get_heads().into_iter().collect(),
                &sync_state.shared_heads,
            );
        }

        // trim down the sent hashes to those that we know they haven't seen
        self.filter_changes(&message_heads, &mut sync_state.sent_hashes);

        if changes_is_empty && message_heads == before_heads {
            sync_state.last_sent_heads = message_heads.clone();
        }

        let known_heads = message_heads
            .iter()
            .filter(|head| self.get_change_by_hash(head).is_some())
            .collect::<Vec<_>>();
        if known_heads.len() == message_heads.len() {
            sync_state.shared_heads = message_heads.clone();
            // If the remote peer has lost all its data, reset our state to perform a full resync
            if message_heads.is_empty() {
                sync_state.last_sent_heads = Default::default();
                sync_state.sent_hashes = Default::default();
            }
        } else {
            sync_state.shared_heads = sync_state
                .shared_heads
                .iter()
                .chain(known_heads)
                .copied()
                .unique()
                .sorted()
                .collect::<Vec<_>>();
        }

        sync_state.their_have = Some(message_have);
        sync_state.their_heads = Some(message_heads);
        sync_state.their_need = Some(message_need);

        Ok(())
    }

    fn make_bloom_filter(&self, last_sync: Vec<ChangeHash>) -> Have {
        let new_changes = self.get_changes(&last_sync);
        let hashes = new_changes
            .into_iter()
            .map(|change| change.hash())
            .collect::<Vec<_>>();
        Have {
            last_sync,
            bloom: BloomFilter::from(&hashes[..]),
        }
    }

    fn get_changes_to_send(&self, have: Vec<Have>, need: &[ChangeHash]) -> Vec<&Change> {
        if have.is_empty() {
            need.iter()
                .filter_map(|hash| self.get_change_by_hash(hash))
                .collect()
        } else {
            let mut last_sync_hashes = HashSet::new();
            let mut bloom_filters = Vec::with_capacity(have.len());

            for h in have {
                let Have { last_sync, bloom } = h;
                for hash in last_sync {
                    last_sync_hashes.insert(hash);
                }
                bloom_filters.push(bloom);
            }
            let last_sync_hashes = last_sync_hashes.into_iter().collect::<Vec<_>>();

            let changes = self.get_changes(&last_sync_hashes);

            let mut change_hashes = HashSet::with_capacity(changes.len());
            let mut dependents: HashMap<ChangeHash, Vec<ChangeHash>> = HashMap::new();
            let mut hashes_to_send = HashSet::new();

            for change in &changes {
                change_hashes.insert(change.hash());

                for dep in change.deps() {
                    dependents.entry(*dep).or_default().push(change.hash());
                }

                if bloom_filters
                    .iter()
                    .all(|bloom| !bloom.contains_hash(&change.hash()))
                {
                    hashes_to_send.insert(change.hash());
                }
            }

            let mut stack = hashes_to_send.iter().copied().collect::<Vec<_>>();
            while let Some(hash) = stack.pop() {
                if let Some(deps) = dependents.get(&hash) {
                    for dep in deps {
                        if hashes_to_send.insert(*dep) {
                            stack.push(*dep);
                        }
                    }
                }
            }

            let mut changes_to_send = Vec::new();
            for hash in need {
                hashes_to_send.insert(*hash);
                if !change_hashes.contains(hash) {
                    let change = self.get_change_by_hash(hash);
                    if let Some(change) = change {
                        changes_to_send.push(change);
                    }
                }
            }

            for change in changes {
                if hashes_to_send.contains(&change.hash()) {
                    changes_to_send.push(change);
                }
            }
            changes_to_send
        }
    }
}

#[cfg(feature = "storage-v2")]
#[derive(Debug, thiserror::Error)]
pub enum ReadMessageError {
    #[error("expected {expected_one_of:?} but found {found}")]
    WrongType { expected_one_of: Vec<u8>, found: u8 },
    #[error("{0}")]
    Parse(String),
    #[error(transparent)]
    ReadChangeOps(#[from] ReadChangeOpError),
    #[error("not enough input")]
    NotEnoughInput,
}

#[cfg(feature = "storage-v2")]
impl From<ReadMessageError> for parse::ParseError<ReadMessageError> {
    fn from(e: ReadMessageError) -> Self {
        parse::ParseError::Error(e)
    }
}

#[cfg(feature = "storage-v2")]
impl From<parse::ErrorKind> for ReadMessageError {
    fn from(k: parse::ErrorKind) -> Self {
        ReadMessageError::Parse(k.to_string())
    }
}

#[cfg(feature = "storage-v2")]
impl From<parse::ParseError<ReadMessageError>> for ReadMessageError {
    fn from(p: parse::ParseError<ReadMessageError>) -> Self {
        match p {
            parse::ParseError::Error(e) => e,
            parse::ParseError::Incomplete(..) => Self::NotEnoughInput,
        }
    }
}

/// The sync message to be sent.
#[derive(Debug, Clone)]
pub struct Message {
    /// The heads of the sender.
    pub heads: Vec<ChangeHash>,
    /// The hashes of any changes that are being explicitly requested from the recipient.
    pub need: Vec<ChangeHash>,
    /// A summary of the changes that the sender already has.
    pub have: Vec<Have>,
    /// The changes for the recipient to apply.
    pub changes: Vec<Change>,
}

#[cfg(feature = "storage-v2")]
fn parse_have(input: &[u8]) -> parse::ParseResult<'_, Have, ReadMessageError> {
    let (i, last_sync) = parse::length_prefixed(parse::leb128_u64, parse::change_hash)(input)?;
    let (i, bloom_bytes) = parse::length_prefixed_bytes(i)?;
    let (_, bloom) = BloomFilter::parse(bloom_bytes).map_err(parse::lift_errorkind)?;
    Ok((i, Have { last_sync, bloom }))
}

impl Message {
    #[cfg(feature = "storage-v2")]
    pub fn decode(input: &[u8]) -> Result<Self, ReadMessageError> {
        match Self::parse(input) {
            Ok((_, msg)) => Ok(msg),
            Err(parse::ParseError::Error(e)) => Err(e),
            Err(parse::ParseError::Incomplete(_)) => Err(ReadMessageError::NotEnoughInput),
        }
    }

    #[cfg(feature = "storage-v2")]
    pub(crate) fn parse(input: &[u8]) -> parse::ParseResult<'_, Self, ReadMessageError> {
        let (i, message_type) = parse::take1(input)?;
        if message_type != MESSAGE_TYPE_SYNC {
            return Err(parse::ParseError::Error(ReadMessageError::WrongType {
                expected_one_of: vec![MESSAGE_TYPE_SYNC],
                found: message_type,
            }));
        }

        let (i, heads) = parse::length_prefixed(parse::leb128_u64, parse::change_hash)(i)?;
        let (i, need) = parse::length_prefixed(parse::leb128_u64, parse::change_hash)(i)?;
        let (i, have) = parse::length_prefixed(parse::leb128_u64, parse_have)(i)?;

        let change_parser = |i| {
            let (i, bytes) = parse::length_prefixed_bytes(i)?;
            let (_, change) = StoredChange::parse(bytes).map_err(parse::lift_errorkind)?;
            Ok((i, change))
        };
        let (i, stored_changes) = parse::length_prefixed(parse::leb128_u64, change_parser)(i)?;
        let changes_len = stored_changes.len();
        let changes: Vec<Change> = stored_changes
            .into_iter()
            .try_fold::<_, _, Result<_, ReadMessageError>>(
                Vec::with_capacity(changes_len),
                |mut acc, stored| {
                    let change = Change::new_from_unverified(stored.into_owned(), None)
                        .map_err(ReadMessageError::ReadChangeOps)?;
                    acc.push(change);
                    Ok(acc)
                },
            )?;

        Ok((
            i,
            Message {
                heads,
                need,
                have,
                changes,
            },
        ))
    }

    #[cfg(feature = "storage-v2")]
    pub fn encode(mut self) -> Vec<u8> {
        let mut buf = vec![MESSAGE_TYPE_SYNC];

        encode_hashes(&mut buf, &self.heads);
        encode_hashes(&mut buf, &self.need);
        encode_many(&mut buf, self.have.iter(), |buf, h| {
            encode_hashes(buf, &h.last_sync);
            leb128::write::unsigned(buf, h.bloom.to_bytes().len() as u64).unwrap();
            buf.extend(h.bloom.to_bytes());
        });

        encode_many(&mut buf, self.changes.iter_mut(), |buf, change| {
            leb128::write::unsigned(buf, change.raw_bytes().len() as u64).unwrap();
            buf.extend(change.compressed_bytes().as_ref())
        });

        buf
    }

    #[cfg(not(feature = "storage-v2"))]
    pub fn encode(self) -> Vec<u8> {
        let mut buf = vec![MESSAGE_TYPE_SYNC];

        encode_hashes(&mut buf, &self.heads);
        encode_hashes(&mut buf, &self.need);
        (self.have.len() as u32).encode_vec(&mut buf);
        for have in self.have {
            encode_hashes(&mut buf, &have.last_sync);
            have.bloom.to_bytes().encode_vec(&mut buf);
        }

        (self.changes.len() as u32).encode_vec(&mut buf);
        for mut change in self.changes {
            change.compress();
            change.compressed_bytes().encode_vec(&mut buf);
        }

        buf
    }

    #[cfg(not(feature = "storage-v2"))]
    pub fn decode(bytes: &[u8]) -> Result<Message, decoding::Error> {
        let mut decoder = Decoder::new(Cow::Borrowed(bytes));

        let message_type = decoder.read::<u8>()?;
        if message_type != MESSAGE_TYPE_SYNC {
            return Err(decoding::Error::WrongType {
                expected_one_of: vec![MESSAGE_TYPE_SYNC],
                found: message_type,
            });
        }

        let heads = decode_hashes(&mut decoder)?;
        let need = decode_hashes(&mut decoder)?;
        let have_count = decoder.read::<u32>()?;
        let mut have = Vec::with_capacity(have_count as usize);
        for _ in 0..have_count {
            let last_sync = decode_hashes(&mut decoder)?;
            let bloom_bytes: Vec<u8> = decoder.read()?;
            let bloom = BloomFilter::try_from(bloom_bytes.as_slice())?;
            have.push(Have { last_sync, bloom });
        }

        let change_count = decoder.read::<u32>()?;
        let mut changes = Vec::with_capacity(change_count as usize);
        for _ in 0..change_count {
            let change = decoder.read()?;
            changes.push(Change::from_bytes(change)?);
        }

        Ok(Message {
            heads,
            need,
            have,
            changes,
        })
    }
}

#[cfg(not(feature = "storage-v2"))]
fn encode_hashes(buf: &mut Vec<u8>, hashes: &[ChangeHash]) {
    debug_assert!(
        hashes.windows(2).all(|h| h[0] <= h[1]),
        "hashes were not sorted"
    );
    hashes.encode_vec(buf);
}

#[cfg(feature = "storage-v2")]
fn encode_many<'a, I, It, F>(out: &mut Vec<u8>, data: I, f: F)
where
    I: Iterator<Item = It> + ExactSizeIterator + 'a,
    F: Fn(&mut Vec<u8>, It),
{
    leb128::write::unsigned(out, data.len() as u64).unwrap();
    for datum in data {
        f(out, datum)
    }
}

#[cfg(feature = "storage-v2")]
fn encode_hashes(buf: &mut Vec<u8>, hashes: &[ChangeHash]) {
    debug_assert!(
        hashes.windows(2).all(|h| h[0] <= h[1]),
        "hashes were not sorted"
    );
    encode_many(buf, hashes.iter(), |buf, hash| buf.extend(hash.as_bytes()))
}

#[cfg(not(feature = "storage-v2"))]
impl Encodable for &[ChangeHash] {
    fn encode<W: Write>(&self, buf: &mut W) -> io::Result<usize> {
        let head = self.len().encode(buf)?;
        let mut body = 0;
        for hash in self.iter() {
            buf.write_all(&hash.0)?;
            body += hash.0.len();
        }
        Ok(head + body)
    }
}

#[cfg(not(feature = "storage-v2"))]
fn decode_hashes(decoder: &mut Decoder<'_>) -> Result<Vec<ChangeHash>, decoding::Error> {
    let length = decoder.read::<u32>()?;
    let mut hashes = Vec::with_capacity(length as usize);

    for _ in 0..length {
        let hash_bytes = decoder.read_bytes(HASH_SIZE)?;
        let hash = ChangeHash::try_from(hash_bytes).map_err(decoding::Error::BadChangeFormat)?;
        hashes.push(hash);
    }

    Ok(hashes)
}

fn advance_heads(
    my_old_heads: &HashSet<&ChangeHash>,
    my_new_heads: &HashSet<ChangeHash>,
    our_old_shared_heads: &[ChangeHash],
) -> Vec<ChangeHash> {
    let new_heads = my_new_heads
        .iter()
        .filter(|head| !my_old_heads.contains(head))
        .copied()
        .collect::<Vec<_>>();

    let common_heads = our_old_shared_heads
        .iter()
        .filter(|head| my_new_heads.contains(head))
        .copied()
        .collect::<Vec<_>>();

    let mut advanced_heads = HashSet::with_capacity(new_heads.len() + common_heads.len());
    for head in new_heads.into_iter().chain(common_heads) {
        advanced_heads.insert(head);
    }
    let mut advanced_heads = advanced_heads.into_iter().collect::<Vec<_>>();
    advanced_heads.sort();
    advanced_heads
}
