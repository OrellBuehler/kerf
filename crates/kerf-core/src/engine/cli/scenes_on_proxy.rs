//! Scene detection and contact sheets read a ready proxy in place of the original
//! (gpu-media WP3), which is only allowed because **a proxy answers a time with the frame the
//! original does** (`#[ignore]`d: these drive the real `ffmpeg` and `ffprobe`).
//!
//! What is measured, on clips of five shots with cuts of different strength (so some sit under
//! the detector's 0.4 and some over), for each kind of source the proxy builder treats
//! specially:
//!
//! * the *scene score of every frame*, original against proxy: the same frames at the same
//!   times, and no score more than a few hundredths apart. A cut list can only differ where an
//!   original score lies within that distance of the threshold, so each case also asserts that
//!   **none does** — a clip that fails that is a bad fixture, not a flaky test;
//! * the cut lists themselves at the default threshold and at a lower one, frame-exact;
//! * sources: 8-bit H.264 with B-frames and audio, 10-bit HEVC, 4:2:2 10-bit HEVC, a video
//!   that starts 64 ms and 1.5 s after its audio (head-padded `.lead.mp4` proxies), a portrait
//!   clip carried by a display matrix, a variable-frame-rate Matroska file;
//! * the control: the *unpadded* proxy of a late-starting source finds every cut `lead`
//!   seconds early, which is what the head pad prevents;
//! * an HDR (HLG) source, where the proxy moves the scores by a fifth of the scale: scene detection
//!   keeps reading the original, and a transport stream is not read from its proxy either;
//! * a contact sheet shows the same frame in every cell from the proxy as from the original,
//!   from the start and from a seek, for an ordinary and a head-padded proxy.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored scenes_on_proxy`
//! (and again with `KERF_FFMPEG` / `KERF_FFPROBE` at the pinned build.)

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::*;
use crate::analysis::{FfmpegSceneDetector, SceneDetector};
use crate::clip_timing::is_head_padded_proxy;
use crate::engine::test_support::{av_asset, remove_proxy, ProxyGuard, StatusBounded};
use crate::model::Asset;
use crate::project::Project;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-scenes-proxy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn ffmpeg(args: &[&str]) -> bool {
    command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .status_bounded()
        .is_ok_and(|s| s.success())
}

