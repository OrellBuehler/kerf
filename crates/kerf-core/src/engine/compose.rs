//! Compose: render a [`MusicScore`] to audio, in-process.
//!
//! The score becomes notes ([`crate::model::expand_score`]), the notes become sound through
//! `rustysynth` (a pure-Rust SoundFont synthesizer — nothing native to ship) playing
//! **GeneralUser GS** v2.0.3, a General MIDI SoundFont whose licence lets anyone use the music
//! it makes for anything. The SoundFont (~32 MB) is downloaded on the first composition through
//! the same resumable, cancellable fetch the speech and voice models use (pinned by SHA-256;
//! `KERF_SOUNDFONT_URL` points it at a mirror) into `<cache>/kerf/soundfonts/`.
//!
//! Every role of the arrangement — drums, the top of the kit, bass, pad, keys, lead, effects — is
//! rendered by a synthesizer of its own, all sharing the one loaded SoundFont, and the roles are
//! mixed in Rust: role gains from the style's preset, a sidechain gain that dips the bass and pad
//! at every kick, the picture's dropouts (everything but the effects goes silent), then a soft
//! limit and a peak normalization to −1 dBFS. Loudness stays the master bus's and the export
//! preset's job. The mix is made a block at a time, so memory is the finished stereo mix and
//! nothing else. Output is 48 kHz stereo 16-bit WAV in the **data** directory
//! (`<data>/kerf/music/music-<hash>.wav` — the only copy, like a voiceover), named by a hash of
//! the score, the SoundFont and [`RENDERER_VERSION`], so the same score is a cache hit and the
//! same asset.
//!
//! A render is a foreground job (`cpu::lease_waiting`: someone is waiting on it), never holds the
//! project lock, and polls its cancel between blocks.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::download::{fetch, Download, DownloadProgress};
use super::tts::{download_progress, ProgressFn};
use crate::error::{Error, Result};
use crate::model::{expand_score, Arrangement, Makeup, MusicScore, MusicStyle, NoteEvent, Role, ROLES};

/// The rate scores are rendered at.
pub const SAMPLE_RATE: u32 = 48_000;

/// Bump when what a score sounds like changes (patterns, presets, the mix), so a render made
/// before is not reused for it.
pub const RENDERER_VERSION: u32 = 1;

/// The final peak, dBFS.
pub const PEAK_DBFS: f64 = -1.0;

const SOUNDFONT_NAME: &str = "GeneralUser-GS-v2.0.3.sf2";
/// GeneralUser GS v2.0.3, at a pinned commit of its repository.
const SOUNDFONT_URL: &str =
    "https://raw.githubusercontent.com/mrbumpy409/GeneralUser-GS/684543d5e5efaef08d02be50dcda8d552478fa60/GeneralUser-GS.sf2";
const SOUNDFONT_SHA256: &str = "9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe";
const SOUNDFONT_BYTES: u64 = 32_319_396;

/// Frames mixed per step: a tenth of a second.
const BLOCK_FRAMES: usize = 4_800;

// ---- where things live ---------------------------------------------------------------------

/// The SoundFont's URL: the pinned one, or `KERF_SOUNDFONT_URL` (a mirror of the same file).
pub fn soundfont_url() -> String {
    std::env::var("KERF_SOUNDFONT_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| SOUNDFONT_URL.to_string())
}

/// Where the SoundFont is kept: `<cache>/kerf/soundfonts/GeneralUser-GS-v2.0.3.sf2`.
pub fn soundfont_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("kerf").join("soundfonts").join(SOUNDFONT_NAME))
}

/// Where composed music is written: the *data* directory, because this file is the only copy of
/// the audio.
pub fn output_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("kerf").join("music"))
}

/// The file `score` renders to. Named by a hash of the renderer version, the SoundFont and the
/// score itself, so a score is rendered once.
pub fn output_path(score: &MusicScore) -> Option<PathBuf> {
    let mut hash = Sha256::new();
    for part in [
        format!("v{RENDERER_VERSION}"),
        SOUNDFONT_SHA256.to_string(),
        serde_json::to_string(score).ok()?,
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    Some(output_dir()?.join(format!("music-{name}.wav")))
}

// ---- the SoundFont -------------------------------------------------------------------------

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::Engine(format!("could not read the SoundFont: {e}")))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| Error::Engine(format!("could not read the SoundFont: {e}")))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A SoundFont is a RIFF `sfbk` file and the published one has exactly this digest: anything else
/// — an error page, a truncated transfer, a different file at a mirror — is refused before it is
/// renamed into place.
fn verify_soundfont(path: &Path) -> Result<()> {
    let mut head = [0u8; 12];
    let riff = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && &head[0..4] == b"RIFF"
        && &head[8..12] == b"sfbk";
    if !riff {
        return Err(Error::Engine(
            "the downloaded SoundFont is not a SoundFont file (the download URL may be wrong or behind a login)".into(),
        ));
    }
    let got = sha256_file(path)?;
    if got == SOUNDFONT_SHA256 {
        Ok(())
    } else {
        Err(Error::Engine(format!(
            "the downloaded SoundFont does not match its published checksum (got {got})"
        )))
    }
}

