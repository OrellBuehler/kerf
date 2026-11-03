//! swscale's bicubic filter tables, ported from `initFilter` (libswscale
//! `utils.c`) for the one configuration Kerf uses: `SWS_BICUBIC` with the default
//! parameters (B = 0, C = 0.6), no source / destination filter, 8-bit planar.
//!
//! A formula for "bicubic" is not enough to match FFmpeg's scaler: the *table*
//! it builds has quirks that show wherever the picture meets its own edge, and a
//! busy picture shows them everywhere along the border —
//!
//! * the window of an output sample starts at `(pos - 3) / 2` **truncated toward
//!   zero**, so at the first pixels the window starts one source pixel late and
//!   the tap that should be at index -1 is simply absent (rather than folded into
//!   pixel 0 as a clamped read would);
//! * taps near the right edge are shifted back inside the picture, the weight of
//!   what fell off added to the last pixel;
//! * leading and trailing taps whose cumulative weight is under 0.2% are dropped;
//! * the coefficients are 14 bits (horizontal) / 12 bits (vertical) integers,
//!   normalized to sum to exactly 1 by error diffusion;
//! * the step is `((src << 16) + dst / 2) / dst` in 16.16 fixed point, so the
//!   ratio is not quite `src / dst`.
//!
//! The shader reads these tables and does swscale's integer arithmetic on them
//! (a 15-bit horizontal intermediate clipped at the top, a vertical pass rounded
//! at bit 19), so what comes out is the scaler's output, not an approximation
//! of it.
//!
//! Chroma sample position is the default (centred, `-513` in swscale's terms) for
//! source and destination alike, which cancels: a plane is resampled as if its
//! samples sat at pixel centres.

use std::sync::{Arc, Mutex};

/// The horizontal pass's coefficient precision (`1 << 14`).
pub const ONE_HORIZONTAL: i32 = 1 << 14;
/// The vertical pass's (`1 << 12`).
pub const ONE_VERTICAL: i32 = 1 << 12;

/// The widest filter swscale handles without cascading two scalers (it switches
/// at `MAX_FILTER_SIZE`, 256 taps); a ratio that needs more is refused.
const MAX_FILTER_SIZE: usize = 256;

/// `SWS_MAX_REDUCE_CUTOFF`.
const REDUCE_CUTOFF: f64 = 0.002;

/// The taps of every output sample along one axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Taps per output sample (rows are padded with zeros to this).
    pub size: usize,
    /// The first source sample of each output sample's window.
    pub pos: Vec<i32>,
    /// `pos.len() * size` integer weights, summing to `one` per output sample.
    pub coeff: Vec<i32>,
}

impl Filter {
    /// The row-major table the shader reads: per output sample, the window start
    /// followed by `size` weights, as floats (exact: every value is under 2^24).
    pub fn texels(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.pos.len() * (self.size + 1));
        for (i, p) in self.pos.iter().enumerate() {
            out.push(*p as f32);
            out.extend(self.coeff[i * self.size..(i + 1) * self.size].iter().map(|c| *c as f32));
        }
        out
    }
}

fn rounded_div(a: i64, b: i64) -> i64 {
    (if a >= 0 { a + (b >> 1) } else { a - (b >> 1) }) / b
}

/// `av_log2` for a non-negative value (0 and 1 give 0).
fn log2(v: i64) -> u32 {
    if v <= 1 {
        0
    } else {
        63 - (v as u64).leading_zeros()
    }
}

/// The step swscale derives for a `src` -> `dst` resample, in 16.16.
fn x_inc(src: u32, dst: u32) -> i64 {
    ((i64::from(src) << 16) + i64::from(dst >> 1)) / i64::from(dst)
}

