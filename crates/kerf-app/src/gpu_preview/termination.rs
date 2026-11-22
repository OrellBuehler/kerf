//! Taking the GPU preview's markers away when a session ends the process with a signal.
//!
//! A logout or a shutdown on Linux and macOS ends a run with SIGTERM or SIGHUP and no `RunEvent::Exit` (a terminal
//! that closes sends SIGHUP, Ctrl+C in one SIGINT). Such a run would be a strike of the GPU preview on a healthy
//! machine, and two in a row block it. (Windows has no such signals here: a shutdown or a restart manager's close
//! reaches `RunEvent::Exit` through tao's `WM_ENDSESSION`.)
//!
//! What this does is small and has to be hard to get wrong, since it stands between the signal and the process
//! ending:
//!
//! * It is installed **only once a marker exists** (the first time the preview puts one down), so a run that never
//!   starts the GPU preview has no handler at all.
//! * It leaves a signal alone **whose disposition is not the default** — ignored (`nohup kerf-app &` inherits
//!   `SIG_IGN` for SIGHUP) or handled by something else — since replacing it would change what the signal does.
//! * The handler only writes a byte to a pipe (async-signal-safe). A thread of its own reads it, runs the clearing on
//!   *another* thread and waits for that at most [`GRACE`], so a clearing that panics or hangs cannot hold the signal
//!   back, and no runtime has to be free to run it.
//! * Then it **always** puts the default action back and raises the signal again, so the process dies of it as it
//!   always did. A second signal while this runs only writes another byte.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// How long the clearing is given before the signal is let through anyway.
pub const GRACE: Duration = Duration::from_millis(500);

/// The signals a session or a terminal ends a run with.
const SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGHUP, libc::SIGINT];

/// The write end of the pipe the handler writes to (`-1` before [`before_termination`]).
static WRITE_FD: AtomicI32 = AtomicI32::new(-1);