/// Fetch the SoundFont unless it is already on disk, returning its path.
pub fn ensure_soundfont(progress: &mut dyn FnMut(DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    let dst = soundfont_path().ok_or_else(|| Error::Engine("no cache directory available for the SoundFont".into()))?;
    let url = soundfont_url();
    fetch(
        &Download {
            url: &url,
            dst: &dst,
            what: "SoundFont",
            mirror_env: "KERF_SOUNDFONT_URL",
            verify: &verify_soundfont,
        },
        progress,
        cancel,
    )
}

/// Download the SoundFont now, so the first composition starts rendering at once.
pub fn prepare(progress: ProgressFn, cancel: &dyn Fn() -> bool) -> Result<()> {
    ensure_soundfont(&mut download_progress("download_soundfont", progress), cancel)?;
    Ok(())
}

/// One style, for a picker.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct StyleInfo {
    pub name: &'static str,
    pub label: &'static str,
    pub default_bpm: f64,
}

/// What composing has on disk and in flight. The Compose dialog and an agent both read it.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct MusicStatus {
    /// The SoundFont is on disk: the next composition renders at once.
    pub soundfont_ready: bool,
    pub soundfont: &'static str,
    /// What the first composition downloads, bytes (0 when it is ready).
    pub approx_download_bytes: u64,
    /// Compositions rendering now.
    pub rendering: usize,
    pub sample_rate: u32,
    pub styles: Vec<StyleInfo>,
}

static RENDERING: AtomicUsize = AtomicUsize::new(0);

struct InFlight;

impl InFlight {
    fn start() -> Self {
        RENDERING.fetch_add(1, Ordering::SeqCst);
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        RENDERING.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn status() -> MusicStatus {
    let ready = soundfont_path().is_some_and(|p| p.is_file());
    MusicStatus {
        soundfont_ready: ready,
        soundfont: SOUNDFONT_NAME,
        approx_download_bytes: if ready { 0 } else { SOUNDFONT_BYTES },
        rendering: RENDERING.load(Ordering::SeqCst),
        sample_rate: SAMPLE_RATE,
        styles: MusicStyle::ALL
            .iter()
            .map(|s| StyleInfo {
                name: s.name(),
                label: s.label(),
                default_bpm: s.default_bpm(),
            })
            .collect(),
    }
}

// ---- the mix, pure -------------------------------------------------------------------------

/// How far each of `len` frames from `start` is into a kick's duck, 0..=1: the dip begins
/// `attack_s` before the kick (so a bass note's own attack is already down under it), is full at
/// the kick, and recovers over `release_s` along a squared curve. Overlapping kicks take the
/// deeper dip. `kicks` is sorted, in seconds.
pub fn sidechain_curve(kicks: &[f64], start: usize, len: usize, rate: u32, attack_s: f32, release_s: f32) -> Vec<f32> {
    let mut out = vec![0.0_f32; len];
    let r = rate as f64;
    let (attack, release) = (attack_s.max(1e-4) as f64, release_s.max(1e-3) as f64);
    let (first, last) = (start as f64 / r, (start + len) as f64 / r);
    // The kicks whose dip can reach this block.
    let from = kicks.partition_point(|k| *k + release < first);
    for k in &kicks[from..] {
        if *k - attack > last {
            break;
        }
        let lo = (((*k - attack) * r).floor() as i64 - start as i64).max(0) as usize;
        let hi = ((((*k + release) * r).ceil() as i64) - start as i64).clamp(0, len as i64) as usize;
        for (i, slot) in out.iter_mut().enumerate().take(hi).skip(lo) {
            let t = (start + i) as f64 / r - *k;
            let dip = if t < 0.0 {
                1.0 + t / attack
            } else {
                let x = 1.0 - t / release;
                x * x
            };
            if dip > 0.0 {
                *slot = slot.max(dip as f32);
            }
        }
    }
    out
}

/// The gain of everything a dropout silences, for `len` frames from `start`: 0 inside each
/// `(from, to)` window, a `fade_s` ramp down ending at `from` and one up ending at `to` — so a
/// note that begins on `to` is whole.
pub fn gate_curve(windows: &[(f64, f64)], start: usize, len: usize, rate: u32, fade_s: f64) -> Vec<f32> {
    let mut out = vec![1.0_f32; len];
    let r = rate as f64;
    for (from, to) in windows {
        let lo = (((*from - fade_s) * r).floor() as i64 - start as i64).max(0) as usize;
        let hi = (((*to * r).ceil() as i64) - start as i64).clamp(0, len as i64) as usize;
        for (i, slot) in out.iter_mut().enumerate().take(hi).skip(lo) {
            let t = (start + i) as f64 / r;
            let down = ((*from - t) / fade_s).clamp(0.0, 1.0);
            let up = ((t - (*to - fade_s)) / fade_s).clamp(0.0, 1.0);
            let g = if t < *from {
                down
            } else if t < *to - fade_s {
                0.0
            } else {
                up
            };
            *slot = slot.min(g as f32);
        }
    }
    out
}

/// The gain of the sections' make-up ([`Arrangement::makeup`]) for `len` frames from `start`: a
/// section's own gain, easing to the next section's over the `fade_s` before the boundary (so
/// what the next section plays on its first beat is at its level whole). After the last section
/// the last gain holds.
pub fn makeup_curve(makeup: &[Makeup], start: usize, len: usize, rate: u32, fade_s: f64) -> Vec<f32> {
    let mut out = vec![1.0_f32; len];
    if makeup.is_empty() {
        return out;
    }
    let r = rate as f64;
    for (i, slot) in out.iter_mut().enumerate() {
        let t = (start + i) as f64 / r;
        let at = makeup.partition_point(|m| m.end_s <= t).min(makeup.len() - 1);
        let here = makeup[at];
        let mut g = here.gain as f64;
        if let Some(next) = makeup.get(at + 1) {
            let x = ((t - (here.end_s - fade_s)) / fade_s).clamp(0.0, 1.0);
            let eased = x * x * (3.0 - 2.0 * x);
            g += (next.gain as f64 - g) * eased;
        }
        *slot = g as f32;
    }
    out
}

/// The knee of the soft clip: below it a sample passes untouched.
const KNEE: f32 = 0.8;

fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        x.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh())
    }
}

