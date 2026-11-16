//! The live preview stream: its request, its argv and the pump that reads its frames.

use super::*;

// ---- live preview streaming -------------------------------------------------

/// One composited frame of the live preview.
pub struct PreviewFrame {
    /// The timeline time this frame shows, in seconds.
    pub time: f64,
    /// The frame as JPEG bytes.
    pub jpeg: Vec<u8>,
    /// How far behind its slot on the wall-clock schedule the frame arrived, in seconds (0 when
    /// it was early or on time). A stream that renders slower than it was asked to play grows
    /// this frame by frame; a hiccup shows once and is gone.
    pub late: f64,
}

/// The width a playback stream renders at when the caller does not say.
pub(super) const PREVIEW_STREAM_WIDTH: u32 = 960;

/// What a playback stream is asked for. Both are optional: the caller that knows better (the
/// preview, after the machine failed to keep up with the rate it was given) states them, and
/// leaving them out means the timeline's own rate and the default width.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlaybackRequest {
    pub fps: Option<f64>,
    pub max_width: Option<u32>,
}

/// What a stream really runs at, once [`PlaybackRequest`] is resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackParams {
    pub fps: f64,
    pub max_width: u32,
}

/// The frame rate and width a stream plays at.
///
/// An unstated rate is the timeline's own — the one the export would deliver, read from the
/// cut as it renders (a muted track's footage does not set it) — held to
/// [`MIN_STREAM_FPS`]`..=`[`MAX_STREAM_FPS`], so a 24 fps cut plays at 24 and a 60 fps one at 60
/// instead of both at the 24 this once was. A stated rate is clamped to the same range, and an
/// unusable one (not finite, not positive) is treated as unstated. An unstated width is
/// [`PREVIEW_STREAM_WIDTH`]; a stated one is held to [`MIN_STREAM_WIDTH`]`..=`[`MAX_STREAM_WIDTH`].
pub fn playback_params(timeline: &Timeline, assets: &[Asset], request: PlaybackRequest) -> PlaybackParams {
    let stated = request.fps.filter(|f| f.is_finite() && *f > 0.0);
    let fps = stated.unwrap_or_else(|| export_format(&timeline.for_render(), assets, &ExportOptions::default()).fps);
    PlaybackParams {
        fps: fps.clamp(MIN_STREAM_FPS, MAX_STREAM_FPS),
        max_width: request
            .max_width
            .map_or(PREVIEW_STREAM_WIDTH, |w| w.clamp(MIN_STREAM_WIDTH, MAX_STREAM_WIDTH)),
    }
}

/// JPEG quality (`-q:v`) for streamed frames: 2 is best, 31 worst. 6 is visually
/// clean while keeping a 960px frame near 40 KB, so 30 fps costs ~1.2 MB/s over
/// the IPC channel instead of the ~4 MB/s that q=2 would.
pub(super) const PREVIEW_STREAM_QUALITY: u8 = 6;

/// The preview's render size: the export geometry scaled down to at most
/// `max_width`, keeping the aspect and staying even (yuv420 needs it).
pub(super) fn preview_resolution(timeline: &Timeline, assets: &[Asset], max_width: u32) -> (u32, u32) {
    let natural = export_format(timeline, assets, &ExportOptions::default());
    let even = |v: u32| v.max(2) & !1;
    if natural.width <= max_width {
        return (even(natural.width), even(natural.height));
    }
    let scale = max_width as f64 / natural.width.max(1) as f64;
    (even(max_width), even((natural.height as f64 * scale).round() as u32))
}

/// Build the ffmpeg argument list that renders the timeline from `start` onwards
/// as a stream of JPEG frames on stdout. Pure, so the graph can be unit-tested.
///
/// Playback composites through **the same filtergraph the export builds** — from
/// a `Timeline::slice` starting at the playhead — so what plays is what renders:
/// every track, effect, transform, keyframe and overlay, not just the raw clip
/// under the playhead. Only the two ends differ from an export: proxy sources go
/// in (the caller substitutes those paths) and MJPEG comes out on a pipe.
#[cfg(test)]
pub(super) fn build_preview_args(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    max_width: u32,
    quality: u8,
) -> Result<Vec<String>> {
    build_preview_args_with(timeline, assets, start, fps, max_width, quality, decode_hwaccel())
}

