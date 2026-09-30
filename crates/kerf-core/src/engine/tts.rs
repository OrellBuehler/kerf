//! Voiceover: text-to-speech with Kokoro-82M, run in-process.
//!
//! Kokoro is an Apache-2.0, 82M-parameter TTS model distributed as ONNX, so it
//! runs on ONNX Runtime inside Kerf rather than behind a server someone has to
//! install. Nothing of it ships with the app: the runtime library (~30 MB, from
//! Microsoft's own release archives), the model (~90 MB) and each voice (~0.5 MB)
//! are downloaded the first time a voiceover is generated, through the same
//! resumable, cancellable fetch the speech models use. `ort` is built with
//! `load-dynamic` for exactly that reason — a statically linked runtime would
//! add its size to every install (and pyke's prebuilt one needs a newer glibc
//! than the Ubuntu 22.04 the Linux bundles target).
//!
//! Text becomes phonemes through `misaki-rs`, the Rust port of the G2P Kokoro
//! was trained against, built **without** its espeak-ng fallback: espeak-ng is
//! GPL-3, which Kerf cannot ship. Unknown words are spelled out instead, and the
//! lexicons are English only — so only the American and British voices are
//! offered. misaki-rs writes espeak-flavoured IPA (diphthongs as two letters
//! joined by U+200D, length marks), which [`to_kokoro_phonemes`] maps onto the
//! single-symbol set Kokoro's vocabulary actually has.
//!
//! Each sentence is synthesized on its own. That keeps every call inside the
//! model's 510-token window, and it is what makes subtitles exact: a sentence's
//! start and end in the finished file are sample counts, so the transcript this
//! produces needs none of the guessing a speech model does.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

use super::download::{fetch, Download, DownloadProgress};
use crate::error::{Error, Result};
use crate::model::TranscriptSegment;

/// Kokoro renders 24 kHz mono.
pub const SAMPLE_RATE: u32 = 24_000;

/// Phoneme tokens per call, not counting the two pad tokens: the model's
/// context is 512.
pub const MAX_TOKENS: usize = 510;

const STYLE_DIM: usize = 256;

/// A voice pack is one 256-float style vector per possible input length.
const VOICE_BYTES: usize = MAX_TOKENS * STYLE_DIM * 4;

/// Silence between sentences, and between paragraphs (a blank line in the
/// script) — Kokoro trims its own output tightly, so without these a script
/// reads as one breathless run-on.
pub const SENTENCE_GAP: f64 = 0.25;
pub const PARAGRAPH_GAP: f64 = 0.6;

pub const MIN_SPEED: f64 = 0.5;
pub const MAX_SPEED: f64 = 2.0;

pub const DEFAULT_VOICE: &str = "af_heart";

/// Identifies the model a voiceover was rendered with, so a cached file is
/// never reused across a model change.
const MODEL_ID: &str = "kokoro-82m-v1.0-q8";
const MODEL_FILE: &str = "model_quantized.onnx";
const MB: u64 = 1024 * 1024;
const MODEL_APPROX_BYTES: u64 = 88 * MB;

/// One Kokoro voice.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct VoiceInfo {
    /// The id the model knows it by, e.g. `af_heart`.
    pub id: &'static str,
    pub name: &'static str,
    /// `us` or `gb` — which lexicon its script is read with.
    pub accent: &'static str,
    /// `female` or `male`.
    pub gender: &'static str,
    /// Whether its voice pack is already on disk.
    pub downloaded: bool,
}

/// `(id, display name)` for every English Kokoro v1.0 voice. The id's first
/// letter is the accent (`a` American, `b` British), the second the gender.
const VOICES: &[(&str, &str)] = &[
    ("af_heart", "Heart"),
    ("af_bella", "Bella"),
    ("af_nicole", "Nicole"),
    ("af_sarah", "Sarah"),
    ("af_sky", "Sky"),
    ("af_nova", "Nova"),
    ("af_alloy", "Alloy"),
    ("af_aoede", "Aoede"),
    ("af_jessica", "Jessica"),
    ("af_kore", "Kore"),
    ("af_river", "River"),
    ("am_michael", "Michael"),
    ("am_fenrir", "Fenrir"),
    ("am_puck", "Puck"),
    ("am_adam", "Adam"),
    ("am_echo", "Echo"),
    ("am_eric", "Eric"),
    ("am_liam", "Liam"),
    ("am_onyx", "Onyx"),
    ("bf_emma", "Emma"),
    ("bf_isabella", "Isabella"),
    ("bf_alice", "Alice"),
    ("bf_lily", "Lily"),
    ("bm_george", "George"),
    ("bm_fable", "Fable"),
    ("bm_lewis", "Lewis"),
    ("bm_daniel", "Daniel"),
];

fn voice_entry(id: &str) -> Option<&'static (&'static str, &'static str)> {
    VOICES.iter().find(|(v, _)| *v == id)
}

fn is_british(voice: &str) -> bool {
    voice.starts_with('b')
}

/// Every voice Kerf offers, with whether each is downloaded yet.
pub fn voices() -> Vec<VoiceInfo> {
    VOICES
        .iter()
        .map(|&(id, name)| VoiceInfo {
            id,
            name,
            accent: if is_british(id) { "gb" } else { "us" },
            gender: if id.as_bytes().get(1) == Some(&b'm') { "male" } else { "female" },
            downloaded: voice_path(id).is_some_and(|p| p.is_file()),
        })
        .collect()
}

/// Reject a voice Kerf does not offer, naming the ones it does.
pub fn validate_voice(voice: &str) -> Result<()> {
    if voice_entry(voice).is_some() {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!(
            "unknown voice '{voice}'; expected one of: {}",
            VOICES.iter().map(|(id, _)| *id).collect::<Vec<_>>().join(", ")
        )))
    }
}

