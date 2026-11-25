//! The roll, slip, slide and split-and-remove edits of a timeline.

use super::*;

// ---- edit modes: roll, slip, slide, split-and-remove -------------------------

/// Shortest a clip an edit-mode op may leave behind (seconds) — the same floor
/// the timeline's edge-trim handles hold.
pub const MIN_EDIT_CLIP: f64 = 0.05;

/// Two clip edges closer than this are one edge. It is the engine's own test for
/// a transition partner (`transition_fx`), so a cut that blends is a cut that
/// can be rolled, and a clip that "touches" its neighbour here is one the render
/// treats as touching.
pub const ADJACENT_EPS: f64 = 1e-3;

/// How far each asset's footage reaches, for the edit modes that move a clip's
/// source window over it: [`Asset::source_limit`] per asset id (infinite for a
/// still).
pub type SourceLimits = HashMap<Uuid, f64>;

// The doc comments on this type are what an MCP client reads in the tool schema,
// so they are written for a model, not as rustdoc links.
/// Which half of a split to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SplitSide {
    /// Remove everything before the split point (trims the clip's start to it).
    Left,
    /// Remove everything after the split point (trims the clip's end to it).
    Right,
}

impl SplitSide {
    pub fn as_str(self) -> &'static str {
        match self {
            SplitSide::Left => "left",
            SplitSide::Right => "right",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" => Some(SplitSide::Left),
            "right" => Some(SplitSide::Right),
            _ => None,
        }
    }
}

/// What a roll, slip or slide did: how far it was asked to go, how far it
/// actually went (it clamps to the footage and the neighbours rather than
/// refusing), and the clips it changed, as they stand afterwards, in timeline
/// order.
#[derive(Debug, Clone, Serialize)]
pub struct EditOutcome {
    pub requested: f64,
    pub applied: f64,
    /// `applied` is not what was asked for.
    pub clamped: bool,
    pub clips: Vec<Clip>,
}

impl EditOutcome {
    pub(super) fn new(requested: f64, applied: f64, clips: Vec<Clip>) -> Self {
        Self {
            requested,
            applied,
            clamped: num_changed(requested, applied),
            clips,
        }
    }
}

/// How far an edit-mode op may go each way, in the units of its `delta`, and
/// what stops it. The ops clamp to it, and a UI that drags one reads it to hold
/// the pointer where the edit would go no further.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaRange {
    /// Furthest it may go in the negative direction (≤ 0).
    pub min: f64,
    /// Furthest it may go in the positive direction (≥ 0).
    pub max: f64,
    pub(super) why_min: String,
    pub(super) why_max: String,
}

impl DeltaRange {
    fn open() -> Self {
        Self {
            min: f64::NEG_INFINITY,
            max: f64::INFINITY,
            why_min: String::new(),
            why_max: String::new(),
        }
    }

    /// Cap the positive direction at `room` seconds (a negative room is none).
    fn up_to(mut self, room: f64, why: impl Into<String>) -> Self {
        let room = room.max(0.0);
        if room < self.max {
            self.max = room;
            self.why_max = why.into();
        }
        self
    }

    /// Cap the negative direction at `room` seconds (a magnitude).
    fn down_to(mut self, room: f64, why: impl Into<String>) -> Self {
        let room = -room.max(0.0);
        if room > self.min {
            self.min = room;
            self.why_min = why.into();
        }
        self
    }

    /// `delta` held inside the range. When that leaves nothing to do it is an
    /// error that names what stopped it, so an edit that would change nothing
    /// never records a revision.
    pub(super) fn resolve(&self, delta: f64, verb: &str) -> Result<f64> {
        let applied = delta.clamp(self.min, self.max);
        if applied.abs() > DIFF_EPS {
            return Ok(applied);
        }
        let (way, why) = if delta > 0.0 {
            ("later", &self.why_max)
        } else {
            ("earlier", &self.why_min)
        };
        Err(Error::InvalidArgument(format!("cannot {verb} {way}: {why}")))
    }
}

