//! Drawing a thumbnail ([`crate::model::Thumbnail`]) and suggesting the frames worth one.
//!
//! **The background is the cover frame.** It comes off the export graph (`export_still`, the
//! originals, PNG), of the cut with only the graphics the thumbnail keeps, rendered larger than the
//! delivery frame when the crop needs the pixels (`Thumbnail::background_scale`) — so a vertical
//! thumbnail of a landscape cut is not an upscaled sliver. At scale 1 the argv is the cover's. The
//! crop, the layers and the encode are not ffmpeg's: the frame is cropped and scaled with tiny-skia
//! and each layer is drawn by the titles' own rasteriser (`svg::raster`, the fonts the project
//! embeds), so a template looks as it does on the cut. A PNG is written here; JPEG and WebP are
//! that PNG re-encoded by ffmpeg (`transcode_image`).
//!
//! Everything here runs without the project: the caller resolves its inputs under the lock and
//! releases it. Every ffmpeg run is bounded and polls `cancel`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tiny_skia::{Color, FilterQuality, Pixmap, PixmapPaint, Transform};
use uuid::Uuid;

use crate::engine;
use crate::error::{Error, Result};
use crate::fonts;
use crate::model::thumbnail::settled_moment;
use crate::model::{Asset, StreamKind, Thumbnail, ThumbnailLayer, Timeline, VideoGraphics};
use crate::svg::{raster, SvgDoc};

/// The image file formats a thumbnail is written as, by the path's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailFormat {
    Png,
    Jpeg,
    Webp,
}

impl ThumbnailFormat {
    /// The format `path`'s extension names: `.png`, `.jpg` / `.jpeg` or `.webp`.
    pub fn from_path(path: &Path) -> Result<Self> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => Ok(Self::Png),
            Some("jpg" | "jpeg") => Ok(Self::Jpeg),
            Some("webp") => Ok(Self::Webp),
            _ => Err(Error::InvalidArgument(format!(
                "{} is not a picture Kerf writes: name a .png, .jpg or .webp file",
                path.display()
            ))),
        }
    }

    fn codec(self) -> &'static [&'static str] {
        match self {
            Self::Png => &[],
            Self::Jpeg => &["-c:v", "mjpeg", "-q:v", "2"],
            Self::Webp => &["-c:v", "libwebp", "-quality", "90"],
        }
    }
}

/// A temporary file removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(ext: &str) -> Self {
        Self(std::env::temp_dir().join(format!("kerf-thumbnail-{}.{ext}", Uuid::new_v4().simple())))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn alloc(w: u32, h: u32) -> Result<Pixmap> {
    Pixmap::new(w.max(1), h.max(1)).ok_or_else(|| Error::Engine(format!("cannot allocate a {w} x {h} picture")))
}

fn even(v: f64) -> u32 {
    ((v.round() as u32).max(2)) & !1
}

/// The background of `thumb`, `out_w` x `out_h`: its frame of the cut, cropped and scaled. `assets` are
/// the originals for a file and may be the proxies for a preview.
fn background(
    thumb: &Thumbnail,
    timeline: &Timeline,
    assets: &[Asset],
    (out_w, out_h): (u32, u32),
    cancel: &dyn Fn() -> bool,
) -> Result<Pixmap> {
    let cut = thumb.graphics.background_of(timeline);
    let (fw, fh) = engine::delivery_frame(&cut.for_render(), assets);
    let k = thumb.background_scale((fw, fh)) * f64::from(out_w) / f64::from(thumb.width.max(1));
    let (rw, rh) = (even(f64::from(fw) * k), even(f64::from(fh) * k));
    let opts = engine::ExportOptions {
        hwaccel: engine::decode_hwaccel(),
        resolution: ((rw, rh) != (fw, fh)).then_some((rw, rh)),
        ..engine::ExportOptions::default()
    };
    let still = Scratch::new("png");
    engine::export_still_cancellable(&cut, assets, &opts, thumb.time, &still.0, engine::ImageFormat::Png, 2, cancel)?;
    let frame = Pixmap::load_png(&still.0).map_err(|e| Error::Engine(format!("could not read the background frame: {e}")))?;
    let (cx, cy, cw, _) = thumb.crop_in(f64::from(frame.width()), f64::from(frame.height()));
    let s = (f64::from(out_w) / cw) as f32;
    let mut out = alloc(out_w, out_h)?;
    out.fill(Color::BLACK);
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..PixmapPaint::default()
    };
    out.draw_pixmap(
        0,
        0,
        frame.as_ref(),
        &paint,
        Transform::from_row(s, 0.0, 0.0, s, -(cx as f32) * s, -(cy as f32) * s),
        None,
    );
    Ok(out)
}

