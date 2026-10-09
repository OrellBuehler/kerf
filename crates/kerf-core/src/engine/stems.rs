//! Stem separation: Demucs `htdemucs` (MIT) splits music into drums, bass, other and
//! vocals, in-process on ONNX Runtime (the runtime Kokoro voiceover already loads).
//!
//! Only the **network** is ONNX: the model's own STFT does not export, so the spectrogram
//! it reads and the waveform it writes back are computed here, matching
//! `HTDemucs._spec` / `_magnitude` / `_mask` / `_ispec` and torch's `stft` / `istft`
//! (centred, reflect-padded, periodic Hann, `normalized=True`) — the parity tests pin
//! values taken from PyTorch. So is Demucs's `apply_model`: a long file is cut into
//! 7.8 s segments overlapping by a quarter, each padded with its neighbours' audio,
//! and the outputs blended with triangular weights.
//!
//! The model (~170 MB) and the runtime are fetched on first use. The model is a
//! network-only export of the pretrained `htdemucs` weights (see `scripts/` in the docs).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

use super::cli::{bg_command, ffmpeg_bin, fnv1a, launch_err, source_key, status_within};
use super::cpu;
use super::download::{fetch, Download};
use super::pcm::{decode_audio, write_wav, AudioBuffer};
use super::tts::{download_progress, ensure_runtime, init_runtime, ready_runtime, verify_onnx, verify_sha256, Report};
use crate::error::{Error, Result};

/// What the model was trained at; the input is decoded to this.
pub const STEM_SAMPLE_RATE: u32 = 44_100;
/// The model's sources, in its output order.
pub const STEM_NAMES: [&str; 4] = ["drums", "bass", "other", "vocals"];

/// The training segment, 7.8 s at 44.1 kHz: every model call sees exactly this many samples.
const SEGMENT: usize = 343_980;
const NFFT: usize = 4096;
const HOP: usize = 1024;
/// Spectrogram bins the model reads: the STFT's, without the Nyquist bin.
const BINS: usize = NFFT / 2;
/// Frames per segment: `ceil(SEGMENT / HOP)`.
const FRAMES: usize = SEGMENT.div_ceil(HOP);
/// The reflect padding `_spec` adds before torch's own centring.
const PAD: usize = HOP / 2 * 3;
/// Segments overlap by this fraction.
const OVERLAP: f64 = 0.25;

const MODEL_FILE: &str = "htdemucs-v1.onnx";
/// The network-only export of the pretrained `htdemucs` weights, pinned by checksum.
const MODEL_SHA256: &str = "f244dd73484aac5233260af82faa7b4467ad96ce0177d26b6e767c4eb91640a7";
/// Where the model is fetched from; `KERF_DEMUCS_MODEL_URL` points elsewhere (a mirror,
/// or a `file://`-free local copy via the cache path).
const MODEL_URL: &str = "https://github.com/OrellBuehler/kerf/releases/download/models-v1/htdemucs-v1.onnx";
const MODEL_APPROX_BYTES: u64 = 174 * 1024 * 1024;
/// Encoding one stem to FLAC is seconds; anything near this is a hang.
const ENCODE_LIMIT: std::time::Duration = std::time::Duration::from_secs(600);
/// Bump when the separation changes, so cached stems are not reused.
const STEMS_VERSION: u32 = 1;

// ---- provisioning ------------------------------------------------------------------

fn model_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("kerf").join("models").join(MODEL_FILE))
}

