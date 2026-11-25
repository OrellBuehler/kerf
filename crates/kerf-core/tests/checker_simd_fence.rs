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
/// The widest reservation asked for first; a host that caps the address space (`ulimit -v`, a CI runner) gets the
/// widest one it allows, halving down to [`MIN_REGION`]. The suite opens about 6 GiB of it.
const MAX_REGION: usize = 1 << 38;
const MIN_REGION: usize = 1 << 33;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static FRONT: Cell<bool> = const { Cell::new(false) };
}

static BASE: AtomicUsize = AtomicUsize::new(0);
static SIZE: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
/// Past this many live fenced blocks the kernel's map count (65 530) is near: System's blocks from there on.
const MAX_LIVE: usize = 12_000;

/// Inside the allocator a panic cannot unwind: say why on stderr, then abort.
fn die(why: &str) -> ! {
    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let mut msg = [0u8; 160];
    let mut n = 0;
    for b in why.bytes().chain(*b" (errno ") {
        if n < msg.len() {
            msg[n] = b;
            n += 1;
        }
    }
    let mut digits = [0u8; 10];
    let mut d = 0;
    let mut e = err.unsigned_abs();
    loop {
        digits[d] = b'0' + (e % 10) as u8;
        d += 1;
        e /= 10;
        if e == 0 {
            break;
        }
    }
    for &b in digits[..d].iter().rev().chain(b")\n") {
        if n < msg.len() {
            msg[n] = b;
            n += 1;
        }
    }
    // SAFETY: a write of a stack buffer to stderr, then the abort.
    unsafe {
        libc::write(2, msg.as_ptr().cast(), n);
        libc::abort()
    }
}

/// The reservation fenced blocks are carved from, never reused: one `PROT_NONE` mapping made on first use.
fn region() -> (usize, usize) {
    let have = BASE.load(Ordering::Acquire);
    if have != 0 {
        while SIZE.load(Ordering::Acquire) == 0 {
            std::hint::spin_loop();
        }
        return (have, SIZE.load(Ordering::Acquire));
    }
    let mut size = MAX_REGION;
    let made = loop {
        // SAFETY: an anonymous reservation, nothing mapped there yet.
        let made = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if made != libc::MAP_FAILED {
            break made;
        }
        if size == MIN_REGION {
            die("checker_simd_fence: no PROT_NONE reservation of 8 GiB or more");
        }
        size /= 2;
    };
    match BASE.compare_exchange(0, made as usize, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            SIZE.store(size, Ordering::Release);
            (made as usize, size)
        }
        Err(other) => {
            // SAFETY: the mapping just made, unused.
            unsafe { libc::munmap(made, size) };
            while SIZE.load(Ordering::Acquire) == 0 {
                std::hint::spin_loop();
            }
            (other, SIZE.load(Ordering::Acquire))
        }
    }
}

fn fenced(p: *mut u8) -> bool {
    let base = BASE.load(Ordering::Acquire);
    base != 0 && (p as usize).wrapping_sub(base) < SIZE.load(Ordering::Acquire)
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
        let (base, size) = region();
        let off = NEXT.fetch_add(need, Ordering::Relaxed);
        if off + need > size {
            // SAFETY: as System's.
            return unsafe { System.alloc(l) };
        }
        LIVE.fetch_add(1, Ordering::Relaxed);
        let block = base + off;
        let start = block + PAGE;
        // SAFETY: the data pages of a block of the reservation.
        if unsafe { libc::mprotect(start as *mut _, data, libc::PROT_READ | libc::PROT_WRITE) } != 0 {
            die("checker_simd_fence: mprotect of a fenced block failed");
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
        // SAFETY: the data pages `alloc` opened for this block, replaced in place by a fresh `PROT_NONE`
        // reservation: a mapping that was writable keeps its commit charge until it is unmapped, so closing it
        // with `mprotect` alone would add up every page the suite ever opened against a strict overcommit limit.
        let made = unsafe {
            libc::mmap(
                start as *mut _,
                data,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        if made == libc::MAP_FAILED {
            die("checker_simd_fence: closing a fenced block failed");
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

/// Set in the children this binary runs: a plain `--ignored` run passes them by.
const CHILD: &str = "CHECKER_SIMD_FENCE_CHILD";

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
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert_eq!(!out.status.success(), signal, "{name}: {:?}", out.status);
    }
}

#[test]
#[ignore = "run by the test above"]
fn checker_simd_fence_child_reads_past_the_end() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
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
    if std::env::var_os(CHILD).is_none() {
        return;
    }
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

#[test]
fn checker_simd2_fence_rect_rows() {
    both_ends(|| {
        c::rect_random(300, 100);
        c::rect_one_pixel_and_huge_planes();
    });
}
