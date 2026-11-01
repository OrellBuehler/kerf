//! Whether this FFmpeg converts a 4:4:4 or 4:2:2 picture to `yuva420p` **in a filter graph** as it does at the
//! decode — measured once per process, never assumed and never keyed on a version string.
//!
//! The compositor decodes every layer to 4:2:0 ahead of the graph (`scale=out_range=tv ... -pix_fmt yuv420p`, a simple
//! graph, the CLI's own scaler flags), and a clip whose zoom or position moves ends its chain in the placement, which
//! begins with `format=yuva420p`: the conversion happens **in the graph**, by the `scale` filter's own flags. Where that
//! filter defaults to the scaler the decode uses the two are the same picture (6.1.1, 9.0.2); where it does not (4.4.2's
//! `scale` defaults to bilinear: chroma 24 levels off on 4:4:4, 16 on 4:2:2) they are not, and a placed 4:4:4 or 4:2:2 layer
//! would be drawn from the wrong chroma. The probe renders a small picture of each format both ways and compares the
//! planes; `GpuCaps::placed_chroma` is set only on a `Match`.

use std::time::{Duration, Instant};

use crate::measure::{self, Budget, Probe, Timing, Verdict};

/// What the probe found: [`Verdict::Match`] (the graph's conversion is the decode's, for 4:4:4 and for 4:2:2),
/// [`Verdict::Differs`] (it is not: the detail says where) or [`Verdict::Unknown`] (not measured yet, FFmpeg would not run
/// or timed out: asked again after a backoff).
pub type Conversion = Verdict;

const PROBE: (u32, u32) = (64, 36);
/// One FFmpeg run is bounded at 20 s, the whole probe (four runs of tens of milliseconds) at 30 s, and a probe that could not
/// tell is not asked again before 30 s. One probe runs at a time whoever asks ([`crate::measure`]).
const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(30),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// What is known without running anything: the measured answer, else [`Conversion::Unknown`].
pub fn measured() -> Conversion {
    measure::known(&STATE)
}

/// The answer, measured on the first call (a few short child processes: call it from a blocking thread, off any lock;
/// [`crate::prepare`] does, ahead of a plan that needs it). A caller that arrives while the probe runs waits for it (no longer
/// than its deadline) and takes its answer. `Unknown` is not remembered but for a backoff, inside which the probe is not run
/// again.
pub fn conversion() -> Conversion {
    conversion_in(&STATE, Instant::now(), probe)
}

/// [`conversion`] over a given memory, clock and probe: the state machine without the child processes.
fn conversion_in(state: &Probe, now: Instant, probe: impl FnOnce(&Budget) -> Conversion) -> Conversion {
    measure::cached(state, now, TIMING, probe)
}

/// The picture: `testsrc2` (hard colour edges, a gradient, a counter), in the format asked for.
fn source(format: &str) -> String {
    let (w, h) = PROBE;
    format!("testsrc2=size={w}x{h}:rate=1:duration=1,format={format}")
}

fn probe(budget: &Budget) -> Conversion {
    let (w, h) = PROBE;
    let (luma, chroma) = ((w * h) as usize, (w * h / 4) as usize);
    for format in ["yuv444p", "yuv422p"] {
        let src = source(format);
        // The decode's: a simple graph with the scale to limited range, to 4:2:0.
        let Some(decoded) = budget.ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                &src,
                "-vf",
                "scale=out_range=tv",
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "pipe:1",
            ],
            Vec::new(),
        ) else {
            return Conversion::Unknown;
        };
        // The placement chain's: a complex graph, the fit's `scale` and `format=yuva420p`, which merge into one conversion.
        let graph = format!("[0:v]scale={w}:{h}:force_original_aspect_ratio=decrease,setsar=1,format=yuva420p[o]");
        let Some(in_graph) = budget.ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                &src,
                "-filter_complex",
                &graph,
                "-map",
                "[o]",
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuva420p",
                "-f",
                "rawvideo",
                "pipe:1",
            ],
            Vec::new(),
        ) else {
            return Conversion::Unknown;
        };
        let planes = luma + 2 * chroma;
        if decoded.len() != planes || in_graph.len() != planes + luma {
            return Conversion::Unknown;
        }
        let differing = decoded.iter().zip(&in_graph[..planes]).filter(|(a, b)| a != b).count();
        if differing != 0 {
            let max = decoded
                .iter()
                .zip(&in_graph[..planes])
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            return Conversion::Differs(format!(
                "{format}: the graph's conversion to yuva420p differs from the decode's in {differing} samples (max {max})"
            ));
        }
    }
    Conversion::Match
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memory of the probe: a probe that could not tell is asked again after the backoff (30 s) and not before, and
    /// what it finds, a match or a difference, is kept for good (`measured()` reads it, nothing runs again).
    #[test]
    fn an_unknown_is_asked_again_after_thirty_seconds_and_a_match_or_a_difference_is_kept() {
        assert_eq!(TIMING.backoff, Duration::from_secs(30));
        let state = Probe::new();
        let t0 = Instant::now();
        assert_eq!(measure::known(&state), Conversion::Unknown, "nothing measured yet");
        assert_eq!(conversion_in(&state, t0, |_| Conversion::Unknown), Conversion::Unknown);
        // Inside the backoff the probe does not run, and a probe that would now succeed is not asked.
        let inside = t0 + Duration::from_secs(29);
        assert_eq!(
            conversion_in(&state, inside, |_| panic!("probed inside the backoff")),
            Conversion::Unknown
        );
        assert_eq!(measure::known(&state), Conversion::Unknown);
        // After it, it is, and what it finds is kept.
        let after = Instant::now() + TIMING.backoff + Duration::from_secs(1);
        let differs = Conversion::Differs("yuv444p: 3 samples".into());
        assert_eq!(conversion_in(&state, after, |_| differs.clone()), differs);
        assert_eq!(measure::known(&state), differs);
        assert_eq!(conversion_in(&state, after, |_| panic!("measured twice")), differs);
        // A match is kept the same way.
        let state = Probe::new();
        assert_eq!(conversion_in(&state, t0, |_| Conversion::Match), Conversion::Match);
        assert_eq!(measure::known(&state), Conversion::Match);
        assert_eq!(conversion_in(&state, after, |_| panic!("measured twice")), Conversion::Match);
    }

    /// The probe itself, on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell: it answers `Match` or
    /// `Differs` (never `Unknown`), and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_converts_in_a_graph_as_it_does_at_the_decode_or_the_probe_says_where_not() {
        let first = probe(&Budget::new(&TIMING));
        assert_ne!(first, Conversion::Unknown, "the probe could not run");
        assert_eq!(first, probe(&Budget::new(&TIMING)));
        eprintln!("graph conversion of 4:4:4 and 4:2:2: {first:?}");
    }
}
