//! `checker/simd2/…`: what the swscale memo holds, read off a counting allocator (live bytes, and the allocations
//! one thread makes in a call): 64 entries, 32 MiB in all, no table over 8 MiB, a refused ratio held for nothing, a
//! hit costing the copy of a table and not a build. One test in its own binary so nothing else allocates while it
//! counts.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};

use kerf_gpu::sws::{bicubic, bicubic_uncached, Filter, ONE_HORIZONTAL, ONE_VERTICAL};

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

fn live() -> isize {
    LIVE.load(Ordering::Relaxed)
}

/// The allocations this thread makes in `f`.
fn allocations<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = CALLS.with(Cell::get);
    let r = f();
    (r, CALLS.with(Cell::get) - before)
}

fn bytes(f: &Filter) -> isize {
    4 * (f.pos.len() + f.coeff.len()) as isize
}

#[test]
fn checker_simd2_the_memo_holds_what_it_says_it_holds() {
    const MIB: isize = 1 << 20;
    let base = live();

    // A refused ratio is held as refused: the second ask is a lookup, not a build.
    let refused = (100_000u32, 100u32, ONE_HORIZONTAL);
    assert!(bicubic_uncached(refused.0, refused.1, refused.2).is_none());
    let (first, built) = allocations(|| bicubic(refused.0, refused.1, refused.2));
    assert!(first.is_none() && built > 0, "{built}");
    let (second, held) = allocations(|| bicubic(refused.0, refused.1, refused.2));
    assert!(
        second.is_none() && held == 0,
        "a held refusal allocated {held} times (a build: {built})"
    );

    // A table is held: the second ask copies it (its two vectors), it does not build it.
    let small = (640u32, 360u32, ONE_VERTICAL);
    let (a, built) = allocations(|| bicubic(small.0, small.1, small.2));
    let (b, held) = allocations(|| bicubic(small.0, small.1, small.2));
    assert_eq!(a, b);
    assert!(built > held && held <= 3, "built with {built} allocations, held {held}");

    // Over 8 MiB: built every time, never kept.
    let huge = (1000u32, 450_000u32, ONE_HORIZONTAL);
    let mark = live();
    let (t, first) = allocations(|| bicubic(huge.0, huge.1, huge.2));
    let t = t.expect("a table");
    assert!(bytes(&t) > 8 * MIB, "{}", bytes(&t));
    drop(t);
    let (t, second) = allocations(|| bicubic(huge.0, huge.1, huge.2));
    drop(t.expect("a table"));
    assert!(
        second > 3,
        "a table over 8 MiB was served from the memo: {first} then {second} allocations"
    );
    assert!(live() - mark < MIB, "a table over 8 MiB is held: {} bytes", live() - mark);

    // Just under 8 MiB: kept (the second ask copies, and the memo holds about the table's size).
    let under = (1001u32, 340_000u32, ONE_HORIZONTAL);
    let mark = live();
    let t = bicubic(under.0, under.1, under.2).expect("a table");
    let size = bytes(&t);
    assert!(size > 6 * MIB && size <= 8 * MIB, "{size}");
    drop(t);
    let (t, again) = allocations(|| bicubic(under.0, under.1, under.2));
    drop(t);
    assert!(again <= 3, "{again}");
    let kept = live() - mark;
    assert!((kept - size).abs() < 64 << 10, "kept {kept} of a {size}-byte table");

    // The total cap: more of them than 32 MiB holds, and the memo stays within it.
    for i in 1..8 {
        drop(bicubic(1001 + i, 340_000, ONE_HORIZONTAL));
    }
    let held = live() - base;
    assert!(held <= 32 * MIB + (512 << 10), "the memo holds {held} bytes");
    assert!(held >= 3 * size, "the memo holds only {held} bytes of {size}-byte tables");

    // The entry cap: 200 tables of a few hundred KiB each (64 of them are far under the byte cap).
    let mut sizes = Vec::new();
    for i in 0..200u32 {
        let t = bicubic(2000 + i, 10_000, ONE_VERTICAL).expect("a table");
        sizes.push(bytes(&t));
    }
    let newest: isize = sizes[200 - 64..].iter().sum();
    let held = live() - base;
    assert!(
        (held - newest).abs() < 256 << 10,
        "the memo holds {held} bytes; the newest 64 tables are {newest} (the 65 newest {})",
        newest + sizes[200 - 65]
    );
    let (_, edge) = allocations(|| bicubic(2200 - 64, 10_000, ONE_VERTICAL));
    assert!(
        edge <= 3,
        "the 64th newest key took {edge} allocations: it must still be held"
    );
    let (_, hit) = allocations(|| bicubic(2199, 10_000, ONE_VERTICAL));
    assert!(hit <= 3, "the newest key took {hit} allocations");
    let (_, beyond) = allocations(|| bicubic(2200 - 65, 10_000, ONE_VERTICAL));
    assert!(beyond > 3, "the 65th newest key took {beyond} allocations: it must be out");

    // Threads that all miss one cold key at once keep one copy of its table, not one each.
    let (s, d) = (1500u32, 200_000u32);
    let mark = live();
    let go = std::sync::Barrier::new(12);
    let size = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..12)
            .map(|_| {
                scope.spawn(|| {
                    go.wait();
                    bytes(&bicubic(s, d, ONE_HORIZONTAL).expect("a table"))
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).max().unwrap()
    });
    assert!(size > 3 * MIB, "{size}");
    let kept = live() - mark;
    assert!(
        kept < 2 * size,
        "12 threads on one key left {kept} bytes held of a {size}-byte table"
    );

    // Refused ratios take entries too, for no bytes: the tables they push out are rebuilt, the refusals are held.
    for i in 0..70u32 {
        assert!(bicubic(100_000 + i, 100, ONE_HORIZONTAL).is_none());
    }
    assert!(live() - base < 64 << 10, "refusals hold {} bytes", live() - base);
    let (_, held) = allocations(|| bicubic(100_069, 100, ONE_HORIZONTAL));
    assert_eq!(held, 0);
    let (t, rebuilt) = allocations(|| bicubic(2199, 10_000, ONE_VERTICAL));
    assert!(
        t.is_some() && rebuilt > 3,
        "70 newer entries must have pushed it out of 64: {rebuilt}"
    );
}