/// [`build_preview_args`] with the decode acceleration given rather than read from
/// the machine (`KERF_HWACCEL`, and whether an earlier hardware failure turned it
/// off). The one seam the golden argv oracle needs to be independent of the machine
/// it runs on; the argv it builds is exactly what `build_preview_args` always did.
#[cfg(test)]
pub(super) fn build_preview_args_with(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    max_width: u32,
    quality: u8,
    hwaccel: Option<String>,
) -> Result<Vec<String>> {
    let cut = preview_cut(timeline, assets, start, fps, max_width, hwaccel)?;
    preview_args(&cut, &cut.opts, assets, quality)
}

/// What the playback stream plays: the cut from the playhead, how it is rendered and the frame it
/// is drawn in. Worked out apart from the argv so that what the argv will ask for — the animated
/// titles' pictures — can be on disk before ffmpeg is spawned.
pub(super) struct PreviewCut {
    sliced: Timeline,
    opts: ExportOptions,
    fmt: ExportFormat,
    /// The animated titles of the cut before it was sliced, how long that cut is, and the playhead
    /// the slice starts at: a title's pictures are named for the title, not for the playhead.
    titles: Vec<crate::model::SvgOverlay>,
    total: f64,
    start: f64,
}

impl PreviewCut {
    /// The animated titles' pictures the stream reads (named, not drawn).
    fn svg(&self) -> Vec<svg_layer::SvgSeq<'_>> {
        svg_layer::stream_sequences(&self.titles, self.total, &self.sliced, self.start, &self.fmt)
    }

    /// Every picture the stream reads: the floating titles' and the title clips' (whose input is
    /// named for the slice, so a stream from another playhead draws the rest of the title again).
    fn pictures(&self, assets: &[Asset]) -> Vec<svg_layer::SvgSeq<'_>> {
        let mut all = self.svg();
        let fx = transition_fx(&self.sliced, assets);
        all.extend(svg_layer::clip_seqs(&svg_layer::clip_sequences(&self.sliced, &self.fmt, &fx)));
        all
    }
}

pub(super) fn preview_cut(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    max_width: u32,
    hwaccel: Option<String>,
) -> Result<PreviewCut> {
    // Same gate as the export: muted / solo-shadowed tracks and disabled clips
    // never reach the graph, so what plays is what would render.
    let timeline = &timeline.for_render();
    let end = timeline.duration();
    // `is_finite` first so a NaN playhead is rejected here rather than becoming a
    // nonsensical `-ss` argument.
    if !start.is_finite() || start >= end {
        return Err(Error::Engine("nothing to play past the playhead".to_string()));
    }
    let sliced = timeline.slice(start, end);
    let has_video = sliced
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    if !has_video {
        return Err(Error::Engine("no video on the timeline from here".to_string()));
    }

    let opts = ExportOptions {
        include_audio: false,
        fps: Some(fps),
        resolution: Some(preview_resolution(&sliced, assets, max_width)),
        // mjpeg is a full-range JPEG codec: matching the graph's terminal format
        // to it keeps ffmpeg from inserting a range conversion per frame.
        pix_fmt: Some("yuvj420p".to_string()),
        // Bilinear over the export's default: at preview size the difference is
        // invisible and the scaler runs on every frame of every clip.
        scaler: Some("bilinear".to_string()),
        hwaccel,
        ..ExportOptions::default()
    };
    let fmt = export_format(&sliced, assets, &opts);
    Ok(PreviewCut {
        sliced,
        opts,
        fmt,
        titles: timeline.svg_overlays.clone(),
        total: end,
        start,
    })
}

/// The head every playback run starts with.
const PREVIEW_HEAD: [&str; 5] = ["-hide_banner", "-nostats", "-nostdin", "-loglevel", "error"];

/// The JPEGs on stdout every playback run ends with.
fn push_preview_sink(args: &mut Vec<String>, quality: u8) {
    args.extend(["-c:v", "mjpeg", "-q:v"].map(String::from));
    args.push(quality.clamp(2, 31).to_string());
    args.extend(["-f", "image2pipe", "pipe:1"].map(String::from));
}

