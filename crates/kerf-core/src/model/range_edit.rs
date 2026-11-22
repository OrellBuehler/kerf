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
//! **No slivers.** A range that reaches into a clip would often leave a few milliseconds of it
//! behind — a head or a tail nobody can see or grab. [`RangeCtx::frame`] is the length of one frame
//! of the timeline (`Project` reads it from the rate the cut renders at, 30 fps when it has none,
//! held to 1–240 fps like a delivery rate): a lift or an extract **never changes the range it was
//! asked for**; when that range would leave a remainder shorter than a frame on a targeted lane
//! or a partner it cuts, it refuses, changing nothing, naming the clip and the track and giving
//! the nearest range that works (the clip's own edge, for every clip it would then leave a
//! sliver of). A remainder of exactly a frame or more is kept, so a frame-accurate range cuts where
//! it says. A cut of every track leaves a clip whole when a half of it would be that short
//! ([`CutAll::not_cut`]).
//!
//! Pure and all or nothing: an op works on a copy and replaces the timeline only when it went
//! through, so an error leaves the timeline as it was. `Project` runs them inside one revision
//! (`edit_named_exact`: they decide their own layout and never ripple), and the sync lock and
//! guard of `run_edit` still apply afterwards.

use super::links::{locked_partner, runs_into_unlinked};
use super::*;
use std::collections::BTreeSet;

/// What a range edit reads besides the timeline.
#[derive(Debug, Clone, Copy)]
pub struct RangeCtx<'a> {
    /// Whether linked partners are cut and moved with the clips they are linked to
    /// (`Project::links_active`).
    pub links: bool,
    /// How long one frame of the timeline lasts, in seconds (`1 / delivery_rate`). Held to
    /// [`MIN_DELIVERY_FPS`]`..=`[`MAX_DELIVERY_FPS`] fps; a value that is no length at all leaves
    /// only the `DIFF_EPS` noise floor, which is no rule.
    pub frame: f64,
    /// The project's assets, to name a clip in a refusal.
    pub assets: &'a [Asset],
}

/// A track a range edit left alone because it is locked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkippedTrack {
    pub track_id: Uuid,
    pub name: String,
}

/// A clip the sync lock trimmed back to make room for a linked sound it carried along: what an
/// extract or a gap delete that moves a sound which led or trailed its picture does to the linked
/// sound of the neighbouring shot it lands on. Reported by `Project`, not by the model op (the lock
/// runs after it).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LockTrim {
    pub track_id: Uuid,
    /// The track's name.
    pub track: String,
    pub clip_id: Uuid,
    /// How much of the clip is gone, in timeline seconds.
    pub seconds: f64,
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
    /// The locked tracks that hold a clip the cut would have split had they been unlocked (only
    /// when no tracks were named: an unnamed cut targets every unlocked track), in track order.
    pub skipped: Vec<SkippedTrack>,
    /// Clips of the **targeted** tracks with `at` inside them that were left whole because one half
    /// would have been shorter than a frame (the cut is as good as there already). A linked partner
    /// left whole for the same reason is not listed.
    pub not_cut: Vec<Uuid>,
}

/// What [`Timeline::lift_range`] / [`Timeline::extract_range`] did.
#[derive(Debug, Clone, Serialize)]
pub struct RangeEdit {
    /// The range as it was asked for (a start before 0 is held at 0).
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
    /// The tracks that changed, in track order — linked partners' tracks included. (A partner the
    /// sync lock carries along afterwards is the lock's, and not counted.)
    pub tracks: Vec<Uuid>,
    /// The locked tracks that hold a clip the range would have cut (or, for an extract, moved) had
    /// they been unlocked (only when no tracks were named), in track order.
    pub skipped: Vec<SkippedTrack>,
    /// What the sync lock trimmed back after the edit, to make room for a linked sound it carried
    /// along (empty when it trimmed nothing; filled in by `Project`).
    pub lock_trimmed: Vec<LockTrim>,
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
    /// What the sync lock trimmed back after the edit, to make room for a linked sound it carried
    /// along (empty when it trimmed nothing; filled in by `Project`).
    pub lock_trimmed: Vec<LockTrim>,
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

/// One frame of the timeline in seconds, as the rule uses it: held to what a delivery rate may be,
/// or `None` when `frame` is no length at all.
fn frame_len(frame: f64) -> Option<f64> {
    (frame.is_finite() && frame > 0.0).then(|| frame.clamp(1.0 / MAX_DELIVERY_FPS, 1.0 / MIN_DELIVERY_FPS))
}

/// The longest remainder that still counts as a sliver for a timeline whose frame lasts `frame`
/// seconds: anything up to it is shorter than a frame (to within float noise), anything longer is a
/// frame or more and is kept. A frame that is no length at all leaves only the noise floor
/// (`DIFF_EPS`), which is no rule.
fn sliver(frame: f64) -> f64 {
    frame_len(frame).map_or(DIFF_EPS, |f| f - DIFF_EPS)
}

/// `at` is inside `clip` with more than `keep` seconds of it on each side.
fn cuttable(clip: &Clip, at: f64, keep: f64) -> bool {
    clip.timeline_start + keep < at && at < clip.timeline_end() - keep
}

/// How much two spans overlap, in seconds (0 when they do not).
fn overlap_len(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.1.min(b.1) - a.0.max(b.0)).max(0.0)
}

/// Seconds as an agent can paste them back: rounded to the microsecond, shortest digits.
fn secs(x: f64) -> String {
    format!("{}", (x * 1e6).round() / 1e6)
}

/// Where every clip stood: `(lane, start, end)`.
type Spans = HashMap<Uuid, (usize, f64, f64)>;

/// The clips an edit of `[a, b)` takes part with: every clip of the targeted lanes, and the linked
/// partners it cuts or moves with them, wherever they sit (those on `guest_lanes`).
struct Participants {
    taking_part: HashSet<Uuid>,
    guest_lanes: BTreeSet<usize>,
}

/// A clip a range would leave a remainder shorter than a frame of.
struct Sliver {
    id: Uuid,
    lane: usize,
    /// What would stay before the range, when that is a sliver.
    before: Option<f64>,
    /// What would stay after it, when that is a sliver.
    after: Option<f64>,
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

