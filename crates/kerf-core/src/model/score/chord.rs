//! Keys, chord symbols and voicings: the harmony half of the score model. Pure.
//!
//! A chord is written either as a **name** (`C`, `Em`, `F#m7`, `Gsus4`, `D/F#`, `Bbmaj7`) or as a
//! **roman numeral relative to the key** (`I`, `V`, `vi`, `IV`, `ii7`, `bVII`, `V/…` is not
//! supported). Numerals count the degrees of the key's own scale (major, or natural minor), so
//! in A minor `VI` is F and `VII` is G; the case of the numeral is the quality (`V` major, `v`
//! minor), a `b` or `#` in front moves the root a semitone.

/// Major or (natural) minor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Major,
    Minor,
}

/// A key: a tonic pitch class and a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// Pitch class of the tonic, `0` = C.
    pub tonic: u8,
    pub mode: Mode,
}

const MAJOR_SCALE: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
const MINOR_SCALE: [u8; 7] = [0, 2, 3, 5, 7, 8, 10];

/// The pitch class of a note letter (`A`..`G`), or `None`.
fn letter_pc(c: char) -> Option<i32> {
    Some(match c.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    })
}

/// A note name (`F#`, `Bb`, `E`) at the start of `s`: its pitch class and the rest of the text.
fn take_note(s: &str) -> Option<(u8, &str)> {
    let mut chars = s.chars();
    let letter = chars.next()?;
    let mut pc = letter_pc(letter)?;
    let mut rest = chars.as_str();
    loop {
        match rest.chars().next() {
            Some('#' | '♯') => pc += 1,
            Some('b' | '♭') => pc -= 1,
            _ => break,
        }
        rest = &rest[rest.chars().next().map_or(0, char::len_utf8)..];
    }
    Some((pc.rem_euclid(12) as u8, rest))
}

impl Key {
    /// `G`, `F#m`, `Bb`, `Ebm` — a tonic and an optional `m` (also `min`, `minor`, `maj`, `major`).
    pub fn parse(text: &str) -> Result<Key, String> {
        let t = text.trim();
        let Some((tonic, rest)) = take_note(t) else {
            return Err(format!(
                "\"{text}\" is not a key: write a tonic and an optional m, like G, F#m, Bb or Ebm"
            ));
        };
        let mode = match rest {
            "" | "maj" | "major" | "M" => Mode::Major,
            "m" | "min" | "minor" | "-" => Mode::Minor,
            _ => {
                return Err(format!(
                    "\"{text}\" is not a key: write a tonic and an optional m, like G, F#m, Bb or Ebm"
                ))
            }
        };
        Ok(Key { tonic, mode })
    }

    pub fn is_minor(&self) -> bool {
        self.mode == Mode::Minor
    }

    fn scale(&self) -> &'static [u8; 7] {
        match self.mode {
            Mode::Major => &MAJOR_SCALE,
            Mode::Minor => &MINOR_SCALE,
        }
    }

    /// Pitch class of scale degree `degree` (0 = tonic … 6).
    pub fn degree_pc(&self, degree: usize) -> u8 {
        (self.tonic + self.scale()[degree % 7]) % 12
    }

    /// Whether pitch class `pc` is a note of the key's scale.
    pub fn in_scale(&self, pc: u8) -> bool {
        let rel = (pc + 12 - self.tonic) % 12;
        self.scale().contains(&rel)
    }

    /// The tonic triad.
    pub fn tonic_chord(&self) -> Chord {
        Chord {
            root: self.tonic,
            bass: self.tonic,
            quality: if self.is_minor() { Quality::Min } else { Quality::Maj },
        }
    }

    /// The nearest note of the scale above (`up`) or below `pitch`, a semitone or more away.
    pub fn neighbor(&self, pitch: u8, up: bool) -> u8 {
        let mut p = pitch as i32;
        for _ in 0..12 {
            p += if up { 1 } else { -1 };
            if !(0..=127).contains(&p) {
                return pitch;
            }
            if self.in_scale((p % 12) as u8) {
                return p as u8;
            }
        }
        pitch
    }
}

/// What a chord is made of above its root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Maj,
    Min,
    Dim,
    Aug,
    Sus2,
    Sus4,
    Power,
    Dom7,
    Maj7,
    Min7,
    HalfDim,
    Dim7,
    MinMaj7,
    Dom7Sus4,
    Six,
    MinSix,
    Add9,
    MinAdd9,
    Dom9,
    Maj9,
    Min9,
}

