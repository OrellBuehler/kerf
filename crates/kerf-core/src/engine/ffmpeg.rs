//! In-process FFmpeg (libav) engine via `ffmpeg-next`.
//!
//! `probe` reads stream metadata in-process. `render` assembles the timeline's
//! clips into an FFmpeg `filter_complex` (trim + concat) and runs the render.
//! Probing/analysis are in-process; the final export currently drives the
//! `ffmpeg` binary with the generated filtergraph — the graph builder is kept
//! pure so it can later be executed in-process as well.

use std::path::Path;
use std::process::Command;
use std::sync::Once;

use ffmpeg_next as ff;

use super::ProbeResult;
use crate::error::{Error, Result};
use crate::model::{Asset, StreamInfo, StreamKind, Timeline};

static INIT: Once = Once::new();

fn ensure_init() {
    INIT.call_once(|| {
        let _ = ff::init();
    });
}

pub fn probe(path: &Path) -> Result<ProbeResult> {
    ensure_init();

    let ictx = ff::format::input(&path)?;
    let duration = (ictx.duration() as f64 / f64::from(ff::ffi::AV_TIME_BASE)).max(0.0);

    let mut streams = Vec::new();
    for stream in ictx.streams() {
        let codec = ff::codec::context::Context::from_parameters(stream.parameters())?;
        let medium = codec.medium();
        let kind = match medium {
            ff::media::Type::Video => StreamKind::Video,
            ff::media::Type::Audio => StreamKind::Audio,
            ff::media::Type::Subtitle => StreamKind::Subtitle,
            _ => StreamKind::Data,
        };

        let mut info = StreamInfo {
            index: stream.index() as u32,
            kind,
            codec: format!("{:?}", codec.id()).to_lowercase(),
            width: None,
            height: None,
            fps: None,
            sample_rate: None,
            channels: None,
        };

        match medium {
            ff::media::Type::Video => {
                if let Ok(video) = codec.decoder().video() {
                    info.width = Some(video.width());
                    info.height = Some(video.height());
                }
                let rate = stream.rate();
                if rate.denominator() != 0 {
                    info.fps = Some(rate.numerator() as f64 / rate.denominator() as f64);
                }
            }
            ff::media::Type::Audio => {
                if let Ok(audio) = codec.decoder().audio() {
                    info.sample_rate = Some(audio.rate());
                    info.channels = Some(audio.channels());
                }
            }
            _ => {}
        }

        streams.push(info);
    }

    Ok(ProbeResult { duration, streams })
}

pub fn render(timeline: &Timeline, assets: &[Asset], output: &Path, _format: &str) -> Result<()> {
    ensure_init();

    let path_of = |id| assets.iter().find(|a| a.id == id).map(|a| a.path.clone());

    // Pick the first non-empty video track, otherwise the first non-empty track.
    let track = timeline
        .tracks
        .iter()
        .find(|t| t.kind == StreamKind::Video && !t.clips.is_empty())
        .or_else(|| timeline.tracks.iter().find(|t| !t.clips.is_empty()))
        .ok_or_else(|| Error::InvalidArgument("timeline has no clips to export".to_string()))?;

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y");

    let mut filter = String::new();
    let mut concat_inputs = String::new();
    for (i, clip) in track.clips.iter().enumerate() {
        let path = path_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        cmd.arg("-i").arg(path);
        filter.push_str(&format!(
            "[{i}:v]trim=start={start}:end={end},setpts=PTS-STARTPTS[v{i}];",
            i = i,
            start = clip.source_in,
            end = clip.source_out
        ));
        filter.push_str(&format!(
            "[{i}:a]atrim=start={start}:end={end},asetpts=PTS-STARTPTS,volume={vol}[a{i}];",
            i = i,
            start = clip.source_in,
            end = clip.source_out,
            vol = clip.volume
        ));
        concat_inputs.push_str(&format!("[v{i}][a{i}]", i = i));
    }
    let n = track.clips.len();
    filter.push_str(&format!(
        "{inputs}concat=n={n}:v=1:a=1[outv][outa]",
        inputs = concat_inputs,
        n = n
    ));

    cmd.arg("-filter_complex").arg(&filter);
    cmd.arg("-map").arg("[outv]").arg("-map").arg("[outa]");
    cmd.arg(output);

    let status = cmd
        .status()
        .map_err(|e| Error::Other(format!("failed to launch ffmpeg: {e}")))?;
    if !status.success() {
        return Err(Error::Other(format!("ffmpeg exited with {status}")));
    }
    Ok(())
}
