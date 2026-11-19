//! A clip: a range of an asset on a track, with its speed, fades and beats.

use super::*;

/// A single non-destructive edit referencing a source range of an asset.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: Uuid,
    pub asset_id: Uuid,
    /// In-point in the source asset (seconds).
    pub source_in: f64,
    /// Out-point in the source asset (seconds).
    pub source_out: f64,
    /// Position of the clip on the timeline (seconds).
    pub timeline_start: f64,
    /// Linear gain applied to this clip (1.0 = unchanged).
    pub volume: f32,
    /// Fade-in duration at the clip's start (seconds); 0.0 = no fade. Applied to
    /// both picture (fade from black) and audio (fade from silence) at export.
    #[serde(default)]
    pub fade_in: f64,
    /// Fade-out duration at the clip's end (seconds); 0.0 = no fade.
    #[serde(default)]
    pub fade_out: f64,
    /// Playback rate (1.0 = unchanged). > 1.0 speeds up, < 1.0 slows down, and a
    /// negative value plays the source in reverse. The clip's timeline duration
    /// is its source span divided by the magnitude of the speed.
    #[serde(default = "one")]
    pub speed: f64,
    /// A rate that changes over the clip ([`SpeedKey`]s in clip-local timeline seconds). While it
    /// holds a well-formed ramp it decides the rate and `speed` only gives the direction (see the
    /// `speed_ramp` module). Empty, and absent from the file, for a clip at one speed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub speed_ramp: Vec<SpeedKey>,
    /// Geometric transform (scale / position / crop / rotation / opacity).
    #[serde(default)]
    pub transform: Transform,
    /// Color correction (brightness / contrast / saturation / gamma).
    #[serde(default)]
    pub color: Color,
    /// Curves, colour wheels and a 3D LUT, applied after `color` ([`Grade`]). Empty for every
    /// clip saved before they existed.
    #[serde(default, skip_serializing_if = "Grade::is_empty")]
    pub grade: Grade,
    /// Transition blending this clip's start with the preceding clip, if any.
    #[serde(default)]
    pub transition_in: Option<Transition>,
    /// Video effects applied in order at export (blur, chroma key, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<VideoEffect>,
    /// Audio effects applied in order at export (EQ, compressor, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioEffect>,
    /// Transform animation. Empty = the static `transform` is used; otherwise the
    /// engine interpolates these keyframes to animate scale / position / rotation
    /// / opacity over the clip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
    /// Per-property animation: any one number of the transform, the colour or the clip's
    /// volume with keys of its own ([`PropertyTrack`]). A property with a track is driven by
    /// it, whatever `keyframes` says; one without keeps reading the bundle (transform numbers)
    /// or its static value. Empty for every clip saved before channels existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<PropertyTrack>,
    /// Reprojection of 360 source footage, when this clip references a spherical
    /// asset. `None` for ordinary flat video (and for a 360 clip the user has
    /// explicitly un-reframed, to work in the raw projection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reframe: Option<Reframe>,
    /// A shape cut out of this clip, making the rest transparent so a lower
    /// track shows through. `None` is the whole frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Mask>,
    /// How the clip composites onto the tracks below ([`BlendMode`]). `Normal`, and absent from the
    /// file, for plain alpha-over: every graph without a blend is byte-identical.
    #[serde(default, skip_serializing_if = "BlendMode::is_normal")]
    pub blend_mode: BlendMode,
    /// Crops for delivery shapes other than the one the project is cut for —
    /// what lets one cut render as a 9:16 Reel *and* a 1:1 post with each shot
    /// framed for each. Written by the multi-format export's framing pass, read
    /// only by [`Timeline::for_delivery`]; the project frame's own crop stays in
    /// `transform`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub framings: Vec<Framing>,
    /// Whether this clip renders. A disabled clip keeps its place on the
    /// timeline (and its trims, effects and keyframes) but is dropped before the
    /// render graph is built — the per-clip counterpart of muting a track.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub enabled: bool,
    /// The link group this clip belongs to: clips sharing a `link_id` are one
    /// piece of material — a picture and the sound that goes with it — and an
    /// edit to one is carried to the others (see [`Timeline::link_partners`] and
    /// the `links` module). At most one clip of a group per track. Never part of
    /// the render graph; `None` (omitted from the file) is an unlinked clip, which
    /// is every clip a project made before links existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_id: Option<Uuid>,
    /// Whether the clip plays the audio of its *own* asset. A video clip's
    /// footage normally carries its sound with it; `false` mutes just that —
    /// the picture is untouched — which is how a clip's sound is **detached**
    /// onto an audio track (`Project::detach_audio`) without being heard twice.
    /// Defaulted and omitted at `true`, so every graph written before the field
    /// existed is byte-identical.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub source_audio: bool,
    /// What the clip draws when its picture is generated rather than cut from an asset (an animated
    /// title on a track, [`Generator`]); `None`, and absent from the file, for every clip of footage.
    /// A generated clip's `asset_id` is [`GENERATED_ASSET`] and its source is the generator's clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<Generator>,
    /// Video stabilization ([`Stabilize`]): the camera's shake taken out of the picture, per source
    /// frame, from an analysis of the footage (`crate::stabilize`). `None`, and absent from the
    /// file, for every clip that is not stabilized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stabilize: Option<Stabilize>,
    /// Procedural motion layered on the pose ([`Behavior`]): a sway, a breath, a spin, a drift or a
    /// shake, each a closed-form function of clip-local time that is added to the clip's scale,
    /// position or rotation on top of its static transform and its keys. Empty, and absent from the
    /// file, for every clip that has none, so every stored timeline and every graph is byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behaviors: Vec<Behavior>,
}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

