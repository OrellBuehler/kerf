//! Edits that act on a stretch of time across tracks: cut every track at one time, **lift** or
//! **extract** a range, close a gap.
//!
//! The ops of a timeline that name a clip (`split_clip`, `split_remove`, `ripple_delete_clip`, ...)
//! work on one clip and let linked partners follow. These name a *time* (or a range) and the tracks
//! it is read on, which is what an editor means by "Add Edit to All Tracks" (Premiere), "Split Clip
//! on All Tracks" (Resolve), Lift and Extract, and "Delete Gap".
//!
//! **Targeted tracks.** Kerf has no track-targeting switch, so the tracks an op works on are the
//! ones it is asked for: with none named, every **unlocked** track (a locked track is left exactly
//! as it is and reported); named tracks must exist and be unlocked. Linked clips are not left
//! behind by that choice: a clip the range reaches whose linked partner sits on a track that was
//! not named is cut or moved with it, and a partner on a locked track refuses the whole edit (the
//! rule every other linked edit has).
//!
//! Every cut is made with the pieces the single-clip ops use, so keyframes, speed ramps, fades,
//! transitions and generated titles behave exactly as they do for a split, a trim to the playhead
//! and a remove: a head that moves re-times the animation (`rebase_animation`), the half that gets
//! a new edge loses the fade and the transition that belonged to the old one, and a range
//! reaching into a clip from one side is `split_remove`'s cut without its length floor.
//!
//! Pure and all or nothing: an op works on a copy and replaces the timeline only when it went
//! through, so an error leaves the timeline as it was. `Project` runs them inside one revision
//! (`edit_named_exact`: they decide their own layout and never ripple), and the sync lock and
//! guard of `run_edit` still apply afterwards.

use super::links::{locked_partner, runs_into_unlinked};
use super::*;

/// A track a range edit left alone because it is locked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkippedTrack {
    pub track_id: Uuid,
    pub name: String,
}

/// The two halves one clip was cut into by [`Timeline::split_all_tracks`].
#[derive(Debug, Clone, Serialize)]
pub struct ClipSplit {
    pub track_id: Uuid,
    /// The half before the cut: keeps the clip's id.
    pub left: Clip,
    /// The half after it: a new clip.
    pub right: Clip,
}

/// What [`Timeline::split_all_tracks`] did.
#[derive(Debug, Clone, Serialize)]
pub struct CutAll {
    /// The time that was cut, as given.
    pub at: f64,
    /// Every clip that was cut, in track order — the clips under `at` on the targeted tracks and
    /// the linked partners they took with them.
    pub cuts: Vec<ClipSplit>,
    /// Locked tracks with a clip under `at` that were left alone (only when no tracks were named).
    pub skipped: Vec<SkippedTrack>,
}

/// What [`Timeline::lift_range`] / [`Timeline::extract_range`] did.
#[derive(Debug, Clone, Serialize)]
pub struct RangeEdit {
    /// The range as it was applied (a start before 0 is held at 0).
    pub from: f64,
    pub to: f64,
    /// Clips that lay wholly inside the range and are gone.
    pub removed: usize,
    /// Clips the range reached into from one side, shortened at that end.
    pub trimmed: usize,
    /// Clips the range sat in the middle of, cut in two around it.
    pub split: usize,
    /// Clips that moved up to close the gap (an extract; always 0 for a lift).
    pub moved: usize,
    /// How far the later clips moved, in seconds: the range's length for an extract, 0 for a lift.
    pub closed: f64,
    /// The tracks that changed, in track order — linked partners' tracks included.
    pub tracks: Vec<Uuid>,
    /// Locked tracks the range reaches that were left alone (only when no tracks were named).
    pub skipped: Vec<SkippedTrack>,
}

impl RangeEdit {
    /// How many clips the range cut (removed, shortened or split).
    pub fn clips(&self) -> usize {
        self.removed + self.trimmed + self.split
    }
}

/// What [`Timeline::ripple_delete_gap`] closed.
#[derive(Debug, Clone, Serialize)]
pub struct GapClosed {
    pub track_id: Uuid,
    /// Where the gap was: the end of the clip before it (0 for a gap at the start) to the start of
    /// the clip after it.
    pub from: f64,
    pub to: f64,
    /// Clips of the track that moved up (linked partners on other tracks follow through the sync lock).
    pub moved: usize,
}

/// What the range did to one clip, for putting its link group back together.
enum Fate {
    Removed,
    /// Cut off at the range's start: the part before it stays.
    Tail,
    /// Cut off at the range's end: the part after it stays.
    Head,
    /// Cut in two; the new clip is the half after the range.
    Split(Uuid),
}

/// `at` is inside `clip`, not on one of its edges.
fn strictly_inside(clip: &Clip, at: f64) -> bool {
    clip.timeline_start + DIFF_EPS < at && at < clip.timeline_end() - DIFF_EPS
}

impl Timeline {
    /// The lanes a range edit works on, as indices in track order: with `tracks` the named ones, each
    /// of which must exist and be unlocked, else every unlocked track.
    fn range_lanes(&self, tracks: Option<&[Uuid]>) -> Result<Vec<usize>> {
        let Some(ids) = tracks else {
            let lanes: Vec<usize> = (0..self.tracks.len()).filter(|&ti| !self.tracks[ti].locked).collect();
            if lanes.is_empty() {
                return Err(Error::InvalidArgument(
                    "there is no unlocked track to edit — unlock one or add a track".to_string(),
                ));
            }
            return Ok(lanes);
        };
        if ids.is_empty() {
            return Err(Error::InvalidArgument(
                "name at least one track, or leave the list out to use every unlocked track".to_string(),
            ));
        }
        let mut lanes = Vec::with_capacity(ids.len());
        for id in ids {
            let ti = self
                .tracks
                .iter()
                .position(|t| t.id == *id)
                .ok_or(Error::TrackNotFound(*id))?;
            if self.tracks[ti].locked {
                return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
            }
            lanes.push(ti);
        }
        lanes.sort_unstable();
        lanes.dedup();
        Ok(lanes)
    }