/// The suffixes a chord name may carry, and what they mean. Exact (case-sensitive: `M7` is
/// major, `m7` minor).
const SUFFIXES: &[(&str, Quality)] = &[
    ("", Quality::Maj),
    ("maj", Quality::Maj),
    ("M", Quality::Maj),
    ("m", Quality::Min),
    ("min", Quality::Min),
    ("-", Quality::Min),
    ("dim", Quality::Dim),
    ("°", Quality::Dim),
    ("o", Quality::Dim),
    ("aug", Quality::Aug),
    ("+", Quality::Aug),
    ("sus2", Quality::Sus2),
    ("sus4", Quality::Sus4),
    ("sus", Quality::Sus4),
    ("5", Quality::Power),
    ("7", Quality::Dom7),
    ("maj7", Quality::Maj7),
    ("M7", Quality::Maj7),
    ("Δ", Quality::Maj7),
    ("Δ7", Quality::Maj7),
    ("m7", Quality::Min7),
    ("min7", Quality::Min7),
    ("-7", Quality::Min7),
    ("m7b5", Quality::HalfDim),
    ("ø", Quality::HalfDim),
    ("ø7", Quality::HalfDim),
    ("dim7", Quality::Dim7),
    ("°7", Quality::Dim7),
    ("o7", Quality::Dim7),
    ("mmaj7", Quality::MinMaj7),
    ("mM7", Quality::MinMaj7),
    ("m(maj7)", Quality::MinMaj7),
    ("7sus4", Quality::Dom7Sus4),
    ("7sus", Quality::Dom7Sus4),
    ("6", Quality::Six),
    ("maj6", Quality::Six),
    ("m6", Quality::MinSix),
    ("add9", Quality::Add9),
    ("add2", Quality::Add9),
    ("madd9", Quality::MinAdd9),
    ("m(add9)", Quality::MinAdd9),
    ("9", Quality::Dom9),
    ("maj9", Quality::Maj9),
    ("M9", Quality::Maj9),
    ("m9", Quality::Min9),
    ("min9", Quality::Min9),
];

impl Quality {
    /// Semitones above the root, ascending (a ninth is written as 14).
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Quality::Maj => &[0, 4, 7],
            Quality::Min => &[0, 3, 7],
            Quality::Dim => &[0, 3, 6],
            Quality::Aug => &[0, 4, 8],
            Quality::Sus2 => &[0, 2, 7],
            Quality::Sus4 => &[0, 5, 7],
            Quality::Power => &[0, 7],
            Quality::Dom7 => &[0, 4, 7, 10],
            Quality::Maj7 => &[0, 4, 7, 11],
            Quality::Min7 => &[0, 3, 7, 10],
            Quality::HalfDim => &[0, 3, 6, 10],
            Quality::Dim7 => &[0, 3, 6, 9],
            Quality::MinMaj7 => &[0, 3, 7, 11],
            Quality::Dom7Sus4 => &[0, 5, 7, 10],
            Quality::Six => &[0, 4, 7, 9],
            Quality::MinSix => &[0, 3, 7, 9],
            Quality::Add9 => &[0, 4, 7, 14],
            Quality::MinAdd9 => &[0, 3, 7, 14],
            Quality::Dom9 => &[0, 4, 7, 10, 14],
            Quality::Maj9 => &[0, 4, 7, 11, 14],
            Quality::Min9 => &[0, 3, 7, 10, 14],
        }
    }

    pub fn is_minor(self) -> bool {
        matches!(
            self,
            Quality::Min
                | Quality::Min7
                | Quality::MinMaj7
                | Quality::MinSix
                | Quality::MinAdd9
                | Quality::Min9
                | Quality::Dim
                | Quality::HalfDim
                | Quality::Dim7
        )
    }
}

/// How much colour a style adds to a plain triad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    /// Chords sound as written.
    None,
    /// Triads become seventh chords (`C` → `Cmaj7`, `Am` → `Am7`).
    Sevenths,
    /// Triads gain a ninth (`C` → `Cadd9`, `Am` → `Am(add9)`).
    Ninths,
}

/// A chord: root, bass note and quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    /// Pitch class of the root.
    pub root: u8,
    /// Pitch class of the lowest note (the root unless written as a slash chord).
    pub bass: u8,
    pub quality: Quality,
}