/// Smallest speed magnitude allowed, to keep clip durations finite.
pub const MIN_SPEED: f64 = 0.01;

/// Largest speed magnitude a clip may be set to (100×): the audio tempo is a chain of `atempo`
/// steps, one per doubling, and a clip sped past this is a few frames long whatever it was.
pub const MAX_SPEED: f64 = 100.0;

impl Clip {
    /// A new clip with default volume, no fades, full speed and identity
    /// transform / color and no transition.
    pub fn new(asset_id: Uuid, source_in: f64, source_out: f64, timeline_start: f64) -> Self {
        Self {
            id: Uuid::new_v4(),
            asset_id,
            source_in,
            source_out,
            timeline_start,
            volume: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            speed_ramp: Vec::new(),
            transform: Transform::default(),
            color: Color::default(),
            grade: Grade::default(),
            transition_in: None,
            effects: Vec::new(),
            audio: Vec::new(),
            keyframes: Vec::new(),
            channels: Vec::new(),
            reframe: None,
            mask: None,
            blend_mode: BlendMode::Normal,
            framings: Vec::new(),
            enabled: true,
            link_id: None,
            source_audio: true,
            generator: None,
            stabilize: None,
            behaviors: Vec::new(),
        }
    }

    /// The crop this clip carries for a delivery shape, if it was framed for it.
    pub fn framing_for(&self, ratio: (u32, u32)) -> Option<&Framing> {
        self.framings.iter().find(|f| f.ratio() == ratio)
    }

    /// Record the crop for one delivery shape, replacing an earlier one for the
    /// same shape. Returns whether anything changed.
    pub fn set_framing(&mut self, framing: Framing) -> bool {
        match self.framings.iter_mut().find(|f| f.ratio() == framing.ratio()) {
            Some(existing) if *existing == framing => false,
            Some(existing) => {
                *existing = framing;
                true
            }
            None => {
                self.framings.push(framing);
                true
            }
        }
    }

    /// A new clip that also reframes, when `asset` is 360 footage — the shape
    /// every clip-creating op should use so a spherical source lands on the
    /// timeline already looking like ordinary video.
    pub fn for_asset(asset: &Asset, source_in: f64, source_out: f64, timeline_start: f64) -> Self {
        let mut clip = Self::new(asset.id, source_in, source_out, timeline_start);
        clip.reframe = asset.projection().map(Reframe::new);
        clip
    }