/// The moment of `doc` a layer draws by default: 0 for a still, else the last of 17 moments across
/// its run at which the most of it shows ([`settled_moment`]).
fn default_moment(doc: &SvgDoc, aspect: f64, db: &Arc<fontdb::Database>) -> Result<f64> {
    if !doc.is_animated() {
        return Ok(0.0);
    }
    let run = doc.natural_duration().unwrap_or_else(|| doc.default_span());
    let (w, h) = if aspect >= 1.0 {
        (96, ((96.0 / aspect).round() as u32).max(1))
    } else {
        (((96.0 * aspect).round() as u32).max(1), 96)
    };
    let mut samples = Vec::with_capacity(17);
    for i in 0..=16 {
        let t = run * f64::from(i) / 16.0;
        let pic = raster::render_with(&doc.at(t), w, h, 1.0, db.clone())?;
        let cover: f64 = pic.pixels().iter().map(|p| f64::from(p.alpha())).sum();
        samples.push((t, cover));
    }
    Ok(settled_moment(&samples))
}

/// Draw `layer` onto `out` (a `tw` x `th` thumbnail at the scale of `out`).
fn draw_layer(out: &mut Pixmap, layer: &ThumbnailLayer, db: &Arc<fontdb::Database>) -> Result<()> {
    let Some((x, y, w, h)) = layer.rect_px(out.width(), out.height()) else {
        return Ok(());
    };
    let doc = SvgDoc::parse(&layer.svg)?;
    let at = match layer.at {
        Some(at) => at,
        None => default_moment(&doc, f64::from(w) / f64::from(h), db)?,
    };
    let pic = raster::render_with(&doc.at(at), w, h, layer.opacity, db.clone())?;
    let (x, y) = (i32::try_from(x).unwrap_or(i32::MIN), i32::try_from(y).unwrap_or(i32::MIN));
    out.draw_pixmap(x, y, pic.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    Ok(())
}

/// `thumb` drawn `width` pixels wide (its own width, or less for a preview; never more), its layers on
/// its background frame.
pub fn render(thumb: &Thumbnail, timeline: &Timeline, assets: &[Asset], width: u32, cancel: &dyn Fn() -> bool) -> Result<Pixmap> {
    thumb.check()?;
    let out_w = width.clamp(16, thumb.width);
    let out_h = ((f64::from(thumb.height) * f64::from(out_w) / f64::from(thumb.width)).round() as u32).max(1);
    let mut out = background(thumb, timeline, assets, (out_w, out_h), cancel)?;
    let db = fonts::snapshot_for(thumb.layers.iter().map(|l| l.svg.as_str())).db();
    for layer in &thumb.layers {
        if cancel() {
            return Err(Error::Cancelled);
        }
        draw_layer(&mut out, layer, &db)?;
    }
    Ok(out)
}

/// Write `thumb` at full size to `path`, in the format its extension names. A failure or a cancel
/// leaves no partial file behind (an earlier file at `path` this run never touched stays).
pub fn write(
    thumb: &Thumbnail,
    timeline: &Timeline,
    assets: &[Asset],
    path: &Path,
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    let format = ThumbnailFormat::from_path(path)?;
    let picture = render(thumb, timeline, assets, thumb.width, cancel)?;
    let png = raster::encode_png(&picture)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    if format == ThumbnailFormat::Png {
        raster::write_atomic(path, &png)?;
    } else {
        let scratch = Scratch::new("png");
        std::fs::write(&scratch.0, &png)?;
        engine::transcode_image(&scratch.0, path, format.codec(), cancel)?;
    }
    Ok(path.to_path_buf())
}

// ---- frame suggestions ------------------------------------------------------------------------

/// The most frames one suggestion looks at.
pub const MAX_CANDIDATES: usize = 24;
/// The most frames one suggestion returns.
pub const MAX_SUGGESTIONS: usize = 12;
/// How wide each looked-at frame is decoded.
const MEASURE_WIDTH: u32 = 320;
/// How many frames are decoded side by side.
const MEASURE_THREADS: usize = 3;

/// One suggested background frame and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuggestedFrame {
    /// Timeline seconds.
    pub time: f64,
    /// 0..1, higher is better: half sharpness, a quarter exposure, a quarter contrast.
    pub score: f64,
    /// Detail (Laplacian energy) relative to the sharpest frame looked at, 0..1.
    pub sharpness: f64,
    /// 1 for a mid-grey mean with nothing clipped, lower when darker, brighter or clipped.
    pub exposure: f64,
    /// Spread of the brightness, 0..1.
    pub contrast: f64,
}