fn quality_of(suffix: &str, minor_numeral: bool) -> Option<Quality> {
    let find = |s: &str| SUFFIXES.iter().find(|(name, _)| *name == s).map(|(_, q)| *q);
    if minor_numeral {
        if let Some(q) = find(&format!("m{suffix}")) {
            return Some(q);
        }
        if ["dim", "°", "o", "ø", "sus", "aug", "+"]
            .iter()
            .any(|p| suffix.starts_with(p))
        {
            return find(suffix);
        }
        None
    } else {
        find(suffix)
    }
}

impl Chord {
    /// A chord name or a roman numeral in `key`. The error is a sentence naming the chord.
    pub fn parse(text: &str, key: Key) -> Result<Chord, String> {
        let t = text.trim();
        if t.is_empty() {
            return Err("an empty chord".to_string());
        }
        let bad = |why: &str| format!("\"{text}\" is not a chord: {why}");
        let (main, bass_text) = match t.rsplit_once('/') {
            Some((m, b)) => (m, Some(b)),
            None => (t, None),
        };
        let bass = match bass_text {
            Some(b) => match take_note(b.trim()) {
                Some((pc, "")) => Some(pc),
                _ => return Err(bad("after the / goes a note name, like D/F#")),
            },
            None => None,
        };
        let first = main.chars().next().unwrap_or(' ');
        let (root, quality) = if matches!(first, 'A'..='G') {
            let (root, rest) = take_note(main).ok_or_else(|| bad("it starts with no note name"))?;
            let q = quality_of(rest, false).ok_or_else(|| bad(&format!("unknown chord type \"{rest}\"")))?;
            (root, q)
        } else {
            let (shift, body) = match main.chars().next() {
                Some('b' | '♭') => (-1_i32, &main[main.chars().next().map_or(0, char::len_utf8)..]),
                Some('#' | '♯') => (1, &main[main.chars().next().map_or(0, char::len_utf8)..]),
                _ => (0, main),
            };
            let len = body.chars().take_while(|c| matches!(c, 'I' | 'V' | 'i' | 'v')).count();
            let (numeral, suffix) = body.split_at(len);
            let degree = match numeral.to_ascii_uppercase().as_str() {
                "I" => 0,
                "II" => 1,
                "III" => 2,
                "IV" => 3,
                "V" => 4,
                "VI" => 5,
                "VII" => 6,
                _ => {
                    return Err(bad(
                        "write a chord name (C, Em, F#m7, Gsus4, D/F#) or a roman numeral (I, V, vi, IV, ii7)",
                    ))
                }
            };
            let lower = numeral.chars().all(|c| c.is_ascii_lowercase());
            let upper = numeral.chars().all(|c| c.is_ascii_uppercase());
            if !lower && !upper {
                return Err(bad("a roman numeral is all upper case (major) or all lower case (minor)"));
            }
            let q = quality_of(suffix, lower).ok_or_else(|| bad(&format!("unknown chord type \"{suffix}\"")))?;
            let root = (key.degree_pc(degree) as i32 + shift).rem_euclid(12) as u8;
            (root, q)
        };
        Ok(Chord {
            root,
            bass: bass.unwrap_or(root),
            quality,
        })
    }

    /// The chord with a style's colour added to a plain triad (anything else is left as written).
    pub fn coloured(self, colour: Colour) -> Chord {
        let quality = match (colour, self.quality) {
            (Colour::Sevenths, Quality::Maj) => Quality::Maj7,
            (Colour::Sevenths, Quality::Min) => Quality::Min7,
            (Colour::Ninths, Quality::Maj) => Quality::Add9,
            (Colour::Ninths, Quality::Min) => Quality::MinAdd9,
            (_, q) => q,
        };
        Chord { quality, ..self }
    }

