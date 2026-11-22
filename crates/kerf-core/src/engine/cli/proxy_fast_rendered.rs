//! The Fast (GPU) proxy pipeline against the real `ffmpeg` and `ffprobe` (`#[ignore]`d: they need the
//! binaries, a cache directory, and a few seconds each).
//!
//! **No GPU is needed and none is claimed.** What is measured here is everything around the hardware:
//!
//! * the size `fast_dims` computes is the one `scale='min(W,iw)':-2` makes (a random sweep, both builds);
//! * the guard (`check_fast_output`) passes the proxy x264 makes of ordinary sources, and refuses a
//!   cut one and a retimed one: it is the definition of "what the software encode would give";
//! * the sources the pipeline refuses are refused when they are real files (a rotated one, Matroska,
//!   MPEG-TS), and the ones it accepts are accepted;
//! * every vendor's pipeline **fails cleanly** where its hardware is not: an error and a reason, no hang,
//!   no panic, nothing left behind;
//! * with the setting **on** and no hardware, the capability check says no, once, and every proxy is built in
//!   software exactly as with the setting off;
//! * the whole build through the real runner, with a **stand-in for the GPU** (a script that runs the real
//!   ffmpeg on the software equivalent of the vendor's argv, and logs the argv it was given): a good fast
//!   build lands with its encoder in the sidecar; one the guard refuses, and one the hardware fails, are
//!   built again in software, are labelled so, and a hardware failure turns the pipeline off on its own flag.
//!
//! **State.** CI runs every ignored test of the crate in one process, in parallel, on a runner whose
//! `-hwaccel auto` aborts (no `libva-drm`), where the first software proxy of any test clears the
//! previews' learned hardware-decode flag for the process. These tests therefore take nothing from the
//! process: the setting, the conviction, the capability check and the `ffmpeg` they drive are pinned on
//! their own thread (`with_fast_world`, `with_pinned_fast_check`, `with_ffmpeg`), they compare no
//! process-wide flag (`HW_ENCODE_OK`, `HWACCEL_PROVEN_BAD`: another test may set them), and they ask
//! `why_not_fast` first so that a build that quietly lands in software names the reason. The ones that need
//! a whole process (the check and the queue's real workers) run in a child.
//!
//! //! `cargo test -p kerf-core --no-default-features -- --ignored proxy_fast_rendered`
//! (and again with `KERF_FFMPEG` / `KERF_FFPROBE` at the pinned build.)

#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};

use super::*;
#[cfg(unix)]
use crate::engine::test_support::remove_proxy;
use crate::engine::test_support::{ProxyGuard, StatusBounded};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-fast-proxy-{tag}-{}", std::process::id()));
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

/// An ordinary phone-like source: H.264 4:2:0 8-bit with B-frames, BT.709 tagged, `secs` long.
fn make_clip(path: &Path, size: &str, fps: u32, secs: f64) {
    let source = format!("testsrc2=s={size}:r={fps}:d={secs}");
    assert!(
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &source,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-f",
            "mp4",
            path.to_str().unwrap(),
        ]),
        "could not make the clip"
    );
}

fn facts_of(clip: &Path) -> FastFacts {
    FastFacts {
        traits: source_traits(clip).expect("the source probes"),
        stored: read_stored_video(clip).expect("the source is read"),
    }
}

/// The vendor pipelines' argv stand-in: a script that **is** the hardware where there is none. Given the
/// AMF pipeline's argv it runs `real` (an ffmpeg) on the software equivalent (no `-hwaccel`, `scale` for
/// `vpp_amf`, x264 for `h264_amf` and the rate control flags dropped), after `delay` seconds (a GPU that
/// takes its time), and appends the argv it was given to `log`. The encoder test of `h264_amf` (one frame
/// from a generated picture, which the check and `hw_encoders` run) succeeds on x264 too; any other argv
/// goes to `real` untouched. `mode`: `pass` is a pipeline that works, `truncate` one whose output stops
/// after seven frames, `fail` one with no device.
#[cfg(unix)]
fn write_stand_in(dir: &Path, mode: &str, log: &Path, real: &str, delay: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = format!(
        r#"#!/bin/sh
real='{real}'
mode='{mode}'
log='{log}'
delay='{delay}'
hw=0; enc=0
for a in "$@"; do [ "$a" = amf ] && hw=1; [ "$a" = h264_amf ] && enc=1; done
if [ $hw = 0 ] && [ $enc = 0 ]; then exec "$real" "$@"; fi
if [ $hw = 1 ]; then
  echo "$@" >> "$log"
  if [ "$mode" = fail ]; then
    echo "[AMF @ 0x1] DLL libamfrt64.so.1 failed to open" >&2
    echo "Device creation failed: -1313558101." >&2
    exit 1
  fi
  sleep "$delay"
fi
n=$#
while [ "$n" -gt 0 ]; do
  a="$1"; shift; n=$((n-1))
  case "$a" in
    -hwaccel|-hwaccel_output_format|-rc|-qp_i|-qp_p|-quality|-max_b_frames) shift; n=$((n-1));;
    -vf)
      v="$1"; shift; n=$((n-1))
      w=$(echo "$v" | sed -E 's/.*w=([0-9]+):h=([0-9]+).*/\1/')
      h=$(echo "$v" | sed -E 's/.*w=([0-9]+):h=([0-9]+).*/\2/')
      set -- "$@" -vf "scale=$w:$h:flags=bilinear,format=nv12"
      if [ "$mode" = truncate ]; then set -- "$@" -frames:v 7; fi;;
    -c:v)
      v="$1"; shift; n=$((n-1))
      if [ "$v" = h264_amf ]; then set -- "$@" -c:v libx264 -preset ultrafast; [ $hw = 1 ] && set -- "$@" -crf 24; else set -- "$@" -c:v "$v"; fi;;
    *) set -- "$@" "$a";;
  esac
