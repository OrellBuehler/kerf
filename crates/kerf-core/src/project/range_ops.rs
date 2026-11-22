//! Edits that name a time or a range and the tracks it is read on: cut every track at the playhead,
//! lift or extract an in/out range, delete a gap.
//!
//! The math is `model::range_edit`'s (pure, all or nothing); this is the revision around it. Each is
//! **one** `edit_named_exact` revision — they decide their own layout, so project ripple mode does
//! not apply to them (a lift must leave its gap whatever the toolbar says; an extract and a gap
//! delete close it themselves) — and the linked-A/V sync lock and guard of `run_edit` still run, so
//! an agent's call stages like any other edit and a partner on a locked track refuses it.

use super::*;
use crate::model::{CutAll, GapClosed, RangeEdit};

/// One frame of the cut, in seconds: the rate it renders at (the delivery frame's, else the first
/// picture's, else 30 fps) as the edit will find the timeline. What a range edit takes for a sliver.
fn one_frame(timeline: &Timeline, assets: &[Asset]) -> f64 {
    let (fps, _) = crate::delivery_rate(timeline, assets);
    1.0 / fps
}

/// The label of a revision that cut `n` clips: the plain `verb` for one, `(N clips)` appended for more.
fn counted(verb: &str, n: usize) -> String {
    match n {
        0 | 1 => verb.to_string(),
        n => format!("{verb} ({n} clips)"),
    }
}

impl Project {
    /// **Cut every track at `at`** ("Add Edit to All Tracks"): one `Split all tracks` revision that
    /// splits each clip with `at` inside it on every unlocked track, linked partners included.
    ///
    /// * `tracks` — the tracks to cut. `None` is every unlocked track (locked ones with a clip under
    ///   `at` are left alone and listed in [`CutAll::skipped`]); a named track must exist and not be
    ///   locked ([`Error::TrackNotFound`], else `InvalidArgument`); an empty list is refused.
    /// * A time on a clip's edge is no cut for that clip; a time where nothing can be cut — no clip
    ///   under it, it is already a cut everywhere, the clips are on locked tracks — is an
    ///   `InvalidArgument` saying which, and records no revision. A non-finite `at` is refused.
    /// * **Linked clips** are split with the clip they are linked to and the halves re-linked by side
    ///   (`Timeline::split_clip_linked`); a partner that would be cut on a locked track refuses the
    ///   whole edit. `with_links(Some(false))` splits exactly the clips on the targeted tracks.
    ///
    /// Each half is what [`Project::split_at`] makes of a clip. A clip with `at` closer than one frame
    /// (of the rate the cut renders at) to either of its edges is left whole and listed in
    /// [`CutAll::not_cut`]: the cut is as good as there. Never shifts anything, so ripple mode
    /// changes nothing about it. The label counts what was cut: `Split all tracks (3 clips)`.
    pub fn split_all_tracks(&self, at: f64, tracks: Option<&[Uuid]>) -> Result<CutAll> {
        finite("the cut time", at)?;
        let links = self.links_active();
        let assets = self.list_assets()?;
        self.edit_named_exact(
            &[],
            |cut: &CutAll| counted("Split all tracks", cut.cuts.len()),
            |timeline| {
                let frame = one_frame(timeline, &assets);
                timeline.split_all_tracks(at, tracks, links, frame)
            },
        )
    }