// ---- where things live -------------------------------------------------------

fn model_dir() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("kerf").join("models").join("kokoro"))
}

fn model_path() -> Option<PathBuf> {
    Some(model_dir()?.join(MODEL_FILE))
}

fn voice_path(voice: &str) -> Option<PathBuf> {
    Some(model_dir()?.join("voices").join(format!("{voice}.bin")))
}

/// The Kokoro ONNX repository on Hugging Face. `KERF_KOKORO_MODEL_URL` points
/// it at a mirror (a directory URL laid out the same way, no trailing slash).
fn model_base_url() -> String {
    std::env::var("KERF_KOKORO_MODEL_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main".to_string())
}

/// Where generated voiceovers are written: the *data* directory rather than
/// the cache, because unlike a proxy this file is the only copy of the audio.
pub fn output_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("kerf").join("voiceovers"))
}

/// The file a voiceover of `text` in `voice` at `speed` is written to. Named by
/// a hash of everything that shapes the audio, so generating the same script
/// twice reuses the file — and lands on the same asset.
pub fn output_path(text: &str, voice: &str, speed: f64) -> Option<PathBuf> {
    let mut hash = Sha256::new();
    for part in [MODEL_ID, voice, &format!("{speed:.3}"), text.trim()] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    Some(output_dir()?.join(format!("voiceover-{name}.wav")))
}

// ---- the ONNX Runtime library ------------------------------------------------

/// A Microsoft ONNX Runtime release archive and the one file Kerf needs out of
/// it, pinned by digest so the library that gets loaded into the process is
/// exactly the one that was published.
struct RuntimeSpec {
    version: &'static str,
    archive: &'static str,
    sha256: &'static str,
    /// Path of the shared library inside the archive, below its top directory.
    member: &'static str,
    /// What the extracted library is saved as.
    file: &'static str,
}

/// Every ONNX Runtime from 1.17 on serves the API `ort` is built against. The
/// newest is used everywhere it is published; Intel Macs stay on 1.20, the
/// last release Microsoft built for them.
fn runtime_spec() -> Option<RuntimeSpec> {
    let spec = |version, archive, sha256, member, file| {
        Some(RuntimeSpec {
            version,
            archive,
            sha256,
            member,
            file,
        })
    };
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => spec(
            "1.30.0",
            "onnxruntime-linux-x64-1.30.0.tgz",
            "a5ed5a3cac51fbb2e90da632ae43d19212faaa20e76484e62bcb7c23ddb3b3fd",
            "lib/libonnxruntime.so.1.30.0",
            "libonnxruntime.so",
        ),
        ("linux", "aarch64") => spec(
            "1.30.0",
            "onnxruntime-linux-aarch64-1.30.0.tgz",
            "e16a27a8ed330bbc698df7330b0cf56e722f354e3bcc92118682c74ef3c3e3da",
            "lib/libonnxruntime.so.1.30.0",
            "libonnxruntime.so",
        ),
        ("macos", "aarch64") => spec(
            "1.30.0",
            "onnxruntime-osx-arm64-1.30.0.tgz",
            "6ebb5062a934537c352937821f9fe9718e7de1a2db1122a93dd363ffd53a7012",
            "lib/libonnxruntime.1.30.0.dylib",
            "libonnxruntime.dylib",
        ),
        ("macos", "x86_64") => spec(
            "1.20.0",
            "onnxruntime-osx-x86_64-1.20.0.tgz",
            "d28e603b47b74050f2c30a7069bf3fb371cfba7205d7771f22cabc7b02953757",
            "lib/libonnxruntime.1.20.0.dylib",
            "libonnxruntime.dylib",
        ),
        ("windows", "x86_64") => spec(
            "1.30.0",
            "onnxruntime-win-x64-1.30.0.zip",
            "c6ba983baf5681af108599675d2a89c2d145512d02de28aed0bff177cd0ba949",
            "lib/onnxruntime.dll",
            "onnxruntime.dll",
        ),
        ("windows", "aarch64") => spec(
            "1.30.0",
            "onnxruntime-win-arm64-1.30.0.zip",
            "e53db8a50b23ae35be901cc93428baf997dc8d420333b097b2eae53d3ea9f2d3",
            "lib/onnxruntime.dll",
            "onnxruntime.dll",
        ),
        _ => None,
    }
}

