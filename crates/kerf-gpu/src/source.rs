//! `FrameSource` v0: one decoded frame of a source, as the planes FFmpeg
//! produced — nothing converted, nothing scaled.
//!
//! FFmpeg stays the decoder (D4): the frame comes from the `ffmpeg` **binary**
//! over a pipe as raw 8-bit `yuv420p`, so every codec, every hardware decoder
//! and the container's rotation (every FFmpeg decode autorotates) behave as
//! they do everywhere else in Kerf, and the build needs no dev libraries. The
//! planes are uploaded as they are and converted in a shader, because that is
//! where the colour matrix lives and where it can be changed in one place.
//!
//! Asking for `yuv420p` is also what the still graph ends up doing — `overlay`
//! only takes 4:2:0 — so a 4:2:2 / 4:4:4 / 10-bit source is reduced here the
//! same way, one stage earlier. A still image decodes through `yuva420p`
//! instead, so a picture with transparency is *seen* (alpha is not 255) and
//! refused instead of being silently flattened onto black.
//!
//! v0 scope, deliberately: one `ffmpeg` spawn per frame, software decode, no
//! cache, no proxy. The long-lived per-asset decoder, the frame cache, the proxy
//! and hardware decode are A1. The CPU budget's thread cap is applied (it is a
//! moment read: ungated, normal priority, like the still).

use std::process::Stdio;

use kerf_core::PlanLayer;

use crate::gpu::GpuError;

/// One decoded picture: planar 8-bit 4:2:0, tightly packed (no row padding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl YuvFrame {
    /// Size of the chroma planes: half the luma size, rounded up.
    pub fn chroma_size(&self) -> (u32, u32) {
        chroma_size(self.width, self.height)
    }

    /// Split raw `yuv420p` bytes into planes. `None` unless `raw` is exactly one
    /// frame of that size.
    pub fn from_yuv420p(width: u32, height: u32, raw: &[u8]) -> Option<Self> {
        let (cw, ch) = chroma_size(width, height);
        let (ys, cs) = ((width * height) as usize, (cw * ch) as usize);
        if raw.len() != ys + 2 * cs {
            return None;
        }
        Some(Self {
            width,
            height,
            y: raw[..ys].to_vec(),
            u: raw[ys..ys + cs].to_vec(),
            v: raw[ys + cs..].to_vec(),
        })
    }
}

pub(crate) fn chroma_size(w: u32, h: u32) -> (u32, u32) {
    (w.div_ceil(2), h.div_ceil(2))
}

/// The `ffmpeg` argv that decodes the frame a layer shows: the same
/// `-ss <t> -i <path>` the still graph uses (a still image is not seeked), one
/// frame, raw planar output on stdout.
pub fn decode_args(layer: &PlanLayer) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    if !layer.is_image {
        args.push("-ss".into());
        args.push(format!("{:.3}", layer.source_time));
    }
    args.extend(["-i".to_string(), layer.path.clone()]);
    // `out_range=tv` states the range the shader assumes (limited) instead of
    // trusting `-pix_fmt` to imply it: a full-range source (JPEG, some MJPEG
    // cameras) is converted by FFmpeg 6.1's `-pix_fmt yuva420p` but comes back
    // *unconverted* from FFmpeg 9's, which treats `yuvj420p` as `yuv420p` plus a
    // range tag a raw pipe cannot carry. Limited-range input passes through
    // `scale` untouched.
    args.extend(
        [
            "-frames:v",
            "1",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            "scale=out_range=tv",
            "-f",
            "rawvideo",
            "-pix_fmt",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    );
    // A still image is decoded with its alpha plane too, to see whether it has
    // any (an `rgb24` PNG comes back fully opaque).
    args.push(if layer.is_image { "yuva420p" } else { "yuv420p" }.into());
    args.push("pipe:1".into());
    args
}

