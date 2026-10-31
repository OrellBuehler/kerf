//! A **sound effect**: a whoosh, an impact, a riser, a UI pop. Kerf synthesizes them procedurally
//! ([`crate::engine::sfx`]) from a small [`SfxSpec`] — a kind, a length, a pitch, how hard it
//! hits, how much room it has, a seed — so there is nothing to download or license and the same
//! spec is always the same audio. Pure and unit-tested.
//!
//! Every effect has a **hit point** ([`anchor_s`]): the moment that should land on the picture
//! event. A whoosh peaks part-way through, an impact hits within a few milliseconds of its start,
//! a riser builds to its end. Placement says where the *hit* goes (`at`), and the clip starts
//! that much earlier.
//!
//! The spec rides on the asset it produced (`Asset.sound_effect`), which is what lets the effect
//! be reshaped and rendered again.

use serde::{Deserialize, Serialize};

use super::score::MAX_SEED;
use crate::error::{Error, Result};

/// What an effect is, by role in an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SfxCategory {
    /// Moves between shots: whoosh, swoosh, swipe.
    Transition,
    /// Hits that land on a moment: impact, boom, thud, punch.
    Impact,
    /// Tension that builds to (or falls from) a moment: riser, downlifter, reverse swell.
    Build,
    /// Small interface sounds for titles and on-screen things: pop, click, ding, notify, bubble, typewriter.
    Ui,
    /// Colour and character: glitch, tape stop, record scratch, shutter, heartbeat.
    Texture,
}

impl SfxCategory {
    pub const ALL: [SfxCategory; 5] = [
        SfxCategory::Transition,
        SfxCategory::Impact,
        SfxCategory::Build,
        SfxCategory::Ui,
        SfxCategory::Texture,
    ];

    /// The snake_case name used in JSON.
    pub fn name(self) -> &'static str {
        match self {
            SfxCategory::Transition => "transition",
            SfxCategory::Impact => "impact",
            SfxCategory::Build => "build",
            SfxCategory::Ui => "ui",
            SfxCategory::Texture => "texture",
        }
    }

    /// A name for a person.
    pub fn label(self) -> &'static str {
        match self {
            SfxCategory::Transition => "Transitions",
            SfxCategory::Impact => "Impacts",
            SfxCategory::Build => "Builds",
            SfxCategory::Ui => "UI",
            SfxCategory::Texture => "Textures",
        }
    }
}

/// Which sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SfxKind {
    /// Wind-like pass of noise that swells to a peak and falls away, left to right: a cut, a pan, a fast move.
    Whoosh,
    /// A lighter, airier, quicker whoosh with a whistle in it: a sword through the air, a title sliding in.
    Swoosh,
    /// A short bright swish: a lower third, a menu or a page sliding on screen.
    Swipe,
    /// A cinematic hit: a deep sub drop with a noise burst and a room around it.
    Impact,
    /// A long, low explosion-style boom with a falling sub and a rumble.
    Boom,
    /// A short, dry, low thump with no tail.
    Thud,
    /// A tight whack with a crack on top: a punch-in zoom, a hard text hit.
    Punch,
    /// Tension that climbs in pitch and brightness and ends on its peak; its end is the hit.
    Riser,
    /// A riser's opposite: it starts on a hit and falls away in pitch and brightness.
    Downlifter,
    /// A reversed crash and reverb that swells from silence and ends sharply on its peak.
    ReverseSwell,
    /// A short, round bubble pop: a title, sticker or caption appearing.
    Pop,
    /// A crisp interface click: a button, a cursor.
    Click,
    /// A clear bell ding with a short ring: a correct answer, a milestone.
    Ding,
    /// A friendly two-note chime, like a message arriving.
    Notify,
    /// A few rising water-bubble blips: playful, something floating up.
    Bubble,
    /// A run of keystrokes under a line of text that types itself on.
    Typewriter,
    /// A burst of bit-crushed digital stutter: a distortion, a rewind, an error.
    Glitch,
    /// A tape machine losing power: the sound slows and drops in pitch to a stop.
    TapeStop,
    /// A vinyl record scratched back and forth: the comedic "wait, what?".
    RecordScratch,
    /// A camera shutter: two mechanical clicks.
    Shutter,
    /// A low lub-dub heartbeat, repeating when it is long.
    Heartbeat,
}