done
exec "$real" "$@"
"#,
        log = log.display()
    );
    let path = dir.join(format!("stand-in-{mode}.sh"));
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// What a proxy build told its hooks.
#[derive(Default)]
struct Told {
    pipelines: Vec<ProxyPipeline>,
    progress: Vec<f64>,
}

fn build(clip: &Path, secs: f64) -> (Result<PathBuf>, Told) {
    let mut told = Told::default();
    let (mut seen, mut ran) = (Vec::new(), Vec::new());
    let built = generate_proxy_with(
        clip,
        PROXY_MAX_WIDTH,
        ProxyRun {
            reservation: Some(cpu::reserve()),
            duration: Some(secs),
            progress: &mut |f| seen.push(f),
            cancel: &|| false,
            pipeline: &mut |p| ran.push(p),
        },
    );
    told.progress = seen;
    told.pipelines = ran;
    (built, told)
}

#[cfg(unix)]
fn parts_left_beside(proxy: &Path) -> Vec<String> {
    let stem = proxy.file_stem().unwrap().to_string_lossy().into_owned();
    std::fs::read_dir(proxy.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(&stem) && n.ends_with(".part"))
        .collect()
}

// ---- the size -------------------------------------------------------------------------------------

#[test]
#[ignore = "drives the ffmpeg binary"]
fn proxy_dims_equal_what_scale_makes() {
    // A small deterministic generator: sizes of every shape a phone, a camera and a 360 rig make.
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move |lo: u32, hi: u32| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        lo + (state % u64::from(hi - lo + 1)) as u32
    };
    let mut checked = 0;
    for _ in 0..80 {
        let (iw, ih) = (next(200, 8000), next(120, 4500));
        let width = [720, 1080, 1280, 3072][next(0, 3) as usize];
        let out = command(&ffmpeg_bin())
            .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("nullsrc=s={iw}x{ih}:d=0.04:r=25"))
            .args(["-vf", &format!("scale='min({width},iw)':-2:flags=bilinear")])
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .expect("ffmpeg runs");
        let w = width.min(iw);
        // `fast_dims` also refuses sizes the hardware does not take: compare the rest.
        let h = 2 * ((u64::from(w) * u64::from(ih) + u64::from(iw)) / (2 * u64::from(iw)));
        assert_eq!(out.stdout.len() as u64, u64::from(w) * h, "{iw}x{ih} at {width}");
        if let Some(dims) = fast_dims(iw, ih, width) {
            assert_eq!(
                u64::from(dims.0) * u64::from(dims.1),
                u64::from(w) * h,
                "{iw}x{ih} at {width}"
            );
            checked += 1;
        }
    }
    eprintln!("{checked} of 80 sizes were ones the hardware encoders take; every one equals what scale made");
}

// ---- the guard against x264's own proxies ----------------------------------------------------------------

