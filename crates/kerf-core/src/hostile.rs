//! Seeded, deterministic hostile-input mutation for the robustness tests of every parser that reads
//! what another program or a file wrote (caption files, ffprobe's JSON, a stored timeline, ffmpeg's
//! y4m and `showinfo` output, the settings file). No fuzzing engine: a fixed seed, a bounded number
//! of rounds, every prefix of a small input, and each case run under `catch_unwind` so one panic
//! names its input instead of ending the run. Used by tests only; public so the other crates' tests
//! share it.

use std::panic::{catch_unwind, AssertUnwindSafe};

/// The seed every robustness test starts from, so a failure reproduces.
pub const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// An input is cut at every length up to this; past it, at this many evenly spread lengths.
const PREFIXES: usize = 512;

/// xorshift64*: enough randomness to mutate bytes, and the same sequence on every machine.
#[derive(Debug, Clone)]
pub struct XorShift(u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform enough in `0..n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }
}

/// Bytes that make parsers branch: separators, digits, signs, quotes, brackets, newlines, a NUL, the
/// start of a UTF-8 sequence and an ANSI escape.
const INTERESTING: &[u8] = b"0123456789:;.,-+e/\\\"'{}[]<>\n\r\t \x00\x1b\xc3\xff";

/// The cases one input yields: every prefix (spread over the input when it is long), then `rounds`
/// random mutations — bit flips, inserted, deleted, overwritten and duplicated bytes and spans.
pub fn mutants(input: &[u8], seed: u64, rounds: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::with_capacity(PREFIXES + rounds);
    let step = input.len().div_ceil(PREFIXES).max(1);
    out.extend((0..input.len()).step_by(step).map(|n| input[..n].to_vec()));
    let mut rng = XorShift::new(seed);
    for round in 0..rounds {
        let mut v = input.to_vec();
        for _ in 0..1 + rng.below(4) {
            let len = v.len();
            match round % 6 {
                0 if len > 0 => {
                    let i = rng.below(len);
                    v[i] ^= 1 << rng.below(8);
                }
                1 => {
                    let at = rng.below(len + 1);
                    let b = if rng.below(2) == 0 {
                        INTERESTING[rng.below(INTERESTING.len())]
                    } else {
                        rng.byte()
                    };
                    v.insert(at, b);
                }
                2 if len > 0 => {
                    let at = rng.below(len);
                    let n = 1 + rng.below((len - at).min(16));
                    v.drain(at..at + n);
                }
                3 if len > 0 => {
                    let i = rng.below(len);
                    v[i] = INTERESTING[rng.below(INTERESTING.len())];
                }
                4 if len > 0 => {
                    let at = rng.below(len);
                    let n = 1 + rng.below((len - at).min(64));
                    let span = v[at..at + n].to_vec();
                    let to = rng.below(len + 1);
                    v.splice(to..to, span);
                }
                _ => {
                    let at = rng.below(len + 1);
                    v.truncate(at);
                    v.extend((0..rng.below(32)).map(|_| rng.byte()));
                }
            }
        }
        out.push(v);
    }
    out
}

/// Numbers at the edges: of `u32`, `i64` and `f64`, signed zero, a fraction.
const EXTREMES: &[&[u8]] = &[
    b"0",
    b"-1",
    b"-0.0",
    b"0.5",
    b"65536",
    b"2147483648",
    b"4294967295",
    b"9223372036854775807",
    b"18446744073709551616",
    b"1e308",
    b"-1e308",
    b"1e-308",
];

/// The cases where one number of `input` (a run of digits, signs, points and exponents with a
/// digit in it, in a string or not) is replaced by each of [`EXTREMES`], one number at a time: what a
/// byte flip rarely reaches, a width of `4294967295` or a time of `1e308`.
pub fn number_variants(input: &[u8], limit: usize) -> Vec<Vec<u8>> {
    let numeric = |b: u8| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E');
    let mut spans = Vec::new();
    let mut i = 0;
    while i < input.len() {
        if numeric(input[i]) {
            let start = i;
            while i < input.len() && numeric(input[i]) {
                i += 1;
            }
            if input[start..i].iter().any(u8::is_ascii_digit) {
                spans.push(start..i);
            }
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for span in spans.into_iter().take(limit) {
        for x in EXTREMES {
            let mut v = input[..span.start].to_vec();
            v.extend_from_slice(x);
            v.extend_from_slice(&input[span.end..]);
            out.push(v);
        }
    }
    out
}

/// The cases of [`mutants`] on which `f` panicked.
pub fn panicking<F: Fn(&[u8])>(cases: &[Vec<u8>], f: F) -> Vec<Vec<u8>> {
    cases
        .iter()
        .filter(|case| catch_unwind(AssertUnwindSafe(|| f(case))).is_err())
        .cloned()
        .collect()
}

/// Fails, naming the first few inputs, when `f` panics on any mutant or number variant (the first
/// 400 numbers) of any of `inputs`.
pub fn assert_no_panic<F: Fn(&[u8])>(what: &str, inputs: &[&[u8]], rounds: usize, f: F) {
    let mut failed = Vec::new();
    for (i, input) in inputs.iter().enumerate() {
        failed.extend(panicking(&mutants(input, SEED ^ i as u64, rounds), &f));
        failed.extend(panicking(&number_variants(input, 400), &f));
    }
    assert!(
        failed.is_empty(),
        "{what}: {} hostile inputs panicked, first: {:?}",
        failed.len(),
        failed
            .iter()
            .take(3)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect::<Vec<_>>()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_cases() {
        let input = b"1\n00:00:01,000 --> 00:00:02,000\nhello\n";
        assert_eq!(mutants(input, 7, 200), mutants(input, 7, 200));
        assert_ne!(mutants(input, 7, 200), mutants(input, 8, 200));
        let cases = mutants(input, 7, 200);
        assert_eq!(cases.len(), input.len() + 200);
        assert!(cases[..input.len()]
            .iter()
            .enumerate()
            .all(|(n, c)| c.as_slice() == &input[..n]));
    }

    #[test]
    fn a_panic_is_reported_with_its_input() {
        let cases = mutants(b"abc", 1, 50);
        let hit = panicking(&cases, |b| assert!(!b.contains(&b'\x00'), "nul"));
        assert!(hit.iter().all(|b| b.contains(&0)));
        assert!(panicking(&[b"x".to_vec()], |_| {}).is_empty());
        assert!(mutants(b"", 1, 10).len() == 10);
    }

    #[test]
    fn each_number_is_pushed_to_the_edges_in_turn() {
        let v = number_variants(br#"{"w":1920,"r":"30000/1001","id":"x"}"#, 10);
        assert_eq!(v.len(), 3 * EXTREMES.len());
        assert!(v.contains(&br#"{"w":4294967295,"r":"30000/1001","id":"x"}"#.to_vec()));
        assert!(v.contains(&br#"{"w":1920,"r":"30000/1e308","id":"x"}"#.to_vec()));
        assert_eq!(number_variants(b"no numbers - here", 10), Vec::<Vec<u8>>::new());
    }
}
