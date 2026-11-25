//! Preview frames, the playback stream, waveforms and filmstrips.

use crate::*;

// ---- media (preview frames, waveforms) -------------------------------------

/// While the preview source is **Proxy only**, the preview never decodes an original: a frame of
/// (or playback over) clips whose proxy is not ready is refused with what it is waiting for.
/// Under any other source this is nothing. The page keeps the last good frame and says what the
/// preview is waiting on from the proxy statuses it already holds.
///
/// The waits are read under the project lock (`Project::proxy_waits`) and acted on once it is
/// released: the proxy queue is told what the preview is waiting on (nothing, when it is not), so
/// those proxies are built before the rest, and a frame that waits is refused with the message.
fn refuse_while_waiting(waits: &[kerf_core::ProxyWait]) -> CmdResult<()> {
    crate::proxy_queue::note_waits(waits);
    match kerf_core::proxy::waits_message(waits) {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

#[tauri::command]
pub(crate) async fn get_frame(
    state: State<'_, AppState>,
    asset_id: String,
    time_secs: f64,
    max_width: Option<u32>,
    accurate: Option<bool>,
) -> CmdResult<String> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve the asset under the lock, then *drop the guard* before decoding: the
        // ffmpeg run must not hold the shared Project mutex for its whole duration, or
        // it freezes every other op (timeline edits, MCP, the next scrub frame).
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        // JPEG rather than PNG: the preview pane never needs lossless frames, and a
        // q=4 JPEG is ~5–10× smaller to encode and ship over IPC — which matters now
        // that the preview fetches frames continuously during playback. `accurate`
        // is false for rough scrub frames (keyframe-snap), true for the settled frame.
        let jpeg = Project::decode_preview_frame(&asset, time_secs, max_width.unwrap_or(960), 4, accurate.unwrap_or(true))
            .map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg);
        Ok(format!("data:image/jpeg;base64,{b64}"))
    })
    .await
}

/// The composited timeline still at `time_secs` — every visible clip put through
/// the same color / effect / transform / overlay chain the export uses, so the
/// preview reflects Inspector edits live (unlike `get_frame`, a raw source decode).
#[tauri::command]
pub(crate) async fn get_timeline_frame(state: State<'_, AppState>, time_secs: f64, max_width: Option<u32>) -> CmdResult<String> {
    let shared = state.project.clone();
    let scrub = state.scrub.clone();
    blocking(move || {
        // Resolve the inputs under the lock, then *drop the guard* before the ffmpeg
        // composite — the preview fetches frames continuously during playback, and
        // holding the shared Project mutex for the whole decode would freeze every
        // other op (timeline edits, MCP, the next scrub frame). Mirrors `get_frame`.
        let (waits, (timeline, assets)) = {
            let project = lock_user(&shared);
            let waits = project.proxy_waits(time_secs, Some(time_secs)).map_err(|e| e.to_string())?;
            (waits, project.timeline_frame_inputs().map_err(|e| e.to_string())?)
        };
        refuse_while_waiting(&waits)?;
        // Through the scrub cache: the frame of the cut's grid that holds the time, from the cache,
        // or from the window of neighbours one process draws for it, or alone (`ScrubFrames`).
        let jpeg = scrub
            .frame(&timeline, &assets, time_secs, max_width.unwrap_or(960), 4)
            .map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(jpeg.as_slice());
        Ok(format!("data:image/jpeg;base64,{b64}"))
    })
    .await
}

