//! Ripple and the edits of several clips at once.

use super::*;

// ---- ripple ----------------------------------------------------------------

impl Timeline {
    /// The cut after a **ripple edit**: `self` is what an edit left behind and
    /// `before` is what it started from. Clips are matched by id, the way
    /// [`Timeline::diff`] matches them, and each track is looked at on its own.
    ///
    /// The one idea is that an edit which changes how much footage sits ahead of
    /// a clip should carry that clip along, with every gap in front of it kept.
    /// The track's *followers* — clips the edit left starting where they started
    /// (same id, same start; a clip trimmed in place follows what was done ahead
    /// of it too) — are shifted by the net change in length of what the edit did
    /// ahead of them:
    ///
    /// | the edit… | followers at or after… | move by |
    /// |---|---|---|
    /// | changes a clip's length (right trim, speed, a left trim) | its old end | the change in its length |
    /// | removes a clip from the timeline | its old end | minus its length |
    /// | adds a clip onto footage that was there | the new clip's start | the new clip's length |
    ///
    /// Everything else is deliberately left alone:
    ///
    /// * **A left-edge trim keeps the clip's start.** A left trim is committed as
    ///   a new `source_in` *and* a later `timeline_start`, so that the right edge
    ///   stays put (the non-ripple behaviour). In a ripple edit the conventional
    ///   thing happens instead: the clip stays where it started, its length
    ///   changes, and the followers move by that change — so the result is the
    ///   same whether or not the caller moved `timeline_start`. This only applies
    ///   when the trim is the *whole* edit on the track; a bulk edit that moves
    ///   several clips has no single edit point to hold still.
    /// * **An add that fits is not an insert.** A clip put onto free space — the
    ///   end of the track, a gap it fits in — moves nothing. Only a clip that
    ///   lands on footage that was there pushes it: that footage and everything
    ///   after it moves right by the new clip's length (adjacent adds count as
    ///   one). A clip that lands in the *middle* of another cannot be resolved
    ///   without splitting it, which ripple never does, so that track is left as
    ///   the edit made it.
    /// * **Splits do not shift.** The cut-off half is an add over the footage the
    ///   shortened half just gave up, and the two cancel exactly.
    /// * **Moves do not ripple**, within a track or across tracks: a clip
    ///   arriving from or leaving for another track, or merely changing its
    ///   start, is not a change in how much footage sits ahead of anything.
    /// * **An edit that already rippled is not rippled twice.** A clip the edit
    ///   itself moved is not a follower, so `ripple_delete` and `cut_clip_range`
    ///   (whose later clips are all moved) come back unchanged.
    /// * **Tracks are independent — except for linked clips.** A ripple on `V1`
    ///   does not move the rest of `A1`, but a clip it moved takes its *linked
    ///   partners* along by the same amount (the sync lock, see
    ///   `Timeline::conform_links`); a **locked** track never moves (an edit to it
    ///   is left as made). **Overlays and markers do not move** either; they are
    ///   timeline-level, not track-level, and rippling titles can come later.
    ///
    /// **It never produces an overlap.** If shifting the followers (or restoring
    /// a left-trimmed start) would leave a clip the edit or the ripple touched
    /// overlapping another, or starting before 0, that track is returned exactly
    /// as the edit made it — no ripple for that track, rather than a broken lane.
    /// Overlaps between two clips nothing touched (an old project) are not this
    /// function's business and block nothing.
    ///
    /// Pure, and the identity when nothing about any track's timing changed.
    pub fn ripple_from(&self, before: &Timeline) -> Timeline {
        self.ripple_from_with(before, true)
    }

    /// [`Timeline::ripple_from`] with the sync lock for linked material made
    /// explicit: with `links` on, the clips the ripple moved take their linked
    /// partners on other tracks along ([`Timeline::conform_links`], best effort — a
    /// conform that would be refused leaves the plain per-track ripple, and the
    /// edit's own sync guard says why) so the picture's sound stays with it even
    /// when the sound's own track had nothing to ripple. With `links` off, tracks
    /// are independent.
    pub fn ripple_from_with(&self, before: &Timeline, links: bool) -> Timeline {
        self.ripple_from_anchored(before, links, &HashSet::new())
    }

