//! Sound effects at the `Project` level: the project half of [`crate::engine::sfx`].
//!
//! The usual heavy-op split, mirroring music and the voiceover:
//!
//! * **Add.** [`Project::sound_effects_inputs`] (under the lock: checks every placement against
//!   the **working timeline** and rehearses the placement, so a refusal comes before anything is
//!   rendered) → [`Project::synthesize_sound_effects`] (static, lock released: renders, probes,
//!   names each `SFX: Whoosh`) → [`Project::place_sound_effects`] (under the lock, one
//!   transaction: stores the assets and puts every clip on the timeline as **one** `Add sound
//!   effects` revision).
//! * **Reshape.** [`Project::update_sound_effect_inputs`] → [`Project::synthesize_sound_effect`] →
//!   [`Project::apply_sound_effect_update`]: the same effect rendered again from an edited spec
//!   takes its clip's place with its **hit point** where it was.
//! * **Audition.** [`Project::preview_sound_effect`]: a render and nothing else.
//! * **Suggest.** [`Project::suggest_sound_effects_inputs`] (under the lock) → the pure
//!   [`crate::model::suggest_sound_effects`]: where the working cut would take an effect, as placements
//!   `add_sound_effects` takes as they are. A read.
//!
//! A placement says where the effect's *hit* goes (`at`); the clip starts that much earlier
//! ([`crate::model::sfx_anchor_s`]). Effects overlap often (a riser into an impact), so with no
//! `track_id` each goes on the first audio lane named `FX`, `FX 2`, `FX 3`… where it fits, and a new
//! lane is made when none does.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use super::{Project, SFX_TRACK};
use crate::engine;
use crate::error::{Error, Result};
use crate::model::{
    sfx_anchor_s, sfx_span, suggest_sound_effects, Asset, Clip, SfxSpec, SfxSuggestInputs, SfxSuggestRequest, SfxSuggestion,
    StreamKind, Timeline, Track, MIN_SFX_CLIP_S,
};

fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidArgument(msg.into())
}

/// `e` with `at` (`effects[2] (riser)`) in front of its message, when it is a refusal of the request.
fn within(at: &str, e: Error) -> Error {
    match e {
        Error::InvalidArgument(msg) => invalid(format!("{at}: {msg}")),
        other => other,
    }
}

/// The most effects one call places.
pub const MAX_SFX_PER_CALL: usize = 256;
/// The quietest and the loudest a placement's gain may be, dB (the fader's range).
pub const MIN_SFX_GAIN_DB: f64 = -60.0;
pub const MAX_SFX_GAIN_DB: f64 = 12.0;
/// The fade a head cut off at the start of the timeline gets, so it does not click, seconds.
const TRIM_FADE_S: f64 = 0.004;

// ---- requests ------------------------------------------------------------------------------

/// One effect to place: what to make, where its hit lands, how loud, and where.
///
/// On the wire it is flat — `{"kind":"whoosh","at":12.5,"pitch":-2,"volume_db":-6}` — and a
/// misspelled field is an error naming it.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct SfxPlacement {
    #[serde(flatten)]
    pub spec: SfxSpec,
    /// Where the effect's hit point lands on the timeline, seconds: a whoosh's peak, an impact's
    /// first transient, a riser's end. The clip starts that much earlier (cut off at 0 when that
    /// would be before the start of the timeline).
    pub at: f64,
    /// The clip's gain, dB (default 0, -60..=12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_db: Option<f64>,
    /// The audio track to put it on. Default: the first lane named `FX`, `FX 2`, … where it fits,
    /// a new one when none does. A named track must be an unlocked audio track with room for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub track_id: Option<Uuid>,
}

impl<'de> Deserialize<'de> for SfxPlacement {
    fn deserialize<D: Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        // `deny_unknown_fields` cannot be combined with `flatten`, so the spec's own fields are
        // handed to `SfxSpec` (which does refuse an unknown one) once the placement's are taken.
        #[derive(Deserialize)]
        struct Wire {
            at: f64,
            #[serde(default)]
            volume_db: Option<f64>,
            #[serde(default)]
            track_id: Option<Uuid>,
            #[serde(flatten)]
            spec: serde_json::Map<String, serde_json::Value>,
        }
        let wire = Wire::deserialize(de)?;
        let spec = SfxSpec::deserialize(serde_json::Value::Object(wire.spec)).map_err(serde::de::Error::custom)?;
        Ok(SfxPlacement {
            spec,
            at: wire.at,
            volume_db: wire.volume_db,
            track_id: wire.track_id,
        })
    }
}