/// What [`suggest_frames`] found: the best frames, best first, and a PNG contact sheet of them.
#[derive(Debug, Clone)]
pub struct FrameSuggestions {
    pub frames: Vec<SuggestedFrame>,
    /// How many frames were looked at (black, faded and flat ones dropped).
    pub looked_at: usize,
    pub sheet_png: Vec<u8>,
}

/// The stretches of the cut where a still is a clean picture of one shot: each video clip's span less
/// its fades and the transitions into it and out of it, as `(start, end)` timeline seconds.
fn clean_windows(timeline: &Timeline) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for track in timeline.tracks.iter().filter(|t| t.kind == StreamKind::Video) {
        for (i, clip) in track.clips.iter().enumerate() {
            let (start, end) = (clip.timeline_start, clip.timeline_end());
            let into = clip.transition_in.as_ref().map_or(0.0, |t| t.duration);
            let out_of = track
                .clips
                .get(i + 1)
                .filter(|n| (n.timeline_start - end).abs() < 0.05)
                .and_then(|n| n.transition_in.as_ref())
                .map_or(0.0, |t| t.duration);
            let (a, b) = (start + into + clip.fade_in + 0.1, end - out_of - clip.fade_out - 0.1);
            if b > a {
                out.push((a, b));
            }
        }
    }
    out
}

/// The times worth looking at: the middle of each shot (a clip's clean window split at the scene
/// changes of its footage, `scenes` by asset, in source seconds), topped up with times spread evenly
/// through the clean windows, all inside `range`, at most `max`, in time order. Pure.
pub fn candidate_times(
    timeline: &Timeline,
    scenes: &std::collections::HashMap<Uuid, Vec<f64>>,
    range: Option<(f64, f64)>,
    max: usize,
) -> Vec<f64> {
    let cut = timeline.for_render();
    let (lo, hi) = range.unwrap_or((0.0, cut.duration()));
    let windows: Vec<(f64, f64)> = clean_windows(&cut)
        .into_iter()
        .filter_map(|(a, b)| {
            let (a, b) = (a.max(lo), b.min(hi));
            (b > a).then_some((a, b))
        })
        .collect();
    let mut times = Vec::new();
    for track in cut.tracks.iter().filter(|t| t.kind == StreamKind::Video) {
        for clip in track.clips.iter().filter(|c| c.generator.is_none()) {
            let Some(&(a, b)) = windows
                .iter()
                .find(|(a, b)| *a >= clip.timeline_start - 1e-9 && *b <= clip.timeline_end() + 1e-9)
            else {
                continue;
            };
            let mut cuts = vec![a];
            if clip.speed > 0.0 {
                for &s in scenes.get(&clip.asset_id).map(Vec::as_slice).unwrap_or_default() {
                    let t = clip.timeline_start + (s - clip.source_in) / clip.speed;
                    if t > a && t < b {
                        cuts.push(t);
                    }
                }
            }
            cuts.push(b);
            cuts.sort_by(f64::total_cmp);
            times.extend(cuts.windows(2).filter(|w| w[1] - w[0] >= 0.3).map(|w| (w[0] + w[1]) / 2.0));
        }
    }
    let total: f64 = windows.iter().map(|(a, b)| b - a).sum();
    let want = max.saturating_sub(times.len()).max(max / 3);
    if total > 0.0 && want > 0 {
        let step = total / want as f64;
        for k in 0..want {
            let mut at = step * (k as f64 + 0.5);
            for &(a, b) in &windows {
                if at <= b - a {
                    times.push(a + at);
                    break;
                }
                at -= b - a;
            }
        }
    }
    times.sort_by(f64::total_cmp);
    times.dedup_by(|b, a| *b - *a < 0.25);
    if times.len() > max {
        let n = times.len();
        times = (0..max).map(|i| times[i * n / max]).collect();
    }
    times
}

