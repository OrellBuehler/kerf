//! What drawing one frame of a document costs, for the parts of resvg whose time is not
//! proportional to the size of the document.
//!
//! The expansion guard counts nodes, the image guard counts pixels, and both are about the
//! *document*. A filter is about the *picture*: `feMorphology` visits `(2 r_x) x (2 r_y)` pixels
//! for every pixel of its region, so a single `radius="40"` on a full-frame title is minutes a frame
//! out of a document of a hundred bytes, and `feConvolveMatrix` and `feTurbulence` multiply by their
//! `order` and `numOctaves` the same way. What that costs depends on the size the title is drawn at.
//!
//! **The walk is resvg's, transform for transform.** A radius or a region is in the user space of
//! the group it hangs on, and the scale that carries it to pixels is every group's `transform`
//! multiplied on the way down — including the one usvg puts on the root for the `viewBox` (so
//! `viewBox="0 0 96 54"` at 1920 x 1080 draws a radius of 1 at 20 px), a `<g transform="scale(1000)">`
//! round a tiny filtered rect, and a transform an animation drives. So the estimate carries the
//! transform down as `resvg::render` does (`transform.pre_concat(group.transform())`), and a mask, a
//! pattern or an `feImage` starts again from the transform resvg gives it. A filter region is held to
//! resvg's own limit, five times the canvas each way (`max_filter_bbox`), not to the canvas.
//!
//! The estimate is asked once when a document is read (at a nominal 1080p frame, on the document as
//! stored) and again by the rasteriser for every picture, on the static document of that moment and
//! the transform it is really drawn with — so a title that grows into a heavy filter is refused at
//! the frame where it does, before that frame is drawn.
//!
//! Beyond filters it counts what else grows with the size of the picture rather than the document:
//! a group that needs a pixmap of its own (`opacity`, a clip, a mask, a blend mode), the pixels each
//! path or image covers, and a pattern tile (which must also fit the limit: an allocation that fails
//! aborts the process, it is not an error). Together they bound one picture to seconds, which is
//! what bounds a preparation: resvg cannot be stopped half way through a picture.

use resvg::usvg::filter::Kind;
use resvg::usvg::{self, Group, Node, Paint, Transform};

use super::{invalid, raster};
use crate::error::Result;

/// The most work one frame may ask of its filters and layers, in per-pixel operations: a few seconds
/// on one core. A full-frame 1080p morphology of radius 16 px is this much; blurs, lighting, colour
/// matrices and a handful of turbulence octaves are one to two orders below it.
pub(crate) const MAX_FRAME_OPS: f64 = 2.0e9;

/// The frame a document is judged at when it is read, before anyone knows where it will be drawn.
const NOMINAL: (u32, u32) = (1920, 1080);

/// Operations a plain filter primitive costs for each pixel of its region (a blur is a few passes,
/// a lighting model a handful of multiplications).
const PER_PIXEL: f64 = 10.0;

/// A group's own pixmap: allocated, drawn into and composited back.
const LAYER: f64 = 2.0;

/// A clip or a mask: one more pass over the group's pixmap.
const MASKING: f64 = 3.0;

/// A path or an image: the pixels of its box (an upper bound: a thin diagonal has a box of the
/// whole frame).
const PAINT: f64 = 1.0;

/// Nesting followed through masks, patterns and `feImage` before the estimate gives up descending
/// (and refuses: nothing real nests deeper).
const MAX_DEPTH: usize = 32;

/// The pixmap being drawn into, as resvg has it: its size, and the limit a layer or filter region
/// made inside it is held to (`max_filter_bbox`: five times the canvas each way, or the region
/// itself inside an `feImage`).
#[derive(Clone, Copy)]
struct Surface {
    w: f64,
    h: f64,
    limit_w: f64,
    limit_h: f64,
}

impl Surface {
    fn canvas(w: f64, h: f64) -> Self {
        Surface {
            w,
            h,
            limit_w: w * 5.0,
            limit_h: h * 5.0,
        }
    }

    fn within(self, w: f64, h: f64) -> Self {
        Surface { w, h, ..self }
    }

    /// The pixels of a `w` x `h` box that land on this surface.
    fn covered(self, w: f64, h: f64) -> f64 {
        w.min(self.w) * h.min(self.h)
    }
}

/// The estimated work of drawing `tree` with `ts` as the root transform onto a pixmap of `canvas`
/// pixels (what `resvg::render` is given).
pub(crate) fn frame_ops(tree: &usvg::Tree, ts: Transform, canvas: (f64, f64)) -> f64 {
    let mut walk = Walk { ops: 0.0 };
    walk.children(tree.root(), ts, Surface::canvas(canvas.0, canvas.1), 0);
    walk.ops
}

