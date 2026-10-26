//! A clip laid onto the composite with a blend mode ([`crate::model::BlendMode`]) instead of `overlay`'s
//! alpha-over, in the export graph and the scrubbed still alike.
//!
//! **The shape.** `overlay` composes a layer of its own size at a position; `blend` mixes two whole frames of
//! one size and format. So the clip is first laid out as a whole frame, by the very `overlay` (position, travel,
//! per-frame zoom, `enable` window) the plain composite would use, twice: once **opaque** (`lutyuv=a=255`) onto a
//! copy of the composite, which gives the clip's own colour wherever its picture is, and once onto a
//! **transparent** copy (`lutyuv=a=0`), which gives its alpha — opacity, mask, key, fades of a dissolve —
//! wherever its picture is, and 0 everywhere else, including outside its `enable` window. (One `overlay` onto a
//! transparent canvas would do both if it kept colours, but `overlay` mixes the canvas's chroma into a layer
//! that is not opaque.) `alphamerge` joins the two, `blend` mixes the clip (top) with the composite (bottom)
//! and keeps the clip's alpha (`c3` is left `normal`, which copies the top plane), and a last `overlay` lays
//! the result onto the untouched composite by that alpha: `composite·(1−α) + mode(clip, composite)·α`, the
//! textbook formula for a separable mode over an opaque backdrop. Where the clip has no picture, α is 0 and
//! the composite comes out byte for byte.
//!
//! **Colour.** Blend modes are defined in RGB, so both frames go to 8-bit `gbrap` and the result back to
//! `yuva420p` (the composite is the 8-bit 4:2:0 every `overlay` composes in). The matrix and range of the
//! conversions are the composite's own as the build negotiates it — not named here: FFmpeg 6 carries the
//! composite untagged (BT.601 both ways), FFmpeg 9 negotiates the colourspace across the graph (its tag both
//! ways); naming a matrix made FFmpeg 9 convert the whole composite into it at the last `overlay`. The scaler
//! is `neighbor`, so 4:2:0 chroma is repeated on the way up and taken back on the way down (a bicubic scaler
//! bled chroma across the clip's edge: 14 levels): the same graph run with `normal` is the plain `overlay`'s
//! frame byte for byte for a flat colour, opaque or not, and to a level of rounding for a smooth picture, on 6.1
//! and 9.0 (`blend_rendered.rs`). **Known limits:** a 4:2:0 picture with sharp saturated edges, where a pixel's
//! luma and its block's chroma make no RGB colour, is clipped into the gamut on the way (any RGB blend has to;
//! ~25 levels at such edges of a test pattern); and at the right or bottom edge of a layer of odd size, where a
//! 2x2 chroma block is only part covered, FFmpeg 9's `overlay` weighs that chroma sample its own way and the
//! blended frame takes it from one pixel of the block (up to some tens of levels in that one column or row of
//! chroma; on 6.1 it matches the plain `overlay` there too).
//!
//! **Cost.** Three whole-frame conversions, a `blend` and three `overlay`s per frame for as long as the
//! composite runs, not only while the clip is on screen; FFmpeg draws it, the GPU compositor refuses it
//! (`Unsupported::BlendMode`).

/// The chains that lay the clip on pad `layer` onto the composite on pad `base` with FFmpeg `blend` mode `mode`
/// (its name for the mode with the clip as the top input, [`crate::model::BlendMode::ffmpeg`]) and end on `out`,
/// where a plain composite would be `[base][layer]{placement}[out]`. `placement` is that `overlay=…` filter;
/// `tag` makes the pads unique in the graph.
pub(super) fn blend_chains(base: &str, layer: &str, placement: &str, mode: &str, tag: &str, out: &str) -> Vec<String> {
    let p = |name: &str| format!("[bld{tag}{name}]");
    vec![
        format!("[{base}]format=yuv420p,split=4{}{}{}{}", p("b"), p("c"), p("a"), p("m")),
        format!("[{layer}]split{}{}", p("lc"), p("la")),
        format!("{}lutyuv=a=255{}", p("lc"), p("lo")),
        format!("{}{}{placement}{}", p("c"), p("lo"), p("cc")),
        format!("{}format=yuva420p,lutyuv=a=0{}", p("a"), p("az")),
        format!("{}{}{placement},alphaextract{}", p("az"), p("la"), p("aa")),
        format!("{}{}alphamerge,scale=flags=neighbor,format=gbrap{}", p("cc"), p("aa"), p("t")),
        format!("{}scale=flags=neighbor,format=gbrap{}", p("b"), p("u")),
        format!(
            "{}{}blend=c0_mode={mode}:c1_mode={mode}:c2_mode={mode},scale=flags=neighbor,format=yuva420p{}",
            p("t"),
            p("u"),
            p("y")
        ),
        format!("{}{}overlay=eof_action=pass[{out}]", p("m"), p("y")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layer_is_laid_out_twice_by_its_own_placement_and_merged_by_its_alpha() {
        let placement = "overlay=x=(W-w)/2+(0.25)*W:y=(H-h)/2+(0)*H:eof_action=pass:enable='between(t,1,3)'";
        let chains = blend_chains("vbase", "v2", placement, "multiply", "2", "vov0");
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
            "the composite's own matrix, as negotiated: {graph}"
        );
        assert!(graph.ends_with("[bld2m][bld2y]overlay=eof_action=pass[vov0]"));
        // Every pad is written once and read once.
        for pad in ["b", "c", "a", "m", "lc", "la", "lo", "cc", "az", "aa", "t", "u", "y"] {
            assert_eq!(graph.matches(&format!("[bld2{pad}]")).count(), 2, "{pad}: {graph}");
        }
    }
}
