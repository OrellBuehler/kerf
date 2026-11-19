//! Placing and cutting clips: add, split, trim and move, and the roll, slip, slide and insert edit modes.

use super::*;

impl Project {
    // ---- timeline operations ---------------------------------------------

    /// Add a clip referencing `[source_in, source_out)` of an asset to a track.
    /// When `track_id` is omitted the asset's primary kind picks the track;
    /// when `timeline_start` is omitted the clip is appended after the last one.
    pub fn add_clip_to_timeline(
        &self,
        asset_id: Uuid,
        track_id: Option<Uuid>,
        source_in: f64,
        source_out: f64,
        timeline_start: Option<f64>,
    ) -> Result<Clip> {
        let asset = self.require_asset(asset_id)?;
        if !source_in.is_finite() || !source_out.is_finite() {
            return Err(Error::InvalidArgument(
                "source_in and source_out must be finite numbers".to_string(),
            ));
        }
        if source_in < 0.0 {
            return Err(Error::InvalidArgument("source_in must be >= 0".to_string()));
        }
        if source_out <= source_in {
            return Err(Error::InvalidArgument(
                "source_out must be greater than source_in".to_string(),
            ));
        }
        asset.check_source_out(source_out)?;
        if timeline_start.is_some_and(|t| !t.is_finite() || t < 0.0) {
            return Err(Error::InvalidArgument("timeline_start must be >= 0".to_string()));
        }
        let primary = asset.primary_kind();
        // The new clip may not land on another clip of its track (checked after the ripple, which
        // is how an insert onto footage pushes the later clips along instead).
        let clip_id = Uuid::new_v4();
        self.edit_lanes.borrow_mut().push(clip_id);
        self.edit_timeline("Add clip", |timeline| {
            let tid = match track_id {
                Some(t) => {
                    timeline.ensure_track_editable(t)?;
                    let track = timeline.track(t).ok_or(Error::TrackNotFound(t))?;
                    let picture = asset.streams.iter().any(|s| s.kind == StreamKind::Video);
                    check_track_kind(track, picture, asset.has_audio(), "add it to a track of its kind")?;
                    t
                }
                None => default_track(timeline, primary)?,
            };
            let start = timeline_start.unwrap_or_else(|| timeline.track(tid).map(Track::end).unwrap_or(0.0));
            let mut clip = Clip::for_asset(&asset, source_in, source_out, start);
            clip.id = clip_id;
            let track = timeline.track_mut(tid).unwrap();
            track.clips.push(clip.clone());
            track.sort_by_start();
            Ok(clip)
        })
    }

    /// Append a cut of `[start, end)` of an asset to the matching track.
    pub fn cut_clip(&self, asset_id: Uuid, start: f64, end: f64) -> Result<Clip> {
        self.add_clip_to_timeline(asset_id, None, start, end, None)
    }

    /// Split a timeline clip at timeline time `at` into two adjacent clips.
    ///
    /// **Linked clips are split with it**: each partner that has `at` inside it is
    /// cut at the same time, and the new right halves are linked to each other (the
    /// left halves keep the group) — the razor on a picture cuts its sound too. A
    /// partner on a locked track refuses the whole split; `with_links(Some(false))`
    /// splits the named clip alone. The `(left, right)` returned are the named
    /// clip's halves.
    pub fn split_at(&self, clip_id: Uuid, at: f64) -> Result<(Clip, Clip)> {
        finite("the split point", at)?;
        let links = self.links_active();
        self.edit_timeline("Split clip", |timeline| {
            timeline.ensure_editable(clip_id)?;
            if links {
                timeline.split_clip_linked(clip_id, at)
            } else {
                timeline.split_clip(clip_id, at)
            }
        })
    }

