use std::convert::TryFrom;
use std::io;
use std::io::Write;

use automerge_protocol::Patch;

use crate::AutomergeError;
use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use automerge_protocol::ChangeHash;

use crate::{
    encoding::{Decoder, Encodable},
    Backend, Change,
};

mod bloom;
mod state;

pub use bloom::BloomFilter;
pub use state::SyncState;

const MESSAGE_TYPE_SYNC: u8 = 0x42; // first byte of a sync message, for identification

impl Backend {
    pub fn generate_sync_message(
        &self,
        mut sync_state: SyncState,
    ) -> (SyncState, Option<SyncMessage>) {
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
                    let reset_msg = SyncMessage {
                        heads: our_heads,
                        need: Vec::new(),
                        have: vec![SyncHave::default()],
                        changes: Vec::new(),
                    };
                    return (sync_state, Some(reset_msg));
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

        let heads_unchanged = if let Some(last_sent_heads) = sync_state.last_sent_heads.as_ref() {
            last_sent_heads == &our_heads
        } else {
            false
        };

        let heads_equal = if let Some(their_heads) = sync_state.their_heads.as_ref() {
            their_heads == &our_heads
        } else {
            false
        };

        if heads_unchanged && heads_equal && changes_to_send.is_empty() && our_need.is_empty() {
            return (sync_state, None);
        }

        if !sync_state.sent_changes.is_empty() && !changes_to_send.is_empty() {
            changes_to_send = deduplicate_changes(&sync_state.sent_changes, changes_to_send)
        }

        let sync_message = SyncMessage {
            heads: our_heads.clone(),
            have: our_have,
            need: our_need,
            changes: changes_to_send.clone(),
        };

        sync_state.last_sent_heads = Some(our_heads);
        sync_state.sent_changes.extend(changes_to_send);

        (sync_state, Some(sync_message))
    }

    pub fn receive_sync_message(
        &mut self,
        message: SyncMessage,
        mut sync_state: SyncState,
    ) -> Result<(SyncState, Option<Patch>), AutomergeError> {
        let mut patch = None;

        let before_heads = self.get_heads();

        let SyncMessage {
            heads: message_heads,
            changes: message_changes,
            need: message_need,
            have: message_have,
        } = message;

        let changes_is_empty = message_changes.is_empty();
        if !changes_is_empty {
            patch = Some(self.apply_changes(message_changes)?);
            sync_state.shared_heads = advance_heads(
                &before_heads.iter().collect(),
                &self.get_heads().into_iter().collect(),
                &sync_state.shared_heads,
            )
        }

        if changes_is_empty && message_heads == before_heads {
            sync_state.last_sent_heads = Some(message_heads.clone())
        }

        let known_heads = message_heads
            .iter()
            .filter(|head| self.get_change_by_hash(head).is_some())
            .collect::<Vec<_>>();
        if known_heads.len() == message_heads.len() {
            sync_state.shared_heads = message_heads.clone()
        } else {
            sync_state.shared_heads = sync_state
                .shared_heads
                .iter()
                .chain(known_heads)
                .collect::<HashSet<_>>()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            sync_state.shared_heads.sort();
        }

        sync_state.their_have = Some(message_have);
        sync_state.their_heads = Some(message_heads);
        sync_state.their_need = Some(message_need);

        Ok((sync_state, patch))
    }

    fn make_bloom_filter(&self, last_sync: Vec<ChangeHash>) -> SyncHave {
        let new_changes = self.get_changes(&last_sync);
        let hashes = new_changes
            .into_iter()
            .map(|change| change.hash)
            .collect::<Vec<_>>();
        SyncHave {
            last_sync,
            bloom: BloomFilter::from(&hashes[..]),
        }
    }

    pub fn get_changes_to_send(&self, have: Vec<SyncHave>, need: &[ChangeHash]) -> Vec<Change> {
        if have.is_empty() {
            need.iter()
                .filter_map(|hash| self.get_change_by_hash(hash).cloned())
                .collect()
        } else {
            let mut last_sync_hashes = HashSet::new();
            let mut bloom_filters = Vec::new();

            for h in have {
                let SyncHave { last_sync, bloom } = h;
                for hash in last_sync {
                    last_sync_hashes.insert(hash);
                }
                bloom_filters.push(bloom)
            }
            let last_sync_hashes = last_sync_hashes.into_iter().collect::<Vec<_>>();

            let changes = self.get_changes(&last_sync_hashes);

            let mut change_hashes = HashSet::new();
            let mut dependents: HashMap<ChangeHash, Vec<ChangeHash>> = HashMap::new();
            let mut hashes_to_send = HashSet::new();

            for change in &changes {
                change_hashes.insert(change.hash);

                for dep in &change.deps {
                    dependents.entry(*dep).or_default().push(change.hash);
                }

                if bloom_filters
                    .iter()
                    .all(|bloom| !bloom.contains_hash(&change.hash))
                {
                    hashes_to_send.insert(change.hash);
                }
            }

            let mut stack = hashes_to_send.iter().cloned().collect::<Vec<_>>();
            while let Some(hash) = stack.pop() {
                if let Some(deps) = dependents.get(&hash) {
                    for dep in deps {
                        if hashes_to_send.insert(*dep) {
                            stack.push(*dep)
                        }
                    }
                }
            }

            let mut changes_to_send = Vec::new();
            for hash in need {
                hashes_to_send.insert(*hash);
                if !change_hashes.contains(&hash) {
                    let change = self.get_change_by_hash(&hash);
                    if let Some(change) = change {
                        changes_to_send.push(change.clone())
                    }
                }
            }

            for change in changes {
                if hashes_to_send.contains(&change.hash) {
                    changes_to_send.push(change.clone())
                }
            }
            changes_to_send
        }
    }
}

