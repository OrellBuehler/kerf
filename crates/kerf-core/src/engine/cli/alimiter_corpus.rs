//! The differential corpus for the preview's port of the master limiter
//! (`frontend/src/lib/alimiter.ts`, which plays it in an AudioWorklet): seeded signals put through
//! the real `ffmpeg` with the export's own limiter text ([`limiter_filter`], `latency=1`), and what
//! came out, sample for sample in doubles. The port replays the file (`alimiter.test.ts`) and must
//! give the same bits, so a limiter that changed here or in FFmpeg without the port following
//! fails there, naming the case.
//!
//! The signals are written as segments the two sides build the same way, with nothing but
//! `+ - * /` and an LCG on `u32` (no `sin`, whose last digit differs between libms).
//!
//! A case longer than [`HASHED_FRAMES`] is stored as the FNV-1a (32-bit) of its output's bytes and
//! its length instead of the samples, so the file stays small; the replay hashes what it made the same
//! way.
//!
//! `#[ignore]`d (it needs the ffmpeg binary). Regenerate after an intended change:
//! `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- --ignored alimiter_corpus`
//! (`KERF_BLESS_CORPUS` is shared: the same variable re-blesses the links corpus when its test runs too).

use std::process::Stdio;

use serde_json::{json, Value};

use super::rendered::scratch;
use super::*;
use crate::model::MasterBus;

/// Longer than this many frames, a case's output is stored as its hash.
const HASHED_FRAMES: u32 = 2000;

const CORPUS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../frontend/src/lib/fixtures/alimiter-corpus.json"
);

/// A stretch of a signal: `frames` frames whose left and right amplitudes run linearly from
/// `from` to `to`, of one shape.
#[derive(Clone, Copy)]
struct Segment {
    frames: u32,
    shape: Shape,
    from: (f64, f64),
    to: (f64, f64),
}

#[derive(Clone, Copy)]
enum Shape {
    /// The amplitude itself.
    Dc,
    /// A triangle of `period` frames between -1 and 1.
    Tri(u32),
    /// Uniform noise in -1..1 from an LCG seeded with this, left then right each frame.
    Noise(u32),
}

fn lcg(state: &mut u32) -> f64 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    f64::from(*state) / 4_294_967_296.0 * 2.0 - 1.0
}

/// The interleaved stereo signal the segments make (`signal` in `alimiter.test.ts`).
fn signal(segments: &[Segment]) -> Vec<f64> {
    let mut out = Vec::new();
    for s in segments {
        let mut state = match s.shape {
            Shape::Noise(seed) => seed,
            _ => 0,
        };
        let frames = f64::from(s.frames);
        for n in 0..s.frames {
            let at = f64::from(n);
            let amp = |a: f64, b: f64| a + (b - a) * at / frames;
            let (l, r) = (amp(s.from.0, s.to.0), amp(s.from.1, s.to.1));
            let (sl, sr) = match s.shape {
                Shape::Dc => (1.0, 1.0),
                Shape::Tri(p) => {
                    let t = 4.0 * (f64::from(n % p) / f64::from(p) - 0.5).abs() - 1.0;
                    (t, t)
                }
                Shape::Noise(_) => (lcg(&mut state), lcg(&mut state)),
            };
            out.push(l * sl);
            out.push(r * sr);
        }
    }
    out
}

fn seg(frames: u32, shape: Shape, from: (f64, f64), to: (f64, f64)) -> Segment {
    Segment { frames, shape, from, to }
}

fn flat(frames: u32, shape: Shape, amp: (f64, f64)) -> Segment {
    seg(frames, shape, amp, amp)
}

/// Each case: a name, a sample rate, a ceiling (dBFS) and its signal.
fn cases() -> Vec<(&'static str, u32, f64, Vec<Segment>)> {
    let quiet = |frames, seed| flat(frames, Shape::Noise(seed), (0.1, 0.08));
    let mut stairs = Vec::new();
    for (k, a) in [0.8, 1.0, 1.25, 1.5, 1.9, 1.6, 1.3, 1.1, 0.9, 2.2, 0.75]
        .into_iter()
        .enumerate()
    {
        stairs.push(quiet(7 + k as u32 % 3 * 5, 11 + k as u32));
        stairs.push(flat(1 + k as u32 % 2, Shape::Dc, (a, -a * 0.75)));
    }
    stairs.push(quiet(500, 99));
    vec![
        (
            "a burst rides up into the ceiling and is released",
            8000,
            -6.0,
            vec![
                flat(60, Shape::Dc, (0.0, 0.0)),
                seg(400, Shape::Tri(16), (0.2, 0.1), (1.4, 1.0)),
                flat(300, Shape::Tri(16), (1.0, 1.2)),
                flat(700, Shape::Tri(20), (0.3, 0.3)),
            ],
        ),
        (
            "transients close together, each side its own sign",
            8000,
            -1.5,
            vec![
                quiet(100, 1),
                flat(2, Shape::Dc, (1.3, 1.3)),
                quiet(15, 2),
                flat(3, Shape::Dc, (-1.6, 0.9)),
                quiet(30, 3),
                flat(1, Shape::Dc, (1.1, -1.1)),
                flat(600, Shape::Noise(4), (0.2, 0.2)),
            ],
        ),
        ("a staircase of peaks up and down between quiet noise", 8000, -3.0, stairs),
        (
            "noise pushed far over the ceiling, then let go",
            8000,
            -1.5,
            vec![
                seg(1000, Shape::Noise(5), (0.5, 0.5), (2.5, 1.8)),
                flat(500, Shape::Noise(6), (0.3, 0.3)),
            ],
        ),
        (
            "at 48 kHz: a triangle swelling over the ceiling",
            48_000,
            -1.5,
            vec![
                seg(1200, Shape::Tri(109), (0.5, 0.4), (1.6, 1.2)),
                flat(400, Shape::Noise(7), (0.4, 0.4)),
            ],
        ),
        (
            "at 44.1 kHz (an odd lookahead) on the lowest ceiling",
            44_100,
            -24.0,
            vec![flat(700, Shape::Noise(8), (0.3, 0.2))],
        ),
        (
            "a 0 dB ceiling: nothing over full scale",
            8000,
            0.0,
            vec![
                seg(600, Shape::Noise(9), (0.6, 0.6), (2.0, 1.4)),
                flat(400, Shape::Tri(13), (0.9, 1.05)),
            ],
        ),
        (
            "at 48 kHz, long: bursts released back past unity again and again",
            48_000,
            -3.0,
            released(),
        ),
        (
            "at 96 kHz: a swell over the ceiling and noise after it",
            96_000,
            -1.5,
            vec![
                seg(3000, Shape::Tri(211), (0.4, 0.5), (1.7, 1.3)),
                flat(1500, Shape::Noise(10), (0.5, 0.3)),
            ],
        ),
    ]
}