impl SfxKind {
    pub const ALL: [SfxKind; 21] = [
        SfxKind::Whoosh,
        SfxKind::Swoosh,
        SfxKind::Swipe,
        SfxKind::Impact,
        SfxKind::Boom,
        SfxKind::Thud,
        SfxKind::Punch,
        SfxKind::Riser,
        SfxKind::Downlifter,
        SfxKind::ReverseSwell,
        SfxKind::Pop,
        SfxKind::Click,
        SfxKind::Ding,
        SfxKind::Notify,
        SfxKind::Bubble,
        SfxKind::Typewriter,
        SfxKind::Glitch,
        SfxKind::TapeStop,
        SfxKind::RecordScratch,
        SfxKind::Shutter,
        SfxKind::Heartbeat,
    ];

    /// The snake_case name used in JSON.
    pub fn name(self) -> &'static str {
        match self {
            SfxKind::Whoosh => "whoosh",
            SfxKind::Swoosh => "swoosh",
            SfxKind::Swipe => "swipe",
            SfxKind::Impact => "impact",
            SfxKind::Boom => "boom",
            SfxKind::Thud => "thud",
            SfxKind::Punch => "punch",
            SfxKind::Riser => "riser",
            SfxKind::Downlifter => "downlifter",
            SfxKind::ReverseSwell => "reverse_swell",
            SfxKind::Pop => "pop",
            SfxKind::Click => "click",
            SfxKind::Ding => "ding",
            SfxKind::Notify => "notify",
            SfxKind::Bubble => "bubble",
            SfxKind::Typewriter => "typewriter",
            SfxKind::Glitch => "glitch",
            SfxKind::TapeStop => "tape_stop",
            SfxKind::RecordScratch => "record_scratch",
            SfxKind::Shutter => "shutter",
            SfxKind::Heartbeat => "heartbeat",
        }
    }

    /// The kind named `name` (as [`SfxKind::name`] writes it).
    pub fn parse(name: &str) -> Option<SfxKind> {
        SfxKind::ALL.into_iter().find(|k| k.name() == name.trim())
    }

    /// The catalogue row for this kind.
    pub fn preset(self) -> &'static SfxPreset {
        &CATALOGUE[SfxKind::ALL.iter().position(|k| *k == self).expect("every kind is in ALL")]
    }

    /// A name for a person.
    pub fn label(self) -> &'static str {
        self.preset().label
    }

    pub fn category(self) -> SfxCategory {
        self.preset().category
    }
}

/// Where an effect's hit point is.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SfxAnchor {
    /// A transient `seconds` after the start (impacts, pops, clicks): the sound begins on the hit.
    Hit { seconds: f64 },
    /// The sound peaks `fraction` of the way through (a whoosh passes its listener there).
    Peak { fraction: f64 },
    /// The sound builds to its very end (a riser, a reverse swell): it ends on the hit.
    End,
}

/// One kind of effect in the catalogue: what it is called, what it sounds like, how long it
/// may be, where its hit is and how loud it is meant to sit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, schemars::JsonSchema)]
pub struct SfxPreset {
    pub kind: SfxKind,
    pub label: &'static str,
    pub category: SfxCategory,
    /// What it sounds like and where it belongs in an edit.
    pub description: &'static str,
    /// The length when `duration_s` is not given, seconds.
    pub default_duration_s: f64,
    pub min_duration_s: f64,
    pub max_duration_s: f64,
    /// The hit point, before `reverse` mirrors it.
    pub anchor: SfxAnchor,
    /// The file's peak relative to the −1 dBFS every render is scaled to, dB (0 or less): a
    /// click is not meant to sit as loud as a boom.
    pub level_db: f64,
}

