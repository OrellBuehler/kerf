//! The blurred fill of a delivery frame ([`crate::model::Fit::Blur`]): the integer geometry of the thumbnail
//! chain FFmpeg builds it with (`engine::cli::blur_fill`), pure, shared by the plan (which refuses what the sizes make
//! unrenderable) and the GPU compositor (which draws it).
//!
//! The chain, for a picture of `iw` x `ih` fitted into a `w` x `h` frame:
//!
//! ```text
//! split[b][f];
//! [b]scale=bw:bh:force_original_aspect_ratio=increase:flags=bilinear,crop=bw:bh,boxblur=r:2,
//!    scale=w:h:flags=bilinear[bb];
//! [f]scale=w:h:force_original_aspect_ratio=decrease[ff];
//! [bb][ff]overlay=(W-w)/2:(H-h)/2
//! ```
//!
//! with `bw` x `bh` a sixteenth of the frame on each side doubled (`2*floor(w/16)`, at least 4) and `r` a sixteenth of the
//! thumbnail's smaller side (at least 1). The copy is scaled to *cover* the thumbnail, cropped to it about its centre
//! (`crop` takes `lrint((in - out) / 2)` rounded down to the chroma grid), blurred twice horizontally and twice vertically
//! by `boxblur` on each plane at its own size, and scaled up to the frame; the picture, fitted as under `Contain`, is
//! overlaid on it.

use crate::layer_geometry::{av_rescale, even, lrint, GeometryError, Rect};

/// The largest side of the cover the thumbnail's scaler is made for: a side the size of a texture the GPUs accept.
pub const MAX_COVER: i64 = 16_384;

/// The thumbnail chain of one picture in one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlurFill {
    /// The picture as decoded (the size the thumbnail is scaled from).
    pub source: (u32, u32),
    /// The delivery frame the fill covers.
    pub frame: (u32, u32),
    /// `scale=bw:bh:force_original_aspect_ratio=increase`: the picture scaled to cover the thumbnail.
    pub cover: (u32, u32),
    /// The window of the cover that `crop=bw:bh` keeps (even offsets: the 4:2:0 grid). Its size is the thumbnail's.
    pub crop: Rect,
    /// `boxblur`'s radius, in samples of each plane (the chroma planes use the same number).
    pub radius: u32,
}

impl BlurFill {
    /// The chain for a `source` picture and a `frame`, or why `boxblur` or the scalers could not run it as drawn.
    pub fn resolve(source: (u32, u32), frame: (u32, u32)) -> Result<Self, GeometryError> {
        let ((iw, ih), (w, h)) = (source, frame);
        if iw == 0 || ih == 0 || w == 0 || h == 0 {
            return Err(GeometryError("an empty picture or frame".into()));
        }
        let (bw, bh) = ((w / 16 * 2).max(4), (h / 16 * 2).max(4));
        // A radius above half the chroma plane is refused by `boxblur` itself.
        let radius = (bw.min(bh) / 16).max(1);
        // `increase`: each side is the larger of the thumbnail's and the other side's aspect-preserving one.
        let tmp_w = av_rescale(i64::from(bh), i64::from(iw), i64::from(ih));
        let tmp_h = av_rescale(i64::from(bw), i64::from(ih), i64::from(iw));
        let cover = (tmp_w.max(i64::from(bw)), tmp_h.max(i64::from(bh)));
        // (A picture of a degenerate aspect covers the thumbnail with millions of rows; the scalers' tables for it are as many.)
        if cover.0 > MAX_COVER || cover.1 > MAX_COVER {
            return Err(GeometryError(format!("a thumbnail cover of {}x{}", cover.0, cover.1)));
        }
        let cover = (cover.0 as u32, cover.1 as u32);
        let crop = Rect {
            x: even(lrint(f64::from(cover.0 - bw) / 2.0)) as u32,
            y: even(lrint(f64::from(cover.1 - bh) / 2.0)) as u32,
            w: bw,
            h: bh,
        };
        // The running sum of `boxblur` reads the sample `radius` past a position it is computing at, up to `2 * radius`
        // from the start of a line: a line shorter than that is read past its end (chroma: half the thumbnail).
        let shortest = bw.min(bh) / 2;
        if 2 * radius + 1 > shortest {
            return Err(GeometryError(format!(
                "a {bw}x{bh} thumbnail is too small for a box blur of radius {radius} on its {}x{} chroma planes",
                bw / 2,
                bh / 2
            )));
        }
        Ok(Self {
            source,
            frame,
            cover,
            crop,
            radius,
        })
    }

