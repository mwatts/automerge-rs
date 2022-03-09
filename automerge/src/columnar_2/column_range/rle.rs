use std::{
    borrow::{Borrow, Cow},
    fmt::Debug,
    marker::PhantomData,
    ops::Range,
};

use crate::columnar_2::encoding::{Decodable, Encodable, RleDecoder, RleEncoder};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RleRange<T> {
    range: Range<usize>,
    _phantom: PhantomData<T>,
}

impl<T> RleRange<T> {
    pub(crate) fn decoder<'a>(&self, data: &'a [u8]) -> RleDecoder<'a, T> {
        RleDecoder::from(Cow::Borrowed(&data[self.range.clone()]))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.range.is_empty()
    }

    pub(crate) fn start(&self) -> usize {
        self.range.start
    }

    pub(crate) fn end(&self) -> usize {
        self.range.end
    }
}

impl<T: Clone + Decodable + Encodable + PartialEq + Eq + Debug> RleRange<T> {
    /// The semantics of this are similar to `Vec::splice`
    ///
    /// # Arguments
    ///
    /// * `data` - The buffer containing the original rows
    /// * `replace` - The range of elements in the original collection to replace
    /// * `replace_with` - An iterator to insert in place of the original elements.
    /// * `out` - The buffer to encode the resulting collection into
    pub(crate) fn splice<'a, I: Iterator<Item = Option<TB>>, TB: Borrow<T> + 'a>(
        &self,
        data: &[u8],
        replace: Range<usize>,
        mut replace_with: I,
        out: &mut Vec<u8>,
    ) -> Self {
        let start = out.len();
        let mut encoder = self.encoder(out);
        let mut decoder = self.decoder(data);
        let mut idx = 0;
        while idx < replace.start {
            match decoder.next() {
                Some(elem) => encoder.append(elem.as_ref()),
                None => panic!("out of bounds"),
            }
            idx += 1;
        }
        for _ in 0..replace.len() {
            decoder.next();
            if let Some(next) = replace_with.next() {
                encoder.append(next.as_ref().map(|n| n.borrow()));
            }
        }
        for next in replace_with {
            encoder.append(next.as_ref().map(|n| n.borrow()));
        }
        for next in decoder {
            encoder.append(next.as_ref());
        }
        let range = start..(start + encoder.finish());
        range.into()
    }
}

impl<'a, T: Encodable + Clone + PartialEq + 'a> RleRange<T> {
    pub(crate) fn encoder<'b>(&self, output: &'b mut Vec<u8>) -> RleEncoder<'b, T> {
        RleEncoder::from(output)
    }

    pub(crate) fn encode<BT: Borrow<T>, I: Iterator<Item = Option<BT>>>(
        items: I,
        out: &mut Vec<u8>,
    ) -> Self {
        let start = out.len();
        let mut encoder = RleEncoder::new(out);
        for item in items {
            encoder.append(item);
        }
        let len = encoder.finish();
        (start..(start + len)).into()
    }
}

impl<T> AsRef<Range<usize>> for RleRange<T> {
    fn as_ref(&self) -> &Range<usize> {
        &self.range
    }
}

impl<T> From<Range<usize>> for RleRange<T> {
    fn from(r: Range<usize>) -> RleRange<T> {
        RleRange {
            range: r,
            _phantom: PhantomData,
        }
    }
}

impl<T> From<RleRange<T>> for Range<usize> {
    fn from(r: RleRange<T>) -> Range<usize> {
        r.range
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columnar_2::encoding::properties::option_splice_scenario;
    use proptest::prelude::*;
    use std::borrow::Cow;

    #[test]
    fn rle_int_round_trip() {
        let vals = [1, 1, 2, 2, 3, 2, 3, 1, 3];
        let mut buf = Vec::with_capacity(vals.len() * 3);
        let mut encoder: RleEncoder<'_, u64> = RleEncoder::new(&mut buf);
        for val in vals {
            encoder.append_value(&val)
        }
        let total_slice_len = encoder.finish();
        let mut decoder: RleDecoder<'_, u64> =
            RleDecoder::from(Cow::Borrowed(&buf[0..total_slice_len]));
        let mut result = Vec::new();
        while let Some(Some(val)) = decoder.next() {
            result.push(val);
        }
        assert_eq!(result, vals);
    }

    #[test]
    fn rle_int_insert() {
        let vals = [1, 1, 2, 2, 3, 2, 3, 1, 3];
        let mut buf = Vec::with_capacity(vals.len() * 3);
        let mut encoder: RleEncoder<'_, u64> = RleEncoder::new(&mut buf);
        for val in vals.iter().take(4) {
            encoder.append_value(val)
        }
        encoder.append_value(&5);
        for val in vals.iter().skip(4) {
            encoder.append_value(val);
        }
        let total_slice_len = encoder.finish();
        let mut decoder: RleDecoder<'_, u64> =
            RleDecoder::from(Cow::Borrowed(&buf[0..total_slice_len]));
        let mut result = Vec::new();
        while let Some(Some(val)) = decoder.next() {
            result.push(val);
        }
        let expected = [1, 1, 2, 2, 5, 3, 2, 3, 1, 3];
        assert_eq!(result, expected);
    }

    fn encode<T: Clone + Encodable + PartialEq>(vals: &[Option<T>]) -> (RleRange<T>, Vec<u8>) {
        let mut buf = Vec::with_capacity(vals.len() * 3);
        let range = RleRange::<T>::encode(vals.iter().map(|v| v.as_ref()), &mut buf);
        (range, buf)
    }

    fn decode<T: Clone + Decodable + Debug>(range: RleRange<T>, buf: &[u8]) -> Vec<Option<T>> {
        range.decoder(buf).collect()
    }

    proptest! {
        #[test]
        fn splice_ints(scenario in option_splice_scenario(any::<Option<i32>>())) {
            let (range, buf) = encode(&scenario.initial_values);
            let mut out = Vec::new();
            let new_range = range.splice(&buf, scenario.replace_range.clone(), scenario.replacements.iter().cloned(), &mut out);
            let result = decode::<i32>(new_range, &out);
            scenario.check_optional(result)
        }

        #[test]
        fn splice_strings(scenario in option_splice_scenario(any::<Option<String>>())) {
            let (range, buf) = encode(&scenario.initial_values);
            let mut out = Vec::new();
            let new_range = range.splice(&buf, scenario.replace_range.clone(), scenario.replacements.iter().cloned(), &mut out);
            let result = decode::<String>(new_range, &out);
            scenario.check_optional(result)
        }
    }
}