/// The major version of this machine's ffmpeg (`9` for `n9.0.2-...`, `6` for `6.1.1-3ubuntu5`).
fn ffmpeg_major() -> u32 {
    let out = command(&ffmpeg_bin()).arg("-version").output().expect("run ffmpeg");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(2)
        .map(|v| {
            v.trim_start_matches('n')
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// FFmpeg 6 and older write an mp4 at a constant frame rate by default, which regrids a
/// variable-frame-rate source's frames and every late head (`engine-cli.md`): the proxy is then
/// within half a frame of the original, not exactly on it, and the cases that need it say nothing.
fn proxy_keeps_timestamps() -> bool {
    ffmpeg_major() >= 7
}

fn path_str(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

/// Seconds per shot.
const SHOT: f64 = 1.2;

/// Five shots, `SHOT` seconds each, of unlike picture (a test card with moving parts, a fractal
/// zoom, a moving colour wave, a cellular automaton, an RGB card) under film-grain noise: cuts
/// of scores from about 0.35 to 1, and a grain the proxy's compression changes. Every part is
/// deterministic (`gradients` is not: it differs from one run to the next whatever its seed), so
/// the scores a case is checked against do not wander. `None` when this ffmpeg lacks a generator.
fn shots(dir: &Path, name: &str, w: u32, h: u32, fps: u32) -> Option<PathBuf> {
    let out = dir.join(name);
    let d = SHOT.to_string();
    let (size, rate) = (format!("{w}x{h}"), fps.to_string());
    let inputs = [
        format!("testsrc2=s={size}:r={rate}:d={d}"),
        format!("mandelbrot=s=480x270:r={rate}:end_scale=0.01"),
        format!(
            "nullsrc=s={size}:r={rate}:d={d},format=yuv420p,\
             geq=lum='128+90*sin((X+Y)/37+N/9)':cb='128+60*sin(X/91+N/17)':cr='128+60*cos(Y/73-N/13)'"
        ),
        format!("life=s=240x135:r={rate}:ratio=0.08:mold=20:random_seed=7"),
        format!("rgbtestsrc=s={size}:r={rate}:d={d}"),
    ];
    let graph = format!(
        "[0:v]format=yuv420p,setsar=1[a];\
         [1:v]trim=duration={d},setpts=PTS-STARTPTS,scale={w}:{h}:flags=bilinear,format=yuv420p,setsar=1[b];\
         [2:v]format=yuv420p,setsar=1[c];\
         [3:v]trim=duration={d},setpts=PTS-STARTPTS,scale={w}:{h}:flags=neighbor,format=yuv420p,setsar=1[e];\
         [4:v]format=yuv420p,setsar=1[f];\
         [a][b][c][e][f]concat=n=5:v=1:a=0,noise=all_strength=7:all_flags=t[v]"
    );
    let mut args: Vec<&str> = Vec::new();
    for input in &inputs {
        args.extend(["-f", "lavfi", "-i", input]);
    }
    args.extend([
        "-filter_complex",
        &graph,
        "-map",
        "[v]",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "12",
        "-pix_fmt",
        "yuv420p",
        "-r",
        &rate,
        path_str(&out),
    ]);
    ffmpeg(&args).then_some(out)
}

/// `src` re-encoded as `codec_args` with a sine track alongside (so the container has an audio
/// stream whose start the video's can differ from).
fn encoded(dir: &Path, src: &Path, name: &str, codec_args: &[&str]) -> Option<PathBuf> {
    let out = dir.join(name);
    let secs = (SHOT * 5.0 + 1.0).to_string();
    let sine = format!("sine=frequency=440:sample_rate=48000:duration={secs}");
    let mut args = vec!["-i", path_str(src), "-f", "lavfi", "-i", &sine, "-map", "0:v", "-map", "1:a"];
    args.extend(codec_args);
    args.extend(["-c:a", "aac", "-b:a", "64k", "-shortest", path_str(&out)]);
    ffmpeg(&args).then_some(out)
}

const H264: [&str; 8] = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "22", "-bf", "2"];

fn hevc(pix_fmt: &'static str) -> [&'static str; 10] {
    [
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-crf",
        "24",
        "-x265-params",
        "log-level=error",
        "-pix_fmt",
        pix_fmt,
    ]
}

/// `src` with its video held back `lead` seconds against its audio (an empty edit in the mp4,
/// the way a camera that opens its microphone first writes it).
fn late(dir: &Path, src: &Path, name: &str, lead: f64) -> Option<PathBuf> {
    let video = dir.join(format!("v-{name}"));
    let audio = dir.join(format!("a-{name}"));
    (ffmpeg(&["-i", path_str(src), "-map", "0:v", "-c", "copy", path_str(&video)])
        && ffmpeg(&["-i", path_str(src), "-map", "0:a", "-c", "copy", path_str(&audio)]))
    .then(|| late_mux(dir, &video, &audio, name, lead))
    .flatten()
}

/// `src` as a phone writes a portrait recording: the landscape frame plus a display matrix.
fn turned(dir: &Path, src: &Path, name: &str) -> Option<PathBuf> {
    let out = dir.join(name);
    ffmpeg(&["-display_rotation", "-90", "-i", path_str(src), "-c", "copy", path_str(&out)]).then_some(out)
}

/// `src` with every sixth frame dropped and the timestamps kept, in Matroska (millisecond ticks).
fn variable_rate(dir: &Path, src: &Path, name: &str) -> Option<PathBuf> {
    let out = dir.join(name);
    ffmpeg(&[
        "-i",
        path_str(src),
        "-an",
        "-vf",
        "select='not(eq(mod(n,6),2))'",
        fps_mode_flag(),
        "vfr",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "22",
        path_str(&out),
    ])
    .then_some(out)
}

/// `src` as 10-bit HLG BT.2020 (tagged), by converting the SDR card up. `None` without libzimg.
fn hlg(dir: &Path, src: &Path, name: &str) -> Option<PathBuf> {
    let out = dir.join(name);
    let tagged = dir.join(format!("sdr-{name}"));
    (ffmpeg(&[
        "-i",
        path_str(src),
        "-vf",
        "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "12",
        "-pix_fmt",
        "yuv420p",
        path_str(&tagged),
    ]) && ffmpeg(&[
        "-i",
        path_str(&tagged),
        "-vf",
        "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-crf",
        "22",
        "-x265-params",
        "log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc",
        "-pix_fmt",
        "yuv420p10le",
        "-tag:v",
        "hvc1",
        path_str(&out),
    ]))
    .then_some(out)
}

/// `(time, score)` of every frame of `path`, the score being what `select='gt(scene,t)'`
/// compares to its threshold on the frames [`detect_scenes`] looks at.
fn scene_scores(path: &Path) -> Vec<(f64, f64)> {
    let out = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-nostats", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-vf",
            &format!("scale='min({SCENE_DETECT_WIDTH},iw)':-2:flags=bilinear,select='gte(scene,0)',metadata=print:key=lavfi.scene_score:file=-"),
            "-f",
            "null",
            "-",
        ])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let (mut time, mut scores) = (f64::NAN, Vec::new());
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(t) = line.split("pts_time:").nth(1) {
            time = t.trim().parse().expect("a time");
        } else if let Some(s) = line.strip_prefix("lavfi.scene_score=") {
            scores.push((time, s.trim().parse().expect("a score")));
        }
    }
    scores
}