    /// Adjust a clip's source in/out points. `timeline_start` moves the clip in
    /// the same edit — a left-edge trim from the GUI shifts the start so the
    /// right edge stays put, and doing both here keeps undo a single step.
    /// Omitted, the timeline position is preserved.
    ///
    /// In **ripple mode** the later clips on the track follow the change in the
    /// clip's length, and a left-edge trim keeps the clip's start whether or not
    /// `timeline_start` was passed (see [`Timeline::ripple_from`]); the returned
    /// clip is the one as it ended up.
    ///
    /// The new points must be numbers, `source_in` at least 0 and `source_out` no further than
    /// the asset's footage; the trimmed clip must not end up on another clip of its track (judged
    /// after the ripple, which may make the room).
    ///
    /// **Linked clips follow the edge**: a partner that shares the edge being
    /// trimmed (within 1 ms) has that edge moved by the same amount, clamped to its
    /// own footage; a pure move (`timeline_start` alone) moves them by the same Δt.
    /// A locked partner refuses the trim, and so does one that would land on a clip
    /// outside its group — judged after the ripple, which may have made room — as
    /// [`Project::move_clip`] refuses a move onto one. `with_links(Some(false))` trims
    /// the named clip alone.
    pub fn trim(
        &self,
        clip_id: Uuid,
        source_in: Option<f64>,
        source_out: Option<f64>,
        timeline_start: Option<f64>,
    ) -> Result<Clip> {
        // The partners follow the trimmed edge only if there are any: a project that
        // links nothing never loads its assets for this.
        let links = self.links_active() && self.working_has_links()?;
        let footage = if links { self.source_limits()? } else { SourceLimits::new() };
        Clip::check_trim(source_in, source_out, timeline_start)?;
        self.edit_lanes.borrow_mut().push(clip_id);
        let clip = self.edit_named(
            &[clip_id],
            |_| "Trim clip".to_string(),
            |timeline| {
                timeline.ensure_editable(clip_id)?;
                let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
                let was = timeline.tracks[ti].clips[ci].clone();
                if let Some(value) = source_out {
                    // Footage can only be asked for as far as it goes (a still has no end).
                    if let Some(asset) = self.get_asset(was.asset_id)? {
                        asset.check_source_out(value)?;
                    }
                }
                let clip = &mut timeline.tracks[ti].clips[ci];
                if let Some(value) = source_in {
                    clip.source_in = value;
                }
                if let Some(value) = source_out {
                    clip.source_out = value;
                }
                if clip.source_out <= clip.source_in {
                    return Err(Error::InvalidArgument(
                        "source_out must be greater than source_in".to_string(),
                    ));
                }
                // A speed ramp rides with the footage: a head that moved takes it along.
                clip.follow_head(&was);
                if let Some(start) = timeline_start {
                    clip.timeline_start = start.max(0.0);
                }
                let out = clip.clone();
                if timeline_start.is_some() {
                    timeline.tracks[ti].sort_by_start();
                }
                if links {
                    // The partners follow the edge that moved (see `carry_extent_edit`).
                    let mut trimmed = Vec::new();
                    let partners = timeline.carry_extent_edit_noted(clip_id, &was, &footage, &mut trimmed)?;
                    self.edit_notes.borrow_mut().extend(trimmed);
                    self.edit_carried.borrow_mut().extend(partners.iter().map(|p| p.id));
                }
                Ok(out)
            },
        )?;
        // The ripple can move the trimmed clip itself (a left-edge trim keeps its
        // start), so hand back what is on the timeline rather than what the trim
        // alone produced.
        if self.ripple_active()? {
            if let Some(current) = self.working_timeline()?.clip(clip.id) {
                return Ok(current.clone());
            }
        }
        Ok(clip)
    }

    /// Cut a **source-time** range out of a clip: the clip is split around the
    /// intersection of `[from, to]` with its source window, the middle piece
    /// removed, and later clips on the track ripple left to close the gap.
    /// This is the transcript-editing primitive — delete a sentence and the
    /// cut tightens. Returns the kept pieces in play order. It closes the gap
    /// itself, so ripple mode leaves it alone.
    ///
    /// **Linked clips lose the same stretch**: the *timeline* span the cut removes
    /// is taken out of every partner it overlaps too, each closing up on its own
    /// track — so cutting a sentence from a picture cuts its detached sound. A
    /// locked partner refuses the cut; `with_links(Some(false))` cuts the named clip
    /// alone.
    pub fn cut_clip_range(&self, clip_id: Uuid, from: f64, to: f64) -> Result<Vec<Clip>> {
        finite("from", from)?;
        finite("to", to)?;
        let links = self.links_active();
        self.edit_named_exact(
            &[clip_id],
            |_| "Cut range".to_string(),
            |timeline| {
                timeline.ensure_editable(clip_id)?;
                if links {
                    let mut trimmed = Vec::new();
                    let kept = timeline.cut_clip_range_linked_noted(clip_id, from, to, &mut trimmed);
                    self.edit_notes.borrow_mut().extend(trimmed);
                    kept
                } else {
                    timeline.cut_clip_range(clip_id, from, to)
                }
            },
        )
    }

