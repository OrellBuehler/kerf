//! `FrameSource` v0: one decoded frame of a source, as the planes FFmpeg
//! produced — nothing converted, nothing scaled.
//!
//! FFmpeg stays the decoder (D4): the frame comes from the `ffmpeg` **binary**
//! over a pipe as 8-bit `yuv420p`, so every codec, every hardware decoder and
//! the container's rotation (every FFmpeg decode autorotates) behave as they do
//! everywhere else in Kerf, and the build needs no dev libraries. The planes are
//! uploaded as they are and converted in a shader, because that is where the
//! colour matrix lives and where it can be changed in one place.
//!
//! Asking for `yuv420p` is also what the still graph ends up doing — `overlay`
//! only takes 4:2:0 — so a 4:2:2 / 4:4:4 / 10-bit source is reduced here the
//! same way, one stage earlier.
//!
//! **The decode states its own size.** The pipe is `yuv4mpegpipe`, whose header
//! says `W..H..`, and the size is compared with the probed one: a JPEG with an
//! EXIF orientation probes as 480x270 and decodes autorotated as 270x480 — the
//! same number of bytes, and a sheared mess if the two were taken on trust. A
//! mismatch is [`GpuError::Unsupported`], so that frame goes through FFmpeg,
//! whose own graph reads the real size.
//!
//! **Alpha is never flattened silently.** A pixel format the probe recorded as
//! having alpha is refused up front (the plan does too). For an asset probed
//! before the format was recorded (`StreamInfo::pix_fmt` is `None`) the frame is
//! decoded a second time as `yuva420p` and refused if any alpha is below 255 —
//! costing a second decode for those assets only; re-importing them re-probes.
//!
//! **Nothing waits on FFmpeg forever or without bound**: the child is killed
//! after [`DECODE_TIMEOUT`], and what it writes to stdout is capped at the size
//! the probe allows (plus the header).
//!
//! **No frame is not an error.** `-ss` past the last frame (the final instants of
//! a clip that ends where its source does) decodes nothing, and FFmpeg's own
//! still then draws nothing for that layer — black over whatever is below — so
//! [`decode_layer`] returns `Ok(None)` and the compositor skips the layer.
//!
//! v0 scope, deliberately: one `ffmpeg` spawn per frame, software decode, no
//! cache, no proxy. The long-lived per-asset decoder, the frame cache, the proxy
//! and hardware decode are A1. The CPU budget's thread cap is applied, divided
//! among the layers decoded side by side (it is a moment read: ungated, normal
//! priority, like the still).

use std::io::Read;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kerf_core::PlanLayer;

use crate::gpu::GpuError;

/// How long a single frame decode may take before the child is killed — the
/// preview stream gives up on a first frame at the same 30 s.
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(30);

/// The largest picture side the decode accepts (wgpu's default texture limit).
/// Bounds the memory one decode can ask for.
pub const MAX_SIDE: u32 = 8192;

/// Room for a `yuv4mpegpipe` stream header and frame marker, beyond the frame.
const HEADER_SLACK: usize = 4096;

/// How much of FFmpeg's stderr is kept for an error message.
const STDERR_CAP: usize = 8192;

/// One decoded picture: planar 8-bit 4:2:0, tightly packed (no row padding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

/// Bytes of a `w` x `h` 8-bit 4:2:0 frame, or `None` on overflow.
pub(crate) fn yuv420p_len(w: u32, h: u32) -> Option<usize> {
    let (cw, ch) = chroma_size(w, h);
    let luma = u64::from(w).checked_mul(u64::from(h))?;
    let chroma = u64::from(cw).checked_mul(u64::from(ch))?;
    usize::try_from(luma.checked_add(chroma.checked_mul(2)?)?).ok()
}

impl YuvFrame {
    /// Size of the chroma planes: half the luma size, rounded up.
    pub fn chroma_size(&self) -> (u32, u32) {
        chroma_size(self.width, self.height)
    }

    /// Whether the planes are exactly the sizes `width` x `height` implies (and
    /// the picture is not empty) — what the compositor checks before it trusts
    /// the lengths to upload.
    pub fn is_consistent(&self) -> bool {
        let (cw, ch) = self.chroma_size();
        let plane = |w: u32, h: u32| u64::from(w).checked_mul(u64::from(h));
        self.width > 0
            && self.height > 0
            && plane(self.width, self.height) == Some(self.y.len() as u64)
            && plane(cw, ch) == Some(self.u.len() as u64)
            && plane(cw, ch) == Some(self.v.len() as u64)
    }

