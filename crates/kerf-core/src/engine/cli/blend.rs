//! A clip laid onto the composite with a blend mode ([`crate::model::BlendMode`]) instead of `overlay`'s
//! alpha-over, in the export graph and the scrubbed still alike.
//!
//! **The shape.** `overlay` composes a layer of its own size at a position; `blend` mixes two whole frames of
//! one size and format. So the clip is first laid out as a whole frame, by the very `overlay` (position, travel,
//! per-frame zoom, `enable` window) the plain composite would use, twice: once **opaque** (`lutyuv=a=255`) onto a
//! copy of the composite, which gives the clip's own colour wherever its picture is, and once onto a
//! **transparent** copy (`lutyuv=a=0`), which gives its alpha — opacity, mask, key, fades and dips (a blended
//! clip fades its alpha, [`crate::clip_timing::ClipTiming::fades`]: faded as colour, a multiplied clip went
//! through black), a dissolve — wherever its picture is, and 0 everywhere else: outside its `enable` window, and
//! in the bars of a letterboxed clip, which are padded transparent ([`keeps_clear_bars`]). (One `overlay` onto a
//! transparent canvas would do both if it kept colours, but `overlay` mixes the canvas's chroma into a layer
//! that is not opaque.) `alphamerge` joins the two, `blend` mixes the clip (top) with the composite (bottom)
//! and keeps the clip's alpha (`c3` is left `normal`, which copies the top plane), and a last `overlay` lays
//! the result onto the untouched composite by that alpha: `composite·(1−α) + mode(clip, composite)·α`, the
//! textbook formula for a separable mode over an opaque backdrop. Where the clip has no picture, α is 0 and
//! the composite comes out byte for byte.
//!
//! **Colour.** Blend modes are defined in RGB, so both frames go to 8-bit `gbrap` and the result back to
//! `yuva420p` (the composite is the 8-bit 4:2:0 every `overlay` composes in), in the composite's matrix: the
//! bottom layer's tag ([`composite_matrix`]), named to both conversions with limited range. FFmpeg 9 negotiates
//! that same tag across the graph, so naming it changes nothing there and the composite outside the clip stays
//! byte for byte (naming any other made FFmpeg 9 convert the whole composite into it at the last `overlay`);
//! FFmpeg 6 carries the composite untagged and converted BT.709 footage as BT.601, mixing the wrong RGB (up to 30
//! levels for `difference`). An untagged bottom layer names nothing: BT.601 both ways on both builds. The scaler
//! is `neighbor`, so 4:2:0 chroma is repeated on the way up and taken back on the way down (a bicubic scaler
//! bled chroma across the clip's edge: 14 levels): the same graph run with `normal` is the plain `overlay`'s
//! frame byte for byte for a flat colour, opaque or not, and to a level of rounding for a smooth picture, on 6.1
//! and 9.0 (`blend_rendered.rs`). A rotated blended clip's corners are filled with transparent black, not left as
//! the zeroed buffer (chroma 0), which put its edge 33 levels of luma off. **Known limits:** a 4:2:0 picture with
//! sharp saturated edges, where a pixel's luma and its block's chroma make no RGB colour, is clipped into the
//! gamut on the way (any RGB blend has to; ~25 levels at such edges of a test pattern); along an edge that cuts
//! a 2x2 chroma block — a rotated clip's slanted edges, the right or bottom edge of a layer of odd size — the
//! blended frame takes the block's chroma from one pixel of it where `overlay` weighs it by the block's alpha
//! (luma byte for byte; up to 80 levels of chroma in those blocks, which is where a `normal` run through the
//! graph differs from the plain `overlay`); and a letterboxed clip whose chain drops the alpha plane after the
//! pad (a tone-map, a grade given back to a tagged matrix, a vignette) keeps black bars, which mix like picture.
//!
//! **Cost.** Only the frames in the clip's `enable` window go through the blend: the composite is split, one
//! copy passing on untouched to the last `overlay`, and the copies the blend reads are `select`ed to the window
//! (the same expression as the `enable`, so the same frames), so every other frame is the composite's, byte for
//! byte, at the cost of a `split`. In the window: three conversions, a `blend` and three `overlay`s a frame
//! (1080p on 6.1: ~30 ms a frame, a plain `overlay` ~3 ms). FFmpeg draws it, the GPU compositor refuses it
//! (`Unsupported::BlendMode`).