    /// Intervals above the root.
    pub fn intervals(&self) -> &'static [u8] {
        self.quality.intervals()
    }

    /// The distinct pitch classes of the chord (root first), the bass note's included.
    pub fn pitch_classes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        for i in self.intervals() {
            let pc = (self.root + i) % 12;
            if !out.contains(&pc) {
                out.push(pc);
            }
        }
        if !out.contains(&self.bass) {
            out.push(self.bass);
        }
        out
    }

    /// The chord tones to voice above a bass line: at most four distinct pitch classes, the
    /// fifth dropped from a ninth chord first, ascending from the root.
    pub fn voicing_classes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        let ints = self.intervals();
        for i in ints {
            let pc = (self.root + i) % 12;
            if !out.contains(&pc) {
                out.push(pc);
            }
        }
        if out.len() > 4 {
            let fifth = (self.root + 7) % 12;
            if let Some(at) = out.iter().position(|p| *p == fifth) {
                out.remove(at);
            }
        }
        out.truncate(4);
        out
    }

    /// The interval of the chord's fifth (7 unless it is diminished or augmented).
    pub fn fifth(&self) -> u8 {
        self.intervals().iter().copied().find(|i| (6..=8).contains(i)).unwrap_or(7)
    }

    /// The scale a melody may pass through over this chord: the major scale of its root for a
    /// major chord, the dominant (mixolydian) for a seventh, the natural minor for a minor one.
    fn scale_intervals(&self) -> &'static [u8; 7] {
        match self.quality {
            Quality::Dom7 | Quality::Dom9 | Quality::Dom7Sus4 => &[0, 2, 4, 5, 7, 9, 10],
            Quality::Dim | Quality::HalfDim | Quality::Dim7 => &[0, 2, 3, 5, 6, 8, 10],
            q if q.is_minor() => &MINOR_SCALE,
            _ => &MAJOR_SCALE,
        }
    }

    /// Whether every note of the chord belongs to `key`'s scale.
    pub fn is_diatonic(&self, key: Key) -> bool {
        self.pitch_classes().iter().all(|pc| key.in_scale(*pc))
    }

    /// The nearest note of the chord's own scale ([`Chord::scale_intervals`]) above (`up`) or
    /// below `pitch`: what a passing note over a chord from outside the key may be.
    pub fn scale_neighbor(&self, pitch: u8, up: bool) -> u8 {
        let scale = self.scale_intervals();
        let mut p = pitch as i32;
        for _ in 0..12 {
            p += if up { 1 } else { -1 };
            if !(0..=127).contains(&p) {
                return pitch;
            }
            if scale.contains(&(((p - self.root as i32).rem_euclid(12)) as u8)) {
                return p as u8;
            }
        }
        pitch
    }

    /// A readable symbol (`F#m7`), spelled with sharps.
    pub fn symbol(&self) -> String {
        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        let suffix = SUFFIXES
            .iter()
            .find(|(_, q)| *q == self.quality)
            .map(|(s, _)| *s)
            .unwrap_or("");
        let mut out = format!("{}{}", NAMES[self.root as usize], suffix);
        if self.bass != self.root {
            out.push('/');
            out.push_str(NAMES[self.bass as usize]);
        }
        out
    }
}

/// The lowest MIDI pitch of pitch class `pc` that is at least `lo`.
pub fn pitch_at_or_above(pc: u8, lo: u8) -> u8 {
    lo + (pc + 12 - lo % 12) % 12
}