    /// The thumbnail's size (`bw` x `bh`).
    pub fn thumb(&self) -> (u32, u32) {
        (self.crop.w, self.crop.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_portrait_phone_clip_in_a_16_9_frame_is_covered_by_a_240_by_134_thumbnail() {
        let f = BlurFill::resolve((1080, 1920), (1920, 1080)).unwrap();
        assert_eq!(f.thumb(), (240, 134));
        assert_eq!(f.radius, 8);
        // 134 rows from a 240-wide cover of a 9:16 picture: 240 x 427, cropped about its centre.
        assert_eq!(f.cover, (240, 427));
        assert_eq!(
            f.crop,
            Rect {
                x: 0,
                y: 146,
                w: 240,
                h: 134
            }
        );
    }

    #[test]
    fn a_picture_of_a_degenerate_aspect_has_no_thumbnail() {
        // A 1x100000 picture covers a 240-wide thumbnail with 24 million rows: refused, not tabled.
        assert!(BlurFill::resolve((1, 100_000), (1920, 1080)).is_err());
        assert!(BlurFill::resolve((100_000, 1), (1920, 1080)).is_err());
        assert!(BlurFill::resolve((u32::MAX, 1), (u32::MAX, u32::MAX)).is_err());
        assert!(BlurFill::resolve((1080, 1920), (1920, 1080)).is_ok());
    }

    #[test]
    fn a_wide_picture_in_a_tall_frame_crops_columns() {
        let f = BlurFill::resolve((1920, 1080), (1080, 1920)).unwrap();
        assert_eq!(f.thumb(), (134, 240));
        assert_eq!(f.cover, (427, 240));
        assert_eq!(
            f.crop,
            Rect {
                x: 146,
                y: 0,
                w: 134,
                h: 240
            }
        );
    }

    #[test]
    fn the_crop_offset_is_rounded_half_to_even_and_then_down_to_the_chroma_grid() {
        // A cover 5 wider than the thumbnail: lrint(2.5) is 2 (ties to even), and an offset of 3.5 is 4.
        let f = BlurFill::resolve((165, 100), (100, 100)).unwrap();
        assert_eq!(f.thumb(), (12, 12));
        assert_eq!(f.cover.1, f.crop.h.max(f.cover.1));
        assert!(f.crop.x.is_multiple_of(2) && f.crop.y.is_multiple_of(2));
    }

    #[test]
    fn a_frame_too_small_for_the_blur_is_not_resolved() {
        // 4x4 thumbnail: radius 1 needs 3 samples of a 2-sample chroma line.
        assert!(BlurFill::resolve((100, 200), (32, 32)).is_err());
        // 8x8 thumbnail: chroma lines of 4.
        assert!(BlurFill::resolve((100, 200), (64, 64)).is_ok());
    }

    #[test]
    fn the_radius_grows_with_the_frame_and_stays_inside_the_chroma_plane() {
        for (w, h) in [
            (1920u32, 1080u32),
            (3840, 2160),
            (1080, 1920),
            (640, 360),
            (256, 256),
            (7680, 4320),
        ] {
            let f = BlurFill::resolve((w / 2 + 1, h / 3 + 1), (w, h)).unwrap();
            assert!(2 * f.radius < f.thumb().0.min(f.thumb().1) / 2, "{w}x{h}");
            assert_eq!(f.radius, (f.thumb().0.min(f.thumb().1) / 16).max(1));
        }
    }
}
