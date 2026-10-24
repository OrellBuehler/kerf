//! Playback parity: the frames a play session draws against the export's.
//!
//! `tests/motion_parity.rs` holds the compositor to the export of a whole cut. A play session draws
//! the cut a [`PlayCut`] plans — `Timeline::slice` from the playhead, planned as the export plans it,
//! one forward-only cursor per clip — for the frames the page asks for, **skipping** the ones a newer
//! request replaced. This holds *that* to the export, plane for plane (the planes `overlay` blended,
//! no colour conversion between), with a stated bound per case:
//!
//! * the slice's own export, which is the graph `stream_preview` plays (all of them);
//! * where the content is aligned so that the slice and the whole cut pick the same source frames
//!   (same rates, a start on the grid), the **whole cut's export** from the start frame on — which is
//!   what a play that starts mid-clip or inside a dissolve has to be, or the playing picture would
//!   disagree with the file.
//!
//! Needs `ffmpeg` and an adapter (lavapipe is enough); `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test playback -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use kerf_core::{
    Asset, Clip, CompositeColorPolicy, Container, ExportOptions, OriginalMedia, PlayCut, Project, RateControl, StreamKind,
    Timeline, Track, Transition, TransitionKind,
};
use kerf_gpu::{Compositor, FrameSource, FrameSourceConfig, Gpu, GpuError, GpuOptions, MotionFrames, Opened};

// ---- the machinery -------------------------------------------------------------------

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn compositor() -> &'static Compositor {
    static C: OnceLock<Compositor> = OnceLock::new();
    C.get_or_init(|| Compositor::new(gpu()).expect("a compositor"))
}

fn frame_source() -> Arc<FrameSource> {
    static S: OnceLock<Arc<FrameSource>> = OnceLock::new();
    S.get_or_init(|| FrameSource::new(FrameSourceConfig::default())).clone()
}

fn policy() -> CompositeColorPolicy {
    let measured = kerf_core::composite_color_policy();
    assert_ne!(
        measured,
        CompositeColorPolicy::Unknown,
        "the colour probe could not read this FFmpeg"
    );
    measured
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn probed(path: &Path) -> Asset {
    Project::probe_asset(path).unwrap_or_else(|e| panic!("probe {}: {e}", path.display()))
}

struct Media {
    testsrc: Asset,
    bars: Asset,
    testsrc25: Asset,
    /// A 640x360 still image (RGB).
    image: Asset,
    /// The same pictures as `testsrc` in a transport stream, which runs are not trusted for.
    transport: PathBuf,
}

fn media() -> &'static Media {
    static M: OnceLock<Media> = OnceLock::new();
    M.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("playback-media");
        std::fs::create_dir_all(&dir).unwrap();
        let untagged = "setparams=colorspace=unknown:color_primaries=unknown:color_trc=unknown";
        let video = |name: &str, lavfi: &str| -> PathBuf {
            let p = dir.join(name);
            ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                lavfi,
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-qp",
                "0",
                "-g",
                "10",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            p
        };
        let testsrc = video("testsrc.mp4", &format!("testsrc2=size=640x360:rate=30:duration=6,{untagged}"));
        let bars = video("bars.mp4", &format!("smptehdbars=size=640x360:rate=30:duration=6,{untagged}"));
        let testsrc25 = video(
            "testsrc25.mp4",
            &format!("testsrc2=size=640x360:rate=25:duration=6,{untagged}"),
        );
        let image = dir.join("image.png");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "gradients=size=640x360:rate=1:duration=1:c0=0x20a040:c1=0xc04080:nb_colors=2:seed=3:speed=0.00001",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            image.to_str().unwrap(),
        ]);
        let transport = dir.join("testsrc.ts");
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size=640x360:rate=30:duration=3,{untagged}"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "mpegts",
            transport.to_str().unwrap(),
        ]);
        Media {
            testsrc: probed(&testsrc),
            bars: probed(&bars),
            testsrc25: probed(&testsrc25),
            image: probed(&image),
            transport,
        }
    })
}

fn timeline(clips: Vec<Clip>) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

/// Two one-second clips, the second entering through `kind` over `secs`.
fn pair(m: &Media, kind: Option<TransitionKind>, secs: f64) -> (Timeline, Vec<Asset>) {
    let out = Clip::new(m.bars.id, 0.5, 1.5, 0.0);
    let mut inc = Clip::new(m.testsrc.id, 0.5, 1.5, 1.0);
    inc.transition_in = kind.map(|kind| Transition { kind, duration: secs });
    (timeline(vec![out, inc]), vec![m.bars.clone(), m.testsrc.clone()])
}