/// A `delta` is a finite, non-zero number of seconds.
pub(super) fn check_delta(delta: f64) -> Result<()> {
    if !delta.is_finite() {
        return Err(Error::InvalidArgument("delta must be a finite number of seconds".to_string()));
    }
    if delta.abs() <= DIFF_EPS {
        return Err(Error::InvalidArgument("delta is zero — there is nothing to move".to_string()));
    }
    Ok(())
}

pub(super) fn footage_of(footage: &SourceLimits, clip: &Clip) -> Result<f64> {
    footage
        .get(&clip.asset_id)
        .copied()
        .ok_or(Error::AssetNotFound(clip.asset_id))
}

/// One clip's cut in a group split-and-remove ([`Timeline::split_remove_clips`]):
/// the clip and the timeline time it is cut at.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipCut {
    pub clip_id: Uuid,
    /// Timeline seconds, inside the clip.
    pub at: f64,
}

/// A slide's neighbour on one side: its index in the lane and whether it touches
/// the clip.
type Neighbour = Option<(usize, bool)>;

/// Close a cut exactly. A roll or a slide computes one side of a cut from a source
/// window (`start + (out - in) / speed`) and the other from `start + delta`, and the
/// two disagree by a few ulps (±7e-15 s in about one case in thirteen): invisible to
/// a render, but a strict overlap test — `Project::move_clip`'s — reads it as one
/// clip lying on the next. So when `follower` starts where `leader` ends to within
/// float noise ([`DIFF_EPS`]), it is set to start *exactly* at `leader.timeline_end()`,
/// the very expression the overlap checks compare against. A genuine gap or overlap
/// (a cut that was merely within [`ADJACENT_EPS`]) is real data and is left alone.
fn weld(leader: &Clip, follower: &mut Clip) {
    let end = leader.timeline_end();
    if (follower.timeline_start - end).abs() <= DIFF_EPS {
        follower.timeline_start = end;
    }
}

/// [`weld`]'s mirror, for the edge the edit left where it was: `clip`'s far end
/// meets a clip that did not move (it starts at `limit`), and the window arithmetic
/// can leave `clip.timeline_end()` a few ulps *past* it (observed up to ~6e-14 s).
/// That overshoot, if it is float noise ([`DIFF_EPS`]), is taken back by shortening the
/// window point the end is written on by what it overshoots — and by a single ulp when
/// that is too small to move the point — until the end is no longer past `limit`: a
/// change of ~1e-14 s to the source window, on a clip whose neighbour cannot be moved
/// to meet it. `looping` as in [`Clip::move_tail`].
fn fit_end(clip: &mut Clip, limit: f64, looping: bool) {
    for _ in 0..16 {
        let over = clip.timeline_end() - limit;
        if over <= 0.0 || over > DIFF_EPS {
            return;
        }
        let by = over * clip.speed_at(clip.duration());
        if clip.is_reversed() && !looping {
            // The end of a reversed clip is its in-point: raise it to shorten.
            let moved = clip.source_in + by;
            clip.source_in = if moved == clip.source_in {
                clip.source_in.next_up()
            } else {
                moved
            };
        } else {
            let moved = clip.source_out - by;
            clip.source_out = if moved == clip.source_out {
                clip.source_out.next_down()
            } else {
                moved
            };
        }
    }
}

impl Timeline {
    /// Find a clip for an edit: it must exist and its track must not be locked.
    fn editable_clip(&self, clip_id: Uuid) -> Result<(usize, usize)> {
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        if self.tracks[ti].locked {
            return Err(Error::InvalidArgument(format!("track {} is locked", self.tracks[ti].name)));
        }
        Ok((ti, ci))
    }

    /// Refuse a layout edit to a clip on a locked track (or one that is not there). A lock guards
    /// what is on a track, where, how long and whether it plays; a clip's look and sound stay
    /// editable, as the Inspector leaves them.
    pub fn ensure_editable(&self, clip_id: Uuid) -> Result<()> {
        self.editable_clip(clip_id).map(drop)
    }

    /// Refuse an edit that puts a clip on (or reorders) a locked track.
    pub fn ensure_track_editable(&self, track_id: Uuid) -> Result<()> {
        let track = self.track(track_id).ok_or(Error::TrackNotFound(track_id))?;
        if track.locked {
            return Err(Error::InvalidArgument(format!("track {} is locked", track.name)));
        }
        Ok(())
    }

