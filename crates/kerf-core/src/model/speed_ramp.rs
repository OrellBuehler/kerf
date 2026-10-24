//! Speed ramps: a clip whose playback rate changes over its length.
//!
//! [`Clip::speed`] is one rate for the whole clip. A ramp ([`Clip::speed_ramp`]) is a list of
//! [`SpeedKey`]s — a **clip-local timeline** time, a speed magnitude and the [`RampEasing`] of the
//! segment that leaves it — and while it holds a key it decides the rate everywhere: before the
//! first key the clip runs at the first key's speed, after the last at the last's, and between two
//! keys the speed moves in a straight line *in timeline time* (`Linear`) or stays at the left key's
//! (`Hold`). The **direction** is still `speed`'s sign; its magnitude is not read while a ramp is set.
//!
//! **Keys are in timeline time, like keyframes.** So a ramp reads the way an editor draws it ("slow
//! to 0.3× over half a second"), a head trim rebases it as it does the animation, and a slip — the
//! same window moved over the footage — keeps the clip's length, since the area under the curve over
//! the clip is still its source span.
//!
//! **One map.** What a ramp does is the pair [`Clip::along`] (timeline seconds into the clip → source
//! seconds travelled) and its inverse [`Clip::local_at_along`]; [`Clip::duration`],
//! [`Clip::source_to_timeline`] and [`Clip::timeline_to_source`] read them, and so must anything
//! that converts between the two clocks. A linear segment's distance is a trapezoid and its inverse
//! the root of a quadratic, both closed forms, so the export's per-frame expression can be exactly
//! this arithmetic. A clip without a ramp keeps the plain `* speed` / `/ speed` it always had, to the
//! last bit, so no existing graph moves.
//!
//! A ramp that is not well formed (a non-finite number, times out of order, more than
//! [`MAX_SPEED_KEYS`]) is not read: the clip plays at its constant speed, as it did before ramps.

use super::*;

/// The most keys one clip's ramp may hold: the export nests one `if` per key in a balanced tree.
pub const MAX_SPEED_KEYS: usize = 16;

/// How the speed moves from one key to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RampEasing {
    /// The speed changes at a constant rate over timeline time.
    #[default]
    Linear,
    /// The speed stays at this key's until the next key, then jumps.
    Hold,
}

impl RampEasing {
    pub fn is_linear(&self) -> bool {
        matches!(self, RampEasing::Linear)
    }
}

/// One key of a speed ramp.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpeedKey {
    /// Clip-local timeline seconds (0 = the clip's start).
    pub time: f64,
    /// Speed magnitude at `time`, `MIN_SPEED..=MAX_SPEED` (the clip's direction is `Clip::speed`'s sign).
    pub speed: f64,
    /// How the speed moves from here to the next key.
    #[serde(default, skip_serializing_if = "RampEasing::is_linear")]
    pub easing: RampEasing,
}

impl SpeedKey {
    pub fn new(time: f64, speed: f64) -> Self {
        Self {
            time,
            speed,
            easing: RampEasing::Linear,
        }
    }
}

/// Whether `keys` is a ramp the map can read: one to [`MAX_SPEED_KEYS`] keys, finite strictly
/// increasing times, finite speeds in `MIN_SPEED..=MAX_SPEED`.
pub fn ramp_is_valid(keys: &[SpeedKey]) -> bool {
    !keys.is_empty()
        && keys.len() <= MAX_SPEED_KEYS
        && keys
            .iter()
            .all(|k| k.time.is_finite() && k.speed.is_finite() && (MIN_SPEED..=MAX_SPEED).contains(&k.speed))
        && keys.windows(2).all(|w| w[0].time < w[1].time)
}

/// Speed magnitude of a valid ramp at clip-local time `t`.
pub fn ramp_speed_at(keys: &[SpeedKey], t: f64) -> f64 {
    let first = keys[0];
    let last = keys[keys.len() - 1];
    if t <= first.time {
        return first.speed;
    }
    if t >= last.time {
        return last.speed;
    }
    let i = keys.partition_point(|k| k.time <= t) - 1;
    let (a, b) = (keys[i], keys[i + 1]);
    match a.easing {
        RampEasing::Hold => a.speed,
        RampEasing::Linear => a.speed + (b.speed - a.speed) * (t - a.time) / (b.time - a.time),
    }
}