// ---- the export, read back as planes --------------------------------------------------

struct Played {
    w: usize,
    h: usize,
    yuv: Vec<u8>,
}

impl Played {
    fn frame_len(&self) -> usize {
        self.w * self.h * 3 / 2
    }

    fn count(&self) -> usize {
        self.yuv.len() / self.frame_len()
    }

    fn planes(&self, k: usize) -> [&[u8]; 3] {
        let f = &self.yuv[k * self.frame_len()..(k + 1) * self.frame_len()];
        let (y, uv) = f.split_at(self.w * self.h);
        let (u, v) = uv.split_at(self.w * self.h / 4);
        [y, u, v]
    }
}

fn export(slug: &str, tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Played {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("playback-ref");
    std::fs::create_dir_all(&scratch).unwrap();
    let file = scratch.join(format!("{slug}.mkv"));
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..opts.clone()
    };
    kerf_core::render_with(tl, assets, &file, &lossless).expect("export");
    let yuv = ffmpeg(&[
        "-i",
        file.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ]);
    let info = Project::probe_asset(&file).expect("probe the export");
    let video = info
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .expect("a video stream");
    let _ = std::fs::remove_file(&file);
    Played {
        w: video.width.unwrap() as usize,
        h: video.height.unwrap() as usize,
        yuv,
    }
}

fn report() -> &'static Mutex<Vec<String>> {
    static R: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

fn write_report() {
    let mut text = String::new();
    for line in report().lock().unwrap().iter() {
        let _ = writeln!(text, "{line}");
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .expect("target/")
        .join("parity");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("playback-report.txt"), text);
}

// ---- the comparison ------------------------------------------------------------------------

/// Which export a played frame is held to.
#[derive(Clone, Copy, PartialEq)]
enum Against {
    /// The export of the slice the session plays: the graph `stream_preview` runs.
    Slice,
    /// The export of the whole cut, frame `k0 + j`: for content that picks the same source frames
    /// from the slice as from the whole.
    Whole,
}

struct Case<'a> {
    name: &'a str,
    tl: &'a Timeline,
    assets: &'a [Asset],
    /// Where the session starts, timeline seconds.
    at: f64,
    /// Seconds played.
    secs: f64,
    /// Every n-th frame is asked for (a newer request replaced the ones between).
    every: usize,
    against: Against,
    /// The bound in levels of 255: how far luma may be from the export's.
    bound: i32,
    /// ... and chroma (a layer the compositor scales *and* makes translucent is a level further in chroma).
    uv_bound: i32,
}