    /// True when any number of the clip's transform is keyed (the bundle's keys or a
    /// property's own track) or moved by a live behaviour ([`Clip::behaves`]), i.e. the picture is
    /// animated.
    pub fn is_animated(&self) -> bool {
        Property::TRANSFORM.iter().any(|p| self.is_driven(*p))
    }

    /// True when the keyframes move the clip's *scale*, i.e. the picture the
    /// export hands to `overlay` changes size from one frame to the next.
    ///
    /// That is a different thing from [`Clip::is_animated`]: a clip keyed only on
    /// position, rotation or opacity keeps a constant picture size, which every
    /// filter in its chain can be built around. A zoom that actually moves cannot
    /// be: most filters read the frame size once, when the graph is configured, so
    /// the export restructures the chain around it (`video_clip_chain` puts the
    /// size-changing `scale` last). The test is on the keyed values, not on the
    /// expression the engine writes.
    pub fn zoom_animated(&self) -> bool {
        // Against the first key *in time*, which is the one the engine's expression holds
        // before the clip's first moment (the stored order is not guaranteed to be sorted).
        let keys = self.property_keys(Property::Scale);
        keys.first()
            .is_some_and(|first| keys.iter().any(|k| (k.value - first.value).abs() > 1e-9))
            || self.behaves(Property::Scale)
    }

    /// True when the keyframes move the clip's zoom or its position: where the picture lands on
    /// the canvas changes from frame to frame, by fractions of a pixel. The export then places it
    /// with a sub-pixel resample (`video_clip_chain`'s `perspective`) instead of a per-frame
    /// `scale` and `overlay` offsets, which step in whole (and, in 4:2:0, even) pixels.
    pub fn placement_animated(&self) -> bool {
        let moves = |prop: Property| {
            let keys = self.property_keys(prop);
            keys.first()
                .is_some_and(|first| keys.iter().any(|k| (k.value - first.value).abs() > 1e-9))
        };
        moves(Property::Scale)
            || moves(Property::PosX)
            || moves(Property::PosY)
            || self.behaves(Property::Scale)
            || self.behaves(Property::PosX)
            || self.behaves(Property::PosY)
    }

    /// The zoom the clip holds still at: its static scale, or the one value a keyed scale
    /// that never moves ([`Clip::zoom_animated`]) holds. `None` for a zoom that moves.
    pub fn held_scale(&self) -> Option<f64> {
        if self.zoom_animated() {
            None
        } else if self.is_keyed(Property::Scale) {
            Some(self.base_at(Property::Scale, 0.0))
        } else {
            Some(self.transform.scale)
        }
    }

    /// In what order the clip's chain fits the picture to the delivery frame and zooms it
    /// (pure; `fit` is the delivery's, a [`Fit::Blur`] counting as `Contain` for a clip that
    /// is not the plain fit). One answer for the export, the stream, the scrubbed still and
    /// the render plan, so none can disagree on whether a layer is the order a compositor
    /// draws. See [`FitOrder`].
    ///
    /// `cover_overflows` is [`crate::layer_geometry::cover_overflows`] for this clip's picture and the frame (`true`
    /// when the picture's size is not known): under `Cover` a picture of the frame's own shape has
    /// nothing past the frame to reveal, so only a zoom in changes its chain.
    pub fn fit_order(&self, fit: Fit, cover_overflows: bool) -> FitOrder {
        // The identity transform is the fit alone, and a moving zoom is the chain's last stage,
        // at the fit size: both keep the order they always had.
        if (self.transform.is_identity() && !self.is_animated()) || self.zoom_animated() {
            return FitOrder::Fitted;
        }
        let Some(scale) = self.held_scale().filter(|s| s.is_finite() && *s > 0.0) else {
            return FitOrder::Fitted;
        };
        let zooms_in = scale > 1.0 + 1e-9;
        match fit {
            Fit::Contain | Fit::Blur if zooms_in => FitOrder::Folded,
            Fit::Contain | Fit::Blur => FitOrder::Fitted,
            Fit::Cover => {
                // A mask and a vignette are laid out over the picture the chain hands them, which
                // under `Cover` has always been the frame's: where the cover hangs over the frame
                // they keep cropping first.
                let picture_relative = self.mask.is_some() || self.effects.iter().any(|e| matches!(e, VideoEffect::Vignette));
                if cover_overflows && picture_relative {
                    return FitOrder::Fitted;
                }
                let (px, py) = self.pan_extent();
                let reveals = cover_overflows && ((scale - 1.0).abs() > 1e-9 || px > 1e-9 || py > 1e-9);
                if reveals || zooms_in {
                    FitOrder::Uncropped
                } else {
                    FitOrder::Fitted
                }
            }
        }
    }

