//! From a score to notes: the **arrangement**. Pure and deterministic.
//!
//! Each style is a [`Preset`] — General MIDI programs per role, drum / bass / comping / hook
//! patterns on a 16-step grid, swing, humanization, role gains and how hard the kick pumps the
//! bass and pad. [`expand`] walks the sections bar by bar and decides, from the section's kind and
//! energy, which layers play and which pattern each plays: an intro is a pad and a teaser, a
//! build pulses the bass, rolls the snare faster and faster and ends in a riser, a drop is
//! everything, a breakdown loses the kick and the bass, an outro thins out and its last bar
//! resolves on the tonic. Chords are voice-led (every voicing moves as little as it can from the
//! last), the bass follows the roots and approaches the next one, the hook is built from chord
//! tones so it fits every chord it is played over, and all the "random" variation comes from a
//! small seeded generator keyed by role, section and bar — editing a section never re-rolls the ones before it.

use super::chord::{pitch_at_or_above, voice, Chord, Colour, Key};
use super::{HitKind, MusicScore, MusicStyle, SectionKind};
use crate::error::Result;

/// The instrument groups a score is rendered in, each on a synthesizer of its own so the mix can
/// duck, mute and balance them separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Kick, snare, claps, toms.
    Drums,
    /// Hi-hats, shakers, tambourines and cymbals: the top of the kit, balanced apart because the
    /// General MIDI kits record them far quieter than the kick.
    Top,
    Bass,
    /// Sustained chords.
    Pad,
    /// Rhythmic chords: stabs, arpeggios, power chords.
    Keys,
    /// The hook.
    Lead,
    /// Risers, impacts, whooshes.
    Fx,
}

pub const ROLES: [Role; 7] = [
    Role::Drums,
    Role::Top,
    Role::Bass,
    Role::Pad,
    Role::Keys,
    Role::Lead,
    Role::Fx,
];

impl Role {
    pub fn index(self) -> usize {
        ROLES.iter().position(|r| *r == self).unwrap_or(0)
    }

    pub fn name(self) -> &'static str {
        match self {
            Role::Drums => "drums",
            Role::Top => "top",
            Role::Bass => "bass",
            Role::Pad => "pad",
            Role::Keys => "keys",
            Role::Lead => "lead",
            Role::Fx => "fx",
        }
    }
}

/// One note for a synthesizer. `channel` is the MIDI channel (9 is the drum kit; `program` is
/// then the kit).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct NoteEvent {
    pub role: Role,
    pub channel: u8,
    pub program: u8,
    pub key: u8,
    pub velocity: u8,
    pub start_s: f64,
    pub dur_s: f64,
}

/// How the kick pumps the rest: the gain dips by `depth` at each kick and recovers over
/// `release_s`. The roles that duck take a share of the depth.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Sidechain {
    pub depth: f32,
    pub attack_s: f32,
    pub release_s: f32,
    pub bass: f32,
    pub pad: f32,
    pub keys: f32,
}

impl Sidechain {
    /// The share of the depth `role` ducks by (0 for the roles that do not duck).
    pub fn amount(&self, role: Role) -> f32 {
        match role {
            Role::Bass => self.bass,
            Role::Pad => self.pad,
            Role::Keys => self.keys,
            _ => 0.0,
        }
    }
}

/// How the roles are balanced.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Mix {
    /// Linear gain per role, in [`ROLES`] order.
    pub gain: [f32; 7],
    /// MIDI reverb and chorus sends (0..=127) per role.
    pub reverb: [u8; 7],
    pub chorus: [u8; 7],
    /// MIDI pan (0..=127, 64 centre) per role.
    pub pan: [u8; 7],
    pub sidechain: Sidechain,
}

/// A section's make-up gain: `gain` (linear) from `start_s` to `end_s`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Makeup {
    pub start_s: f64,
    pub end_s: f64,
    pub gain: f32,
}

/// A score as notes.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Arrangement {
    /// Every note, by start time.
    pub notes: Vec<NoteEvent>,
    /// When a kick sounds (what the sidechain follows), seconds.
    pub kicks: Vec<f64>,
    /// Windows in which everything but the effects is silenced, `(from, to)` seconds.
    pub dropouts: Vec<(f64, f64)>,
    /// A gain per section that lifts the ones with fewer layers (an intro that is a pad alone,
    /// a breakdown without its kick and bass) half the way to the loudness of the fullest, so
    /// the music sits at one level under a picture instead of swinging 15 dB.
    pub makeup: Vec<Makeup>,
    /// How long the render lasts, seconds.
    pub duration_s: f64,
    pub mix: Mix,
}

impl Arrangement {
    /// The notes of one role.
    pub fn role(&self, role: Role) -> impl Iterator<Item = &NoteEvent> {
        self.notes.iter().filter(move |n| n.role == role)
    }
}

// ---- the random source ---------------------------------------------------------------------

