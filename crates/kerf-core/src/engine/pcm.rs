//! Decoded audio as interleaved `f32`, and a WAV writer for tests and debug renders.
//!
//! The decode is the `ffmpeg` binary (resampled and down/up-mixed by `-ar` / `-ac`), so
//! like the rest of the CLI engine it needs no dev libraries and no Rust decoder.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use super::cli::{bg_command, ffmpeg_bin, launch_err};
use super::cpu;
use super::peaks::pump_pcm;
use crate::error::{Error, Result};

/// A decode silent this long is killed: see `peaks.rs`.
const DECODE_STALL: Duration = Duration::from_secs(60);

/// Interleaved `f32` PCM: frame `i`, channel `c` is `samples[i * channels + c]`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl AudioBuffer {
    pub fn new(sample_rate: u32, channels: u16, samples: Vec<f32>) -> Self {
        Self {
            sample_rate,
            channels,
            samples,
        }
    }

    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / usize::from(self.channels)
        }
    }

    pub fn duration(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.sample_rate as f64
        }
    }

    /// The channels averaged into one.
    pub fn mono(&self) -> Vec<f32> {
        let ch = usize::from(self.channels.max(1));
        if ch == 1 {
            return self.samples.clone();
        }
        self.samples
            .chunks_exact(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect()
    }

    /// The largest absolute sample.
    pub fn peak(&self) -> f32 {
        self.samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
    }
}

/// The ffmpeg arguments that decode `path`'s first audio stream to interleaved f32le
/// at `sample_rate` in `channels` channels on stdout. Pure.
fn decode_args(path: &Path, sample_rate: u32, channels: u16) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["-hide_banner", "-loglevel", "error", "-i"].iter().map(Into::into).collect();
    args.push(path.as_os_str().to_owned());
    for a in [
        "-map",
        "0:a:0",
        "-ac",
        &channels.to_string(),
        "-ar",
        &sample_rate.to_string(),
        "-f",
        "f32le",
        "pipe:1",
    ] {
        args.push(a.into());
    }
    args
}

/// Decode the first audio stream of `path` (an audio file or a video's sound) at
/// `sample_rate` in `channels` channels. A whole-file decode into memory, so it takes
/// the heavy-job slot; a decode that stalls is killed.
pub fn decode_audio(path: &Path, sample_rate: u32, channels: u16) -> Result<AudioBuffer> {
    if sample_rate == 0 || channels == 0 {
        return Err(Error::InvalidArgument(
            "decode needs a sample rate and a channel count".to_string(),
        ));
    }
    let bin = ffmpeg_bin();
    let cpu = cpu::lease();
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu.threads());
    let child = cmd
        .args(decode_args(path, sample_rate, channels))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let mut samples = Vec::new();
    pump_pcm(child, usize::from(channels) * 4, DECODE_STALL, &mut |frames| {
        samples.extend_from_slice(frames);
    })?;
    Ok(AudioBuffer::new(sample_rate, channels, samples))
}

/// `buf` as a 32-bit IEEE-float WAV file. Pure.
pub fn wav_bytes(buf: &AudioBuffer) -> Vec<u8> {
    let data_len = (buf.samples.len() * 4) as u32;
    let block_align = buf.channels * 4;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&buf.channels.to_le_bytes());
    out.extend_from_slice(&buf.sample_rate.to_le_bytes());
    out.extend_from_slice(&(buf.sample_rate * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &buf.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Write `buf` to `path` as a float WAV.
pub fn write_wav(path: &Path, buf: &AudioBuffer) -> Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&wav_bytes(buf))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_float_pcm() {
        let buf = AudioBuffer::new(44_100, 2, vec![0.5, -0.5, 0.25, -0.25]);
        let b = wav_bytes(&buf);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 36 + 16);
        assert_eq!(u16::from_le_bytes(b[20..22].try_into().unwrap()), 3, "IEEE float");
        assert_eq!(u16::from_le_bytes(b[22..24].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(b[24..28].try_into().unwrap()), 44_100);
        assert_eq!(u32::from_le_bytes(b[28..32].try_into().unwrap()), 44_100 * 8);
        assert_eq!(u16::from_le_bytes(b[32..34].try_into().unwrap()), 8);
        assert_eq!(&b[36..40], b"data");
        assert_eq!(u32::from_le_bytes(b[40..44].try_into().unwrap()), 16);
        assert_eq!(f32::from_le_bytes(b[44..48].try_into().unwrap()), 0.5);
        assert_eq!(b.len(), 44 + 16);
    }

    #[test]
    fn buffer_frames_duration_mono_and_peak() {
        let buf = AudioBuffer::new(4, 2, vec![1.0, 0.0, 0.5, 0.5, -0.8, 0.0, 0.0, 0.0]);
        assert_eq!(buf.frames(), 4);
        assert_eq!(buf.duration(), 1.0);
        assert_eq!(buf.mono(), vec![0.5, 0.5, -0.4, 0.0]);
        assert_eq!(buf.peak(), 1.0);
        assert_eq!(AudioBuffer::default().frames(), 0);
    }

    #[test]
    fn decode_args_ask_for_interleaved_f32_at_the_rate() {
        let args: Vec<String> = decode_args(Path::new("a.wav"), 22_050, 2)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(joined.ends_with("-map 0:a:0 -ac 2 -ar 22050 -f f32le pipe:1"), "{joined}");
    }

    #[test]
    #[ignore = "drives the ffmpeg binary"]
    fn a_written_wav_decodes_back_sample_identical() {
        let dir = std::env::temp_dir().join(format!("kerf-pcm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let samples: Vec<f32> = (0..48_000 * 2)
            .map(|i| ((i / 2) as f32 * 0.01).sin() * if i % 2 == 0 { 0.5 } else { 0.25 })
            .collect();
        let buf = AudioBuffer::new(48_000, 2, samples);
        let path = dir.join("tone.wav");
        write_wav(&path, &buf).unwrap();
        let back = decode_audio(&path, 48_000, 2).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(back, buf);
    }
}