/// A runtime library the user pointed Kerf at (`ORT_DYLIB_PATH`, the variable
/// `ort` itself honours), used instead of downloading one.
fn runtime_override() -> Option<PathBuf> {
    std::env::var_os("ORT_DYLIB_PATH").filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn runtime_path(spec: &RuntimeSpec) -> Option<PathBuf> {
    Some(
        dirs::cache_dir()?
            .join("kerf")
            .join("runtime")
            .join(format!("onnxruntime-{}", spec.version))
            .join(spec.file),
    )
}

/// The runtime library if it is already on disk.
fn ready_runtime() -> Option<PathBuf> {
    if let Some(path) = runtime_override() {
        return path.is_file().then_some(path);
    }
    runtime_spec().and_then(|s| runtime_path(&s)).filter(|p| p.is_file())
}

fn ensure_runtime(progress: &mut dyn FnMut(DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    if let Some(path) = runtime_override() {
        return if path.is_file() {
            Ok(path)
        } else {
            Err(Error::Engine(format!("ORT_DYLIB_PATH points at {}, which does not exist", path.display())))
        };
    }
    let spec = runtime_spec().ok_or_else(unsupported_platform)?;
    let lib = runtime_path(&spec).ok_or_else(|| Error::Engine("no cache directory available for the voice runtime".into()))?;
    if lib.is_file() {
        return Ok(lib);
    }
    let dir = lib.parent().expect("runtime path has a parent");
    let archive = dir.with_file_name(spec.archive);
    let url = format!(
        "https://github.com/microsoft/onnxruntime/releases/download/v{}/{}",
        spec.version, spec.archive
    );
    let sha256 = spec.sha256;
    let verify = move |path: &Path| verify_sha256(path, sha256);
    fetch(
        &Download {
            url: &url,
            dst: &archive,
            what: "voice runtime",
            mirror_env: "ORT_DYLIB_PATH",
            verify: &verify,
        },
        progress,
        cancel,
    )?;
    std::fs::create_dir_all(dir).map_err(|e| Error::Engine(format!("could not create voice runtime dir: {e}")))?;
    let tmp = lib.with_extension(format!("{}.part", std::process::id()));
    extract_member(&archive, spec.member, &tmp).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    std::fs::rename(&tmp, &lib).map_err(|e| Error::Engine(format!("could not install voice runtime: {e}")))?;
    // The archive also carries headers, debug symbols and licences Kerf has no
    // use for; only the library is kept.
    let _ = std::fs::remove_file(&archive);
    Ok(lib)
}

fn unsupported_platform() -> Error {
    Error::Engine(format!(
        "voiceover is not available on {}-{}: ONNX Runtime publishes no build for it (set ORT_DYLIB_PATH to one you have)",
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let mut file = std::fs::File::open(path).map_err(|e| Error::Engine(format!("could not read download: {e}")))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| Error::Engine(format!("could not read download: {e}")))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if got == expected {
        Ok(())
    } else {
        Err(Error::Engine(format!(
            "voice runtime download does not match its published checksum (got {got})"
        )))
    }
}

/// Whether archive entry `name` is `member` below the archive's single top
/// directory (`onnxruntime-linux-x64-1.30.0/lib/…`, possibly `./`-prefixed).
fn is_member(name: &str, member: &str) -> bool {
    let name = name.trim_start_matches("./").replace('\\', "/");
    name.split_once('/').is_some_and(|(_, rest)| rest == member)
}

/// Copy one file out of a `.tgz` or `.zip` archive to `dst`.
fn extract_member(archive: &Path, member: &str, dst: &Path) -> Result<()> {
    let err = |e: &dyn std::fmt::Display| Error::Engine(format!("could not unpack the voice runtime: {e}"));
    let file = std::fs::File::open(archive).map_err(|e| err(&e))?;
    let mut out = std::fs::File::create(dst).map_err(|e| err(&e))?;
    if archive.extension().is_some_and(|e| e == "zip") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| err(&e))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| err(&e))?;
            if entry.is_file() && is_member(entry.name(), member) {
                std::io::copy(&mut entry, &mut out).map_err(|e| err(&e))?;
                return Ok(());
            }
        }
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        for entry in tar.entries().map_err(|e| err(&e))? {
            let mut entry = entry.map_err(|e| err(&e))?;
            let path = entry.path().map_err(|e| err(&e))?.to_string_lossy().into_owned();
            if entry.header().entry_type().is_file() && is_member(&path, member) {
                std::io::copy(&mut entry, &mut out).map_err(|e| err(&e))?;
                return Ok(());
            }
        }
    }
    Err(Error::Engine(format!("the voice runtime archive has no {member}")))
}

// ---- the model and voices ----------------------------------------------------

fn ensure_model(progress: &mut dyn FnMut(DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    let dst = model_path().ok_or_else(|| Error::Engine("no cache directory available for the voice model".into()))?;
    let url = format!("{}/onnx/{MODEL_FILE}", model_base_url());
    fetch(
        &Download {
            url: &url,
            dst: &dst,
            what: "voice model",
            mirror_env: "KERF_KOKORO_MODEL_URL",
            verify: &verify_onnx,
        },
        progress,
        cancel,
    )
}

/// An ONNX file is a protobuf `ModelProto` whose first field is `ir_version`
/// (field 1, varint: tag byte `0x08`) — enough to tell a model from an error
/// page saved under its name.
fn verify_onnx(path: &Path) -> Result<()> {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut head = [0u8; 1];
    let ok = len > MB
        && std::fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut head))
            .is_ok()
        && head[0] == 0x08;
    if ok {
        Ok(())
    } else {
        Err(Error::Engine(
            "downloaded voice model is not an ONNX file (the download URL may be wrong or behind a login)".into(),
        ))
    }
}

fn ensure_voice(voice: &str, progress: &mut dyn FnMut(DownloadProgress), cancel: &dyn Fn() -> bool) -> Result<PathBuf> {
    validate_voice(voice)?;
    let dst = voice_path(voice).ok_or_else(|| Error::Engine("no cache directory available for voices".into()))?;
    let url = format!("{}/voices/{voice}.bin", model_base_url());
    let verify = |path: &Path| {
        if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) == VOICE_BYTES as u64 {
            Ok(())
        } else {
            Err(Error::Engine(format!("downloaded voice '{voice}' is not a Kokoro voice pack")))
        }
    };
    fetch(
        &Download {
            url: &url,
            dst: &dst,
            what: "voice",
            mirror_env: "KERF_KOKORO_MODEL_URL",
            verify: &verify,
        },
        progress,
        cancel,
    )
}

