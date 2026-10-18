//! Video scopes of one frame: the luma waveform, the RGB parade, the histogram and the Y'CbCr
//! vectorscope as count grids, plus a numeric summary an agent can read without looking.
//!
//! The input is an RGBA frame of display-encoded code values (alpha ignored). Y'CbCr comes from
//! the frame's [`YuvMatrix`] — the one the delivery is decoded with ([`delivery_matrix`]) unless
//! the caller names one. The **summary and the histogram read every pixel**: a single clipped
//! pixel shows in `clipped_high`. The waveform, parade and vectorscope read a decimated frame
//! (every `step`th sample, at most [`MAX_SAMPLE_W`] x [`MAX_SAMPLE_H`]), which is enough to draw.
//!
//! Grids are `counts[row * columns + column]` with row 0 the **lowest** level (waveform, parade)
//! or the most negative Cr (vectorscope, Cb across), so a drawer flips them. Levels are 0..1
//! (code value / 255; Y' is the matrix's weighted sum of R'G'B'), Cb and Cr -0.5..0.5, hue the
//! angle of (Cb, Cr) counter-clockwise from +Cb in degrees, saturation `|CbCr| / 0.5`.
//!
//! Pure apart from [`timeline_scopes`], which renders the composited still through FFmpeg.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{Asset, Timeline};
use crate::render_plan::YuvMatrix;

/// The decimated frame the grids are binned from is at most this wide and tall.
pub const MAX_SAMPLE_W: u32 = 480;
pub const MAX_SAMPLE_H: u32 = 270;
/// Half the vectorscope's side in Cb / Cr units: every matrix's 100% primaries fit.
pub const VECTOR_EXTENT: f64 = 0.6;
/// The vectorscope grid peaks are found on, whatever size the caller draws.
const PEAK_GRID: u32 = 64;
const MAX_PEAKS: usize = 5;
/// A peak holds at least this share of the samples.
const MIN_PEAK_SHARE: f64 = 0.01;

/// One of the drawn scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
}

/// The matrix a caller asks the scopes to read the frame with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMatrix {
    /// The delivery's: [`delivery_matrix`] of the frame the cut renders at.
    #[default]
    Auto,
    Bt601,
    Bt709,
    Bt2020,
}

impl ScopeMatrix {
    pub fn resolve(self, frame: (u32, u32)) -> YuvMatrix {
        match self {
            Self::Auto => delivery_matrix(frame.0, frame.1),
            Self::Bt601 => YuvMatrix::Bt601,
            Self::Bt709 => YuvMatrix::Bt709,
            Self::Bt2020 => YuvMatrix::Bt2020,
        }
    }
}

/// The matrix a player decodes an untagged delivery of this frame with (the export is SDR and
/// carries no colour tags): BT.709 for HD and up, BT.601 for standard definition.
pub fn delivery_matrix(width: u32, height: u32) -> YuvMatrix {
    if width >= 1280 || height > 576 {
        YuvMatrix::Bt709
    } else {
        YuvMatrix::Bt601
    }
}

pub fn matrix_name(m: YuvMatrix) -> &'static str {
    match m {
        YuvMatrix::Bt601 => "bt601",
        YuvMatrix::Bt709 => "bt709",
        YuvMatrix::Bt2020 => "bt2020",
    }
}

/// How finely the drawn grids are binned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridSize {
    /// Waveform / parade columns (each parade channel has this many).
    pub columns: u32,
    /// Waveform / parade levels.
    pub rows: u32,
    /// The vectorscope's side.
    pub vector: u32,
}

impl Default for GridSize {
    fn default() -> Self {
        Self {
            columns: 160,
            rows: 128,
            vector: 96,
        }
    }
}

impl GridSize {
    pub fn clamped(self) -> Self {
        Self {
            columns: self.columns.clamp(8, 512),
            rows: self.rows.clamp(8, 256),
            vector: self.vector.clamp(8, 256),
        }
    }
}

/// Which grids to return besides the summary, and at what size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeOptions {
    pub grids: Vec<ScopeKind>,
    pub size: GridSize,
}

/// A frame of RGBA code values, row-major.
#[derive(Debug, Clone, Copy)]
pub struct RgbaFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

/// One channel over the whole frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChannelStats {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    /// Pixels at 0 / at full scale.
    pub clipped_low: u64,
    pub clipped_high: u64,
    /// The same, in percent of the frame (unrounded: one pixel of a 4K frame is 0.000012 %).
    pub clipped_low_pct: f64,
    pub clipped_high_pct: f64,
}