/// The bicubic coefficient of one tap, at distance `d` (in 2^30 units of a
/// source pixel), scaled so a whole kernel sums to about `fone`.
fn bicubic_coeff(d: i64, fone: i64) -> i64 {
    const B: i64 = 0;
    // (0.6 * (1 << 24)) as int64: truncated, not rounded.
    const C: i64 = 10_066_329;
    if d >= 1 << 31 {
        return 0;
    }
    let dd = (d * d) >> 30;
    let ddd = (dd * d) >> 30;
    let coeff = if d < 1 << 30 {
        (12 * (1 << 24) - 9 * B - 6 * C) * ddd + (-18 * (1 << 24) + 12 * B + 6 * C) * dd + (6 * (1 << 24) - 2 * B) * (1 << 30)
    } else {
        (-B - 6 * C) * ddd + (6 * B + 30 * C) * dd + (-12 * B - 48 * C) * d + (8 * B + 24 * C) * (1 << 30)
    };
    coeff / ((1_i64 << 54) / fone)
}

/// The tables built lately, newest first: a scaled layer asks for the same six (a horizontal and a
/// vertical table for each plane) every frame of a clip, and building one is a loop of 64-bit
/// divisions. A table is a pure function of `(src, dst, one)`, so a hit is the table
/// [`bicubic_uncached`] builds, byte for byte.
static MEMO: Mutex<Memo> = Mutex::new(Memo::new(MEMO_ENTRIES, MEMO_BYTES));

/// How many tables the memo keeps, and the most bytes (4 for each window start and weight) they hold in all.
const MEMO_ENTRIES: usize = 64;
const MEMO_BYTES: usize = 32 << 20;
/// A table over this is built and not kept (it would push out the rest of the memo for one layer).
const MEMO_ENTRY_BYTES: usize = 8 << 20;

type Key = (u32, u32, i32);

struct Memo {
    /// Newest first; `None` is a ratio swscale needs two passes for.
    entries: Vec<(Key, Option<Arc<Filter>>)>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

fn table_bytes(f: &Option<Arc<Filter>>) -> usize {
    f.as_ref().map_or(0, |f| 4 * (f.pos.len() + f.coeff.len()))
}

impl Memo {
    const fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    /// The table held for `key` (made the newest), `Some(None)` if the ratio is held as refused.
    fn get(&mut self, key: Key) -> Option<Option<Arc<Filter>>> {
        let at = self.entries.iter().position(|(k, _)| *k == key)?;
        let hit = self.entries.remove(at);
        let table = hit.1.clone();
        self.entries.insert(0, hit);
        Some(table)
    }

    fn put(&mut self, key: Key, table: Option<Arc<Filter>>) {
        let size = table_bytes(&table);
        if size > MEMO_ENTRY_BYTES.min(self.max_bytes) || self.entries.iter().any(|(k, _)| *k == key) {
            return;
        }
        self.bytes += size;
        self.entries.insert(0, (key, table));
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            match self.entries.pop() {
                Some((_, evicted)) => self.bytes -= table_bytes(&evicted),
                None => break,
            }
        }
    }
}

/// Build the table that resamples `src` samples to `dst` along one axis, with
/// weights summing to `one` (`ONE_HORIZONTAL` or `ONE_VERTICAL`). `None` when the
/// ratio is beyond what swscale does in one pass. Kept in a small memo, which is
/// only locked to look a table up or to keep one, never while one is built.
pub fn bicubic(src: u32, dst: u32, one: i32) -> Option<Filter> {
    let key = (src, dst, one);
    let held = lock(&MEMO).get(key);
    if let Some(table) = held {
        return table.as_deref().cloned();
    }
    let built = bicubic_uncached(src, dst, one);
    lock(&MEMO).put(key, built.clone().map(Arc::new));
    built
}

fn lock(memo: &Mutex<Memo>) -> std::sync::MutexGuard<'_, Memo> {
    memo.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// [`bicubic`] built from scratch every time: the reference the memo is held to.
