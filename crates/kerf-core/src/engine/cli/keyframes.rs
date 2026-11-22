//! Keyframe curves written as ffmpeg expressions.

use super::*;

/// Polyline points above which [`keyframe_expr`] writes a balanced tree instead of a chain.
/// libavutil refuses an expression nested deeper than 100 levels (`Invalid argument`, from
/// the filter that holds it), a chain is a level per point, and an eased key is 12 points: ten
/// eased keys failed the export and the playback stream alike. Up to this many points the
/// chain's depth is harmless and its text is the one every earlier graph carried.
pub(super) const KEYFRAME_TREE_POINTS: usize = 24;

/// Build a piecewise-linear ffmpeg expression over **clip-local time** for a
/// channel of keyframes. `points` are `(seconds_from_clip_start, value)` and are
/// sorted here. `tvar` is the time variable the target filter exposes (`t` for
/// overlay / scale / rotate, `T` for geq); `start` is the clip's `timeline_start`
/// so the expression reads time relative to the clip. Values hold flat before the
/// first and after the last keyframe.
///
/// Few points are nested `if(lt(..))`s walked from the first segment; many are a balanced
/// binary tree over the segments (`if(lt(t,t_mid),left,right)`, [`KEYFRAME_TREE_POINTS`]),
/// which is `log2(n)` levels deep and `log2(n)` comparisons per evaluation (a `geq` runs it per
/// pixel). Both pick the one segment whose span holds the time, so they evaluate alike.
pub(super) fn keyframe_expr(points: &[(f64, f64)], tvar: &str, start: f64) -> String {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    if pts.is_empty() {
        return "0".to_string();
    }
    if pts.len() == 1 {
        return fnum(pts[0].1);
    }
    let lt = format!("({tvar}-{})", fnum(start));
    let last = pts[pts.len() - 1];
    // The part between the first and the last key; the ends hold their values.
    let inside = if pts.len() > KEYFRAME_TREE_POINTS {
        let tree = keyframe_tree(&pts, 0, pts.len() - 1, &lt);
        format!("if(lt({lt},{tl}),{tree},{vl})", tl = fnum(last.0), vl = fnum(last.1))
    } else {
        // Fold segments in from the end; `expr` starts as the value held after the
        // last keyframe.
        let mut expr = fnum(last.1);
        for w in (0..pts.len() - 1).rev() {
            expr = format!(
                "if(lt({lt},{t1}),{seg},{expr})",
                lt = lt,
                t1 = fnum(pts[w + 1].0),
                seg = keyframe_segment(&pts, w, &lt),
                expr = expr
            );
        }
        expr
    };
    // Hold the first value before the first keyframe.
    format!(
        "if(lt({lt},{t0}),{v0},{inside})",
        lt = lt,
        t0 = fnum(pts[0].0),
        v0 = fnum(pts[0].1),
        inside = inside
    )
}

/// What [`keyframe_expr`]`(points, _, start)` evaluates to at time `t`, in the expression's own
/// `double` arithmetic and order (`av_expr` parses the constants back to these `f64`s, subtracts
/// as it adds the negation and multiplies by its sign 1): the same segment, picked by the same
/// comparisons, and the same line through it. A filter that keys a number per frame (`geq`'s
/// opacity) can be followed to the last bit.
pub(crate) fn keyframe_value(points: &[(f64, f64)], t: f64, start: f64) -> f64 {
    // (A curve is sorted already; the stable sort of one that is changes nothing, so it is not copied.)
    let sorted;
    let pts: &[(f64, f64)] = if points.windows(2).all(|w| w[0].0.total_cmp(&w[1].0).is_le()) {
        points
    } else {
        let mut copy = points.to_vec();
        copy.sort_by(|a, b| a.0.total_cmp(&b.0));
        sorted = copy;
        &sorted
    };
    match pts.len() {
        0 => return 0.0,
        1 => return pts[0].1,
        _ => {}
    }
    let lt = t - start;
    let segment = |w: usize| {
        let (t0, v0) = pts[w];
        let (t1, v1) = pts[w + 1];
        if (t1 - t0).abs() < 1e-9 {
            v0
        } else {
            v0 + (v1 - v0) * (lt - t0) / (t1 - t0)
        }
    };
    if lt < pts[0].0 {
        return pts[0].1;
    }
    let last = pts[pts.len() - 1];
    if pts.len() > KEYFRAME_TREE_POINTS {
        if lt >= last.0 {
            return last.1;
        }
        let (mut lo, mut hi) = (0, pts.len() - 1);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if lt < pts[mid].0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        return segment(lo);
    }
    (0..pts.len() - 1).find(|&w| lt < pts[w + 1].0).map_or(last.1, segment)
}

/// The value inside segment `w` (`pts[w]` to `pts[w + 1]`): a straight line, or the first
/// value for a step (equal times).
fn keyframe_segment(pts: &[(f64, f64)], w: usize, lt: &str) -> String {
    let (t0, v0) = pts[w];
    let (t1, v1) = pts[w + 1];
    if (t1 - t0).abs() < 1e-9 {
        fnum(v0)
    } else {
        format!(
            "({v0}+({dv})*({lt}-{t0})/({dt}))",
            v0 = fnum(v0),
            dv = fnum(v1 - v0),
            lt = lt,
            t0 = fnum(t0),
            dt = fnum(t1 - t0),
        )
    }
}

/// Segments `lo..hi` of `pts` as a balanced tree, for a time known to lie in
/// `[pts[lo].0, pts[hi].0)`. A step's empty span is never reached.
pub(super) fn keyframe_tree(pts: &[(f64, f64)], lo: usize, hi: usize, lt: &str) -> String {
    if hi - lo == 1 {
        return keyframe_segment(pts, lo, lt);
    }
    let mid = (lo + hi) / 2;
    format!(
        "if(lt({lt},{t}),{left},{right})",
        t = fnum(pts[mid].0),
        left = keyframe_tree(pts, lo, mid, lt),
        right = keyframe_tree(pts, mid, hi, lt),
    )
}
