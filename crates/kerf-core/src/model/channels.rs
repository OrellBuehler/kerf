//! Per-property animation channels (B5b): any one number of a clip can have keys of its own.
//!
//! [`Clip::keyframes`] animates the five numbers of a [`Transform`] *together*: every key
//! carries all of them, so the opacity cannot be keyed at other moments than the zoom. A
//! [`PropertyTrack`] is one number (a [`Property`]) with keys of its own — a time, a value
//! and the [`Easing`] of the segment that leaves it — and can be a transform number (scale,
//! position, rotation, opacity), a colour number (brightness, contrast, saturation, gamma,
//! temperature) or the clip's volume.
//!
//! **One resolver.** What a property does is [`Clip::property_keys`], and everything else
//! reads that: the eased polyline ([`Clip::property_curve`]) the export writes into its
//! per-frame expressions, the sample ([`Clip::property_at`]) the still, the preview and
//! the GPU plan take, and the edits. A property with a track uses it (even an empty one: "static,
//! whatever the bundle says"); a transform property without one falls back to the legacy
//! bundle ([`Clip::keyframes`]); anything else is its static value. So a project saved before
//! channels existed has none, reads through the bundle exactly as it always did and renders
//! the graph it always did; and the first per-property write *detaches* just that property from
//! the bundle (its bundle keys are copied into a track, then edited), leaving the others where
//! they were. Nothing is converted behind anyone's back.
//!
//! Colour and volume values are held to the range the static setters accept
//! ([`Property::clamp`], applied to a track's keys on read — a hand-edited file never reaches
//! an `eq` or `volume` filter with a value it refuses), which is also what makes the export's
//! `1 + 0.3 * temperature` gamma exactly the model's.

use super::*;

/// The most a clip's volume channel may reach (the master bus's own ceiling, +12 dB).
pub const MAX_CHANNEL_VOLUME: f64 = MASTER_MAX_VOLUME;

/// One number of a clip that can be keyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Property {
    Scale,
    PosX,
    PosY,
    Rotation,
    Opacity,
    Brightness,
    Contrast,
    Saturation,
    Gamma,
    Temperature,
    /// The clip's own gain, linear (`Clip::volume`).
    Volume,
}

impl Property {
    pub const ALL: [Property; 11] = [
        Property::Scale,
        Property::PosX,
        Property::PosY,
        Property::Rotation,
        Property::Opacity,
        Property::Brightness,
        Property::Contrast,
        Property::Saturation,
        Property::Gamma,
        Property::Temperature,
        Property::Volume,
    ];

    /// The five numbers of a [`Transform`] that [`Clip::keyframes`] animates together.
    pub const TRANSFORM: [Property; 5] = [
        Property::Scale,
        Property::PosX,
        Property::PosY,
        Property::Rotation,
        Property::Opacity,
    ];

    /// The five numbers of a [`Color`].
    pub const COLOR: [Property; 5] = [
        Property::Brightness,
        Property::Contrast,
        Property::Saturation,
        Property::Gamma,
        Property::Temperature,
    ];

    /// The wire name (the JSON the model, the Tauri commands and the MCP tools share).
    pub fn as_str(self) -> &'static str {
        match self {
            Property::Scale => "scale",
            Property::PosX => "pos_x",
            Property::PosY => "pos_y",
            Property::Rotation => "rotation",
            Property::Opacity => "opacity",
            Property::Brightness => "brightness",
            Property::Contrast => "contrast",
            Property::Saturation => "saturation",
            Property::Gamma => "gamma",
            Property::Temperature => "temperature",
            Property::Volume => "volume",
        }
    }

    /// Every wire name, for an "expected one of" message.
    pub fn wire_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|p| p.as_str()).collect()
    }

    /// The property a wire name spells.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    pub fn is_transform(self) -> bool {
        Self::TRANSFORM.contains(&self)
    }

    pub fn is_color(self) -> bool {
        Self::COLOR.contains(&self)
    }

    /// The range the static setter accepts (`None` is unbounded on that side) — what a key's
    /// value is checked against when it is written and held to when it is read.
    pub fn range(self) -> (Option<f64>, Option<f64>) {
        match self {
            Property::Scale => (Some(0.0), None),
            Property::PosX | Property::PosY | Property::Rotation => (None, None),
            Property::Opacity => (Some(0.0), Some(1.0)),
            Property::Brightness | Property::Temperature => (Some(-1.0), Some(1.0)),
            Property::Contrast => (Some(0.0), Some(4.0)),
            Property::Saturation => (Some(0.0), Some(3.0)),
            Property::Gamma => (Some(0.1), Some(10.0)),
            Property::Volume => (Some(0.0), Some(MAX_CHANNEL_VOLUME)),
        }
    }

    /// Whether `value` is a legal key value (a finite number inside [`Property::range`]; a scale
    /// is strictly positive).
    pub fn check(self, value: f64) -> Result<()> {
        let (lo, hi) = self.range();
        let bad = !value.is_finite()
            || (self == Property::Scale && value <= 0.0)
            || lo.is_some_and(|lo| value < lo)
            || hi.is_some_and(|hi| value > hi);
        if bad {
            let range = match (lo, hi) {
                (Some(lo), Some(hi)) => format!("within {lo}..={hi}"),
                (Some(_), None) => "a finite value > 0".to_string(),
                _ => "finite".to_string(),
            };
            return Err(Error::InvalidArgument(format!(
                "{} keyframe values must be {range} (got {value})",
                self.as_str()
            )));
        }
        Ok(())
    }

    /// `value` brought into range (a number that is not one becomes the property's neutral
    /// value): what a stored key is read as, so a hand-edited file cannot hand a filter a value
    /// it refuses.
    pub fn clamp(self, value: f64) -> f64 {
        if !value.is_finite() {
            return self.neutral();
        }
        let (lo, hi) = self.range();
        let value = if self == Property::Scale {
            value.max(MIN_CHANNEL_SCALE)
        } else {
            value
        };
        value.max(lo.unwrap_or(f64::NEG_INFINITY)).min(hi.unwrap_or(f64::INFINITY))
    }

    /// The value that changes nothing.
    pub fn neutral(self) -> f64 {
        match self {
            Property::Scale
            | Property::Opacity
            | Property::Contrast
            | Property::Saturation
            | Property::Gamma
            | Property::Volume => 1.0,
            _ => 0.0,
        }
    }

    /// A short name for a sentence ("opacity keyframes 2 → 3").
    pub fn label(self) -> &'static str {
        match self {
            Property::PosX => "position x",
            Property::PosY => "position y",
            other => other.as_str(),
        }
    }
}

