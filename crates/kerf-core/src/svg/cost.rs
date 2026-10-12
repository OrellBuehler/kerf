//! What drawing one frame of a document costs, for the parts of resvg whose time is not
//! proportional to the size of the document.
//!
//! The expansion guard counts nodes, the image guard counts pixels, and both are about the
//! *document*. A filter is about the *picture*: `feMorphology` visits `(2 r_x) x (2 r_y)` pixels
//! for every pixel of its region, so a single `radius="40"` on a full-frame title is minutes a frame
//! out of a document of a hundred bytes, and `feConvolveMatrix` and `feTurbulence` multiply by their
//! `order` and `numOctaves` the same way. What that costs depends on the size the title is drawn at,
//! so the estimate is a function of the scale, asked once when a document is read (at a nominal
//! 1080p frame) and again by the rasteriser for each picture at its real size.

use resvg::usvg::filter::Kind;
use resvg::usvg::{self, Group, Node, Paint};

use super::invalid;
use crate::error::Result;

/// The most work one frame may ask of its filters, in per-pixel operations: a few seconds on one
/// core. A full-frame 1080p morphology of radius 16 px is this much; blurs, lighting, colour
/// matrices and a handful of turbulence octaves are one to two orders below it.
pub(crate) const MAX_FRAME_OPS: f64 = 2.0e9;

/// The frame a document is judged at when it is read, before anyone knows where it will be drawn.
const NOMINAL: (f64, f64) = (1920.0, 1080.0);

/// Operations a plain filter primitive costs for each pixel of its region (a blur is a few passes,
/// a lighting model a handful of multiplications).
const PER_PIXEL: f64 = 10.0;

/// Nesting followed through masks, patterns and `feImage` before the estimate gives up descending.
const MAX_DEPTH: usize = 32;

/// The estimated filter work of drawing `tree` at `scale` device pixels per user unit onto a canvas
/// of `canvas` pixels (a filter's region never reaches past it).
pub(crate) fn frame_ops(tree: &usvg::Tree, scale: f64, canvas: (f64, f64)) -> f64 {
    let mut ops = 0.0;
    walk(tree.root(), scale, canvas, 0, &mut ops);
    ops
}

fn walk(group: &Group, scale: f64, canvas: (f64, f64), depth: usize, ops: &mut f64) {
    if depth > MAX_DEPTH {
        return;
    }
    for filter in group.filters() {
        let region = filter.rect();
        let px = ((f64::from(region.width()) * scale).min(canvas.0)) * ((f64::from(region.height()) * scale).min(canvas.1));
        for primitive in filter.primitives() {
            *ops += match primitive.kind() {
                Kind::Morphology(m) => {
                    let (rx, ry) = (f64::from(m.radius_x().get()), f64::from(m.radius_y().get()));
                    if rx == 0.0 || ry == 0.0 {
                        0.0
                    } else {
                        // resvg's window, clipped to the picture: `min(2 * ceil(r), size)`.
                        let columns = ((rx * scale).ceil() * 2.0).min(canvas.0);
                        let rows = ((ry * scale).ceil() * 2.0).min(canvas.1);
                        px * columns * rows
                    }
                }
                Kind::ConvolveMatrix(c) => px * f64::from(c.matrix().columns()) * f64::from(c.matrix().rows()),
                Kind::Turbulence(t) => px * f64::from(t.num_octaves()) * PER_PIXEL,
                Kind::Image(image) => {
                    walk(image.root(), scale, canvas, depth + 1, ops);
                    px * PER_PIXEL
                }
                _ => px * PER_PIXEL,
            };
        }
    }
    if let Some(mask) = group.mask() {
        walk(mask.root(), scale, canvas, depth + 1, ops);
    }
    for node in group.children() {
        match node {
            Node::Group(child) => walk(child, scale, canvas, depth + 1, ops),
            Node::Path(path) => {
                let paints = [path.fill().map(|f| f.paint()), path.stroke().map(|s| s.paint())];
                for paint in paints.into_iter().flatten() {
                    if let Paint::Pattern(pattern) = paint {
                        walk(pattern.root(), scale, canvas, depth + 1, ops);
                    }
                }
            }
            Node::Image(_) | Node::Text(_) => {}
        }
    }
}