/// The frame under the playhead for the Preview panel, through the GPU compositor when that is
/// on and draws it exactly, else FFmpeg's JPEG — the same one `get_timeline_frame` returns, in
/// the same call, with the reasons the GPU did not draw it.
///
/// `overlays` is the page saying it has something to draw over the picture (a title box, the trim
/// monitor, safe-area guides): a surface that sits above the page cannot show those, so such a
/// frame is the JPEG. See [`gpu_preview`]; GUI-only, so there is no MCP tool.
///
/// **Latest wins**: a request does not wait behind the ones before it. It replaces the one waiting
/// (that call answers `renderer: "superseded"` at once, with nothing drawn) and is drawn when the
/// frame in hand is done, so a drag can post a request per playhead move without queueing them. The
/// frame's inputs are a snapshot of the project (`gpu_preview::snapshot`): the project lock is taken
/// only when the plan epoch says something changed, never per frame.
///
/// `client` and `seq` say which request of which page this is: Tauri runs each call as a task of
/// its own, so two can arrive in the other order than they were made, and one that arrives after a
/// newer request of the same `client` answers `superseded` instead of replacing it.
///
/// `hint: "forward"` (with `client` and a `session` number) is a frame of **GPU playback**: the page
/// asks for the time the picture should show, once per animation frame while the cut plays at 1x,
/// and the first request of a session decides whether the GPU plays it (`renderer: "gpu"`) or the
/// stream does (`renderer: "stream"`, with the reasons); every answer says in `play.ahead` where the
/// GPU will stop drawing, if it will. See [`gpu_preview::play`]'s module docs; no other hint is known.
///
/// `draft` is what a gesture under way would write (the clips and titles it changes, whole): the frame
/// is drawn as the cut would be with it written, and nothing is written (`gpu_preview::draft`). A drag's
/// frame the GPU does not draw is FFmpeg's JPEG of the cut as it is.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn get_preview_frame(
    state: State<'_, AppState>,
    gpu: State<'_, Arc<gpu_preview::GpuPreview>>,
    time_secs: f64,
    max_width: Option<u32>,
    overlays: Option<bool>,
    client: Option<String>,
    seq: Option<u64>,
    hint: Option<String>,
    session: Option<u64>,
    draft: Option<gpu_preview::PreviewDraft>,
) -> CmdResult<gpu_preview::PreviewFrameResult> {
    let shared = state.project.clone();
    let mut request = gpu_preview::FrameRequest::new(
        Arc::new(move || {
            let project = lock_user(&shared);
            gpu_preview::take_inputs(&project)
        }),
        time_secs,
        max_width.unwrap_or(960),
        overlays.unwrap_or(false),
    );
    request = request.with_hint(hint.as_deref(), client.as_deref(), session, draft.is_some())?;
    if let Some(draft) = draft {
        request = request.drafted(draft);
    }
    if let (Some(client), Some(seq)) = (client, seq) {
        request = request.ordered(client, seq);
    }
    let answer = gpu.post(request, |job| {
        // The worker waits for FFmpeg and the GPU: the blocking pool's, never a runtime thread.
        tauri::async_runtime::spawn_blocking(job);
    });
    match answer.await {
        Ok(gpu_preview::Delivery::Done(result)) => result,
        Ok(gpu_preview::Delivery::Superseded) => Ok(gpu_preview::PreviewFrameResult::superseded()),
        Ok(gpu_preview::Delivery::Abandoned) | Err(_) => Err("the GPU preview's frame worker stopped".to_string()),
    }
}

/// Where the Preview frame is in the window (device pixels, relative to the webview) and whether
/// the native surface should be showing. Called on mount, resize, dock moves, workspace switches
/// and window moves; cheap, and it never waits for a render.
#[tauri::command(async)]
pub(crate) fn set_preview_bounds(
    gpu: State<'_, Arc<gpu_preview::GpuPreview>>,
    bounds: gpu_preview::BoundsReport,
) -> CmdResult<()> {
    gpu.set_bounds(&bounds)
}

/// What the GPU preview is doing on this machine: the setting, whether the platform has a surface
/// technique, whether a device is up, and why not when it is not.
#[tauri::command(async)]
pub(crate) fn gpu_preview_status(gpu: State<'_, Arc<gpu_preview::GpuPreview>>) -> gpu_preview::GpuPreviewStatus {
    gpu.status()
}

/// The page is about to run the installer of an update, which on Windows ends the process without a `RunEvent::Exit`
/// (`tauri-plugin-updater` calls `std::process::exit(0)`, and its `on_before_exit` is out of reach of the page's
/// `downloadAndInstall`): this run is not one that ended while the GPU preview was up. See [`gpu_preview::GpuPreview::leaving`].
#[tauri::command(async)]
pub(crate) fn gpu_preview_leaving(gpu: State<'_, Arc<gpu_preview::GpuPreview>>) {
    gpu.leaving();
}

