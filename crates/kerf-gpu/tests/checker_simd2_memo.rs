//! `checker/simd2/…`: the swscale filter-table memo (`sws::bicubic`) held to the table built from scratch
//! (`sws::bicubic_uncached`), byte for byte, whatever the memo held when it was asked: more keys than it has
//! entries, tables over its entry and total byte caps, refused ratios, many threads on one key and on many.
//! No adapter, no FFmpeg. Always run.

use std::sync::{Arc, Barrier};

use kerf_gpu::sws::{bicubic, bicubic_uncached, Filter, ONE_HORIZONTAL, ONE_VERTICAL};

type Key = (u32, u32, i32);

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
}

const SIDES: [u32; 22] = [
    1, 2, 3, 5, 7, 16, 17, 31, 64, 100, 101, 203, 360, 480, 640, 641, 720, 960, 1080, 1280, 1920, 3840,
];

/// Every pair of sides, both precisions: 968 keys, with unscaled ones, up- and down-scales and ratios too far
/// for one pass.
fn corpus() -> Vec<(Key, Option<Filter>)> {
    let mut keys = Vec::new();
    for &s in &SIDES {
        for &d in &SIDES {
            for one in [ONE_HORIZONTAL, ONE_VERTICAL] {
                keys.push((s, d, one));
            }
        }
    }
    keys.into_iter().map(|k| (k, bicubic_uncached(k.0, k.1, k.2))).collect()
}

fn same(got: &Option<Filter>, want: &Option<Filter>, what: &str) {
    assert_eq!(got, want, "{what}");
    if let (Some(g), Some(w)) = (got, want) {
        assert_eq!(g.texels(), w.texels(), "{what}: the shader's table");
    }
}

#[test]
fn checker_simd2_the_corpus_has_refused_and_unscaled_and_scaled_ratios() {
    let all = corpus();
    let refused = all.iter().filter(|(_, f)| f.is_none()).count();
    let unscaled = all.iter().filter(|(k, f)| k.0 == k.1 && f.is_some()).count();
    let scaled = all.iter().filter(|(k, f)| k.0 != k.1 && f.is_some()).count();
    assert!(refused > 40 && unscaled > 20 && scaled > 400, "{refused} {unscaled} {scaled}");
}

#[test]
fn checker_simd2_a_hit_is_a_fresh_build_for_every_key_of_a_corpus_bigger_than_the_memo() {
    let all = corpus();
    let mut rng = Rng(0x5EED_0001);
    for pass in 0..3 {
        let mut order: Vec<usize> = (0..all.len()).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i + 1));
        }
        for i in order {
            let ((s, d, one), want) = &all[i];
            same(&bicubic(*s, *d, *one), want, &format!("pass {pass}: {s}->{d} {one}"));
        }
    }
}

#[test]
fn checker_simd2_eviction_churn_does_not_change_what_an_old_key_returns() {
    let all = corpus();
    for round in 0..4 {
        let hot = &all[(round * 37) % all.len()];
        for (i, ((s, d, one), want)) in all.iter().enumerate() {
            same(&bicubic(*s, *d, *one), want, &format!("round {round}: {s}->{d} {one}"));
            if i % 17 == 0 {
                same(
                    &bicubic(hot.0 .0, hot.0 .1, hot.0 .2),
                    &hot.1,
                    "the hot key between the others",
                );
            }
        }
    }
}

#[test]
fn checker_simd2_threads_on_mixed_keys_all_get_the_fresh_table() {
    let all = Arc::new(corpus());
    std::thread::scope(|scope| {
        for t in 0..8u64 {
            let all = all.clone();
            scope.spawn(move || {
                let mut rng = Rng(0xABCD_0000 + t);
                for _ in 0..2500 {
                    let ((s, d, one), want) = &all[rng.below(all.len())];
                    assert_eq!(&bicubic(*s, *d, *one), want, "thread {t}: {s}->{d} {one}");
                }
            });
        }
    });
}

#[test]
fn checker_simd2_threads_asking_for_one_cold_key_at_once_all_get_the_fresh_table() {
    for round in 0..8u32 {
        let key: Key = (1000 + round, 90_000 + 1000 * round, ONE_HORIZONTAL);
        let want = bicubic_uncached(key.0, key.1, key.2);
        assert!(want.is_some());
        let go = Barrier::new(12);
        std::thread::scope(|scope| {
            for _ in 0..12 {
                scope.spawn(|| {
                    go.wait();
                    let got = bicubic(key.0, key.1, key.2);
                    same(&got, &want, &format!("round {round}"));
                    same(&bicubic(key.0, key.1, key.2), &want, &format!("round {round}, again"));
                });
            }
        });
    }
}