/// Play `case` and compare; returns `(frames compared, worst error, the session's start)`.
fn play(case: Case) -> (usize, i32, f64) {
    let Case {
        name,
        tl,
        assets,
        at,
        secs,
        every,
        against,
        bound,
        uv_bound,
    } = case;
    let cut = PlayCut::new(tl, assets, at, policy(), &OriginalMedia).expect("a cut");
    let fps = cut.rate();
    let opts = ExportOptions {
        fps: Some(fps),
        resolution: Some(cut.frame()),
        ..ExportOptions::default()
    };
    let rendered = tl.for_render();
    let k0 = (cut.start() * fps).round() as usize;
    let reference = match against {
        Against::Slice => rendered.slice((cut.start() - 1e-9).max(0.0), rendered.duration()),
        Against::Whole => rendered,
    };
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let played = export(&slug, &reference, assets, &opts);
    let size = (played.w as u32, played.h as u32);
    let mut motion = MotionFrames::new(frame_source());
    let first = cut.frame_at(at);
    let last = first + (secs * fps) as u64 - 1;
    let (mut compared, mut worst, mut differing) = (0usize, 0i32, 0usize);
    let (mut worst_y, mut worst_uv) = (0i32, 0i32);
    let mut over: Vec<(u64, i32)> = Vec::new();
    let mut j = first;
    while j <= last {
        let plan = cut.at_frame(j).unwrap_or_else(|e| panic!("{name}: frame {j}: {e}"));
        let why = plan.reasons(&compositor().caps(), size);
        assert!(why.is_empty(), "{name}: frame {j}: the plan is refused: {why:?}");
        let pictures = motion.frames(&plan).unwrap_or_else(|e| panic!("{name}: frame {j}: {e}"));
        let drawn = compositor()
            .composite_yuv_counted(&plan, &pictures, motion.counts(), size)
            .unwrap_or_else(|e| panic!("{name}: frame {j}: {e}"));
        let index = match against {
            Against::Slice => j as usize,
            Against::Whole => k0 + j as usize,
        };
        assert!(
            index < played.count(),
            "{name}: the export has {} frames, frame {index} was asked",
            played.count()
        );
        let want = played.planes(index);
        let (mut frame_y, mut frame_uv) = (0, 0);
        for (plane, (w, g)) in want.iter().zip([&drawn.y, &drawn.u, &drawn.v]).enumerate() {
            assert_eq!(w.len(), g.len(), "{name}: plane {plane} of frame {j}");
            for (a, b) in w.iter().zip(g.iter()) {
                let d = (i32::from(*a) - i32::from(*b)).abs();
                if plane == 0 {
                    frame_y = frame_y.max(d);
                } else {
                    frame_uv = frame_uv.max(d);
                }
                differing += usize::from(d != 0);
            }
        }
        let frame_worst = frame_y.max(frame_uv);
        worst_y = worst_y.max(frame_y);
        worst_uv = worst_uv.max(frame_uv);
        if std::env::var_os("KERF_PARITY_EXPLORE").is_some() {
            let mut near = Vec::new();
            for other in index.saturating_sub(3)..(index + 4).min(played.count()) {
                let w = played.planes(other);
                let d = w[0]
                    .iter()
                    .zip(drawn.y.iter())
                    .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                    .max()
                    .unwrap_or(0);
                near.push((other, d));
            }
            eprintln!("  frame {j}: worst {frame_worst} layers {} near {near:?}", plan.layers.len());
        }
        if (frame_y > bound || frame_uv > uv_bound) && over.len() < 6 {
            over.push((j, frame_worst));
        }
        worst = worst.max(frame_worst);
        compared += 1;
        j += every as u64;
    }
    let line = format!(
        "{name:<52} start {:>7.4} s | {compared:>3} frames (every {every}) vs {} | worst {worst} levels (Y {worst_y}, UV {worst_uv}), {differing} samples differ",
        cut.start(),
        if against == Against::Slice { "slice" } else { "whole" },
    );
    eprintln!("{line}");
    report().lock().unwrap().push(line);
    write_report();
    assert!(
        (worst_y <= bound && worst_uv <= uv_bound) || std::env::var_os("KERF_PARITY_EXPLORE").is_some(),
        "{name}: a played frame is {worst_y} (Y) / {worst_uv} (UV) levels from the export's (bound {bound} / {uv_bound}); frames over it: {over:?}"
    );
    (compared, worst, cut.start())
}

