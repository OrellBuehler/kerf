//! The pictures FFmpeg makes of a still for the compositor ([`crate::source::StillCopy`]: a moving still's scaled copy, the fit picture ahead
//! of a placement), kept — one cache for the process, apart from the frame cache, so that a copy is made **once** and nothing else is pushed out
//! to make room for it.
//!
//! * **Bounded.** [`CAP_BYTES`] of pictures, the least recently used going first (a 1080p slideshow's copy is 4 to 5 MB, a 4K canvas's 14).
//! * **One decode per picture.** The same copy asked for by the prefetching feeder, the primer, the frame that draws and a second session is made
//!   by whoever asked first; the others wait for it (no longer than a decode may take), and take its picture.
//! * **A failure is remembered**, with a backoff ([`FAILURE_BACKOFF`]): a file FFmpeg could not make the copy of is not tried again for every
//!   frame that asks, and says the same thing meanwhile.
//! * **A caller that cannot wait is not made to** ([`Wait::Never`]): the copy is made on a thread of its own (one at a time) and the caller
//!   is told `Busy` until it is there, which is how a drag over a still plan is served. A still plan's copy is made for the instant (the FFmpeg
//!   still sizes it by the zoom it shows then: `StillMotion::build` over one sample), so every instant of a drag asks for another one, and a
//!   drag cannot be served by spawning a child per position; an export frame's copy is the clip's own, made for all its frames, and the feeder
//!   and [`crate::prepare`] have it ready before the clip is on screen.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::PlanLayer;

use crate::gpu::GpuError;
use crate::source::{check_source_size, copy_of, decode_still_copy, YuvFrame, DECODE_TIMEOUT};

/// The most picture bytes kept.
pub const CAP_BYTES: usize = 96 << 20;

/// A copy FFmpeg failed to make is not asked for again before this.
pub const FAILURE_BACKOFF: Duration = Duration::from_secs(30);

/// The most failures remembered.
const MAX_FAILURES: usize = 64;

/// The most copies made on threads of their own at once (for callers that cannot wait).
const MAX_BACKGROUND: usize = 1;

/// How a caller wants a copy that is not made yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Make it (or wait for whoever is making it): a feeder, the primer, a settled frame.
    Decode,
    /// Do not block: start it on a thread of its own if none is running and answer `Busy`; the next ask finds it.
    Never,
}

/// A copy's identity: the file as it is on disk now, and the filters that make the picture of it.
type Key = (u64, String);

#[derive(Debug, Clone)]
enum Failure {
    Unsupported(String),
    Decode(String),
}

impl Failure {
    fn of(error: &GpuError) -> Option<Self> {
        match error {
            GpuError::Unsupported(why) => Some(Self::Unsupported(why.clone())),
            GpuError::Decode(why) => Some(Self::Decode(why.clone())),
            _ => None,
        }
    }

    fn error(&self) -> GpuError {
        match self {
            Self::Unsupported(why) => GpuError::Unsupported(why.clone()),
            Self::Decode(why) => GpuError::Decode(why.clone()),
        }
    }
}

type Made = Result<Option<Arc<YuvFrame>>, Failure>;

/// A copy being made: those who asked for it wait here.
struct Flight {
    done: Mutex<Option<Made>>,
    ready: Condvar,
}

