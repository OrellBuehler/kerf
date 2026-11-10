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
//! (1080p on 6.1: ~30 ms a frame, a plain `overlay` ~3 ms). FFmpeg draws it; the GPU compositor draws the modes and
//! layers its probe measured as the model (`kerf_gpu::blend`), and refuses the rest (`Unsupported::BlendMode`).
//!
//! **Runs.** Each blended clip forks the composite and joins it back, so a track of them nests forks, and FFmpeg 9.0.2's
//! scheduler multiplies the work of a fork by ~3.3 for each that nests inside it. A run of [`FOLD_MIN`] or more consecutive
//! clips of one track that are drawn on frames no other of them is drawn on is laid on through one fork instead
//! ([`fold_runs`], [`blend_fold`]); what cannot fold is split into parts by the planner instead.

use super::grade::scale_matrix;
use crate::model::{BlendMode, VideoEffect};

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
pub(crate) fn composite_matrix(label_bt709: bool, frame_matrix: Option<&'static str>) -> Option<&'static str> {
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
    let mut chains = match blend.window {
        Some(window) => vec![
            format!("[{base}]format=yuv420p,split{}{}", p("m"), p("w")),
            select_chain(window, tag),
        ],
        None => vec![format!(
            "[{base}]format=yuv420p,split=4{}{}{}{}",
            p("b"),
            p("c"),
            p("a"),
            p("m")
        )],
    };
    chains.extend(placed_chains(layer, blend, tag));
    chains.extend(colour_chains(
        (&p("cc"), &p("aa"), &p("b"), &p("m")),
        blend.mode,
        blend.matrix,
        &p("t"),
        &p("u"),
        &p("y"),
        out,
    ));
    chains
}

/// The frames of the composite a blended clip can touch: `[bld{tag}w]` cut to `window` and split three ways, into
/// the copy the clip is laid on opaque (`c`), the one it is laid on transparent (`a`) and the one it is mixed with (`b`).
fn select_chain(window: &str, tag: &str) -> String {
    format!("[bld{tag}w]select='{window}',split=3[bld{tag}b][bld{tag}c][bld{tag}a]")
}

/// The clip on pad `layer` laid out as a whole frame twice, by `blend`'s placement: opaque onto `[bld{tag}c]` (`cc`, its colour),
/// and onto a transparent copy of `[bld{tag}a]`, whose alpha plane is `aa`.
fn placed_chains(layer: &str, blend: &Blend<'_>, tag: &str) -> [String; 5] {
    let p = |name: &str| format!("[bld{tag}{name}]");
    let placement = blend.placement;
    [
        format!("[{layer}]split{}{}", p("lc"), p("la")),
        format!("{}lutyuv=a=255{}", p("lc"), p("lo")),
        format!("{}{}{placement}{}", p("c"), p("lo"), p("cc")),
        format!("{}format=yuva420p,lutyuv=a=0{}", p("a"), p("az")),
        format!("{}{}{placement},alphaextract{}", p("az"), p("la"), p("aa")),
    ]
}

/// The conversion into RGB, the `blend` and the way back, from the opaque clip on the composite (`cc`), its alpha
/// plane (`aa`), the composite as it was (`b`) and the same again to be laid under the result (`m`): what
/// [`blend_chains`] writes after the clip is placed, and what a renderer's probe runs on pictures it made, so that
/// both are the very text. Ends on `[out]`; `t`, `u` and `y` are the pads it makes, as `[name]`.
pub(super) fn colour_chains(
    pads: (&str, &str, &str, &str),
    mode: &str,
    matrix: Option<&str>,
    t: &str,
    u: &str,
    y: &str,
    out: &str,
) -> Vec<String> {
    let (cc, aa, b, m) = pads;
    let mut chains = mixed_chains((cc, aa, b), mode, matrix, t, u, y);
    chains.push(format!("{m}{y}overlay=eof_action=pass[{out}]"));
    chains
}

/// [`colour_chains`] up to the mixed frame on `y` (`yuva420p`: the blend of the clip with the composite, weighed by the clip's
/// alpha), without the `overlay` that lays it under the composite.
fn mixed_chains(pads: (&str, &str, &str), mode: &str, matrix: Option<&str>, t: &str, u: &str, y: &str) -> Vec<String> {
    let (cc, aa, b) = pads;
    let (into_rgb, out_of_rgb) = matrix.map_or_else(Default::default, |m| {
        let m = scale_matrix(m);
        (
            format!(":in_color_matrix={m}:in_range=tv"),
            format!(":out_color_matrix={m}:out_range=tv"),
        )
    });
    vec![
        format!("{cc}{aa}alphamerge,scale=flags=neighbor{into_rgb},format=gbrap{t}"),
        format!("{b}scale=flags=neighbor{into_rgb},format=gbrap{u}"),
        format!("{t}{u}blend=c0_mode={mode}:c1_mode={mode}:c2_mode={mode},scale=flags=neighbor{out_of_rgb},format=yuva420p{y}"),
    ]
}

/// The colour half of a blended clip's chain on pictures a renderer makes itself: `[cc]` the composite with the clip
/// laid on it opaque, `[aa]` the clip's alpha plane, `[b]` and `[m]` the composite as it was (both 8-bit 4:2:0, one size),
/// `[out]` the frame [`blend_chains`] hands on. `matrix` is the composite's tag ([`composite_matrix`]); `None` for a mode
/// that is no blend ([`BlendMode::Normal`] has no FFmpeg name). The text a renderer's probe runs is the export's own.
pub fn blend_colour_graph(mode: BlendMode, matrix: Option<&str>) -> Option<String> {
    let mode = mode.ffmpeg()?;
    Some(colour_chains(("[cc]", "[aa]", "[b]", "[m]"), mode, matrix, "[t]", "[u]", "[y]", "out").join(";"))
}

/// The whole of a blended clip's chain on pictures a renderer makes itself: `[clip]` the clip's `yuva420p` frame, `[base]` the
/// composite (`yuv420p`), the clip placed at `(x, y)` by the very `overlay` the composite uses (`overlay=x=..:y=..:eof_action=pass`)
/// and ending on `[out]`. A renderer's probe runs the export's own text on small pictures, placement included. `None` for a mode that is
/// no blend.
pub fn blend_probe_graph(mode: BlendMode, (x, y): (i32, i32), matrix: Option<&str>) -> Option<String> {
    let placement = format!("overlay=x={x}:y={y}:eof_action=pass");
    let blend = Blend {
        mode: mode.ffmpeg()?,
        placement: &placement,
        window: None,
        matrix,
    };
    Some(blend_chains("base", "clip", &blend, "p", "out").join(";"))
}

/// The fewest consecutive blended clips on one track that [`fold_runs`] folds. Below it the nested chains of
/// [`blend_chains`] are cheap enough (6 nested: ~1 s on FFmpeg 9.0.2) to keep their byte-for-byte graphs, and
/// [`super::segments::SegmentLimits::blends`] holds a part that cannot fold to as many.
pub(super) const FOLD_MIN: usize = 7;

/// A clip of the picture list as [`fold_runs`] sees it, in the order the graph composites the clips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FoldClip {
    /// The track it is on.
    pub track: usize,
    /// The frames it can be drawn on, `first..end` ([`crate::clip_timing::ClipTiming::drawn_slots`]); `None` for a
    /// clip that cannot be folded: not blended, not footage, or on a rate the grid and the canvas do not agree on.
    pub slots: Option<(i64, i64)>,
}

