//! Linked clips: a picture and its sound as one piece of material.
//!
//! Clips that share a [`Clip::link_id`] are a **link group** — in practice a video
//! clip and the audio clip that carries its sound (`Project::detach_audio` makes
//! the pair), at most one clip of a group per track. Linking is *identity*, not
//! position: two linked clips may sit at different times and have different
//! lengths, and an edit carries the **change** to the partners, never forces them
//! to line up. Everything here is pure and unit-tested; `Project` decides whether
//! links are in force for a call (`Project::with_links`) and hands the edit to
//! these.
//!
//! What each edit does to the partners of the clip it names:
//!
//! | edit | the partners… |
//! |---|---|
//! | move (`with_linked_moves`) | move by the same Δt, **on their own tracks** — a track change belongs to the clip you named |
//! | trim (`carry_extent_edit`) | follow the edge that changed **when they share it** (within 1 ms), clamped to their own footage |
//! | split (`split_clip_linked`) | are split at the same timeline time (if it is inside them); the new halves are linked to each other |
//! | remove / ripple delete | are removed too (`with_link_partners`, `ripple_delete_linked`) |
//! | cut a source span (`cut_clip_range_linked`) | lose the same stretch of *timeline*, and close up |
//! | speed (`set_speed_linked`) | are retimed by the same ratio |
//! | split and remove (`with_linked_cuts`) | are cut at the same time, if it is inside them |
//! | roll / slip / slide (`*_linked`) | get the same edit (roll: a partner *pair* sharing the cut), and the whole group clamps to its tightest member |
//! | ripple (`follow_links`) | follow a partner the ripple moved, by the same amount |
//!
//! **A locked partner refuses the whole edit** (all or nothing). Property edits —
//! volume, fades, effects, colour, transitions — are *not* carried: a picture and
//! its sound legitimately differ in those. `reorder` is lane-level and not
//! link-aware, and the beat snap re-syncs afterwards through `carry_links_since`.
//!
//! Under all of it sits the **sync guard** (`first_sync_break`, run by
//! `Project`'s `run_edit` after every edit): two linked clips of one asset at one
//! speed are *in step* when the moment of footage playing at a given timeline time is
//! the same in both, and an edit that would leave an in-step pair apart — a ripple
//! whose partner another clip blocks, a left trim of a pair never cut to the same
//! edges, a `reorder` — is refused instead of silently desynchronizing sound from
//! picture. `link: false` is how a pair is parted on purpose.

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
    Error::InvalidArgument(format!(
        "a linked clip is on locked track {} — unlock it, or edit with links off",
        track.name
    ))
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

    /// The other clips of `clip_id`'s link group, in track order — empty for an
    /// unlinked clip, a clip that is not on the timeline, and a link whose partners
    /// are all gone.
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
        let mut seen: HashSet<Uuid> = ids.iter().copied().collect();
        let mut out: Vec<Uuid> = Vec::with_capacity(ids.len());
        let mut seen_first = HashSet::new();
        for id in ids {
            if seen_first.insert(*id) {
                out.push(*id);
            }
        }
        for id in ids {
            for partner in self.link_partners(*id) {
                if seen.insert(partner) {
                    out.push(partner);
                }
            }
        }
        out
    }

    fn clip_mut(&mut self, clip_id: Uuid) -> Option<&mut Clip> {
        let (ti, ci) = self.locate(clip_id)?;
        Some(&mut self.tracks[ti].clips[ci])
    }

    /// The partners of `clip_id` that are not in `skip`, each on an unlocked track —
    /// or the error that refuses the whole edit.
    fn unlocked_partners(&self, clip_id: Uuid, skip: &HashSet<Uuid>) -> Result<Vec<Uuid>> {
        let mut out = Vec::new();
        for partner in self.link_partners(clip_id) {
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

    /// Give `ids` — clips an edit just created from one clip and its partners —
    /// one fresh link group when there are two or more of them, else none: a right
    /// half whose partner was not split has nothing to be linked to.
    fn relink_new_halves(&mut self, ids: &[Uuid]) {
        let group = (ids.len() >= 2).then(Uuid::new_v4);
        for id in ids {
            if let Some(clip) = self.clip_mut(*id) {
                clip.link_id = group;
            }
        }
    }

    // ---- the sync guard -----------------------------------------------------

    /// Whether any clip is linked to another — the cheap test that lets an unlinked
    /// project skip the sync guard (and its snapshot) entirely.
    pub fn has_links(&self) -> bool {
        self.tracks.iter().any(|t| t.clips.iter().any(|c| c.link_id.is_some()))
    }

    /// The **sync guard**: the first link group that the edit which turned `before`
    /// into `self` pulled out of step, as the names of the two tracks — or `None`.
    ///
    /// Two linked clips of the same asset at the same speed are *in step* when the
    /// moment of footage playing at any timeline time is the same in both — equal
    /// content offsets, whatever stretch of it each one shows. Every link-aware edit
    /// carries its change so that holds; this is the net under all of them: an edit
    /// that would leave an in-step pair apart (a ripple whose partner is blocked by
    /// another clip, a left trim of a pair that was never cut to the same edges, a
    /// `reorder`) is *refused* rather than silently desynchronizing sound from
    /// picture. A pair that was already apart, or whose clips cannot be compared
    /// (different assets), is not the edit's doing and is not looked at.
    pub fn first_sync_break(&self, before: &Timeline) -> Option<(String, String)> {
        let prior: HashMap<Uuid, &Clip> = before.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c)).collect();
        let mut groups: HashMap<Uuid, Vec<(&Track, &Clip)>> = HashMap::new();
        for track in &self.tracks {
            for clip in &track.clips {
                if let Some(link) = clip.link_id {
                    groups.entry(link).or_default().push((track, clip));
                }
            }
        }
        const STEP_EPS: f64 = 1e-6;
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
                        return Some((ta.name.clone(), tb.name.clone()));
                    }
                }
            }
        }
        None
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
    /// the first audio track that does; never a locked one, nor one in `avoid`.
    /// `None` when no audio track will do.
    fn audio_lane_for(&self, video_track: usize, span: (f64, f64), avoid: &HashSet<usize>) -> Option<usize> {
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
    /// sound is muted (`Clip::source_audio` false) — so the sound is heard once, from
    /// the audio track, where its fader, pan, ducking and mute apply.
    ///
    /// The audio clip carries what shapes the *sound*: volume, audio effects, fades
    /// and the transition (a crossfade or dip fades the sound too). The picture clip
    /// keeps its own copies, inert while muted, so [`Timeline::reattach_audio`]
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
        let (lane_ix, created_track) = match self.audio_lane_for(vi, span, &avoid) {
            Some(lane_ix) => (lane_ix, false),
            None => {
                let count = self.tracks.iter().filter(|t| t.kind == StreamKind::Audio).count();
                self.tracks.push(Track::new(StreamKind::Audio, format!("A{}", count + 1)));
                (self.tracks.len() - 1, true)
            }
        };
        let group = clip.link_id.unwrap_or_else(Uuid::new_v4);
        let mut audio = Clip::new(clip.asset_id, clip.source_in, clip.source_out, clip.timeline_start);
        audio.speed = clip.speed;
        audio.volume = clip.volume;
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

    /// **Reattach** detached sound: the audio clip(s) linked to the picture clip that
    /// carry the same asset are deleted and the picture clip plays its own sound
    /// again, exactly as it was before the detach (edits made to the audio clip are
    /// not carried back). Name either the picture clip or its audio clip. A picture
    /// whose audio clip is already gone is just unmuted. Returns the picture clip.
    pub fn reattach_audio(&mut self, clip_id: Uuid) -> Result<Clip> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let named = &self.tracks[ti].clips[ci];
        let picture_id = if self.tracks[ti].kind == StreamKind::Video {
            clip_id
        } else {
            self.link_partners(clip_id)
                .into_iter()
                .find(|p| {
                    self.clip(*p).is_some_and(|c| !c.source_audio && c.asset_id == named.asset_id)
                        && self
                            .locate(*p)
                            .is_some_and(|(pt, _)| self.tracks[pt].kind == StreamKind::Video)
                })
                .ok_or_else(|| Error::InvalidArgument("no linked picture whose sound was detached".to_string()))?
        };
        let (vi, vc) = self.locate(picture_id).expect("the picture is on the timeline");
        let picture = &self.tracks[vi].clips[vc];
        if picture.source_audio {
            return Err(Error::InvalidArgument("this clip's sound is not detached".to_string()));
        }
        if self.tracks[vi].locked {
            return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[vi].name)));
        }
        let asset = picture.asset_id;
        let group = picture.link_id;
        let doomed: Vec<Uuid> = self
            .link_partners(picture_id)
            .into_iter()
            .filter(|p| {
                self.locate(*p).is_some_and(|(pt, pc)| {
                    self.tracks[pt].kind == StreamKind::Audio && self.tracks[pt].clips[pc].asset_id == asset
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
        for track in &mut self.tracks {
            track.clips.retain(|c| !doomed.contains(&c.id));
        }
        self.clip_mut(picture_id).expect("the picture stays").source_audio = true;
        if let Some(group) = group {
            self.dissolve_orphans(&HashSet::from([group]));
        }
        Ok(self.clip(picture_id).expect("the picture stays").clone())
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
            for partner_id in self.link_partners(m.clip_id) {
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
    /// footage does, and the pair then differs by what it lacked). It never writes
    /// an overlap check: like a trim, it leaves what the ripple pass or the user is
    /// about to settle. An edit that changed the clip's speed is not this function's
    /// (`set_speed_linked`).
    ///
    /// Errors, and the caller discards the edit, when a partner is on a locked track,
    /// would be trimmed away, or would start before 0. Returns the partners as they
    /// stand afterwards.
    pub fn carry_extent_edit(&mut self, clip_id: Uuid, was: &Clip, footage: &SourceLimits) -> Result<Vec<Clip>> {
        let now = self.clip(clip_id).ok_or(Error::ClipNotFound(clip_id))?.clone();
        let looping = footage.get(&now.asset_id).is_some_and(|l| l.is_infinite());
        let Some(edit) = extent_edit(was, &now, looping) else {
            return Ok(Vec::new());
        };
        let skip = HashSet::from([clip_id]);
        // Every partner is worked out before any is written, so a refusal changes nothing.
        let mut updates: Vec<(usize, usize, Clip)> = Vec::new();
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
                    "the linked clip on {} would be trimmed away — edit with links off",
                    self.tracks[pt].name
                )));
            }
            if p.timeline_start < -DIFF_EPS {
                return Err(Error::InvalidArgument(format!(
                    "the linked clip on {} would start before the beginning of the timeline",
                    self.tracks[pt].name
                )));
            }
            p.timeline_start = p.timeline_start.max(0.0);
            p.clamp_fades();
            updates.push((pt, pc, p));
        }
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
        if !self.tracks.iter().any(|t| t.clips.iter().any(|c| c.link_id.is_some())) {
            return Ok(());
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
            let mut scratch = self.clone();
            scratch.carry_extent_edit(*driver, &was, footage)?;
            *self = scratch;
        }
        Ok(())
    }

    // ---- split --------------------------------------------------------------

    /// Split one clip at timeline time `at` into two adjacent halves; the right half
    /// is a new clip (new id, no transition — that stays with the left — and **no
    /// link**: [`Timeline::split_clip_linked`] links the new halves of a group).
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
        if left.is_reversed() {
            let split_src = (left.source_out - offset).clamp(left.source_in, left.source_out);
            left.source_in = split_src;
            right.source_out = split_src;
        } else {
            let split_src = (left.source_in + offset).clamp(left.source_in, left.source_out);
            left.source_out = split_src;
            right.source_in = split_src;
        }
        self.tracks[ti].clips[ci] = left.clone();
        self.tracks[ti].clips.insert(ci + 1, right.clone());
        Ok((left, right))
    }

    /// **Split** `clip_id` at `at` *and* every linked partner that has `at` inside it
    /// (a partner that does not reach that moment is left whole); the new right
    /// halves are linked to each other, the left halves keep the group. A partner
    /// on a locked track that would be split refuses the whole edit. Returns the
    /// named clip's `(left, right)`.
    pub fn split_clip_linked(&mut self, clip_id: Uuid, at: f64) -> Result<(Clip, Clip)> {
        let skip = HashSet::from([clip_id]);
        let mut partners = Vec::new();
        for partner in self.link_partners(clip_id) {
            let clip = self.clip(partner).expect("a partner is on the timeline");
            if clip.timeline_start + DIFF_EPS < at && at < clip.timeline_end() - DIFF_EPS && !skip.contains(&partner) {
                partners.push(partner);
            }
        }
        for partner in &partners {
            let (ti, _) = self.locate(*partner).expect("a partner is on the timeline");
            if self.tracks[ti].locked {
                return Err(locked_partner(&self.tracks[ti]));
            }
        }
        let (left, right) = self.split_clip(clip_id, at)?;
        let mut halves = vec![right.id];
        for partner in partners {
            let (_, partner_right) = self.split_clip(partner, at)?;
            halves.push(partner_right.id);
        }
        self.relink_new_halves(&halves);
        let right = self.clip(right.id).expect("the right half is on the timeline").clone();
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

    /// [`Timeline::ripple_delete_clip`] on the clip and each of its linked
    /// partners, every track closing up behind its own — and the clips that closed up
    /// take *their* partners with them ([`Timeline::sync_pushed`]), so deleting an
    /// unlinked title keeps the shot after it in step with its sound. A partner on a
    /// locked track refuses the lot. Returns how many clips were deleted.
    pub fn ripple_delete_linked(&mut self, clip_id: Uuid) -> Result<usize> {
        self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let partners = self.unlocked_partners(clip_id, &HashSet::from([clip_id]))?;
        let before = self.clone();
        self.ripple_delete_clip(clip_id)?;
        for partner in &partners {
            self.ripple_delete_clip(*partner)?;
        }
        // The clips that closed up carry their own partners with them.
        self.sync_pushed(&before);
        Ok(1 + partners.len())
    }

    /// [`Timeline::remove_clips`] on `ids` and every clip linked to one of them, all or
    /// nothing: a partner on a locked track refuses the lot, with the error that names
    /// the link. Returns how many clips were removed (partners included).
    pub fn remove_clips_linked(&mut self, ids: &[Uuid]) -> Result<usize> {
        let named: HashSet<Uuid> = ids.iter().copied().collect();
        for id in ids {
            if self.locate(*id).is_some() {
                self.unlocked_partners(*id, &named)?;
            }
        }
        let all = self.with_link_partners(ids);
        self.remove_clips(&all)
    }

    // ---- cut a source range -------------------------------------------------

    /// Cut a **source-time** range out of a clip: the clip is split around the
    /// intersection of `[from, to]` with its source window, the middle piece
    /// removed, and later clips on the track ripple left to close the gap. Returns
    /// the kept pieces in play order. A tail piece is a new clip with no link.
    pub fn cut_clip_range(&mut self, clip_id: Uuid, from: f64, to: f64) -> Result<Vec<Clip>> {
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

        // The kept source spans in play order — a reversed clip plays the
        // upper span first. A piece that is the sole survivor keeps the
        // original id and both fades (the cut is just a trim); otherwise
        // the fades facing the removed middle are dropped.
        let (head, tail) = if clip.is_reversed() {
            ((b, clip.source_out), (clip.source_in, a))
        } else {
            ((clip.source_in, a), (b, clip.source_out))
        };
        let head_ok = head.1 - head.0 > 1e-9;
        let tail_ok = tail.1 - tail.0 > 1e-9;
        let mut pieces: Vec<Clip> = Vec::new();
        let mut cursor = clip.timeline_start;
        if head_ok {
            let mut p = clip.clone();
            (p.source_in, p.source_out) = head;
            p.timeline_start = cursor;
            if tail_ok {
                p.fade_out = 0.0;
            }
            cursor = p.timeline_end();
            pieces.push(p);
        }
        if tail_ok {
            let mut p = clip.clone();
            (p.source_in, p.source_out) = tail;
            p.timeline_start = cursor;
            if head_ok {
                p.id = Uuid::new_v4();
                p.fade_in = 0.0;
                p.transition_in = None;
                p.link_id = None;
            }
            pieces.push(p);
        }

        let track = &mut self.tracks[ti];
        track.clips.remove(ci);
        for c in &mut track.clips {
            if c.timeline_start > clip.timeline_start + 1e-9 {
                c.timeline_start = (c.timeline_start - removed).max(0.0);
            }
        }
        track.clips.extend(pieces.iter().cloned());
        track.sort_by_start();
        Ok(pieces)
    }

    /// [`Timeline::cut_clip_range`] on the clip *and* its linked partners: the
    /// stretch of **timeline** the cut removes is taken out of every partner it
    /// overlaps too (a partner of another asset, or sitting at another offset, loses
    /// the same moment, not the same source span), each closing the gap on its own
    /// track. A partner the cut misses is untouched; one on a locked track refuses
    /// the lot. The tail pieces the cut makes are linked to each other, and the clips
    /// that closed up take *their* partners with them ([`Timeline::sync_pushed`]).
    /// Returns the named clip's kept pieces.
    pub fn cut_clip_range_linked(&mut self, clip_id: Uuid, from: f64, to: f64) -> Result<Vec<Clip>> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let clip = self.tracks[ti].clips[ci].clone();
        let a = from.max(clip.source_in);
        let b = to.min(clip.source_out);
        let partners = self.unlocked_partners(clip_id, &HashSet::from([clip_id]))?;
        // Each partner's own cut, in *its* source time.
        let mut cuts: Vec<(Uuid, f64, f64)> = Vec::new();
        if b - a > 1e-9 {
            let span = clip.source_span_to_timeline(a, b);
            for partner in partners {
                let p = self.clip(partner).expect("a partner is on the timeline");
                let (lo, hi) = (span.start.max(p.timeline_start), span.end.min(p.timeline_end()));
                if hi - lo <= DIFF_EPS {
                    continue;
                }
                let (s0, s1) = (p.timeline_to_source(lo), p.timeline_to_source(hi));
                cuts.push((partner, s0.min(s1), s0.max(s1)));
            }
        }
        let mut scratch = self.clone();
        let kept = scratch.cut_clip_range(clip_id, from, to)?;
        for (partner, lo, hi) in cuts {
            scratch.cut_clip_range(partner, lo, hi)?;
        }
        // The new tail pieces (every clip of the group whose id was not there before).
        let new_pieces: Vec<Uuid> = scratch
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| self.locate(c.id).is_none())
            .map(|c| c.id)
            .collect();
        scratch.relink_new_halves(&new_pieces);
        // The clips that closed up carry their own partners with them.
        scratch.sync_pushed(self);
        let kept = kept.into_iter().map(|c| scratch.clip(c.id).cloned().unwrap_or(c)).collect();
        *self = scratch;
        Ok(kept)
    }

    // ---- speed --------------------------------------------------------------

    /// Retime `clip_id` to `speed` and every linked partner by the same *ratio*
    /// (a partner at 1× next to a clip going 1× → 2× goes to 2×; one already at 0.5×
    /// goes to 1×; a sign flip reverses it too), so a picture and its sound stay in
    /// step. Each keeps its window and start, so each track's length changes by its
    /// own — the ripple pass follows, per track. Errors when a partner's new speed
    /// would be zero or not finite, or a partner is on a locked track. Returns the
    /// named clip.
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
                return Err(Error::InvalidArgument(
                    "a linked clip would end up with no speed — set the speed with links off".to_string(),
                ));
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
        let mut lanes: HashSet<usize> = cuts.iter().filter_map(|c| self.locate(c.clip_id).map(|(ti, _)| ti)).collect();
        let mut out = cuts.to_vec();
        for cut in cuts {
            for partner in self.link_partners(cut.clip_id) {
                if named.contains(&partner) {
                    continue;
                }
                let (ti, ci) = self.locate(partner).expect("a partner is on the timeline");
                let p = &self.tracks[ti].clips[ci];
                if !(p.timeline_start + DIFF_EPS < cut.at && cut.at < p.timeline_end() - DIFF_EPS) || !lanes.insert(ti) {
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

    // ---- ripple -------------------------------------------------------------

    /// The sync lock for an op that closes its own gaps (`ripple_delete`, a cut range):
    /// `self` is what it left behind and `before` where it started, and every clip it
    /// *pushed* — shifted, same length — drags the linked partners nobody moved by the
    /// same amount, exactly as [`Timeline::ripple_from`] does after a ripple edit. A
    /// title ripple-deleted from V1 pulls the next shot *and its sound* in.
    pub fn sync_pushed(&mut self, before: &Timeline) {
        self.follow_links(before, before);
    }

    /// The **sync lock**: after the per-track ripple (`self` is its result, `edited`
    /// what the edit left before it, `before` where it started), every clip the
    /// ripple moved takes its linked partners along by the same amount — the
    /// picture's sound stays with it even when the sound's track had no edit of its
    /// own to ripple from (a title trimmed on V1 pulls the next shot *and* its sound
    /// in). Only **clips** follow, never the lane: an unlinked clip on the partner's
    /// track stays where it was.
    ///
    /// Only a clip that *the ripple* moved counts — one that started where it started
    /// and was pushed, not one the edit itself moved (a left trim's start, which the
    /// ripple puts back). A partner follows only if nothing moved it yet (same start
    /// before, after the edit and after the ripple) and its track is not locked; a group whose moved
    /// members disagree on the amount is left alone. Like the ripple itself it never
    /// produces an overlap: if the followers would leave a lane overlapping, or
    /// before 0, that lane keeps the clips where they were.
    pub(super) fn follow_links(&mut self, edited: &Timeline, before: &Timeline) {
        if !self.tracks.iter().any(|t| t.clips.iter().any(|c| c.link_id.is_some())) {
            return;
        }
        let start_in = |t: &Timeline, id: Uuid| t.clip(id).map(|c| c.timeline_start);
        let mut groups: HashMap<Uuid, Vec<(usize, usize)>> = HashMap::new();
        for (ti, track) in self.tracks.iter().enumerate() {
            for (ci, clip) in track.clips.iter().enumerate() {
                if let Some(link) = clip.link_id {
                    groups.entry(link).or_default().push((ti, ci));
                }
            }
        }
        // `(lane, clip index) -> shift` for every follower.
        let mut shifts: HashMap<(usize, usize), f64> = HashMap::new();
        for members in groups.values() {
            let moved: Vec<((usize, usize), f64)> = members
                .iter()
                .filter_map(|&(ti, ci)| {
                    let clip = &self.tracks[ti].clips[ci];
                    let was = start_in(edited, clip.id)?;
                    // A follower: it started where it started, and only the ripple moved
                    // it. A clip the *edit* moved (a left trim's start, which the ripple
                    // then puts back) is not footage that was pushed.
                    if (start_in(before, clip.id)? - was).abs() > DIFF_EPS {
                        return None;
                    }
                    let by = clip.timeline_start - was;
                    (by.abs() > DIFF_EPS).then_some(((ti, ci), by))
                })
                .collect();
            let Some(&(_, by)) = moved.first() else { continue };
            if moved.iter().any(|(_, other)| (other - by).abs() > DIFF_EPS) {
                continue;
            }
            for &(ti, ci) in members {
                let clip = &self.tracks[ti].clips[ci];
                if self.tracks[ti].locked || moved.iter().any(|(at, _)| *at == (ti, ci)) {
                    continue;
                }
                // Untouched so far: at the same start before the edit, after it and now.
                let untouched = match (start_in(before, clip.id), start_in(edited, clip.id)) {
                    (Some(b), Some(e)) => (b - e).abs() <= DIFF_EPS && (e - clip.timeline_start).abs() <= DIFF_EPS,
                    _ => false,
                };
                // A lane the moved clips share with it is that lane's own ripple's business.
                let same_lane = moved.iter().any(|((mt, _), _)| *mt == ti);
                if untouched && !same_lane {
                    shifts.insert((ti, ci), by);
                }
            }
        }
        let mut lanes: HashMap<usize, Vec<usize>> = HashMap::new();
        for &(ti, ci) in shifts.keys() {
            lanes.entry(ti).or_default().push(ci);
        }
        for (ti, followers) in lanes {
            let mut clips = self.tracks[ti].clips.clone();
            let mut pristine = vec![true; clips.len()];
            for ci in followers {
                clips[ci].timeline_start += shifts[&(ti, ci)];
                pristine[ci] = false;
            }
            if lane_is_legal(&clips, &pristine) {
                let mut track = Track {
                    clips,
                    ..self.tracks[ti].clone()
                };
                track.sort_by_start();
                self.tracks[ti] = track;
            }
        }
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
        // Before zero is refused.
        let (mut t, c, a, asset) = pair();
        t.clip_mut(c).unwrap().timeline_start = 3.0;
        t.clip_mut(a).unwrap().timeline_start = 3.0;
        let was = get(&t, c).clone();
        t.clip_mut(a).unwrap().timeline_start = 1.0;
        t.clip_mut(c).unwrap().timeline_start = 0.0;
        assert!(t.carry_extent_edit(c, &was, &limits(&[asset])).is_err());
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
    fn a_group_whose_moved_members_disagree_is_left_alone() {
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
        assert_eq!((start(&out, c2), start(&out, a2)), (4.0, 4.0));
        assert_eq!(start(&out, b2), 5.0, "the third member waits: no single amount to follow");
    }

    #[test]
    fn the_start_a_left_trim_gives_back_is_not_a_ripple_the_partner_follows() {
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
        assert_eq!(
            extent(&out, a1),
            (5.0, 10.0),
            "putting c1's start back is not footage being pushed: the sound stays"
        );
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
}