/// Choose how to voice `classes` (pitch classes, root first) as notes between `lo` and `hi`:
/// every class once, inside `max_spread` semitones, moving as little as possible from `prev`
/// (the voicing before). With nothing before, the voicing nearest the middle of the range.
/// Ascending. Pure and deterministic: the first of equally good voicings wins.
pub fn voice(classes: &[u8], prev: Option<&[u8]>, lo: u8, hi: u8, max_spread: u8) -> Vec<u8> {
    if classes.is_empty() {
        return Vec::new();
    }
    let options: Vec<Vec<u8>> = classes
        .iter()
        .map(|pc| (lo..=hi).filter(|p| p % 12 == *pc).collect::<Vec<u8>>())
        .collect();
    let center = (lo as f64 + hi as f64) / 2.0;
    let mut best: Option<(f64, Vec<u8>)> = None;
    for spread in [max_spread, max_spread + 4, max_spread + 8, 48] {
        let mut pick = vec![0usize; classes.len()];
        loop {
            let mut notes: Vec<u8> = pick
                .iter()
                .enumerate()
                .filter_map(|(i, k)| options[i].get(*k).copied())
                .collect();
            if notes.len() == classes.len() {
                notes.sort_unstable();
                let low = notes[0];
                let high = notes[notes.len() - 1];
                if high - low <= spread {
                    let cost = match prev {
                        Some(prev) if !prev.is_empty() => {
                            let moved: f64 = notes
                                .iter()
                                .map(|n| prev.iter().map(|p| (*n as i32 - *p as i32).abs()).min().unwrap_or(0) as f64)
                                .sum();
                            moved + 0.05 * (high - low) as f64
                        }
                        _ => {
                            let mean = notes.iter().map(|n| *n as f64).sum::<f64>() / notes.len() as f64;
                            (mean - center).abs() + 0.2 * (high - low) as f64
                        }
                    };
                    if best.as_ref().is_none_or(|(c, _)| cost < *c - 1e-9) {
                        best = Some((cost, notes));
                    }
                }
            }
            // Odometer over the octave choices.
            let mut i = 0;
            loop {
                if i == pick.len() {
                    break;
                }
                pick[i] += 1;
                if pick[i] < options[i].len().max(1) {
                    break;
                }
                pick[i] = 0;
                i += 1;
            }
            if i == pick.len() {
                break;
            }
        }
        if best.is_some() {
            break;
        }
    }
    best.map(|(_, n)| n).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Key {
        Key::parse(s).unwrap()
    }

    #[test]
    fn keys_parse_with_accidentals_and_minor() {
        assert_eq!(
            key("G"),
            Key {
                tonic: 7,
                mode: Mode::Major
            }
        );
        assert_eq!(
            key("F#m"),
            Key {
                tonic: 6,
                mode: Mode::Minor
            }
        );
        assert_eq!(
            key("Bb"),
            Key {
                tonic: 10,
                mode: Mode::Major
            }
        );
        assert_eq!(
            key("Ebm"),
            Key {
                tonic: 3,
                mode: Mode::Minor
            }
        );
        assert_eq!(
            key("Aminor"),
            Key {
                tonic: 9,
                mode: Mode::Minor
            }
        );
        assert!(Key::parse("H").is_err());
        assert!(Key::parse("C7").is_err());
        assert!(Key::parse("").is_err());
    }

    #[test]
    fn chord_names_parse_with_qualities_and_slash_basses() {
        let k = key("C");
        let c = |s: &str| Chord::parse(s, k).unwrap();
        assert_eq!(
            c("C"),
            Chord {
                root: 0,
                bass: 0,
                quality: Quality::Maj
            }
        );
        assert_eq!(
            c("Em"),
            Chord {
                root: 4,
                bass: 4,
                quality: Quality::Min
            }
        );
        assert_eq!(
            c("F#m7"),
            Chord {
                root: 6,
                bass: 6,
                quality: Quality::Min7
            }
        );
        assert_eq!(c("Gsus4").quality, Quality::Sus4);
        assert_eq!(
            c("D/F#"),
            Chord {
                root: 2,
                bass: 6,
                quality: Quality::Maj
            }
        );
        assert_eq!(
            c("Bbmaj7"),
            Chord {
                root: 10,
                bass: 10,
                quality: Quality::Maj7
            }
        );
        assert_eq!(c("Am7b5").quality, Quality::HalfDim);
        assert_eq!(c("E5").quality, Quality::Power);
        assert_eq!(c("Cadd9").intervals(), &[0, 4, 7, 14]);
        assert_eq!(c("Bdim").quality, Quality::Dim);
        assert_eq!(c("G7").quality, Quality::Dom7);
    }

    #[test]
    fn roman_numerals_follow_the_key_and_their_case() {
        let c = |s: &str, k: &str| Chord::parse(s, key(k)).unwrap();
        assert_eq!(
            c("I", "G"),
            Chord {
                root: 7,
                bass: 7,
                quality: Quality::Maj
            }
        );
        assert_eq!(c("V", "C").root, 7);
        assert_eq!(
            c("vi", "C"),
            Chord {
                root: 9,
                bass: 9,
                quality: Quality::Min
            }
        );
        assert_eq!(c("IV", "C").root, 5);
        assert_eq!(
            c("ii7", "C"),
            Chord {
                root: 2,
                bass: 2,
                quality: Quality::Min7
            }
        );
        assert_eq!(c("V7", "C").quality, Quality::Dom7);
        assert_eq!(c("Imaj7", "C").quality, Quality::Maj7);
        assert_eq!(c("bVII", "C").root, 10);
        assert_eq!(c("vii°", "C").quality, Quality::Dim);
        // The key's own scale: in A minor VI is F and VII is G, i is Am.
        assert_eq!(
            c("i", "Am"),
            Chord {
                root: 9,
                bass: 9,
                quality: Quality::Min
            }
        );
        assert_eq!(c("VI", "Am").root, 5);
        assert_eq!(c("VII", "Am").root, 7);
        assert_eq!(c("III", "Am").root, 0);
        assert_eq!(c("iv", "Am").root, 2);
        assert_eq!(c("V", "Am").quality, Quality::Maj);
    }

    #[test]
    fn bad_chords_say_what_is_wrong() {
        let k = key("C");
        for bad in ["", "H", "Cxyz", "Vi", "H7", "C/", "C/Q", "VIII"] {
            let e = Chord::parse(bad, k).unwrap_err();
            assert!(e.contains(&format!("\"{bad}\"")) || bad.is_empty(), "{bad}: {e}");
        }
        assert!(Chord::parse("Vi", k).unwrap_err().contains("all upper case"));
    }

    #[test]
    fn style_colour_extends_plain_triads_only() {
        let k = key("C");
        let c = Chord::parse("C", k).unwrap();
        assert_eq!(c.coloured(Colour::Sevenths).quality, Quality::Maj7);
        assert_eq!(c.coloured(Colour::Ninths).quality, Quality::Add9);
        assert_eq!(
            Chord::parse("Am", k).unwrap().coloured(Colour::Sevenths).quality,
            Quality::Min7
        );
        assert_eq!(
            Chord::parse("G7", k).unwrap().coloured(Colour::Sevenths).quality,
            Quality::Dom7
        );
        assert_eq!(c.coloured(Colour::None), c);
    }

    #[test]
    fn voicings_keep_every_class_and_move_little() {
        let c = Chord::parse("C", key("C")).unwrap();
        let g = Chord::parse("G", key("C")).unwrap();
        let am = Chord::parse("Am", key("C")).unwrap();
        let f = Chord::parse("F", key("C")).unwrap();
        let mut prev = voice(&c.voicing_classes(), None, 52, 76, 14);
        assert_eq!(prev.len(), 3);
        assert!(prev.iter().all(|n| (52..=76).contains(n)));
        for chord in [g, am, f, c] {
            let next = voice(&chord.voicing_classes(), Some(&prev), 52, 76, 14);
            let mut want: Vec<u8> = next.iter().map(|n| n % 12).collect();
            want.sort_unstable();
            let mut have = chord.voicing_classes();
            have.sort_unstable();
            assert_eq!(want, have, "{}", chord.symbol());
            let moved: i32 = next
                .iter()
                .map(|n| prev.iter().map(|p| (*n as i32 - *p as i32).abs()).min().unwrap())
                .sum();
            assert!(moved <= 6, "{} moved {moved} from {prev:?} to {next:?}", chord.symbol());
            prev = next;
        }
    }

    #[test]
    fn a_ninth_chord_drops_its_fifth_and_stays_four_notes() {
        let c = Chord::parse("Cmaj9", key("C")).unwrap();
        assert_eq!(c.voicing_classes(), vec![0, 4, 11, 2]);
        assert_eq!(voice(&c.voicing_classes(), None, 52, 76, 14).len(), 4);
    }

    #[test]
    fn a_chord_from_outside_the_key_passes_through_its_own_scale() {
        let k = key("Am");
        let e = Chord::parse("V", k).unwrap();
        assert!(!e.is_diatonic(k), "E major has a G sharp");
        assert!(Chord::parse("i", k).unwrap().is_diatonic(k));
        // Over E major the step above B is C sharp (E major's scale), not the key's C natural.
        assert_eq!(k.neighbor(71, true), 72);
        assert_eq!(e.scale_neighbor(71, true), 73);
        // D7 in C: its scale has F sharp where the key has F natural, and C natural (its flat
        // seventh) where a major scale would have C sharp.
        let d7 = Chord::parse("D7", key("C")).unwrap();
        assert!(!d7.is_diatonic(key("C")));
        assert_eq!(key("C").neighbor(64, true), 65);
        assert_eq!(d7.scale_neighbor(64, true), 66);
        assert_eq!(d7.scale_neighbor(69, true), 71);
        assert_eq!(d7.scale_neighbor(71, true), 72);
        assert_eq!(d7.scale_neighbor(72, false), 71);
    }

    #[test]
    fn scale_neighbours_stay_in_key() {
        let k = key("C");
        assert_eq!(k.neighbor(60, true), 62);
        assert_eq!(k.neighbor(64, true), 65);
        assert_eq!(k.neighbor(64, false), 62);
        assert_eq!(k.neighbor(61, true), 62);
        assert!(k.in_scale(7) && !k.in_scale(6));
    }
}