/// The install [`gpu_preview_leaving`] announced failed, and the app goes on: the markers go back.
#[tauri::command(async)]
pub(crate) fn gpu_preview_staying(gpu: State<'_, Arc<gpu_preview::GpuPreview>>) {
    gpu.staying();
}

/// Bytes in front of the JPEG in a playback message: the frame's timeline time as an `f64`, little
/// endian. Mirrored by `parsePlaybackMessage` in `frontend/src/lib/frame-transport.ts`.
pub(crate) const PLAYBACK_HEADER_BYTES: usize = 8;

/// One composited frame as it goes to the webview during playback: [`PLAYBACK_HEADER_BYTES`] of
/// timeline time (so the webview can drop a frame that arrived after the audio clock has already
/// moved past it), then the JPEG, undecoded.
///
/// Raw bytes through the channel rather than a base64 `data:` URL in JSON: a quarter fewer bytes
/// (76 -> 57 KB a frame at 1080p), no JSON to parse and no base64 to encode here. It is *not* a faster
/// decode: measured in headless Chromium, the old path's `JSON.parse` + `img.src =` cost 0.1-0.3 ms a
/// frame, since a data-URL image is decoded lazily and off the main thread, and `createImageBitmap`
/// plus a canvas draw took no less than the `<img>` did (see `frame-transport.ts`). The page builds
/// the data URL itself, for the frames it shows.
pub(crate) fn playback_message(time: f64, jpeg: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(PLAYBACK_HEADER_BYTES + jpeg.len());
    message.extend_from_slice(&time.to_le_bytes());
    message.extend_from_slice(jpeg);
    message
}

/// Which playback stream should be running, by the id the page gave it (0 = none).
///
/// The ids come from the caller rather than being minted here, because start and stop are
/// separate async IPC calls that can arrive out of order, and a bare generation counter would
/// let a stop meant for the previous stream land after the next one started and kill it, which
/// reads as playback that dies the moment you seek. Ids only ever go up: within a page a new
/// playback's is higher than the last one's, and a page that was reloaded draws its ids from a
/// newer [`playback_session`], so they are higher than anything the page before it used (a
/// counter restarting at 1 would collide with the stale ids still held here). Both questions are
/// then plain comparisons, whichever order the calls arrive in:
///
/// * the stream with the **highest id started** is the one that should run (`begin` keeps the
///   maximum, so a start that arrives late cannot take the pane back from a newer one);
/// * a stream is **stopped** once a stop at or above its id has been seen, which also covers a
///   stop that reaches the backend *before* the start it was meant for: such a stop finds
///   nothing to clear, and the stream would otherwise start with nobody left to end it.
pub(crate) struct Playbacks {
    active: AtomicU64,
    stopped: AtomicU64,
}

impl Playbacks {
    pub(crate) const fn new() -> Self {
        Self {
            active: AtomicU64::new(0),
            stopped: AtomicU64::new(0),
        }
    }

    /// Playback `id` is starting: it supersedes every lower id.
    pub(crate) fn begin(&self, id: u64) {
        self.active.fetch_max(id, Ordering::SeqCst);
    }