const fn preset(
    kind: SfxKind,
    label: &'static str,
    category: SfxCategory,
    description: &'static str,
    durations: [f64; 3],
    anchor: SfxAnchor,
    level_db: f64,
) -> SfxPreset {
    SfxPreset {
        kind,
        label,
        category,
        description,
        default_duration_s: durations[0],
        min_duration_s: durations[1],
        max_duration_s: durations[2],
        anchor,
        level_db,
    }
}

const fn hit(seconds: f64) -> SfxAnchor {
    SfxAnchor::Hit { seconds }
}

const fn peak(fraction: f64) -> SfxAnchor {
    SfxAnchor::Peak { fraction }
}

use SfxCategory::{Build, Impact, Texture, Transition, Ui};
use SfxKind as K;

/// Every effect Kerf can make, one row per kind in [`SfxKind::ALL`] order.
static CATALOGUE: [SfxPreset; 21] = [
    preset(
        K::Whoosh,
        "Whoosh",
        Transition,
        "Wind-like pass of filtered noise that swells to a peak and falls away while it travels from left to right. The classic sound of a cut, a fast pan or an object flying past; put its peak on the cut.",
        [1.0, 0.3, 3.0],
        peak(0.55),
        -3.0,
    ),
    preset(
        K::Swoosh,
        "Swoosh",
        Transition,
        "A lighter, airier, quicker whoosh with a rising whistle in it, like a sword or a hand cutting the air. Good on a snappy cut or a title sliding in.",
        [0.7, 0.2, 2.0],
        peak(0.45),
        -3.0,
    ),
    preset(
        K::Swipe,
        "Swipe",
        Transition,
        "A short, bright swish for an on-screen swipe: a lower third sliding in, a menu, a page turning.",
        [0.35, 0.12, 1.0],
        peak(0.4),
        -2.5,
    ),
    preset(
        K::Impact,
        "Impact",
        Impact,
        "A cinematic hit: a deep sub drop with a noise burst and a room around it. Lands on a big reveal, a title card, a drop in the music.",
        [1.6, 0.5, 4.0],
        hit(0.004),
        0.0,
    ),
    preset(
        K::Boom,
        "Boom",
        Impact,
        "A long, low explosion-style boom: a sub that falls in pitch and rumbles for seconds. For the biggest moments.",
        [3.0, 1.0, 8.0],
        hit(0.006),
        0.0,
    ),
    preset(
        K::Thud,
        "Thud",
        Impact,
        "A short, dry, low thump, like a soft hit on a chest or a heavy object put down. Punctuates a beat without a tail.",
        [0.5, 0.15, 1.5],
        hit(0.003),
        -4.0,
    ),
    preset(
        K::Punch,
        "Punch",
        Impact,
        "A tight whack with a crack on top: for a punch-in zoom, a jump-cut emphasis, a hard text hit.",
        [0.45, 0.12, 1.2],
        hit(0.002),
        -1.0,
    ),
    preset(
        K::Riser,
        "Riser",
        Build,
        "Tension that climbs in pitch and brightness for its whole length and ends on its peak. Put its end (the anchor) on the moment it builds to: a drop, a reveal, a cut to the punchline.",
        [3.0, 0.8, 12.0],
        SfxAnchor::End,
        -3.0,
    ),
    preset(
        K::Downlifter,
        "Downlifter",
        Build,
        "The opposite of a riser: it starts on a hit and the sound falls away in pitch and brightness. Releases tension after a drop or a big moment.",
        [2.0, 0.6, 8.0],
        hit(0.01),
        -4.0,
    ),
    preset(
        K::ReverseSwell,
        "Reverse swell",
        Build,
        "A reversed crash and reverb that swells from silence and ends sharply on its peak. A softer, more musical riser for a transition or the second before a hit.",
        [2.0, 0.6, 6.0],
        SfxAnchor::End,
        -3.0,
    ),
    preset(
        K::Pop,
        "Pop",
        Ui,
        "A short, round bubble pop. For a title, a sticker or a caption appearing.",
        [0.18, 0.06, 0.6],
        hit(0.003),
        -6.0,
    ),
    preset(
        K::Click,
        "Click",
        Ui,
        "A crisp interface click. For a button press, a cursor, a list item landing.",
        [0.08, 0.03, 0.3],
        hit(0.002),
        -6.0,
    ),
    preset(
        K::Ding,
        "Ding",
        Ui,
        "A clear bell ding with a short ring. For a correct answer, a milestone, a price or number appearing.",
        [1.2, 0.3, 4.0],
        hit(0.002),
        -7.0,
    ),
    preset(
        K::Notify,
        "Notify",
        Ui,
        "A friendly two-note chime, like a message arriving.",
        [0.9, 0.3, 2.5],
        hit(0.002),
        -7.0,
    ),
    preset(
        K::Bubble,
        "Bubble",
        Ui,
        "A few rising water-bubble blips. Playful; for an emoji, a sticker or something floating up.",
        [0.35, 0.1, 1.2],
        hit(0.006),
        -6.0,
    ),
    preset(
        K::Typewriter,
        "Typewriter",
        Ui,
        "A run of keystrokes. Lay it under a title or a line of text that types itself on.",
        [1.0, 0.2, 4.0],
        hit(0.003),
        -6.0,
    ),
    preset(
        K::Glitch,
        "Glitch",
        Texture,
        "A burst of digital stutter: bit-crushed bleeps and noise chopped into repeating slices. For a distortion, a rewind, a tech or error moment.",
        [0.5, 0.15, 2.0],
        hit(0.002),
        -5.0,
    ),
    preset(
        K::TapeStop,
        "Tape stop",
        Texture,
        "A tape machine losing power: the sound slows and drops in pitch to a stop. For a freeze frame or the moment the fun stops.",
        [1.2, 0.4, 3.0],
        hit(0.0),
        -2.5,
    ),
    preset(
        K::RecordScratch,
        "Record scratch",
        Texture,
        "A vinyl record scratched back and forth. The comedic \"wait, what?\" before an unexpected cut.",
        [0.8, 0.3, 2.0],
        hit(0.0),
        -3.0,
    ),
    preset(
        K::Shutter,
        "Shutter",
        Texture,
        "A camera shutter: two mechanical clicks. For a photo being taken or a freeze frame.",
        [0.35, 0.12, 1.0],
        hit(0.002),
        -4.0,
    ),
    preset(
        K::Heartbeat,
        "Heartbeat",
        Texture,
        "A low lub-dub heartbeat, repeating when it is long. For tension or a close call.",
        [1.2, 0.4, 4.0],
        hit(0.006),
        -6.0,
    ),
];

