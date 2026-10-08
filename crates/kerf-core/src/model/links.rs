//! Linked clips: a picture and its sound as one piece of material.
//!
//! Clips that share a [`Clip::link_id`] are a **link group** — in practice a video
//! clip and the audio clip that carries its sound (`Project::detach_audio` makes
//! the pair), at most one clip per track of a group. Linking is *identity*, not
//! position: two linked clips may sit at different times and have different
//! lengths (a J-cut or an L-cut: the sound leads or trails the picture), and an
//! edit carries the **change** to the partners, never forces them to line up.
//! Everything here is pure and unit-tested; `Project` decides whether links are in
//! force for a call (`Project::with_links`) and hands the edit to these.
//!
//! What each edit does to the partners of the clip it names:
//!
//! | edit | the partners… |
//! |---|---|
//! | move (`with_linked_moves`) | move by the same Δt, **on their own tracks** — a track change belongs to the clip you named |
//! | trim (`carry_extent_edit`) | follow the edge that changed **when they share it** (within 1 ms), clamped to their own footage; one that would then overlap a clip outside its group refuses the edit (`check_carried_lanes`, after the ripple) |
//! | split (`split_clip_linked`) | are split at the same timeline time (if it is inside them); the pieces on each side form a group of their own (`relink_sides`) |
//! | remove (`with_link_partners`) / ripple delete (`ripple_delete_linked`) | are removed too |
//! | cut a source span (`cut_clip_range_linked`) | lose the same stretch of *timeline*; the part of a partner that survives it is put back in step with the named clip (a partner whose head was inside the stretch resumes at the cut) |
//! | speed (`set_speed_linked`) | are retimed by the same ratio, and re-placed about the named clip |
//! | split and remove (`with_linked_cuts`) | are cut at the same time, if it is inside them |
//! | roll / slip / slide (`*_linked`) | get the same edit (roll: a partner *pair* sharing the cut), and the whole group clamps to its tightest member |
//!
//! **A locked partner refuses the whole edit** (all or nothing). Property edits —
//! volume, fades, effects, colour, transitions — are *not* carried: a picture and
//! its sound legitimately differ in those, and so may their lengths.
//!
//! # The sync lock: [`Timeline::conform_links`]
//!
//! Two linked clips of one asset at one speed are **in step** when the moment of
//! footage playing at a given timeline time is the same in both — equal
//! `content_offset`s, whatever stretch of it each one shows. That is the whole
//! meaning of "in sync", and it is what makes J/L-cuts first-class: a sound that
//! leads its picture by two seconds is in step, and stays so.
//!
//! The edits above carry what they *name*; a ripple, a gap closed by a delete, a
//! changed speed also *moves* clips nobody named — the shot after the one you
//! trimmed, and the sound of that shot sits on another track. After every edit
//! `conform_links` puts each group back: it works out how far every member's
//! offset moved, takes the group's **authority** — the clip the edit named, else the
//! member on the named clip's track, else the first member that moved — and shifts
//! the others by the difference (a shift keeps a clip's length, so this is the
//! "range" a ripple removes or inserts, carried to every linked track). Only
//! **linked** clips follow; an unlinked clip on a partner's track stays where it
//! was. Where a follower lands on another clip of the lane, the follower
//! wins against *linked* material (the clip it runs into is trimmed back), and an
//! *unlinked* clip in the way, a locked track, or a clip that would be covered
//! entirely refuses the edit with the reason.
//!
//! Under that sits the **sync guard** (`first_sync_break`, run by `Project`'s
//! `run_edit` last): an edit that still leaves an in-step pair apart — two clips of
//! a pair both moved by hand, by different amounts — is refused instead of silently
//! desynchronizing sound from picture. `link: false` is how a pair is parted on
//! purpose; unlinking is how it stays parted.

use super::*;

/// What [`Timeline::detach_audio`] made.
#[derive(Debug, Clone, Serialize)]
pub struct Detached {
    /// The new audio clip: the picture clip's own span, at its position, linked to it.
    pub clip: Clip,
    /// The audio track it landed on.
    pub track_id: Uuid,
    /// Whether that track had to be created (no audio track had room).
    pub created_track: bool,
}

/// What [`Timeline::detach_audio_many`] did: the clips it detached and the ones it
/// skipped, each with the reason.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DetachedMany {
    pub detached: Vec<Detached>,
    pub skipped: Vec<SkippedDetach>,
}

/// A clip a batch detach left alone.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedDetach {
    pub clip_id: Uuid,
    pub reason: String,
}

/// The timeline time at which source time 0 of `clip` would play: the same for two
/// clips of one asset exactly when they show the same moment of footage at the same
/// moment of the timeline. A reversed clip plays its window backwards, so the offset
/// is measured from the out side.
fn content_offset(clip: &Clip) -> f64 {
    if clip.is_reversed() {
        clip.timeline_start + clip.source_out / clip.speed_mag()
    } else {
        clip.timeline_start - clip.source_in / clip.speed_mag()
    }
}

fn locked_partner(track: &Track) -> Error {
    Error::InvalidArgument(format!("a linked clip is on locked track {} — unlock it first", track.name))
}

/// The refusal for a linked clip that would land on a clip it is not linked to.
fn runs_into_unlinked(track: &str, at: f64) -> Error {
    Error::InvalidArgument(format!(
        "the clip linked to this one would run into another clip on {track} at {} that is not linked to it — move that clip first",
        fmt_time(at)
    ))
}

/// Offsets closer than this are the same offset: a microsecond of float noise from
/// a JSON round-trip or a chain of shifts is not a desynchronization.
const STEP_EPS: f64 = 1e-6;

/// A track fader at or below this is silent: nothing can be carried onto it by
/// scaling a clip up.
const MIN_FADER: f32 = 1e-3;

/// Two faders closer than this are the same fader.
const FADER_EPS: f32 = 1e-6;

/// What a sound's gain is multiplied by to keep its level when it moves from the fader
/// `from` to the fader `to` (1 for faders that are the same within [`FADER_EPS`]).
fn fader_ratio(from: f32, to: f32) -> f64 {
    if (from - to).abs() <= FADER_EPS {
        1.0
    } else {
        f64::from(from) / f64::from(to)
    }
}

/// The link groups of one timeline, built in a single pass so an edit that asks
/// about many clips (a multi-select delete, the conform) does not scan every track
/// per question. Clips are named by id — an index into a track goes stale the
/// moment an edit inserts or removes a clip.
pub(super) struct LinkIndex {
    /// link id → its members, in track order.
    by_link: HashMap<Uuid, Vec<Uuid>>,
    /// clip id → its link id.
    link_of: HashMap<Uuid, Uuid>,
}

impl LinkIndex {
    /// The other members of `clip`'s group, in track order.
    pub(super) fn partners(&self, clip: Uuid) -> impl Iterator<Item = Uuid> + '_ {
        self.link_of
            .get(&clip)
            .and_then(|link| self.by_link.get(link))
            .into_iter()
            .flatten()
            .copied()
            .filter(move |id| *id != clip)
    }

    /// Every group with two or more members, each in track order.
    fn groups(&self) -> impl Iterator<Item = &Vec<Uuid>> {
        self.by_link.values().filter(|members| members.len() >= 2)
    }
}

impl Track {
    /// Make a lane legal again after [`Timeline::conform_links`] shifted the clips
    /// in `movers` into it. A mover that starts before 0 loses its head (trimming
    /// the head keeps a clip's sync, moving it would not) — if it is **sound**: a
    /// picture is never trimmed to fit, and a clip left under [`MIN_EDIT_CLIP`] is
    /// refused rather than stubbed. Where a mover overlaps a clip, the mover wins — of
    /// two movers, the later — and the clip it ran into is trimmed back, which is
    /// allowed only for a **linked** clip (`linked`) on an **audio** track, and only
    /// while at least [`MIN_EDIT_CLIP`] of it is left; an unlinked clip, a picture or
    /// a clip that would be left shorter refuses with the reason. Every sound trimmed
    /// (its track's name) is pushed to `notes`, so the edit can say so. Overlaps
    /// between clips nothing moved are old news and ignored.
    fn settle_followers(&mut self, movers: &HashSet<Uuid>, linked: &HashSet<Uuid>, notes: &mut Vec<String>) -> Result<()> {
        let picture = self.kind == StreamKind::Video;
        for clip in &mut self.clips {
            if movers.contains(&clip.id) && clip.timeline_start < -DIFF_EPS {
                if picture {
                    return Err(Error::InvalidArgument(format!(
                        "the linked clip on {} would start before the beginning of the timeline — a picture is never trimmed to fit",
                        self.name
                    )));
                }
                let by = -clip.timeline_start;
                if clip.duration() - by < MIN_EDIT_CLIP {
                    return Err(Error::InvalidArgument(format!(
                        "the linked clip on {} would be left under {MIN_EDIT_CLIP}s by the beginning of the timeline",
                        self.name
                    )));
                }
                clip.move_head(by, false);
                clip.timeline_start = 0.0;
                clip.clamp_fades();
                notes.push(self.name.clone());
            }
        }
        self.sort_by_start();
        for _ in 0..=self.clips.len() * 2 {
            let hit = self.clips.windows(2).position(|w| {
                w[1].timeline_start < w[0].timeline_end() - DIFF_EPS && (movers.contains(&w[0].id) || movers.contains(&w[1].id))
            });
            let Some(i) = hit else { return Ok(()) };
            let later_loses = movers.contains(&self.clips[i].id) && !movers.contains(&self.clips[i + 1].id);
            let loser = if later_loses { i + 1 } else { i };
            let at = fmt_time(self.clips[loser].timeline_start);
            if !linked.contains(&self.clips[loser].id) {
                return Err(runs_into_unlinked(&self.name, self.clips[loser].timeline_start));
            }
            if picture {
                return Err(Error::InvalidArgument(format!(
                    "the clip linked to this one would cut into a picture on {} at {} — a picture is never trimmed to make room; move one of them first",
                    self.name, at
                )));
            }
            let overlap = self.clips[i].timeline_end() - self.clips[i + 1].timeline_start;
            if self.clips[loser].duration() - overlap < MIN_EDIT_CLIP {
                return Err(Error::InvalidArgument(format!(
                    "the linked clip on {} would cover another linked clip at {} (under {MIN_EDIT_CLIP}s of it would be left) — move one of them first",
                    self.name, at
                )));
            }
            let clip = &mut self.clips[loser];
            if later_loses {
                clip.move_head(overlap, false);
            } else {
                clip.move_tail(-overlap, false);
            }
            clip.clamp_fades();
            notes.push(self.name.clone());
            self.sort_by_start();
        }
        Err(Error::InvalidArgument(format!(
            "the linked clips on {} cannot be laid out without overlapping",
            self.name
        )))
    }
}

/// How an edit changed one clip's extent, split into the three things it can be:
/// the content moving (`shift`), the head trimmed (`head`, + = shorter from the
/// front) and the tail moved (`tail`, + = longer), all in timeline seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ExtentEdit {
    shift: f64,
    head: f64,
    tail: f64,
}

/// What an edit did to `now`, from `was` (the same clip, same speed). `None` when
/// the extent did not change. A change that keeps the length is a **move**
/// (`shift` only). Otherwise the source window tells a trimmed edge from a moved
/// one: footage taken off the head is `head`, the rest of the start's travel is
/// the content moving. A still has no window to tell them apart (its in-point is
/// never written), so its edges are read straight off the extent.
fn extent_edit(was: &Clip, now: &Clip, looping: bool) -> Option<ExtentEdit> {
    let ds = now.timeline_start - was.timeline_start;
    let de = now.timeline_end() - was.timeline_end();
    if ds.abs() <= DIFF_EPS && de.abs() <= DIFF_EPS {
        return None;
    }
    if (ds - de).abs() <= DIFF_EPS {
        return Some(ExtentEdit {
            shift: ds,
            head: 0.0,
            tail: 0.0,
        });
    }
    let mag = now.speed_mag();
    let (head, tail) = if looping {
        (ds, de)
    } else if now.is_reversed() {
        ((was.source_out - now.source_out) / mag, (was.source_in - now.source_in) / mag)
    } else {
        ((now.source_in - was.source_in) / mag, (now.source_out - was.source_out) / mag)
    };
    Some(ExtentEdit {
        shift: ds - head,
        head,
        tail,
    })
}

impl DeltaRange {
    /// The range both `self` and `other` allow: the tighter bound each way, with
    /// the reason it came from.
    fn intersect(mut self, other: &DeltaRange) -> DeltaRange {
        if other.min > self.min {
            self.min = other.min;
            self.why_min = other.why_min.clone();
        }
        if other.max < self.max {
            self.max = other.max;
            self.why_max = other.why_max.clone();
        }
        self
    }

    /// The same range in units `k` times as large (`k > 0`).
    fn scaled(&self, k: f64) -> DeltaRange {
        DeltaRange {
            min: self.min * k,
            max: self.max * k,
            why_min: self.why_min.clone(),
            why_max: self.why_max.clone(),
        }
    }

    /// The reasons, prefixed with the track of the linked clip they belong to.
    fn on_linked(mut self, track: &str) -> DeltaRange {
        for why in [&mut self.why_min, &mut self.why_max] {
            if !why.is_empty() {
                *why = format!("linked clip on {track}: {why}");
            }
        }
        self
    }
}

impl Timeline {
    // ---- groups -------------------------------------------------------------

