//! Speaker labels ("who spoke when") without a model: MFCC statistics + agglomerative
//! clustering, the classical method of the speaker-clustering literature.
//!
//! 1. The words are grouped into **chunks**: a new chunk starts at a pause of at least
//!    [`Params::chunk_gap`] or when the chunk reaches [`Params::max_chunk`]; chunks shorter
//!    than [`Params::min_chunk`] join the previous one (a short first one joins the next).
//! 2. Each chunk gets a voice vector: 40-band mel energies → log → DCT → MFCC 1–19 over the
//!    chunk's voiced frames (within 30 dB of its loudest), then the mean and standard
//!    deviation of each coefficient (38 values), each scaled by that coefficient's spread
//!    over all voiced frames of the file — an absolute yardstick, so one voice stays close
//!    to itself however many chunks there are.
//! 3. Average-linkage agglomerative clustering on RMS distance merges the closest clusters
//!    until the closest pair is farther apart than [`Params::threshold`] and at most
//!    `max_speakers` remain. Clusters holding less than 5 % of the voiced frames fold into
//!    their nearest neighbour.
//! 4. Speakers are numbered in order of first appearance.
//!
//! The distance matrix is n² floats, so past [`MAX_CHUNKS`] chunks neighbouring ones are
//! merged (a long file's turns then resolve more coarsely), and chunks whose vectors all
//! coincide are one speaker without clustering.
//!
//! It works for interviews and dialogue with distinct voices and clean turns; overlapping
//! speech, very similar voices or a music bed confuse it.

use super::mel::{self, HOP, RATE};

/// Tunables (FilmCraft's defaults, chosen on LibriSpeech test-clean dialogues).
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub chunk_gap: f64,
    pub min_chunk: f64,
    pub max_chunk: f64,
    pub threshold: f32,
    pub max_speakers: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            chunk_gap: 0.25,
            min_chunk: 0.7,
            max_chunk: 4.0,
            threshold: 0.4,
            max_speakers: 6,
        }
    }
}

const N_MELS: usize = 40;
const N_MFCC: usize = 20;

/// The most chunks clustered: a 36 MB distance matrix. Two hours of dialogue in 4 s chunks
/// stay under it.
pub const MAX_CHUNKS: usize = 3000;

/// `ch` with runs of neighbouring chunks merged so that at most `max` remain.
fn cap_chunks(ch: Vec<std::ops::Range<usize>>, max: usize) -> Vec<std::ops::Range<usize>> {
    if ch.len() <= max {
        return ch;
    }
    let per = ch.len().div_ceil(max.max(1));
    ch.chunks(per).map(|g| g[0].start..g[g.len() - 1].end).collect()
}

/// Word index ranges of the chunks; `words` are `(start, end)` seconds in time order.
pub fn chunks(words: &[(f64, f64)], p: &Params) -> Vec<std::ops::Range<usize>> {
    let w = words;
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut a = 0;
    for i in 1..=w.len() {
        if i == w.len() || w[i].0 - w[i - 1].1 >= p.chunk_gap || w[i].1 - w[a].0 > p.max_chunk {
            if a < i {
                out.push(a..i);
            }
            a = i;
        }
    }
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for c in out {
        let dur = w[c.end - 1].1 - w[c.start].0;
        match merged.last_mut() {
            Some(l) if dur < p.min_chunk => l.end = c.end,
            _ => merged.push(c),
        }
    }
    if merged.len() > 1 && w[merged[0].end - 1].1 - w[0].0 < p.min_chunk {
        let first = merged.remove(0);
        merged[0].start = first.start;
    }
    merged
}

/// Per frame: the level (dB) and MFCC 0..N_MFCC, frame-major.
fn mfcc_frames(audio: &[f32]) -> (usize, Vec<f32>, Vec<f32>) {
    let dct: Vec<f32> = (0..N_MFCC * N_MELS)
        .map(|i| {
            let (k, m) = (i / N_MELS, i % N_MELS);
            (std::f32::consts::PI * k as f32 * (m as f32 + 0.5) / N_MELS as f32).cos()
        })
        .collect();
    let frames = audio.len() / HOP;
    let mut energy = vec![0f32; frames];
    let mut mfcc = vec![0f32; frames * N_MFCC];
    let mut lm = [0f32; N_MELS];
    mel::mel_frames(audio, N_MELS, |f, mel| {
        let mut e = 0f32;
        for (l, v) in lm.iter_mut().zip(mel) {
            e += v;
            *l = (v + 1e-10).ln();
        }
        energy[f] = 10.0 * (e + 1e-10).log10();
        for k in 0..N_MFCC {
            mfcc[f * N_MFCC + k] = dct[k * N_MELS..(k + 1) * N_MELS].iter().zip(&lm).map(|(d, l)| d * l).sum();
        }
    });
    (frames, energy, mfcc)
}