/// The style vector for an input of `tokens` phonemes: a voice pack holds one
/// per length, since Kokoro conditions on how long the utterance is.
pub fn voice_style(pack: &[u8], tokens: usize) -> Result<Vec<f32>> {
    if pack.len() != VOICE_BYTES {
        return Err(Error::Engine(format!(
            "voice pack is {} bytes, expected {VOICE_BYTES}",
            pack.len()
        )));
    }
    let row = tokens.min(MAX_TOKENS - 1) * STYLE_DIM * 4;
    Ok(pack[row..row + STYLE_DIM * 4]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

// ---- status ------------------------------------------------------------------

/// Whether voiceovers can be generated here, and what the first one would have
/// to download. The Voiceover dialog and an agent both read it.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct VoiceoverStatus {
    /// Whether this platform can run the voice model at all.
    pub available: bool,
    /// Whether the runtime library and the model are both on disk. When false,
    /// the next voiceover starts by downloading about `approx_download_bytes`
    /// (plus ~0.5 MB for a voice not used before).
    pub ready: bool,
    pub approx_download_bytes: u64,
    pub default_voice: &'static str,
    pub voices: Vec<VoiceInfo>,
    /// Why voiceover is unavailable, when it is.
    pub reason: Option<String>,
}

pub fn status() -> VoiceoverStatus {
    let available = runtime_override().is_some() || runtime_spec().is_some();
    let runtime_ready = ready_runtime().is_some();
    let model_ready = model_path().is_some_and(|p| p.is_file());
    let runtime_bytes = if runtime_ready {
        0
    } else {
        runtime_spec()
            .map(|s| if s.archive.ends_with(".zip") { 80 * MB } else { 12 * MB })
            .unwrap_or(0)
    };
    VoiceoverStatus {
        available,
        ready: runtime_ready && model_ready,
        approx_download_bytes: runtime_bytes + if model_ready { 0 } else { MODEL_APPROX_BYTES },
        default_voice: DEFAULT_VOICE,
        voices: voices(),
        reason: (!available).then(|| unsupported_platform().to_string()),
    }
}

// ---- text → phonemes → tokens ------------------------------------------------

/// A script split for synthesis: its sentences in order, each marked with
/// whether a paragraph break follows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Sentence {
    pub text: String,
    pub paragraph_end: bool,
}

/// Abbreviations whose period does not end a sentence.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "approx", "no", "fig", "inc", "ltd",
    "co", "mt",
];

/// Split a script into sentences at `.` `!` `?` `…` (with any closing quotes or
/// brackets) followed by whitespace, never at an abbreviation or a decimal
/// point, and at every blank line.
pub fn split_sentences(text: &str) -> Vec<Sentence> {
    let mut out = Vec::new();
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .flat_map(|p| p.split("\r\n\r\n"))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    for paragraph in paragraphs {
        let chars: Vec<char> = paragraph.chars().collect();
        let mut start = 0;
        let mut i = 0;
        while i < chars.len() {
            if matches!(chars[i], '.' | '!' | '?' | '…') {
                let mut end = i + 1;
                while end < chars.len() && matches!(chars[end], '.' | '!' | '?' | '…' | '"' | '\'' | '”' | '’' | ')' | ']') {
                    end += 1;
                }
                let at_break = end >= chars.len() || chars[end].is_whitespace();
                if at_break && !is_abbreviation(&chars[start..i], chars[i]) {
                    push_sentence(&mut out, &chars[start..end]);
                    start = end;
                }
                i = end;
            } else {
                i += 1;
            }
        }
        push_sentence(&mut out, &chars[start..]);
        if let Some(last) = out.last_mut() {
            last.paragraph_end = true;
        }
    }
    out
}

fn push_sentence(out: &mut Vec<Sentence>, chars: &[char]) {
    let text: String = chars.iter().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    if !text.is_empty() {
        out.push(Sentence {
            text,
            paragraph_end: false,
        });
    }
}

