//! `checker/merge/…`: the swscale filter-table memo that now sits at `sws::table_aligned` (key: scaler, alignment,
//! source, destination, precision), held to two references that do not go through it:
//!
//! * `sws::table_aligned_uncached`, the builder the merge renamed, and
//! * `checker_merge/sws_b1e41f04.rs`, a verbatim copy of lane B's `table_aligned` before the memo existed (b1e41f04), so
//!   that the merge is also shown not to have moved the builder.
//!
//! Whatever the memo held when it was asked (more keys than it has entries, tables over its byte caps, refused ratios,
//! the same key from many threads, mixed keys from many threads, a build that panicked), the answer is the reference's
//! to the bit. No adapter, no FFmpeg. Always run.

#![allow(dead_code, clippy::print_stderr)]

use std::collections::HashSet;
use std::sync::{Arc, Barrier, OnceLock};

use kerf_gpu::scaler::{scale_plane, vertical_start, Kernel};
use kerf_gpu::sws::{
    bicubic, bicubic_uncached, table, table_aligned, table_aligned_uncached, Filter, Scaler, ONE_HORIZONTAL, ONE_VERTICAL,
};

#[path = "checker_merge/sws_b1e41f04.rs"]
mod frozen;

const SCALERS: [Scaler; 2] = [Scaler::Bicubic, Scaler::Bilinear];
const ALIGNS: [usize; 3] = [1, 2, 4];
const ONES: [i32; 2] = [ONE_HORIZONTAL, ONE_VERTICAL];

type Key = (Scaler, usize, u32, u32, i32);

fn frozen_scaler(s: Scaler) -> frozen::Scaler {
    match s {
        Scaler::Bicubic => frozen::Scaler::Bicubic,
        Scaler::Bilinear => frozen::Scaler::Bilinear,
        other => unreachable!("{other:?} is not in the frozen builder (SCALERS lists the two it has)"),
    }
}

/// The table lane B's builder made before the memo, in this crate's type.
fn lane(k: Key) -> Option<Filter> {
    frozen::table_aligned(frozen_scaler(k.0), k.2, k.3, k.4, k.1).map(|f| Filter {
        size: f.size,
        pos: f.pos,
        coeff: f.coeff,
    })
}

fn ask(k: Key) -> Option<Filter> {
    table_aligned(k.0, k.2, k.3, k.4, k.1)
}

fn fnv(f: &Option<Filter>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    let mut eat = |v: u64| {
        for b in v.to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    match f {
        None => eat(u64::MAX),
        Some(f) => {
            eat(f.size as u64);
            eat(f.pos.len() as u64);
            f.pos.iter().for_each(|p| eat(*p as u32 as u64));
            f.coeff.iter().for_each(|c| eat(*c as u32 as u64));
        }
    }
    h
}

fn same(got: &Option<Filter>, want: &Option<Filter>, what: &str) {
    match (got, want) {
        (None, None) => {}
        (Some(g), Some(w)) => {
            assert_eq!(g.size, w.size, "{what}: taps per sample");
            assert_eq!(g.pos.len(), w.pos.len(), "{what}: output samples");
            assert_eq!(g.coeff.len(), w.coeff.len(), "{what}: weights");
            if let Some(i) = g.pos.iter().zip(&w.pos).position(|(a, b)| a != b) {
                panic!("{what}: window start of sample {i}: {} against {}", g.pos[i], w.pos[i]);
            }
            if let Some(i) = g.coeff.iter().zip(&w.coeff).position(|(a, b)| a != b) {
                panic!(
                    "{what}: weight {i} (sample {}, tap {}): {} against {}",
                    i / w.size,
                    i % w.size,
                    g.coeff[i],
                    w.coeff[i]
                );
            }
            assert_eq!(g.texels(), w.texels(), "{what}: the shader's table");
        }
        (g, w) => panic!(
            "{what}: {} against {}",
            g.as_ref().map_or("refused", |_| "a table"),
            w.as_ref().map_or("refused", |_| "a table")
        ),
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, self.below(i + 1));
        }
    }
}