/// A checked placement: the spec spelled out (its length resolved) and the gain defaulted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlannedSfx {
    pub spec: SfxSpec,
    pub at: f64,
    pub volume_db: f64,
    pub track_id: Option<Uuid>,
}

/// A checked batch: what [`Project::synthesize_sound_effects`] renders and
/// [`Project::place_sound_effects`] lands, in the order it was asked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SfxPlan {
    pub effects: Vec<PlannedSfx>,
}

/// A checked reshape: what [`Project::synthesize_sound_effect`] renders and
/// [`Project::apply_sound_effect_update`] swaps in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SfxUpdate {
    pub clip_id: Uuid,
    /// The new spec, its length spelled out.
    pub spec: SfxSpec,
    pub volume_db: Option<f64>,
    /// Where on the timeline the effect's hit point was when the plan was made, seconds: what the
    /// new render's hit goes back to (read again when the swap is made).
    pub anchor_at: f64,
}

/// An effect as it landed on the timeline: its clip, the asset the clip plays **as stored** and the
/// timeline second its hit really falls on.
///
/// The asset is the project's own row: the same render put on twice is one file and one asset, so it
/// is the one the first placement stored, not the fresh copy a caller rendered the second time. The
/// hit is read from the clip (the render's hit point through the clip's own mapping), so it is where
/// the effect is heard: the `at` that was asked for, except that a start a few milliseconds before 0
/// is put at 0 ([`crate::model::SFX_HEAD_SNAP_S`]) and its hit lands that much later.
#[derive(Debug, Clone, Serialize)]
pub struct SfxPlaced {
    pub clip: Clip,
    pub asset: Asset,
    /// Where on the timeline the hit point is, seconds.
    pub hit: f64,
}

/// `clip`, which plays `asset` (a render of `spec`), with the asset and where its hit falls.
fn placed(clip: Clip, asset: Asset, spec: &SfxSpec) -> SfxPlaced {
    let hit = clip.source_to_timeline(sfx_anchor_s(spec, asset.duration));
    SfxPlaced { clip, asset, hit }
}

// ---- placing -------------------------------------------------------------------------------

/// A stand-in for the render of `spec`: the length it will have, nothing rendered or probed.
fn stand_in(spec: &SfxSpec) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: String::new(),
        name: spec.title(),
        duration: spec.resolved_duration_s(),
        streams: Vec::new(),
        imported_at: chrono::Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: Some(spec.clone()),
        tags: Vec::new(),
    }
}

/// Where a render of `len` seconds starts so its hit lands on `at`, and how much of its head is cut off
/// to keep the clip from starting before 0: `(timeline_start, source_in)` ([`crate::model::sfx_span`]).
fn span_for(spec: &SfxSpec, len: f64, at: f64) -> Result<(f64, f64)> {
    sfx_span(spec, len, at).ok_or_else(|| {
        let anchor = sfx_anchor_s(spec, len);
        // Rounded up to the hundredth the message shows, so the time it names is one that lands.
        let earliest = (((anchor - len + MIN_SFX_CLIP_S) * 100.0 - 1e-6).ceil() / 100.0).max(0.0);
        invalid(format!(
            "its hit is {anchor:.2} s into its {len:.2} s, so it cannot land at {at:.2} s with the start of the timeline in the way; \
             place it at {earliest:.2} s or later",
        ))
    })
}

/// A clip for `asset` with its hit at `at`: [`Clip::for_asset`] over the render, cut at 0 when it
/// would start before it, with a short fade-in over a cut head.
fn effect_clip(asset: &Asset, spec: &SfxSpec, at: f64, volume_db: f64, id: Uuid) -> Result<Clip> {
    let (start, source_in) = span_for(spec, asset.duration, at)?;
    let mut clip = Clip::for_asset(asset, source_in, asset.duration, start);
    clip.id = id;
    clip.volume = 10f64.powf(volume_db / 20.0) as f32;
    if source_in > 0.0 {
        clip.fade_in = TRIM_FADE_S.min(clip.duration() / 2.0);
    }
    Ok(clip)
}

/// The lane number `FX` (1), `FX 2` (2), … a track name stands for.
fn lane_number(name: &str) -> Option<u32> {
    if name == SFX_TRACK {
        return Some(1);
    }
    let n: u32 = name.strip_prefix(SFX_TRACK)?.strip_prefix(' ')?.parse().ok()?;
    (n >= 2 && lane_name(n) == name).then_some(n)
}