    /// Split raw `yuv420p` bytes into planes. `None` unless `raw` is exactly one
    /// frame of that size.
    pub fn from_yuv420p(width: u32, height: u32, raw: &[u8]) -> Option<Self> {
        if width == 0 || height == 0 || raw.len() != yuv420p_len(width, height)? {
            return None;
        }
        let (cw, ch) = chroma_size(width, height);
        let ys = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
        let cs = usize::try_from(u64::from(cw) * u64::from(ch)).ok()?;
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
/// frame, on stdout. `alpha: false` is the picture as `yuv4mpegpipe` (which
/// states its own size); `alpha: true` is raw `yuva420p`, whose last plane says
/// whether anything is transparent.
pub fn decode_args(layer: &PlanLayer, alpha: bool) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    if !layer.is_image {
        args.push("-ss".into());
        // The one spelling the FFmpeg still uses too, so both decode the same frame
        // on a fine time base (milliseconds skip one).
        args.push(kerf_core::seek_arg(layer.source_time));
    }
    args.extend(["-i".to_string(), layer.path.clone()]);
    // `out_range=tv` states the range the shader assumes (limited) instead of
    // trusting `-pix_fmt` to imply it: a full-range source (JPEG, some MJPEG
    // cameras) is converted by FFmpeg 6.1's `-pix_fmt yuva420p` but comes back
    // *unconverted* from FFmpeg 9's, which treats `yuvj420p` as `yuv420p` plus a
    // range tag a raw pipe cannot carry. Limited-range input passes through
    // `scale` untouched.
    args.extend(
        ["-frames:v", "1", "-an", "-sn", "-dn", "-vf", "scale=out_range=tv", "-f"]
            .iter()
            .map(|s| (*s).to_string()),
    );
    if alpha {
        args.extend(["rawvideo", "-pix_fmt", "yuva420p"].map(String::from));
    } else {
        args.extend(["yuv4mpegpipe", "-pix_fmt", "yuv420p"].map(String::from));
    }
    args.push("pipe:1".into());
    args
}

/// Parse the `yuv4mpegpipe` stream of one 8-bit 4:2:0 frame: the stated size and
/// its planes, or `None` when the stream holds no frame at all (a seek past the
/// end). Anything else malformed is an error.
pub(crate) fn parse_y4m(raw: &[u8]) -> Result<Option<YuvFrame>, String> {
    let line_end = |from: usize| raw[from..].iter().position(|b| *b == b'\n').map(|i| from + i);
    let Some(header_end) = line_end(0) else {
        // Nothing at all, or a header that never ended.
        return if raw.is_empty() {
            Ok(None)
        } else {
            Err("a truncated stream header".into())
        };
    };
    let header = std::str::from_utf8(&raw[..header_end]).map_err(|_| "a non-text stream header")?;
    let mut tokens = header.split_ascii_whitespace();
    if tokens.next() != Some("YUV4MPEG2") {
        return Err(format!("not a yuv4mpeg stream: {header:?}"));
    }
    let (mut width, mut height) = (None, None);
    for t in tokens {
        match t.split_at(1) {
            ("W", v) => width = v.parse::<u32>().ok(),
            ("H", v) => height = v.parse::<u32>().ok(),
            // No `C` tag means 4:2:0; any other layout is not what was asked for.
            ("C", v) if !v.starts_with("420") => return Err(format!("the stream is {v}, not 4:2:0")),
            _ => {}
        }
    }
    let (Some(w), Some(h)) = (width, height) else {
        return Err(format!("no picture size in {header:?}"));
    };
    let body = header_end + 1;
    if body >= raw.len() {
        return Ok(None); // header only: no frame was decoded
    }
    let Some(frame_end) = line_end(body) else {
        return Err("a truncated frame header".into());
    };
    if !raw[body..frame_end].starts_with(b"FRAME") {
        return Err("expected a FRAME marker".into());
    }
    let pixels = &raw[frame_end + 1..];
    let want = yuv420p_len(w, h).ok_or("a picture size that overflows")?;
    if pixels.len() != want {
        return Err(format!(
            "{} bytes of picture for a {w}x{h} frame (expected {want})",
            pixels.len()
        ));
    }
    YuvFrame::from_yuv420p(w, h, pixels)
        .map(Some)
        .ok_or_else(|| format!("an unusable {w}x{h} frame"))
}

/// Why a child run did not produce output.
enum RunError {
    /// It wrote more than the cap allows.
    Overflow,
    Failed(String),
}

/// Read up to `cap` bytes. Past it: with a `flag`, stop and raise it (the owner
/// kills the child); without, keep draining and discard so the child never
/// blocks on a full pipe.
fn read_capped(mut from: impl Read, cap: usize, flag: Option<&AtomicBool>) -> Vec<u8> {
    let mut out = Vec::with_capacity(cap.min(1 << 20));
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match from.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() + n > cap {
                    match flag {
                        Some(f) => {
                            f.store(true, Ordering::Relaxed);
                            break;
                        }
                        None => out.extend_from_slice(&chunk[..cap.saturating_sub(out.len()).min(n)]),
                    }
                } else {
                    out.extend_from_slice(&chunk[..n]);
                }
            }
        }
    }
    out
}