/// Every effect Kerf can make, one row per kind.
pub fn catalogue() -> &'static [SfxPreset] {
    &CATALOGUE
}

/// Where the hit is in a render of `rendered_len` seconds: the moment that should land on the
/// picture event. `reverse` mirrors it (`len - anchor`), since the render plays backwards.
pub fn anchor_s(spec: &SfxSpec, rendered_len: f64) -> f64 {
    let len = rendered_len.max(0.0);
    let at = match spec.kind.preset().anchor {
        SfxAnchor::Hit { seconds } => seconds.min(len),
        SfxAnchor::Peak { fraction } => fraction * len,
        SfxAnchor::End => len,
    };
    if spec.reverse {
        len - at
    } else {
        at
    }
}

/// The default `intensity`.
pub const DEFAULT_SFX_INTENSITY: f64 = 0.7;
/// The default `tail`.
pub const DEFAULT_SFX_TAIL: f64 = 0.3;
/// The most a pitch may move an effect, semitones either way.
pub const MAX_SFX_PITCH: f64 = 12.0;

fn default_intensity() -> f64 {
    DEFAULT_SFX_INTENSITY
}

fn default_tail() -> f64 {
    DEFAULT_SFX_TAIL
}

fn invalid(msg: String) -> Error {
    Error::InvalidArgument(msg)
}