fn model_url() -> String {
    std::env::var("KERF_DEMUCS_MODEL_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| MODEL_URL.to_string())
}

fn ensure_model(progress: &mut dyn FnMut(super::download::DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    let dst = model_path().ok_or_else(|| Error::Engine("no cache directory available for the stem model".into()))?;
    let url = model_url();
    let verify = |path: &Path| -> Result<()> {
        verify_onnx(path).map_err(|_| Error::Engine("downloaded stem model is not an ONNX file".into()))?;
        verify_sha256(path, MODEL_SHA256)
            .map_err(|_| Error::Engine("downloaded stem model does not match its published checksum".into()))
    };
    fetch(
        &Download {
            url: &url,
            dst: &dst,
            what: "stem model",
            mirror_env: "KERF_DEMUCS_MODEL_URL",
            verify: &verify,
        },
        progress,
        cancel,
    )
}

/// What stem separation has on disk: whether it would still download the runtime or
/// the model first, and how big the model is.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StemsStatus {
    pub runtime_ready: bool,
    pub model_ready: bool,
    pub model_bytes: u64,
    pub stems: [&'static str; 4],
}

pub fn status() -> StemsStatus {
    StemsStatus {
        runtime_ready: ready_runtime().is_some(),
        model_ready: model_path().is_some_and(|p| p.is_file()),
        model_bytes: MODEL_APPROX_BYTES,
        stems: STEM_NAMES,
    }
}

/// Where `src`'s stems are cached: `<cache>/kerf/stems/<hash>/`, keyed by the file's
/// identity like the waveform cache.
fn stems_dir(src: &Path) -> Option<PathBuf> {
    let key = format!("{}|stems-v{STEMS_VERSION}", source_key(src));
    Some(
        dirs::cache_dir()?
            .join("kerf")
            .join("stems")
            .join(format!("{:016x}", fnv1a(&key))),
    )
}

/// Encode a float WAV to 24-bit FLAC (a 10-minute stem is ~200 MB as float WAV).
fn encode_flac(wav: &Path, flac: &Path) -> Result<()> {
    let bin = ffmpeg_bin();
    let mut cmd = bg_command(&bin);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(wav)
        .args(["-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"])
        .arg(flac)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match status_within(&mut cmd, ENCODE_LIMIT).map_err(|e| launch_err(&bin, e))? {
        Some(s) if s.success() => Ok(()),
        Some(_) => Err(Error::Engine(format!("could not encode {}", flac.display()))),
        None => Err(Error::Engine("encoding a stem timed out".into())),
    }
}

/// Separate `path`'s sound into its four stems (FLAC, 44.1 kHz stereo, in
/// [`STEM_NAMES`] order), downloading the runtime and model on first use. Cached per
/// source file. Reports `download_runtime` / `download_model` / `separate` / `encode`.
pub fn separate(path: &Path, progress: &mut Report, cancel: &dyn Fn() -> bool) -> Result<Vec<PathBuf>> {
    let dir = stems_dir(path).ok_or_else(|| Error::Engine("no cache directory available for stems".into()))?;
    let outs: Vec<PathBuf> = STEM_NAMES.iter().map(|n| dir.join(format!("{n}.flac"))).collect();
    if outs.iter().all(|p| p.is_file()) {
        return Ok(outs);
    }
    let runtime = ensure_runtime(&mut download_progress("download_runtime", progress), cancel)?;
    let model = ensure_model(&mut download_progress("download_model", progress), cancel)?;
    init_runtime(&runtime)?;
    let mix = decode_audio(path, STEM_SAMPLE_RATE, 2)?;
    if mix.frames() == 0 {
        return Err(Error::InvalidArgument("the file has no sound to separate".into()));
    }
    let stems = {
        let lease = cpu::lease();
        let threads = lease.threads();
        progress("separate", Some(0.0), None);
        separate_buffer(&model, threads, &mix, &mut |f| progress("separate", Some(f), None), cancel)?
    };
    std::fs::create_dir_all(&dir)?;
    for (k, (stem, out)) in stems.iter().zip(&outs).enumerate() {
        if cancel() {
            return Err(Error::Cancelled);
        }
        progress("encode", Some(k as f64 / 4.0), Some(STEM_NAMES[k].to_string()));
        let tmp = out.with_extension(format!("{}.wav", std::process::id()));
        let part = out.with_extension(format!("{}.part.flac", std::process::id()));
        let done = write_wav(&tmp, stem)
            .and_then(|()| encode_flac(&tmp, &part))
            .and_then(|()| std::fs::rename(&part, out).map_err(Into::into));
        let _ = std::fs::remove_file(&tmp);
        if done.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        done?;
    }
    progress("encode", Some(1.0), None);
    Ok(outs)
}

// ---- spectrogram -----------------------------------------------------------------

/// `x` padded by reflection (the edge sample not repeated), like torch's `reflect`.
fn reflect_pad(x: &[f32], left: usize, right: usize) -> Vec<f32> {
    let n = x.len();
    let mut out = Vec::with_capacity(left + n + right);
    out.extend((0..left).map(|i| x[(left - i).min(n - 1)]));
    out.extend_from_slice(x);
    out.extend((0..right).map(|i| x[n.saturating_sub(2 + i)]));
    out
}

fn hann() -> Vec<f32> {
    (0..NFFT)
        .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / NFFT as f64).cos()) as f32)
        .collect()
}