/// A scale smaller than this is read as this (a scale of 0 is "unset" to FFmpeg's `scale`).
const MIN_CHANNEL_SCALE: f64 = 1e-6;

/// One key of a [`PropertyTrack`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PropertyKey {
    /// Seconds from the clip's start (on the clip's own timeline, so speed is already in it).
    pub time: f64,
    pub value: f64,
    /// The shape of the segment from this key to the next ([`Easing`]); omitted when linear.
    #[serde(default, skip_serializing_if = "Easing::is_linear")]
    pub easing: Easing,
}

impl PropertyKey {
    pub fn new(time: f64, value: f64) -> Self {
        Self {
            time,
            value,
            easing: Easing::Linear,
        }
    }

    fn triple(&self) -> (f64, f64, Easing) {
        (self.time, self.value, self.easing)
    }
}

/// The keys of one [`Property`] of a clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PropertyTrack {
    pub prop: Property,
    #[serde(default)]
    pub keys: Vec<PropertyKey>,
}

/// The value of a bundle key's `prop` (a transform property).
fn bundle_value(k: &Keyframe, prop: Property) -> f64 {
    match prop {
        Property::Scale => k.scale,
        Property::PosX => k.pos_x,
        Property::PosY => k.pos_y,
        Property::Rotation => k.rotation,
        Property::Opacity => k.opacity,
        _ => prop.neutral(),
    }
}

fn by_time(keys: &mut [PropertyKey]) {
    keys.sort_by(|a, b| a.time.total_cmp(&b.time));
}

/// The polyline `keys` (sorted by time) are drawn as ([`eased_points`]).
pub fn key_polyline(keys: &[PropertyKey]) -> Vec<(f64, f64)> {
    eased_points(&keys.iter().map(PropertyKey::triple).collect::<Vec<_>>())
}

/// `keys` (sorted, in range) re-timed after the clip's start moved `by` seconds **later**: the
/// pose the head now opens on is pinned as a key at 0, the keys after it shift back, and a cut
/// inside an eased segment keeps the rest of it exactly (a hold keeps holding; a curve's
/// remaining pieces become plain keys — the curve *is* those pieces, see [`Easing`]).
/// [`Clip::rebase_animation`] does the same to a transform bundle.
fn rebase_head(keys: &[PropertyKey], by: f64) -> Vec<PropertyKey> {
    let Some(pose) = interpolate(&key_polyline(keys), by) else {
        return Vec::new();
    };
    let mut out = vec![PropertyKey::new(0.0, pose)];
    let segment = keys
        .windows(2)
        .find(|w| w[0].time <= by && by < w[1].time && w[1].time - w[0].time >= 1e-9);
    if let Some(&[a, b]) = segment {
        match a.easing {
            Easing::Linear => {}
            Easing::Hold => out[0].easing = Easing::Hold,
            curved => {
                for j in 1..EASE_STEPS {
                    let u = j as f64 / EASE_STEPS as f64;
                    let at = a.time + (b.time - a.time) * u;
                    if at > by {
                        out.push(PropertyKey::new(at - by, a.value + (b.value - a.value) * curved.curve(u)));
                    }
                }
            }
        }
    }
    out.extend(
        keys.iter()
            .filter(|k| k.time > by)
            .map(|k| PropertyKey { time: k.time - by, ..*k }),
    );
    out
}

/// `key` put into `keys` ([`Clip::insert_keyframe`]'s rule): a key within a microsecond of one
/// replaces it and keeps how that one leaves; one inside a segment splits it
/// ([`Easing::split`]), so a hold stays held and a curve stays the same curve.
fn upsert(keys: &mut Vec<PropertyKey>, mut key: PropertyKey) {
    const SAME: f64 = 1e-6;
    by_time(keys);
    if let Some(old) = keys.iter().find(|k| (k.time - key.time).abs() <= SAME) {
        key.easing = old.easing;
    } else if let Some(i) = keys.windows(2).position(|w| w[0].time < key.time && key.time < w[1].time) {
        let u = (key.time - keys[i].time) / (keys[i + 1].time - keys[i].time);
        let (before, after) = keys[i].easing.split(u);
        key.easing = after;
        keys[i].easing = before;
    }
    keys.retain(|k| (k.time - key.time).abs() > SAME);
    keys.push(key);
    by_time(keys);
}

impl Clip {
    /// The track of `prop`, if the clip has one.
    pub fn channel(&self, prop: Property) -> Option<&PropertyTrack> {
        self.channels.iter().find(|c| c.prop == prop)
    }

