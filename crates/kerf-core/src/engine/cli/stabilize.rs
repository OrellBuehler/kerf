//! Stabilization in the graph, and the passes that measure a clip's camera (`crate::stabilize` has the
//! design). A child of `cli`, so it reaches the private runners and escapes.
//!
//! **In a chain** a stabilized clip's frames are moved straight after its `trim`, before `reverse` and
//! the retime: the correction belongs to a frame of the file. With vid.stab the chain trims at the
//! [`Slice`]'s start (between two measured frames) and `vidstabtransform` reads the slice's absolute
//! corrections; a clip that has not been measured gets nothing (it plays as shot), so every graph of
//! an unstabilized cut is byte-identical. Without vid.stab, `deshake` steadies it on its own, sized to
//! the frame it reads, and a centred crop takes its edges away.
//!
//! **Measuring** is two runs over the clip's window of the original, in a fresh working directory so
//! the files they write have plain names: `vidstabdetect` (the local motions, and `showinfo` for each
//! frame's time and size), then one frame through `vidstabtransform=debug=1`, which turns the local
//! motions into vid.stab's own per-frame transforms as it opens and writes them as
//! `global_motions.trf` — the numbers its second pass would smooth. Both are scaled to at most
//! [`MEASURE_WIDTH`] first, the same in both, so the measured size is that.

use super::*;
use crate::model::Stabilize;
use crate::stabilize::{InputFrame, MotionAnalysis, Slice, ANALYSIS_VERSION};

/// The widest frame the motion is measured at: a 4K picture is measured at 1920, its motion read in
/// those pixels (the corrections are scaled to whatever a chain decodes).
const MEASURE_WIDTH: u32 = 1920;

#[cfg(test)]
thread_local! {
    /// What [`vidstab_available`] answers on this thread while a test pins it.
    static VIDSTAB_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg did (`true`) or did not (`false`) have vid.stab.
#[cfg(test)]
pub(crate) fn with_vidstab<R>(available: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            VIDSTAB_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(VIDSTAB_OVERRIDE.with(|c| c.replace(Some(available))));
    f()
}

/// Whether this ffmpeg has `vidstabdetect` and `vidstabtransform` (libvidstab, a GPL build option the
/// bundled builds have and a minimal or LGPL one lacks), probed once per process. A binary that does
/// not run answers `false`: its renders fail whatever the chain says.
pub fn vidstab_available() -> bool {
    #[cfg(test)]
    if let Some(forced) = VIDSTAB_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ok = output_bounded(
            command(&ffmpeg_bin()).args(["-hide_banner", "-loglevel", "quiet", "-filters"]),
            CAPABILITY_PROBE_TIMEOUT,
            0,
        )
        .map(|o| filters_have_vidstab(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(false);
        tracing::debug!(available = ok, "probed ffmpeg for vid.stab");
        ok
    })
}

/// Whether `ffmpeg -filters` lists both vid.stab filters (pure).
fn filters_have_vidstab(list: &str) -> bool {
    let has = |name: &str| list.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
    has("vidstabdetect") && has("vidstabtransform")
}

/// What steadies a clip in one chain.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Stabilizer {
    /// vid.stab's corrections for the frames from the slice's start.
    Vidstab(Slice),
    /// `deshake` (and its crop), when this ffmpeg has no vid.stab.
    Deshake(String),
}

impl Stabilizer {
    /// The filters, in chain order.
    pub(super) fn filters(&self) -> String {
        match self {
            Stabilizer::Vidstab(s) => vidstab_filter(s),
            Stabilizer::Deshake(f) => f.clone(),
        }
    }

    /// The source time the chain's trim starts at instead of its window's (vid.stab's slice is
    /// numbered from it).
    pub(super) fn trim_start(&self) -> Option<f64> {
        match self {
            Stabilizer::Vidstab(s) => Some(s.start),
            Stabilizer::Deshake(_) => None,
        }
    }
}

/// `vidstabtransform` moving each frame by the slice's absolute corrections: no smoothing and no zoom
/// of its own (both are the slice's), black where the frame moved away from the edge.
fn vidstab_filter(slice: &Slice) -> String {
    format!(
        "vidstabtransform=input='{}':relative=0:smoothing=0:optzoom=0:zoom={}:crop=black:interpol=bicubic",
        escape_drawtext_path(&slice.file.to_string_lossy()),
        fnum(slice.zoom)
    )
}