/// Two times are the same frame's when they agree to far better than a frame (ffmpeg prints
/// them to six significant digits, and a proxy's time base is its own).
const SAME_FRAME: f64 = 0.0005;

const THRESHOLDS: [f64; 2] = [0.4, 0.3];

/// The proxy of `original` finds the scene changes the original does. Returns the cuts at the default threshold.
fn same_cuts(label: &str, original: &Path, padded: bool) -> Vec<f64> {
    let proxy = generate_proxy(original, PROXY_MAX_WIDTH).expect("proxy");
    let _cleanup = ProxyGuard(proxy.clone());
    assert_eq!(is_head_padded_proxy(&proxy.to_string_lossy()), padded, "{label}: {proxy:?}");

    let (want, mut got) = (scene_scores(original), scene_scores(&proxy));
    if padded {
        // The pad is a clone of the first frame at time zero, which the original has no frame for;
        // it scores nothing, and the real first frame (identical to it) scores nothing either.
        let clone = got.remove(0);
        assert_eq!(clone, (0.0, 0.0), "{label}: the head pad");
    }
    assert_eq!(
        got.len(),
        want.len(),
        "{label}: the proxy has {} frames, the original {}",
        got.len(),
        want.len()
    );
    let mut max_delta = 0.0f64;
    for ((wt, ws), (gt, gs)) in want.iter().zip(&got) {
        assert!(
            (wt - gt).abs() < SAME_FRAME,
            "{label}: a frame at {wt}s of the original is at {gt}s of the proxy"
        );
        max_delta = max_delta.max((ws - gs).abs());
    }
    let margin = THRESHOLDS
        .iter()
        .flat_map(|t| want.iter().map(move |(_, s)| (s - t).abs()))
        .fold(f64::MAX, f64::min);
    assert!(
        margin > max_delta,
        "{label}: a score lies {margin:.4} from a threshold, closer than the proxy moves a score ({max_delta:.4}) - \
         the cut list could differ by chance; change the fixture"
    );

    let mut cuts = Vec::new();
    for threshold in THRESHOLDS {
        let (want, got) = (
            detect_scenes(original, threshold).unwrap(),
            detect_scenes(&proxy, threshold).unwrap(),
        );
        assert_eq!(got.len(), want.len(), "{label} at {threshold}: {got:?} against {want:?}");
        for (g, w) in got.iter().zip(&want) {
            assert!((g - w).abs() < SAME_FRAME, "{label} at {threshold}: {got:?} against {want:?}");
        }
        if threshold == THRESHOLDS[0] {
            cuts = want;
        }
    }
    eprintln!("{label}: cuts {cuts:?}, scores differ by at most {max_delta:.4}, nearest to a threshold {margin:.4}");
    cuts
}

