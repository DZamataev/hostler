//! The streaming secret masker (plan P2, Task 3): raw bytes, every injected value → `***`,
//! leftmost match, longest value first, at most `max_len − 1` bytes held back. `push`, `finish`
//! and `seal` only move bytes between buffers — no I/O — so a caller may hold a lock around
//! them (L14).

use std::fmt;
use std::sync::Arc;

use zeroize::{Zeroize, Zeroizing};

pub const MASK: &[u8] = b"***";

/// The values to mask: non-empty, deduplicated, longest first. Erased on drop.
pub struct MaskSet {
    values: Vec<Zeroizing<Vec<u8>>>,
}

/// What the input at one position calls for.
enum Step {
    /// A value of this many bytes starts here.
    Mask(usize),
    /// The rest of the input is a proper prefix of a value that more input may complete.
    Hold,
    /// No value starts here.
    Byte,
}

impl MaskSet {
    pub fn new(values: impl IntoIterator<Item = Vec<u8>>) -> MaskSet {
        let mut values: Vec<Zeroizing<Vec<u8>>> = values
            .into_iter()
            .filter(|v| !v.is_empty())
            .map(Zeroizing::new)
            .collect();
        values.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[..].cmp(&b[..])));
        values.dedup_by(|a, b| a[..] == b[..]);
        MaskSet { values }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn max_len(&self) -> usize {
        self.values.first().map_or(0, |v| v.len())
    }

    /// The whole input masked, as if pushed to a fresh masker and then finished.
    pub fn mask_all(&self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        let used = self.scan(input, true, &mut out);
        debug_assert_eq!(used, input.len());
        out
    }

    fn step(&self, rest: &[u8], eof: bool) -> Step {
        // Longest first: a longer value that may still complete wins over a shorter full match,
        // and the first full match found is the longest one.
        for v in &self.values {
            if rest.len() < v.len() {
                if !eof && v.starts_with(rest) {
                    return Step::Hold;
                }
            } else if rest.starts_with(v) {
                return Step::Mask(v.len());
            }
        }
        Step::Byte
    }

    /// Masks `buf` into `out`; returns how many bytes were decided (the rest must be held).
    fn scan(&self, buf: &[u8], eof: bool, out: &mut Vec<u8>) -> usize {
        let mut i = 0;
        while i < buf.len() {
            match self.step(&buf[i..], eof) {
                Step::Mask(n) => {
                    out.extend_from_slice(MASK);
                    i += n;
                }
                Step::Hold => break,
                Step::Byte => {
                    out.push(buf[i]);
                    i += 1;
                }
            }
        }
        i
    }
}

impl fmt::Debug for MaskSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MaskSet")
            .field("values", &self.values.len())
            .finish()
    }
}

/// Masks one output stream chunk by chunk.
pub struct Masker {
    set: Arc<MaskSet>,
    /// Held bytes: always empty or a proper prefix of a value.
    pending: Zeroizing<Vec<u8>>,
    sealed: bool,
}

impl Masker {
    pub fn new(set: Arc<MaskSet>) -> Masker {
        Masker {
            set,
            pending: Zeroizing::new(Vec::new()),
            sealed: false,
        }
    }

    /// Appends the masked part of `chunk` that is decided to `out`. A no-op once sealed.
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        if self.sealed {
            return;
        }
        let need = self.pending.len() + chunk.len();
        if need > self.pending.capacity() {
            // Grow by hand: Vec's own realloc would free the old buffer without erasing it.
            let mut grown =
                Zeroizing::new(Vec::with_capacity(need.max(2 * self.pending.capacity())));
            grown.extend_from_slice(&self.pending);
            self.pending = grown;
        }
        self.pending.extend_from_slice(chunk);
        let used = self.set.scan(&self.pending, false, out);
        self.pending.drain(..used);
    }

    /// Real EOF: masks what is held, emits the rest verbatim. A no-op once sealed.
    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.sealed {
            return;
        }
        let used = self.set.scan(&self.pending, true, out);
        debug_assert_eq!(used, self.pending.len());
        self.pending.zeroize();
    }

    /// The drain grace is over (L7): a held prefix becomes `***`, and every later `push` and
    /// `finish` emits nothing.
    pub fn seal(&mut self, out: &mut Vec<u8>) {
        if !self.pending.is_empty() {
            out.extend_from_slice(MASK);
        }
        self.pending.zeroize();
        self.sealed = true;
    }

    pub fn is_sealed(&self) -> bool {
        self.sealed
    }
}