    /// The furthest the picture is ever moved from the centre, as a fraction of the frame width
    /// and of its height: the static position, or the greatest value of a keyed one's curve (the
    /// polyline every renderer draws, so its extremes are its points), and as far again as the
    /// behaviours can throw it over the clip ([`Clip::pan_extent_over`]).
    pub fn pan_extent(&self) -> (f64, f64) {
        self.pan_extent_over(self.duration())
    }

    /// [`Clip::pan_extent`] for a clip drawn over `horizon` seconds of its own clock (a clip kept
    /// past its end under a transition is drawn longer than it is): a drift keeps moving.
    pub fn pan_extent_over(&self, horizon: f64) -> (f64, f64) {
        let reach = |prop: Property| {
            if self.is_keyed(prop) {
                self.property_curve(prop).iter().fold(0.0_f64, |m, p| m.max(p.1.abs()))
            } else {
                self.static_value(prop).abs()
            }
        };
        let (bx, by) = self.behavior_reach(horizon);
        (reach(Property::PosX) + bx, reach(Property::PosY) + by)
    }

    /// The clip's keyframes sorted by time (the stored order is kept sorted by
    /// the editing op, but render code must not assume it).
    pub fn sorted_keyframes(&self) -> Vec<Keyframe> {
        let mut k = self.keyframes.clone();
        k.sort_by(|a, b| a.time.total_cmp(&b.time));
        k
    }

    /// One animatable channel of the keyframes as the polyline every renderer draws
    /// ([`eased_points`]): what [`Clip::transform_at`] interpolates and what the export's
    /// per-frame expression is written from, so the two cannot disagree.
    pub fn keyframe_channel(&self, get: fn(&Keyframe) -> f64) -> Vec<(f64, f64)> {
        let keys: Vec<(f64, f64, Easing)> = self.sorted_keyframes().iter().map(|k| (k.time, get(k), k.easing)).collect();
        eased_points(&keys)
    }

    /// Put `key` into the animation. A key within a microsecond of an existing one replaces it
    /// and keeps how that one leaves (re-keying a moment moves the pose, not the shape). A key
    /// that lands inside a segment splits it ([`Easing::split`]): the key before it eases into
    /// the new one along the first part of the curve and the new one eases out along the rest —
    /// a hold stays held through the new key rather than turning the rest of it into a ramp. The
    /// key's own `easing` is replaced when it replaces or splits a key; a key before the first
    /// or after the last keeps the one it came with.
    pub fn insert_keyframe(&mut self, mut key: Keyframe) {
        const SAME: f64 = 1e-6;
        let sorted = self.sorted_keyframes();
        if let Some(old) = sorted.iter().find(|k| (k.time - key.time).abs() <= SAME) {
            key.easing = old.easing;
        } else if let Some(w) = sorted.windows(2).find(|w| w[0].time < key.time && key.time < w[1].time) {
            let u = (key.time - w[0].time) / (w[1].time - w[0].time);
            let (before, after) = w[0].easing.split(u);
            key.easing = after;
            if let Some(a) = self.keyframes.iter_mut().find(|k| k.time == w[0].time) {
                a.easing = before;
            }
        }
        self.keyframes.retain(|k| (k.time - key.time).abs() > SAME);
        self.keyframes.push(key);
        self.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
    }