/// `deshake` for a frame `width` pixels wide (pure): its search reaches about `0.5% + 2.5% × strength`
/// of the width, in the 16, 32, 48 or 64 pixels the filter takes, and the picture is enlarged by the
/// setting's zoom, else by twice that reach, so the edges it moves are cropped away.
fn deshake_filter(setting: &Stabilize, width: u32) -> String {
    let setting = setting.normalized();
    let reach = (f64::from(width) * (0.005 + 0.025 * setting.strength) / 16.0)
        .round()
        .clamp(1.0, 4.0)
        * 16.0;
    let zoom = setting
        .zoom
        .unwrap_or_else(|| (200.0 * reach / f64::from(width.max(1))).min(crate::model::MAX_STABILIZE_ZOOM));
    let r = reach as u32;
    let mut out = format!("deshake=rx={r}:ry={r}:edge=mirror");
    if zoom > 0.0 {
        let k = 1.0 + zoom / 100.0;
        out.push_str(&format!(",crop=w=iw/{k}:h=ih/{k}", k = fnum((k * 1e4).round() / 1e4)));
    }
    out
}

/// What steadies `clip` in a chain reading its source `window` from `asset` (the input as the graph
/// has it: an original, or a preview's proxy in its place), or `None` when nothing does: the clip is
/// not stabilized, cannot be (a still, 360 footage, a title), or waits for its analysis.
pub(super) fn stabilizer(clip: &Clip, asset: Option<&Asset>, window: (f64, f64)) -> Option<Stabilizer> {
    let setting = clip.stabilize.as_ref().filter(|_| clip.is_stabilized())?;
    let asset = asset.filter(|a| !a.is_image() && a.projection().is_none() && clip.reframe.is_none())?;
    let proxy = crate::clip_timing::is_cache_proxy(&asset.path);
    let original = asset.streams.iter().find(|s| s.kind == StreamKind::Video);
    let proxy_frame = || proxy_frame(asset, original);
    if vidstab_available() {
        let input = if proxy { Some(proxy_frame()?) } else { None };
        let file = (!proxy).then(|| Path::new(&asset.path));
        let slice = crate::stabilize::slice_for(clip, setting, window, file, input)?;
        return Some(Stabilizer::Vidstab(slice));
    }
    let width = if proxy {
        proxy_frame()?.width
    } else {
        original.and_then(|s| s.width)?
    };
    Some(Stabilizer::Deshake(deshake_filter(setting, width)))
}

/// The size a preview proxy of `asset` decodes to: its own description (never probed from a test).
fn proxy_frame(asset: &Asset, original: Option<&StreamInfo>) -> Option<InputFrame> {
    #[cfg(test)]
    if crate::stabilize::pinned::active() {
        // The pinned oracle's proxies are names, not files: the width they would be made at.
        let (w, h) = (original?.width?, original?.height?);
        let pw = w.min(1280);
        return Some(InputFrame {
            width: pw,
            height: ((f64::from(h) * f64::from(pw) / f64::from(w) / 2.0).round() as u32) * 2,
        });
    }
    let _ = original;
    let video = proxy_video_info(Path::new(&asset.path))?;
    Some(InputFrame {
        width: video.width?,
        height: video.height?,
    })
}

// ---- measuring ---------------------------------------------------------------------------------

/// The local motions' file the detect pass writes, in its working directory.
const MOTIONS_FILE: &str = "motions.trf";
/// The transforms `vidstabtransform=debug=1` writes as it opens, in its working directory.
const TRANSFORMS_FILE: &str = "global_motions.trf";

/// The `scale` both passes measure through: no wider than [`MEASURE_WIDTH`], the same in both.
fn measure_scale() -> String {
    format!("scale=w='min({MEASURE_WIDTH},iw)':h=-2")
}