    /// The page stopped playback `id` (a pause, a seek, an edit).
    pub(crate) fn stop(&self, id: u64) {
        self.stopped.fetch_max(id, Ordering::SeqCst);
        let _ = self.active.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// Whether playback `id` should end: superseded by a newer one, or stopped.
    pub(crate) fn ended(&self, id: u64) -> bool {
        self.active.load(Ordering::SeqCst) != id || self.stopped.load(Ordering::SeqCst) >= id
    }

    /// Playback `id` ran out; free the slot if it is still the one in it.
    pub(crate) fn finish(&self, id: u64) {
        let _ = self.active.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

static PLAYBACKS: Playbacks = Playbacks::new();

/// Pages (webview loads) that have asked for playback ids this run.
static PLAYBACK_SESSIONS: AtomicU64 = AtomicU64::new(0);

/// A number for a page to build its playback ids from, higher than any this run has handed
/// out: the page's ids are `session * 2^32 + n` (`playback-id.ts`), so a reloaded page's ids
/// outrank the ones the page before it left behind. 2^21 sessions fit the 2^53 a JS number
/// holds exactly; a run has no use for that many reloads.
#[tauri::command(async)]
pub(crate) fn playback_session() -> u64 {
    PLAYBACK_SESSIONS.fetch_add(1, Ordering::SeqCst) + 1
}

/// Play the timeline from `start`, streaming composited frames to `on_frame`
/// until playback is stopped, superseded, or the timeline ends.
///
/// Scrubbing and the settled frame still go through `get_timeline_frame` — one
/// process per frame is right when you want *one* frame. Playback is the case
/// that can't work that way: a spawn-seek-decode-exit cycle per frame caps out
/// well below frame rate, so this hands the whole span to a single long-lived
/// ffmpeg and pushes frames up as they render, each as a raw message
/// ([`playback_message`]).
///
/// `fps` and `max_width` are what the preview asks for; left out, the stream plays at the
/// timeline's own rate and the default width (`kerf_core::playback_params`). The preview states
/// them only after a stream could not keep up and it is restarting cheaper.
///
/// Resolves only when playback ends; the caller is not expected to await it.
#[tauri::command]
pub(crate) async fn start_playback(
    state: State<'_, AppState>,
    playback_id: u64,
    start: f64,
    fps: Option<f64>,
    max_width: Option<u32>,
    on_frame: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> CmdResult<()> {
    let shared = state.project.clone();
    // The stream takes the pane over: the window of frames being drawn around the playhead is of
    // no use until it stops, and is not left running beside it.
    state.scrub.stop();
    PLAYBACKS.begin(playback_id);
    blocking(move || {
        // Resolve the inputs under the lock and drop the guard before streaming:
        // playback runs for as long as the user watches, and holding the shared
        // mutex for that would freeze every edit and the whole MCP server.
        let (waits, (timeline, assets)) = {
            let project = lock_user(&shared);
            let waits = project.proxy_waits(start, None).map_err(|e| e.to_string())?;
            (waits, project.timeline_frame_inputs().map_err(|e| e.to_string())?)
        };
        refuse_while_waiting(&waits)?;
        stream_playback(
            playback_id,
            &timeline,
            &assets,
            start,
            kerf_core::PlaybackRequest { fps, max_width },
            on_frame,
        )
    })
    .await
}

/// Play one library asset alone from `start` (its own time in seconds), streaming frames to
/// `on_frame` as [`start_playback`] does, over a timeline of nothing but that asset
/// (`Project::asset_playback_timeline`): the frame is the asset's own, so nothing is letterboxed,
/// and each frame's time is a time of the asset's file. It takes the same `playback_id`s, so it
/// supersedes the cut's stream and is superseded by it, and `stop_playback` stops it. Reads the
/// asset's proxy as the cut's preview does, and under **Proxy only** refuses an asset whose proxy
/// is not ready, as the cut does. An asset with no picture is refused.
#[tauri::command]
pub(crate) async fn start_asset_playback(
    state: State<'_, AppState>,
    playback_id: u64,
    asset_id: String,
    start: f64,
    fps: Option<f64>,
    max_width: Option<u32>,
    on_frame: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> CmdResult<()> {
    let asset_id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    state.scrub.stop();
    PLAYBACKS.begin(playback_id);
    blocking(move || {
        // The asset under the lock; the proxy checks touch the disk, so they run with it released.
        let input = lock_user(&shared).proxy_input(asset_id).map_err(|e| e.to_string())?;
        let play = Project::asset_playback(input).map_err(|e| e.to_string())?;
        refuse_while_waiting(&play.waits)?;
        stream_playback(
            playback_id,
            &play.timeline,
            &play.assets,
            start,
            kerf_core::PlaybackRequest { fps, max_width },
            on_frame,
        )
    })
    .await
}

/// The stream of [`start_playback`] and [`start_asset_playback`]: `timeline` from `start` to
/// `on_frame` until it ends, is stopped or is superseded under `playback_id`. Holds no lock.
fn stream_playback(
    playback_id: u64,
    timeline: &Timeline,
    assets: &[Asset],
    start: f64,
    request: kerf_core::PlaybackRequest,
    on_frame: tauri::ipc::Channel<tauri::ipc::InvokeResponseBody>,
) -> CmdResult<()> {
    // Counted and logged below: whether frames reached the webview at all, and
    // how long the first one took, are the two facts that tell a stream that
    // never started from one whose frames the preview received and discarded.
    // Without them a black pane looks identical either way. Late frames are the
    // third: a stream whose count of them keeps growing is one that cannot render
    // at the rate it was given, and the preview is restarting it cheaper.
    let params = kerf_core::playback_params(timeline, assets, request);
    let began = std::time::Instant::now();
    let mut frames: u64 = 0;
    let mut late_frames: u64 = 0;
    let mut worst_late = 0.0_f64;
    let mut first_frame: Option<std::time::Duration> = None;
    // Superseded by a newer playback, or explicitly stopped — including by a stop that arrived
    // before this stream even started.
    let ended = || PLAYBACKS.ended(playback_id);
    // `ended` is also the cancel of the animated titles' drawing, which can take seconds before
    // the first frame exists for the callback below to decline.
    let result = kerf_core::stream_preview_until(timeline, assets, start, request, &ended, &mut |f| {
        if ended() {
            return false;
        }
        let sent = on_frame
            .send(tauri::ipc::InvokeResponseBody::Raw(playback_message(f.time, &f.jpeg)))
            .is_ok();
        if sent {
            frames += 1;
            if f.late > 2.0 / params.fps {
                late_frames += 1;
            }
            worst_late = worst_late.max(f.late);
            first_frame.get_or_insert_with(|| began.elapsed());
        }
        sent
    });
    tracing::info!(
        playback_id,
        frames,
        fps = params.fps,
        max_width = params.max_width,
        late_frames,
        worst_late_ms = (worst_late * 1000.0) as u64,
        first_frame_ms = first_frame.map(|d| d.as_millis() as u64),
        elapsed_ms = began.elapsed().as_millis() as u64,
        "playback stream ended"
    );
    // Running out of timeline, or being superseded or stopped, is not an
    // error; only a genuine ffmpeg failure on a stream someone is still
    // watching is reported — the preview would otherwise just go black.
    let current = !PLAYBACKS.ended(playback_id);
    PLAYBACKS.finish(playback_id);
    match result {
        Err(e) if current => {
            tracing::warn!(error = %e, "preview stream failed");
            Err(e.to_string())
        }
        Err(e) => {
            tracing::debug!(error = %e, "preview stream ended");
            Ok(())
        }
        Ok(_) => Ok(()),
    }
}

/// Stop the playback stream with this id (pause, seek, or a timeline edit).
/// A stop for a stream that has already been superseded is a no-op.
#[tauri::command(async)]
pub(crate) fn stop_playback(playback_id: u64) {
    PLAYBACKS.stop(playback_id);
}

#[tauri::command]
pub(crate) async fn get_waveform_range(
    state: State<'_, AppState>,
    asset_id: String,
    start: f64,
    end: f64,
    buckets: usize,
) -> CmdResult<WaveformRange> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve under the lock, read (and on a first call decode) with it
        // released.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        Project::decode_waveform_range(&asset, start, end, buckets).map_err(|e| e.to_string())
    })
    .await
}

/// A [`Filmstrip`] as the webview receives it: the strip's own JSON (geometry
/// only — the pixels are `#[serde(skip)]` in core) with each sheet carrying its
/// JPEG as a base64 `data:` URL. `data:` rather than a `blob:` URL because the
/// CSP admits `data:` images and nothing else; the strip is at most ~2 MB of
/// JPEG, so the ~2.7 MB string is one IPC message, not a stream.
#[derive(Serialize)]
pub(crate) struct FilmstripPayload {
    interval: f64,
    frame_width: u32,
    frame_height: u32,
    frames: u32,
    columns: u32,
    sheets: Vec<FilmstripSheetPayload>,
}

/// One sheet of a [`FilmstripPayload`]: the core sheet's own fields (flattened,
/// so a field added there reaches the webview without touching the adapter)
/// plus its pixels.
#[derive(Serialize)]
struct FilmstripSheetPayload {
    #[serde(flatten)]
    sheet: FilmstripSheet,
    /// `data:image/jpeg;base64,…` — `width` x `height` pixels, `count` thumbnails
    /// side by side from the left.
    data_url: String,
}

/// Transport only: the geometry is copied across untouched and the JPEG bytes
/// are base64'd the way `get_frame`'s are.
pub(crate) fn filmstrip_payload(strip: &Filmstrip) -> FilmstripPayload {
    FilmstripPayload {
        interval: strip.interval,
        frame_width: strip.frame_width,
        frame_height: strip.frame_height,
        frames: strip.frames,
        columns: strip.columns,
        sheets: strip
            .sheets
            .iter()
            .map(|sheet| FilmstripSheetPayload {
                data_url: format!(
                    "data:image/jpeg;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(&sheet.jpeg)
                ),
                sheet: sheet.clone(),
            })
            .collect(),
    }
}

/// An asset's **filmstrip** — the thumbnails the timeline draws a video clip
/// from: one sampled strip per asset, cached on disk by core, of which any
/// source window is a few thumbnails (`Filmstrip::frame_at` / `locate`, mirrored
/// by the webview's `filmstrip-geometry.ts`). The first call for an asset decodes
/// it; later ones are a cache read. Rejects for an asset with no video stream.
#[tauri::command]
pub(crate) async fn get_filmstrip(state: State<'_, AppState>, asset_id: String) -> CmdResult<FilmstripPayload> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Only the asset record under the lock — a database read — then drop the
        // guard before anything touches the media: `decode_filmstrip` finds the
        // asset's ready proxy (which can run an ffprobe) and, the first time, reads
        // the whole file, and the timeline asks for every clip's strip at once.
        // Same shape as `get_waveform_range`.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        let strip = Project::decode_filmstrip(&asset).map_err(|e| e.to_string())?;
        Ok(filmstrip_payload(&strip))
    })
    .await
}