struct Walk {
    ops: f64,
}

impl Walk {
    /// `resvg::render::render_nodes`: the children of `parent` under `ts`.
    fn children(&mut self, parent: &Group, ts: Transform, surface: Surface, depth: usize) {
        if depth > MAX_DEPTH {
            self.ops = f64::INFINITY;
            return;
        }
        for node in parent.children() {
            match node {
                Node::Group(group) => self.group(group, ts, surface, depth),
                Node::Path(path) => self.path(path, ts, surface, depth),
                Node::Image(image) => {
                    if let Some(r) = image.bounding_box().transform(ts) {
                        self.ops += surface.covered(f64::from(r.width()), f64::from(r.height())) * PAINT;
                    }
                }
                Node::Text(text) => self.group(text.flattened(), ts, surface, depth),
            }
        }
    }

    /// `resvg::render::render_group`.
    fn group(&mut self, group: &Group, ts: Transform, surface: Surface, depth: usize) {
        let ts = ts.pre_concat(group.transform());
        if !group.should_isolate() {
            self.children(group, ts, surface, depth);
            return;
        }
        // resvg draws nothing, children included, for a layer it cannot place.
        let Some(bbox) = group.layer_bounding_box().transform(ts) else {
            return;
        };
        // A layer is its box, grown by 2 px a side unless filters clip it, and held to the limit.
        let pad = if group.filters().is_empty() { 4.0 } else { 0.0 };
        let layer_w = (f64::from(bbox.width()).ceil().max(1.0) + pad).min(surface.limit_w);
        let layer_h = (f64::from(bbox.height()).ceil().max(1.0) + pad).min(surface.limit_h);
        let layer = layer_w * layer_h;
        self.ops += layer * LAYER;
        let inner = surface.within(layer_w, layer_h);
        self.children(group, ts, inner, depth);
        for filter in group.filters() {
            self.filter(filter, ts, inner, depth);
        }
        if group.clip_path().is_some() {
            self.ops += layer * MASKING;
        }
        let mut mask = group.mask();
        while let Some(m) = mask {
            self.ops += layer * MASKING;
            self.children(m.root(), ts, inner, depth + 1);
            mask = m.mask();
        }
    }

    fn path(&mut self, path: &usvg::Path, ts: Transform, surface: Surface, depth: usize) {
        if !path.is_visible() {
            return;
        }
        let bbox = if path.stroke().is_some() {
            path.stroke_bounding_box()
        } else {
            path.bounding_box()
        };
        if let Some(r) = bbox.transform(ts) {
            self.ops += surface.covered(f64::from(r.width()), f64::from(r.height())) * PAINT;
        }
        let paints = [path.fill().map(|f| f.paint()), path.stroke().map(|s| s.paint())];
        for paint in paints.into_iter().flatten() {
            if let Paint::Pattern(pattern) = paint {
                // `render_pattern_pixmap`: a tile of the rect at the scale of the pattern's own
                // transform, its content drawn at that scale.
                let (sx, sy) = ts.pre_concat(pattern.transform()).get_scale();
                let rect = pattern.rect();
                let (w, h) = (
                    (f64::from(rect.width()) * f64::from(sx)).round(),
                    (f64::from(rect.height()) * f64::from(sy)).round(),
                );
                if w * h > surface.limit_w * surface.limit_h {
                    // A tile larger than any layer may be: its allocation is the process's to lose.
                    self.ops = f64::INFINITY;
                    return;
                }
                self.ops += w * h * LAYER;
                let tile = Surface { w, h, ..surface };
                self.children(pattern.root(), Transform::from_scale(sx, sy), tile, depth + 1);
            }
        }
    }

    /// `resvg::filter::apply` for one filter of a group whose pixmap is `layer`.
    fn filter(&mut self, filter: &usvg::filter::Filter, ts: Transform, layer: Surface, depth: usize) {
        let Some(region) = filter.rect().transform(ts) else {
            return;
        };
        // The region is clipped to the group's pixmap, which is itself held to the limit.
        let (rw, rh) = (
            f64::from(region.width()).ceil().max(1.0).min(layer.w),
            f64::from(region.height()).ceil().max(1.0).min(layer.h),
        );
        let px = rw * rh;
        let (sx, sy) = ts.get_scale();
        for primitive in filter.primitives() {
            self.ops += match primitive.kind() {
                Kind::Morphology(m) => {
                    let (rx, ry) = (
                        f64::from(m.radius_x().get()) * f64::from(sx),
                        f64::from(m.radius_y().get()) * f64::from(sy),
                    );
                    if rx > 0.0 && ry > 0.0 {
                        // resvg's window, clipped to the region: `min(2 * ceil(r), size)`.
                        px * (rx.ceil() * 2.0).min(rw) * (ry.ceil() * 2.0).min(rh)
                    } else {
                        0.0
                    }
                }
                Kind::ConvolveMatrix(c) => px * f64::from(c.matrix().columns()) * f64::from(c.matrix().rows()),
                Kind::Turbulence(t) => px * f64::from(t.num_octaves()) * PER_PIXEL,
                Kind::Image(image) => {
                    // `apply_image`: the content drawn at the group's scale into a region-sized
                    // pixmap, and held to that pixmap rather than to the canvas.
                    let inside = Surface {
                        w: rw,
                        h: rh,
                        limit_w: rw,
                        limit_h: rh,
                    };
                    self.children(image.root(), Transform::from_scale(sx, sy), inside, depth + 1);
                    px * PER_PIXEL
                }
                _ => px * PER_PIXEL,
            };
        }
    }
}