/// Whether the word before a period is an abbreviation (or a single initial,
/// as in "J. R. R. Tolkien").
fn is_abbreviation(before: &[char], mark: char) -> bool {
    if mark != '.' {
        return false;
    }
    let word: String = before
        .iter()
        .rev()
        .take_while(|c| !c.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let word = word.trim_start_matches(['(', '"', '\'', '“', '‘']).to_lowercase();
    ABBREVIATIONS.contains(&word.as_str()) || (word.chars().count() == 1 && word.chars().all(char::is_alphabetic))
}

/// Map misaki-rs output onto Kokoro's phoneme vocabulary.
///
/// misaki-rs writes diphthongs and affricates as two letters joined by U+200D
/// and keeps espeak's length marks; Kokoro v1.0 was trained on misaki's own
/// compact set, where each of those is one symbol (`A` = eɪ, `I` = aɪ, `W` =
/// aʊ, `Y` = ɔɪ, `O` / `Q` = the American / British GOAT vowel, `ʤ`, `ʧ`) and
/// American English has no length marks and a rhotic NURSE vowel (`ɜɹ`).
pub fn to_kokoro_phonemes(ipa: &str, british: bool) -> String {
    let goat = if british { "Q" } else { "O" };
    let mut s = ipa
        .replace("d\u{200d}ʒ", "ʤ")
        .replace("t\u{200d}ʃ", "ʧ")
        .replace("e\u{200d}ɪ", "A")
        .replace("a\u{200d}ɪ", "I")
        .replace("a\u{200d}ʊ", "W")
        .replace("ɔ\u{200d}ɪ", "Y")
        .replace("o\u{200d}ʊ", goat)
        .replace("ə\u{200d}ʊ", goat)
        .replace('\u{200d}', "");
    if !british {
        let mut rhotic = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == 'ɜ' && chars.peek() == Some(&'ː') {
                chars.next();
                rhotic.push('ɜ');
                if chars.peek() != Some(&'ɹ') {
                    rhotic.push('ɹ');
                }
            } else if c != 'ː' {
                rhotic.push(c);
            }
        }
        s = rhotic;
    }
    // misaki-rs spaces punctuation off as its own token; Kokoro's training
    // text attaches it to the word before, and reads a stray " ." as a pause.
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !out.is_empty() && !word.chars().all(|c| ",.!?;:—…".contains(c)) {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Kokoro's phoneme vocabulary. Token 0 (`$`) pads both ends of every input.
const VOCAB: &[(char, i64)] = &[
    (';', 1),
    (':', 2),
    (',', 3),
    ('.', 4),
    ('!', 5),
    ('?', 6),
    ('—', 9),
    ('…', 10),
    ('"', 11),
    ('(', 12),
    (')', 13),
    ('“', 14),
    ('”', 15),
    (' ', 16),
    ('\u{303}', 17),
    ('ʣ', 18),
    ('ʥ', 19),
    ('ʦ', 20),
    ('ʨ', 21),
    ('ᵝ', 22),
    ('ꭧ', 23),
    ('A', 24),
    ('I', 25),
    ('O', 31),
    ('Q', 33),
    ('S', 35),
    ('T', 36),
    ('W', 39),
    ('Y', 41),
    ('ᵊ', 42),
    ('a', 43),
    ('b', 44),
    ('c', 45),
    ('d', 46),
    ('e', 47),
    ('f', 48),
    ('h', 50),
    ('i', 51),
    ('j', 52),
    ('k', 53),
    ('l', 54),
    ('m', 55),
    ('n', 56),
    ('o', 57),
    ('p', 58),
    ('q', 59),
    ('r', 60),
    ('s', 61),
    ('t', 62),
    ('u', 63),
    ('v', 64),
    ('w', 65),
    ('x', 66),
    ('y', 67),
    ('z', 68),
    ('ɑ', 69),
    ('ɐ', 70),
    ('ɒ', 71),
    ('æ', 72),
    ('β', 75),
    ('ɔ', 76),
    ('ɕ', 77),
    ('ç', 78),
    ('ɖ', 80),
    ('ð', 81),
    ('ʤ', 82),
    ('ə', 83),
    ('ɚ', 85),
    ('ɛ', 86),
    ('ɜ', 87),
    ('ɟ', 90),
    ('ɡ', 92),
    ('ɥ', 99),
    ('ɨ', 101),
    ('ɪ', 102),
    ('ʝ', 103),
    ('ɯ', 110),
    ('ɰ', 111),
    ('ŋ', 112),
    ('ɳ', 113),
    ('ɲ', 114),
    ('ɴ', 115),
    ('ø', 116),
    ('ɸ', 118),
    ('θ', 119),
    ('œ', 120),
    ('ɹ', 123),
    ('ɾ', 125),
    ('ɻ', 126),
    ('ʁ', 128),
    ('ɽ', 129),
    ('ʂ', 130),
    ('ʃ', 131),
    ('ʈ', 132),
    ('ʧ', 133),
    ('ʊ', 135),
    ('ʋ', 136),
    ('ʌ', 138),
    ('ɣ', 139),
    ('ɤ', 140),
    ('χ', 142),
    ('ʎ', 143),
    ('ʒ', 147),
    ('ʔ', 148),
    ('ˈ', 156),
    ('ˌ', 157),
    ('ː', 158),
    ('ʰ', 162),
    ('ʲ', 164),
    ('↓', 169),
    ('→', 171),
    ('↗', 172),
    ('↘', 173),
    ('ᵻ', 177),
];

/// Phonemes to token ids, dropping anything outside the vocabulary (misaki's
/// `❓` for an unpronounceable token, stray symbols) rather than failing.
pub fn tokenize(phonemes: &str) -> Vec<i64> {
    phonemes
        .chars()
        .filter_map(|c| VOCAB.iter().find(|(v, _)| *v == c).map(|(_, id)| *id))
        .collect()
}

/// Split a token sequence that overflows the model's window, preferring the
/// last clause break (`,` `;` `:` `—`) and then the last word break before the
/// limit, so a long sentence is read as its clauses rather than cut mid-word.
pub fn chunk_tokens(tokens: &[i64]) -> Vec<Vec<i64>> {
    const SPACE: i64 = 16;
    const CLAUSE: &[i64] = &[1, 2, 3, 9];
    let mut out = Vec::new();
    let mut rest = tokens;
    while rest.len() > MAX_TOKENS {
        let window = &rest[..MAX_TOKENS];
        let cut = window
            .iter()
            .rposition(|t| CLAUSE.contains(t))
            .map(|i| i + 1)
            .or_else(|| window.iter().rposition(|t| *t == SPACE))
            .filter(|&i| i > 0)
            .unwrap_or(MAX_TOKENS);
        out.push(rest[..cut].to_vec());
        rest = &rest[cut..];
        while rest.first() == Some(&SPACE) {
            rest = &rest[1..];
        }
    }
    if !rest.is_empty() {
        out.push(rest.to_vec());
    }
    out
}

// ---- assembling the file -----------------------------------------------------

/// Lay sentences end to end with the gaps between them, returning one
/// transcript segment per sentence in the finished file's own time.
pub fn build_transcript(sentences: &[Sentence], durations: &[f64]) -> Vec<TranscriptSegment> {
    let mut at = 0.0;
    let mut out = Vec::with_capacity(sentences.len());
    for (i, (sentence, &duration)) in sentences.iter().zip(durations).enumerate() {
        if i > 0 {
            at += if sentences[i - 1].paragraph_end { PARAGRAPH_GAP } else { SENTENCE_GAP };
        }
        out.push(TranscriptSegment {
            start: at,
            end: at + duration,
            text: sentence.text.clone(),
        });
        at += duration;
    }
    out
}

/// A 16-bit PCM mono WAV of `samples` (floats in -1..1, clipped).
pub fn wav_bytes(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16).to_le_bytes());
    }
    out
}

