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

use std::io::Read;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conversion {
    /// The graph's conversion is the decode's, for 4:4:4 and for 4:2:2.
    Match,
    /// It is not (the detail says where).
    Differs(String),
    /// Not known: not measured yet, FFmpeg would not run or timed out (asked again after a backoff).
    Unknown,
}

const PROBE: (u32, u32) = (64, 36);
const LIMIT: Duration = Duration::from_secs(20);
const BACKOFF: Duration = Duration::from_secs(30);

struct State {
    measured: Option<Conversion>,
    retry_at: Option<Instant>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(State {
            measured: None,
            retry_at: None,
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What is known without running anything: the measured answer, else [`Conversion::Unknown`].
pub fn measured() -> Conversion {
    lock().measured.clone().unwrap_or(Conversion::Unknown)
}

/// The answer, measured on the first call (a few short child processes: call it from a blocking thread, off any lock;
/// [`crate::prepare`] does, ahead of a plan that needs it). `Unknown` is not remembered: the probe is run again after a
/// backoff.
pub fn conversion() -> Conversion {
    let now = Instant::now();
    {
        let s = lock();
        if let Some(known) = &s.measured {
            return known.clone();
        }
        if s.retry_at.is_some_and(|t| t > now) {
            return Conversion::Unknown;
        }
    }
    let found = probe();
    let mut s = lock();
    if found == Conversion::Unknown {
        s.retry_at = Some(Instant::now() + BACKOFF);
    } else {
        s.measured = Some(found.clone());
    }
    found
}

/// The picture: `testsrc2` (hard colour edges, a gradient, a counter), in the format asked for.
fn source(format: &str) -> String {
    let (w, h) = PROBE;
    format!("testsrc2=size={w}x{h}:rate=1:duration=1,format={format}")
}

/// One FFmpeg run, its raw stdout, or `None` when it would not run, failed or took too long (killed).
fn run(args: &[&str]) -> Option<Vec<u8>> {
    let mut child = kerf_core::ffmpeg_command()
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + LIMIT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let buf = reader.join().ok()?;
    status.filter(|s| s.success()).map(|_| buf)
}

fn probe() -> Conversion {
    let (w, h) = PROBE;
    let (luma, chroma) = ((w * h) as usize, (w * h / 4) as usize);
    for format in ["yuv444p", "yuv422p"] {
        let src = source(format);
        // The decode's: a simple graph with the scale to limited range, to 4:2:0.
        let Some(decoded) = run(&[
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
        ]) else {
            return Conversion::Unknown;
        };
        // The placement chain's: a complex graph, the fit's `scale` and `format=yuva420p`, which merge into one conversion.
        let graph = format!("[0:v]scale={w}:{h}:force_original_aspect_ratio=decrease,setsar=1,format=yuva420p[o]");
        let Some(in_graph) = run(&[
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
        ]) else {
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

    /// The probe itself, on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell: it answers `Match` or
    /// `Differs` (never `Unknown`), and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_converts_in_a_graph_as_it_does_at_the_decode_or_the_probe_says_where_not() {
        let first = probe();
        assert_ne!(first, Conversion::Unknown, "the probe could not run");
        assert_eq!(first, probe());
        eprintln!("graph conversion of 4:4:4 and 4:2:2: {first:?}");
    }
}
