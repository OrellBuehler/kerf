//! `checker/bl`: the number of threads FFmpeg runs the translucent blend chain on does not change a sample. The export's own text for a clip
//! below full opacity (`format=yuva420p,colorchannelmixer=aa=..` and the clip's `fade alpha=1` steps ahead of the blend's `split`, then
//! `kerf_core::blend_probe_graph`) is run by the real `ffmpeg` on noise at frame sizes whose slice boundaries fall on odd rows and whose
//! rows end in the loops' tails, with `-filter_threads` / `-filter_complex_threads` from 1 to 16 (more threads than a machine has: the
//! budget of an export never goes past its cores, but a slice boundary is a boundary whoever runs it), and every output frame must be
//! byte for byte the single-threaded run's. The model is held to the single-threaded run by `motion_parity`'s cases (the export's threads there are
//! the machine's budget); this holds the threads to each other. No adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_bl_threads -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use kerf_core::BlendMode;

struct Lcg(u64);

impl Lcg {
    fn byte(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u8
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

fn scratch() -> PathBuf {
    common::media_dir("checker-bl-threads")
}

/// One run: the base and the clip as raw frames (`frames` identical copies of each, 30 fps), the clip's chain `chain` ahead of the blend, the
/// blend's own chains from `kerf_core::blend_probe_graph`, `threads` filter threads; every output frame as `yuv420p` bytes.
#[allow(clippy::too_many_arguments)]
fn run(
    threads: usize,
    base: (&Path, (usize, usize)),
    clip: (&Path, (usize, usize)),
    chain: &str,
    mode: BlendMode,
    place: (i32, i32),
    tag: Option<&str>,
    frames: usize,
) -> Vec<u8> {
    let tagged = tag.map_or_else(String::new, |t| format!("setparams=colorspace={t}:range=tv,"));
    let graph = format!(
        "[0:v]{tagged}format=yuv420p[base];[1:v]{tagged}format=yuva420p,{chain}[clip];{}",
        kerf_core::blend_probe_graph(mode, place, tag).expect("a blend mode")
    );
    let n = threads.to_string();
    let mut cmd = Command::new(kerf_core::ffmpeg_path());
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-threads",
        "1",
        "-filter_threads",
        &n,
    ])
    .args(["-filter_complex_threads", &n]);
    for (path, (w, h)) in [base, clip] {
        cmd.args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{w}x{h}"),
            "-framerate",
            "30",
            "-i",
        ])
        .arg(path);
    }
    cmd.args(["-filter_complex", &graph, "-map", "[out]", "-frames:v", &frames.to_string()])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"]);
    let out = common::output_within(cmd.stdin(Stdio::null()), std::time::Duration::from_secs(600)).expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg ({threads} threads): {}\n{graph}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout.len(), frames * base.1 .0 * base.1 .1 * 3 / 2, "{graph}");
    out.stdout
}

#[test]
#[ignore = "needs ffmpeg"]
fn checker_bl_a_translucent_blend_chain_gives_the_same_frames_on_every_thread_count() {
    let dir = scratch();
    let frames = 5usize;
    let mut checked = 0usize;
    let mut rng = Lcg(0xb1e5);
    for (si, ((bw, bh), (cw, ch), place)) in [
        ((130, 74), (66, 38), (14, -6)),
        ((642, 362), (386, 218), (51, -20)),
        ((642, 362), (642, 362), (0, 0)),
        ((1922, 1082), (1282, 722), (321, 7)),
    ]
    .into_iter()
    .enumerate()
    {
        // One frame of noise, written `frames` times: the fade's steps are what changes between the frames.
        let one = |w: usize, h: usize, rng: &mut Lcg| rng.bytes(w * h * 3 / 2);
        let (b, c) = (one(bw, bh, &mut rng), one(cw, ch, &mut rng));
        let write = |name: &str, frame: &[u8]| -> PathBuf {
            let p = dir.join(name);
            common::write_atomic(&p, &frame.repeat(frames));
            p
        };
        let base = write(&format!("base-{si}.yuv"), &b);
        let clip = write(&format!("clip-{si}.yuv"), &c);
        for (ci, chain) in [
            "colorchannelmixer=aa=0.6".to_string(),
            "colorchannelmixer=aa=0.043,fade=t=in:st=0.0333333:d=0.1:alpha=1,fade=t=out:st=0.1:d=0.1:alpha=1".to_string(),
            "colorchannelmixer=aa=0.5,fade=t=in:st=0:d=0.2:alpha=1,fade=t=in:st=0:d=0.1:alpha=1".to_string(),
        ]
        .into_iter()
        .enumerate()
        {
            let mode = [
                BlendMode::Multiply,
                BlendMode::Overlay,
                BlendMode::Difference,
                BlendMode::Screen,
            ][(si + ci) % 4];
            let tag = [None, Some("bt709"), None][ci];
            let reference = run(1, (&base, (bw, bh)), (&clip, (cw, ch)), &chain, mode, place, tag, frames);
            for threads in [2usize, 3, 4, 5, 7, 8, 13, 16] {
                let got = run(
                    threads,
                    (&base, (bw, bh)),
                    (&clip, (cw, ch)),
                    &chain,
                    mode,
                    place,
                    tag,
                    frames,
                );
                let differing = got.iter().zip(&reference).filter(|(a, b)| a != b).count();
                let first = got.iter().zip(&reference).position(|(a, b)| a != b);
                eprintln!(
                    "{bw}x{bh} clip {cw}x{ch} at {place:?} {mode:?} {tag:?} chain {ci}: {threads} threads vs 1: {differing} differing samples (first {first:?})"
                );
                assert_eq!(
                    differing, 0,
                    "{bw}x{bh} clip {cw}x{ch} {mode:?} {chain}: {threads} threads differ from 1 at byte {first:?}"
                );
                checked += 1;
            }
        }
    }
    eprintln!("{checked} thread counts held to the single-threaded run, 0 differing samples");
}