    /// Move a clip to a new index within its track and re-flow the track gaplessly.
    pub fn reorder(&self, track_id: Uuid, clip_id: Uuid, new_index: usize) -> Result<()> {
        self.edit_named_exact(
            &[clip_id],
            |_| "Reorder clip".to_string(),
            |timeline| {
                timeline.ensure_track_editable(track_id)?;
                let track = timeline.track_mut(track_id).ok_or(Error::TrackNotFound(track_id))?;
                let current = track
                    .clips
                    .iter()
                    .position(|c| c.id == clip_id)
                    .ok_or(Error::ClipNotFound(clip_id))?;
                let clip = track.clips.remove(current);
                let index = new_index.min(track.clips.len());
                track.clips.insert(index, clip);
                track.reflow();
                Ok(())
            },
        )
    }

    /// Move a clip to a new timeline position, optionally onto another track of
    /// the **same kind**. Free positioning: the clip keeps its duration and
    /// gaps are allowed. A move that would overlap another clip on the
    /// destination track is rejected, so each track stays a well-ordered,
    /// non-overlapping lane (which keeps the positional render well-defined).
    /// A move never ripples, whatever the ripple mode.
    ///
    /// **Linked clips move with it** — each partner by the same Δt, on its own track
    /// (a track change is the named clip's alone). A clip with partners is moved as
    /// the group it is, through [`Project::move_clips`]: one `Move N clips`
    /// revision, all or nothing, refusing a locked track (the named clip's too) and
    /// a partner that would start before 0 or land on another clip.
    /// `with_links(Some(false))` moves the named clip alone, by the rules below.
    pub fn move_clip(&self, clip_id: Uuid, timeline_start: f64, track_id: Option<Uuid>) -> Result<Clip> {
        finite("timeline_start", timeline_start)?;
        let start = timeline_start.max(0.0);
        if self.links_active() && self.working_has_links()? && !self.working_timeline()?.link_partners(clip_id).is_empty() {
            let moved = self.move_clips(&[ClipMove {
                clip_id,
                timeline_start: start,
                track_id,
            }])?;
            return Ok(moved.into_iter().next().expect("the named clip was moved"));
        }
        self.edit_timeline_exact("Move clip", |timeline| {
            timeline.ensure_editable(clip_id)?;
            if let Some(tid) = track_id {
                timeline.ensure_track_editable(tid)?;
            }
            let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
            let src_kind = timeline.tracks[ti].kind;
            let dest_ti = match track_id {
                Some(tid) => {
                    let d = timeline
                        .tracks
                        .iter()
                        .position(|t| t.id == tid)
                        .ok_or(Error::TrackNotFound(tid))?;
                    if timeline.tracks[d].kind != src_kind {
                        return Err(Error::InvalidArgument(
                            "cannot move a clip to a track of a different kind".to_string(),
                        ));
                    }
                    d
                }
                None => ti,
            };
            let mut clip = timeline.tracks[ti].clips[ci].clone();
            let end = start + clip.duration();
            let overlaps = timeline.tracks[dest_ti]
                .clips
                .iter()
                .any(|c| c.id != clip_id && start < c.timeline_end() && c.timeline_start < end);
            if overlaps {
                return Err(Error::InvalidArgument(
                    "clip would overlap another clip on the destination track".to_string(),
                ));
            }
            clip.timeline_start = start;
            timeline.tracks[ti].clips.remove(ci);
            timeline.tracks[dest_ti].clips.push(clip.clone());
            timeline.tracks[dest_ti].sort_by_start();
            Ok(clip)
        })
    }