/// The argv that plays a [`PreviewCut`] in one run as JPEGs on stdout, with `opts` (the cut's own,
/// but for the decode acceleration, which is read again for every run).
fn preview_args(cut: &PreviewCut, opts: &ExportOptions, assets: &[Asset], quality: u8) -> Result<Vec<String>> {
    let PreviewCut { sliced, fmt, .. } = cut;
    let mut args: Vec<String> = Vec::from(PREVIEW_HEAD.map(String::from));
    let plan = push_inputs(sliced, assets, fmt, opts, &mut args)?;
    let titles = cut.svg();
    svg_layer::push_inputs(&titles, fmt, &mut args);
    let graph = build_filter_complex_with(
        sliced,
        assets,
        fmt,
        sliced.duration(),
        opts,
        true,
        false,
        &plan,
        false,
        Some(&titles),
        "",
        None,
    );
    args.push("-filter_complex".to_string());
    args.push(graph.filter);
    args.push("-map".to_string());
    args.push("[outv]".to_string());
    args.push("-an".to_string());
    push_preview_sink(&mut args, quality);
    Ok(args)
}

/// The argv that plays one part of a stream planned in parts ([`preview_parts`]) as JPEGs on
/// stdout: exactly the part's frames, drawn by the export's part graph (pure).
fn preview_part_args(
    plan: &segments::PartsPlan,
    part: &segments::Part,
    titles: &[svg_layer::SvgSeq<'_>],
    opts: &ExportOptions,
    assets: &[Asset],
    quality: u8,
    fps_mode: &str,
) -> Result<Vec<String>> {
    let mut args: Vec<String> = Vec::from(PREVIEW_HEAD.map(String::from));
    segments::push_part_picture(plan, assets, opts, part, titles, &mut args)?;
    args.extend([fps_mode, "passthrough", "-frames:v"].map(String::from));
    args.push(part.frames.to_string());
    push_preview_sink(&mut args, quality);
    Ok(args)
}

/// The parts a stream of `cut` plays in, or `None` to play it in one run (pure). One run opens
/// every clip from the playhead to the end before it draws its first frame — measured here at
/// about 40 ms a clip, so a reel of twenty short clips took most of a second (two on the Windows
/// build) to start — where a part opens only the few on screen for it, and the next part is
/// started while this one plays.
pub(super) fn preview_parts(cut: &PreviewCut, assets: &[Asset], limits: segments::SegmentLimits) -> Option<segments::PartsPlan> {
    segments::plan_export(&cut.sliced, assets, &cut.opts, limits)
}

/// The limits a stream is planned in parts with, or `None` (`KERF_PLAYBACK_PARTS=off`) to play every
/// stream in one run.
fn playback_limits() -> Option<segments::SegmentLimits> {
    match std::env::var("KERF_PLAYBACK_PARTS") {
        Ok(v) if matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "no" | "false") => None,
        _ => Some(segments::PLAYBACK_LIMITS),
    }
}

/// Find the first complete JPEG in `buf` — its `FFD8` start-of-image through the
/// byte after its `FFD9` end-of-image — or `None` while one is still arriving.
///
/// Scanning for the markers is safe here: inside JPEG entropy-coded data every
/// `FF` byte is followed by `00` (byte stuffing) or a restart marker
/// (`FFD0`..`FFD7`), so `FFD9` only ever appears as the real end of image — and
/// ffmpeg's mjpeg encoder writes no embedded thumbnail that could nest one.
pub(super) fn next_jpeg(buf: &[u8]) -> Option<(usize, usize)> {
    let start = buf.windows(2).position(|w| w == [0xFF, 0xD8])?;
    let end = buf[start + 2..].windows(2).position(|w| w == [0xFF, 0xD9])?;
    Some((start, start + 2 + end + 2))
}