/// Source seconds a segment leaving `a` covers in its first `dt` timeline seconds (`dt` within it).
fn segment_distance(a: SpeedKey, b: SpeedKey, dt: f64) -> f64 {
    match a.easing {
        RampEasing::Hold => a.speed * dt,
        RampEasing::Linear => a.speed * dt + (b.speed - a.speed) * dt * dt / (2.0 * (b.time - a.time)),
    }
}

/// Timeline seconds a segment leaving `a` takes to cover `r` source seconds (`r` within it).
fn segment_time(a: SpeedKey, b: SpeedKey, r: f64) -> f64 {
    match a.easing {
        RampEasing::Hold => r / a.speed,
        RampEasing::Linear => {
            // `k/2 dt² + v dt = r`, solved in the form that does not cancel when `k` is near 0.
            let k = (b.speed - a.speed) / (b.time - a.time);
            let disc = (a.speed * a.speed + 2.0 * k * r).max(0.0);
            2.0 * r / (a.speed + disc.sqrt())
        }
    }
}

/// Source seconds a valid ramp has covered at clip-local time `t`, measured from its first key's
/// time (negative before it).
fn distance_from_first(keys: &[SpeedKey], t: f64) -> f64 {
    let first = keys[0];
    if t <= first.time {
        return (t - first.time) * first.speed;
    }
    let mut acc = 0.0;
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.time {
            return acc + segment_distance(a, b, t - a.time);
        }
        acc += segment_distance(a, b, b.time - a.time);
    }
    let last = keys[keys.len() - 1];
    acc + (t - last.time) * last.speed
}

/// The inverse of [`distance_from_first`]: the clip-local time at which a valid ramp has covered
/// `d` source seconds since its first key.
fn time_from_first(keys: &[SpeedKey], d: f64) -> f64 {
    let first = keys[0];
    if d <= 0.0 {
        return first.time + d / first.speed;
    }
    let mut acc = 0.0;
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        let span = segment_distance(a, b, b.time - a.time);
        if d <= acc + span {
            return a.time + segment_time(a, b, d - acc).min(b.time - a.time);
        }
        acc += span;
    }
    let last = keys[keys.len() - 1];
    last.time + (d - acc) / last.speed
}

/// Source seconds a valid ramp covers from the clip's start to clip-local time `t` (negative for
/// `t < 0`, extrapolated at the first key's speed).
pub fn ramp_along(keys: &[SpeedKey], t: f64) -> f64 {
    distance_from_first(keys, t) - distance_from_first(keys, 0.0)
}

/// The clip-local time at which a valid ramp has covered `along` source seconds from the clip's
/// start — the inverse of [`ramp_along`].
pub fn ramp_local_at(keys: &[SpeedKey], along: f64) -> f64 {
    time_from_first(keys, along + distance_from_first(keys, 0.0))
}

impl Clip {
    /// The clip's ramp, when it holds one the map reads ([`ramp_is_valid`]).
    pub fn ramp(&self) -> Option<&[SpeedKey]> {
        ramp_is_valid(&self.speed_ramp).then_some(self.speed_ramp.as_slice())
    }

    /// Whether the clip's rate changes over its length.
    pub fn is_ramped(&self) -> bool {
        self.ramp().is_some()
    }

    /// Speed magnitude at clip-local time `local`: the ramp's there, else the constant one.
    pub fn speed_at(&self, local: f64) -> f64 {
        match self.ramp() {
            Some(keys) => ramp_speed_at(keys, local),
            None => self.speed_mag(),
        }
    }

    /// Source seconds the clip travels (in its playing direction) from its start to clip-local time
    /// `local`. Not clamped to the clip.
    pub fn along(&self, local: f64) -> f64 {
        match self.ramp() {
            Some(keys) => ramp_along(keys, local),
            None => local * self.speed_mag(),
        }
    }

    /// The clip-local time at which the clip has travelled `along` source seconds from its start —
    /// the inverse of [`Clip::along`]. Not clamped to the clip.
    pub fn local_at_along(&self, along: f64) -> f64 {
        match self.ramp() {
            Some(keys) => ramp_local_at(keys, along),
            None => along / self.speed_mag(),
        }
    }