    /// Move several clips in **one** edit — one revision, `Move N clips` — for a
    /// marquee selection dragged together. Each [`ClipMove`] names a clip, where
    /// it starts afterwards (absolute seconds) and, optionally, another track of
    /// the same kind; the UI works out the landing spots, this decides whether
    /// they are legal.
    ///
    /// All or nothing: the whole group is validated before anything moves, and
    /// any error — an unknown clip or track, a different-kind track, a locked
    /// track (source or destination), a clip named twice, a start before 0 or
    /// not finite, or a landing spot that overlaps a clip that is staying or
    /// another clip in the group — leaves the timeline and its history exactly as
    /// they were. Clips moving together may pass through the places they are
    /// leaving, so nudging a run of abutting clips by a second is fine.
    /// Never ripples. Returns the moved clips in request order.
    ///
    /// **Linked clips travel**: every clip linked to one named, and not named
    /// itself, moves by the same Δt on its own track (`Timeline::with_linked_moves`),
    /// and is checked in the same group — so the answer is the named clips in
    /// request order *then* the partners carried along, and the label counts all of
    /// them. A partner that would start before 0, or sits on a locked track, refuses
    /// the move. `with_links(Some(false))` moves exactly what is named.
    pub fn move_clips(&self, moves: &[ClipMove]) -> Result<Vec<Clip>> {
        let links = self.links_active();
        // The label counts the partners carried along, which only the edit knows.
        let label = |moved: &Vec<Clip>| match moved.len() {
            1 => "Move clip".to_string(),
            n => format!("Move {n} clips"),
        };
        // Named clips are authoritative: two partners both named and moved apart were
        // parted by hand, which the guard refuses rather than the sync lock undoing.
        let anchors: Vec<Uuid> = moves.iter().map(|m| m.clip_id).collect();
        self.edit_named_exact(&anchors, label, |timeline| {
            if links {
                let all = timeline.with_linked_moves(moves)?;
                timeline.move_clips(&all)
            } else {
                timeline.move_clips(moves)
            }
        })
    }

    // ---- edit modes: roll, slip, slide, split-and-remove ---------------------

    /// How far each asset's footage reaches — what roll, slip and slide clamp a
    /// clip's source window to ([`crate::model::Asset::source_limit`]: the
    /// duration, infinite for a still), and a generated title's document its first 24 hours.
    pub fn source_limits(&self) -> Result<SourceLimits> {
        Ok(self
            .list_assets()?
            .into_iter()
            .map(|asset| (asset.id, asset.source_limit()))
            .chain([(GENERATED_ASSET, MAX_SVG_SECONDS)])
            .collect())
    }

    /// **Roll** the cut between two adjacent clips of one track by `delta`
    /// seconds (positive is later): `clip_a`'s end and `clip_b`'s start move
    /// together, so the pair covers the same stretch and nothing after it moves.
    /// Clamped to each clip's footage and a 0.05 s floor; the outcome says how far
    /// it really went. One `Roll edit` revision. See [`Timeline::roll_edit`] for
    /// every rule. Never ripples — it moves no length, only where footage changes
    /// hands — whatever the ripple mode.
    ///
    /// **Linked clips roll too**: each pair of partners sharing the cut (a partner of
    /// `clip_a` and one of `clip_b` that touch on one track) rolls by the same
    /// amount, and the whole group clamps to its tightest pair. A partner with no
    /// pair — a clip running through the cut — is left alone.
    pub fn roll_edit(&self, clip_a: Uuid, clip_b: Uuid, delta: f64) -> Result<EditOutcome> {
        finite("delta", delta)?;
        let footage = self.source_limits()?;
        let links = self.links_active();
        self.edit_timeline_exact("Roll edit", |timeline| {
            if links {
                timeline.roll_edit_linked(clip_a, clip_b, delta, &footage)
            } else {
                timeline.roll_edit(clip_a, clip_b, delta, &footage)
            }
        })
    }