    /// [`Timeline::ripple_from_with`] told which clips the edit **named**
    /// (`anchors`), so the sync lock knows whose track speaks for a linked group
    /// when the partners' tracks were rippled by different amounts.
    pub fn ripple_from_anchored(&self, before: &Timeline, links: bool, anchors: &HashSet<Uuid>) -> Timeline {
        let out = self.ripple_lanes(before);
        if links {
            let mut conformed = out.clone();
            if conformed
                .conform_links_noted(before, anchors, &HashMap::new(), Some(self), &mut Vec::new())
                .is_ok()
            {
                return conformed;
            }
        }
        out
    }

    /// The per-track half of [`Timeline::ripple_from`]: every unlocked track
    /// rippled on its own, nothing carried between them.
    pub fn ripple_lanes(&self, before: &Timeline) -> Timeline {
        let in_before: HashSet<Uuid> = before.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
        let in_after: HashSet<Uuid> = self.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).collect();
        let mut out = self.clone();
        for track in &mut out.tracks {
            let Some(prior) = before.track(track.id) else {
                continue;
            };
            if track.locked || prior.locked {
                continue;
            }
            if let Some(rippled) = ripple_track(track, prior, &in_before, &in_after) {
                *track = rippled;
            }
        }
        out
    }
}

/// The rippled version of one track, or `None` when there is nothing to do or
/// the ripple would leave the lane illegal. `in_before` / `in_after` hold every
/// clip id on any track of each timeline, which is what tells a clip that left
/// (or arrived) from one that was deleted (or created).
fn ripple_track(after: &Track, before: &Track, in_before: &HashSet<Uuid>, in_after: &HashSet<Uuid>) -> Option<Track> {
    let prior: HashMap<Uuid, &Clip> = before.clips.iter().map(|c| (c.id, c)).collect();
    let here: HashSet<Uuid> = after.clips.iter().map(|c| c.id).collect();

    // Clips the edit left starting where they started: they follow whatever the
    // edit did ahead of them, whether or not their own length changed too.
    // `(index in `after`, where it stood, whether it is untouched)`.
    let mut anchored: Vec<(usize, f64, bool)> = Vec::new();
    let mut resized: Vec<(usize, &Clip)> = Vec::new(); // same id, new length (and the old clip)
    let mut added: Vec<usize> = Vec::new();
    let mut other_edits = 0usize; // moves, arrivals, departures

    for (i, clip) in after.clips.iter().enumerate() {
        match prior.get(&clip.id) {
            Some(was) => {
                let same_start = !num_changed(was.timeline_start, clip.timeline_start);
                let same_length = !num_changed(was.duration(), clip.duration());
                if same_start {
                    anchored.push((i, was.timeline_start, same_length));
                }
                match (same_start, same_length) {
                    (_, false) => resized.push((i, was)),
                    (false, true) => other_edits += 1,
                    (true, true) => {}
                }
            }
            None if !in_before.contains(&clip.id) => added.push(i),
            None => other_edits += 1,
        }
    }
    let removed: Vec<&Clip> = before.clips.iter().filter(|c| !in_after.contains(&c.id)).collect();
    other_edits += before
        .clips
        .iter()
        .filter(|c| in_after.contains(&c.id) && !here.contains(&c.id))
        .count();

    // What the edit did to the amount of footage ahead of the clips after it:
    // `(where, by how much)`, in before-timeline terms.
    let mut events: Vec<(f64, f64)> = Vec::new();
    for (i, was) in &resized {
        events.push((was.timeline_end(), after.clips[*i].duration() - was.duration()));
    }
    for clip in &removed {
        events.push((clip.timeline_end(), -clip.duration()));
    }
    // Adjacent adds are one insertion; it only counts when it landed on footage
    // that was there (a clip that fits in free space moves nothing).
    let mut spans: Vec<(f64, f64)> = added
        .iter()
        .map(|&i| (after.clips[i].timeline_start, after.clips[i].timeline_end()))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut chains: Vec<(f64, f64)> = Vec::new();
    for (start, end) in spans {
        match chains.last_mut() {
            Some(last) if start <= last.1 + DIFF_EPS => last.1 = last.1.max(end),
            _ => chains.push((start, end)),
        }
    }
    for (lo, hi) in chains {
        if before
            .clips
            .iter()
            .any(|c| spans_overlap((lo, hi), (c.timeline_start, c.timeline_end())))
        {
            events.push((lo, hi - lo));
        }
    }

    let mut clips = after.clips.clone();
    // Untouched clips are pristine until shifted; everything else the edit
    // touched is not.
    let mut pristine = vec![false; clips.len()];
    let mut changed = false;

    // A left-edge trim that held the right edge still: the clip keeps its start.
    // Only when it is the whole edit — see `ripple_from`.
    if let [(i, was)] = resized.as_slice() {
        let sole = removed.is_empty() && added.is_empty() && other_edits == 0;
        if sole && !num_changed(was.timeline_end(), clips[*i].timeline_end()) {
            clips[*i].timeline_start = was.timeline_start;
            changed = true;
        }
    }

    for (i, stood, untouched) in anchored {
        pristine[i] = untouched;
        let shift: f64 = events
            .iter()
            .filter(|(at, _)| *at <= stood + DIFF_EPS)
            .map(|(_, by)| by)
            .sum();
        if shift.abs() > DIFF_EPS {
            clips[i].timeline_start += shift;
            pristine[i] = false;
            changed = true;
        }
    }
    if !changed || !lane_is_legal(&clips, &pristine) {
        return None;
    }
    let mut track = Track { clips, ..after.clone() };
    track.sort_by_start();
    Some(track)
}