/// Play the timeline from `start`, calling `on_frame` with each composited frame
/// in turn; returning `false` from it stops playback and kills ffmpeg.
///
/// This is the difference between the preview being a slideshow and being video.
/// Frames used to come one `ffmpeg` process at a time — spawn, seek, decode,
/// exit, repeat — which caps out well below frame rate however fast the machine
/// is. One long-lived process decoding sequentially instead amortizes all of
/// that, and the all-intra proxies keep each decode cheap.
///
/// Frames are paced to the rate against the wall clock rather than pushed as fast
/// as they render, which keeps the pipe (and so ffmpeg itself) throttled to real
/// time instead of racing ahead and buffering the whole timeline. Each frame
/// carries its timeline time so a caller following the audio clock can drop one
/// that arrived too late to be worth showing, and how late it was against the
/// schedule, which is how a caller tells a hiccup from a stream that cannot keep
/// up (see [`playback_params`] for what the rate and width default to).
pub fn stream_preview_with(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    request: PlaybackRequest,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
) -> Result<()> {
    stream_preview_until(timeline, assets, start, request, &|| false, on_frame)
}

/// [`stream_preview_with`] that can be told to stop **before the first frame**: `cancel` is looked
/// at while the animated titles' pictures are being drawn (about forty times a second, and between
/// the steps that come before the drawing), which `on_frame` — called only once there are frames —
/// cannot interrupt. A stream cancelled then ends with [`Error::Cancelled`]; one that has started
/// is stopped by `on_frame` returning `false`, as ever.
pub fn stream_preview_until(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    request: PlaybackRequest,
    cancel: &dyn Fn() -> bool,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
) -> Result<()> {
    stream_preview_in(timeline, assets, start, request, cancel, playback_limits(), on_frame)
}

/// [`stream_preview_until`] planned in parts under `limits`, or in one run with `None`.
pub(super) fn stream_preview_in(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    request: PlaybackRequest,
    cancel: &dyn Fn() -> bool,
    limits: Option<segments::SegmentLimits>,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
) -> Result<()> {
    let PlaybackParams { fps, max_width } = playback_params(timeline, assets, request);
    let cut = preview_cut(timeline, assets, start, fps, max_width, decode_hwaccel())?;
    // Animated titles are pictures on disk by now (a few seconds' work at most); held until the
    // stream is over so the cache cannot take them away mid-play.
    let _titles = svg_layer::prepare_preview(&cut.pictures(assets), cancel)?;
    let plan = limits.and_then(|limits| preview_parts(&cut, assets, limits));
    let titles = cut.svg();
    if let Some(plan) = &plan {
        tracing::debug!(parts = plan.parts.len(), frames = plan.frames, "playing in parts");
    }
    let mut spawn = |i: usize| -> Result<Option<PreviewRun>> {
        // Read for every run: a hardware decode that failed has turned it off for the rest.
        let opts = ExportOptions {
            hwaccel: decode_hwaccel(),
            ..cut.opts.clone()
        };
        let (mut args, first) = match &plan {
            None if i == 0 => (preview_args(&cut, &opts, assets, PREVIEW_STREAM_QUALITY)?, 0),
            None => return Ok(None),
            Some(plan) => match plan.parts.get(i) {
                Some(part) => (
                    preview_part_args(plan, part, &titles, &opts, assets, PREVIEW_STREAM_QUALITY, fps_mode_flag())?,
                    part.first,
                ),
                None => return Ok(None),
            },
        };
        // The composited graph outgrows argv just as the export's does.
        let graph = externalize_filter_complex(&mut args, "preview")?;
        // Hardware decode is per input, and a proxy (what the stream reads once one is ready) is
        // faster in software, start-up included.
        let hw = tune_inputs(&mut args, HostOs::CURRENT, Reading::Stream, &facts_from_assets(assets)).hw;
        // Playback is paced to the wall clock, so it never races ahead; the cap is
        // there so the budget means the same thing while the cut is playing.
        cpu::limit_args(&mut args, cpu::budget());
        let bin = ffmpeg_bin();
        tracing::debug!(start, fps, max_width, part = i, "starting preview stream");
        let child = command(&bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| launch_err(&bin, e))?;
        Ok(Some(PreviewRun::start(child, first, hw, graph)))
    };
    let mut sent = None;
    pump_runs(&mut spawn, start, fps, PREVIEW_TIMEOUTS, on_frame, &mut sent)
}

/// [`stream_preview_with`] at a stated `fps` and the default width.
pub fn stream_preview(
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    fps: f64,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
) -> Result<()> {
    let request = PlaybackRequest {
        fps: Some(fps),
        max_width: None,
    };
    stream_preview_with(timeline, assets, start, request, on_frame)
}