#[test]
#[ignore = "drives the ffmpeg binary"]
fn the_guard_passes_the_software_proxy_of_ordinary_sources_and_refuses_a_cut_or_a_retimed_one() {
    let dir = scratch("guard");
    for (name, size, fps) in [
        ("1080p30", "1920x1080", 30),
        ("1440p25", "2560x1440", 25),
        ("720p60", "1280x720", 60),
    ] {
        let clip = dir.join(format!("{name}.mp4"));
        make_clip(&clip, size, fps, 3.0);
        let facts = facts_of(&clip);
        let (class, dims) = fast_eligible(&facts, PROXY_MAX_WIDTH).unwrap_or_else(|why| panic!("{name} is refused: {why}"));
        assert_eq!(class, SourceClass::H264);
        let proxy = generate_proxy(&clip, PROXY_MAX_WIDTH).expect("a software proxy");
        let _guard = ProxyGuard(proxy.clone());
        let expect = FastExpect::of(&facts.stored, dims).unwrap();
        let read = read_fast_output(&proxy).expect("the proxy reads");
        eprintln!(
            "{name}: source {} frames {:.3}s {:?} -> proxy {}x{} {} frames first {:.4} last {:.4} {:?}",
            expect.frames,
            expect.duration,
            expect.color.space,
            read.width,
            read.height,
            read.pts.len(),
            read.pts[0],
            read.pts[read.pts.len() - 1],
            read.color.space
        );
        check_fast_output(&expect, &read).unwrap_or_else(|why| panic!("{name}: x264's own proxy is refused: {why}"));
        // The same source cut short (an encoder that died), and the same frames over another time (a retime).
        let cut = dir.join(format!("{name}-cut.mp4"));
        assert!(ffmpeg(&[
            "-i",
            proxy.to_str().unwrap(),
            "-c",
            "copy",
            "-frames:v",
            &(expect.frames / 2).to_string(),
            "-f",
            "mp4",
            cut.to_str().unwrap()
        ]));
        let why = check_fast_output(&expect, &read_fast_output(&cut).unwrap()).expect_err("a cut proxy");
        assert!(why.contains("frames"), "{why}");
        let slow = dir.join(format!("{name}-slow.mp4"));
        assert!(ffmpeg(&[
            "-i",
            proxy.to_str().unwrap(),
            "-vf",
            "setpts=1.25*PTS",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "1",
            fps_mode_flag(),
            "passthrough",
            "-f",
            "mp4",
            slow.to_str().unwrap()
        ]));
        let why = check_fast_output(&expect, &read_fast_output(&slow).unwrap()).expect_err("a retimed proxy");
        assert!(why.contains("runs"), "{why}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_variable_frame_rate_source_encoded_the_way_the_fast_pipeline_asks_passes_the_guard() {
    let dir = scratch("vfr");
    let clip = dir.join("vfr.mp4");
    // Frames at irregular times: a phone's variable-rate clip.
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1920x1080:r=30:d=4",
        "-vf",
        "setpts='N/30/TB+0.012*sin(N*1.7)/TB'",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        fps_mode_flag(),
        "passthrough",
        "-f",
        "mp4",
        clip.to_str().unwrap(),
    ]));
    let facts = facts_of(&clip);
    let (_, dims) = fast_eligible(&facts, PROXY_MAX_WIDTH).expect("a variable-rate clip is eligible");
    let proxy = dir.join("vfr-proxy.mp4");
    let plan = FastPlan {
        vendor: FastVendor::Amf,
        device: None,
        width: dims.0,
        height: dims.1,
        fps_flag: fps_mode_flag(),
    };
    // x264 with the one flag set the fast argv shares with it: all-intra, no B-frames, timestamps kept (`vfr`).
    assert!(ffmpeg(&[
        "-i",
        clip.to_str().unwrap(),
        "-an",
        "-vf",
        &format!("scale={}:{}:flags=bilinear", plan.width, plan.height),
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "1",
        "-bf",
        "0",
        plan.fps_flag,
        "vfr",
        "-f",
        "mp4",
        proxy.to_str().unwrap(),
    ]));
    let read = read_fast_output(&proxy).unwrap();
    let expect = FastExpect::of(&facts.stored, dims).unwrap();
    eprintln!(
        "vfr: source {} frames {:.3}s -> proxy {} frames first {:.4} last {:.4}",
        expect.frames,
        expect.duration,
        read.pts.len(),
        read.pts[0],
        read.pts[read.pts.len() - 1]
    );
    check_fast_output(&expect, &read).expect("a variable-rate proxy that kept every frame and timestamp");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- who is refused, on real files ------------------------------------------------------------------------