/// Whether a lane is fit to keep after a ripple: nothing the edit or the ripple
/// touched starts before 0 or overlaps another clip. Two `pristine` clips
/// overlapping is old news and does not count.
fn lane_is_legal(clips: &[Clip], pristine: &[bool]) -> bool {
    if clips
        .iter()
        .zip(pristine)
        .any(|(c, pristine)| !pristine && c.timeline_start < -DIFF_EPS)
    {
        return false;
    }
    let mut order: Vec<usize> = (0..clips.len()).collect();
    order.sort_by(|&a, &b| clips[a].timeline_start.total_cmp(&clips[b].timeline_start));
    for (pos, &i) in order.iter().enumerate() {
        let end = clips[i].timeline_end();
        for &j in &order[pos + 1..] {
            if clips[j].timeline_start >= end - DIFF_EPS {
                break;
            }
            if !pristine[i] || !pristine[j] {
                return false;
            }
        }
    }
    true
}

/// Do two spans share any time? Touching end-to-start is not an overlap, and a
/// microsecond of float noise from a JSON round-trip does not count either.
pub(super) fn spans_overlap(a: (f64, f64), b: (f64, f64)) -> bool {
    a.0 < b.1 - DIFF_EPS && b.0 < a.1 - DIFF_EPS
}

// ---- multi-clip edits --------------------------------------------------------

/// One clip's destination in a group move ([`Timeline::move_clips`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipMove {
    pub clip_id: Uuid,
    /// Where the clip starts afterwards (seconds) — absolute, not a delta, so a
    /// caller that snaps to frames or beats decides the exact landing spot.
    pub timeline_start: f64,
    /// The track it lands on, which must be the same kind as the one it leaves.
    /// Omitted, the clip stays on its track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<Uuid>,
}