/// How long a playback stream may go without producing a frame before it is
/// declared dead. A stalled ffmpeg (a hardware decoder that opens and then never
/// delivers, a driver stuck in teardown) is otherwise indistinguishable from a
/// slow one, and the read below would wait for it forever — the webview then
/// shows a frozen frame while the stream is "playing". `first` covers startup
/// (graph setup, hardware probing, the first decode of a long source); `stall`
/// covers the gap between frames once running, which a healthy stream paced to
/// real time keeps well under a second.
#[derive(Clone, Copy)]
pub(super) struct PreviewTimeouts {
    pub(super) first: std::time::Duration,
    pub(super) stall: std::time::Duration,
}

const PREVIEW_TIMEOUTS: PreviewTimeouts = PreviewTimeouts {
    first: std::time::Duration::from_secs(30),
    stall: std::time::Duration::from_secs(15),
};

/// One ffmpeg of a playback stream: its stdout read on a side thread so the pump can give up on a
/// silent one (`recv_timeout`) instead of blocking in `read`, its stderr drained on another so a
/// warning flood can't deadlock the frame read. Killed and reaped when dropped.
pub(super) struct PreviewRun {
    child: std::process::Child,
    rx: Option<std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>>,
    stderr: Option<std::thread::JoinHandle<String>>,
    /// The frame of the stream its first picture is.
    first: u64,
    /// Whether it asked any input for hardware decode.
    hw: bool,
    _graph: GraphScript,
}

impl PreviewRun {
    pub(super) fn start(mut child: std::process::Child, first: u64, hw: bool, graph: GraphScript) -> Self {
        use std::io::Read;
        let stderr = child.stderr.take().expect("stderr piped");
        let stderr = std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::BufReader::new(stderr).read_to_string(&mut buf);
            buf
        });
        // The bounded channel keeps the backpressure: once a few chunks are queued the reader stops
        // reading, the pipe fills and ffmpeg is throttled to the pacing — or, for the run started
        // ahead, held at its first frames until it is the one playing.
        let mut stdout = child.stdout.take().expect("stdout piped");
        let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<Vec<u8>>>(4);
        // Detached: after the kill it ends on EOF (or on the dropped receiver).
        std::thread::spawn(move || {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let msg = match stdout.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(n) => Ok(chunk[..n].to_vec()),
                    Err(e) => Err(e),
                };
                let failed = msg.is_err();
                if tx.send(msg).is_err() || failed {
                    return;
                }
            }
        });
        Self {
            child,
            rx: Some(rx),
            stderr: Some(stderr),
            first,
            hw,
            _graph: graph,
        }
    }

    fn recv(
        &self,
        wait: std::time::Duration,
    ) -> std::result::Result<std::io::Result<Vec<u8>>, std::sync::mpsc::RecvTimeoutError> {
        match &self.rx {
            Some(rx) => rx.recv_timeout(wait),
            None => Err(std::sync::mpsc::RecvTimeoutError::Disconnected),
        }
    }

    /// Kill it (with `kill`) and reap it: how it exited and the tail of what it said.
    fn end(&mut self, kill: bool) -> (Result<std::process::ExitStatus>, String) {
        if kill {
            let _ = self.child.kill();
        }
        self.rx = None;
        let status = reap(&mut self.child);
        let text = self.stderr.take().and_then(|h| h.join().ok()).unwrap_or_default();
        let mut tail: Vec<&str> = text.lines().rev().take(12).collect();
        tail.reverse();
        (status, tail.join("\n").trim().to_string())
    }
}

impl Drop for PreviewRun {
    fn drop(&mut self) {
        if self.stderr.is_some() {
            let _ = self.end(true);
        }
    }
}

/// [`pump_runs`] over the one ffmpeg already running.
#[cfg(test)]
pub(super) fn pump_preview(
    child: std::process::Child,
    start: f64,
    fps: f64,
    timeouts: PreviewTimeouts,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
    sent: &mut Option<u64>,
) -> Result<()> {
    let mut one = Some(PreviewRun::start(child, 0, false, GraphScript(None)));
    pump_runs(&mut |_| Ok(one.take()), start, fps, timeouts, on_frame, sent)
}