/// The argv of the detect pass over `[seek, seek + span)` of `path` (pure, before the thread caps).
fn detect_args(path: &Path, seek: f64, span: f64) -> Vec<String> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostats".into()];
    if seek > 0.0 {
        args.extend(["-ss".into(), export_seek_arg(seek)]);
    }
    args.extend(["-i".into(), path.to_string_lossy().into_owned()]);
    args.extend(["-t".into(), format!("{span:.6}")]);
    args.extend(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"].map(String::from));
    args.push(format!(
        "{},vidstabdetect=result={MOTIONS_FILE}:shakiness=5:accuracy=15,showinfo",
        measure_scale()
    ));
    args.extend(["-f", "null", "-"].map(String::from));
    args
}

/// The argv of the pass that turns the local motions into transforms (pure): one frame, at the size
/// the detect pass measured, through `vidstabtransform=debug=1`.
fn transforms_args(path: &Path, seek: f64) -> Vec<String> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostats".into()];
    if seek > 0.0 {
        args.extend(["-ss".into(), export_seek_arg(seek)]);
    }
    args.extend(["-i".into(), path.to_string_lossy().into_owned()]);
    args.extend(["-map", "0:v:0", "-an", "-sn", "-dn", "-frames:v", "1", "-vf"].map(String::from));
    args.push(format!("{},vidstabtransform=input={MOTIONS_FILE}:debug=1", measure_scale()));
    args.extend(["-f", "null", "-"].map(String::from));
    args
}

/// The frames a detect pass saw, from its `showinfo` log (pure): each frame's source time (`seek` plus
/// its timestamp in the stream's time base) and the frame size.
pub(super) fn parse_frames(stderr: &str, seek: f64) -> Option<(Vec<f64>, u32, u32)> {
    let mut tb: Option<(f64, f64)> = None;
    let mut times = Vec::new();
    let mut size = None;
    for line in stderr.lines() {
        if !line.contains("Parsed_showinfo") {
            continue;
        }
        if let Some(rest) = line.split("config in time_base:").nth(1) {
            let (num, den) = rest.trim().split(',').next()?.split_once('/')?;
            let (num, den): (f64, f64) = (num.trim().parse().ok()?, den.trim().parse().ok()?);
            if den > 0.0 && num > 0.0 {
                tb = Some((num, den));
            }
            continue;
        }
        let Some(pts) = line.split(" pts:").nth(1) else {
            continue;
        };
        let pts: i64 = pts.split_whitespace().next()?.parse().ok()?;
        let (num, den) = tb?;
        times.push(seek + pts as f64 * num / den);
        if size.is_none() {
            size = line.split(" s:").nth(1).and_then(|s| {
                let (w, h) = s.split_whitespace().next()?.split_once('x')?;
                Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
            });
        }
    }
    let (w, h) = size?;
    Some((times, w, h))
}

/// The per-frame transforms vid.stab wrote with `debug=1` (pure): `[x, y, angle]` of each line that is
/// not a comment (`0 x y angle zoom extra`).
pub(super) fn parse_transforms(text: &str) -> Option<Vec<[f64; 3]>> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let v: Vec<f64> = l.split_whitespace().map(|f| f.parse().ok()).collect::<Option<_>>()?;
            (v.len() >= 4 && v[1..4].iter().all(|x| x.is_finite())).then(|| [v[1], v[2], v[3]])
        })
        .collect()
}

