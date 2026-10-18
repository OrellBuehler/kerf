//! Partial accept of a staged proposal: lay *some* of its changes onto the live cut.
//!
//! A proposal is a whole timeline; its [`TimelineDiff`] lists what it would change, each
//! entry named by a stable [`DiffEntry::id`]. [`Timeline::merge_subset`] takes the live cut,
//! the cut the proposal branched from, the proposal and the ids the reviewer picked, and
//! writes each picked change onto the live cut **by identity** (this clip, this text, this
//! track) — never by position — so changes the user made since keep standing.
//!
//! Some changes are not separable from others, and the merge says so instead of guessing:
//!
//! * a linked partner's change comes along (a picture and its sound are one piece of
//!   material), and so does the track a picked clip lands on, or the moves that take clips
//!   off a track the proposal removes — those are **auto-included** and reported;
//! * a pick that would put two clips on top of each other, or pull linked clips out of
//!   step, is refused with the place it goes wrong, because the changes that make room are
//!   a different entry the reviewer did not pick.
//!
//! Pure and unit-tested; [`crate::project::Project::apply_staged_subset`] adds the
//! persistence around it.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{fmt_time, spans_overlap, Clip, DiffKind, Timeline};
use crate::error::{Error, Result};

/// What a change does to an item of a list.
#[derive(Clone, Copy)]
enum Act {
    Add,
    Change,
    Remove,
}

/// Lay one add / change / remove of the item `id` onto `list`, or say why it no longer applies
/// (`what` names the item for the message). Items are matched by id, never by position.
fn lay<T: Clone>(
    list: &mut Vec<T>,
    id: Uuid,
    key: impl Fn(&T) -> Uuid,
    act: Act,
    wanted: Option<&T>,
    what: &str,
) -> std::result::Result<(), String> {
    let there = list.iter().position(|t| key(t) == id);
    match (act, there, wanted) {
        (Act::Remove, Some(at), _) => {
            list.remove(at);
            Ok(())
        }
        (Act::Remove, None, _) => Err(format!("the {what} is already gone")),
        (Act::Add, None, Some(t)) => {
            list.push(t.clone());
            Ok(())
        }
        (Act::Add, Some(_), _) => Err(format!("the {what} is already on the timeline")),
        (Act::Change, Some(at), Some(t)) => {
            list[at] = t.clone();
            Ok(())
        }
        _ => Err(format!("that {what} is no longer on the timeline")),
    }
}

/// A change that was applied although the reviewer did not pick it, and why it had to be.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoIncluded {
    pub id: String,
    pub summary: String,
    pub because: String,
}

/// A picked change that no longer applies, and why (the user changed that part of the cut since).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkippedChange {
    pub id: String,
    pub summary: String,
    pub why: String,
}

/// What [`Timeline::merge_subset`] made of a pick.
#[derive(Debug, Clone)]
pub struct SubsetMerge {
    /// The live cut with the picked changes laid on it.
    pub timeline: Timeline,
    /// Ids of the changes applied, picked or not, in diff order.
    pub applied: Vec<String>,
    pub auto_included: Vec<AutoIncluded>,
    pub skipped: Vec<SkippedChange>,
}

/// Everything the picks of one clip ask of it.
#[derive(Default)]
struct ClipOps {
    added: bool,
    removed: bool,
    moved: bool,
    retrimmed: bool,
    changed: bool,
    entries: Vec<usize>,
}