fn lane_name(n: u32) -> String {
    if n <= 1 {
        SFX_TRACK.to_string()
    } else {
        format!("{SFX_TRACK} {n}")
    }
}

/// Whether `[start, end)` is free on `track` (touching a neighbour is not an overlap).
fn fits(track: &Track, start: f64, end: f64) -> bool {
    const EPS: f64 = 1e-6;
    track
        .clips
        .iter()
        .all(|c| !(start < c.timeline_end() - EPS && c.timeline_start < end - EPS))
}

/// The id of the effects lane an effect spanning `[start, end)` goes on: the first unlocked audio track named `FX`, `FX 2`, …
/// with room for `[start, end)`, or a new one (the first name no audio track has) pushed on the end.
fn fx_lane(timeline: &mut Timeline, start: f64, end: f64) -> Uuid {
    let mut lanes: Vec<(u32, Uuid, bool)> = timeline
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Audio)
        .filter_map(|t| lane_number(&t.name).map(|n| (n, t.id, !t.locked && fits(t, start, end))))
        .collect();
    lanes.sort_by_key(|(n, ..)| *n);
    if let Some((_, id, _)) = lanes.iter().find(|(.., ok)| *ok) {
        return *id;
    }
    let n = (1..)
        .find(|n| !lanes.iter().any(|(m, ..)| m == n))
        .expect("a free lane number");
    let track = Track::new(StreamKind::Audio, lane_name(n));
    let id = track.id;
    timeline.tracks.push(track);
    id
}

/// Put every effect of `plan` on `timeline`, in order, as the clips `ids` of `assets` (one asset per
/// effect): the one placement rule, shared by [`Project::place_sound_effects`] and the rehearsal
/// before the render. A named track must be an unlocked audio track; an effect with no track goes on
/// the lane [`fx_lane`] finds, the ones before it in the batch counted.
fn put_sound_effects(timeline: &mut Timeline, assets: &[Asset], plan: &SfxPlan, ids: &[Uuid]) -> Result<Vec<Clip>> {
    let mut placed = Vec::with_capacity(assets.len());
    for (i, ((asset, effect), id)) in assets.iter().zip(&plan.effects).zip(ids).enumerate() {
        let at = format!("effects[{i}] ({})", effect.spec.kind.name());
        let clip = effect_clip(asset, &effect.spec, effect.at, effect.volume_db, *id).map_err(|e| within(&at, e))?;
        let tid = match effect.track_id {
            Some(t) => {
                timeline.ensure_track_editable(t).map_err(|e| within(&at, e))?;
                let track = timeline.track(t).ok_or(Error::TrackNotFound(t))?;
                if track.kind != StreamKind::Audio {
                    return Err(invalid(format!(
                        "{at}: a sound effect goes on an audio track; {} is not one",
                        track.name
                    )));
                }
                t
            }
            None => fx_lane(timeline, clip.timeline_start, clip.timeline_end()),
        };
        let track = timeline.track_mut(tid).expect("track resolved above");
        track.clips.push(clip.clone());
        track.sort_by_start();
        placed.push(clip);
    }
    Ok(placed)
}

/// Swap the effect clip `clip_id` of `timeline` for a render of `spec` (`asset`) with its hit where
/// it was (`anchor_at`): the new clip keeps the old one's id, gain (unless `volume_db` says another),
/// fades, effects and keys, and plays the whole render at normal speed.
fn replace_effect(
    timeline: &mut Timeline,
    clip_id: Uuid,
    asset: &Asset,
    spec: &SfxSpec,
    volume_db: Option<f64>,
    anchor_at: f64,
) -> Result<Clip> {
    let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
    timeline.ensure_track_editable(timeline.tracks[ti].id)?;
    let old = timeline.tracks[ti].clips[ci].clone();
    let (start, source_in) = span_for(spec, asset.duration, anchor_at)?;
    let mut clip = old;
    clip.asset_id = asset.id;
    clip.source_in = source_in;
    clip.source_out = asset.duration;
    clip.timeline_start = start;
    clip.speed = 1.0;
    clip.speed_ramp.clear();
    if let Some(db) = volume_db {
        clip.volume = 10f64.powf(db / 20.0) as f32;
    }
    let duration = clip.duration();
    clip.fade_in = clip
        .fade_in
        .max(if source_in > 0.0 { TRIM_FADE_S } else { 0.0 })
        .min(duration);
    clip.fade_out = clip.fade_out.min(duration);
    timeline.tracks[ti].clips[ci] = clip.clone();
    timeline.tracks[ti].sort_by_start();
    Ok(clip)
}