/// `HTDemucs._spec` of one channel: `ceil(len / HOP)` frames of `BINS` bins, stored
/// bin-major (`z[f * frames + t]`). Needs more than `PAD` samples.
pub(crate) fn spec(x: &[f32]) -> (Vec<Complex<f32>>, usize) {
    let frames = x.len().div_ceil(HOP);
    let padded = reflect_pad(x, PAD, PAD + frames * HOP - x.len());
    let centred = reflect_pad(&padded, NFFT / 2, NFFT / 2);
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(NFFT);
    let window = hann();
    let scale = 1.0 / (NFFT as f32).sqrt();
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut z = vec![Complex::new(0.0, 0.0); BINS * frames];
    for t in 0..frames {
        // torch's frames 2 .. 2 + frames of the `frames + 4` it makes.
        let start = (t + 2) * HOP;
        for (n, v) in input.iter_mut().enumerate() {
            *v = centred[start + n] * window[n];
        }
        fft.process(&mut input, &mut spectrum).expect("buffers sized by the plan");
        for f in 0..BINS {
            z[f * frames + t] = spectrum[f] * scale;
        }
    }
    (z, frames)
}

/// `HTDemucs._ispec` of one channel: the inverse of [`spec`] (`frames` frames, bin-major)
/// back to `length` samples — the Nyquist bin and two frames either side restored as
/// zeros, overlap-added and divided by the window's squared envelope like `torch.istft`.
pub(crate) fn ispec(z: &[Complex<f32>], frames: usize, length: usize) -> Vec<f32> {
    let total = frames + 4;
    let fft = RealFftPlanner::<f32>::new().plan_fft_inverse(NFFT);
    let window = hann();
    // irfft divides by NFFT; `normalized` multiplies back by sqrt(NFFT).
    let scale = 1.0 / (NFFT as f32).sqrt();
    let span = NFFT + HOP * (total - 1);
    let mut out = vec![0.0_f32; span];
    let mut env = vec![0.0_f32; span];
    let mut spectrum = fft.make_input_vec();
    let mut frame = fft.make_output_vec();
    for t in 0..total {
        for (f, s) in spectrum.iter_mut().enumerate() {
            *s = if (2..frames + 2).contains(&t) && f < BINS {
                z[f * frames + (t - 2)]
            } else {
                Complex::new(0.0, 0.0)
            };
        }
        // A real signal's DC (and Nyquist) bin has no imaginary part; torch ignores it.
        spectrum[0].im = 0.0;
        fft.process(&mut spectrum, &mut frame).expect("buffers sized by the plan");
        let at = t * HOP;
        for n in 0..NFFT {
            out[at + n] += frame[n] * scale * window[n];
            env[at + n] += window[n] * window[n];
        }
    }
    let start = NFFT / 2 + PAD;
    (start..start + length)
        .map(|i| if env[i] > 1e-11 { out[i] / env[i] } else { 0.0 })
        .collect()
}

// ---- the model -------------------------------------------------------------------

struct Loaded {
    threads: usize,
    session: ort::session::Session,
}

fn session_slot() -> &'static Mutex<Option<Loaded>> {
    static SLOT: OnceLock<Mutex<Option<Loaded>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn ort_err(e: impl std::fmt::Display) -> Error {
    Error::Engine(format!("stem model: {e}"))
}