    /// **Slip** a clip: show a different part of its footage in the same place
    /// and for the same length. `delta` is in *source* seconds; positive starts
    /// the clip later in its own footage (the mirrored window for a reversed
    /// clip). Clamped to the footage; a still has none to slip. One `Slip clip`
    /// revision. See [`Timeline::slip_clip`]. Never ripples.
    ///
    /// **Linked clips slip too**, by the same *timeline* shift of the footage (a
    /// partner at another speed slips by the matching source seconds; a still, with
    /// no footage, is skipped), and the group clamps to its tightest member.
    pub fn slip_clip(&self, clip_id: Uuid, delta: f64) -> Result<EditOutcome> {
        finite("delta", delta)?;
        let footage = self.source_limits()?;
        let links = self.links_active();
        self.edit_timeline_exact("Slip clip", |timeline| {
            if links {
                timeline.slip_clip_linked(clip_id, delta, &footage)
            } else {
                timeline.slip_clip(clip_id, delta, &footage)
            }
        })
    }

    /// **Slide** a clip along its track by `delta` timeline seconds, the
    /// neighbours that touch it giving way (the previous clip's end and the next
    /// clip's start move with it), so its content and the stretch the three span
    /// are unchanged. Clamped to the neighbours' footage and a 0.05 s floor. One
    /// `Slide clip` revision. See [`Timeline::slide_clip`] for the rules with
    /// gaps. Never ripples.
    ///
    /// **Linked clips slide too**, each with its own touching neighbours giving way,
    /// and the group clamps to its tightest member.
    pub fn slide_clip(&self, clip_id: Uuid, delta: f64) -> Result<EditOutcome> {
        finite("delta", delta)?;
        let footage = self.source_limits()?;
        let links = self.links_active();
        self.edit_timeline_exact("Slide clip", |timeline| {
            if links {
                timeline.slide_clip_linked(clip_id, delta, &footage)
            } else {
                timeline.slide_clip(clip_id, delta, &footage)
            }
        })
    }

    /// **Split and remove**: cut a clip at timeline time `at` and drop the
    /// `side` half, in one `Split and remove left` / `… right` revision. Unlike
    /// the three above this *does* follow ripple mode, because it changes a clip's
    /// length like any trim: with it on, the later clips on the track close the
    /// gap (a left removal keeps the clip's own start and pulls the rest in); with
    /// it off the gap stays, exactly as after a plain split and delete. The
    /// surviving half keeps the clip's id; see [`Timeline::split_remove`].
    /// Returns it as it ended up on the timeline.
    pub fn split_remove(&self, clip_id: Uuid, at: f64, side: SplitSide) -> Result<Clip> {
        let mut kept = self.split_remove_clips(&[ClipCut { clip_id, at }], side)?;
        Ok(kept.remove(0))
    }

    /// [`Self::split_remove`] on several clips as **one** revision — the playhead
    /// trim of a selection, V1 and its A1 partner together, which undoes in one
    /// step. Each cut names a clip and the time it is cut at; `side` is the same for
    /// all. All or nothing, one clip per track (see
    /// [`Timeline::split_remove_clips`]); each track then ripples on its own, from its
    /// own one cut, when ripple mode is on. The label is `Split and remove left` /
    /// `… right`, with `(N clips)` appended for a group. Returns the surviving
    /// clips, in request order, as they ended up on the timeline.
    ///
    /// **Linked clips are cut with them**: a partner not named, on a track the
    /// request does not already cut, with the cut time inside it, is cut at the same
    /// time (`Timeline::with_linked_cuts`) — so a one-clip call on a picture trims
    /// its sound too, and the answer is the named survivors then the partners'. A
    /// partner the cut time falls outside is untouched. `with_links(Some(false))`
    /// cuts exactly what is named.
    pub fn split_remove_clips(&self, cuts: &[ClipCut], side: SplitSide) -> Result<Vec<Clip>> {
        for cut in cuts {
            finite("the cut time", cut.at)?;
        }
        let what = match side {
            SplitSide::Left => "Split and remove left",
            SplitSide::Right => "Split and remove right",
        };
        let links = self.links_active();
        let anchors: Vec<Uuid> = cuts.iter().map(|c| c.clip_id).collect();
        // The label counts the partners cut with the named clips: what the edit cut.
        let label = |kept: &Vec<Clip>| match kept.len() {
            0 | 1 => what.to_string(),
            n => format!("{what} ({n} clips)"),
        };
        let kept = self.edit_named(&anchors, label, |timeline| {
            if links {
                let all = timeline.with_linked_cuts(cuts)?;
                timeline.split_remove_clips(&all, side)
            } else {
                timeline.split_remove_clips(cuts, side)
            }
        })?;
        // As `trim`: ripple can move a surviving half itself (a left removal keeps
        // its start), so hand back what is on the timeline.
        if self.ripple_active()? {
            let now = self.working_timeline()?;
            return Ok(kept
                .into_iter()
                .map(|clip| now.clip(clip.id).cloned().unwrap_or(clip))
                .collect());
        }
        Ok(kept)
    }