    /// The link groups of two or more clips, each with the clips in track order, the groups in the
    /// order their first clip appears (track by track): the order a refusal names a lane in, so the
    /// same edit says the same thing every time.
    fn link_groups(&self) -> Vec<(Uuid, Vec<Uuid>)> {
        let mut groups: Vec<(Uuid, Vec<Uuid>)> = Vec::new();
        for clip in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            let Some(link) = clip.link_id else { continue };
            match groups.iter_mut().find(|(l, _)| *l == link) {
                Some((_, members)) => members.push(clip.id),
                None => groups.push((link, vec![clip.id])),
            }
        }
        groups.retain(|(_, members)| members.len() >= 2);
        groups
    }

    // ---- cut every track at one time ---------------------------------------

    /// **Cut every track at `at`** ("Add Edit to All Tracks"): each clip of the targeted tracks that
    /// has `at` inside it is split there, as [`Timeline::split_clip`] splits one — and with `ctx.links`
    /// its linked partners with it ([`Timeline::split_clip_linked`]: the halves are re-linked by side,
    /// a partner on a locked track refuses the lot). A track whose clips all end or start at `at`
    /// already has a cut there and is passed over, and so is a clip that would be left with a half
    /// shorter than one frame (it is listed in [`CutAll::not_cut`]). A time with nothing to cut at all
    /// is an error that says why (nothing under it, it is already a cut or within a frame of one, the
    /// clips are locked), never an empty edit.
    ///
    /// `tracks` names the tracks to cut (each must exist and be unlocked); `None` is every unlocked
    /// track, and the locked ones with a clip the cut would have split are reported in
    /// [`CutAll::skipped`]. Shifts nothing, so it is the same with or without ripple mode.
    pub fn split_all_tracks(&mut self, at: f64, tracks: Option<&[Uuid]>, ctx: &RangeCtx) -> Result<CutAll> {
        if !at.is_finite() {
            return Err(Error::InvalidArgument("the cut time must be a finite number".to_string()));
        }
        let keep = sliver(ctx.frame);
        let links = ctx.links;
        let lanes = self.range_lanes(tracks)?;
        let inside: Vec<&Clip> = lanes
            .iter()
            .flat_map(|&ti| self.tracks[ti].clips.iter())
            .filter(|c| strictly_inside(c, at))
            .collect();
        let under: Vec<Uuid> = inside.iter().filter(|c| cuttable(c, at, keep)).map(|c| c.id).collect();
        let not_cut: Vec<Uuid> = inside.iter().filter(|c| !cuttable(c, at, keep)).map(|c| c.id).collect();
        if under.is_empty() {
            return Err(self.nothing_to_cut(at, &lanes, keep));
        }
        let skipped = if tracks.is_none() {
            self.locked_where(|c| cuttable(c, at, keep))
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
            if !scratch.clip(id).is_some_and(|c| cuttable(c, at, keep)) {
                continue;
            }
            if links {
                scratch.split_clip_linked_keeping(id, at, keep)?;
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
        Ok(CutAll {
            at,
            cuts,
            skipped,
            not_cut,
        })
    }

    /// The error for a cut with nothing to cut, saying what is there instead.
    fn nothing_to_cut(&self, at: f64, lanes: &[usize], keep: f64) -> Error {
        let mut why = Vec::new();
        let clips = || lanes.iter().flat_map(|&ti| self.tracks[ti].clips.iter());
        if clips().any(|c| (c.timeline_start - at).abs() <= DIFF_EPS || (c.timeline_end() - at).abs() <= DIFF_EPS) {
            why.push("it is already a cut".to_string());
        } else if clips().any(|c| strictly_inside(c, at) && !cuttable(c, at, keep)) {
            why.push("it is already a cut to within a frame (a half shorter than one frame is not made)".to_string());
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
    /// is an error, and so is one that would leave a remainder shorter than one frame of a clip it cuts
    /// (the module docs: the range is never changed, the refusal gives the nearest one that works).
    /// `tracks` as for [`Timeline::split_all_tracks`]; with `ctx.links` the linked partners of the
    /// clips the range reaches are cut with them, and a partner on a locked track refuses the lot.
    pub fn lift_range(&mut self, from: f64, to: f64, tracks: Option<&[Uuid]>, ctx: &RangeCtx) -> Result<RangeEdit> {
        self.range_edit(from, to, tracks, false, ctx)
    }

    /// **Extract** `[from, to)` from the targeted tracks and close the gap: [`Timeline::lift_range`],
    /// then every clip that started at or after `to` moves up by the range's length, each track on
    /// its own — and a clip that is linked to one of them moves with it wherever it sits, the way the
    /// sync lock carries it for any ripple. Unlinked clips on a track that was not named stay where
    /// they are, and so do overlays and markers (no ripple edit moves them).
    ///
    /// A range with nothing in it still closes up when something follows it on a targeted track
    /// (deleting an empty stretch); with nothing in it and nothing after it, it is an error.
    pub fn extract_range(&mut self, from: f64, to: f64, tracks: Option<&[Uuid]>, ctx: &RangeCtx) -> Result<RangeEdit> {
        self.range_edit(from, to, tracks, true, ctx)
    }

    /// Who takes part in an edit of `[a, b)`: see [`Participants`]. A partner on a locked track
    /// that the range cuts or moves refuses the lot.
    fn participants(
        &self,
        lanes: &[usize],
        info: &Spans,
        groups: &[(Uuid, Vec<Uuid>)],
        (a, b): (f64, f64),
        close: bool,
    ) -> Result<Participants> {
        let on_lane: HashSet<usize> = lanes.iter().copied().collect();
        let overlaps = |s: f64, e: f64| s < b - DIFF_EPS && e > a + DIFF_EPS;
        let reaches = |s: f64, e: f64| overlaps(s, e) || (close && s >= b - DIFF_EPS);
        let mut taking_part: HashSet<Uuid> = lanes
            .iter()
            .flat_map(|&ti| self.tracks[ti].clips.iter().map(|c| c.id))
            .collect();
        let mut guest_lanes = BTreeSet::new();
        for (_, members) in groups {
            let driven = members.iter().any(|m| {
                let (ti, s, e) = info[m];
                on_lane.contains(&ti) && reaches(s, e)
            });
            if !driven {
                continue;
            }
            // A partner is cut with the clip it is linked to. One that reaches into the range only
            // by leading or trailing its picture, when the picture itself lies after the range (an
            // extract moves it, no more), is not cut: the sync lock carries it with its picture, as
            // it does for a deleted gap, trimming a linked sound it lands on and refusing where it
            // would land on an unlinked clip.
            let cut_with = members.iter().any(|m| {
                let (ti, s, e) = info[m];
                on_lane.contains(&ti) && overlaps(s, e)
            });
            for m in members {
                let (ti, s, e) = info[m];
                if on_lane.contains(&ti) || !reaches(s, e) {
                    continue;
                }
                if overlaps(s, e) && !cut_with {
                    continue;
                }
                if self.tracks[ti].locked {
                    return Err(locked_partner(&self.tracks[ti]));
                }
                taking_part.insert(*m);
                guest_lanes.insert(ti);
            }
        }
        Ok(Participants {
            taking_part,
            guest_lanes,
        })
    }

    /// The clips of `taking_part` that `[a, b)` would leave a remainder of `keep` seconds or less
    /// of, in track order.
    fn slivers(&self, info: &Spans, taking_part: &HashSet<Uuid>, (a, b): (f64, f64), keep: f64) -> Vec<Sliver> {
        let mut found = Vec::new();
        for (ti, track) in self.tracks.iter().enumerate() {
            for clip in track.clips.iter().filter(|c| taking_part.contains(&c.id)) {
                let (_, s, e) = info[&clip.id];
                let before = (s < a - DIFF_EPS && e > a + DIFF_EPS && a - s <= keep).then_some(a - s);
                let after = (s < b - DIFF_EPS && e > b + DIFF_EPS && e - b <= keep).then_some(e - b);
                if before.is_some() || after.is_some() {
                    found.push(Sliver {
                        id: clip.id,
                        lane: ti,
                        before,
                        after,
                    });
                }
            }
        }
        found
    }

    /// The refusal for a range that would leave a sliver: what it would leave of which clip, and the
    /// nearest range that leaves none — the range grown to the edge of each clip it would leave a
    /// sliver of, until no clip is left one (partners recounted for each try).
    #[allow(clippy::too_many_arguments)]
    fn sliver_refusal(
        &self,
        lanes: &[usize],
        info: &Spans,
        groups: &[(Uuid, Vec<Uuid>)],
        (a, b): (f64, f64),
        close: bool,
        keep: f64,
        found: &[Sliver],
        ctx: &RangeCtx,
    ) -> Error {
        let verb = if close { "extracting" } else { "lifting" };
        let (mut lo, mut hi) = (a, b);
        let mut now = found
            .iter()
            .map(|s| (s.id, s.before.is_some(), s.after.is_some()))
            .collect::<Vec<_>>();
        for _ in 0..16 {
            for (id, before, after) in &now {
                let (_, s, e) = info[id];
                if *before {
                    lo = lo.min(s);
                }
                if *after {
                    hi = hi.max(e);
                }
            }
            let Ok(part) = self.participants(lanes, info, groups, (lo, hi), close) else {
                break;
            };
            now = self
                .slivers(info, &part.taking_part, (lo, hi), keep)
                .iter()
                .map(|s| (s.id, s.before.is_some(), s.after.is_some()))
                .collect();
            if now.is_empty() {
                break;
            }
        }
        let first = &found[0];
        let clip = self.clip(first.id).expect("a sliver is of a clip on the timeline");
        let left = first.before.or(first.after).expect("a sliver is of one side at least");
        let named = match clip_name(clip, ctx.assets) {
            Some(name) => format!("{name} on {} (clip {})", self.tracks[first.lane].name, clip.id),
            None => format!("clip {} on {}", clip.id, self.tracks[first.lane].name),
        };
        let more = match found.len() - 1 {
            0 => String::new(),
            n => format!(" (and {n} more remainder{} like it)", if n == 1 { "" } else { "s" }),
        };
        let frame_ms = (frame_len(ctx.frame).unwrap_or(0.0) * 1000.0).round();
        let instead = match (lo != a, hi != b) {
            (true, true) => format!("start at {} and end at {}", secs(lo), secs(hi)),
            (true, false) => format!("start at {}", secs(lo)),
            _ => format!("end at {}", secs(hi)),
        };
        Error::InvalidArgument(format!(
            "{verb} {}–{} s would leave {} ms of {named}{more}, less than a frame ({frame_ms} ms): {instead} instead",
            secs(a),
            secs(b),
            ((left * 1000.0).round() as i64).max(1),
        ))
    }

    fn range_edit(&mut self, from: f64, to: f64, tracks: Option<&[Uuid]>, close: bool, ctx: &RangeCtx) -> Result<RangeEdit> {
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
        let links = ctx.links;
        let lanes = self.range_lanes(tracks)?;

        // Where every clip stood before: `(lane, start, end)`.
        let info: Spans = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(ti, t)| t.clips.iter().map(move |c| (c.id, (ti, c.timeline_start, c.timeline_end()))))
            .collect();
        let overlaps = |s: f64, e: f64| s < b - DIFF_EPS && e > a + DIFF_EPS;
        let reached = |s: f64, e: f64| overlaps(s, e) || (close && s >= b - DIFF_EPS);

        // Who takes part: the clips of the targeted lanes, and the linked partners of those the range
        // reaches, wherever they are.
        let groups = if links { self.link_groups() } else { Vec::new() };
        let Participants {
            taking_part,
            guest_lanes,
        } = self.participants(&lanes, &info, &groups, (a, b), close)?;
        let skipped = if tracks.is_none() {
            self.locked_where(|c| reached(c.timeline_start, c.timeline_end()))
        } else {
            Vec::new()
        };

        // The range is the one that was asked for, or it is refused: a remainder shorter than a frame of
        // a clip it cuts is not left behind, and is not taken with the range either.
        let keep = sliver(ctx.frame);
        let found = self.slivers(&info, &taking_part, (a, b), keep);
        if !found.is_empty() {
            return Err(self.sliver_refusal(&lanes, &info, &groups, (a, b), close, keep, &found, ctx));
        }

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
        // no part (the sync lock's own rule for a follower), nor further onto one it already
        // overlapped: an overlap that was there may stay as it was, never grow. An overlap of less
        // than a frame with linked material is let through: a sound a few milliseconds longer than its
        // picture is routine, and the clip it lies on is its own shot's.
        let part = |id: &Uuid| taking_part.contains(id) || pieces.contains_key(id);
        let was = |id: Uuid| info.get(pieces.get(&id).unwrap_or(&id)).map(|(_, s, e)| (*s, *e));
        for &ti in &guest_lanes {
            let lane = &scratch.tracks[ti];
            for m in lane.clips.iter().filter(|c| part(&c.id)) {
                for q in lane.clips.iter().filter(|c| !part(&c.id)) {
                    let (now_m, now_q) = ((m.timeline_start, m.timeline_end()), (q.timeline_start, q.timeline_end()));
                    if !spans_overlap(now_m, now_q) {
                        continue;
                    }
                    let had = match (was(m.id), was(q.id)) {
                        (Some(x), Some(y)) => overlap_len(x, y),
                        _ => 0.0,
                    };
                    let now = overlap_len(now_m, now_q);
                    if now <= had + DIFF_EPS || (q.link_id.is_some() && now <= keep) {
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
            lock_trimmed: Vec::new(),
        })
    }

    // ---- close a gap ----------------------------------------------------------

    /// **Ripple delete a gap**: the empty stretch of track `track_id` that `at` is in — from the end
    /// of the clip before it (0 at the start of the track) to the start of the clip after it — is
    /// closed by moving every clip from there on up by its length. A clip on another track is not
    /// touched by this (the sync lock of `Project` carries the linked partners of what moved, as for
    /// any ripple). `at` on the edge of a gap is in it.
    ///
    /// Refuses a locked track, a time before the start of the timeline, a time with a clip under
    /// it, a time with no gap (clips touching) and a gap with nothing after it (there is nothing to
    /// close up).
    pub fn ripple_delete_gap(&mut self, track_id: Uuid, at: f64) -> Result<GapClosed> {
        if !at.is_finite() {
            return Err(Error::InvalidArgument("the gap's time must be a finite number".to_string()));
        }
        if at < 0.0 {
            return Err(Error::InvalidArgument(
                "the gap's time is before the start of the timeline".to_string(),
            ));
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
            lock_trimmed: Vec::new(),
        })
    }
}

/// The name a refusal calls a clip by: its asset's, or a title's own; `None` when it has neither.
fn clip_name(clip: &Clip, assets: &[Asset]) -> Option<String> {
    match &clip.generator {
        Some(g) => Some(g.display_name()),
        None => assets.iter().find(|a| a.id == clip.asset_id).map(|a| a.name.clone()),
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

    /// One frame at 30 fps, as the engine's default rate gives it.
    const FRAME: f64 = 1.0 / 30.0;

    /// Linked partners follow, at 30 fps; and with the links off.
    const RULES: RangeCtx = RangeCtx {
        links: true,
        frame: FRAME,
        assets: &[],
    };
    const ALONE: RangeCtx = RangeCtx { links: false, ..RULES };

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
        let done = t.split_all_tracks(5.0, None, &RULES).unwrap();
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
        let done = t.split_all_tracks(5.0, None, &RULES).unwrap();
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
            let why = refused(copy.split_all_tracks(at, None, &RULES));
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
        assert!(refused(empty.split_all_tracks(1.0, None, &RULES)).contains("no unlocked track"));
        let mut blank = timeline(vec![lane(V, "V1", Vec::new()), lane(A, "A1", Vec::new())]);
        assert!(refused(blank.split_all_tracks(1.0, None, &RULES)).contains("no clip on an unlocked track"));
    }

    #[test]
    fn locked_tracks_are_left_alone_and_reported() {
        let asset = Uuid::new_v4();
        let mut a1 = lane(A, "A1", vec![clip(asset, 0.0, 10.0, 0.0)]);
        a1.locked = true;
        let mut far = lane(A, "A2", vec![clip(asset, 0.0, 2.0, 20.0)]);
        far.locked = true;
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]), a1, far]);
        let done = t.split_all_tracks(4.0, None, &RULES).unwrap();
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
        let done = t.split_all_tracks(4.0, Some(&[v2, v2]), &RULES).unwrap();
        assert_eq!(done.cuts.len(), 1, "a track named twice is cut once");
        assert_eq!(
            (t.tracks[0].clips.len(), t.tracks[1].clips.len(), t.tracks[2].clips.len()),
            (1, 2, 1)
        );

        let before = t.clone();
        assert!(matches!(
            t.split_all_tracks(4.0, Some(&[Uuid::new_v4()]), &RULES),
            Err(Error::TrackNotFound(_))
        ));
        assert!(refused(t.split_all_tracks(4.0, Some(&[]), &RULES)).contains("name at least one track"));
        t.tracks[0].locked = true;
        let v1 = t.tracks[0].id;
        assert!(refused(t.split_all_tracks(2.0, Some(&[v1]), &RULES)).contains("track V1 is locked"));
        t.tracks[0].locked = false;
        assert!(same(&t, &before));
    }

    #[test]
    fn linked_partners_are_cut_with_the_clip_and_the_halves_linked_by_side() {
        let (t0, x, s) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        let group = get(&t0, x).link_id;
        for tracks in [None, Some(vec![t0.tracks[0].id])] {
            let mut t = t0.clone();
            let done = t.split_all_tracks(4.0, tracks.as_deref(), &RULES).unwrap();
            assert_eq!(done.cuts.len(), 2, "the picture and its sound, whichever was asked for");
            assert_eq!(get(&t, x).link_id, group);
            assert_eq!(get(&t, s).link_id, group, "the left halves keep the group");
            let (rx, rs) = (id(&t, 0, 1), id(&t, 1, 1));
            let fresh = get(&t, rx).link_id;
            assert!(fresh.is_some() && fresh != group);
            assert_eq!(get(&t, rs).link_id, fresh, "the right halves are a group of their own");
        }
        let mut alone = t0.clone();
        alone.split_all_tracks(4.0, Some(&[t0.tracks[0].id]), &ALONE).unwrap();
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
        let why = refused(t.split_all_tracks(4.0, None, &RULES));
        assert!(why.contains("locked track A1"), "{why}");
        assert!(same(&t, &before));
        // With links off the picture is cut alone.
        t.split_all_tracks(4.0, None, &ALONE).unwrap();
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
            let done = t.split_all_tracks(at, None, &RULES).unwrap();
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
        let done = t.lift_range(3.0, 9.0, None, &RULES).unwrap();
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
        let done = t.lift_range(3.0, 6.0, None, &RULES).unwrap();
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
        let done = t.extract_range(3.0, 6.0, None, &RULES).unwrap();
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
        t.extract_range(0.1, 1.3, None, &RULES).unwrap();
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
        assert!(refused(t.lift_range(2.5, 4.5, None, &RULES)).contains("no clip on a targeted track"));
        assert!(same(&t, &before));
        let done = t.extract_range(2.0, 5.0, None, &RULES).unwrap();
        assert_eq!((done.clips(), done.moved, done.closed), (0, 1, 3.0));
        assert_spans(&t, 0, &[(0.0, 2.0), (2.0, 4.0)]);
        let why = refused(t.extract_range(10.0, 12.0, None, &RULES));
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
                    copy.extract_range(from, to, None, &RULES)
                } else {
                    copy.lift_range(from, to, None, &RULES)
                });
                assert!(got.contains(why), "({from}, {to}) -> {got}");
                assert!(same(&copy, &t));
            }
        }
        // A start before 0 is held at 0; an end far past everything lifts to the end.
        let mut copy = t.clone();
        let done = copy.lift_range(-5.0, 3.0, None, &RULES).unwrap();
        assert_eq!(done.from, 0.0);
        assert_spans(&copy, 0, &[(3.0, 10.0)]);
        let mut copy = t;
        copy.extract_range(4.0, 1e300, None, &RULES).unwrap();
        assert_spans(&copy, 0, &[(0.0, 4.0)]);
        // Nothing at all, and no tracks.
        let mut blank = timeline(vec![lane(V, "V1", Vec::new())]);
        assert!(blank.lift_range(0.0, 5.0, None, &RULES).is_err());
        assert!(blank.extract_range(0.0, 5.0, None, &RULES).is_err());
        let mut none = timeline(Vec::new());
        assert!(refused(none.extract_range(0.0, 5.0, None, &RULES)).contains("no unlocked track"));
    }

    #[test]
    fn locked_tracks_are_skipped_when_not_named_and_refused_when_named() {
        let asset = Uuid::new_v4();
        let mut v2 = lane(V, "V2", vec![clip(asset, 0.0, 10.0, 0.0)]);
        v2.locked = true;
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]), v2]);
        let locked = t.tracks[1].id;
        let before = t.clone();
        assert!(refused(t.lift_range(2.0, 4.0, Some(&[locked]), &RULES)).contains("track V2 is locked"));
        assert!(matches!(
            t.lift_range(2.0, 4.0, Some(&[Uuid::new_v4()]), &RULES),
            Err(Error::TrackNotFound(_))
        ));
        assert!(same(&t, &before));
        let done = t.extract_range(2.0, 4.0, None, &RULES).unwrap();
        assert_spans(&t, 0, &[(0.0, 2.0), (2.0, 8.0)]);
        assert_spans(&t, 1, &[(0.0, 10.0)]);
        assert_eq!(done.skipped.len(), 1);
        assert_eq!(done.skipped[0].name, "V2");
        // Everything locked: nothing to edit.
        t.tracks[0].locked = true;
        assert!(refused(t.lift_range(2.0, 4.0, None, &RULES)).contains("no unlocked track"));
    }

    #[test]
    fn a_partner_on_a_locked_track_refuses_a_range_it_reaches_and_not_one_it_misses() {
        let (mut t, x, _) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        t.tracks[1].locked = true;
        let before = t.clone();
        for close in [false, true] {
            let why = refused(if close {
                t.extract_range(4.0, 6.0, None, &RULES)
            } else {
                t.lift_range(4.0, 6.0, None, &RULES)
            });
            assert!(why.contains("locked track A1"), "{why}");
            assert!(same(&t, &before));
        }
        // With links off the picture alone is cut.
        t.lift_range(4.0, 6.0, None, &ALONE).unwrap();
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
        t.lift_range(4.0, 6.0, None, &RULES).unwrap();
        assert_spans(&t, 1, &[(0.0, 3.0)]);
    }

    #[test]
    fn a_linked_group_falls_in_two_by_side_and_stays_in_step() {
        let (t0, x, s) = pair_of(Uuid::new_v4(), (0.0, 10.0));
        let group = get(&t0, x).link_id;
        for close in [false, true] {
            let mut t = t0.clone();
            if close {
                t.extract_range(4.0, 6.0, None, &RULES).unwrap();
            } else {
                t.lift_range(4.0, 6.0, None, &RULES).unwrap();
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
        t.extract_range(5.0, 7.0, None, &RULES).unwrap();
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
        let done = t.extract_range(4.0, 6.0, Some(&[v1]), &RULES).unwrap();
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
        let why = refused(t.extract_range(4.0, 6.0, Some(&[v1]), &RULES));
        assert!(why.contains("A1") && why.contains("not linked to it"), "{why}");
        assert!(same(&t, &before));
        // Lifting leaves everything where it is, so nothing lands on anything.
        t.lift_range(4.0, 6.0, Some(&[v1]), &RULES).unwrap();
    }

    /// For every moment a clip showed outside the range, the piece that shows it afterwards plays the
    /// same footage with the same animation (at the moment it moved to).
    fn assert_frames_kept(name: &str, whole: &Clip, a: f64, b: f64) {
        let (start, end) = span(whole);
        for close in [false, true] {
            let mut t = timeline(vec![lane(V, "V1", vec![whole.clone()])]);
            if close {
                t.extract_range(a, b, None, &RULES).unwrap();
            } else {
                t.lift_range(a, b, None, &RULES).unwrap();
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
        t.lift_range(2.0, 4.0, None, &RULES).unwrap();
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
        t.lift_range(5.0, 7.0, None, &RULES).unwrap();
        let (l, r) = (&t.tracks[0].clips[1], &t.tracks[0].clips[2]);
        assert_eq!(l.id, b);
        assert_eq!((l.transition_in, l.fade_in, l.fade_out), (crossfade, 0.5, 0.0));
        assert_eq!((r.transition_in, r.fade_in, r.fade_out), (None, 0.0, 0.5));
        // A range that ends B's head: its new edge has no transition and no fade.
        let mut t = build();
        t.lift_range(3.0, 5.0, None, &RULES).unwrap();
        let (a, b) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
        assert_eq!((a.fade_out, b.transition_in, b.fade_in, b.fade_out), (0.0, None, 0.0, 0.5));
        // A range ending exactly at C's start leaves C's own transition alone.
        let mut t = build();
        t.lift_range(6.0, 8.0, None, &RULES).unwrap();
        assert_eq!(t.tracks[0].clips[1].transition_in, crossfade, "B's own start is untouched");
        assert_eq!(t.tracks[0].clips[2].transition_in, crossfade);
        // A fade longer than what is left of its clip is held inside it.
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
        t.tracks[0].clips[0].fade_in = 3.0;
        t.lift_range(2.0, 10.0, None, &RULES).unwrap();
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
        t.extract_range(2.0, 6.0, None, &RULES).unwrap();
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

    // ---- no slivers: a range that would leave a remainder shorter than a frame is refused -------

    /// The asset every sliver test cuts, named so a refusal can say which clip.
    fn interview() -> Asset {
        let mut a = crate::engine::test_support::av_asset(Uuid::new_v4(), 60.0);
        a.name = "interview.mp4".to_string();
        a
    }

    #[test]
    fn a_range_that_would_leave_a_sliver_is_refused_naming_the_clip_and_the_nearest_range() {
        let a = interview();
        let assets = [a.clone()];
        let rules = RangeCtx {
            assets: &assets,
            ..RULES
        };
        let whole = || timeline(vec![lane(V, "V1", vec![clip(a.id, 0.0, 10.0, 0.0)])]);
        let clip_id = |t: &Timeline| t.tracks[0].clips[0].id;

        // 20 ms of head is less than a frame (33.3 ms): the range is not changed, it is refused.
        let mut t = whole();
        let before = t.clone();
        let why = refused(t.lift_range(0.02, 5.0, None, &rules));
        assert_eq!(
            why,
            format!(
                "invalid argument: lifting 0.02–5 s would leave 20 ms of interview.mp4 on V1 (clip {}), less than a frame (33 ms): start at 0 instead",
                clip_id(&before)
            )
        );
        assert!(same(&t, &before), "a refusal changes nothing");
        // The suggestion is the range that works, and it does.
        t.lift_range(0.0, 5.0, None, &rules).unwrap();
        assert_spans(&t, 0, &[(5.0, 10.0)]);

        // The same at the other end, and for an extract.
        let mut t = whole();
        let why = refused(t.extract_range(5.0, 9.98, None, &rules));
        assert!(
            why.starts_with("invalid argument: extracting 5–9.98 s would leave 20 ms of interview.mp4 on V1 (clip ")
                && why.ends_with("), less than a frame (33 ms): end at 10 instead"),
            "{why}"
        );
        // Both ends, in one sentence.
        let why = refused(t.lift_range(0.02, 9.98, None, &rules));
        assert!(
            why.ends_with("less than a frame (33 ms): start at 0 and end at 10 instead"),
            "{why}"
        );
        // A clip with no asset on hand is named by its id.
        let why = refused(whole().lift_range(0.02, 5.0, None, &RULES));
        assert!(
            why.contains("20 ms of clip ") && why.contains(" on V1, less than a frame"),
            "{why}"
        );
    }

    #[test]
    fn exactly_a_frame_or_more_is_kept() {
        let asset = Uuid::new_v4();
        let whole = || timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
        // Exactly one frame and a hair over it are clips; a hair under is a sliver.
        for (head, kept) in [
            (FRAME, true),
            (FRAME + 1e-4, true),
            (0.0333, false),
            (0.03, false),
            (0.5, true),
        ] {
            let mut t = whole();
            let done = t.lift_range(head, 5.0, None, &RULES);
            assert_eq!(done.is_ok(), kept, "head of {head}: {done:?}");
            if kept {
                assert_eq!(t.tracks[0].clips.len(), 2);
                assert_eq!(done.unwrap().from, head, "the range is the one that was asked for");
            }
        }
        // The same after the range, and by the clips that follow it.
        let mut t = whole();
        assert!(t.extract_range(5.0, 10.0 - FRAME, None, &RULES).is_ok());
        assert!(whole().extract_range(5.0, 9.99, None, &RULES).is_err());
    }

    #[test]
    fn the_frame_is_the_timelines_own_and_is_held_to_a_delivery_rate() {
        let asset = Uuid::new_v4();
        let ask = |frame: f64| {
            let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
            t.lift_range(0.035, 5.0, None, &RangeCtx { frame, ..RULES }).is_ok()
        };
        assert!(ask(1.0 / 30.0), "35 ms is a frame at 30 fps");
        assert!(!ask(1.0 / 24.0), "and a sliver at 24 fps (41.7 ms)");
        assert!(ask(1.0 / 60.0));
        // A frame that is no length at all leaves only float noise to refuse.
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(ask(bad), "frame {bad}");
        }
        // An absurd footage rate cannot make the threshold seconds long: a frame is held to 1 fps ..= 240 fps.
        assert!(!ask(30.0), "a 30 s frame is a 1 s frame");
        let mut t = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
        assert!(
            t.lift_range(1.5, 5.0, None, &RangeCtx { frame: 30.0, ..RULES }).is_ok(),
            "1.5 s is more than that frame"
        );
        assert!(ask(1e-9), "a frame under 1/240 s is held to it: 35 ms is kept");
        let mut tiny = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
        assert!(
            tiny.lift_range(0.001, 5.0, None, &RangeCtx { frame: 1e-9, ..RULES }).is_err(),
            "1 ms is under 1/240 s"
        );
    }

    #[test]
    fn the_suggestion_covers_every_lane_and_partner_and_works() {
        // V1 starts 10 ms before the range, V2 a little over a frame before it: once V1's sliver is out of
        // the way, what is left of V2 is a sliver too, so the range to ask for starts where V2 does.
        let asset = Uuid::new_v4();
        let t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 4.99)]),
            lane(V, "V2", vec![clip(asset, 0.0, 10.0, 4.96)]),
        ]);
        let why = refused(t.clone().lift_range(5.0, 8.0, None, &RULES));
        assert!(
            why.contains("would leave 10 ms of clip ") && why.ends_with("start at 4.96 instead"),
            "{why}"
        );
        let mut again = t.clone();
        again.lift_range(4.96, 8.0, None, &RULES).unwrap();
        assert_spans(&again, 0, &[(8.0, 14.99)]);
        assert_spans(&again, 1, &[(8.0, 14.96)]);
        // Two lanes with a sliver each: the first is named and the other counted.
        let mut two = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 4.99)]),
            lane(V, "V2", vec![clip(asset, 0.0, 10.0, 4.98)]),
        ]);
        let why = refused(two.lift_range(5.0, 8.0, None, &RULES));
        assert!(
            why.contains("would leave 10 ms of clip ") && why.contains("(and 1 more remainder like it)"),
            "{why}"
        );
        assert!(why.ends_with("start at 4.98 instead"), "{why}");
        // Naming the track the sliver is not on is the other way out.
        let v2 = t.tracks[1].id;
        let mut named = t;
        named.lift_range(5.0, 8.0, Some(&[v2]), &RULES).unwrap();
        assert_spans(&named, 0, &[(4.99, 14.99)]);
    }

    #[test]
    fn a_partner_the_range_cuts_is_held_to_the_same_rule() {
        // The sound leads its picture by two seconds; the range starts 10 ms into the picture and 2.01 s
        // into the sound, which is only a sliver for the picture.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 5.0, 15.0, 5.0)]),
            lane(A, "A1", vec![clip(asset, 3.0, 15.0, 3.0)]),
        ]);
        let (x, s) = (id(&t, 0, 0), id(&t, 1, 0));
        t.link_clips(&[x, s]).unwrap();
        let v1 = t.tracks[0].id;
        let before = t.clone();
        let why = refused(t.extract_range(5.01, 9.0, Some(&[v1]), &RULES));
        assert!(why.contains("would leave 10 ms of clip ") && why.contains(" on V1"), "{why}");
        assert!(why.ends_with("start at 5 instead"), "{why}");
        assert!(same(&t, &before));
        // The picture's own sliver refuses it whether or not the sound follows.
        t.extract_range(5.01, 9.0, Some(&[v1]), &ALONE).unwrap_err();
        // A sliver of a partner the range does not cut is not the range's to refuse.
        let mut far = before;
        far.tracks[1].clips[0].source_out = 5.005;
        far.tracks[1].clips[0].timeline_start = 3.0;
        far.tracks[1].clips[0].source_in = 3.0;
        assert!(far.extract_range(5.5, 9.0, Some(&[v1]), &RULES).is_ok());
    }

    #[test]
    fn a_cut_that_would_leave_a_sliver_is_no_cut_for_that_clip() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(V, "V2", vec![clip(asset, 0.0, 5.01, 0.0)]),
        ]);
        let short = id(&t, 1, 0);
        let done = t.split_all_tracks(5.0, None, &RULES).unwrap();
        assert_eq!(done.cuts.len(), 1, "V1 is cut, V2 would be left 10 ms");
        assert_eq!(done.not_cut, vec![short]);
        assert_spans(&t, 1, &[(0.0, 5.01)]);

        // One frame is enough, a hair less is not.
        let mut ok = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 5.0 + FRAME, 0.0)])]);
        assert_eq!(ok.split_all_tracks(5.0, None, &RULES).unwrap().cuts.len(), 1);
        let mut no = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 5.0 + FRAME - 1e-4, 0.0)])]);
        let why = refused(no.split_all_tracks(5.0, None, &RULES));
        assert!(why.contains("already a cut to within a frame"), "{why}");

        // Nothing cut anywhere is the refusal, with the reason, and nothing changes.
        let mut head = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)])]);
        let before = head.clone();
        for at in [0.02, 9.98] {
            assert!(refused(head.split_all_tracks(at, None, &RULES)).contains("already a cut to within a frame"));
            assert!(same(&head, &before));
        }
        // A clip shorter than two frames has no cut at all.
        let mut tiny = timeline(vec![lane(V, "V1", vec![clip(asset, 0.0, 0.05, 1.0)])]);
        assert!(tiny.split_all_tracks(1.025, None, &RULES).is_err());
    }

    #[test]
    fn a_partner_with_a_sliver_half_is_left_whole_and_stays_with_its_bigger_side() {
        // The picture is cut at 5; its sound ends 10 ms after that. `not_cut` lists the targeted tracks'
        // clips only: the partner is left whole as well, and is not in it.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 5.01, 0.0)]),
        ]);
        let (x, s) = (id(&t, 0, 0), id(&t, 1, 0));
        t.link_clips(&[x, s]).unwrap();
        let group = get(&t, x).link_id;
        let v1 = t.tracks[0].id;
        let done = t.split_all_tracks(5.0, Some(&[v1]), &RULES).unwrap();
        assert_eq!((done.cuts.len(), done.not_cut.len()), (1, 0), "{done:?}");
        assert_spans(&t, 0, &[(0.0, 5.0), (5.0, 10.0)]);
        assert_spans(&t, 1, &[(0.0, 5.01)]);
        assert_eq!(get(&t, x).link_id, group, "the left half and the whole sound are one group");
        assert_eq!(get(&t, s).link_id, group);
        assert!(t.tracks[0].clips[1].link_id.is_none(), "the right half is alone");
        // With every track targeted the sound is on one and is listed.
        let mut both = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 5.01, 0.0)]),
        ]);
        let (x, s) = (id(&both, 0, 0), id(&both, 1, 0));
        both.link_clips(&[x, s]).unwrap();
        let done = both.split_all_tracks(5.0, None, &RULES).unwrap();
        assert_eq!((done.cuts.len(), done.not_cut), (1, vec![s]));
        // And one that begins 10 ms before the cut goes with the right half.
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(A, "A1", vec![clip(asset, 4.99, 10.0, 4.99)]),
        ]);
        let (x, s) = (id(&t, 0, 0), id(&t, 1, 0));
        t.link_clips(&[x, s]).unwrap();
        t.split_all_tracks(5.0, None, &RULES).unwrap();
        let (right, sound) = (&t.tracks[0].clips[1], &t.tracks[1].clips[0]);
        assert!(
            right.link_id.is_some() && right.link_id == sound.link_id,
            "the sound joined the right half"
        );
    }

    // ---- a partner may not run further into a clip it is not linked to ---------------------------

    /// V1 holds two shots linked to the two sounds on A1, which overlap each other by 89 ms already.
    fn overlapping_sounds() -> (Timeline, Uuid) {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                V,
                "V1",
                vec![clip(asset, 0.21, 3.645, 0.21), clip(asset, 4.096, 7.993, 4.096)],
            ),
            lane(A, "A1", vec![clip(asset, 0.0, 4.001, 0.0), clip(asset, 3.912, 8.163, 3.912)]),
        ]);
        let (p1, s1, p2, s2) = (id(&t, 0, 0), id(&t, 1, 0), id(&t, 0, 1), id(&t, 1, 1));
        t.link_clips(&[p1, s1]).unwrap();
        t.link_clips(&[p2, s2]).unwrap();
        let v1 = t.tracks[0].id;
        (t, v1)
    }

    #[test]
    fn an_edit_that_would_grow_an_overlap_that_was_already_there_is_refused() {
        let (t, v1) = overlapping_sounds();
        let before = t.clone();
        let mut copy = t.clone();
        let why = refused(copy.extract_range(3.761, 6.725, Some(&[v1]), &RULES));
        assert!(
            why.contains("run into another clip on A1") && why.contains("not linked to it"),
            "{why}"
        );
        assert!(same(&copy, &t), "a refusal changes nothing");

        // Cut and not moved, the sound's overlap only shrinks: that stays allowed.
        let mut lifted = t;
        lifted.lift_range(3.761, 6.725, Some(&[v1]), &RULES).unwrap();
        assert_spans(&lifted, 1, &[(0.0, 4.001), (6.725, 8.163)]);
        // And the overlap that was there is not itself a reason to refuse an edit that leaves it alone.
        let mut elsewhere = before;
        elsewhere.extract_range(7.0, 7.5, Some(&[v1]), &RULES).unwrap();
        assert_spans(&elsewhere, 1, &[(0.0, 4.001), (3.912, 7.0), (7.0, 7.663)]);
    }

    #[test]
    fn an_overlap_that_does_not_grow_is_accepted() {
        // Music rides under the end of a sound on A1: 0.5 s of overlap to begin with.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(
                A,
                "A1",
                vec![clip(asset, 0.0, 10.0, 0.0), clip(Uuid::new_v4(), 0.0, 2.5, 9.5)],
            ),
        ]);
        let (x, s) = (id(&t, 0, 0), id(&t, 1, 0));
        t.link_clips(&[x, s]).unwrap();
        let v1 = t.tracks[0].id;
        // A lift cuts the sound in two and leaves its right half exactly where it was: the same overlap.
        let mut lifted = t.clone();
        lifted.lift_range(2.0, 3.0, Some(&[v1]), &RULES).unwrap();
        assert_spans(&lifted, 1, &[(0.0, 2.0), (3.0, 10.0), (9.5, 12.0)]);
        // An extract carries it up, away from the music: the overlap is gone.
        let mut extracted = t.clone();
        extracted.extract_range(2.0, 3.0, Some(&[v1]), &RULES).unwrap();
        assert_spans(&extracted, 1, &[(0.0, 2.0), (2.0, 9.0), (9.5, 12.0)]);
    }

    #[test]
    fn a_sub_frame_overlap_with_linked_material_is_let_through_and_an_unlinked_clip_is_not() {
        // Two shots, each linked to its sound; the first sound runs 10 ms past its picture, into the
        // second shot's sound once that is moved up to where its picture is.
        let asset = Uuid::new_v4();
        let build = || {
            let mut t = timeline(vec![
                lane(V, "V1", vec![clip(asset, 0.0, 10.0, 0.0), clip(asset, 10.0, 30.0, 10.0)]),
                lane(A, "A1", vec![clip(asset, 0.0, 10.01, 0.0), clip(asset, 10.01, 30.0, 10.01)]),
            ]);
            let (p1, s1, p2, s2) = (id(&t, 0, 0), id(&t, 1, 0), id(&t, 0, 1), id(&t, 1, 1));
            t.link_clips(&[p1, s1]).unwrap();
            t.link_clips(&[p2, s2]).unwrap();
            let v1 = t.tracks[0].id;
            (t, v1, s1)
        };
        // A range that starts 20 ms into the second shot is a sliver of it (and 10 ms of its sound):
        // refused by that rule, which says where to start instead.
        let (mut t, v1, _) = build();
        let before = t.clone();
        let why = refused(t.extract_range(10.02, 15.0, Some(&[v1]), &RULES));
        assert!(
            why.contains("would leave 20 ms of clip ") && why.contains("(and 1 more remainder like it)"),
            "{why}"
        );
        assert!(why.ends_with("start at 10 instead"), "{why}");
        assert!(same(&t, &before));
        // At the cut itself it is let through: 10 ms of linked sound overlapping is not worth a refusal.
        t.extract_range(10.0, 15.0, Some(&[v1]), &RULES).unwrap();
        assert_spans(&t, 0, &[(0.0, 10.0), (10.0, 25.0)]);
        assert_spans(&t, 1, &[(0.0, 10.01), (10.0, 25.0)]);
        // The same overlap with an unlinked clip is still refused, naming the lane.
        let (mut t, v1, s1) = build();
        let (ti, ci) = t.locate(s1).unwrap();
        t.tracks[ti].clips[ci].link_id = None;
        let why = refused(t.extract_range(10.0, 15.0, Some(&[v1]), &RULES));
        assert!(
            why.contains("run into another clip on A1") && why.contains("not linked to it"),
            "{why}"
        );
        // And a linked overlap longer than a frame is still refused.
        let (mut t, v1, s1) = build();
        let (ti, ci) = t.locate(s1).unwrap();
        t.tracks[ti].clips[ci].source_out = 10.2;
        let why = refused(t.extract_range(10.0, 15.0, Some(&[v1]), &RULES));
        assert!(why.contains("run into another clip on A1"), "{why}");
    }

    // ---- a partner that only leads or trails its picture is carried, not cut --------------------

    /// V1 [0,7) then [10,15) linked to the sound that begins two seconds early: A1 [0,7) and [8,15).
    fn a_picture_after_the_range_and_a_sound_that_leads_it() -> (Timeline, Uuid, Uuid, Uuid) {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 7.0, 0.0), clip(asset, 10.0, 15.0, 10.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 7.0, 0.0), clip(asset, 8.0, 15.0, 8.0)]),
        ]);
        let (x, s) = (id(&t, 0, 1), id(&t, 1, 1));
        t.link_clips(&[x, s]).unwrap();
        let v1 = t.tracks[0].id;
        (t, v1, x, s)
    }

    #[test]
    fn extracting_an_empty_stretch_does_not_eat_the_lead_in_of_a_linked_sound() {
        let (mut t, v1, x, s) = a_picture_after_the_range_and_a_sound_that_leads_it();
        let done = t.extract_range(7.0, 10.0, Some(&[v1]), &RULES).unwrap();
        assert_spans(&t, 0, &[(0.0, 7.0), (7.0, 12.0)]);
        assert_eq!((done.removed, done.trimmed, done.split, done.moved), (0, 0, 0, 1), "{done:?}");
        assert_eq!(done.tracks, vec![t.tracks[0].id], "the sound's lane is the sync lock's");
        // The sound is exactly as it was — not cut at the range's end, not moved: the lock carries it.
        assert_spans(&t, 1, &[(0.0, 7.0), (8.0, 15.0)]);
        let sound = get(&t, s);
        assert_eq!((sound.source_in, sound.source_out), (8.0, 15.0), "its lead-in is still there");
        assert_eq!(sound.link_id, get(&t, x).link_id);
    }

    #[test]
    fn a_partner_that_overlaps_the_range_with_its_picture_is_still_cut_with_it() {
        // The same sound, but the picture reaches into the range too (it starts at 9): both are cut.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(V, "V1", vec![clip(asset, 0.0, 7.0, 0.0), clip(asset, 9.0, 15.0, 9.0)]),
            lane(A, "A1", vec![clip(asset, 0.0, 7.0, 0.0), clip(asset, 8.0, 15.0, 8.0)]),
        ]);
        let (x, s) = (id(&t, 0, 1), id(&t, 1, 1));
        t.link_clips(&[x, s]).unwrap();
        let v1 = t.tracks[0].id;
        let done = t.extract_range(7.0, 10.0, Some(&[v1]), &RULES).unwrap();
        assert_spans(&t, 0, &[(0.0, 7.0), (7.0, 12.0)]);
        assert_spans(&t, 1, &[(0.0, 7.0), (7.0, 12.0)]);
        assert_eq!((done.trimmed, done.moved), (2, 2), "{done:?}");
    }

    // ---- refusals name the same lane every time ---------------------------------------------------

    /// V1 holds two shots, each linked to a sound on a track of its own; the tracks are in `order`, and the
    /// sounds' tracks are locked when `lock` says so.
    fn two_sounds_on_two_lanes(order: [&str; 2], lock: bool) -> (Timeline, Uuid) {
        let asset = Uuid::new_v4();
        let mut lanes: Vec<(String, Track)> = Vec::new();
        for (name, start) in [("A1", 0.0), ("A2", 10.0)] {
            lanes.push((name.to_string(), lane(A, name, vec![clip(asset, start, start + 10.0, start)])));
        }
        lanes.sort_by_key(|(name, _)| order.iter().position(|o| o == name));
        let mut t = timeline(
            std::iter::once(lane(
                V,
                "V1",
                vec![clip(asset, 0.0, 10.0, 0.0), clip(asset, 10.0, 20.0, 10.0)],
            ))
            .chain(lanes.into_iter().map(|(_, track)| track))
            .collect(),
        );
        let v1 = t.tracks[0].id;
        let picture = [id(&t, 0, 0), id(&t, 0, 1)];
        for (i, name) in ["A1", "A2"].iter().enumerate() {
            let ti = t.tracks.iter().position(|tr| tr.name == *name).unwrap();
            t.link_clips(&[picture[i], id(&t, ti, 0)]).unwrap();
        }
        for track in t.tracks.iter_mut().skip(1) {
            track.locked = lock;
        }
        (t, v1)
    }

    #[test]
    fn a_refusal_names_the_lane_of_the_first_group_however_the_hash_falls() {
        for order in [["A1", "A2"], ["A2", "A1"]] {
            for _ in 0..40 {
                let (mut t, v1) = two_sounds_on_two_lanes(order, true);
                let why = refused(t.extract_range(5.0, 15.0, Some(&[v1]), &RULES));
                assert!(
                    why.contains("locked track A1"),
                    "the first group's sound is on A1 ({order:?}): {why}"
                );
            }
        }
    }

    #[test]
    fn a_collision_names_the_first_lane_in_track_order() {
        // One shot after a gap on V1, with a sound of the same group on each of two lanes; each lane holds music
        // where the sound would land once the gap is closed. Both refuse: the lane that comes first is named.
        for order in [["A1", "A2"], ["A2", "A1"]] {
            let asset = Uuid::new_v4();
            let mut lanes = vec![lane(V, "V1", vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 10.0, 20.0, 10.0)])];
            for name in order {
                lanes.push(lane(
                    A,
                    name,
                    vec![clip(Uuid::new_v4(), 0.0, 1.0, 6.0), clip(asset, 10.0, 20.0, 10.0)],
                ));
            }
            let mut t = timeline(lanes);
            let group: Vec<Uuid> = [id(&t, 0, 1), id(&t, 1, 1), id(&t, 2, 1)].to_vec();
            t.link_clips(&group).unwrap();
            let v1 = t.tracks[0].id;
            let before = t.clone();
            let why = refused(t.extract_range(5.0, 10.0, Some(&[v1]), &RULES));
            assert!(why.contains(&format!("clip on {} at", order[0])), "{order:?}: {why}");
            assert!(same(&t, &before));
        }
    }

    #[test]
    fn a_gap_before_the_start_of_the_timeline_is_refused() {
        let (mut t, v1) = {
            let t = timeline(vec![lane(V, "V1", vec![clip(Uuid::new_v4(), 0.0, 2.0, 3.0)])]);
            let v1 = t.tracks[0].id;
            (t, v1)
        };
        let before = t.clone();
        for at in [-1.0, -1e-3, -f64::MIN_POSITIVE, -0.0 - 1e-9] {
            let why = refused(t.ripple_delete_gap(v1, at));
            assert!(why.contains("before the start of the timeline"), "{at}: {why}");
            assert!(same(&t, &before));
        }
        // The head of the track is where the gap starts: 0 itself is in it.
        let done = t.ripple_delete_gap(v1, 0.0).unwrap();
        assert_eq!((done.from, done.to), (0.0, 3.0));
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

    /// A start and a length for a random edit. Half the time an end of it is aimed at the edge of a clip,
    /// a few milliseconds off: where a range leaves a sliver.
    fn random_range(rng: &mut Lcg, t: &Timeline) -> (f64, f64) {
        let (a, len) = (rng.next() * 14.0 - 1.0, 0.05 + rng.next() * 5.0);
        let edges: Vec<f64> = t
            .tracks
            .iter()
            .flat_map(|x| x.clips.iter())
            .flat_map(|c| [c.timeline_start, c.timeline_end()])
            .collect();
        if edges.is_empty() || rng.next() >= 0.5 {
            return (a, len);
        }
        let edge = edges[rng.pick(edges.len())];
        let jitter = (rng.next() - 0.5) * 0.08;
        if rng.next() < 0.5 {
            (edge + jitter, len)
        } else {
            (edge + jitter - len, len)
        }
    }

    #[test]
    fn random_edits_keep_every_lane_legal_and_lose_exactly_the_range() {
        let mut landed = [0usize; 3];
        let mut suggested = 0usize;
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
                let (a, len) = random_range(&mut rng, &t);
                let named = [t.tracks[rng.pick(3)].id];
                let tracks = (rng.next() < 0.3).then_some(&named[..]);
                let before = t.clone();
                let context = format!("seed {seed} step {step}: [{a}, {}) on {tracks:?}", a + len);
                match rng.pick(3) {
                    0 => match t.split_all_tracks(a, tracks, &RULES) {
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
                            t.lift_range(a, a + len, tracks, &RULES)
                        } else {
                            t.extract_range(a, a + len, tracks, &RULES)
                        };
                        match r {
                            Ok(done) if tracks.is_none() => {
                                landed[which] += 1;
                                // Everything the range as applied reached is gone, and only that.
                                near(total(&t), total(&before) - overlap_with(&before, done.from, done.to));
                                assert_eq!((done.from, done.to), (a.max(0.0), a + len), "the range is the one asked for");
                            }
                            Ok(_) => {
                                landed[which] += 1;
                                assert!(total(&t) <= total(&before) + 1e-9, "{context}");
                            }
                            Err(e) => {
                                assert!(same(&t, &before), "{context}");
                                // A sliver refusal's range works: it is not itself refused for a sliver.
                                let why = e.to_string();
                                if why.contains("less than a frame") {
                                    let tail = why.rsplit(": ").next().unwrap();
                                    let number = |key: &str| {
                                        tail.split(key)
                                            .nth(1)
                                            .map(|rest| rest.split_whitespace().next().unwrap().parse::<f64>().unwrap())
                                    };
                                    let (lo, hi) = (
                                        number("start at ").unwrap_or(a.max(0.0)),
                                        number("end at ").unwrap_or(a + len),
                                    );
                                    let mut again = before;
                                    let r = if which == 1 {
                                        again.lift_range(lo, hi, tracks, &RULES)
                                    } else {
                                        again.extract_range(lo, hi, tracks, &RULES)
                                    };
                                    if let Err(e) = r {
                                        let e = e.to_string();
                                        assert!(!e.contains("less than a frame"), "{context}: {lo}..{hi} is a sliver too: {e}");
                                    }
                                    suggested += 1;
                                }
                            }
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
                    assert!(
                        track.clips.iter().all(|c| c.duration() >= FRAME - 2e-6),
                        "{context}: {} holds a clip shorter than a frame: {:?}",
                        track.name,
                        track.clips.iter().map(span).collect::<Vec<_>>()
                    );
                }
                t.check().unwrap_or_else(|e| panic!("{context}: {e}"));
            }
        }
        // The edits must really have landed, or the checks above judge nothing.
        assert!(landed.iter().all(|n| *n > 150), "ops that landed: {landed:?}");
        assert!(suggested > 50, "slivers that were refused with a range: {suggested}");
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
                let (a, len) = random_range(&mut rng, &t);
                let named = [t.tracks[rng.pick(2)].id];
                let tracks = (rng.next() < 0.5).then_some(&named[..]);
                let before = t.clone();
                let context = format!("seed {seed} step {step}: [{a}, {}) on {tracks:?}", a + len);
                let r = match rng.pick(3) {
                    0 => t.split_all_tracks(a, tracks, &RULES).map(drop),
                    1 => t.lift_range(a, a + len, tracks, &RULES).map(drop),
                    _ => t.extract_range(a, a + len, tracks, &RULES).map(drop),
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
                assert!(
                    t.tracks
                        .iter()
                        .flat_map(|x| x.clips.iter())
                        .all(|c| c.duration() >= FRAME - 2e-6),
                    "{context}: a clip is shorter than a frame: {sp:?}"
                );
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