fn case<'a>(name: &'a str, tl: &'a Timeline, assets: &'a [Asset], at: f64, secs: f64, against: Against) -> Case<'a> {
    Case {
        name,
        tl,
        assets,
        at,
        secs,
        every: 1,
        against,
        bound: 0,
        uv_bound: 0,
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_session_from_the_start_draws_the_exports_cut_a_dissolve_included() {
    let m = media();
    let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61);
    let (n, worst, _) = play(case("play/from-the-start/dissolve", &tl, &assets, 0.0, 2.0, Against::Whole));
    assert!(n >= 60 && worst == 0);
    let (tl, assets) = pair(m, None, 0.0);
    play(case("play/from-the-start/hard-cut", &tl, &assets, 0.0, 2.0, Against::Whole));
    // A clip's own fades.
    let mut c = Clip::new(m.testsrc.id, 0.5, 1.5, 0.0);
    c.fade_in = 0.4;
    c.fade_out = 0.4;
    let tl = timeline(vec![c]);
    play(case(
        "play/from-the-start/fades",
        &tl,
        std::slice::from_ref(&m.testsrc),
        0.0,
        1.0,
        Against::Whole,
    ));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_session_from_the_middle_of_a_clip_draws_the_exports_frames_from_there() {
    let m = media();
    let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61);
    // Mid-clip on the grid, a start that is between two frames, and one just past the first clip's tail.
    for at in [0.2, 0.2 + 0.4 / 30.0, 0.3] {
        play(case(
            &format!("play/mid-clip/dissolve pair @{at:.4}"),
            &tl,
            &assets,
            at,
            0.3,
            Against::Whole,
        ));
    }
    // A long clip played from deep inside it: the cursor starts at the slice, not at the clip's start.
    let long = timeline(vec![Clip::new(m.testsrc.id, 0.0, 5.0, 0.0)]);
    let (n, _, start) = play(case(
        "play/mid-clip/deep in a long clip",
        &long,
        std::slice::from_ref(&m.testsrc),
        3.5,
        1.0,
        Against::Whole,
    ));
    assert!(n > 20 && (start - 3.5).abs() < 1.0 / 30.0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_session_started_inside_a_dissolve_has_the_dissolve() {
    let m = media();
    let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61);
    // 1.2 s is deep inside it: a slice started there would have neither the outgoing clip nor the ramp.
    let (_, worst, start) = play(case("play/in-a-dissolve", &tl, &assets, 1.2, 0.6, Against::Whole));
    assert!(start < 0.5, "the session starts before the dissolve: {start}");
    assert_eq!(worst, 0);
    // Other kinds, entered at a frame inside them. A dip is the whole cut's frames to the level. A slide
    // or a push is a position the overlay truncates to whole pixels (`overlay x`), and the slice's
    // clock is not the whole cut's by a float's width: a travel that lands exactly on a pixel
    // (0.3 s of a 0.5 s slide of 640 px is 256.0) falls either side of it, so a slide is held to the
    // slice's own export, which is the graph the stream plays.
    let (tl, assets) = pair(m, Some(TransitionKind::DipToBlack), 0.5);
    play(case(
        "play/in-a-transition/DipToBlack",
        &tl,
        &assets,
        1.15,
        0.5,
        Against::Whole,
    ));
    for kind in [TransitionKind::SlideLeft, TransitionKind::PushUp, TransitionKind::SlideDown] {
        let (tl, assets) = pair(m, Some(kind), 0.5);
        let name = format!("play/in-a-transition/{kind:?}");
        play(case(&name, &tl, &assets, 1.15, 0.5, Against::Slice));
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_frames_a_newer_request_replaced_change_no_frame() {
    let m = media();
    let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61);
    for every in [2, 3, 7, 30] {
        let name = format!("play/skipping/every {every}");
        let mut c = case(&name, &tl, &assets, 0.0, 2.0, Against::Whole);
        c.every = every;
        play(c);
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_clip_of_another_rate_is_drawn_as_the_slice_the_stream_plays_is() {
    let m = media();
    // A 25 fps clip in a 30 fps cut: the `fps` filter drops and repeats, and where it does depends on
    // the phase of the slice's first frame to the grid. The slice's own export is the reference.
    let tl = timeline(vec![
        Clip::new(m.testsrc.id, 0.5, 1.5, 0.0),
        Clip::new(m.testsrc25.id, 0.5, 3.5, 1.0),
    ]);
    let assets = [m.testsrc.clone(), m.testsrc25.clone()];
    for at in [0.0, 1.0, 1.7, 2.5 + 0.3 / 30.0] {
        play(case(
            &format!("play/25-in-30/@{at:.3}"),
            &tl,
            &assets,
            at,
            1.0,
            Against::Slice,
        ));
    }
    // And a speed change.
    let mut fast = Clip::new(m.testsrc.id, 0.5, 4.5, 0.0);
    fast.speed = 2.0;
    let tl = timeline(vec![fast]);
    play(case(
        "play/speed-2",
        &tl,
        std::slice::from_ref(&m.testsrc),
        0.6,
        1.0,
        Against::Slice,
    ));
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_still_image_is_drawn_through_a_session_as_the_export_draws_it() {
    let m = media();
    let mut inc = Clip::new(m.testsrc.id, 0.5, 1.5, 1.0);
    inc.transition_in = Some(Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    let tl = timeline(vec![Clip::new(m.image.id, 0.0, 1.0, 0.0), inc]);
    let assets = [m.image.clone(), m.testsrc.clone()];
    // From the start of the image and from inside it: its own frames, to the level.
    for at in [0.0, 0.4] {
        play(case(
            &format!("play/still-image/@{at:.1}"),
            &tl,
            &assets,
            at,
            0.6,
            Against::Whole,
        ));
    }
    // From inside the dissolve out of it. A PNG is a 25 fps stream, so the cut is a 25 fps one with a 30 fps
    // clip in it, and which source frame each output frame picks depends on where the slice starts: the
    // session is held to the slice's export, which is the graph the stream plays (the whole cut's picks
    // differ from it in the frames the 30 to 25 conversion drops, as with any mix of rates).
    let (n, worst, start) = play(case(
        "play/still-image/@1.2 (mixed rates)",
        &tl,
        &assets,
        1.2,
        0.6,
        Against::Slice,
    ));
    assert!(n >= 15 && worst == 0 && start < 0.5, "{n} {worst} {start}");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_layer_over_another_track_with_a_transform_and_an_opacity_is_drawn_through_cursors_as_the_export_draws_it() {
    let m = media();
    let base = Clip::new(m.bars.id, 0.5, 1.5, 0.0);
    let assets = [m.bars.clone(), m.testsrc.clone()];
    let two_tracks = |layer: Clip| Timeline {
        tracks: vec![
            Track {
                clips: vec![base.clone()],
                ..Track::new(StreamKind::Video, "V1")
            },
            Track {
                clips: vec![layer],
                ..Track::new(StreamKind::Video, "V2")
            },
        ],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    // A smaller layer, placed: the scaler's one level, as the export's (`SCALED` in `motion_parity.rs`).
    let mut pip = Clip::new(m.testsrc.id, 0.5, 1.5, 0.2);
    pip.transform.scale = 0.4;
    pip.transform.pos_x = 0.25;
    pip.transform.pos_y = -0.2;
    let tl = two_tracks(pip);
    for at in [0.0, 0.5] {
        let name = format!("play/pip/transform @{at:.1}");
        let mut c = case(&name, &tl, &assets, at, 0.6, Against::Whole);
        (c.bound, c.uv_bound) = (1, 1);
        play(c);
    }
    // A translucent one, scaled too: the scaler's level (`SCALED`: 1 in every plane) and the RGB round trip's
    // (`TRANSLUCENT`: chroma one level, luma exact) on top of it. Measured: two in luma as well as chroma, the
    // scaler's level crossing into luma through the round trip's conversion (not the one and two the sum of the
    // two bounds says).
    let mut glass = Clip::new(m.testsrc.id, 0.5, 1.5, 0.2);
    glass.transform.scale = 0.6;
    glass.transform.opacity = 0.7;
    let tl = two_tracks(glass);
    let mut c = case("play/pip/translucent and scaled", &tl, &assets, 0.0, 0.6, Against::Whole);
    (c.bound, c.uv_bound) = (2, 2);
    play(c);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_title_is_drawn_during_a_session_as_the_export_draws_it() {
    let m = media();
    let mut tl = timeline(vec![Clip::new(m.testsrc.id, 0.0, 2.0, 0.0)]);
    let mut title = kerf_core::TextOverlay::new("Summer 2026", 0.4, 1.4);
    title.bg = Some("black@0.5".into());
    tl.overlays.push(title);
    let assets = [m.testsrc.clone()];
    // Before it, into it, and from inside it; the titles of the frames are the compositor's own text.
    for at in [0.0, 0.5, 1.0] {
        play(case(&format!("play/title/@{at:.1}"), &tl, &assets, at, 0.7, Against::Whole));
    }
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_cursor_opened_ahead_is_the_one_the_cut_uses() {
    let m = media();
    let (tl, assets) = pair(m, None, 0.0);
    let cut = PlayCut::new(&tl, &assets, 0.0, policy(), &OriginalMedia).expect("a cut");
    let mut motion = MotionFrames::new(frame_source());
    let feed = motion.feed();
    // The second clip starts at frame 30; the first is still a candidate there (its closing edge).
    let second = cut.at_frame(31).unwrap();
    assert_eq!(second.layers.len(), 1);
    let layer = &second.layers[0];
    let wanted = cut.time_of(31);
    feed.open(layer, wanted).expect("opened ahead");
    feed.open(layer, wanted).expect("and again: nothing new");
    assert!(feed.has(layer.clip_id));
    assert_eq!(feed.waiting(), 1);
    // Playing the first clip leaves it waiting; the cut takes it over a frame before its first (its
    // overlay opens a frame early), and the first clip's own cursor goes once the plan has no layer of it.
    for k in 0..29 {
        motion.frames(&cut.at_frame(k).unwrap()).expect("a frame");
    }
    assert_eq!((motion.cursors(), feed.waiting()), (1, 1));
    let before = motion.frames(&cut.at_frame(29).unwrap()).expect("the frame before the cut");
    assert_eq!(before.len(), 2);
    assert!(before[1].is_none(), "the second clip has no frame before its first");
    assert_eq!((motion.cursors(), feed.waiting()), (2, 0));
    let at_the_cut = motion.frames(&cut.at_frame(30).unwrap()).expect("the cut");
    assert_eq!(
        at_the_cut.len(),
        2,
        "the closing edge of the first clip, and the first of the second"
    );
    assert!(at_the_cut[1].is_some(), "the second clip's first frame");
    assert_eq!(
        (motion.cursors(), feed.waiting()),
        (2, 0),
        "the cut used the one opened ahead"
    );
    let after = motion.frames(&second).expect("the frame after it");
    assert!(after[0].is_some());
    assert_eq!((motion.cursors(), feed.waiting()), (1, 0), "and the first clip's went");
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_feed_with_more_clips_in_the_window_than_it_holds_keeps_the_ones_wanted_first() {
    let m = media();
    // Nine half-second clips one after the other: the window ahead has more clips than the feed holds.
    let clips: Vec<Clip> = (0..9).map(|k| Clip::new(m.testsrc.id, 0.5, 1.0, k as f64 * 0.5)).collect();
    let ids: Vec<_> = clips.iter().map(|c| c.id).collect();
    let tl = timeline(clips);
    let assets = [m.testsrc.clone()];
    let cut = PlayCut::new(&tl, &assets, 0.0, policy(), &OriginalMedia).expect("a cut");
    let motion = MotionFrames::new(frame_source());
    let feed = motion.feed();
    // The first frame after clip k's start (the frame on its start has the closing edge of the clip before).
    let frame_of = |k: usize| 15 * k as u64 + 1;
    let open = |k: usize| {
        let plan = cut.at_frame(frame_of(k)).unwrap();
        let layer = plan.layers.iter().find(|l| l.clip_id == ids[k]).expect("the clip's layer");
        feed.open(layer, cut.time_of(frame_of(k))).expect("open")
    };
    // Clips 3 to 8 fill it.
    for k in 3..9 {
        assert_eq!(open(k), Opened::Waiting { evicted: None }, "clip {k}");
    }
    assert_eq!(feed.waiting(), 6);
    // A clip wanted sooner than all of them takes the place of the one wanted last, which is the one that goes.
    assert_eq!(open(1), Opened::Waiting { evicted: Some(ids[8]) });
    assert!(feed.has(ids[1]) && !feed.has(ids[8]));
    assert_eq!(feed.waiting(), 6);
    // One wanted after every one of them is not opened at all.
    assert_eq!(open(8), Opened::Full);
    assert!(!feed.has(ids[8]));
    // The ones wanted first are still there when their clips come on screen.
    assert!((1..=7).filter(|k| *k != 2).all(|k| feed.has(ids[k])));
    feed.close();
    assert_eq!(feed.waiting(), 0);
    assert_eq!(open(2), Opened::Closed, "a closed feed starts nothing");
    assert_eq!(feed.waiting(), 0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn a_file_runs_are_not_trusted_for_is_refused_when_it_is_opened_ahead() {
    let m = media();
    let transport = probed(&m.transport);
    let tl = timeline(vec![
        Clip::new(m.testsrc.id, 0.5, 1.5, 0.0),
        Clip::new(transport.id, 0.0, 1.0, 1.0),
    ]);
    let assets = [m.testsrc.clone(), transport];
    let cut = PlayCut::new(&tl, &assets, 0.0, policy(), &OriginalMedia).expect("a cut");
    let motion = MotionFrames::new(frame_source());
    let feed = motion.feed();
    let plan = cut.at_frame(35).unwrap();
    let err = feed.open(&plan.layers[0], cut.time_of(35)).expect_err("refused");
    assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
    assert!(err.to_string().contains("testsrc.ts"), "{err}");
    assert_eq!(feed.waiting(), 0);
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn the_texture_a_session_presents_is_the_picture_the_compositor_composites() {
    let m = media();
    let (tl, assets) = pair(m, Some(TransitionKind::Crossfade), 0.61);
    let cut = PlayCut::new(&tl, &assets, 0.0, policy(), &OriginalMedia).expect("a cut");
    let size = cut.frame();
    let size = (size.0 / 2, size.1 / 2);
    let mut motion = MotionFrames::new(frame_source());
    for k in [5u64, 29, 33, 40, 45] {
        let plan = cut.at_frame(k).unwrap();
        let (texture, timings) = compositor().render_motion_texture(&plan, size, &mut motion).expect("a frame");
        let drawn = texture.read_back(&gpu()).expect("read back");
        // The same pictures through the readback path (the cursors have moved on: a second motion).
        let mut again = MotionFrames::new(frame_source());
        let pictures = again.frames(&plan).expect("pictures");
        let reference = compositor().composite_shared(&plan, &pictures, size).expect("the composite");
        assert_eq!(drawn.data, reference.data, "frame {k}");
        assert!(timings.decode.as_secs_f64() >= 0.0);
    }
}