// ---- synthesis ---------------------------------------------------------------

/// A step of generating a voiceover, reported as it runs: `download_runtime`,
/// `download_model`, `download_voice`, `synthesize`.
pub type ProgressFn<'a> = &'a mut Report;

/// `(stage, fraction, detail)`.
pub type Report = dyn FnMut(&str, Option<f64>, Option<String>);

/// A finished voiceover.
#[derive(Debug, Clone)]
pub struct Synthesis {
    pub path: PathBuf,
    pub duration: f64,
    pub segments: Vec<TranscriptSegment>,
}

fn download_progress<'a>(stage: &'a str, progress: &'a mut Report) -> impl FnMut(DownloadProgress) + 'a {
    move |p: DownloadProgress| {
        let mb = |b: u64| format!("{:.0} MB", b as f64 / MB as f64);
        let detail = match p.total {
            Some(total) => format!("{} / {}", mb(p.downloaded), mb(total)),
            None => mb(p.downloaded),
        };
        progress(stage, p.fraction(), Some(detail));
    }
}

/// Download whatever the voice model still needs, so a later voiceover starts
/// synthesizing at once.
pub fn prepare(voice: &str, progress: ProgressFn, cancel: &dyn Fn() -> bool) -> Result<()> {
    validate_voice(voice)?;
    ensure_runtime(&mut download_progress("download_runtime", progress), cancel)?;
    ensure_model(&mut download_progress("download_model", progress), cancel)?;
    ensure_voice(voice, &mut download_progress("download_voice", progress), cancel)?;
    Ok(())
}

/// The sentence timings saved beside a voiceover, so regenerating the same
/// script reuses the file instead of synthesizing it again.
fn timings_path(wav: &Path) -> PathBuf {
    wav.with_extension("json")
}

fn cached(path: &Path) -> Option<Synthesis> {
    if !path.is_file() {
        return None;
    }
    let segments: Vec<TranscriptSegment> = serde_json::from_slice(&std::fs::read(timings_path(path)).ok()?).ok()?;
    let data = std::fs::metadata(path).ok()?.len().checked_sub(44)?;
    Some(Synthesis {
        path: path.to_path_buf(),
        duration: data as f64 / 2.0 / SAMPLE_RATE as f64,
        segments,
    })
}

/// Read `text` aloud in `voice` at `speed`, writing a WAV to [`output_path`].
///
/// Downloads the runtime, model and voice first if they are missing, then
/// synthesizes sentence by sentence — reporting each — as one heavy job
/// (`cpu::lease`), with ONNX Runtime's threads capped to the same budget ffmpeg
/// runs under. A cancel lands between sentences and writes nothing.
pub fn synthesize(text: &str, voice: &str, speed: f64, progress: ProgressFn, cancel: &dyn Fn() -> bool) -> Result<Synthesis> {
    validate_voice(voice)?;
    let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    let sentences = split_sentences(text);
    if sentences.is_empty() {
        return Err(Error::InvalidArgument("the voiceover script is empty".into()));
    }
    let path = output_path(text, voice, speed).ok_or_else(|| Error::Engine("no data directory for voiceovers".into()))?;
    if let Some(done) = cached(&path) {
        return Ok(done);
    }

    prepare(voice, progress, cancel)?;
    let runtime = ensure_runtime(&mut |_| {}, cancel)?;
    let model = ensure_model(&mut |_| {}, cancel)?;
    let pack = std::fs::read(ensure_voice(voice, &mut |_| {}, cancel)?)
        .map_err(|e| Error::Engine(format!("could not read voice '{voice}': {e}")))?;

    let lease = super::cpu::lease();
    progress("synthesize", Some(0.0), None);
    let g2p = g2p(is_british(voice));
    let mut samples: Vec<f32> = Vec::new();
    let mut durations = Vec::with_capacity(sentences.len());
    for (i, sentence) in sentences.iter().enumerate() {
        if cancel() {
            return Err(Error::Cancelled);
        }
        if i > 0 {
            let gap = if sentences[i - 1].paragraph_end { PARAGRAPH_GAP } else { SENTENCE_GAP };
            samples.extend(std::iter::repeat_n(0.0, (gap * SAMPLE_RATE as f64).round() as usize));
        }
        let (ipa, _) = g2p
            .g2p(&sentence.text)
            .map_err(|e| Error::Engine(format!("could not phonemize \"{}\": {e}", sentence.text)))?;
        let tokens = tokenize(&to_kokoro_phonemes(&ipa, is_british(voice)));
        let before = samples.len();
        for chunk in chunk_tokens(&tokens) {
            let style = voice_style(&pack, chunk.len())?;
            samples.extend(infer(&runtime, &model, lease.threads(), &chunk, style, speed as f32)?);
        }
        durations.push((samples.len() - before) as f64 / SAMPLE_RATE as f64);
        progress(
            "synthesize",
            Some((i + 1) as f64 / sentences.len() as f64),
            Some(format!("{} of {} sentences", i + 1, sentences.len())),
        );
    }
    drop(lease);

    let dir = path.parent().expect("voiceover path has a parent");
    std::fs::create_dir_all(dir).map_err(|e| Error::Engine(format!("could not create voiceover dir: {e}")))?;
    let segments = build_transcript(&sentences, &durations);
    // Timings first: a WAV without them is re-synthesized, one with them is
    // trusted, so the WAV's rename is what marks the voiceover finished.
    let tmp = path.with_extension(format!("{}.part", std::process::id()));
    std::fs::write(timings_path(&path), serde_json::to_vec(&segments)?)
        .and_then(|_| std::fs::write(&tmp, wav_bytes(&samples, SAMPLE_RATE)))
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| Error::Engine(format!("could not write voiceover: {e}")))?;
    Ok(Synthesis {
        path,
        duration: samples.len() as f64 / SAMPLE_RATE as f64,
        segments,
    })
}