/// Edge sides: one sample, odd and prime sizes, the pairs swscale takes for unscaled (`|inc - 0x10000| < 10` from a side
/// of 6554 up: one sample of difference is not a scale there), the chroma and luma of the usual pictures, ratios beyond
/// one pass (more than 255 taps) and the upscales whose tables are the biggest.
const SHAPES: &[(u32, u32)] = &[
    (1, 1),
    (2, 2),
    (3, 3),
    (101, 101),
    (640, 640),
    (1920, 1920),
    (4096, 4096),
    (6552, 6553),
    (6553, 6554),
    (6554, 6555),
    (7000, 7001),
    (7001, 7000),
    (10000, 10001),
    (10001, 10000),
    (6000, 6001),
    (6500, 6502),
    (65535, 65536),
    (1, 2),
    (2, 1),
    (1, 7),
    (7, 1),
    (1, 100),
    (100, 1),
    (2, 3),
    (3, 2),
    (3, 4),
    (5, 3),
    (7, 5),
    (7, 100),
    (31, 100),
    (100, 31),
    (201, 100),
    (203, 101),
    (333, 777),
    (641, 320),
    (1279, 639),
    (1921, 1081),
    (1280, 120),
    (1920, 1080),
    (3840, 2160),
    (640, 360),
    (640, 320),
    (1280, 720),
    (360, 640),
    (720, 1280),
    (320, 180),
    (180, 90),
    (160, 90),
    (960, 540),
    (1280, 5),
    (1280, 4),
    (1280, 1),
    (4000, 10),
    (2000, 3),
    (10000, 50),
    (100_000, 100),
    (100_000, 1),
    (8000, 8),
    (1, 30_000),
    (2, 50_000),
    (64, 100_000),
];

/// Tables of several MiB: kept apart from the corpus (their references are not held, only their hashes).
const HUGE: &[(u32, u32)] = &[
    (1001, 340_000),
    (1000, 450_000),
    (1, 1 << 20),
    (2, 1_100_000),
    (65535, 65536 * 4),
];

fn corpus_keys() -> Vec<Key> {
    let mut keys = Vec::new();
    for &(s, d) in SHAPES {
        for scaler in SCALERS {
            for align in ALIGNS {
                for one in ONES {
                    keys.push((scaler, align, s, d, one));
                }
            }
        }
    }
    keys
}

/// Odd values of the parts that the callers never pass, on a few shapes: another alignment is another key.
fn stray_keys() -> Vec<Key> {
    let mut keys = Vec::new();
    for &(s, d) in &[
        (1280u32, 120u32),
        (640, 360),
        (360, 640),
        (101, 101),
        (1, 100),
        (100_000, 100),
    ] {
        for scaler in SCALERS {
            for align in [0usize, 1, 2, 3, 4, 8, 16] {
                for one in [1, 2, 4095, ONE_VERTICAL, 4097, ONE_HORIZONTAL, 1 << 20] {
                    keys.push((scaler, align, s, d, one));
                }
            }
        }
    }
    keys
}

struct Reference {
    keys: Vec<Key>,
    tables: Vec<Option<Filter>>,
}

fn reference() -> &'static Reference {
    static R: OnceLock<Reference> = OnceLock::new();
    R.get_or_init(|| {
        let mut keys = corpus_keys();
        keys.extend(stray_keys());
        let tables = keys.iter().map(|k| lane(*k)).collect();
        Reference { keys, tables }
    })
}

#[test]
fn checker_merge_the_corpus_has_refused_unscaled_scaled_and_padded_tables() {
    let r = reference();
    let refused = r.tables.iter().filter(|t| t.is_none()).count();
    let unscaled = r
        .keys
        .iter()
        .zip(&r.tables)
        .filter(|(k, t)| k.2 == k.3 && t.as_ref().is_some_and(|f| f.size == 1))
        .count();
    let padded = r.tables.iter().flatten().filter(|f| f.size > 1 && f.size % 4 == 0).count();
    let scaled = r.tables.iter().flatten().filter(|f| f.size > 4).count();
    assert!(
        refused > 100 && unscaled > 40 && padded > 100 && scaled > 100,
        "{refused} refused, {unscaled} unscaled, {padded} padded, {scaled} scaled of {}",
        r.keys.len()
    );
}

#[test]
fn checker_merge_the_builder_the_merge_renamed_is_lane_bs_builder_to_the_bit() {
    let r = reference();
    for (k, want) in r.keys.iter().zip(&r.tables) {
        let got = table_aligned_uncached(k.0, k.2, k.3, k.4, k.1);
        same(&got, want, &format!("{:?} align {} {}->{} {}", k.0, k.1, k.2, k.3, k.4));
    }
    for k in HUGE {
        for scaler in SCALERS {
            for align in [1, 4] {
                let key = (scaler, align, k.0, k.1, ONE_HORIZONTAL);
                assert_eq!(
                    fnv(&table_aligned_uncached(scaler, k.0, k.1, ONE_HORIZONTAL, align)),
                    fnv(&lane(key)),
                    "{key:?}"
                );
            }
        }
    }
}

