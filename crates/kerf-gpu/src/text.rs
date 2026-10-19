//! Text on the GPU: FFmpeg rasterises it, the compositor blends what it made.
//!
//! `drawtext` is layout (shaping, kerning, hinting, the quarter-pixel phase a glyph is rendered
//! at) and a blend, and only the blend is arithmetic a shader can copy exactly. So the layout is
//! never redone here: for each distinct overlay state one `ffmpeg` run draws the overlay's own
//! `drawtext` on a black canvas **at the size and position the still draws it**
//! ([`kerf_core::text_raster_graph`]) and its coverage comes back as 8-bit masks — one for the
//! glyphs and one for the box — which are cached
//! ([`TextAtlas`]) and blended by `text.wgsl` with `ff_blend_mask` / `ff_blend_rectangle`'s own
//! integer arithmetic. The text is then exactly where the still has it, on whatever FFmpeg build
//! this machine has (6.1 and 9.0 lay text out differently, and each is matched by itself).
//!
//! **What the blend is** (libavfilter `drawutils.c`, identical in 6.1.1 and 9.0.2):
//!
//! * a plane sample `d` blends to `((K - a) * d + a * s) >> 24` with `K = 0x1010101`, `s` the
//!   colour's component in that plane and `a` the blend's alpha;
//! * for a mask the alpha is `(t >> shift) * c` where `c = (0x10307 * alpha + 3) >> 8`, `t` the sum
//!   of the mask over the plane sample's block (1 sample for luma, 2x2 for 4:2:0 chroma, `shift`
//!   2 there, a block that sticks out of the mask counting only what is in it) — so a mask
//!   at coverage 255 over black is exactly its own value, which is how it is read from FFmpeg;
//! * for a rectangle (the box) `a` is `0x10203 * alpha + 2` per luma sample; a chroma sample the
//!   rectangle covers by one row or column of its two gets half of it per missing side;
//! * the colour is `ff_draw_color`: BT.601 / 709 / 2020 coefficients (the matrix the composite
//!   is read with), limited range, `+ 0.5` and truncation; the alpha `(a * s) / 255` with `s` the
//!   overlay's opacity `256 * alpha` truncated, or 255.
//!
//! Where it differs from FFmpeg, and by how much: `drawtext` blends **each glyph's bitmap in
//! turn**, a mask here is the picture of all of them, so a pixel two glyph bitmaps both touch is
//! one blend here and two there. For the glyphs themselves that is only rounding (kerned pairs of
//! Latin text, opaque or translucent, are bit-identical on 6.1.1 and 9.0.2; the outlines of a
//! missing glyph's box differ by a level at a sample or two), and it is why the **synthetic-bold
//! border** is not drawn: neighbouring borders overlap along whole letter gaps, and a chroma sample
//! that averages their sum instead of blending them in turn is up to 33 levels off. The plan
//! refuses such a frame (`Unsupported::TextBorder`; it takes a named font with no bold face, and
//! the shipped default has one).
//!
//! **A frame with no layer** is drawn in RGB by FFmpeg (the black base goes to `drawtext` and on
//! to the PNG without ever being YUV), so there the blend is on the RGB picture, three full
//! resolution channels and the colour as written.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, UNIX_EPOCH};

use kerf_core::{PlanText, TextPart, TextRasterGraph, YuvMatrix};

use crate::gpu::GpuError;
use crate::source::{run_child, RunError};

/// How long one rasterisation may take before `ffmpeg` is killed: one frame of a black canvas is a
/// few tens of milliseconds, and the caller may be holding the preview's render lock.
pub const RASTER_TIMEOUT: Duration = Duration::from_secs(5);

/// The most one rasterisation may read back (every part of the canvas, 8-bit): a 4K canvas with
/// all three parts is 25 MB.
pub const MAX_RASTER_BYTES: usize = 64 << 20;

/// The longest graph handed to `ffmpeg` as one argument. Windows allows 32 767 characters on the
/// whole command line; a caption is a few dozen.
const MAX_GRAPH_CHARS: usize = 16 << 10;

/// What the atlas holds at most: masks are small (a caption at 540p is a few KB), so the count is
/// what bounds a long scrub.
pub const ATLAS_MAX_ENTRIES: usize = 256;
pub const ATLAS_MAX_BYTES: usize = 48 << 20;

/// What the atlas of a drag's titles holds at most ([`TextAtlas::get_drafted`]): a pose or two of the
/// title being dragged, which the next pointer move replaces.
pub const DRAFT_ATLAS_MAX_ENTRIES: usize = 4;
pub const DRAFT_ATLAS_MAX_BYTES: usize = 16 << 20;

/// `0x1010101`: the blend's unit. `alpha * value` is in `0..=0xFFFF_FFFF` below it, and `>> 24`
/// rounds down to the plane's 8 bits.
pub const UNIT: u32 = 0x0101_0101;

// ---- colours -----------------------------------------------------------------