/// An asset for `path`, as the import would build it.
fn imported(path: &Path) -> Asset {
    let probed = probe(path).expect("probe");
    let mut asset = av_asset(Uuid::new_v4(), probed.duration);
    asset.path = path.to_string_lossy().into_owned();
    asset.streams = probed.streams;
    asset
}

/// A fixture the build of this ffmpeg may not be able to make.
macro_rules! fixture {
    ($made:expr, $what:expr) => {
        match $made {
            Some(p) => p,
            None => {
                eprintln!("skipped: this ffmpeg cannot make {}", $what);
                return;
            }
        }
    };
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn scenes_on_proxy_ordinary_sources() {
    let dir = scratch("ordinary");
    let base = fixture!(shots(&dir, "base.mp4", 1920, 1080, 30), "the shots");
    let h264 = fixture!(encoded(&dir, &base, "h264.mp4", &H264), "an H.264 clip");
    let cuts = same_cuts("h264 1080p30 with audio", &h264, false);
    assert_eq!(cuts.len(), 3, "the fixture has three cuts over 0.4: {cuts:?}");

    let base25 = fixture!(shots(&dir, "base25.mp4", 1280, 720, 25), "the shots");
    for (name, pix, args) in [
        ("hevc10.mp4", "yuv420p10le", hevc("yuv420p10le")),
        ("hevc422.mp4", "yuv422p10le", hevc("yuv422p10le")),
    ] {
        let clip = fixture!(encoded(&dir, &base25, name, &args), pix);
        same_cuts(name, &clip, false);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn scenes_on_proxy_late_starting_video_is_padded_and_keeps_its_cut_times() {
    let dir = scratch("late");
    let base = fixture!(shots(&dir, "base.mp4", 1280, 720, 30), "the shots");
    let h264 = fixture!(encoded(&dir, &base, "h264.mp4", &H264), "an H.264 clip");
    let reference = detect_scenes(&h264, THRESHOLDS[0]).unwrap();
    assert!(reference.len() >= 2, "{reference:?}");
    for (i, lead) in [0.064, 0.08, 1.5].into_iter().enumerate() {
        let clip = fixture!(late(&dir, &h264, &format!("late{i}.mp4"), lead), "a late-starting clip");
        let traits = source_traits(&clip).expect("probe");
        assert!(
            (traits.lead - lead).abs() < 0.005,
            "lead {lead}: the probe saw {}",
            traits.lead
        );
        let cuts = same_cuts(&format!("lead {lead}"), &clip, true);
        assert_eq!(
            cuts.len(),
            reference.len(),
            "the cuts are the same cuts: {cuts:?} against {reference:?}"
        );

        // The control. A proxy made the way FFmpeg 7+ writes one without the pad starts at the
        // video, so it finds every cut `lead` seconds earlier than the original does.
        if !proxy_keeps_timestamps() {
            continue;
        }
        let plain = dir.join(format!("plain{i}.mp4"));
        let args = build_proxy_args(
            path_str(&clip),
            path_str(&plain),
            2,
            PROXY_MAX_WIDTH,
            "libx264",
            None,
            None,
            None,
        );
        assert!(command(&ffmpeg_bin())
            .args(&args)
            .stdin(Stdio::null())
            .status_bounded()
            .unwrap()
            .success());
        let early = detect_scenes(&plain, THRESHOLDS[0]).unwrap();
        let original = detect_scenes(&clip, THRESHOLDS[0]).unwrap();
        assert_eq!(early.len(), original.len());
        for (e, o) in early.iter().zip(&original) {
            assert!((o - e - lead).abs() < 0.002, "lead {lead}: unpadded {e} against {o}");
        }
    }

    // A transport stream cannot be padded (its demuxer rebases a read without the audio), so its
    // proxy is not trusted for times: the scene detector and the sheet read the original.
    let late = dir.join("late2.mp4");
    let ts = dir.join("late.ts");
    if ffmpeg(&["-i", path_str(&late), "-c", "copy", "-f", "mpegts", path_str(&ts)]) {
        assert_eq!(proxy_fidelity(&ts), ProxyFidelity::default());
        let proxy = generate_proxy(&ts, PROXY_MAX_WIDTH).expect("proxy");
        let _cleanup = ProxyGuard(proxy.clone());
        let (original, early) = (
            detect_scenes(&ts, THRESHOLDS[0]).unwrap(),
            detect_scenes(&proxy, THRESHOLDS[0]).unwrap(),
        );
        eprintln!("transport stream, video 1.5 s late: cuts {original:?} from the original, {early:?} from its proxy");
        let asset = imported(&ts);
        assert_eq!((Project::scoring_proxy(&asset), Project::sheet_proxy(&asset)), (None, None));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn scenes_on_proxy_rotated_and_variable_rate_sources() {
    let dir = scratch("odd");
    let base = fixture!(shots(&dir, "base.mp4", 1280, 720, 30), "the shots");
    let h264 = fixture!(encoded(&dir, &base, "h264.mp4", &H264), "an H.264 clip");

    let portrait = fixture!(turned(&dir, &h264, "portrait.mp4"), "a display matrix");
    let probed = imported(&portrait);
    let video = probed.streams.iter().find(|s| s.kind == StreamKind::Video).unwrap();
    assert_eq!(
        (video.width, video.height),
        (Some(720), Some(1280)),
        "the probe reports the displayed size"
    );
    same_cuts("portrait by display matrix", &portrait, false);

    let vfr = fixture!(variable_rate(&dir, &h264, "vfr.mkv"), "a variable-rate Matroska clip");
    if proxy_keeps_timestamps() {
        same_cuts("variable frame rate, Matroska", &vfr, false);
    } else {
        eprintln!("skipped: this ffmpeg writes a constant-rate proxy, which regrids a variable-rate source");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An HDR source is scored in its HLG code values and its proxy in the tone-mapped SDR picture,
/// which moves a frame's score by a quarter of the scale: the cuts could be different cuts, so
/// scene detection reads the original, and a contact sheet (which only looks) the proxy.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn scenes_on_proxy_hlg_source_is_read_from_the_original_for_scores() {
    let dir = scratch("hlg");
    let base = fixture!(shots(&dir, "base.mp4", 1280, 720, 30), "the shots");
    let clip = fixture!(hlg(&dir, &base, "hlg.mp4"), "an HLG clip");
    assert_eq!(source_traits(&clip).and_then(|t| t.hdr), Some(Hdr::Hlg));
    assert_eq!(
        proxy_fidelity(&clip),
        ProxyFidelity {
            times: true,
            scores: false
        }
    );

    let proxy = generate_proxy(&clip, PROXY_MAX_WIDTH).expect("proxy");
    let _cleanup = ProxyGuard(proxy.clone());
    let (want, got) = (scene_scores(&clip), scene_scores(&proxy));
    assert_eq!(want.len(), got.len());
    let moved = want
        .iter()
        .zip(&got)
        .map(|((_, w), (_, g))| (w - g).abs())
        .fold(0.0, f64::max);
    eprintln!("HLG: the proxy moves a score by up to {moved:.3}");
    assert!(
        moved > 0.05,
        "the proxy no longer moves the scores: the HDR gate can be lifted"
    );

    let asset = imported(&clip);
    assert_eq!(Project::scoring_proxy(&asset), None);
    assert_eq!(Project::sheet_proxy(&asset).as_deref(), Some(proxy.as_path()));

    // The sheet from the proxy is tone-mapped once (the proxy already was), so it is as bright as
    // the original's, which the sheet maps itself.
    let mean = |jpeg: &[u8]| -> Vec<f64> {
        let file = dir.join("mean.jpg");
        std::fs::write(&file, jpeg).unwrap();
        let out = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&file)
            .args(["-vf", "scale=1:1:flags=area", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .output()
            .expect("run ffmpeg");
        out.stdout.iter().map(|v| f64::from(*v)).collect()
    };
    let (want, got) = (
        mean(&engine_sheet(&clip, 0.0, 6.0).0),
        mean(&engine_sheet(&proxy, 0.0, 6.0).0),
    );
    for (w, g) in want.iter().zip(&got) {
        assert!(
            (w - g).abs() < 8.0,
            "the sheet's average colour: {want:?} from the original, {got:?} from the proxy"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The analysis step reads the proxy when there is one and the original when there is not, or
/// when the proxy does not read. The proxy's slot is overwritten with a *different* clip, so which
/// file was read shows in the answer.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn scenes_on_proxy_the_detector_reads_a_ready_proxy_and_falls_back_to_the_original() {
    let dir = scratch("detector");
    let base = fixture!(shots(&dir, "base.mp4", 1280, 720, 30), "the shots");
    let original = fixture!(encoded(&dir, &base, "original.mp4", &H264), "an H.264 clip");
    let other = fixture!(
        {
            let out = dir.join("other.mp4");
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=1280x720:r=30:d=2,format=yuv420p",
                "-f",
                "lavfi",
                "-i",
                "color=c=white:s=1280x720:r=30:d=2,format=yuv420p",
                "-filter_complex",
                "[0:v][1:v]concat=n=2:v=1:a=0[v]",
                "-map",
                "[v]",
                "-c:v",
                "libx264",
                path_str(&out),
            ])
            .then_some(out)
        },
        "a black-to-white clip"
    );
    let asset = imported(&original);
    let want = detect_scenes(&original, 0.4).unwrap();
    let detector = FfmpegSceneDetector::default();
    assert_eq!(Project::scoring_proxy(&asset), None, "no proxy yet");
    assert_eq!(detector.detect_scenes(&asset).unwrap(), want, "no proxy: the original");

    let proxy = generate_proxy(&original, PROXY_MAX_WIDTH).expect("proxy");
    let _cleanup = ProxyGuard(proxy.clone());
    assert_eq!(Project::scoring_proxy(&asset).as_deref(), Some(proxy.as_path()));
    let from_proxy = detector.detect_scenes(&asset).unwrap();
    assert_eq!(from_proxy.len(), want.len());
    for (g, w) in from_proxy.iter().zip(&want) {
        assert!((g - w).abs() < SAME_FRAME, "{from_proxy:?} against {want:?}");
    }

    std::fs::copy(&other, &proxy).unwrap();
    let swapped = detector.detect_scenes(&asset).unwrap();
    assert_eq!(
        swapped.len(),
        1,
        "the detector read the file in the proxy's slot: {swapped:?}"
    );
    assert!((swapped[0] - 2.0).abs() < SAME_FRAME);

    std::fs::write(&proxy, b"not a movie").unwrap();
    assert_eq!(
        detector.detect_scenes(&asset).unwrap(),
        want,
        "a proxy that does not read: the original"
    );
    remove_proxy(&proxy);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A numbered clip: bit `k` of the frame number is the `k`-th eighth of the picture, white for 1.
fn numbered(dir: &Path, name: &str) -> Option<PathBuf> {
    let out = dir.join(name);
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=black:s=640x360:r=30:d=6,format=yuv420p,\
         geq=lum='if(bitand(trunc(N/pow(2,trunc(X*8/W))),1),235,16)':cb=128:cr=128",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "30",
        "-pix_fmt",
        "yuv420p",
        path_str(&out),
    ])
    .then_some(out)
}

/// The frame number each cell of a `columns` x `rows` sheet of a [`numbered`] clip shows.
fn sheet_numbers(dir: &Path, jpeg: &[u8], columns: usize, rows: usize) -> Vec<u32> {
    let file = dir.join("sheet.jpg");
    std::fs::write(&file, jpeg).unwrap();
    let dims = command(&ffprobe_bin())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&file)
        .output()
        .expect("run ffprobe");
    let dims = String::from_utf8_lossy(&dims.stdout);
    let (w, h): (usize, usize) = {
        let mut it = dims.trim().split(',').map(|v| v.parse().unwrap());
        (it.next().unwrap(), it.next().unwrap())
    };
    let raw = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&file)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .output()
        .expect("run ffmpeg");
    let (cw, ch) = (w / columns, h / rows);
    let mut numbers = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            let y = row * ch + ch / 2;
            numbers.push(
                (0..8)
                    .filter(|k| raw.stdout[(y * w + column * cw + (2 * k + 1) * cw / 16) * 3] > 125)
                    .map(|k| 1u32 << k)
                    .sum(),
            );
        }
    }
    numbers
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn scenes_on_proxy_contact_sheet_shows_the_same_frames() {
    let dir = scratch("sheet");
    let plain = fixture!(numbered(&dir, "plain.mp4"), "a numbered clip");
    let lead = fixture!(
        {
            let sine = dir.join("tone.m4a");
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=8",
                "-c:a",
                "aac",
                path_str(&sine),
            ])
            .then_some(())
            .and_then(|()| late_mux(&dir, &plain, &sine, "late.mp4", 0.5))
        },
        "a late-starting clip"
    );
    for (label, clip) in [("ordinary", plain), ("video 0.5 s after the audio", lead)] {
        let asset = imported(&clip);
        let proxy = generate_proxy(&clip, PROXY_MAX_WIDTH).expect("proxy");
        let _cleanup = ProxyGuard(proxy.clone());
        assert_eq!(Project::sheet_proxy(&asset).as_deref(), Some(proxy.as_path()));
        for (start, end) in [(None, None), (Some(1.0), Some(4.0))] {
            let start_s = start.unwrap_or(0.0);
            let end_s = end.unwrap_or(asset.duration).min(asset.duration);
            let (want, times) = engine_sheet(&clip, start_s, end_s);
            let (got, _) = engine_sheet(&proxy, start_s, end_s);
            let (from_project, _) = Project::decode_contact_sheet(&asset, start, end, 4, 2, 160, 2).unwrap();
            let want = sheet_numbers(&dir, &want, 4, 2);
            assert_eq!(
                sheet_numbers(&dir, &got, 4, 2),
                want,
                "{label} {start:?}..{end:?}: the proxy's sheet"
            );
            assert_eq!(
                sheet_numbers(&dir, &from_project, 4, 2),
                want,
                "{label} {start:?}..{end:?}: Project's sheet"
            );
            // (A cell past the end of the picture is black, number 0.)
            let shown: Vec<_> = want.iter().filter(|n| **n > 0).collect();
            assert!(
                shown.len() >= 4 && shown.windows(2).all(|p| p[0] < p[1]),
                "{label}: the clip never moved: {want:?} at {times:?}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn engine_sheet(path: &Path, start: f64, end: f64) -> (Vec<u8>, Vec<f64>) {
    contact_sheet(path, start, end, 4, 2, 160, 2).expect("contact sheet")
}

/// `video` and `audio` in one file with the video starting `lead` seconds in, ending with the video.
fn late_mux(dir: &Path, video: &Path, audio: &Path, name: &str, lead: f64) -> Option<PathBuf> {
    let out = dir.join(name);
    ffmpeg(&[
        "-itsoffset",
        &lead.to_string(),
        "-i",
        path_str(video),
        "-i",
        path_str(audio),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c",
        "copy",
        "-shortest",
        path_str(&out),
    ])
    .then_some(out)
}