#[test]
fn checker_merge_a_table_from_the_memo_is_the_lane_build_in_every_order_and_twice() {
    let r = reference();
    let mut rng = Rng(0x4D45_5247_0001);
    let n = r.keys.len();
    let mut orders: Vec<Vec<usize>> = vec![(0..n).collect(), (0..n).rev().collect()];
    for _ in 0..3 {
        let mut o: Vec<usize> = (0..n).collect();
        rng.shuffle(&mut o);
        orders.push(o);
    }
    for (pass, order) in orders.iter().enumerate() {
        for &i in order {
            let k = r.keys[i];
            let what = format!("pass {pass}: {:?} align {} {}->{} {}", k.0, k.1, k.2, k.3, k.4);
            same(&ask(k), &r.tables[i], &what);
            same(&ask(k), &r.tables[i], &format!("{what}, again"));
        }
    }
}

/// Every key the callers use, with the one field that the key must keep changed, asked in turn and again: each is its
/// own table whichever came first.
#[test]
fn checker_merge_each_field_of_the_key_picks_its_own_table() {
    let r = reference();
    let find = |k: Key| {
        let i = r.keys.iter().position(|x| *x == k).expect("a key of the corpus");
        &r.tables[i]
    };
    let (mut by_scaler, mut by_align, mut by_src, mut by_dst, mut by_one, mut by_swap) = (0, 0, 0, 0, 0, 0);
    for &(s, d) in SHAPES {
        for scaler in SCALERS {
            for align in ALIGNS {
                for one in ONES {
                    let base = (scaler, align, s, d, one);
                    let mut others = vec![];
                    let other_scaler = if scaler == Scaler::Bicubic {
                        Scaler::Bilinear
                    } else {
                        Scaler::Bicubic
                    };
                    others.push((0, (other_scaler, align, s, d, one)));
                    for a in ALIGNS.iter().filter(|a| **a != align) {
                        others.push((1, (scaler, *a, s, d, one)));
                    }
                    let other_one = if one == ONE_HORIZONTAL { ONE_VERTICAL } else { ONE_HORIZONTAL };
                    others.push((4, (scaler, align, s, d, other_one)));
                    if SHAPES.contains(&(d, s)) {
                        others.push((5, (scaler, align, d, s, one)));
                    }
                    if let Some(&(s2, _)) = SHAPES.iter().find(|p| p.1 == d && p.0 != s) {
                        others.push((2, (scaler, align, s2, d, one)));
                    }
                    if let Some(&(_, d2)) = SHAPES.iter().find(|p| p.0 == s && p.1 != d) {
                        others.push((3, (scaler, align, s, d2, one)));
                    }
                    for (field, other) in others {
                        let (a, b) = (find(base), find(other));
                        if a != b {
                            match field {
                                0 => by_scaler += 1,
                                1 => by_align += 1,
                                2 => by_src += 1,
                                3 => by_dst += 1,
                                4 => by_one += 1,
                                _ => by_swap += 1,
                            }
                        }
                        for _ in 0..2 {
                            same(&ask(base), a, &format!("{base:?} after {other:?}"));
                            same(&ask(other), b, &format!("{other:?} after {base:?}"));
                        }
                    }
                }
            }
        }
    }
    assert!(
        by_scaler > 100 && by_align > 100 && by_src > 100 && by_dst > 100 && by_one > 100 && by_swap > 100,
        "scaler {by_scaler}, align {by_align}, src {by_src}, dst {by_dst}, one {by_one}, swapped sides {by_swap}: each must tell \
         tables apart here or this proves nothing"
    );
}

#[test]
fn checker_merge_the_strays_a_caller_never_passes_are_their_own_keys_too() {
    let strays = stray_keys();
    let mut distinct = HashSet::new();
    for pass in 0..2 {
        for k in &strays {
            let want = lane(*k);
            distinct.insert(fnv(&want));
            same(&ask(*k), &want, &format!("pass {pass}: {k:?}"));
        }
    }
    assert!(
        distinct.len() > 150,
        "{} distinct tables of {} keys",
        distinct.len(),
        strays.len()
    );
}