/// One model call: a stereo segment of exactly [`SEGMENT`] samples per channel in,
/// the four stereo sources of the same length out (`[source][channel][sample]`).
fn separate_segment(model: &Path, threads: usize, left: &[f32], right: &[f32]) -> Result<Vec<[Vec<f32>; 2]>> {
    use ort::value::Tensor;
    let (zl, frames) = spec(left);
    let (zr, _) = spec(right);
    debug_assert_eq!(frames, FRAMES);
    let plane = BINS * frames;
    let mut mag = vec![0.0_f32; 4 * plane];
    for (c, z) in [&zl, &zr].into_iter().enumerate() {
        for (i, v) in z.iter().enumerate() {
            mag[(2 * c) * plane + i] = v.re;
            mag[(2 * c + 1) * plane + i] = v.im;
        }
    }
    let mut mix = Vec::with_capacity(2 * SEGMENT);
    mix.extend_from_slice(left);
    mix.extend_from_slice(right);

    let mut slot = session_slot().lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_none_or(|l| l.threads != threads) {
        let session = ort::session::Session::builder()
            .map_err(ort_err)?
            .with_intra_threads(threads)
            .map_err(ort_err)?
            .with_intra_op_spinning(false)
            .map_err(ort_err)?
            .commit_from_file(model)
            .map_err(ort_err)?;
        *slot = Some(Loaded { threads, session });
    }
    let session = &mut slot.as_mut().expect("session loaded above").session;
    let mix = Tensor::from_array((vec![1i64, 2, SEGMENT as i64], mix)).map_err(ort_err)?;
    let mag = Tensor::from_array((vec![1i64, 4, BINS as i64, frames as i64], mag)).map_err(ort_err)?;
    let outputs = session.run(ort::inputs!["mix" => mix, "mag" => mag]).map_err(ort_err)?;
    let (_, spec_out) = outputs["spec_out"].try_extract_tensor::<f32>().map_err(ort_err)?;
    let (_, wave_out) = outputs["wave_out"].try_extract_tensor::<f32>().map_err(ort_err)?;
    if spec_out.len() != 4 * 4 * plane || wave_out.len() != 4 * 2 * SEGMENT {
        return Err(ort_err("unexpected output shape — is this the htdemucs export?"));
    }
    let mut stems = Vec::with_capacity(4);
    for s in 0..4 {
        let mut chans: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
        for (c, chan) in chans.iter_mut().enumerate() {
            let re = &spec_out[(s * 4 + 2 * c) * plane..][..plane];
            let im = &spec_out[(s * 4 + 2 * c + 1) * plane..][..plane];
            let z: Vec<Complex<f32>> = re.iter().zip(im).map(|(r, i)| Complex::new(*r, *i)).collect();
            let wave = &wave_out[(s * 2 + c) * SEGMENT..][..SEGMENT];
            *chan = ispec(&z, frames, SEGMENT).iter().zip(wave).map(|(a, b)| a + b).collect();
        }
        stems.push(chans);
    }
    Ok(stems)
}

/// Demucs's blend weight: a triangle over the segment, peaking at its middle.
fn segment_weights() -> Vec<f32> {
    let half = SEGMENT / 2;
    let w: Vec<f32> = (1..=half).chain((1..=SEGMENT - half).rev()).map(|v| v as f32).collect();
    let max = w.iter().copied().fold(0.0, f32::max);
    w.into_iter().map(|v| v / max).collect()
}

/// Where each segment starts, and the window of the input it reads: `apply_model`'s
/// `TensorChunk.padded` — a segment shorter than [`SEGMENT`] (the last one) is centred in
/// a full-length window, its neighbours' audio filling the rest where there is any.
pub(crate) fn segment_plan(total: usize) -> Vec<(usize, usize, isize)> {
    let stride = ((1.0 - OVERLAP) * SEGMENT as f64) as usize;
    (0..total)
        .step_by(stride.max(1))
        .map(|offset| {
            let len = SEGMENT.min(total - offset);
            let delta = SEGMENT - len;
            (offset, len, offset as isize - (delta / 2) as isize)
        })
        .collect()
}