/// What to synthesize. `{"kind":"whoosh"}` alone is a valid spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SfxSpec {
    pub kind: SfxKind,
    /// How long the render is, seconds: the kind's default when absent, clamped to the kind's
    /// own range (`list_sound_effects` shows it). The tail rings inside this length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    /// Transposes the sound, semitones, -12..=12 (default 0).
    #[serde(default)]
    pub pitch: f64,
    /// 0..=1 (default 0.7): brightness, aggression and density.
    #[serde(default = "default_intensity")]
    pub intensity: f64,
    /// 0..=1 (default 0.3): how much reverb / echo rings after the sound.
    #[serde(default = "default_tail")]
    pub tail: f64,
    /// Play the render backwards: a swell into the hit (default false). The hit point mirrors.
    #[serde(default)]
    pub reverse: bool,
    /// Varies the noise and the details; the same spec and seed are the same audio. 0 to
    /// 2^53 - 1 (9007199254740991): the largest integer JSON in JavaScript keeps exactly.
    #[serde(default)]
    #[schemars(range(max = MAX_SEED))]
    pub seed: u64,
}

impl SfxSpec {
    /// The kind at its defaults.
    pub fn new(kind: SfxKind) -> Self {
        Self {
            kind,
            duration_s: None,
            pitch: 0.0,
            intensity: DEFAULT_SFX_INTENSITY,
            tail: DEFAULT_SFX_TAIL,
            reverse: false,
            seed: 0,
        }
    }

    /// Check the numbers; the error names the field.
    pub fn validate(&self) -> Result<()> {
        if let Some(d) = self.duration_s {
            if !d.is_finite() || d <= 0.0 {
                return Err(invalid(format!(
                    "duration_s must be a positive number of seconds (got {d}); it is clamped to {:.2}..={:.2} s for {}",
                    self.kind.preset().min_duration_s,
                    self.kind.preset().max_duration_s,
                    self.kind.name()
                )));
            }
        }
        if !self.pitch.is_finite() || self.pitch.abs() > MAX_SFX_PITCH {
            return Err(invalid(format!(
                "pitch must be -{MAX_SFX_PITCH}..={MAX_SFX_PITCH} semitones (got {})",
                self.pitch
            )));
        }
        if !self.intensity.is_finite() || !(0.0..=1.0).contains(&self.intensity) {
            return Err(invalid(format!("intensity must be 0..=1 (got {})", self.intensity)));
        }
        if !self.tail.is_finite() || !(0.0..=1.0).contains(&self.tail) {
            return Err(invalid(format!("tail must be 0..=1 (got {})", self.tail)));
        }
        if self.seed > MAX_SEED {
            return Err(invalid(format!(
                "seed must be 0..={MAX_SEED} (2^53 - 1, so it survives JSON in JavaScript; got {})",
                self.seed
            )));
        }
        Ok(())
    }

    /// The render's length, seconds: `duration_s` or the kind's default, clamped to the kind's
    /// range and held to whole milliseconds (so it is a whole number of frames at any common
    /// rate, and a spec that says the default is the spec that says nothing).
    pub fn resolved_duration_s(&self) -> f64 {
        let preset = self.kind.preset();
        let wanted = self.duration_s.unwrap_or(preset.default_duration_s);
        let wanted = if wanted.is_finite() {
            wanted
        } else {
            preset.default_duration_s
        };
        ((wanted * 1000.0).round() / 1000.0).clamp(preset.min_duration_s, preset.max_duration_s)
    }

    /// The same spec with its duration spelled out: what is rendered, hashed and stored on the
    /// asset, so two specs that render alike are one spec.
    pub fn resolved(&self) -> SfxSpec {
        SfxSpec {
            duration_s: Some(self.resolved_duration_s()),
            ..self.clone()
        }
    }