/// Refuse a document whose filters and layers are too slow to draw frame after frame at the nominal
/// frame.
pub(crate) fn check_nominal(tree: &usvg::Tree) -> Result<()> {
    let (w, h) = (tree.size().width(), tree.size().height());
    let (fw, fh) = raster::fit_contain(w, h, NOMINAL.0, NOMINAL.1);
    let ts = Transform::from_scale(fw as f32 / w, fh as f32 / h);
    if frame_ops(tree, ts, (f64::from(fw), f64::from(fh))) > MAX_FRAME_OPS {
        return Err(invalid(TOO_SLOW));
    }
    Ok(())
}

/// What a document is refused with when it is too slow.
pub(crate) const TOO_SLOW: &str = "the SVG would take seconds to draw each frame (a large feMorphology radius, feConvolveMatrix order or feTurbulence numOctaves over a big area, a scaled-up filter, or a great many large overlapping shapes and translucent groups); lower them or shrink the filter region";

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_in(root_attrs: &str, inner: &str) -> usvg::Tree {
        let src = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" {root_attrs}>{inner}</svg>");
        usvg::Tree::from_str(&src, &usvg::Options::default()).unwrap()
    }

    fn tree(inner: &str) -> usvg::Tree {
        tree_in("viewBox=\"0 0 100 50\"", inner)
    }

    fn at_scale(tree: &usvg::Tree, scale: f32) -> f64 {
        frame_ops(tree, Transform::from_scale(scale, scale), (1920.0, 1080.0))
    }

    fn ops(inner: &str, scale: f32) -> f64 {
        at_scale(&tree(inner), scale)
    }

    #[test]
    fn a_plain_document_and_a_blur_cost_next_to_nothing() {
        assert!(ops("<rect width=\"50\" height=\"50\"/>", 19.2) < MAX_FRAME_OPS / 500.0);
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
        assert!(ops(&conv(9), 19.2) > ops(&conv(3), 19.2) * 5.0);
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

    const HEAVY: &str = "<filter id=\"f\" x=\"0\" y=\"0\" width=\"1\" height=\"1\"><feMorphology radius=\"1\"/></filter>";

    #[test]
    fn the_viewbox_scale_in_the_roots_group_is_part_of_the_scale() {
        // 96 x 54 user units in a 1920 x 1080 picture: usvg sizes the tree 1920 x 1080 and puts the
        // 20x of the viewBox in a group's transform, so the radius of 1 is 20 px, not 1.
        let rect = "<rect width=\"96\" height=\"54\" filter=\"url(#f)\"/>";
        let scaled = tree_in(
            "width=\"1920\" height=\"1080\" viewBox=\"0 0 96 54\"",
            &format!("{HEAVY}{rect}"),
        );
        assert_eq!(f64::from(scaled.size().width()), 1920.0);
        let ts = Transform::identity();
        assert!(frame_ops(&scaled, ts, (1920.0, 1080.0)) > MAX_FRAME_OPS);
        // The same document, sized in its own units and scaled by the caller: the same cost.
        let own = tree_in("viewBox=\"0 0 96 54\"", &format!("{HEAVY}{rect}"));
        let by_caller = frame_ops(&own, Transform::from_scale(20.0, 20.0), (1920.0, 1080.0));
        let by_viewbox = frame_ops(&scaled, ts, (1920.0, 1080.0));
        assert!((by_caller / by_viewbox - 1.0).abs() < 0.01, "{by_caller} {by_viewbox}");
        // And drawn small it is cheap, whichever way it is sized.
        assert!(frame_ops(&scaled, Transform::from_scale(0.05, 0.05), (96.0, 54.0)) < MAX_FRAME_OPS / 100.0);
    }

    #[test]
    fn a_scale_on_a_group_round_a_tiny_filtered_rect_is_counted() {
        // 0.01 of a unit is nothing; the group's scale(1000) makes the region 3000 units across, which
        // resvg holds to five canvases each way.
        let big = String::from(
            "<g transform=\"scale(1000)\"><filter id=\"f\" x=\"-100\" y=\"-100\" width=\"300\" height=\"300\"><feMorphology radius=\"0.001\"/></filter><rect width=\"0.01\" height=\"0.01\" filter=\"url(#f)\"/></g>"
        );
        assert!(ops(&big, 19.2) > MAX_FRAME_OPS * 10.0, "{}", ops(&big, 19.2));
        let unscaled = big.replace("scale(1000)", "scale(1)");
        assert!(ops(&unscaled, 19.2) < MAX_FRAME_OPS / 1000.0);
    }

    #[test]
    fn a_filter_region_is_held_to_five_canvases_not_to_one() {
        // A flood over a region far larger than the picture: resvg's pixmap for it is 5W x 5H.
        let doc = "<filter id=\"f\" x=\"-100\" y=\"-100\" width=\"300\" height=\"300\"><feFlood/></filter><rect width=\"10\" height=\"10\" filter=\"url(#f)\"/>";
        let cost = ops(doc, 19.2);
        let limit = 5.0 * 1920.0 * 5.0 * 1080.0;
        assert!(
            cost > limit * PER_PIXEL && cost < limit * (PER_PIXEL + LAYER) * 1.01,
            "{cost} against {limit} px"
        );
    }

    #[test]
    fn a_root_transform_that_is_not_a_scale_still_carries_the_radius() {
        // A rotated group: the scale part of the matrix is what resvg multiplies a radius by.
        let doc = format!("{HEAVY}<g transform=\"rotate(90 50 25)\"><rect width=\"100\" height=\"50\" filter=\"url(#f)\"/></g>");
        assert!(ops(&doc, 19.2) > MAX_FRAME_OPS);
    }

    #[test]
    fn layers_paths_and_tiles_count_when_they_are_large_and_many() {
        let rect = "<rect width=\"100\" height=\"50\" opacity=\"0.5\"/>";
        let few = ops(&rect.repeat(10), 19.2);
        let many = ops(&rect.repeat(400), 19.2);
        assert!(few < MAX_FRAME_OPS / 20.0, "{few}");
        assert!(many > MAX_FRAME_OPS, "400 full-frame translucent layers: {many}");
        // The same count of small ones is nothing.
        let small = "<rect width=\"4\" height=\"2\" opacity=\"0.5\"/>";
        assert!(ops(&small.repeat(400), 19.2) < MAX_FRAME_OPS / 100.0);
        // A pattern tile bigger than any layer could be would be an allocation that aborts.
        let tile = "<pattern id=\"p\" width=\"1000000\" height=\"1000000\" patternUnits=\"userSpaceOnUse\"><rect width=\"1\" height=\"1\"/></pattern><rect width=\"100\" height=\"50\" fill=\"url(#p)\"/>";
        assert!(ops(tile, 19.2).is_infinite());
    }

    #[test]
    fn masks_nested_past_the_limit_are_refused_not_ignored() {
        // A mask whose content is masked by a mask...: each level is a subroot, and a chain deeper
        // than anything real is refused rather than walked half way.
        let chain = |levels: usize| {
            let mut doc = String::from("<rect width=\"10\" height=\"10\" mask=\"url(#m0)\"/>");
            for i in 0..levels {
                doc.push_str(&format!(
                    "<mask id=\"m{i}\"><rect width=\"10\" height=\"10\" mask=\"url(#m{})\"/></mask>",
                    i + 1
                ));
            }
            doc.push_str(&format!("<mask id=\"m{levels}\"><rect width=\"10\" height=\"10\"/></mask>"));
            doc
        };
        let ops = |levels| frame_ops(&tree(&chain(levels)), Transform::from_scale(1.0, 1.0), (100.0, 50.0));
        assert!(ops(8).is_finite());
        assert!(ops(40).is_infinite());
    }

    #[test]
    fn a_document_is_judged_at_a_nominal_frame_when_it_is_read() {
        let heavy = format!("{HEAVY}<rect width=\"100\" height=\"50\" filter=\"url(#f)\"/>");
        assert!(check_nominal(&tree(&heavy)).is_err());
        assert!(check_nominal(&tree("<rect width=\"100\" height=\"50\"/>")).is_ok());
        let viewbox = tree_in(
            "width=\"1920\" height=\"1080\" viewBox=\"0 0 96 54\"",
            &format!("{HEAVY}<rect width=\"96\" height=\"54\" filter=\"url(#f)\"/>"),
        );
        assert!(check_nominal(&viewbox).is_err(), "the viewBox scale is counted");
    }
}