/// The runs of at least `min` consecutive clips of one track that are blended footage drawn on frames no other
/// clip of the run is drawn on, as ranges of `clips` (pure).
///
/// **Why they are folded.** Each blended clip's chain forks the composite so far, runs it through `lutyuv`, `overlay`,
/// `alphaextract`, `alphamerge`, `scale`, `gbrap` and `blend`, and joins it back, and the next clip's chain does the
/// same to the result: the forks nest. FFmpeg 9.0.2's `filter_activate_default` re-requests through a filter whose
/// input is blocked, so every nested fork multiplies the requests (~3.3x a stage): 8 clips take 3 s, 10 take 23 s, 11 take
/// 77 s and 12 gave no frame in 250 s, where 6.1 is linear (12 clips in 4 s). [`blend_fold`] forks the composite
/// **once**, runs every clip's stage on its own copy and lays the mixed frames back in turn.
///
/// **Why it is the same picture.** The clips of a run are drawn on frames no other clip of it is, so on every frame at
/// most one stage has an alpha; the others are 0 there, and a chain of `overlay`s lays a frame of alpha 0 onto the
/// composite without changing a byte. That stage's colour was mixed with the composite the nested chain has at that
/// frame too: the base, because no clip of the run before it is drawn there. The slots are
/// [`crate::clip_timing::ClipTiming::drawn_slots`], wide where the graph is uncertain. A clip that overlaps another (an
/// unaligned cut, a transition's tail, a stack on one track) ends a run and starts the next.
pub(super) fn fold_runs(clips: &[FoldClip], min: usize) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < clips.len() {
        let Some((_, mut reach)) = clips[i].slots else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        // A clip joins when it starts at or after the end of every clip so far (`reach`): disjoint from each whatever the order.
        while let Some(FoldClip {
            track,
            slots: Some((first, end)),
        }) = clips.get(j).copied()
        {
            if track != clips[i].track || first < reach {
                break;
            }
            reach = reach.max(end);
            j += 1;
        }
        if j - i >= min {
            runs.push(i..j);
        }
        i = j;
    }
    runs
}