    /// A short title for the asset: `SFX: Whoosh`.
    pub fn title(&self) -> String {
        format!("SFX: {}", self.kind.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_has_one_sane_row_per_kind_in_order() {
        assert_eq!(catalogue().len(), SfxKind::ALL.len());
        for (kind, row) in SfxKind::ALL.iter().zip(catalogue()) {
            assert_eq!(row.kind, *kind, "{kind:?}");
            assert_eq!(kind.preset().kind, *kind);
            assert!(
                0.0 < row.min_duration_s
                    && row.min_duration_s <= row.default_duration_s
                    && row.default_duration_s <= row.max_duration_s,
                "{kind:?}: {row:?}"
            );
            assert!(row.level_db <= 0.0 && row.level_db > -24.0, "{kind:?}");
            assert!(row.description.len() > 30 && !row.label.is_empty());
            match row.anchor {
                SfxAnchor::Hit { seconds } => {
                    assert!((0.0..=0.02).contains(&seconds), "{kind:?}: a hit is within a few ms");
                    assert!(seconds < row.min_duration_s);
                }
                SfxAnchor::Peak { fraction } => assert!((0.2..=0.8).contains(&fraction), "{kind:?}"),
                SfxAnchor::End => {}
            }
        }
        let mut names: Vec<_> = SfxKind::ALL.iter().map(|k| k.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 21);
        for kind in SfxKind::ALL {
            assert_eq!(SfxKind::parse(kind.name()), Some(kind));
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{}\"", kind.name()));
        }
        // Every category has kinds, and the groups the spec names are where they belong.
        for category in SfxCategory::ALL {
            assert!(SfxKind::ALL.iter().any(|k| k.category() == category), "{category:?}");
        }
        assert_eq!(SfxKind::Riser.category(), SfxCategory::Build);
        assert_eq!(SfxKind::Typewriter.category(), SfxCategory::Ui);
        assert_eq!(SfxKind::Heartbeat.category(), SfxCategory::Texture);
    }