    /// The clip's source span over its timeline length: its average speed magnitude.
    pub fn mean_speed(&self) -> f64 {
        match self.ramp() {
            Some(_) => {
                let d = self.duration();
                if d > 0.0 {
                    self.source_duration() / d
                } else {
                    ramp_speed_at(&self.speed_ramp, 0.0)
                }
            }
            None => self.speed_mag(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn ramped(keys: Vec<SpeedKey>, source_len: f64) -> Clip {
        let mut c = Clip::new(Uuid::nil(), 10.0, 10.0 + source_len, 5.0);
        c.speed_ramp = keys;
        c
    }

    #[test]
    fn no_ramp_keeps_the_constant_arithmetic_bit_for_bit() {
        let mut c = Clip::new(Uuid::nil(), 1.3, 7.1, 2.0);
        c.speed = 1.7;
        assert_eq!(c.duration(), (7.1 - 1.3) / 1.7);
        assert_eq!(c.along(0.9), 0.9 * 1.7);
        assert_eq!(c.local_at_along(0.9), 0.9 / 1.7);
        assert_eq!(c.timeline_to_source(3.0), 1.3 + (3.0 - 2.0) * 1.7);
        assert_eq!(c.source_to_timeline(4.0), 2.0 + (4.0 - 1.3) / 1.7);
    }

    #[test]
    fn a_single_key_is_a_constant_speed() {
        let c = ramped(vec![SpeedKey::new(3.0, 2.0)], 8.0);
        assert!(close(c.duration(), 4.0));
        assert!(close(c.along(1.0), 2.0));
        assert!(close(c.speed_at(-1.0), 2.0));
    }

    #[test]
    fn a_linear_segment_covers_a_trapezoid() {
        // 1× to 3× over 2 s covers 4 source seconds, then 3× after.
        let keys = vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 3.0)];
        let c = ramped(keys, 4.0 + 6.0);
        assert!(close(c.along(2.0), 4.0));
        assert!(close(c.along(1.0), 1.0 + 0.5));
        assert!(close(c.speed_at(1.0), 2.0));
        assert!(close(c.duration(), 2.0 + 2.0));
        assert!(close(c.timeline_end(), 5.0 + 4.0));
    }

    #[test]
    fn a_hold_jumps_at_the_next_key() {
        let mut a = SpeedKey::new(0.0, 0.5);
        a.easing = RampEasing::Hold;
        let c = ramped(vec![a, SpeedKey::new(2.0, 2.0)], 1.0 + 4.0);
        assert!(close(c.speed_at(1.999), 0.5));
        assert!(close(c.along(2.0), 1.0));
        assert!(close(c.duration(), 4.0));
    }

    #[test]
    fn the_map_inverts_everywhere_including_outside_the_keys() {
        let keys = vec![
            SpeedKey::new(0.5, 1.0),
            SpeedKey::new(1.0, 0.25),
            SpeedKey {
                time: 2.0,
                speed: 0.25,
                easing: RampEasing::Hold,
            },
            SpeedKey::new(2.5, 4.0),
            SpeedKey::new(3.0, 1.0),
        ];
        let c = ramped(keys, 6.0);
        for i in -20..=80 {
            let t = i as f64 * 0.05;
            assert!(close(c.local_at_along(c.along(t)), t), "t = {t}");
        }
        for i in 0..=60 {
            let s = 10.0 + i as f64 * 0.1;
            assert!(close(c.timeline_to_source(c.source_to_timeline(s)), s), "s = {s}");
        }
    }

    #[test]
    fn a_reversed_ramp_plays_the_window_backwards_on_the_same_curve() {
        let keys = vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(2.0, 3.0)];
        let mut c = ramped(keys, 10.0);
        let forward = c.duration();
        c.speed = -1.0;
        assert!(close(c.duration(), forward));
        assert!(close(c.timeline_to_source(5.0), 20.0));
        assert!(close(c.timeline_to_source(5.0 + 2.0), 20.0 - 4.0));
    }

    #[test]
    fn a_malformed_ramp_is_not_read() {
        let base = Clip::new(Uuid::nil(), 0.0, 4.0, 0.0);
        for keys in [
            vec![SpeedKey::new(1.0, 2.0), SpeedKey::new(1.0, 3.0)],
            vec![SpeedKey::new(2.0, 2.0), SpeedKey::new(1.0, 3.0)],
            vec![SpeedKey::new(f64::NAN, 2.0)],
            vec![SpeedKey::new(0.0, 0.0)],
            vec![SpeedKey::new(0.0, MAX_SPEED * 2.0)],
            (0..=MAX_SPEED_KEYS).map(|i| SpeedKey::new(i as f64, 1.0)).collect(),
        ] {
            let mut c = base.clone();
            c.speed_ramp = keys;
            assert!(!c.is_ramped());
            assert_eq!(c.duration(), 4.0);
        }
    }
}