fn check_gain(volume_db: f64) -> Result<()> {
    if !volume_db.is_finite() || !(MIN_SFX_GAIN_DB..=MAX_SFX_GAIN_DB).contains(&volume_db) {
        return Err(invalid(format!(
            "volume_db must be {MIN_SFX_GAIN_DB}..={MAX_SFX_GAIN_DB} (got {volume_db})"
        )));
    }
    Ok(())
}

impl Project {
    /// Check a batch of placements against the **working timeline** (an agent's staged cut included)
    /// and resolve it, under the lock: every spec is valid, every `at` and gain is a sane number, a
    /// named track exists, is an audio track and is not locked. Last, the placement is rehearsed on a
    /// copy of the cut ([`Project::check_sfx_fits`]), so an effect that would land on another clip of
    /// its named track is refused with `edit_timeline`'s own words **before anything is rendered**.
    pub fn sound_effects_inputs(&self, placements: &[SfxPlacement]) -> Result<SfxPlan> {
        if placements.is_empty() {
            return Err(invalid("there are no sound effects to add"));
        }
        if placements.len() > MAX_SFX_PER_CALL {
            return Err(invalid(format!("add at most {MAX_SFX_PER_CALL} sound effects at a time")));
        }
        let timeline = self.working_timeline()?;
        let mut effects = Vec::with_capacity(placements.len());
        for (i, p) in placements.iter().enumerate() {
            let at = format!("effects[{i}] ({})", p.spec.kind.name());
            p.spec.validate().map_err(|e| within(&at, e))?;
            if !p.at.is_finite() || p.at < 0.0 {
                return Err(invalid(format!(
                    "{at}: at must be a time in seconds, 0 or later (got {})",
                    p.at
                )));
            }
            if let Some(db) = p.volume_db {
                check_gain(db).map_err(|e| within(&at, e))?;
            }
            if let Some(t) = p.track_id {
                timeline.ensure_track_editable(t).map_err(|e| within(&at, e))?;
                let track = timeline.track(t).ok_or(Error::TrackNotFound(t))?;
                if track.kind != StreamKind::Audio {
                    return Err(invalid(format!(
                        "{at}: a sound effect goes on an audio track; {} is not one",
                        track.name
                    )));
                }
            }
            effects.push(PlannedSfx {
                spec: p.spec.resolved(),
                at: p.at,
                volume_db: p.volume_db.unwrap_or(0.0),
                track_id: p.track_id,
            });
        }
        let plan = SfxPlan { effects };
        self.check_sfx_fits(&timeline, &plan)?;
        Ok(plan)
    }

    /// Rehearse putting `plan`'s renders on `timeline` the way [`Project::place_sound_effects`] will,
    /// with the ripple the edit will have (the project's flag, or a [`Project::with_ripple`] override
    /// around the call): stand-in assets of the renders' lengths go through the same placement and the
    /// same overlap check `edit_timeline` makes, so what it refuses is refused with the same words and
    /// with ripple on an effect that only *pushes* what is there passes.
    fn check_sfx_fits(&self, timeline: &Timeline, plan: &SfxPlan) -> Result<()> {
        let assets: Vec<Asset> = plan.effects.iter().map(|e| stand_in(&e.spec)).collect();
        let ids: Vec<Uuid> = plan.effects.iter().map(|_| Uuid::new_v4()).collect();
        let mut after = timeline.clone();
        put_sound_effects(&mut after, &assets, plan, &ids)?;
        if self.ripple_active()? {
            after = after.ripple_lanes(timeline);
        }
        after.check_own_lanes(timeline, &ids)
    }

    /// Render `spec` and describe the result as an importable [`Asset`], *without* `&self`: rendering
    /// is milliseconds, but like every generated sound it happens with the project lock released.
    /// The asset carries its spec (length spelled out) and is named `SFX: Whoosh`.
    pub fn synthesize_sound_effect(spec: &SfxSpec, cancel: &dyn Fn() -> bool) -> Result<Asset> {
        let path = engine::sfx::synthesize(spec, cancel)?;
        let spec = spec.resolved();
        tracing::info!(path = %path.display(), kind = spec.kind.name(), "synthesized sound effect");
        let mut asset = Self::probe_asset_with(&path, cancel)?;
        asset.name = spec.title();
        asset.sound_effect = Some(spec);
        Ok(asset)
    }