    #[test]
    fn a_kind_alone_is_a_spec_and_a_misspelled_field_is_an_error() {
        let spec: SfxSpec = serde_json::from_str(r#"{"kind":"whoosh"}"#).unwrap();
        assert_eq!(spec, SfxSpec::new(SfxKind::Whoosh));
        assert_eq!(spec.intensity, 0.7);
        assert_eq!(spec.tail, 0.3);
        let spec: SfxSpec = serde_json::from_str(
            r#"{"kind":"tape_stop","duration_s":2,"pitch":-3,"intensity":0.2,"tail":0,"reverse":true,"seed":9}"#,
        )
        .unwrap();
        assert_eq!(
            (spec.duration_s, spec.pitch, spec.reverse, spec.seed),
            (Some(2.0), -3.0, true, 9)
        );
        let e = serde_json::from_str::<SfxSpec>(r#"{"kind":"whoosh","pitc":2}"#)
            .unwrap_err()
            .to_string();
        assert!(e.contains("unknown field `pitc`"), "{e}");
        assert!(serde_json::from_str::<SfxSpec>(r#"{"kind":"zap"}"#).is_err());
        assert!(serde_json::from_str::<SfxSpec>(r#"{}"#).is_err(), "a kind is required");
        // The default spec writes no duration, and what it writes reads back.
        let json = serde_json::to_string(&SfxSpec::new(SfxKind::Pop)).unwrap();
        assert!(!json.contains("duration_s"), "{json}");
        assert_eq!(serde_json::from_str::<SfxSpec>(&json).unwrap(), SfxSpec::new(SfxKind::Pop));
    }

    #[test]
    fn validation_names_the_bad_field() {
        let bad = |edit: fn(&mut SfxSpec)| {
            let mut s = SfxSpec::new(SfxKind::Impact);
            edit(&mut s);
            s.validate().unwrap_err().to_string()
        };
        assert!(SfxSpec::new(SfxKind::Impact).validate().is_ok());
        assert!(bad(|s| s.duration_s = Some(0.0)).contains("duration_s"));
        assert!(bad(|s| s.duration_s = Some(f64::NAN)).contains("duration_s"));
        assert!(bad(|s| s.duration_s = Some(-1.0)).contains("duration_s"));
        assert!(bad(|s| s.pitch = 12.5).contains("pitch"));
        assert!(bad(|s| s.pitch = f64::INFINITY).contains("pitch"));
        assert!(bad(|s| s.intensity = 1.5).contains("intensity"));
        assert!(bad(|s| s.intensity = -0.1).contains("intensity"));
        assert!(bad(|s| s.tail = f64::NAN).contains("tail"));
        assert!(bad(|s| s.seed = MAX_SEED + 1).contains("seed"));
        let mut edge = SfxSpec::new(SfxKind::Impact);
        edge.pitch = -12.0;
        edge.intensity = 0.0;
        edge.tail = 1.0;
        edge.seed = MAX_SEED;
        edge.validate().unwrap();
        // A seed that does not survive a JavaScript number is not accepted either way round.
        assert_eq!(MAX_SEED as f64 as u64, MAX_SEED);
    }

    #[test]
    fn the_length_is_the_default_clamped_to_the_kinds_range_in_whole_milliseconds() {
        let mut s = SfxSpec::new(SfxKind::Whoosh);
        assert_eq!(s.resolved_duration_s(), 1.0);
        s.duration_s = Some(10.0);
        assert_eq!(s.resolved_duration_s(), 3.0, "a whoosh is at most 3 s");
        s.duration_s = Some(0.01);
        assert_eq!(s.resolved_duration_s(), 0.3);
        s.duration_s = Some(0.4567891);
        assert_eq!(s.resolved_duration_s(), 0.457);
        // Spelling out the default is the same spec as saying nothing.
        let mut spelled = SfxSpec::new(SfxKind::Whoosh);
        spelled.duration_s = Some(1.0);
        assert_eq!(spelled.resolved(), SfxSpec::new(SfxKind::Whoosh).resolved());
        assert_eq!(spelled.resolved().resolved(), spelled.resolved());
        for kind in SfxKind::ALL {
            let r = SfxSpec::new(kind).resolved_duration_s();
            assert_eq!(r, kind.preset().default_duration_s, "{kind:?}");
            assert!(
                ((r * 48_000.0) - (r * 48_000.0).round()).abs() < 1e-6,
                "{kind:?}: a whole number of frames"
            );
        }
    }

    #[test]
    fn the_hit_point_is_where_the_kind_says_and_reverse_mirrors_it() {
        let at = |kind, len, reverse| {
            let mut s = SfxSpec::new(kind);
            s.reverse = reverse;
            anchor_s(&s, len)
        };
        assert!((at(SfxKind::Whoosh, 2.0, false) - 1.1).abs() < 1e-9, "55 % in");
        assert!((at(SfxKind::Whoosh, 2.0, true) - 0.9).abs() < 1e-9);
        assert!((at(SfxKind::Impact, 1.6, false) - 0.004).abs() < 1e-9);
        assert!(
            (at(SfxKind::Impact, 1.6, true) - 1.596).abs() < 1e-9,
            "a reversed impact ends on the hit"
        );
        assert_eq!(at(SfxKind::Riser, 3.0, false), 3.0, "a riser builds to its end");
        assert_eq!(at(SfxKind::Riser, 3.0, true), 0.0);
        assert_eq!(at(SfxKind::TapeStop, 1.0, false), 0.0);
        assert!(at(SfxKind::Click, 0.001, false) <= 0.001, "never past the end");
        for kind in SfxKind::ALL {
            let len = kind.preset().default_duration_s;
            let a = at(kind, len, false);
            let b = at(kind, len, true);
            assert!((0.0..=len).contains(&a) && (0.0..=len).contains(&b), "{kind:?}");
            assert!((a + b - len).abs() < 1e-9, "{kind:?}: mirrored");
        }
    }
}