/// Decode the frame `layer` shows.
pub fn decode_layer(layer: &PlanLayer) -> Result<YuvFrame, GpuError> {
    // Spawned the way the engine spawns every moment read: its `Command` (no
    // console window flashing over a GUI on Windows) and the CPU budget's thread
    // cap, applied here at spawn time because `decode_args` stays pure. Normal
    // priority and no `cpu::lease` — a frame read must not wait out a render.
    let mut args = decode_args(layer);
    kerf_core::limit_ffmpeg_args(&mut args);
    let output = kerf_core::ffmpeg_command()
        .args(&args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| GpuError::Decode(format!("could not run {}: {e}", kerf_core::ffmpeg_path())))?;
    if !output.status.success() {
        return Err(GpuError::Decode(format!(
            "ffmpeg failed on {}: {}",
            layer.path,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let (w, h) = (layer.stream.width, layer.stream.height);
    let raw = output.stdout;
    let luma_chroma = {
        let (cw, ch) = chroma_size(w, h);
        (w * h + 2 * cw * ch) as usize
    };
    let planes = if layer.is_image {
        // The alpha plane follows V; anything but fully opaque is not drawn.
        if raw.len() != luma_chroma + (w * h) as usize {
            return Err(GpuError::Decode(format!(
                "{}: ffmpeg returned {} bytes for a {w}x{h} frame, expected {}",
                layer.path,
                raw.len(),
                luma_chroma + (w * h) as usize
            )));
        }
        if raw[luma_chroma..].iter().any(|a| *a != 255) {
            return Err(GpuError::Unsupported(format!("{} has transparency", layer.path)));
        }
        &raw[..luma_chroma]
    } else {
        &raw[..]
    };
    YuvFrame::from_yuv420p(w, h, planes).ok_or_else(|| {
        GpuError::Decode(format!(
            "{}: ffmpeg returned {} bytes for a {w}x{h} frame (expected {luma_chroma}) — does the probed size match the decode?",
            layer.path,
            planes.len()
        ))
    })
}

/// Decode every layer's frame, one `ffmpeg` per layer **in parallel**: they are
/// independent processes, and FFmpeg's own still decodes its inputs side by side
/// too. No de-duplication — two layers of the same frame are decoded twice, as the
/// still graph does; that (and a decoder that stays alive) is A1's cache.
pub fn decode_layers(layers: &[PlanLayer]) -> Result<Vec<YuvFrame>, GpuError> {
    if let [only] = layers {
        return decode_layer(only).map(|f| vec![f]);
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = layers.iter().map(|l| scope.spawn(move || decode_layer(l))).collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(GpuError::Decode("a decode thread panicked".into())))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::{Color, PlanStream, Transform};
    use uuid::Uuid;

    fn layer(is_image: bool) -> PlanLayer {
        PlanLayer {
            clip_id: Uuid::nil(),
            asset_id: Uuid::nil(),
            track: 0,
            path: "/m/a.mp4".into(),
            is_image,
            source_time: 7.0,
            clip_time: 0.0,
            stream: PlanStream {
                width: 640,
                height: 360,
                rotation: 0,
                fps: Some(30.0),
                codec: "h264".into(),
                color_transfer: None,
                color_primaries: None,
            },
            transform: Transform::default(),
            color: Color::default(),
        }
    }

    #[test]
    fn a_video_frame_is_seeked_like_the_still_graph_does() {
        let a = decode_args(&layer(false)).join(" ");
        assert!(a.contains("-ss 7.000 -i /m/a.mp4"), "{a}");
        assert!(
            a.ends_with("-vf scale=out_range=tv -f rawvideo -pix_fmt yuv420p pipe:1"),
            "{a}"
        );
    }

    #[test]
    fn a_still_image_is_not_seeked_and_keeps_its_alpha_plane() {
        let a = decode_args(&layer(true)).join(" ");
        assert!(!a.contains("-ss"), "{a}");
        assert!(a.ends_with("-f rawvideo -pix_fmt yuva420p pipe:1"), "{a}");
    }

    #[test]
    fn planes_split_exactly_and_odd_sizes_round_the_chroma_up() {
        // 3x3: luma 9, chroma 2x2 = 4 each.
        let raw: Vec<u8> = (0..17).collect();
        let f = YuvFrame::from_yuv420p(3, 3, &raw).unwrap();
        assert_eq!((f.y.len(), f.u.len(), f.v.len()), (9, 4, 4));
        assert_eq!(f.chroma_size(), (2, 2));
        assert_eq!(f.u[0], 9);
        assert_eq!(f.v[0], 13);
        // A short or long read is not a frame.
        assert!(YuvFrame::from_yuv420p(3, 3, &raw[..16]).is_none());
        assert!(YuvFrame::from_yuv420p(3, 3, &[0; 18]).is_none());
    }
}