    /// **Lift** `[from, to)` off the targeted tracks, leaving the gap — one `Lift range` revision.
    ///
    /// A clip wholly inside the range is removed; one the range reaches into from a side is
    /// shortened at that end; one it sits in the middle of is cut in two around it. Footage, speed
    /// ramps, keyframes, fades and transitions at the new edges are exactly what a split and a trim to
    /// the playhead make of them (`Timeline::lift_range`); a generated title is cut like any clip.
    /// Nothing else moves, and **it ignores ripple mode**: lifting is leaving the gap.
    ///
    /// * `tracks` as for [`Project::split_all_tracks`]: none named = every unlocked track.
    /// * `from` below 0 is held at 0. A non-finite end, an empty or reversed range, and a range with
    ///   nothing in it on the targeted tracks are `InvalidArgument` and record no revision.
    /// * **Linked clips**: a partner of a clip the range reaches is cut with it, on whichever track it
    ///   is; the halves are re-linked by side. One on a locked track refuses the whole edit.
    ///   `with_links(Some(false))` cuts only the targeted tracks' clips.
    ///
    /// A remainder of a clip shorter than one frame (of the rate the cut renders at) is not left
    /// behind: the range absorbs it, and the reply's `from` / `to` are the range as applied with
    /// `absorbed_before` / `absorbed_after` saying how much. Overlays, text titles and markers are
    /// not touched. The label counts the clips cut: `Lift range (4 clips)`.
    pub fn lift_range(&self, from: f64, to: f64, tracks: Option<&[Uuid]>) -> Result<RangeEdit> {
        finite("the start of the range", from)?;
        finite("the end of the range", to)?;
        let links = self.links_active();
        let assets = self.list_assets()?;
        self.edit_named_exact(
            &[],
            |lift: &RangeEdit| counted("Lift range", lift.clips()),
            |timeline| {
                let frame = one_frame(timeline, &assets);
                timeline.lift_range(from, to, tracks, links, frame)
            },
        )
    }

    /// **Extract** `[from, to)` from the targeted tracks and close the gap — one `Extract range`
    /// revision. [`Project::lift_range`], then every clip from `to` on moves up by the range's
    /// length, each track on its own, the later clips of a linked group together (the sync lock).
    ///
    /// Same arguments, refusals and linked-clip rules as [`Project::lift_range`]; the range may
    /// also be empty of clips when something follows it on a targeted track (it then just closes
    /// the stretch up). Unlinked clips on tracks that were not targeted stay where they are, and
    /// so do overlays and markers (as in every ripple edit), which can leave them out of place
    /// against the picture afterwards. **Ignores ripple mode** (it closes the gap itself); an absorbed
    /// sliver is closed with the rest (`closed` is the range as applied). A linked sound that reaches
    /// into the range only because it leads or trails a picture that lies after it is not cut: it is
    /// carried with its picture by the sync lock, as a deleted gap carries it. A moved linked sound
    /// that would land on a clip it is not linked to (or further onto one it already overlaps), or a
    /// locked track, refuses the edit naming the lane.
    pub fn extract_range(&self, from: f64, to: f64, tracks: Option<&[Uuid]>) -> Result<RangeEdit> {
        finite("the start of the range", from)?;
        finite("the end of the range", to)?;
        let links = self.links_active();
        let assets = self.list_assets()?;
        self.edit_named_exact(
            &[],
            |extract: &RangeEdit| counted("Extract range", extract.clips()),
            |timeline| {
                let frame = one_frame(timeline, &assets);
                timeline.extract_range(from, to, tracks, links, frame)
            },
        )
    }

    /// **Ripple delete a gap**: close the empty stretch of `track_id` that `at` is in, moving every
    /// later clip of the track up by its length — one `Ripple delete gap` revision. The linked
    /// partners of the clips that moved follow through the sync lock (a sound that would land on a
    /// clip it is not linked to, or a locked track, refuses the edit naming the lane); clips of
    /// other tracks that are not linked, overlays and markers stay. Ignores ripple mode (it is the
    /// ripple).
    ///
    /// Refuses a locked or unknown track, a time before the start of the timeline, a time that is inside a clip, a time with no gap (the
    /// clips touch) and a gap with nothing after it. See `Timeline::ripple_delete_gap`.
    pub fn ripple_delete_gap(&self, track_id: Uuid, at: f64) -> Result<GapClosed> {
        finite("the gap's time", at)?;
        self.edit_named_exact(
            &[],
            |_: &GapClosed| "Ripple delete gap".to_string(),
            |timeline| timeline.ripple_delete_gap(track_id, at),
        )
    }
}