use super::grade::scale_matrix;
use crate::model::VideoEffect;

/// How a clip is blended: what [`blend_chains`] needs besides its pads.
pub(super) struct Blend<'a> {
    /// FFmpeg `blend`'s name for the mode with the clip as the top input ([`crate::model::BlendMode::ffmpeg`]).
    pub mode: &'a str,
    /// The `overlay=…` filter a plain composite would place the clip with.
    pub placement: &'a str,
    /// The frames the clip can show on, as a `select` expression (its overlay's `enable`); `None` for a still,
    /// which has one frame.
    pub window: Option<&'a str>,
    /// The composite's YCbCr matrix tag (the bottom layer's, [`composite_matrix`]), named to the conversions into
    /// RGB and back; `None` leaves them to the build's default.
    pub matrix: Option<&'a str>,
}

/// The matrix tag of the composite a clip is blended onto: the bottom layer's (`setparams` names, BT.709 for a
/// layer the graph labels so, else [`crate::model::frame_matrix`]'s answer). FFmpeg 9 negotiates the bottom
/// layer's tag across the graph and converts every other layer into it; FFmpeg 6 carries the composite untagged.
pub(super) fn composite_matrix(label_bt709: bool, frame_matrix: Option<&'static str>) -> Option<&'static str> {
    if label_bt709 {
        Some("bt709")
    } else {
        frame_matrix
    }
}

/// Whether a clip's letterbox bars are padded transparent rather than black: it is `blended` (on black bars a
/// multiply would black out the lower tracks), and every filter between the pad and the overlay keeps the alpha
/// plane, which a tone-map (`tone_mapped`), a grade given back to a tagged matrix (its `format=yuv444p…` in
/// `grade`) and a `vignette` in `effects` do not. Those keep the black bars.
pub(super) fn keeps_clear_bars(blended: bool, tone_mapped: bool, effects: &[VideoEffect], grade: &[String]) -> bool {
    blended
        && !tone_mapped
        && !effects.iter().any(|e| matches!(e, VideoEffect::Vignette))
        && !grade.iter().any(|f| f.starts_with("format="))
}

/// The chains that lay the clip on pad `layer` onto the composite on pad `base` as `blend` says and end on
/// `out`, where a plain composite would be `[base][layer]{placement}[out]`. `tag` makes the pads unique in
/// the graph.
pub(super) fn blend_chains(base: &str, layer: &str, blend: &Blend<'_>, tag: &str, out: &str) -> Vec<String> {
    let p = |name: &str| format!("[bld{tag}{name}]");
    let Blend {
        mode,
        placement,
        window,
        matrix,
    } = blend;
    let (into_rgb, out_of_rgb) = matrix.map_or_else(Default::default, |m| {
        let m = scale_matrix(m);
        (
            format!(":in_color_matrix={m}:in_range=tv"),
            format!(":out_color_matrix={m}:out_range=tv"),
        )
    });
    let mut chains = match window {
        Some(window) => vec![
            format!("[{base}]format=yuv420p,split{}{}", p("m"), p("w")),
            format!("{}select='{window}',split=3{}{}{}", p("w"), p("b"), p("c"), p("a")),
        ],
        None => vec![format!(
            "[{base}]format=yuv420p,split=4{}{}{}{}",
            p("b"),
            p("c"),
            p("a"),
            p("m")
        )],
    };
    chains.extend([
        format!("[{layer}]split{}{}", p("lc"), p("la")),
        format!("{}lutyuv=a=255{}", p("lc"), p("lo")),
        format!("{}{}{placement}{}", p("c"), p("lo"), p("cc")),
        format!("{}format=yuva420p,lutyuv=a=0{}", p("a"), p("az")),
        format!("{}{}{placement},alphaextract{}", p("az"), p("la"), p("aa")),
        format!(
            "{}{}alphamerge,scale=flags=neighbor{into_rgb},format=gbrap{}",
            p("cc"),
            p("aa"),
            p("t")
        ),
        format!("{}scale=flags=neighbor{into_rgb},format=gbrap{}", p("b"), p("u")),
        format!(
            "{}{}blend=c0_mode={mode}:c1_mode={mode}:c2_mode={mode},scale=flags=neighbor{out_of_rgb},format=yuva420p{}",
            p("t"),
            p("u"),
            p("y")
        ),
        format!("{}{}overlay=eof_action=pass[{out}]", p("m"), p("y")),
    ]);
    chains
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layer_is_laid_out_twice_by_its_own_placement_and_merged_by_its_alpha() {
        let placement = "overlay=x=(W-w)/2+(0.25)*W:y=(H-h)/2+(0)*H:eof_action=pass:enable='between(t,1,3)'";
        let blend = Blend {
            mode: "multiply",
            placement,
            window: None,
            matrix: None,
        };
        let chains = blend_chains("vbase", "v2", &blend, "2", "vov0");
        assert_eq!(chains.len(), 10);
        let graph = chains.join(";");
        assert!(graph.starts_with("[vbase]format=yuv420p,split=4[bld2b][bld2c][bld2a][bld2m];[v2]split[bld2lc][bld2la];"));
        assert_eq!(
            graph.matches(placement).count(),
            2,
            "the colour and the alpha are placed alike: {graph}"
        );
        assert!(graph.contains("[bld2t][bld2u]blend=c0_mode=multiply:c1_mode=multiply:c2_mode=multiply,"));
        assert!(!graph.contains("c3_mode"), "the alpha plane is the clip's: {graph}");
        assert!(
            !graph.contains("color_matrix"),
            "an untagged composite keeps the build's default: {graph}"
        );
        assert!(graph.ends_with("[bld2m][bld2y]overlay=eof_action=pass[vov0]"));
        // Every pad is written once and read once.
        for pad in ["b", "c", "a", "m", "lc", "la", "lo", "cc", "az", "aa", "t", "u", "y"] {
            assert_eq!(graph.matches(&format!("[bld2{pad}]")).count(), 2, "{pad}: {graph}");
        }
    }

    #[test]
    fn only_the_clips_window_is_converted_in_the_composites_matrix() {
        let blend = Blend {
            mode: "screen",
            placement: "overlay=eof_action=pass:enable='between(t,1,3)'",
            window: Some("between(t,1,3)"),
            matrix: Some("bt2020nc"),
        };
        let graph = blend_chains("vbase", "v2", &blend, "2", "vov0").join(";");
        assert!(graph.starts_with(
            "[vbase]format=yuv420p,split[bld2m][bld2w];[bld2w]select='between(t,1,3)',split=3[bld2b][bld2c][bld2a];"
        ));
        assert_eq!(
            graph
                .matches("scale=flags=neighbor:in_color_matrix=bt2020:in_range=tv,format=gbrap")
                .count(),
            2,
            "{graph}"
        );
        assert!(graph.contains("scale=flags=neighbor:out_color_matrix=bt2020:out_range=tv,format=yuva420p[bld2y]"));
        assert!(graph.ends_with("[bld2m][bld2y]overlay=eof_action=pass[vov0]"));
        for pad in ["w", "b", "c", "a", "m", "t", "u", "y"] {
            assert_eq!(graph.matches(&format!("[bld2{pad}]")).count(), 2, "{pad}: {graph}");
        }
        assert_eq!(composite_matrix(true, Some("smpte170m")), Some("bt709"));
        assert_eq!(composite_matrix(false, None), None);
    }
}