    /// The keys that drive `prop`, sorted by time: the property's own track (values held to
    /// [`Property::range`]), else — for a transform property — the legacy bundle's keys
    /// ([`Clip::keyframes`], exactly as stored), else none. The one answer every reader takes.
    pub fn property_keys(&self, prop: Property) -> Vec<PropertyKey> {
        if let Some(track) = self.channel(prop) {
            let mut keys: Vec<PropertyKey> = track
                .keys
                .iter()
                .map(|k| PropertyKey {
                    value: prop.clamp(k.value),
                    ..*k
                })
                .collect();
            by_time(&mut keys);
            return keys;
        }
        if prop.is_transform() {
            return self
                .sorted_keyframes()
                .iter()
                .map(|k| PropertyKey {
                    time: k.time,
                    value: bundle_value(k, prop),
                    easing: k.easing,
                })
                .collect();
        }
        Vec::new()
    }

    /// Whether `prop` has any key.
    pub fn is_keyed(&self, prop: Property) -> bool {
        match self.channel(prop) {
            Some(track) => !track.keys.is_empty(),
            None => prop.is_transform() && !self.keyframes.is_empty(),
        }
    }

    /// The polyline every renderer draws for `prop` ([`eased_points`] over
    /// [`Clip::property_keys`]); empty when the property is not keyed.
    pub fn property_curve(&self, prop: Property) -> Vec<(f64, f64)> {
        key_polyline(&self.property_keys(prop))
    }

    /// The value `prop` is written as when it is not keyed.
    pub fn static_value(&self, prop: Property) -> f64 {
        match prop {
            Property::Scale => self.transform.scale,
            Property::PosX => self.transform.pos_x,
            Property::PosY => self.transform.pos_y,
            Property::Rotation => self.transform.rotation,
            Property::Opacity => self.transform.opacity,
            Property::Brightness => self.color.brightness,
            Property::Contrast => self.color.contrast,
            Property::Saturation => self.color.saturation,
            Property::Gamma => self.color.gamma,
            Property::Temperature => self.color.temperature,
            Property::Volume => f64::from(self.volume),
        }
    }

    /// `prop` at `local` seconds from the clip's start: its curve, else its static value.
    pub fn property_at(&self, prop: Property, local: f64) -> f64 {
        interpolate(&self.property_curve(prop), local).unwrap_or_else(|| self.static_value(prop))
    }

    /// Whether any colour number is keyed (so the clip's `eq` is written per frame).
    pub fn color_animated(&self) -> bool {
        Property::COLOR.iter().any(|p| self.is_keyed(*p))
    }

    /// Whether the clip's volume is keyed.
    pub fn volume_animated(&self) -> bool {
        self.is_keyed(Property::Volume)
    }

    /// The (possibly animated) colour at `local` seconds from the clip's start. Used by the
    /// still / preview path and the GPU plan, which cannot evaluate the export's per-frame
    /// expressions.
    pub fn color_at(&self, local: f64) -> Color {
        if !self.color_animated() {
            return self.color;
        }
        Color {
            brightness: self.property_at(Property::Brightness, local),
            contrast: self.property_at(Property::Contrast, local),
            saturation: self.property_at(Property::Saturation, local),
            gamma: self.property_at(Property::Gamma, local),
            temperature: self.property_at(Property::Temperature, local),
        }
    }

    /// The clip's gain (linear) at `local` seconds from its start, fades and the track fader
    /// not included.
    pub fn volume_at(&self, local: f64) -> f64 {
        self.property_at(Property::Volume, local)
    }

    // ---- edits ------------------------------------------------------------------

    /// Give `prop` a track to edit: the one it has, else a new one holding what drives it
    /// now (a transform property's bundle keys), so that taking a property over from the bundle
    /// changes nothing about how it moves.
    fn channel_mut(&mut self, prop: Property) -> &mut PropertyTrack {
        let at = match self.channels.iter().position(|c| c.prop == prop) {
            Some(i) => i,
            None => {
                let keys = self.property_keys(prop);
                self.channels.push(PropertyTrack { prop, keys });
                self.channels.len() - 1
            }
        };
        &mut self.channels[at]
    }

    /// Replace `prop`'s keys (sorted here). No keys leaves the property static — and if it was
    /// driven by the bundle, an empty track is kept to say so (it would otherwise fall back to
    /// the bundle).
    pub fn set_property_keys(&mut self, prop: Property, mut keys: Vec<PropertyKey>) {
        by_time(&mut keys);
        match self.channels.iter().position(|c| c.prop == prop) {
            Some(i) => self.channels[i].keys = keys,
            None => self.channels.push(PropertyTrack { prop, keys }),
        }
        self.prune_channels();
    }

    /// Drop the tracks that say nothing: an empty one, unless it is holding a property off the
    /// bundle.
    pub fn prune_channels(&mut self) {
        let bundled = !self.keyframes.is_empty();
        self.channels
            .retain(|c| !c.keys.is_empty() || (bundled && c.prop.is_transform()));
        self.channels.sort_by_key(|c| c.prop);
    }

    /// Put a key into `prop`'s animation ([`upsert`]).
    pub fn insert_property_key(&mut self, prop: Property, key: PropertyKey) {
        let track = self.channel_mut(prop);
        upsert(&mut track.keys, key);
        self.prune_channels();
    }

    /// Set the easing of the segment leaving `prop`'s key at `time` (the key nearest it within
    /// a millisecond). `false` when there is no such key.
    pub fn set_property_easing(&mut self, prop: Property, time: f64, easing: Easing) -> bool {
        if !self.property_keys(prop).iter().any(|k| (k.time - time).abs() <= 1e-3) {
            return false;
        }
        let track = self.channel_mut(prop);
        match track
            .keys
            .iter_mut()
            .filter(|k| (k.time - time).abs() <= 1e-3)
            .min_by(|a, b| (a.time - time).abs().total_cmp(&(b.time - time).abs()))
        {
            Some(key) => {
                key.easing = easing;
                true
            }
            None => false,
        }
    }