/// A window of an asset's audio as raw mono s16le PCM for the preview's Web
/// Audio playback. Returns raw bytes rather than JSON — a minute of 32 kHz
/// audio is ~3.8 MB, which a JSON number array would balloon ~5×.
#[tauri::command]
pub(crate) async fn get_audio(
    state: State<'_, AppState>,
    asset_id: String,
    start: f64,
    duration: f64,
    sample_rate: Option<u32>,
    clip_id: Option<String>,
) -> CmdResult<tauri::ipc::Response> {
    let clip_id = opt_id(clip_id.as_deref(), "clip_id")?;
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    let pcm = blocking(move || {
        // Resolve the asset under the lock, then drop the guard before the decode —
        // same reasoning as `get_frame`. `clip_id` names the clip this window is
        // being fetched for, so its effect chain is baked into the decode and the
        // monitor plays what the export will render, not the dry source.
        let (asset, effects) = {
            let project = lock_user(&shared);
            let asset = project.require_asset(id).map_err(|e| e.to_string())?;
            let effects = match clip_id {
                Some(clip_id) => project
                    .timeline()
                    .ok()
                    .and_then(|tl| tl.clip(clip_id).map(|c| c.audio.clone()))
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            (asset, effects)
        };
        let rate = sample_rate.unwrap_or(32_000).clamp(8_000, 48_000);
        Project::decode_audio_pcm(&asset, start, duration, rate, &effects).map_err(|e| e.to_string())
    })
    .await?;
    Ok(tauri::ipc::Response::new(pcm))
}

#[tauri::command]
pub(crate) async fn get_energy(state: State<'_, AppState>, asset_id: String, buckets: usize) -> CmdResult<Vec<f32>> {
    let id = id(&asset_id, "asset_id")?;
    let shared = state.project.clone();
    blocking(move || {
        // Resolve under the lock, decode with it released.
        let asset = lock_user(&shared).require_asset(id).map_err(|e| e.to_string())?;
        Project::decode_energy(&asset, buckets).map_err(|e| e.to_string())
    })
    .await
}