/// misaki's lexicons take a moment to parse, so each accent's G2P is built once
/// per process.
fn g2p(british: bool) -> &'static misaki_rs::G2P {
    static US: OnceLock<misaki_rs::G2P> = OnceLock::new();
    static GB: OnceLock<misaki_rs::G2P> = OnceLock::new();
    if british {
        GB.get_or_init(|| misaki_rs::G2P::new(misaki_rs::Language::EnglishGB))
    } else {
        US.get_or_init(|| misaki_rs::G2P::new(misaki_rs::Language::EnglishUS))
    }
}

/// The loaded model, rebuilt only when the thread budget it was built with
/// changes.
struct Loaded {
    threads: usize,
    session: ort::session::Session,
}

fn session_slot() -> &'static Mutex<Option<Loaded>> {
    static SLOT: OnceLock<Mutex<Option<Loaded>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Load the runtime library into the process, once. A second call with a
/// different path is ignored — a process can hold one ONNX Runtime.
fn init_runtime(runtime: &Path) -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        ort::init_from(runtime)
            .map(|env| {
                env.with_name("kerf").commit();
            })
            .map_err(|e| e.to_string())
    })
    .clone()
    .map_err(|e| Error::Engine(format!("could not load the voice runtime: {e}")))
}

fn ort_err(e: impl std::fmt::Display) -> Error {
    Error::Engine(format!("voice model: {e}"))
}