/// What one decoded frame measures: mean and spread of its brightness, the share of clipped pixels, and
/// the energy of its Laplacian (detail). From the Y plane of a limited-range frame. Pure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStats {
    pub mean: f64,
    pub spread: f64,
    pub clipped: f64,
    pub detail: f64,
}

pub fn frame_stats(luma: &[u8], width: u32, height: u32) -> FrameStats {
    let (w, h) = (width as usize, height as usize);
    let y = |v: u8| ((f64::from(v) - 16.0) / 219.0).clamp(0.0, 1.0);
    let n = (w * h).max(1) as f64;
    let (mut sum, mut sq, mut clipped) = (0.0, 0.0, 0usize);
    for &v in &luma[..w * h] {
        let v = y(v);
        sum += v;
        sq += v * v;
        if !(0.02..=0.98).contains(&v) {
            clipped += 1;
        }
    }
    let mean = sum / n;
    let spread = (sq / n - mean * mean).max(0.0).sqrt();
    let (mut lsum, mut lsq, mut count) = (0.0, 0.0, 0usize);
    for r in 1..h.saturating_sub(1) {
        for c in 1..w.saturating_sub(1) {
            let at = |rr: usize, cc: usize| y(luma[rr * w + cc]);
            let l = 4.0 * at(r, c) - at(r - 1, c) - at(r + 1, c) - at(r, c - 1) - at(r, c + 1);
            lsum += l;
            lsq += l * l;
            count += 1;
        }
    }
    let count = count.max(1) as f64;
    let detail = (lsq / count - (lsum / count).powi(2)).max(0.0);
    FrameStats {
        mean,
        spread,
        clipped: clipped as f64 / n,
        detail,
    }
}

/// Whether a frame is nothing worth showing: black, faded out or flat.
pub fn is_blank(s: &FrameStats) -> bool {
    s.mean < 0.07 || s.spread < 0.04
}

/// The scores of measured frames (`sharpness` relative to the most detailed of them). Pure.
pub fn score_frames(measured: &[(f64, FrameStats)]) -> Vec<SuggestedFrame> {
    let most = measured.iter().map(|(_, s)| s.detail.sqrt()).fold(0.0, f64::max);
    let round = |v: f64| (v * 1000.0).round() / 1000.0;
    measured
        .iter()
        .map(|&(time, s)| {
            let sharpness = if most > 0.0 { s.detail.sqrt() / most } else { 0.0 };
            let exposure = (1.0 - (s.mean - 0.5).abs() * 2.0 - s.clipped * 2.0).clamp(0.0, 1.0);
            let contrast = (s.spread / 0.25).min(1.0);
            SuggestedFrame {
                time: round(time),
                score: round(0.5 * sharpness + 0.25 * exposure + 0.25 * contrast),
                sharpness: round(sharpness),
                exposure: round(exposure),
                contrast: round(contrast),
            }
        })
        .collect()
}

/// The best `count` of `scored`, best first, no two closer than `gap` seconds. Pure.
pub fn pick_best(mut scored: Vec<SuggestedFrame>, count: usize, gap: f64) -> Vec<SuggestedFrame> {
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.time.total_cmp(&b.time)));
    let mut out: Vec<SuggestedFrame> = Vec::new();
    for f in scored {
        if out.len() == count {
            break;
        }
        if out.iter().all(|o| (o.time - f.time).abs() >= gap) {
            out.push(f);
        }
    }
    out
}