#[test]
#[ignore = "drives the ffmpeg binary"]
fn the_sources_the_pipeline_cannot_vouch_for_are_refused_when_they_are_real_files() {
    let dir = scratch("refused");
    let ok = dir.join("ok.mp4");
    make_clip(&ok, "1920x1080", 30, 2.0);
    assert!(fast_eligible(&facts_of(&ok), PROXY_MAX_WIDTH).is_ok());

    // A phone held upright: the display matrix turns it (FFmpeg 6 and newer write it from `-display_rotation`,
    // older ones from the `rotate` tag).
    let rotated = dir.join("rotated.mp4");
    let turned = ffmpeg(&[
        "-display_rotation",
        "90",
        "-i",
        ok.to_str().unwrap(),
        "-c",
        "copy",
        "-f",
        "mp4",
        rotated.to_str().unwrap(),
    ]) || ffmpeg(&[
        "-i",
        ok.to_str().unwrap(),
        "-c",
        "copy",
        "-metadata:s:v:0",
        "rotate=90",
        "-f",
        "mp4",
        rotated.to_str().unwrap(),
    ]);
    assert!(turned, "could not make a rotated clip");
    let why = fast_eligible(&facts_of(&rotated), PROXY_MAX_WIDTH).expect_err("a rotated source");
    assert!(why.contains("rotated"), "{why}");

    // Matroska keeps no frame count in its index; MPEG-TS no stored timestamps.
    let mkv = dir.join("clip.mkv");
    assert!(ffmpeg(&["-i", ok.to_str().unwrap(), "-c", "copy", mkv.to_str().unwrap()]));
    let why = fast_eligible(&facts_of(&mkv), PROXY_MAX_WIDTH).expect_err("a Matroska source");
    assert!(why.contains("how many frames"), "{why}");
    let ts = dir.join("clip.ts");
    assert!(ffmpeg(&["-i", ok.to_str().unwrap(), "-c", "copy", ts.to_str().unwrap()]));
    let why = fast_eligible(&facts_of(&ts), PROXY_MAX_WIDTH).expect_err("an MPEG-TS source");
    assert!(why.contains("MPEG-TS"), "{why}");

    // A video that starts after its container (the sound starts first).
    let late = dir.join("late.mp4");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1920x1080:r=30:d=2",
        "-f",
        "lavfi",
        "-i",
        "sine=d=2.5",
        "-itsoffset",
        "0.5",
        "-i",
        ok.to_str().unwrap(),
        "-map",
        "2:v",
        "-map",
        "1:a",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "mp4",
        late.to_str().unwrap()
    ]));
    if let Err(why) = fast_eligible(&facts_of(&late), PROXY_MAX_WIDTH) {
        eprintln!("late start: refused: {why}");
    }

    // 10-bit H.264 (High 10): no hardware decoder.
    let high10 = dir.join("high10.mp4");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1920x1080:r=30:d=2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p10le",
        high10.to_str().unwrap()
    ]));
    let why = fast_eligible(&facts_of(&high10), PROXY_MAX_WIDTH).expect_err("High 10");
    assert!(why.contains("hardware decoder"), "{why}");

    // Full range (a `yuvj420p` / `pc` clip).
    let full = dir.join("full.mp4");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1920x1080:r=30:d=2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuvj420p",
        "-color_range",
        "pc",
        full.to_str().unwrap()
    ]));
    let why = fast_eligible(&facts_of(&full), PROXY_MAX_WIDTH).expect_err("full range");
    assert!(why.contains("full range"), "{why}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- no hardware: every vendor fails cleanly ----------------------------------------------------------------

/// Every `ffmpeg` this checkout can run: the one on `PATH` (or `KERF_FFMPEG`), and the pinned build when it is here.
fn builds() -> Vec<String> {
    let mut builds = vec![ffmpeg_bin()];
    let pinned = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu");
    if pinned.is_file() && pinned.to_str() != Some(builds[0].as_str()) {
        builds.push(pinned.to_string_lossy().into_owned());
    }
    builds
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn every_vendors_pipeline_fails_cleanly_where_its_hardware_is_not() {
    let dir = scratch("probe");
    for bin in builds() {
        let clips = make_probe_clips(&bin, &dir);
        assert!(
            clips.iter().any(|c| c.class == SourceClass::H264),
            "{bin} could not make the H.264 probe clip"
        );
        // The listings of this build: what it could run, were there hardware.
        let (hwaccels, filters) = (listing(&bin, "-hwaccels"), listing(&bin, "-filters"));
        for vendor in FastVendor::ALL {
            let in_build = vendor_in_listing(vendor, &hwaccels, &filters);
            // The flag this build takes (`fps_mode_flag()` is the one of the build the process started with).
            let help = output_bounded(
                command(&bin).args(["-hide_banner", "-loglevel", "quiet", "-h", "full"]),
                CAPABILITY_PROBE_TIMEOUT,
                0,
            )
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
            let plan = FastPlan {
                vendor,
                device: (vendor == FastVendor::Vaapi).then(|| "/dev/dri/renderD128".to_string()),
                width: PROBE_TARGET.0,
                height: PROBE_TARGET.1,
                fps_flag: fps_mode_flag_for(help.contains("-fps_mode"), help.contains("-vsync")),
            };
            let started = Instant::now();
            let h264 = clips.iter().find(|c| c.class == SourceClass::H264).unwrap();
            let outcome = probe_pipeline_on(&bin, &plan, h264, &dir);
            let why = outcome.expect_err("there is no GPU here");
            assert!(!why.trim().is_empty(), "{vendor:?} fails with no reason");
            assert!(
                started.elapsed() < HW_PROBE_TIMEOUT,
                "{vendor:?} took {:?}",
                started.elapsed()
            );
            eprintln!(
                "{bin} {}: in this build: {}; whole pipeline: {why}",
                vendor.name(),
                in_build.map_or_else(|e| e, |()| "yes".to_string())
            );
            // Nothing is left behind.
            let left: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with("out-"))
                .collect();
            assert!(left.is_empty(), "{left:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn fast_gpu_on_a_machine_with_no_gpu_encoder_checks_once_says_why_not_and_builds_every_proxy_in_software() {
    // The gate: this build asks for the hardware's slot (or the cores', behind it). The setting and the
    // conviction are this thread's own (`with_fast_world`), so no other test sees the setting on.
    let _gate = cpu::test_lock();
    allow_real_fast_probe();
    let dir = scratch("no-gpu");
    let clip = dir.join("clip.mp4");
    make_clip(&clip, "1920x1080", 30, 3.0);
    with_fast_world(true, || {
        // The first look never waits: it starts the check and says so.
        let first = fast_pipeline_status();
        eprintln!("first look: {first:?}");
        // The check finishes in the background.
        let started = Instant::now();
        let status = loop {
            let status = fast_pipeline_status();
            if status.checked {
                break status;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(120),
                "the check never finished"
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        eprintln!("checked after {:?}: {status:?}", started.elapsed());
        assert!(!status.available && status.pipeline.is_none());
        let reason = status.reason.expect("a reason");
        assert!(!reason.trim().is_empty());

        // Every proxy is built exactly as with the setting off: x264, software, no sidecar encoder.
        let (built, told) = build(&clip, 3.0);
        let proxy = built.expect("a proxy");
        let _guard = ProxyGuard(proxy.clone());
        assert_eq!(told.pipelines, [ProxyPipeline::Software], "no fast attempt was made");
        assert!(!proxy_hardware_encoded(&proxy));
        assert!(fast_health().usable().is_ok(), "nothing failed, so nothing is convicted");
        let read = read_fast_output(&proxy).unwrap();
        let stored = read_stored_video(&clip).unwrap();
        eprintln!(
            "software proxy: {}x{}, {} frames of {}",
            read.width,
            read.height,
            read.pts.len(),
            stored.nb_frames.unwrap()
        );
    });
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the whole build, with a stand-in for the hardware --------------------------------------------------------

/// A foreground job on the cores' slot, held on a thread of its own until [`CoresHeld::release`]: the
/// slot a fast build must not need (it is on the hardware's), so a build that finishes while this is
/// held did not wait for the cores.
#[cfg(unix)]
struct CoresHeld {
    release: std::sync::mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

#[cfg(unix)]
impl CoresHeld {
    fn take() -> Self {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (taken, ready) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _slot = cpu::lease_priority(cpu::Priority::Foreground);
            taken.send(()).unwrap();
            let _ = wait.recv();
        });
        ready.recv().unwrap();
        Self { release, thread }
    }

    fn release(self) {
        drop(self.release);
        self.thread.join().unwrap();
    }
}

/// Why the fast pipeline would not be chosen for `clip` here, as the test's own failure message (a build
/// that quietly ends up in software says nothing of why): `None` when it would be.
#[cfg(unix)]
fn why_not_fast(clip: &Path) -> Option<String> {
    let traits = source_traits(clip).unwrap_or_default();
    match choose_pipeline(clip, PROXY_MAX_WIDTH, traits, &|| false) {
        FastChoice::Gpu(..) => None,
        FastChoice::Software(why) => Some(why.unwrap_or_else(|| "the setting is off".to_string())),
    }
}

#[cfg(unix)]
#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_fast_build_through_the_real_runner_is_checked_and_recorded_with_the_encoder_that_wrote_it() {
    let _gate = cpu::test_lock();
    let dir = scratch("stand-in-pass");
    let (clip, log) = (dir.join("clip.mp4"), dir.join("argv.log"));
    make_clip(&clip, "1920x1080", 30, 3.0);
    let stand_in = write_stand_in(&dir, "pass", &log, &ffmpeg_bin(), "0");
    let (built, told, cores_were_held) = with_fast_world(true, || {
        with_ffmpeg(stand_in.to_str().unwrap(), || {
            with_pinned_fast_check(Some(FastVendor::Amf), || {
                assert_eq!(why_not_fast(&clip), None, "the pipeline is offered for this source");
                // The cores' slot is taken by somebody else for the whole build: a fast build is on the hardware's.
                let cores = CoresHeld::take();
                let (built, told) = build(&clip, 3.0);
                cores.release();
                (built, told, true)
            })
        })
    });
    assert!(cores_were_held);
    let proxy = built.expect("a fast proxy that did not wait for the cores");
    let _guard = ProxyGuard(proxy.clone());
    // It was built by the pipeline, said so once, and reported progress for it.
    assert_eq!(told.pipelines, [ProxyPipeline::Gpu(FastVendor::Amf)]);
    assert_eq!(told.progress.first(), Some(&0.0));
    // The argv the runner handed ffmpeg is the tested one (plus the spawn-time `-progress`).
    let seen = std::fs::read_to_string(&log).unwrap();
    assert!(
        seen.contains("-hwaccel amf -hwaccel_output_format amf -i ")
            && seen.contains("-vf vpp_amf=w=1280:h=720:format=nv12:scale_type=bilinear"),
        "{seen}"
    );
    assert!(
        seen.contains("-c:v h264_amf -rc cqp -qp_i 24 -qp_p 24 -quality speed -g 1 -bf 0 -max_b_frames 0"),
        "{seen}"
    );
    assert_eq!(seen.lines().count(), 1, "one attempt");
    // The proxy is where every proxy is, and says which encoder wrote it.
    assert_eq!(ready_proxy(&clip, PROXY_MAX_WIDTH), Some(proxy.clone()));
    assert!(proxy_hardware_encoded(&proxy));
    let sidecar: serde_json::Value = serde_json::from_slice(&std::fs::read(proxy_sidecar_path(&proxy)).unwrap()).unwrap();
    assert_eq!(sidecar["encoder"], "h264_amf");
    let video = proxy_video_info(&proxy).expect("the sidecar describes the proxy");
    assert_eq!((video.width, video.height), (Some(1280), Some(720)));
    // Nothing is left behind.
    assert!(parts_left_beside(&proxy).is_empty());
    eprintln!(
        "fast build through the stand-in: {told_pipelines:?}",
        told_pipelines = told.pipelines
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_fast_proxy_that_is_not_what_x264_would_have_made_is_deleted_and_built_again_in_software() {
    let _gate = cpu::test_lock();
    let dir = scratch("stand-in-truncate");
    let (clip, log) = (dir.join("clip.mp4"), dir.join("argv.log"));
    make_clip(&clip, "1920x1080", 30, 3.0);
    let stand_in = write_stand_in(&dir, "truncate", &log, &ffmpeg_bin(), "0");
    let (built, told, usable) = with_fast_world(true, || {
        with_ffmpeg(stand_in.to_str().unwrap(), || {
            with_pinned_fast_check(Some(FastVendor::Amf), || {
                assert_eq!(why_not_fast(&clip), None, "the pipeline is offered for this source");
                let (built, told) = build(&clip, 3.0);
                (built, told, fast_health().usable())
            })
        })
    });
    let proxy = built.expect("the software rebuild lands");
    let _guard = ProxyGuard(proxy.clone());
    // It tried the pipeline, the guard saw seven frames of ninety, and x264 made the proxy.
    assert_eq!(told.pipelines, [ProxyPipeline::Gpu(FastVendor::Amf), ProxyPipeline::Software]);
    assert!(!proxy_hardware_encoded(&proxy), "the proxy that stays is x264's");
    let stored = read_stored_video(&clip).unwrap();
    assert_eq!(read_fast_output(&proxy).unwrap().pts.len() as u64, stored.nb_frames.unwrap());
    assert!(parts_left_beside(&proxy).is_empty(), "the rejected proxy was deleted");
    // One refusal is one strike: the pipeline is still offered.
    assert!(usable.is_ok());
    // The progress went back to the start for the second encode.
    assert!(
        told.progress.iter().filter(|f| **f == 0.0).count() >= 2,
        "{:?}",
        told.progress
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_fast_build_that_fails_on_the_hardware_is_built_in_software_and_the_pipeline_is_not_offered_again() {
    let _gate = cpu::test_lock();
    let dir = scratch("stand-in-fail");
    let (clip, other, log) = (dir.join("clip.mp4"), dir.join("other.mp4"), dir.join("argv.log"));
    make_clip(&clip, "1920x1080", 30, 3.0);
    make_clip(&other, "1280x720", 30, 2.0);
    let stand_in = write_stand_in(&dir, "fail", &log, &ffmpeg_bin(), "0");
    with_fast_world(true, || {
        with_ffmpeg(stand_in.to_str().unwrap(), || {
            with_pinned_fast_check(Some(FastVendor::Amf), || {
                assert_eq!(why_not_fast(&clip), None, "the pipeline is offered for this source");
                // The first proxy: the pipeline is tried, fails like a machine with no device, x264 builds it.
                let (built, told) = build(&clip, 3.0);
                let proxy = built.expect("the software rebuild lands");
                let _guard = ProxyGuard(proxy.clone());
                assert_eq!(told.pipelines, [ProxyPipeline::Gpu(FastVendor::Amf), ProxyPipeline::Software]);
                assert!(!proxy_hardware_encoded(&proxy));
                let off = fast_health()
                    .usable()
                    .expect_err("a failure like the hardware's turns it off");
                assert!(off.contains("Device creation failed") || off.contains("libamfrt64"), "{off}");
                // The status says it: not available, which vendor, why.
                let status = fast_pipeline_status();
                assert!(!status.available && status.pipeline == Some("AMD AMF"));
                assert!(status.reason.unwrap().contains("stopped working"));
                // The next proxy is not even tried on it, and says why in the choice.
                assert!(why_not_fast(&other).unwrap().contains("stopped working"));
                let (built, told) = build(&other, 2.0);
                let second = built.expect("a software proxy");
                let _guard2 = ProxyGuard(second);
                assert_eq!(told.pipelines, [ProxyPipeline::Software]);
                assert_eq!(
                    std::fs::read_to_string(&log).unwrap().lines().count(),
                    1,
                    "the pipeline ran once"
                );
            });
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
#[ignore = "drives the ffmpeg binary"]
fn a_proxy_a_hardware_encoder_wrote_is_not_scored_but_is_good_for_a_contact_sheet() {
    use crate::project::Project;
    let _gate = cpu::test_lock();
    let dir = scratch("fidelity");
    let (clip, log) = (dir.join("clip.mp4"), dir.join("argv.log"));
    make_clip(&clip, "1920x1080", 30, 3.0);
    let mut asset = crate::engine::test_support::av_asset(uuid::Uuid::new_v4(), 3.0);
    asset.path = clip.to_string_lossy().into_owned();
    let stand_in = write_stand_in(&dir, "pass", &log, &ffmpeg_bin(), "0");
    // An x264 proxy (the setting off, whatever another test has the process's on) stands in for the file for both reads.
    let proxy = with_fast_world(false, || generate_proxy(&clip, PROXY_MAX_WIDTH)).expect("a proxy");
    let _guard = ProxyGuard(proxy.clone());
    assert_eq!(Project::scoring_proxy(&asset).as_deref(), Some(proxy.as_path()));
    assert_eq!(Project::sheet_proxy(&asset).as_deref(), Some(proxy.as_path()));
    // The same proxy built by a hardware encoder (the stand-in): scores are the original's, times are the proxy's.
    remove_proxy(&proxy);
    let built = with_fast_world(true, || {
        with_ffmpeg(stand_in.to_str().unwrap(), || {
            with_pinned_fast_check(Some(FastVendor::Amf), || {
                assert_eq!(why_not_fast(&clip), None, "the pipeline is offered for this source");
                build(&clip, 3.0).0
            })
        })
    });
    assert_eq!(built.expect("a fast proxy"), proxy);
    assert!(proxy_hardware_encoded(&proxy));
    assert_eq!(
        Project::scoring_proxy(&asset),
        None,
        "a hardware proxy's scores are unmeasured"
    );
    assert_eq!(Project::sheet_proxy(&asset).as_deref(), Some(proxy.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the whole machinery, with a stand-in for the GPU in a process of its own ---------------------------------

/// What a stand-in world's child reports on its stdout, one `key=value` per line, for the parent to read.
#[cfg(unix)]
const WORLD_MARK: &str = "STAND-IN-WORLD ";

/// The check and the queue **end to end** with a stand-in for the AMD GPU: a child process of this test
/// binary whose `KERF_FFMPEG` is the stand-in script over the pinned FFmpeg 9.0.2. The stand-in lets the
/// encoder test of `h264_amf` pass (so `hw_encoders()` lists it) and runs the whole AMF pipeline on the
/// software equivalent, taking two seconds over it. A child because the check, `hw_encoders()` and the
/// proxy queue are process-wide and run on threads of their own, which a thread-local override would not
/// reach.
#[cfg(unix)]
#[test]
#[ignore = "drives the ffmpeg binary"]
fn with_a_stand_in_for_the_gpu_the_check_finds_it_and_the_queue_builds_two_at_once() {
    let pinned = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu");
    if !pinned.is_file() {
        eprintln!("skipped: the pinned FFmpeg 9.0.2 is not fetched (bun scripts/fetch-ffmpeg.mjs)");
        return;
    }
    let dir = scratch("stand-in-world");
    let log = dir.join("argv.log");
    let stand_in = write_stand_in(&dir, "pass", &log, pinned.to_str().unwrap(), "2");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "engine::cli::proxy_fast_rendered::stand_in_world_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("KERF_FFMPEG", &stand_in)
        .env("KERF_STAND_IN_CHILD", "1")
        // The child is a clean world: what a developer's shell sets for the parity suites (no hardware
        // decode, no hardware encode, no hardware lane) would turn the very thing it checks off.
        .env_remove("KERF_HWACCEL")
        .env_remove("KERF_HW_ENCODE")
        .env_remove("KERF_GPU_LANE")
        .env_remove("KERF_PROXY_WORKERS")
        .output()
        .expect("the child runs");
    let said = String::from_utf8_lossy(&out.stdout);
    for line in said.lines().filter(|l| l.starts_with(WORLD_MARK)) {
        eprintln!("{}", line.trim_start_matches(WORLD_MARK));
    }
    assert!(
        out.status.success(),
        "the child failed:\n{said}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(said.contains(&format!("{WORLD_MARK}done")), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
#[ignore = "run by with_a_stand_in_for_the_gpu_the_check_finds_it_and_the_queue_builds_two_at_once"]
#[allow(clippy::print_stdout)]
fn stand_in_world_child() {
    use crate::proxy::{self, ProxyEncoder, ProxyInput, ProxyPhase, QueueState};
    if std::env::var_os("KERF_STAND_IN_CHILD").is_none() {
        return;
    }
    let say = |text: String| println!("{WORLD_MARK}{text}");
    allow_real_fast_probe();
    proxy::set_proxy_encoder(ProxyEncoder::FastGpu);
    // 1. The check, run for real over the stand-in: not checked yet, then a vendor.
    say(format!("first look: {:?}", proxy::fast_gpu_status()));
    let started = Instant::now();
    let status = loop {
        let status = proxy::fast_gpu_status();
        if status.checked {
            break status;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(180),
            "the check never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    say(format!("checked after {:?}: {status:?}", started.elapsed()));
    assert!(
        status.available && status.pipeline.as_deref() == Some("AMD AMF"),
        "{status:?}"
    );
    say(format!("hardware encoders seen: {:?}", hw_encoders()));

    // 2. Three MP4 sources and one Matroska one (which the pipeline refuses) go through the real queue.
    let dir = scratch("stand-in-queue");
    let mut assets = Vec::new();
    for name in ["a", "b", "c"] {
        let path = dir.join(format!("{name}.mp4"));
        make_clip(&path, "1280x720", 30, 3.0);
        assets.push(crate::Project::open_in_memory().unwrap().import_asset(&path).unwrap());
    }
    let mkv = dir.join("m.mkv");
    assert!(ffmpeg(&[
        "-i",
        dir.join("a.mp4").to_str().unwrap(),
        "-c",
        "copy",
        mkv.to_str().unwrap()
    ]));
    assets.push(crate::Project::open_in_memory().unwrap().import_asset(&mkv).unwrap());
    let inputs: Vec<ProxyInput> = assets
        .iter()
        .map(|asset| ProxyInput {
            asset: asset.clone(),
            declined: false,
        })
        .collect();
    let notify: proxy::Notify = std::sync::Arc::new(|_| {});
    for input in &inputs {
        assert!(proxy::queue_auto(input, notify.clone()).is_some());
    }
    let (mut most_at_once, mut labels): (usize, std::collections::BTreeSet<String>) = (0, Default::default());
    let until = Instant::now() + std::time::Duration::from_secs(180);
    while inputs.iter().any(|i| proxy::status(i).state != ProxyPhase::Ready) {
        assert!(Instant::now() < until, "timed out: {:?}", proxy::queue(&inputs));
        let queue = proxy::queue(&inputs);
        let building: Vec<_> = queue.jobs.iter().filter(|j| j.state == QueueState::Building).collect();
        most_at_once = most_at_once.max(building.iter().filter(|j| j.pipeline == proxy::Pipeline::Gpu).count());
        for job in &building {
            labels.insert(format!("{} on {}", job.name, job.pipeline.as_str()));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    say(format!("most GPU builds at once: {most_at_once}; seen building: {labels:?}"));
    for (asset, input) in assets.iter().zip(&inputs) {
        let proxy = ready_proxy(Path::new(&asset.path), proxy_width(None)).expect("a proxy");
        say(format!(
            "{}: written by {}",
            asset.name,
            stored_proxy_encoder(&proxy).as_deref().unwrap_or("libx264")
        ));
        let _ = input;
    }
    let by = |name: &str| assets.iter().find(|a| a.name == name).unwrap();
    for name in ["a.mp4", "b.mp4", "c.mp4"] {
        let proxy = ready_proxy(Path::new(&by(name).path), proxy_width(None)).unwrap();
        assert!(proxy_hardware_encoded(&proxy), "{name} is built by the pipeline");
    }
    let proxy = ready_proxy(Path::new(&by("m.mkv").path), proxy_width(None)).unwrap();
    assert!(
        !proxy_hardware_encoded(&proxy),
        "a Matroska source is refused and built by x264"
    );
    assert!(
        most_at_once >= 2,
        "two workers build on the hardware's two slots: {most_at_once}"
    );
    assert!(labels.iter().any(|l| l.starts_with("m.mkv on software")), "{labels:?}");
    assert!(proxy::queue(&inputs).jobs.is_empty());
    assert!(fast_health().usable().is_ok());
    for asset in &assets {
        proxy::delete(asset);
    }
    let _ = std::fs::remove_dir_all(&dir);
    say("done".to_string());
}