/// Run `ffmpeg` with `args`: stdout capped at `cap` bytes, the child killed on
/// overflow or after [`DECODE_TIMEOUT`].
fn run_ffmpeg(args: &[String], cap: usize) -> Result<Vec<u8>, RunError> {
    let mut cmd = kerf_core::ffmpeg_command();
    cmd.args(args);
    run_child(cmd, cap, DECODE_TIMEOUT)
}

/// Run `cmd`, capturing stdout up to `cap` bytes and a tail of stderr; kill the
/// child when it writes more than `cap` or runs past `timeout`.
fn run_child(mut cmd: std::process::Command, cap: usize, timeout: Duration) -> Result<Vec<u8>, RunError> {
    let bin = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunError::Failed(format!("could not run {bin}: {e}")))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Failed(format!("{bin}'s pipes were not captured")));
    };
    let overflow = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&overflow);
    let out = std::thread::spawn(move || read_capped(stdout, cap, Some(&flag)));
    let err = std::thread::spawn(move || read_capped(stderr, STDERR_CAP, None));

    let deadline = Instant::now() + timeout;
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if overflow.load(Ordering::Relaxed) => break Err(RunError::Overflow),
            Ok(None) if Instant::now() >= deadline => {
                break Err(RunError::Failed(format!(
                    "{bin} did not finish within {:.1} s",
                    timeout.as_secs_f64()
                )))
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => break Err(RunError::Failed(format!("waiting for {bin}: {e}"))),
        }
    };
    if outcome.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    // The pipes are closed now, so the readers finish.
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    match outcome {
        Ok(status) if status.success() && !overflow.load(Ordering::Relaxed) => Ok(stdout),
        Ok(_) if overflow.load(Ordering::Relaxed) => Err(RunError::Overflow),
        Ok(status) => {
            let tail = String::from_utf8_lossy(&stderr);
            Err(RunError::Failed(format!("{status}: {}", tail.trim())))
        }
        Err(e) => Err(e),
    }
}

/// Decode the frame `layer` shows. `Ok(None)` is "the source has no frame there"
/// (see the module docs), not a failure.
pub fn decode_layer(layer: &PlanLayer) -> Result<Option<YuvFrame>, GpuError> {
    decode_layer_shared(layer, 1)
}

