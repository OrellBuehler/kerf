//! How this ffmpeg's `geq` reads the last column and the last row of a plane.
//!
//! A mask is `geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='(keep)*alpha(X,Y)'`: the three colour
//! planes are copied through `lum` / `cb` / `cr` and the alpha plane is rewritten. `geq` reads a
//! sample with bilinear interpolation by default, and FFmpeg 6.1 clamps the position to
//! `0..=w-2` and `0..=h-2` first (`av_clipd(x, 0, w - 2)`), so at the last column and the last row
//! the "copy" returns the sample **one in**: the masked picture's right-most column and bottom row
//! are its neighbours' (measured: a 640x360 frame comes out with its last chroma column and last
//! chroma row replaced). FFmpeg 9.0 returns the sample itself. Which one a build does is a property
//! of the build, so it is measured, not read off a version string, the way the composite's colour
//! policy is: [`geq_edge`] runs the very filter on a picture whose every sample differs, once per
//! process, and compares what comes back with both answers. A probe that cannot tell is
//! [`GeqEdge::Unknown`], not a guess, and is retried after a backoff.
//!
//! **The first call blocks** for the length of one short ffmpeg run (tens of milliseconds, bounded
//! by [`PROBE_TIMEOUT`]) and holds no lock while it does: call it from a blocking thread, ahead of the
//! frame that needs it (`kerf_gpu::prepare`).

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::cli::{command, ffmpeg_bin, run_piped_until};

/// What `geq` returns for `lum(X,Y)`, `cb(X,Y)`, `cr(X,Y)` at a plane's last column and row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeqEdge {
    /// The sample at `(X, Y)`: a plain copy (FFmpeg 9.0).
    Exact,
    /// The sample at `(min(X, w - 2), min(Y, h - 2))`: the last column and row are the ones before
    /// them (FFmpeg 6.1).
    Penultimate,
    /// Neither was measured: the ffmpeg would not run, timed out, or returned something else.
    Unknown,
}

/// The longest the probe may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The probe picture's size: luma 16x16, chroma 8x8.
const PROBE_SIZE: usize = 16;

/// What the process knows: the measured answer once there is one (kept for good), else how many
/// probes failed and when the next may run.
struct State {
    measured: Option<GeqEdge>,
    failures: u32,
    retry_at: Option<Instant>,
}

/// How `geq` reads the last column and row of the ffmpeg this process runs (see the module
/// documentation). Measured once; [`GeqEdge::Unknown`] while it cannot be.
pub fn geq_edge() -> GeqEdge {
    static STATE: Mutex<State> = Mutex::new(State {
        measured: None,
        failures: 0,
        retry_at: None,
    });
    cached(&STATE, Instant::now(), || measure(&ffmpeg_bin(), PROBE_TIMEOUT))
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

fn cached(state: &Mutex<State>, now: Instant, measure: impl FnOnce() -> Option<GeqEdge>) -> GeqEdge {
    {
        let s = state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(edge) = s.measured {
            return edge;
        }
        if s.retry_at.is_some_and(|t| now < t) {
            return GeqEdge::Unknown;
        }
    }
    // Measured with no lock held: a probe is a child process, and nothing else (the answer for another
    // thread that already has it) waits behind it. Callers that arrive meanwhile measure too, boundedly;
    // the first answer stored wins.
    let measured = measure();
    let mut s = state.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(edge) = s.measured {
        return edge;
    }
    match measured {
        Some(edge) => {
            tracing::debug!(?edge, "probed how ffmpeg's geq reads a plane's last column and row");
            s.measured = Some(edge);
            edge
        }
        None => {
            s.failures += 1;
            let wait = backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure how ffmpeg's geq reads a plane's edge; masks are not drawn on the GPU until it can"
            );
            GeqEdge::Unknown
        }
    }
}

/// A plane read the way `geq` reads it where it clamps to `w - 2` / `h - 2`: every sample is the
/// one at `(min(x, w - 2), min(y, h - 2))`. The reference for [`GeqEdge::Penultimate`]; `w` and `h`
/// must be at least 2 (`geq` reads outside the plane below that).
pub fn geq_penultimate_plane(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    assert!(w >= 2 && h >= 2 && src.len() == w * h);
    let mut out = Vec::with_capacity(src.len());
    for y in 0..h {
        for x in 0..w {
            out.push(src[y.min(h - 2) * w + x.min(w - 2)]);
        }
    }
    out
}

/// The probe picture, raw `yuv420p`: every sample of a plane differs from its neighbours, so the
/// two readings cannot be mistaken for each other or for a flat answer.
fn probe_picture() -> Vec<u8> {
    let c = PROBE_SIZE / 2;
    let mut raw: Vec<u8> = (0..PROBE_SIZE * PROBE_SIZE).map(|i| i as u8).collect();
    raw.extend((0..c * c).map(|i| 64 + i as u8));
    raw.extend((0..c * c).map(|i| 160 + i as u8));
    raw
}

/// The probe picture's three planes, each with its side length.
fn planes_of(raw: &[u8]) -> [(&[u8], usize); 3] {
    let (y, rest) = raw.split_at(PROBE_SIZE * PROBE_SIZE);
    let (u, v) = rest.split_at(rest.len() / 2);
    [(y, PROBE_SIZE), (u, PROBE_SIZE / 2), (v, PROBE_SIZE / 2)]
}