fn infer(runtime: &Path, model: &Path, threads: usize, tokens: &[i64], style: Vec<f32>, speed: f32) -> Result<Vec<f32>> {
    use ort::value::Tensor;
    init_runtime(runtime)?;
    let mut slot = session_slot().lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_none_or(|l| l.threads != threads) {
        let session = ort::session::Session::builder()
            .map_err(ort_err)?
            .with_intra_threads(threads)
            .map_err(ort_err)?
            // A spinning pool burns the cores it was not given work for, which
            // is the opposite of what the CPU budget is for.
            .with_intra_op_spinning(false)
            .map_err(ort_err)?
            .commit_from_file(model)
            .map_err(ort_err)?;
        *slot = Some(Loaded { threads, session });
    }
    let session = &mut slot.as_mut().expect("session loaded above").session;

    let mut ids = Vec::with_capacity(tokens.len() + 2);
    ids.push(0);
    ids.extend_from_slice(tokens);
    ids.push(0);
    let input_ids = Tensor::from_array((vec![1i64, ids.len() as i64], ids)).map_err(ort_err)?;
    let style = Tensor::from_array((vec![1i64, STYLE_DIM as i64], style)).map_err(ort_err)?;
    let speed = Tensor::from_array((vec![1i64], vec![speed])).map_err(ort_err)?;
    let outputs = session
        .run(ort::inputs!["input_ids" => input_ids, "style" => style, "speed" => speed])
        .map_err(ort_err)?;
    let waveform = outputs.values().next().ok_or_else(|| ort_err("the model returned no audio"))?;
    let (_, audio) = waveform.try_extract_tensor::<f32>().map_err(ort_err)?;
    Ok(audio.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_split_at_terminators_but_not_abbreviations_or_decimals() {
        let s = split_sentences("Dr. Smith paid $3.50 today. Was it worth it?  \"Yes!\" she said… Then e.g. more.");
        let texts: Vec<&str> = s.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Dr. Smith paid $3.50 today.",
                "Was it worth it?",
                "\"Yes!\"",
                "she said…",
                "Then e.g. more."
            ]
        );
    }

    #[test]
    fn blank_lines_are_paragraph_breaks() {
        let s = split_sentences("One. Two.\n\nThree\nstill three.");
        assert_eq!(s.len(), 3);
        assert!(!s[0].paragraph_end);
        assert!(s[1].paragraph_end);
        assert_eq!(s[2].text, "Three still three.");
        assert!(s[2].paragraph_end);
        assert!(split_sentences("  \n\n ").is_empty());
    }

    #[test]
    fn initials_do_not_end_a_sentence() {
        let s = split_sentences("J. R. R. Tolkien wrote it. The end.");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn misaki_digraphs_become_kokoro_symbols() {
        assert_eq!(to_kokoro_phonemes("həlˈo\u{200d}ʊ wˈɜːld ! ", false), "həlˈO wˈɜɹld!");
        assert_eq!(to_kokoro_phonemes("həlˈo\u{200d}ʊ wˈɜːld ! ", true), "həlˈQ wˈɜːld!");
        assert_eq!(
            to_kokoro_phonemes("t\u{200d}ʃˈɜːt\u{200d}ʃ d\u{200d}ʒˈʌd\u{200d}ʒ", false),
            "ʧˈɜɹʧ ʤˈʌʤ"
        );
        assert_eq!(to_kokoro_phonemes("mˈe\u{200d}ɪbiː , bˈɔ\u{200d}ɪ", false), "mˈAbi, bˈY");
        assert_eq!(to_kokoro_phonemes("fˈa\u{200d}ɪv nˈa\u{200d}ʊ", false), "fˈIv nˈW");
        // Already rhotic: no second r.
        assert_eq!(to_kokoro_phonemes("stˈɜːɹ", false), "stˈɜɹ");
    }

    #[test]
    fn tokenize_maps_the_vocab_and_drops_the_rest() {
        assert_eq!(tokenize("hə❓l"), vec![50, 83, 54]);
        assert_eq!(tokenize("ˈO ,"), vec![156, 31, 16, 3]);
        assert_eq!(tokenize("\""), vec![11]);
    }

    #[test]
    fn long_inputs_break_at_clauses_then_words() {
        let mut tokens = vec![43; 300];
        tokens.push(3);
        tokens.push(16);
        tokens.extend(vec![44; 300]);
        let chunks = chunk_tokens(&tokens);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 301);
        assert_eq!(chunks[1], vec![44; 300]);

        let mut words = Vec::new();
        for _ in 0..200 {
            words.extend([43, 44, 16]);
        }
        let chunks = chunk_tokens(&words);
        assert!(chunks.iter().all(|c| c.len() <= MAX_TOKENS));
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>() + chunks.len() - 1, words.len());

        assert_eq!(chunk_tokens(&[]), Vec::<Vec<i64>>::new());
        assert_eq!(chunk_tokens(&vec![43; 1000]).len(), 2);
    }

    #[test]
    fn a_voice_style_is_the_row_for_its_length() {
        let mut pack = vec![0u8; VOICE_BYTES];
        let row = 7 * STYLE_DIM * 4;
        pack[row..row + 4].copy_from_slice(&1.5f32.to_le_bytes());
        let last = (MAX_TOKENS - 1) * STYLE_DIM * 4;
        pack[last..last + 4].copy_from_slice(&2.5f32.to_le_bytes());
        assert_eq!(voice_style(&pack, 7).unwrap()[0], 1.5);
        assert_eq!(voice_style(&pack, 7).unwrap().len(), STYLE_DIM);
        // An input at the window's limit uses the last row rather than reading past it.
        assert_eq!(voice_style(&pack, MAX_TOKENS).unwrap()[0], 2.5);
        assert!(voice_style(&pack[1..], 7).is_err());
    }

    #[test]
    fn transcript_lays_sentences_out_with_their_gaps() {
        let sentences = split_sentences("One. Two.\n\nThree.");
        let t = build_transcript(&sentences, &[1.0, 2.0, 0.5]);
        assert_eq!(t.len(), 3);
        assert_eq!((t[0].start, t[0].end), (0.0, 1.0));
        assert!((t[1].start - (1.0 + SENTENCE_GAP)).abs() < 1e-9);
        assert!((t[2].start - (t[1].end + PARAGRAPH_GAP)).abs() < 1e-9);
        assert_eq!(t[2].text, "Three.");
    }

    #[test]
    fn wav_header_describes_the_samples() {
        let wav = wav_bytes(&[0.0, 1.0, -1.0, 2.0], SAMPLE_RATE);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), SAMPLE_RATE);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8);
        let samples: Vec<i16> = wav[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert_eq!(samples, [0, i16::MAX, -i16::MAX, i16::MAX]);
    }

    #[test]
    fn output_path_depends_on_everything_that_shapes_the_audio() {
        let a = output_path("Hello.", "af_heart", 1.0).unwrap();
        assert_eq!(a, output_path("  Hello.  ", "af_heart", 1.0).unwrap());
        assert_ne!(a, output_path("Hello.", "af_bella", 1.0).unwrap());
        assert_ne!(a, output_path("Hello.", "af_heart", 1.1).unwrap());
        assert_ne!(a, output_path("Hello!", "af_heart", 1.0).unwrap());
    }

    #[test]
    fn archive_members_are_matched_below_the_top_directory() {
        assert!(is_member(
            "./onnxruntime-osx-x86_64-1.20.0/lib/libonnxruntime.1.20.0.dylib",
            "lib/libonnxruntime.1.20.0.dylib"
        ));
        assert!(is_member("onnxruntime-win-x64-1.30.0/lib/onnxruntime.dll", "lib/onnxruntime.dll"));
        assert!(!is_member(
            "onnxruntime-osx-x86_64-1.20.0/lib/libonnxruntime.1.20.0.dylib.dSYM/Contents/Resources/DWARF/libonnxruntime.1.20.0.dylib",
            "lib/libonnxruntime.1.20.0.dylib"
        ));
    }

    #[test]
    fn unknown_voices_are_rejected_and_voices_describe_themselves() {
        assert!(validate_voice("af_heart").is_ok());
        assert!(validate_voice("ef_dora").is_err());
        let v = voices();
        let george = v.iter().find(|v| v.id == "bm_george").unwrap();
        assert_eq!((george.accent, george.gender), ("gb", "male"));
        let heart = v.iter().find(|v| v.id == "af_heart").unwrap();
        assert_eq!((heart.accent, heart.gender), ("us", "female"));
    }

    /// Downloads ONNX Runtime, the model and a voice (~100 MB) and synthesizes
    /// for real. Run with `-- --ignored` when touching the voice pipeline.
    #[test]
    #[ignore]
    fn synthesizes_a_script_end_to_end() {
        let text = "Hello from Kerf. This is the second sentence.";
        let result = synthesize(text, DEFAULT_VOICE, 1.0, &mut |stage, f, d| eprintln!("{stage} {f:?} {d:?}"), &|| false)
            .expect("synthesis");
        assert_eq!(result.segments.len(), 2);
        assert!(result.duration > 1.5, "two sentences should take more than 1.5 s, got {}", result.duration);
        let bytes = std::fs::read(&result.path).unwrap();
        let peak = bytes[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 1000, "the voiceover is silent");
        assert!(result.segments[1].start > result.segments[0].end);
    }
}