pub fn bicubic_uncached(src: u32, dst: u32, one: i32) -> Option<Filter> {
    if src == 0 || dst == 0 {
        return None;
    }
    let (src_w, dst_w) = (i64::from(src), i64::from(dst));
    let inc = x_inc(src, dst);
    let fone: i64 = 1 << (54 - log2(src_w / dst_w).min(8));

    // 1. the raw taps (`initFilter`'s scaler branches).
    let (mut filter_size, raw_pos, raw): (usize, Vec<i64>, Vec<i64>);
    if (inc - 0x10000).abs() < 10 {
        // Unscaled (source and destination positions are the same here).
        filter_size = 1;
        raw_pos = (0..dst_w).collect();
        raw = vec![fone; dst as usize];
    } else {
        const SIZE_FACTOR: i64 = 4;
        let mut size = if inc <= 1 << 16 {
            1 + SIZE_FACTOR
        } else {
            1 + (SIZE_FACTOR * src_w + dst_w - 1) / dst_w
        };
        size = size.min(src_w - 2).max(1);
        filter_size = size as usize;
        let mut pos = Vec::with_capacity(dst as usize);
        let mut taps = Vec::with_capacity(dst as usize * filter_size);
        // Centred samples on both sides: `(128 * inc >> 7) - (128 * 0x10000 >> 7)`.
        let mut x_dst_in_src: i64 = ((128 * inc) >> 7) - ((128 * 0x10000_i64) >> 7);
        for _ in 0..dst {
            let first = (x_dst_in_src - (size - 2) * (1 << 16)) / (1 << 17);
            pos.push(first);
            for xx in first..first + size {
                let mut d = ((xx * (1 << 17)) - x_dst_in_src).abs() << 13;
                if inc > 1 << 16 {
                    d = d * dst_w / src_w;
                }
                taps.push(bicubic_coeff(d, fone));
            }
            x_dst_in_src += 2 * inc;
        }
        raw_pos = pos;
        raw = taps;
    }

    // 2. `filter2` is `filter` (no source / destination filter); the window
    // start does not move (`(filterSize - 1) / 2 - (filter2Size - 1) / 2` = 0).
    let filter2_size = filter_size;
    let mut filter2 = raw;
    let mut pos = raw_pos;

    // 3. trim near-zero taps, from the last output sample back (the left trim
    // keeps the windows monotonic against the next one's).
    let cutoff = REDUCE_CUTOFF * fone as f64;
    let mut min_filter_size = 0usize;
    for i in (0..dst as usize).rev() {
        let mut min = filter2_size;
        let row = i * filter2_size;
        // The running total of what has been dropped on the left.
        let mut dropped = 0i64;
        for _ in 0..filter2_size {
            dropped += filter2[row].abs();
            if dropped as f64 > cutoff {
                break;
            }
            if i + 1 < dst as usize && pos[i] >= pos[i + 1] {
                break;
            }
            filter2.copy_within(row + 1..row + filter2_size, row);
            filter2[row + filter2_size - 1] = 0;
            pos[i] += 1;
        }
        let mut acc = 0i64;
        for j in (1..filter2_size).rev() {
            acc += filter2[row + j].abs();
            if acc as f64 > cutoff {
                break;
            }
            min -= 1;
        }
        min_filter_size = min_filter_size.max(min);
    }
    // x86's scalers want a multiple of 4 taps horizontally and 2 vertically;
    // the padding is zero weights, which changes no result — and no window,
    // because the borders below only shift by what is past the picture.
    filter_size = min_filter_size.max(1);
    if filter_size >= MAX_FILTER_SIZE {
        return None;
    }
    let mut filter = vec![0i64; dst as usize * filter_size];
    for i in 0..dst as usize {
        for j in 0..filter_size {
            if j < filter2_size {
                filter[i * filter_size + j] = filter2[i * filter2_size + j];
            }
        }
    }

    // 4. borders: weights past either edge are folded onto the edge sample.
    for (i, start) in pos.iter_mut().enumerate() {
        let row = i * filter_size;
        if *start < 0 {
            for j in 1..filter_size {
                let left = (j as i64 + *start).max(0) as usize;
                filter[row + left] += filter[row + j];
                filter[row + j] = 0;
            }
            *start = 0;
        }
        if *start + filter_size as i64 > src_w {
            let shift = *start + (filter_size as i64 - src_w).min(0);
            let mut acc = 0i64;
            for j in (0..filter_size).rev() {
                if *start + j as i64 >= src_w {
                    acc += filter[row + j];
                    filter[row + j] = 0;
                }
            }
            for j in (0..filter_size).rev() {
                filter[row + j] = if (j as i64) < shift {
                    0
                } else {
                    filter[row + j - shift as usize]
                };
            }
            *start -= shift;
            let last = (src_w - 1 - *start).clamp(0, filter_size as i64 - 1) as usize;
            filter[row + last] += acc;
        }
    }

    // 5. normalize to `one` by error diffusion.
    let mut coeff = vec![0i32; dst as usize * filter_size];
    for i in 0..dst as usize {
        let row = i * filter_size;
        let sum: i64 = filter[row..row + filter_size].iter().sum();
        let sum = ((sum + i64::from(one) / 2) / i64::from(one)).max(1);
        let mut error = 0i64;
        for j in 0..filter_size {
            let v = filter[row + j] + error;
            let int_v = rounded_div(v, sum);
            coeff[row + j] = int_v as i32;
            error = v - int_v * sum;
        }
    }
    Some(Filter {
        size: filter_size,
        pos: pos.into_iter().map(|p| p as i32).collect(),
        coeff,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPES: [(u32, u32); 14] = [
        (640, 480),
        (640, 960),
        (360, 270),
        (203, 101),
        (641, 320),
        (7, 100),
        (1920, 1080),
        (3840, 2160),
        (100, 99),
        (360, 360),
        (1, 1),
        (0, 5),
        (5, 0),
        (8000, 8),
    ];

    #[test]
    fn a_table_from_the_memo_is_the_table_built_whether_it_was_held_or_not() {
        for (src, dst) in SHAPES {
            for one in [ONE_HORIZONTAL, ONE_VERTICAL] {
                let want = bicubic_uncached(src, dst, one);
                assert_eq!(bicubic(src, dst, one), want, "{src}->{dst} {one}: made");
                assert_eq!(bicubic(src, dst, one), want, "{src}->{dst} {one}: held");
            }
        }
    }

    #[test]
    fn the_memo_is_shared_between_threads() {
        let want: Vec<_> = SHAPES.iter().map(|(s, d)| bicubic_uncached(*s, *d, ONE_VERTICAL)).collect();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..20 {
                        for ((s, d), want) in SHAPES.iter().zip(&want) {
                            assert_eq!(&bicubic(*s, *d, ONE_VERTICAL), want);
                        }
                    }
                });
            }
        });
    }

    fn table(n: u32) -> Option<Arc<Filter>> {
        bicubic_uncached(100 + n, 50, ONE_VERTICAL).map(Arc::new)
    }

    #[test]
    fn the_memo_keeps_the_newest_tables_under_its_caps() {
        let mut memo = Memo::new(3, usize::MAX);
        for n in 0..5 {
            memo.put((n, 0, 0), table(n));
        }
        let keys: Vec<u32> = memo.entries.iter().map(|(k, _)| k.0).collect();
        assert_eq!(keys, [4, 3, 2], "newest first, the oldest out");
        assert!(memo.get((0, 0, 0)).is_none());
        // A hit is made the newest, and so is not the next out.
        assert!(memo.get((2, 0, 0)).is_some());
        memo.put((9, 0, 0), table(9));
        let keys: Vec<u32> = memo.entries.iter().map(|(k, _)| k.0).collect();
        assert_eq!(keys, [9, 2, 4]);
        // The same key is kept once.
        memo.put((9, 0, 0), table(9));
        assert_eq!(memo.entries.len(), 3);
        let held: usize = memo.entries.iter().map(|(_, t)| table_bytes(t)).sum();
        assert_eq!(memo.bytes, held, "the bytes it counts are the bytes it holds");
    }

    #[test]
    fn the_memo_holds_to_its_bytes_and_keeps_no_table_over_them() {
        let one = table_bytes(&table(0));
        let mut memo = Memo::new(100, 2 * one + one / 2);
        for n in 0..6 {
            memo.put((n, 0, 0), table(0));
        }
        assert_eq!(memo.entries.len(), 2);
        assert!(memo.bytes <= memo.max_bytes);
        let mut small = Memo::new(100, one - 1);
        small.put((0, 0, 0), table(0));
        assert!(
            small.entries.is_empty() && small.bytes == 0,
            "a table over the cap is not kept"
        );
        // A refused ratio is held as refused, for no bytes.
        memo.put((77, 0, 0), None);
        assert_eq!(memo.get((77, 0, 0)), Some(None));
    }

    #[test]
    fn every_output_sample_sums_to_one() {
        for (src, dst) in [
            (640, 480),
            (640, 960),
            (360, 270),
            (203, 101),
            (641, 320),
            (7, 100),
            (1920, 1080),
            (100, 99),
        ] {
            for one in [ONE_HORIZONTAL, ONE_VERTICAL] {
                let f = bicubic(src, dst, one).unwrap_or_else(|| panic!("{src}->{dst}"));
                assert_eq!(f.pos.len(), dst as usize);
                for (i, p) in f.pos.iter().enumerate() {
                    let row = &f.coeff[i * f.size..(i + 1) * f.size];
                    assert_eq!(row.iter().sum::<i32>(), one, "{src}->{dst} sample {i}");
                    assert!(
                        *p >= 0 && (*p as usize) < src as usize,
                        "{src}->{dst} sample {i} starts at {p}"
                    );
                    // Never reads past the picture (zero weights may pad the row).
                    for (j, c) in row.iter().enumerate() {
                        assert!(*c == 0 || (*p as usize + j) < src as usize, "{src}->{dst} sample {i} tap {j}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_unscaled_axis_is_the_identity() {
        let f = bicubic(360, 360, ONE_VERTICAL).unwrap();
        assert_eq!(f.size, 1);
        assert!(f.coeff.iter().all(|c| *c == ONE_VERTICAL));
        assert_eq!(f.pos, (0..360).collect::<Vec<_>>());
    }

    #[test]
    fn a_constant_picture_stays_constant_through_every_table() {
        // The one property a quantized, normalized table must keep.
        for (src, dst) in [(640, 480), (640, 960), (203, 101), (100, 99)] {
            let f = bicubic(src, dst, ONE_HORIZONTAL).unwrap();
            for i in 0..dst as usize {
                let acc: i64 = (0..f.size).map(|j| i64::from(f.coeff[i * f.size + j]) * 200).sum();
                assert_eq!(acc >> 7, 200 * 128, "{src}->{dst} sample {i}");
            }
        }
    }

    #[test]
    fn the_window_of_the_second_sample_of_a_1_5x_upscale_starts_at_zero() {
        // The quirk the shader's old clamped read missed: the window start is
        // truncated toward zero, so sample 1 (centre 0.5) has no tap at -1 and
        // its weights are renormalized over pixels 0..3.
        let f = bicubic(640, 960, ONE_HORIZONTAL).unwrap();
        assert_eq!(f.pos[1], 0);
        let row = &f.coeff[f.size..2 * f.size];
        // Symmetric about 0.5 over pixels 0 and 1 (to the diffused rounding
        // error), and a negative lobe on 2.
        assert!((row[0] - row[1]).abs() <= 1, "{row:?}");
        assert!(row[2] < 0, "{row:?}");
    }

    #[test]
    fn a_ratio_past_the_single_pass_limit_is_refused() {
        assert!(bicubic(8000, 8, ONE_HORIZONTAL).is_none());
    }
}