/// Read JPEG frames off the runs `spawn` starts (run `i`, or `None` past the last), pace them to
/// `fps` against the wall clock and hand them to `on_frame`, until the last run ends, the callback
/// declines a frame, or a run stops producing for longer than `timeouts` allows (it is then killed
/// and the call fails). **The next run is started once this one has shown its first frame**, so it
/// opens its inputs while this one plays and the pipe holds it at its first frames until its turn.
/// A run that fails before its first frame having asked for hardware decode is `-hwaccel` being
/// unusable here, not the graph (a static ffmpeg aborts outright when `auto` probes a VAAPI whose
/// libva isn't installed): decode goes to software for the process and that run is started again,
/// which replays nothing. Every run is reaped.
pub(super) fn pump_runs(
    spawn: &mut dyn FnMut(usize) -> Result<Option<PreviewRun>>,
    start: f64,
    fps: f64,
    timeouts: PreviewTimeouts,
    on_frame: &mut dyn FnMut(PreviewFrame) -> bool,
    sent: &mut Option<u64>,
) -> Result<()> {
    let Some(mut run) = spawn(0)? else {
        return Ok(());
    };
    *sent = Some(0);
    let mut at = 0usize;
    // The run after this one, once it has been asked for (`None` in it past the last).
    let mut next: Option<Option<PreviewRun>> = None;
    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    // Frames shown in all, and of this run.
    let mut index: u64 = 0;
    let mut shown: u64 = 0;
    let mut origin: Option<std::time::Instant> = None;

    loop {
        while let Some((s, e)) = next_jpeg(&buf) {
            let jpeg = buf[s..e].to_vec();
            buf.drain(..e);
            // Anchor the clock to the *first* frame: the graph takes a moment to
            // set up, and pacing from before that would make every later frame
            // look overdue and play the whole stream at a sprint.
            let t0 = *origin.get_or_insert_with(std::time::Instant::now);
            let frame_no = run.first + shown;
            let due = std::time::Duration::from_secs_f64(frame_no as f64 / fps);
            let late = match due.checked_sub(t0.elapsed()) {
                Some(wait) => {
                    std::thread::sleep(wait);
                    0.0
                }
                // The first frame is the clock's origin, on time by definition.
                None if index == 0 => 0.0,
                None => (t0.elapsed() - due).as_secs_f64(),
            };
            let frame = PreviewFrame {
                time: start + frame_no as f64 / fps,
                jpeg,
                late,
            };
            index += 1;
            shown += 1;
            *sent = Some(index);
            if !on_frame(frame) {
                return Ok(());
            }
            if shown == 1 {
                next = Some(spawn(at + 1)?);
            }
        }
        let wait = if shown == 0 { timeouts.first } else { timeouts.stall };
        let failure = match run.recv(wait) {
            Ok(Ok(bytes)) => {
                buf.extend_from_slice(&bytes);
                continue;
            }
            Ok(Err(e)) => format!("preview stream read failed: {e}"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let (status, tail) = run.end(false);
                if status?.success() {
                    let following = match next.take() {
                        Some(n) => n,
                        None => spawn(at + 1)?,
                    };
                    let Some(following) = following else {
                        return Ok(());
                    };
                    run = following;
                    at += 1;
                    shown = 0;
                    buf.clear();
                    continue;
                }
                format!("preview stream failed: {tail}")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => format!(
                "preview stream stalled: ffmpeg produced no {} for {}s",
                if shown == 0 { "first frame" } else { "frame" },
                wait.as_secs_f32()
            ),
        };
        let (_, tail) = run.end(true);
        if shown == 0 && run.hw {
            HWACCEL_OK.store(false, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!("hardware decode failed starting playback; using software decode: {failure}\n{tail}");
            // The run started ahead asked for it too.
            next = None;
            run = spawn(at)?.ok_or_else(|| Error::Engine("preview stream lost its part".to_string()))?;
            buf.clear();
            continue;
        }
        let reason = if failure.ends_with(&tail) {
            failure
        } else {
            format!("{failure}\n{tail}")
        };
        return Err(Error::Engine(reason.trim().to_string()));
    }
}