impl Timeline {
    /// Lay the changes of `proposed` (relative to `base`) named by `picked` onto `self`, the
    /// live cut. See the module docs for what is auto-included and what is refused.
    pub fn merge_subset(&self, base: &Timeline, proposed: &Timeline, picked: &[String]) -> Result<SubsetMerge> {
        let diff = base.diff(proposed);
        let entries = &diff.entries;
        if picked.is_empty() {
            return Err(Error::InvalidArgument("pick at least one change to apply".to_string()));
        }
        let index: HashMap<&str, usize> = entries.iter().enumerate().map(|(i, e)| (e.id.as_str(), i)).collect();
        let mut chosen: BTreeSet<usize> = BTreeSet::new();
        for id in picked {
            let i = index.get(id.as_str()).ok_or_else(|| {
                Error::InvalidArgument(format!(
                    "no change {id} in the proposal — ids come from the proposal's diff entries"
                ))
            })?;
            chosen.insert(*i);
        }
        let picked_set: BTreeSet<usize> = chosen.clone();
        let mut because: HashMap<usize, String> = HashMap::new();

        // The closure: what has to come with what was picked.
        let groups = link_groups([self, base, proposed]);
        loop {
            let mut more: Vec<(usize, String)> = Vec::new();
            for &i in &chosen {
                let e = &entries[i];
                if let Some(clip) = e.clip_id {
                    for partner in partners(&groups, [self, base, proposed], clip) {
                        for (j, f) in entries.iter().enumerate() {
                            if f.clip_id == Some(partner) && !chosen.contains(&j) {
                                more.push((
                                    j,
                                    "it is linked to a clip you picked — a picture and its sound go together".to_string(),
                                ));
                            }
                        }
                    }
                }
                match e.kind {
                    DiffKind::ClipAdded | DiffKind::ClipMoved => {
                        if let Some(track) = e.track_id {
                            for (j, f) in entries.iter().enumerate() {
                                if f.kind == DiffKind::TrackAdded && f.track_id == Some(track) && !chosen.contains(&j) {
                                    more.push((j, "a clip you picked is on this new track".to_string()));
                                }
                            }
                        }
                    }
                    DiffKind::TrackRemoved => {
                        let gone = e.track_id;
                        for (j, f) in entries.iter().enumerate() {
                            let leaves = f.kind == DiffKind::ClipMoved
                                && f.clip_id
                                    .and_then(|c| base.locate(c))
                                    .is_some_and(|(ti, _)| Some(base.tracks[ti].id) == gone);
                            if leaves && !chosen.contains(&j) {
                                more.push((j, "the clip would be lost with the track you picked for removal".to_string()));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if more.is_empty() {
                break;
            }
            for (j, why) in more {
                if chosen.insert(j) {
                    because.insert(j, why);
                }
            }
        }

        let mut out = self.clone();
        let mut applied: BTreeSet<usize> = BTreeSet::new();
        let mut skipped: Vec<SkippedChange> = Vec::new();
        let mut skip = |i: usize, why: &str| {
            skipped.push(SkippedChange {
                id: entries[i].id.clone(),
                summary: entries[i].summary.clone(),
                why: why.to_string(),
            });
        };
        let mut touched: HashSet<Uuid> = HashSet::new();

        // 1. New tracks, so the clips that land on them have somewhere to go.
        for &i in chosen.iter().filter(|&&i| entries[i].kind == DiffKind::TrackAdded) {
            let Some(id) = entries[i].track_id else { continue };
            let Some(at) = proposed.tracks.iter().position(|t| t.id == id) else {
                skip(i, "the proposal no longer has that track");
                continue;
            };
            if out.track(id).is_some() {
                skip(i, "the cut already has that track");
                continue;
            }
            let mut track = proposed.tracks[at].clone();
            track.clips.clear();
            out.tracks.insert(at.min(out.tracks.len()), track);
            applied.insert(i);
        }

        // 2. Track settings (name, mute, fader …) — everything on a track but its clips.
        for &i in chosen.iter().filter(|&&i| entries[i].kind == DiffKind::TrackChanged) {
            let Some(id) = entries[i].track_id else { continue };
            let (Some(from), Some(to)) = (proposed.track(id), out.track_mut(id)) else {
                skip(i, "that track is no longer on the timeline");
                continue;
            };
            let clips = std::mem::take(&mut to.clips);
            *to = from.clone();
            to.clips = clips;
            applied.insert(i);
        }

        // 3. Clips, one clip at a time however many of its changes were picked.
        let mut ops: Vec<(Uuid, ClipOps)> = Vec::new();
        for &i in &chosen {
            let e = &entries[i];
            let Some(clip) = e.clip_id else { continue };
            let slot = match ops.iter().position(|(id, _)| *id == clip) {
                Some(slot) => slot,
                None => {
                    ops.push((clip, ClipOps::default()));
                    ops.len() - 1
                }
            };
            let op = &mut ops[slot].1;
            match e.kind {
                DiffKind::ClipAdded => op.added = true,
                DiffKind::ClipRemoved => op.removed = true,
                DiffKind::ClipMoved => op.moved = true,
                DiffKind::ClipRetrimmed => op.retrimmed = true,
                DiffKind::ClipChanged => op.changed = true,
                _ => continue,
            }
            op.entries.push(i);
        }
        for (id, op) in &ops {
            if op.removed {
                match out.locate(*id) {
                    Some((ti, ci)) => {
                        out.tracks[ti].clips.remove(ci);
                        op.entries.iter().for_each(|i| {
                            applied.insert(*i);
                        });
                    }
                    None => op
                        .entries
                        .iter()
                        .for_each(|i| skip(*i, "the clip is already gone from the timeline")),
                }
                continue;
            }
            if op.added {
                if out.locate(*id).is_some() {
                    op.entries
                        .iter()
                        .for_each(|i| skip(*i, "the clip is already on the timeline"));
                    continue;
                }
                let Some((pt, pc)) = proposed.locate(*id) else {
                    op.entries
                        .iter()
                        .for_each(|i| skip(*i, "the proposal no longer has that clip"));
                    continue;
                };
                let Some(track) = out.track_mut(proposed.tracks[pt].id) else {
                    op.entries
                        .iter()
                        .for_each(|i| skip(*i, "the track it was added to is gone from the timeline"));
                    continue;
                };
                track.clips.push(proposed.tracks[pt].clips[pc].clone());
                track.sort_by_start();
                touched.insert(*id);
                op.entries.iter().for_each(|i| {
                    applied.insert(*i);
                });
                continue;
            }
            let Some((ti, ci)) = out.locate(*id) else {
                op.entries
                    .iter()
                    .for_each(|i| skip(*i, "the clip is no longer on the timeline"));
                continue;
            };
            let Some((pt, pc)) = proposed.locate(*id) else {
                op.entries
                    .iter()
                    .for_each(|i| skip(*i, "the proposal no longer has that clip"));
                continue;
            };
            let live: Clip = out.tracks[ti].clips[ci].clone();
            let wanted: &Clip = &proposed.tracks[pt].clips[pc];
            let mut clip = if op.changed { wanted.clone() } else { live.clone() };
            // The position and footage are their own changes: only a pick takes them.
            clip.timeline_start = live.timeline_start;
            clip.source_in = live.source_in;
            clip.source_out = live.source_out;
            if op.moved {
                clip.timeline_start = wanted.timeline_start;
            }
            if op.retrimmed {
                clip.source_in = wanted.source_in;
                clip.source_out = wanted.source_out;
            }
            let dest = if op.moved { proposed.tracks[pt].id } else { out.tracks[ti].id };
            if dest == out.tracks[ti].id {
                out.tracks[ti].clips[ci] = clip;
                out.tracks[ti].sort_by_start();
            } else {
                let Some(dest_ti) = out.tracks.iter().position(|t| t.id == dest) else {
                    op.entries
                        .iter()
                        .for_each(|i| skip(*i, "the track it moves to is gone from the timeline"));
                    continue;
                };
                out.tracks[ti].clips.remove(ci);
                out.tracks[dest_ti].clips.push(clip);
                out.tracks[dest_ti].sort_by_start();
            }
            touched.insert(*id);
            op.entries.iter().for_each(|i| {
                applied.insert(*i);
            });
        }

        // 4. Text, markers, the delivery frame and the master bus.
        for &i in &chosen {
            let e = &entries[i];
            match e.kind {
                DiffKind::OverlayAdded | DiffKind::OverlayChanged | DiffKind::OverlayRemoved => {
                    let Some(id) = e.overlay_id else { continue };
                    let act = match e.kind {
                        DiffKind::OverlayAdded => Act::Add,
                        DiffKind::OverlayChanged => Act::Change,
                        _ => Act::Remove,
                    };
                    // Plain text and animated titles share the overlay kinds; the id says which.
                    let animated =
                        proposed.svg_overlay(id).is_some() || base.svg_overlay(id).is_some() || self.svg_overlay(id).is_some();
                    let laid = if animated {
                        lay(
                            &mut out.svg_overlays,
                            id,
                            |o| o.id,
                            act,
                            proposed.svg_overlay(id),
                            "animated title",
                        )
                    } else {
                        lay(&mut out.overlays, id, |o| o.id, act, proposed.overlay(id), "text")
                    };
                    if let Err(why) = laid {
                        skip(i, &why);
                        continue;
                    }
                    applied.insert(i);
                }
                DiffKind::MarkerAdded | DiffKind::MarkerChanged | DiffKind::MarkerRemoved => {
                    let Some(id) = e.marker_id else { continue };
                    let act = match e.kind {
                        DiffKind::MarkerAdded => Act::Add,
                        DiffKind::MarkerChanged => Act::Change,
                        _ => Act::Remove,
                    };
                    let wanted = proposed.markers.iter().find(|m| m.id == id);
                    if let Err(why) = lay(&mut out.markers, id, |m| m.id, act, wanted, "marker") {
                        skip(i, &why);
                        continue;
                    }
                    out.markers.sort_by(|a, b| a.time.total_cmp(&b.time));
                    applied.insert(i);
                }
                DiffKind::FormatChanged => {
                    out.format = proposed.format;
                    applied.insert(i);
                }
                DiffKind::MasterChanged => {
                    out.master = proposed.master;
                    applied.insert(i);
                }
                _ => {}
            }
        }

        // 5. Removed tracks last: clips the proposal moved off them have already left.
        for &i in chosen.iter().filter(|&&i| entries[i].kind == DiffKind::TrackRemoved) {
            let Some(id) = entries[i].track_id else { continue };
            match out.tracks.iter().position(|t| t.id == id) {
                Some(at) if out.tracks.len() > 1 => {
                    out.tracks.remove(at);
                    applied.insert(i);
                }
                Some(_) => skip(i, "it is the last track"),
                None => skip(i, "the track is already gone"),
            }
        }

        if out.has_links() {
            out.dissolve_all_orphans();
        }
        out.check_subset_lanes(self, &touched)?;
        if out.has_links() {
            if let Some((a, b)) = out.first_sync_break(self) {
                return Err(Error::InvalidArgument(format!(
                    "applying only those changes would pull the linked clips on {a} and {b} out of step — pick the changes to both, or apply the whole proposal"
                )));
            }
        }

        let auto_included = chosen
            .iter()
            .filter(|i| !picked_set.contains(i) && applied.contains(i))
            .map(|&i| AutoIncluded {
                id: entries[i].id.clone(),
                summary: entries[i].summary.clone(),
                because: because.get(&i).cloned().unwrap_or_default(),
            })
            .collect();
        Ok(SubsetMerge {
            timeline: out,
            applied: applied.iter().map(|&i| entries[i].id.clone()).collect(),
            auto_included,
            skipped,
        })
    }

    /// Refuse a merge that left a clip it wrote on top of another clip of its track, unless the
    /// two already overlapped in `live` (an overlap that was not this merge's doing).
    fn check_subset_lanes(&self, live: &Timeline, touched: &HashSet<Uuid>) -> Result<()> {
        if touched.is_empty() {
            return Ok(());
        }
        let was: HashMap<Uuid, (f64, f64)> = live
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| (c.id, (c.timeline_start, c.timeline_end())))
            .collect();
        for track in &self.tracks {
            for p in track.clips.iter().filter(|c| touched.contains(&c.id)) {
                let span = (p.timeline_start, p.timeline_end());
                for q in &track.clips {
                    let same_group = p.link_id.is_some() && p.link_id == q.link_id;
                    if q.id == p.id || same_group || !spans_overlap(span, (q.timeline_start, q.timeline_end())) {
                        continue;
                    }
                    if let (Some(a), Some(b)) = (was.get(&p.id), was.get(&q.id)) {
                        if spans_overlap(*a, *b) {
                            continue;
                        }
                    }
                    return Err(Error::InvalidArgument(format!(
                        "applying only those changes would put two clips on top of each other on {} at {} — also pick the moves that make room, or apply the whole proposal",
                        track.name,
                        fmt_time(p.timeline_start.max(q.timeline_start))
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Every link group across the given cuts: link id → the clips ever in it.
fn link_groups<'a>(cuts: impl IntoIterator<Item = &'a Timeline>) -> HashMap<Uuid, HashSet<Uuid>> {
    let mut groups: HashMap<Uuid, HashSet<Uuid>> = HashMap::new();
    for cut in cuts {
        for clip in cut.tracks.iter().flat_map(|t| t.clips.iter()) {
            if let Some(link) = clip.link_id {
                groups.entry(link).or_default().insert(clip.id);
            }
        }
    }
    groups
}

/// The clips linked to `clip` in any of the cuts.
fn partners<'a>(groups: &HashMap<Uuid, HashSet<Uuid>>, cuts: impl IntoIterator<Item = &'a Timeline>, clip: Uuid) -> Vec<Uuid> {
    let mut out: BTreeSet<Uuid> = BTreeSet::new();
    for cut in cuts {
        if let Some(link) = cut.clip(clip).and_then(|c| c.link_id) {
            out.extend(groups.get(&link).into_iter().flatten().copied().filter(|c| *c != clip));
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Clip, StreamKind, Track};

    fn clip(asset: Uuid, start: f64, len: f64) -> Clip {
        Clip::new(asset, 0.0, len, start)
    }

    /// V1 holding `a` (0..4) and `b` (4..8), A1 empty.
    fn cut() -> (Timeline, Uuid, Uuid) {
        let asset = Uuid::new_v4();
        let mut t = Timeline::new();
        let (a, b) = (clip(asset, 0.0, 4.0), clip(asset, 4.0, 4.0));
        let ids = (a.id, b.id);
        t.tracks[0].clips = vec![a, b];
        (t, ids.0, ids.1)
    }

    fn id_of(t: &Timeline, base: &Timeline, kind: DiffKind, clip: Uuid) -> String {
        base.diff(t)
            .entries
            .into_iter()
            .find(|e| e.kind == kind && e.clip_id == Some(clip))
            .unwrap_or_else(|| panic!("no {kind:?} entry for the clip"))
            .id
    }

    #[test]
    fn entries_are_named_by_what_they_are_about_and_the_names_are_unique() {
        let (base, a, b) = cut();
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].volume = 0.5;
        proposed.tracks[0].clips[1].timeline_start = 5.0;
        proposed.tracks.push(Track::new(StreamKind::Audio, "A2"));
        let diff = base.diff(&proposed);
        let ids: Vec<&str> = diff.entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&format!("clip_changed:{a}").as_str()), "{ids:?}");
        assert!(ids.contains(&format!("clip_moved:{b}").as_str()), "{ids:?}");
        let unique: HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "ids repeat: {ids:?}");
        assert!(diff.entries.iter().all(|e| !e.id.is_empty()));
    }

    #[test]
    fn only_the_picked_change_lands_and_the_rest_of_the_proposal_stays_out() {
        let (base, a, b) = cut();
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].volume = 0.5;
        proposed.tracks[0].clips[1].volume = 0.25;
        let pick = id_of(&proposed, &base, DiffKind::ClipChanged, a);

        let merge = base.merge_subset(&base, &proposed, std::slice::from_ref(&pick)).unwrap();
        assert_eq!(merge.applied, vec![pick]);
        assert!(merge.auto_included.is_empty() && merge.skipped.is_empty());
        assert_eq!(merge.timeline.clip(a).unwrap().volume, 0.5);
        assert_eq!(merge.timeline.clip(b).unwrap().volume, 1.0, "the unpicked clip is as it was");
    }

    #[test]
    fn a_pick_lands_on_the_live_cut_by_identity_and_leaves_the_users_newer_work_alone() {
        let (base, a, b) = cut();
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].volume = 0.5;
        // The user kept cutting after the proposal began: clip b moved later.
        let mut live = base.clone();
        live.tracks[0].clips[1].timeline_start = 6.0;

        let pick = id_of(&proposed, &base, DiffKind::ClipChanged, a);
        let merged = live.merge_subset(&base, &proposed, &[pick]).unwrap().timeline;
        assert_eq!(merged.clip(a).unwrap().volume, 0.5);
        assert_eq!(merged.clip(b).unwrap().timeline_start, 6.0, "the user's move survives");
    }

    #[test]
    fn a_moved_and_retrimmed_clip_takes_only_the_picked_half() {
        let (base, a, _) = cut();
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].source_out = 2.0;
        let pick = id_of(&proposed, &base, DiffKind::ClipRetrimmed, a);
        let merged = base.merge_subset(&base, &proposed, &[pick]).unwrap().timeline;
        assert_eq!(merged.clip(a).unwrap().source_out, 2.0);
        assert_eq!(merged.clip(a).unwrap().timeline_start, 0.0);
    }

    #[test]
    fn a_linked_partner_comes_along_and_is_reported() {
        let asset = Uuid::new_v4();
        let mut base = Timeline::new();
        let link = Uuid::new_v4();
        let (mut v, mut s) = (clip(asset, 0.0, 4.0), clip(asset, 0.0, 4.0));
        v.link_id = Some(link);
        s.link_id = Some(link);
        let (vid, sid) = (v.id, s.id);
        base.tracks[0].clips = vec![v];
        base.tracks[1].clips = vec![s];
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].volume = 0.5;
        proposed.tracks[1].clips[0].volume = 0.5;

        let pick = id_of(&proposed, &base, DiffKind::ClipChanged, vid);
        let merge = base.merge_subset(&base, &proposed, &[pick]).unwrap();
        assert_eq!(merge.applied.len(), 2);
        assert_eq!(merge.auto_included.len(), 1);
        assert!(merge.auto_included[0].id.ends_with(&sid.to_string()));
        assert!(merge.auto_included[0].because.contains("linked"));
        assert_eq!(merge.timeline.clip(sid).unwrap().volume, 0.5);
    }

    #[test]
    fn a_clip_added_to_a_new_track_brings_the_track() {
        let (base, a, _) = cut();
        let mut proposed = base.clone();
        let mut track = Track::new(StreamKind::Video, "V2");
        let extra = clip(Uuid::new_v4(), 0.0, 2.0);
        let extra_id = extra.id;
        track.clips.push(extra);
        proposed.tracks.insert(1, track);

        let pick = id_of(&proposed, &base, DiffKind::ClipAdded, extra_id);
        let merge = base.merge_subset(&base, &proposed, &[pick]).unwrap();
        assert!(merge.timeline.clip(extra_id).is_some());
        assert_eq!(merge.timeline.tracks.len(), 3);
        assert_eq!(merge.auto_included.len(), 1, "{:?}", merge.auto_included);
        assert!(merge.auto_included[0].id.starts_with("track_added:"));
        let _ = a;
    }

    #[test]
    fn a_pick_that_would_overlap_a_clip_is_refused_naming_the_place() {
        let (base, a, _) = cut();
        let mut proposed = base.clone();
        // The proposal lengthens a and moves b out of the way; picking the lengthening alone overlaps.
        proposed.tracks[0].clips[0].source_out = 6.0;
        proposed.tracks[0].clips[1].timeline_start = 6.0;
        let pick = id_of(&proposed, &base, DiffKind::ClipRetrimmed, a);

        let err = base
            .merge_subset(&base, &proposed, std::slice::from_ref(&pick))
            .unwrap_err()
            .to_string();
        assert!(err.contains("on top of each other") && err.contains("V1"), "{err}");

        // With the move picked too, it is the proposal itself and lands.
        let moved = id_of(&proposed, &base, DiffKind::ClipMoved, base.tracks[0].clips[1].id);
        assert!(base.merge_subset(&base, &proposed, &[pick, moved]).is_ok());
    }

    #[test]
    fn a_change_to_something_the_user_since_removed_is_skipped_not_fatal() {
        let (base, a, b) = cut();
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].volume = 0.5;
        proposed.tracks[0].clips[1].volume = 0.25;
        let mut live = base.clone();
        live.tracks[0].clips.remove(0);

        let (pa, pb) = (
            id_of(&proposed, &base, DiffKind::ClipChanged, a),
            id_of(&proposed, &base, DiffKind::ClipChanged, b),
        );
        let merge = live.merge_subset(&base, &proposed, &[pa.clone(), pb.clone()]).unwrap();
        assert_eq!(merge.applied, vec![pb]);
        assert_eq!(merge.skipped.len(), 1);
        assert_eq!(merge.skipped[0].id, pa);
        assert!(merge.skipped[0].why.contains("no longer"));
    }

    #[test]
    fn unknown_and_empty_picks_are_refused() {
        let (base, _, _) = cut();
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].volume = 0.5;
        assert!(base.merge_subset(&base, &proposed, &[]).is_err());
        let err = base
            .merge_subset(&base, &proposed, &["clip_changed:nope".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("no change clip_changed:nope"), "{err}");
    }

    #[test]
    fn text_markers_the_frame_and_the_master_apply_by_identity_too() {
        let (base, _, _) = cut();
        let mut proposed = base.clone();
        proposed.overlays.push(crate::model::TextOverlay::new("hello", 1.0, 2.0));
        proposed.markers.push(crate::model::Marker {
            id: Uuid::new_v4(),
            time: 3.0,
            name: "beat".into(),
            color: None,
        });
        proposed.master.volume = 0.5;
        let diff = base.diff(&proposed);
        let all: Vec<String> = diff.entries.iter().map(|e| e.id.clone()).collect();
        assert_eq!(all.len(), 3, "{all:?}");

        let merge = base.merge_subset(&base, &proposed, &all).unwrap();
        assert_eq!(merge.timeline.overlays.len(), 1);
        assert_eq!(merge.timeline.markers.len(), 1);
        assert_eq!(merge.timeline.master.volume, 0.5);
        assert!(base.diff(&merge.timeline).entries.len() == 3 && merge.timeline.diff(&proposed).is_empty());
    }

    #[test]
    fn animated_titles_apply_by_identity_like_text() {
        use crate::model::SvgOverlay;
        let title = |name: &str, start: f64| SvgOverlay {
            id: Uuid::new_v4(),
            name: name.into(),
            svg: "<svg xmlns='http://www.w3.org/2000/svg'/>".into(),
            start,
            end: start + 2.0,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            opacity: 1.0,
            offset: 0.0,
            preset: None,
        };
        let (mut base, _, _) = cut();
        let kept = title("kept", 0.0);
        let dropped = title("dropped", 4.0);
        base.svg_overlays = vec![kept.clone(), dropped];
        let mut proposed = base.clone();
        proposed.svg_overlays[0].opacity = 0.5;
        proposed.svg_overlays.remove(1);
        let added = title("added", 8.0);
        proposed.svg_overlays.push(added.clone());
        let ids: Vec<String> = base.diff(&proposed).entries.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), 3, "{ids:?}");

        // The user dropped one of them meanwhile: that pick is skipped, the others land.
        let mut live = base.clone();
        live.svg_overlays.remove(1);
        let merge = live.merge_subset(&base, &proposed, &ids).unwrap();
        assert_eq!(merge.skipped.len(), 1, "{:?}", merge.skipped);
        assert!(merge.skipped[0].why.contains("animated title"));
        let ids_now: Vec<Uuid> = merge.timeline.svg_overlays.iter().map(|o| o.id).collect();
        assert_eq!(ids_now, vec![kept.id, added.id]);
        assert_eq!(merge.timeline.svg_overlays[0].opacity, 0.5);
        assert!(merge.timeline.overlays.is_empty(), "no text overlay appeared");
    }

    #[test]
    fn picking_everything_reproduces_the_proposal() {
        let (base, a, b) = cut();
        let mut proposed = base.clone();
        proposed.tracks[0].clips[0].source_out = 3.0;
        proposed.tracks[0].clips[1].timeline_start = 3.0;
        proposed.tracks[0].clips[1].volume = 0.4;
        let all: Vec<String> = base.diff(&proposed).entries.iter().map(|e| e.id.clone()).collect();
        let merge = base.merge_subset(&base, &proposed, &all).unwrap();
        assert!(
            merge.timeline.diff(&proposed).is_empty(),
            "{:?}",
            merge.timeline.diff(&proposed).summary()
        );
        let _ = (a, b);
    }
}