/// `av_parse_color`'s named colours (libavutil `parseutils.c`, the same 140 in 6.1.1 and 9.0.2).
const COLOR_NAMES: [(&str, [u8; 3]); 140] = [
    ("AliceBlue", [0xF0, 0xF8, 0xFF]),
    ("AntiqueWhite", [0xFA, 0xEB, 0xD7]),
    ("Aqua", [0x00, 0xFF, 0xFF]),
    ("Aquamarine", [0x7F, 0xFF, 0xD4]),
    ("Azure", [0xF0, 0xFF, 0xFF]),
    ("Beige", [0xF5, 0xF5, 0xDC]),
    ("Bisque", [0xFF, 0xE4, 0xC4]),
    ("Black", [0x00, 0x00, 0x00]),
    ("BlanchedAlmond", [0xFF, 0xEB, 0xCD]),
    ("Blue", [0x00, 0x00, 0xFF]),
    ("BlueViolet", [0x8A, 0x2B, 0xE2]),
    ("Brown", [0xA5, 0x2A, 0x2A]),
    ("BurlyWood", [0xDE, 0xB8, 0x87]),
    ("CadetBlue", [0x5F, 0x9E, 0xA0]),
    ("Chartreuse", [0x7F, 0xFF, 0x00]),
    ("Chocolate", [0xD2, 0x69, 0x1E]),
    ("Coral", [0xFF, 0x7F, 0x50]),
    ("CornflowerBlue", [0x64, 0x95, 0xED]),
    ("Cornsilk", [0xFF, 0xF8, 0xDC]),
    ("Crimson", [0xDC, 0x14, 0x3C]),
    ("Cyan", [0x00, 0xFF, 0xFF]),
    ("DarkBlue", [0x00, 0x00, 0x8B]),
    ("DarkCyan", [0x00, 0x8B, 0x8B]),
    ("DarkGoldenRod", [0xB8, 0x86, 0x0B]),
    ("DarkGray", [0xA9, 0xA9, 0xA9]),
    ("DarkGreen", [0x00, 0x64, 0x00]),
    ("DarkKhaki", [0xBD, 0xB7, 0x6B]),
    ("DarkMagenta", [0x8B, 0x00, 0x8B]),
    ("DarkOliveGreen", [0x55, 0x6B, 0x2F]),
    ("Darkorange", [0xFF, 0x8C, 0x00]),
    ("DarkOrchid", [0x99, 0x32, 0xCC]),
    ("DarkRed", [0x8B, 0x00, 0x00]),
    ("DarkSalmon", [0xE9, 0x96, 0x7A]),
    ("DarkSeaGreen", [0x8F, 0xBC, 0x8F]),
    ("DarkSlateBlue", [0x48, 0x3D, 0x8B]),
    ("DarkSlateGray", [0x2F, 0x4F, 0x4F]),
    ("DarkTurquoise", [0x00, 0xCE, 0xD1]),
    ("DarkViolet", [0x94, 0x00, 0xD3]),
    ("DeepPink", [0xFF, 0x14, 0x93]),
    ("DeepSkyBlue", [0x00, 0xBF, 0xFF]),
    ("DimGray", [0x69, 0x69, 0x69]),
    ("DodgerBlue", [0x1E, 0x90, 0xFF]),
    ("FireBrick", [0xB2, 0x22, 0x22]),
    ("FloralWhite", [0xFF, 0xFA, 0xF0]),
    ("ForestGreen", [0x22, 0x8B, 0x22]),
    ("Fuchsia", [0xFF, 0x00, 0xFF]),
    ("Gainsboro", [0xDC, 0xDC, 0xDC]),
    ("GhostWhite", [0xF8, 0xF8, 0xFF]),
    ("Gold", [0xFF, 0xD7, 0x00]),
    ("GoldenRod", [0xDA, 0xA5, 0x20]),
    ("Gray", [0x80, 0x80, 0x80]),
    ("Green", [0x00, 0x80, 0x00]),
    ("GreenYellow", [0xAD, 0xFF, 0x2F]),
    ("HoneyDew", [0xF0, 0xFF, 0xF0]),
    ("HotPink", [0xFF, 0x69, 0xB4]),
    ("IndianRed", [0xCD, 0x5C, 0x5C]),
    ("Indigo", [0x4B, 0x00, 0x82]),
    ("Ivory", [0xFF, 0xFF, 0xF0]),
    ("Khaki", [0xF0, 0xE6, 0x8C]),
    ("Lavender", [0xE6, 0xE6, 0xFA]),
    ("LavenderBlush", [0xFF, 0xF0, 0xF5]),
    ("LawnGreen", [0x7C, 0xFC, 0x00]),
    ("LemonChiffon", [0xFF, 0xFA, 0xCD]),
    ("LightBlue", [0xAD, 0xD8, 0xE6]),
    ("LightCoral", [0xF0, 0x80, 0x80]),
    ("LightCyan", [0xE0, 0xFF, 0xFF]),
    ("LightGoldenRodYellow", [0xFA, 0xFA, 0xD2]),
    ("LightGreen", [0x90, 0xEE, 0x90]),
    ("LightGrey", [0xD3, 0xD3, 0xD3]),
    ("LightPink", [0xFF, 0xB6, 0xC1]),
    ("LightSalmon", [0xFF, 0xA0, 0x7A]),
    ("LightSeaGreen", [0x20, 0xB2, 0xAA]),
    ("LightSkyBlue", [0x87, 0xCE, 0xFA]),
    ("LightSlateGray", [0x77, 0x88, 0x99]),
    ("LightSteelBlue", [0xB0, 0xC4, 0xDE]),
    ("LightYellow", [0xFF, 0xFF, 0xE0]),
    ("Lime", [0x00, 0xFF, 0x00]),
    ("LimeGreen", [0x32, 0xCD, 0x32]),
    ("Linen", [0xFA, 0xF0, 0xE6]),
    ("Magenta", [0xFF, 0x00, 0xFF]),
    ("Maroon", [0x80, 0x00, 0x00]),
    ("MediumAquaMarine", [0x66, 0xCD, 0xAA]),
    ("MediumBlue", [0x00, 0x00, 0xCD]),
    ("MediumOrchid", [0xBA, 0x55, 0xD3]),
    ("MediumPurple", [0x93, 0x70, 0xD8]),
    ("MediumSeaGreen", [0x3C, 0xB3, 0x71]),
    ("MediumSlateBlue", [0x7B, 0x68, 0xEE]),
    ("MediumSpringGreen", [0x00, 0xFA, 0x9A]),
    ("MediumTurquoise", [0x48, 0xD1, 0xCC]),
    ("MediumVioletRed", [0xC7, 0x15, 0x85]),
    ("MidnightBlue", [0x19, 0x19, 0x70]),
    ("MintCream", [0xF5, 0xFF, 0xFA]),
    ("MistyRose", [0xFF, 0xE4, 0xE1]),
    ("Moccasin", [0xFF, 0xE4, 0xB5]),
    ("NavajoWhite", [0xFF, 0xDE, 0xAD]),
    ("Navy", [0x00, 0x00, 0x80]),
    ("OldLace", [0xFD, 0xF5, 0xE6]),
    ("Olive", [0x80, 0x80, 0x00]),
    ("OliveDrab", [0x6B, 0x8E, 0x23]),
    ("Orange", [0xFF, 0xA5, 0x00]),
    ("OrangeRed", [0xFF, 0x45, 0x00]),
    ("Orchid", [0xDA, 0x70, 0xD6]),
    ("PaleGoldenRod", [0xEE, 0xE8, 0xAA]),
    ("PaleGreen", [0x98, 0xFB, 0x98]),
    ("PaleTurquoise", [0xAF, 0xEE, 0xEE]),
    ("PaleVioletRed", [0xD8, 0x70, 0x93]),
    ("PapayaWhip", [0xFF, 0xEF, 0xD5]),
    ("PeachPuff", [0xFF, 0xDA, 0xB9]),
    ("Peru", [0xCD, 0x85, 0x3F]),
    ("Pink", [0xFF, 0xC0, 0xCB]),
    ("Plum", [0xDD, 0xA0, 0xDD]),
    ("PowderBlue", [0xB0, 0xE0, 0xE6]),
    ("Purple", [0x80, 0x00, 0x80]),
    ("Red", [0xFF, 0x00, 0x00]),
    ("RosyBrown", [0xBC, 0x8F, 0x8F]),
    ("RoyalBlue", [0x41, 0x69, 0xE1]),
    ("SaddleBrown", [0x8B, 0x45, 0x13]),
    ("Salmon", [0xFA, 0x80, 0x72]),
    ("SandyBrown", [0xF4, 0xA4, 0x60]),
    ("SeaGreen", [0x2E, 0x8B, 0x57]),
    ("SeaShell", [0xFF, 0xF5, 0xEE]),
    ("Sienna", [0xA0, 0x52, 0x2D]),
    ("Silver", [0xC0, 0xC0, 0xC0]),
    ("SkyBlue", [0x87, 0xCE, 0xEB]),
    ("SlateBlue", [0x6A, 0x5A, 0xCD]),
    ("SlateGray", [0x70, 0x80, 0x90]),
    ("Snow", [0xFF, 0xFA, 0xFA]),
    ("SpringGreen", [0x00, 0xFF, 0x7F]),
    ("SteelBlue", [0x46, 0x82, 0xB4]),
    ("Tan", [0xD2, 0xB4, 0x8C]),
    ("Teal", [0x00, 0x80, 0x80]),
    ("Thistle", [0xD8, 0xBF, 0xD8]),
    ("Tomato", [0xFF, 0x63, 0x47]),
    ("Turquoise", [0x40, 0xE0, 0xD0]),
    ("Violet", [0xEE, 0x82, 0xEE]),
    ("Wheat", [0xF5, 0xDE, 0xB3]),
    ("White", [0xFF, 0xFF, 0xFF]),
    ("WhiteSmoke", [0xF5, 0xF5, 0xF5]),
    ("Yellow", [0xFF, 0xFF, 0x00]),
    ("YellowGreen", [0x9A, 0xCD, 0x32]),
];