    /// Sample the (possibly animated) transform at `local` seconds from the
    /// clip's start: the static [`Transform`] with its animatable numbers
    /// (scale / position / rotation / opacity) overridden by the interpolated
    /// value of every one that is keyed ([`Clip::property_at`]), with the clip's live behaviours
    /// applied on top. Used by the still / preview path, which cannot evaluate the export's per-frame
    /// expressions. An edit that writes keys samples [`Clip::base_transform_at`] instead.
    pub fn transform_at(&self, local: f64) -> Transform {
        let mut t = self.transform;
        if !self.is_animated() {
            return t;
        }
        t.scale = self.property_at(Property::Scale, local);
        t.pos_x = self.property_at(Property::PosX, local);
        t.pos_y = self.property_at(Property::PosY, local);
        t.rotation = self.property_at(Property::Rotation, local);
        t.opacity = self.property_at(Property::Opacity, local);
        t
    }

    /// Sample the (possibly animated) reframe at `local` seconds from the clip's
    /// start. `None` when the clip is not reframed. Used by the still / preview
    /// path, which cannot drive `v360` with runtime commands the way export does.
    pub fn reframe_at(&self, local: f64) -> Option<ResolvedReframe> {
        self.reframe.as_ref().map(|r| r.sample(local))
    }

    /// Length of the referenced source span (seconds), ignoring speed.
    pub fn source_duration(&self) -> f64 {
        (self.source_out - self.source_in).max(0.0)
    }

    /// Speed magnitude, clamped away from zero (direction dropped). A ramped clip's rate varies:
    /// convert between the clocks with [`Clip::along`] / [`Clip::local_at_along`] instead.
    pub fn speed_mag(&self) -> f64 {
        self.speed.abs().max(MIN_SPEED)
    }

    /// True when the clip plays its source in reverse.
    pub fn is_reversed(&self) -> bool {
        self.speed < 0.0
    }

    /// Duration on the timeline (seconds), i.e. the source span retimed by speed (or its ramp).
    pub fn duration(&self) -> f64 {
        self.local_at_along(self.source_duration())
    }

    pub fn timeline_end(&self) -> f64 {
        self.timeline_start + self.duration()
    }

    /// Where a source timestamp of this clip lands on the timeline (seconds).
    /// Honors speed and reverse, so an analysis marker maps to the moment it is
    /// actually heard.
    pub fn source_to_timeline(&self, source: f64) -> f64 {
        let offset = if self.is_reversed() {
            self.source_out - source
        } else {
            source - self.source_in
        };
        self.timeline_start + self.local_at_along(offset)
    }

    /// The source timestamp playing at timeline time `time` — the inverse of
    /// [`Clip::source_to_timeline`], honoring speed and reverse. Not clamped to the
    /// clip's window.
    pub fn timeline_to_source(&self, time: f64) -> f64 {
        let along = self.along(time - self.timeline_start);
        if self.is_reversed() {
            self.source_out - along
        } else {
            self.source_in + along
        }
    }

    /// True when any part of the source span `[from, to)` is inside this clip's
    /// source window — i.e. whether this clip actually shows that footage.
    pub fn covers_source(&self, from: f64, to: f64) -> bool {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        hi.min(self.source_out) > lo.max(self.source_in)
    }

    /// Where a source span lands on the timeline, as an ordered range. The ends
    /// are mapped through [`Clip::source_to_timeline`] and **not** clamped to the
    /// clip, so a span that starts before the in-point maps to a time before
    /// `timeline_start` — the caller decides what to do with the part that was
    /// trimmed away. A reversed clip swaps the ends, which is why the result is
    /// ordered rather than built from `from`/`to` directly.
    pub fn source_span_to_timeline(&self, from: f64, to: f64) -> TimeRange {
        let a = self.source_to_timeline(from);
        let b = self.source_to_timeline(to);
        TimeRange {
            start: a.min(b),
            end: a.max(b),
        }
    }