/// [`decode_layer`] when `share` decodes run side by side, each getting its
/// fraction of the CPU budget's thread cap.
fn decode_layer_shared(layer: &PlanLayer, share: usize) -> Result<Option<YuvFrame>, GpuError> {
    let path = &layer.path;
    let (w, h) = (layer.stream.width, layer.stream.height);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(GpuError::Unsupported(format!(
            "{path}: a {w}x{h} picture (the compositor takes 1 to {MAX_SIDE} px a side)"
        )));
    }
    // A recorded pixel format must be on the allow-list of known-opaque ones: the
    // plan refuses the rest too, but a caller that skipped it must not get alpha
    // flattened onto black.
    if let Some(fmt) = layer.stream.pix_fmt.as_deref() {
        if kerf_core::model::pix_fmt_layout(fmt).is_none() {
            return Err(GpuError::Unsupported(format!(
                "{path}: the pixel format {fmt} is not one known to be opaque (it may carry alpha)"
            )));
        }
    }
    // The probed size bounds what the child may write; a bigger decode than the
    // probe said is a mismatch like a different one.
    let frame_len = yuv420p_len(w, h).ok_or_else(|| GpuError::Unsupported(format!("{path}: a {w}x{h} picture")))?;
    let run = |alpha: bool, cap: usize| {
        let mut args = decode_args(layer, alpha);
        kerf_core::limit_ffmpeg_args(&mut args, share);
        run_ffmpeg(&args, cap).map_err(|e| match e {
            RunError::Overflow => GpuError::Unsupported(format!(
                "{path}: the decode is larger than the probed {w}x{h} (a display rotation the probe missed?)"
            )),
            RunError::Failed(why) => GpuError::Decode(format!("{path}: {why}")),
        })
    };

    let raw = run(false, frame_len + HEADER_SLACK)?;
    let frame = parse_y4m(&raw).map_err(|why| GpuError::Decode(format!("{path}: {why}")))?;
    let Some(frame) = frame else {
        return Ok(None);
    };
    if (frame.width, frame.height) != (w, h) {
        return Err(GpuError::Unsupported(format!(
            "{path}: it decodes as {}x{} but was probed as {w}x{h} (an EXIF or display orientation the probe did not apply)",
            frame.width, frame.height
        )));
    }

    // A probe from before the pixel format was recorded cannot rule alpha out.
    if layer.stream.pix_fmt.is_none() {
        let plane = (u64::from(w) * u64::from(h)) as usize;
        let with_alpha = run(true, frame_len + plane)?;
        if with_alpha.len() != frame_len + plane {
            return Err(GpuError::Decode(format!(
                "{path}: {} bytes of yuva420p for a {w}x{h} frame (expected {})",
                with_alpha.len(),
                frame_len + plane
            )));
        }
        if with_alpha[frame_len..].iter().any(|a| *a != 255) {
            return Err(GpuError::Unsupported(format!("{path} has transparency")));
        }
    }
    Ok(Some(frame))
}