/// Refuse a document whose filters are too slow to draw frame after frame at the nominal frame.
pub(crate) fn check_nominal(tree: &usvg::Tree) -> Result<()> {
    let size = tree.size();
    let scale = (NOMINAL.0 / f64::from(size.width())).min(NOMINAL.1 / f64::from(size.height()));
    if frame_ops(tree, scale, NOMINAL) > MAX_FRAME_OPS {
        return Err(invalid(TOO_SLOW));
    }
    Ok(())
}

/// What a document is refused with when its filters are too slow.
pub(crate) const TOO_SLOW: &str = "the SVG's filters (a large feMorphology radius, feConvolveMatrix order or feTurbulence numOctaves over a big area) would take seconds to draw each frame; lower them or shrink the filter region";

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(inner: &str) -> usvg::Tree {
        let src = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 50\">{inner}</svg>");
        usvg::Tree::from_str(&src, &usvg::Options::default()).unwrap()
    }

    fn ops(inner: &str, scale: f64) -> f64 {
        frame_ops(&tree(inner), scale, (1920.0, 1080.0))
    }

    #[test]
    fn a_plain_document_and_a_blur_cost_next_to_nothing() {
        assert_eq!(ops("<rect width=\"50\" height=\"50\"/>", 19.2), 0.0);
        let blur =
            "<filter id=\"f\"><feGaussianBlur stdDeviation=\"3\"/></filter><rect width=\"50\" height=\"50\" filter=\"url(#f)\"/>";
        let cost = ops(blur, 19.2);
        assert!(cost > 0.0 && cost < MAX_FRAME_OPS / 50.0, "{cost}");
    }

    #[test]
    fn a_morphology_costs_its_window_for_every_pixel_and_so_grows_with_the_scale() {
        let m = |r: f64| {
            format!("<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology operator=\"dilate\" radius=\"{r}\"/></filter><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/>")
        };
        let small = ops(&m(0.1), 19.2);
        let big = ops(&m(1.0), 19.2);
        assert!(big > small * 50.0, "{small} {big}");
        // 1 user unit at 19.2 px a unit is a 40 x 40 window over the whole frame.
        assert!(big > MAX_FRAME_OPS, "{big}");
        // The same radius drawn small is cheap: the estimate follows the picture, not the document.
        assert!(ops(&m(1.0), 1.0) < MAX_FRAME_OPS / 100.0);
    }

    #[test]
    fn a_convolution_and_a_turbulence_multiply_by_their_order_and_octaves() {
        let conv = |order: u32| {
            let kernel = vec!["1"; (order * order) as usize].join(" ");
            format!("<filter id=\"f\"><feConvolveMatrix order=\"{order}\" kernelMatrix=\"{kernel}\"/></filter><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/>")
        };
        assert!(ops(&conv(9), 19.2) > ops(&conv(3), 19.2) * 8.0);
        let turb = |n: u32| {
            format!("<filter id=\"f\"><feTurbulence baseFrequency=\"0.05\" numOctaves=\"{n}\"/></filter><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/>")
        };
        assert!(ops(&turb(100_000), 19.2) > MAX_FRAME_OPS);
        assert!(ops(&turb(4), 19.2) < MAX_FRAME_OPS / 10.0);
    }

    #[test]
    fn a_filter_inside_a_mask_or_a_pattern_is_counted_too() {
        let f = "<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology radius=\"1\"/></filter>";
        let masked = format!(
            "{f}<mask id=\"m\"><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/></mask><rect width=\"100\" height=\"50\" mask=\"url(#m)\"/>"
        );
        assert!(ops(&masked, 19.2) > MAX_FRAME_OPS);
        let patterned = format!(
            "{f}<pattern id=\"p\" width=\"100\" height=\"50\" patternUnits=\"userSpaceOnUse\"><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/></pattern><rect width=\"100\" height=\"50\" fill=\"url(#p)\"/>"
        );
        assert!(ops(&patterned, 19.2) > MAX_FRAME_OPS);
    }

    #[test]
    fn a_document_is_judged_at_a_nominal_frame_when_it_is_read() {
        let heavy = "<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology radius=\"1\"/></filter><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/>";
        assert!(check_nominal(&tree(heavy)).is_err());
        assert!(check_nominal(&tree("<rect width=\"100\" height=\"50\"/>")).is_ok());
    }
}