/// `filterAlign` 2 becomes 1 for an axis that is not scaled, and 4 does not: the memo keeps the alignment in the key and
/// so keeps them apart whichever is asked first.
#[test]
fn checker_merge_unscaled_axes_keep_one_tap_at_alignment_two_and_four_at_four() {
    for scaler in SCALERS {
        for n in [1u32, 2, 3, 5, 100, 101, 640, 1920, 4096, 6554, 65535] {
            for one in ONES {
                for order in [[1usize, 2, 4], [4, 2, 1], [2, 4, 1], [2, 1, 4]] {
                    let mut got = Vec::new();
                    for a in order {
                        got.push((a, table_aligned(scaler, n, n, one, a).expect("an unscaled table")));
                    }
                    let by = |a: usize| &got.iter().find(|g| g.0 == a).unwrap().1;
                    let what = format!("{scaler:?} {n}->{n} {one} order {order:?}");
                    assert_eq!(by(1).size, 1, "{what}");
                    assert_eq!(by(2).size, 1, "{what}: alignment 2 on an unscaled axis");
                    assert_eq!(by(2), by(1), "{what}");
                    assert_eq!(by(4).size, 4, "{what}: alignment 4 pads even an unscaled axis");
                    same(
                        &Some(by(2).clone()),
                        &table_aligned_uncached(scaler, n, n, one, 2),
                        &format!("{what}: 2"),
                    );
                    same(
                        &Some(by(4).clone()),
                        &table_aligned_uncached(scaler, n, n, one, 4),
                        &format!("{what}: 4"),
                    );
                }
            }
        }
    }
}

#[test]
fn checker_merge_refused_ratios_stay_refused_and_zero_sides_are_refused() {
    let refused = [
        (1280u32, 1u32),
        (4000, 10),
        (100_000, 100),
        (100_000, 1),
        (2000, 3),
        (10000, 50),
    ];
    for _ in 0..3 {
        for &(s, d) in &refused {
            for scaler in SCALERS {
                for align in ALIGNS {
                    for one in ONES {
                        let key = (scaler, align, s, d, one);
                        assert!(lane(key).is_none(), "{key:?} is not a refused ratio");
                        assert!(ask(key).is_none(), "{key:?}: the memo answered a refused ratio with a table");
                        assert!(table_aligned_uncached(scaler, s, d, one, align).is_none());
                    }
                }
            }
        }
        for (s, d) in [(0u32, 5u32), (5, 0), (0, 0)] {
            for scaler in SCALERS {
                for one in ONES {
                    assert!(table_aligned(scaler, s, d, one, 1).is_none());
                    assert!(table_aligned(scaler, s, d, one, 4).is_none());
                    assert!(table(scaler, s, d, one).is_none());
                }
            }
        }
    }
    assert!(bicubic(0, 4, ONE_VERTICAL).is_none() && bicubic(1280, 1, ONE_VERTICAL).is_none());
}

/// The convenience entries are the memoised function at alignment 1.
#[test]
fn checker_merge_bicubic_and_table_are_table_aligned_at_alignment_one() {
    for &(s, d) in SHAPES {
        for one in ONES {
            let want = lane((Scaler::Bicubic, 1, s, d, one));
            for _ in 0..2 {
                same(&bicubic(s, d, one), &want, &format!("bicubic {s}->{d} {one}"));
                same(
                    &bicubic_uncached(s, d, one),
                    &want,
                    &format!("bicubic_uncached {s}->{d} {one}"),
                );
                same(&table(Scaler::Bicubic, s, d, one), &want, &format!("table {s}->{d} {one}"));
                same(
                    &table_aligned(Scaler::Bicubic, s, d, one, 1),
                    &want,
                    &format!("aligned 1 {s}->{d} {one}"),
                );
            }
            let want = lane((Scaler::Bilinear, 1, s, d, one));
            same(
                &table(Scaler::Bilinear, s, d, one),
                &want,
                &format!("table bilinear {s}->{d} {one}"),
            );
        }
    }
}