    /// Where the first clip of lane `ti` that starts at or after `end` (less the
    /// adjacency tolerance) starts, skipping the lane indices in `skip` — the clip an
    /// edit's far edge runs into, which the edit did not move.
    fn start_after(&self, ti: usize, end: f64, skip: &[usize]) -> Option<f64> {
        self.tracks[ti]
            .clips
            .iter()
            .enumerate()
            .filter(|(i, c)| !skip.contains(i) && c.timeline_start >= end - ADJACENT_EPS)
            .map(|(_, c)| c.timeline_start)
            .min_by(f64::total_cmp)
    }

    // ---- roll ---------------------------------------------------------------

    /// Everything a roll needs, checked: the lane, the two clips' indices in it,
    /// and how far the cut may move.
    pub(super) fn roll_plan(
        &self,
        clip_a: Uuid,
        clip_b: Uuid,
        footage: &SourceLimits,
    ) -> Result<(usize, usize, usize, DeltaRange)> {
        if clip_a == clip_b {
            return Err(Error::InvalidArgument("a cut lies between two different clips".to_string()));
        }
        let (ta, ia) = self.editable_clip(clip_a)?;
        let (tb, ib) = self.editable_clip(clip_b)?;
        if ta != tb {
            return Err(Error::InvalidArgument(format!(
                "clips are on different tracks ({} and {}) — a roll moves the cut between two clips of one track",
                self.tracks[ta].name, self.tracks[tb].name
            )));
        }
        let (a, b) = (&self.tracks[ta].clips[ia], &self.tracks[ta].clips[ib]);
        let gap = b.timeline_start - a.timeline_end();
        if gap.abs() >= ADJACENT_EPS {
            return Err(Error::InvalidArgument(
                if (a.timeline_start - b.timeline_end()).abs() < ADJACENT_EPS {
                    "clip_a must be the earlier clip: a roll moves the cut where clip_a ends and clip_b begins".to_string()
                } else if gap > 0.0 {
                    format!("the clips are not adjacent — there is a {gap:.2}s gap between them, and a roll needs a shared cut")
                } else {
                    format!(
                        "the clips are not adjacent — they overlap by {:.2}s, and a roll needs a shared cut",
                        -gap
                    )
                },
            ));
        }
        let (_, tail_a) = a.handles(footage_of(footage, a)?);
        let (head_b, _) = b.handles(footage_of(footage, b)?);
        let range = DeltaRange::open()
            .up_to(tail_a, "the outgoing clip has no footage left to extend into")
            .up_to(b.duration() - MIN_EDIT_CLIP, "the incoming clip would be shorter than 0.05s")
            .down_to(a.duration() - MIN_EDIT_CLIP, "the outgoing clip would be shorter than 0.05s")
            .down_to(head_b, "the incoming clip has no footage left to extend into");
        Ok((ta, ia, ib, range))
    }

    /// How far the cut between `clip_a` and `clip_b` may move each way — the
    /// bounds [`Timeline::roll_edit`] clamps to.
    pub fn roll_range(&self, clip_a: Uuid, clip_b: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        self.roll_plan(clip_a, clip_b, footage).map(|plan| plan.3)
    }