    /// The link groups of two or more clips, each with the clips in track order.
    fn link_groups(&self) -> Vec<(Uuid, Vec<Uuid>)> {
        let mut by_link: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for clip in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            if let Some(link) = clip.link_id {
                by_link.entry(link).or_default().push(clip.id);
            }
        }
        by_link.into_iter().filter(|(_, members)| members.len() >= 2).collect()
    }

    // ---- cut every track at one time ---------------------------------------

    /// **Cut every track at `at`** ("Add Edit to All Tracks"): each clip of the targeted tracks that
    /// has `at` strictly inside it is split there, as [`Timeline::split_clip`] splits one — and with
    /// `links` its linked partners with it ([`Timeline::split_clip_linked`]: the halves are
    /// re-linked by side, a partner on a locked track refuses the lot). A track whose clips all end
    /// or start at `at` already has a cut there and is passed over; a time with nothing to cut at all
    /// is an error that says why (nothing under it, it is already a cut, the clips are locked),
    /// never an empty edit.
    ///
    /// `tracks` names the tracks to cut (each must exist and be unlocked); `None` is every unlocked
    /// track, and the locked ones with a clip under `at` are reported in [`CutAll::skipped`]. Shifts
    /// nothing, so it is the same with or without ripple mode.
    pub fn split_all_tracks(&mut self, at: f64, tracks: Option<&[Uuid]>, links: bool) -> Result<CutAll> {
        if !at.is_finite() {
            return Err(Error::InvalidArgument("the cut time must be a finite number".to_string()));
        }
        let lanes = self.range_lanes(tracks)?;
        let under: Vec<Uuid> = lanes
            .iter()
            .flat_map(|&ti| self.tracks[ti].clips.iter())
            .filter(|c| strictly_inside(c, at))
            .map(|c| c.id)
            .collect();
        if under.is_empty() {
            return Err(self.nothing_to_cut(at, &lanes));
        }
        let skipped = if tracks.is_none() {
            self.locked_where(|c| strictly_inside(c, at))
        } else {
            Vec::new()
        };
        // Every clip the cut will reach, partners included, to pair the halves afterwards.
        let reached: Vec<(Uuid, Uuid)> = self
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
            .filter(|(_, c)| strictly_inside(c, at))
            .map(|(t, c)| (t.id, c.id))
            .collect();
        let before: HashSet<Uuid> = self.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();

        let mut scratch = self.clone();
        for id in under {
            // A partner cut with an earlier clip is two clips now and `at` is on their edge.
            if !scratch.clip(id).is_some_and(|c| strictly_inside(c, at)) {
                continue;
            }
            if links {
                scratch.split_clip_linked(id, at)?;
            } else {
                scratch.split_clip(id, at)?;
            }
        }

        let mut cuts = Vec::new();
        for (track_id, id) in reached {
            let Some(track) = scratch.track(track_id) else { continue };
            let Some(left) = track.clips.iter().find(|c| c.id == id) else {
                continue;
            };
            if (left.timeline_end() - at).abs() > DIFF_EPS {
                continue;
            }
            let right = track
                .clips
                .iter()
                .find(|c| !before.contains(&c.id) && (c.timeline_start - at).abs() <= DIFF_EPS && c.asset_id == left.asset_id);
            if let Some(right) = right {
                cuts.push(ClipSplit {
                    track_id,
                    left: left.clone(),
                    right: right.clone(),
                });
            }
        }
        *self = scratch;
        Ok(CutAll { at, cuts, skipped })
    }

    /// The error for a cut with nothing to cut, saying what is there instead.
    fn nothing_to_cut(&self, at: f64, lanes: &[usize]) -> Error {
        let mut why = Vec::new();
        let on_edge = lanes.iter().any(|&ti| {
            self.tracks[ti]
                .clips
                .iter()
                .any(|c| (c.timeline_start - at).abs() <= DIFF_EPS || (c.timeline_end() - at).abs() <= DIFF_EPS)
        });
        if on_edge {
            why.push("it is already a cut".to_string());
        }
        let locked = self.locked_where(|c| strictly_inside(c, at));
        if !locked.is_empty() {
            let names: Vec<&str> = locked.iter().map(|t| t.name.as_str()).collect();
            why.push(format!("the clips under it are on locked tracks ({})", names.join(", ")));
        }
        if why.is_empty() {
            why.push("no clip on an unlocked track is under that time".to_string());
        }
        Error::InvalidArgument(format!("nothing to cut at {}: {}", fmt_time(at), why.join("; ")))
    }

    /// The locked tracks that hold a clip `reaches` says yes to.
    fn locked_where(&self, reaches: impl Fn(&Clip) -> bool) -> Vec<SkippedTrack> {
        self.tracks
            .iter()
            .filter(|t| t.locked && t.clips.iter().any(&reaches))
            .map(|t| SkippedTrack {
                track_id: t.id,
                name: t.name.clone(),
            })
            .collect()
    }

    // ---- lift and extract ---------------------------------------------------

    /// **Lift** `[from, to)` off the targeted tracks and leave the gap: a clip wholly inside is
    /// removed, one the range reaches into from a side is shortened at that end, one the range sits in
    /// the middle of is cut in two around it (the right half a new clip, as a split makes it). Nothing
    /// else moves; [`Timeline::extract_range`] is the version that closes the gap.
    ///
    /// `from` before 0 is held at 0; a range that is empty, reversed, not finite or has nothing in it
    /// is an error. `tracks` as for [`Timeline::split_all_tracks`]; with `links` the linked partners
    /// of the clips the range reaches are cut with them, and a partner on a locked track refuses the
    /// lot.
    pub fn lift_range(&mut self, from: f64, to: f64, tracks: Option<&[Uuid]>, links: bool) -> Result<RangeEdit> {
        self.range_edit(from, to, tracks, false, links)
    }

    /// **Extract** `[from, to)` from the targeted tracks and close the gap: [`Timeline::lift_range`],
    /// then every clip that started at or after `to` moves up by the range's length, each track on
    /// its own — and a clip that is linked to one of them moves with it wherever it sits, the way the
    /// sync lock carries it for any ripple. Unlinked clips on a track that was not named stay where
    /// they are, and so do overlays and markers (no ripple edit moves them).
    ///
    /// A range with nothing in it still closes up when something follows it on a targeted track
    /// (deleting an empty stretch); with nothing in it and nothing after it, it is an error.
    pub fn extract_range(&mut self, from: f64, to: f64, tracks: Option<&[Uuid]>, links: bool) -> Result<RangeEdit> {
        self.range_edit(from, to, tracks, true, links)
    }

    fn range_edit(&mut self, from: f64, to: f64, tracks: Option<&[Uuid]>, close: bool, links: bool) -> Result<RangeEdit> {
        let verb = if close { "extract" } else { "lift" };
        if !from.is_finite() || !to.is_finite() {
            return Err(Error::InvalidArgument(format!("the range to {verb} must have finite ends")));
        }
        if to < from - DIFF_EPS {
            return Err(Error::InvalidArgument(format!(
                "the range to {verb} ends ({}) before it starts ({})",
                fmt_time(to),
                fmt_time(from)
            )));
        }
        let (a, b) = (from.max(0.0), to);
        if b - a <= DIFF_EPS {
            return Err(Error::InvalidArgument(if to <= 0.0 {
                format!("the range to {verb} is before the start of the timeline")
            } else {
                format!("the range to {verb} is empty")
            }));
        }
        let d = b - a;
        let lanes = self.range_lanes(tracks)?;
        let on_lane: HashSet<usize> = lanes.iter().copied().collect();

        // Where every clip stood before: `(lane, start, end)`.
        let info: HashMap<Uuid, (usize, f64, f64)> = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(ti, t)| t.clips.iter().map(move |c| (c.id, (ti, c.timeline_start, c.timeline_end()))))
            .collect();
        let overlaps = |s: f64, e: f64| s < b - DIFF_EPS && e > a + DIFF_EPS;
        let reached = |s: f64, e: f64| overlaps(s, e) || (close && s >= b - DIFF_EPS);

        // Who takes part: the clips of the targeted lanes, and the linked partners of those the range
        // reaches, wherever they are.
        let mut taking_part: HashSet<Uuid> = lanes
            .iter()
            .flat_map(|&ti| self.tracks[ti].clips.iter().map(|c| c.id))
            .collect();
        let groups = if links { self.link_groups() } else { Vec::new() };
        let mut guest_lanes: HashSet<usize> = HashSet::new();
        for (_, members) in &groups {
            let driven = members.iter().any(|m| {
                let (ti, s, e) = info[m];
                on_lane.contains(&ti) && reached(s, e)
            });
            if !driven {
                continue;
            }
            for m in members {
                let (ti, s, e) = info[m];
                if on_lane.contains(&ti) || !reached(s, e) {
                    continue;
                }
                if self.tracks[ti].locked {
                    return Err(locked_partner(&self.tracks[ti]));
                }
                taking_part.insert(*m);
                guest_lanes.insert(ti);
            }
        }
        let skipped = if tracks.is_none() {
            self.locked_where(|c| reached(c.timeline_start, c.timeline_end()))
        } else {
            Vec::new()
        };

        let mut scratch = self.clone();
        let mut fate: HashMap<Uuid, Fate> = HashMap::new();
        // The clips this edit made (the halves after the range) and the clip each was cut from.
        let mut pieces: HashMap<Uuid, Uuid> = HashMap::new();
        let mut touched: HashSet<usize> = HashSet::new();
        let (mut removed, mut trimmed, mut split, mut moved) = (0, 0, 0, 0);

        let cutting: Vec<Uuid> = scratch
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| taking_part.contains(&c.id) && overlaps(c.timeline_start, c.timeline_end()))
            .map(|c| c.id)
            .collect();
        for id in cutting {
            let (ti, ci) = scratch.locate(id).expect("collected from the timeline");
            let (start, end) = (
                scratch.tracks[ti].clips[ci].timeline_start,
                scratch.tracks[ti].clips[ci].timeline_end(),
            );
            touched.insert(ti);
            match (start < a - DIFF_EPS, end > b + DIFF_EPS) {
                (false, false) => {
                    scratch.tracks[ti].clips.remove(ci);
                    fate.insert(id, Fate::Removed);
                    removed += 1;
                }
                (true, false) => {
                    scratch.tracks[ti].clips[ci].cut_end_at(a);
                    fate.insert(id, Fate::Tail);
                    trimmed += 1;
                }
                (false, true) => {
                    scratch.tracks[ti].clips[ci].cut_start_at(b);
                    fate.insert(id, Fate::Head);
                    trimmed += 1;
                }
                (true, true) => {
                    // Two ordinary splits, the part between them dropped: the pieces are exactly
                    // what a split makes of a clip, keyframes, ramp and fades included.
                    let (_, rest) = scratch.split_clip(id, a)?;
                    let (middle, right) = scratch.split_clip(rest.id, b)?;
                    let (mti, mci) = scratch.locate(middle.id).expect("the middle piece was just made");
                    scratch.tracks[mti].clips.remove(mci);
                    fate.insert(id, Fate::Split(right.id));
                    pieces.insert(right.id, id);
                    split += 1;
                }
            }
        }

        if close {
            for (ti, track) in scratch.tracks.iter_mut().enumerate() {
                for clip in &mut track.clips {
                    let mine = taking_part.contains(&clip.id) || pieces.contains_key(&clip.id);
                    if !mine || clip.timeline_start < b - DIFF_EPS {
                        continue;
                    }
                    // What began at the range's end begins at its start, exactly.
                    clip.timeline_start = if (clip.timeline_start - b).abs() <= DIFF_EPS {
                        a
                    } else {
                        (clip.timeline_start - d).max(0.0)
                    };
                    moved += 1;
                    touched.insert(ti);
                }
            }
        }
        for &ti in &touched {
            scratch.tracks[ti].sort_by_start();
        }
        if removed + trimmed + split == 0 && moved == 0 {
            let locked: Vec<&str> = skipped.iter().map(|t| t.name.as_str()).collect();
            let and = if locked.is_empty() {
                String::new()
            } else {
                format!(" ({} locked)", locked.join(", "))
            };
            return Err(Error::InvalidArgument(format!(
                "nothing to {verb} between {} and {}: {}{and}",
                fmt_time(a),
                fmt_time(b),
                if close {
                    "no clip is in that range and none follows it on a targeted track"
                } else {
                    "no clip on a targeted track is in that range"
                }
            )));
        }

        // A linked clip that was not on a named track may not have been moved onto a clip that took
        // no part (the sync lock's own rule for a follower).
        let part = |id: &Uuid| taking_part.contains(id) || pieces.contains_key(id);
        for &ti in &guest_lanes {
            let lane = &scratch.tracks[ti];
            for m in lane.clips.iter().filter(|c| part(&c.id)) {
                for q in lane.clips.iter().filter(|c| !part(&c.id)) {
                    if !spans_overlap((m.timeline_start, m.timeline_end()), (q.timeline_start, q.timeline_end())) {
                        continue;
                    }
                    let was = |id: Uuid| info.get(pieces.get(&id).unwrap_or(&id)).map(|(_, s, e)| (*s, *e));
                    if matches!((was(m.id), was(q.id)), (Some(x), Some(y)) if spans_overlap(x, y)) {
                        continue;
                    }
                    return Err(runs_into_unlinked(&lane.name, m.timeline_start.max(q.timeline_start)));
                }
            }
        }

        // A group the range cut in two falls in two by side, as for a split: what is before the
        // range keeps the link, what is after it gets a new one.
        for (link, members) in &groups {
            if !members.iter().any(|m| fate.contains_key(m)) {
                continue;
            }
            let (mut lefts, mut rights) = (Vec::new(), Vec::new());
            for m in members {
                match fate.get(m) {
                    Some(Fate::Removed) => {}
                    Some(Fate::Tail) => lefts.push(*m),
                    Some(Fate::Head) => rights.push(*m),
                    Some(Fate::Split(right)) => {
                        lefts.push(*m);
                        rights.push(*right);
                    }
                    None => {
                        let (_, s, _) = info[m];
                        if s >= b - DIFF_EPS {
                            rights.push(*m);
                        } else {
                            lefts.push(*m);
                        }
                    }
                }
            }
            scratch.relink_sides(Some(*link), &lefts, &rights);
        }

        let tracks_changed = (0..scratch.tracks.len())
            .filter(|ti| touched.contains(ti))
            .map(|ti| scratch.tracks[ti].id)
            .collect();
        *self = scratch;
        Ok(RangeEdit {
            from: a,
            to: b,
            removed,
            trimmed,
            split,
            moved,
            closed: if close { d } else { 0.0 },
            tracks: tracks_changed,
            skipped,
        })
    }

    // ---- close a gap ----------------------------------------------------------

    /// **Ripple delete a gap**: the empty stretch of track `track_id` that `at` is in — from the end
    /// of the clip before it (0 at the start of the track) to the start of the clip after it — is
    /// closed by moving every clip from there on up by its length. A clip on another track is not
    /// touched by this (the sync lock of `Project` carries the linked partners of what moved, as for
    /// any ripple). `at` on the edge of a gap is in it.
    ///
    /// Refuses a locked track, a time with a clip under it, a time with no gap (clips touching) and
    /// a gap with nothing after it (there is nothing to close up).
    pub fn ripple_delete_gap(&mut self, track_id: Uuid, at: f64) -> Result<GapClosed> {
        if !at.is_finite() {
            return Err(Error::InvalidArgument("the gap's time must be a finite number".to_string()));
        }
        self.ensure_track_editable(track_id)?;
        let track = self.track(track_id).ok_or(Error::TrackNotFound(track_id))?;
        if track.clips.iter().any(|c| strictly_inside(c, at)) {
            return Err(Error::InvalidArgument(format!(
                "there is a clip on {} at {}, not a gap",
                track.name,
                fmt_time(at)
            )));
        }
        let lo = track
            .clips
            .iter()
            .filter(|c| c.timeline_end() <= at + DIFF_EPS)
            .map(Clip::timeline_end)
            .fold(0.0, f64::max);
        let hi = track
            .clips
            .iter()
            .filter(|c| c.timeline_start >= at - DIFF_EPS)
            .map(|c| c.timeline_start)
            .min_by(f64::total_cmp);
        let Some(hi) = hi else {
            return Err(Error::InvalidArgument(format!(
                "there is nothing after {} on {} to close up",
                fmt_time(at),
                track.name
            )));
        };
        if hi - lo <= DIFF_EPS {
            return Err(Error::InvalidArgument(format!(
                "there is no gap on {} at {}",
                track.name,
                fmt_time(at)
            )));
        }
        let track = self.track_mut(track_id).expect("looked up above");
        let mut moved = 0;
        for clip in &mut track.clips {
            if clip.timeline_start < hi - DIFF_EPS {
                continue;
            }
            clip.timeline_start = if (clip.timeline_start - hi).abs() <= DIFF_EPS {
                lo
            } else {
                (clip.timeline_start - (hi - lo)).max(0.0)
            };
            moved += 1;
        }
        track.sort_by_start();
        Ok(GapClosed {
            track_id,
            from: lo,
            to: hi,
            moved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(asset: Uuid, source_in: f64, source_out: f64, at: f64) -> Clip {
        Clip::new(asset, source_in, source_out, at)
    }

    fn lane(kind: StreamKind, name: &str, clips: Vec<Clip>) -> Track {
        Track {
            clips,
            ..Track::new(kind, name)
        }
    }

    fn timeline(tracks: Vec<Track>) -> Timeline {
        Timeline {
            tracks,
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
            thumbnails: Vec::new(),
        }
    }

    const V: StreamKind = StreamKind::Video;
    const A: StreamKind = StreamKind::Audio;

    fn span(c: &Clip) -> (f64, f64) {
        (c.timeline_start, c.timeline_end())
    }

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} is not {b}");
    }

    fn assert_spans(t: &Timeline, track: usize, want: &[(f64, f64)]) {
        let got: Vec<(f64, f64)> = t.tracks[track].clips.iter().map(span).collect();
        assert_eq!(
            got.len(),
            want.len(),
            "{} holds {got:?}, wanted {want:?}",
            t.tracks[track].name
        );
        for (g, w) in got.iter().zip(want) {
            near(g.0, w.0);
            near(g.1, w.1);
        }
    }

    fn refused<T: std::fmt::Debug>(r: Result<T>) -> String {
        r.unwrap_err().to_string()
    }

    fn same(a: &Timeline, b: &Timeline) -> bool {
        serde_json::to_string(a).unwrap() == serde_json::to_string(b).unwrap()
    }

    fn id(t: &Timeline, track: usize, clip: usize) -> Uuid {
        t.tracks[track].clips[clip].id
    }

    fn get(t: &Timeline, id: Uuid) -> &Clip {
        t.clip(id).expect("the clip is on the timeline")
    }

    /// Where `source_in` would sit if the clip played at 1x from the timeline's 0: equal for two
    /// forward clips of one asset exactly when they are in step.
    fn offset(c: &Clip) -> f64 {
        c.timeline_start - c.source_in
    }

    fn pair_of(asset: Uuid, span: (f64, f64)) -> (Timeline, Uuid, Uuid) {
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, span.0, span.1, span.0)]),
            lane(A, "A1", vec![clip(asset, span.0, span.1, span.0)]),
        ]);
        let (c, a) = (id(&t, 0, 0), id(&t, 1, 0));
        t.link_clips(&[c, a]).unwrap();
        (t, c, a)
    }

    // ---- cut every track --------------------------------------------------------

    #[test]
    fn every_unlocked_track_with_a_clip_under_the_time_is_cut_in_one_call() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(V, "V2", vec![clip(asset, 0.0, 6.0, 2.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(A, "A2", vec![clip(asset, 0.0, 3.0, 0.0)]),
        ]);
        let done = t.split_all_tracks(5.0, None, true).unwrap();
        assert_eq!(done.cuts.len(), 3);
        assert!(done.skipped.is_empty());
        assert_spans(&t, 0, &[(0.0, 5.0), (5.0, 10.0)]);
        assert_spans(&t, 1, &[(2.0, 5.0), (5.0, 8.0)]);
        assert_spans(&t, 2, &[(0.0, 5.0), (5.0, 10.0)]);
        assert_spans(&t, 3, &[(0.0, 3.0)]);
        for cut in &done.cuts {
            near(cut.left.timeline_end(), 5.0);
            near(cut.right.timeline_start, 5.0);
            assert_ne!(cut.left.id, cut.right.id);
        }
        let tracks: Vec<Uuid> = done.cuts.iter().map(|c| c.track_id).collect();
        assert_eq!(tracks, vec![t.tracks[0].id, t.tracks[1].id, t.tracks[2].id], "in track order");
    }

    #[test]
    fn a_time_on_an_existing_cut_is_no_cut_for_that_track() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 5.0, 10.0, 5.0)]),
            lane(V, "V2", vec![clip(asset, 0.0, 10.0, 0.0)]),
        ]);
        let done = t.split_all_tracks(5.0, None, true).unwrap();
        assert_eq!(done.cuts.len(), 1);
        assert_eq!(t.tracks[0].clips.len(), 2, "V1 already had its cut there");
        assert_spans(&t, 1, &[(0.0, 5.0), (5.0, 10.0)]);
    }

    #[test]
    fn nothing_to_cut_is_a_refusal_that_says_why_and_changes_nothing() {
        let asset = Uuid::new_v4();
        let mut locked = lane(A, "A1", vec![clip(asset, 0.0, 10.0, 0.0)]);
        locked.locked = true;
        let t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 5.0, 10.0, 5.0)]),
            locked,
        ]);
        let ask = |at: f64| {
            let mut copy = t.clone();
            let why = refused(copy.split_all_tracks(at, None, true));
            assert!(same(&copy, &t), "a refusal leaves the timeline as it was");
            why
        };
        let on_a_cut = ask(5.0);
        assert!(on_a_cut.contains("already a cut"), "{on_a_cut}");
        assert!(on_a_cut.contains("locked tracks (A1)"), "{on_a_cut}");
        assert!(ask(20.0).contains("no clip on an unlocked track is under that time"));
        assert!(ask(-3.0).contains("no clip on an unlocked track"));
        assert!(ask(0.0).contains("already a cut"));
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(ask(bad).contains("finite"));
        }

        let mut empty = timeline(Vec::new());
        assert!(refused(empty.split_all_tracks(1.0, None, true)).contains("no unlocked track"));
        let mut blank = timeline(vec![lane(V, "V1", Vec::new()), lane(A, "A1", Vec::new())]);
        assert!(refused(blank.split_all_tracks(1.0, None, true)).contains("no clip on an unlocked track"));
    }

    #[test]
    fn locked_tracks_are_left_alone_and_reported() {
        let asset = Uuid::new_v4();
        let mut a1 = lane(A, "A1", vec![clip(asset, 0.0, 10.0, 0.0)]);
        a1.locked = true;
        let mut far = lane(A, "A2", vec![clip(asset, 0.0, 2.0, 20.0)]);
        far.locked = true;
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]), a1, far]);
        let done = t.split_all_tracks(4.0, None, true).unwrap();
        assert_eq!(done.cuts.len(), 1);
        assert_eq!(
            done.skipped,
            vec![SkippedTrack {
                track_id: t.tracks[1].id,
                name: "A1".into()
            }],
            "only a locked track with a clip under the time is reported"
        );
        assert_spans(&t, 1, &[(0.0, 10.0)]);
    }

    #[test]
    fn named_tracks_limit_the_cut_and_must_be_unlocked() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(V, "V2", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 10.0, 0.0)]),
        ]);
        let v2 = t.tracks[1].id;
        let done = t.split_all_tracks(4.0, Some(&[v2, v2]), true).unwrap();
        assert_eq!(done.cuts.len(), 1, "a track named twice is cut once");
        assert_eq!(
            (t.tracks[0].clips.len(), t.tracks[1].clips.len(), t.tracks[2].clips.len()),
            (1, 2, 1)
        );

        let before = t.clone();
        assert!(matches!(
            t.split_all_tracks(4.0, Some(&[Uuid::new_v4()]), true),
            Err(Error::TrackNotFound(_))
        ));
        assert!(refused(t.split_all_tracks(4.0, Some(&[]), true)).contains("name at least one track"));
        t.tracks[0].locked = true;
        let v1 = t.tracks[0].id;
        assert!(refused(t.split_all_tracks(2.0, Some(&[v1]), true)).contains("track V1 is locked"));
        t.tracks[0].locked = false;
        assert!(same(&t, &before));
    }

    #[test]
    fn linked_partners_are_cut_with_the_clip_and_the_halves_linked_by_side() {
        let (t0, x, s) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        let group = get(&t0, x).link_id;
        for tracks in [None, Some(vec![t0.tracks[0].id])] {
            let mut t = t0.clone();
            let done = t.split_all_tracks(4.0, tracks.as_deref(), true).unwrap();
            assert_eq!(done.cuts.len(), 2, "the picture and its sound, whichever was asked for");
            assert_eq!(get(&t, x).link_id, group);
            assert_eq!(get(&t, s).link_id, group, "the left halves keep the group");
            let (rx, rs) = (id(&t, 0, 1), id(&t, 1, 1));
            let fresh = get(&t, rx).link_id;
            assert!(fresh.is_some() && fresh != group);
            assert_eq!(get(&t, rs).link_id, fresh, "the right halves are a group of their own");
        }
        let mut alone = t0.clone();
        alone.split_all_tracks(4.0, Some(&[t0.tracks[0].id]), false).unwrap();
        assert_eq!(
            (alone.tracks[0].clips.len(), alone.tracks[1].clips.len()),
            (2, 1),
            "link off cuts what is named"
        );
    }

    #[test]
    fn a_partner_on_a_locked_track_refuses_the_cut_and_changes_nothing() {
        let (mut t, _, _) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        t.tracks[1].locked = true;
        let before = t.clone();
        let why = refused(t.split_all_tracks(4.0, None, true));
        assert!(why.contains("locked track A1"), "{why}");
        assert!(same(&t, &before));
        // With links off the picture is cut alone.
        t.split_all_tracks(4.0, None, false).unwrap();
        assert_eq!((t.tracks[0].clips.len(), t.tracks[1].clips.len()), (2, 1));
    }

    fn keyed(at: f64, source: (f64, f64)) -> Clip {
        let mut c = clip(Uuid::new_v4(), source.0, source.1, at);
        c.keyframes = [(0.0, 1.0), (3.0, 2.0), (6.0, 0.5)]
            .iter()
            .map(|&(time, scale)| Keyframe {
                time,
                scale,
                pos_x: 0.1 * time,
                pos_y: 0.0,
                rotation: 0.0,
                opacity: 1.0,
                easing: Default::default(),
            })
            .collect();
        c
    }

    /// One clip of every kind the cut has to keep honest.
    fn flavours() -> Vec<(&'static str, Clip)> {
        let asset = Uuid::new_v4();
        let mut out = vec![("plain", clip(asset, 2.0, 12.0, 1.0))];
        let mut fast = clip(asset, 2.0, 12.0, 1.0);
        fast.speed = 2.0;
        out.push(("2x", fast));
        let mut slow = clip(asset, 2.0, 12.0, 1.0);
        slow.speed = 0.5;
        out.push(("0.5x", slow));
        let mut back = clip(asset, 2.0, 12.0, 1.0);
        back.speed = -1.0;
        out.push(("reversed", back));
        let mut back2 = clip(asset, 2.0, 12.0, 1.0);
        back2.speed = -2.0;
        out.push(("reversed 2x", back2));
        let mut ramp = clip(asset, 2.0, 12.0, 1.0);
        ramp.speed_ramp = vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(4.0, 3.0), SpeedKey::new(6.0, 0.5)];
        out.push(("ramped", ramp.clone()));
        ramp.speed = -1.0;
        out.push(("ramped reversed", ramp));
        out.push(("keyed", keyed(1.0, (2.0, 12.0))));
        let mut both = keyed(1.0, (2.0, 12.0));
        both.speed_ramp = vec![SpeedKey::new(0.0, 2.0), SpeedKey::new(2.0, 1.0)];
        out.push(("keyed and ramped", both));
        let mut dressed = clip(asset, 2.0, 12.0, 1.0);
        dressed.fade_in = 1.0;
        dressed.fade_out = 1.5;
        dressed.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        out.push(("faded", dressed));
        out.push((
            "title",
            Clip::title(
                TitleClip {
                    name: "T".into(),
                    svg: "<svg/>".into(),
                    template: None,
                },
                1.0,
                9.0,
            ),
        ));
        out
    }

    #[test]
    fn a_cut_makes_the_halves_a_split_makes() {
        for (name, c) in flavours() {
            let at = c.timeline_start + c.duration() * 0.37;
            let mut t = timeline(vec![lane(V, "V1", vec![c.clone()])]);
            let done = t.split_all_tracks(at, None, true).unwrap();
            let mut plain = timeline(vec![lane(V, "V1", vec![c.clone()])]);
            let (left, mut right) = plain.split_clip(c.id, at).unwrap();
            let mut cut = done.cuts[0].clone();
            right.id = Uuid::nil();
            cut.right.id = Uuid::nil();
            let wire = |c: &Clip| serde_json::to_string(c).unwrap();
            assert_eq!(wire(&cut.left), wire(&left), "{name}: the left half");
            assert_eq!(wire(&cut.right), wire(&right), "{name}: the right half");
        }
    }

    // ---- lift and extract ---------------------------------------------------------

    #[test]
    fn a_lift_removes_the_range_and_leaves_the_gap() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![lane(
            V,
            "V1",
            vec![
                clip(asset, 0.0, 4.0, 0.0),
                clip(asset, 10.0, 14.0, 4.0),
                clip(asset, 20.0, 24.0, 8.0),
            ],
        )]);
        let (a, b, c) = (id(&t, 0, 0), id(&t, 0, 1), id(&t, 0, 2));
        let done = t.lift_range(3.0, 9.0, None, true).unwrap();
        assert_spans(&t, 0, &[(0.0, 3.0), (9.0, 12.0)]);
        assert_eq!((done.removed, done.trimmed, done.split, done.moved), (1, 2, 0, 0));
        assert_eq!((done.from, done.to, done.closed), (3.0, 9.0, 0.0));
        assert_eq!(done.clips(), 3);
        assert_eq!(done.tracks, vec![t.tracks[0].id]);
        assert!(t.clip(b).is_none(), "the clip inside the range is gone");
        let (kept_a, kept_c) = (get(&t, a), get(&t, c));
        assert_eq!((kept_a.source_in, kept_a.source_out), (0.0, 3.0), "the tail was cut off");
        assert_eq!(
            (kept_c.source_in, kept_c.source_out),
            (21.0, 24.0),
            "the head was cut off, the footage kept"
        );
    }

    #[test]
    fn a_range_inside_one_clip_cuts_it_in_two_around_it() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 5.0, 15.0, 0.0)])]);
        let whole = id(&t, 0, 0);
        let done = t.lift_range(3.0, 6.0, None, true).unwrap();
        assert_eq!((done.removed, done.trimmed, done.split), (0, 0, 1));
        assert_spans(&t, 0, &[(0.0, 3.0), (6.0, 10.0)]);
        let (left, right) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
        assert_eq!(left.id, whole, "the left half keeps the clip's identity");
        assert_ne!(right.id, whole);
        assert_eq!((left.source_in, left.source_out), (5.0, 8.0));
        assert_eq!((right.source_in, right.source_out), (11.0, 15.0));
    }

    #[test]
    fn an_extract_closes_the_gap_on_every_track_and_welds_the_cut() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                V,
                "V1",
                vec![
                    clip(asset, 0.0, 4.0, 0.0),
                    clip(asset, 10.0, 14.0, 4.0),
                    clip(asset, 20.0, 24.0, 8.0),
                ],
            ),
            lane(A, "A1", vec![clip(asset, 0.0, 12.0, 0.0)]),
        ]);
        let done = t.extract_range(3.0, 6.0, None, true).unwrap();
        assert_spans(&t, 0, &[(0.0, 3.0), (3.0, 5.0), (5.0, 9.0)]);
        assert_spans(&t, 1, &[(0.0, 3.0), (3.0, 9.0)]);
        assert_eq!(
            t.tracks[0].clips[1].timeline_start, 3.0,
            "what began at the range's end begins at its start, exactly"
        );
        assert_eq!(t.tracks[1].clips[1].timeline_start, 3.0);
        assert_eq!((done.removed, done.trimmed, done.split, done.moved), (0, 2, 1, 3), "{done:?}");
        assert_eq!(done.closed, 3.0);
        assert_eq!(done.tracks, vec![t.tracks[0].id, t.tracks[1].id]);
    }

    #[test]
    fn extract_welds_on_numbers_that_do_not_subtract_cleanly() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![lane(
            V,
            "V1",
            vec![clip(asset, 0.0, 3.0, 0.0), clip(asset, 0.0, 1.0, 3.0)],
        )]);
        // 1.3 - (1.3 - 0.1) is 0.10000000000000009, the weld is 0.1.
        t.extract_range(0.1, 1.3, None, true).unwrap();
        assert_eq!(t.tracks[0].clips[1].timeline_start, 0.1);
        near(t.tracks[0].clips[1].timeline_end(), 1.8);
        near(t.tracks[0].clips[2].timeline_start, 1.8);
    }

    #[test]
    fn an_empty_stretch_has_nothing_to_lift_but_can_be_closed_when_something_follows() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![lane(
            V,
            "V1",
            vec![clip(asset, 0.0, 2.0, 0.0), clip(asset, 0.0, 2.0, 5.0)],
        )]);
        let before = t.clone();
        assert!(refused(t.lift_range(2.5, 4.5, None, true)).contains("no clip on a targeted track"));
        assert!(same(&t, &before));
        let done = t.extract_range(2.0, 5.0, None, true).unwrap();
        assert_eq!((done.clips(), done.moved, done.closed), (0, 1, 3.0));
        assert_spans(&t, 0, &[(0.0, 2.0), (2.0, 4.0)]);
        let why = refused(t.extract_range(10.0, 12.0, None, true));
        assert!(why.contains("none follows it"), "{why}");
    }

    #[test]
    fn hostile_ranges_are_refused_and_change_nothing() {
        let asset = Uuid::new_v4();
        let t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
        let bad: [(f64, f64, &str); 10] = [
            (f64::NAN, 1.0, "finite"),
            (1.0, f64::NAN, "finite"),
            (f64::INFINITY, 2.0, "finite"),
            (1.0, f64::INFINITY, "finite"),
            (f64::NEG_INFINITY, 1.0, "finite"),
            (5.0, 2.0, "before it starts"),
            (2.0, 2.0, "empty"),
            (2.0, 2.0 + 1e-9, "empty"),
            (-5.0, -1.0, "before the start of the timeline"),
            (-5.0, 0.0, "before the start of the timeline"),
        ];
        for (from, to, why) in bad {
            for close in [false, true] {
                let mut copy = t.clone();
                let got = refused(if close {
                    copy.extract_range(from, to, None, true)
                } else {
                    copy.lift_range(from, to, None, true)
                });
                assert!(got.contains(why), "({from}, {to}) -> {got}");
                assert!(same(&copy, &t));
            }
        }
        // A start before 0 is held at 0; an end far past everything lifts to the end.
        let mut copy = t.clone();
        let done = copy.lift_range(-5.0, 3.0, None, true).unwrap();
        assert_eq!(done.from, 0.0);
        assert_spans(&copy, 0, &[(3.0, 10.0)]);
        let mut copy = t;
        copy.extract_range(4.0, 1e300, None, true).unwrap();
        assert_spans(&copy, 0, &[(0.0, 4.0)]);
        // Nothing at all, and no tracks.
        let mut blank = timeline(vec![lane(V, "V1", Vec::new())]);
        assert!(blank.lift_range(0.0, 5.0, None, true).is_err());
        assert!(blank.extract_range(0.0, 5.0, None, true).is_err());
        let mut none = timeline(Vec::new());
        assert!(refused(none.extract_range(0.0, 5.0, None, true)).contains("no unlocked track"));
    }

    #[test]
    fn locked_tracks_are_skipped_when_not_named_and_refused_when_named() {
        let asset = Uuid::new_v4();
        let mut v2 = lane(V, "V2", vec![clip(asset, 0.0, 10.0, 0.0)]);
        v2.locked = true;
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]), v2]);
        let locked = t.tracks[1].id;
        let before = t.clone();
        assert!(refused(t.lift_range(2.0, 4.0, Some(&[locked]), true)).contains("track V2 is locked"));
        assert!(matches!(
            t.lift_range(2.0, 4.0, Some(&[Uuid::new_v4()]), true),
            Err(Error::TrackNotFound(_))
        ));
        assert!(same(&t, &before));
        let done = t.extract_range(2.0, 4.0, None, true).unwrap();
        assert_spans(&t, 0, &[(0.0, 2.0), (2.0, 8.0)]);
        assert_spans(&t, 1, &[(0.0, 10.0)]);
        assert_eq!(done.skipped.len(), 1);
        assert_eq!(done.skipped[0].name, "V2");
        // Everything locked: nothing to edit.
        t.tracks[0].locked = true;
        assert!(refused(t.lift_range(2.0, 4.0, None, true)).contains("no unlocked track"));
    }

    #[test]
    fn a_partner_on_a_locked_track_refuses_a_range_it_reaches_and_not_one_it_misses() {
        let (mut t, x, _) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        t.tracks[1].locked = true;
        let before = t.clone();
        for close in [false, true] {
            let why = refused(if close {
                t.extract_range(4.0, 6.0, None, true)
            } else {
                t.lift_range(4.0, 6.0, None, true)
            });
            assert!(why.contains("locked track A1"), "{why}");
            assert!(same(&t, &before));
        }
        // With links off the picture alone is cut.
        t.lift_range(4.0, 6.0, None, false).unwrap();
        assert_spans(&t, 0, &[(0.0, 4.0), (6.0, 10.0)]);
        assert_spans(&t, 1, &[(0.0, 10.0)]);
        assert_eq!(
            get(&t, x).link_id,
            before.clip(x).unwrap().link_id,
            "the left half stays linked"
        );

        // A sound that ends before the range is not reached by it.
        let (mut t, _, s) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        let (ti, ci) = t.locate(s).unwrap();
        t.tracks[ti].clips[ci].source_out = 3.0;
        t.tracks[1].locked = true;
        t.lift_range(4.0, 6.0, None, true).unwrap();
        assert_spans(&t, 1, &[(0.0, 3.0)]);
    }

    #[test]
    fn a_linked_group_falls_in_two_by_side_and_stays_in_step() {
        let (t0, x, s) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        let group = get(&t0, x).link_id;
        for close in [false, true] {
            let mut t = t0.clone();
            if close {
                t.extract_range(4.0, 6.0, None, true).unwrap();
            } else {
                t.lift_range(4.0, 6.0, None, true).unwrap();
            }
            assert_eq!(get(&t, x).link_id, group);
            assert_eq!(get(&t, s).link_id, group);
            let (rx, rs) = (&t.tracks[0].clips[1], &t.tracks[1].clips[1]);
            assert!(rx.link_id.is_some() && rx.link_id != group && rx.link_id == rs.link_id);
            near(offset(rx), offset(rs));
            near(offset(rx), if close { -2.0 } else { 0.0 });
            assert_eq!(rx.timeline_start, if close { 4.0 } else { 6.0 });
            assert!(t.first_sync_break(&t0).is_none());
        }
    }

    #[test]
    fn a_j_cut_pair_is_cut_and_moved_in_step() {
        // The sound leads the picture by two seconds: same footage, same offset, longer.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 2.0, 12.0, 2.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 12.0, 0.0)]),
        ]);
        let (x, s) = (id(&t, 0, 0), id(&t, 1, 0));
        t.link_clips(&[x, s]).unwrap();
        let before = t.clone();
        t.extract_range(5.0, 7.0, None, true).unwrap();
        assert_spans(&t, 0, &[(2.0, 5.0), (5.0, 10.0)]);
        assert_spans(&t, 1, &[(0.0, 5.0), (5.0, 10.0)]);
        near(offset(&t.tracks[0].clips[1]), offset(&t.tracks[1].clips[1]));
        assert!(t.first_sync_break(&before).is_none());
        let mut conformed = t.clone();
        conformed.conform_links(&before, &HashSet::new(), &HashMap::new()).unwrap();
        assert!(same(&conformed, &t), "the sync lock finds nothing left to put right");
    }

    #[test]
    fn a_partner_on_a_track_that_was_not_named_is_cut_and_moved_with_its_clip() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0), clip(asset, 0.0, 10.0, 10.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 10.0, 0.0), clip(asset, 0.0, 5.0, 20.0)]),
        ]);
        let (x, s, music) = (id(&t, 0, 0), id(&t, 1, 0), id(&t, 1, 1));
        t.link_clips(&[x, s]).unwrap();
        let v1 = t.tracks[0].id;
        let done = t.extract_range(4.0, 6.0, Some(&[v1]), true).unwrap();
        assert_spans(&t, 0, &[(0.0, 4.0), (4.0, 8.0), (8.0, 18.0)]);
        assert_spans(&t, 1, &[(0.0, 4.0), (4.0, 8.0), (20.0, 25.0)]);
        assert_eq!(
            get(&t, music).timeline_start,
            20.0,
            "an unlinked clip of a track nobody named stays"
        );
        assert_eq!(done.tracks.len(), 2, "the sound's track changed too");
        assert_eq!(done.moved, 3);
    }

    #[test]
    fn a_moved_sound_that_would_land_on_a_clip_it_is_not_linked_to_refuses() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 3.0, 2.0), clip(asset, 6.0, 10.0, 6.0)]),
        ]);
        let (x, s) = (id(&t, 0, 0), id(&t, 1, 1));
        t.link_clips(&[x, s]).unwrap();
        let before = t.clone();
        let v1 = t.tracks[0].id;
        let why = refused(t.extract_range(4.0, 6.0, Some(&[v1]), true));
        assert!(why.contains("A1") && why.contains("not linked to it"), "{why}");
        assert!(same(&t, &before));
        // Lifting leaves everything where it is, so nothing lands on anything.
        t.lift_range(4.0, 6.0, Some(&[v1]), true).unwrap();
    }

    /// For every moment a clip showed outside the range, the piece that shows it afterwards plays the
    /// same footage with the same animation (at the moment it moved to).
    fn assert_frames_kept(name: &str, whole: &Clip, a: f64, b: f64) {
        let (start, end) = span(whole);
        for close in [false, true] {
            let mut t = timeline(vec![lane(V, "V1", vec![whole.clone()])]);
            if close {
                t.extract_range(a, b, None, true).unwrap();
            } else {
                t.lift_range(a, b, None, true).unwrap();
            }
            for i in 0..400 {
                let time = start + (end - start) * (f64::from(i) + 0.5) / 400.0;
                if time > a - 1e-6 && time < b + 1e-6 {
                    continue;
                }
                let now = if close && time >= b { time - (b - a) } else { time };
                let piece = t.tracks[0]
                    .clips
                    .iter()
                    .find(|c| c.timeline_start - 1e-9 <= now && now < c.timeline_end())
                    .unwrap_or_else(|| panic!("{name} [{a}, {b}) close={close}: nothing shows {time}"));
                let local = now - piece.timeline_start;
                let (was, has) = (whole.transform_at(time - start), piece.transform_at(local));
                assert!(
                    (piece.timeline_to_source(now) - whole.timeline_to_source(time)).abs() < 1e-6,
                    "{name} [{a}, {b}) close={close} at {time}: footage {} vs {}",
                    piece.timeline_to_source(now),
                    whole.timeline_to_source(time)
                );
                for (x, y, what) in [
                    (was.scale, has.scale, "scale"),
                    (was.pos_x, has.pos_x, "pos_x"),
                    (was.opacity, has.opacity, "opacity"),
                ] {
                    assert!(
                        (x - y).abs() < 1e-6,
                        "{name} [{a}, {b}) close={close} at {time}: {what} {x} vs {y}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_frame_outside_the_range_stays_on_its_footage_and_its_keys() {
        for (name, c) in flavours() {
            let (s, d) = (c.timeline_start, c.duration());
            let ranges = [
                (s + 0.3 * d, s + 0.6 * d),        // in the middle: cut in two
                (s - 0.5, s + 0.3 * d),            // from before the clip: the head goes
                (s + 0.6 * d, s + d + 0.5),        // to past its end: the tail goes
                (s + 0.2 * d, s + 0.2 * d + 0.07), // a sliver
            ];
            for (a, b) in ranges {
                assert_frames_kept(name, &c, a, b);
            }
        }
    }

    #[test]
    fn a_title_on_a_track_is_cut_like_any_clip() {
        let title = Clip::title(
            TitleClip {
                name: "Card".into(),
                svg: "<svg/>".into(),
                template: None,
            },
            0.0,
            6.0,
        );
        let mut t = timeline(vec![lane(V, "V1", vec![title])]);
        t.lift_range(2.0, 4.0, None, true).unwrap();
        assert_spans(&t, 0, &[(0.0, 2.0), (4.0, 6.0)]);
        let right = &t.tracks[0].clips[1];
        assert!(right.is_generated() && t.tracks[0].clips[0].is_generated());
        assert_eq!(
            (right.source_in, right.source_out),
            (4.0, 6.0),
            "its document runs on from where it was"
        );
    }

    #[test]
    fn fades_and_transitions_go_with_the_half_that_keeps_their_edge() {
        let asset = Uuid::new_v4();
        let crossfade = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 1.0,
        });
        let build = || {
            let mut a = clip(asset, 0.0, 4.0, 0.0);
            a.fade_out = 0.5;
            let mut b = clip(asset, 0.0, 4.0, 4.0);
            b.transition_in = crossfade;
            b.fade_in = 0.5;
            b.fade_out = 0.5;
            let mut c = clip(asset, 0.0, 4.0, 8.0);
            c.transition_in = crossfade;
            timeline(vec![lane(V, "V1", vec![a, b, c])])
        };
        // The range sits inside B: the left half keeps B's transition, the right half has none.
        let mut t = build();
        let b = id(&t, 0, 1);
        t.lift_range(5.0, 7.0, None, true).unwrap();
        let (l, r) = (&t.tracks[0].clips[1], &t.tracks[0].clips[2]);
        assert_eq!(l.id, b);
        assert_eq!((l.transition_in, l.fade_in, l.fade_out), (crossfade, 0.5, 0.0));
        assert_eq!((r.transition_in, r.fade_in, r.fade_out), (None, 0.0, 0.5));
        // A range that ends B's head: its new edge has no transition and no fade.
        let mut t = build();
        t.lift_range(3.0, 5.0, None, true).unwrap();
        let (a, b) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
        assert_eq!((a.fade_out, b.transition_in, b.fade_in, b.fade_out), (0.0, None, 0.0, 0.5));
        // A range ending exactly at C's start leaves C's own transition alone.
        let mut t = build();
        t.lift_range(6.0, 8.0, None, true).unwrap();
        assert_eq!(t.tracks[0].clips[1].transition_in, crossfade, "B's own start is untouched");
        assert_eq!(t.tracks[0].clips[2].transition_in, crossfade);
        // A fade longer than what is left of its clip is held inside it.
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
        t.tracks[0].clips[0].fade_in = 3.0;
        t.lift_range(2.0, 10.0, None, true).unwrap();
        assert_eq!(t.tracks[0].clips[0].fade_in, 2.0);
    }

    #[test]
    fn overlays_and_markers_are_not_moved_by_an_extract() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 20.0, 0.0)])]);
        t.overlays.push(TextOverlay::new("Title", 12.0, 14.0));
        t.markers.push(Marker {
            id: Uuid::new_v4(),
            time: 15.0,
            name: "m".into(),
            color: None,
            duration: 0.0,
            comment: String::new(),
            kind: MarkerKind::Comment,
        });
        t.extract_range(2.0, 6.0, None, true).unwrap();
        assert_eq!(
            (t.overlays[0].start, t.overlays[0].end, t.markers[0].time),
            (12.0, 14.0, 15.0)
        );
    }

    // ---- delete a gap ---------------------------------------------------------------

    fn gappy() -> (Timeline, Uuid) {
        let asset = Uuid::new_v4();
        let t = timeline(vec![lane(
            V,
            "V1",
            vec![
                clip(asset, 0.0, 2.0, 0.0),
                clip(asset, 0.0, 2.0, 5.0),
                clip(asset, 0.0, 1.0, 9.0),
            ],
        )]);
        let v1 = t.tracks[0].id;
        (t, v1)
    }

    #[test]
    fn a_gap_is_closed_by_moving_everything_after_it_up() {
        for at in [3.0, 2.0, 5.0, 2.0 + 1e-9, 4.999] {
            let (mut t, v1) = gappy();
            let done = t.ripple_delete_gap(v1, at).unwrap();
            assert_spans(&t, 0, &[(0.0, 2.0), (2.0, 4.0), (6.0, 7.0)]);
            assert_eq!((done.from, done.to, done.moved), (2.0, 5.0, 2));
            assert_eq!(
                t.tracks[0].clips[1].timeline_start, 2.0,
                "the next clip starts where the gap did, exactly"
            );
        }
    }

    #[test]
    fn a_gap_at_the_start_of_a_track_closes_to_zero() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 2.0, 3.0)])]);
        let v1 = t.tracks[0].id;
        let done = t.ripple_delete_gap(v1, 1.0).unwrap();
        assert_eq!((done.from, done.to), (0.0, 3.0));
        assert_spans(&t, 0, &[(0.0, 2.0)]);
    }

    #[test]
    fn a_time_that_is_not_in_a_gap_is_refused() {
        let (t, v1) = gappy();
        let before = t.clone();
        let mut copy = t.clone();
        assert!(refused(copy.ripple_delete_gap(v1, 1.0)).contains("not a gap"));
        assert!(refused(copy.ripple_delete_gap(v1, 9.5)).contains("not a gap"));
        assert!(refused(copy.ripple_delete_gap(v1, 11.0)).contains("nothing after"));
        assert!(refused(copy.ripple_delete_gap(v1, 100.0)).contains("nothing after"));
        for bad in [f64::NAN, f64::INFINITY] {
            assert!(refused(copy.ripple_delete_gap(v1, bad)).contains("finite"));
        }
        assert!(matches!(
            copy.ripple_delete_gap(Uuid::new_v4(), 3.0),
            Err(Error::TrackNotFound(_))
        ));
        copy.tracks[0].locked = true;
        assert!(refused(copy.ripple_delete_gap(v1, 3.0)).contains("locked"));
        assert!(same(&before, &t));

        // Clips that touch have no gap between them.
        let asset = Uuid::new_v4();
        let mut touching = timeline(vec![lane(
            V,
            "V1",
            vec![clip(asset, 0.0, 2.0, 0.0), clip(asset, 0.0, 2.0, 2.0)],
        )]);
        let v1 = touching.tracks[0].id;
        assert!(refused(touching.ripple_delete_gap(v1, 2.0)).contains("no gap"));
        let mut blank = timeline(vec![lane(V, "V1", Vec::new())]);
        let v1 = blank.tracks[0].id;
        assert!(refused(blank.ripple_delete_gap(v1, 1.0)).contains("nothing after"));
    }

    #[test]
    fn a_gap_beside_an_old_overlap_is_still_the_empty_stretch() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![lane(
            V,
            "V1",
            vec![
                clip(asset, 0.0, 10.0, 0.0),
                clip(asset, 0.0, 2.0, 3.0),
                clip(asset, 0.0, 2.0, 12.0),
            ],
        )]);
        let v1 = t.tracks[0].id;
        let done = t.ripple_delete_gap(v1, 11.0).unwrap();
        assert_eq!((done.from, done.to, done.moved), (10.0, 12.0, 1));
        assert_eq!(t.tracks[0].clips[2].timeline_start, 10.0);
    }

    // ---- invariants over many random edits ---------------------------------------------

    /// A small deterministic generator, so a failure names its seed.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f64) / f64::from(1u32 << 31)
        }
        fn pick(&mut self, n: usize) -> usize {
            ((self.next() * n as f64) as usize).min(n - 1)
        }
    }

    fn lane_is_legal(track: &Track) -> bool {
        let mut clips: Vec<&Clip> = track.clips.iter().collect();
        clips.sort_by(|a, b| a.timeline_start.total_cmp(&b.timeline_start));
        clips.windows(2).all(|w| w[0].timeline_end() <= w[1].timeline_start + 1e-6)
            && clips
                .iter()
                .all(|c| c.timeline_start >= -1e-9 && c.duration() > 0.0 && c.duration().is_finite())
    }

    fn total(t: &Timeline) -> f64 {
        t.tracks.iter().flat_map(|t| t.clips.iter()).map(Clip::duration).sum()
    }

    fn overlap_with(t: &Timeline, a: f64, b: f64) -> f64 {
        t.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| (c.timeline_end().min(b) - c.timeline_start.max(a)).max(0.0))
            .sum()
    }

    #[test]
    fn random_edits_keep_every_lane_legal_and_lose_exactly_the_range() {
        let mut landed = [0usize; 3];
        for seed in 0..300u64 {
            let mut rng = Lcg(seed + 1);
            let asset = Uuid::new_v4();
            let mut tracks = Vec::new();
            for (kind, name) in [(V, "V1"), (V, "V2"), (A, "A1")] {
                let (mut at, mut clips) = (rng.next() * 2.0, Vec::new());
                for _ in 0..rng.pick(5) + 1 {
                    let len = 0.2 + rng.next() * 4.0;
                    let mut c = clip(asset, 1.0, 1.0 + len, at);
                    c.speed = [1.0, 2.0, 0.5, -1.0][rng.pick(4)];
                    clips.push(c);
                    at = clips.last().unwrap().timeline_end() + if rng.next() < 0.4 { rng.next() * 3.0 } else { 0.0 };
                }
                tracks.push(lane(kind, name, clips));
            }
            let mut t = timeline(tracks);
            for step in 0..6 {
                let (a, len) = (rng.next() * 14.0 - 1.0, 0.05 + rng.next() * 5.0);
                let named = [t.tracks[rng.pick(3)].id];
                let tracks = (rng.next() < 0.3).then_some(&named[..]);
                let before = t.clone();
                let a_held = a.max(0.0);
                let reached = overlap_with(&before, a_held, a + len);
                let context = format!("seed {seed} step {step}: [{a}, {}) on {tracks:?}", a + len);
                match rng.pick(3) {
                    0 => match t.split_all_tracks(a, tracks, true) {
                        Ok(done) => {
                            landed[0] += 1;
                            assert!(!done.cuts.is_empty(), "{context}");
                            near(total(&t), total(&before));
                            assert_eq!(
                                t.tracks.iter().map(|x| x.clips.len()).sum::<usize>(),
                                before.tracks.iter().map(|x| x.clips.len()).sum::<usize>() + done.cuts.len(),
                                "{context}"
                            );
                        }
                        Err(_) => assert!(same(&t, &before), "{context}"),
                    },
                    which => {
                        let r = if which == 1 {
                            t.lift_range(a, a + len, tracks, true)
                        } else {
                            t.extract_range(a, a + len, tracks, true)
                        };
                        match r {
                            Ok(_) if tracks.is_none() => {
                                landed[which] += 1;
                                near(total(&t), total(&before) - reached);
                            }
                            Ok(_) => {
                                landed[which] += 1;
                                assert!(total(&t) <= total(&before) + 1e-9, "{context}");
                            }
                            Err(_) => assert!(same(&t, &before), "{context}"),
                        }
                    }
                }
                for track in &t.tracks {
                    assert!(
                        lane_is_legal(track),
                        "{context}: {} is not a legal lane: {:?}",
                        track.name,
                        track.clips.iter().map(span).collect::<Vec<_>>()
                    );
                }
                t.check().unwrap_or_else(|e| panic!("{context}: {e}"));
            }
        }
        // The edits must really have landed, or the checks above judge nothing.
        assert!(landed.iter().all(|n| *n > 150), "ops that landed: {landed:?}");
    }

    #[test]
    fn random_edits_leave_linked_pairs_identical_and_in_step() {
        let mut landed = 0usize;
        for seed in 0..300u64 {
            let mut rng = Lcg(seed + 7);
            let asset = Uuid::new_v4();
            let (mut at, mut picture, mut sound) = (rng.next(), Vec::new(), Vec::new());
            for _ in 0..rng.pick(5) + 2 {
                let len = 0.3 + rng.next() * 4.0;
                let (src, speed) = (rng.next() * 3.0, [1.0, 2.0, -1.0][rng.pick(3)]);
                for out in [&mut picture, &mut sound] {
                    let mut c = clip(asset, src, src + len, at);
                    c.speed = speed;
                    out.push(c);
                }
                at = picture.last().unwrap().timeline_end() + if rng.next() < 0.4 { rng.next() * 2.0 } else { 0.0 };
            }
            let mut t = timeline(vec![lane(V, "V1", picture), lane(A, "A1", sound)]);
            for i in 0..t.tracks[0].clips.len() {
                let (p, s) = (id(&t, 0, i), id(&t, 1, i));
                t.link_clips(&[p, s]).unwrap();
            }
            for step in 0..5 {
                let (a, len) = (rng.next() * 12.0 - 0.5, 0.05 + rng.next() * 4.0);
                let named = [t.tracks[rng.pick(2)].id];
                let tracks = (rng.next() < 0.5).then_some(&named[..]);
                let before = t.clone();
                let context = format!("seed {seed} step {step}: [{a}, {}) on {tracks:?}", a + len);
                let r = match rng.pick(3) {
                    0 => t.split_all_tracks(a, tracks, true).map(drop),
                    1 => t.lift_range(a, a + len, tracks, true).map(drop),
                    _ => t.extract_range(a, a + len, tracks, true).map(drop),
                };
                if r.is_err() {
                    assert!(same(&t, &before), "{context}");
                    continue;
                }
                landed += 1;
                let spans = |t: &Timeline, i: usize| t.tracks[i].clips.iter().map(span).collect::<Vec<_>>();
                let (sp, ss) = (spans(&t, 0), spans(&t, 1));
                assert_eq!(sp.len(), ss.len(), "{context}: {sp:?} {ss:?}");
                for (x, y) in sp.iter().zip(&ss) {
                    near(x.0, y.0);
                    near(x.1, y.1);
                }
                assert!(t.first_sync_break(&before).is_none(), "{context}");
                let mut conformed = t.clone();
                conformed.conform_links(&before, &HashSet::new(), &HashMap::new()).unwrap();
                assert!(same(&conformed, &t), "{context}: the sync lock still had something to move");
                for (p, s) in t.tracks[0].clips.iter().zip(&t.tracks[1].clips) {
                    assert_eq!(p.link_id.is_some(), s.link_id.is_some(), "{context}");
                    if p.link_id.is_some() {
                        assert_eq!(p.link_id, s.link_id, "{context}: partners stay one group");
                    }
                }
                t.dissolve_all_orphans();
                t.check().unwrap_or_else(|e| panic!("{context}: {e}"));
            }
        }
        assert!(landed > 600, "ops that landed: {landed}");
    }
}