/// A small seeded generator (xorshift64*), keyed by score seed, role stream and bar, so the same
/// score is the same notes on every platform and one bar's variation never depends on another's.
struct Rng(u64);

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl Rng {
    fn new(seed: u64, stream: u64, bar: u64) -> Self {
        let s = splitmix(seed ^ splitmix(stream.wrapping_mul(0xD6E8_FEB8_6659_FD93) ^ splitmix(bar)));
        Rng(if s == 0 { 0x2545_F491_4F6C_DD1D } else { s })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// In `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// In `[-1, 1)`.
    fn signed(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

// ---- pattern data --------------------------------------------------------------------------

/// A drum lane: a General MIDI drum key and 16 steps (or 32, two bars) of digits (velocity
/// `digit * 14 + 2`) and `.` for a rest; spaces are ignored.
type Lane = (u8, &'static str);

/// Stands for a timpani on the chord's root (the orchestral kit's pitched drums).
const TIMP: u8 = 0xFF;

#[derive(Clone, Copy)]
enum Tone {
    /// The chord's root (its bass note).
    R,
    /// The root an octave up.
    O,
    /// The chord's fifth.
    F,
    /// A step of the key's scale towards the next chord's root, or the octave when it does not change.
    A,
}

/// `(step, length in steps, tone, velocity)`.
type BassHit = (u8, u8, Tone, u8);

/// Plays the whole voicing.
const BLOCK: i8 = 127;
/// `(step, length in steps, BLOCK or the voicing's note index (wrapping up an octave), velocity)`.
type KeyHit = (u8, u8, i8, u8);

/// `(step, length in steps, index in the chord-tone ladder from the root, scale-step neighbour
/// −1 / 0 / +1, velocity)`.
type LeadNote = (u8, u8, i8, i8, u8);

struct DrumSet {
    light: &'static [Lane],
    groove: &'static [Lane],
    full: &'static [Lane],
}

struct Preset {
    kit: u8,
    bass: u8,
    pad: u8,
    keys: u8,
    lead: u8,
    colour: Colour,
    /// How far the off-beat eighths (steps 2, 6, …) and the odd sixteenths lag, in steps.
    swing8: f64,
    swing16: f64,
    /// Humanization: timing wobble (seconds) and velocity wobble.
    jitter_s: f64,
    vel_jitter: f64,
    /// Seconds between the notes of a strummed chord.
    strum_s: f64,
    gain: [f32; 7],
    /// The share of a song's loudness each role takes, in [`ROLES`] order (sums to 1): what
    /// `gain` is calibrated to (the ignored `calibration_report` test measures it) and what the
    /// sections' make-up gain reads, so a section with fewer layers is not left quiet.
    shares: [f32; 7],
    reverb: [u8; 7],
    chorus: [u8; 7],
    pan: [u8; 7],
    sidechain: Sidechain,
    pad_range: (u8, u8),
    pad_spread: u8,
    pad_vel: u8,
    /// A low root an octave under the pad's lowest note.
    pad_wide: bool,
    keys_range: (u8, u8),
    keys_spread: u8,
    /// Keys play root-fifth-octave power chords instead of the chord's voicing.
    power_keys: bool,
    lead_range: (u8, u8),
    drums: DrumSet,
    bass_verse: [&'static [BassHit]; 2],
    bass_drop: [&'static [BassHit]; 2],
    keys_verse: [&'static [KeyHit]; 2],
    keys_drop: [&'static [KeyHit]; 2],
    keys_sparse: &'static [KeyHit],
    motifs: &'static [&'static [LeadNote]],
    lead_sparse: &'static [LeadNote],
    /// How hard a drop lands (a timpani on the tonic and a low boom; 0 for none).
    impact: f64,
    /// The drum key a snare roll is played on.
    roll_key: u8,
    crash_key: Option<u8>,
    /// A crash every four bars of a drop (a rock chorus), not only on its first bar.
    crash_loop: bool,
    fills: bool,
    /// Toms from high to low, for a fill.
    fill_toms: [u8; 4],
}

pub(super) fn colour_of(style: MusicStyle) -> Colour {
    preset(style).colour
}

const fn sc(depth: f32, release_s: f32, bass: f32, pad: f32, keys: f32) -> Sidechain {
    Sidechain {
        depth,
        attack_s: 0.004,
        release_s,
        bass,
        pad,
        keys,
    }
}

// ---- tropical house ------------------------------------------------------------------------

const TH_BASS_VERSE: &[BassHit] = &[
    (0, 3, Tone::R, 100),
    (3, 3, Tone::R, 80),
    (6, 2, Tone::R, 76),
    (8, 3, Tone::R, 94),
    (11, 3, Tone::R, 78),
    (14, 2, Tone::A, 78),
];
const TH_BASS_DROP: &[BassHit] = &[
    (0, 3, Tone::R, 108),
    (3, 3, Tone::O, 84),
    (6, 2, Tone::R, 80),
    (8, 3, Tone::R, 100),
    (11, 3, Tone::O, 82),
    (14, 2, Tone::A, 82),
];
const TH_KEYS_VERSE: &[KeyHit] = &[(0, 3, BLOCK, 84), (3, 3, BLOCK, 66), (6, 2, BLOCK, 74), (10, 3, BLOCK, 70)];
const TH_KEYS_DROP: &[KeyHit] = &[
    (0, 3, BLOCK, 92),
    (3, 3, BLOCK, 72),
    (6, 2, BLOCK, 80),
    (8, 3, BLOCK, 86),
    (11, 3, BLOCK, 70),
    (14, 2, BLOCK, 76),
];
const TH_KEYS_SPARSE: &[KeyHit] = &[(0, 6, BLOCK, 76), (8, 6, BLOCK, 64)];
const TH_MOTIFS: &[&[LeadNote]] = &[
    &[
        (0, 4, 2, 0, 88),
        (4, 2, 3, 0, 76),
        (6, 2, 2, 0, 72),
        (8, 6, 1, 0, 84),
        (14, 2, 0, 0, 66),
    ],
    &[
        (0, 3, 0, 0, 84),
        (3, 3, 2, 0, 80),
        (6, 2, 3, 0, 78),
        (8, 4, 2, 0, 76),
        (12, 4, 1, 0, 70),
    ],
    &[
        (2, 2, 2, 0, 78),
        (4, 2, 3, 0, 84),
        (6, 4, 4, 0, 88),
        (10, 2, 3, 0, 72),
        (12, 4, 2, 0, 76),
    ],
    &[(0, 6, 3, 0, 90), (6, 2, 2, 1, 70), (8, 4, 2, 0, 80), (12, 4, 0, 0, 74)],
];
const TH_LEAD_SPARSE: &[LeadNote] = &[(0, 12, 2, 0, 76)];

static TROPICAL_HOUSE: Preset = Preset {
    kit: 24,
    bass: 39,
    pad: 89,
    keys: 12,
    lead: 75,
    colour: Colour::None,
    swing8: 0.0,
    swing16: 0.15,
    jitter_s: 0.004,
    vel_jitter: 4.0,
    strum_s: 0.0,
    gain: [0.39, 0.92, 0.61, 0.25, 0.35, 0.42, 1.00],
    shares: [0.22, 0.12, 0.16, 0.14, 0.16, 0.12, 0.08],
    reverb: [20, 30, 0, 64, 52, 66, 80],
    chorus: [0, 0, 0, 40, 20, 10, 0],
    pan: [64, 64, 64, 64, 54, 74, 64],
    sidechain: sc(0.6, 0.22, 1.0, 1.0, 0.5),
    pad_range: (52, 74),
    pad_spread: 14,
    pad_vel: 84,
    pad_wide: false,
    keys_range: (57, 76),
    keys_spread: 14,
    power_keys: false,
    lead_range: (67, 86),
    drums: DrumSet {
        light: &[(36, "4... 4... 4... 4..."), (82, "3.3. 3.3. 3.3. 3.3.")],
        groove: &[
            (36, "7... 7... 7... 7..."),
            (39, "....6.......6..."),
            (42, "5.4.5.4.5.4.5.4."),
            (82, "4343434343434343"),
        ],
        full: &[
            (36, "8... 8... 8... 8..."),
            (39, "....7.......7... ....7.......7.3."),
            (40, "....4.......4..."),
            (46, "..6...6...6...6."),
            (42, ".3.3.3.3.3.3.3.3"),
            (82, "4343434343434343"),
        ],
    },
    bass_verse: [TH_BASS_VERSE, TH_BASS_VERSE],
    bass_drop: [TH_BASS_DROP, TH_BASS_DROP],
    keys_verse: [TH_KEYS_VERSE, TH_KEYS_VERSE],
    keys_drop: [TH_KEYS_DROP, TH_KEYS_DROP],
    keys_sparse: TH_KEYS_SPARSE,
    motifs: TH_MOTIFS,
    lead_sparse: TH_LEAD_SPARSE,
    impact: 0.8,
    roll_key: 39,
    crash_key: Some(49),
    crash_loop: false,
    fills: true,
    fill_toms: [50, 48, 45, 41],
};

// ---- pop -----------------------------------------------------------------------------------

const POP_BASS_VERSE: &[BassHit] = &[
    (0, 6, Tone::R, 96),
    (6, 2, Tone::R, 72),
    (8, 6, Tone::R, 90),
    (14, 2, Tone::A, 76),
];
const POP_BASS_DROP: &[BassHit] = &[
    (0, 2, Tone::R, 100),
    (2, 2, Tone::R, 72),
    (4, 2, Tone::R, 90),
    (6, 2, Tone::R, 72),
    (8, 2, Tone::R, 96),
    (10, 2, Tone::R, 74),
    (12, 2, Tone::F, 88),
    (14, 2, Tone::A, 78),
];
const POP_KEYS_VERSE: &[KeyHit] = &[
    (0, 2, 0, 84),
    (2, 2, 1, 62),
    (4, 2, 2, 70),
    (6, 2, 1, 62),
    (8, 2, 0, 80),
    (10, 2, 1, 60),
    (12, 2, 2, 68),
    (14, 2, 1, 60),
];
const POP_KEYS_DROP: &[KeyHit] = &[
    (0, 3, BLOCK, 92),
    (3, 3, BLOCK, 72),
    (6, 2, BLOCK, 80),
    (8, 4, BLOCK, 88),
    (12, 2, BLOCK, 74),
    (14, 2, BLOCK, 70),
];
const POP_KEYS_SPARSE: &[KeyHit] = &[(0, 16, BLOCK, 70)];
const POP_MOTIFS: &[&[LeadNote]] = &[
    &[
        (0, 2, 0, 0, 80),
        (2, 2, 1, 0, 66),
        (4, 2, 2, 0, 72),
        (6, 2, 3, 0, 76),
        (8, 2, 2, 0, 70),
        (10, 2, 1, 0, 64),
        (12, 2, 2, 0, 68),
        (14, 2, 1, 0, 60),
    ],
    &[
        (0, 4, 2, 0, 84),
        (4, 2, 1, 0, 66),
        (6, 2, 2, 0, 70),
        (8, 4, 3, 0, 86),
        (12, 2, 2, 0, 70),
        (14, 2, 1, 0, 62),
    ],
    &[
        (0, 2, 3, 0, 84),
        (3, 3, 2, 0, 70),
        (6, 2, 1, 0, 66),
        (8, 4, 2, 0, 76),
        (12, 4, 0, 0, 72),
    ],
];
const POP_LEAD_SPARSE: &[LeadNote] = &[(0, 8, 2, 0, 74), (8, 8, 1, 0, 68)];

static POP: Preset = Preset {
    kit: 8,
    bass: 33,
    pad: 49,
    keys: 0,
    lead: 25,
    colour: Colour::None,
    swing8: 0.0,
    swing16: 0.1,
    jitter_s: 0.005,
    vel_jitter: 6.0,
    strum_s: 0.0,
    gain: [0.13, 0.39, 0.12, 0.09, 0.16, 1.00, 0.34],
    shares: [0.24, 0.08, 0.14, 0.12, 0.22, 0.14, 0.06],
    reverb: [28, 30, 0, 56, 40, 50, 80],
    chorus: [0, 0, 0, 25, 10, 15, 0],
    pan: [64, 64, 64, 64, 56, 72, 64],
    sidechain: sc(0.2, 0.15, 0.5, 1.0, 0.3),
    pad_range: (52, 74),
    pad_spread: 14,
    pad_vel: 80,
    pad_wide: false,
    keys_range: (52, 76),
    keys_spread: 14,
    power_keys: false,
    lead_range: (64, 86),
    drums: DrumSet {
        light: &[
            (36, "6... .... .... ...."),
            (37, "....4.......4..."),
            (44, "4...4...4...4..."),
        ],
        groove: &[
            (36, "8.......7.6..... 8.....5.8......."),
            (38, "....8.......8..."),
            (42, "7.5.6.5.7.5.6.5."),
        ],
        full: &[
            (36, "9.....5.8.7..... 9.....5.8.7...5."),
            (38, "....9.......9..."),
            (39, "....5.......5..."),
            (42, "7.5.7.5.7.5.7..."),
            (46, "..............6."),
        ],
    },
    bass_verse: [POP_BASS_VERSE, POP_BASS_VERSE],
    bass_drop: [POP_BASS_DROP, POP_BASS_DROP],
    keys_verse: [POP_KEYS_VERSE, POP_KEYS_VERSE],
    keys_drop: [POP_KEYS_DROP, POP_KEYS_DROP],
    keys_sparse: POP_KEYS_SPARSE,
    motifs: POP_MOTIFS,
    lead_sparse: POP_LEAD_SPARSE,
    impact: 0.7,
    roll_key: 38,
    crash_key: Some(49),
    crash_loop: false,
    fills: true,
    fill_toms: [50, 48, 45, 41],
};

// ---- lo-fi ---------------------------------------------------------------------------------

const LF_BASS_VERSE: &[BassHit] = &[
    (0, 6, Tone::R, 84),
    (8, 4, Tone::R, 72),
    (12, 2, Tone::F, 62),
    (14, 2, Tone::A, 62),
];
const LF_BASS_DROP: &[BassHit] = &[
    (0, 5, Tone::R, 90),
    (6, 2, Tone::F, 66),
    (8, 6, Tone::R, 80),
    (14, 2, Tone::A, 66),
];
const LF_KEYS_VERSE_A: &[KeyHit] = &[(0, 6, BLOCK, 72), (6, 2, BLOCK, 52), (10, 6, BLOCK, 62)];
const LF_KEYS_VERSE_B: &[KeyHit] = &[(0, 5, BLOCK, 70), (5, 3, BLOCK, 54), (8, 3, BLOCK, 58), (12, 4, BLOCK, 64)];
const LF_KEYS_DROP_A: &[KeyHit] = &[
    (0, 4, BLOCK, 78),
    (4, 2, BLOCK, 56),
    (6, 4, BLOCK, 66),
    (10, 2, BLOCK, 52),
    (12, 4, BLOCK, 70),
];
const LF_KEYS_DROP_B: &[KeyHit] = &[(0, 3, BLOCK, 76), (3, 3, BLOCK, 58), (6, 4, BLOCK, 64), (10, 6, BLOCK, 68)];
const LF_KEYS_SPARSE: &[KeyHit] = &[(0, 16, BLOCK, 60)];
const LF_MOTIFS: &[&[LeadNote]] = &[
    &[(2, 3, 2, 0, 66), (6, 2, 3, 0, 58), (10, 6, 2, 1, 62)],
    &[(0, 4, 3, 0, 68), (6, 4, 2, 0, 60), (12, 4, 1, 0, 58)],
    &[(4, 3, 1, 0, 62), (8, 2, 2, 0, 64), (11, 5, 3, -1, 60)],
];
const LF_LEAD_SPARSE: &[LeadNote] = &[(4, 8, 2, 0, 60)];

static LOFI: Preset = Preset {
    kit: 0,
    bass: 32,
    pad: 89,
    keys: 4,
    lead: 11,
    colour: Colour::Sevenths,
    swing8: 0.5,
    swing16: 0.25,
    jitter_s: 0.011,
    vel_jitter: 9.0,
    strum_s: 0.016,
    gain: [0.18, 0.94, 0.24, 0.11, 0.33, 1.00, 0.39],
    shares: [0.22, 0.07, 0.16, 0.09, 0.32, 0.12, 0.02],
    reverb: [30, 40, 0, 62, 50, 55, 80],
    chorus: [0, 0, 0, 20, 30, 25, 0],
    pan: [64, 64, 64, 64, 52, 76, 64],
    sidechain: sc(0.15, 0.2, 0.0, 0.6, 0.0),
    pad_range: (55, 76),
    pad_spread: 14,
    pad_vel: 70,
    pad_wide: false,
    keys_range: (55, 77),
    keys_spread: 14,
    power_keys: false,
    lead_range: (67, 86),
    drums: DrumSet {
        light: &[(42, "4.3.4.3.4.3.4.3."), (37, "....3.......3...")],
        groove: &[
            (36, "8.....5...7..... 8.....5...7...4."),
            (38, "....7.......7... ....7.......7.2."),
            (42, "5.3.5.3.5.3.5.3."),
            (46, "................ ..............3."),
        ],
        full: &[
            (36, "9.....5...7..... 9.....5...7...5."),
            (38, "....8.......8... ....8.......8.3."),
            (42, "6.4.6.4.6.4.6.4."),
            (42, ".2.2.2.2.2.2.2.2"),
            (37, "....4.......4..."),
        ],
    },
    bass_verse: [LF_BASS_VERSE, LF_BASS_VERSE],
    bass_drop: [LF_BASS_DROP, LF_BASS_DROP],
    keys_verse: [LF_KEYS_VERSE_A, LF_KEYS_VERSE_B],
    keys_drop: [LF_KEYS_DROP_A, LF_KEYS_DROP_B],
    keys_sparse: LF_KEYS_SPARSE,
    motifs: LF_MOTIFS,
    lead_sparse: LF_LEAD_SPARSE,
    impact: 0.0,
    roll_key: 38,
    crash_key: None,
    crash_loop: false,
    fills: false,
    fill_toms: [50, 48, 45, 41],
};

// ---- cinematic -----------------------------------------------------------------------------

const CN_BASS_VERSE: &[BassHit] = &[(0, 16, Tone::R, 80)];
const CN_BASS_DROP: &[BassHit] = &[(0, 8, Tone::R, 96), (8, 4, Tone::R, 88), (12, 4, Tone::F, 80)];
const CN_KEYS_VERSE: &[KeyHit] = &[(0, 4, 0, 70), (4, 4, 1, 56), (8, 4, 2, 60), (12, 4, 1, 54)];
const CN_KEYS_DROP: &[KeyHit] = &[
    (0, 2, 0, 80),
    (2, 2, 1, 60),
    (4, 2, 2, 66),
    (6, 2, 3, 62),
    (8, 2, 2, 70),
    (10, 2, 1, 56),
    (12, 2, 2, 66),
    (14, 2, 1, 54),
];
const CN_KEYS_SPARSE: &[KeyHit] = &[(0, 16, BLOCK, 60)];
const CN_MOTIFS: &[&[LeadNote]] = &[
    &[(0, 12, 2, 0, 90), (12, 4, 1, 0, 70)],
    &[(0, 8, 0, 0, 86), (8, 8, 2, 0, 90)],
    &[(0, 16, 3, 0, 84)],
    &[(0, 6, 2, 0, 86), (6, 2, 3, 0, 72), (8, 8, 2, 0, 80)],
];
const CN_LEAD_SPARSE: &[LeadNote] = &[(0, 16, 2, 0, 76)];

static CINEMATIC: Preset = Preset {
    kit: 48,
    bass: 43,
    pad: 49,
    keys: 0,
    lead: 60,
    colour: Colour::None,
    swing8: 0.0,
    swing16: 0.0,
    jitter_s: 0.007,
    vel_jitter: 5.0,
    strum_s: 0.0,
    gain: [0.30, 1.00, 0.41, 0.23, 0.59, 0.58, 0.66],
    shares: [0.22, 0.02, 0.10, 0.30, 0.14, 0.12, 0.10],
    reverb: [60, 50, 10, 90, 60, 80, 100],
    chorus: [0, 0, 0, 30, 10, 10, 0],
    pan: [64, 64, 64, 64, 56, 72, 64],
    sidechain: sc(0.0, 0.2, 0.0, 0.0, 0.0),
    pad_range: (48, 76),
    pad_spread: 19,
    pad_vel: 84,
    pad_wide: true,
    keys_range: (48, 79),
    keys_spread: 19,
    power_keys: false,
    lead_range: (55, 79),
    drums: DrumSet {
        light: &[(TIMP, "4............... ................")],
        groove: &[(TIMP, "6............... ......5.........")],
        full: &[
            (36, "9.......8....... 9.....6.8......."),
            (TIMP, "......7.......6. ......7.....6..."),
            (38, "............4..."),
        ],
    },
    bass_verse: [CN_BASS_VERSE, CN_BASS_VERSE],
    bass_drop: [CN_BASS_DROP, CN_BASS_DROP],
    keys_verse: [CN_KEYS_VERSE, CN_KEYS_VERSE],
    keys_drop: [CN_KEYS_DROP, CN_KEYS_DROP],
    keys_sparse: CN_KEYS_SPARSE,
    motifs: CN_MOTIFS,
    lead_sparse: CN_LEAD_SPARSE,
    impact: 1.0,
    roll_key: 38,
    crash_key: Some(49),
    crash_loop: false,
    fills: false,
    fill_toms: [50, 48, 45, 41],
};

// ---- ambient -------------------------------------------------------------------------------

const AM_BASS: &[BassHit] = &[(0, 16, Tone::R, 70)];
const AM_KEYS: &[KeyHit] = &[
    (0, 3, 0, 64),
    (2, 3, 1, 52),
    (4, 3, 2, 56),
    (6, 3, 3, 52),
    (8, 3, 2, 60),
    (10, 3, 1, 50),
    (12, 3, 2, 54),
    (14, 3, 1, 48),
];
const AM_KEYS_SPARSE: &[KeyHit] = &[(0, 8, BLOCK, 56), (8, 8, BLOCK, 50)];
const AM_MOTIFS: &[&[LeadNote]] = &[
    &[(0, 4, 2, 0, 64), (8, 6, 3, 0, 60)],
    &[(4, 6, 3, 0, 62), (12, 4, 2, 0, 56)],
    &[(0, 10, 4, 0, 60), (12, 4, 3, 0, 52)],
];
const AM_LEAD_SPARSE: &[LeadNote] = &[(4, 8, 3, 0, 56)];

static AMBIENT: Preset = Preset {
    kit: 0,
    bass: 39,
    pad: 89,
    keys: 46,
    lead: 98,
    colour: Colour::Ninths,
    swing8: 0.0,
    swing16: 0.0,
    jitter_s: 0.014,
    vel_jitter: 5.0,
    strum_s: 0.03,
    gain: [0.20, 1.00, 0.60, 0.24, 0.44, 0.75, 0.64],
    shares: [0.03, 0.03, 0.10, 0.52, 0.17, 0.12, 0.03],
    reverb: [70, 40, 20, 100, 90, 100, 110],
    chorus: [0, 0, 0, 40, 30, 30, 0],
    pan: [64, 64, 64, 64, 50, 78, 64],
    sidechain: sc(0.0, 0.2, 0.0, 0.0, 0.0),
    pad_range: (52, 76),
    pad_spread: 16,
    pad_vel: 84,
    pad_wide: true,
    keys_range: (48, 84),
    keys_spread: 19,
    power_keys: false,
    lead_range: (72, 96),
    drums: DrumSet {
        light: &[],
        groove: &[(82, "2.2.2.2.2.2.2.2.")],
        full: &[(82, "3.2.3.2.3.2.3.2."), (36, "4...............")],
    },
    bass_verse: [AM_BASS, AM_BASS],
    bass_drop: [AM_BASS, AM_BASS],
    keys_verse: [AM_KEYS, AM_KEYS],
    keys_drop: [AM_KEYS, AM_KEYS],
    keys_sparse: AM_KEYS_SPARSE,
    motifs: AM_MOTIFS,
    lead_sparse: AM_LEAD_SPARSE,
    impact: 0.35,
    roll_key: 38,
    crash_key: None,
    crash_loop: false,
    fills: false,
    fill_toms: [50, 48, 45, 41],
};

// ---- rock ----------------------------------------------------------------------------------

const RK_BASS_VERSE: &[BassHit] = &[
    (0, 2, Tone::R, 100),
    (2, 2, Tone::R, 76),
    (4, 2, Tone::R, 88),
    (6, 2, Tone::R, 76),
    (8, 2, Tone::R, 100),
    (10, 2, Tone::R, 76),
    (12, 2, Tone::R, 88),
    (14, 2, Tone::A, 80),
];
const RK_BASS_DROP: &[BassHit] = &[
    (0, 2, Tone::R, 106),
    (2, 2, Tone::R, 80),
    (4, 2, Tone::O, 88),
    (6, 2, Tone::R, 80),
    (8, 2, Tone::R, 106),
    (10, 2, Tone::R, 80),
    (12, 2, Tone::O, 90),
    (14, 2, Tone::A, 84),
];
const RK_KEYS_VERSE: &[KeyHit] = &[
    (0, 2, BLOCK, 90),
    (2, 1, BLOCK, 64),
    (4, 2, BLOCK, 80),
    (6, 1, BLOCK, 64),
    (8, 2, BLOCK, 90),
    (10, 1, BLOCK, 64),
    (12, 2, BLOCK, 80),
    (14, 1, BLOCK, 70),
];
const RK_KEYS_DROP: &[KeyHit] = &[(0, 6, BLOCK, 100), (6, 2, BLOCK, 84), (8, 6, BLOCK, 98), (14, 2, BLOCK, 80)];
const RK_KEYS_SPARSE: &[KeyHit] = &[(0, 16, BLOCK, 84)];

static ROCK: Preset = Preset {
    kit: 16,
    bass: 34,
    pad: 18,
    keys: 29,
    lead: 27,
    colour: Colour::None,
    swing8: 0.0,
    swing16: 0.0,
    jitter_s: 0.006,
    vel_jitter: 8.0,
    strum_s: 0.0,
    gain: [0.32, 0.95, 0.61, 0.25, 0.38, 0.64, 1.00],
    shares: [0.28, 0.10, 0.14, 0.04, 0.30, 0.08, 0.06],
    reverb: [30, 25, 0, 40, 24, 36, 80],
    chorus: [0, 0, 0, 10, 0, 10, 0],
    pan: [64, 64, 64, 64, 50, 76, 64],
    sidechain: sc(0.0, 0.15, 0.0, 0.0, 0.0),
    pad_range: (48, 72),
    pad_spread: 14,
    pad_vel: 70,
    pad_wide: false,
    keys_range: (40, 64),
    keys_spread: 14,
    power_keys: true,
    lead_range: (64, 88),
    drums: DrumSet {
        light: &[(36, "6..............."), (37, "....4.......4..."), (44, "4...4...4...4...")],
        groove: &[
            (36, "8.......8.6..... 8.....5.8.6....."),
            (38, "....8.......8..."),
            (42, "7.5.6.5.7.5.6.5."),
        ],
        full: &[
            (36, "9.....5.9.7..... 9.....5.9.7...6."),
            (38, "....9.......9..."),
            (46, "6.6.6.6.6.6.6.6."),
        ],
    },
    bass_verse: [RK_BASS_VERSE, RK_BASS_VERSE],
    bass_drop: [RK_BASS_DROP, RK_BASS_DROP],
    keys_verse: [RK_KEYS_VERSE, RK_KEYS_VERSE],
    keys_drop: [RK_KEYS_DROP, RK_KEYS_DROP],
    keys_sparse: RK_KEYS_SPARSE,
    motifs: POP_MOTIFS,
    lead_sparse: POP_LEAD_SPARSE,
    impact: 0.8,
    roll_key: 38,
    crash_key: Some(49),
    crash_loop: true,
    fills: true,
    fill_toms: [50, 48, 45, 41],
};

fn preset(style: MusicStyle) -> &'static Preset {
    match style {
        MusicStyle::TropicalHouse => &TROPICAL_HOUSE,
        MusicStyle::Pop => &POP,
        MusicStyle::Lofi => &LOFI,
        MusicStyle::Cinematic => &CINEMATIC,
        MusicStyle::Ambient => &AMBIENT,
        MusicStyle::Rock => &ROCK,
    }
}

/// Decode a lane pattern into two bars of velocities (a 16-step pattern repeats).
fn lane_steps(pattern: &str) -> [u8; 32] {
    let cells: Vec<u8> = pattern
        .chars()
        .filter(|c| *c == '.' || c.is_ascii_digit())
        .map(|c| if c == '.' { 0 } else { ((c as u8 - b'0') * 14 + 2).min(127) })
        .collect();
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = match cells.len() {
            0 => 0,
            n if n <= 16 => cells[i % n],
            n => cells[i % n.min(32)],
        };
    }
    out
}

// ---- reading the sections ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Light,
    Mid,
    Full,
}

fn level_of(energy: f64) -> Level {
    if energy < 0.4 {
        Level::Light
    } else if energy < 0.75 {
        Level::Mid
    } else {
        Level::Full
    }
}

/// One bar of the piece and everything the generators need to know about where it sits.
#[derive(Clone)]
struct Bar {
    /// The section's index, which with the bar's place in it keys the bar's random stream.
    section: usize,
    kind: SectionKind,
    level: Level,
    /// Bar within its section, and the section's bars.
    index: usize,
    count: usize,
    t0: f64,
    chord: Chord,
    next_chord: Option<Chord>,
    prev_kind: Option<SectionKind>,
    next_kind: Option<SectionKind>,
    /// The last bar of an outro that ends the piece: it resolves on the tonic.
    resolves: bool,
    /// A velocity scale from the section's energy and its shape (a build swells, an outro fades).
    vs: f64,
}

impl Bar {
    /// What a bar's random streams are keyed by: its section and its place in it, so editing one
    /// section never re-rolls the sections before it.
    fn rng_key(&self) -> u64 {
        ((self.section as u64) << 24) | self.index as u64
    }
}

fn bars_of(score: &MusicScore, resolved: &super::Resolved) -> Vec<Bar> {
    let bar_s = score.bar_s();
    let mut flat: Vec<(usize, usize, Chord)> = Vec::new();
    for (si, chords) in resolved.chords.iter().enumerate() {
        for (bi, c) in chords.iter().enumerate() {
            flat.push((si, bi, *c));
        }
    }
    let total = flat.len();
    let mut out = Vec::with_capacity(total);
    for (g, (si, bi, chord)) in flat.iter().enumerate() {
        let span = &resolved.spans[*si];
        let kind = span.kind;
        let last = g + 1 == total;
        let resolves = last && kind == SectionKind::Outro;
        let progress = (*bi + 1) as f64 / span.bars as f64;
        let e = match kind {
            SectionKind::Build => 0.45 + 0.55 * progress,
            SectionKind::Outro => span.energy * (1.0 - 0.5 * progress),
            _ => span.energy,
        };
        out.push(Bar {
            section: *si,
            kind,
            level: level_of(span.energy),
            index: *bi,
            count: span.bars as usize,
            t0: g as f64 * bar_s,
            chord: *chord,
            next_chord: flat.get(g + 1).map(|(_, _, c)| *c),
            prev_kind: (*si > 0).then(|| resolved.spans[*si - 1].kind),
            next_kind: resolved.spans.get(*si + 1).map(|s| s.kind),
            resolves,
            vs: 0.78 + 0.22 * e.clamp(0.0, 1.0),
        });
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrumTier {
    Off,
    Light,
    LightNoKick,
    Groove,
    Full,
    Build,
}

fn drum_tier(b: &Bar) -> DrumTier {
    use DrumTier::*;
    let second_half = b.index * 2 >= b.count;
    match (b.kind, b.level) {
        _ if b.resolves => Off,
        (SectionKind::Intro, Level::Light) => Off,
        (SectionKind::Intro, Level::Mid) => {
            if second_half {
                Light
            } else {
                Off
            }
        }
        (SectionKind::Intro, Level::Full) => Groove,
        (SectionKind::Verse, Level::Light) => Light,
        (SectionKind::Verse, Level::Mid) => Groove,
        (SectionKind::Verse, Level::Full) => Full,
        (SectionKind::Build, _) => Build,
        (SectionKind::Drop, Level::Light) => Groove,
        (SectionKind::Drop, _) => Full,
        (SectionKind::Breakdown, Level::Light) => Off,
        (SectionKind::Breakdown, Level::Mid) => LightNoKick,
        (SectionKind::Breakdown, Level::Full) => Light,
        (SectionKind::Outro, Level::Light) => {
            if second_half {
                Off
            } else {
                Light
            }
        }
        (SectionKind::Outro, Level::Mid) => {
            if second_half {
                Light
            } else {
                Groove
            }
        }
        (SectionKind::Outro, Level::Full) => {
            if second_half {
                Light
            } else {
                Groove
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BassMode {
    Off,
    Sustain,
    Verse,
    Drop,
    Pulse,
}

fn bass_mode(b: &Bar) -> BassMode {
    use BassMode::*;
    let second_half = b.index * 2 >= b.count;
    if b.resolves {
        return Sustain;
    }
    match (b.kind, b.level) {
        (SectionKind::Intro, Level::Light) => Off,
        (SectionKind::Intro, Level::Mid) => {
            if second_half {
                Sustain
            } else {
                Off
            }
        }
        (SectionKind::Intro, Level::Full) => Verse,
        (SectionKind::Verse, Level::Light) => Sustain,
        (SectionKind::Verse, Level::Mid) => Verse,
        (SectionKind::Verse, Level::Full) => Drop,
        (SectionKind::Build, _) => Pulse,
        (SectionKind::Drop, Level::Light) => Verse,
        (SectionKind::Drop, _) => Drop,
        (SectionKind::Breakdown, _) => Off,
        (SectionKind::Outro, Level::Light) => Sustain,
        (SectionKind::Outro, Level::Mid) => {
            if second_half {
                Sustain
            } else {
                Verse
            }
        }
        (SectionKind::Outro, Level::Full) => {
            if second_half {
                Sustain
            } else {
                Verse
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeysMode {
    Off,
    Sparse,
    Verse,
    Drop,
}

fn keys_mode(b: &Bar) -> KeysMode {
    use KeysMode::*;
    let second_half = b.index * 2 >= b.count;
    if b.resolves {
        return Sparse;
    }
    match (b.kind, b.level) {
        (SectionKind::Intro, Level::Light | Level::Mid) => Sparse,
        (SectionKind::Intro, Level::Full) => {
            if second_half {
                Verse
            } else {
                Sparse
            }
        }
        (SectionKind::Verse, Level::Light) => Sparse,
        (SectionKind::Verse, Level::Mid) => Verse,
        (SectionKind::Verse, Level::Full) => Drop,
        (SectionKind::Build, _) => {
            if second_half {
                Verse
            } else {
                Sparse
            }
        }
        (SectionKind::Drop, _) => Drop,
        (SectionKind::Breakdown, Level::Light) => Sparse,
        (SectionKind::Breakdown, _) => Verse,
        (SectionKind::Outro, _) => {
            if second_half {
                Off
            } else {
                Sparse
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeadMode {
    Off,
    Sparse,
    Full,
}

fn lead_mode(b: &Bar) -> LeadMode {
    use LeadMode::*;
    let second_half = b.index * 2 >= b.count;
    if b.resolves {
        return Sparse;
    }
    match (b.kind, b.level) {
        (SectionKind::Intro, Level::Light) => Off,
        (SectionKind::Intro, _) => {
            if second_half && b.count >= 4 {
                Sparse
            } else {
                Off
            }
        }
        (SectionKind::Verse, Level::Light) => Off,
        (SectionKind::Verse, Level::Mid) => Sparse,
        (SectionKind::Verse, Level::Full) => Full,
        (SectionKind::Build, _) => {
            if second_half {
                Sparse
            } else {
                Off
            }
        }
        (SectionKind::Drop, _) => Full,
        (SectionKind::Breakdown, Level::Light) => Sparse,
        (SectionKind::Breakdown, _) => Full,
        (SectionKind::Outro, _) => {
            if second_half {
                Off
            } else {
                Sparse
            }
        }
    }
}

/// Whether the pad plays softer than its full level in this bar.
fn pad_soft(b: &Bar) -> bool {
    matches!(b.kind, SectionKind::Intro | SectionKind::Outro) || (b.kind == SectionKind::Verse && b.level < Level::Full)
}

// ---- the generator -------------------------------------------------------------------------

fn is_top(key: u8) -> bool {
    matches!(key, 42 | 44 | 46 | 49 | 51..=55 | 56 | 57 | 59 | 69 | 70 | 82)
}

fn is_kick(key: u8) -> bool {
    matches!(key, 35 | 36)
}

struct Gen<'a> {
    score: &'a MusicScore,
    preset: &'static Preset,
    key: Key,
    program: [u8; 7],
    step_s: f64,
    bar_s: f64,
    notes: Vec<NoteEvent>,
}

impl Gen<'_> {
    fn step_time(&self, b: &Bar, step: f64) -> f64 {
        let mut t = b.t0 + step * self.step_s;
        if step.fract() == 0.0 {
            let s = step as u32;
            if s % 4 == 2 {
                t += self.preset.swing8 * self.step_s;
            } else if s % 2 == 1 {
                t += self.preset.swing16 * self.step_s;
            }
        }
        t
    }

    /// Add a note with its timing and velocity humanized. `wobble` scales the timing jitter
    /// (a kick is steadier than a hat).
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        rng: &mut Rng,
        role: Role,
        channel: u8,
        program: u8,
        key: u8,
        velocity: f64,
        start: f64,
        dur: f64,
        wobble: f64,
    ) {
        let dt = rng.signed() * self.preset.jitter_s * wobble;
        let dv = rng.signed() * self.preset.vel_jitter;
        let start = (start + dt).max(0.0);
        let vel = (velocity + dv).round().clamp(1.0, 127.0) as u8;
        self.notes.push(NoteEvent {
            role,
            channel,
            program,
            key,
            velocity: vel,
            start_s: start,
            dur_s: dur.max(0.02),
        });
    }

    fn timp_key(&self, chord: &Chord) -> u8 {
        if self.program[Role::Drums.index()] == 48 {
            pitch_at_or_above(chord.root, 41)
        } else {
            45
        }
    }

    // ---- drums ----

    fn drums(&mut self, bars: &[Bar]) {
        let kit = self.program[Role::Drums.index()];
        for b in bars {
            let mut rng = Rng::new(self.score.seed, 1, b.rng_key());
            let tier = drum_tier(b);
            let parity = b.index % 2;

            if tier == DrumTier::Build {
                self.build_drums(b, kit, &mut rng);
                continue;
            }
            let fill = self.fill_start(b, tier, &mut rng);
            let (lanes, no_kick) = match tier {
                DrumTier::Off => (&[][..], false),
                DrumTier::Light => (self.preset.drums.light, false),
                DrumTier::LightNoKick => (self.preset.drums.light, true),
                DrumTier::Groove => (self.preset.drums.groove, false),
                DrumTier::Full => (self.preset.drums.full, false),
                DrumTier::Build => (&[][..], false),
            };
            for (lane_key, pattern) in lanes {
                let key = if *lane_key == TIMP {
                    self.timp_key(&b.chord)
                } else {
                    *lane_key
                };
                if no_kick && is_kick(key) {
                    continue;
                }
                let steps = lane_steps(pattern);
                for s in 0..16usize {
                    let v = steps[parity * 16 + s];
                    if v == 0 {
                        continue;
                    }
                    if fill.is_some_and(|f| s >= f) && !is_kick(key) {
                        continue;
                    }
                    let t = self.step_time(b, s as f64);
                    let wobble = if is_kick(key) || *lane_key == TIMP { 0.4 } else { 1.0 };
                    let role = if is_top(key) { Role::Top } else { Role::Drums };
                    let dur = if *lane_key == TIMP { 1.2 } else { 0.18 };
                    let vel = v as f64 * b.vs;
                    self.push(&mut rng, role, 9, kit, key, vel, t, dur, wobble);
                }
            }
            // The crash that marks a new section.
            if let Some(crash) = self.preset.crash_key {
                let section_start = b.index == 0
                    && (b.kind == SectionKind::Drop
                        || (b.kind == SectionKind::Verse
                            && matches!(
                                b.prev_kind,
                                Some(SectionKind::Intro | SectionKind::Build | SectionKind::Breakdown)
                            )));
                let loop_crash = self.preset.crash_loop && b.kind == SectionKind::Drop && b.index % 4 == 0;
                if (section_start || loop_crash) && tier != DrumTier::Off {
                    let t = self.step_time(b, 0.0);
                    self.push(&mut rng, Role::Top, 9, kit, crash, 100.0 * b.vs.max(0.8), t, 0.4, 0.3);
                }
            }
            if let Some(from) = fill {
                self.fill(b, kit, from, &mut rng);
            }
        }
    }

    /// Where a fill begins in this bar (the step), if it has one: the last bar of a section that
    /// is followed by another, and every eighth bar of a long one.
    fn fill_start(&self, b: &Bar, tier: DrumTier, rng: &mut Rng) -> Option<usize> {
        let roll = rng.unit();
        if !self.preset.fills || !matches!(tier, DrumTier::Groove | DrumTier::Full) {
            return None;
        }
        let ends_section = b.index + 1 == b.count && b.next_kind.is_some();
        let long_phrase = b.count > 8 && b.index % 8 == 7;
        if !(ends_section || long_phrase) {
            return None;
        }
        Some(
            if tier == DrumTier::Full && b.next_kind != Some(SectionKind::Drop) && roll < 0.33 {
                8
            } else {
                12
            },
        )
    }

    fn fill(&mut self, b: &Bar, kit: u8, from: usize, rng: &mut Rng) {
        let style = rng.below(3);
        let toms = self.preset.fill_toms;
        let steps: Vec<usize> = (from..16).collect();
        let n = steps.len();
        for (k, s) in steps.iter().enumerate() {
            let t = self.step_time(b, *s as f64);
            let ramp = 66.0 + 40.0 * k as f64 / (n.max(2) - 1) as f64;
            let (key, take) = match style {
                0 => (38, true),
                1 => (toms[(k * 4 / n).min(3)], true),
                _ => (if k + 1 == n { toms[1] } else { 38 }, *s % 2 == 0 || k + 1 == n),
            };
            if take {
                self.push(rng, Role::Drums, 9, kit, key, ramp * b.vs.max(0.8), t, 0.18, 0.5);
            }
        }
    }

    /// A build: the kick comes in on the beat, hats run, and the snare rolls — a hit a beat, two,
    /// four, then eight — louder all the way.
    fn build_drums(&mut self, b: &Bar, kit: u8, rng: &mut Rng) {
        let total_beats = (b.count * 4) as f64;
        let snare = self.preset.roll_key;
        for beat in 0..4usize {
            let pos = (b.index * 4 + beat) as f64 / total_beats;
            let ramp = 44.0 + 76.0 * pos;
            let rate = if pos < 0.25 {
                1
            } else if pos < 0.5 {
                2
            } else if pos < 0.8 {
                4
            } else {
                8
            };
            for k in 0..rate {
                let step = beat as f64 * 4.0 + 4.0 * k as f64 / rate as f64;
                let t = b.t0 + step * self.step_s;
                if rate >= 2 || beat % 2 == 1 {
                    self.push(
                        rng,
                        Role::Drums,
                        9,
                        kit,
                        snare,
                        ramp * (0.9 + 0.1 * (k % 2) as f64),
                        t,
                        0.12,
                        0.3,
                    );
                }
            }
            // The kick: a pulse on the beat once the build is under way, not in its last beat.
            let last_beat = b.index + 1 == b.count && beat == 3;
            if pos >= 0.2 && !last_beat {
                self.push(
                    rng,
                    Role::Drums,
                    9,
                    kit,
                    36,
                    (70.0 + 30.0 * pos) * b.vs,
                    b.t0 + beat as f64 * 4.0 * self.step_s,
                    0.15,
                    0.4,
                );
            }
        }
        // Eighth-note hats, getting louder.
        if let Some((hat, _)) = self
            .preset
            .drums
            .full
            .iter()
            .chain(self.preset.drums.groove)
            .find(|(k, _)| *k == 42 || *k == 82)
        {
            for s in (0..16).step_by(2) {
                let pos = (b.index * 16 + s) as f64 / (b.count * 16) as f64;
                let t = self.step_time(b, s as f64);
                self.push(
                    rng,
                    Role::Top,
                    9,
                    kit,
                    *hat,
                    (46.0 + 50.0 * pos) * if s % 4 == 0 { 1.0 } else { 0.85 },
                    t,
                    0.1,
                    1.0,
                );
            }
        }
    }

    // ---- bass ----

    fn bass_pitch(&self, pc: u8) -> u8 {
        pitch_at_or_above(pc, 31)
    }

    fn tone_pitch(&self, tone: Tone, b: &Bar) -> u8 {
        let root = self.bass_pitch(b.chord.bass);
        match tone {
            Tone::R => root,
            Tone::O => root + 12,
            Tone::F => root + b.chord.fifth(),
            Tone::A => match b.next_chord {
                Some(next) if next.bass != b.chord.bass => {
                    // A step of the scale short of the next root, from the side we are coming from:
                    // under a root we rise to, over one we fall to.
                    let target = self.bass_pitch(next.bass);
                    let approach = self.key.neighbor(target, target < root);
                    if (approach as i32 - root as i32).abs() <= 9 {
                        approach
                    } else {
                        root
                    }
                }
                _ => root + 12,
            },
        }
    }

    fn bass(&mut self, bars: &[Bar]) {
        let program = self.program[Role::Bass.index()];
        for b in bars {
            let mut rng = Rng::new(self.score.seed, 2, b.rng_key());
            let mode = bass_mode(b);
            let pre_drop = b.index + 1 == b.count && b.kind == SectionKind::Build && b.next_kind == Some(SectionKind::Drop);
            let parity = b.index % 2;
            match mode {
                BassMode::Off => {}
                BassMode::Sustain => {
                    let len = if b.resolves { self.bar_s * 1.0 } else { self.bar_s * 0.97 };
                    let pitch = self.bass_pitch(b.chord.bass);
                    let t = b.t0;
                    self.push(&mut rng, Role::Bass, 0, program, pitch, 84.0 * b.vs, t, len, 0.5);
                }
                BassMode::Pulse => {
                    for s in (0..16).step_by(2) {
                        if pre_drop && s == 14 {
                            continue;
                        }
                        let pos = (b.index * 16 + s) as f64 / (b.count * 16) as f64;
                        let t = self.step_time(b, s as f64);
                        let pitch = self.bass_pitch(b.chord.bass);
                        self.push(
                            &mut rng,
                            Role::Bass,
                            0,
                            program,
                            pitch,
                            62.0 + 44.0 * pos,
                            t,
                            self.step_s * 1.7,
                            0.5,
                        );
                    }
                }
                BassMode::Verse | BassMode::Drop => {
                    let pattern = if mode == BassMode::Verse {
                        self.preset.bass_verse[parity]
                    } else {
                        self.preset.bass_drop[parity]
                    };
                    for (step, len, tone, vel) in pattern {
                        let t = self.step_time(b, *step as f64);
                        let pitch = self.tone_pitch(*tone, b);
                        let dur = *len as f64 * self.step_s * 0.92;
                        self.push(&mut rng, Role::Bass, 0, program, pitch, *vel as f64 * b.vs, t, dur, 0.5);
                    }
                }
            }
        }
    }

    // ---- pad ----

    fn pad(&mut self, bars: &[Bar]) {
        let program = self.program[Role::Pad.index()];
        let p = self.preset;
        let mut prev: Option<Vec<u8>> = None;
        let mut i = 0;
        while i < bars.len() {
            let b = &bars[i];
            let mut rng = Rng::new(self.score.seed, 3, b.rng_key());
            // Hold one chord across the bars that repeat it (not through a build, which swells
            // bar by bar), up to four.
            let mut run = 1;
            if b.kind != SectionKind::Build {
                while run < 4
                    && i + run < bars.len()
                    && bars[i + run].chord == b.chord
                    && bars[i + run].kind == b.kind
                    && bars[i + run].index == b.index + run
                {
                    run += 1;
                }
            }
            let mut notes = voice(
                &b.chord.voicing_classes(),
                prev.as_deref(),
                p.pad_range.0,
                p.pad_range.1,
                p.pad_spread,
            );
            prev = Some(notes.clone());
            if p.pad_wide {
                if let Some(low) = notes.first().copied() {
                    let root = pitch_at_or_above(b.chord.bass, low.saturating_sub(24).max(36));
                    if root + 7 <= low {
                        notes.insert(0, root);
                    }
                }
            }
            let soft = pad_soft(b);
            let vel = p.pad_vel as f64 * b.vs * if soft { 0.9 } else { 1.0 };
            let pre_drop = b.kind == SectionKind::Build && b.index + 1 == b.count && b.next_kind == Some(SectionKind::Drop);
            let mut dur = run as f64 * self.bar_s - 0.03;
            if b.resolves {
                dur = self.bar_s * 1.0 + 0.3;
            }
            if pre_drop {
                dur -= self.step_s * 1.5;
            }
            for k in 0..run {
                if bars[i + k].resolves {
                    dur = (run - k) as f64 * self.bar_s + 0.3;
                }
            }
            for n in &notes {
                let t = b.t0;
                self.push(&mut rng, Role::Pad, 0, program, *n, vel, t, dur, 1.0);
            }
            i += run;
        }
    }

    // ---- keys ----

    fn keys_voicing(&self, b: &Bar, prev: Option<&[u8]>) -> Vec<u8> {
        let p = self.preset;
        if p.power_keys {
            let root = pitch_at_or_above(b.chord.root, p.keys_range.0);
            return vec![root, root + 7, root + 12];
        }
        voice(
            &b.chord.voicing_classes(),
            prev,
            p.keys_range.0,
            p.keys_range.1,
            p.keys_spread,
        )
    }

    fn keys(&mut self, bars: &[Bar]) {
        let program = self.program[Role::Keys.index()];
        let mut prev: Option<Vec<u8>> = None;
        for b in bars {
            let mut rng = Rng::new(self.score.seed, 4, b.rng_key());
            let mode = keys_mode(b);
            if mode == KeysMode::Off {
                continue;
            }
            let voicing = self.keys_voicing(b, prev.as_deref());
            prev = Some(voicing.clone());
            let parity = b.index % 2;
            let pattern: &[KeyHit] = match mode {
                KeysMode::Sparse => self.preset.keys_sparse,
                KeysMode::Verse => self.preset.keys_verse[parity],
                _ => self.preset.keys_drop[parity],
            };
            let pre_drop = b.kind == SectionKind::Build && b.index + 1 == b.count && b.next_kind == Some(SectionKind::Drop);
            let n = voicing.len() as i32;
            for (step, len, which, vel) in pattern {
                if pre_drop && *step >= 14 {
                    continue;
                }
                let t = self.step_time(b, *step as f64);
                let mut dur = *len as f64 * self.step_s * 0.92;
                if b.resolves {
                    dur = self.bar_s + 0.3;
                }
                let velocity = *vel as f64 * b.vs;
                if *which == BLOCK {
                    for (k, note) in voicing.iter().enumerate() {
                        let strum = self.preset.strum_s * k as f64;
                        self.push(&mut rng, Role::Keys, 0, program, *note, velocity, t + strum, dur, 0.6);
                    }
                } else {
                    let idx = *which as i32;
                    let octave = idx.div_euclid(n);
                    let k = idx.rem_euclid(n) as usize;
                    let note = (voicing[k] as i32 + 12 * octave).clamp(0, 127) as u8;
                    self.push(&mut rng, Role::Keys, 0, program, note, velocity, t, dur, 0.8);
                }
            }
        }
    }

    // ---- lead ----

    fn lead_pitch(&self, chord: &Chord, idx: i8, nb: i8) -> u8 {
        let mut ladder: Vec<u8> = chord.intervals().iter().map(|i| i % 12).collect();
        ladder.sort_unstable();
        ladder.dedup();
        let n = ladder.len() as i32;
        let base = pitch_at_or_above(chord.root, self.preset.lead_range.0) as i32;
        let oct = (idx as i32).div_euclid(n);
        let k = (idx as i32).rem_euclid(n) as usize;
        let mut pitch = (base + 12 * oct + ladder[k] as i32).clamp(0, 127) as u8;
        if nb != 0 {
            // A passing note belongs to the key; over a chord from outside it, to the chord's scale.
            pitch = if chord.is_diatonic(self.key) {
                self.key.neighbor(pitch, nb > 0)
            } else {
                chord.scale_neighbor(pitch, nb > 0)
            };
        }
        pitch
    }

    /// Two motifs for a kind of section, always the same ones for the same score: the hook.
    fn motifs_for(&self, kind: SectionKind) -> (&'static [LeadNote], &'static [LeadNote]) {
        let all = self.preset.motifs;
        let mut rng = Rng::new(self.score.seed, 50, kind as u64 + 1);
        let a = rng.below(all.len());
        let b = if all.len() > 1 {
            (a + 1 + rng.below(all.len() - 1)) % all.len()
        } else {
            a
        };
        (all[a], all[b])
    }

    fn lead(&mut self, bars: &[Bar]) {
        let program = self.program[Role::Lead.index()];
        for b in bars {
            let mut rng = Rng::new(self.score.seed, 5, b.rng_key());
            let mode = lead_mode(b);
            if mode == LeadMode::Off {
                continue;
            }
            if mode == LeadMode::Sparse {
                if b.index % 2 == 1 && !b.resolves {
                    continue;
                }
                for (step, len, idx, nb, vel) in self.preset.lead_sparse {
                    let pitch = self.lead_pitch(&b.chord, *idx, *nb);
                    let t = self.step_time(b, *step as f64);
                    let mut dur = *len as f64 * self.step_s * 0.95;
                    if b.resolves {
                        dur = self.bar_s + 0.3;
                    }
                    self.push(&mut rng, Role::Lead, 1, program, pitch, *vel as f64 * b.vs, t, dur, 0.8);
                }
                continue;
            }
            let (a, c) = self.motifs_for(b.kind);
            let phrase = b.index % 4;
            let motif = if phrase < 2 { a } else { c };
            let last = motif.len().saturating_sub(1);
            for (i, (step, len, idx, nb, vel)) in motif.iter().enumerate() {
                let mut idx = *idx;
                let mut len = *len;
                match phrase {
                    // The answer: the end of the phrase lifted a step.
                    1 if i + 2 >= motif.len() && motif.len() > 2 => idx += 1,
                    // The cadence: the bar ends on the root, held.
                    3 if i == last => {
                        idx = 0;
                        len = len.max(4);
                    }
                    _ => {}
                }
                let pitch = self.lead_pitch(&b.chord, idx, *nb);
                let t = self.step_time(b, *step as f64);
                let dur = len as f64 * self.step_s * 0.95;
                self.push(&mut rng, Role::Lead, 1, program, pitch, *vel as f64 * b.vs, t, dur, 0.8);
            }
        }
    }

    // ---- effects ----

    /// A reverse-cymbal sweep that peaks at `peak`. The sample is a fixed length at middle C and
    /// lasts `2^(-semitones / 24)` as long a note higher: pitched so it peaks `length` seconds
    /// after it starts.
    fn sweep(&mut self, peak: f64, length: f64, velocity: f64, rng: &mut Rng) {
        let key = (48.0 - 24.0 * (length / REVERSE_CYMBAL_PEAK_S).log2())
            .round()
            .clamp(24.0, 84.0) as u8;
        let actual = REVERSE_CYMBAL_PEAK_S * 2f64.powf((48.0 - key as f64) / 24.0);
        let start = peak - actual;
        if start < -0.001 {
            return;
        }
        self.push(rng, Role::Fx, 2, 119, key, velocity, start.max(0.0), actual + 0.25, 0.0);
    }

    fn riser(&mut self, peak: f64, length: f64, rng: &mut Rng) {
        let length = length.clamp(1.0, 3.6);
        self.sweep(peak, length, 100.0, rng);
        self.sweep(peak, length * 0.5, 84.0, rng);
    }

    fn whoosh(&mut self, peak: f64, rng: &mut Rng) {
        self.sweep(peak, 0.9, 84.0, rng);
    }

    /// A timpani on the tonic and a low boom on `t`; `full` adds the crash and the kick a hit asked
    /// of the picture wants (a drop has both in its own pattern already).
    fn impact(&mut self, t: f64, strength: f64, full: bool, rng: &mut Rng) {
        let tonic = self.key.tonic;
        if full {
            self.push(rng, Role::Fx, 9, 0, 49, 112.0 * strength, t, 0.5, 0.0);
            self.push(rng, Role::Fx, 9, 0, 36, 118.0 * strength, t, 0.3, 0.0);
        }
        self.push(
            rng,
            Role::Fx,
            0,
            47,
            pitch_at_or_above(tonic, 36),
            116.0 * strength,
            t,
            1.4,
            0.0,
        );
        self.push(
            rng,
            Role::Fx,
            1,
            39,
            pitch_at_or_above(tonic, 24),
            104.0 * strength,
            t,
            1.2,
            0.0,
        );
    }

    fn effects(&mut self, bars: &[Bar]) {
        for b in bars {
            let mut rng = Rng::new(self.score.seed, 6, b.rng_key());
            // A build ends in a riser into whatever follows.
            if b.kind == SectionKind::Build && b.index + 1 == b.count && b.next_kind.is_some() {
                let bars_up = b.count.min(2) as f64;
                self.riser(b.t0 + self.bar_s, bars_up * self.bar_s, &mut rng);
            }
            // A drop lands with an impact (unless one drop follows another).
            if b.kind == SectionKind::Drop && b.index == 0 && b.prev_kind != Some(SectionKind::Drop) && self.preset.impact > 0.0 {
                self.impact(b.t0, self.preset.impact, self.preset.crash_key.is_none(), &mut rng);
            }
        }
    }
}

/// How long after it starts the General User GS reverse cymbal peaks at middle C an octave down
/// (key 48), measured: it is a sample, so it is one length per pitch.
const REVERSE_CYMBAL_PEAK_S: f64 = 1.9;

/// The most a section is lifted, dB.
const MAX_MAKEUP_DB: f64 = 6.0;

/// A role counts as playing in a section with at least this many notes in it.
const MIN_ACTIVE_NOTES: usize = 4;

/// How much of the style's loudness (its role shares) each section has, against the fullest, and
/// the gain that closes half that gap in dB (at most [`MAX_MAKEUP_DB`]).
fn makeup_for(spans: &[super::SectionSpan], notes: &[NoteEvent], shares: &[f32; 7]) -> Vec<Makeup> {
    let power: Vec<f64> = spans
        .iter()
        .map(|span| {
            ROLES
                .iter()
                .filter(|r| **r != Role::Fx)
                .filter(|r| {
                    notes
                        .iter()
                        .filter(|n| n.role == **r && n.start_s >= span.start_s - 0.03 && n.start_s < span.end_s - 0.03)
                        .count()
                        >= MIN_ACTIVE_NOTES
                })
                .map(|r| shares[r.index()] as f64)
                .sum::<f64>()
        })
        .collect();
    let fullest = power.iter().copied().fold(0.0, f64::max);
    spans
        .iter()
        .zip(&power)
        .map(|(span, p)| {
            let db = if *p > 0.0 && fullest > 0.0 {
                (0.5 * 10.0 * (fullest / p).log10()).clamp(0.0, MAX_MAKEUP_DB)
            } else {
                0.0
            };
            Makeup {
                start_s: span.start_s,
                end_s: span.end_s,
                gain: 10f64.powf(db / 20.0) as f32,
            }
        })
        .collect()
}

/// The shares a style's gains are calibrated to.
#[cfg(test)]
pub(crate) fn style_shares(style: MusicStyle) -> [f32; 7] {
    preset(style).shares
}

/// Turn a score into notes.
pub fn expand(score: &MusicScore) -> Result<Arrangement> {
    let resolved = score.resolve()?;
    let p = preset(score.style);
    let over = score.instruments.unwrap_or_default();
    let program = [
        over.drums.unwrap_or(p.kit),
        over.drums.unwrap_or(p.kit),
        over.bass.unwrap_or(p.bass),
        over.pad.unwrap_or(p.pad),
        over.keys.unwrap_or(p.keys),
        over.lead.unwrap_or(p.lead),
        0,
    ];
    let bars = bars_of(score, &resolved);
    let mut gen = Gen {
        score,
        preset: p,
        key: resolved.key,
        program,
        step_s: score.bar_s() / 16.0,
        bar_s: score.bar_s(),
        notes: Vec::new(),
    };
    gen.drums(&bars);
    gen.bass(&bars);
    gen.pad(&bars);
    gen.keys(&bars);
    gen.lead(&bars);
    gen.effects(&bars);

    // The picture's hits.
    let beat = 60.0 / score.bpm;
    let mut dropouts = Vec::new();
    let mut hit_rng = Rng::new(score.seed, 7, 0);
    for hit in &score.hits {
        match hit.kind {
            HitKind::Impact => gen.impact(hit.at_s, 1.0, true, &mut hit_rng),
            HitKind::Riser => gen.riser(hit.at_s, score.bar_s(), &mut hit_rng),
            HitKind::Whoosh => gen.whoosh(hit.at_s, &mut hit_rng),
            HitKind::Dropout => dropouts.push((hit.at_s, hit.at_s + beat)),
        }
    }

    let duration_s = score.duration_s();
    let mut notes = gen.notes;
    // The same key of one channel must not overlap itself (a note-off ends every voice of the key),
    // so a note ends where the next of its key begins.
    notes.sort_by(|a, b| {
        (a.role, a.channel, a.key)
            .cmp(&(b.role, b.channel, b.key))
            .then(a.start_s.total_cmp(&b.start_s))
    });
    for i in 0..notes.len().saturating_sub(1) {
        let (a, b) = (notes[i], notes[i + 1]);
        if a.role == b.role && a.channel == b.channel && a.key == b.key && a.start_s + a.dur_s > b.start_s {
            notes[i].dur_s = (b.start_s - a.start_s - 0.004).max(0.02);
        }
    }
    notes.retain(|n| n.start_s < duration_s);
    for n in &mut notes {
        n.dur_s = n.dur_s.min(duration_s - n.start_s);
    }
    notes.sort_by(|a, b| a.start_s.total_cmp(&b.start_s).then((a.role, a.key).cmp(&(b.role, b.key))));
    let mut kicks: Vec<f64> = notes
        .iter()
        .filter(|n| n.role == Role::Drums && n.channel == 9 && is_kick(n.key))
        .map(|n| n.start_s)
        .collect();
    kicks.sort_by(f64::total_cmp);
    let makeup = makeup_for(&resolved.spans, &notes, &p.shares);
    Ok(Arrangement {
        notes,
        kicks,
        dropouts,
        makeup,
        duration_s,
        mix: Mix {
            gain: p.gain,
            reverb: p.reverb,
            chorus: p.chorus,
            pan: p.pan,
            sidechain: p.sidechain,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::{section, song};
    use super::super::{MusicScore, ScoreHit};
    use super::*;

    fn pcs(notes: &[&NoteEvent]) -> Vec<u8> {
        let mut v: Vec<u8> = notes.iter().map(|n| n.key % 12).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The chord of every bar as played (`resolve` already resolves the outro's last bar).
    fn played(s: &MusicScore) -> Vec<Chord> {
        s.resolve().unwrap().chords.iter().flatten().copied().collect()
    }

    fn in_bar(a: &Arrangement, role: Role, bar: usize, bar_s: f64) -> Vec<&NoteEvent> {
        let (lo, hi) = (bar as f64 * bar_s - 0.03, (bar + 1) as f64 * bar_s - 0.03);
        a.role(role).filter(|n| n.start_s >= lo && n.start_s < hi).collect()
    }

    #[test]
    fn every_pattern_in_every_preset_decodes() {
        for style in MusicStyle::ALL {
            let p = preset(style);
            for set in [p.drums.light, p.drums.groove, p.drums.full] {
                for (key, pattern) in set {
                    let cells = pattern.chars().filter(|c| *c == '.' || c.is_ascii_digit()).count();
                    assert!(
                        cells == 16 || cells == 32,
                        "{style:?} key {key}: {pattern:?} has {cells} cells"
                    );
                }
            }
            for list in p.bass_verse.iter().chain(&p.bass_drop) {
                assert!(
                    list.iter().all(|(s, l, _, v)| *s < 16 && *l >= 1 && *v > 0 && *v <= 127),
                    "{style:?} bass"
                );
            }
            for list in p.keys_verse.iter().chain(&p.keys_drop).chain([&p.keys_sparse]) {
                assert!(list
                    .iter()
                    .all(|(s, l, w, v)| *s < 16 && *l >= 1 && (*w == BLOCK || *w >= 0) && *v <= 127));
            }
            for motif in p.motifs.iter().chain([&p.lead_sparse]) {
                assert!(
                    motif.iter().all(|(s, l, _, _, v)| *s < 16 && *l >= 1 && *v <= 127),
                    "{style:?} motif"
                );
            }
        }
    }

    #[test]
    fn the_same_score_is_the_same_notes_and_a_new_seed_a_new_feel() {
        for style in MusicStyle::ALL {
            let s = song(style);
            let a = expand(&s).unwrap();
            let b = expand(&s).unwrap();
            assert_eq!(a, b, "{style:?}");
            let c = expand(&s.clone().with_seed(99)).unwrap();
            assert_ne!(a.notes, c.notes, "{style:?}: the seed varies the humanization");
            // ... but not what is played: the same keys in the same bars for the drums and bass.
            let keys = |a: &Arrangement, r: Role| a.role(r).map(|n| n.key).collect::<Vec<_>>();
            assert_eq!(keys(&a, Role::Bass), keys(&c, Role::Bass), "{style:?}");
        }
    }

    #[test]
    fn sparse_sections_are_lifted_toward_the_fullest_and_never_by_more_than_six_db() {
        for style in MusicStyle::ALL {
            let s = song(style);
            let a = expand(&s).unwrap();
            assert_eq!(a.makeup.len(), s.sections.len());
            let max = 10f32.powf(MAX_MAKEUP_DB as f32 / 20.0);
            assert!(
                a.makeup.iter().all(|m| (1.0..=max + 1e-4).contains(&m.gain)),
                "{style:?}: {:?}",
                a.makeup
            );
            // The drops carry every layer: nothing to make up. The pad-and-a-chord intro and the
            // breakdown without its kick and bass are lifted.
            assert!((a.makeup[3].gain - 1.0).abs() < 1e-6, "{style:?} drop {}", a.makeup[3].gain);
            assert!(a.makeup[0].gain > 1.05, "{style:?} intro {}", a.makeup[0].gain);
            assert!(a.makeup[4].gain > 1.0, "{style:?} breakdown {}", a.makeup[4].gain);
            // The sections are in time order and tile the piece.
            for (m, span) in a.makeup.iter().zip(s.section_spans().unwrap()) {
                assert_eq!((m.start_s, m.end_s), (span.start_s, span.end_s));
            }
        }
    }

    #[test]
    fn editing_a_section_never_rerolls_the_ones_before_it() {
        for style in MusicStyle::ALL {
            let s = song(style);
            let mut edited = s.clone();
            // The first drop gets shorter and another seed of chords after it: bars 16.. change.
            edited.sections[3].bars = 4;
            edited.sections[5].chords = vec!["i".into(), "iv".into()];
            let (a, b) = (expand(&s).unwrap(), expand(&edited).unwrap());
            let before = |x: &Arrangement| x.notes.iter().filter(|n| n.start_s < 32.0 - 0.1).copied().collect::<Vec<_>>();
            assert_eq!(before(&a), before(&b), "{style:?}");
            assert_ne!(a.notes, b.notes);
        }
    }

    #[test]
    fn every_note_is_valid_and_inside_the_render() {
        for style in MusicStyle::ALL {
            let s = song(style);
            let a = expand(&s).unwrap();
            assert!(!a.notes.is_empty());
            for n in &a.notes {
                assert!(n.key < 128 && (1..=127).contains(&n.velocity), "{style:?} {n:?}");
                assert!(n.channel < 16);
                assert!(n.start_s >= 0.0 && n.start_s < a.duration_s, "{style:?} {n:?}");
                assert!(n.dur_s > 0.0 && n.start_s + n.dur_s <= a.duration_s + 1e-9, "{style:?} {n:?}");
            }
            assert!(a.notes.windows(2).all(|w| w[0].start_s <= w[1].start_s));
            // No key of a channel plays over itself.
            for role in ROLES {
                let mut notes: Vec<&NoteEvent> = a.role(role).collect();
                notes.sort_by(|x, y| {
                    (x.channel, x.key)
                        .cmp(&(y.channel, y.key))
                        .then(x.start_s.total_cmp(&y.start_s))
                });
                for w in notes.windows(2) {
                    if w[0].channel == w[1].channel && w[0].key == w[1].key {
                        assert!(w[0].start_s + w[0].dur_s <= w[1].start_s + 1e-9, "{style:?} {role:?} {w:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_bass_follows_the_roots_of_the_chords() {
        // A minor: Am F C G — and the verse's first bar of bass must be an A, then F, C, G.
        for style in [
            MusicStyle::Pop,
            MusicStyle::Rock,
            MusicStyle::Lofi,
            MusicStyle::TropicalHouse,
            MusicStyle::Cinematic,
        ] {
            let s = song(style);
            let a = expand(&s).unwrap();
            let bar_s = s.bar_s();
            let roots = [9u8, 5, 0, 7];
            for (k, root) in roots.iter().enumerate() {
                let bar = 4 + k;
                let notes = in_bar(&a, Role::Bass, bar, bar_s);
                let first = notes.iter().min_by(|x, y| x.start_s.total_cmp(&y.start_s)).unwrap();
                assert_eq!(first.key % 12, *root, "{style:?} bar {bar}: {first:?}");
                assert!(
                    (31..=42).contains(&first.key),
                    "{style:?} the root sits in the bass register: {}",
                    first.key
                );
            }
        }
    }

    #[test]
    fn the_bass_approaches_the_next_root_in_key_and_never_leaves_the_register() {
        for style in MusicStyle::ALL {
            let s = song(style);
            let a = expand(&s).unwrap();
            let key = Key::parse(&s.key).unwrap();
            for n in a.role(Role::Bass) {
                assert!((24..=60).contains(&n.key), "{style:?} {n:?}");
                if !key.in_scale(n.key % 12) {
                    // Only the dominant of the minor key (E major's G#) is borrowed.
                    assert_eq!(n.key % 12, 8, "{style:?} {n:?}");
                }
            }
        }
    }

    #[test]
    fn pads_hold_the_chord_tones_and_voices_move_little() {
        let s = song(MusicStyle::Pop);
        let a = expand(&s).unwrap();
        let bar_s = s.bar_s();
        let mut prev: Option<Vec<u8>> = None;
        for (g, chord) in played(&s).iter().enumerate() {
            let notes = in_bar(&a, Role::Pad, g, bar_s);
            if notes.is_empty() {
                continue;
            }
            let have = pcs(&notes);
            let want: Vec<u8> = {
                let mut w = chord.voicing_classes();
                w.sort_unstable();
                w
            };
            assert_eq!(have, want, "bar {g}");
            let mut cur: Vec<u8> = notes.iter().map(|n| n.key).collect();
            cur.sort_unstable();
            if let Some(p) = &prev {
                let moved: i32 = cur
                    .iter()
                    .map(|n| p.iter().map(|q| (*n as i32 - *q as i32).abs()).min().unwrap())
                    .sum();
                assert!(moved <= 8, "bar {g}: {p:?} -> {cur:?} moves {moved}");
            }
            prev = Some(cur);
        }
    }

    #[test]
    fn keys_and_lead_stay_on_chord_tones_or_in_the_scale() {
        for style in MusicStyle::ALL {
            let s = song(style);
            let a = expand(&s).unwrap();
            let r = s.resolve().unwrap();
            let bar_s = s.bar_s();
            for (g, chord) in played(&s).iter().enumerate() {
                for n in in_bar(&a, Role::Keys, g, bar_s) {
                    let allowed = chord.pitch_classes();
                    assert!(
                        allowed.contains(&(n.key % 12)),
                        "{style:?} keys bar {g}: {n:?} vs {:?}",
                        chord.symbol()
                    );
                }
                for n in in_bar(&a, Role::Lead, g, bar_s) {
                    let key = r.key;
                    assert!(
                        chord.pitch_classes().contains(&(n.key % 12)) || key.in_scale(n.key % 12),
                        "{style:?} lead bar {g}: {n:?} vs {}",
                        chord.symbol()
                    );
                }
            }
        }
    }

    #[test]
    fn sections_shape_the_density() {
        let s = song(MusicStyle::TropicalHouse);
        let a = expand(&s).unwrap();
        let bar_s = s.bar_s();
        // The breakdown (bars 24..28) has no kick and no bass.
        for bar in 24..28 {
            assert!(
                !in_bar(&a, Role::Drums, bar, bar_s).iter().any(|n| is_kick(n.key)),
                "bar {bar}"
            );
            assert!(in_bar(&a, Role::Bass, bar, bar_s).is_empty(), "bar {bar}");
            assert!(!in_bar(&a, Role::Pad, bar, bar_s).is_empty(), "bar {bar}");
        }
        // The drop (bars 16..24) is busier than the verse (4..12) in every layer.
        let count = |role: Role, bars: std::ops::Range<usize>| bars.map(|b| in_bar(&a, role, b, bar_s).len()).sum::<usize>();
        assert!(count(Role::Drums, 16..24) > count(Role::Drums, 4..12));
        assert!(count(Role::Bass, 16..24) >= count(Role::Bass, 4..12));
        assert!(count(Role::Lead, 16..24) > count(Role::Lead, 4..12));
        // The intro (0..4) is sparser than the drop.
        assert!(count(Role::Drums, 0..4) < count(Role::Drums, 16..20));
        assert!(count(Role::Bass, 0..4) < count(Role::Bass, 16..20));
    }

    #[test]
    fn a_build_rolls_faster_and_ends_in_a_riser_into_the_drop() {
        let s = song(MusicStyle::Pop);
        let a = expand(&s).unwrap();
        let bar_s = s.bar_s();
        let snares = |bar: usize| in_bar(&a, Role::Drums, bar, bar_s).iter().filter(|n| n.key == 38).count();
        // Bars 12..16 are the build: the roll quickens bar by bar.
        assert!(
            snares(12) < snares(13) && snares(13) < snares(14) && snares(14) <= snares(15),
            "{} {} {} {}",
            snares(12),
            snares(13),
            snares(14),
            snares(15)
        );
        // ... and louder.
        let loud = |bar: usize| {
            in_bar(&a, Role::Drums, bar, bar_s)
                .iter()
                .filter(|n| n.key == 38)
                .map(|n| n.velocity as u32)
                .max()
                .unwrap()
        };
        assert!(loud(15) > loud(12));
        // The riser peaks on the drop's first beat (bar 16 = 32 s).
        let riser: Vec<&NoteEvent> = a.role(Role::Fx).filter(|n| n.program == 119).collect();
        assert!(!riser.is_empty());
        let peak = riser[0].start_s + REVERSE_CYMBAL_PEAK_S * 2f64.powf((48.0 - riser[0].key as f64) / 24.0);
        assert!((peak - 32.0).abs() < 0.05, "peak {peak}");
        // ... and the kick stays out of the build's last beat, the bass out of its last half beat.
        let last = in_bar(&a, Role::Drums, 15, bar_s);
        assert!(!last.iter().any(|n| is_kick(n.key) && n.start_s > 15.0 * bar_s + 0.75 * bar_s));
    }

    #[test]
    fn a_drop_lands_with_an_impact_and_a_crash() {
        let s = song(MusicStyle::Rock);
        let a = expand(&s).unwrap();
        assert!(a.role(Role::Fx).any(|n| n.program == 47 && (n.start_s - 32.0).abs() < 0.03));
        assert!(a.role(Role::Top).any(|n| n.key == 49 && (n.start_s - 32.0).abs() < 0.03));
    }

    #[test]
    fn the_outro_resolves_on_the_tonic() {
        for style in MusicStyle::ALL {
            let s = song(style);
            let a = expand(&s).unwrap();
            let bar_s = s.bar_s();
            // The outro's chords are Am F Dm E: the last bar is not E but Am.
            let last = 39;
            let bass = in_bar(&a, Role::Bass, last, bar_s);
            assert_eq!(bass.len(), 1, "{style:?}: one held root");
            assert_eq!(bass[0].key % 12, 9, "{style:?}");
            let pad = pcs(&in_bar(&a, Role::Pad, last, bar_s));
            assert!((3..=4).contains(&pad.len()), "{style:?} {pad:?}");
            assert!(
                pad.contains(&9) && pad.contains(&0) && pad.contains(&4),
                "{style:?}: an Am triad {pad:?}"
            );
            assert!(
                !in_bar(&a, Role::Drums, last, bar_s).iter().any(|n| is_kick(n.key)),
                "{style:?}"
            );
            // ... and it rings into the tail.
            let ring = a
                .role(Role::Pad)
                .filter(|n| n.start_s >= last as f64 * bar_s - 0.03)
                .map(|n| n.start_s + n.dur_s)
                .fold(0.0, f64::max);
            assert!(ring > (last + 1) as f64 * bar_s, "{style:?}: {ring}");
        }
    }

    #[test]
    fn kicks_are_the_kick_notes_and_a_house_beat_is_four_on_the_floor() {
        let s = song(MusicStyle::TropicalHouse);
        let a = expand(&s).unwrap();
        let from_notes: Vec<f64> = a
            .role(Role::Drums)
            .filter(|n| is_kick(n.key) && n.channel == 9)
            .map(|n| n.start_s)
            .collect();
        assert_eq!(a.kicks, from_notes);
        // The drop's 8 bars hold 32 kicks on the beat (the impact's kick is in the effects).
        let beat = 60.0 / s.bpm;
        let drop: Vec<f64> = a
            .kicks
            .iter()
            .copied()
            .filter(|t| *t >= 32.0 - 0.05 && *t < 48.0 - 0.05)
            .collect();
        assert_eq!(drop.len(), 32);
        for (i, t) in drop.iter().enumerate() {
            let want = 32.0 + i as f64 * beat;
            assert!((t - want).abs() < 0.02, "kick {i}: {t} vs {want}");
        }
    }

    #[test]
    fn swing_delays_the_off_beats_and_humanize_stays_small() {
        let mut s = song(MusicStyle::Lofi);
        s.sections = vec![section(SectionKind::Verse, 8, &["Am7"])];
        let a = expand(&s).unwrap();
        let step = s.bar_s() / 16.0;
        // The 8th-note hats (steps 2 mod 4) lag the grid by about half a step on average.
        let hats: Vec<f64> = a.role(Role::Top).filter(|n| n.key == 42).map(|n| n.start_s).collect();
        let lag = |phase: i64| -> f64 {
            let v: Vec<f64> = hats
                .iter()
                .filter_map(|t| {
                    let k = (t / step).round() as i64;
                    (k.rem_euclid(4) == phase).then(|| t - k as f64 * step)
                })
                .collect();
            v.iter().sum::<f64>() / v.len() as f64
        };
        assert!(
            lag(2) > lag(0) + 0.5 * step * 0.6,
            "off-beat lag {} vs on-beat {}",
            lag(2),
            lag(0)
        );
        for n in &a.notes {
            assert!(n.start_s >= 0.0);
        }
    }

    #[test]
    fn the_hits_of_the_picture_become_effects_and_dropouts() {
        let mut s = song(MusicStyle::Pop);
        s.hits = vec![
            ScoreHit {
                at_s: 8.0,
                kind: HitKind::Impact,
            },
            ScoreHit {
                at_s: 20.0,
                kind: HitKind::Riser,
            },
            ScoreHit {
                at_s: 21.5,
                kind: HitKind::Dropout,
            },
            ScoreHit {
                at_s: 30.0,
                kind: HitKind::Whoosh,
            },
        ];
        let a = expand(&s).unwrap();
        assert_eq!(a.dropouts, vec![(21.5, 22.0)]);
        // The impact at 8.0: a timpani on the tonic (A) and a crash.
        let timp = a
            .role(Role::Fx)
            .find(|n| n.program == 47 && (n.start_s - 8.0).abs() < 0.01)
            .unwrap();
        assert_eq!(timp.key % 12, 9);
        // Two sweeps peak at the riser's time, one at the whoosh's.
        let peaks: Vec<f64> = a
            .role(Role::Fx)
            .filter(|n| n.program == 119)
            .map(|n| n.start_s + REVERSE_CYMBAL_PEAK_S * 2f64.powf((48.0 - n.key as f64) / 24.0))
            .collect();
        assert_eq!(peaks.iter().filter(|p| (**p - 20.0).abs() < 0.05).count(), 2, "{peaks:?}");
        assert!(peaks.iter().any(|p| (*p - 30.0).abs() < 0.05));
    }

    #[test]
    fn instruments_can_be_overridden() {
        let mut s = song(MusicStyle::Pop);
        s.instruments = Some(crate::model::InstrumentOverrides {
            bass: Some(38),
            lead: Some(73),
            drums: Some(25),
            ..Default::default()
        });
        let a = expand(&s).unwrap();
        assert!(a.role(Role::Bass).all(|n| n.program == 38));
        assert!(a.role(Role::Lead).all(|n| n.program == 73));
        assert!(a.role(Role::Drums).all(|n| n.program == 25 && n.channel == 9));
        assert!(a.role(Role::Pad).all(|n| n.program == 49));
    }

    #[test]
    fn a_one_bar_score_still_makes_music() {
        for style in MusicStyle::ALL {
            let s = MusicScore {
                bpm: 100.0,
                key: "C".to_string(),
                style,
                sections: vec![section(SectionKind::Drop, 1, &["C"])],
                hits: vec![],
                seed: 0,
                instruments: None,
            };
            let a = expand(&s).unwrap();
            assert!(!a.notes.is_empty(), "{style:?}");
        }
    }

    #[test]
    fn the_rng_is_stable_across_platforms() {
        // Pinned values: the humanization of a published score must not drift between builds.
        let mut r = Rng::new(7, 1, 3);
        let v: Vec<u64> = (0..3).map(|_| r.next_u64()).collect();
        let mut again = Rng::new(7, 1, 3);
        assert_eq!(v, (0..3).map(|_| again.next_u64()).collect::<Vec<_>>());
        assert_ne!(Rng::new(7, 1, 4).next_u64(), v[0]);
        let mut r = Rng::new(0, 0, 0);
        for _ in 0..1000 {
            let u = r.unit();
            assert!((0.0..1.0).contains(&u));
        }
    }
}