    /// Insert `placements` — each a `(track_id, clip)` pair — so the earliest
    /// lands at `at`, preserving the relative offsets between them. Backs both
    /// paste (clips carried on a clipboard, whose sources may already be gone)
    /// and (through `insert_clips_into`) [`Self::duplicate_clips`].
    ///
    /// Everything about a clip comes along — trims, speed, transform, color,
    /// transition, effects, keyframes and reframe — which is exactly what
    /// `add_clip_to_timeline` cannot do, since that builds a fresh
    /// [`Clip::for_asset`]. Each clip is given a new id, so a clipboard can be
    /// pasted repeatedly.
    ///
    /// All-or-nothing: if any clip would overlap, the whole insert is rejected,
    /// so a partial paste can never land. Clips pasted alongside each other are
    /// checked against one another too, not just against what is already there.
    pub fn insert_clips(&self, placements: &[(Uuid, Clip)], at: f64) -> Result<Vec<Clip>> {
        self.insert_clips_into(placements, at, true)
    }

    /// [`Project::insert_clips`], judging each clip's track kind or not: a duplicate keeps a clip on the
    /// track it is already on, which is not the duplicate's to question.
    fn insert_clips_into(&self, placements: &[(Uuid, Clip)], at: f64, check_kind: bool) -> Result<Vec<Clip>> {
        finite("the paste position", at)?;
        if placements.is_empty() {
            return Err(Error::InvalidArgument("no clips to insert".to_string()));
        }
        let at = at.max(0.0);
        let base = placements.iter().map(|(_, c)| c.timeline_start).fold(f64::INFINITY, f64::min);
        // What each clip's footage has, to keep it off a track it cannot play on: a picture on a video track
        // (a title is one), sound on an audio track. A clip whose asset is unknown here is not judged.
        let mut streams: HashMap<Uuid, (bool, bool)> = HashMap::new();
        if check_kind {
            for (_, clip) in placements {
                if !clip.is_generated() && !streams.contains_key(&clip.asset_id) {
                    if let Some(asset) = self.get_asset(clip.asset_id)? {
                        let video = asset.streams.iter().any(|s| s.kind == StreamKind::Video);
                        streams.insert(clip.asset_id, (video, asset.has_audio()));
                    }
                }
            }
        }

        self.edit_timeline_exact("Insert clips", |timeline| {
            // A copy is a new clip, so it cannot share a link group with the original
            // — but copies of a linked *group* (a picture and its sound, pasted
            // together) are linked to each other under a fresh id, and a copy whose
            // partner was not pasted along with it is simply unlinked.
            let mut members: HashMap<Uuid, usize> = HashMap::new();
            for (_, clip) in placements {
                if let Some(link) = clip.link_id {
                    *members.entry(link).or_default() += 1;
                }
            }
            let fresh: HashMap<Uuid, Uuid> = members
                .into_iter()
                .filter(|(_, n)| *n >= 2)
                .map(|(link, _)| (link, Uuid::new_v4()))
                .collect();
            // Resolve every destination first, so an unknown or locked track fails before
            // any edit lands.
            let mut staged: Vec<(usize, Clip)> = Vec::with_capacity(placements.len());
            for (track_id, clip) in placements {
                timeline.ensure_track_editable(*track_id)?;
                let ti = timeline
                    .tracks
                    .iter()
                    .position(|t| t.id == *track_id)
                    .ok_or(Error::TrackNotFound(*track_id))?;
                if check_kind {
                    let (picture, sound) = if clip.is_generated() {
                        (true, false)
                    } else {
                        streams.get(&clip.asset_id).copied().unwrap_or((true, true))
                    };
                    check_track_kind(&timeline.tracks[ti], picture, sound, "paste it on a track of its kind")?;
                }
                let mut copy = clip.clone();
                copy.id = Uuid::new_v4();
                copy.timeline_start = at + (clip.timeline_start - base);
                copy.link_id = clip.link_id.and_then(|link| fresh.get(&link).copied());
                staged.push((ti, copy));
            }

            // A picture whose sound was detached is silent *because* that sound plays
            // from its partner. Pasted without the partner it would be a silent clip
            // for good, so it gets its own sound back; kept with a partner that does
            // carry the same footage's audio it stays muted (heard once, from there).
            let carriers: HashSet<(Uuid, Uuid)> = staged
                .iter()
                .filter(|(ti, c)| timeline.tracks[*ti].kind == StreamKind::Audio && c.link_id.is_some())
                .map(|(_, c)| (c.link_id.expect("filtered"), c.asset_id))
                .collect();
            for (ti, clip) in &mut staged {
                if timeline.tracks[*ti].kind == StreamKind::Video
                    && !clip.source_audio
                    && !clip.link_id.is_some_and(|link| carriers.contains(&(link, clip.asset_id)))
                {
                    clip.source_audio = true;
                }
            }

            for (i, (ti, clip)) in staged.iter().enumerate() {
                let (start, end) = (clip.timeline_start, clip.timeline_end());
                let hits_existing = timeline.tracks[*ti]
                    .clips
                    .iter()
                    .any(|c| start < c.timeline_end() && c.timeline_start < end);
                let hits_sibling = staged
                    .iter()
                    .enumerate()
                    .any(|(j, (oti, o))| j != i && oti == ti && start < o.timeline_end() && o.timeline_start < end);
                if hits_existing || hits_sibling {
                    return Err(Error::InvalidArgument(
                        "pasted clips would overlap existing clips — move the playhead to free space".to_string(),
                    ));
                }
            }

            for (ti, clip) in &staged {
                timeline.tracks[*ti].clips.push(clip.clone());
                timeline.tracks[*ti].sort_by_start();
            }
            Ok(staged.into_iter().map(|(_, c)| c).collect())
        })
    }

