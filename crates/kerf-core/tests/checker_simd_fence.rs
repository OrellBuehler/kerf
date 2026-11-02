//! `checker/simd/…` under an electric-fence allocator: every heap allocation of this binary is its own run of
//! pages with a `PROT_NONE` page on each side, and its data is put flush against the far page (a read or write one
//! byte past the end is a SIGSEGV) or the near one (one byte before the start is). The kernels' raw-pointer loads
//! and stores (`mask_plane/x86.rs`) and the scans' loads are run on exact-size allocations on every path.

#![cfg(target_os = "linux")]

mod checker_simd_cases;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use checker_simd_cases as c;

const PAGE: usize = 4096;
const REGION: usize = 1 << 38;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static FRONT: Cell<bool> = const { Cell::new(false) };
}

static BASE: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// Past this many live fenced blocks the kernel's map count (65 530) is near: System's blocks from there on.
const MAX_LIVE: usize = 12_000;

/// The reservation fenced blocks are carved from, never reused: one `PROT_NONE` mapping made on first use.
fn region() -> usize {
    let have = BASE.load(Ordering::Acquire);
    if have != 0 {
        return have;
    }
    // SAFETY: an anonymous reservation, nothing mapped there yet.
    let made = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            REGION,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    assert!(made != libc::MAP_FAILED, "the reservation");
    match BASE.compare_exchange(0, made as usize, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => made as usize,
        Err(other) => {
            // SAFETY: the mapping just made, unused.
            unsafe { libc::munmap(made, REGION) };
            other
        }
    }
}

fn fenced(p: *mut u8) -> bool {
    let base = BASE.load(Ordering::Acquire);
    base != 0 && (p as usize).wrapping_sub(base) < REGION
}

struct Fence;

// SAFETY: a block is either System's or a run of pages of the reservation (a page of `PROT_NONE` before and after
// it, aligned as asked) that is made inaccessible again when it is freed.
unsafe impl GlobalAlloc for Fence {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if !ACTIVE.with(Cell::get) || l.align() > PAGE || LIVE.load(Ordering::Relaxed) >= MAX_LIVE {
            // SAFETY: as System's.
            return unsafe { System.alloc(l) };
        }
        let data = l.size().max(1).div_ceil(PAGE) * PAGE;
        let need = data + 2 * PAGE;
        let off = NEXT.fetch_add(need, Ordering::Relaxed);
        if off + need > REGION {
            // SAFETY: as System's.
            return unsafe { System.alloc(l) };
        }
        LIVE.fetch_add(1, Ordering::Relaxed);
        let block = region() + off;
        let start = block + PAGE;
        // SAFETY: the data pages of a block of the reservation.
        if unsafe { libc::mprotect(start as *mut _, data, libc::PROT_READ | libc::PROT_WRITE) } != 0 {
            // SAFETY: as above.
            unsafe { libc::abort() };
        }
        let at = if FRONT.with(Cell::get) {
            start
        } else {
            (start + data - l.size()) & !(l.align() - 1)
        };
        at as *mut u8
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if !fenced(p) {
            // SAFETY: not ours: System's.
            return unsafe { System.dealloc(p, l) };
        }
        LIVE.fetch_sub(1, Ordering::Relaxed);
        let data = l.size().max(1).div_ceil(PAGE) * PAGE;
        let start = p as usize & !(PAGE - 1);
        // SAFETY: the data pages `alloc` opened for this block: closed again and given back.
        unsafe {
            libc::mprotect(start as *mut _, data, libc::PROT_NONE);
            libc::madvise(start as *mut _, data, libc::MADV_DONTNEED);
        }
    }
}

#[global_allocator]
static FENCE: Fence = Fence;

/// `f` with every allocation of this thread fenced, once against the far guard page and once against the near.
fn both_ends(f: impl Fn()) {
    ACTIVE.with(|c| c.set(true));
    for front in [false, true] {
        FRONT.with(|c| c.set(front));
        f();
    }
    FRONT.with(|c| c.set(false));
    ACTIVE.with(|c| c.set(false));
}

#[test]
fn checker_simd_fence_the_allocator_faults_one_byte_past_the_end() {
    // The harness is only worth anything if a one-byte overrun is caught: run a child that reads past the end.
    let exe = std::env::current_exe().unwrap();
    for (name, signal) in [
        ("checker_simd_fence_child_reads_past_the_end", true),
        ("checker_simd_fence_child_reads_the_last_byte", false),
    ] {
        let out = std::process::Command::new(&exe)
            .args(["--exact", name, "--ignored", "--test-threads=1"])
            .output()
            .unwrap();
        assert_eq!(!out.status.success(), signal, "{name}: {:?}", out.status);
    }
}

#[test]
#[ignore = "run by the test above"]
fn checker_simd_fence_child_reads_past_the_end() {
    ACTIVE.with(|c| c.set(true));
    let v: Vec<u8> = std::iter::repeat_n(1u8, 64).collect();
    let p = v.as_ptr();
    // SAFETY: deliberately not: one byte past the end must fault.
    let b = unsafe { std::ptr::read_volatile(p.add(64)) };
    std::process::exit(i32::from(b));
}

#[test]
#[ignore = "run by the test above"]
fn checker_simd_fence_child_reads_the_last_byte() {
    ACTIVE.with(|c| c.set(true));
    let v: Vec<u8> = std::iter::repeat_n(1u8, 64).collect();
    let p = v.as_ptr();
    // SAFETY: in range.
    let b = unsafe { std::ptr::read_volatile(p.add(63)) };
    std::process::exit(i32::from(b) - 1);
}

#[test]
fn checker_simd_fence_scans() {
    both_ends(|| c::scan_every_length_every_position(150));
}

#[test]
fn checker_simd_fence_scans_random() {
    both_ends(|| c::scan_random(8_000, 300));
}

#[test]
fn checker_simd_fence_bounds() {
    both_ends(|| {
        c::bounds_single_pixels_and_pairs();
        c::bounds_patterns(false);
    });
}

#[test]
fn checker_simd_fence_mask_every_width() {
    both_ends(|| {
        c::mask_every_size();
        c::mask_every_feather_every_width();
    });
}

#[test]
fn checker_simd_fence_mask_random() {
    both_ends(|| {
        c::mask_random(300, 90);
        c::mask_centres_and_sizes(37, 11);
    });
}

#[test]
fn checker_simd_fence_mask_picture_sizes() {
    both_ends(|| c::mask_picture_sizes(&[(1920, 1080), (401, 7), (3, 600), (1, 4096), (4097, 3), (1023, 17)], 1));
}