/// The frame's colour in Y'CbCr.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ChromaStats {
    /// Mean Cb and Cr: a cast is the mean away from 0.
    pub cb: f64,
    pub cr: f64,
    /// Hue and saturation of the mean (the cast's direction and strength).
    pub hue_deg: f64,
    pub saturation: f64,
    /// The mean of every pixel's saturation: how colourful the frame is.
    pub mean_saturation: f64,
}

/// A dense spot of the vectorscope.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VectorPeak {
    pub cb: f64,
    pub cr: f64,
    pub hue_deg: f64,
    pub saturation: f64,
    /// Share of the samples in this spot (0..1).
    pub share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeSummary {
    pub width: u32,
    pub height: u32,
    /// `bt601` / `bt709` / `bt2020`.
    pub matrix: String,
    pub red: ChannelStats,
    pub green: ChannelStats,
    pub blue: ChannelStats,
    /// Y'. Clipped low / high is black / white in all three channels.
    pub luma: ChannelStats,
    pub chroma: ChromaStats,
    /// The densest vectorscope spots, densest first.
    pub peaks: Vec<VectorPeak>,
}

/// A count grid: `counts[row * columns + column]`, row 0 the lowest level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grid {
    pub columns: u32,
    pub rows: u32,
    pub counts: Vec<u32>,
}

impl Grid {
    fn new(columns: u32, rows: u32) -> Self {
        Self {
            columns,
            rows,
            counts: vec![0; (columns * rows) as usize],
        }
    }

    fn hit(&mut self, column: u32, row: u32) {
        self.counts[(row * self.columns + column) as usize] += 1;
    }

    pub fn at(&self, column: u32, row: u32) -> u32 {
        self.counts[(row * self.columns + column) as usize]
    }
}

/// 256 bins per channel, over every pixel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Histogram {
    pub red: Vec<u32>,
    pub green: Vec<u32>,
    pub blue: Vec<u32>,
    pub luma: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scopes {
    pub summary: ScopeSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waveform: Option<Grid>,
    /// Red, green, blue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parade: Option<Vec<Grid>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vectorscope: Option<Grid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub histogram: Option<Histogram>,
}

/// Y', Cb, Cr of normalized R'G'B' under `m`.
pub fn ycbcr(m: YuvMatrix, r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (kr, kb) = m.weights();
    let y = kr * r + (1.0 - kr - kb) * g + kb * b;
    (y, (b - y) / (2.0 * (1.0 - kb)), (r - y) / (2.0 * (1.0 - kr)))
}

/// Limited-range 4:4:4 planar Y'CbCr (Y plane, then Cb, then Cr) as RGBA under `m`: what a
/// player shows of those samples. Values the RGB cube cannot hold are clipped, as on screen.
pub fn yuv444_to_rgba(planes: &[u8], width: u32, height: u32, m: YuvMatrix) -> Vec<u8> {
    let n = (width as usize) * (height as usize);
    let (kr, kb) = m.weights();
    let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut out = vec![255u8; n * 4];
    if planes.len() < n * 3 {
        return out;
    }
    let (ys, rest) = planes.split_at(n);
    let (us, vs) = rest.split_at(n);
    for i in 0..n {
        let y = (ys[i] as f64 - 16.0) / 219.0;
        let cb = (us[i] as f64 - 128.0) / 224.0;
        let cr = (vs[i] as f64 - 128.0) / 224.0;
        let r = y + 2.0 * (1.0 - kr) * cr;
        let b = y + 2.0 * (1.0 - kb) * cb;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        out[i * 4] = to8(r);
        out[i * 4 + 1] = to8(g);
        out[i * 4 + 2] = to8(b);
    }
    out
}

fn hue_deg(cb: f64, cr: f64) -> f64 {
    if cb == 0.0 && cr == 0.0 {
        return 0.0;
    }
    cr.atan2(cb).to_degrees().rem_euclid(360.0)
}

