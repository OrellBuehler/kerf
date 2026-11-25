//! `checker/merge/…`: what the memo at `sws::table_aligned` holds, read off a counting allocator (live bytes, and the
//! allocations this thread makes in a call), for keys with a scaler and an alignment in them: a refusal held for nothing,
//! a hit costing the copy of a table and not a build, a table of exactly the entry cap kept and one byte-pair over it not,
//! four tables of exactly the total cap kept and the next pushing out the oldest, the 64-entry cap to the entry, a hot
//! key kept alive by its hits, and every part of the key told apart by whether a second ask is a hit. One test in its
//! own binary so nothing else allocates while it counts. No adapter, no FFmpeg. Always run.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};

use kerf_gpu::sws::{table_aligned, table_aligned_uncached, Filter, Scaler, ONE_HORIZONTAL, ONE_VERTICAL};

static LIVE: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: System's, with two counters beside it.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Ordering::Relaxed);
        CALLS.with(|c| c.set(c.get() + 1));
        // SAFETY: as System's.
        unsafe { System.alloc(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Ordering::Relaxed);
        // SAFETY: as System's.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

const MIB: isize = 1 << 20;

fn live() -> isize {
    LIVE.load(Ordering::Relaxed)
}

fn allocations<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = CALLS.with(Cell::get);
    let r = f();
    (r, CALLS.with(Cell::get) - before)
}

fn bytes(f: &Filter) -> isize {
    4 * (f.pos.len() + f.coeff.len()) as isize
}

type Key = (Scaler, usize, u32, u32, i32);

/// The allocations of asking the memo for `k` once.
fn ask(k: Key) -> (Option<Filter>, usize) {
    allocations(|| table_aligned(k.0, k.2, k.3, k.4, k.1))
}

/// The allocations of building `k` from scratch.
fn build(k: Key) -> (Option<Filter>, usize) {
    allocations(|| table_aligned_uncached(k.0, k.2, k.3, k.4, k.1))
}

/// A hit copies two vectors; a build takes dozens of allocations.
const HIT: usize = 3;

fn assert_hit(k: Key, what: &str) {
    let (t, n) = ask(k);
    assert!(
        n <= HIT,
        "{what}: {k:?} took {n} allocations: not held (a hit takes at most {HIT})"
    );
    drop(t);
}

fn assert_miss(k: Key, what: &str) {
    let (t, n) = ask(k);
    assert!(n > HIT, "{what}: {k:?} took {n} allocations: held (a miss builds)");
    drop(t);
}

/// 70 refused ratios: every entry the memo had is out, and the memo holds no bytes (a refusal is held for nothing).
fn flush() {
    for i in 0..70u32 {
        assert!(table_aligned(Scaler::Bicubic, 100_000 + i, 100, ONE_HORIZONTAL, 1).is_none());
    }
}