impl Flight {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            done: Mutex::new(None),
            ready: Condvar::new(),
        })
    }

    fn finish(&self, made: Made) {
        *self.done.lock().unwrap_or_else(PoisonError::into_inner) = Some(made);
        self.ready.notify_all();
    }

    /// The outcome, waited for no longer than `limit`.
    fn wait(&self, limit: Duration) -> Option<Made> {
        let deadline = Instant::now() + limit;
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(made) = done.as_ref() {
                return Some(made.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            done = self.ready.wait_timeout(done, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

#[derive(Default)]
struct State {
    ready: HashMap<Key, (Arc<YuvFrame>, u64)>,
    bytes: usize,
    clock: u64,
    flights: HashMap<Key, Arc<Flight>>,
    background: usize,
    failed: HashMap<Key, (Instant, Failure)>,
}

/// The copies of a process, or of a test.
pub struct Copies {
    state: Mutex<State>,
    cap: usize,
}

static COPIES: LazyLock<Copies> = LazyLock::new(|| Copies::new(CAP_BYTES));

/// Publishes a failure for a flight whose leader never finished it (a panic), so nobody waits for it for good.
struct Leader<'a> {
    copies: &'a Copies,
    key: Key,
    flight: Arc<Flight>,
    finished: bool,
    /// The copy is made on a thread of its own, which counts against [`MAX_BACKGROUND`] until it is over.
    background: bool,
}

impl Drop for Leader<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.copies.conclude(
                &self.key,
                &self.flight,
                Err(Failure::Decode("the decode of a still's copy panicked".into())),
                Instant::now(),
            );
        }
        if self.background {
            self.copies.lock().background -= 1;
        }
    }
}