    /// **Roll** the cut between two adjacent clips: `clip_a`'s end and `clip_b`'s
    /// start both move by `delta` seconds (positive is later), so the pair covers
    /// the same stretch of timeline and nothing after it moves. The footage
    /// changes hands at the cut — one clip gains what the other gives up.
    ///
    /// `clip_a` must be the earlier clip and the two must touch (within
    /// [`ADJACENT_EPS`]) on one unlocked track. The roll **clamps** rather than
    /// refuses — to the footage each clip has left to extend into (honoring speed
    /// and direction: a reversed clip's outgoing edge is its in-point, and a still
    /// has footage without limit) and to [`MIN_EDIT_CLIP`] for the clip that
    /// shrinks — and errors only when the clamp leaves nothing to move.
    ///
    /// Everything else about both clips is kept. `clip_b`'s head moves, so its
    /// keyframes shift with the content (`Clip::rebase_animation`); `clip_a`'s
    /// head does not, so its animation stays. Fades are held inside what is left
    /// of a clip that shrank. A transition into `clip_b` stays with the cut it
    /// blends. All or nothing: an error leaves the timeline untouched.
    pub fn roll_edit(&mut self, clip_a: Uuid, clip_b: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let (ti, ia, ib, range) = self.roll_plan(clip_a, clip_b, footage)?;
        let applied = range.resolve(delta, "roll the cut")?;
        let (mut a, mut b) = (self.tracks[ti].clips[ia].clone(), self.tracks[ti].clips[ib].clone());
        let (looping_a, looping_b) = (footage_of(footage, &a)?.is_infinite(), footage_of(footage, &b)?.is_infinite());
        let far = b.timeline_end();
        a.move_tail(applied, looping_a);
        b.move_head(applied, looping_b);
        weld(&a, &mut b);
        if let Some(limit) = self.start_after(ti, far, &[ia, ib]) {
            fit_end(&mut b, limit, looping_b);
        }
        a.clamp_fades();
        b.clamp_fades();
        self.tracks[ti].clips[ia] = a.clone();
        self.tracks[ti].clips[ib] = b.clone();
        Ok(EditOutcome::new(delta, applied, vec![a, b]))
    }

    // ---- slip ---------------------------------------------------------------