fn rms_distance(a: &[f32], b: &[f32]) -> f32 {
    (a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f32>() / a.len().max(1) as f32).sqrt()
}

/// Average-linkage clusters over a distance matrix, kept current by Lance–Williams updates,
/// with each live cluster's nearest neighbour cached. A merge rescans only the rows that
/// pointed at the pair and now lie farther from it (a tie keeps its pointer), so typical
/// input costs about O(n²); the worst case stays O(n³), which [`MAX_CHUNKS`] bounds.
struct Clusters {
    n: usize,
    d: Vec<f32>,
    alive: Vec<bool>,
    size: Vec<f32>,
    members: Vec<Vec<usize>>,
    nn: Vec<(f32, usize)>,
}

impl Clusters {
    fn new(d: Vec<f32>, n: usize) -> Self {
        let mut c = Self {
            n,
            d,
            alive: vec![true; n],
            size: vec![1.0; n],
            members: (0..n).map(|i| vec![i]).collect(),
            nn: vec![(f32::INFINITY, 0); n],
        };
        for i in 0..n {
            c.rescan(i);
        }
        c
    }

    fn count(&self) -> usize {
        self.alive.iter().filter(|a| **a).count()
    }

    fn rescan(&mut self, i: usize) {
        let mut best = (f32::INFINITY, i);
        for j in (0..self.n).filter(|&j| j != i && self.alive[j]) {
            if self.d[i * self.n + j] < best.0 {
                best = (self.d[i * self.n + j], j);
            }
        }
        self.nn[i] = best;
    }

    /// The closest live pair `(distance, i, j)`.
    fn closest(&self) -> Option<(f32, usize, usize)> {
        (0..self.n)
            .filter(|&i| self.alive[i] && self.nn[i].0.is_finite())
            .map(|i| (self.nn[i].0, i, self.nn[i].1))
            .min_by(|a, b| a.0.total_cmp(&b.0))
    }

    fn merge(&mut self, i: usize, j: usize) {
        let n = self.n;
        let (si, sj) = (self.size[i], self.size[j]);
        for k in (0..n).filter(|&k| k != i && k != j && self.alive[k]) {
            let v = (si * self.d[i * n + k] + sj * self.d[j * n + k]) / (si + sj);
            self.d[i * n + k] = v;
            self.d[k * n + i] = v;
        }
        self.alive[j] = false;
        self.size[i] = si + sj;
        let moved = std::mem::take(&mut self.members[j]);
        self.members[i].extend(moved);
        self.rescan(i);
        for k in 0..n {
            if k == i || !self.alive[k] {
                continue;
            }
            if self.nn[k].1 == i || self.nn[k].1 == j {
                // The merged distance averages two that were at least the old minimum: equal
                // to it, it is still the minimum.
                if self.d[k * n + i] <= self.nn[k].0 {
                    self.nn[k] = (self.d[k * n + i], i);
                } else {
                    self.rescan(k);
                }
            } else if self.d[k * n + i] < self.nn[k].0 {
                self.nn[k] = (self.d[k * n + i], i);
            }
        }
    }
}