    /// Unused footage either side of the clip's source window, in **timeline**
    /// seconds, as `(head, tail)`: how far the clip's start / end could be pulled
    /// out before the source runs dry. A reversed clip plays the window
    /// backwards, so its start is the source's *out* side and the two swap. A
    /// still (`limit` infinite — see [`Asset::source_limit`]) has no footage to
    /// run out of, so both are unbounded.
    pub(super) fn handles(&self, limit: f64) -> (f64, f64) {
        if limit.is_infinite() {
            return (f64::INFINITY, f64::INFINITY);
        }
        if self.is_ramped() {
            // The footage either side is a source span; how long it plays depends on where it
            // plays. Pulling the start out shifts the ramp later (the new head holds the first
            // key's speed), extending the end runs on at the last key's.
            let (before, after) = (self.source_in.max(0.0), (limit - self.source_out).max(0.0));
            let (head, tail) = if self.is_reversed() {
                (after, before)
            } else {
                (before, after)
            };
            return (
                -self.local_at_along(-head),
                self.local_at_along(self.source_duration() + tail) - self.duration(),
            );
        }
        let mag = self.speed_mag();
        let before = self.source_in.max(0.0) / mag;
        let after = (limit - self.source_out).max(0.0) / mag;
        if self.is_reversed() {
            (after, before)
        } else {
            (before, after)
        }
    }

    /// Source seconds the end moves when it moves by `by` timeline seconds (positive lengthens): the
    /// ramp from the end on, not a ratio, for a clip whose rate changes.
    pub(super) fn tail_shift(&self, by: f64) -> f64 {
        if !self.is_ramped() {
            return by * self.speed_mag();
        }
        if by == 0.0 {
            return 0.0;
        }
        self.along(self.duration() + by) - self.source_duration()
    }

    /// Move the clip's end by `by` timeline seconds (positive lengthens),
    /// writing the source window the way the clip plays it: a forward clip's
    /// out-point, a reversed clip's in-point. `looping` — a still — writes only
    /// the out-point whichever way it plays: a still's window is its length, and
    /// this way it can never reach below zero.
    pub(super) fn move_tail(&mut self, by: f64, looping: bool) {
        let shift = self.tail_shift(by);
        if self.is_reversed() && !looping {
            self.source_in -= shift;
        } else {
            self.source_out += shift;
        }
    }

    /// Move the clip's start by `by` timeline seconds — positive shortens it from
    /// the front, negative pulls the start earlier and the clip longer — so its
    /// **end stays where it was**. The source window follows as in
    /// [`Clip::move_tail`], and the animation rides with the content
    /// ([`Clip::rebase_animation`]).
    pub(super) fn move_head(&mut self, by: f64, looping: bool) {
        // The footage the head gives up is read off the ramp as it was, before it is rebased.
        let shift = self.along(by);
        self.rebase_animation(by);
        if self.is_reversed() || looping {
            self.source_out -= shift;
        } else {
            self.source_in += shift;
        }
        self.timeline_start += by;
    }

