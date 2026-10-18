//! Energy-based tightening of word bounds, no model.
//!
//! A recognizer's word boundaries (and, worse, words spread over a line by character share)
//! absorb the silence between words into the word before it, and leading silence into the
//! first. Cutting a word and finding pauses need the silences, so each word's start and end
//! are moved inward past 10 ms frames whose level is below a threshold set between the
//! file's noise floor (10th percentile of the frames above −100 dBFS) and its speech level
//! (95th percentile): 30 % of the way up, and at least 6 dB above the floor. A word keeps at
//! least 60 ms, and a word with nothing voiced in it is left alone.

use super::mel::RATE;
use crate::model::Word;

/// One level frame: 10 ms at 16 kHz.
const FRAME: usize = (RATE / 100) as usize;
const MIN_WORD_FRAMES: usize = 6;

/// Frame levels in dBFS (10 ms RMS) of 16 kHz mono audio.
pub fn frame_db(audio: &[f32]) -> Vec<f32> {
    audio
        .chunks(FRAME)
        .map(|c| {
            let e = c.iter().map(|v| v * v).sum::<f32>() / c.len().max(1) as f32;
            10.0 * (e + 1e-12).log10()
        })
        .collect()
}

/// The speech/silence threshold in dBFS.
pub fn threshold(db: &[f32]) -> f32 {
    // Digital silence (< −100 dBFS) would drag the floor far below the recording's own noise.
    let mut s: Vec<f32> = db.iter().copied().filter(|v| *v > -100.0).collect();
    if s.is_empty() {
        return -60.0;
    }
    s.sort_by(f32::total_cmp);
    let q = |p: f32| s[((s.len() - 1) as f32 * p) as usize];
    let (floor, speech) = (q(0.10), q(0.95));
    (floor + 0.3 * (speech - floor)).max(floor + 6.0)
}

/// A file's frame levels and its threshold, computed once for every word.
pub struct Levels {
    db: Vec<f32>,
    threshold: f32,
}

impl Levels {
    pub fn of(audio: &[f32]) -> Self {
        let db = frame_db(audio);
        let threshold = threshold(&db);
        Self { db, threshold }
    }

    /// Move word bounds inward past silent frames (see the module docs).
    pub fn tighten(&self, words: &mut [Word]) {
        let db = &self.db;
        if db.is_empty() {
            return;
        }
        let th = self.threshold;
        let sample = |t: f64| (t * f64::from(RATE)).round().max(0.0) as usize;
        let secs = |frame: usize| (frame * FRAME) as f64 / f64::from(RATE);
        for w in words {
            if !(w.start.is_finite() && w.end.is_finite()) {
                continue;
            }
            let a = sample(w.start) / FRAME;
            let b = sample(w.end).div_ceil(FRAME).min(db.len());
            if b <= a + MIN_WORD_FRAMES {
                continue;
            }
            let mut s = a;
            while s + MIN_WORD_FRAMES < b && db[s] < th {
                s += 1;
            }
            let mut e = b;
            while e > s + MIN_WORD_FRAMES && db[e - 1] < th {
                e -= 1;
            }
            if (s..e).all(|f| db[f] < th) {
                continue;
            }
            if s > a {
                w.start = secs(s);
            }
            if e < b {
                w.end = secs(e).max(w.start);
            }
        }
    }
}

/// [`Levels::tighten`] for one batch of words.
pub fn tighten_words(audio: &[f32], words: &mut [Word]) {
    Levels::of(audio).tighten(words);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.into(),
            start,
            end,
            ..Word::default()
        }
    }

    #[test]
    fn silence_is_trimmed_from_words() {
        // 0.0–0.5 near-silence, 0.5–1.0 tone, 1.0–2.0 near-silence, 2.0–2.5 tone.
        let mut audio: Vec<f32> = (0..40_000).map(|i| if i % 2 == 0 { 0.0001 } else { 0.0 }).collect();
        for i in (8_000..16_000).chain(32_000..40_000) {
            audio[i] = 0.3 * (i as f32 * 0.2).sin();
        }
        let mut w = vec![word("a", 0.0, 2.0), word("b", 2.0, 2.5)];
        tighten_words(&audio, &mut w);
        assert!((w[0].start - 0.5).abs() < 1e-9, "{:?}", w[0]);
        assert!((w[0].end - 1.0).abs() < 1e-9, "{:?}", w[0]);
        assert!((w[1].start - 2.0).abs() < 1e-9 && (w[1].end - 2.5).abs() < 1e-9, "{:?}", w[1]);
    }

    #[test]
    fn a_word_with_nothing_voiced_or_too_short_is_left_alone() {
        let mut audio = vec![0.0001f32; 32_000];
        for (i, s) in audio.iter_mut().enumerate().skip(16_000) {
            *s = 0.3 * (i as f32 * 0.2).sin();
        }
        let mut w = vec![word("quiet", 0.1, 0.9), word("short", 1.2, 1.25)];
        tighten_words(&audio, &mut w);
        assert_eq!((w[0].start, w[0].end), (0.1, 0.9));
        assert_eq!((w[1].start, w[1].end), (1.2, 1.25));
    }

    #[test]
    fn digital_silence_does_not_set_the_floor() {
        let db = [-200.0, -200.0, -50.0, -50.0, -10.0];
        assert!(threshold(&db) > -50.0);
        assert_eq!(threshold(&[]), -60.0);
    }
}