    fn slip_plan(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<(usize, usize, DeltaRange)> {
        let (ti, ci) = self.editable_clip(clip_id)?;
        let clip = &self.tracks[ti].clips[ci];
        let limit = footage_of(footage, clip)?;
        if limit.is_infinite() {
            return Err(Error::InvalidArgument(
                "a still image has no footage to slip — it looks the same at every moment".to_string(),
            ));
        }
        // Source seconds the window could move earlier / later.
        let earlier = clip.source_in.max(0.0);
        let later = (limit - clip.source_out).max(0.0);
        let (no_later, no_earlier) = (
            "there is no footage left after the clip's out-point",
            "there is no footage left before the clip's in-point",
        );
        // Positive `delta` is "starts later in its own footage", which is the
        // *lower* source time for a reversed clip: the two directions swap.
        let range = if clip.is_reversed() {
            DeltaRange::open().up_to(earlier, no_earlier).down_to(later, no_later)
        } else {
            DeltaRange::open().up_to(later, no_later).down_to(earlier, no_earlier)
        };
        Ok((ti, ci, range))
    }

    /// How far `clip_id`'s footage may slip each way, in source seconds — the
    /// bounds [`Timeline::slip_clip`] clamps to. Errors for a still, which has
    /// nothing to slip.
    pub fn slip_range(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        self.slip_plan(clip_id, footage).map(|plan| plan.2)
    }

    /// **Slip** a clip: change which part of its footage it shows without moving
    /// it or changing its length. The source window (`source_in` / `source_out`)
    /// shifts by `delta` **source** seconds — not timeline seconds, so at 2× speed
    /// a 1 s slip moves the picture half a second — and nothing on the timeline
    /// moves.
    ///
    /// Positive `delta` means the clip starts **later in its own footage**: you
    /// see material that comes later. For a clip playing forward that moves the
    /// window up (`source_in` and `source_out` both grow). A reversed clip plays
    /// its window backwards, so later in its footage is *lower* source time: the
    /// window moves the mirrored way (both shrink). The sign therefore always
    /// means the same thing on screen, whichever way the clip plays.
    ///
    /// Clamps to the asset's footage (the window cannot leave `0..duration`) and
    /// errors when that leaves nothing to move. A still has no footage to slip and
    /// is an error. The keyframes (transform and reframe) are clip-local and the
    /// timing is unchanged, so they stay exactly as they are; so do the fades.
    pub fn slip_clip(&mut self, clip_id: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let (ti, ci, range) = self.slip_plan(clip_id, footage)?;
        let applied = range.resolve(delta, "slip the footage")?;
        let clip = &mut self.tracks[ti].clips[ci];
        let shift = if clip.is_reversed() { -applied } else { applied };
        clip.source_in += shift;
        clip.source_out += shift;
        Ok(EditOutcome::new(delta, applied, vec![clip.clone()]))
    }

    // ---- slide --------------------------------------------------------------

    /// The clip's place in its lane — `(track, clip, previous, next)`, the
    /// neighbours by start time and whether each touches the clip — and how far
    /// it may slide.
    fn slide_plan(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<(usize, usize, Neighbour, Neighbour, DeltaRange)> {
        let (ti, ci) = self.editable_clip(clip_id)?;
        let clips = &self.tracks[ti].clips;
        let clip = &clips[ci];
        let mut order: Vec<usize> = (0..clips.len()).collect();
        order.sort_by(|&a, &b| clips[a].timeline_start.total_cmp(&clips[b].timeline_start));
        let pos = order.iter().position(|&i| i == ci).expect("the clip is in its own lane");
        let prev = pos
            .checked_sub(1)
            .map(|p| order[p])
            .map(|i| (i, (clip.timeline_start - clips[i].timeline_end()).abs() < ADJACENT_EPS));
        let next = order
            .get(pos + 1)
            .map(|&i| (i, (clips[i].timeline_start - clip.timeline_end()).abs() < ADJACENT_EPS));

        let mut range = DeltaRange::open();
        // Later: a touching previous clip grows to fill what the clip leaves, a
        // touching next clip is pushed back; a next clip across a gap is left alone.
        if let Some((i, true)) = prev {
            let (_, tail) = clips[i].handles(footage_of(footage, &clips[i])?);
            range = range.up_to(tail, "the previous clip has no footage left to extend into");
        }
        match next {
            Some((i, true)) => {
                range = range.up_to(
                    clips[i].duration() - MIN_EDIT_CLIP,
                    "the next clip would be shorter than 0.05s",
                );
            }
            Some((i, false)) => {
                range = range.up_to(
                    clips[i].timeline_start - clip.timeline_end(),
                    "the next clip is not touching this one, so it is left alone and the clip can only use the free space before it",
                );
            }
            None => {}
        }
        // Earlier: the mirror image.
        match prev {
            Some((i, true)) => {
                range = range.down_to(
                    clips[i].duration() - MIN_EDIT_CLIP,
                    "the previous clip would be shorter than 0.05s",
                );
            }
            Some((i, false)) => {
                range = range.down_to(
                    clip.timeline_start - clips[i].timeline_end(),
                    "the previous clip is not touching this one, so it is left alone and the clip can only use the free space after it",
                );
            }
            None => {
                range = range.down_to(clip.timeline_start, "the clip is already at the start of the timeline");
            }
        }
        if let Some((i, true)) = next {
            let (head, _) = clips[i].handles(footage_of(footage, &clips[i])?);
            range = range.down_to(head, "the next clip has no footage left to extend into");
        }
        Ok((ti, ci, prev, next, range))
    }

    /// How far `clip_id` may slide each way, in timeline seconds — the bounds
    /// [`Timeline::slide_clip`] clamps to.
    pub fn slide_range(&self, clip_id: Uuid, footage: &SourceLimits) -> Result<DeltaRange> {
        self.slide_plan(clip_id, footage).map(|plan| plan.4)
    }

    /// **Slide** a clip along its track, keeping its content: it moves by `delta`
    /// timeline seconds (positive is later) and the neighbours give way — the
    /// previous clip's end and the next clip's start both move by `delta`, so the
    /// clip's own source window, its length and the length of the stretch it and
    /// its neighbours span are unchanged.
    ///
    /// The exact rules, for a lane with gaps:
    ///
    /// * A neighbour that **touches** the clip (within [`ADJACENT_EPS`]) follows
    ///   its edge: sliding later extends the previous clip and trims the next;
    ///   sliding earlier does the reverse. It is bounded by that neighbour's
    ///   footage (`Clip::handles`, speed and direction honored, a still
    ///   unbounded) when it grows and by [`MIN_EDIT_CLIP`] when it shrinks.
    /// * A neighbour **across a gap** is never touched — the slide cannot trim a
    ///   clip it is not touching. The clip moves through the free space and stops
    ///   where it would meet that neighbour.
    /// * With no previous clip the clip cannot go before 0; with no next clip it
    ///   can slide later without limit — and then the track's end moves with it,
    ///   the one case where the overall length changes, since there is no next
    ///   clip to give way.
    ///
    /// Like the other modes it **clamps** and errors only when nothing can move.
    /// The slid clip is only repositioned, so its keyframes and fades stay; a next
    /// clip whose start moved has its keyframes shift with its content
    /// (`Clip::rebase_animation`). All or nothing.
    pub fn slide_clip(&mut self, clip_id: Uuid, delta: f64, footage: &SourceLimits) -> Result<EditOutcome> {
        check_delta(delta)?;
        let (ti, ci, prev, next, range) = self.slide_plan(clip_id, footage)?;
        let applied = range.resolve(delta, "slide the clip")?;
        let mut moved = self.tracks[ti].clips[ci].clone();
        // Where the edit's far edge was — the end of the last clip it changes — and
        // which clips are the edit's own, so what that edge runs into can be found.
        let mut far = moved.timeline_end();
        let mut skip = vec![ci];
        moved.timeline_start += applied;
        let mut prev_clip = None;
        if let Some((i, true)) = prev {
            let mut p = self.tracks[ti].clips[i].clone();
            let looping = footage_of(footage, &p)?.is_infinite();
            p.move_tail(applied, looping);
            weld(&p, &mut moved);
            p.clamp_fades();
            skip.push(i);
            prev_clip = Some((i, p));
        }
        let mut next_clip = None;
        if let Some((i, true)) = next {
            let mut n = self.tracks[ti].clips[i].clone();
            let looping = footage_of(footage, &n)?.is_infinite();
            far = n.timeline_end();
            n.move_head(applied, looping);
            weld(&moved, &mut n);
            n.clamp_fades();
            skip.push(i);
            next_clip = Some((i, n, looping));
        }
        if let Some(limit) = self.start_after(ti, far, &skip) {
            match next_clip.as_mut() {
                Some((_, n, looping)) => fit_end(n, limit, *looping),
                None => {
                    let looping = footage.get(&moved.asset_id).is_some_and(|l| l.is_infinite());
                    fit_end(&mut moved, limit, looping);
                }
            }
        }
        let mut out = Vec::new();
        if let Some((i, p)) = prev_clip {
            self.tracks[ti].clips[i] = p.clone();
            out.push(p);
        }
        self.tracks[ti].clips[ci] = moved.clone();
        out.push(moved);
        if let Some((i, n, _)) = next_clip {
            self.tracks[ti].clips[i] = n.clone();
            out.push(n);
        }
        Ok(EditOutcome::new(delta, applied, out))
    }

    // ---- split and remove ---------------------------------------------------

    /// **Split and remove**: cut a clip at timeline time `at` and throw one half
    /// away — "trim the start / end to the playhead" in one edit. Returns the
    /// half that stays, which **keeps the clip's id** (so a selection survives and
    /// [`Timeline::ripple_from`] reads the edit for what it is, an ordinary trim).
    ///
    /// Leaving `Right` shortens the clip at its end. Leaving `Left`, the clip's
    /// start moves up to `at` so its end stays put — the same shape a left-edge
    /// trim has — which leaves the gap where the removed half was; under ripple
    /// mode the caller's `ripple_from` closes it, holding the clip's start and
    /// pulling the rest of the track in (see [`Timeline::ripple_from`]).
    ///
    /// The clip keeps what belonged to the half that stayed and drops what
    /// belonged to the half that went: a new hard edge carries no fade and no
    /// transition, so removing the left drops `fade_in` and `transition_in` (the
    /// transition blended the old start, which is gone — the same half a plain
    /// split leaves it off), and removing the right drops `fade_out`; the other
    /// fade is held inside what is left. Keyframes ride with the content when the
    /// head moved. `at` must lie inside the clip and leave at least
    /// [`MIN_EDIT_CLIP`] of it; the track must be unlocked.
    pub fn split_remove(&mut self, clip_id: Uuid, at: f64, side: SplitSide) -> Result<Clip> {
        if !at.is_finite() {
            return Err(Error::InvalidArgument("the split point must be a finite time".to_string()));
        }
        let (ti, ci) = self.editable_clip(clip_id)?;
        let mut clip = self.tracks[ti].clips[ci].clone();
        let (start, end) = (clip.timeline_start, clip.timeline_end());
        if at <= start + DIFF_EPS || at >= end - DIFF_EPS {
            return Err(Error::InvalidArgument(format!(
                "the split point {} is not inside the clip ({}–{})",
                fmt_time(at),
                fmt_time(start),
                fmt_time(end)
            )));
        }
        let kept = match side {
            SplitSide::Left => end - at,
            SplitSide::Right => at - start,
        };
        if kept < MIN_EDIT_CLIP - DIFF_EPS {
            return Err(Error::InvalidArgument(format!(
                "that would leave only {kept:.2}s of the clip — remove the clip instead"
            )));
        }
        match side {
            SplitSide::Left => clip.cut_start_at(at),
            SplitSide::Right => clip.cut_end_at(at),
        }
        self.tracks[ti].clips[ci] = clip.clone();
        Ok(clip)
    }
}

impl Clip {
    /// Cut the clip's end back to timeline time `at`, which lies inside it: the part after `at` is
    /// gone, so the new hard edge carries no fade-out and the fade-in is held inside what is left.
    /// What [`Timeline::split_remove`] does for [`SplitSide::Right`] and what a range edit does to
    /// the clip a range reaches into from the left.
    pub(super) fn cut_end_at(&mut self, at: f64) {
        self.move_tail(at - self.timeline_end(), false);
        self.fade_out = 0.0;
        self.clamp_fades();
    }

    /// Cut the clip's start up to timeline time `at`, which lies inside it, so its end stays where it
    /// was: the footage and the animation ride with the part that stays, and the new hard edge
    /// carries no fade-in and no transition (the transition blended the old start, which is gone).
    /// What [`Timeline::split_remove`] does for [`SplitSide::Left`]; the clip now starts at `at`.
    pub(super) fn cut_start_at(&mut self, at: f64) {
        self.move_head(at - self.timeline_start, false);
        self.timeline_start = at;
        self.fade_in = 0.0;
        self.transition_in = None;
        self.clamp_fades();
    }
}

impl Timeline {
    /// **Split and remove** on several clips at once — the playhead trim of a
    /// selection, V1 and its A1 partner together — as one edit: every cut in `cuts`
    /// is [`Timeline::split_remove`] with the same `side`, and the survivors come
    /// back in request order.
    ///
    /// All or nothing: an unknown clip, a locked track, a cut outside its clip or
    /// one that would leave under [`MIN_EDIT_CLIP`] refuses the whole group and the
    /// timeline is exactly as it was. **At most one clip per track**, and a clip once:
    /// a playhead is inside one clip of a lane, and under ripple mode a lane trimmed
    /// at two places has no single edit point to hold still (see
    /// [`Timeline::ripple_from`]) — so each track ripples on its own, from its own
    /// one cut. Different tracks may be cut at different times.
    pub fn split_remove_clips(&mut self, cuts: &[ClipCut], side: SplitSide) -> Result<Vec<Clip>> {
        if cuts.is_empty() {
            return Err(Error::InvalidArgument("no clips to cut".to_string()));
        }
        let mut seen_clips = HashSet::new();
        let mut seen_tracks = HashSet::new();
        for cut in cuts {
            let (ti, _) = self.locate(cut.clip_id).ok_or(Error::ClipNotFound(cut.clip_id))?;
            if !seen_clips.insert(cut.clip_id) {
                return Err(Error::InvalidArgument(format!("clip {} appears more than once", cut.clip_id)));
            }
            if !seen_tracks.insert(ti) {
                return Err(Error::InvalidArgument(format!(
                    "two of the clips are on track {} — a group trim cuts one clip per track",
                    self.tracks[ti].name
                )));
            }
        }
        // Cut a copy, so a refusal part-way through leaves the timeline untouched.
        let mut scratch = self.clone();
        let kept = cuts
            .iter()
            .map(|cut| scratch.split_remove(cut.clip_id, cut.at, side))
            .collect::<Result<Vec<_>>>()?;
        *self = scratch;
        Ok(kept)
    }
}