impl Timeline {
    /// Move several clips at once, **all or nothing**: every destination is
    /// checked before anything moves, so an error leaves the timeline as it was.
    ///
    /// The group is checked as a group. Clips moving together never collide with
    /// the places they are leaving (nudging abutting clips by a second is legal,
    /// where moving them one at a time would trip over each other), but they must
    /// not overlap each other or any clip that is *not* moving, on whichever
    /// track they land. A clip may change track only to another of the same
    /// kind, a locked track — as source or destination — refuses the whole move,
    /// and a clip may appear once. A start before 0 is an error rather than a
    /// clamp: clamping each clip separately would quietly reshape the group, and
    /// the caller knows how far the group may slide.
    ///
    /// Returns the moved clips in request order. Moves never ripple.
    pub fn move_clips(&mut self, moves: &[ClipMove]) -> Result<Vec<Clip>> {
        if moves.is_empty() {
            return Err(Error::InvalidArgument("no clips to move".to_string()));
        }
        // (clip, destination track index, start, duration) — resolved up front.
        let mut plan: Vec<(Uuid, usize, f64, f64)> = Vec::with_capacity(moves.len());
        let mut moving: HashSet<Uuid> = HashSet::with_capacity(moves.len());
        for m in moves {
            if !moving.insert(m.clip_id) {
                return Err(Error::InvalidArgument(format!("clip {} appears more than once", m.clip_id)));
            }
            if !m.timeline_start.is_finite() {
                return Err(Error::InvalidArgument("timeline_start must be a finite number".to_string()));
            }
            if m.timeline_start < -DIFF_EPS {
                return Err(Error::InvalidArgument(format!(
                    "clip {} would start before the beginning of the timeline",
                    m.clip_id
                )));
            }
            let (from, ci) = self.locate(m.clip_id).ok_or(Error::ClipNotFound(m.clip_id))?;
            let to = match m.track_id {
                Some(id) => self.tracks.iter().position(|t| t.id == id).ok_or(Error::TrackNotFound(id))?,
                None => from,
            };
            if self.tracks[to].kind != self.tracks[from].kind {
                return Err(Error::InvalidArgument(
                    "cannot move a clip to a track of a different kind".to_string(),
                ));
            }
            for ti in [from, to] {
                if self.tracks[ti].locked {
                    return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
                }
            }
            plan.push((
                m.clip_id,
                to,
                m.timeline_start.max(0.0),
                self.tracks[from].clips[ci].duration(),
            ));
        }

        // Where everything lands: the moved clips against the ones staying put,
        // and against each other.
        for (i, (_, to, start, duration)) in plan.iter().enumerate() {
            let span = (*start, *start + *duration);
            let track = &self.tracks[*to];
            let hits_staying = track
                .clips
                .iter()
                .any(|c| !moving.contains(&c.id) && spans_overlap(span, (c.timeline_start, c.timeline_end())));
            let hits_moving = plan
                .iter()
                .enumerate()
                .any(|(j, (_, other_to, other_start, other_duration))| {
                    j != i && other_to == to && spans_overlap(span, (*other_start, *other_start + *other_duration))
                });
            if hits_staying || hits_moving {
                return Err(Error::InvalidArgument(format!(
                    "moved clips would overlap on track {} at {}",
                    track.name,
                    fmt_time(*start)
                )));
            }
        }

        // Everything checks out: lift the clips, set them down, re-order the lanes.
        let mut lifted: HashMap<Uuid, Clip> = HashMap::with_capacity(plan.len());
        for track in &mut self.tracks {
            let (take, keep): (Vec<Clip>, Vec<Clip>) = std::mem::take(&mut track.clips)
                .into_iter()
                .partition(|c| moving.contains(&c.id));
            track.clips = keep;
            lifted.extend(take.into_iter().map(|c| (c.id, c)));
        }
        let mut landed = Vec::with_capacity(plan.len());
        for (id, to, start, _) in &plan {
            let mut clip = lifted.remove(id).expect("every planned clip was located above");
            clip.timeline_start = *start;
            landed.push(clip.clone());
            self.tracks[*to].clips.push(clip);
        }
        for (_, to, _, _) in &plan {
            self.tracks[*to].sort_by_start();
        }
        Ok(landed)
    }

    /// Remove several clips at once, all or nothing: an unknown id, or a clip on
    /// a locked track, refuses the lot. A clip named twice is removed once.
    /// Leaves gaps — under ripple mode the caller's [`Timeline::ripple_from`]
    /// closes them, per track. Returns how many clips were removed.
    pub fn remove_clips(&mut self, ids: &[Uuid]) -> Result<usize> {
        if ids.is_empty() {
            return Err(Error::InvalidArgument("no clips to remove".to_string()));
        }
        for id in ids {
            let (ti, _) = self.locate(*id).ok_or(Error::ClipNotFound(*id))?;
            if self.tracks[ti].locked {
                return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
            }
        }
        let doomed: HashSet<Uuid> = ids.iter().copied().collect();
        for track in &mut self.tracks {
            track.clips.retain(|c| !doomed.contains(&c.id));
        }
        Ok(doomed.len())
    }
}