#[test]
fn checker_merge_the_memo_at_table_aligned_holds_what_it_says_it_holds() {
    let base = live();

    // A refusal is held as a refusal, for every scaler and alignment, and costs the next ask nothing.
    for scaler in [Scaler::Bicubic, Scaler::Bilinear] {
        for align in [1usize, 2, 4] {
            let k = (scaler, align, 100_000, 100, ONE_HORIZONTAL);
            let (t, built) = ask(k);
            assert!(t.is_none() && built > 0, "{k:?}: {built}");
            let (t, held) = ask(k);
            assert!(
                t.is_none() && held == 0,
                "a held refusal of {k:?} allocated {held} times (a build: {built})"
            );
        }
    }
    let zero_side = (Scaler::Bilinear, 4, 0, 50, ONE_VERTICAL);
    assert!(ask(zero_side).0.is_none());
    assert_eq!(ask(zero_side).1, 0, "a zero side is a refusal that is held too");

    // A small table: the second ask copies it and does not build it.
    let small = (Scaler::Bicubic, 4, 641, 320, ONE_HORIZONTAL);
    let (a, built) = ask(small);
    let (b, held) = ask(small);
    let (_, rebuilt) = build(small);
    assert_eq!(a, b);
    assert!(
        built >= rebuilt && held <= HIT && held < rebuilt,
        "built {built} (alone {rebuilt}), held {held}"
    );

    // Every part of the key is told apart: the same ask with one part changed is a miss once and a hit after, and the
    // first key is still held beside it.
    let variants: [(&str, Key); 6] = [
        ("scaler", (Scaler::Bilinear, 4, 641, 320, ONE_HORIZONTAL)),
        ("alignment", (Scaler::Bicubic, 2, 641, 320, ONE_HORIZONTAL)),
        ("source", (Scaler::Bicubic, 4, 642, 320, ONE_HORIZONTAL)),
        ("destination", (Scaler::Bicubic, 4, 641, 321, ONE_HORIZONTAL)),
        ("precision", (Scaler::Bicubic, 4, 641, 320, ONE_VERTICAL)),
        ("sides swapped", (Scaler::Bicubic, 4, 320, 641, ONE_HORIZONTAL)),
    ];
    for (part, k) in variants {
        assert_miss(k, &format!("another {part}, first ask"));
        assert_hit(k, &format!("another {part}, second ask"));
        assert_hit(small, &format!("the first key beside another {part}"));
    }
    for align in [0usize, 1, 3, 8] {
        let k = (Scaler::Bicubic, align, 641, 320, ONE_HORIZONTAL);
        assert_miss(k, &format!("alignment {align}"));
        assert_hit(k, &format!("alignment {align}, again"));
    }

    // The entry cap: a table of exactly 8 MiB is kept, one of 8 MiB and a pair of words is not (and is not copied).
    flush();
    let at_cap = (Scaler::Bicubic, 1, 1, 1 << 20, ONE_HORIZONTAL);
    let (t, alone) = build(at_cap);
    let t = t.expect("a table");
    assert_eq!(
        bytes(&t),
        8 * MIB,
        "the exact-cap table is not 8 MiB: this check needs another shape"
    );
    drop(t);
    let mark = live();
    let (t, first) = ask(at_cap);
    drop(t);
    assert!(
        first > alone,
        "an 8 MiB table took {first} allocations to ask for, {alone} to build: it must also be kept"
    );
    assert!(
        live() - mark > 7 * MIB,
        "a table of exactly 8 MiB is not held: {} bytes",
        live() - mark
    );
    assert_hit(at_cap, "a table of exactly 8 MiB");
    flush();
    assert!(live() - base < MIB, "the flush left {} bytes", live() - base);

    let over_cap = (Scaler::Bicubic, 1, 1, (1 << 20) + 1, ONE_HORIZONTAL);
    let (t, alone) = build(over_cap);
    let t = t.expect("a table");
    assert_eq!(bytes(&t), 8 * MIB + 8);
    drop(t);
    let mark = live();
    let (t, first) = ask(over_cap);
    drop(t);
    assert_eq!(
        first, alone,
        "a table over 8 MiB took {first} allocations to ask for and {alone} to build: it was copied"
    );
    assert!(live() - mark < MIB, "a table over 8 MiB is held: {} bytes", live() - mark);
    assert_miss(over_cap, "a table over 8 MiB, again");

    // The same at another scaler and alignment: the cap is on bytes, not on a shape.
    let wide = (Scaler::Bilinear, 4, 4096, 140_000, ONE_VERTICAL);
    let (t, _) = build(wide);
    let wide_bytes = bytes(&t.expect("a table"));
    assert!(wide_bytes > 2 * MIB && wide_bytes <= 8 * MIB, "{wide_bytes}");
    flush();
    drop(ask(wide).0);
    assert_hit(wide, "a Bilinear table of a few MiB at alignment 4");

    // The total cap: four tables of exactly 8 MiB are 32 MiB, the cap, and all stay; the next ask pushes out the oldest
    // and only that one.
    flush();
    let four: [Key; 4] = [
        (Scaler::Bicubic, 1, 1, 1 << 20, ONE_HORIZONTAL),
        (Scaler::Bicubic, 1, 2, 1 << 20, ONE_HORIZONTAL),
        (Scaler::Bicubic, 1, 3, 1 << 20, ONE_HORIZONTAL),
        (Scaler::Bilinear, 1, 1, 1 << 20, ONE_HORIZONTAL),
    ];
    for k in four {
        let (t, _) = build(k);
        assert_eq!(bytes(&t.expect("a table")), 8 * MIB, "{k:?}");
    }
    let mark = live();
    for k in four {
        drop(ask(k).0);
    }
    let held = live() - mark;
    assert!(
        (held - 32 * MIB).abs() < MIB / 2,
        "four tables of exactly 8 MiB: the memo holds {held} bytes, not 32 MiB"
    );
    let tiny = (Scaler::Bicubic, 1, 640, 360, ONE_VERTICAL);
    assert_miss(tiny, "a small table after four of 8 MiB");
    assert_hit(four[3], "the newest of the four");
    assert_hit(four[2], "the third");
    assert_hit(four[1], "the second");
    assert_miss(four[0], "the oldest of the four, pushed out by the next ask");
    let held = live() - mark;
    assert!(held <= 32 * MIB + MIB / 2, "the memo holds {held} bytes over a 32 MiB cap");

    // The entry cap, to the entry: 200 keys of every scaler, alignment and precision.
    flush();
    let keys: Vec<Key> = (0..200u32)
        .map(|i| {
            (
                if i % 2 == 0 { Scaler::Bicubic } else { Scaler::Bilinear },
                [1usize, 2, 4][(i % 3) as usize],
                300 + i,
                200 + i % 5,
                if i % 4 < 2 { ONE_HORIZONTAL } else { ONE_VERTICAL },
            )
        })
        .collect();
    for k in &keys {
        drop(ask(*k).0);
    }
    assert_hit(keys[199], "the newest of 200");
    assert_hit(keys[200 - 64], "the 64th newest");
    assert_miss(keys[200 - 65], "the 65th newest");

    // A key that is asked again stays: 60 keys after it, a hit on it, 60 more, and it is still there.
    flush();
    let hot = (Scaler::Bilinear, 2, 777, 333, ONE_VERTICAL);
    drop(ask(hot).0);
    for k in &keys[..60] {
        drop(ask(*k).0);
    }
    assert_hit(hot, "the hot key after 60 others");
    for k in &keys[60..120] {
        drop(ask(*k).0);
    }
    assert_hit(hot, "the hot key after 120 others, 60 of them since its last hit");
    for k in &keys[120..190] {
        drop(ask(*k).0);
    }
    assert_miss(hot, "the hot key after 70 others since its last hit");

    // Refusals hold no bytes and are held: 70 of them leave the memo at its starting size.
    flush();
    assert!(live() - base < MIB, "refusals hold {} bytes", live() - base);
    let (t, held) = allocations(|| table_aligned(Scaler::Bicubic, 100_069, 100, ONE_HORIZONTAL, 1));
    assert!(t.is_none() && held == 0, "{held}");
    assert_miss(keys[199], "a table pushed out by 70 refusals");

    // Threads that all miss one cold key at once keep one copy of its table, not one each, for several keys at once.
    flush();
    let cold: [Key; 3] = [
        (Scaler::Bicubic, 4, 1500, 200_000, ONE_HORIZONTAL),
        (Scaler::Bilinear, 4, 1500, 200_000, ONE_HORIZONTAL),
        (Scaler::Bicubic, 1, 1500, 200_000, ONE_HORIZONTAL),
    ];
    let mark = live();
    let go = std::sync::Barrier::new(12);
    let sizes = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..12)
            .map(|t| {
                let (go, k) = (&go, cold[t % 3]);
                scope.spawn(move || {
                    go.wait();
                    bytes(&ask(k).0.expect("a table"))
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect::<Vec<_>>()
    });
    let total: isize = [sizes[0], sizes[1], sizes[2]].iter().sum();
    assert!(total > 6 * MIB, "{total}");
    let kept = live() - mark;
    assert!(
        kept < total + total / 3,
        "12 threads on three keys left {kept} bytes held of {total} bytes of tables"
    );
    for k in cold {
        assert_hit(k, "after the threads");
    }
}