/// Make the mix ready to be a file: raise it until the loudest 0.5 % of samples sit at 0.7
/// (no more than 6 dB over full scale before clipping), soft-clip what passes the knee, and
/// scale so the peak is exactly [`PEAK_DBFS`]. Returns that peak (linear), or 0 for silence.
pub fn master(left: &mut [f32], right: &mut [f32]) -> f32 {
    let peak = left.iter().chain(right.iter()).fold(0.0_f32, |m, s| m.max(s.abs()));
    if peak < 1e-9 {
        return 0.0;
    }
    // The 99.5th percentile of |x|, from a histogram over [0, peak].
    const BINS: usize = 4096;
    let mut hist = vec![0u64; BINS];
    for s in left.iter().chain(right.iter()) {
        hist[((s.abs() / peak) * (BINS - 1) as f32) as usize] += 1;
    }
    let total = (left.len() + right.len()) as u64;
    let mut seen = 0;
    let mut p995 = peak;
    for (i, n) in hist.iter().enumerate() {
        seen += n;
        if seen as f64 >= total as f64 * 0.995 {
            p995 = (i as f32 + 1.0) / BINS as f32 * peak;
            break;
        }
    }
    let gain = (0.7 / p995.max(1e-9)).min(2.0 / peak);
    let mut clipped_peak = 0.0_f32;
    for s in left.iter_mut().chain(right.iter_mut()) {
        *s = soft_clip(*s * gain);
        clipped_peak = clipped_peak.max(s.abs());
    }
    let target = 10f32.powf(PEAK_DBFS as f32 / 20.0);
    let scale = target / clipped_peak.max(1e-9);
    for s in left.iter_mut().chain(right.iter_mut()) {
        *s *= scale;
    }
    target
}

/// Make the ends of the mix click-free: a 2 ms fade in (a note that begins on the first sample)
/// and a raised-cosine fade out over the last `out_s` seconds (what rings in the release tail is
/// cut by the file's end otherwise). Never longer than a quarter of the mix.
pub fn edge_fades(left: &mut [f32], right: &mut [f32], rate: u32, out_s: f64) {
    let len = left.len().min(right.len());
    let r = rate as f64;
    let fade_in = ((0.002 * r) as usize).min(len / 4);
    let fade_out = ((out_s * r) as usize).min(len / 4);
    for i in 0..fade_in {
        let g = (i as f32 / fade_in as f32).min(1.0);
        left[i] *= g;
        right[i] *= g;
    }
    for i in 0..fade_out {
        let x = i as f32 / fade_out as f32;
        let g = 0.5 + 0.5 * (std::f32::consts::PI * x).cos();
        left[len - fade_out + i] *= g;
        right[len - fade_out + i] *= g;
    }
}