/// Far more distinct keys than the memo holds, tables of several MiB between them (the byte cap flushes the small
/// ones), the same hot key between all of them: each answer is the reference's.
#[test]
fn checker_merge_eviction_churn_by_count_and_by_bytes_changes_no_answer() {
    let r = reference();
    let big: Vec<(Key, Option<Filter>)> = HUGE
        .iter()
        .take(4)
        .flat_map(|&(s, d)| {
            SCALERS.map(|sc| {
                let k = (sc, 4, s, d, ONE_HORIZONTAL);
                (k, lane(k))
            })
        })
        .collect();
    let mut rng = Rng(0x4D45_5247_0002);
    let hot = r
        .keys
        .iter()
        .position(|k| *k == (Scaler::Bilinear, 4, 640, 360, ONE_VERTICAL))
        .unwrap();
    for round in 0..3 {
        let mut order: Vec<usize> = (0..r.keys.len()).collect();
        rng.shuffle(&mut order);
        for (n, &i) in order.iter().enumerate() {
            let k = r.keys[i];
            same(&ask(k), &r.tables[i], &format!("round {round}: {k:?}"));
            if n % 11 == 0 {
                same(&ask(r.keys[hot]), &r.tables[hot], "the hot key between the others");
            }
            if n % 400 == 0 {
                let (bk, bw) = &big[(n / 400 + round) % big.len()];
                assert!(bw.is_some(), "{bk:?}");
                assert_eq!(fnv(&ask(*bk)), fnv(bw), "round {round}: big {bk:?}");
            }
        }
    }
}

#[test]
fn checker_merge_a_table_over_the_entry_cap_is_the_reference_every_time() {
    for &(s, d) in HUGE {
        for scaler in SCALERS {
            for align in [1, 4] {
                let key = (scaler, align, s, d, ONE_HORIZONTAL);
                let want = fnv(&lane(key));
                for n in 0..2 {
                    assert_eq!(fnv(&ask(key)), want, "{key:?} ask {n}");
                }
            }
        }
    }
}

#[test]
fn checker_merge_threads_on_mixed_keys_all_get_the_reference() {
    let r = reference();
    std::thread::scope(|scope| {
        for t in 0..8u64 {
            scope.spawn(move || {
                let mut rng = Rng(0x4D45_5247_1000 + t);
                for _ in 0..4000 {
                    let i = rng.below(r.keys.len());
                    let k = r.keys[i];
                    assert_eq!(&ask(k), &r.tables[i], "thread {t}: {k:?}");
                }
            });
        }
    });
}

#[test]
fn checker_merge_threads_walking_the_same_keys_in_opposite_orders_get_the_reference() {
    let r = reference();
    let go = Barrier::new(4);
    std::thread::scope(|scope| {
        for t in 0..4usize {
            let go = &go;
            scope.spawn(move || {
                go.wait();
                let order: Vec<usize> = if t % 2 == 0 {
                    (0..r.keys.len()).collect()
                } else {
                    (0..r.keys.len()).rev().collect()
                };
                for i in order {
                    assert_eq!(&ask(r.keys[i]), &r.tables[i], "thread {t}: {:?}", r.keys[i]);
                }
            });
        }
    });
}

/// Threads that all miss one cold key at once, for every kind of key (a scaler, an alignment, an unscaled axis, a
/// refused ratio, a table over the entry cap).
#[test]
fn checker_merge_threads_missing_one_cold_key_at_once_all_get_the_reference() {
    let mut keys: Vec<Key> = Vec::new();
    for (i, scaler) in SCALERS.iter().enumerate() {
        for (j, align) in ALIGNS.iter().enumerate() {
            keys.push((
                *scaler,
                *align,
                9100 + 17 * i as u32 + j as u32,
                4000 + 3 * j as u32,
                ONE_HORIZONTAL,
            ));
            keys.push((*scaler, *align, 5000 + 7 * j as u32, 5000 + 7 * j as u32, ONE_VERTICAL));
            keys.push((*scaler, *align, 70_000 + i as u32, 100 + j as u32, ONE_HORIZONTAL));
        }
    }
    keys.push((Scaler::Bicubic, 4, 1000, 450_000, ONE_HORIZONTAL));
    keys.push((Scaler::Bilinear, 2, 1001, 340_000, ONE_VERTICAL));
    for (round, key) in keys.iter().enumerate() {
        let want = lane(*key);
        let go = Barrier::new(10);
        let (go, want) = (&go, &want);
        std::thread::scope(|scope| {
            for t in 0..10 {
                scope.spawn(move || {
                    go.wait();
                    let got = ask(*key);
                    assert_eq!(fnv(&got), fnv(want), "round {round}, thread {t}: {key:?}");
                    assert_eq!(fnv(&ask(*key)), fnv(want), "round {round}, thread {t}: {key:?}, again");
                });
            }
        });
    }
}