/// Which answer `output` is for the probe picture `input`: a plain copy, the penultimate read, or
/// neither.
fn classify(input: &[u8], output: &[u8]) -> Option<GeqEdge> {
    if output.len() != input.len() {
        return None;
    }
    if output == input {
        return Some(GeqEdge::Exact);
    }
    let shifted: Vec<u8> = planes_of(input)
        .iter()
        .flat_map(|(plane, side)| geq_penultimate_plane(plane, *side, *side))
        .collect();
    (output == shifted.as_slice()).then_some(GeqEdge::Penultimate)
}

/// Run the mask's own `geq` over the probe picture and read the answer.
fn measure(ffmpeg: &str, timeout: Duration) -> Option<GeqEdge> {
    let raw = probe_picture();
    let size = format!("{PROBE_SIZE}x{PROBE_SIZE}");
    let mut run = command(ffmpeg);
    run.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        &size,
    ])
    .args(["-i", "pipe:0", "-vf"])
    .arg("format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)',format=yuv420p")
    .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"]);
    let out = run_piped_until(&mut run, raw.clone(), Instant::now() + timeout)?;
    classify(&raw, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_answers_differ_on_the_probe_picture_and_are_told_apart() {
        let raw = probe_picture();
        assert_eq!(classify(&raw, &raw), Some(GeqEdge::Exact));
        let shifted: Vec<u8> = planes_of(&raw)
            .iter()
            .flat_map(|(plane, side)| geq_penultimate_plane(plane, *side, *side))
            .collect();
        assert_ne!(shifted, raw);
        assert_eq!(classify(&raw, &shifted), Some(GeqEdge::Penultimate));
        // Neither: one plane shifted and not the others, a short answer, noise.
        let mut half = raw.clone();
        half[PROBE_SIZE - 1] = 0;
        assert_eq!(classify(&raw, &half), None);
        assert_eq!(classify(&raw, &raw[..raw.len() - 1]), None);
        assert_eq!(classify(&raw, &vec![0; raw.len()]), None);
    }

    #[test]
    fn the_penultimate_read_repeats_the_second_to_last_column_and_row() {
        let plane: Vec<u8> = (0..12).collect();
        // 4 wide, 3 high: rows 0 1 2, and the last row (2) reads row 1, the last column (3) column 2.
        let got = geq_penultimate_plane(&plane, 4, 3);
        assert_eq!(got, [0, 1, 2, 2, 4, 5, 6, 6, 4, 5, 6, 6]);
    }

    #[test]
    #[ignore = "needs ffmpeg"]
    fn the_real_ffmpeg_answers_the_probe() {
        let edge = measure(&ffmpeg_bin(), PROBE_TIMEOUT);
        assert!(
            matches!(edge, Some(GeqEdge::Exact | GeqEdge::Penultimate)),
            "this ffmpeg's geq is neither a copy nor a penultimate read: {edge:?}"
        );
        assert_eq!(geq_edge(), edge.unwrap());
        // A binary that is not one answers nothing.
        assert_eq!(measure("/nonexistent/ffmpeg", Duration::from_secs(2)), None);
    }

    #[test]
    fn the_probe_runs_with_no_lock_held_and_the_first_answer_stored_wins() {
        let state = Mutex::new(State {
            measured: None,
            failures: 0,
            retry_at: None,
        });
        let now = Instant::now();
        // While the probe runs another caller is not queued behind it: it can take the lock, and its own
        // answer (here the whole nested call) is stored first.
        let got = cached(&state, now, || {
            assert!(state.try_lock().is_ok(), "the state is locked while the probe runs");
            assert_eq!(cached(&state, now, || Some(GeqEdge::Penultimate)), GeqEdge::Penultimate);
            Some(GeqEdge::Exact)
        });
        assert_eq!(got, GeqEdge::Penultimate, "the first writer wins");
        assert_eq!(cached(&state, now, || panic!("measured twice")), GeqEdge::Penultimate);
        // A failure that lost the race to an answer changes nothing.
        let state = Mutex::new(State {
            measured: None,
            failures: 0,
            retry_at: None,
        });
        let got = cached(&state, now, || {
            cached(&state, now, || Some(GeqEdge::Exact));
            None
        });
        assert_eq!(got, GeqEdge::Exact);
        assert_eq!(state.lock().unwrap().failures, 0);
    }

    #[test]
    fn a_measured_answer_is_kept_and_a_failure_is_retried_after_a_backoff() {
        let state = Mutex::new(State {
            measured: None,
            failures: 0,
            retry_at: None,
        });
        let t0 = Instant::now();
        assert_eq!(cached(&state, t0, || None), GeqEdge::Unknown);
        // Inside the backoff the probe is not run again.
        assert_eq!(
            cached(&state, t0, || panic!("probed again inside the backoff")),
            GeqEdge::Unknown
        );
        let later = t0 + Duration::from_secs(60);
        assert_eq!(cached(&state, later, || Some(GeqEdge::Penultimate)), GeqEdge::Penultimate);
        assert_eq!(
            cached(&state, later, || panic!("a measured answer is kept")),
            GeqEdge::Penultimate
        );
        assert_eq!(backoff(1).as_secs(), 5);
        assert_eq!(backoff(3).as_secs(), 20);
        assert_eq!(backoff(40).as_secs(), 300);
    }
}