/// FFmpeg's `av_parse_color` for the spellings Kerf lets through (`valid_color`): `#RRGGBB[AA]`,
/// `0xRRGGBB[AA]`, a name (any case), each with an optional `@alpha` (`0..=1`, truncated as
/// `255 * alpha`; or `0x..`). `None` is what FFmpeg refuses (and so, there, the whole render).
/// `random` is not a colour here: it differs from one run to the next.
pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let (hex_offset, body) = if let Some(rest) = s.strip_prefix('#') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix("0x") {
        (true, rest)
    } else {
        (false, s)
    };
    let (color, alpha) = match body.split_once('@') {
        Some((c, a)) => (c, Some(a)),
        None => (body, None),
    };
    let mut rgba = [0, 0, 0, 255];
    if hex_offset || (!color.is_empty() && color.bytes().all(|b| b.is_ascii_hexdigit())) {
        if !color.bytes().all(|b| b.is_ascii_hexdigit()) || !matches!(color.len(), 6 | 8) {
            return None;
        }
        let v = u32::from_str_radix(color, 16).ok()?;
        let rgb = if color.len() == 8 {
            rgba[3] = (v & 0xFF) as u8;
            v >> 8
        } else {
            v
        };
        rgba[..3].copy_from_slice(&[(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8]);
    } else {
        let (_, rgb) = COLOR_NAMES.iter().find(|(name, _)| name.eq_ignore_ascii_case(color))?;
        rgba[..3].copy_from_slice(rgb);
    }
    if let Some(a) = alpha {
        let value = if let Some(hex) = a.strip_prefix("0x") {
            f64::from(u32::from_str_radix(hex, 16).ok()?)
        } else {
            let norm: f64 = a.parse().ok().filter(|v: &f64| v.is_finite())?;
            if (0.0..=1.0).contains(&norm) {
                255.0 * norm
            } else {
                256.0
            }
        };
        if !(0.0..=255.0).contains(&value) {
            return None;
        }
        rgba[3] = value as u8;
    }
    Some(rgba)
}

/// The luma coefficients `ff_fill_rgb2yuv_table` starts from (`AVR(d)` is `(int)(d * 100000 + 0.5)`
/// over 100000).
fn luma(matrix: YuvMatrix) -> (f64, f64, f64) {
    let q = |d: f64| f64::from((d * 100_000.0 + 0.5) as i32) / 100_000.0;
    match matrix {
        YuvMatrix::Bt601 => (q(0.299), q(0.587), q(0.114)),
        YuvMatrix::Bt709 => (q(0.2126), q(0.7152), q(0.0722)),
        YuvMatrix::Bt2020 => (q(0.2627), q(0.6780), q(0.0593)),
    }
}

/// `ff_draw_color` for a limited-range 8-bit YUV format: the colour's `(Y, U, V)` in the matrix the
/// composite carries.
pub fn draw_color(rgb: [u8; 3], matrix: YuvMatrix) -> [u8; 3] {
    let (cr, cg, cb) = luma(matrix);
    let bscale = 0.5 / (cb - 1.0);
    let rscale = 0.5 / (cr - 1.0);
    let table = [[cr, cg, cb], [bscale * cr, bscale * cg, 0.5], [0.5, rscale * cg, rscale * cb]];
    let v = rgb.map(|c| f64::from(c) / 255.0);
    let mut out = [0u8; 3];
    for (i, o) in out.iter_mut().enumerate() {
        let mut x = v[0] * table[i][0] + v[1] * table[i][1] + v[2] * table[i][2];
        let chroma = i > 0;
        x *= (if chroma { 224.0 } else { 219.0 }) / 255.0;
        x += (if chroma { 128.0 } else { 16.0 }) / 255.0;
        *o = (x * 255.0 + 0.5) as u8;
    }
    out
}

/// `drawtext`'s opacity: `alpha` as the expression gave it, `256 * alpha` truncated below 1.
pub fn opacity_byte(alpha: f64) -> u32 {
    if alpha.is_nan() || alpha >= 1.0 {
        255
    } else if alpha <= 0.0 {
        0
    } else {
        (256.0 * alpha) as u32
    }
}

// ---- what is blended ---------------------------------------------------------

/// A rectangle of the canvas in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// One part's coverage, trimmed to its bounding box: `data` is `w` x `h` bytes, row after row, and
/// `(x, y)` is where its top left is on the canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Coverage {
    /// The coverage at canvas pixel `(x, y)`; 0 outside the box.
    pub fn at(&self, x: i64, y: i64) -> u32 {
        let (dx, dy) = (x - i64::from(self.x), y - i64::from(self.y));
        if dx < 0 || dy < 0 || dx >= i64::from(self.w) || dy >= i64::from(self.h) {
            return 0;
        }
        u32::from(self.data[(dy as usize) * self.w as usize + dx as usize])
    }

    fn rect(&self) -> Rect {
        Rect {
            x: self.x as i32,
            y: self.y as i32,
            w: self.w as i32,
            h: self.h as i32,
        }
    }
}

/// What `drawtext` drew for an overlay, as masks: see the module docs. Parts the overlay does not
/// have, and parts that came out empty (nothing on the canvas), are `None`.
pub struct Raster {
    pub glyphs: Option<Coverage>,
    /// The box: where `drawtext` filled one on the canvas.
    pub boxed: Option<Rect>,
    /// The masks as textures, made by the first frame that draws them.
    pub(crate) uploaded: OnceLock<UploadedMasks>,
}

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Raster")
            .field("glyphs", &self.glyphs.as_ref().map(Coverage::rect))
            .field("boxed", &self.boxed)
            .finish_non_exhaustive()
    }
}

/// A mask on the GPU.
pub(crate) struct UploadedMask {
    pub(crate) view: wgpu::TextureView,
    _texture: wgpu::Texture,
}

pub(crate) struct UploadedMasks {
    pub(crate) glyphs: Option<UploadedMask>,
}

impl UploadedMask {
    pub(crate) fn new(texture: wgpu::Texture) -> Self {
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            _texture: texture,
        }
    }
}

impl Raster {
    /// What it holds, for the atlas's budget.
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.glyphs.iter().map(|c| c.data.len() * 2).sum::<usize>()
    }

    /// The masks cut out of `parts` (each `w` x `h`, in the order `graph.parts` names).
    fn from_parts(graph: &TextRasterGraph, planes: &[u8]) -> Result<Self, GpuError> {
        let (w, h) = (graph.size.0 as usize, graph.size.1 as usize);
        let mut raster = Raster {
            glyphs: None,
            boxed: None,
            uploaded: OnceLock::new(),
        };
        for (part, plane) in graph.parts.iter().zip(planes.chunks_exact(w * h)) {
            let Some((x0, y0, x1, y1)) = bounds(plane, w, h) else {
                continue;
            };
            match part {
                TextPart::Glyphs => {
                    let (bw, bh) = (x1 - x0, y1 - y0);
                    let mut data = Vec::with_capacity(bw * bh);
                    for row in y0..y1 {
                        data.extend_from_slice(&plane[row * w + x0..row * w + x1]);
                    }
                    raster.glyphs = Some(Coverage {
                        x: x0 as u32,
                        y: y0 as u32,
                        w: bw as u32,
                        h: bh as u32,
                        data,
                    });
                }
                TextPart::Box => {
                    // A box is a rectangle of full coverage; anything else is not what this reads.
                    let solid = (y0..y1).all(|row| plane[row * w + x0..row * w + x1].iter().all(|v| *v == 255));
                    if !solid {
                        return Err(GpuError::Unsupported("the text's box did not come out as a rectangle".into()));
                    }
                    raster.boxed = Some(Rect {
                        x: x0 as i32,
                        y: y0 as i32,
                        w: (x1 - x0) as i32,
                        h: (y1 - y0) as i32,
                    });
                }
            }
        }
        Ok(raster)
    }
}