/// Measure the camera through `[start, end)` source seconds of the original at `path` (its picture
/// starting `lead` seconds in): the two passes (see the module docs) in the **background** lane of the
/// heavy-job queue, each killed after [`READ_STALL`] without a word, stopped by `cancel`
/// ([`Error::Cancelled`]). `progress` gets the fraction of the window read.
pub(crate) fn measure_motion(
    path: &Path,
    start: f64,
    end: f64,
    lead: f64,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<MotionAnalysis> {
    if !vidstab_available() {
        return Err(Error::Engine(
            "this FFmpeg has no vid.stab (libvidstab) to measure the camera with".into(),
        ));
    }
    if !(end > start) || !start.is_finite() || !end.is_finite() {
        return Err(Error::InvalidArgument("the window to measure is empty".into()));
    }
    let bin = ffmpeg_bin();
    let lease = cpu::lease_priority_waiting(cpu::Priority::Background, &mut |_| progress(0.0), cancel)?;
    if cancel() {
        return Err(Error::Cancelled);
    }
    let dir = crate::stabilize::work_dir().ok_or_else(|| Error::Engine("no directory to measure the camera in".into()))?;
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _remove = Remove(dir.clone());
    let seek = clip_seek(start, lead);
    let span = end - seek;

    let mut args = detect_args(path, seek, span);
    cpu::limit_args(&mut args, lease.threads());
    let detect = run_ffmpeg_streamed_in(
        &bin,
        &args,
        Some(&dir),
        Some(span),
        READ_STALL,
        &mut |f| progress(f * 0.95),
        cancel,
    )?;
    match detect.outcome {
        StreamOutcome::Cancelled => return Err(Error::Cancelled),
        StreamOutcome::Stalled => {
            return Err(Error::Engine(format!(
                "measuring the camera stalled: ffmpeg said nothing for {}s",
                READ_STALL.as_secs()
            )))
        }
        StreamOutcome::Ended if !detect.status.success() => {
            return Err(Error::Engine(format!(
                "measuring the camera failed: {}",
                tail(&detect.stderr)
            )));
        }
        StreamOutcome::Ended => {}
    }
    let (times, width, height) =
        parse_frames(&detect.stderr, seek).ok_or_else(|| Error::Engine("measuring the camera read no picture".into()))?;
    if times.is_empty() {
        return Err(Error::Engine("measuring the camera read no picture".into()));
    }

    let mut args = transforms_args(path, seek);
    cpu::limit_args(&mut args, lease.threads());
    let solved = run_ffmpeg_streamed_in(&bin, &args, Some(&dir), None, READ_STALL, &mut |_| {}, cancel)?;
    match solved.outcome {
        StreamOutcome::Cancelled => return Err(Error::Cancelled),
        StreamOutcome::Stalled => return Err(Error::Engine("solving the camera's motion stalled".into())),
        StreamOutcome::Ended if !solved.status.success() => {
            return Err(Error::Engine(format!(
                "solving the camera's motion failed: {}",
                tail(&solved.stderr)
            )));
        }
        StreamOutcome::Ended => {}
    }
    let text = std::fs::read_to_string(dir.join(TRANSFORMS_FILE))
        .map_err(|e| Error::Engine(format!("vid.stab wrote no transforms: {e}")))?;
    let motions = parse_transforms(&text).ok_or_else(|| Error::Engine("vid.stab's transforms did not read".into()))?;
    if motions.len() != times.len() {
        return Err(Error::Engine(format!(
            "vid.stab solved {} frames of the {} it measured",
            motions.len(),
            times.len()
        )));
    }
    progress(1.0);
    let analysis = MotionAnalysis {
        version: ANALYSIS_VERSION,
        source: source_key(path),
        width,
        height,
        times,
        motions,
    };
    if !analysis.is_sound() {
        return Err(Error::Engine("the camera's measurement is out of order".into()));
    }
    Ok(analysis)
}

/// The last lines of an ffmpeg log, for an error.
fn tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().rev().take(6).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn the_filter_list_is_read_by_name() {
        let list =
            " ... vidstabdetect     V->V       Extract relative transformations\n ... vidstabtransform  V->V       Transform";
        assert!(filters_have_vidstab(list));
        assert!(!filters_have_vidstab(
            " ... deshake           V->V       Stabilize shaky video."
        ));
        assert!(!filters_have_vidstab(" ... vidstabdetect     V->V       x"));
    }

    #[test]
    fn deshake_is_sized_to_the_frame_it_reads_and_crops_its_edges() {
        let s = Stabilize::default();
        assert_eq!(
            deshake_filter(&s, 1920),
            "deshake=rx=32:ry=32:edge=mirror,crop=w=iw/1.0333:h=ih/1.0333"
        );
        // A small frame searches the least the filter takes, a fair share of it.
        assert_eq!(
            deshake_filter(&s, 640),
            "deshake=rx=16:ry=16:edge=mirror,crop=w=iw/1.05:h=ih/1.05"
        );
        let none = Stabilize {
            strength: 1.0,
            zoom: Some(0.0),
        };
        assert_eq!(deshake_filter(&none, 3840), "deshake=rx=64:ry=64:edge=mirror");
    }

    #[test]
    fn the_passes_measure_at_one_size_and_write_plain_names() {
        let d = detect_args(Path::new("/m/a.mov"), 2.5, 4.0);
        assert_eq!(
            d.join(" "),
            "-hide_banner -nostats -ss 2.5 -i /m/a.mov -t 4.000000 -map 0:v:0 -an -sn -dn -vf \
             scale=w='min(1920,iw)':h=-2,vidstabdetect=result=motions.trf:shakiness=5:accuracy=15,showinfo -f null -"
        );
        let t = transforms_args(Path::new("/m/a.mov"), 0.0);
        assert_eq!(
            t.join(" "),
            "-hide_banner -nostats -i /m/a.mov -map 0:v:0 -an -sn -dn -frames:v 1 -vf \
             scale=w='min(1920,iw)':h=-2,vidstabtransform=input=motions.trf:debug=1 -f null -"
        );
    }

    #[test]
    fn the_frames_are_read_from_showinfo_in_the_streams_time_base() {
        let log = "\
[Parsed_showinfo_2 @ 0x1] config in time_base: 1/15360, frame_rate: 30/1
[Parsed_showinfo_2 @ 0x1] config out time_base: 0/0, frame_rate: 0/0
[Parsed_showinfo_2 @ 0x1] n:   0 pts:      0 pts_time:0       duration:    512 fmt:yuv420p cl:left sar:1/1 s:560x300 i:P
[Parsed_showinfo_2 @ 0x1] color_range:unknown color_space:unknown
[Parsed_showinfo_2 @ 0x1] n:   1 pts:    512 pts_time:0.0333333 duration:    512 fmt:yuv420p sar:1/1 s:560x300 i:P
frame=    2 fps=0.0 q=-0.0 Lsize=N/A";
        let (times, w, h) = parse_frames(log, 1.0).unwrap();
        assert_eq!((w, h), (560, 300));
        assert_eq!(times.len(), 2);
        assert_eq!(times[0], 1.0);
        assert!((times[1] - (1.0 + 512.0 / 15360.0)).abs() < 1e-12);
        assert!(parse_frames("nothing here", 0.0).is_none());
    }

    #[test]
    fn the_transforms_skip_comments_and_refuse_garbage() {
        let text = "0 0 0 0 0 1\n# no fields\n0 1.938609 -3.997462 0.003857 0.509595 0\n#\t\t 1.78 2\n";
        assert_eq!(
            parse_transforms(text).unwrap(),
            vec![[0.0, 0.0, 0.0], [1.938609, -3.997462, 0.003857]]
        );
        assert!(parse_transforms("0 x y z 0 0\n").is_none());
    }

    #[test]
    fn a_clip_waiting_for_its_analysis_or_one_that_cannot_be_steadied_gets_nothing() {
        let asset = av_asset(uuid::Uuid::from_u128(11), 20.0);
        let mut clip = Clip::new(asset.id, 1.0, 3.0, 0.0);
        clip.stabilize = Some(Stabilize::default());
        with_vidstab(true, || {
            crate::stabilize::pinned::with(HashMap::new(), || {
                assert_eq!(stabilizer(&clip, Some(&asset), (1.0, 3.0)), None);
            });
            let analyses = HashMap::from([(asset.id, Arc::new(crate::stabilize::pinned::shaky(10.0, 25.0, 1920, 1080)))]);
            crate::stabilize::pinned::with(analyses, || {
                let s = stabilizer(&clip, Some(&asset), (1.0, 3.0)).unwrap();
                assert!(
                    s.filters().starts_with("vidstabtransform=input='/kerf-test/stabilize/"),
                    "{s:?}"
                );
                assert!(s.filters().ends_with(":crop=black:interpol=bicubic"), "{s:?}");
                assert!((s.trim_start().unwrap() - 0.98).abs() < 1e-9);
                let mut off = clip.clone();
                off.stabilize = None;
                assert_eq!(stabilizer(&off, Some(&asset), (1.0, 3.0)), None);
                let still = img_asset(asset.id);
                assert_eq!(stabilizer(&clip, Some(&still), (1.0, 3.0)), None);
            });
        });
        with_vidstab(false, || {
            let s = stabilizer(&clip, Some(&asset), (1.0, 3.0)).unwrap();
            assert!(s.filters().starts_with("deshake=rx="), "{s:?}");
            assert_eq!(s.trim_start(), None);
        });
    }
}