/// Suggest the `count` best background frames of `timeline` (inside `range` when given): candidate
/// times from the shots ([`candidate_times`]), each decoded small through the still graph without the
/// video's graphics, black / faded / flat ones dropped, the rest scored ([`score_frames`]). At most
/// [`MAX_CANDIDATES`] decodes, a few at a time, each bounded; `cancel` is polled between them.
pub fn suggest_frames(
    timeline: &Timeline,
    assets: &[Asset],
    scenes: &std::collections::HashMap<Uuid, Vec<f64>>,
    count: usize,
    range: Option<(f64, f64)>,
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<FrameSuggestions> {
    let count = count.clamp(1, MAX_SUGGESTIONS);
    let times = candidate_times(timeline, scenes, range, MAX_CANDIDATES);
    if times.is_empty() {
        return Err(Error::InvalidArgument(
            "there is no picture to suggest a frame from: the cut (or the range) has no video clip with a clean stretch".into(),
        ));
    }
    let cut = VideoGraphics::None.background_of(timeline);
    let opts = engine::ExportOptions::default();
    let next = AtomicUsize::new(0);
    type Decoded = (f64, u32, u32, Vec<u8>);
    let decoded: Mutex<Vec<Decoded>> = Mutex::new(Vec::new());
    let failure: Mutex<Option<Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..MEASURE_THREADS.min(times.len()) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= times.len() || cancel() || failure.lock().unwrap_or_else(PoisonError::into_inner).is_some() {
                    break;
                }
                match engine::timeline_frame_yuv_cancellable(&cut, assets, &opts, times[i], MEASURE_WIDTH, cancel) {
                    Ok((w, h, planes)) => decoded
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((times[i], w, h, planes)),
                    Err(e) => {
                        failure.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(e);
                    }
                }
            });
        }
    });
    if cancel() {
        return Err(Error::Cancelled);
    }
    if let Some(e) = failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
        return Err(e);
    }
    let mut decoded = decoded.into_inner().unwrap_or_else(PoisonError::into_inner);
    decoded.sort_by(|a, b| a.0.total_cmp(&b.0));
    let measured: Vec<(f64, FrameStats)> = decoded
        .iter()
        .map(|(t, w, h, planes)| (*t, frame_stats(planes, *w, *h)))
        .filter(|(_, s)| !is_blank(s))
        .collect();
    let span = range.map_or(timeline.duration(), |(a, b)| b - a);
    let frames = pick_best(score_frames(&measured), count, (span / (count as f64 * 4.0)).clamp(0.5, 10.0));
    if frames.is_empty() {
        return Err(Error::InvalidArgument(
            "every frame looked at is black, faded or flat: try another range".into(),
        ));
    }
    let pictures: Vec<(&SuggestedFrame, &Decoded)> = frames
        .iter()
        .filter_map(|f| decoded.iter().find(|d| (d.0 - f.time).abs() < 0.001).map(|d| (f, d)))
        .collect();
    let sheet_png = suggestion_sheet(&pictures)?;
    Ok(FrameSuggestions {
        frames,
        looked_at: measured.len(),
        sheet_png,
    })
}