/// Tables over the memo's 8 MiB entry cap (built, not kept), tables just under it (kept, and five of them are
/// over the 32 MiB total), and small ones between them.
#[test]
fn checker_simd2_tables_over_the_entry_cap_and_the_total_cap_are_the_fresh_table() {
    let big: Vec<Key> = std::iter::once((1000, 450_000, ONE_HORIZONTAL))
        .chain((0..6).map(|i| (1001 + i, 340_000, ONE_HORIZONTAL)))
        .collect();
    let want: Vec<_> = big.iter().map(|k| bicubic_uncached(k.0, k.1, k.2)).collect();
    let bytes = |f: &Option<Filter>| f.as_ref().map_or(0, |f| 4 * (f.pos.len() + f.coeff.len()));
    assert!(bytes(&want[0]) > 8 << 20, "{}", bytes(&want[0]));
    for w in &want[1..] {
        assert!(bytes(w) > 6 << 20 && bytes(w) <= 8 << 20, "{}", bytes(w));
    }
    let small = corpus();
    for round in 0..3 {
        for (k, w) in big.iter().zip(&want) {
            same(&bicubic(k.0, k.1, k.2), w, &format!("round {round}: big {k:?}"));
            same(&bicubic(k.0, k.1, k.2), w, &format!("round {round}: big {k:?} again"));
            let ((s, d, one), sw) = &small[(round * 131 + k.0 as usize) % small.len()];
            same(&bicubic(*s, *d, *one), sw, "a small one between");
        }
    }
}

#[test]
fn checker_simd2_refused_ratios_stay_refused_whatever_surrounds_them() {
    let all = corpus();
    let refused: Vec<&(Key, Option<Filter>)> = all.iter().filter(|(_, f)| f.is_none()).collect();
    assert!(refused.len() > 40);
    for round in 0..3 {
        for (n, ((s, d, one), _)) in refused.iter().enumerate() {
            assert!(bicubic(*s, *d, *one).is_none(), "round {round}: {s}->{d} {one}");
            let ((s2, d2, o2), w2) = &all[(n * 53 + round * 7) % all.len()];
            same(&bicubic(*s2, *d2, *o2), w2, "an accepted ratio beside it");
            assert!(bicubic(*s, *d, *one).is_none(), "round {round}: {s}->{d} {one}, again");
        }
    }
    for (s, d) in [(0, 0), (0, 5), (5, 0), (0, u32::MAX), (u32::MAX, 0)] {
        for one in [ONE_HORIZONTAL, ONE_VERTICAL] {
            assert!(bicubic(s, d, one).is_none() && bicubic(s, d, one).is_none());
        }
    }
}

/// The key is all three: a table for one precision is not the other's, `(a, b)` is not `(b, a)`, and
/// neighbouring sizes and precisions are their own.
#[test]
fn checker_simd2_each_part_of_the_key_picks_its_own_table() {
    let ones = [
        1 << 10,
        1 << 11,
        3 << 10,
        4095,
        ONE_VERTICAL,
        4097,
        1 << 13,
        ONE_HORIZONTAL,
        1 << 15,
        1 << 16,
    ];
    let mut differing = 0;
    for &(s, d) in &[
        (640u32, 360u32),
        (360, 640),
        (641, 360),
        (640, 361),
        (1920, 1080),
        (1080, 1920),
        (203, 101),
        (101, 203),
    ] {
        for one in ones {
            let want = bicubic_uncached(s, d, one);
            for _ in 0..2 {
                same(&bicubic(s, d, one), &want, &format!("{s}->{d} {one}"));
            }
        }
        let (h, v) = (bicubic_uncached(s, d, ONE_HORIZONTAL), bicubic_uncached(s, d, ONE_VERTICAL));
        if h != v {
            differing += 1;
        }
        for _ in 0..2 {
            same(&bicubic(s, d, ONE_HORIZONTAL), &h, "horizontal after vertical");
            same(&bicubic(s, d, ONE_VERTICAL), &v, "vertical after horizontal");
        }
    }
    assert_eq!(
        differing, 8,
        "the two precisions must give different tables here or this test proves nothing"
    );
    for &(s, d) in &[(640u32, 360u32), (1920, 1080)] {
        assert_ne!(bicubic_uncached(s, d, ONE_VERTICAL), bicubic_uncached(d, s, ONE_VERTICAL));
        same(&bicubic(d, s, ONE_VERTICAL), &bicubic_uncached(d, s, ONE_VERTICAL), "swapped");
        same(
            &bicubic(s, d, ONE_VERTICAL),
            &bicubic_uncached(s, d, ONE_VERTICAL),
            "as asked",
        );
    }
}

/// What a caller does to a returned table is the caller's: the next hit is the fresh table.
#[test]
fn checker_simd2_changing_a_returned_table_does_not_change_the_next_hit() {
    for (s, d, one) in [
        (640u32, 360u32, ONE_VERTICAL),
        (1920, 1080, ONE_HORIZONTAL),
        (7, 100, ONE_VERTICAL),
    ] {
        let want = bicubic_uncached(s, d, one);
        for _ in 0..3 {
            let mut got = bicubic(s, d, one).unwrap();
            got.coeff.fill(7);
            got.pos.clear();
            got.size = 99;
            same(&bicubic(s, d, one), &want, &format!("{s}->{d}"));
        }
    }
}