    /// Re-time the clip's animation after its start moved by `by` timeline
    /// seconds. Transform and reframe keyframes are clip-local, so when the head
    /// moves they have to move with it or the animation slides off the footage it
    /// was written against: a key that fired two seconds into the shot still
    /// fires on that moment of it.
    ///
    /// Shortened from the front (`by > 0`) the keys inside the removed span are
    /// gone, so the pose the clip now opens on is pinned as a key at 0 (what
    /// [`Timeline::slice`] does for a range export) and later keys shift back.
    /// Pulled earlier (`by < 0`) every key shifts later, and the new head holds the
    /// first key's pose — which is what interpolation does before the first key.
    /// The speed ramp rides with the content the same way ([`Clip::rebase_ramp`]).
    pub(super) fn rebase_animation(&mut self, by: f64) {
        if by > 0.0 {
            if !self.keyframes.is_empty() {
                // (The keys' pose, not the behaviours': those carry on by their own clock, below.)
                let pose = self.base_transform_at(by);
                let mut kfs = vec![Keyframe::from_transform(0.0, &pose)];
                // Cut inside an eased segment: the rest of it is kept exactly. A hold keeps
                // holding to the next key; a curve's remaining pieces become plain keys (the
                // curve *is* those pieces, see `Easing`).
                let sorted = self.sorted_keyframes();
                let segment = sorted
                    .windows(2)
                    .find(|w| w[0].time <= by && by < w[1].time && w[1].time - w[0].time >= 1e-9);
                if let Some(&[a, b]) = segment {
                    match a.easing {
                        Easing::Linear => {}
                        Easing::Hold => kfs[0].easing = Easing::Hold,
                        curved => {
                            for j in 1..EASE_STEPS {
                                let u = j as f64 / EASE_STEPS as f64;
                                let at = a.time + (b.time - a.time) * u;
                                if at > by {
                                    kfs.push(a.toward(&b, curved.curve(u), at - by));
                                }
                            }
                        }
                    }
                }
                kfs.extend(
                    sorted
                        .iter()
                        .filter(|k| k.time > by)
                        .map(|k| Keyframe { time: k.time - by, ..*k }),
                );
                self.keyframes = kfs;
            }
            if self.reframe.as_ref().is_some_and(|r| r.is_animated()) {
                let pose = self.reframe_at(by).expect("clip reframes");
                let rf = self.reframe.as_mut().expect("clip reframes");
                let mut kfs = vec![ReframeKeyframe::from_pose(0.0, &pose)];
                kfs.extend(
                    rf.keyframes
                        .iter()
                        .filter(|k| k.time > by)
                        .map(|k| ReframeKeyframe { time: k.time - by, ..*k }),
                );
                rf.keyframes = kfs;
            }
        } else if by < 0.0 {
            for k in &mut self.keyframes {
                k.time -= by;
            }
            if let Some(rf) = self.reframe.as_mut() {
                for k in &mut rf.keyframes {
                    k.time -= by;
                }
            }
        }
        if by != 0.0 {
            self.rebase_channels(by);
            self.rebase_behaviors(by);
            self.rebase_ramp(by);
        }
    }

    /// Hold both fades inside the clip: an edit that shortens a clip must not
    /// leave a fade longer than what is left of it. Only ever shortens one.
    pub(super) fn clamp_fades(&mut self) {
        let duration = self.duration();
        self.fade_in = self.fade_in.min(duration);
        self.fade_out = self.fade_out.min(duration);
    }
}

/// Tempo estimates below this confidence are ignored when building a beat grid
/// — the same gate the timeline ruler uses to decide whether to draw beat ticks.
pub const BEAT_MIN_CONFIDENCE: f64 = 0.25;

/// Shortest clip a beat alignment may leave behind (seconds).
pub const MIN_BEAT_CLIP: f64 = 0.05;

/// The beat nearest `time` within `tolerance`, from an ascending beat grid.
pub fn nearest_beat(beats: &[f64], time: f64, tolerance: f64) -> Option<f64> {
    if tolerance <= 0.0 {
        return None;
    }
    let after = beats.partition_point(|b| *b < time);
    beats[after.saturating_sub(1)..beats.len().min(after + 1)]
        .iter()
        .copied()
        .filter(|b| (b - time).abs() <= tolerance)
        .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()))
}

/// Half the median beat interval — the widest tolerance that still has a single
/// answer, so every cut moves to the beat it is already closest to.
pub fn default_beat_tolerance(beats: &[f64]) -> f64 {
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    if gaps.is_empty() {
        return 0.0;
    }
    gaps.sort_by(f64::total_cmp);
    gaps[gaps.len() / 2] / 2.0
}