impl Copies {
    fn new(cap: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            cap,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The picture for `key`: kept, failed (remembered), being made (waited for) or made now by `decode`. `Never` answers `Busy` for one that is
    /// not made, starting `decode` on a thread of its own when none is running.
    fn get<F>(&'static self, key: Key, wait: Wait, now: Instant, decode: F) -> Result<Option<Arc<YuvFrame>>, GpuError>
    where
        F: FnOnce() -> Result<Option<Arc<YuvFrame>>, GpuError> + Send + 'static,
    {
        let flight = {
            let mut st = self.lock();
            st.clock += 1;
            let tick = st.clock;
            if let Some((frame, used)) = st.ready.get_mut(&key) {
                *used = tick;
                return Ok(Some(Arc::clone(frame)));
            }
            if let Some((at, why)) = st.failed.get(&key) {
                if now.saturating_duration_since(*at) < FAILURE_BACKOFF {
                    return Err(why.error());
                }
                st.failed.remove(&key);
            }
            if let Some(flight) = st.flights.get(&key) {
                if wait == Wait::Never {
                    return Err(busy());
                }
                Arc::clone(flight)
            } else if wait == Wait::Never {
                if st.background >= MAX_BACKGROUND {
                    return Err(busy());
                }
                let flight = Flight::new();
                st.flights.insert(key.clone(), Arc::clone(&flight));
                st.background += 1;
                drop(st);
                let started = std::thread::Builder::new().name("kerf-still-copy".into()).spawn({
                    let (key, flight) = (key.clone(), Arc::clone(&flight));
                    move || {
                        let mut leader = Leader {
                            copies: self,
                            key,
                            flight,
                            finished: false,
                            background: true,
                        };
                        let made = decode().map_err(|e| Failure::of(&e).unwrap_or(Failure::Decode(e.to_string())));
                        self.conclude(&leader.key, &leader.flight, made, Instant::now());
                        leader.finished = true;
                    }
                });
                if started.is_err() {
                    let mut st = self.lock();
                    st.flights.remove(&key);
                    st.background -= 1;
                }
                return Err(busy());
            } else {
                let flight = Flight::new();
                st.flights.insert(key.clone(), Arc::clone(&flight));
                drop(st);
                let mut leader = Leader {
                    copies: self,
                    key: key.clone(),
                    flight,
                    finished: false,
                    background: false,
                };
                let made = decode().map_err(|e| Failure::of(&e).unwrap_or(Failure::Decode(e.to_string())));
                let out = made.clone();
                self.conclude(&leader.key, &leader.flight, made, Instant::now());
                leader.finished = true;
                return out.map_err(|f| f.error());
            }
        };
        match flight.wait(DECODE_TIMEOUT + Duration::from_secs(5)) {
            Some(made) => made.map_err(|f| f.error()),
            None => Err(GpuError::Decode("a still's copy was not made in time".into())),
        }
    }

    /// Keep what a flight made (or what it failed with), take the flight down and let the waiters go.
    fn conclude(&self, key: &Key, flight: &Arc<Flight>, made: Made, now: Instant) {
        {
            let mut st = self.lock();
            st.flights.remove(key);
            match &made {
                Ok(Some(frame)) => {
                    let size = frame.y.len() + frame.u.len() + frame.v.len();
                    st.clock += 1;
                    let tick = st.clock;
                    st.bytes += size;
                    st.ready.insert(key.clone(), (Arc::clone(frame), tick));
                    // The least recently used go until the cap is met; the one just made never does.
                    while st.bytes > self.cap && st.ready.len() > 1 {
                        let oldest = st
                            .ready
                            .iter()
                            .filter(|(k, _)| *k != key)
                            .min_by_key(|(_, (_, used))| *used)
                            .map(|(k, _)| k.clone());
                        let Some(oldest) = oldest else { break };
                        if let Some((gone, _)) = st.ready.remove(&oldest) {
                            st.bytes -= gone.y.len() + gone.u.len() + gone.v.len();
                        }
                    }
                }
                Ok(None) => {}
                Err(why) => {
                    if st.failed.len() >= MAX_FAILURES {
                        st.failed
                            .retain(|_, (at, _)| now.saturating_duration_since(*at) < FAILURE_BACKOFF);
                        if st.failed.len() >= MAX_FAILURES {
                            st.failed.clear();
                        }
                    }
                    st.failed.insert(key.clone(), (now, why.clone()));
                }
            }
        }
        flight.finish(made);
    }

    /// Kept copies, and their bytes.
    pub fn held(&self) -> (usize, usize) {
        let st = self.lock();
        (st.ready.len(), st.bytes)
    }
}

fn busy() -> GpuError {
    GpuError::Busy("a still's copy is being made".into())
}

/// The picture FFmpeg makes of `layer`'s still for the compositor ([`copy_of`]): kept, or made now (`Wait::Decode`), or started and
/// answered `Busy` until it is there (`Wait::Never`). `Ok(None)` is no frame; `Err(Unsupported)` a layer that has no copy, a source over the
/// compositor's size or a picture FFmpeg could not make it of (remembered for [`FAILURE_BACKOFF`]).
pub fn copy(layer: &PlanLayer, wait: Wait) -> Result<Option<Arc<YuvFrame>>, GpuError> {
    check_source_size(layer)?;
    let Some(made) = copy_of(layer)? else {
        return Err(GpuError::Unsupported(format!("{}: not a moving still", layer.path)));
    };
    let key = (kerf_core::source_identity(Path::new(&layer.path)), made.filters.clone());
    let layer = layer.clone();
    COPIES.get(key, wait, Instant::now(), move || {
        decode_still_copy(&layer, &made, 1).map(|frame| frame.map(Arc::new))
    })
}

/// What the process keeps: copies and bytes.
pub fn held() -> (usize, usize) {
    COPIES.held()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn picture(bytes: usize) -> Arc<YuvFrame> {
        let (w, h) = (bytes as u32 / 3, 2);
        // (a w x h 4:2:0 picture's planes are w*h, w*h/4 (twice) bytes; the tests only add the lengths up)
        Arc::new(YuvFrame {
            width: w,
            height: h,
            y: vec![0; bytes * 2 / 3],
            u: vec![0; bytes / 6],
            v: vec![0; bytes / 6],
        })
    }

    fn leaked(cap: usize) -> &'static Copies {
        Box::leak(Box::new(Copies::new(cap)))
    }

    fn key(n: u64) -> Key {
        (n, format!("scale={n}"))
    }

    #[test]
    fn a_copy_is_made_once_whoever_asks_and_whenever() {
        let copies = leaked(CAP_BYTES);
        let made = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let made = Arc::clone(&made);
                std::thread::spawn(move || {
                    copies
                        .get(key(1), Wait::Decode, Instant::now(), move || {
                            made.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(60));
                            Ok(Some(picture(600)))
                        })
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        assert!(threads.into_iter().all(|t| t.join().unwrap()));
        assert_eq!(made.load(Ordering::SeqCst), 1);
        // And again, after: kept.
        let again = copies
            .get(key(1), Wait::Decode, Instant::now(), || panic!("made twice"))
            .unwrap();
        assert!(again.is_some());
        assert_eq!(copies.held().0, 1);
    }

    #[test]
    fn the_least_recently_used_goes_when_the_cap_is_met_and_never_the_one_just_made() {
        let copies = leaked(1000);
        for n in 1..=3 {
            copies
                .get(key(n), Wait::Decode, Instant::now(), || Ok(Some(picture(450))))
                .unwrap();
        }
        // 3 x 450 over 1000: the oldest went. Asking for 2 keeps it as the newest; the next one made takes 3.
        assert_eq!(copies.held(), (2, 900));
        assert!(copies.get(key(1), Wait::Never, Instant::now(), || Ok(None)).is_err());
        copies.get(key(2), Wait::Decode, Instant::now(), || panic!("kept")).unwrap();
        copies
            .get(key(4), Wait::Decode, Instant::now(), || Ok(Some(picture(450))))
            .unwrap();
        assert!(copies.get(key(2), Wait::Decode, Instant::now(), || panic!("kept")).is_ok());
        assert_eq!(copies.held().0, 2);
        // A copy over the cap alone is still kept (it is the one asked for).
        let big = leaked(100);
        big.get(key(9), Wait::Decode, Instant::now(), || Ok(Some(picture(600))))
            .unwrap();
        assert_eq!(big.held().0, 1);
    }

    #[test]
    fn a_failure_is_remembered_for_the_backoff_and_then_tried_again() {
        let copies = leaked(CAP_BYTES);
        let t0 = Instant::now();
        let first = copies.get(key(1), Wait::Decode, t0, || Err(GpuError::Unsupported("no".into())));
        assert!(matches!(first, Err(GpuError::Unsupported(_))));
        let again = copies.get(key(1), Wait::Decode, t0 + Duration::from_secs(5), || {
            panic!("tried again inside the backoff")
        });
        assert!(matches!(again, Err(GpuError::Unsupported(why)) if why == "no"));
        let later = copies.get(key(1), Wait::Decode, t0 + FAILURE_BACKOFF + Duration::from_secs(1), || {
            Ok(Some(picture(300)))
        });
        assert!(later.unwrap().is_some());
    }

    #[test]
    fn a_caller_that_cannot_wait_is_told_busy_while_the_copy_is_made_on_a_thread_of_its_own_and_has_it_after() {
        let copies = leaked(CAP_BYTES);
        let first = copies.get(key(1), Wait::Never, Instant::now(), || {
            std::thread::sleep(Duration::from_millis(150));
            Ok(Some(picture(300)))
        });
        assert!(matches!(first, Err(GpuError::Busy(_))));
        // One background copy at a time: another is refused without a thread, and so is the same one.
        let other = copies.get(key(2), Wait::Never, Instant::now(), || panic!("a second thread"));
        assert!(matches!(other, Err(GpuError::Busy(_))));
        let same = copies.get(key(1), Wait::Never, Instant::now(), || panic!("a second thread"));
        assert!(matches!(same, Err(GpuError::Busy(_))));
        // A caller that can wait takes the one under way.
        let waited = copies
            .get(key(1), Wait::Decode, Instant::now(), || panic!("made twice"))
            .unwrap();
        assert!(waited.is_some());
        let ready = copies.get(key(1), Wait::Never, Instant::now(), || panic!("kept")).unwrap();
        assert!(ready.is_some());
    }

    #[test]
    fn a_decode_that_panics_does_not_leave_the_others_waiting() {
        let copies = leaked(CAP_BYTES);
        let first = std::thread::spawn(move || {
            let _ = copies.get(key(1), Wait::Decode, Instant::now(), || {
                std::thread::sleep(Duration::from_millis(50));
                panic!("a decoder that panics")
            });
        });
        std::thread::sleep(Duration::from_millis(10));
        let waiter = copies.get(key(1), Wait::Decode, Instant::now(), || Ok(Some(picture(300))));
        assert!(first.join().is_err());
        // The waiter was told it failed, or (if it came after) made it itself; either way it returned.
        assert!(waiter.is_ok() || matches!(waiter, Err(GpuError::Decode(_))));
    }
}