/// The bounding box `(x0, y0, x1, y1)` (exclusive end) of the non-zero samples of a `w` x `h` plane.
fn bounds(plane: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut b: Option<(usize, usize, usize, usize)> = None;
    for y in 0..h {
        let row = &plane[y * w..(y + 1) * w];
        let (Some(first), Some(last)) = (row.iter().position(|v| *v != 0), row.iter().rposition(|v| *v != 0)) else {
            continue;
        };
        b = Some(match b {
            None => (first, y, last + 1, y + 1),
            Some((x0, y0, x1, _)) => (x0.min(first), y0, x1.max(last + 1), y + 1),
        });
    }
    b
}

/// What a blend draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The overlay's glyph mask.
    Glyphs,
    /// The box.
    Box(Rect),
}

/// One blend of an overlay: in order, the box and the glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blend {
    pub source: Source,
    /// The colour's three components in the picture's own format: `(Y, U, V)` of the composite's
    /// matrix, or `(R, G, B)` when the picture is RGB.
    pub colour: [u8; 3],
    /// The blend's alpha constant: what `drawtext` hands `ff_blend_mask` (`0x10307 * a + 3 >> 8`)
    /// or `ff_blend_rectangle` (`0x10203 * a + 2`).
    pub alpha: u32,
}

/// The blends that draw `text` from its `raster`, in `drawtext`'s order. `rgb` is a picture that is
/// RGB rather than composite YUV (a frame with no layer); `matrix` is the composite's.
pub fn blends(text: &PlanText, raster: &Raster, matrix: YuvMatrix, rgb: bool) -> Result<Vec<Blend>, GpuError> {
    let colour = |rgba: [u8; 4]| {
        if rgb {
            [rgba[0], rgba[1], rgba[2]]
        } else {
            draw_color([rgba[0], rgba[1], rgba[2]], matrix)
        }
    };
    let refused = |what: &str, s: &str| GpuError::Unsupported(format!("{what} colour {s:?} is not one FFmpeg reads"));
    let opacity = opacity_byte(text.alpha);
    let mut out = Vec::with_capacity(3);
    if let (Some(rect), Some(bg)) = (raster.boxed, text.bg.as_deref()) {
        let rgba = parse_color(bg).ok_or_else(|| refused("the box", bg))?;
        let a = u32::from(rgba[3]) * opacity / 255;
        if a > 0 {
            out.push(Blend {
                source: Source::Box(rect),
                colour: colour(rgba),
                alpha: 0x10203 * a + 2,
            });
        }
    }
    let rgba = parse_color(&text.color).ok_or_else(|| refused("the text", &text.color))?;
    let a = u32::from(rgba[3]) * opacity / 255;
    if a > 0 {
        let alpha = (0x10307 * a + 3) >> 8;
        if raster.glyphs.is_some() {
            out.push(Blend {
                source: Source::Glyphs,
                colour: colour(rgba),
                alpha,
            });
        }
    }
    Ok(out)
}

// ---- the scalar reference ------------------------------------------------------

/// A picture the reference blends into: three planes of `w` x `h` luma samples, chroma at half
/// size (`subsampled`) or at full size (an RGB picture).
pub struct Planes<'a> {
    pub w: usize,
    pub h: usize,
    pub subsampled: bool,
    pub planes: [&'a mut [u8]; 3],
}

fn mix(dst: u8, alpha: u32, src: u8) -> u8 {
    ((u64::from(UNIT - alpha) * u64::from(dst) + u64::from(alpha) * u64::from(src)) >> 24) as u8
}

/// `ff_blend_mask`, on one [`Blend`] of a mask: the scalar reference of `text.wgsl` and the check
/// of both against `drawtext`.
pub fn blend_coverage(pic: &mut Planes, mask: &Coverage, colour: [u8; 3], alpha: u32) {
    let (w, h) = (pic.w, pic.h);
    for y in 0..h {
        for x in 0..w {
            let a = mask.at(x as i64, y as i64) * alpha;
            let i = y * w + x;
            if pic.subsampled {
                pic.planes[0][i] = mix(pic.planes[0][i], a, colour[0]);
            } else {
                for (plane, c) in pic.planes.iter_mut().zip(colour) {
                    plane[i] = mix(plane[i], a, c);
                }
            }
        }
    }
    if !pic.subsampled {
        return;
    }
    let cw = w / 2;
    for cy in 0..h / 2 {
        for cx in 0..cw {
            let (x, y) = ((cx * 2) as i64, (cy * 2) as i64);
            let t = mask.at(x, y) + mask.at(x + 1, y) + mask.at(x, y + 1) + mask.at(x + 1, y + 1);
            let a = (t >> 2) * alpha;
            let i = cy * cw + cx;
            for (plane, c) in pic.planes.iter_mut().zip(colour).skip(1) {
                plane[i] = mix(plane[i], a, c);
            }
        }
    }
}

/// `ff_blend_rectangle` for the part of `rect` on the picture.
pub fn blend_rect(pic: &mut Planes, rect: Rect, colour: [u8; 3], alpha: u32) {
    let (w, h) = (pic.w as i32, pic.h as i32);
    let (x0, y0) = (rect.x.max(0), rect.y.max(0));
    let (x1, y1) = ((rect.x + rect.w).min(w), (rect.y + rect.h).min(h));
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) as usize;
            if pic.subsampled {
                pic.planes[0][i] = mix(pic.planes[0][i], alpha, colour[0]);
            } else {
                for (plane, c) in pic.planes.iter_mut().zip(colour) {
                    plane[i] = mix(plane[i], alpha, c);
                }
            }
        }
    }
    if !pic.subsampled {
        return;
    }
    let cw = pic.w / 2;
    for cy in 0..pic.h / 2 {
        for cx in 0..cw {
            let (bx, by) = ((cx * 2) as i32, (cy * 2) as i32);
            let nx = ((x1).min(bx + 2) - x0.max(bx)).max(0);
            let ny = ((y1).min(by + 2) - y0.max(by)).max(0);
            if nx == 0 || ny == 0 {
                continue;
            }
            let row = if ny == 2 { alpha } else { alpha >> 1 };
            let a = if nx == 2 { row } else { row >> 1 };
            let i = cy * cw + cx;
            for (plane, c) in pic.planes.iter_mut().zip(colour).skip(1) {
                plane[i] = mix(plane[i], a, c);
            }
        }
    }
}