fn saturation(cb: f64, cr: f64) -> f64 {
    cb.hypot(cr) / 0.5
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[derive(Default)]
struct Acc {
    min: f64,
    max: f64,
    sum: f64,
    low: u64,
    high: u64,
}

impl Acc {
    fn new() -> Self {
        Self {
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            ..Self::default()
        }
    }

    fn add(&mut self, v: f64, low: bool, high: bool) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v;
        self.low += low as u64;
        self.high += high as u64;
    }

    fn stats(&self, n: u64) -> ChannelStats {
        let pct = |c: u64| if n == 0 { 0.0 } else { c as f64 * 100.0 / n as f64 };
        let (min, max) = if n == 0 { (0.0, 0.0) } else { (self.min, self.max) };
        ChannelStats {
            min: round4(min),
            max: round4(max),
            mean: round4(if n == 0 { 0.0 } else { self.sum / n as f64 }),
            clipped_low: self.low,
            clipped_high: self.high,
            clipped_low_pct: pct(self.low),
            clipped_high_pct: pct(self.high),
        }
    }
}

/// The step between the samples the grids read, across and down.
pub fn decimation(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(MAX_SAMPLE_W).max(1), height.div_ceil(MAX_SAMPLE_H).max(1))
}

fn level_row(v: f64, rows: u32) -> u32 {
    ((v.clamp(0.0, 1.0) * (rows - 1) as f64).round() as u32).min(rows - 1)
}

fn vector_cell(c: f64, size: u32) -> u32 {
    let t = (c + VECTOR_EXTENT) / (2.0 * VECTOR_EXTENT);
    ((t * size as f64).floor().max(0.0) as u32).min(size - 1)
}

fn vector_centre(cell: u32, size: u32) -> f64 {
    (cell as f64 + 0.5) / size as f64 * 2.0 * VECTOR_EXTENT - VECTOR_EXTENT
}

/// The scopes of `frame` read with `m`. See the [module](self).
pub fn analyze(frame: RgbaFrame<'_>, m: YuvMatrix, opts: &ScopeOptions) -> Scopes {
    let (w, h) = (frame.width, frame.height);
    let n = (w as usize) * (h as usize);
    let px = if frame.pixels.len() >= n * 4 { frame.pixels } else { &[][..] };
    let n = if px.is_empty() { 0 } else { n };
    let wants = |k| opts.grids.contains(&k);
    let mut size = opts.size.clamped();
    // No more columns than samples across, or a narrow frame draws as stripes.
    size.columns = size.columns.min(w.div_ceil(decimation(w, h).0).max(1));

    let mut acc = [Acc::new(), Acc::new(), Acc::new(), Acc::new()];
    let (mut cb_sum, mut cr_sum, mut sat_sum) = (0.0, 0.0, 0.0);
    let mut hist = Histogram {
        red: vec![0; 256],
        green: vec![0; 256],
        blue: vec![0; 256],
        luma: vec![0; 256],
    };
    for p in px.chunks_exact(4).take(n) {
        let (r8, g8, b8) = (p[0], p[1], p[2]);
        let (r, g, b) = (r8 as f64 / 255.0, g8 as f64 / 255.0, b8 as f64 / 255.0);
        let (y, cb, cr) = ycbcr(m, r, g, b);
        for (a, (v, c)) in acc.iter_mut().zip([(r, r8), (g, g8), (b, b8)]) {
            a.add(v, c == 0, c == 255);
        }
        let black = r8 == 0 && g8 == 0 && b8 == 0;
        let white = r8 == 255 && g8 == 255 && b8 == 255;
        acc[3].add(y, black, white);
        cb_sum += cb;
        cr_sum += cr;
        sat_sum += saturation(cb, cr);
        hist.red[r8 as usize] += 1;
        hist.green[g8 as usize] += 1;
        hist.blue[b8 as usize] += 1;
        hist.luma[(y.clamp(0.0, 1.0) * 255.0).round() as usize] += 1;
    }
    let count = n as u64;
    let mean = |v: f64| if n == 0 { 0.0 } else { v / n as f64 };
    let (cb, cr) = (mean(cb_sum), mean(cr_sum));

    let mut waveform = wants(ScopeKind::Waveform).then(|| Grid::new(size.columns, size.rows));
    let mut parade = wants(ScopeKind::Parade).then(|| vec![Grid::new(size.columns, size.rows); 3]);
    let mut vector = wants(ScopeKind::Vectorscope).then(|| Grid::new(size.vector, size.vector));
    let mut peaks = Grid::new(PEAK_GRID, PEAK_GRID);
    let mut samples = 0u64;
    if n > 0 {
        let (sx, sy) = decimation(w, h);
        for yy in (0..h).step_by(sy as usize) {
            for xx in (0..w).step_by(sx as usize) {
                let i = ((yy as usize) * (w as usize) + xx as usize) * 4;
                let (r, g, b) = (px[i] as f64 / 255.0, px[i + 1] as f64 / 255.0, px[i + 2] as f64 / 255.0);
                let (y, cb, cr) = ycbcr(m, r, g, b);
                let col = |cols: u32| ((xx as u64 * cols as u64) / w as u64) as u32;
                if let Some(g) = waveform.as_mut() {
                    g.hit(col(g.columns), level_row(y, g.rows));
                }
                if let Some(p) = parade.as_mut() {
                    for (grid, v) in p.iter_mut().zip([r, g, b]) {
                        grid.hit(col(grid.columns), level_row(v, grid.rows));
                    }
                }
                if let Some(v) = vector.as_mut() {
                    v.hit(vector_cell(cb, v.columns), vector_cell(cr, v.rows));
                }
                peaks.hit(vector_cell(cb, PEAK_GRID), vector_cell(cr, PEAK_GRID));
                samples += 1;
            }
        }
    }

    let summary = ScopeSummary {
        width: w,
        height: h,
        matrix: matrix_name(m).to_string(),
        red: acc[0].stats(count),
        green: acc[1].stats(count),
        blue: acc[2].stats(count),
        luma: acc[3].stats(count),
        chroma: ChromaStats {
            cb: round4(cb),
            cr: round4(cr),
            hue_deg: round4(hue_deg(cb, cr)),
            saturation: round4(saturation(cb, cr)),
            mean_saturation: round4(mean(sat_sum)),
        },
        peaks: vector_peaks(&peaks, samples),
    };
    Scopes {
        summary,
        waveform,
        parade,
        vectorscope: vector,
        histogram: wants(ScopeKind::Histogram).then_some(hist),
    }
}