#[test]
fn checker_merge_a_table_handed_out_is_the_callers_own() {
    let key = (Scaler::Bicubic, 4, 1280, 120, ONE_HORIZONTAL);
    let want = lane(key);
    let mut a = ask(key).unwrap();
    let mut b = ask(key).unwrap();
    for v in a.pos.iter_mut().chain(a.coeff.iter_mut()) {
        *v = -777;
    }
    a.size = 99;
    b.coeff.clear();
    b.pos.truncate(1);
    same(&ask(key), &want, "after two callers wrecked their copies");
    let mut c = ask(key).unwrap();
    c.coeff[0] += 1;
    same(&ask(key), &want, "after a third changed one weight");
    std::thread::scope(|scope| {
        for t in 0..6 {
            let want = &want;
            scope.spawn(move || {
                for _ in 0..50 {
                    let mut f = ask(key).unwrap();
                    f.coeff.iter_mut().for_each(|c| *c ^= 0x55);
                    f.pos.iter_mut().for_each(|p| *p ^= 0x2A);
                    same(&ask(key), want, &format!("thread {t}"));
                }
            });
        }
    });
}

/// A build that panics (a precision of 0 divides by zero) is not a refusal and does not poison the memo.
#[test]
fn checker_merge_a_build_that_panics_is_not_remembered_and_does_not_break_the_memo() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut panicked = 0;
    for _ in 0..3 {
        for scaler in SCALERS {
            let r = std::panic::catch_unwind(|| table_aligned(scaler, 100, 50, 0, 1));
            panicked += usize::from(r.is_err());
            let r = std::panic::catch_unwind(|| table_aligned_uncached(scaler, 100, 50, 0, 1));
            assert!(r.is_err(), "the uncached builder is expected to divide by zero here");
        }
    }
    std::panic::set_hook(hook);
    assert_eq!(
        panicked, 6,
        "a precision of 0 must panic the same way through the memo, every time"
    );
    let r = reference();
    for (k, want) in r.keys.iter().zip(&r.tables).step_by(7) {
        same(&ask(*k), want, &format!("after the panics: {k:?}"));
    }
}

type ScaleCase = (
    Vec<u8>,
    (usize, usize),
    (usize, usize),
    Scaler,
    Kernel,
    usize,
    Option<Vec<u8>>,
);

fn plane(w: usize, h: usize, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    (0..w * h).map(|_| (rng.next() >> 33) as u8).collect()
}

/// `scaler::scale_plane` redone with lane B's tables: the same arithmetic, the tables from the frozen builder.
fn scale_with_lane(
    src: &[u8],
    (sw, sh): (usize, usize),
    (dw, dh): (usize, usize),
    scaler: Scaler,
    kernel: Kernel,
    c_rows: usize,
) -> Option<Vec<u8>> {
    let (align_h, align_v) = kernel.alignment();
    let hf = lane((scaler, align_h, sw as u32, dw as u32, ONE_HORIZONTAL))?;
    let vf = lane((scaler, align_v, sh as u32, dh as u32, ONE_VERTICAL))?;
    let mut mid = vec![0i32; dw * sh];
    for y in 0..sh {
        for x in 0..dw {
            let mut acc = 0i32;
            for j in 0..hf.size {
                let col = (hf.pos[x] as usize + j).min(sw - 1);
                acc += hf.coeff[x * hf.size + j] * i32::from(src[y * sw + col]);
            }
            mid[y * dw + x] = (acc >> 7).min(32767);
        }
    }
    let mut out = vec![0u8; dw * dh];
    for i in 0..dh {
        let simd = kernel.simd_vertical() && vf.size > 1 && i + c_rows < dh;
        for x in 0..dw {
            let mut taps = Vec::new();
            for j in 0..vf.size {
                let row = (vf.pos[i] as usize + j).min(sh - 1);
                taps.push((vf.coeff[i * vf.size + j], mid[row * dw + x]));
            }
            out[i * dw + x] = if simd {
                let acc: i32 = vertical_start(vf.size) + taps.iter().map(|(c, s)| (c * s) >> 16).sum::<i32>();
                (acc >> 3).clamp(0, 255) as u8
            } else {
                let acc: i64 = (64 << 12) + taps.iter().map(|(c, s)| i64::from(*c) * i64::from(*s)).sum::<i64>();
                (acc >> 19).clamp(0, 255) as u8
            };
        }
    }
    Some(out)
}

