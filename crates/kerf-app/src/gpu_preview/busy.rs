//! What a test waits for before its process may exit: the threads of a preview that can hold the device.
//!
//! The priming thread holds a strong reference to the preview while it works and may be the one that
//! drops the last, and a session's prefetch thread holds the compositor and so the device. A process
//! that exits while a device is torn down on another thread crashes in the driver (lavapipe's shader
//! compiler), so the tests' turn ([`wait_idle`]) waits until none is left.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static WORKING: AtomicUsize = AtomicUsize::new(0);

/// A thread (or a spell of work) that may hold the device; it counts until dropped.
pub struct Busy;

impl Busy {
    pub fn enter() -> Self {
        WORKING.fetch_add(1, Ordering::SeqCst);
        Busy
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        WORKING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How many are counting now.
pub fn working() -> usize {
    WORKING.load(Ordering::SeqCst)
}

/// Wait, for `limit` at most, until none is.
pub fn wait_idle(limit: Duration) {
    let until = Instant::now() + limit;
    while working() > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
}