/// Every blend of `blends` into `pic`: the whole of what the compositor draws for an overlay.
pub fn blend_all(pic: &mut Planes, raster: &Raster, blends: &[Blend]) {
    for b in blends {
        match b.source {
            Source::Box(rect) => blend_rect(pic, rect, b.colour, b.alpha),
            Source::Glyphs => {
                if let Some(mask) = &raster.glyphs {
                    blend_coverage(pic, mask, b.colour, b.alpha);
                }
            }
        }
    }
}

/// The part of the canvas a set of blends can change, widened to whole chroma blocks and clipped
/// to a `w` x `h` canvas: `(x0, y0, x1, y1)`, `None` when they touch nothing.
pub fn touched(raster: &Raster, blends: &[Blend], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let mut region: Option<(i64, i64, i64, i64)> = None;
    for b in blends {
        let r = match b.source {
            Source::Box(rect) => rect,
            Source::Glyphs => raster.glyphs.as_ref()?.rect(),
        };
        let (x0, y0) = (i64::from(r.x), i64::from(r.y));
        let (x1, y1) = (x0 + i64::from(r.w), y0 + i64::from(r.h));
        region = Some(match region {
            None => (x0, y0, x1, y1),
            Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
        });
    }
    let (x0, y0, x1, y1) = region?;
    let (x0, y0) = (x0.max(0) & !1, y0.max(0) & !1);
    let (x1, y1) = ((x1.min(i64::from(w)) + 1) & !1, (y1.min(i64::from(h)) + 1) & !1);
    (x1 > x0 && y1 > y0).then_some((x0 as u32, y0 as u32, x1.min(i64::from(w)) as u32, y1.min(i64::from(h)) as u32))
}

// ---- rasterising --------------------------------------------------------------

/// Draw `graph` with FFmpeg and read the masks back. Blocking: a process of tens of milliseconds,
/// killed after [`RASTER_TIMEOUT`]; callers are on a blocking thread and hold no project lock (they may
/// hold their own render lock, which waits this long on a first-seen state).
pub fn rasterize(graph: &TextRasterGraph) -> Result<Raster, GpuError> {
    let (w, h) = graph.size;
    let plane = (w as usize).checked_mul(h as usize);
    let want = plane.and_then(|p| p.checked_mul(graph.parts.len()));
    let (Some(plane), Some(want)) = (plane, want) else {
        return Err(GpuError::Unsupported(format!("a {w}x{h} canvas of text")));
    };
    if plane == 0 || want > MAX_RASTER_BYTES {
        return Err(GpuError::Unsupported(format!(
            "a {w}x{h} canvas of text is too big to rasterise"
        )));
    }
    if graph.filter_complex.len() > MAX_GRAPH_CHARS {
        return Err(GpuError::Unsupported("a text too long to rasterise".into()));
    }
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-filter_complex",
        graph.filter_complex.as_str(),
        "-map",
        "[o]",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "gray",
        "pipe:1",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    kerf_core::limit_frame_args(&mut args, 1);
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(&args);
    let out = run_child(cmd, want, RASTER_TIMEOUT).map_err(|e| match e {
        RunError::Overflow => GpuError::Decode("the text rasterisation is larger than its canvas".into()),
        // FFmpeg ran and said no (a font it cannot open, a colour it cannot read): that overlay is
        // its own to draw, and so is its error.
        RunError::Failed(why) => GpuError::Unsupported(format!("FFmpeg could not rasterise the text: {why}")),
    })?;
    if out.len() != want {
        return Err(GpuError::Decode(format!(
            "{} bytes of text coverage for {} planes of {w}x{h}",
            out.len(),
            graph.parts.len()
        )));
    }
    Raster::from_parts(graph, &out)
}

// ---- the atlas -----------------------------------------------------------------

/// What makes two rasterisations the same picture: the graph (text, size, font file, position,
/// canvas), the font file's identity (a replaced font is a new one) and the `ffmpeg` that drew it
/// (two builds lay text out differently).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    ffmpeg: String,
    graph: String,
    font: Option<(u64, u128)>,
}

impl Key {
    pub fn new(graph: &TextRasterGraph, font_file: Option<&std::path::Path>) -> Self {
        let font = font_file.and_then(|p| std::fs::metadata(p).ok()).map(|m| {
            let modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (m.len(), modified)
        });
        Key {
            ffmpeg: kerf_core::ffmpeg_path(),
            graph: graph.filter_complex.clone(),
            font,
        }
    }
}

struct Slot {
    raster: Mutex<Option<Arc<Raster>>>,
    used: AtomicU64,
}

#[derive(Default)]
struct AtlasInner {
    slots: HashMap<Key, Arc<Slot>>,
    bytes: usize,
    clock: u64,
}