    /// Render every effect of `plan` (one asset each, in order; the same spec is rendered and
    /// probed once and is the same asset), without `&self`.
    pub fn synthesize_sound_effects(plan: &SfxPlan, cancel: &dyn Fn() -> bool) -> Result<Vec<Asset>> {
        let mut made: Vec<Asset> = Vec::new();
        let mut assets = Vec::with_capacity(plan.effects.len());
        for effect in &plan.effects {
            if cancel() {
                return Err(Error::Cancelled);
            }
            let known = made.iter().find(|a| a.sound_effect.as_ref() == Some(&effect.spec));
            let asset = match known {
                Some(asset) => asset.clone(),
                None => {
                    let asset = Self::synthesize_sound_effect(&effect.spec, cancel)?;
                    made.push(asset.clone());
                    asset
                }
            };
            assets.push(asset);
        }
        Ok(assets)
    }

    /// Render `spec` for auditioning and say where the WAV is: no edit, no asset. The same audio as the
    /// one placing it would use, written to the cache directory and pruned there, not to the data
    /// directory where the effects of a project live.
    pub fn preview_sound_effect(spec: &SfxSpec, cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
        engine::sfx::preview(spec, cancel)
    }

    /// Land rendered effects: store the assets (the same render is the same asset) and put every clip
    /// on the timeline as **one** `Add sound effects` revision (`Add sound effect` for a single one).
    /// The assets are stored **inside** the edit, so a refused placement (a locked or missing track, a
    /// clip in the way, a hit that cannot land) stores nothing; an agent's edit stages through
    /// `edit_timeline` — the assets land at once, the placement waits. Each effect goes where
    /// [`SfxPlacement`] says: a named track, or the first `FX` lane with room.
    pub fn place_sound_effects(&self, assets: &[Asset], plan: &SfxPlan) -> Result<Vec<SfxPlaced>> {
        if assets.len() != plan.effects.len() {
            return Err(invalid("there must be one rendered asset for each planned effect"));
        }
        if let Some((i, effect)) = plan
            .effects
            .iter()
            .enumerate()
            .find(|(i, e)| assets[*i].sound_effect.as_ref() != Some(&e.spec))
        {
            let at = format!("effects[{i}] ({})", effect.spec.kind.name());
            return Err(invalid(format!("{at}: that asset is not the render of the planned effect")));
        }
        let ids: Vec<Uuid> = plan.effects.iter().map(|_| Uuid::new_v4()).collect();
        self.edit_lanes.borrow_mut().extend(ids.iter().copied());
        let label = if plan.effects.len() == 1 {
            "Add sound effect"
        } else {
            "Add sound effects"
        };
        self.edit_timeline(label, |timeline| {
            // Inside the edit's transaction: if the placement is refused, so are the assets.
            let stored = assets
                .iter()
                .map(|a| self.insert_or_get_asset(a))
                .collect::<Result<Vec<_>>>()?;
            let clips = put_sound_effects(timeline, &stored, plan, &ids)?;
            Ok(clips
                .into_iter()
                .zip(stored)
                .zip(&plan.effects)
                .map(|((clip, asset), effect)| placed(clip, asset, &effect.spec))
                .collect())
        })
    }

    /// Check, render and place `placements` in one go, with `&self` held for the whole of it: for tests.
    /// A caller that shares the project behind a lock uses the three steps and renders with it released.
    #[cfg(test)]
    pub fn add_sound_effects(&self, placements: &[SfxPlacement]) -> Result<Vec<Clip>> {
        let plan = self.sound_effects_inputs(placements)?;
        let assets = Self::synthesize_sound_effects(&plan, &|| false)?;
        Ok(self
            .place_sound_effects(&assets, &plan)?
            .into_iter()
            .map(|p| p.clip)
            .collect())
    }

    /// Check a reshape under the lock: the clip is on an unlocked audio track and its asset was
    /// synthesized as a sound effect, the spec and gain are valid, and the swap is rehearsed on a copy
    /// of the working cut — a longer render that would run into the next clip is refused now (with
    /// ripple on it pushes).
    pub fn update_sound_effect_inputs(&self, clip_id: Uuid, spec: &SfxSpec, volume_db: Option<f64>) -> Result<SfxUpdate> {
        spec.validate()?;
        if let Some(db) = volume_db {
            check_gain(db)?;
        }
        let timeline = self.working_timeline()?;
        let (ti, ci) = timeline.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let track = &timeline.tracks[ti];
        if track.kind != StreamKind::Audio {
            return Err(invalid("only a sound effect on an audio track can be reshaped"));
        }
        timeline.ensure_track_editable(track.id)?;
        let anchor_at = self.effect_anchor_at(&track.clips[ci])?.0;
        let spec = spec.resolved();
        let mut after = timeline.clone();
        replace_effect(&mut after, clip_id, &stand_in(&spec), &spec, volume_db, anchor_at)?;
        if self.ripple_active()? {
            after = after.ripple_lanes(&timeline);
        }
        after.check_own_lanes(&timeline, &[clip_id])?;
        Ok(SfxUpdate {
            clip_id,
            spec,
            volume_db,
            anchor_at,
        })
    }