/// Bursts over the ceiling with quiet gaps longer than the 100 ms release between them, so the gain
/// climbs back past unity after each (the `att > 1` reset) — five seconds of it at 48 kHz.
fn released() -> Vec<Segment> {
    let mut out = Vec::new();
    for k in 0..12u32 {
        out.push(flat(6000 + k * 700, Shape::Noise(20 + k), (0.15, 0.1)));
        out.push(seg(
            1500 + k * 90,
            Shape::Tri(37 + k * 6),
            (0.9, 0.8),
            (1.2 + f64::from(k) * 0.2, 1.6),
        ));
        out.push(flat(2 + k % 3, Shape::Dc, (2.4, -1.9)));
    }
    out.push(flat(9000, Shape::Noise(40), (0.1, 0.1)));
    out
}

/// FNV-1a, 32-bit, over the output's interleaved samples as little-endian doubles
/// (`fnv1a` in `alimiter.test.ts`).
fn fnv1a(samples: &[f64]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in samples.iter().flat_map(|v| v.to_le_bytes()) {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// A scratch directory that goes when it does, whether the run passed or not.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn segment_json(s: &Segment) -> Value {
    let shape = match s.shape {
        Shape::Dc => json!({ "kind": "dc" }),
        Shape::Tri(period) => json!({ "kind": "tri", "period": period }),
        Shape::Noise(seed) => json!({ "kind": "noise", "seed": seed }),
    };
    json!({ "frames": s.frames, "shape": shape, "from": [s.from.0, s.from.1], "to": [s.to.0, s.to.1] })
}

/// `samples` (interleaved stereo at `rate`) through `filter` in the real ffmpeg, as doubles.
fn through_ffmpeg(samples: &[f64], rate: u32, filter: &str, tag: &str) -> Vec<f64> {
    let dir = Scratch(scratch(&format!("alimiter-{tag}")));
    let input = dir.0.join("in.f64");
    let output = dir.0.join("out.f64");
    std::fs::write(&input, samples.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    let run = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "f64le", "-ar"])
        .arg(rate.to_string())
        .args(["-ac", "2", "-i"])
        .arg(&input)
        .args(["-af", filter, "-f", "f64le", "-c:a", "pcm_f64le"])
        .arg(&output)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(run.status.success(), "{filter}: {}", String::from_utf8_lossy(&run.stderr));
    let raw = std::fs::read(&output).unwrap();
    raw.as_chunks::<8>().0.iter().map(|b| f64::from_le_bytes(*b)).collect()
}

fn corpus() -> Value {
    let cases: Vec<Value> = cases()
        .iter()
        .enumerate()
        .map(|(i, (name, rate, ceiling_db, segments))| {
            let master = MasterBus {
                limiter: true,
                ceiling_db: *ceiling_db,
                ..MasterBus::default()
            };
            let filter = with_alimiter_latency(true, || limiter_filter(master.limit_linear()));
            let input = signal(segments);
            let out = through_ffmpeg(&input, *rate, &filter, &i.to_string());
            assert_eq!(out.len(), input.len(), "{name}: latency=1 keeps the length");
            let side = |c: usize| out.iter().skip(c).step_by(2).copied().collect::<Vec<f64>>();
            let frames = out.len() / 2;
            let mut case = json!({
                "name": name,
                "rate": rate,
                "ceiling_db": ceiling_db,
                "filter": filter,
                "segments": segments.iter().map(segment_json).collect::<Vec<_>>(),
            });
            if frames > HASHED_FRAMES as usize {
                case["frames"] = json!(frames);
                case["out_fnv1a"] = json!(fnv1a(&out));
            } else {
                case["out"] = json!([side(0), side(1)]);
            }
            case
        })
        .collect();
    json!({
        "about": "Written by kerf-core (engine/cli/alimiter_corpus.rs) through the real ffmpeg; replayed by alimiter.test.ts. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- --ignored alimiter_corpus",
        "cases": cases,
    })
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn alimiter_corpus_is_what_ffmpeg_does() {
    if !alimiter_latency_available() {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("skipped: this ffmpeg's alimiter has no latency option");
        }
        return;
    }
    let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
    if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
        std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
        return;
    }
    let on_disk = std::fs::read_to_string(CORPUS_PATH)
        .expect("frontend/src/lib/fixtures/alimiter-corpus.json — generate it with KERF_BLESS_CORPUS=1")
        .replace("\r\n", "\n");
    assert!(
        on_disk == fresh,
        "the alimiter corpus is stale: this ffmpeg limits the signals differently from the checked-in answers. \
         Regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- --ignored alimiter_corpus, \
         then run `bun test` — the replay says what the port has to follow."
    );
}