    /// Back to the static transform: the bundle and every transform property's track go.
    pub fn clear_transform_animation(&mut self) {
        self.keyframes.clear();
        self.channels.retain(|c| !c.prop.is_transform());
    }

    /// The channels' share of [`Clip::rebase_animation`]: the start moved `by` seconds later
    /// (the head is cut: [`rebase_head`]) or earlier (every key shifts later).
    pub(super) fn rebase_channels(&mut self, by: f64) {
        let tracks: Vec<Property> = self.channels.iter().map(|c| c.prop).collect();
        for prop in tracks {
            let keys = self.property_keys(prop);
            let rebased = if by > 0.0 {
                rebase_head(&keys, by)
            } else {
                keys.into_iter().map(|k| PropertyKey { time: k.time - by, ..k }).collect()
            };
            if let Some(track) = self.channels.iter_mut().find(|c| c.prop == prop) {
                track.keys = rebased;
            }
        }
    }

    /// The keys of `props` (every keyed property when empty) as they would be on a clip whose
    /// start is `offset` seconds *later* than this one's: shifted by it, or — a negative offset
    /// — with the head cut off and the pose it opens on pinned at 0.
    pub fn property_keys_shifted(&self, props: &[Property], offset: f64) -> Vec<PropertyTrack> {
        let props: Vec<Property> = if props.is_empty() {
            Property::ALL.into_iter().filter(|p| self.is_keyed(*p)).collect()
        } else {
            props.to_vec()
        };
        props
            .into_iter()
            .map(|prop| {
                let keys = self.property_keys(prop);
                let keys = if keys.is_empty() {
                    keys
                } else if offset < 0.0 {
                    rebase_head(&keys, -offset)
                } else {
                    keys.into_iter()
                        .map(|k| PropertyKey {
                            time: k.time + offset,
                            ..k
                        })
                        .collect()
                };
                PropertyTrack { prop, keys }
            })
            .collect()
    }
}