#[derive(Debug)]
pub struct SyncMessage {
    pub heads: Vec<ChangeHash>,
    pub need: Vec<ChangeHash>,
    pub have: Vec<SyncHave>,
    pub changes: Vec<Change>,
}

impl SyncMessage {
    pub fn encode(self) -> Result<Vec<u8>, AutomergeError> {
        let mut buf = vec![MESSAGE_TYPE_SYNC];

        encode_hashes(&mut buf, &self.heads)?;
        encode_hashes(&mut buf, &self.need)?;
        (self.have.len() as u32).encode(&mut buf)?;
        for have in self.have {
            encode_hashes(&mut buf, &have.last_sync)?;
            have.bloom.into_bytes()?.encode(&mut buf)?;
        }

        (self.changes.len() as u32).encode(&mut buf)?;
        for change in self.changes {
            change.raw_bytes().encode(&mut buf)?;
        }

        Ok(buf)
    }

    pub fn decode(bytes: &[u8]) -> Result<SyncMessage, AutomergeError> {
        let mut decoder = Decoder::new(Cow::Borrowed(bytes));

        let message_type = decoder.read::<u8>()?;
        if message_type != MESSAGE_TYPE_SYNC {
            return Err(AutomergeError::EncodingError);
        }

        let heads = decode_hashes(&mut decoder)?;
        let need = decode_hashes(&mut decoder)?;
        let have_count = decoder.read::<u32>()?;
        let mut have = Vec::new();
        for _ in 0..have_count {
            let last_sync = decode_hashes(&mut decoder)?;
            let bloom_bytes: Vec<u8> = decoder.read()?;
            let bloom = BloomFilter::try_from(bloom_bytes.as_slice())?;
            have.push(SyncHave { last_sync, bloom });
        }

        let change_count = decoder.read::<u32>()?;
        let mut changes = Vec::new();
        for _ in 0..change_count {
            let change = decoder.read()?;
            changes.push(Change::from_bytes(change)?);
        }

        Ok(SyncMessage {
            heads,
            need,
            have,
            changes,
        })
    }
}

fn encode_hashes(buf: &mut Vec<u8>, hashes: &[ChangeHash]) -> Result<(), AutomergeError> {
    debug_assert!(
        hashes.windows(2).all(|h| h[0] <= h[1]),
        "hashes were not sorted"
    );
    hashes.encode(buf)?;
    Ok(())
}

impl Encodable for &[ChangeHash] {
    fn encode<W: Write>(&self, buf: &mut W) -> io::Result<usize> {
        let head = self.len().encode(buf)?;
        let mut body = 0;
        for hash in self.iter() {
            buf.write_all(&hash.0)?;
            body += hash.0.len()
        }
        Ok(head + body)
    }
}

fn decode_hashes(decoder: &mut Decoder) -> Result<Vec<ChangeHash>, AutomergeError> {
    let length = decoder.read::<u32>()?;
    let mut hashes = Vec::new();

    const HASH_SIZE: usize = 32; // 256 bits = 32 bytes
    for _ in 0..length {
        let hash_bytes = decoder.read_bytes(HASH_SIZE)?;
        let hash = ChangeHash::try_from(hash_bytes)
            .map_err(|source| AutomergeError::ChangeBadFormat { source })?;
        hashes.push(hash);
    }

    Ok(hashes)
}

#[derive(Debug, Clone, Default)]
pub struct SyncHave {
    pub last_sync: Vec<ChangeHash>,
    pub bloom: BloomFilter,
}

fn deduplicate_changes(previous_changes: &[Change], new_changes: Vec<Change>) -> Vec<Change> {
    let mut index: HashMap<u32, Vec<usize>> = HashMap::new();

    for (i, change) in previous_changes.iter().enumerate() {
        let checksum = change.checksum();
        index.entry(checksum).or_default().push(i);
    }

    new_changes
        .into_iter()
        .filter(|change| {
            if let Some(positions) = index.get(&change.checksum()) {
                !positions.iter().any(|i| change == &previous_changes[*i])
            } else {
                true
            }
        })
        .collect()
}

fn advance_heads(
    my_old_heads: &HashSet<&ChangeHash>,
    my_new_heads: &HashSet<ChangeHash>,
    our_old_shared_heads: &[ChangeHash],
) -> Vec<ChangeHash> {
    let new_heads = my_new_heads
        .iter()
        .filter(|head| !my_old_heads.contains(head))
        .cloned()
        .collect::<Vec<_>>();

    let common_heads = our_old_shared_heads
        .iter()
        .filter(|head| my_new_heads.contains(head))
        .cloned()
        .collect::<Vec<_>>();

    let mut advanced_heads = HashSet::new();
    for head in new_heads {
        advanced_heads.insert(head);
    }
    for head in common_heads {
        advanced_heads.insert(head);
    }
    let mut advanced_heads = advanced_heads.into_iter().collect::<Vec<_>>();
    advanced_heads.sort();
    advanced_heads
}