/// A 16-bit stereo WAV: the header, then the samples with a triangular dither of one least
/// significant bit (a generator seeded the same every time, so a file is reproducible).
pub fn write_wav16(out: &mut dyn Write, left: &[f32], right: &[f32], rate: u32) -> std::io::Result<()> {
    let frames = left.len().min(right.len());
    let data_len = (frames * 4) as u32;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?;
    out.write_all(&2u16.to_le_bytes())?;
    out.write_all(&rate.to_le_bytes())?;
    out.write_all(&(rate * 4).to_le_bytes())?;
    out.write_all(&4u16.to_le_bytes())?;
    out.write_all(&16u16.to_le_bytes())?;
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut noise = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / (1u64 << 24) as f32
    };
    let mut buf = Vec::with_capacity(64 * 1024);
    for i in 0..frames {
        for s in [left[i], right[i]] {
            let dither = noise() - noise();
            let v = (s * 32767.0 + dither).round().clamp(-32768.0, 32767.0) as i16;
            buf.extend_from_slice(&v.to_le_bytes());
        }
        if buf.len() >= 64 * 1024 - 4 {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    out.write_all(&buf)
}

// ---- scheduling notes onto a synthesizer ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Off,
    On(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timed {
    frame: usize,
    channel: u8,
    key: u8,
    kind: Kind,
}

/// The note-ons and -offs of `notes` as frames, in the order a synthesizer must meet them: by
/// time, and at one frame a note-off first (a key struck again the instant it ends).
fn schedule(notes: &[&NoteEvent], rate: u32) -> Vec<Timed> {
    let r = rate as f64;
    let mut out = Vec::with_capacity(notes.len() * 2);
    for n in notes {
        let on = (n.start_s * r).round() as usize;
        let off = (((n.start_s + n.dur_s) * r).round() as usize).max(on + 1);
        out.push(Timed {
            frame: on,
            channel: n.channel,
            key: n.key,
            kind: Kind::On(n.velocity),
        });
        out.push(Timed {
            frame: off,
            channel: n.channel,
            key: n.key,
            kind: Kind::Off,
        });
    }
    out.sort_by_key(|e| (e.frame, matches!(e.kind, Kind::On(_))));
    out
}

struct Voice {
    role: Role,
    synth: Synthesizer,
    events: Vec<Timed>,
    next: usize,
    gain: f32,
    duck: f32,
    ducked: bool,
}

impl Voice {
    /// Render `l.len()` frames from `start`, applying the events that fall inside.
    fn render(&mut self, start: usize, l: &mut [f32], r: &mut [f32]) {
        let end = start + l.len();
        let mut pos = start;
        while pos < end {
            while let Some(e) = self.events.get(self.next).copied() {
                if e.frame > pos {
                    break;
                }
                match e.kind {
                    Kind::On(v) => self.synth.note_on(e.channel as i32, e.key as i32, v as i32),
                    Kind::Off => self.synth.note_off(e.channel as i32, e.key as i32),
                }
                self.next += 1;
            }
            let upto = self.events.get(self.next).map_or(end, |e| e.frame.min(end));
            self.synth
                .render(&mut l[pos - start..upto - start], &mut r[pos - start..upto - start]);
            pos = upto;
        }
    }
}

fn make_voice(font: &Arc<SoundFont>, arrangement: &Arrangement, role: Role, rate: u32) -> Result<Option<Voice>> {
    let notes: Vec<&NoteEvent> = arrangement.role(role).collect();
    if notes.is_empty() {
        return Ok(None);
    }
    let settings = SynthesizerSettings::new(rate as i32);
    let mut synth =
        Synthesizer::new(font, &settings).map_err(|e| Error::Engine(format!("could not start the synthesizer: {e}")))?;
    let i = role.index();
    let mut channels: Vec<(u8, u8)> = notes.iter().map(|n| (n.channel, n.program)).collect();
    channels.sort_unstable();
    channels.dedup_by_key(|c| c.0);
    for (channel, program) in channels {
        let ch = channel as i32;
        synth.process_midi_message(ch, 0xC0, program as i32, 0);
        synth.process_midi_message(ch, 0xB0, 7, 100);
        synth.process_midi_message(ch, 0xB0, 10, arrangement.mix.pan[i] as i32);
        synth.process_midi_message(ch, 0xB0, 91, arrangement.mix.reverb[i] as i32);
        synth.process_midi_message(ch, 0xB0, 93, arrangement.mix.chorus[i] as i32);
    }
    let sidechain = &arrangement.mix.sidechain;
    Ok(Some(Voice {
        role,
        synth,
        events: schedule(&notes, rate),
        next: 0,
        gain: arrangement.mix.gain[i],
        duck: sidechain.depth * sidechain.amount(role),
        ducked: sidechain.depth * sidechain.amount(role) > 0.0,
    }))
}

/// Play an arrangement through `font` and mix it: the stereo result, `duration_s` long.
/// `progress` hears the fraction done; `cancel` is checked every block.
fn mix(
    font: &Arc<SoundFont>,
    arrangement: &Arrangement,
    rate: u32,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<(Vec<f32>, Vec<f32>)> {
    let total = (arrangement.duration_s * rate as f64).ceil() as usize;
    let mut voices: Vec<Voice> = Vec::new();
    for role in ROLES {
        if let Some(v) = make_voice(font, arrangement, role, rate)? {
            voices.push(v);
        }
    }
    let mut left = vec![0.0_f32; total];
    let mut right = vec![0.0_f32; total];
    let (mut tl, mut tr) = (vec![0.0_f32; BLOCK_FRAMES], vec![0.0_f32; BLOCK_FRAMES]);
    let sc = arrangement.mix.sidechain;
    let mut reported = -1.0_f64;
    let mut start = 0;
    while start < total {
        if cancel() {
            return Err(Error::Cancelled);
        }
        let len = BLOCK_FRAMES.min(total - start);
        let curve = (voices.iter().any(|v| v.ducked))
            .then(|| sidechain_curve(&arrangement.kicks, start, len, rate, sc.attack_s, sc.release_s));
        let gate = (!arrangement.dropouts.is_empty()).then(|| gate_curve(&arrangement.dropouts, start, len, rate, 0.006));
        let makeup = (!arrangement.makeup.is_empty()).then(|| makeup_curve(&arrangement.makeup, start, len, rate, 0.15));
        let block = start..start + len;
        for v in &mut voices {
            let (l, r) = (&mut tl[..len], &mut tr[..len]);
            v.render(start, l, r);
            let silenced = v.role != Role::Fx;
            for i in 0..len {
                let mut g = v.gain;
                if let (true, Some(c)) = (v.ducked, &curve) {
                    g *= 1.0 - v.duck * c[i];
                }
                if let (true, Some(gate)) = (silenced, &gate) {
                    g *= gate[i];
                }
                left[start + i] += l[i] * g;
                right[start + i] += r[i] * g;
            }
        }
        if let Some(m) = &makeup {
            for (i, g) in m.iter().enumerate() {
                left[block.start + i] *= g;
                right[block.start + i] *= g;
            }
        }
        start += len;
        let f = start as f64 / total as f64;
        if f - reported >= 0.01 || start == total {
            reported = f;
            progress(f);
        }
    }
    Ok((left, right))
}

// ---- rendering a score ---------------------------------------------------------------------

/// A finished composition.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub path: PathBuf,
    pub duration_s: f64,
    /// The file was already there (the same score was rendered before).
    pub cached: bool,
}

fn finished(path: &Path) -> bool {
    let mut head = [0u8; 12];
    std::fs::metadata(path).is_ok_and(|m| m.len() > 44)
        && std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok()
        && &head[0..4] == b"RIFF"
        && &head[8..12] == b"WAVE"
}

/// Render `score` to [`output_path`]: download the SoundFont if it is missing (`download_soundfont`),
/// wait for the machine's slot (`waiting`), then play and mix it (`render`) — a foreground job
/// that holds `cpu::lease` and checks `cancel` every tenth of a second of audio. A cancel writes
/// nothing. A score rendered before is returned from disk.
pub fn render(score: &MusicScore, progress: ProgressFn, cancel: &dyn Fn() -> bool) -> Result<Rendered> {
    score.validate()?;
    let arrangement = expand_score(score)?;
    let path = output_path(score).ok_or_else(|| Error::Engine("no data directory for music".into()))?;
    let duration_s = arrangement.duration_s;
    if finished(&path) {
        return Ok(Rendered {
            path,
            duration_s,
            cached: true,
        });
    }
    let _in_flight = InFlight::start();
    let soundfont = ensure_soundfont(&mut download_progress("download_soundfont", progress), cancel)?;
    let lease = super::cpu::lease_waiting(
        &mut |wait| progress("waiting", None, Some(wait.message().to_string())),
        cancel,
    )?;
    progress("render", Some(0.0), None);
    let font = Arc::new(
        SoundFont::new(&mut BufReader::new(
            std::fs::File::open(&soundfont).map_err(|e| Error::Engine(format!("could not open the SoundFont: {e}")))?,
        ))
        .map_err(|e| Error::Engine(format!("could not read the SoundFont: {e}")))?,
    );
    let total = duration_s;
    let (mut left, mut right) = mix(
        &font,
        &arrangement,
        SAMPLE_RATE,
        &mut |f| progress("render", Some(f), Some(format!("{:.0} of {:.0} s", f * total, total))),
        cancel,
    )?;
    drop(lease);
    master(&mut left, &mut right);
    edge_fades(&mut left, &mut right, SAMPLE_RATE, 0.3);

    let dir = path.parent().expect("music path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| Error::Engine(format!("could not create the music folder: {e}")))?;
    let tmp = path.with_extension(format!("{}.part", std::process::id()));
    let written = std::fs::File::create(&tmp)
        .and_then(|f| {
            let mut w = BufWriter::new(f);
            write_wav16(&mut w, &left, &right, SAMPLE_RATE)?;
            w.flush()
        })
        .and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Engine(format!("could not write the music: {e}")));
    }
    Ok(Rendered {
        path,
        duration_s,
        cached: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ScoreSection, SectionKind};

    const R: u32 = 48_000;

    #[test]
    fn a_sidechain_dips_at_the_kick_and_recovers() {
        let kicks = [1.0, 1.5];
        let curve = sidechain_curve(&kicks, 0, 2 * R as usize, R, 0.004, 0.25);
        let at = |t: f64| curve[(t * R as f64) as usize];
        assert!(at(0.9) == 0.0, "before the first dip begins");
        assert!(
            at(0.998) > 0.4 && at(0.998) < 1.0,
            "the dip begins just before the kick: {}",
            at(0.998)
        );
        assert!((at(1.0) - 1.0).abs() < 0.01, "full at the kick");
        assert!(at(1.1) < at(1.05) && at(1.05) < at(1.0), "recovering");
        assert!(at(1.2) < 0.1, "nearly recovered after 0.2 s: {}", at(1.2));
        // The second kick comes before the first is over: the deeper dip wins, never the sum.
        assert!((at(1.5) - 1.0).abs() < 0.01);
        assert!(curve.iter().all(|c| (0.0..=1.0).contains(c)));
        assert!(at(1.9) == 0.0);
    }

    #[test]
    fn a_sidechain_block_is_the_same_curve_as_the_whole() {
        let kicks: Vec<f64> = (0..8).map(|k| 0.3 + k as f64 * 0.5).collect();
        let whole = sidechain_curve(&kicks, 0, 4 * R as usize, R, 0.004, 0.22);
        let mut stitched = Vec::new();
        for start in (0..4 * R as usize).step_by(4_800) {
            stitched.extend(sidechain_curve(&kicks, start, 4_800, R, 0.004, 0.22));
        }
        assert_eq!(whole, stitched);
    }

    #[test]
    fn make_up_gain_eases_into_the_next_section_before_its_first_beat() {
        let m = [
            Makeup {
                start_s: 0.0,
                end_s: 4.0,
                gain: 2.0,
            },
            Makeup {
                start_s: 4.0,
                end_s: 8.0,
                gain: 1.0,
            },
        ];
        let g = makeup_curve(&m, 0, 9 * R as usize, R, 0.15);
        let at = |t: f64| g[(t * R as f64) as usize];
        assert_eq!(at(1.0), 2.0);
        assert!(at(3.9) < 2.0 && at(3.9) > 1.0, "easing: {}", at(3.9));
        assert!(at(4.0) == 1.0, "the next section's first beat is at its own level");
        assert_eq!(at(6.0), 1.0);
        assert_eq!(at(8.5), 1.0, "the tail holds the last gain");
        // A block is the same curve as the whole.
        let part = makeup_curve(&m, 3 * R as usize, R as usize, R, 0.15);
        assert_eq!(part[..], g[3 * R as usize..4 * R as usize]);
        assert!(makeup_curve(&[], 0, 4, R, 0.15).iter().all(|g| *g == 1.0));
    }

    #[test]
    fn a_dropout_silences_its_window_and_leaves_the_downbeat_whole() {
        let g = gate_curve(&[(1.0, 1.5)], 0, 2 * R as usize, R, 0.006);
        let at = |t: f64| g[(t * R as f64) as usize];
        assert_eq!(at(0.9), 1.0);
        assert!(at(0.998) < 1.0, "ramping down into the window");
        assert_eq!(at(1.0), 0.0);
        assert_eq!(at(1.25), 0.0);
        assert!(at(1.497) > 0.0 && at(1.497) < 1.0, "ramping up to the hit");
        assert!(at(1.5) >= 1.0 - 1e-6, "a note that begins on the window's end is whole");
        assert_eq!(at(1.8), 1.0);
        let none = gate_curve(&[], 0, 100, R, 0.006);
        assert!(none.iter().all(|g| *g == 1.0));
    }

    #[test]
    fn mastering_peaks_at_minus_one_dbfs_and_never_clips() {
        let target = 10f32.powf(-1.0 / 20.0);
        // A quiet mix with one loud transient.
        let mut l: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.1).collect();
        let mut r = l.clone();
        l[1000] = 0.9;
        r[2000] = -0.95;
        let peak = master(&mut l, &mut r);
        let got = l.iter().chain(&r).fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((got - target).abs() < 1e-4, "peak {got} vs {target}");
        assert!((peak - target).abs() < 1e-6);
        // The body came up (the original rms is 0.07): the soft limit tames the spike instead of
        // the spike setting the level, within the six dB over full scale it allows.
        let rms = (l.iter().map(|s| s * s).sum::<f32>() / l.len() as f32).sqrt();
        assert!(rms > 0.11, "rms {rms}");

        // A very quiet sine is normalized up to the same peak with no distortion.
        let mut l: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.001).collect();
        let mut r = l.clone();
        master(&mut l, &mut r);
        let got = l.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        assert!((got - target).abs() < 1e-4);
        // Silence stays silence.
        let (mut l, mut r) = (vec![0.0; 100], vec![0.0; 100]);
        assert_eq!(master(&mut l, &mut r), 0.0);
        assert!(l.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn the_ends_of_the_mix_fade_so_nothing_clicks() {
        let n = 2 * R as usize;
        let (mut l, mut r) = (vec![0.5_f32; n], vec![-0.5_f32; n]);
        edge_fades(&mut l, &mut r, R, 0.3);
        assert_eq!(l[0], 0.0);
        assert!(l[50] > 0.2 && l[50] < 0.5, "fading in");
        assert_eq!(l[R as usize], 0.5, "untouched between");
        assert!(l[n - 1].abs() < 1e-3, "fading out to zero");
        assert!(l[n - 2000] < 0.5 && l[n - 2000] > l[n - 1]);
        assert_eq!(r[0], 0.0);
        assert!(r[n - 1].abs() < 1e-3);
        // A very short mix is faded in proportion, not past its middle.
        let (mut l, mut r) = (vec![1.0_f32; 100], vec![1.0_f32; 100]);
        edge_fades(&mut l, &mut r, R, 0.3);
        assert!(l[50] == 1.0 && l[0] == 0.0);
    }

    #[test]
    fn the_soft_clip_is_transparent_below_the_knee_and_bounded_above() {
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-0.7), -0.7);
        assert!(soft_clip(1.0) > 0.8 && soft_clip(1.0) < 1.0);
        assert!(soft_clip(100.0) <= 1.0);
        assert!(soft_clip(-100.0) >= -1.0);
        let mut last = 0.0;
        for i in 0..400 {
            let y = soft_clip(i as f32 * 0.01);
            assert!(y >= last, "monotonic");
            last = y;
        }
    }

    #[test]
    fn the_wav_is_a_16_bit_stereo_file_of_the_right_length() {
        let l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        let r: Vec<f32> = l.iter().map(|s| -s).collect();
        let mut bytes = Vec::new();
        write_wav16(&mut bytes, &l, &r, R).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1, "PCM");
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), R);
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 4000);
        assert_eq!(bytes.len(), 44 + 4000);
        let s0 = i16::from_le_bytes(bytes[44 + 4 * 500..44 + 4 * 500 + 2].try_into().unwrap());
        assert!((s0 as f32 / 32767.0 - l[500]).abs() < 2.0 / 32767.0);
        // Reproducible: the dither is seeded.
        let mut again = Vec::new();
        write_wav16(&mut again, &l, &r, R).unwrap();
        assert_eq!(bytes, again);
    }

    #[test]
    fn notes_are_scheduled_off_before_on_at_the_same_frame() {
        let n = |key: u8, start: f64, dur: f64| NoteEvent {
            role: Role::Keys,
            channel: 0,
            program: 0,
            key,
            velocity: 90,
            start_s: start,
            dur_s: dur,
        };
        let a = n(60, 0.0, 0.5);
        let b = n(60, 0.5, 0.5);
        let c = n(64, 0.25, 0.1);
        let events = schedule(&[&a, &b, &c], R);
        let frames: Vec<usize> = events.iter().map(|e| e.frame).collect();
        assert!(frames.windows(2).all(|w| w[0] <= w[1]));
        let at_half: Vec<Kind> = events.iter().filter(|e| e.frame == 24_000).map(|e| e.kind).collect();
        assert_eq!(at_half, vec![Kind::Off, Kind::On(90)]);
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn a_score_names_one_file_and_a_new_score_another() {
        let score = MusicScore {
            bpm: 100.0,
            key: "C".into(),
            style: MusicStyle::Pop,
            sections: vec![ScoreSection {
                kind: SectionKind::Verse,
                bars: 2,
                chords: vec!["C".into()],
                energy: None,
            }],
            hits: vec![],
            seed: 0,
            instruments: None,
        };
        let a = output_path(&score);
        assert_eq!(a, output_path(&score));
        assert_ne!(a, output_path(&score.clone().with_seed(1)));
        let mut other = score;
        other.bpm = 101.0;
        assert_ne!(a, output_path(&other));
        let name = a.unwrap();
        assert!(name.file_name().unwrap().to_string_lossy().starts_with("music-"));
    }

    #[test]
    fn the_status_names_the_soundfont_and_every_style() {
        let s = status();
        assert_eq!(s.styles.len(), MusicStyle::ALL.len());
        assert_eq!(s.soundfont, SOUNDFONT_NAME);
        assert_eq!(s.sample_rate, SAMPLE_RATE);
        if !s.soundfont_ready {
            assert_eq!(s.approx_download_bytes, SOUNDFONT_BYTES);
        }
    }

    /// The audio really sits on the score's grid: the beat tracker, which knows nothing of the
    /// score, finds the tempo and the bars the score says (what lets the analysis be written from
    /// the score, and `fit_music` and the beat snap trust it).
    #[test]
    #[ignore = "downloads the SoundFont, renders audio and drives the ffmpeg binary"]
    fn the_rendered_audio_sits_on_the_scores_beat_grid() {
        for (style, bpm) in [
            (MusicStyle::TropicalHouse, 120.0),
            (MusicStyle::Pop, 104.0),
            (MusicStyle::Rock, 132.0),
        ] {
            let mut score = crate::model::sample_song(style);
            score.bpm = bpm;
            score.sections = vec![
                ScoreSection {
                    kind: SectionKind::Verse,
                    bars: 8,
                    chords: vec!["i".into(), "VI".into(), "III".into(), "VII".into()],
                    energy: None,
                },
                ScoreSection {
                    kind: SectionKind::Drop,
                    bars: 8,
                    chords: vec!["i".into(), "VI".into(), "III".into(), "VII".into()],
                    energy: None,
                },
            ];
            let rendered = render(&score, &mut |_, _, _| {}, &|| false).unwrap();
            let audio = crate::engine::decode_audio(&rendered.path, 22_050, 1).unwrap();
            let music = crate::engine::music::analyze_music(&audio.samples, 22_050).expect("a steady pulse");
            let period = 60.0 / bpm;
            assert!(
                (music.grid.period_s - period).abs() / period < 0.004,
                "{style:?}: {} vs {period}",
                music.grid.period_s
            );
            // The grid sits on the score's eighth notes to within a few ms. (The tracker may lock
            // to the off-beats of a four-on-the-floor beat, whose hats are the broadband accent;
            // that is why the analysis is written from the score and not left to it.)
            let half = period / 2.0;
            let phase = music.grid.phase_s.rem_euclid(half);
            let off = phase.min(half - phase);
            assert!(off < 0.02, "{style:?}: the grid is {off} s off the score's eighths");
        }
    }

    fn loudness_of(l: &[f32], r: &[f32]) -> f64 {
        let mut meter = crate::engine::bands::BandMeter::new(
            SAMPLE_RATE,
            2,
            &[crate::engine::bands::BandWindow {
                start: 0.0,
                end: l.len() as f64 / SAMPLE_RATE as f64,
                label: None,
            }],
        )
        .unwrap();
        let inter: Vec<f32> = l.iter().zip(r).flat_map(|(a, b)| [*a, *b]).collect();
        meter.push(&inter);
        meter.finish().remove(0).loudness_lufs.unwrap_or(-120.0)
    }

    /// Calibration: each role of a full song solo, per style — its loudness and the share of the
    /// mix it takes against the share its style aims for (asserted within 1.5 points + 25 %: change
    /// a pattern or a program and this says which gain to move, and by how much). `KERF_COMPOSE_DUMP=<dir>`
    /// also writes the finished songs there. The gains in the presets are the multipliers this
    /// converges on.
    #[test]
    #[ignore = "downloads the SoundFont and renders audio"]
    #[allow(clippy::print_stderr)]
    fn calibration_report() {
        let font_path = ensure_soundfont(&mut |_| {}, &|| false).unwrap();
        let font = Arc::new(SoundFont::new(&mut BufReader::new(std::fs::File::open(font_path).unwrap())).unwrap());
        let mut off: Vec<String> = Vec::new();
        for style in MusicStyle::ALL {
            let score = crate::model::sample_song(style);
            let arrangement = expand_score(&score).unwrap();
            let started = std::time::Instant::now();
            let (mut l, mut r) = mix(&font, &arrangement, SAMPLE_RATE, &mut |_| {}, &|| false).unwrap();
            let took = started.elapsed().as_secs_f64();
            let mix_lufs = loudness_of(&l, &r);
            eprintln!(
                "{style:?}: {:.0} s of music in {took:.1} s, mix {mix_lufs:.1} LUFS (unmastered)",
                arrangement.duration_s
            );
            let target: Vec<f64> = crate::model::style_shares(style).iter().map(|v| *v as f64).collect();
            let mut shares = [0.0; 7];
            for role in ROLES {
                let mut solo = arrangement.clone();
                solo.notes.retain(|n| n.role == role);
                if solo.notes.is_empty() {
                    continue;
                }
                let (sl, sr) = mix(&font, &solo, SAMPLE_RATE, &mut |_| {}, &|| false).unwrap();
                let lufs = loudness_of(&sl, &sr);
                let share = 10f64.powf((lufs - mix_lufs) / 10.0);
                shares[role.index()] = share;
                eprintln!(
                    "  {:6} {:6.1} LUFS  share {:5.1}%  aims {:4.1}%  gain x{:.2} -> {:.2}",
                    role.name(),
                    lufs,
                    share * 100.0,
                    target[role.index()] * 100.0,
                    (target[role.index()] / share).sqrt(),
                    arrangement.mix.gain[role.index()] as f64 * (target[role.index()] / share).sqrt()
                );
                let aim = target[role.index()];
                if (share - aim).abs() > 0.015 + 0.25 * aim {
                    off.push(format!(
                        "{style:?} {} takes {:.1}% of the loudness; its preset aims at {:.1}%",
                        role.name(),
                        share * 100.0,
                        aim * 100.0
                    ));
                }
            }
            eprintln!("  shares add to {:.0}%", shares.iter().sum::<f64>() * 100.0);
            master(&mut l, &mut r);
            // Where the energy sits, per section of the song.
            let spans = score.section_spans().unwrap();
            let windows: Vec<crate::engine::bands::BandWindow> = spans
                .iter()
                .map(|s| crate::engine::bands::BandWindow {
                    start: s.start_s,
                    end: s.end_s,
                    label: Some(s.kind.name().to_string()),
                })
                .chain([crate::engine::bands::BandWindow {
                    start: 0.0,
                    end: l.len() as f64 / SAMPLE_RATE as f64 - 0.01,
                    label: Some("all".to_string()),
                }])
                .collect();
            let mut meter = crate::engine::bands::BandMeter::new(SAMPLE_RATE, 2, &windows).unwrap();
            let inter: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
            meter.push(&inter);
            for w in meter.finish() {
                let shares: Vec<String> = w
                    .bands
                    .iter()
                    .map(|b| format!("{}:{:4.1}", &b.name[..3.min(b.name.len())], b.share_pct))
                    .collect();
                eprintln!(
                    "    {:10} {:6.1} LUFS {:6.1} rms  {}",
                    w.label.unwrap_or_default(),
                    w.loudness_lufs.unwrap_or(-99.0),
                    w.rms_dbfs.unwrap_or(-99.0),
                    shares.join(" ")
                );
            }
            if let Some(dir) = std::env::var_os("KERF_COMPOSE_DUMP") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                let mut out = std::io::BufWriter::new(std::fs::File::create(dir.join(format!("{}.wav", style.name()))).unwrap());
                write_wav16(&mut out, &l, &r, SAMPLE_RATE).unwrap();
            }
        }
        assert!(
            off.is_empty(),
            "out of balance (the gain column above is the fix):\n{}",
            off.join("\n")
        );
    }

    /// Downloads the SoundFont (32 MB, once) and renders a short score of every style: the length
    /// is the score's, the mix is not silent, and the peak is at most −1 dBFS.
    #[test]
    #[ignore = "downloads the SoundFont and renders audio"]
    fn a_short_score_of_every_style_renders_to_a_wav_at_minus_one_dbfs() {
        for style in MusicStyle::ALL {
            let score = MusicScore {
                bpm: 120.0,
                key: "Am".into(),
                style,
                sections: vec![
                    ScoreSection {
                        kind: SectionKind::Verse,
                        bars: 2,
                        chords: vec!["i".into(), "VI".into()],
                        energy: None,
                    },
                    ScoreSection {
                        kind: SectionKind::Build,
                        bars: 2,
                        chords: vec!["iv".into(), "V".into()],
                        energy: None,
                    },
                    ScoreSection {
                        kind: SectionKind::Drop,
                        bars: 2,
                        chords: vec!["i".into(), "VI".into()],
                        energy: None,
                    },
                    ScoreSection {
                        kind: SectionKind::Outro,
                        bars: 2,
                        chords: vec!["i".into(), "V".into()],
                        energy: None,
                    },
                ],
                hits: vec![],
                seed: 3,
                instruments: None,
            };
            let rendered = render(&score, &mut |_, _, _| {}, &|| false).unwrap();
            let bytes = std::fs::read(&rendered.path).unwrap();
            assert_eq!(&bytes[0..4], b"RIFF");
            let frames = (bytes.len() - 44) / 4;
            let secs = frames as f64 / SAMPLE_RATE as f64;
            assert!(
                (secs - score.duration_s()).abs() < 0.01,
                "{style:?}: {secs} vs {}",
                score.duration_s()
            );
            assert!((rendered.duration_s - secs).abs() < 0.01);
            let samples: Vec<f32> = bytes[44..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
                .collect();
            let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
            let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
            assert!(rms > 0.02, "{style:?} is not silent: rms {rms}");
            let dbfs = 20.0 * peak.log10();
            assert!(dbfs <= -0.95 && dbfs > -1.2, "{style:?}: peak {dbfs} dBFS");
            // The same score again is a cache hit on the same file.
            let again = render(&score, &mut |_, _, _| {}, &|| false).unwrap();
            assert!(again.cached && again.path == rendered.path);
        }
    }
}