/// What differs between the channels of two versions of a clip, one phrase per property
/// that moved (the bundle is [`Timeline::diff`]'s own).
pub(super) fn channel_changes(before: &Clip, after: &Clip) -> Vec<String> {
    let mut parts = Vec::new();
    for prop in Property::ALL {
        let (a, b) = (before.channel(prop), after.channel(prop));
        if a == b {
            continue;
        }
        let (a, b) = (a.map_or(&[][..], |t| &t.keys[..]), b.map_or(&[][..], |t| &t.keys[..]));
        let label = prop.label();
        if a.len() != b.len() {
            parts.push(format!("{label} keyframes {} → {}", a.len(), b.len()));
            continue;
        }
        let plain = |k: &PropertyKey| PropertyKey {
            easing: Easing::Linear,
            ..*k
        };
        if a.iter().zip(b).any(|(x, y)| plain(x) != plain(y)) {
            parts.push(format!("{label} keyframes changed"));
        }
        let eased = a.iter().zip(b).filter(|(x, y)| x.easing != y.easing).count();
        if eased > 0 {
            parts.push(format!(
                "easing changed on {eased} {label} keyframe{}",
                if eased == 1 { "" } else { "s" }
            ));
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> Clip {
        Clip::new(Uuid::new_v4(), 0.0, 10.0, 0.0)
    }

    fn key(time: f64, value: f64, easing: Easing) -> PropertyKey {
        PropertyKey { time, value, easing }
    }

    fn bundle_key(time: f64, scale: f64, opacity: f64, easing: Easing) -> Keyframe {
        Keyframe {
            time,
            scale,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity,
            easing,
        }
    }

    /// Every easing the editor offers plus an S that turns back.
    fn easings() -> [Easing; 6] {
        [
            Easing::Linear,
            Easing::Hold,
            Easing::EaseIn,
            Easing::EaseOut,
            Easing::EaseInOut,
            Easing::Bezier {
                x1: 0.2,
                y1: 0.9,
                x2: 0.3,
                y2: 0.1,
            },
        ]
    }

    #[test]
    fn a_project_saved_before_channels_has_none_and_writes_none() {
        let json = serde_json::to_value(clip()).unwrap();
        assert!(json.get("channels").is_none(), "{json}");
        let old: Clip = serde_json::from_value(json).unwrap();
        assert!(old.channels.is_empty());

        let mut c = clip();
        c.set_property_keys(
            Property::Brightness,
            vec![key(0.0, 0.0, Easing::EaseIn), key(2.0, 0.5, Easing::Linear)],
        );
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""channels":[{"prop":"brightness","keys":[{"time":0.0,"value":0.0,"easing":"ease_in"},{"time":2.0,"value":0.5}]}]"#), "{json}");
        let back: Clip = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channels, c.channels);
        // A key without an easing is linear, and a track without keys is an empty one.
        let bare: PropertyTrack = serde_json::from_str(r#"{"prop":"volume"}"#).unwrap();
        assert!(bare.keys.is_empty());
        let k: PropertyKey = serde_json::from_str(r#"{"time":1.0,"value":2.0}"#).unwrap();
        assert_eq!(k.easing, Easing::Linear);
        assert_eq!(Property::wire_names().len(), Property::ALL.len());
        for p in Property::ALL {
            assert_eq!(Property::parse(p.as_str()), Some(p));
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_str());
        }
    }

    #[test]
    fn an_unkeyed_property_is_its_static_value() {
        let mut c = clip();
        c.color.brightness = 0.25;
        c.volume = 0.5;
        assert!(!c.is_keyed(Property::Brightness) && !c.color_animated() && !c.volume_animated());
        assert_eq!(c.color_at(3.0), c.color);
        assert_eq!(c.volume_at(3.0), 0.5);
        assert_eq!(c.transform_at(3.0), c.transform);
        assert!(c.property_keys(Property::Brightness).is_empty());
    }

    #[test]
    fn a_channel_animates_one_number_and_leaves_the_rest_alone() {
        let mut c = clip();
        c.transform.scale = 1.5;
        c.color.contrast = 1.2;
        c.set_property_keys(
            Property::Opacity,
            vec![key(0.0, 0.0, Easing::Linear), key(2.0, 1.0, Easing::Linear)],
        );
        assert!(c.is_animated() && c.is_keyed(Property::Opacity) && !c.is_keyed(Property::Scale));
        let t = c.transform_at(1.0);
        assert!((t.opacity - 0.5).abs() < 1e-12, "{t:?}");
        assert_eq!((t.scale, t.pos_x, t.rotation), (1.5, 0.0, 0.0), "the static numbers stay");
        assert!(!c.zoom_animated());
        assert_eq!(c.color_at(1.0), c.color);

        c.set_property_keys(
            Property::Brightness,
            vec![key(1.0, -0.5, Easing::Linear), key(3.0, 0.5, Easing::Linear)],
        );
        assert!(c.color_animated());
        let col = c.color_at(2.0);
        assert!((col.brightness - 0.0).abs() < 1e-12 && col.contrast == 1.2, "{col:?}");
        // Held flat before the first key and after the last.
        assert_eq!(c.color_at(0.0).brightness, -0.5);
        assert_eq!(c.color_at(9.0).brightness, 0.5);

        c.set_property_keys(
            Property::Volume,
            vec![key(0.0, 0.0, Easing::Linear), key(4.0, 2.0, Easing::Linear)],
        );
        assert!((c.volume_at(1.0) - 0.5).abs() < 1e-12);
        // The static volume is the value of a clip with no track.
        c.set_property_keys(Property::Volume, vec![]);
        assert!(!c.volume_animated() && c.channel(Property::Volume).is_none());
    }

    #[test]
    fn a_track_is_read_in_range_and_in_time_order() {
        let mut c = clip();
        c.channels.push(PropertyTrack {
            prop: Property::Saturation,
            keys: vec![key(2.0, 99.0, Easing::Linear), key(0.0, f64::NAN, Easing::Linear)],
        });
        let keys = c.property_keys(Property::Saturation);
        assert_eq!(
            keys.iter().map(|k| (k.time, k.value)).collect::<Vec<_>>(),
            [(0.0, 1.0), (2.0, 3.0)],
            "NaN is neutral, the rest is held to 0..=3"
        );
        assert_eq!(Property::Scale.clamp(-3.0), 1e-6);
        assert_eq!(Property::Volume.clamp(40.0), MAX_CHANNEL_VOLUME);
        assert!(Property::Opacity.check(1.5).is_err() && Property::Opacity.check(0.5).is_ok());
        assert!(Property::Scale.check(0.0).is_err() && Property::PosX.check(f64::INFINITY).is_err());
        assert!(Property::Gamma.check(0.05).is_err() && Property::Temperature.check(-1.0).is_ok());
    }

    #[test]
    fn a_property_without_a_track_reads_through_the_legacy_bundle_untouched() {
        let mut c = clip();
        c.keyframes = vec![
            bundle_key(2.0, 2.0, 0.5, Easing::EaseOut),
            bundle_key(0.0, 1.0, 1.0, Easing::EaseIn),
        ];
        for p in Property::TRANSFORM {
            assert!(c.is_keyed(p));
        }
        assert!(!c.is_keyed(Property::Volume));
        // The same polyline the bundle has always drawn, sample for sample.
        assert_eq!(c.property_curve(Property::Scale), c.keyframe_channel(|k| k.scale));
        assert_eq!(c.property_curve(Property::Opacity), c.keyframe_channel(|k| k.opacity));
        let by_bundle = |local: f64| interpolate(&c.keyframe_channel(|k| k.scale), local).unwrap();
        for i in 0..=40 {
            let local = f64::from(i) * 0.06;
            assert_eq!(c.transform_at(local).scale, by_bundle(local));
        }
    }

    #[test]
    fn taking_a_property_over_from_the_bundle_changes_nothing_about_how_it_moves() {
        let mut c = clip();
        c.keyframes = vec![
            bundle_key(0.0, 1.0, 1.0, Easing::EaseInOut),
            bundle_key(2.0, 2.0, 0.5, Easing::Hold),
            bundle_key(3.0, 1.0, 0.0, Easing::Linear),
        ];
        let before = c.clone();
        // Keying the opacity once more: its bundle keys come along, and the new one splits a segment.
        c.insert_property_key(
            Property::Opacity,
            PropertyKey::new(1.0, before.property_at(Property::Opacity, 1.0)),
        );
        assert!(c.channel(Property::Opacity).is_some() && c.channel(Property::Scale).is_none());
        for i in 0..=120 {
            let local = f64::from(i) * 0.03;
            for p in Property::TRANSFORM {
                // The one that got a key re-draws its curve (see the next test); the others are as they were.
                let tol = if p == Property::Opacity { 1.5e-2 } else { 1e-9 };
                let (a, b) = (before.property_at(p, local), c.property_at(p, local));
                assert!((a - b).abs() < tol, "{p:?} at {local}: {a} became {b}");
            }
        }
        // ... and an empty track holds a property off the bundle without touching the others.
        c.set_property_keys(Property::Scale, vec![]);
        assert!(!c.is_keyed(Property::Scale) && c.is_keyed(Property::PosX));
        assert_eq!(c.property_at(Property::Scale, 1.0), c.transform.scale);
        assert!(c.channel(Property::Scale).is_some_and(|t| t.keys.is_empty()));
        // Once the bundle is gone the empty track has nothing to say.
        c.keyframes.clear();
        c.prune_channels();
        assert!(c.channel(Property::Scale).is_none());
        assert!(c.channel(Property::Opacity).is_some());
    }

    #[test]
    fn a_key_put_on_the_curve_does_not_change_it() {
        for easing in easings() {
            let mut c = clip();
            c.set_property_keys(
                Property::Brightness,
                vec![key(1.0, -0.4, easing), key(5.0, 0.6, Easing::Linear)],
            );
            let before = c.clone();
            let at = 2.7;
            c.insert_property_key(
                Property::Brightness,
                PropertyKey::new(at, before.property_at(Property::Brightness, at)),
            );
            assert_eq!(c.property_keys(Property::Brightness).len(), 3);
            // The same curve drawn in two pieces of 12 straight lines each differs from the one
            // drawn in 12 by how a steep stretch is drawn (about 0.003 of a range of 1 here);
            // the S that turns back is re-fitted (a few hundredths); a line and a hold are exact.
            let tol = match easing {
                Easing::Linear | Easing::Hold => 1e-9,
                Easing::Bezier { .. } => 0.05,
                _ => 1.5e-2,
            };
            for i in 0..=100 {
                let local = f64::from(i) * 0.06;
                let (a, b) = (before.color_at(local).brightness, c.color_at(local).brightness);
                assert!((a - b).abs() <= tol, "{easing:?} at {local}: {a} became {b}");
            }
        }
        // Re-keying a moment moves the value and keeps how it leaves.
        let mut c = clip();
        c.set_property_keys(
            Property::Gamma,
            vec![key(0.0, 1.0, Easing::Hold), key(2.0, 2.0, Easing::Linear)],
        );
        c.insert_property_key(Property::Gamma, PropertyKey::new(0.0, 1.5));
        assert_eq!(c.property_keys(Property::Gamma)[0], key(0.0, 1.5, Easing::Hold));
    }

    #[test]
    fn the_easing_of_a_segment_is_set_on_the_key_that_leaves_it() {
        let mut c = clip();
        c.set_property_keys(
            Property::Contrast,
            vec![key(0.0, 1.0, Easing::Linear), key(2.0, 2.0, Easing::Linear)],
        );
        assert!(c.set_property_easing(Property::Contrast, 0.0004, Easing::Hold));
        assert_eq!(c.property_keys(Property::Contrast)[0].easing, Easing::Hold);
        assert_eq!(c.property_at(Property::Contrast, 1.0), 1.0, "a hold steps at the next key");
        assert!(!c.set_property_easing(Property::Contrast, 1.0, Easing::Hold), "no key at 1 s");
        assert!(
            !c.set_property_easing(Property::Saturation, 0.0, Easing::Hold),
            "no track, no key"
        );
        assert!(
            c.channel(Property::Saturation).is_none(),
            "a refused easing leaves no track behind"
        );
        // A bundle-driven property is taken over by it.
        let mut b = clip();
        b.keyframes = vec![
            bundle_key(0.0, 1.0, 1.0, Easing::Linear),
            bundle_key(2.0, 2.0, 0.0, Easing::Linear),
        ];
        assert!(b.set_property_easing(Property::Opacity, 0.0, Easing::Hold));
        assert_eq!(b.property_at(Property::Opacity, 1.0), 1.0);
        assert_eq!(
            b.keyframes[0].easing,
            Easing::Linear,
            "the bundle's other numbers keep theirs"
        );
        assert!((b.property_at(Property::Scale, 1.0) - 1.5).abs() < 1e-12);
    }

    /// The channel's value at `at`, from the original, minus the head cut off.
    fn cut_matches(original: &Clip, prop: Property, by: f64, tol: f64) -> Clip {
        let mut cut = original.clone();
        cut.rebase_animation(by);
        for i in 0..=140 {
            let local = f64::from(i) * 0.05;
            let (a, b) = (original.property_at(prop, local + by), cut.property_at(prop, local));
            assert!(
                (a - b).abs() <= tol,
                "{prop:?} cut at {by}: at {local} the original is {a}, the cut {b}"
            );
        }
        cut
    }

    #[test]
    fn a_cut_head_keeps_every_channel_exactly_where_it_was() {
        for easing in easings() {
            let mut c = clip();
            c.set_property_keys(
                Property::Saturation,
                vec![
                    key(0.0, 1.0, easing),
                    key(2.0, 2.5, Easing::EaseInOut),
                    key(2.0 + 1e-3, 0.5, easing),
                    key(5.0, 1.5, Easing::Linear),
                ],
            );
            c.set_property_keys(
                Property::Volume,
                vec![
                    key(1.0, 0.2, easing),
                    key(4.0, 1.8, Easing::Hold),
                    key(6.0, 1.0, Easing::Linear),
                ],
            );
            // Before the first key, on a key, inside each kind of segment, past the last key.
            for by in [0.0, 0.5, 1.0, 1.7, 2.0, 2.0005, 3.0, 4.0, 4.5, 5.0, 6.0, 7.0] {
                if by == 0.0 {
                    continue;
                }
                for prop in [Property::Saturation, Property::Volume] {
                    cut_matches(&c, prop, by, 1e-9);
                }
            }
        }
    }

    #[test]
    fn a_cut_head_keeps_the_hold_and_the_bundle_beside_the_channels() {
        let mut c = clip();
        c.keyframes = vec![
            bundle_key(0.0, 1.0, 1.0, Easing::EaseIn),
            bundle_key(3.0, 2.0, 0.2, Easing::Linear),
        ];
        c.set_property_keys(
            Property::Gamma,
            vec![key(0.0, 1.0, Easing::Hold), key(4.0, 2.0, Easing::Linear)],
        );
        let cut = cut_matches(&c, Property::Gamma, 2.0, 1e-12);
        assert_eq!(cut.property_keys(Property::Gamma)[0].easing, Easing::Hold, "still a hold");
        assert_eq!(cut.property_keys(Property::Gamma).len(), 2);
        for prop in Property::TRANSFORM {
            cut_matches(&c, prop, 1.3, 1e-9);
        }
        // Earlier instead: every key moves later, the head holds the first key's value.
        let mut late = c.clone();
        late.rebase_animation(-1.5);
        assert_eq!(late.property_keys(Property::Gamma)[0].time, 1.5);
        assert_eq!(late.property_at(Property::Gamma, 0.0), 1.0);
        assert!((late.property_at(Property::Gamma, 3.5) - c.property_at(Property::Gamma, 2.0)).abs() < 1e-12);
    }

    #[test]
    fn a_range_slice_and_a_split_carry_channels_with_the_footage() {
        use crate::model::{Timeline, Track};
        let mut c = clip();
        c.set_property_keys(
            Property::Brightness,
            vec![key(0.0, -1.0, Easing::EaseInOut), key(8.0, 1.0, Easing::Linear)],
        );
        c.set_property_keys(
            Property::Volume,
            vec![key(2.0, 1.0, Easing::Linear), key(6.0, 0.0, Easing::Linear)],
        );
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips.push(c.clone());
        let mut tl = Timeline {
            tracks: vec![track],
            ..Timeline::default()
        };
        // A range export starting 3 s in.
        let sliced = tl.slice(3.0, 9.0);
        let s = &sliced.tracks[0].clips[0];
        for i in 0..=100 {
            let local = f64::from(i) * 0.05;
            assert!((s.color_at(local).brightness - c.color_at(local + 3.0).brightness).abs() < 1e-9);
            assert!((s.volume_at(local) - c.volume_at(local + 3.0)).abs() < 1e-9);
        }
        // A split: the two halves play the animation the whole clip did.
        let (left, right) = tl.split_clip(c.id, 3.0).unwrap();
        for i in 0..=100 {
            let local = f64::from(i) * 0.05;
            assert!(
                (right.color_at(local).brightness - c.color_at(local + 3.0).brightness).abs() < 1e-9,
                "right half at {local}"
            );
            assert!((right.volume_at(local) - c.volume_at(local + 3.0)).abs() < 1e-9);
            if local < 3.0 {
                assert!((left.color_at(local).brightness - c.color_at(local).brightness).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn a_split_also_keeps_the_legacy_bundle_playing_through() {
        use crate::model::{Timeline, Track};
        let mut c = clip();
        c.keyframes = vec![
            bundle_key(0.0, 1.0, 1.0, Easing::EaseInOut),
            bundle_key(6.0, 2.0, 0.0, Easing::Linear),
        ];
        let mut track = Track::new(StreamKind::Video, "V1");
        track.clips.push(c.clone());
        let mut tl = Timeline {
            tracks: vec![track],
            ..Timeline::default()
        };
        let (left, right) = tl.split_clip(c.id, 2.5).unwrap();
        for i in 0..=60 {
            let local = f64::from(i) * 0.05;
            assert!((right.transform_at(local).scale - c.transform_at(local + 2.5).scale).abs() < 1e-9);
            assert!((right.transform_at(local).opacity - c.transform_at(local + 2.5).opacity).abs() < 1e-9);
            assert_eq!(left.transform_at(local), c.transform_at(local));
        }
    }

    #[test]
    fn detached_sound_takes_the_volume_curve_through_the_fader_ratio() {
        use crate::model::{Timeline, Track};
        let mut c = clip();
        c.set_property_keys(
            Property::Volume,
            vec![key(0.0, 0.5, Easing::EaseIn), key(4.0, 1.0, Easing::Linear)],
        );
        let mut video = Track::new(StreamKind::Video, "V1");
        video.volume = 0.5;
        video.clips.push(c.clone());
        let mut audio = Track::new(StreamKind::Audio, "A1");
        audio.volume = 1.0;
        let mut tl = Timeline {
            tracks: vec![video, audio],
            ..Timeline::default()
        };
        let done = tl.detach_audio(c.id, true).unwrap();
        // The picture track's fader (0.5) rode this sound; the audio track's (1.0) rides it now.
        let keys = done.clip.property_keys(Property::Volume);
        assert_eq!(
            keys.iter().map(|k| (k.time, k.value, k.easing)).collect::<Vec<_>>(),
            [(0.0, 0.25, Easing::EaseIn), (4.0, 0.5, Easing::Linear)]
        );
        assert!(
            tl.clip(c.id).unwrap().volume_animated(),
            "the picture keeps its own, inert while muted"
        );
    }

    #[test]
    fn copying_keys_shifts_them_and_a_negative_offset_cuts_the_head() {
        let mut c = clip();
        c.set_property_keys(
            Property::Volume,
            vec![key(1.0, 0.0, Easing::Linear), key(3.0, 2.0, Easing::Linear)],
        );
        c.keyframes = vec![
            bundle_key(0.0, 1.0, 1.0, Easing::Linear),
            bundle_key(2.0, 2.0, 0.0, Easing::Linear),
        ];
        let all = c.property_keys_shifted(&[], 0.5);
        let props: Vec<Property> = all.iter().map(|t| t.prop).collect();
        assert_eq!(
            props,
            [
                Property::Scale,
                Property::PosX,
                Property::PosY,
                Property::Rotation,
                Property::Opacity,
                Property::Volume
            ]
        );
        let volume = all.iter().find(|t| t.prop == Property::Volume).unwrap();
        assert_eq!(volume.keys.iter().map(|k| k.time).collect::<Vec<_>>(), [1.5, 3.5]);
        let cut = c.property_keys_shifted(&[Property::Volume], -2.0);
        assert_eq!(
            cut[0].keys.iter().map(|k| (k.time, k.value)).collect::<Vec<_>>(),
            [(0.0, 1.0), (1.0, 2.0)],
            "2 s in, the volume is at 1 and the last key 1 s later"
        );
        assert!(c.property_keys_shifted(&[Property::Gamma], 1.0)[0].keys.is_empty());
    }

    /// The clip `frontend/src/lib/channels.test.ts` builds too.
    fn mirrored() -> Clip {
        let mut c = clip();
        c.keyframes = vec![
            bundle_key(0.0, 1.0, 1.0, Easing::EaseOut),
            bundle_key(2.0, 2.0, 0.5, Easing::Linear),
        ];
        c.set_property_keys(
            Property::Volume,
            vec![
                key(0.2, 0.2, Easing::EaseIn),
                key(1.1, 1.8, Easing::Linear),
                key(1.7, 0.4, Easing::Hold),
                key(2.2013, 1.0, Easing::Linear),
            ],
        );
        c.set_property_keys(
            Property::Brightness,
            vec![
                key(0.0, -0.3, Easing::EaseInOut),
                key(
                    2.0,
                    0.4,
                    Easing::Bezier {
                        x1: 0.2,
                        y1: 0.9,
                        x2: 0.3,
                        y2: 1.0,
                    },
                ),
                key(3.0, 0.0, Easing::Linear),
            ],
        );
        c
    }

    /// The numbers `frontend/src/lib/channels.test.ts` pins too: the TS mirror is the harness's
    /// whole idea of a channel, so a rule changed here has to change there.
    #[test]
    fn channels_match_the_frontend_mirror_bit_for_bit() {
        let c = mirrored();
        let volume = [
            (0.0, 0.2),
            (0.5, 0.449862273587892),
            (1.1, 1.8),
            (1.4, 1.1000000000000005),
            (1.7, 0.4),
            (1.9, 0.4),
            (2.2013, 1.0),
            (3.0, 1.0),
        ];
        for (t, want) in volume {
            assert_eq!(c.volume_at(t), want, "volume at {t}");
        }
        let brightness = [
            (0.25, -0.27559255044876013),
            (0.9, -0.0092929134307706),
            (1.6, 0.3402597151672505),
            (2.5, 0.020021298908220964),
        ];
        for (t, want) in brightness {
            assert_eq!(c.color_at(t).brightness, want, "brightness at {t}");
        }
        // The bundle still drives what has no track.
        let at = c.transform_at(1.0);
        assert_eq!((at.scale, at.opacity), (1.6846431874269898, 0.6576784062865051));
        // A head cut inside an eased segment: the pose pinned, the rest baked into plain keys.
        let rebased: Vec<(f64, f64, Easing)> = super::rebase_head(&c.property_keys(Property::Volume), 0.9)
            .iter()
            .map(|k| (k.time, k.value, k.easing))
            .collect();
        assert_eq!(
            rebased,
            [
                (0.0, 1.2578044843619944, Easing::Linear),
                (0.050000000000000155, 1.3834554717250953, Easing::Linear),
                (0.1250000000000001, 1.5842902877063407, Easing::Linear),
                (0.20000000000000007, 1.8, Easing::Linear),
                (0.7999999999999999, 0.4, Easing::Hold),
                (1.3013, 1.0, Easing::Linear),
            ]
        );
        // A key put inside an eased segment splits it (de Casteljau, each half in its own unit square).
        let mut split = c.clone();
        split.insert_property_key(Property::Volume, PropertyKey::new(0.7, 0.55));
        let keys: Vec<(f64, f64, Easing)> = split
            .property_keys(Property::Volume)
            .iter()
            .map(|k| (k.time, k.value, k.easing))
            .collect();
        let bezier = |x1, y1, x2, y2| Easing::Bezier { x1, y1, x2, y2 };
        assert_eq!(
            keys,
            [
                (
                    0.2,
                    0.2,
                    bezier(0.31544631633977743, 0.0, 0.681034420783558, 0.4617901153530115)
                ),
                (0.7, 0.55, bezier(0.5568358766841033, 0.4548965203318149, 1.0, 1.0)),
                (1.1, 1.8, Easing::Linear),
                (1.7, 0.4, Easing::Hold),
                (2.2013, 1.0, Easing::Linear),
            ]
        );
        // A transform number taken off the bundle keeps the bundle's keys and shapes, and splits.
        let mut taken = c.clone();
        taken.insert_property_key(Property::Scale, PropertyKey::new(1.0, 1.5));
        let keys: Vec<(f64, f64, Easing)> = taken
            .property_keys(Property::Scale)
            .iter()
            .map(|k| (k.time, k.value, k.easing))
            .collect();
        assert_eq!(
            keys,
            [
                (0.0, 1.0, bezier(0.0, 0.0, 0.4542081650096481, 0.5719165400747731)),
                (
                    1.0,
                    1.5,
                    bezier(0.3264332254175324, 0.5558502990076779, 0.6856271141503777, 1.0)
                ),
                (2.0, 2.0, Easing::Linear),
            ]
        );
    }

    #[test]
    fn the_diff_names_the_property_that_moved() {
        let before = clip();
        let mut after = before.clone();
        after.set_property_keys(
            Property::Volume,
            vec![key(0.0, 1.0, Easing::Linear), key(2.0, 0.0, Easing::Linear)],
        );
        assert_eq!(channel_changes(&before, &after), ["volume keyframes 0 → 2"]);
        let mut later = after.clone();
        later.channels[0].keys[1].value = 0.5;
        assert_eq!(channel_changes(&after, &later), ["volume keyframes changed"]);
        let mut eased = after.clone();
        eased.channels[0].keys[0].easing = Easing::Hold;
        assert_eq!(channel_changes(&after, &eased), ["easing changed on 1 volume keyframe"]);
        assert!(channel_changes(&after, &after).is_empty());
    }
}