    /// Index the link groups: one pass over every clip.
    pub(super) fn link_index(&self) -> LinkIndex {
        let mut by_link: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        let mut link_of = HashMap::new();
        for clip in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            if let Some(link) = clip.link_id {
                by_link.entry(link).or_default().push(clip.id);
                link_of.insert(clip.id, link);
            }
        }
        LinkIndex { by_link, link_of }
    }

    /// The other clips of `clip_id`'s link group, in track order — empty for an
    /// unlinked clip, a clip that is not on the timeline, and a link whose partners
    /// are all gone. One scan; a caller asking about many clips builds
    /// [`Timeline::link_index`] once instead.
    pub fn link_partners(&self, clip_id: Uuid) -> Vec<Uuid> {
        let Some(link) = self.clip(clip_id).and_then(|c| c.link_id) else {
            return Vec::new();
        };
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.link_id == Some(link) && c.id != clip_id)
            .map(|c| c.id)
            .collect()
    }

    /// `ids` followed by the partners of each, every clip once (the order of `ids`
    /// kept, so a caller indexing the answer by request position still can).
    pub fn with_link_partners(&self, ids: &[Uuid]) -> Vec<Uuid> {
        let index = self.link_index();
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut out: Vec<Uuid> = Vec::with_capacity(ids.len());
        for id in ids {
            if seen.insert(*id) {
                out.push(*id);
            }
        }
        for id in ids {
            for partner in index.partners(*id) {
                if seen.insert(partner) {
                    out.push(partner);
                }
            }
        }
        out
    }

    /// Every clip that is linked to at least one other — what a summary asks, once,
    /// instead of [`Timeline::link_partners`] per clip.
    pub fn linked_clip_ids(&self) -> HashSet<Uuid> {
        self.link_index().groups().flatten().copied().collect()
    }

    fn clip_mut(&mut self, clip_id: Uuid) -> Option<&mut Clip> {
        let (ti, ci) = self.locate(clip_id)?;
        Some(&mut self.tracks[ti].clips[ci])
    }

    /// The partners of `clip_id` that are not in `skip`, each on an unlocked track —
    /// or the error that refuses the whole edit.
    fn unlocked_partners(&self, clip_id: Uuid, skip: &HashSet<Uuid>) -> Result<Vec<Uuid>> {
        self.unlocked_partners_in(&self.link_index(), clip_id, skip)
    }

    /// [`Timeline::unlocked_partners`] against an index already built.
    fn unlocked_partners_in(&self, index: &LinkIndex, clip_id: Uuid, skip: &HashSet<Uuid>) -> Result<Vec<Uuid>> {
        let mut out = Vec::new();
        for partner in index.partners(clip_id) {
            if skip.contains(&partner) {
                continue;
            }
            let (ti, _) = self.locate(partner).expect("a partner is on the timeline");
            if self.tracks[ti].locked {
                return Err(locked_partner(&self.tracks[ti]));
            }
            out.push(partner);
        }
        Ok(out)
    }

    /// Clear the link of any of `groups` left with a single member: a link of one
    /// clip is nothing, and a stale id would read as a link in the file.
    fn dissolve_orphans(&mut self, groups: &HashSet<Uuid>) {
        let mut members: HashMap<Uuid, usize> = HashMap::new();
        for clip in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            if let Some(link) = clip.link_id.filter(|l| groups.contains(l)) {
                *members.entry(link).or_default() += 1;
            }
        }
        for clip in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if clip.link_id.is_some_and(|l| members.get(&l) == Some(&1)) {
                clip.link_id = None;
            }
        }
    }

    /// [`Timeline::dissolve_orphans`] for every group on the timeline — what an edit
    /// that removed clips (a cut, a delete, a removed track) leaves behind: a
    /// picture whose sound is gone is not linked to anything. Reports whether it
    /// cleared one.
    pub fn dissolve_all_orphans(&mut self) -> bool {
        let mut members: HashMap<Uuid, usize> = HashMap::new();
        for clip in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            if let Some(link) = clip.link_id {
                *members.entry(link).or_default() += 1;
            }
        }
        let mut cleared = false;
        for clip in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if clip.link_id.is_some_and(|l| members.get(&l) == Some(&1)) {
                clip.link_id = None;
                cleared = true;
            }
        }
        cleared
    }

    /// Re-form a group after an edit cut it in two: the clips of `left` (which
    /// are before the cut) keep `group`, the clips of `right` (after it) get a
    /// fresh id, and a side with fewer than two clips is no group at all. Both sides
    /// matter — a partner the cut did not touch still has to land on the right one,
    /// or moving one half would silently drag a clip that belongs to the other.
    fn relink_sides(&mut self, group: Option<Uuid>, left: &[Uuid], right: &[Uuid]) {
        let left_id = group.filter(|_| left.len() >= 2);
        let right_id = (right.len() >= 2).then(Uuid::new_v4);
        for (ids, link) in [(left, left_id), (right, right_id)] {
            for id in ids {
                if let Some(clip) = self.clip_mut(*id) {
                    clip.link_id = link;
                }
            }
        }
    }

    // ---- the sync guard -----------------------------------------------------

    /// Whether any clip is linked to another — the cheap test that lets an unlinked
    /// project skip the sync lock and the guard (and their snapshot) entirely.
    pub fn has_links(&self) -> bool {
        self.tracks.iter().any(|t| t.clips.iter().any(|c| c.link_id.is_some()))
    }

    /// The **sync guard**: the first link group that the edit which turned `before`
    /// into `self` pulled out of step, as the names of the two tracks — or `None`.
    ///
    /// Two linked clips of the same asset at the same speed are *in step* when the
    /// moment of footage playing at any timeline time is the same in both — equal
    /// content offsets, whatever stretch of it each one shows. Every link-aware edit
    /// carries its change and [`Timeline::conform_links`] puts the rest back; this is
    /// the net under all of it: what is still apart afterwards (two partners both
    /// named and moved by different amounts) is *refused* rather than silently
    /// desynchronizing sound from picture. A pair that was already apart, or whose
    /// clips cannot be compared (different assets), is not the edit's doing and is
    /// not looked at.
    pub fn first_sync_break(&self, before: &Timeline) -> Option<(String, String)> {
        let prior: HashMap<Uuid, &Clip> = before.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect();
        let mut groups: HashMap<Uuid, Vec<(usize, &Clip)>> = HashMap::new();
        for (ti, track) in self.tracks.iter().enumerate() {
            for clip in &track.clips {
                if let Some(link) = clip.link_id {
                    groups.entry(link).or_default().push((ti, clip));
                }
            }
        }
        // Of every pair that broke, the one on the lowest tracks: the answer must not
        // depend on the order a hash map happens to hold the groups in.
        let mut first: Option<(usize, usize)> = None;
        for members in groups.values() {
            for (i, (ta, a)) in members.iter().enumerate() {
                for (tb, b) in &members[i + 1..] {
                    let (Some(pa), Some(pb)) = (prior.get(&a.id), prior.get(&b.id)) else {
                        continue;
                    };
                    let in_step_before = pa.asset_id == pb.asset_id
                        && (pa.speed - pb.speed).abs() < STEP_EPS
                        && (content_offset(pa) - content_offset(pb)).abs() < STEP_EPS;
                    if !in_step_before {
                        continue;
                    }
                    if (a.speed - b.speed).abs() >= STEP_EPS || (content_offset(a) - content_offset(b)).abs() >= STEP_EPS {
                        let pair = (*ta.min(tb), *ta.max(tb));
                        if first.is_none_or(|f| pair < f) {
                            first = Some(pair);
                        }
                    }
                }
            }
        }
        first.map(|(a, b)| (self.tracks[a].name.clone(), self.tracks[b].name.clone()))
    }

    // ---- the sync lock ------------------------------------------------------

    /// The **sync lock**. `self` is what an edit (and, in ripple mode, the per-lane
    /// ripple) left, `before` where it started, `anchors` the clips the edit named
    /// and `origin` maps a clip the edit *created* (the tail of a cut) to the clip it
    /// was made from, so it can be judged against what that clip was. Every link
    /// group is put back in the relationship it had: each member's offset
    /// ([`content_offset`]) is compared with its own before, and the members that
    /// did not move as far as the group's **authority** are shifted by the
    /// difference.
    ///
    /// The authority is, in order, the first member — in track order — the edit
    /// named; the member on the track of a clip the edit named (a ripple of V1 is
    /// V1's ripple, whatever the partner track did of its own); the first member that
    /// moved at all (a ripple that pushed one clip, whose partner nobody touched).
    /// Two *named* members that were **moved apart by the edit itself** were parted by
    /// hand and are left alone: that is the guard's to refuse. That is judged on the
    /// timeline *as the edit left it* (`left`, which is the timeline before the
    /// per-lane ripple; `None` judges `self`): a trim to the playhead that names a
    /// picture and its sound cuts both at the same time and agrees, and only the
    /// ripple — which closes each track by a different length for a J- or L-cut — sets
    /// them apart afterwards, which is the lock's to put right: the other named members
    /// are shifted to the first one. A shift keeps a clip's length, so this carries the
    /// same removed or inserted span to every linked track without ever cutting a
    /// partner — only [`Timeline::cut_clip_range_linked`], which is an explicit
    /// removal, cuts one.
    ///
    /// Only clips **in a group** follow; the other clips of a partner's lane are
    /// nobody's business. Where a follower lands on another clip it wins against a
    /// *linked* one on an *audio* track (that clip is trimmed back, keeping its own
    /// sync, and the track's name is pushed to `notes`), and a sound stops before 0 by
    /// losing its head. It refuses — with the reason, leaving `self` partly changed,
    /// so call it on a scratch copy — when a follower is on a locked track, when an
    /// *unlinked* clip or a *picture* is in its way, when less than
    /// [`MIN_EDIT_CLIP`] of a clip would be left, or a picture would start before 0.
    pub fn conform_links(&mut self, before: &Timeline, anchors: &HashSet<Uuid>, origin: &HashMap<Uuid, Uuid>) -> Result<()> {
        self.conform_links_noted(before, anchors, origin, None, &mut Vec::new())
    }

    /// [`Timeline::conform_links`] with the timeline as the edit left it (`left`) and
    /// the sounds it trimmed reported in `notes`.
    pub fn conform_links_noted(
        &mut self,
        before: &Timeline,
        anchors: &HashSet<Uuid>,
        origin: &HashMap<Uuid, Uuid>,
        left: Option<&Timeline>,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        let index = self.link_index();
        if index.groups().next().is_none() {
            return Ok(());
        }
        let prior: HashMap<Uuid, &Clip> = before.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect();
        let left_at: Option<HashMap<Uuid, &Clip>> =
            left.map(|l| l.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect());
        let at: HashMap<Uuid, (usize, &Clip)> = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(ti, t)| t.clips.iter().map(move |c| (c.id, (ti, c))))
            .collect();
        // The tracks the edit named a clip on: where it speaks for the group.
        let anchor_tracks: HashSet<Uuid> = anchors
            .iter()
            .filter_map(|id| match before.locate(*id) {
                Some((ti, _)) => Some(before.tracks[ti].id),
                None => self.locate(*id).map(|(ti, _)| self.tracks[ti].id),
            })
            .collect();

        let mut shifts: Vec<(Uuid, f64)> = Vec::new();
        for members in index.groups() {
            // `(clip, its track, how far its offset moved)` for every member that has a before.
            let moved: Vec<(Uuid, usize, f64)> = members
                .iter()
                .filter_map(|id| {
                    let (ti, now) = at.get(id)?;
                    let was = prior.get(origin.get(id).unwrap_or(id))?;
                    Some((*id, *ti, content_offset(now) - content_offset(was)))
                })
                .collect();
            if moved.len() < 2 {
                continue;
            }
            let named: Vec<&(Uuid, usize, f64)> = moved.iter().filter(|(id, _, _)| anchors.contains(id)).collect();
            let (authority, reference) = if let Some((first, _, d)) = named.first() {
                // How far each named member was moved by the edit itself, before any ripple.
                let as_left = |id: &Uuid, now: f64| match (&left_at, prior.get(origin.get(id).unwrap_or(id))) {
                    (Some(l), Some(was)) => l.get(id).map_or(now, |c| content_offset(c) - content_offset(was)),
                    _ => now,
                };
                let first_left = as_left(first, *d);
                if named
                    .iter()
                    .any(|(id, _, other)| (as_left(id, *other) - first_left).abs() > STEP_EPS)
                {
                    continue;
                }
                (Some(*first), *d)
            } else if let Some((_, _, d)) = moved.iter().find(|(_, ti, _)| anchor_tracks.contains(&self.tracks[*ti].id)) {
                (None, *d)
            } else if let Some((_, _, d)) = moved.iter().find(|(_, _, d)| d.abs() > STEP_EPS) {
                (None, *d)
            } else {
                continue;
            };
            for (id, _, d) in &moved {
                let shift = reference - d;
                if Some(*id) != authority && shift.abs() > STEP_EPS {
                    shifts.push((*id, shift));
                }
            }
        }
        let linked = self.settle_linked(before, origin);
        self.apply_shifts(&shifts, &linked, notes)
    }

    /// The clips that may be trimmed back to make room for a follower: those linked now,
    /// and those that *were* linked before the edit (a piece an edit cut off from its
    /// partner is a leftover of a linked clip, and `origin` says which clip a new piece
    /// came from).
    fn settle_linked(&self, before: &Timeline, origin: &HashMap<Uuid, Uuid>) -> HashSet<Uuid> {
        let mut linked = self.linked_clip_ids();
        let was = before.linked_clip_ids();
        linked.extend(
            self.tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .map(|c| c.id)
                .filter(|id| was.contains(origin.get(id).unwrap_or(id))),
        );
        linked
    }

    /// Shift clips by `shifts` (a clip, by how much) and lay each lane they landed on out
    /// again ([`Track::settle_followers`]); a lane that is locked refuses the lot.
    fn apply_shifts(&mut self, shifts: &[(Uuid, f64)], linked: &HashSet<Uuid>, notes: &mut Vec<String>) -> Result<()> {
        let shifts: Vec<(Uuid, f64)> = shifts.iter().copied().filter(|(_, by)| by.abs() > STEP_EPS).collect();
        if shifts.is_empty() {
            return Ok(());
        }
        let lane_of: HashMap<Uuid, usize> = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(ti, t)| t.clips.iter().map(move |c| (c.id, ti)))
            .collect();
        let movers: HashSet<Uuid> = shifts.iter().map(|(id, _)| *id).collect();
        let mut lanes: Vec<usize> = shifts.iter().filter_map(|(id, _)| lane_of.get(id).copied()).collect();
        lanes.sort_unstable();
        lanes.dedup();
        let shift_of: HashMap<Uuid, f64> = shifts.into_iter().collect();
        for ti in lanes {
            if self.tracks[ti].locked {
                return Err(locked_partner(&self.tracks[ti]));
            }
            for clip in &mut self.tracks[ti].clips {
                if let Some(by) = shift_of.get(&clip.id) {
                    clip.timeline_start += by;
                }
            }
            self.tracks[ti].settle_followers(&movers, linked, notes)?;
        }
        Ok(())
    }

    // ---- link / unlink ------------------------------------------------------

    /// **Link** `ids` into one group (a new link id; clips already in another group
    /// leave it, and a group left with one member dissolves). Needs at least two
    /// clips, **on different tracks** — a link joins a picture to its sound, and two
    /// clips of one lane cannot be carried by one edit — none of them on a locked
    /// track. Returns the group's id. Linking clips that are already exactly one
    /// group is an error, so a no-op records no revision.
    pub fn link_clips(&mut self, ids: &[Uuid]) -> Result<Uuid> {
        let mut seen = HashSet::new();
        let mut lanes: HashMap<usize, Uuid> = HashMap::new();
        for id in ids {
            let (ti, _) = self.locate(*id).ok_or(Error::ClipNotFound(*id))?;
            if !seen.insert(*id) {
                return Err(Error::InvalidArgument(format!("clip {id} appears more than once")));
            }
            if self.tracks[ti].locked {
                return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
            }
            if lanes.insert(ti, *id).is_some() {
                return Err(Error::InvalidArgument(format!(
                    "two of the clips are on track {} — a link joins one clip per track (a picture and its sound)",
                    self.tracks[ti].name
                )));
            }
        }
        if seen.len() < 2 {
            return Err(Error::InvalidArgument("linking needs at least two clips".to_string()));
        }
        let old: HashSet<Uuid> = ids.iter().filter_map(|id| self.clip(*id).and_then(|c| c.link_id)).collect();
        let already_one_group = old.len() == 1
            && ids.iter().all(|id| self.clip(*id).is_some_and(|c| c.link_id.is_some()))
            && self.with_link_partners(ids).len() == seen.len();
        if already_one_group {
            return Err(Error::InvalidArgument("those clips are already linked".to_string()));
        }
        let group = Uuid::new_v4();
        for id in ids {
            self.clip_mut(*id).expect("located above").link_id = Some(group);
        }
        self.dissolve_orphans(&old);
        Ok(group)
    }

    /// **Unlink** `ids`: each leaves its group, and a group left with a single clip
    /// dissolves (so unlinking either half of a pair unlinks the pair). Errors when
    /// none of them was linked, or one is on a locked track. Returns how many of
    /// the named clips were linked.
    pub fn unlink_clips(&mut self, ids: &[Uuid]) -> Result<usize> {
        let mut old = HashSet::new();
        let mut linked = 0;
        for id in ids {
            let (ti, ci) = self.locate(*id).ok_or(Error::ClipNotFound(*id))?;
            if self.tracks[ti].locked {
                return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
            }
            if let Some(link) = self.tracks[ti].clips[ci].link_id {
                old.insert(link);
                linked += 1;
            }
        }
        if linked == 0 {
            return Err(Error::InvalidArgument("none of those clips is linked".to_string()));
        }
        for id in ids {
            self.clip_mut(*id).expect("located above").link_id = None;
        }
        self.dissolve_orphans(&old);
        Ok(linked)
    }

    // ---- detach / reattach --------------------------------------------------

    /// The audio lane to put a detached clip spanning `span` on: the audio track at
    /// the picture track's own position (V1 → A1, V2 → A2) when it has room, else
    /// the first audio track that does; never a locked one nor one in `avoid`. With
    /// `fader` — the clip's sound must meet the same fader it did, because something on
    /// its chain reacts to level — only a lane at exactly that fader will do; without
    /// it, any lane whose fader is not at zero (the picture's level is carried onto it
    /// by scaling the clip). `None` when no audio track will do.
    fn audio_lane_for(&self, video_track: usize, span: (f64, f64), avoid: &HashSet<usize>, fader: Option<f32>) -> Option<usize> {
        let ordinal = self.tracks[..video_track]
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .count();
        let audio: Vec<usize> = (0..self.tracks.len())
            .filter(|&i| self.tracks[i].kind == StreamKind::Audio)
            .collect();
        let preferred = audio.get(ordinal).copied();
        preferred
            .into_iter()
            .chain(audio.iter().copied().filter(|&i| Some(i) != preferred))
            .find(|&i| {
                let track = &self.tracks[i];
                !track.locked
                    && fader.map_or(track.volume > MIN_FADER, |f| (track.volume - f).abs() <= FADER_EPS)
                    && !avoid.contains(&i)
                    && !track
                        .clips
                        .iter()
                        .any(|c| spans_overlap(span, (c.timeline_start, c.timeline_end())))
            })
    }

    /// **Detach** a picture clip's own sound: a new audio clip with the **same source
    /// span, speed and timeline position** goes on an audio track (a new one when
    /// none has room), is linked to the picture clip, and the picture clip's own
    /// sound is muted (`Clip::source_audio` false) — so the sound is heard once,
    /// from the audio track.
    ///
    /// **What is heard stays the same where it can be:** the level. A video track's
    /// fader rides its clips' own sound, and the audio track the sound moves to has
    /// a fader of its own, so the new clip's gain is `volume × picture track's fader
    /// ÷ audio track's fader` — through that fader it comes out exactly as loud as
    /// it was, **provided everything on the clip's chain is linear** (a lane whose
    /// fader is at zero cannot take it and is not chosen). A **compressor or gate** is
    /// not: folding the fader into the clip's volume would move the gain ahead of it,
    /// and it would react to a different level. So when the chain has one and the
    /// faders differ, the sound goes to a lane whose fader *equals* the picture
    /// track's — an existing one with room, else a **new audio track at that fader** —
    /// and the volume is left alone. A **keyed volume** rides over the same way (its keys
    /// times the ratio) unless that would push a key past [`MAX_CHANNEL_VOLUME`], which a
    /// keyed volume is read held to and a static one is not: the sound then takes the
    /// equal-fader route too, with its keys as they are, rather than being quietly turned
    /// down. What *cannot* be carried is the rest of the
    /// destination's strip: its **pan**, its **duck** flag and its **mute / solo** now
    /// decide how the sound is mixed, where the picture track's did before — a pan on
    /// V1 no longer leans the dialogue, and a muted or ducked A1 changes it.
    ///
    /// The audio clip also carries what shapes the *sound* itself: audio effects,
    /// fades and the transition (a crossfade or dip fades the sound too). The picture
    /// clip keeps its own copies, inert while muted, so [`Timeline::reattach_audio`]
    /// restores it as it was. `has_audio` is whether the clip's asset carries an
    /// audio stream (the timeline does not know). Refuses a clip that is not on a
    /// video track, whose sound is already detached, or whose track — or the
    /// destination — is locked.
    pub fn detach_audio(&mut self, clip_id: Uuid, has_audio: bool) -> Result<Detached> {
        let (vi, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let clip = self.tracks[vi].clips[ci].clone();
        if self.tracks[vi].kind != StreamKind::Video {
            return Err(Error::InvalidArgument(
                "only a clip on a video track has sound of its own to detach".to_string(),
            ));
        }
        if self.tracks[vi].locked {
            return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[vi].name)));
        }
        if !has_audio {
            return Err(Error::InvalidArgument("the clip's asset has no audio stream".to_string()));
        }
        if !clip.source_audio {
            return Err(Error::InvalidArgument("this clip's sound is already detached".to_string()));
        }
        // A track already holding a member of the group cannot take another.
        let avoid: HashSet<usize> = self
            .link_partners(clip_id)
            .iter()
            .filter_map(|p| self.locate(*p).map(|(ti, _)| ti))
            .collect();
        let span = (clip.timeline_start, clip.timeline_end());
        let picture_fader = self.tracks[vi].volume;
        // Something that reacts to level must meet the same fader it did.
        let mut fader = clip.audio.iter().any(AudioEffect::is_dynamic).then_some(picture_fader);
        let mut lane = self.audio_lane_for(vi, span, &avoid, fader);
        // A keyed volume is read held to 0..=MAX_CHANNEL_VOLUME and a static one is not, so
        // folding the fader ratio into its keys could push one past the cap and quietly lower
        // the sound. When it would, carry the keys as they are to a lane whose fader equals the
        // picture track's (the same choice a compressor makes), where the ratio is 1.
        if fader.is_none() && clip.is_keyed(Property::Volume) {
            let peak = clip
                .property_keys(Property::Volume)
                .iter()
                .fold(0.0_f64, |most, k| most.max(k.value));
            let dest = lane.map_or(Track::new(StreamKind::Audio, "").volume, |i| self.tracks[i].volume);
            if peak * fader_ratio(picture_fader, dest) > MAX_CHANNEL_VOLUME {
                fader = Some(picture_fader);
                lane = self.audio_lane_for(vi, span, &avoid, fader);
            }
        }
        let (lane_ix, created_track) = match lane {
            Some(lane_ix) => (lane_ix, false),
            None => {
                let count = self.tracks.iter().filter(|t| t.kind == StreamKind::Audio).count();
                let mut track = Track::new(StreamKind::Audio, format!("A{}", count + 1));
                if let Some(f) = fader {
                    track.volume = f;
                }
                self.tracks.push(track);
                (self.tracks.len() - 1, true)
            }
        };
        let group = clip.link_id.unwrap_or_else(Uuid::new_v4);
        let mut audio = Clip::new(clip.asset_id, clip.source_in, clip.source_out, clip.timeline_start);
        audio.speed = clip.speed;
        // The picture track's fader rode this sound; the destination's rides it now.
        let dest_fader = self.tracks[lane_ix].volume;
        audio.volume = if (picture_fader - dest_fader).abs() <= FADER_EPS {
            clip.volume
        } else {
            clip.volume * picture_fader / dest_fader
        };
        // A keyed volume *is* the gain, so it rides over with the sound, through the same
        // fader ratio the static one is (the lane was chosen above so that this cannot push a
        // key past the cap it is read under).
        if let Some(track) = clip.channel(Property::Volume).filter(|t| !t.keys.is_empty()) {
            let ratio = fader_ratio(picture_fader, dest_fader);
            audio.channels.push(PropertyTrack {
                prop: Property::Volume,
                keys: track
                    .keys
                    .iter()
                    .map(|k| PropertyKey {
                        value: k.value * ratio,
                        ..*k
                    })
                    .collect(),
            });
        }
        audio.fade_in = clip.fade_in;
        audio.fade_out = clip.fade_out;
        audio.audio = clip.audio.clone();
        audio.transition_in = clip.transition_in;
        audio.enabled = clip.enabled;
        audio.link_id = Some(group);
        let track_id = self.tracks[lane_ix].id;
        self.tracks[lane_ix].clips.push(audio.clone());
        self.tracks[lane_ix].sort_by_start();
        let picture = &mut self.tracks[vi].clips[ci];
        picture.source_audio = false;
        picture.link_id = Some(group);
        Ok(Detached {
            clip: audio,
            track_id,
            created_track,
        })
    }

    /// **Detach** several picture clips at once. A clip that cannot be detached
    /// (not on a video track, no audio, already detached, a locked track) is
    /// **skipped and reported** rather than failing the rest — a selection or an
    /// asset's every use is rarely uniform. `has_audio` answers, per asset id,
    /// whether it carries an audio stream. Fails (with the first reason) only when
    /// nothing at all could be detached, so a caller never records an empty edit.
    pub fn detach_audio_many(&mut self, ids: &[Uuid], has_audio: &dyn Fn(Uuid) -> bool) -> Result<DetachedMany> {
        let mut out = DetachedMany::default();
        let mut seen = HashSet::new();
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            let asset = self.clip(*id).map(|c| c.asset_id);
            let attempt = match asset {
                Some(asset) => self.detach_audio(*id, has_audio(asset)),
                None => Err(Error::ClipNotFound(*id)),
            };
            match attempt {
                Ok(detached) => out.detached.push(detached),
                Err(e) => out.skipped.push(SkippedDetach {
                    clip_id: *id,
                    reason: e.to_string(),
                }),
            }
        }
        if out.detached.is_empty() {
            return Err(Error::InvalidArgument(
                out.skipped
                    .first()
                    .map(|s| s.reason.clone())
                    .unwrap_or_else(|| "no clips to detach".to_string()),
            ));
        }
        Ok(out)
    }

    /// Whether `picture` would be heard **twice** if it played its own sound: some
    /// *other* clip on an audio track carries the same footage in step with it
    /// (`content_offset`), over time the two share — a detached copy that was
    /// unlinked, a duplicated audio clip. `without` are clips about to be deleted.
    fn sound_already_playing(&self, picture: &Clip, without: &HashSet<Uuid>) -> bool {
        self.tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Audio)
            .flat_map(|t| t.clips.iter())
            .any(|c| {
                !without.contains(&c.id)
                    && c.asset_id == picture.asset_id
                    && (c.speed - picture.speed).abs() < STEP_EPS
                    && (content_offset(c) - content_offset(picture)).abs() < STEP_EPS
                    && spans_overlap(
                        (c.timeline_start, c.timeline_end()),
                        (picture.timeline_start, picture.timeline_end()),
                    )
            })
    }

    /// The picture a reattach of `clip_id` means: the clip itself when it is on a video
    /// track, else the linked picture on one whose sound was detached.
    fn detached_picture(&self, clip_id: Uuid) -> Result<Uuid> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        if self.tracks[ti].kind == StreamKind::Video {
            return Ok(clip_id);
        }
        let named = &self.tracks[ti].clips[ci];
        self.link_partners(clip_id)
            .into_iter()
            .find(|p| {
                self.clip(*p).is_some_and(|c| !c.source_audio && c.asset_id == named.asset_id)
                    && self
                        .locate(*p)
                        .is_some_and(|(pt, _)| self.tracks[pt].kind == StreamKind::Video)
            })
            .ok_or_else(|| Error::InvalidArgument("no linked picture whose sound was detached".to_string()))
    }

    /// **Reattach** detached sound: the audio clip(s) linked to the picture clip that
    /// carry the same asset are deleted and the picture clip plays its own sound
    /// again, exactly as it was before the detach (edits made to the audio clip are
    /// not carried back). Name either the picture clip or its audio clip. A picture
    /// whose audio clip is already gone is just unmuted — **unless** some other audio
    /// clip is already playing the same footage in step with it, in which case
    /// unmuting would double the sound and the reattach is refused with that reason.
    /// Returns the picture clip.
    pub fn reattach_audio(&mut self, clip_id: Uuid) -> Result<Clip> {
        let picture_id = self.detached_picture(clip_id)?;
        let (vi, vc) = self.locate(picture_id).expect("the picture is on the timeline");
        let picture = self.tracks[vi].clips[vc].clone();
        if picture.source_audio {
            return Err(Error::InvalidArgument("this clip's sound is not detached".to_string()));
        }
        if self.tracks[vi].locked {
            return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[vi].name)));
        }
        let doomed: Vec<Uuid> = self
            .link_partners(picture_id)
            .into_iter()
            .filter(|p| {
                self.locate(*p).is_some_and(|(pt, pc)| {
                    self.tracks[pt].kind == StreamKind::Audio && self.tracks[pt].clips[pc].asset_id == picture.asset_id
                })
            })
            .collect();
        for p in &doomed {
            let (pt, _) = self.locate(*p).expect("located above");
            if self.tracks[pt].locked {
                return Err(locked_partner(&self.tracks[pt]));
            }
        }
        let doomed: HashSet<Uuid> = doomed.into_iter().collect();
        if self.sound_already_playing(&picture, &doomed) {
            return Err(Error::InvalidArgument(
                "this clip's sound is already playing from another audio clip — remove that clip first, or the sound would be heard twice".to_string(),
            ));
        }
        for track in &mut self.tracks {
            track.clips.retain(|c| !doomed.contains(&c.id));
        }
        self.clip_mut(picture_id).expect("the picture stays").source_audio = true;
        if let Some(group) = picture.link_id {
            self.dissolve_orphans(&HashSet::from([group]));
        }
        Ok(self.clip(picture_id).expect("the picture stays").clone())
    }

    /// [`Timeline::reattach_audio`] on several clips, **all or nothing**. Each id names a
    /// picture or its sound; a picture named twice (itself and its sound, as a selection
    /// does) is reattached once. Every reattach is judged against the cut the earlier ones
    /// left, so they run on a copy that replaces `self` only when all went through; the
    /// first refusal is the error (naming its clip when there are several) and `self` is as
    /// it was. Returns the pictures in the order named.
    pub fn reattach_audio_many(&mut self, ids: &[Uuid]) -> Result<Vec<Clip>> {
        if ids.is_empty() {
            return Err(Error::InvalidArgument("no clips to reattach".to_string()));
        }
        let mut pictures: Vec<Uuid> = Vec::new();
        for id in ids {
            let picture = self.detached_picture(*id)?;
            if !pictures.contains(&picture) {
                pictures.push(picture);
            }
        }
        let many = pictures.len() > 1;
        let mut scratch = self.clone();
        let mut out = Vec::with_capacity(pictures.len());
        for picture in pictures {
            match scratch.reattach_audio(picture) {
                Ok(clip) => out.push(clip),
                Err(Error::InvalidArgument(why)) if many => {
                    return Err(Error::InvalidArgument(format!("{why} (clip {picture})")));
                }
                Err(e) => return Err(e),
            }
        }
        *self = scratch;
        Ok(out)
    }

    // ---- move ---------------------------------------------------------------

    /// `moves` widened with the clips linked to the ones it moves: each partner not
    /// already named moves by the same Δt (the named clip's new start minus its old
    /// one) and **stays on its own track** — a track change belongs to the clip that
    /// was named. Where the request names two members of one group, the first's Δt
    /// is the group's. A partner that would start before 0, or sits on a locked
    /// track, is an error — the whole move is refused. The named moves come first,
    /// unchanged, so the answer can be read by request position.
    pub fn with_linked_moves(&self, moves: &[ClipMove]) -> Result<Vec<ClipMove>> {
        let named: HashSet<Uuid> = moves.iter().map(|m| m.clip_id).collect();
        let index = self.link_index();
        let mut out = moves.to_vec();
        let mut added: HashSet<Uuid> = HashSet::new();
        for m in moves {
            let Some(clip) = self.clip(m.clip_id) else {
                continue; // `move_clips` reports the unknown clip
            };
            let delta = m.timeline_start - clip.timeline_start;
            if !delta.is_finite() || delta.abs() <= DIFF_EPS {
                continue;
            }
            for partner_id in index.partners(m.clip_id) {
                if named.contains(&partner_id) || !added.insert(partner_id) {
                    continue;
                }
                let (pt, pc) = self.locate(partner_id).expect("a partner is on the timeline");
                if self.tracks[pt].locked {
                    return Err(locked_partner(&self.tracks[pt]));
                }
                let start = self.tracks[pt].clips[pc].timeline_start + delta;
                if start < -DIFF_EPS {
                    return Err(Error::InvalidArgument(format!(
                        "moving the clip that far would take its linked clip on {} before the beginning of the timeline",
                        self.tracks[pt].name
                    )));
                }
                out.push(ClipMove {
                    clip_id: partner_id,
                    timeline_start: start.max(0.0),
                    track_id: None,
                });
            }
        }
        Ok(out)
    }

    // ---- trim ---------------------------------------------------------------

    /// `clip_id` has just been edited from `was` to what it is now: carry the change
    /// to its linked partners. A **move** (same length, new start) moves them by the
    /// same Δt. A **trim** moves a partner's edge by the same amount *when the
    /// partner shares that edge* with the clip as it was (within [`ADJACENT_EPS`]) —
    /// a partner that was never in sync at that edge is left alone there — clamped
    /// to the footage the partner has (the partner's end stops where its asset's
    /// footage does, and the pair then differs by what it lacked). It writes no lane
    /// check of its own, because the ripple pass may yet make room: whoever calls it
    /// hands the partners it returns to [`Timeline::check_carried_lanes`] once the
    /// ripple has run (`Project::run_edit` does). An edit that changed the clip's
    /// speed is not this function's (`set_speed_linked`).
    ///
    /// A **sound** carried before 0 loses what hangs off the front (and its track is
    /// pushed to `notes`); a **picture** is never trimmed to fit and refuses, as does a
    /// clip that would be left under [`MIN_EDIT_CLIP`]. Errors, and the caller discards
    /// the edit, when a partner is on a locked track or would be trimmed away entirely.
    /// Returns the partners as they stand afterwards.
    pub fn carry_extent_edit(&mut self, clip_id: Uuid, was: &Clip, footage: &SourceLimits) -> Result<Vec<Clip>> {
        self.carry_extent_edit_noted(clip_id, was, footage, &mut Vec::new())
    }

    /// [`Timeline::carry_extent_edit`] reporting the sounds it trimmed in `notes`.
    pub fn carry_extent_edit_noted(
        &mut self,
        clip_id: Uuid,
        was: &Clip,
        footage: &SourceLimits,
        notes: &mut Vec<String>,
    ) -> Result<Vec<Clip>> {
        let now = self.clip(clip_id).ok_or(Error::ClipNotFound(clip_id))?.clone();
        let looping = footage.get(&now.asset_id).is_some_and(|l| l.is_infinite());
        let Some(edit) = extent_edit(was, &now, looping) else {
            return Ok(Vec::new());
        };
        let skip = HashSet::from([clip_id]);
        // Every partner is worked out before any is written, so a refusal changes nothing.
        let mut updates: Vec<(usize, usize, Clip)> = Vec::new();
        let mut trimmed: Vec<String> = Vec::new();
        for partner_id in self.unlocked_partners(clip_id, &skip)? {
            let (pt, pc) = self.locate(partner_id).expect("a partner is on the timeline");
            let mut p = self.tracks[pt].clips[pc].clone();
            let limit = footage.get(&p.asset_id).copied().unwrap_or(f64::INFINITY);
            let p_looping = limit.is_infinite();
            let head_shared = edit.head.abs() > DIFF_EPS && (p.timeline_start - was.timeline_start).abs() <= ADJACENT_EPS;
            let tail_shared = edit.tail.abs() > DIFF_EPS && (p.timeline_end() - was.timeline_end()).abs() <= ADJACENT_EPS;
            if edit.shift.abs() > DIFF_EPS {
                p.timeline_start += edit.shift;
            }
            let (head_room, tail_room) = p.handles(limit);
            if head_shared {
                // Pulling the start earlier (negative) is limited to the footage before
                // the window; shortening from the front is not.
                let by = if edit.head < 0.0 && !p_looping {
                    edit.head.max(-head_room)
                } else {
                    edit.head
                };
                p.move_head(by, p_looping);
            }
            if tail_shared {
                let by = if edit.tail > 0.0 && !p_looping {
                    edit.tail.min(tail_room)
                } else {
                    edit.tail
                };
                p.move_tail(by, p_looping);
            }
            if p.duration() <= DIFF_EPS {
                return Err(Error::InvalidArgument(format!(
                    "the linked clip on {} would be trimmed away by this edit",
                    self.tracks[pt].name
                )));
            }
            if p.timeline_start < -DIFF_EPS {
                // Carried before 0: what hangs off the front of a sound is cut away —
                // losing the head keeps the clip in step, moving it would not. A picture
                // is never trimmed to fit.
                if self.tracks[pt].kind == StreamKind::Video {
                    return Err(Error::InvalidArgument(format!(
                        "the linked clip on {} would start before the beginning of the timeline — a picture is never trimmed to fit",
                        self.tracks[pt].name
                    )));
                }
                let over = -p.timeline_start;
                if p.duration() - over < MIN_EDIT_CLIP {
                    return Err(Error::InvalidArgument(format!(
                        "the linked clip on {} would be left under {MIN_EDIT_CLIP}s by the beginning of the timeline",
                        self.tracks[pt].name
                    )));
                }
                p.move_head(over, p_looping);
                trimmed.push(self.tracks[pt].name.clone());
            }
            p.timeline_start = p.timeline_start.max(0.0);
            p.clamp_fades();
            updates.push((pt, pc, p));
        }
        notes.extend(trimmed);
        let mut out = Vec::with_capacity(updates.len());
        for (pt, pc, p) in updates {
            self.tracks[pt].clips[pc] = p.clone();
            out.push(p);
        }
        Ok(out)
    }

    /// [`Timeline::carry_extent_edit`] for every clip an edit changed, read off a
    /// snapshot: a lane-level op (the beat snap) retimes clips without knowing about
    /// links, and this carries each one's change to its partners afterwards. A group
    /// where **more than one** member changed is left alone — the edit named them
    /// explicitly — and so is a partner that changed itself.
    pub fn carry_links_since(&mut self, before: &Timeline, footage: &SourceLimits) -> Result<()> {
        self.carry_links_since_noted(before, footage, &mut Vec::new()).map(|_| ())
    }

    /// [`Timeline::carry_links_since`] reporting the sounds it trimmed in `notes` and
    /// returning the partners it carried, for [`Timeline::check_carried_lanes`].
    pub fn carry_links_since_noted(
        &mut self,
        before: &Timeline,
        footage: &SourceLimits,
        notes: &mut Vec<String>,
    ) -> Result<Vec<Uuid>> {
        let mut carried = Vec::new();
        if !self.tracks.iter().any(|t| t.clips.iter().any(|c| c.link_id.is_some())) {
            return Ok(carried);
        }
        let changed = |was: &Clip, now: &Clip| {
            (was.timeline_start - now.timeline_start).abs() > DIFF_EPS
                || (was.source_in - now.source_in).abs() > DIFF_EPS
                || (was.source_out - now.source_out).abs() > DIFF_EPS
        };
        let mut drivers: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for clip in self.tracks.iter().flat_map(|t| t.clips.iter()) {
            let (Some(link), Some(was)) = (clip.link_id, before.clip(clip.id)) else {
                continue;
            };
            if changed(was, clip) {
                drivers.entry(link).or_default().push(clip.id);
            }
        }
        for ids in drivers.into_values() {
            // Exactly one changed member means no partner changed on its own.
            let [driver] = ids.as_slice() else { continue };
            let was = before.clip(*driver).expect("a driver was on the timeline before").clone();
            let partners = self.carry_extent_edit_noted(*driver, &was, footage, notes)?;
            carried.extend(partners.iter().map(|p| p.id));
        }
        Ok(carried)
    }

    /// The lane check for the partners a trim carried ([`Timeline::carry_extent_edit`]),
    /// run on the timeline **after** the per-lane ripple and the sync lock, because the
    /// ripple is what makes room: a sound extended with its picture's tail pushes the
    /// clips behind it. A partner that now overlaps a clip of its lane that is not in its
    /// own group, where the two did not overlap in `before`, refuses the edit — the rule
    /// [`Timeline::move_clips`] holds a moved partner to (an overlap that was already
    /// there is old news). The named clip's own lane is not looked at, as a trim never has.
    pub fn check_carried_lanes(&self, before: &Timeline, carried: &[Uuid]) -> Result<()> {
        if carried.is_empty() {
            return Ok(());
        }
        let prior: HashMap<Uuid, &Clip> = before.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect();
        let span = |c: &Clip| (c.timeline_start, c.timeline_end());
        for track in &self.tracks {
            for p in track.clips.iter().filter(|c| carried.contains(&c.id)) {
                for q in &track.clips {
                    let same_group = p.link_id.is_some() && q.link_id == p.link_id;
                    if q.id == p.id || same_group || !spans_overlap(span(p), span(q)) {
                        continue;
                    }
                    if matches!((prior.get(&p.id), prior.get(&q.id)), (Some(a), Some(b)) if spans_overlap(span(a), span(b))) {
                        continue;
                    }
                    return Err(runs_into_unlinked(&track.name, p.timeline_start.max(q.timeline_start)));
                }
            }
        }
        Ok(())
    }

    // ---- split --------------------------------------------------------------

    /// Split one clip at timeline time `at` into two adjacent halves; the right half
    /// is a new clip (new id, no transition — that stays with the left — and **no
    /// link**: [`Timeline::split_clip_linked`] links the new halves of a group).
    /// Each fade stays with the half that holds its edge: the left keeps the fade-in
    /// and the right the fade-out, and the fades at the cut are dropped — a fade-out on
    /// the left half would dip the picture and the sound to black at the split. A fade
    /// longer than the half it stays on is clamped to it, as for any clip that shrank.
    /// `at` must lie strictly inside the clip. Returns `(left, right)`.
    pub fn split_clip(&mut self, clip_id: Uuid, at: f64) -> Result<(Clip, Clip)> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let clip = self.tracks[ti].clips[ci].clone();
        if at <= clip.timeline_start || at >= clip.timeline_end() {
            return Err(Error::InvalidArgument(
                "split point must lie strictly inside the clip".to_string(),
            ));
        }
        // Map the timeline split point to a source point honoring speed (the
        // source advances by |speed| per timeline second), and backwards for a
        // reversed clip, so the two halves stay gapless and keep total duration.
        let offset = (at - clip.timeline_start) * clip.speed_mag();
        let (mut left, mut right) = (clip.clone(), clip);
        right.id = Uuid::new_v4();
        right.timeline_start = at;
        right.transition_in = None; // the transition stays with the left (start) half
        right.link_id = None;
        // The animation is clip-local: the right half starts `at - start` into it, so it
        // opens on the pose the whole clip had there and keeps the keys after it (without
        // this the right half played the animation again from its first key).
        right.rebase_animation(at - left.timeline_start);
        if left.is_reversed() {
            let split_src = (left.source_out - offset).clamp(left.source_in, left.source_out);
            left.source_in = split_src;
            right.source_out = split_src;
        } else {
            let split_src = (left.source_in + offset).clamp(left.source_in, left.source_out);
            left.source_out = split_src;
            right.source_in = split_src;
        }
        left.fade_out = 0.0;
        right.fade_in = 0.0;
        left.clamp_fades();
        right.clamp_fades();
        self.tracks[ti].clips[ci] = left.clone();
        self.tracks[ti].clips.insert(ci + 1, right.clone());
        Ok((left, right))
    }

    /// **Split** `clip_id` at `at` *and* every linked partner that has `at` inside it
    /// (a partner that does not reach that moment is left whole). The group then
    /// falls in two, by **side**: the left halves, and any partner that lies wholly
    /// before `at`, keep the group; the right halves, and any partner that lies
    /// wholly at or after `at`, form a new one. (A partner left whole after the cut
    /// belongs to one side or the other — leaving it with the left half when it lies
    /// after the cut would let moving the right half silently desynchronize it.) A
    /// side of one clip is no group. A partner on a locked track that would be split
    /// refuses the whole edit. Returns the named clip's `(left, right)`.
    pub fn split_clip_linked(&mut self, clip_id: Uuid, at: f64) -> Result<(Clip, Clip)> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let group = self.tracks[ti].clips[ci].link_id;
        let index = self.link_index();
        let (mut cut, mut before_at, mut after_at) = (Vec::new(), Vec::new(), Vec::new());
        for partner in index.partners(clip_id) {
            let clip = self.clip(partner).expect("a partner is on the timeline");
            if clip.timeline_start + DIFF_EPS < at && at < clip.timeline_end() - DIFF_EPS {
                cut.push(partner);
            } else if clip.timeline_start + DIFF_EPS >= at {
                after_at.push(partner);
            } else {
                before_at.push(partner);
            }
        }
        for partner in &cut {
            let (ti, _) = self.locate(*partner).expect("a partner is on the timeline");
            if self.tracks[ti].locked {
                return Err(locked_partner(&self.tracks[ti]));
            }
        }
        let (left, right) = self.split_clip(clip_id, at)?;
        let (mut lefts, mut rights) = (vec![clip_id], vec![right.id]);
        for partner in cut {
            let (_, partner_right) = self.split_clip(partner, at)?;
            lefts.push(partner);
            rights.push(partner_right.id);
        }
        lefts.extend(before_at);
        rights.extend(after_at);
        self.relink_sides(group, &lefts, &rights);
        let right = self.clip(right.id).expect("the right half is on the timeline").clone();
        let left = self.clip(left.id).expect("the left half is on the timeline").clone();
        Ok((left, right))
    }

    // ---- remove -------------------------------------------------------------

    /// Remove a clip and close the gap it leaves: every later clip on the **same
    /// track** shifts left by its duration.
    pub fn ripple_delete_clip(&mut self, clip_id: Uuid) -> Result<()> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let removed = self.tracks[ti].clips[ci].clone();
        let dur = removed.duration();
        let from = removed.timeline_start;
        self.tracks[ti].clips.remove(ci);
        for c in &mut self.tracks[ti].clips {
            if c.timeline_start >= from {
                c.timeline_start = (c.timeline_start - dur).max(0.0);
            }
        }
        Ok(())
    }

    /// [`Timeline::ripple_delete_clip`] on the clip, and its linked partners removed
    /// with it. The named clip's track closes the gap by **its** length; the
    /// partners' tracks do not close one of their own — the clips that were pushed
    /// left take their linked partners with them ([`Timeline::conform_links`], the
    /// named clip's track the authority), so a J-cut or L-cut pair, whose sound is
    /// longer or shorter than its picture, still closes up by the amount of
    /// *picture* removed and every later pair stays in step. An unlinked clip on a
    /// partner's track stays where it was. A partner on a locked track refuses the
    /// lot. Returns how many clips were deleted.
    pub fn ripple_delete_linked(&mut self, clip_id: Uuid) -> Result<usize> {
        self.ripple_delete_linked_noted(clip_id, &mut Vec::new())
    }

    /// [`Timeline::ripple_delete_linked`] reporting the sounds the sync lock trimmed in `notes`.
    pub fn ripple_delete_linked_noted(&mut self, clip_id: Uuid, notes: &mut Vec<String>) -> Result<usize> {
        self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let partners = self.unlocked_partners(clip_id, &HashSet::from([clip_id]))?;
        let mut scratch = self.clone();
        scratch.ripple_delete_clip(clip_id)?;
        let doomed: HashSet<Uuid> = partners.iter().copied().collect();
        for track in &mut scratch.tracks {
            track.clips.retain(|c| !doomed.contains(&c.id));
        }
        scratch.conform_links_noted(self, &HashSet::from([clip_id]), &HashMap::new(), None, notes)?;
        *self = scratch;
        Ok(1 + partners.len())
    }

    /// [`Timeline::remove_clips`] on `ids` and every clip linked to one of them, all or
    /// nothing: a partner on a locked track refuses the lot, with the error that names
    /// the link. Returns how many clips were removed (partners included).
    pub fn remove_clips_linked(&mut self, ids: &[Uuid]) -> Result<usize> {
        let named: HashSet<Uuid> = ids.iter().copied().collect();
        let index = self.link_index();
        for id in ids {
            if self.locate(*id).is_some() {
                self.unlocked_partners_in(&index, *id, &named)?;
            }
        }
        let all = self.with_link_partners(ids);
        self.remove_clips(&all)
    }

    // ---- cut a source range -------------------------------------------------

    /// The cut itself: split `clip_id` around the intersection of `[from, to]` with
    /// its source window and drop the middle. Returns the `(head, tail)` pieces that
    /// survive (in play order — a reversed clip plays the upper span first), and how
    /// much **timeline** the cut removed. A piece that is the sole survivor keeps the
    /// original id and both fades (the cut is just a trim); otherwise the fades
    /// facing the removed middle are dropped and the tail is a new clip with no link.
    /// A tail piece starts after the head and the removed middle, so its animation is
    /// re-timed to open on the pose the clip had there. With `close_gap`, later clips on the
    /// track ripple left over the removed span; without, the lane is left for the caller to
    /// settle.
    fn cut_range_pieces(
        &mut self,
        clip_id: Uuid,
        from: f64,
        to: f64,
        close_gap: bool,
    ) -> Result<(Option<Clip>, Option<Clip>, f64)> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let clip = self.tracks[ti].clips[ci].clone();
        let a = from.max(clip.source_in);
        let b = to.min(clip.source_out);
        if b - a <= 1e-9 {
            return Err(Error::InvalidArgument(
                "range does not overlap the clip's source window".to_string(),
            ));
        }
        let removed = (b - a) / clip.speed_mag();

        let (head, tail) = if clip.is_reversed() {
            ((b, clip.source_out), (clip.source_in, a))
        } else {
            ((clip.source_in, a), (b, clip.source_out))
        };
        let head_ok = head.1 - head.0 > 1e-9;
        let tail_ok = tail.1 - tail.0 > 1e-9;
        let mut head_piece = None;
        let mut tail_piece = None;
        let mut cursor = clip.timeline_start;
        if head_ok {
            let mut p = clip.clone();
            (p.source_in, p.source_out) = head;
            p.timeline_start = cursor;
            if tail_ok {
                p.fade_out = 0.0;
            }
            cursor = p.timeline_end();
            head_piece = Some(p);
        }
        if tail_ok {
            let mut p = clip.clone();
            (p.source_in, p.source_out) = tail;
            p.timeline_start = cursor;
            // The animation is clip-local: the tail starts after the head and the removed
            // middle, so it opens on the pose the whole clip had there (without this it
            // replayed the animation from its first key; a sole-surviving tail is a head trim).
            p.rebase_animation((head.1 - head.0 + (b - a)) / clip.speed_mag());
            if head_ok {
                p.id = Uuid::new_v4();
                p.fade_in = 0.0;
                p.transition_in = None;
                p.link_id = None;
            }
            tail_piece = Some(p);
        }

        let track = &mut self.tracks[ti];
        track.clips.remove(ci);
        if close_gap {
            for c in &mut track.clips {
                if c.timeline_start > clip.timeline_start + 1e-9 {
                    c.timeline_start = (c.timeline_start - removed).max(0.0);
                }
            }
        }
        track.clips.extend(head_piece.iter().chain(tail_piece.iter()).cloned());
        track.sort_by_start();
        Ok((head_piece, tail_piece, removed))
    }

    /// Cut a **source-time** range out of a clip: the clip is split around the
    /// intersection of `[from, to]` with its source window, the middle piece
    /// removed, and later clips on the track ripple left to close the gap. Returns
    /// the kept pieces in play order. A tail piece is a new clip with no link.
    pub fn cut_clip_range(&mut self, clip_id: Uuid, from: f64, to: f64) -> Result<Vec<Clip>> {
        let (head, tail, _) = self.cut_range_pieces(clip_id, from, to, true)?;
        Ok(head.into_iter().chain(tail).collect())
    }

    /// [`Timeline::cut_clip_range`] on the clip *and* its linked partners: the
    /// stretch of **timeline** the cut removes is taken out of every partner it
    /// overlaps too (a partner of another asset, or sitting at another offset, loses
    /// the same moment, not the same source span). The named clip's track closes up
    /// by the stretch; every other track gets its **linked** clips put back in step
    /// with what survived — a partner wholly after the stretch moves up by it, one
    /// whose head was inside the stretch **resumes at the cut**, one that spanned it
    /// is cut in two and its tail follows — and only those: an unlinked clip on a
    /// partner's track stays where it was ([`Timeline::conform_links`]). A partner the
    /// cut misses and that lies before it is untouched; one the cut overlaps on a
    /// locked track refuses the lot. The partners' surviving pieces after the stretch
    /// are moved explicitly, so a piece whose group no longer has a second member
    /// (the named clip left nothing after the cut) still lands where the footage it
    /// shows now plays.
    ///
    /// The group then falls in two by side, as for a split: what is before the
    /// stretch keeps the link, what is after it (the tail pieces, and the partners
    /// that moved up to meet them) gets a new one. Returns the named clip's kept
    /// pieces.
    pub fn cut_clip_range_linked(&mut self, clip_id: Uuid, from: f64, to: f64) -> Result<Vec<Clip>> {
        self.cut_clip_range_linked_noted(clip_id, from, to, &mut Vec::new())
    }

    /// [`Timeline::cut_clip_range_linked`] reporting the sounds trimmed to make room in `notes`.
    pub fn cut_clip_range_linked_noted(
        &mut self,
        clip_id: Uuid,
        from: f64,
        to: f64,
        notes: &mut Vec<String>,
    ) -> Result<Vec<Clip>> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let clip = self.tracks[ti].clips[ci].clone();
        let group = clip.link_id;
        let a = from.max(clip.source_in);
        let b = to.min(clip.source_out);
        let index = self.link_index();
        let partners: Vec<Uuid> = index.partners(clip_id).collect();
        let mut scratch = self.clone();
        let (head, tail, _) = scratch.cut_range_pieces(clip_id, from, to, true)?;
        // The stretch of timeline the cut removed, and the pieces on either side of it.
        let span = clip.source_span_to_timeline(a, b);
        let removed = span.end - span.start;
        let (mut lefts, mut rights) = (Vec::new(), Vec::new());
        // What lies after the stretch comes up to it: `(piece, by how much)`.
        let mut closing: Vec<(Uuid, f64)> = Vec::new();
        let mut origin: HashMap<Uuid, Uuid> = HashMap::new();
        match (&head, &tail) {
            (Some(h), Some(t)) => {
                lefts.push(h.id);
                rights.push(t.id);
                origin.insert(t.id, clip_id);
            }
            (Some(h), None) => lefts.push(h.id),
            (None, Some(t)) => rights.push(t.id),
            (None, None) => {}
        }
        for partner in partners {
            let p = self.clip(partner).expect("a partner is on the timeline");
            let (lo, hi) = (span.start.max(p.timeline_start), span.end.min(p.timeline_end()));
            if hi - lo <= DIFF_EPS {
                // The cut misses it: before the stretch it stays with the left, after it with the right.
                if p.timeline_end() <= span.start + DIFF_EPS {
                    lefts.push(partner);
                } else {
                    rights.push(partner);
                    closing.push((partner, -removed));
                }
                continue;
            }
            let (ptrack, _) = self.locate(partner).expect("a partner is on the timeline");
            if self.tracks[ptrack].locked {
                return Err(locked_partner(&self.tracks[ptrack]));
            }
            // The partner's own cut, in *its* source time.
            let (s0, s1) = (p.timeline_to_source(lo), p.timeline_to_source(hi));
            let (ph, pt, _) = scratch.cut_range_pieces(partner, s0.min(s1), s0.max(s1), false)?;
            match (&ph, &pt) {
                (Some(h), Some(t)) => {
                    lefts.push(h.id);
                    rights.push(t.id);
                    origin.insert(t.id, partner);
                }
                (Some(h), None) => lefts.push(h.id),
                (None, Some(t)) => rights.push(t.id),
                (None, None) => {}
            }
            // What survives the stretch resumes at the cut — the footage after it, which
            // played at `span.end`, now plays at `span.start`.
            if let Some(t) = &pt {
                closing.push((t.id, span.start - t.timeline_start));
            }
        }
        scratch.relink_sides(group, &lefts, &rights);
        let linked = scratch.settle_linked(self, &origin);
        scratch.apply_shifts(&closing, &linked, notes)?;
        scratch.conform_links_noted(self, &HashSet::from([clip_id]), &origin, None, notes)?;
        let kept = head
            .into_iter()
            .chain(tail)
            .map(|c| scratch.clip(c.id).cloned().unwrap_or(c))
            .collect();
        *self = scratch;
        Ok(kept)
    }

    // ---- speed --------------------------------------------------------------

    /// Retime `clip_id` to `speed` and every linked partner by the same *ratio*
    /// (a partner at 1× next to a clip going 1× → 2× goes to 2×; one already at 0.5×
    /// goes to 1×; a sign flip reverses it too), so a picture and its sound stay in
    /// step. Each keeps its window; **where** each ends up — a partner that began
    /// earlier or later than the named clip has its offset to it stretched by the same
    /// ratio, the whole group re-timed about the named clip — and what the longer
    /// or shorter clips push is [`Timeline::conform_links`]'s, after the ripple, with
    /// the named clip the authority. Errors when a partner's new speed would be zero or
    /// not finite, or a partner is on a locked track. Returns the named clip.
    pub fn set_speed_linked(&mut self, clip_id: Uuid, speed: f64) -> Result<Clip> {
        if !speed.is_finite() || speed == 0.0 {
            return Err(Error::InvalidArgument("speed must be a non-zero, finite number".to_string()));
        }
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let old = self.tracks[ti].clips[ci].speed;
        let ratio = if old == 0.0 || !old.is_finite() { 1.0 } else { speed / old };
        let mut updates: Vec<(Uuid, f64)> = Vec::new();
        for partner in self.unlocked_partners(clip_id, &HashSet::from([clip_id]))? {
            let p = self.clip(partner).expect("a partner is on the timeline");
            let new = p.speed * ratio;
            if !new.is_finite() || new == 0.0 {
                return Err(Error::InvalidArgument("a linked clip would end up with no speed".to_string()));
            }
            updates.push((partner, new));
        }
        self.tracks[ti].clips[ci].speed = speed;
        for (partner, new) in updates {
            self.clip_mut(partner).expect("a partner is on the timeline").speed = new;
        }
        Ok(self.tracks[ti].clips[ci].clone())
    }

    // ---- split and remove ---------------------------------------------------

    /// `cuts` widened with the partners of the clips it cuts: a partner not already
    /// named, on a track the request does not already cut, with `at` inside it, is
    /// cut at the same time (a partner `at` does not fall inside is untouched; one
    /// the cut would leave under 0.05 s refuses the whole edit, like a named clip
    /// would). A partner on a locked track that would be cut refuses it too. The
    /// named cuts come first, unchanged.
    pub fn with_linked_cuts(&self, cuts: &[ClipCut]) -> Result<Vec<ClipCut>> {
        let named: HashSet<Uuid> = cuts.iter().map(|c| c.clip_id).collect();
        let index = self.link_index();
        let mut lanes: HashSet<usize> = cuts.iter().filter_map(|c| self.locate(c.clip_id).map(|(ti, _)| ti)).collect();
        let mut out = cuts.to_vec();
        for cut in cuts {
            for partner in index.partners(cut.clip_id) {
                if named.contains(&partner) {
                    continue;
                }
                let (ti, ci) = self.locate(partner).expect("a partner is on the timeline");
                let p = &self.tracks[ti].clips[ci];
                if !(p.timeline_start + DIFF_EPS < cut.at && cut.at < p.timeline_end() - DIFF_EPS && lanes.insert(ti)) {
                    continue;
                }
                if self.tracks[ti].locked {
                    return Err(locked_partner(&self.tracks[ti]));
                }
                out.push(ClipCut {
                    clip_id: partner,
                    at: cut.at,
                });
            }
        }
        Ok(out)
    }

    // ---- roll / slip / slide ------------------------------------------------

    /// The pairs of partners a roll of the cut between `clip_a` and `clip_b` also
    /// rolls: a partner of `clip_a` and a partner of `clip_b` that touch on one
    /// track with the first earlier. Partners that form no such pair (a continuous
    /// clip running through the cut) have no cut to roll and are left alone.
    fn roll_partner_pairs(&self, clip_a: Uuid, clip_b: Uuid, footage: &SourceLimits) -> Result<Vec<(Uuid, Uuid)>> {
        let named = HashSet::from([clip_a, clip_b]);
        let (pa, pb) = (
            self.unlocked_partners(clip_a, &named)?,
            self.unlocked_partners(clip_b, &named)?,
        );
        let mut pairs = Vec::new();
        for a in &pa {
            for b in &pb {
                if a != b && self.roll_plan(*a, *b, footage).is_ok() {
                    pairs.push((*a, *b));
                }
            }
        }
        Ok(pairs)
    }

    fn track_name_of(&self, clip_id: Uuid) -> String {
        self.locate(clip_id)
            .map(|(ti, _)| self.tracks[ti].name.clone())
            .unwrap_or_default()
    }

    /// [`Timeline::roll_range`] for the group: the roll's range intersected with each
    /// partner pair's.
    pub fn roll_range_linked(&self, clip_a: Uuid, clip_b: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        let mut range = self.roll_range(clip_a, clip_b, footage)?;
        for (a, b) in self.roll_partner_pairs(clip_a, clip_b, footage)? {
            range = range.intersect(&self.roll_range(a, b, footage)?.on_linked(&self.track_name_of(a)));
        }
        Ok(range)
    }

    /// **Roll** the cut between `clip_a` and `clip_b` *and* the cut of each linked
    /// partner pair sharing it, all by the same `delta`, clamped to the tightest
    /// pair. A single clip's partner that has no pair is left alone.
    pub fn roll_edit_linked(&mut self, clip_a: Uuid, clip_b: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let pairs = self.roll_partner_pairs(clip_a, clip_b, footage)?;
        if pairs.is_empty() {
            return self.roll_edit(clip_a, clip_b, delta, footage);
        }
        let applied = self
            .roll_range_linked(clip_a, clip_b, footage)?
            .resolve(delta, "roll the cut")?;
        let mut scratch = self.clone();
        let mut clips = scratch.roll_edit(clip_a, clip_b, applied, footage)?.clips;
        for (a, b) in pairs {
            clips.extend(scratch.roll_edit(a, b, applied, footage)?.clips);
        }
        *self = scratch;
        Ok(EditOutcome::new(delta, applied, clips))
    }

    /// Partners a slip also slips: every linked partner but a still (which has no
    /// footage to slip and is skipped).
    fn slip_partners(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<Vec<Uuid>> {
        let mut out = Vec::new();
        for partner in self.unlocked_partners(clip_id, &HashSet::from([clip_id]))? {
            let p = self.clip(partner).expect("a partner is on the timeline");
            if footage_of(footage, p)?.is_infinite() {
                continue;
            }
            out.push(partner);
        }
        Ok(out)
    }

    /// [`Timeline::slip_range`] for the group, in the named clip's source seconds: a
    /// partner's range is converted by the ratio of the two speeds (a slip is the
    /// same *timeline* shift of the footage in each), then intersected.
    pub fn slip_range_linked(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        let mut range = self.slip_range(clip_id, footage)?;
        let mag = self.clip(clip_id).expect("slip_range found the clip").speed_mag();
        for partner in self.slip_partners(clip_id, footage)? {
            let p_mag = self.clip(partner).expect("a partner is on the timeline").speed_mag();
            let theirs = self.slip_range(partner, footage)?;
            range = range.intersect(&theirs.scaled(mag / p_mag).on_linked(&self.track_name_of(partner)));
        }
        Ok(range)
    }

    /// **Slip** the clip and its linked partners by the same *timeline* shift of the
    /// footage (so a partner at another speed slips by the matching source
    /// seconds), clamped to the tightest member. `delta` is in the named clip's
    /// source seconds, as for [`Timeline::slip_clip`].
    pub fn slip_clip_linked(&mut self, clip_id: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let partners = self.slip_partners(clip_id, footage)?;
        if partners.is_empty() {
            return self.slip_clip(clip_id, delta, footage);
        }
        let applied = self.slip_range_linked(clip_id, footage)?.resolve(delta, "slip the footage")?;
        let mag = self.clip(clip_id).expect("slip_range found the clip").speed_mag();
        let mut scratch = self.clone();
        let mut clips = scratch.slip_clip(clip_id, applied, footage)?.clips;
        for partner in partners {
            let p_mag = scratch.clip(partner).expect("a partner is on the timeline").speed_mag();
            let theirs = applied * p_mag / mag;
            if theirs.abs() > DIFF_EPS {
                clips.extend(scratch.slip_clip(partner, theirs, footage)?.clips);
            }
        }
        *self = scratch;
        Ok(EditOutcome::new(delta, applied, clips))
    }

    /// [`Timeline::slide_range`] for the group: the slide's range intersected with
    /// each partner's.
    pub fn slide_range_linked(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        let mut range = self.slide_range(clip_id, footage)?;
        for partner in self.unlocked_partners(clip_id, &HashSet::from([clip_id]))? {
            range = range.intersect(&self.slide_range(partner, footage)?.on_linked(&self.track_name_of(partner)));
        }
        Ok(range)
    }

    /// **Slide** the clip and each linked partner by the same `delta`, every one's
    /// touching neighbours giving way on its own track, clamped to the tightest.
    pub fn slide_clip_linked(&mut self, clip_id: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let partners = self.unlocked_partners(clip_id, &HashSet::from([clip_id]))?;
        if partners.is_empty() {
            return self.slide_clip(clip_id, delta, footage);
        }
        let applied = self.slide_range_linked(clip_id, footage)?.resolve(delta, "slide the clip")?;
        let mut scratch = self.clone();
        let mut clips = scratch.slide_clip(clip_id, applied, footage)?.clips;
        for partner in partners {
            clips.extend(scratch.slide_clip(partner, applied, footage)?.clips);
        }
        *self = scratch;
        Ok(EditOutcome::new(delta, applied, clips))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const X: f64 = 100.0;

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
            markers: Vec::new(),
            format: None,
            master: Default::default(),
        }
    }

    fn limits(assets: &[Uuid]) -> SourceLimits {
        assets.iter().map(|a| (*a, X)).collect()
    }

    fn id_of(t: &Timeline, track: usize, clip: usize) -> Uuid {
        t.tracks[track].clips[clip].id
    }

    fn get(t: &Timeline, id: Uuid) -> &Clip {
        t.clip(id).expect("the clip is on the timeline")
    }

    fn start(t: &Timeline, id: Uuid) -> f64 {
        get(t, id).timeline_start
    }

    fn linked(t: &mut Timeline, ids: &[Uuid]) -> Uuid {
        t.link_clips(ids).expect("linkable")
    }

    /// V1 holds `c` (source 0..10 at 0), A1 its sound `a` (the same), linked.
    fn pair() -> (Timeline, Uuid, Uuid, Uuid) {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 10.0, 0.0)]),
        ]);
        let (c, a) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[c, a]);
        (t, c, a, asset)
    }

    fn lock(t: &mut Timeline, track: usize) {
        t.tracks[track].locked = true;
    }

    // ---- link / unlink --------------------------------------------------------

    #[test]
    fn a_link_joins_a_picture_to_its_sound_and_nothing_else() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 5.0, 9.0, 5.0)],
            ),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 5.0, 0.0)]),
        ]);
        let (v1, v2, a1) = (id_of(&t, 0, 0), id_of(&t, 0, 1), id_of(&t, 1, 0));
        assert!(t.link_partners(v1).is_empty(), "a fresh clip has no partners");

        assert!(t.link_clips(&[v1]).is_err(), "one clip is not a link");
        assert!(t.link_clips(&[v1, v1]).is_err(), "a clip once");
        assert!(t.link_clips(&[v1, Uuid::new_v4()]).is_err(), "unknown clip");
        let two_on_a_lane = t.link_clips(&[v1, v2]).unwrap_err().to_string();
        assert!(two_on_a_lane.contains("V1"), "{two_on_a_lane}");
        assert!(t.link_partners(v1).is_empty(), "a refusal changes nothing");

        let group = linked(&mut t, &[v1, a1]);
        assert_eq!(t.link_partners(v1), vec![a1]);
        assert_eq!(t.link_partners(a1), vec![v1]);
        assert_eq!(get(&t, v1).link_id, Some(group));
        assert!(t.link_partners(v2).is_empty());
        // Linking what is already exactly that group is not an edit.
        assert!(t.link_clips(&[v1, a1]).is_err());
        // A group may take a third track — and a clip may leave for another group.
        let a2 = {
            t.tracks.push(lane(StreamKind::Audio, "A2", vec![clip(asset, 0.0, 5.0, 0.0)]));
            id_of(&t, 2, 0)
        };
        linked(&mut t, &[v1, a1, a2]);
        assert_eq!(t.link_partners(a2).len(), 2);
        assert_eq!(t.with_link_partners(&[v1]), vec![v1, a1, a2]);
    }

    #[test]
    fn linking_into_a_new_group_dissolves_the_one_left_behind() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A2", vec![clip(asset, 0.0, 5.0, 0.0)]),
        ]);
        let (v, a1, a2) = (id_of(&t, 0, 0), id_of(&t, 1, 0), id_of(&t, 2, 0));
        linked(&mut t, &[v, a1]);
        // a1 leaves for a group with a2: v is left alone in its old one.
        linked(&mut t, &[a1, a2]);
        assert!(t.link_partners(v).is_empty());
        assert_eq!(
            get(&t, v).link_id,
            None,
            "a link of one clip is cleared, not kept as a stale id"
        );
        assert_eq!(t.link_partners(a1), vec![a2]);
    }

    #[test]
    fn unlinking_either_half_of_a_pair_unlinks_the_pair() {
        let (mut t, c, a, _) = pair();
        assert_eq!(t.unlink_clips(&[a]).unwrap(), 1);
        assert_eq!(get(&t, c).link_id, None);
        assert_eq!(get(&t, a).link_id, None);
        assert!(t.unlink_clips(&[c, a]).is_err(), "nothing was linked: no revision for that");
        assert!(t.unlink_clips(&[Uuid::new_v4()]).is_err());
    }

    #[test]
    fn unlinking_one_of_three_leaves_the_other_two_together() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A2", vec![clip(asset, 0.0, 5.0, 0.0)]),
        ]);
        let ids: Vec<Uuid> = (0..3).map(|i| id_of(&t, i, 0)).collect();
        linked(&mut t, &ids);
        assert_eq!(t.unlink_clips(&ids[..1]).unwrap(), 1);
        assert_eq!(t.link_partners(ids[1]), vec![ids[2]]);
        assert!(t.link_partners(ids[0]).is_empty());
    }

    #[test]
    fn linking_and_unlinking_refuse_a_locked_track() {
        let (mut t, c, a, _) = pair();
        lock(&mut t, 1);
        assert!(t.unlink_clips(&[c, a]).is_err());
        assert!(t.unlink_clips(&[c]).is_ok(), "a locked partner is only dissolved, not edited");
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 5.0, 0.0)]),
        ]);
        let ids = [id_of(&t, 0, 0), id_of(&t, 1, 0)];
        lock(&mut t, 1);
        assert!(t.link_clips(&ids).is_err());
    }

    // ---- move ----------------------------------------------------------------

    fn mv(t: &Timeline, id: Uuid, start: f64, track: Option<usize>) -> ClipMove {
        ClipMove {
            clip_id: id,
            timeline_start: start,
            track_id: track.map(|i| t.tracks[i].id),
        }
    }

    #[test]
    fn a_move_carries_the_partner_by_the_same_time_on_its_own_track() {
        let (mut t, c, a, _) = pair();
        let wide = t.with_linked_moves(&[mv(&t, c, 4.0, None)]).unwrap();
        assert_eq!(wide.len(), 2);
        assert_eq!((wide[1].clip_id, wide[1].timeline_start, wide[1].track_id), (a, 4.0, None));
        t.move_clips(&wide).unwrap();
        assert_eq!((start(&t, c), start(&t, a)), (4.0, 4.0));
    }

    #[test]
    fn a_partner_at_another_offset_keeps_its_offset() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().timeline_start = 2.0; // the sound runs 2 s late
        let wide = t.with_linked_moves(&[mv(&t, c, 5.0, None)]).unwrap();
        t.move_clips(&wide).unwrap();
        assert_eq!((start(&t, c), start(&t, a)), (5.0, 7.0), "Δt, not a snap to the picture");
    }

    #[test]
    fn a_track_change_belongs_to_the_clip_that_was_named() {
        let (mut t, c, a, _) = pair();
        t.tracks.insert(1, lane(StreamKind::Video, "V2", vec![]));
        let wide = t.with_linked_moves(&[mv(&t, c, 3.0, Some(1))]).unwrap();
        t.move_clips(&wide).unwrap();
        assert_eq!(t.locate(c).unwrap().0, 1, "the picture went up to V2");
        assert_eq!(t.locate(a).unwrap().0, 2, "its sound stayed on A1");
        assert_eq!((start(&t, c), start(&t, a)), (3.0, 3.0));
    }

    #[test]
    fn a_partner_that_is_named_is_not_moved_twice_and_a_still_move_adds_nothing() {
        let (t, c, a, _) = pair();
        let both = t.with_linked_moves(&[mv(&t, c, 4.0, None), mv(&t, a, 6.0, None)]).unwrap();
        assert_eq!(both.len(), 2, "the request already says where each goes");
        assert_eq!(both[1].timeline_start, 6.0);
        let none = t.with_linked_moves(&[mv(&t, c, 0.0, None)]).unwrap();
        assert_eq!(none.len(), 1, "a zero Δt moves nobody");
        // A dangling id is the clip's own business, not a partner.
        let mut lone = t;
        lone.tracks[1].clips.clear();
        assert_eq!(lone.with_linked_moves(&[mv(&lone, c, 4.0, None)]).unwrap().len(), 1);
    }

    #[test]
    fn a_partner_that_would_start_before_zero_or_sit_on_a_locked_track_refuses_the_move() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(c).unwrap().timeline_start = 3.0;
        t.clip_mut(a).unwrap().timeline_start = 1.0;
        // The picture could go to 0.5, but its sound would land at -1.5.
        let err = t.with_linked_moves(&[mv(&t, c, 0.5, None)]).unwrap_err().to_string();
        assert!(err.contains("A1") && err.contains("beginning"), "{err}");
        assert!(t.with_linked_moves(&[mv(&t, c, 2.0, None)]).is_ok());
        lock(&mut t, 1);
        let err = t.with_linked_moves(&[mv(&t, c, 4.0, None)]).unwrap_err().to_string();
        assert!(err.contains("locked") && err.contains("A1"), "{err}");
        // …but a pure track change moves no partner, so a locked one is no obstacle.
        t.tracks.insert(1, lane(StreamKind::Video, "V2", vec![]));
        assert!(t.with_linked_moves(&[mv(&t, c, 3.0, Some(1))]).is_ok());
    }

    #[test]
    fn a_group_move_that_cannot_land_changes_nothing() {
        let (mut t, c, _, asset) = pair();
        // A clip sits in the way of the sound only.
        t.tracks[1].clips.push(clip(asset, 20.0, 25.0, 12.0));
        let before = serde_json::to_string(&t).unwrap();
        let wide = t.with_linked_moves(&[mv(&t, c, 8.0, None)]).unwrap();
        assert!(t.move_clips(&wide).is_err(), "the sound would land on the clip at 12 s");
        assert_eq!(serde_json::to_string(&t).unwrap(), before, "all or nothing");
    }

    // ---- trim ----------------------------------------------------------------

    fn extent(t: &Timeline, id: Uuid) -> (f64, f64) {
        let c = get(t, id);
        (c.timeline_start, c.timeline_end())
    }

    #[test]
    fn a_right_trim_follows_when_the_partner_shares_the_edge() {
        let (mut t, c, a, asset) = pair();
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().source_out = 7.0;
        let carried = t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap();
        assert_eq!(carried.len(), 1);
        assert_eq!(extent(&t, a), (0.0, 7.0));
        assert_eq!(get(&t, a).source_out, 7.0);
        assert_eq!(get(&t, a).source_in, 0.0);
    }

    #[test]
    fn a_partner_that_never_shared_the_edge_is_left_alone_there() {
        let (mut t, c, a, asset) = pair();
        t.clip_mut(a).unwrap().source_out = 8.0; // the sound ends 2 s early
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().source_out = 7.0;
        t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap();
        assert_eq!(extent(&t, a), (0.0, 8.0), "its tail was somewhere else");
    }

    #[test]
    fn a_left_trim_moves_the_partners_head_and_window_too() {
        let (mut t, c, a, asset) = pair();
        let was = get(&t, c).clone();
        // The GUI's left-edge trim: in-point and start together, the end stays put.
        {
            let clip = t.clip_mut(c).unwrap();
            clip.source_in = 3.0;
            clip.timeline_start = 3.0;
        }
        t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap();
        let p = get(&t, a);
        assert_eq!((p.timeline_start, p.timeline_end()), (3.0, 10.0));
        assert_eq!((p.source_in, p.source_out), (3.0, 10.0));
    }

    #[test]
    fn extending_past_the_partners_footage_stops_where_it_runs_out() {
        let asset = Uuid::new_v4();
        let music = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(music, 90.0, 100.0, 0.0)]),
        ]);
        let (c, a) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[c, a]);
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().source_out = 15.0; // the picture has footage left; the music does not
        t.carry_extent_edit(c, &was, &limits(&[asset, music])).unwrap();
        assert_eq!(extent(&t, c), (0.0, 15.0));
        assert_eq!(extent(&t, a), (0.0, 10.0), "100 s is where the music ends");
    }

    #[test]
    fn a_partner_at_another_speed_follows_in_timeline_seconds() {
        let (mut t, c, a, asset) = pair();
        // The sound plays at 2x, so its 5 s of source are 2.5 timeline seconds — placed
        // to end where the picture does (10 s).
        {
            let sound = t.clip_mut(a).unwrap();
            sound.speed = 2.0;
            sound.source_in = 0.0;
            sound.source_out = 10.0;
            sound.timeline_start = 5.0;
        }
        assert_eq!(extent(&t, a), (5.0, 10.0));
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().source_out = 8.0; // the picture ends 2 s earlier
        t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap();
        let p = get(&t, a);
        assert_eq!(p.timeline_end(), 8.0, "the sound's end moved 2 timeline seconds");
        assert_eq!(p.source_out, 6.0, "which is 4 source seconds at 2x");
        assert_eq!(p.timeline_start, 5.0, "its head was never at the picture's");
    }

    #[test]
    fn a_pure_move_through_trim_moves_the_partner_by_the_same_time() {
        let (mut t, c, a, asset) = pair();
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().timeline_start = 6.0;
        t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap();
        assert_eq!(extent(&t, a), (6.0, 16.0));
        assert_eq!(get(&t, a).source_in, 0.0, "moved, not trimmed");
        // Carried before zero, the partner loses what hangs off the front (and keeps its
        // sync: its head is trimmed, it is not slid) — unless nothing would be left.
        let (mut t, c, a, asset) = pair();
        t.clip_mut(c).unwrap().timeline_start = 3.0;
        t.clip_mut(a).unwrap().timeline_start = 3.0;
        let was = get(&t, c).clone();
        t.clip_mut(a).unwrap().timeline_start = 1.0;
        t.clip_mut(c).unwrap().timeline_start = 0.0;
        t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap();
        let p = get(&t, a);
        assert_eq!((p.timeline_start, p.timeline_end(), p.source_in), (0.0, 8.0, 2.0));
        let (mut t, c, a, asset) = pair();
        t.clip_mut(a).unwrap().source_out = 1.0;
        t.clip_mut(a).unwrap().timeline_start = 1.0;
        t.clip_mut(c).unwrap().timeline_start = 5.0;
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().timeline_start = 0.0;
        let err = t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap_err().to_string();
        assert!(err.contains("beginning of the timeline"), "{err}");
    }

    #[test]
    fn a_trim_that_would_remove_the_partner_or_touch_a_locked_one_is_refused() {
        let (mut t, c, a, asset) = pair();
        t.clip_mut(a).unwrap().source_out = 4.0; // the sound is the short one
        let was = get(&t, c).clone();
        {
            let clip = t.clip_mut(c).unwrap();
            clip.source_in = 5.0;
            clip.timeline_start = 5.0;
        }
        // The partner shares the head edge (both start at 0) but is only 4 s long.
        let err = t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap_err().to_string();
        assert!(err.contains("trimmed away"), "{err}");

        let (mut t, c, _, asset) = pair();
        lock(&mut t, 1);
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().source_out = 7.0;
        let err = t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap_err().to_string();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn a_reversed_clips_head_is_its_out_point() {
        let (mut t, c, a, asset) = pair();
        for id in [c, a] {
            t.clip_mut(id).unwrap().speed = -1.0;
        }
        let was = get(&t, c).clone();
        // Trim 3 s off the *start* of a reversed clip: its out-point comes down.
        {
            let clip = t.clip_mut(c).unwrap();
            clip.source_out = 7.0;
            clip.timeline_start = 3.0;
        }
        t.carry_extent_edit(c, &was, &limits(&[asset])).unwrap();
        let p = get(&t, a);
        assert_eq!((p.timeline_start, p.timeline_end()), (3.0, 10.0));
        assert_eq!((p.source_in, p.source_out), (0.0, 7.0));
    }

    #[test]
    fn a_still_partner_is_trimmed_by_extent() {
        let asset = Uuid::new_v4();
        let still = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Video, "V2", vec![clip(still, 0.0, 10.0, 0.0)]),
        ]);
        let (c, s) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[c, s]);
        let mut footage = limits(&[asset]);
        footage.insert(still, f64::INFINITY);
        let was = get(&t, c).clone();
        {
            let clip = t.clip_mut(c).unwrap();
            clip.source_in = 4.0;
            clip.timeline_start = 4.0;
        }
        t.carry_extent_edit(c, &was, &footage).unwrap();
        assert_eq!(extent(&t, s), (4.0, 10.0), "a still's window is its length");
        assert!(get(&t, s).source_in >= 0.0, "never a negative in-point");
    }

    #[test]
    fn a_carried_partner_may_not_land_on_a_clip_outside_its_group() {
        let (mut t, c, a, asset) = pair();
        let vo = Uuid::new_v4();
        t.tracks[1].clips.push(clip(vo, 0.0, 3.0, 12.0));
        let other = id_of(&t, 1, 1);
        let before = t.clone();
        let was = get(&t, c).clone();
        t.clip_mut(c).unwrap().source_out = 13.0;
        let carried: Vec<Uuid> = t
            .carry_extent_edit(c, &was, &limits(&[asset]))
            .unwrap()
            .iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(extent(&t, a), (0.0, 13.0), "the carry itself writes no lane check");
        let err = t.check_carried_lanes(&before, &carried).unwrap_err().to_string();
        assert!(
            err.contains("A1") && err.contains("0:12.0") && err.contains("not linked to it"),
            "{err}"
        );
        // Short of it, or with the clip out of the way (the ripple pushed it), nothing is wrong.
        t.clip_mut(a).unwrap().source_out = 11.5;
        assert!(t.check_carried_lanes(&before, &carried).is_ok());
        t.clip_mut(a).unwrap().source_out = 13.0;
        t.clip_mut(other).unwrap().timeline_start = 14.0;
        assert!(t.check_carried_lanes(&before, &carried).is_ok(), "room made by a ripple");
        // An overlap that was there before the edit is old news.
        t.clip_mut(other).unwrap().timeline_start = 12.0;
        let mut was_overlapping = before.clone();
        was_overlapping.clip_mut(a).unwrap().source_out = 12.5;
        assert!(t.check_carried_lanes(&was_overlapping, &carried).is_ok());
        assert!(
            t.check_carried_lanes(&before, &[]).is_ok(),
            "nothing carried, nothing to check"
        );
    }

    // ---- split ---------------------------------------------------------------

    #[test]
    fn a_split_cuts_the_partner_and_links_the_new_halves() {
        let (mut t, c, a, _) = pair();
        let group = get(&t, c).link_id;
        let (left, right) = t.split_clip_linked(c, 4.0).unwrap();
        assert_eq!((left.id, extent(&t, c)), (c, (0.0, 4.0)));
        assert_eq!(extent(&t, a), (0.0, 4.0), "the sound was cut at the same moment");
        assert_eq!(get(&t, a).link_id, group, "the left halves keep the pair");
        let a_right = t.tracks[1].clips.iter().find(|x| x.id != a).unwrap().id;
        assert_eq!(extent(&t, a_right), (4.0, 10.0));
        assert_eq!(extent(&t, right.id), (4.0, 10.0));
        assert!(right.link_id.is_some() && right.link_id != group, "a new pair");
        assert_eq!(t.link_partners(right.id), vec![a_right]);
        assert_eq!(t.link_partners(c), vec![a]);
    }

    #[test]
    fn a_split_keeps_each_fade_on_the_half_that_holds_its_edge() {
        let (mut t, c, a, _) = pair();
        for id in [c, a] {
            let clip = t.clip_mut(id).unwrap();
            clip.fade_in = 1.0;
            clip.fade_out = 2.0;
        }
        t.clip_mut(c).unwrap().transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
        let (left, right) = t.split_clip_linked(c, 4.0).unwrap();
        // The cut is not a fade edge: the left half does not fade out into it, nor the right in.
        assert_eq!((left.fade_in, left.fade_out), (1.0, 0.0));
        assert_eq!((right.fade_in, right.fade_out), (0.0, 2.0));
        assert!(left.transition_in.is_some() && right.transition_in.is_none());
        let a_right = t.tracks[1].clips.iter().find(|x| x.id != a).unwrap();
        let a_left = get(&t, a);
        assert_eq!((a_left.fade_in, a_left.fade_out), (1.0, 0.0));
        assert_eq!((a_right.fade_in, a_right.fade_out), (0.0, 2.0));
    }

    #[test]
    fn a_fade_longer_than_the_half_it_stays_on_is_clamped_to_it() {
        let (mut t, c, _, _) = pair();
        {
            let clip = t.clip_mut(c).unwrap();
            clip.fade_in = 5.0;
            clip.fade_out = 6.0;
        }
        // The cut is 2 s in: the left half is 2 s long, the right 8 s.
        let (left, right) = t.split_clip(c, 2.0).unwrap();
        assert_eq!((left.fade_in, left.fade_out), (2.0, 0.0));
        assert_eq!((right.fade_in, right.fade_out), (0.0, 6.0));
        // Cut 9 s in, inside the fade-out: the right half is 1 s long and holds it to that.
        let (_, tail) = t.split_clip(right.id, 9.0).unwrap();
        assert_eq!((tail.fade_in, tail.fade_out), (0.0, 1.0));
    }

    #[test]
    fn a_partner_the_cut_does_not_reach_is_left_whole_and_the_new_half_unlinked() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().source_out = 3.0; // the sound ends at 3
        let (_, right) = t.split_clip_linked(c, 6.0).unwrap();
        assert_eq!(extent(&t, a), (0.0, 3.0));
        assert_eq!(t.tracks[1].clips.len(), 1);
        assert_eq!(right.link_id, None, "its partner was not cut, so it has no new pair");
        assert_eq!(t.link_partners(c), vec![a]);
    }

    #[test]
    fn a_split_at_a_partners_edge_does_not_make_a_sliver() {
        let (mut t, c, _, _) = pair();
        // The sound starts at 5: splitting at 5 is *at* its edge, not inside it.
        t.clip_mut(id_of(&t, 1, 0)).unwrap().timeline_start = 5.0;
        t.split_clip_linked(c, 5.0).unwrap();
        assert_eq!(t.tracks[1].clips.len(), 1);
    }

    #[test]
    fn a_partner_at_another_start_is_split_at_the_same_timeline_time() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().timeline_start = 2.0; // the sound starts late
        let (_, right) = t.split_clip_linked(c, 6.0).unwrap();
        assert_eq!(extent(&t, a), (2.0, 6.0));
        assert_eq!(get(&t, a).source_out, 4.0, "4 s of the sound before the cut");
        let a_right = t.link_partners(right.id)[0];
        assert_eq!(extent(&t, a_right), (6.0, 12.0));
        assert_eq!(get(&t, a_right).source_in, 4.0);
    }

    #[test]
    fn a_locked_partner_refuses_the_split_and_nothing_changes() {
        let (mut t, c, _, _) = pair();
        lock(&mut t, 1);
        let before = serde_json::to_string(&t).unwrap();
        let err = t.split_clip_linked(c, 4.0).unwrap_err().to_string();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(serde_json::to_string(&t).unwrap(), before);
        // A locked track the cut does not reach is no obstacle.
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().source_out = 3.0;
        lock(&mut t, 1);
        assert!(t.split_clip_linked(c, 6.0).is_ok());
    }

    #[test]
    fn a_plain_split_leaves_the_left_half_linked_and_the_right_free() {
        let (mut t, c, a, _) = pair();
        let (_, right) = t.split_clip(c, 4.0).unwrap();
        assert_eq!(right.link_id, None);
        assert_eq!(t.link_partners(c), vec![a]);
        assert_eq!(extent(&t, a), (0.0, 10.0), "the partner was not touched");
    }

    // ---- remove --------------------------------------------------------------

    #[test]
    fn partners_are_named_once_after_the_clips_that_were() {
        let (t, c, a, _) = pair();
        assert_eq!(t.with_link_partners(&[a, c]), vec![a, c]);
        assert_eq!(t.with_link_partners(&[c, c]), vec![c, a]);
        let lone = Uuid::new_v4();
        assert_eq!(
            t.with_link_partners(&[lone]),
            vec![lone],
            "an unknown id is the caller's to report"
        );
    }

    #[test]
    fn removing_clips_takes_the_partners_and_a_locked_one_refuses_with_the_link_named() {
        let (mut t, c, a, _) = pair();
        assert_eq!(t.remove_clips_linked(&[c]).unwrap(), 2);
        assert!(t.clip(c).is_none() && t.clip(a).is_none());
        let (mut t, c, a, _) = pair();
        lock(&mut t, 1);
        let err = t.remove_clips_linked(&[c]).unwrap_err().to_string();
        assert!(err.contains("linked") && err.contains("A1"), "{err}");
        assert!(t.clip(c).is_some() && t.clip(a).is_some(), "nothing was removed");
        assert!(t.remove_clips_linked(&[Uuid::new_v4()]).is_err());
    }

    #[test]
    fn a_ripple_delete_closes_the_gap_on_both_tracks() {
        let (mut t, c, a, asset) = pair();
        for track in [0, 1] {
            t.tracks[track].clips.push(clip(asset, 10.0, 16.0, 10.0));
        }
        let (c2, a2) = (id_of(&t, 0, 1), id_of(&t, 1, 1));
        linked(&mut t, &[c2, a2]);
        assert_eq!(t.ripple_delete_linked(c).unwrap(), 2);
        assert!(t.clip(c).is_none() && t.clip(a).is_none());
        assert_eq!((start(&t, c2), start(&t, a2)), (0.0, 0.0));
        // A locked partner refuses the lot.
        let (mut t, c, _, _) = pair();
        lock(&mut t, 1);
        assert!(t.ripple_delete_linked(c).is_err());
        assert!(t.clip(c).is_some());
    }

    // ---- cut a source range --------------------------------------------------

    #[test]
    fn cutting_a_range_cuts_the_partner_and_closes_up_both_tracks() {
        let (mut t, c, a, asset) = pair();
        for track in [0, 1] {
            t.tracks[track].clips.push(clip(asset, 20.0, 25.0, 10.0));
        }
        let (c2, a2) = (id_of(&t, 0, 1), id_of(&t, 1, 1));
        linked(&mut t, &[c2, a2]);
        let kept = t.cut_clip_range_linked(c, 3.0, 5.0).unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(extent(&t, c), (0.0, 3.0));
        assert_eq!(extent(&t, a), (0.0, 3.0));
        let (c_tail, a_tail) = (
            kept[1].id,
            t.tracks[1].clips.iter().find(|x| x.id != a && x.id != a2).unwrap().id,
        );
        assert_eq!(extent(&t, c_tail), (3.0, 8.0));
        assert_eq!(extent(&t, a_tail), (3.0, 8.0));
        assert_eq!((start(&t, c2), start(&t, a2)), (8.0, 8.0), "both tracks closed 2 s");
        assert_eq!(t.link_partners(c_tail), vec![a_tail], "the new halves are a pair");
        assert_eq!(t.link_partners(c), vec![a]);
    }

    #[test]
    fn a_partner_at_another_offset_loses_the_same_moment_not_the_same_source() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().timeline_start = 2.0; // sound runs 2 s late
                                                     // Cut source 4..6 of the picture = timeline 4..6; the sound plays source 2..4 there.
        t.cut_clip_range_linked(c, 4.0, 6.0).unwrap();
        let sound: Vec<(f64, f64, f64, f64)> = t.tracks[1]
            .clips
            .iter()
            .map(|x| (x.timeline_start, x.timeline_end(), x.source_in, x.source_out))
            .collect();
        assert_eq!(sound, vec![(2.0, 4.0, 0.0, 2.0), (4.0, 10.0, 4.0, 10.0)]);
    }

    #[test]
    fn a_partner_the_cut_misses_is_untouched_and_a_locked_one_refuses() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().source_out = 3.0;
        t.cut_clip_range_linked(c, 5.0, 8.0).unwrap();
        assert_eq!(t.tracks[1].clips.len(), 1);
        assert_eq!(extent(&t, a), (0.0, 3.0));
        assert_eq!(t.link_partners(c), vec![a], "still a pair");
        let (mut t, c, _, _) = pair();
        lock(&mut t, 1);
        let before = serde_json::to_string(&t).unwrap();
        assert!(t.cut_clip_range_linked(c, 3.0, 5.0).is_err());
        assert_eq!(serde_json::to_string(&t).unwrap(), before);
    }

    #[test]
    fn a_cut_that_removes_a_whole_head_leaves_the_pair_linked() {
        let (mut t, c, a, _) = pair();
        t.cut_clip_range_linked(c, 0.0, 4.0).unwrap();
        assert_eq!((extent(&t, c), extent(&t, a)), ((0.0, 6.0), (0.0, 6.0)));
        assert_eq!(t.link_partners(c), vec![a], "no new clips, so nothing to relink");
    }

    // ---- speed ---------------------------------------------------------------

    #[test]
    fn speed_is_carried_as_a_ratio() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().speed = 0.5;
        t.set_speed_linked(c, 2.0).unwrap();
        assert_eq!(get(&t, c).speed, 2.0);
        assert_eq!(get(&t, a).speed, 1.0, "0.5 x (2 / 1)");
        // A sign flip reverses the partner too.
        t.set_speed_linked(c, -2.0).unwrap();
        assert_eq!(get(&t, a).speed, -1.0);
        assert!(t.set_speed_linked(c, 0.0).is_err());
        lock(&mut t, 1);
        assert!(t.set_speed_linked(c, 1.0).is_err());
        assert_eq!(get(&t, c).speed, -2.0, "a refusal changes nothing");
    }

    // ---- split and remove ----------------------------------------------------

    fn cut(id: Uuid, at: f64) -> ClipCut {
        ClipCut { clip_id: id, at }
    }

    #[test]
    fn a_trim_to_the_playhead_reaches_the_partner_that_spans_it() {
        let (t, c, a, _) = pair();
        let all = t.with_linked_cuts(&[cut(c, 4.0)]).unwrap();
        assert_eq!(all, vec![cut(c, 4.0), cut(a, 4.0)]);
        let mut t = t;
        let kept = t.split_remove_clips(&all, SplitSide::Left).unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!((extent(&t, c), extent(&t, a)), ((4.0, 10.0), (4.0, 10.0)));
        // Naming both is not cutting the sound twice.
        let (t, c, a, _) = pair();
        assert_eq!(t.with_linked_cuts(&[cut(c, 4.0), cut(a, 5.0)]).unwrap().len(), 2);
    }

    #[test]
    fn a_partner_the_playhead_is_outside_is_left_alone_and_a_locked_one_refuses() {
        let (mut t, c, a, _) = pair();
        t.clip_mut(a).unwrap().source_out = 3.0;
        assert_eq!(t.with_linked_cuts(&[cut(c, 6.0)]).unwrap().len(), 1);
        t.clip_mut(a).unwrap().source_out = 10.0;
        lock(&mut t, 1);
        assert!(t.with_linked_cuts(&[cut(c, 6.0)]).is_err());
    }

    #[test]
    fn a_track_the_request_already_cuts_is_not_cut_twice() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(
                StreamKind::Audio,
                "A1",
                vec![clip(asset, 0.0, 10.0, 0.0), clip(asset, 0.0, 10.0, 20.0)],
            ),
        ]);
        let (c, a, other) = (id_of(&t, 0, 0), id_of(&t, 1, 0), id_of(&t, 1, 1));
        linked(&mut t, &[c, a]);
        // The request names a different clip of A1: it takes the lane.
        let all = t.with_linked_cuts(&[cut(c, 4.0), cut(other, 24.0)]).unwrap();
        assert_eq!(all.len(), 2, "{all:?}");
    }

    // ---- roll / slip / slide -------------------------------------------------

    /// V1: a [0,5) b [5,10); A1: the same cut, partners pairwise.
    fn two_cut_pairs() -> (Timeline, [Uuid; 4], Uuid) {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 10.0, 15.0, 0.0), clip(asset, 20.0, 25.0, 5.0)],
            ),
            lane(
                StreamKind::Audio,
                "A1",
                vec![clip(asset, 10.0, 15.0, 0.0), clip(asset, 20.0, 25.0, 5.0)],
            ),
        ]);
        let ids = [id_of(&t, 0, 0), id_of(&t, 0, 1), id_of(&t, 1, 0), id_of(&t, 1, 1)];
        linked(&mut t, &[ids[0], ids[2]]);
        linked(&mut t, &[ids[1], ids[3]]);
        (t, ids, asset)
    }

    #[test]
    fn a_roll_moves_the_cut_of_every_partner_pair_by_the_same_amount() {
        let (mut t, [va, vb, aa, ab], asset) = two_cut_pairs();
        let out = t.roll_edit_linked(va, vb, 1.0, &limits(&[asset])).unwrap();
        assert_eq!(out.applied, 1.0);
        assert_eq!(out.clips.len(), 4);
        assert_eq!((extent(&t, va), extent(&t, vb)), ((0.0, 6.0), (6.0, 10.0)));
        assert_eq!((extent(&t, aa), extent(&t, ab)), ((0.0, 6.0), (6.0, 10.0)));
    }

    #[test]
    fn a_roll_clamps_to_the_tightest_pair() {
        let (mut t, [va, vb, aa, ab], asset) = two_cut_pairs();
        // The sound comes from a shorter file: its outgoing clip (source 10..15) has
        // only 0.5 s of footage left to extend into, where the picture has plenty.
        let sound = Uuid::new_v4();
        for id in [aa, ab] {
            t.clip_mut(id).unwrap().asset_id = sound;
        }
        let mut footage = limits(&[asset]);
        footage.insert(sound, 15.5);
        let alone = t.roll_range(va, vb, &footage).unwrap();
        assert!(alone.max > 4.0, "the picture alone could go much further: {alone:?}");
        let range = t.roll_range_linked(va, vb, &footage).unwrap();
        assert!((range.max - 0.5).abs() < 1e-9, "{range:?}");
        let out = t.roll_edit_linked(va, vb, 2.0, &footage).unwrap();
        assert!(out.clamped && (out.applied - 0.5).abs() < 1e-9, "{out:?}");
        assert!(
            (extent(&t, va).1 - 5.5).abs() < 1e-9,
            "the picture only went as far as the sound could"
        );
        assert!((extent(&t, aa).1 - 5.5).abs() < 1e-9);
        assert!((extent(&t, ab).0 - 5.5).abs() < 1e-9);
    }

    #[test]
    fn a_partner_running_through_the_cut_is_not_rolled() {
        let (mut t, [va, vb, aa, _], asset) = two_cut_pairs();
        // The sound is one clip across the cut, linked to the first picture clip only.
        t.tracks[1].clips.truncate(1);
        t.clip_mut(aa).unwrap().source_out = 20.0; // 10 s, 0..10 on the timeline
        let out = t.roll_edit_linked(va, vb, 1.0, &limits(&[asset])).unwrap();
        assert_eq!(out.clips.len(), 2, "only the picture's own cut: the sound has no cut there");
        assert_eq!(extent(&t, aa), (0.0, 10.0));
        assert_eq!((extent(&t, va), extent(&t, vb)), ((0.0, 6.0), (6.0, 10.0)));
    }

    #[test]
    fn a_roll_with_a_locked_partner_refuses() {
        let (mut t, [va, vb, ..], asset) = two_cut_pairs();
        lock(&mut t, 1);
        assert!(t.roll_edit_linked(va, vb, 1.0, &limits(&[asset])).is_err());
        assert_eq!(extent(&t, va), (0.0, 5.0));
    }

    #[test]
    fn a_slip_slips_the_partner_by_the_same_moment_of_footage() {
        let (mut t, [va, _, aa, _], asset) = two_cut_pairs();
        let out = t.slip_clip_linked(va, 1.5, &limits(&[asset])).unwrap();
        assert_eq!(out.applied, 1.5);
        assert_eq!(out.clips.len(), 2);
        assert_eq!((get(&t, va).source_in, get(&t, aa).source_in), (11.5, 11.5));
        assert_eq!(
            (extent(&t, va), extent(&t, aa)),
            ((0.0, 5.0), (0.0, 5.0)),
            "nothing moves on the timeline"
        );
    }

    #[test]
    fn a_slip_converts_between_speeds_and_skips_a_still() {
        let (mut t, [va, _, aa, _], asset) = two_cut_pairs();
        // The sound plays at 2x: 5 timeline seconds cover 10 source seconds.
        {
            let sound = t.clip_mut(aa).unwrap();
            sound.speed = 2.0;
            sound.source_in = 10.0;
            sound.source_out = 20.0;
        }
        t.slip_clip_linked(va, 1.0, &limits(&[asset])).unwrap();
        assert_eq!(get(&t, va).source_in, 11.0);
        assert_eq!(
            get(&t, aa).source_in,
            12.0,
            "1 s of footage under the picture is 2 source seconds at 2x"
        );

        // A still partner has no footage to slip and is left out.
        let still = Uuid::new_v4();
        t.tracks.push(lane(StreamKind::Video, "V2", vec![clip(still, 0.0, 5.0, 0.0)]));
        let s = id_of(&t, 2, 0);
        t.link_clips(&[va, aa, s]).unwrap();
        let mut footage = limits(&[asset]);
        footage.insert(still, f64::INFINITY);
        let out = t.slip_clip_linked(va, 0.5, &footage).unwrap();
        assert_eq!(out.clips.len(), 2, "the picture and the sound, not the still");
    }

    #[test]
    fn a_slip_clamps_to_the_tightest_member() {
        let (mut t, [va, _, aa, _], asset) = two_cut_pairs();
        t.clip_mut(aa).unwrap().source_in = 1.0; // the sound can only slip 1 s earlier
        t.clip_mut(aa).unwrap().source_out = 6.0;
        t.clip_mut(va).unwrap().source_in = 3.0;
        t.clip_mut(va).unwrap().source_out = 8.0;
        let out = t.slip_clip_linked(va, -2.5, &limits(&[asset])).unwrap();
        assert!(out.clamped, "{out:?}");
        assert!((out.applied + 1.0).abs() < 1e-9, "{out:?}");
        assert!((get(&t, va).source_in - 2.0).abs() < 1e-9);
        assert!((get(&t, aa).source_in - 0.0).abs() < 1e-9);
    }

    #[test]
    fn a_slide_slides_the_partner_with_its_own_neighbours_giving_way() {
        let (mut t, [va, vb, aa, ab], asset) = two_cut_pairs();
        let asset2 = Uuid::new_v4();
        // A third pair after the two, so b has a next neighbour to give way.
        t.tracks[0].clips.push(clip(asset2, 10.0, 15.0, 10.0));
        t.tracks[1].clips.push(clip(asset2, 10.0, 15.0, 10.0));
        let (vc, ac) = (id_of(&t, 0, 2), id_of(&t, 1, 2));
        linked(&mut t, &[vc, ac]);
        let out = t.slide_clip_linked(vb, 1.0, &limits(&[asset, asset2])).unwrap();
        assert_eq!(out.applied, 1.0);
        for (prev, mid, next) in [(va, vb, vc), (aa, ab, ac)] {
            assert_eq!(extent(&t, prev), (0.0, 6.0));
            assert_eq!(extent(&t, mid), (6.0, 11.0));
            assert_eq!(extent(&t, next), (11.0, 15.0));
        }
    }

    #[test]
    fn a_slide_clamps_to_the_tightest_member() {
        let (mut t, [_, vb, _, ab], asset) = two_cut_pairs();
        // A third clip after each: the picture's has 5 s to give, the sound's only 0.2.
        t.tracks[0].clips.push(clip(asset, 30.0, 35.0, 10.0));
        t.tracks[1].clips.push(clip(asset, 30.0, 30.2, 10.0));
        let (vc, ac) = (id_of(&t, 0, 2), id_of(&t, 1, 2));
        let footage = limits(&[asset]);
        let alone = t.slide_range(vb, &footage).unwrap();
        assert!(alone.max > 4.0, "{alone:?}");
        let out = t.slide_clip_linked(vb, 1.0, &footage).unwrap();
        assert!(out.clamped, "{out:?}");
        assert!((out.applied - 0.15).abs() < 1e-9, "0.2 s less the 0.05 s floor: {out:?}");
        assert!(
            (start(&t, vc) - 10.15).abs() < 1e-9,
            "the picture slid no further than the sound could"
        );
        assert!((start(&t, ac) - 10.15).abs() < 1e-9);
        assert!((start(&t, ab) - 5.15).abs() < 1e-9);
    }

    #[test]
    fn a_slide_with_a_locked_partner_refuses_and_changes_nothing() {
        let (mut t, [_, vb, ..], asset) = two_cut_pairs();
        lock(&mut t, 1);
        let before = serde_json::to_string(&t).unwrap();
        assert!(t.slide_clip_linked(vb, 1.0, &limits(&[asset])).is_err());
        assert!(t.slip_clip_linked(vb, 1.0, &limits(&[asset])).is_err());
        assert_eq!(serde_json::to_string(&t).unwrap(), before);
    }

    // ---- ripple: the sync lock ------------------------------------------------

    /// V1: title [0,5) then c2 [5,15); A1: a2 [5,15) linked to c2. The title has no sound.
    fn title_then_shot() -> (Timeline, Uuid, Uuid, Uuid) {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 20.0, 30.0, 5.0)],
            ),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 20.0, 30.0, 5.0)]),
        ]);
        let (title, c2, a2) = (id_of(&t, 0, 0), id_of(&t, 0, 1), id_of(&t, 1, 0));
        linked(&mut t, &[c2, a2]);
        (t, title, c2, a2)
    }

    fn trimmed_title(before: &Timeline, title: Uuid, by: f64) -> Timeline {
        let mut after = before.clone();
        after.clip_mut(title).unwrap().source_out += by;
        after
    }

    #[test]
    fn a_ripple_takes_the_linked_partner_along() {
        let (before, title, c2, a2) = title_then_shot();
        let after = trimmed_title(&before, title, -1.0);
        let out = after.ripple_from(&before);
        assert_eq!(start(&out, c2), 4.0, "the shot followed the shortened title");
        assert_eq!(start(&out, a2), 4.0, "and so did its sound, which had no edit of its own");
        // Links off: tracks are independent again.
        let out = after.ripple_from_with(&before, false);
        assert_eq!((start(&out, c2), start(&out, a2)), (4.0, 5.0));
    }

    #[test]
    fn only_linked_clips_follow_never_the_rest_of_the_lane() {
        let (mut before, title, c2, a2) = title_then_shot();
        let asset = get(&before, c2).asset_id;
        before.tracks[1].clips.push(clip(asset, 0.0, 5.0, 20.0)); // a music clip, unlinked
        let music = id_of(&before, 1, 1);
        let out = trimmed_title(&before, title, -1.0).ripple_from(&before);
        assert_eq!(start(&out, a2), 4.0);
        assert_eq!(start(&out, music), 20.0, "an unlinked clip stays where it was");
    }

    #[test]
    fn a_partner_that_would_overlap_stays_where_it_was() {
        let (mut before, title, c2, a2) = title_then_shot();
        let asset = get(&before, c2).asset_id;
        before.tracks[1].clips.insert(0, clip(asset, 0.0, 5.0, 0.0)); // unlinked, ends at 5
        let blocker = id_of(&before, 1, 0);
        // Trimming the title 1 s moves the sound to 4..14, onto the clip that ends at 5.
        let out = trimmed_title(&before, title, -1.0).ripple_from(&before);
        assert_eq!(start(&out, c2), 4.0, "the picture still rippled");
        assert_eq!(
            (start(&out, a2), start(&out, blocker)),
            (5.0, 0.0),
            "no overlap is ever produced"
        );
    }

    #[test]
    fn a_locked_partner_track_does_not_move() {
        let (mut before, title, c2, a2) = title_then_shot();
        lock(&mut before, 1);
        let out = trimmed_title(&before, title, -1.0).ripple_from(&before);
        assert_eq!((start(&out, c2), start(&out, a2)), (4.0, 5.0));
    }

    #[test]
    fn tracks_that_already_rippled_the_same_way_are_not_shifted_twice() {
        let asset = Uuid::new_v4();
        let mut before = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 20.0, 30.0, 5.0)],
            ),
            lane(
                StreamKind::Audio,
                "A1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 20.0, 30.0, 5.0)],
            ),
        ]);
        let ids = [
            id_of(&before, 0, 0),
            id_of(&before, 0, 1),
            id_of(&before, 1, 0),
            id_of(&before, 1, 1),
        ];
        linked(&mut before, &[ids[0], ids[2]]);
        linked(&mut before, &[ids[1], ids[3]]);
        // Trim the first pair's tails together (what the linked trim does), then ripple.
        let mut after = before.clone();
        let footage = limits(&[asset]);
        let was = get(&after, ids[0]).clone();
        after.clip_mut(ids[0]).unwrap().source_out = 4.0;
        after.carry_extent_edit(ids[0], &was, &footage).unwrap();
        let out = after.ripple_from(&before);
        assert_eq!((start(&out, ids[1]), start(&out, ids[3])), (4.0, 4.0), "1 s each, not 2");
    }

    #[test]
    fn a_group_whose_members_were_rippled_by_different_amounts_follows_the_first_that_moved() {
        let asset = Uuid::new_v4();
        let mut before = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 20.0, 30.0, 5.0)],
            ),
            lane(
                StreamKind::Audio,
                "A1",
                vec![clip(asset, 0.0, 6.0, 0.0), clip(asset, 20.0, 30.0, 6.0)],
            ),
            lane(StreamKind::Audio, "A2", vec![clip(asset, 20.0, 30.0, 5.0)]),
        ]);
        let (c2, a2, b2) = (id_of(&before, 0, 1), id_of(&before, 1, 1), id_of(&before, 2, 0));
        linked(&mut before, &[c2, a2, b2]);
        // Both V1 and A1 shorten (by 1 and 2): c2 and a2 ripple by different amounts.
        let mut after = before.clone();
        after.tracks[0].clips[0].source_out = 4.0;
        after.tracks[1].clips[0].source_out = 4.0;
        let out = after.ripple_from(&before);
        // No clip was named, so the first member that moved speaks for the group (V1,
        // by 1 s) and the others keep the relationship they had to it: a2 sat 1 s after
        // c2 and b2 level with it.
        assert_eq!((start(&out, c2), start(&out, a2), start(&out, b2)), (4.0, 5.0, 4.0));
    }

    #[test]
    fn a_left_trim_that_ripples_pulls_a_partner_that_never_shared_the_head_along() {
        // c1 [0,10) on V1, its sound only shares the *tail* (it runs 5..10), c2 after it.
        let asset = Uuid::new_v4();
        let mut before = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 0.0, 10.0, 0.0), clip(asset, 20.0, 26.0, 10.0)],
            ),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 5.0, 10.0, 5.0)]),
        ]);
        let (c1, a1) = (id_of(&before, 0, 0), id_of(&before, 1, 0));
        linked(&mut before, &[c1, a1]);
        // The GUI's left-edge trim: in-point and start, so the right edge holds.
        let mut after = before.clone();
        {
            let c = after.clip_mut(c1).unwrap();
            c.source_in = 3.0;
            c.timeline_start = 3.0;
        }
        after.carry_extent_edit(c1, get(&before, c1), &limits(&[asset])).unwrap();
        assert_eq!(
            extent(&after, a1),
            (5.0, 10.0),
            "it never shared the head, so the trim left it be"
        );
        let out = after.ripple_from(&before);
        assert_eq!(
            extent(&out, c1),
            (0.0, 7.0),
            "the ripple holds the start and follows the length"
        );
        assert_eq!(start(&out, id_of(&out, 0, 1)), 7.0);
        // The picture's content slid 3 s earlier (its start was put back while its
        // in-point moved on), so the sound that goes with it slides too — whole, not
        // cut: 3 s of timeline came out of every linked track.
        assert_eq!(extent(&out, a1), (2.0, 7.0), "the sound keeps its place against the picture");
        assert_eq!(out.first_sync_break(&before), None);
    }

    #[test]
    fn a_slide_moves_a_partner_at_another_offset_by_the_same_time() {
        let (mut t, [_, vb, aa, ab], asset) = two_cut_pairs();
        // The sound is one second late all along: its cut is at 6, not 5.
        t.clip_mut(aa).unwrap().source_out = 16.0; // 6 s, 0..6
        t.clip_mut(ab).unwrap().timeline_start = 6.0;
        let out = t.slide_clip_linked(vb, 1.0, &limits(&[asset])).unwrap();
        assert_eq!(out.applied, 1.0);
        assert_eq!(extent(&t, vb).0, 6.0);
        assert_eq!(extent(&t, ab).0, 7.0, "by Δt, not onto the picture's position");
        assert_eq!(extent(&t, aa).1, 7.0, "and its previous clip gave way by the same");
    }

    // ---- the beat snap's re-sync ----------------------------------------------

    #[test]
    fn a_lane_level_retime_is_carried_to_the_partners_afterwards() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 20.0, 30.0, 5.0)],
            ),
            lane(
                StreamKind::Audio,
                "A1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 20.0, 30.0, 5.0)],
            ),
        ]);
        let ids = [id_of(&t, 0, 0), id_of(&t, 0, 1), id_of(&t, 1, 0), id_of(&t, 1, 1)];
        linked(&mut t, &[ids[0], ids[2]]);
        linked(&mut t, &[ids[1], ids[3]]);
        let before = t.clone();
        // What a beat snap does to V1: the first cut lengthens by 0.5, the rest follows.
        t.clip_mut(ids[0]).unwrap().source_out = 5.5;
        t.clip_mut(ids[1]).unwrap().timeline_start = 5.5;
        t.clip_mut(ids[1]).unwrap().source_out = 29.0;
        t.carry_links_since(&before, &limits(&[asset])).unwrap();
        assert_eq!(extent(&t, ids[2]), (0.0, 5.5));
        assert_eq!(extent(&t, ids[3]), (5.5, 14.5), "moved by 0.5 and its tail trimmed by 1");
        assert_eq!(get(&t, ids[3]).source_in, 20.0, "moved, not head-trimmed");
        assert_eq!(get(&t, ids[3]).source_out, 29.0);
    }

    #[test]
    fn a_group_the_edit_changed_in_several_members_is_left_as_made() {
        let (mut t, c, a, asset) = pair();
        let before = t.clone();
        t.clip_mut(c).unwrap().source_out = 7.0;
        t.clip_mut(a).unwrap().source_out = 6.0;
        t.carry_links_since(&before, &limits(&[asset])).unwrap();
        assert_eq!((extent(&t, c), extent(&t, a)), ((0.0, 7.0), (0.0, 6.0)));
    }

    // ---- the sync guard ------------------------------------------------------

    #[test]
    fn a_pair_that_stays_in_step_is_not_a_break_however_far_it_moved() {
        let (before, c, a, _) = pair();
        let mut t = before.clone();
        for id in [c, a] {
            t.clip_mut(id).unwrap().timeline_start = 7.0;
        }
        assert_eq!(t.first_sync_break(&before), None, "moved together");
        // Trimmed differently, but showing the same moment at the same time: still in step.
        let mut t = before.clone();
        {
            let clip = t.clip_mut(c).unwrap();
            clip.source_in = 3.0;
            clip.timeline_start = 3.0;
        }
        assert_eq!(
            t.first_sync_break(&before),
            None,
            "one head trimmed, the content has not moved"
        );
    }

    #[test]
    fn moving_one_clip_of_a_pair_is_a_break_and_names_both_tracks() {
        let (before, c, _, _) = pair();
        let mut t = before.clone();
        t.clip_mut(c).unwrap().timeline_start = 1.0;
        assert_eq!(t.first_sync_break(&before), Some(("V1".to_string(), "A1".to_string())));
        let mut t = before.clone();
        t.clip_mut(c).unwrap().speed = 2.0;
        assert!(t.first_sync_break(&before).is_some(), "a speed the sound does not share");
        // Showing other footage in the same place is a break too (a slip of one).
        let mut t = before.clone();
        {
            let clip = t.clip_mut(c).unwrap();
            clip.source_in = 1.0;
            clip.source_out = 11.0;
        }
        assert!(t.first_sync_break(&before).is_some());
    }

    #[test]
    fn a_pair_that_was_already_apart_or_cannot_be_compared_is_left_alone() {
        let (mut before, c, a, _) = pair();
        before.clip_mut(a).unwrap().timeline_start = 2.0; // apart already
        let mut t = before.clone();
        t.clip_mut(c).unwrap().timeline_start = 5.0;
        assert_eq!(t.first_sync_break(&before), None);
        // Different assets: there is no "same moment of footage" to compare.
        let (mut before, c, a, _) = pair();
        before.clip_mut(a).unwrap().asset_id = Uuid::new_v4();
        let mut t = before.clone();
        t.clip_mut(c).unwrap().timeline_start = 5.0;
        assert_eq!(t.first_sync_break(&before), None);
        // A member that is gone or new is not a pair the edit broke.
        let (before, c, _, _) = pair();
        let mut t = before.clone();
        t.tracks[1].clips.clear();
        t.clip_mut(c).unwrap().timeline_start = 5.0;
        assert_eq!(t.first_sync_break(&before), None);
        assert!(!t.has_links() || t.link_partners(c).is_empty());
    }

    #[test]
    fn a_reversed_pair_is_compared_from_its_out_point() {
        let (mut before, c, a, _) = pair();
        for id in [c, a] {
            before.clip_mut(id).unwrap().speed = -1.0;
        }
        let mut t = before.clone();
        for id in [c, a] {
            let clip = t.clip_mut(id).unwrap();
            clip.source_out = 7.0;
            clip.timeline_start = 3.0;
        }
        assert_eq!(t.first_sync_break(&before), None);
        t.clip_mut(c).unwrap().timeline_start = 3.5;
        assert!(t.first_sync_break(&before).is_some());
    }

    // ---- what detaching leaves alone ----------------------------------------------

    #[test]
    fn a_detached_pair_is_captioned_once_and_the_picture_still_renders() {
        let (before, c, _, asset) = pair();
        let mut t = before.clone();
        t.detach_audio(c, true).unwrap();
        let transcripts = HashMap::from([(
            asset,
            vec![TranscriptSegment {
                start: 1.0,
                end: 4.0,
                text: "hello there".to_string(),
            }],
        )]);
        let words = |t: &Timeline| -> Vec<(String, f64, f64)> {
            t.captions(&transcripts, CaptionOptions::default())
                .iter()
                .map(|o| (o.text.clone(), o.start, o.end))
                .collect()
        };
        // `pair()` is the picture and a hand-made sound reaching the same words; detaching
        // adds a second sound on a free track. Neither doubles a caption.
        assert!(!words(&before).is_empty());
        assert_eq!(words(&t), words(&before));
        // The muted picture is still part of the render: only its sound is dropped.
        assert!(t.for_render().tracks[0].clips.iter().any(|x| x.id == c));
    }

    // ---- detach / reattach -----------------------------------------------------

    fn picture_timeline() -> (Timeline, Uuid) {
        let asset = Uuid::new_v4();
        let t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 2.0, 12.0, 3.0)]),
            lane(StreamKind::Audio, "A1", vec![]),
        ]);
        let id = id_of(&t, 0, 0);
        (t, id)
    }

    #[test]
    fn detaching_makes_a_linked_audio_clip_of_the_same_span_and_mutes_the_picture() {
        let (mut t, id) = picture_timeline();
        {
            let c = t.clip_mut(id).unwrap();
            c.speed = 1.25;
            c.volume = 0.7;
            c.fade_in = 0.3;
            c.fade_out = 0.6;
            c.audio = vec![crate::model::AudioEffect::Highpass { hz: 90.0 }];
        }
        let picture_was = get(&t, id).clone();
        let d = t.detach_audio(id, true).unwrap();
        assert!(!d.created_track);
        assert_eq!(d.track_id, t.tracks[1].id, "V1's sound goes to A1");
        let audio = &d.clip;
        assert_eq!(audio.asset_id, picture_was.asset_id);
        assert_eq!((audio.source_in, audio.source_out), (2.0, 12.0));
        assert_eq!((audio.timeline_start, audio.speed), (3.0, 1.25));
        assert_eq!(audio.timeline_end(), picture_was.timeline_end());
        assert_eq!((audio.volume, audio.fade_in, audio.fade_out), (0.7, 0.3, 0.6));
        assert_eq!(audio.audio, picture_was.audio);
        let picture = get(&t, id);
        assert!(!picture.source_audio);
        assert_eq!(picture.link_id, audio.link_id);
        assert!(picture.link_id.is_some());
        assert_eq!(t.link_partners(id), vec![audio.id]);
        assert_eq!(
            Clip {
                source_audio: true,
                link_id: None,
                ..picture.clone()
            }
            .timeline_start,
            picture_was.timeline_start
        );
        assert_eq!(picture.volume, 0.7, "the picture keeps its own, inert while muted");
        // A second detach is refused: there is no sound left to take.
        assert!(t.detach_audio(id, true).is_err());
    }

    #[test]
    fn the_audio_goes_to_the_track_at_the_pictures_own_position_when_it_has_room() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Video, "V2", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![]),
            lane(StreamKind::Audio, "A2", vec![]),
        ]);
        let v2 = id_of(&t, 1, 0);
        let d = t.detach_audio(v2, true).unwrap();
        assert_eq!(d.track_id, t.tracks[3].id, "V2 → A2");
        // V1's sound then takes A1.
        let v1 = id_of(&t, 0, 0);
        assert_eq!(t.detach_audio(v1, true).unwrap().track_id, t.tracks[2].id);
    }

    #[test]
    fn a_busy_or_locked_audio_track_is_skipped_and_a_new_one_made_when_none_fits() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 9.0, 2.0)]), // in the way
            lane(StreamKind::Audio, "A2", vec![]),
        ]);
        lock(&mut t, 2);
        let v = id_of(&t, 0, 0);
        let d = t.detach_audio(v, true).unwrap();
        assert!(d.created_track, "A1 is occupied, A2 is locked");
        assert_eq!(t.tracks.len(), 4);
        assert_eq!(t.tracks[3].name, "A3");
        assert_eq!(t.tracks[3].kind, StreamKind::Audio);
        assert_eq!(d.track_id, t.tracks[3].id);
        // No audio track at all: one is made.
        let mut bare = timeline(vec![lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)])]);
        let v = id_of(&bare, 0, 0);
        let d = bare.detach_audio(v, true).unwrap();
        assert!(d.created_track);
        assert_eq!(bare.tracks[1].name, "A1");
    }

    #[test]
    fn detaching_refuses_what_has_no_sound_to_detach() {
        let (mut t, id) = picture_timeline();
        assert!(t.detach_audio(id, false).is_err(), "the asset has no audio stream");
        lock(&mut t, 0);
        assert!(t.detach_audio(id, true).is_err(), "the picture's track is locked");
        t.tracks[0].locked = false;
        // An audio-track clip has no sound of its own to detach.
        let d = t.detach_audio(id, true).unwrap();
        assert!(t.detach_audio(d.clip.id, true).is_err());
        assert!(t.detach_audio(Uuid::new_v4(), true).is_err());
        let before = serde_json::to_string(&t).unwrap();
        assert!(t.detach_audio(id, true).is_err(), "already detached");
        assert_eq!(serde_json::to_string(&t).unwrap(), before, "a refusal changes nothing");
    }

    #[test]
    fn a_group_never_puts_two_clips_on_one_track() {
        // The picture is already linked to a clip on A1; its detached sound must not land there.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 50.0, 55.0, 20.0)]),
            lane(StreamKind::Audio, "A2", vec![]),
        ]);
        let (v, other) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[v, other]);
        let d = t.detach_audio(v, true).unwrap();
        assert_eq!(d.track_id, t.tracks[2].id, "A1 holds a member already, so A2");
        assert_eq!(t.link_partners(v).len(), 2);
    }

    #[test]
    fn reattaching_deletes_the_audio_clip_and_unmutes_the_picture() {
        let (mut t, id) = picture_timeline();
        let d = t.detach_audio(id, true).unwrap();
        let picture = t.reattach_audio(id).unwrap();
        assert!(picture.source_audio);
        assert_eq!(picture.link_id, None, "a link of one clip is cleared");
        assert!(t.clip(d.clip.id).is_none());
        assert!(t.tracks[1].clips.is_empty());
        // Naming the audio clip works too.
        let d = t.detach_audio(id, true).unwrap();
        assert!(t.reattach_audio(d.clip.id).unwrap().source_audio);
        assert!(t.clip(d.clip.id).is_none());
        // Not detached: nothing to do.
        assert!(t.reattach_audio(id).is_err());
        assert!(t.reattach_audio(Uuid::new_v4()).is_err());
    }

    #[test]
    fn reattaching_with_the_audio_already_gone_just_unmutes() {
        let (mut t, id) = picture_timeline();
        let d = t.detach_audio(id, true).unwrap();
        let track = t.locate(d.clip.id).unwrap().0;
        t.tracks[track].clips.clear();
        let picture = t.reattach_audio(id).unwrap();
        assert!(picture.source_audio);
    }

    #[test]
    fn reattaching_refuses_a_locked_audio_track() {
        let (mut t, id) = picture_timeline();
        t.detach_audio(id, true).unwrap();
        lock(&mut t, 1);
        assert!(t.reattach_audio(id).is_err());
        assert!(!get(&t, id).source_audio, "still detached");
    }

    #[test]
    fn reattaching_several_is_all_or_nothing_and_a_pair_named_twice_counts_once() {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 0.0, 5.0, 0.0), clip(asset, 10.0, 15.0, 5.0)],
            ),
            lane(StreamKind::Audio, "A1", vec![]),
        ]);
        let (x1, x2) = (id_of(&t, 0, 0), id_of(&t, 0, 1));
        let s1 = t.detach_audio(x1, true).unwrap().clip.id;
        let s2 = t.detach_audio(x2, true).unwrap().clip.id;
        // The picture and its sound are one pair; the order named is the order returned.
        let mut done = t.clone();
        let pictures = done.reattach_audio_many(&[s2, x2, x1]).unwrap();
        assert_eq!(pictures.iter().map(|c| c.id).collect::<Vec<_>>(), vec![x2, x1]);
        assert!(done.tracks[1].clips.is_empty() && get(&done, x1).source_audio && get(&done, x2).source_audio);
        // A second picture whose reattach is refused leaves the first one's sound where it was.
        t.unlink_clips(&[x2]).unwrap();
        let before = serde_json::to_string(&t).unwrap();
        let err = t.reattach_audio_many(&[x1, x2]).unwrap_err().to_string();
        assert!(err.contains("heard twice") && err.contains(&x2.to_string()), "{err}");
        assert_eq!(serde_json::to_string(&t).unwrap(), before, "a refusal changes nothing");
        assert!(t.clip(s1).is_some() && t.clip(s2).is_some());
        // Alone, the refusal is reattach_audio's own words.
        let alone = t.reattach_audio_many(&[x2]).unwrap_err().to_string();
        assert_eq!(alone, t.clone().reattach_audio(x2).unwrap_err().to_string());
        assert!(t.reattach_audio_many(&[]).is_err());
        assert!(t.reattach_audio_many(&[x1, Uuid::new_v4()]).is_err());
        assert_eq!(serde_json::to_string(&t).unwrap(), before);
    }

    #[test]
    fn a_pair_split_after_detaching_reattaches_piecewise() {
        let (mut t, id) = picture_timeline();
        t.detach_audio(id, true).unwrap();
        let (_, right) = t.split_clip_linked(id, 8.0).unwrap();
        assert!(
            !right.source_audio,
            "the right half is muted too: it was cut from a muted clip"
        );
        t.reattach_audio(id).unwrap();
        assert_eq!(t.tracks[1].clips.len(), 1, "only the left half's sound was reattached");
        assert!(get(&t, id).source_audio);
        assert!(!get(&t, right.id).source_audio);
    }

    // ---- the sync lock: range-based, for J- and L-cuts -------------------------

    /// A J/L-cut pair of shots: V1 holds `x1` (5..15, footage 105..115) and `x2`
    /// (15..25, footage 215..225); A1 holds their sound, which **leads** the first
    /// picture by 5 s and trails nothing — `y1` 0..15 (footage 100..115) — and starts
    /// the second sound 2 s before its picture, `y2` 13..25 (footage 213..225). Each
    /// sound is in step with its picture (the same content offset) while covering a
    /// different stretch. Returns `(timeline, [x1, x2, y1, y2], asset)`.
    fn jl_cut() -> (Timeline, [Uuid; 4], Uuid) {
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 105.0, 115.0, 5.0), clip(asset, 215.0, 225.0, 15.0)],
            ),
            lane(
                StreamKind::Audio,
                "A1",
                vec![clip(asset, 100.0, 115.0, 0.0), clip(asset, 213.0, 225.0, 13.0)],
            ),
        ]);
        // Sound of x1 would run on past x2 if it were longer; here y1 ends at 15 and y2 starts at 13:
        // overlapping lanes are illegal, so y1 is trimmed to end where y2 begins.
        t.tracks[1].clips[0].source_out = 113.0;
        let ids = [id_of(&t, 0, 0), id_of(&t, 0, 1), id_of(&t, 1, 0), id_of(&t, 1, 1)];
        linked(&mut t, &[ids[0], ids[2]]);
        linked(&mut t, &[ids[1], ids[3]]);
        (t, ids, asset)
    }

    /// The same edit the project's `run_edit` makes: the per-lane ripple, then the sync
    /// lock with `anchors` named, strictly.
    fn edited(before: &Timeline, after: &Timeline, ripple: bool, anchors: &[Uuid]) -> Result<Timeline> {
        let mut out = if ripple { after.ripple_lanes(before) } else { after.clone() };
        out.conform_links(before, &anchors.iter().copied().collect(), &HashMap::new())?;
        Ok(out)
    }

    #[test]
    fn a_ripple_delete_of_a_jl_cut_closes_by_the_picture_removed_and_keeps_every_later_pair_in_step() {
        let (mut t, [x1, x2, y1, y2], _) = jl_cut();
        assert_eq!(t.first_sync_break(&t.clone()), None);
        let before = t.clone();
        // x1 is 10 s of picture, y1 15 s of sound: a ripple of each lane by its own
        // length would pull y2 5 s further than x2.
        assert_eq!(t.ripple_delete_linked(x1).unwrap(), 2);
        assert!(t.clip(x1).is_none() && t.clip(y1).is_none());
        assert_eq!(extent(&t, x2), (5.0, 15.0), "the picture's lane closed by 10 s");
        assert_eq!(extent(&t, y2), (3.0, 15.0), "and the sound followed by the same 10 s");
        assert_eq!(t.first_sync_break(&before), None);
    }

    #[test]
    fn a_ripple_delete_leaves_an_unlinked_clip_on_the_partners_track_where_it_was() {
        let (mut t, [x1, x2, _, y2], asset) = jl_cut();
        t.tracks[1].clips.push(clip(asset, 0.0, 4.0, 40.0));
        let bed = id_of(&t, 1, 2);
        t.ripple_delete_linked(x1).unwrap();
        assert_eq!(extent(&t, bed), (40.0, 44.0), "only linked clips follow");
        assert_eq!((start(&t, x2), start(&t, y2)), (5.0, 3.0));
    }

    #[test]
    fn a_follower_that_would_run_into_an_unlinked_clip_refuses_and_names_the_lane() {
        let (mut t, [x1, ..], asset) = jl_cut();
        // Something unlinked sits on A1 where the second sound must land (13 - 10 = 3).
        t.tracks[1].clips.push(clip(asset, 0.0, 2.0, 2.0));
        let err = t.ripple_delete_linked(x1).unwrap_err().to_string();
        assert!(
            err.contains("A1") && err.contains("not linked") && !err.contains("links off"),
            "{err}"
        );
        assert!(t.clip(x1).is_some(), "nothing changed");
    }

    #[test]
    fn a_follower_that_lands_on_linked_material_trims_it_back_and_one_it_would_cover_refuses() {
        // Two pairs on V1/A1, the second pair's sound pulled left over the first's.
        let (mut t, [x1, x2, y1, y2], _) = jl_cut();
        // A ripple trim of the first picture's tail by 3 s: x1 5..12, x2 pushed to 12..22.
        let before = t.clone();
        let mut after = t.clone();
        after.clip_mut(x1).unwrap().source_out = 112.0;
        // carry the edge to a partner that shares it (y1 does not: it ends at 13).
        after
            .carry_extent_edit(x1, get(&before, x1), &limits(&[Uuid::new_v4()]))
            .unwrap();
        let out = edited(&before, &after, true, &[x1]).unwrap();
        assert_eq!(extent(&out, x2), (12.0, 22.0));
        // y2 followed by -3: 10..22, which runs into y1 (0..13): y1 is linked, so it is trimmed back.
        assert_eq!(extent(&out, y2), (10.0, 22.0));
        assert_eq!(extent(&out, y1), (0.0, 10.0), "the clip the follower ran into gives way");
        assert_eq!(out.first_sync_break(&before), None);

        // A much bigger pull would cover y1 entirely: refused, with the reason.
        t.tracks[1].clips[0].source_out = 101.0; // y1 is now 1 s long (0..1)
        t.tracks[1].clips[1].timeline_start = 1.5;
        t.tracks[1].clips[1].source_in = 201.5;
        let before = t.clone();
        let mut after = t.clone();
        after.clip_mut(x2).unwrap().timeline_start = 0.0; // dragged far left, named
        let err = edited(&before, &after, false, &[x2]).unwrap_err().to_string();
        assert!(err.contains("cover") && err.contains("A1"), "{err}");
    }

    #[test]
    fn a_follower_pulled_before_zero_loses_its_head_and_keeps_its_sync() {
        // x2 (15..25) is pulled to 0 by a named move: its sound y2 (13..25, led by 2 s)
        // follows by -15 to -2..10, and the 2 s that hang off the start are trimmed away.
        let (t, [x1, x2, y1, y2], _) = jl_cut();
        let mut t = t;
        t.remove_clips(&[x1, y1]).unwrap();
        let before = t.clone();
        let mut after = t.clone();
        after.clip_mut(x2).unwrap().timeline_start = 0.0;
        let out = edited(&before, &after, false, &[x2]).unwrap();
        let y = get(&out, y2);
        assert_eq!((y.timeline_start, y.timeline_end()), (0.0, 10.0));
        assert_eq!(
            y.source_in, 215.0,
            "the head was trimmed — footage 213..215 is gone — not the clip slid"
        );
        assert_eq!(out.first_sync_break(&before), None);
    }

    #[test]
    fn the_clip_an_edit_names_speaks_for_the_group_and_its_track_for_the_rest() {
        let (t, [x1, x2, y1, y2], _) = jl_cut();
        // The sound track was pushed 4 s by something of its own, the picture track 6 s.
        let before = t.clone();
        let mut after = t;
        after.clip_mut(x2).unwrap().timeline_start += 6.0;
        after.clip_mut(y2).unwrap().timeline_start += 4.0;
        // Naming the picture clip x2: y2 is brought to where x2 went (+6).
        let by_picture = edited(&before, &after, false, &[x2]).unwrap();
        assert_eq!((start(&by_picture, x2), start(&by_picture, y2)), (21.0, 19.0));
        // Naming the sound: x2 follows it (+4).
        let by_sound = edited(&before, &after, false, &[y2]).unwrap();
        assert_eq!((start(&by_sound, x2), start(&by_sound, y2)), (19.0, 17.0));
        // Naming a *different* clip on the picture's track still makes that track the authority.
        let by_lane = edited(&before, &after, false, &[x1]).unwrap();
        assert_eq!((start(&by_lane, x2), start(&by_lane, y2)), (21.0, 19.0));
        // Naming nothing: the first member (track order) that moved.
        let nobody = edited(&before, &after, false, &[]).unwrap();
        assert_eq!((start(&nobody, x2), start(&nobody, y2)), (21.0, 19.0));
        let _ = y1;
    }

    #[test]
    fn two_partners_both_named_and_moved_apart_are_left_for_the_guard() {
        let (t, [_, x2, _, y2], _) = jl_cut();
        let before = t.clone();
        let mut after = t;
        after.clip_mut(x2).unwrap().timeline_start += 6.0;
        after.clip_mut(y2).unwrap().timeline_start += 4.0;
        let out = edited(&before, &after, false, &[x2, y2]).unwrap();
        assert_eq!((start(&out, x2), start(&out, y2)), (21.0, 17.0), "the lock does not choose");
        assert!(out.first_sync_break(&before).is_some(), "the guard does");
    }

    #[test]
    fn a_follower_on_a_locked_track_refuses() {
        let (mut t, [x1, ..], _) = jl_cut();
        lock(&mut t, 1);
        let before = t.clone();
        let mut after = t.clone();
        after.clip_mut(x1).unwrap().timeline_start += 1.0;
        let err = edited(&before, &after, false, &[x1]).unwrap_err().to_string();
        assert!(
            err.contains("locked") && err.contains("A1") && !err.contains("links off"),
            "{err}"
        );
    }

    #[test]
    fn a_speed_change_re_places_the_partners_about_the_named_clip() {
        let (mut t, [x1, x2, y1, y2], _) = jl_cut();
        let before = t.clone();
        // x1 (5..15) to 2x: 5..10. y1 led it by 5 s: re-placed about x1's start, its lead
        // halves too — 2.5..10 — so it still begins 2.5 s before the picture's first frame.
        let mut after = t.clone();
        after.set_speed_linked(x1, 2.0).unwrap();
        assert_eq!(get(&after, y1).speed, 2.0);
        let out = edited(&before, &after, true, &[x1]).unwrap();
        assert_eq!(extent(&out, x1), (5.0, 10.0));
        // y1 was 0..13: its lead stretched by the same ratio, 2.5.., and its tail gave way to
        // the next sound, which leads its picture by 2 s and so now starts at 8.
        assert_eq!(extent(&out, y1), (2.5, 8.0));
        // The pair after it, pushed by the 5 s the picture lost, keeps its own lead.
        assert_eq!(extent(&out, x2), (10.0, 20.0));
        assert_eq!(extent(&out, y2), (8.0, 20.0));
        assert_eq!(out.first_sync_break(&before), None);
        t.tracks[1].locked = true;
        assert!(t.set_speed_linked(x1, 2.0).is_err());
    }

    #[test]
    fn a_cut_range_whose_stretch_swallows_a_partners_head_resumes_it_at_the_cut() {
        // x 0..20 (footage 0..20), its sound only 8..20 (starts inside the stretch we cut).
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 20.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 8.0, 20.0, 8.0)]),
        ]);
        let (x, y) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[x, y]);
        let before = t.clone();
        let kept = t.cut_clip_range_linked(x, 5.0, 12.0).unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(extent(&t, x), (0.0, 5.0));
        let (x_tail, y_tail) = (kept[1].id, id_of(&t, 1, 0));
        assert_eq!(extent(&t, x_tail), (5.0, 13.0));
        // The sound lost 8..12, and what survived (footage 12..20) resumes at the cut — 5 —
        // exactly where the picture's footage 12 now plays.
        assert_eq!(y_tail, y, "the sole survivor keeps its id");
        assert_eq!(extent(&t, y), (5.0, 13.0));
        assert_eq!(get(&t, y).source_in, 12.0);
        assert_eq!(t.link_partners(x_tail), vec![y], "the two tails are a pair");
        assert_eq!(
            t.link_partners(x),
            Vec::<Uuid>::new(),
            "the head has nothing left to be linked to"
        );
        assert_eq!(t.first_sync_break(&before), None);
    }

    #[test]
    fn a_cut_range_cuts_a_partner_that_spans_the_stretch_in_two_and_the_tail_follows() {
        let (mut t, [x1, x2, y1, y2], _) = jl_cut();
        // x1 5..15 (footage 105..115); cut footage 108..111 = timeline 8..11.
        let kept = t.cut_clip_range_linked(x1, 108.0, 111.0).unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(extent(&t, x1), (5.0, 8.0));
        assert_eq!(extent(&t, kept[1].id), (8.0, 12.0));
        // y1 0..13 loses the same 8..11: 0..8 stays and 11..13 comes up to 8..10.
        let sound: Vec<(f64, f64)> = t.tracks[1]
            .clips
            .iter()
            .map(|c| (c.timeline_start, c.timeline_end()))
            .collect();
        assert_eq!(sound[0], (0.0, 8.0));
        assert_eq!(sound[1], (8.0, 10.0));
        // ...and the second pair closed up by 3 on both lanes.
        assert_eq!((extent(&t, x2), extent(&t, y2)), ((12.0, 22.0), (10.0, 22.0)));
        let _ = y1;
        let before = jl_cut().0;
        // (The pre-cut pairs were in step; everything the cut left is: the new ids have no before.)
        assert_eq!(t.first_sync_break(&before), None);
    }

    #[test]
    fn a_split_hands_an_unsplit_partner_to_the_side_it_lies_on() {
        // Picture 0..10, its sound only 5..10: split the picture at 3 and the sound lies
        // wholly after the cut, so it belongs with the right half.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 5.0, 10.0, 5.0)]),
        ]);
        let (c, a) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[c, a]);
        let (left, right) = t.split_clip_linked(c, 3.0).unwrap();
        assert_eq!(t.link_partners(left.id), Vec::<Uuid>::new(), "the left half is alone");
        assert_eq!(get(&t, left.id).link_id, None, "and a group of one is no group");
        assert_eq!(
            t.link_partners(right.id),
            vec![a],
            "the sound goes with the half it lies under"
        );
        // Moving the left half no longer drags the sound; moving the right one does.
        let moves = t
            .with_linked_moves(&[ClipMove {
                clip_id: left.id,
                timeline_start: 1.0,
                track_id: None,
            }])
            .unwrap();
        assert_eq!(moves.len(), 1);
        let moves = t
            .with_linked_moves(&[ClipMove {
                clip_id: right.id,
                timeline_start: 4.0,
                track_id: None,
            }])
            .unwrap();
        assert_eq!(moves.len(), 2);

        // And a partner wholly before the cut stays with the left half.
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 4.0, 0.0)]),
        ]);
        let (c, a) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[c, a]);
        let (left, right) = t.split_clip_linked(c, 6.0).unwrap();
        assert_eq!(t.link_partners(left.id), vec![a]);
        assert_eq!(get(&t, right.id).link_id, None);
    }

    #[test]
    fn orphaned_links_are_dissolved_in_one_pass() {
        let (mut t, c, a, asset) = pair();
        let lone = clip(asset, 0.0, 1.0, 50.0);
        let lone_id = lone.id;
        t.tracks[0].clips.push(lone);
        t.clip_mut(lone_id).unwrap().link_id = Some(Uuid::new_v4());
        assert!(t.dissolve_all_orphans(), "the stale one went");
        assert_eq!(get(&t, lone_id).link_id, None);
        assert_eq!(t.link_partners(c), vec![a], "a real pair is left alone");
        assert!(!t.dissolve_all_orphans(), "and nothing is left to do");
        // The index says the same thing in one pass.
        assert_eq!(t.linked_clip_ids(), HashSet::from([c, a]));
    }

    #[test]
    fn detaching_several_clips_skips_what_cannot_be_and_fails_only_when_all_are() {
        let asset = Uuid::new_v4();
        let silent = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![
                    clip(asset, 0.0, 5.0, 0.0),
                    clip(silent, 0.0, 5.0, 5.0),
                    clip(asset, 5.0, 9.0, 10.0),
                ],
            ),
            lane(StreamKind::Audio, "A1", vec![]),
        ]);
        let ids: Vec<Uuid> = t.tracks[0].clips.iter().map(|c| c.id).collect();
        let done = t.detach_audio_many(&ids, &|a| a == asset).unwrap();
        assert_eq!((done.detached.len(), done.skipped.len()), (2, 1));
        assert_eq!(done.skipped[0].clip_id, ids[1]);
        assert!(done.skipped[0].reason.contains("no audio"), "{}", done.skipped[0].reason);
        let before = t.clone();
        let err = t.detach_audio_many(&ids, &|a| a == asset).unwrap_err().to_string();
        assert!(err.contains("already detached") || err.contains("no audio"), "{err}");
        assert_eq!(serde_json::to_string(&t).unwrap(), serde_json::to_string(&before).unwrap());
    }

    // ---- the second review: authority of named members, leftovers, victims, faders ----

    /// [`edited`], judging what the edit moved apart on `after` as the edit left it (the timeline
    /// before the per-lane ripple), and reporting the sounds trimmed.
    fn edited_noted(before: &Timeline, after: &Timeline, ripple: bool, anchors: &[Uuid]) -> Result<(Timeline, Vec<String>)> {
        let mut out = if ripple { after.ripple_lanes(before) } else { after.clone() };
        let mut notes = Vec::new();
        out.conform_links_noted(
            before,
            &anchors.iter().copied().collect(),
            &HashMap::new(),
            Some(after),
            &mut notes,
        )?;
        Ok((out, notes))
    }

    #[test]
    fn named_partners_that_the_edit_cut_together_are_one_authority_whatever_the_ripple_did() {
        // x1 5..15 / y1 0..13, then x2 15..25 / y2 13..25: trim to the playhead at 8 from
        // the left names *both* x1 and y1 and cuts them at the same time — they agree.
        let (t, [x1, x2, y1, y2], _) = jl_cut();
        let before = t.clone();
        let mut after = t;
        for id in [x1, y1] {
            let c = after.clip_mut(id).unwrap();
            let head = 8.0 - c.timeline_start;
            c.source_in += head;
            c.timeline_start = 8.0;
        }
        let (out, notes) = edited_noted(&before, &after, true, &[x1, y1]).unwrap();
        // The ripple puts each start back and pulls each track in by its own length (x1 lost 3 s,
        // y1 8 s): the first named member (V1) is the authority and the sound follows.
        assert_eq!(extent(&out, x1), (5.0, 12.0));
        assert_eq!(
            extent(&out, y1),
            (5.0, 10.0),
            "y1 is moved to x1's lane: it lost 5 s, x1 only 3"
        );
        assert_eq!(extent(&out, y2), (10.0, 22.0));
        let (x, y) = (get(&out, x1), get(&out, y1));
        assert!(
            (content_offset(x) - content_offset(y)).abs() < 1e-9,
            "the pair stayed in step"
        );
        assert!((start(&out, x2) - 12.0).abs() < 1e-9, "the picture track closed by 3");
        assert!((content_offset(get(&out, x2)) - content_offset(get(&out, y2))).abs() < 1e-9);
        assert!(out.first_sync_break(&before).is_none());
        assert!(notes.is_empty() || notes == ["A1".to_string()]);
        // Moved apart by the edit itself they are still left for the guard.
        let mut apart = before.clone();
        apart.clip_mut(x1).unwrap().timeline_start += 1.0;
        apart.clip_mut(y1).unwrap().timeline_start += 2.0;
        let (out, _) = edited_noted(&before, &apart, true, &[x1, y1]).unwrap();
        assert!(out.first_sync_break(&before).is_some());
    }

    #[test]
    fn the_guard_names_the_lowest_pair_of_tracks_whatever_order_it_scans_in() {
        let asset = Uuid::new_v4();
        let mut before = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Video, "V2", vec![clip(asset, 0.0, 5.0, 10.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 0.0, 5.0, 0.0)]),
            lane(StreamKind::Audio, "A2", vec![clip(asset, 0.0, 5.0, 10.0)]),
        ]);
        let ids = [
            id_of(&before, 0, 0),
            id_of(&before, 1, 0),
            id_of(&before, 2, 0),
            id_of(&before, 3, 0),
        ];
        linked(&mut before, &[ids[0], ids[2]]);
        linked(&mut before, &[ids[1], ids[3]]);
        let mut after = before.clone();
        after.clip_mut(ids[2]).unwrap().timeline_start = 1.0; // V1/A1 apart
        after.clip_mut(ids[3]).unwrap().timeline_start = 11.0; // V2/A2 apart
                                                               // A fresh hash map holds the groups in a different order every time it is built.
        for _ in 0..40 {
            assert_eq!(after.first_sync_break(&before), Some(("V1".to_string(), "A1".to_string())));
        }
    }

    #[test]
    fn a_follower_never_trims_a_picture_and_reports_the_sound_it_does_trim() {
        // The sound track is named: V1's second picture follows y2 onto the first one.
        let (t, [x1, x2, y1, y2], _) = jl_cut();
        let before = t.clone();
        let mut after = t.clone();
        after.clip_mut(y2).unwrap().timeline_start -= 4.0;
        let err = edited_noted(&before, &after, false, &[y2]).unwrap_err().to_string();
        assert!(
            err.contains("picture") && err.contains("V1") && err.contains("never trimmed"),
            "{err}"
        );
        // A sound in the way is trimmed back — and reported.
        let mut after = t;
        after.clip_mut(x2).unwrap().timeline_start -= 3.0;
        let (out, notes) = edited_noted(&before, &after, false, &[x2]).unwrap();
        assert_eq!(extent(&out, y2), (10.0, 22.0));
        assert_eq!(extent(&out, y1), (0.0, 10.0));
        assert_eq!(notes, ["A1".to_string()]);
        let _ = x1;
    }

    #[test]
    fn a_clip_that_would_be_left_under_the_floor_is_refused_not_stubbed() {
        let (mut t, [_, x2, y1, _], _) = jl_cut();
        // y1 is 13 s long (0..13); y2 would land 12.97 s in: 0.03 s of y1 would remain.
        t.clip_mut(y1).unwrap().source_out = 100.0 + 13.0;
        let before = t.clone();
        let mut after = t;
        after.clip_mut(x2).unwrap().timeline_start -= 2.03;
        let err = edited_noted(&before, &after, false, &[x2]);
        assert!(err.is_ok(), "x2 pulled 2.03 s leaves 10.97 s of y1: {err:?}");
        let mut after = before.clone();
        after.clip_mut(x2).unwrap().timeline_start -= 12.97;
        let err = edited_noted(&before, &after, false, &[x2]).unwrap_err().to_string();
        assert!(err.contains("under 0.05s") && err.contains("A1"), "{err}");
    }

    #[test]
    fn a_sound_pulled_before_zero_is_trimmed_and_reported_and_a_picture_refuses() {
        let (t, [x1, x2, y1, y2], _) = jl_cut();
        let mut t = t;
        t.remove_clips(&[x1, y1]).unwrap();
        let before = t.clone();
        let mut after = t.clone();
        after.clip_mut(x2).unwrap().timeline_start = 0.0;
        let (out, notes) = edited_noted(&before, &after, false, &[x2]).unwrap();
        assert_eq!(start(&out, y2), 0.0);
        assert_eq!(notes, ["A1".to_string()], "the lost lead is reported");
        // A picture that would have to start before 0 refuses: x 0..10, its sound y 3..10 named and
        // pulled up to 0 takes the picture with it.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 3.0, 10.0, 3.0)]),
        ]);
        let (x, y) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[x, y]);
        let before = t.clone();
        let mut after = t;
        after.clip_mut(y).unwrap().timeline_start = 0.0;
        let err = edited_noted(&before, &after, false, &[y]).unwrap_err().to_string();
        assert!(err.contains("V1") && err.contains("never trimmed"), "{err}");
    }

    #[test]
    fn a_move_that_carries_a_sound_before_zero_reports_it_and_a_picture_refuses() {
        // The picture at 5, its sound leading it by 3 (at 2): moving the picture to 0 would put the sound at -3.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 5.0, 15.0, 5.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 2.0, 15.0, 2.0)]),
        ]);
        let (x, y) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[x, y]);
        let was = get(&t, x).clone();
        t.clip_mut(x).unwrap().timeline_start = 0.0;
        let mut notes = Vec::new();
        t.carry_extent_edit_noted(x, &was, &limits(&[asset]), &mut notes).unwrap();
        assert_eq!(notes, ["A1".to_string()]);
        assert_eq!(extent(&t, y), (0.0, 10.0));
        assert_eq!(get(&t, y).source_in, 5.0);
        // The other way round, the picture is the one that would start before 0: refused, nothing changes.
        let mut u = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 2.0, 15.0, 2.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 5.0, 15.0, 5.0)]),
        ]);
        let (p, s) = (id_of(&u, 0, 0), id_of(&u, 1, 0));
        linked(&mut u, &[p, s]);
        let was_s = get(&u, s).clone();
        u.clip_mut(s).unwrap().timeline_start = 0.0;
        let picture = serde_json::to_value(&u.tracks[0]).unwrap();
        let err = u.carry_extent_edit(s, &was_s, &limits(&[asset])).unwrap_err().to_string();
        assert!(err.contains("V1") && err.contains("never trimmed"), "{err}");
        assert_eq!(serde_json::to_value(&u.tracks[0]).unwrap(), picture);
        // And a sound that would be left under the floor is refused outright.
        let mut w = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 5.0, 15.0, 5.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 2.0, 5.04, 2.0)]),
        ]);
        let (px, sx) = (id_of(&w, 0, 0), id_of(&w, 1, 0));
        linked(&mut w, &[px, sx]);
        let was_w = get(&w, px).clone();
        w.clip_mut(px).unwrap().timeline_start = 0.0;
        let err = w.carry_extent_edit(px, &was_w, &limits(&[asset])).unwrap_err().to_string();
        assert!(err.contains("under 0.05s"), "{err}");
    }

    #[test]
    fn a_cut_that_leaves_a_partners_head_no_group_does_not_make_it_an_obstacle() {
        // V1 X 5..15 / A1 S 0..13, X2 15..25 / S2 13..25 (footage offset -10): cutting all of X.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(
                StreamKind::Video,
                "V1",
                vec![clip(asset, 5.0, 15.0, 5.0), clip(asset, 25.0, 35.0, 15.0)],
            ),
            lane(
                StreamKind::Audio,
                "A1",
                vec![clip(asset, 0.0, 13.0, 0.0), clip(asset, 23.0, 35.0, 13.0)],
            ),
        ]);
        let ids = [id_of(&t, 0, 0), id_of(&t, 0, 1), id_of(&t, 1, 0), id_of(&t, 1, 1)];
        linked(&mut t, &[ids[0], ids[2]]);
        linked(&mut t, &[ids[1], ids[3]]);
        let before = t.clone();
        let mut notes = Vec::new();
        t.cut_clip_range_linked_noted(ids[0], 5.0, 15.0, &mut notes).unwrap();
        // X is gone; S keeps its head (0..5), now trimmed by S2 coming up to 3 — a leftover of a
        // linked clip, so it gives way, and the edit says so.
        assert_eq!(extent(&t, ids[1]), (5.0, 15.0));
        assert_eq!(extent(&t, ids[3]), (3.0, 15.0));
        assert_eq!(extent(&t, ids[2]), (0.0, 3.0));
        assert_eq!(notes, ["A1".to_string()]);
        assert_eq!(get(&t, ids[2]).link_id, None, "and it is no longer linked to anything");
        assert!(t.first_sync_break(&before).is_none());
    }

    #[test]
    fn a_lone_leftover_of_a_partner_still_resumes_at_the_cut() {
        // X 0..10 / S 9..15 (the sound starts inside what is cut): cut X's 8..10.
        let asset = Uuid::new_v4();
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 9.0, 15.0, 9.0)]),
        ]);
        let (x, s) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[x, s]);
        t.cut_clip_range_linked(x, 8.0, 10.0).unwrap();
        assert_eq!(extent(&t, x), (0.0, 8.0));
        assert_eq!(
            extent(&t, s),
            (8.0, 13.0),
            "what survives resumes at the cut, not where its head was"
        );
        assert_eq!(get(&t, s).source_in, 10.0);
        // A partner wholly after the stretch comes up by it even when the named clip keeps nothing after.
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![clip(asset, 0.0, 10.0, 0.0)]),
            lane(StreamKind::Audio, "A1", vec![clip(asset, 12.0, 20.0, 12.0)]),
        ]);
        let (x, s) = (id_of(&t, 0, 0), id_of(&t, 1, 0));
        linked(&mut t, &[x, s]);
        t.cut_clip_range_linked(x, 8.0, 10.0).unwrap();
        assert_eq!(extent(&t, s), (10.0, 18.0));
    }

    fn compressed(volume: f32) -> Clip {
        let mut c = clip(Uuid::new_v4(), 0.0, 10.0, 0.0);
        c.volume = volume;
        c.audio = vec![AudioEffect::Compressor {
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
        }];
        c
    }

    #[test]
    fn a_compressor_or_gate_makes_detach_meet_the_same_fader_instead_of_folding_it() {
        for dynamic in [
            AudioEffect::Compressor {
                threshold_db: -18.0,
                ratio: 4.0,
                attack_ms: 10.0,
                release_ms: 100.0,
                makeup_db: 0.0,
            },
            AudioEffect::Gate { threshold_db: -40.0 },
        ] {
            let mut c = compressed(0.8);
            c.audio = vec![dynamic];
            let id = c.id;
            let mut t = timeline(vec![
                lane(StreamKind::Video, "V1", vec![c]),
                lane(StreamKind::Audio, "A1", vec![]),
                lane(StreamKind::Audio, "A2", vec![]),
            ]);
            t.tracks[0].volume = 0.5;
            t.tracks[1].volume = 2.0;
            t.tracks[2].volume = 0.5;
            // A1's fader differs; A2's equals the picture track's: the sound goes to A2, volume untouched.
            let d = t.detach_audio(id, true).unwrap();
            assert_eq!((d.track_id, d.created_track, d.clip.volume), (t.tracks[2].id, false, 0.8));
            // With no lane at that fader a new one is made at it.
            let mut c = compressed(0.8);
            let id2 = c.id;
            c.audio = t.tracks[0].clips[0].audio.clone();
            let mut u = timeline(vec![
                lane(StreamKind::Video, "V1", vec![c]),
                lane(StreamKind::Audio, "A1", vec![]),
            ]);
            u.tracks[0].volume = 0.5;
            u.tracks[1].volume = 2.0;
            let d = u.detach_audio(id2, true).unwrap();
            assert!(d.created_track);
            let made = u.tracks.iter().find(|x| x.id == d.track_id).unwrap();
            assert_eq!((made.name.as_str(), made.volume, d.clip.volume), ("A2", 0.5, 0.8));
            assert_eq!(
                u.tracks[1].clips.len(),
                0,
                "A1 stayed empty: its fader would have moved the level"
            );
        }
        // Linear chains still fold, and equal faders need neither.
        let mut c = clip(Uuid::new_v4(), 0.0, 10.0, 0.0);
        c.audio = vec![AudioEffect::Highpass { hz: 80.0 }];
        let id = c.id;
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", vec![c]),
            lane(StreamKind::Audio, "A1", vec![]),
        ]);
        t.tracks[0].volume = 0.5;
        t.tracks[1].volume = 2.0;
        let d = t.detach_audio(id, true).unwrap();
        assert!(!d.created_track);
        assert!(
            (d.clip.volume * 2.0 - 0.5).abs() < 1e-6,
            "a high-pass is linear: the fader folds"
        );
    }
}