/// A PNG grid of the suggested frames, best first, each labelled with its time and score.
fn suggestion_sheet(pictures: &[(&SuggestedFrame, &(f64, u32, u32, Vec<u8>))]) -> Result<Vec<u8>> {
    const GAP: u32 = 6;
    let (cw, ch) = pictures
        .iter()
        .fold((1, 1), |(w, h), (_, d)| (u32::max(w, d.1), u32::max(h, d.2)));
    let label_h = 22;
    let cols = if pictures.len() <= 4 { 2.min(pictures.len()) } else { 3 } as u32;
    let rows = (pictures.len() as u32).div_ceil(cols.max(1));
    let mut sheet = alloc(cols * (cw + GAP) + GAP, rows * (ch + label_h + GAP) + GAP)?;
    sheet.fill(Color::from_rgba8(28, 28, 28, 255));
    for (n, (f, (_, w, h, planes))) in pictures.iter().enumerate() {
        let (col, row) = (n as u32 % cols, n as u32 / cols);
        let (x, y) = (GAP + col * (cw + GAP), GAP + row * (ch + label_h + GAP));
        let rgba = crate::scopes::yuv444_to_rgba(planes, *w, *h, crate::scopes::delivery_matrix(*w, *h));
        let frame = Pixmap::from_vec(
            rgba,
            tiny_skia::IntSize::from_wh(*w, *h).ok_or_else(|| Error::Engine("an empty frame".into()))?,
        )
        .ok_or_else(|| Error::Engine("could not hold a suggested frame".into()))?;
        sheet.draw_pixmap(
            x as i32,
            y as i32,
            frame.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        let label = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {cw} {label_h}\"><text x=\"2\" y=\"16\" font-size=\"14\" fill=\"#d0d0d0\">#{} · {:.2}s · score {:.2}</text></svg>",
            n + 1,
            f.time,
            f.score
        );
        if let Ok(label) = raster::render(&label, cw, label_h, 1.0) {
            sheet.draw_pixmap(
                x as i32,
                (y + ch) as i32,
                label.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
    }
    raster::encode_png(&sheet)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::model::{Clip, Transition, TransitionKind};

    fn cut(lengths: &[f64]) -> (Timeline, Vec<Uuid>) {
        let mut tl = Timeline::new();
        let v1 = tl.first_track_of(StreamKind::Video).unwrap();
        let mut at = 0.0;
        let mut ids = Vec::new();
        for &len in lengths {
            let asset = Uuid::new_v4();
            ids.push(asset);
            tl.track_mut(v1).unwrap().clips.push(Clip::new(asset, 0.0, len, at));
            at += len;
        }
        (tl, ids)
    }

    #[test]
    fn candidates_are_mid_shot_away_from_fades_and_transitions_and_inside_the_range() {
        let (mut tl, ids) = cut(&[4.0, 4.0]);
        let track = &mut tl.tracks[0];
        track.clips[0].fade_in = 1.0;
        track.clips[1].transition_in = Some(Transition {
            kind: TransitionKind::DipToWhite,
            duration: 1.0,
        });
        let scenes = HashMap::from([(ids[1], vec![2.0])]);
        let times = candidate_times(&tl, &scenes, None, 4);
        assert!(!times.is_empty() && times.len() <= 4);
        for t in &times {
            assert!(*t > 1.1 && !(2.9..5.1).contains(t) && *t < 7.9, "{t} in {times:?}");
        }
        let windows = clean_windows(&tl);
        assert_eq!(windows.len(), 2);
        assert!(
            (windows[0].0 - 1.1).abs() < 1e-9 && (windows[0].1 - 2.9).abs() < 1e-9,
            "{windows:?}"
        );
        let shots = candidate_times(&tl, &scenes, None, 24);
        assert!(
            shots.iter().any(|t| (t - 2.0).abs() < 1e-9),
            "the middle of the first shot: {shots:?}"
        );
        assert!(
            shots.iter().any(|t| (t - 5.55).abs() < 1e-9),
            "the middle of the second's first scene: {shots:?}"
        );
        let ranged = candidate_times(&tl, &scenes, Some((5.0, 8.0)), 24);
        assert!(
            ranged.iter().all(|t| (5.0..=8.0).contains(t)) && !ranged.is_empty(),
            "{ranged:?}"
        );
        assert!(candidate_times(&Timeline::new(), &HashMap::new(), None, 8).is_empty());
    }

    #[test]
    fn the_stats_tell_black_flat_and_detailed_frames_apart() {
        let (w, h) = (32u32, 32u32);
        let black = vec![16u8; 32 * 32];
        assert!(is_blank(&frame_stats(&black, w, h)));
        let grey = vec![126u8; 32 * 32];
        assert!(is_blank(&frame_stats(&grey, w, h)));
        let checker: Vec<u8> = (0..32 * 32)
            .map(|i| if (i % 32 + i / 32) % 2 == 0 { 40 } else { 200 })
            .collect();
        let stripes: Vec<u8> = (0..32 * 32).map(|i| if (i / 32) < 16 { 40 } else { 200 }).collect();
        let (c, s) = (frame_stats(&checker, w, h), frame_stats(&stripes, w, h));
        assert!(!is_blank(&c) && !is_blank(&s));
        assert!(c.detail > s.detail * 10.0, "{c:?} {s:?}");
        let scored = score_frames(&[(1.0, c), (2.0, s)]);
        assert_eq!(scored[0].sharpness, 1.0);
        assert!(scored[0].score > scored[1].score);
    }

    #[test]
    fn the_best_are_picked_apart_from_each_other() {
        let f = |time: f64, score: f64| SuggestedFrame {
            time,
            score,
            sharpness: 0.0,
            exposure: 0.0,
            contrast: 0.0,
        };
        let picked = pick_best(vec![f(1.0, 0.5), f(1.2, 0.9), f(5.0, 0.7), f(9.0, 0.1)], 2, 1.0);
        assert_eq!(picked.iter().map(|p| p.time).collect::<Vec<_>>(), vec![1.2, 5.0]);
    }

    #[test]
    fn a_format_is_named_by_the_extension() {
        assert_eq!(ThumbnailFormat::from_path(Path::new("a.PNG")).unwrap(), ThumbnailFormat::Png);
        assert_eq!(
            ThumbnailFormat::from_path(Path::new("a.jpeg")).unwrap(),
            ThumbnailFormat::Jpeg
        );
        assert_eq!(
            ThumbnailFormat::from_path(Path::new("a.webp")).unwrap(),
            ThumbnailFormat::Webp
        );
        assert!(ThumbnailFormat::from_path(Path::new("a.gif")).is_err());
    }
}