/// Speaker labels for `words` (`(start, end)` seconds, in time order) from `audio` (mono
/// 16 kHz, the words' time base): one label per word, numbered by first appearance from 0,
/// and the number of speakers. No words is no speakers.
pub fn diarize(audio: &[f32], words: &[(f64, f64)], p: &Params) -> (Vec<usize>, usize) {
    if words.is_empty() {
        return (Vec::new(), 0);
    }
    let one = || (vec![0; words.len()], 1);
    let ch = cap_chunks(chunks(words, p), MAX_CHUNKS);
    if ch.len() < 2 {
        return one();
    }
    let (frames, energy, mfcc) = mfcc_frames(audio);
    let frame_of = |t: f64| (((t * f64::from(RATE)).round().max(0.0) as usize) / HOP).min(frames);
    let dim = 2 * (N_MFCC - 1);
    let mut vecs: Vec<Vec<f32>> = Vec::with_capacity(ch.len());
    let mut weight: Vec<f32> = Vec::with_capacity(ch.len());
    for c in &ch {
        let a = frame_of(words[c.start].0);
        let b = frame_of(words[c.end - 1].1).max(a);
        let peak = energy[a..b].iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let voiced: Vec<usize> = (a..b).filter(|&f| energy[f] > peak - 30.0).collect();
        let mut v = vec![0f32; dim];
        if !voiced.is_empty() {
            let n = voiced.len() as f32;
            for k in 1..N_MFCC {
                let mean = voiced.iter().map(|&f| mfcc[f * N_MFCC + k]).sum::<f32>() / n;
                let var = voiced.iter().map(|&f| (mfcc[f * N_MFCC + k] - mean).powi(2)).sum::<f32>() / n;
                v[k - 1] = mean;
                v[N_MFCC - 1 + k - 1] = var.sqrt();
            }
        }
        vecs.push(v);
        weight.push(voiced.len() as f32);
    }
    let all: Vec<usize> = {
        let peak = energy.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        (0..frames).filter(|&f| energy[f] > peak - 40.0).collect()
    };
    let nf = all.len().max(1) as f32;
    for k in 1..N_MFCC {
        let mean = all.iter().map(|&f| mfcc[f * N_MFCC + k]).sum::<f32>() / nf;
        let sd = (all.iter().map(|&f| (mfcc[f * N_MFCC + k] - mean).powi(2)).sum::<f32>() / nf)
            .sqrt()
            .max(1e-6);
        for v in &mut vecs {
            v[k - 1] = (v[k - 1] - mean) / sd;
            v[N_MFCC - 1 + k - 1] /= sd;
        }
    }
    if vecs.iter().all(|v| rms_distance(v, &vecs[0]) < 1e-6) {
        return one();
    }
    let n = vecs.len();
    let mut dist = vec![0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let d = rms_distance(&vecs[i], &vecs[j]);
            dist[i * n + j] = d;
            dist[j * n + i] = d;
        }
    }
    let mut clusters = Clusters::new(dist, n);
    let max_speakers = p.max_speakers.max(1);
    while clusters.count() > 1 {
        let Some((d, i, j)) = clusters.closest() else { break };
        if d > p.threshold && clusters.count() <= max_speakers {
            break;
        }
        clusters.merge(i, j);
    }
    let total: f32 = weight.iter().sum::<f32>().max(1.0);
    while clusters.count() > 1 {
        let small =
            (0..n).find(|&c| clusters.alive[c] && clusters.members[c].iter().map(|&i| weight[i]).sum::<f32>() < 0.05 * total);
        let Some(s) = small else { break };
        let near = clusters.nn[s].1;
        clusters.merge(near, s);
    }
    if clusters.count() == 1 {
        return one();
    }
    let mut label = vec![0usize; n];
    for (ci, members) in clusters.members.iter().enumerate() {
        for &i in members {
            label[i] = ci;
        }
    }
    let mut order: Vec<usize> = Vec::new();
    for &l in &label {
        if !order.contains(&l) {
            order.push(l);
        }
    }
    let mut out = vec![0usize; words.len()];
    for (k, c) in ch.iter().enumerate() {
        let sp = order.iter().position(|&o| o == label[k]).unwrap_or(0);
        out[c.clone()].fill(sp);
    }
    (out, order.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic "voice": a pulse train at `f0` through two resonances (formants), with a
    /// slow syllable envelope.
    fn voice(f0: f32, formants: [(f32, f32); 2], secs: f32, seed: u32) -> Vec<f32> {
        let n = (secs * 16_000.0) as usize;
        let mut out = vec![0f32; n];
        let mut phase = 0f32;
        let mut st = [[0f32; 2]; 2];
        let mut rng = seed;
        for (i, o) in out.iter_mut().enumerate() {
            let t = i as f32 / 16_000.0;
            let jitter = 1.0 + 0.03 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
            phase += f0 * jitter / 16_000.0;
            let mut x = if phase >= 1.0 {
                phase -= 1.0;
                1.0
            } else {
                0.0
            };
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            x += ((rng >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.02;
            let mut y = 0f32;
            for (k, (fc, bw)) in formants.iter().enumerate() {
                let r = (-std::f32::consts::PI * bw / 16_000.0).exp();
                let a1 = 2.0 * r * (2.0 * std::f32::consts::PI * fc / 16_000.0).cos();
                let a2 = -r * r;
                let v = x + a1 * st[k][0] + a2 * st[k][1];
                st[k][1] = st[k][0];
                st[k][0] = v;
                y += v;
            }
            let env = (std::f32::consts::PI * (t * 4.0).fract()).sin().max(0.0);
            *o = y * env * 0.05;
        }
        out
    }

    #[test]
    fn two_synthetic_voices_are_told_apart() {
        let a = |s| voice(110.0, [(600.0, 90.0), (1100.0, 120.0)], 2.0, s);
        let b = |s| voice(230.0, [(400.0, 80.0), (2600.0, 150.0)], 2.0, s);
        let silence = vec![0f32; 8000];
        let turns = [true, false, true, true, false, false, true, false];
        let mut audio = Vec::new();
        let mut words = Vec::new();
        for (k, &is_a) in turns.iter().enumerate() {
            let t0 = audio.len() as f64 / 16_000.0;
            audio.extend(if is_a { a(k as u32) } else { b(k as u32) });
            for w in 0..4 {
                let s = t0 + w as f64 * 0.5;
                words.push((s, s + 0.45));
            }
            audio.extend(&silence);
        }
        let (labels, n) = diarize(&audio, &words, &Params::default());
        assert_eq!(n, 2);
        for (k, &is_a) in turns.iter().enumerate() {
            let want = if is_a { 0 } else { 1 };
            assert!(labels[k * 4..k * 4 + 4].iter().all(|l| *l == want), "turn {k}: {labels:?}");
        }
    }

    #[test]
    fn one_voice_is_one_speaker() {
        let audio = voice(140.0, [(500.0, 90.0), (1500.0, 120.0)], 12.0, 1);
        let words: Vec<(f64, f64)> = (0..20).map(|i| (i as f64 * 0.6, i as f64 * 0.6 + 0.4)).collect();
        let (labels, n) = diarize(&audio, &words, &Params::default());
        assert_eq!(n, 1);
        assert!(labels.iter().all(|l| *l == 0));
    }

    #[test]
    fn identical_voices_are_one_speaker_without_clustering() {
        let audio = vec![0f32; 16_000 * 60];
        let words: Vec<(f64, f64)> = (0..60).map(|i| (i as f64, i as f64 + 0.7)).collect();
        let (labels, n) = diarize(&audio, &words, &Params::default());
        assert_eq!(n, 1);
        assert!(labels.iter().all(|l| *l == 0));
    }

    #[test]
    fn tied_distances_merge_without_rescanning_every_row() {
        let n = 2000;
        let mut c = Clusters::new(vec![0f32; n * n], n);
        while c.count() > 1 {
            let (_, i, j) = c.closest().unwrap();
            c.merge(i, j);
        }
        assert_eq!(c.members.iter().map(Vec::len).max(), Some(n));
    }

    #[test]
    fn too_many_chunks_merge_neighbours() {
        let ch: Vec<std::ops::Range<usize>> = (0..10).map(|i| i..i + 1).collect();
        assert_eq!(cap_chunks(ch.clone(), 20), ch);
        assert_eq!(cap_chunks(ch, 4), vec![0..3, 3..6, 6..9, 9..10]);
    }

    #[test]
    fn no_words_no_speakers() {
        assert_eq!(diarize(&[0.0; 1600], &[], &Params::default()), (Vec::new(), 0));
    }

    #[test]
    fn chunks_split_at_pauses_and_join_short_ones() {
        let words = [(0.0, 0.5), (0.5, 1.0), (2.0, 2.2), (3.0, 3.6), (3.6, 4.4)];
        assert_eq!(chunks(&words, &Params::default()), vec![0..3, 3..5]);
    }

    #[test]
    fn average_linkage_merges_the_nearest_pairs_first() {
        let d = |a: f32, b: f32| (a - b).abs();
        let pts = [0.0f32, 10.0, 20.0, 30.0];
        let n = pts.len();
        let mut m = vec![0f32; n * n];
        for i in 0..n {
            for j in 0..n {
                m[i * n + j] = d(pts[i], pts[j]);
            }
        }
        let mut c = Clusters::new(m, n);
        while c.count() > 2 {
            let (_, i, j) = c.closest().unwrap();
            c.merge(i, j);
        }
        let mut groups: Vec<Vec<usize>> = c.members.iter().filter(|g| !g.is_empty()).cloned().collect();
        for g in &mut groups {
            g.sort();
        }
        groups.sort();
        assert_eq!(groups, vec![vec![0, 1], vec![2, 3]]);
    }
}