/// One clip of a run [`blend_fold`] lays onto the composite.
pub(super) struct FoldStage {
    /// The pad of the clip's picture.
    pub layer: String,
    /// Makes the clip's pads unique in the graph, as [`blend_chains`]'s `tag`.
    pub tag: String,
    /// What [`Blend`] holds for one clip.
    pub mode: &'static str,
    pub placement: String,
    pub window: String,
}

/// The chains that lay a run of blended clips ([`fold_runs`]) onto the composite on pad `base` and end on `out`: where
/// [`blend_chains`] per clip forks the composite once a clip, this forks it **once** (`split` into a copy for the
/// accumulation and one for each clip's window), runs each clip's own chains on its copy up to its mixed frame
/// (`[bld{tag}y]`, byte for byte what [`blend_chains`] makes of it), and lays the mixed frames onto the first copy in turn.
/// `tag` makes the pads of the fold unique (`bldf{tag}`, `bldf{tag}_{i}`).
pub(super) fn blend_fold(base: &str, stages: &[FoldStage], matrix: Option<&str>, tag: &str, out: &str) -> Vec<String> {
    let acc = |i: usize| {
        if i == 0 {
            format!("[bldf{tag}]")
        } else {
            format!("[bldf{tag}_{i}]")
        }
    };
    let arms: String = stages.iter().map(|s| format!("[bld{}w]", s.tag)).collect();
    let mut chains = vec![format!("[{base}]format=yuv420p,split={}{}{arms}", stages.len() + 1, acc(0))];
    for (i, stage) in stages.iter().enumerate() {
        let p = |name: &str| format!("[bld{}{name}]", stage.tag);
        let blend = Blend {
            mode: stage.mode,
            placement: &stage.placement,
            window: Some(&stage.window),
            matrix,
        };
        chains.push(select_chain(&stage.window, &stage.tag));
        chains.extend(placed_chains(&stage.layer, &blend, &stage.tag));
        chains.extend(mixed_chains(
            (&p("cc"), &p("aa"), &p("b")),
            stage.mode,
            matrix,
            &p("t"),
            &p("u"),
            &p("y"),
        ));
        let onto = if i + 1 == stages.len() {
            format!("[{out}]")
        } else {
            acc(i + 1)
        };
        chains.push(format!("{}{}overlay=eof_action=pass{onto}", acc(i), p("y")));
    }
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

    fn slots(track: usize, first: i64, end: i64) -> FoldClip {
        FoldClip {
            track,
            slots: Some((first, end)),
        }
    }

    /// `n` clips of `each` frames one after the other on `track`.
    fn butted(track: usize, n: i64, each: i64) -> Vec<FoldClip> {
        (0..n).map(|i| slots(track, i * each, (i + 1) * each)).collect()
    }

    #[test]
    fn a_run_needs_at_least_the_minimum_of_clips_that_share_no_frame() {
        assert!(fold_runs(&butted(0, 6, 2), 7).is_empty(), "six nest cheaply");
        assert_eq!(fold_runs(&butted(0, 7, 2), 7), vec![0..7]);
        assert_eq!(fold_runs(&butted(0, 40, 1), 7), vec![0..40]);
        // A gap between two is no overlap.
        let mut gapped = butted(0, 8, 2);
        gapped[3] = slots(0, 7, 8);
        assert_eq!(fold_runs(&gapped, 7), vec![0..8]);
        // Clips that are no clips (empty slots) are still disjoint from their neighbours.
        let mut empty = butted(0, 8, 2);
        empty[4] = slots(0, 8, 8);
        assert_eq!(fold_runs(&empty, 7), vec![0..8]);
    }

    #[test]
    fn a_frame_shared_by_two_clips_ends_a_run() {
        // One frame of overlap between the 7th and 8th of 14: two runs of seven, nothing shorter folds.
        let mut both = butted(0, 14, 2);
        both[7].slots = Some((13, 16));
        assert_eq!(fold_runs(&both, 7), vec![0..7, 7..14]);
        // ... and in the 4th of 8, neither half is long enough.
        let mut short = butted(0, 8, 2);
        short[4].slots = Some((7, 10));
        assert!(fold_runs(&short, 7).is_empty());
        // A clip inside another's slots is not disjoint from it: the run starts again at the inner one.
        let inside = vec![slots(0, 0, 10), slots(0, 2, 4), slots(0, 10, 12), slots(0, 12, 14)];
        assert_eq!(fold_runs(&inside, 2), vec![1..4]);
        // The same slots in another order are no run.
        let mut backwards = butted(0, 8, 2);
        backwards.reverse();
        assert!(fold_runs(&backwards, 2).is_empty());
    }

    #[test]
    fn a_run_is_one_track_of_blended_footage() {
        let mut two = butted(0, 7, 2);
        two.extend(butted(1, 7, 2));
        assert_eq!(fold_runs(&two, 7), vec![0..7, 7..14]);
        // 4 + 4 on two tracks are no run of 7.
        let mut split = butted(0, 4, 2);
        split.extend(butted(1, 4, 2));
        assert!(fold_runs(&split, 7).is_empty());
        // A clip that cannot fold (not blended, a still) between them cuts the run.
        let mut cut = butted(0, 15, 2);
        cut[7].slots = None;
        assert_eq!(fold_runs(&cut, 7), vec![0..7, 8..15]);
        cut[3].slots = None;
        assert_eq!(fold_runs(&cut, 7), vec![8..15]);
        assert!(fold_runs(&[], 7).is_empty());
    }

    fn stages(n: usize) -> Vec<FoldStage> {
        (1..=n)
            .map(|k| FoldStage {
                layer: format!("v{k}"),
                tag: format!("{k}"),
                mode: "multiply",
                placement: format!("overlay=eof_action=pass:enable='between(t,{},{k})'", k - 1),
                window: format!("between(t,{},{k})", k - 1),
            })
            .collect()
    }

    #[test]
    fn a_fold_forks_the_composite_once_and_lays_the_mixed_frames_back_in_turn() {
        let list = stages(8);
        let chains = blend_fold("vbase", &list, Some("bt709"), "1", "vout");
        let graph = chains.join(";");
        assert!(
            graph.starts_with("[vbase]format=yuv420p,split=9[bldf1][bld1w][bld2w][bld3w][bld4w][bld5w][bld6w][bld7w][bld8w];")
        );
        assert_eq!(graph.matches("format=yuv420p,split").count(), 1, "{graph}");
        assert!(graph.ends_with("[bldf1_7][bld8y]overlay=eof_action=pass[vout]"));
        for k in 0..7 {
            let from = if k == 0 {
                "[bldf1]".to_string()
            } else {
                format!("[bldf1_{k}]")
            };
            assert!(
                graph.contains(&format!("{from}[bld{}y]overlay=eof_action=pass[bldf1_{}];", k + 1, k + 1)),
                "{k}: {graph}"
            );
        }
        // Each pad is written once and read once.
        for pad in ["w", "b", "c", "a", "lc", "la", "lo", "cc", "az", "aa", "t", "u", "y"] {
            for k in 1..=8 {
                assert_eq!(graph.matches(&format!("[bld{k}{pad}]")).count(), 2, "{k}{pad}: {graph}");
            }
        }
        for pad in (0..8).map(|k| {
            if k == 0 {
                "[bldf1]".to_string()
            } else {
                format!("[bldf1_{k}]")
            }
        }) {
            assert_eq!(graph.matches(&pad).count(), 2, "{pad}: {graph}");
        }
        assert!(!graph.contains("[bld1m]"), "no clip keeps a copy of its own: {graph}");
    }

    #[test]
    fn a_fold_runs_each_clips_own_chains_up_to_its_mixed_frame() {
        // Every statement of a clip's nested chain but its fork and the overlay that joins it is in the fold, as written.
        let (layer, placement, window) = (
            "v3",
            "overlay=x=10:y=4:eof_action=pass:enable='between(t,2,3)'",
            "between(t,2,3)",
        );
        let blend = Blend {
            mode: "screen",
            placement,
            window: Some(window),
            matrix: Some("bt709"),
        };
        let nested = blend_chains("vbase", layer, &blend, "3", "o");
        let stage = FoldStage {
            layer: layer.into(),
            tag: "3".into(),
            mode: "screen",
            placement: placement.into(),
            window: window.into(),
        };
        let folded = blend_fold("vbase", &[stage], Some("bt709"), "3", "o");
        // [fork, select, 5 placed, 3 mixed, join] against [fork, select, 5 placed, 3 mixed, accumulate].
        assert_eq!(nested.len(), 11);
        assert_eq!(nested[1..10], folded[1..10]);
        assert_eq!(folded[0], "[vbase]format=yuv420p,split=2[bldf3][bld3w]");
        assert_eq!(folded[10], "[bldf3][bld3y]overlay=eof_action=pass[o]");
    }
}