#[cfg(target_os = "linux")]
fn errno_slot() -> *mut libc::c_int {
    // SAFETY: returns the calling thread's errno location.
    unsafe { libc::__errno_location() }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn errno_slot() -> *mut libc::c_int {
    // SAFETY: returns the calling thread's errno location.
    unsafe { libc::__error() }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios", target_os = "freebsd")))]
fn errno_slot() -> *mut libc::c_int {
    std::ptr::null_mut()
}

/// The signal handler: one byte down the pipe, and nothing else (a write to a pipe is async-signal-safe; errno is put
/// back as the interrupted code had it).
extern "C" fn note(signal: libc::c_int) {
    let slot = errno_slot();
    // SAFETY: reads and restores this thread's errno, and writes one byte to a descriptor that stays open.
    unsafe {
        let saved = if slot.is_null() { 0 } else { *slot };
        let fd = WRITE_FD.load(Ordering::Relaxed);
        if fd >= 0 {
            let byte = signal as u8;
            libc::write(fd, (&byte as *const u8).cast(), 1);
        }
        if !slot.is_null() {
            *slot = saved;
        }
    }
}

/// Whether `signal` is at its default action, which is the only one this replaces.
pub fn is_default(signal: libc::c_int) -> bool {
    // SAFETY: a zeroed `sigaction` is a valid out-parameter, and a null new action only reads the current one.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut current) == 0 && current.sa_sigaction == libc::SIG_DFL
    }
}

/// Run `before` when the session ends this process with SIGTERM, SIGHUP or SIGINT, then let the signal do what it
/// always did (see the module). Returns the signals taken over: none for one that is ignored or handled elsewhere, and
/// none at all when the pipe or the thread could not be made (the signal then does what it always did, and the run is
/// counted as it was before). Call it once per process.
pub fn before_termination(before: impl FnOnce() + Send + 'static) -> Vec<libc::c_int> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` holds the two descriptors a pipe returns.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        tracing::warn!(error = %std::io::Error::last_os_error(), "could not make the pipe the termination handler needs");
        return Vec::new();
    }
    let [read_fd, write_fd] = fds;
    // SAFETY: both descriptors are the ones just made. Not inherited by what this process starts (FFmpeg), and a
    // full pipe must not block the handler.
    unsafe {
        for fd in fds {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let flags = libc::fcntl(write_fd, libc::F_GETFL);
        libc::fcntl(write_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let reader = std::thread::Builder::new()
        .name("kerf-gpu-signal".into())
        .spawn(move || end_with(read_fd, before));
    if let Err(e) = reader {
        tracing::warn!(error = %e, "could not start the thread that clears the GPU preview's markers at a termination signal");
        // SAFETY: closing the descriptors made above, which nothing else has.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
        return Vec::new();
    }
    WRITE_FD.store(write_fd, Ordering::SeqCst);
    let mut taken = Vec::new();
    for signal in SIGNALS {
        if !is_default(signal) {
            continue;
        }
        // SAFETY: `note` is an async-signal-safe handler; the action is fully initialised.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = note as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut()) == 0
        };
        if installed {
            taken.push(signal);
        }
    }
    taken
}

/// The reader: waits for the handler's byte, clears on a thread of its own for at most [`GRACE`], then restores the
/// default action of the signal and raises it, whatever happened to the clearing.
fn end_with(read_fd: libc::c_int, before: impl FnOnce() + Send + 'static) {
    let mut byte = 0u8;
    loop {
        // SAFETY: reads one byte into a local the length of one.
        let n = unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) };
        if n == 1 {
            break;
        }
        if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return;
    }
    let signal = libc::c_int::from(byte);
    let (done, finished) = mpsc::channel();
    let clearing = std::thread::Builder::new().name("kerf-gpu-clear".into()).spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(before));
        let _ = done.send(());
    });
    if clearing.is_ok() {
        let _ = finished.recv_timeout(GRACE);
    }
    // SAFETY: puts the default action back and raises the signal for this process; nothing is dereferenced.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::kill(libc::getpid(), signal);
    }
    // The default action of these signals ends the process; if it somehow did not (the signal is blocked in every
    // thread), this thread has nothing left to do.
    loop {
        std::thread::park();
    }
}

/// What the tests that need a process of their own share: a handler for a signal is process-wide, so each of them
/// runs its body in a child (this test binary, one test, an environment variable saying where) and looks at how it ended.
#[cfg(test)]
pub(crate) mod testing {
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::path::Path;
    use std::process::ExitStatus;
    use std::time::{Duration, Instant};

    pub const DIR: &str = "KERF_TEST_TERMINATION_DIR";
    pub const MODE: &str = "KERF_TEST_TERMINATION_MODE";