fn vector_peaks(grid: &Grid, samples: u64) -> Vec<VectorPeak> {
    if samples == 0 {
        return Vec::new();
    }
    let mut cells: Vec<(u32, usize)> = grid
        .counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0)
        .map(|(i, c)| (*c, i))
        .collect();
    cells.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    cells
        .into_iter()
        .map(|(c, i)| (c as f64 / samples as f64, i as u32))
        .filter(|(share, _)| *share >= MIN_PEAK_SHARE)
        .take(MAX_PEAKS)
        .map(|(share, i)| {
            let cb = vector_centre(i % grid.columns, grid.columns);
            let cr = vector_centre(i / grid.columns, grid.rows);
            VectorPeak {
                cb: round4(cb),
                cr: round4(cr),
                hue_deg: round4(hue_deg(cb, cr)),
                saturation: round4(saturation(cb, cr)),
                share: round4(share),
            }
        })
        .collect()
}

/// What [`timeline_scopes`] measures.
#[derive(Debug, Clone, Default)]
pub struct ScopeRequest {
    pub time_secs: f64,
    /// Render width; `None` is the delivery frame itself.
    pub max_width: Option<u32>,
    pub matrix: ScopeMatrix,
    pub options: ScopeOptions,
}

/// The scopes of the composited frame at `req.time_secs`: the export's still of the cut (the
/// timeline and assets a caller resolved under the project lock — the **originals**, as the file
/// is made of them) as its Y'CbCr samples, decoded to RGB with the delivery's matrix. Runs FFmpeg:
/// never under the project lock.
pub fn timeline_scopes(timeline: &Timeline, assets: &[Asset], req: &ScopeRequest) -> Result<Scopes> {
    if !req.time_secs.is_finite() {
        return Err(Error::Engine("the time to measure is not a number".to_string()));
    }
    let frame = crate::engine::delivery_frame(&timeline.for_render(), assets);
    let matrix = req.matrix.resolve(frame);
    let opts = crate::engine::ExportOptions {
        hwaccel: crate::engine::decode_hwaccel(),
        ..crate::engine::ExportOptions::default()
    };
    let max_width = req.max_width.unwrap_or(u32::MAX);
    let (w, h, planes) = crate::engine::timeline_frame_yuv(timeline, assets, &opts, req.time_secs.max(0.0), max_width)?;
    let rgba = yuv444_to_rgba(&planes, w, h, matrix);
    Ok(analyze(
        RgbaFrame {
            width: w,
            height: h,
            pixels: &rgba,
        },
        matrix,
        &req.options,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(x, y);
                px.extend([r, g, b, 255]);
            }
        }
        px
    }

    fn run(w: u32, h: u32, px: &[u8], m: YuvMatrix, grids: &[ScopeKind]) -> Scopes {
        analyze(
            RgbaFrame {
                width: w,
                height: h,
                pixels: px,
            },
            m,
            &ScopeOptions {
                grids: grids.to_vec(),
                size: GridSize::default(),
            },
        )
    }

    const ALL: [ScopeKind; 4] = [
        ScopeKind::Waveform,
        ScopeKind::Parade,
        ScopeKind::Vectorscope,
        ScopeKind::Histogram,
    ];

    #[test]
    fn a_grey_frame_is_one_level_on_every_scope() {
        let px = frame(64, 36, |_, _| [128, 128, 128]);
        let s = run(64, 36, &px, YuvMatrix::Bt709, &ALL);
        let v = 128.0 / 255.0;
        for c in [s.summary.red, s.summary.green, s.summary.blue, s.summary.luma] {
            assert_eq!((c.min, c.max, c.mean), (round4(v), round4(v), round4(v)));
            assert_eq!((c.clipped_low, c.clipped_high), (0, 0));
        }
        assert_eq!(s.summary.chroma.saturation, 0.0);
        assert_eq!(s.summary.chroma.mean_saturation, 0.0);
        let wf = s.waveform.unwrap();
        let row = level_row(v, wf.rows);
        let total: u32 = wf.counts.iter().sum();
        assert_eq!(total, 64 * 36);
        assert_eq!((0..wf.columns).map(|c| wf.at(c, row)).sum::<u32>(), total);
        let peaks = &s.summary.peaks;
        assert_eq!(peaks.len(), 1);
        assert_eq!(peaks[0].share, 1.0);
        assert!(peaks[0].saturation < 0.03, "{peaks:?}");
        let vs = s.vectorscope.unwrap();
        let mid = vector_cell(0.0, vs.columns);
        assert_eq!(vs.at(mid, vector_cell(0.0, vs.rows)), 64 * 36);
        let hist = s.histogram.unwrap();
        assert_eq!(hist.red[128], 64 * 36);
        assert_eq!(hist.luma[128], 64 * 36);
    }

    #[test]
    fn one_clipped_pixel_is_found_where_the_grids_skip_it() {
        let (w, h) = (1920, 1080);
        let px = frame(w, h, |x, y| if (x, y) == (1, 1) { [255, 255, 255] } else { [0, 0, 0] });
        let s = run(w, h, &px, YuvMatrix::Bt709, &[ScopeKind::Waveform]);
        assert_eq!(decimation(w, h), (4, 4));
        for c in [s.summary.red, s.summary.green, s.summary.blue, s.summary.luma] {
            assert_eq!(c.clipped_high, 1);
            assert_eq!(c.max, 1.0);
            assert_eq!(c.clipped_low, (w * h) as u64 - 1);
            assert!(c.clipped_high_pct > 0.0 && c.clipped_high_pct < 0.0001);
        }
        let wf = s.waveform.unwrap();
        assert_eq!(
            (0..wf.columns).map(|c| wf.at(c, wf.rows - 1)).sum::<u32>(),
            0,
            "the decimated frame does not hold the pixel"
        );
    }

    #[test]
    fn red_reads_on_each_matrix_weights() {
        let px = frame(8, 8, |_, _| [255, 0, 0]);
        let s709 = run(8, 8, &px, YuvMatrix::Bt709, &[]);
        let s601 = run(8, 8, &px, YuvMatrix::Bt601, &[]);
        let s2020 = run(8, 8, &px, YuvMatrix::Bt2020, &[]);
        assert_eq!(s709.summary.luma.mean, 0.2126);
        assert_eq!(s601.summary.luma.mean, 0.299);
        assert_eq!(s2020.summary.luma.mean, 0.2627);
        assert_eq!(s709.summary.matrix, "bt709");
        // Red sits a little past the +Cr axis: Cr is 0.5 and Cb negative.
        let c = s709.summary.chroma;
        assert_eq!(c.cr, 0.5);
        assert!(c.cb < 0.0);
        assert!((c.hue_deg - 102.9).abs() < 0.2, "{}", c.hue_deg);
        assert!(c.saturation > 1.0 && c.saturation < 1.05);
        assert_eq!(s709.summary.red.clipped_high, 64);
        assert_eq!(s709.summary.green.clipped_low, 64);
        assert_eq!((s709.summary.luma.clipped_low, s709.summary.luma.clipped_high), (0, 0));
        assert!(s709.summary.peaks[0].hue_deg > 95.0 && s709.summary.peaks[0].hue_deg < 110.0);
        assert_eq!(s709.waveform, None);
        assert_eq!(s709.histogram, None);
    }

    #[test]
    fn the_parade_keeps_each_channel_where_it_is_in_the_frame() {
        let (w, h) = (32, 8);
        let px = frame(w, h, |x, _| if x < w / 2 { [255, 0, 0] } else { [0, 0, 255] });
        let s = run(w, h, &px, YuvMatrix::Bt709, &[ScopeKind::Parade]);
        let p = s.parade.unwrap();
        let (red, blue) = (&p[0], &p[2]);
        assert_eq!(red.columns, w);
        let (left, right) = (0, red.columns - 1);
        assert!(red.at(left, red.rows - 1) > 0 && red.at(right, 0) > 0);
        assert_eq!(red.at(left, 0), 0);
        assert!(blue.at(right, blue.rows - 1) > 0 && blue.at(left, 0) > 0);
        assert_eq!(blue.at(right, 0), 0);
        let mean = s.summary.chroma;
        assert!(mean.mean_saturation > 0.9, "both halves are saturated: {mean:?}");
    }

    #[test]
    fn a_warm_cast_points_the_mean_chroma_at_orange() {
        let px = frame(16, 16, |x, y| {
            let v = ((x + y) * 8) as u8;
            [v.saturating_add(30), v.saturating_add(10), v]
        });
        let c = run(16, 16, &px, YuvMatrix::Bt709, &[]).summary.chroma;
        assert!(c.cb < 0.0 && c.cr > 0.0, "{c:?}");
        assert!(c.hue_deg > 90.0 && c.hue_deg < 180.0, "{c:?}");
        assert!(c.saturation > 0.02 && c.saturation < 0.2, "{c:?}");
    }

    #[test]
    fn histogram_counts_every_pixel_once() {
        let px = frame(37, 23, |x, y| [(x * 7) as u8, (y * 11) as u8, ((x + y) * 3) as u8]);
        let h = run(37, 23, &px, YuvMatrix::Bt601, &[ScopeKind::Histogram]).histogram.unwrap();
        for bins in [&h.red, &h.green, &h.blue, &h.luma] {
            assert_eq!(bins.len(), 256);
            assert_eq!(bins.iter().sum::<u32>(), 37 * 23);
        }
    }

    #[test]
    fn grids_are_clamped_and_an_empty_frame_reads_nothing() {
        let size = GridSize {
            columns: 0,
            rows: 100_000,
            vector: 3,
        }
        .clamped();
        assert_eq!((size.columns, size.rows, size.vector), (8, 256, 8));
        let s = run(4, 4, &[], YuvMatrix::Bt709, &ALL);
        assert_eq!(s.summary.luma.max, 0.0);
        assert!(s.summary.peaks.is_empty());
        assert_eq!(s.waveform.unwrap().counts.iter().sum::<u32>(), 0);
    }

    #[test]
    fn limited_range_samples_decode_to_the_rgb_they_were_made_from() {
        for m in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            let colors: [[u8; 3]; 6] = [
                [0, 0, 0],
                [255, 255, 255],
                [255, 0, 0],
                [0, 255, 0],
                [20, 80, 200],
                [200, 150, 90],
            ];
            let n = colors.len();
            let mut planes = vec![0u8; n * 3];
            for (i, c) in colors.iter().enumerate() {
                let (y, cb, cr) = ycbcr(m, c[0] as f64 / 255.0, c[1] as f64 / 255.0, c[2] as f64 / 255.0);
                planes[i] = (16.0 + 219.0 * y).round() as u8;
                planes[n + i] = (128.0 + 224.0 * cb).round() as u8;
                planes[2 * n + i] = (128.0 + 224.0 * cr).round() as u8;
            }
            let rgba = yuv444_to_rgba(&planes, n as u32, 1, m);
            for (i, c) in colors.iter().enumerate() {
                for k in 0..3 {
                    let d = (rgba[i * 4 + k] as i32 - c[k] as i32).abs();
                    assert!(d <= 2, "{m:?} {c:?} channel {k}: {}", rgba[i * 4 + k]);
                }
            }
        }
        // A super-white is clipped, as a player shows it.
        let rgba = yuv444_to_rgba(&[250, 128, 128], 1, 1, YuvMatrix::Bt709);
        assert_eq!(&rgba[..3], &[255, 255, 255]);
    }

    #[test]
    fn the_delivery_matrix_follows_the_frame() {
        assert_eq!(delivery_matrix(1920, 1080), YuvMatrix::Bt709);
        assert_eq!(delivery_matrix(1080, 1920), YuvMatrix::Bt709);
        assert_eq!(delivery_matrix(1280, 720), YuvMatrix::Bt709);
        assert_eq!(delivery_matrix(720, 576), YuvMatrix::Bt601);
        assert_eq!(delivery_matrix(640, 360), YuvMatrix::Bt601);
        assert_eq!(ScopeMatrix::Bt2020.resolve((640, 360)), YuvMatrix::Bt2020);
        assert_eq!(ScopeMatrix::Auto.resolve((3840, 2160)), YuvMatrix::Bt709);
    }

    /// The composited still of a real clip, through FFmpeg: a grey 1280x720 frame with one white
    /// pixel and a red block. The pixel survives the encode and the still (lossless, no resize),
    /// and only the full-resolution summary finds it.
    ///
    /// `cargo test -p kerf-core --no-default-features -- --ignored scopes_of_a_rendered_frame`
    #[test]
    #[ignore = "drives the real ffmpeg"]
    fn scopes_of_a_rendered_frame() {
        use crate::engine::test_support::{make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded};
        let dir = std::env::temp_dir().join(format!("kerf-scopes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pixel.mp4");
        let made = crate::engine::ffmpeg_command()
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(
                "color=c=0x808080:s=1280x720:r=30:d=1,\
                 drawbox=x=640:y=0:w=640:h=360:color=0xE02020:t=fill,\
                 drawbox=x=101:y=101:w=1:h=1:color=white:t=fill,format=yuv420p",
            )
            .args(["-c:v", "libx264", "-qp", "0", "-g", "1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
        let mut asset = test_asset(vec![video_stream(1280, 720, 30.0)]);
        asset.streams[0].pix_fmt = Some("yuv420p".into());
        asset.path = path.to_string_lossy().into_owned();
        asset.duration = 1.0;
        let tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 1.0, 0.0)])]);
        let req = ScopeRequest {
            time_secs: 0.5,
            options: ScopeOptions {
                grids: vec![ScopeKind::Waveform, ScopeKind::Vectorscope],
                size: GridSize::default(),
            },
            ..ScopeRequest::default()
        };
        let s = timeline_scopes(&tl, std::slice::from_ref(&asset), &req).expect("scopes");
        let _ = std::fs::remove_dir_all(&dir);
        let sum = &s.summary;
        assert_eq!((sum.width, sum.height, sum.matrix.as_str()), (1280, 720, "bt709"));
        assert_eq!(sum.luma.clipped_high, 1, "the one white pixel: {:?}", sum.luma);
        assert_eq!(sum.luma.max, 1.0);
        assert_eq!(sum.luma.clipped_low, 0);
        assert!((sum.luma.min - 128.0 / 255.0).abs() < 0.3, "{:?}", sum.luma);
        let wf = s.waveform.unwrap();
        assert_eq!((0..wf.columns).map(|c| wf.at(c, wf.rows - 1)).sum::<u32>(), 0);
        // A quarter of the frame is red: the densest spots are grey and red.
        let hues: Vec<f64> = sum.peaks.iter().filter(|p| p.saturation > 0.5).map(|p| p.hue_deg).collect();
        assert!(hues.iter().any(|h| (95.0..115.0).contains(h)), "{:?}", sum.peaks);
        assert!(sum.peaks[0].saturation < 0.05, "grey first: {:?}", sum.peaks);
        assert!(s.vectorscope.is_some());
    }

    #[test]
    fn the_summary_serializes_snake_case() {
        let px = frame(2, 2, |_, _| [10, 20, 30]);
        let s = run(2, 2, &px, YuvMatrix::Bt709, &[ScopeKind::Histogram]);
        let json = serde_json::to_value(&s).unwrap();
        assert!(json["summary"]["luma"]["clipped_high_pct"].is_number());
        assert!(json["summary"]["chroma"]["mean_saturation"].is_number());
        assert!(json.get("waveform").is_none());
        assert_eq!(serde_json::to_value(ScopeKind::Vectorscope).unwrap(), "vectorscope");
    }
}
