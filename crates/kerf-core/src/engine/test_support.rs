//! What the engine's tests share (compiled for tests only): the ffmpeg runner that
//! bounds a hung binary, and the asset / clip / timeline fixtures the graph
//! builders, the clip timing and the render plan are all tested over.

use chrono::Utc;
use uuid::Uuid;

use crate::model::{Asset, Clip, StreamInfo, StreamKind, Timeline, Track};

use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Generous: synthesizing a few seconds of test video takes well under a
/// second, so anything near this is a hang, not a slow machine.
const LIMIT: Duration = Duration::from_secs(180);

pub(crate) trait StatusBounded {
    /// Like [`Command::status`], with stdin closed, and a panic naming the
    /// command if it has not finished within [`LIMIT`].
    fn status_bounded(&mut self) -> std::io::Result<ExitStatus>;
}

impl StatusBounded for Command {
    fn status_bounded(&mut self) -> std::io::Result<ExitStatus> {
        let mut child = self.stdin(Stdio::null()).spawn()?;
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("`{self:?}` still running after {}s; killed it", LIMIT.as_secs());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

// ---- fixtures ---------------------------------------------------------------

/// A 100 s asset at `/x.mp4` with `streams`.
pub(crate) fn test_asset(streams: Vec<StreamInfo>) -> Asset {
    Asset {
        id: Uuid::new_v4(),
        path: "/x.mp4".into(),
        name: "x.mp4".into(),
        duration: 100.0,
        streams,
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
    }
}

pub(crate) fn video_stream(w: u32, h: u32, fps: f64) -> StreamInfo {
    StreamInfo {
        index: 0,
        kind: StreamKind::Video,
        codec: "h264".into(),
        width: Some(w),
        height: Some(h),
        fps: Some(fps),
        sample_rate: None,
        channels: None,
        image: false,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: None,
        color_space: None,
    }
}

pub(crate) fn audio_stream(rate: u32, channels: u16) -> StreamInfo {
    StreamInfo {
        index: 1,
        kind: StreamKind::Audio,
        codec: "aac".into(),
        width: None,
        height: None,
        fps: None,
        sample_rate: Some(rate),
        channels: Some(channels),
        image: false,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: None,
        color_space: None,
    }
}

pub(crate) fn image_stream(w: u32, h: u32) -> StreamInfo {
    StreamInfo {
        index: 0,
        kind: StreamKind::Video,
        codec: "png".into(),
        width: Some(w),
        height: Some(h),
        fps: None,
        sample_rate: None,
        channels: None,
        image: true,
        projection: None,
        rotation: 0,
        color_transfer: None,
        color_primaries: None,
        pix_fmt: None,
        color_space: None,
    }
}

/// A clip of `asset_id`'s `source_in..source_out` placed at `timeline_start`.
pub(crate) fn make_clip(asset_id: Uuid, source_in: f64, source_out: f64, timeline_start: f64) -> Clip {
    Clip::new(asset_id, source_in, source_out, timeline_start)
}

/// A video track `V1` holding `clips`.
pub(crate) fn video_track(clips: Vec<Clip>) -> Track {
    Track {
        clips,
        ..Track::new(StreamKind::Video, "V1")
    }
}

/// An audio track `A1` holding `clips`.
pub(crate) fn audio_track(clips: Vec<Clip>) -> Track {
    Track {
        clips,
        ..Track::new(StreamKind::Audio, "A1")
    }
}

/// A timeline of `tracks` and nothing else.
pub(crate) fn timeline_of(tracks: Vec<Track>) -> Timeline {
    Timeline {
        tracks,
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
    }
}

/// A video + stereo-audio asset at `/media/clip.mp4`.
pub(crate) fn av_asset(id: Uuid, duration: f64) -> Asset {
    Asset {
        id,
        path: "/media/clip.mp4".into(),
        name: "clip.mp4".into(),
        duration,
        streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
    }
}

/// A still image asset at `/media/title.png`.
pub(crate) fn img_asset(id: Uuid) -> Asset {
    Asset {
        id,
        path: "/media/title.png".into(),
        name: "title.png".into(),
        duration: crate::model::DEFAULT_IMAGE_DURATION,
        streams: vec![image_stream(1920, 1080)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
    }
}

/// A timeline with a single video track holding `clips`.
pub(crate) fn single(clips: Vec<Clip>) -> Timeline {
    timeline_of(vec![video_track(clips)])
}