    /// Run `test` (its full path in the test binary) as a child with `dir` and `mode` in its environment, SIGTERM ignored
    /// from the start when `ignore_term` (what `nohup` does to SIGHUP, and a parent may do to any signal). How it ended,
    /// and how long it took.
    pub fn run_child(test: &str, dir: &Path, mode: &str, ignore_term: bool) -> (ExitStatus, Duration) {
        let mut command = std::process::Command::new(std::env::current_exe().expect("this test binary"));
        command.args(["--exact", test, "--nocapture"]).env(DIR, dir).env(MODE, mode);
        if ignore_term {
            // SAFETY: only async-signal-safe `signal` between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                    Ok(())
                });
            }
        }
        let started = Instant::now();
        let status = command.status().expect("run the child");
        (status, started.elapsed())
    }

    /// The child's side: every signal this module takes is at its default action, whatever the parent made of them.
    pub fn default_dispositions(except: &[libc::c_int]) {
        for signal in super::SIGNALS {
            if !except.contains(&signal) {
                // SAFETY: setting a default action.
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                }
            }
        }
    }

    /// The child sends itself `signal`.
    pub fn raise(signal: libc::c_int) {
        // SAFETY: signalling this very process.
        unsafe {
            libc::kill(libc::getpid(), signal);
        }
    }

    pub fn died_of(status: &ExitStatus, signal: libc::c_int) -> bool {
        status.signal() == Some(signal)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::path::PathBuf;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-gpu-termination-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const MARKS: &str =
        "gpu_preview::termination::tests::a_signal_clears_what_it_is_given_and_the_process_dies_of_it_as_it_always_did";
    const SLOW: &str = "gpu_preview::termination::tests::a_clearing_that_hangs_or_panics_does_not_hold_the_signal_back";
    const IGNORED: &str = "gpu_preview::termination::tests::a_signal_that_is_ignored_is_left_ignored_and_the_others_are_taken";

    #[test]
    fn a_signal_clears_what_it_is_given_and_the_process_dies_of_it_as_it_always_did() {
        if let Some(dir) = std::env::var_os(DIR) {
            let marker = PathBuf::from(dir).join("marker");
            std::fs::write(&marker, "1").unwrap();
            default_dispositions(&[]);
            let taken = before_termination(move || {
                let _ = std::fs::remove_file(&marker);
            });
            assert_eq!(taken, SIGNALS.to_vec(), "all three at their default are taken");
            raise(libc::SIGTERM);
            std::thread::sleep(Duration::from_secs(20));
            std::process::exit(7); // not reached: the signal ends it
        }
        let dir = scratch();
        let (status, took) = run_child(MARKS, &dir, "", false);
        assert!(died_of(&status, libc::SIGTERM), "{status:?}");
        assert!(!dir.join("marker").exists(), "cleared before it died");
        assert!(took < Duration::from_secs(15), "{took:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clearing_that_hangs_or_panics_does_not_hold_the_signal_back() {
        if let Some(mode) = std::env::var_os(MODE) {
            default_dispositions(&[]);
            let hang = mode == "hang";
            before_termination(move || {
                if hang {
                    std::thread::sleep(Duration::from_secs(60));
                } else {
                    panic!("a marker that cannot be removed");
                }
            });
            // Twice, as a session does when the first does not end it fast enough.
            raise(libc::SIGTERM);
            raise(libc::SIGTERM);
            std::thread::sleep(Duration::from_secs(20));
            std::process::exit(7);
        }
        let dir = scratch();
        for mode in ["hang", "panic"] {
            let (status, took) = run_child(SLOW, &dir, mode, false);
            assert!(died_of(&status, libc::SIGTERM), "{mode}: {status:?}");
            assert!(took < GRACE + Duration::from_secs(10), "{mode}: the signal waited {took:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_signal_that_is_ignored_is_left_ignored_and_the_others_are_taken() {
        if let Some(dir) = std::env::var_os(DIR) {
            let dir = PathBuf::from(dir);
            let marker = dir.join("marker");
            std::fs::write(&marker, "1").unwrap();
            // SIGTERM was ignored by whoever started this (as `nohup` does for SIGHUP): it stays so.
            default_dispositions(&[libc::SIGTERM]);
            assert!(!is_default(libc::SIGTERM));
            let taken = before_termination({
                let marker = marker.clone();
                move || {
                    let _ = std::fs::remove_file(&marker);
                }
            });
            assert_eq!(taken, vec![libc::SIGHUP, libc::SIGINT], "not the ignored one");
            raise(libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(400));
            // Still here, and the markers are as they were: the signal did nothing, as before.
            std::fs::write(dir.join("survived"), "1").unwrap();
            assert!(marker.exists());
            // A signal that is taken still ends it, after the clearing.
            raise(libc::SIGHUP);
            std::thread::sleep(Duration::from_secs(20));
            std::process::exit(7);
        }
        let dir = scratch();
        let (status, _) = run_child(IGNORED, &dir, "", true);
        assert!(
            dir.join("survived").exists(),
            "an ignored SIGTERM did not end the run: {status:?}"
        );
        assert!(died_of(&status, libc::SIGHUP), "{status:?}");
        assert!(!dir.join("marker").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