    /// Copy the clips named by `clip_ids` and insert the copies so the earliest
    /// lands at `at`, each staying on its source track. The by-id convenience
    /// over [`Self::insert_clips`], for duplicate and for agent use — except
    /// that it does not judge a clip's track kind (the clip is already on that
    /// track) and so looks up no asset.
    pub fn duplicate_clips(&self, clip_ids: &[Uuid], at: f64) -> Result<Vec<Clip>> {
        finite("the paste position", at)?;
        let timeline = self.working_timeline()?;
        let placements = clip_ids
            .iter()
            .map(|id| {
                timeline
                    .locate(*id)
                    .map(|(ti, ci)| (timeline.tracks[ti].id, timeline.tracks[ti].clips[ci].clone()))
                    .ok_or(Error::ClipNotFound(*id))
            })
            .collect::<Result<Vec<_>>>()?;
        self.insert_clips_into(&placements, at, false)
    }

    /// Remove a clip and close the gap it leaves: every later clip on the **same
    /// track** shifts left by the removed clip's duration. (Plain [`remove`]
    /// leaves a gap — unless ripple mode is on, which makes it this.)
    ///
    /// **Linked clips are deleted with it**, each closing the gap on its own track
    /// by its own length; a partner on a locked track refuses the delete.
    /// `with_links(Some(false))` deletes the named clip alone.
    pub fn ripple_delete(&self, clip_id: Uuid) -> Result<()> {
        let links = self.links_active();
        self.edit_named_exact(
            &[clip_id],
            |_| "Ripple delete".to_string(),
            |timeline| {
                timeline.ensure_editable(clip_id)?;
                if links {
                    let mut trimmed = Vec::new();
                    let done = timeline.ripple_delete_linked_noted(clip_id, &mut trimmed);
                    self.edit_notes.borrow_mut().extend(trimmed);
                    done.map(|_| ())
                } else {
                    timeline.ripple_delete_clip(clip_id)
                }
            },
        )
    }
}