    /// Where on the timeline the hit point of effect clip `clip` is, and the spec it was made from:
    /// the clip's own mapping of source time (speed, trim and all), not its start. Refused for a clip
    /// whose asset was not synthesized as a sound effect.
    fn effect_anchor_at(&self, clip: &Clip) -> Result<(f64, SfxSpec)> {
        let asset = self.require_asset(clip.asset_id)?;
        let spec = asset.sound_effect.clone().ok_or_else(|| {
            invalid("that clip is not a sound effect: only a clip of a synthesized sound effect can be reshaped")
        })?;
        Ok((clip.source_to_timeline(sfx_anchor_s(&spec, asset.duration)), spec))
    }

    /// Swap the effect clip `update.clip_id` for the render `asset` of the edited spec, as one
    /// `Edit sound effect` revision: the hit point goes back where it was **when the swap is made**
    /// (read from the clip as it stands then), the clip keeps its id, gain, fades, audio effects and
    /// keys, and plays the whole render at normal speed. It goes through `edit_timeline`, so ripple
    /// applies as to any edit and without it a longer render that would run into the next clip is
    /// refused with nothing stored or changed.
    pub fn apply_sound_effect_update(&self, asset: &Asset, update: &SfxUpdate) -> Result<SfxPlaced> {
        if asset.sound_effect.as_ref() != Some(&update.spec) {
            return Err(invalid("that asset is not the render of the edited effect"));
        }
        self.edit_lanes.borrow_mut().push(update.clip_id);
        self.edit_timeline("Edit sound effect", |timeline| {
            let (ti, ci) = timeline.locate(update.clip_id).ok_or(Error::ClipNotFound(update.clip_id))?;
            let anchor_at = self.effect_anchor_at(&timeline.tracks[ti].clips[ci])?.0;
            let stored = self.insert_or_get_asset(asset)?;
            let clip = replace_effect(timeline, update.clip_id, &stored, &update.spec, update.volume_db, anchor_at)?;
            Ok(placed(clip, stored, &update.spec))
        })
    }

    /// Reshape effect clip `clip_id` in one go (render with `&self` held), for tests: see
    /// [`Project::update_sound_effect_inputs`] and [`Project::apply_sound_effect_update`].
    #[cfg(test)]
    pub fn update_sound_effect(&self, clip_id: Uuid, spec: &SfxSpec, volume_db: Option<f64>) -> Result<Clip> {
        let update = self.update_sound_effect_inputs(clip_id, spec, volume_db)?;
        let asset = Self::synthesize_sound_effect(&update.spec, &|| false)?;
        Ok(self.apply_sound_effect_update(&asset, &update)?.clip)
    }

    /// What suggesting sound effects reads, under the lock: the **working timeline** (an agent's staged
    /// cut included, so it sees its own proposal), the assets and the cached analysis of the assets the
    /// timeline uses. Nothing is analysed here; footage that has not been analysed simply suggests less.
    pub fn suggest_sound_effects_inputs(&self) -> Result<SfxSuggestInputs> {
        let timeline = self.working_timeline()?;
        let mut analyses = HashMap::new();
        for clip in timeline.tracks.iter().flat_map(|t| t.clips.iter()) {
            if analyses.contains_key(&clip.asset_id) {
                continue;
            }
            if let Some(analysis) = self.get_analysis(clip.asset_id)? {
                analyses.insert(clip.asset_id, analysis);
            }
        }
        Ok(SfxSuggestInputs {
            assets: self.list_assets()?,
            timeline,
            analyses,
        })
    }

    /// Suggest sound effects for the working cut ([`crate::model::suggest_sound_effects`]): a read, so
    /// no revision. Each suggestion's placement is what `add_sound_effects` takes.
    pub fn suggest_sound_effects(&self, request: &SfxSuggestRequest) -> Result<Vec<SfxSuggestion>> {
        suggest_sound_effects(&self.suggest_sound_effects_inputs()?, request)
    }
}