/// Decode every layer's frame, one `ffmpeg` per layer **in parallel** (each gets
/// its share of the CPU budget): they are independent processes, and FFmpeg's own
/// still decodes its inputs side by side too. No de-duplication — two layers of
/// the same frame are decoded twice, as the still graph does; that (and a decoder
/// that stays alive) is A1's cache.
pub fn decode_layers(layers: &[PlanLayer]) -> Result<Vec<Option<YuvFrame>>, GpuError> {
    if let [only] = layers {
        return decode_layer(only).map(|f| vec![f]);
    }
    let share = layers.len();
    std::thread::scope(|scope| {
        let handles: Vec<_> = layers
            .iter()
            .map(|l| scope.spawn(move || decode_layer_shared(l, share)))
            .collect();
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
    use kerf_core::{Color, LayerFx, Pick, PlanSource, PlanStream, PlanTiming, Transform};
    use uuid::Uuid;

    fn layer(is_image: bool) -> PlanLayer {
        PlanLayer {
            clip_id: Uuid::nil(),
            asset_id: Uuid::nil(),
            track: 0,
            path: "/m/a.mp4".into(),
            source: PlanSource::ORIGINAL,
            pick: Pick::AtOrAfter(7.0),
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
                pix_fmt: Some("yuv420p".into()),
                color_space: None,
            },
            transform: Transform::default(),
            color: Color::default(),
            name: "a".into(),
            projection: None,
            hdr: None,
            effects: Vec::new(),
            mask: None,
            reframe: None,
            fx: LayerFx::default(),
            animated: None,
            timing: PlanTiming {
                window: (0.0, 10.0),
                source_window: (0.0, 10.0),
                speed: 1.0,
                reversed: false,
            },
        }
    }

    #[test]
    fn a_video_frame_is_seeked_like_the_still_graph_does() {
        let a = decode_args(&layer(false), false).join(" ");
        assert!(a.contains("-ss 7.000000 -i /m/a.mp4"), "{a}");
        assert!(
            a.ends_with("-vf scale=out_range=tv -f yuv4mpegpipe -pix_fmt yuv420p pipe:1"),
            "{a}"
        );
    }

    /// A frame at 1.0006 s: `-ss 1.0006` returns it, the old `{:.3}` spelling
    /// (`1.001`) the frame after. A 10 fps ramp (frame `n` is luma `16 + 8n`) whose
    /// frames from the 10th on sit 0.6 ms late, on a 1/10000 time base the encoder
    /// and the mp4 both keep.
    ///
    /// `cargo test -p kerf-gpu --no-default-features -- --ignored fine_time_base`
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_decode_on_a_fine_time_base_lands_on_the_frame_at_the_second_not_the_next() {
        let dir = std::env::temp_dir().join(format!("kerf-gpu-fine-ss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let media = dir.join("ramp.mp4");
        let made = std::process::Command::new(kerf_core::ffmpeg_path())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(
                "color=c=gray:s=64x64:r=10:d=2,format=yuv420p,geq=lum='16+8*N':cb=128:cr=128,\
                 settb=1/10000,setpts='PTS+if(gte(N,10),6,0)'",
            )
            .args(["-c:v", "libx264", "-qp", "0", "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p"])
            .args([
                "-fps_mode",
                "passthrough",
                "-enc_time_base",
                "1/10000",
                "-video_track_timescale",
                "10000",
            ])
            .arg(&media)
            .stdin(std::process::Stdio::null())
            .status()
            .expect("run ffmpeg");
        assert!(made.success());

        let mut l = layer(false);
        l.path = media.to_string_lossy().into_owned();
        l.stream.width = 64;
        l.stream.height = 64;
        l.source_time = 1.0006;
        let frame = decode_layer(&l).expect("decode").expect("a frame");
        // The premise, on this ffmpeg: the millisecond spelling of that second (`1.001`,
        // what `{:.3}` made of it) lands on the frame after.
        l.source_time = 1.001;
        let next = decode_layer(&l).expect("decode").expect("a frame");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(next.y[0], 16 + 8 * 11, "this ffmpeg does not skip a frame on `-ss 1.001`");
        assert_eq!(
            frame.y[0],
            16 + 8 * 10,
            "ramp frame {} is not the one at 1.0006 s",
            (frame.y[0] - 16) / 8
        );
    }

    #[test]
    fn a_still_image_is_not_seeked_and_the_alpha_check_is_raw_yuva() {
        let a = decode_args(&layer(true), false).join(" ");
        assert!(!a.contains("-ss"), "{a}");
        let alpha = decode_args(&layer(true), true).join(" ");
        assert!(alpha.ends_with("-f rawvideo -pix_fmt yuva420p pipe:1"), "{alpha}");
    }

    #[test]
    fn planes_split_exactly_and_odd_sizes_round_the_chroma_up() {
        // 3x3: luma 9, chroma 2x2 = 4 each.
        let raw: Vec<u8> = (0..17).collect();
        let f = YuvFrame::from_yuv420p(3, 3, &raw).unwrap();
        assert_eq!((f.y.len(), f.u.len(), f.v.len()), (9, 4, 4));
        assert_eq!(f.chroma_size(), (2, 2));
        assert!(f.is_consistent());
        assert_eq!(f.u[0], 9);
        assert_eq!(f.v[0], 13);
        // A short or long read is not a frame.
        assert!(YuvFrame::from_yuv420p(3, 3, &raw[..16]).is_none());
        assert!(YuvFrame::from_yuv420p(3, 3, &[0; 18]).is_none());
        // Neither is a frame of no size, or one whose size overflows.
        assert!(YuvFrame::from_yuv420p(0, 3, &[]).is_none());
        assert!(YuvFrame::from_yuv420p(u32::MAX, u32::MAX, &raw).is_none());
        // A frame whose planes disagree with its size is inconsistent.
        let mut bad = f;
        bad.u.pop();
        assert!(!bad.is_consistent());
        let empty = YuvFrame {
            width: 0,
            height: 0,
            y: vec![],
            u: vec![],
            v: vec![],
        };
        assert!(!empty.is_consistent());
    }

    fn y4m(header: &str, frame: Option<&[u8]>) -> Vec<u8> {
        let mut v = format!("{header}\n").into_bytes();
        if let Some(f) = frame {
            v.extend_from_slice(b"FRAME\n");
            v.extend_from_slice(f);
        }
        v
    }

    #[test]
    fn a_y4m_stream_states_its_size_and_carries_the_planes() {
        let raw = y4m("YUV4MPEG2 W4 H2 F30:1 Ip A1:1 C420jpeg XYSCSS=420JPEG", Some(&[7u8; 12]));
        let f = parse_y4m(&raw).unwrap().unwrap();
        assert_eq!((f.width, f.height), (4, 2));
        assert_eq!(f.y.len(), 8);
        // A different declared size is reported as it is — the caller compares.
        let raw = y4m("YUV4MPEG2 W2 H4 C420mpeg2", Some(&[0u8; 12]));
        let f = parse_y4m(&raw).unwrap().unwrap();
        assert_eq!((f.width, f.height), (2, 4));
    }

    #[test]
    fn a_y4m_stream_with_no_frame_is_none_not_an_error() {
        assert!(parse_y4m(&y4m("YUV4MPEG2 W640 H360 F30:1", None)).unwrap().is_none());
        assert!(parse_y4m(&[]).unwrap().is_none());
    }

    #[test]
    fn a_malformed_y4m_stream_is_an_error() {
        assert!(parse_y4m(b"nonsense\nFRAME\n").is_err());
        assert!(
            parse_y4m(&y4m("YUV4MPEG2 W4 H2 C444", Some(&[0u8; 24]))).is_err(),
            "not 4:2:0"
        );
        assert!(parse_y4m(&y4m("YUV4MPEG2 F30:1", Some(&[0u8; 12]))).is_err(), "no size");
        assert!(parse_y4m(&y4m("YUV4MPEG2 W4 H2", Some(&[0u8; 11]))).is_err(), "short picture");
        assert!(parse_y4m(&y4m("YUV4MPEG2 W4 H2", Some(&[0u8; 13]))).is_err(), "long picture");
        assert!(parse_y4m(b"YUV4MPEG2 W4 H2\nNOPE\n").is_err());
        assert!(parse_y4m(b"YUV4MPEG2 W4 H2").is_err(), "unterminated header");
    }

    #[test]
    fn a_probed_alpha_format_is_refused_before_anything_is_spawned() {
        let mut l = layer(false);
        l.stream.pix_fmt = Some("yuva420p".into());
        l.path = "/definitely/not/there.mkv".into();
        assert!(matches!(decode_layer(&l), Err(GpuError::Unsupported(why)) if why.contains("alpha")));
    }

    #[test]
    fn a_picture_beyond_the_limit_is_refused_before_anything_is_spawned() {
        let mut l = layer(false);
        l.stream.width = MAX_SIDE + 1;
        assert!(matches!(decode_layer(&l), Err(GpuError::Unsupported(_))));
    }

    #[cfg(unix)]
    fn sh(script: &str) -> std::process::Command {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_writes_too_much_is_killed_not_buffered() {
        // Ten megabytes against a 64 KiB cap, from a process that would go on
        // forever: it must be stopped, quickly, with the overflow reported.
        let t = Instant::now();
        let r = run_child(sh("exec yes"), 64 * 1024, Duration::from_secs(20));
        assert!(matches!(r, Err(RunError::Overflow)));
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_does_not_finish_is_killed_at_the_deadline() {
        let t = Instant::now();
        let r = run_child(sh("exec sleep 30"), 1024, Duration::from_millis(300));
        assert!(matches!(&r, Err(RunError::Failed(why)) if why.contains("did not finish")));
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_fails_reports_its_stderr_tail_and_one_that_succeeds_its_output() {
        let r = run_child(sh("echo boom >&2; exit 3"), 1024, Duration::from_secs(20));
        assert!(matches!(&r, Err(RunError::Failed(why)) if why.contains("boom")));
        let r = run_child(sh("printf hello"), 1024, Duration::from_secs(20));
        assert!(matches!(r, Ok(ref out) if out == b"hello"));
        // Exactly the cap is fine; a byte more is not.
        assert!(run_child(sh("head -c 100 /dev/zero"), 100, Duration::from_secs(20)).is_ok());
        assert!(matches!(
            run_child(sh("head -c 101 /dev/zero"), 100, Duration::from_secs(20)),
            Err(RunError::Overflow)
        ));
    }

    #[test]
    fn a_missing_binary_is_a_decode_error_naming_it() {
        let r = run_child(
            std::process::Command::new("/definitely/not/ffmpeg"),
            1024,
            Duration::from_secs(1),
        );
        assert!(matches!(&r, Err(RunError::Failed(why)) if why.contains("/definitely/not/ffmpeg")));
    }
}