/// The caller in `scaler.rs`: every kernel and scaler on luma and chroma geometries, the plane it scales equal to the one
/// scaled with the frozen tables, cold, held, after a churn of other keys and from several threads.
#[test]
fn checker_merge_scale_plane_through_the_memo_is_scale_plane_through_the_lane_tables() {
    let geometries: [((usize, usize), (usize, usize)); 9] = [
        ((64, 36), (32, 18)),
        ((64, 36), (160, 90)),
        ((33, 17), (16, 8)),
        ((50, 30), (50, 30)),
        ((50, 30), (50, 15)),
        ((50, 30), (25, 30)),
        ((17, 9), (40, 23)),
        ((96, 54), (33, 55)),
        ((1, 1), (7, 5)),
    ];
    let mut rng = Rng(0x4D45_5247_0003);
    let mut cases = Vec::new();
    for (gi, (src, dst)) in geometries.iter().enumerate() {
        let pic = plane(src.0, src.1, 77 + gi as u64);
        for kernel in [Kernel::C, Kernel::Aligned, Kernel::X86] {
            for scaler in SCALERS {
                for c_rows in [1usize, 2] {
                    let want = scale_with_lane(&pic, *src, *dst, scaler, kernel, c_rows);
                    cases.push((pic.clone(), *src, *dst, scaler, kernel, c_rows, want));
                }
            }
        }
    }
    assert!(cases.iter().any(|c| c.6.is_some()));
    let check = |label: &str, c: &ScaleCase| {
        let got = scale_plane(&c.0, c.1, c.2, c.3, c.4, c.5);
        assert_eq!(got, c.6, "{label}: {:?} {:?} {:?}->{:?} c_rows {}", c.3, c.4, c.1, c.2, c.5);
    };
    for c in &cases {
        check("cold", c);
    }
    for c in &cases {
        check("held", c);
    }
    for i in 0..200 {
        let _ = table_aligned(Scaler::Bicubic, 2000 + i, 1000 + (i % 7), ONE_VERTICAL, 1 + (i as usize % 4));
    }
    for c in &cases {
        check("after a churn", c);
    }
    let cases = Arc::new(cases);
    std::thread::scope(|scope| {
        for t in 0..4u64 {
            let cases = cases.clone();
            scope.spawn(move || {
                let mut rng = Rng(0x4D45_5247_2000 + t);
                for _ in 0..300 {
                    let c = &cases[rng.below(cases.len())];
                    let _ = table_aligned(SCALERS[rng.below(2)], 3000 + rng.below(500) as u32, 700, ONE_HORIZONTAL, 4);
                    let got = scale_plane(&c.0, c.1, c.2, c.3, c.4, c.5);
                    assert_eq!(got, c.6, "thread {t}");
                }
            });
        }
    });
    let _ = rng.next();
}

/// A miss builds outside the lock: while one thread builds a table that takes a while, another thread's hits are not
/// held up behind it.
#[test]
fn checker_merge_a_hit_is_not_held_up_by_a_build_in_progress() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    let hot = (Scaler::Bicubic, 4, 640, 360, ONE_HORIZONTAL);
    let want = lane(hot);
    same(&ask(hot), &want, "the hot key, made");
    let slow = (Scaler::Bilinear, 1, 3, 2_000_000, ONE_VERTICAL);
    let t0 = Instant::now();
    let reference = fnv(&table_aligned_uncached(slow.0, slow.2, slow.3, slow.4, slow.1));
    let build = t0.elapsed();
    assert!(
        build.as_millis() >= 100,
        "a build of {build:?} is too quick to tell a held lock from none"
    );

    let started = Barrier::new(2);
    let done = AtomicBool::new(false);
    let (started, done) = (&started, &done);
    std::thread::scope(|scope| {
        let builder = scope.spawn(move || {
            started.wait();
            let got = ask(slow);
            done.store(true, Ordering::SeqCst);
            fnv(&got)
        });
        started.wait();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let t = Instant::now();
        for _ in 0..200 {
            assert_eq!(&ask(hot), &want);
        }
        let hits = t.elapsed();
        let finished = done.load(Ordering::SeqCst);
        assert!(
            !finished && hits < build / 3,
            "200 hits took {hits:?} beside a build of {build:?} (the build had finished: {finished}): the lock is held while a table is built"
        );
        assert_eq!(builder.join().unwrap(), reference);
    });
}
