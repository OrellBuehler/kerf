//! Editing a speed ramp: what the edit modes do to one, the check on keys handed in, and how a
//! change reads in the diff. The ramp and its time map are `speed_ramp`.
//!
//! Keys are clip-local timeline seconds, so every edit that moves the clip's **head** has to move the
//! ramp with the footage ([`Clip::rebase_ramp`]) or the slow-motion slides off the moment it was written
//! for; a tail move leaves the keys where they are (the new end plays on at the last key's speed, and a
//! key past the end lies in wait for the clip to grow back); a slip keeps them too — the same window
//! moved over the footage still covers the source span the curve was drawn for, so the clip keeps its
//! length.

use super::speed_ramp::ramp_speed_at;
use super::*;

/// Hand-written keys, checked and put in time order: at most [`MAX_SPEED_KEYS`], each a finite time
/// from the clip's start (`0..=`[`MAX_RAMP_TIME`]) and a speed in `MIN_SPEED..=MAX_SPEED`, no two at one
/// time. An empty list is a clear and passes.
pub fn check_speed_ramp(keys: Vec<SpeedKey>) -> Result<Vec<SpeedKey>> {
    if keys.len() > MAX_SPEED_KEYS {
        return Err(Error::InvalidArgument(format!(
            "a speed ramp holds at most {MAX_SPEED_KEYS} keys, got {}",
            keys.len()
        )));
    }
    for (i, k) in keys.iter().enumerate() {
        let n = i + 1;
        if !k.time.is_finite() || !(0.0..=MAX_RAMP_TIME).contains(&k.time) {
            return Err(Error::InvalidArgument(format!(
                "speed ramp key {n}: time must be seconds from the clip's start, 0 to {MAX_RAMP_TIME}, got {}",
                k.time
            )));
        }
        if !k.speed.is_finite() || !(MIN_SPEED..=MAX_SPEED).contains(&k.speed) {
            return Err(Error::InvalidArgument(format!(
                "speed ramp key {n}: speed must be between {MIN_SPEED}× and {MAX_SPEED}× (a magnitude: the clip keeps its direction), got {}",
                k.speed
            )));
        }
    }
    let mut keys: Vec<SpeedKey> = keys.into_iter().map(|k| SpeedKey { time: k.time + 0.0, ..k }).collect();
    keys.sort_by(|a, b| a.time.total_cmp(&b.time));
    if let Some(w) = keys.windows(2).find(|w| w[0].time == w[1].time) {
        return Err(Error::InvalidArgument(format!(
            "two speed ramp keys at {:.3}s — each key needs its own time",
            w[0].time
        )));
    }
    Ok(keys)
}

/// Keys in strictly increasing time again: an edit that shifts them can land two within rounding of
/// each other, and a ramp that is not strictly increasing is not read at all.
fn settle(mut keys: Vec<SpeedKey>) -> Vec<SpeedKey> {
    keys.dedup_by(|next, kept| next.time <= kept.time);
    keys
}

impl Clip {
    /// Re-time the speed ramp after the clip's start moved by `by` timeline seconds, so the slow-motion
    /// stays on the footage it was drawn over (what [`Clip::rebase_animation`] does for the keyframes).
    ///
    /// Shortened from the front (`by > 0`) the keys at or before `by` are gone, so the speed the clip now
    /// opens at is pinned as a key at 0 — taking the easing of the segment the cut fell in, so a hold keeps
    /// holding and a straight run keeps its slope — and later keys shift back. A cut before the first key
    /// pins nothing (the first key's speed already holds there). Pulled earlier (`by < 0`) every key
    /// shifts later, and the new head holds the first key's speed, as the map does before the first key.
    /// A clip without a readable ramp is left alone.
    pub(super) fn rebase_ramp(&mut self, by: f64) {
        let Some(keys) = self.ramp() else { return };
        if by == 0.0 || !by.is_finite() {
            return;
        }
        let rebased: Vec<SpeedKey> = if by > 0.0 {
            let gone = keys.partition_point(|k| k.time <= by);
            let mut out = Vec::with_capacity(keys.len() - gone + 1);
            if gone > 0 {
                out.push(SpeedKey {
                    time: 0.0,
                    speed: ramp_speed_at(keys, by),
                    easing: if gone < keys.len() {
                        keys[gone - 1].easing
                    } else {
                        RampEasing::Linear
                    },
                });
            }
            out.extend(keys[gone..].iter().map(|k| SpeedKey { time: k.time - by, ..*k }));
            out
        } else {
            keys.iter().map(|k| SpeedKey { time: k.time - by, ..*k }).collect()
        };
        self.speed_ramp = settle(rebased);
    }

    /// Bring the ramp along after a trim that wrote the source window directly
    /// (`Project::trim`): the head moved by the footage between the two in-points, read off the clip as
    /// it was (`was`), and the ramp moves with it.
    pub(crate) fn follow_head(&mut self, was: &Clip) {
        if !was.is_ramped() {
            return;
        }
        let moved = if was.is_reversed() {
            was.source_out - self.source_out
        } else {
            self.source_in - was.source_in
        };
        if moved != 0.0 {
            self.rebase_ramp(was.local_at_along(moved));
        }
    }
}

impl Timeline {
    /// Give a clip a speed ramp, or clear it with no keys: the source window stays, so the clip's length
    /// changes (the ramp decides how fast the window plays). The clip's direction stays its `speed`'s
    /// sign. Not carried to linked clips — see [`Timeline::set_speed_ramp_linked`]. Errors on bad keys
    /// ([`check_speed_ramp`]), a locked track, or a clear of a ramp that is not there. Returns the clip.
    pub fn set_speed_ramp(&mut self, clip_id: Uuid, keys: Vec<SpeedKey>) -> Result<Clip> {
        let keys = check_speed_ramp(keys)?;
        self.ensure_editable(clip_id)?;
        let (ti, ci) = self.locate(clip_id).ok_or(Error::ClipNotFound(clip_id))?;
        let clip = &mut self.tracks[ti].clips[ci];
        if keys.is_empty() && clip.speed_ramp.is_empty() {
            return Err(Error::InvalidArgument("the clip has no speed ramp to clear".to_string()));
        }
        clip.speed_ramp = keys;
        Ok(clip.clone())
    }
}

/// "3 keys, 0.30×–2.00×".
fn describe(keys: &[SpeedKey]) -> String {
    let (lo, hi) = keys
        .iter()
        .fold((f64::INFINITY, 0.0_f64), |(lo, hi), k| (lo.min(k.speed), hi.max(k.speed)));
    format!(
        "{} key{}, {lo:.2}×–{hi:.2}×",
        keys.len(),
        if keys.len() == 1 { "" } else { "s" }
    )
}

/// How a clip's speed ramp changed, for the diff: set, cleared, a different number of keys, keys moved
/// in time (a head trim does this) or keys with other speeds or easings.
pub(super) fn ramp_changes(before: &Clip, after: &Clip) -> Option<String> {
    let (a, b) = (&before.speed_ramp, &after.speed_ramp);
    let same = a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| !num_changed(x.time, y.time) && !num_changed(x.speed, y.speed) && x.easing == y.easing);
    if same {
        return None;
    }
    Some(if b.is_empty() {
        "speed ramp cleared".to_string()
    } else if a.is_empty() {
        format!("speed ramp {}", describe(b))
    } else if a.len() != b.len() {
        format!("speed ramp {} → {}", a.len(), describe(b))
    } else if a.iter().zip(b).any(|(x, y)| num_changed(x.time, y.time)) {
        "speed ramp retimed".to_string()
    } else {
        "speed ramp changed".to_string()
    })
}