impl fmt::Debug for Masker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Masker")
            .field("set", &self.set)
            .field("sealed", &self.sealed)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: &[&[u8]]) -> Arc<MaskSet> {
        Arc::new(MaskSet::new(values.iter().map(|v| v.to_vec())))
    }

    fn masker(values: &[&[u8]]) -> Masker {
        Masker::new(set(values))
    }

    /// An independent model: at each position the longest value starting there, else the byte.
    fn reference(values: &[&[u8]], input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < input.len() {
            let longest = values
                .iter()
                .filter(|v| !v.is_empty() && input[i..].starts_with(v))
                .map(|v| v.len())
                .max();
            match longest {
                Some(n) => {
                    out.extend_from_slice(MASK);
                    i += n;
                }
                None => {
                    out.push(input[i]);
                    i += 1;
                }
            }
        }
        out
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    /// Pushes `input` in every one of its 2^(n−1) chunkings (an empty push first) and checks
    /// each against the reference: after every push the output is a prefix of the final one
    /// (nothing undecided is emitted) and what is held is a proper prefix of a value; after
    /// `finish` the output equals the reference and holds no value. Failure messages name the
    /// chunking, never the bytes.
    fn check_every_chunking(values: &[&[u8]], set: &Arc<MaskSet>, input: &[u8]) {
        let want = reference(values, input);
        assert!(
            set.mask_all(input) == want,
            "mask_all differs from the reference for a {}-byte input",
            input.len()
        );
        assert!(
            values.iter().all(|v| !contains(&want, v)),
            "the reference leaves a value for a {}-byte input",
            input.len()
        );
        let cuts = input.len().saturating_sub(1);
        for split in 0u32..(1 << cuts) {
            let mut m = Masker::new(set.clone());
            let mut out = Vec::new();
            m.push(b"", &mut out);
            let mut start = 0;
            for end in 1..=input.len() {
                if end < input.len() && split & (1 << (end - 1)) == 0 {
                    continue;
                }
                m.push(&input[start..end], &mut out);
                start = end;
                assert!(
                    want.starts_with(&out),
                    "chunking {split:#b} of a {}-byte input emitted undecided bytes",
                    input.len()
                );
                let held = &m.pending[..];
                assert!(
                    held.is_empty()
                        || set
                            .values
                            .iter()
                            .any(|v| v.len() > held.len() && v.starts_with(held)),
                    "chunking {split:#b} of a {}-byte input holds {} bytes that are no proper prefix",
                    input.len(),
                    held.len()
                );
            }
            m.finish(&mut out);
            assert!(
                out == want,
                "chunking {split:#b} of a {}-byte input differs from mask_all",
                input.len()
            );
            assert!(m.pending.is_empty());
        }
    }

    /// Every input over `alphabet` up to `len` bytes.
    fn all_inputs(alphabet: &[u8], len: usize) -> Vec<Vec<u8>> {
        let mut all = vec![Vec::new()];
        let mut level = vec![Vec::new()];
        for _ in 0..len {
            level = level
                .iter()
                .flat_map(|p: &Vec<u8>| {
                    alphabet.iter().map(move |&c| {
                        let mut n = p.clone();
                        n.push(c);
                        n
                    })
                })
                .collect();
            all.extend(level.iter().cloned());
        }
        all
    }

    #[test]
    fn masks_every_occurrence() {
        let mut out = Vec::new();
        masker(&[b"tok3n"]).push(b"tok3n a tok3ntok3n b", &mut out);
        assert_eq!(out, b"*** a ****** b");
    }

    #[test]
    fn longest_value_wins() {
        let s = set(&[b"abcd", b"abcdefgh"]);
        assert_eq!(s.mask_all(b"xabcdefghy"), b"x***y");
        // A shorter full match waits for the longer value, then wins when that fails.
        let mut m = Masker::new(s);
        let mut out = Vec::new();
        m.push(b"xabcdef", &mut out);
        assert_eq!(out, b"x");
        m.push(b"Z", &mut out);
        assert_eq!(out, b"x***efZ");
    }

    #[test]
    fn overlap_is_leftmost() {
        assert_eq!(set(&[b"abc", b"bcd"]).mask_all(b"abcd"), b"***d");
        assert_eq!(set(&[b"bcd", b"abc"]).mask_all(b"abcd"), b"***d");
    }

    #[test]
    fn split_value_is_masked_for_every_chunking() {
        let sets: [&[&[u8]]; 3] = [
            // self-overlap, a value extending another, an inner overlap, a short one
            &[b"abab", b"ababc", b"bab", b"ca"],
            // equal-length rotations: leftmost decides
            &[b"abc", b"bca", b"cab"],
            // nested prefixes and duplicates
            &[b"aaa", b"aabaa", b"aabaa", b"ba", b"aabaab"],
        ];
        let mut lcg: u64 = 0x2545_f491_4f6c_dd1d;
        for values in sets {
            let s = set(values);
            // every input up to 7 bytes, every chunking
            for input in all_inputs(b"abc", 7) {
                check_every_chunking(values, &s, &input);
            }
            // longer inputs up to 12 bytes: built from the values and pseudo-random, every chunking
            let mut inputs: Vec<Vec<u8>> = vec![
                values
                    .iter()
                    .flat_map(|v| v.iter().copied())
                    .take(12)
                    .collect(),
                values
                    .iter()
                    .rev()
                    .flat_map(|v| v.iter().copied())
                    .take(12)
                    .collect(),
            ];
            for len in 8..=12 {
                for _ in 0..12 {
                    let input = (0..len)
                        .map(|_| {
                            lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                            b"abc"[(lcg >> 33) as usize % 3]
                        })
                        .collect();
                    inputs.push(input);
                }
            }
            for input in inputs {
                check_every_chunking(values, &s, &input);
            }
        }
    }

    #[test]
    fn holds_at_most_max_len_minus_one() {
        let mut m = masker(&[b"abcd"]);
        let mut out = Vec::new();
        m.push(b"xxab", &mut out);
        assert_eq!(out, b"xx");
        m.push(b"abc", &mut out);
        assert!(m.pending.len() < m.set.max_len());
        assert_eq!(out, b"xxab");
        m.push(b"d!", &mut out);
        assert_eq!(out, b"xxab***!");

        let mut m = masker(&[b"abcd"]);
        let mut out = Vec::new();
        m.push(b"xxab", &mut out);
        m.push(b"cd!", &mut out);
        assert_eq!(out, b"xx***!");
        assert!(m.pending.is_empty());
    }

    #[test]
    fn partial_prefix_is_flushed_at_finish() {
        let mut m = masker(&[b"abcd"]);
        let mut out = Vec::new();
        m.push(b"ab", &mut out);
        assert!(out.is_empty());
        m.finish(&mut out);
        assert_eq!(out, b"ab");
    }

    #[test]
    fn seal_masks_a_held_prefix_and_discards_the_rest() {
        let mut m = masker(&[b"abcdef"]);
        let mut out = Vec::new();
        m.push(b"xabc", &mut out);
        assert_eq!(out, b"x");
        m.seal(&mut out);
        assert_eq!(out, b"x***");
        assert!(m.is_sealed());
        m.push(b"def", &mut out);
        m.finish(&mut out);
        m.seal(&mut out);
        assert_eq!(out, b"x***");
    }

    #[test]
    fn seal_with_nothing_held_emits_nothing() {
        let mut m = masker(&[b"abcdef"]);
        let mut out = Vec::new();
        m.push(b"xyz", &mut out);
        m.seal(&mut out);
        assert_eq!(out, b"xyz");
        assert!(m.is_sealed());
        m.push(b"more", &mut out);
        assert_eq!(out, b"xyz");
    }

    #[test]
    fn no_values_is_identity() {
        let s = set(&[b"", b""]);
        assert!(s.is_empty());
        assert_eq!(s.max_len(), 0);
        assert_eq!(s.mask_all(b"any \xff thing"), b"any \xff thing");
        let mut m = Masker::new(s);
        let mut out = Vec::new();
        m.push(b"a", &mut out);
        m.push(b"bc", &mut out);
        assert_eq!(out, b"abc");
    }

    #[test]
    fn binary_and_invalid_utf8_pass_through() {
        let value: &[u8] = &[0xff, 0x00, 0xfe, 0x80];
        let s = set(&[value]);
        assert_eq!(
            s.mask_all(&[0x00, 0xff, 0x00, 0xfe, 0x80, 0xc3]),
            [&[0x00][..], MASK, &[0xc3]].concat()
        );
        let mut m = Masker::new(s);
        let mut out = Vec::new();
        m.push(&[0xc3, 0x28, 0xff, 0x00], &mut out);
        m.push(&[0xfe, 0x80, 0xa0], &mut out);
        assert_eq!(out, [&[0xc3, 0x28][..], MASK, &[0xa0]].concat());
    }

    #[test]
    fn masked_display_command() {
        let s = set(&[b"s3cr3t-value"]);
        assert_eq!(
            s.mask_all(b"curl -H 'Authorization: Bearer s3cr3t-value' https://example.test"),
            b"curl -H 'Authorization: Bearer ***' https://example.test"
        );
    }

    #[test]
    fn debug_prints_no_value() {
        let mut m = masker(&[b"s3cr3t-value"]);
        let mut out = Vec::new();
        m.push(b"s3cr3t", &mut out);
        let shown = format!("{m:?} {:?}", m.set);
        assert!(!shown.contains("s3cr3t") && !shown.contains("115"));
    }
}