/// Separate a stereo buffer at [`STEM_SAMPLE_RATE`] into the four stems, reporting the
/// fraction done and giving up when `cancel` says so.
pub(crate) fn separate_buffer(
    model: &Path,
    threads: usize,
    mix: &AudioBuffer,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<AudioBuffer>> {
    let frames = mix.frames();
    let channel = |c: usize| -> Vec<f32> { mix.samples.iter().skip(c).step_by(2).copied().collect() };
    let (mut l, mut r) = (channel(0), channel(1));
    // `demucs.separate`: normalize by the mono reference's mean and (unbiased) std.
    let mono: Vec<f64> = l.iter().zip(&r).map(|(a, b)| (*a as f64 + *b as f64) / 2.0).collect();
    let mean = mono.iter().sum::<f64>() / frames.max(1) as f64;
    let var = mono.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (frames.max(2) - 1) as f64;
    let std = var.sqrt().max(1e-8);
    for v in l.iter_mut().chain(r.iter_mut()) {
        *v = ((*v as f64 - mean) / std) as f32;
    }
    let weights = segment_weights();
    let mut out = vec![[vec![0.0_f32; frames], vec![0.0_f32; frames]]; 4];
    let mut sum = vec![0.0_f32; frames];
    let plan = segment_plan(frames);
    for (k, &(offset, len, start)) in plan.iter().enumerate() {
        if cancel() {
            return Err(Error::Cancelled);
        }
        let window = |x: &[f32]| -> Vec<f32> {
            (0..SEGMENT as isize)
                .map(|i| {
                    let j = start + i;
                    if j >= 0 && (j as usize) < frames {
                        x[j as usize]
                    } else {
                        0.0
                    }
                })
                .collect()
        };
        let stems = separate_segment(model, threads, &window(&l), &window(&r))?;
        // `center_trim`: the segment's own samples out of the padded window.
        let trim = (SEGMENT - len) / 2;
        for (s, chans) in stems.iter().enumerate() {
            for c in 0..2 {
                for i in 0..len {
                    out[s][c][offset + i] += weights[i] * chans[c][trim + i];
                }
            }
        }
        for i in 0..len {
            sum[offset + i] += weights[i];
        }
        progress((k + 1) as f64 / plan.len() as f64);
    }
    Ok(out
        .into_iter()
        .map(|[a, b]| {
            let samples = a
                .iter()
                .zip(&b)
                .zip(&sum)
                .flat_map(|((x, y), w)| [(*x / w) as f64 * std + mean, (*y / w) as f64 * std + mean])
                .map(|v| v as f32)
                .collect();
            AudioBuffer::new(STEM_SAMPLE_RATE, 2, samples)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> [Vec<f32>; 2] {
        let n = 5000;
        let l = (0..n)
            .map(|t| {
                let t = t as f64;
                ((2.0 * std::f64::consts::PI * 440.0 * t / 44_100.0).sin() * 0.5 + 0.1 * (t * 0.37).sin()) as f32
            })
            .collect();
        let r = (0..n)
            .map(|t| ((2.0 * std::f64::consts::PI * 1000.0 * t as f64 / 44_100.0).cos() * 0.25) as f32)
            .collect();
        [l, r]
    }

    /// Values printed by PyTorch for `HTDemucs._spec` of [`fixture`]: `(channel, bin, frame, re, im)`.
    #[allow(clippy::excessive_precision)]
    const SPEC_REF: [(usize, usize, usize, f32, f32); 6] = [
        (0, 0, 0, 2.198004723e-01, 0.0),
        (0, 41, 0, -1.815202236e+00, 3.404658556e+00),
        (0, 41, 2, 2.060684204e+00, -7.636151314e+00),
        (1, 93, 4, 2.700553656e+00, 1.271636844e+00),
        (0, 1000, 3, 1.894116031e-05, 5.067623988e-06),
        (1, 2047, 1, -2.321485226e-10, 7.682757541e-09),
    ];

    /// `HTDemucs._ispec(_spec(x), 5000)` of [`fixture`]: `(channel, sample, value)`.
    #[allow(clippy::excessive_precision)]
    const ISPEC_REF: [(usize, usize, f32); 5] = [
        (0, 0, 4.196711870e-06),
        (0, 1234, 3.751296401e-01),
        (1, 2500, -9.299097955e-02),
        (0, 4999, -1.645979881e-01),
        (1, 4096, 1.794418991e-01),
    ];

    #[test]
    fn the_spectrogram_matches_pytorch() {
        let x = fixture();
        let specs = [spec(&x[0]), spec(&x[1])];
        assert_eq!(specs[0].1, 5);
        for (c, f, t, re, im) in SPEC_REF {
            let v = specs[c].0[f * 5 + t];
            let tol = 1e-4 * (re.abs() + im.abs()).max(1e-3);
            assert!(
                (v.re - re).abs() < tol && (v.im - im).abs() < tol,
                "({c},{f},{t}): {v} vs {re}+{im}i"
            );
        }
    }

    #[test]
    fn the_inverse_matches_pytorch() {
        let x = fixture();
        for (c, i, want) in ISPEC_REF {
            let (z, frames) = spec(&x[c]);
            let y = ispec(&z, frames, 5000);
            assert!((y[i] - want).abs() < 1e-5, "({c},{i}): {} vs {want}", y[i]);
        }
    }

    #[test]
    fn a_full_segment_round_trips_away_from_its_edges() {
        let x: Vec<f32> = (0..SEGMENT).map(|i| ((i as f32) * 0.013).sin() * 0.3).collect();
        let (z, frames) = spec(&x);
        assert_eq!(frames, FRAMES);
        let y = ispec(&z, frames, SEGMENT);
        let worst = (HOP * 4..SEGMENT - HOP * 4)
            .map(|i| (y[i] - x[i]).abs())
            .fold(0.0_f32, f32::max);
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn segments_overlap_by_a_quarter_and_the_last_is_centred() {
        let plan = segment_plan(SEGMENT * 2);
        let stride = 257_985;
        assert_eq!(plan.iter().map(|p| p.0).collect::<Vec<_>>(), vec![0, stride, 2 * stride]);
        assert_eq!(plan[0], (0, SEGMENT, 0));
        let (offset, len, start) = plan[2];
        assert_eq!(len, SEGMENT * 2 - offset);
        assert_eq!(start, offset as isize - ((SEGMENT - len) / 2) as isize);
        let w = segment_weights();
        assert_eq!(w.len(), SEGMENT);
        assert_eq!(w[SEGMENT / 2 - 1], 1.0);
        assert!(w[0] > 0.0 && w[0] < 1e-5);
    }

    #[test]
    fn reflect_padding_skips_the_edge_sample() {
        assert_eq!(
            reflect_pad(&[1.0, 2.0, 3.0, 4.0], 2, 2),
            vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0]
        );
    }

    /// End-to-end against PyTorch: `KERF_DEMUCS_PARITY_DIR` holds `model.onnx` (the
    /// export), `mix.wav` and `ref_<stem>.wav` written by Demucs's own `apply_model`
    /// (`shifts=0`, `overlap=0.25`) — see `.claude/docs/engine-media.md`. Downloads the
    /// ONNX Runtime on first use.
    #[test]
    #[ignore = "needs the exported model, a PyTorch reference and the ONNX Runtime"]
    fn separation_matches_demucs_and_the_stems_sum_to_the_mix() {
        let Some(dir) = std::env::var_os("KERF_DEMUCS_PARITY_DIR").map(PathBuf::from) else {
            panic!("set KERF_DEMUCS_PARITY_DIR");
        };
        let runtime = ensure_runtime(&mut |_| {}, &|| false).unwrap();
        init_runtime(&runtime).unwrap();
        let mix = decode_audio(&dir.join("mix.wav"), STEM_SAMPLE_RATE, 2).unwrap();
        assert!(mix.duration() > 7.8 * 2.0, "the fixture should span several segments");
        let stems = separate_buffer(&dir.join("model.onnx"), cpu::budget_threads(), &mix, &mut |_| {}, &|| false).unwrap();
        let rms = |a: &[f32], b: &[f32]| -> f64 {
            (a.iter().zip(b).map(|(x, y)| ((x - y) as f64).powi(2)).sum::<f64>() / a.len() as f64).sqrt()
        };
        for (name, stem) in STEM_NAMES.iter().zip(&stems) {
            let want = decode_audio(&dir.join(format!("ref_{name}.wav")), STEM_SAMPLE_RATE, 2).unwrap();
            assert_eq!(stem.samples.len(), want.samples.len(), "{name}");
            let err = rms(&stem.samples, &want.samples);
            assert!(err < 1e-3, "{name}: rms {err} from PyTorch");
        }
        let sum: Vec<f32> = (0..mix.samples.len())
            .map(|i| stems.iter().map(|s| s.samples[i]).sum())
            .collect();
        let rec = rms(&sum, &mix.samples);
        // Demucs is not constrained to sum to its input; ~0.002 on produced music, ~0.01
        // on this synthetic mix, the same as PyTorch's own on it.
        assert!(rec < 0.02, "reconstruction rms {rec}");
    }
}