/// The rasterised overlays of a compositor, bounded by [`ATLAS_MAX_ENTRIES`] and
/// [`ATLAS_MAX_BYTES`], least recently used out first. A state is rasterised once even when
/// several frames ask for it at the same moment (the others wait for the first); a failure is
/// not remembered.
pub struct TextAtlas {
    inner: Mutex<AtlasInner>,
    max_entries: usize,
    max_bytes: usize,
    runs: AtomicU64,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Default for TextAtlas {
    fn default() -> Self {
        Self::with_limits(ATLAS_MAX_ENTRIES, ATLAS_MAX_BYTES)
    }
}

impl TextAtlas {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(AtlasInner::default()),
            max_entries,
            max_bytes,
            runs: AtomicU64::new(0),
        }
    }

    /// How many rasterisations were run (not served from the cache).
    pub fn runs(&self) -> u64 {
        self.runs.load(Ordering::Relaxed)
    }

    /// How many states are held and how many bytes they are charged.
    #[cfg(test)]
    fn held(&self) -> (usize, usize) {
        let inner = locked(&self.inner);
        (
            inner.slots.values().filter(|s| locked(&s.raster).is_some()).count(),
            inner.bytes,
        )
    }

    /// The raster for `key`, made by `make` if it is not held. `make` runs with no lock but its own
    /// key's.
    pub fn get(&self, key: Key, make: impl FnOnce() -> Result<Raster, GpuError>) -> Result<Arc<Raster>, GpuError> {
        let slot = {
            let mut inner = locked(&self.inner);
            inner.clock += 1;
            let clock = inner.clock;
            let slot = inner
                .slots
                .entry(key.clone())
                .or_insert_with(|| {
                    Arc::new(Slot {
                        raster: Mutex::new(None),
                        used: AtomicU64::new(0),
                    })
                })
                .clone();
            slot.used.store(clock, Ordering::Relaxed);
            slot
        };
        let mut held = locked(&slot.raster);
        if let Some(raster) = held.as_ref() {
            return Ok(Arc::clone(raster));
        }
        self.runs.fetch_add(1, Ordering::Relaxed);
        let made = match make() {
            Ok(raster) => Arc::new(raster),
            Err(e) => {
                drop(held);
                let mut inner = locked(&self.inner);
                // Nobody else is waiting on a slot that never filled: forget it.
                if inner.slots.get(&key).is_some_and(|s| Arc::ptr_eq(s, &slot)) && Arc::strong_count(&slot) <= 2 {
                    inner.slots.remove(&key);
                }
                return Err(e);
            }
        };
        *held = Some(Arc::clone(&made));
        drop(held);
        let mut inner = locked(&self.inner);
        inner.bytes += made.bytes();
        self.evict(&mut inner, &key);
        Ok(made)
    }

    /// The raster for `key` if this atlas holds it, counted as a use; nothing is made or kept.
    pub fn peek(&self, key: &Key) -> Option<Arc<Raster>> {
        let slot = {
            let mut inner = locked(&self.inner);
            inner.clock += 1;
            let clock = inner.clock;
            let slot = Arc::clone(inner.slots.get(key)?);
            slot.used.store(clock, Ordering::Relaxed);
            slot
        };
        let held = locked(&slot.raster).clone();
        held
    }

    /// [`TextAtlas::get`] for a drag's frame (a title moved or resized: a pose per pointer move, drawn
    /// once and never again): what this atlas holds is served from it, and anything it does not is
    /// made into `drafts`, a small atlas of its own, so the poses of a drag never push the cut's own
    /// titles out of this one.
    pub fn get_drafted(
        &self,
        drafts: &TextAtlas,
        key: Key,
        make: impl FnOnce() -> Result<Raster, GpuError>,
    ) -> Result<Arc<Raster>, GpuError> {
        match self.peek(&key) {
            Some(raster) => Ok(raster),
            None => drafts.get(key, make),
        }
    }

    /// Drop the least recently used filled slots until the limits hold (never `keep`, never one
    /// somebody is still filling).
    fn evict(&self, inner: &mut AtlasInner, keep: &Key) {
        loop {
            let filled = inner.slots.len();
            if filled <= self.max_entries && inner.bytes <= self.max_bytes {
                return;
            }
            let victim = inner
                .slots
                .iter()
                .filter(|(k, s)| *k != keep && Arc::strong_count(s) == 1 && locked(&s.raster).is_some())
                .min_by_key(|(_, s)| s.used.load(Ordering::Relaxed))
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else {
                return;
            };
            if let Some(slot) = inner.slots.remove(&victim) {
                if let Some(raster) = locked(&slot.raster).as_ref() {
                    inner.bytes = inner.bytes.saturating_sub(raster.bytes());
                }
            }
        }
    }

    /// Forget everything (a device that was replaced holds none of the textures).
    pub fn clear(&self) {
        let mut inner = locked(&self.inner);
        inner.slots.clear();
        inner.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage(x: u32, y: u32, w: u32, h: u32, v: u8) -> Coverage {
        Coverage {
            x,
            y,
            w,
            h,
            data: vec![v; (w * h) as usize],
        }
    }

    fn raster(glyphs: Option<Coverage>, boxed: Option<Rect>) -> Raster {
        Raster {
            glyphs,
            boxed,
            uploaded: OnceLock::new(),
        }
    }

    fn text(color: &str, bg: Option<&str>, alpha: f64) -> PlanText {
        PlanText {
            id: uuid::Uuid::nil(),
            text: "x".into(),
            size: 0.1,
            pos: (0.5, 0.5),
            alpha,
            color: color.into(),
            bg: bg.map(String::from),
            font_file: None,
            synthetic_bold: false,
        }
    }

    #[test]
    fn colours_parse_as_av_parse_color_does() {
        assert_eq!(parse_color("white"), Some([255, 255, 255, 255]));
        assert_eq!(parse_color("WhiteSmoke"), Some([0xF5, 0xF5, 0xF5, 255]));
        assert_eq!(parse_color("yellow@0.9"), Some([255, 255, 0, 229]), "255 * 0.9, truncated");
        assert_eq!(parse_color("black@.5"), Some([0, 0, 0, 127]));
        assert_eq!(parse_color("black@1"), Some([0, 0, 0, 255]));
        assert_eq!(parse_color("#ffcc00"), Some([255, 204, 0, 255]));
        assert_eq!(parse_color("#FFCC0080"), Some([255, 204, 0, 128]));
        assert_eq!(parse_color("0x11223344"), Some([0x11, 0x22, 0x33, 0x44]));
        // A bare run of hex digits is a colour too (and so is not a name).
        assert_eq!(parse_color("abcdef"), Some([0xab, 0xcd, 0xef, 255]));
        assert_eq!(parse_color("white@0x80"), Some([255, 255, 255, 128]));
        for bad in [
            "",
            "#fff",
            "0xZZZZZZ",
            "nosuchcolour",
            "white@2",
            "white@-1",
            "white@x",
            "#ff00",
            "white@",
            "@0.5",
        ] {
            assert_eq!(parse_color(bad), None, "{bad:?}");
        }
        assert_eq!(COLOR_NAMES.len(), 140);
        assert!(COLOR_NAMES
            .windows(2)
            .all(|w| w[0].0.to_ascii_lowercase() < w[1].0.to_ascii_lowercase()));
    }

    #[test]
    fn a_colour_converts_as_ff_draw_color_does() {
        // Limited range: black and white are 16 and 235 with neutral chroma, in every matrix.
        for m in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
            assert_eq!(draw_color([0, 0, 0], m), [16, 128, 128], "{m:?}");
            assert_eq!(draw_color([255, 255, 255], m), [235, 128, 128], "{m:?}");
            // The extremes of the chroma planes are the primaries' of every matrix.
            assert_eq!(draw_color([0, 0, 255], m)[1], 240, "{m:?}");
            assert_eq!(draw_color([255, 0, 0], m)[2], 240, "{m:?}");
        }
        // Yellow as BT.601 sees it, which `drawtext` was measured to write (tests/text.rs).
        assert_eq!(draw_color([255, 255, 0], YuvMatrix::Bt601), [210, 16, 146]);
        // The same colour is a different YUV in another matrix.
        assert_ne!(
            draw_color([255, 204, 0], YuvMatrix::Bt709),
            draw_color([255, 204, 0], YuvMatrix::Bt601)
        );
    }

    #[test]
    fn opacity_is_drawtexts_256_times_truncated() {
        assert_eq!(opacity_byte(1.0), 255);
        assert_eq!(opacity_byte(7.0), 255);
        assert_eq!(opacity_byte(0.0), 0);
        assert_eq!(opacity_byte(-1.0), 0);
        assert_eq!(opacity_byte(0.5), 128);
        assert_eq!(opacity_byte(0.999), 255);
        assert_eq!(opacity_byte(0.001), 0);
    }

    #[test]
    fn the_blends_of_an_overlay_are_the_box_then_the_glyphs_with_their_alpha_constants() {
        let r = raster(
            Some(coverage(10, 10, 4, 4, 255)),
            Some(Rect {
                x: 2,
                y: 2,
                w: 20,
                h: 12,
            }),
        );
        let t = text("white", Some("black@0.5"), 1.0);
        let b = blends(&t, &r, YuvMatrix::Bt601, false).unwrap();
        assert_eq!(b.len(), 2);
        assert!(matches!(b[0].source, Source::Box(_)) && b[1].source == Source::Glyphs);
        assert_eq!(b[0].colour, [16, 128, 128]);
        assert_eq!(b[1].colour, [235, 128, 128]);
        // Opaque glyphs: (0x10307 * 255 + 3) >> 8 = 66051, so a full-coverage mask at 255 is `UNIT - 4`.
        assert_eq!(b[1].alpha, 66051);
        assert_eq!(b[1].alpha * 255, UNIT - 4);
        // A box at alpha 127: 0x10203 * 127 + 2.
        assert_eq!(b[0].alpha, 0x10203 * 127 + 2);
        // The overlay's opacity scales both colours' alpha: 255 * 128 / 255 and 127 * 128 / 255.
        let half = blends(&text("white", Some("black@0.5"), 0.5), &r, YuvMatrix::Bt601, false).unwrap();
        assert_eq!(half[1].alpha, (0x10307 * 128 + 3) >> 8);
        assert_eq!(half[0].alpha, 0x10203 * (127 * 128 / 255) + 2);
        // Fully transparent: nothing is blended, as `ff_blend_mask` returns early at alpha 0.
        assert!(blends(&text("white", Some("black"), 0.0), &r, YuvMatrix::Bt601, false)
            .unwrap()
            .is_empty());
        assert!(blends(&text("white@0", None, 1.0), &r, YuvMatrix::Bt601, false)
            .unwrap()
            .is_empty());
        // An RGB picture takes the colour as written.
        let rgb = blends(&text("#336699", None, 1.0), &r, YuvMatrix::Bt709, true).unwrap();
        assert_eq!(rgb[0].colour, [0x33, 0x66, 0x99]);
        // A colour FFmpeg would refuse is refused, not drawn as something else.
        assert!(matches!(
            blends(&text("nosuchcolour", None, 1.0), &r, YuvMatrix::Bt601, false),
            Err(GpuError::Unsupported(_))
        ));
        assert!(matches!(
            blends(&text("white", Some("#12"), 1.0), &r, YuvMatrix::Bt601, false),
            Err(GpuError::Unsupported(_))
        ));
    }

    fn flat(w: usize, h: usize, y: u8, u: u8, v: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (vec![y; w * h], vec![u; w * h / 4], vec![v; w * h / 4])
    }

    #[test]
    fn a_full_mask_over_black_writes_its_own_coverage_which_is_how_it_is_read() {
        // 255 over 0 at coverage m is m: the identity the rasteriser's white-on-black render relies on.
        let alpha = 66051;
        for m in 0..=255u32 {
            let a = m * alpha;
            assert_eq!(mix(0, a, 255), m as u8, "coverage {m}");
        }
    }

    #[test]
    fn the_reference_blends_chroma_over_the_block_and_a_rectangle_half_over_an_odd_edge() {
        let (w, h) = (8, 8);
        // One pixel of full coverage at an odd position: its chroma block averages it with three zeros.
        let (mut y, mut u, mut v) = flat(w, h, 100, 128, 128);
        let mut pic = Planes {
            w,
            h,
            subsampled: true,
            planes: [&mut y, &mut u, &mut v],
        };
        blend_coverage(&mut pic, &coverage(3, 3, 1, 1, 255), [200, 60, 60], 66051);
        assert_eq!(y[3 * 8 + 3], 200, "luma at full coverage and alpha is the colour");
        assert_eq!(y.iter().filter(|p| **p != 100).count(), 1);
        // The block's mask sum is 255, >> 2 = 63, so the chroma sample moves 63/255 of the way.
        assert_eq!(
            u[8 / 2 + 1],
            ((u64::from(UNIT - 63u32 * 66051) * 128 + u64::from(63u32 * 66051) * 60) >> 24) as u8
        );
        assert_eq!(u.iter().filter(|p| **p != 128).count(), 1);

        // A rectangle starting at an odd column and row covers its edge chroma blocks by half.
        let (mut y, mut u, mut v) = flat(w, h, 100, 128, 128);
        let mut pic = Planes {
            w,
            h,
            subsampled: true,
            planes: [&mut y, &mut u, &mut v],
        };
        let alpha = 0x10203 * 255 + 2;
        blend_rect(&mut pic, Rect { x: 1, y: 1, w: 4, h: 4 }, [235, 90, 240], alpha);
        assert_eq!(y.iter().filter(|p| **p != 100).count(), 16);
        let at = |cx: usize, cy: usize| u[cy * 4 + cx];
        // The sample at (0, 0) is covered by one pixel (a quarter of its block): alpha halved twice.
        let quarter = ((u64::from(UNIT - (alpha >> 2)) * 128 + u64::from(alpha >> 2) * 90) >> 24) as u8;
        assert_eq!(at(0, 0), quarter);
        // (1, 1) is covered by all four of its pixels (columns 2..3, rows 2..3).
        assert_eq!(
            at(1, 1),
            ((u64::from(UNIT - alpha) * 128 + u64::from(alpha) * 90) >> 24) as u8
        );
        // (2, 2) is covered by one pixel (column 4, row 4).
        assert_eq!(at(2, 2), quarter);
        assert_eq!(at(3, 3), 128, "untouched");
        // Off the picture: nothing.
        let (mut y, mut u, mut v) = flat(w, h, 100, 128, 128);
        let mut pic = Planes {
            w,
            h,
            subsampled: true,
            planes: [&mut y, &mut u, &mut v],
        };
        blend_rect(&mut pic, Rect { x: 20, y: 1, w: 4, h: 4 }, [235, 90, 240], alpha);
        assert!(y.iter().all(|p| *p == 100));
    }

    #[test]
    fn an_rgb_picture_is_blended_per_pixel_in_all_three_channels() {
        let (w, h) = (4, 4);
        let (mut r, mut g, mut b) = (vec![10u8; 16], vec![20u8; 16], vec![30u8; 16]);
        let mut pic = Planes {
            w,
            h,
            subsampled: false,
            planes: [&mut r, &mut g, &mut b],
        };
        blend_coverage(&mut pic, &coverage(1, 1, 2, 2, 255), [255, 255, 0], 66051);
        assert_eq!((r[5], g[5], b[5]), (255, 255, 0), "full coverage is the colour");
        assert_eq!((r[0], g[0], b[0]), (10, 20, 30));
        assert_eq!(r.iter().filter(|p| **p != 10).count(), 4);
    }

    #[test]
    fn what_a_blend_touches_is_whole_chroma_blocks_on_the_canvas() {
        let r = raster(Some(coverage(5, 5, 3, 3, 255)), Some(Rect { x: -4, y: 2, w: 8, h: 5 }));
        let t = text("white", Some("black@0.5"), 1.0);
        let b = blends(&t, &r, YuvMatrix::Bt601, false).unwrap();
        // The box starts off the canvas, the glyphs at an odd pixel: the union, widened to even, clipped.
        assert_eq!(touched(&r, &b, 100, 50), Some((0, 2, 8, 8)));
        assert_eq!(touched(&r, &b, 6, 6), Some((0, 2, 6, 6)));
        assert_eq!(touched(&r, &[], 100, 50), None);
        let off = raster(
            None,
            Some(Rect {
                x: 200,
                y: 2,
                w: 8,
                h: 5,
            }),
        );
        let b = [Blend {
            source: Source::Box(Rect {
                x: 200,
                y: 2,
                w: 8,
                h: 5,
            }),
            colour: [0; 3],
            alpha: 1,
        }];
        assert_eq!(touched(&off, &b, 100, 50), None, "wholly off the canvas");
    }

    fn graph(parts: Vec<TextPart>, w: u32, h: u32) -> TextRasterGraph {
        TextRasterGraph {
            filter_complex: String::new(),
            parts,
            size: (w, h),
        }
    }

    #[test]
    fn masks_are_cut_to_their_bounding_boxes_and_a_box_must_be_a_rectangle() {
        let (w, h) = (8usize, 6usize);
        let mut glyphs = vec![0u8; w * h];
        glyphs[2 * w + 3] = 200;
        glyphs[4 * w + 6] = 9;
        let mut boxed = vec![0u8; w * h];
        for y in 1..5 {
            for x in 2..7 {
                boxed[y * w + x] = 255;
            }
        }
        let planes: Vec<u8> = glyphs.iter().chain(&boxed).copied().collect();
        let r = Raster::from_parts(&graph(vec![TextPart::Glyphs, TextPart::Box], 8, 6), &planes).unwrap();
        let g = r.glyphs.as_ref().unwrap();
        assert_eq!((g.x, g.y, g.w, g.h), (3, 2, 4, 3));
        assert_eq!(g.at(3, 2), 200);
        assert_eq!(g.at(6, 4), 9);
        assert_eq!(g.at(0, 0), 0, "outside the box is zero");
        assert_eq!(r.boxed, Some(Rect { x: 2, y: 1, w: 5, h: 4 }));
        // A part with nothing in it is not there.
        let empty = Raster::from_parts(&graph(vec![TextPart::Glyphs], 8, 6), &vec![0u8; w * h]).unwrap();
        assert!(empty.glyphs.is_none() && empty.boxed.is_none());
        // A box with a hole in it (or an antialiased edge) is not what this reads.
        let mut holed = boxed.clone();
        holed[2 * w + 3] = 100;
        let err = Raster::from_parts(&graph(vec![TextPart::Box], 8, 6), &holed).unwrap_err();
        assert!(matches!(err, GpuError::Unsupported(_)), "{err}");
    }

    fn key(n: u32) -> Key {
        Key {
            ffmpeg: "ffmpeg".into(),
            graph: format!("graph {n}"),
            font: None,
        }
    }

    fn made(bytes: usize) -> Result<Raster, GpuError> {
        Ok(raster(Some(coverage(0, 0, bytes as u32 / 2, 1, 255)), None))
    }

    #[test]
    fn the_atlas_serves_a_state_once_and_forgets_a_failure() {
        let atlas = TextAtlas::default();
        let a = atlas.get(key(1), || made(100)).unwrap();
        let b = atlas.get(key(1), || panic!("served from the atlas")).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(atlas.runs(), 1);
        let err = atlas.get(key(2), || Err(GpuError::Unsupported("no".into()))).unwrap_err();
        assert!(matches!(err, GpuError::Unsupported(_)));
        // The failure is not remembered: the next ask runs again, and then holds.
        atlas.get(key(2), || made(100)).unwrap();
        atlas.get(key(2), || panic!("held")).unwrap();
        assert_eq!(atlas.runs(), 3);
        assert_eq!(atlas.held().0, 2);
    }

    #[test]
    fn the_atlas_is_bounded_and_drops_the_least_recently_used_first() {
        let atlas = TextAtlas::with_limits(3, usize::MAX);
        for n in 1..=3 {
            atlas.get(key(n), || made(100)).unwrap();
        }
        // Use 1 again, so 2 is the oldest.
        atlas.get(key(1), || panic!("held")).unwrap();
        atlas.get(key(4), || made(100)).unwrap();
        assert_eq!(atlas.held().0, 3);
        atlas.get(key(1), || panic!("1 was used last and stays")).unwrap();
        atlas.get(key(3), || panic!("3 was newer than 2")).unwrap();
        let runs = atlas.runs();
        atlas.get(key(2), || made(100)).unwrap();
        assert_eq!(atlas.runs(), runs + 1, "2 was the one dropped");

        // Bytes bound it too: each of these is charged its masks twice (CPU copy and texture).
        let small = TextAtlas::with_limits(usize::MAX, 4000);
        for n in 0..20 {
            small.get(key(n), || made(1000)).unwrap();
        }
        let (held, bytes) = small.held();
        assert!(
            (1..20).contains(&held) && bytes <= 4000 + 3 * (std::mem::size_of::<Raster>() + 2000),
            "{held} states, {bytes} bytes"
        );
        // The state just made is never its own victim.
        let big = TextAtlas::with_limits(usize::MAX, 10);
        big.get(key(1), || made(100)).unwrap();
        assert_eq!(big.held().0, 1);
    }

    #[test]
    fn a_drags_poses_are_made_into_their_own_atlas_and_never_push_the_cuts_out() {
        let cut = TextAtlas::with_limits(2, usize::MAX);
        let drafts = TextAtlas::with_limits(DRAFT_ATLAS_MAX_ENTRIES, DRAFT_ATLAS_MAX_BYTES);
        let a = cut.get(key(1), || made(100)).unwrap();
        cut.get(key(2), || made(100)).unwrap();
        assert!(cut.peek(&key(3)).is_none(), "a peek makes nothing");
        assert_eq!(cut.held().0, 2, "and keeps nothing");
        // A title of the cut that is not dragged is served from the cut's atlas.
        let served = cut.get_drafted(&drafts, key(1), || panic!("held by the cut")).unwrap();
        assert!(Arc::ptr_eq(&a, &served));
        // A pose per pointer move.
        for n in 10..30 {
            cut.get_drafted(&drafts, key(n), || made(100)).unwrap();
        }
        assert_eq!(drafts.held().0, DRAFT_ATLAS_MAX_ENTRIES, "the drag's atlas is bounded");
        cut.get(key(1), || panic!("the cut's titles are all still held")).unwrap();
        cut.get(key(2), || panic!("the cut's titles are all still held")).unwrap();
        assert_eq!(cut.held().0, 2);
        assert_eq!(cut.runs(), 2);
    }

    #[test]
    fn several_asks_for_one_state_run_one_rasterisation() {
        let atlas = Arc::new(TextAtlas::default());
        let started = Arc::new(AtomicU64::new(0));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (atlas, started) = (Arc::clone(&atlas), Arc::clone(&started));
                std::thread::spawn(move || {
                    atlas
                        .get(key(7), || {
                            started.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(50));
                            made(100)
                        })
                        .unwrap()
                })
            })
            .collect();
        let rasters: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(rasters.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
    }

    #[test]
    fn a_font_that_changes_is_another_state() {
        let dir = std::env::temp_dir().join(format!("kerf-atlas-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let font = dir.join("f.ttf");
        std::fs::write(&font, b"one").unwrap();
        let g = TextRasterGraph {
            filter_complex: "g".into(),
            parts: vec![TextPart::Glyphs],
            size: (1, 1),
        };
        let before = Key::new(&g, Some(&font));
        assert_eq!(before, Key::new(&g, Some(&font)));
        std::fs::write(&font, b"longer").unwrap();
        assert_ne!(before, Key::new(&g, Some(&font)), "the length changed");
        assert_ne!(Key::new(&g, None), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_is_too_big_or_too_long_is_refused_before_ffmpeg_runs() {
        let huge = graph(vec![TextPart::Glyphs, TextPart::Box], 20_000, 20_000);
        assert!(matches!(rasterize(&huge), Err(GpuError::Unsupported(_))));
        assert!(matches!(
            rasterize(&graph(vec![TextPart::Glyphs], 0, 10)),
            Err(GpuError::Unsupported(_))
        ));
        let long = TextRasterGraph {
            filter_complex: "x".repeat(MAX_GRAPH_CHARS + 1),
            parts: vec![TextPart::Glyphs],
            size: (100, 100),
        };
        assert!(matches!(rasterize(&long), Err(GpuError::Unsupported(_))));
    }
}
