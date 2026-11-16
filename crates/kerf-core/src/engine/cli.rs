//! CLI-driven media engine: probing, analysis, frame/waveform extraction and
//! export by invoking the system `ffmpeg` / `ffprobe` binaries.
//!
//! Unlike [`super::ffmpeg`] (in-process libav, gated behind the `ffmpeg`
//! feature and the FFmpeg *development* libraries), everything here only needs
//! the FFmpeg *binaries* on `PATH`, so it compiles and runs in the
//! `--no-default-features` build. The binaries can be overridden with the
//! `KERF_FFMPEG` / `KERF_FFPROBE` environment variables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use super::colors::{safe_color, valid_color};
use super::cpu;
use super::decode_policy::{
    export_hwaccel_request, facts_from_assets, facts_matter, looks_like_hardware_decode_failure, pix_fmt_bits_420,
    resolve_hwaccel, tune_inputs, Chroma, Codec, HostOs, InputFacts, Reading,
};
use super::source_probe::{ProbeCache, SOURCE_PROBE_TIMEOUT};
use super::ProbeResult;
use crate::clip_timing::{
    clip_input_seek, clip_seek, clip_source_window, end_frame, is_cache_proxy, is_head_padded_proxy, picture_window, ramp,
    transition_fx, ClipFx, ClipTiming, FadeEdge, FadeStep, FadeTint, Rational, HEAD_PADDED_SUFFIX,
};
use crate::error::{Error, Result};
use crate::model::{
    pix_fmt_layout, Asset, AudioEffect, Clip, Color, Delivery, Grade, Hdr, Mask, MaskShape, PixLayout, Projection, Property,
    Reframe, ReframeKeyframe, ResolvedReframe, SalienceMap, StreamInfo, StreamKind, TextOverlay, TimeRange, Timeline, Transform,
    VideoEffect,
};
use crate::render_plan::{still_size, CompositeColorPolicy};

/// A small process-global LRU of decoded single frames. Decoded frames are a
/// pure function of (source path, time, filter, codec, quality), so caching is
/// always safe for immutable source media — and it turns scrubbing back over a
/// region, pausing, or replaying into cache hits instead of a fresh `ffmpeg`
/// spawn each time. Bounded so it never grows without limit.
struct FrameCache {
    map: HashMap<String, (u64, Vec<u8>)>,
    tick: u64,
    cap: usize,
    /// Sum of cached frame bytes, kept under [`FRAME_CACHE_MAX_BYTES`]. The
    /// entry cap alone is no memory bound — 96 full-width PNGs can run to
    /// hundreds of megabytes.
    bytes: usize,
}

/// Byte budget for the frame cache (the entry cap still applies too).
const FRAME_CACHE_MAX_BYTES: usize = 64 << 20;

impl FrameCache {
    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self.map.get_mut(key)?;
        entry.0 = tick; // mark recently used
        Some(entry.1.clone())
    }

    fn put(&mut self, key: String, value: Vec<u8>) {
        self.tick += 1;
        if let Some((_, old)) = self.map.remove(&key) {
            self.bytes -= old.len();
        }
        // Evict least-recently-used entries until both the entry and byte caps
        // hold with the new frame counted in.
        while !self.map.is_empty() && (self.map.len() >= self.cap || self.bytes + value.len() > FRAME_CACHE_MAX_BYTES) {
            if let Some(oldest) = self.map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| k.clone()) {
                if let Some((_, evicted)) = self.map.remove(&oldest) {
                    self.bytes -= evicted.len();
                }
            }
        }
        self.bytes += value.len();
        self.map.insert(key, (self.tick, value));
    }
}

fn frame_cache() -> &'static Mutex<FrameCache> {
    static CACHE: OnceLock<Mutex<FrameCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(FrameCache {
            map: HashMap::new(),
            tick: 0,
            cap: 96,
            bytes: 0,
        })
    })
}

pub(super) fn ffmpeg_bin() -> String {
    std::env::var("KERF_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string())
}

/// The `ffmpeg` binary Kerf drives (`KERF_FFMPEG`, else `ffmpeg` on `PATH`) — for
/// callers outside the engine that run it themselves, like the GPU compositor's
/// frame decoder, so they resolve it the way every other run here does.
pub fn ffmpeg_path() -> String {
    ffmpeg_bin()
}

/// A `Command` for the `ffmpeg` binary, set up like every run inside the engine
/// (on Windows `CREATE_NO_WINDOW`, so a GUI app spawning it does not flash a
/// console). Normal priority: it is for interactive reads, which should land now.
pub fn ffmpeg_command() -> Command {
    command(&ffmpeg_bin())
}

/// Apply the CPU budget's thread cap to an `ffmpeg` argv about to be spawned —
/// at spawn time, never in a builder, so the builders keep describing exactly
/// what ffmpeg is handed. `share` is how many such processes run side by side:
/// the budget is the *machine's* share for one job, so each of `share` parallel
/// decodes gets its fraction (at least one thread), even at a full budget. A lone
/// process (`share` 1) at the default 100% is untouched, which is what keeps
/// every run Kerf always issued byte-identical.
pub fn limit_ffmpeg_args(args: &mut Vec<String>, share: usize) {
    cpu::limit_args_shared(args, share);
}

pub(super) fn ffprobe_bin() -> String {
    std::env::var("KERF_FFPROBE").unwrap_or_else(|_| "ffprobe".to_string())
}

/// The decode hardware-acceleration to request for preview frames. Defaults to
/// ffmpeg's `auto` (VAAPI / CUDA / VideoToolbox, falls back to software when none
/// is usable), which offloads 4K decode off the CPU; on Windows `auto` would pick
/// DXVA2, so a run turns it into `d3d11va` per input where the codec allows
/// (`decode_policy`). Set `KERF_HWACCEL=none` (or empty) to force software
/// decoding, or a name (`cuda`, `qsv`, ...) to use exactly that.
fn hwaccel() -> Option<String> {
    match std::env::var("KERF_HWACCEL") {
        Ok(v) if v.is_empty() || v.eq_ignore_ascii_case("none") => None,
        Ok(v) => Some(v),
        Err(_) => Some("auto".to_string()),
    }
}

/// Cleared the first time an accelerated preview decode fails while the software
/// retry succeeds — i.e. `-hwaccel` is broken on this machine (a misconfigured
/// VAAPI / D3D11VA, an unsupported codec for the chosen accelerator). Once
/// cleared, later preview frames skip the accelerated attempt so a broken
/// `-hwaccel auto` doesn't double every frame's latency.
static HWACCEL_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Cleared the first time a hardware *encode* (the Insta360 stitch) fails while the
/// software retry succeeds, so a broken GPU encoder doesn't double every later
/// stitch. Proxies never encode in hardware, and an export's encoder is the user's.
static HW_ENCODE_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Set only when a failure that read as the hardware's (`looks_like_hardware_decode_failure`) was
/// cured by a software rerun of a proxy or an export, where [`HWACCEL_OK`] is cleared by *any*
/// rescue of a still, a frame grab, the stream, an analysis or a filmstrip. What an export's `auto`
/// is dropped on: one spurious preview rescue must not send every later export to software.
static HWACCEL_PROVEN_BAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn hardware decode off for the process on a conviction that read as the hardware's: the
/// preview paths' learned fallback ([`disable_decode_hwaccel`]) and the export's
/// ([`HWACCEL_PROVEN_BAD`]).
fn convict_hardware_decode() {
    HWACCEL_PROVEN_BAD.store(true, std::sync::atomic::Ordering::Relaxed);
    disable_decode_hwaccel();
}

/// The `-hwaccel` for the engine's own decodes (preview stills and streaming,
/// proxy generation, stitching, scene detection): the configured accel unless
/// an earlier fallback proved it broken on this machine. This is what is
/// *requested*; each input of a run then gets what `decode_policy` makes of it
/// (a proxy none, Windows `d3d11va` for `auto`, ...).
pub fn decode_hwaccel() -> Option<String> {
    if HWACCEL_OK.load(std::sync::atomic::Ordering::Relaxed) {
        hwaccel()
    } else {
        None
    }
}

/// What a decode that is handed a bare path knows of the file behind it: whether it is a
/// preview proxy by its name, and for anything else its codec and chroma format from the
/// cached per-file probe (`source_traits`; spawns ffprobe once per file: not under a lock).
fn path_facts(path: &Path) -> InputFacts {
    if is_cache_proxy(&path.to_string_lossy()) {
        return InputFacts {
            proxy: true,
            ..InputFacts::default()
        };
    }
    let traits = source_traits(path).unwrap_or_default();
    InputFacts {
        codec: traits.codec,
        chroma: traits.chroma,
        bits: traits.bits,
        ..InputFacts::default()
    }
}

/// [`decode_hwaccel`] resolved for the file at `path` (see `decode_policy::resolve_hwaccel`).
/// Probes the file only where what it holds can change the answer (`facts_matter`).
fn decode_hwaccel_for(path: &Path, reading: Reading) -> Option<String> {
    let requested = decode_hwaccel()?;
    let os = HostOs::CURRENT;
    let facts = if facts_matter(os, &requested, reading) {
        path_facts(path)
    } else {
        InputFacts::of_path(&path.to_string_lossy())
    };
    resolve_hwaccel(os, &requested, facts, reading)
}

/// [`decode_hwaccel`] resolved for a whole-file read of `asset`'s original, from the facts the
/// import recorded (no probe).
pub(super) fn decode_hwaccel_for_asset(asset: &Asset) -> Option<String> {
    hwaccel_for_asset(decode_hwaccel(), HostOs::CURRENT, asset)
}

/// [`decode_hwaccel_for_asset`] for a given request and operating system (pure).
fn hwaccel_for_asset(requested: Option<String>, os: HostOs, asset: &Asset) -> Option<String> {
    let facts = facts_from_assets(std::slice::from_ref(asset))(&asset.path);
    resolve_hwaccel(os, &requested?, facts, Reading::Whole)
}

/// Record that an accelerated decode failed where the software retry of the very
/// same input worked, so every later preview-class decode skips the accelerated
/// attempt (see [`decode_hwaccel`]). For background decodes living outside this
/// file that run their own retry.
pub(super) fn disable_hwaccel() {
    HWACCEL_OK.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// [`disable_hwaccel`] for a decoder that lives outside the engine (`kerf-gpu`'s
/// long-lived frame decodes): call it when a run with `-hwaccel` died before its
/// first frame and the same run in software worked. One process-wide learned
/// fallback, shared with the preview path, so a broken accelerator costs one failed
/// attempt in the whole process, not one per decoder.
pub fn disable_decode_hwaccel() {
    if HWACCEL_OK.swap(false, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!("hardware decode failed where software worked; decoding in software from now on");
    }
}

/// Whether hardware encoding may be used at all. `KERF_HW_ENCODE=none` (or
/// empty, or `0`) forces every internal encode onto the software encoders.
fn hw_encode_enabled() -> bool {
    match std::env::var("KERF_HW_ENCODE") {
        Ok(v) => !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("none")),
        Err(_) => true,
    }
}

/// Every hardware encoder that takes system-memory frames directly, in family
/// preference order (NVENC, QuickSync, VideoToolbox, AMF). VAAPI is not here:
/// its encoders only accept frames already uploaded to a VAAPI device, which the
/// software filtergraph never produces without a tail of its own
/// ([`VAAPI_ENCODER_CANDIDATES`]) — and a stitch, which has no such tail, picks
/// its encoder from this list.
const HW_ENCODER_CANDIDATES: [&str; 11] = [
    "h264_nvenc",
    "hevc_nvenc",
    "av1_nvenc",
    "h264_qsv",
    "hevc_qsv",
    "av1_qsv",
    "h264_videotoolbox",
    "hevc_videotoolbox",
    "h264_amf",
    "hevc_amf",
    "av1_amf",
];

/// The VAAPI encoders the export can drive: `-vaapi_device` plus a
/// `format=nv12,hwupload` tail on the picture pad (`build_filter_complex`).
const VAAPI_ENCODER_CANDIDATES: [&str; 3] = ["h264_vaapi", "hevc_vaapi", "av1_vaapi"];

/// The render node a VAAPI export names in its argv. The builder is pure, so this
/// is a stand-in: the process that spawns it swaps in the node the probe found
/// (`set_vaapi_device`, from `tune_export_args`).
const VAAPI_DEFAULT_DEVICE: &str = "/dev/dri/renderD128";

/// Render nodes tried for VAAPI before giving up (a laptop has two, a workstation
/// rarely more); each costs up to [`HW_PROBE_TIMEOUT`] should its driver hang.
const VAAPI_DEVICES_TRIED: usize = 4;

/// How long one candidate gets to encode a single frame.
const HW_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// How long the `-encoders` listing gets. Longer than a test encode: the first launch of a
/// freshly installed ffmpeg can sit under a virus scan, and an answer given up on is cached for
/// the whole process as "no hardware encoders".
const HW_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long an export waits for the probe to name the VAAPI device when nothing has probed yet.
const VAAPI_DEVICE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What the one-per-process hardware encoder probe found.
struct HwProbe {
    encoders: Vec<String>,
    vaapi_device: Option<String>,
}

static HW_PROBE: OnceLock<HwProbe> = OnceLock::new();

fn hw_probe() -> &'static HwProbe {
    HW_PROBE.get_or_init(detect_hw_encoders)
}

/// The value of `cell` once something has set it, waiting up to `limit` (unit-tested over a
/// local cell).
fn wait_for<T>(cell: &OnceLock<T>, limit: std::time::Duration) -> Option<&T> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = cell.get() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The hardware video encoders this machine's ffmpeg can actually use, probed
/// once per process and cached. `-encoders` listing an encoder is not proof —
/// an nvenc-enabled build without a usable NVIDIA driver still lists it and
/// then fails at open — so each compiled-in candidate is exercised with a
/// one-frame test encode and only the ones that succeed are reported. Ordered
/// by [`HW_ENCODER_CANDIDATES`], then [`VAAPI_ENCODER_CANDIDATES`].
/// `KERF_HW_ENCODE=none` reports none.
pub fn hw_encoders() -> &'static [String] {
    &hw_probe().encoders
}

/// The render node the VAAPI encoders in [`hw_encoders`] were verified on. An export that is
/// the first to ask starts the probe on its own thread and waits [`VAAPI_DEVICE_WAIT`] for it,
/// not for its worst case (a hung driver is minutes): `None` leaves the builder's stand-in node
/// in the argv, and the probe goes on for the exports after.
fn vaapi_device_within(limit: std::time::Duration) -> Option<&'static str> {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            hw_probe();
        });
    });
    wait_for(&HW_PROBE, limit)?.vaapi_device.as_deref()
}

fn detect_hw_encoders() -> HwProbe {
    let nothing = || HwProbe {
        encoders: Vec::new(),
        vaapi_device: None,
    };
    if !hw_encode_enabled() {
        return nothing();
    }
    let bin = ffmpeg_bin();
    let mut list = command(&bin);
    list.args(["-hide_banner", "-v", "error", "-encoders"]);
    let Some(listed) = run_piped_until(&mut list, Vec::new(), Instant::now() + HW_LIST_TIMEOUT) else {
        return nothing();
    };
    let listed = String::from_utf8_lossy(&listed).into_owned();
    // `-encoders` prints ` V....D h264_nvenc  NVIDIA NVENC…` — the name is
    // the second whitespace-separated token.
    let compiled: std::collections::HashSet<&str> = listed.lines().filter_map(|l| l.split_whitespace().nth(1)).collect();
    // One thread per vendor: a driver is asked about one encoder at a time (consumer
    // NVENC counts sessions), but the vendors' probes — about a second in a row — overlap.
    let (mut system, vaapi) = std::thread::scope(|scope| {
        let (bin, compiled) = (&bin, &compiled);
        let families: Vec<_> = [EncFamily::Nvenc, EncFamily::Qsv, EncFamily::VideoToolbox, EncFamily::Amf]
            .into_iter()
            .map(|family| scope.spawn(move || probe_family(bin, family, compiled, HW_PROBE_TIMEOUT)))
            .collect();
        let vaapi = scope.spawn(move || probe_vaapi(bin, compiled, &vaapi_render_nodes(), HW_PROBE_TIMEOUT));
        let system: Vec<&'static str> = families.into_iter().flat_map(|f| f.join().unwrap_or_default()).collect();
        (system, vaapi.join().unwrap_or_default())
    });
    system.sort_by_key(|enc| HW_ENCODER_CANDIDATES.iter().position(|c| c == enc));
    let (vaapi_encoders, vaapi_device) = vaapi;
    let found: Vec<String> = system.into_iter().chain(vaapi_encoders).map(str::to_string).collect();
    tracing::info!(encoders = ?found, vaapi_device = ?vaapi_device, "hardware video encoders detected");
    HwProbe {
        encoders: found,
        vaapi_device,
    }
}

/// How one test encode ended.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Works,
    Refused,
    /// Still running at [`HW_PROBE_TIMEOUT`] and killed: the driver wedged.
    Hung,
}

/// The test encode for `encoder` (pure): one 256x256 frame — enough to clear every
/// family's minimum-dimension floor — to the null muxer. System-memory encoders
/// take `nv12`, which they all accept; a VAAPI one is uploaded to `vaapi_device`
/// first, the way an export hands it frames.
fn encoder_probe_args(encoder: &str, vaapi_device: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-v", "error"].map(String::from).into();
    if let Some(device) = vaapi_device {
        args.extend(["-vaapi_device".to_string(), device.to_string()]);
    }
    args.extend(["-f", "lavfi", "-i", "color=black:s=256x256:r=30:d=0.2"].map(String::from));
    match vaapi_device {
        Some(_) => args.extend(["-vf", "format=nv12,hwupload", "-frames:v", "1"].map(String::from)),
        None => args.extend(["-frames:v", "1", "-pix_fmt", "nv12"].map(String::from)),
    }
    args.extend(["-c:v", encoder, "-f", "null", "-"].map(String::from));
    args
}

/// Run a test encode. A one-frame encode finishes in well under a second where the
/// encoder works; a driver that hangs instead of failing must not take every later
/// caller of the `OnceLock` with it, so the child is killed at `limit`
/// ([`HW_PROBE_TIMEOUT`]). A child that dies on a signal (a missing hardware library
/// aborts the pinned build) is a refusal like any other.
fn run_probe(bin: &str, args: &[String], limit: std::time::Duration) -> Probe {
    let mut probe = command(bin);
    probe.args(args).stdout(Stdio::null()).stderr(Stdio::null());
    match status_within(&mut probe, limit) {
        Ok(Some(status)) if status.success() => Probe::Works,
        Ok(None) => Probe::Hung,
        _ => Probe::Refused,
    }
}

/// The compiled-in encoders of one vendor family that really encode, in candidate
/// order. A hung one ends the family: its siblings share the driver.
fn probe_family(
    bin: &str,
    family: EncFamily,
    compiled: &std::collections::HashSet<&str>,
    limit: std::time::Duration,
) -> Vec<&'static str> {
    let mut works = Vec::new();
    for encoder in HW_ENCODER_CANDIDATES
        .iter()
        .copied()
        .filter(|e| enc_family(e) == family && compiled.contains(e))
    {
        match run_probe(bin, &encoder_probe_args(encoder, None), limit) {
            Probe::Works => works.push(encoder),
            Probe::Refused => {}
            Probe::Hung => break,
        }
    }
    works
}

/// The render nodes to try VAAPI on: `KERF_VAAPI_DEVICE` when set, else what
/// `/dev/dri` holds. A machine with no such directory (every Windows and macOS one)
/// has none, and so no VAAPI encoder.
fn vaapi_render_nodes() -> Vec<String> {
    render_nodes(Path::new("/dev/dri"), std::env::var("KERF_VAAPI_DEVICE").ok())
}

/// [`vaapi_render_nodes`] for a given directory and override (pure of the machine).
fn render_nodes(dir: &Path, forced: Option<String>) -> Vec<String> {
    if let Some(device) = forced.filter(|d| !d.is_empty()) {
        return vec![device];
    }
    let mut nodes: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
        .map(|entry| entry.path().to_string_lossy().into_owned())
        .collect();
    nodes.sort();
    nodes.truncate(VAAPI_DEVICES_TRIED);
    nodes
}

/// The VAAPI encoders that work on the first of `nodes` that has any, and that node.
/// Per node they are tried in turn and a hang gives the node up.
fn probe_vaapi(
    bin: &str,
    compiled: &std::collections::HashSet<&str>,
    nodes: &[String],
    limit: std::time::Duration,
) -> (Vec<&'static str>, Option<String>) {
    let wanted: Vec<&'static str> = VAAPI_ENCODER_CANDIDATES
        .iter()
        .copied()
        .filter(|e| compiled.contains(e))
        .collect();
    if wanted.is_empty() {
        return (Vec::new(), None);
    }
    for node in nodes {
        let mut works = Vec::new();
        for &encoder in &wanted {
            match run_probe(bin, &encoder_probe_args(encoder, Some(node)), limit) {
                Probe::Works => works.push(encoder),
                Probe::Refused => {}
                Probe::Hung => break,
            }
        }
        if !works.is_empty() {
            return (works, Some(node.clone()));
        }
    }
    (Vec::new(), None)
}

/// Run `cmd` to completion, but give up on it after `limit`: the child is killed
/// and `None` returned. stdin is closed so nothing can wait on a keypress.
///
/// For probes whose *only* job is to answer "does this work here", where a
/// driver that wedges instead of failing would otherwise take the caller with it
/// (the encoder probe runs inside a `OnceLock` every other thread waits on).
pub(super) fn status_within(cmd: &mut Command, limit: std::time::Duration) -> std::io::Result<Option<std::process::ExitStatus>> {
    let mut child = cmd.stdin(Stdio::null()).spawn()?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

pub(super) fn launch_err(bin: &str, e: std::io::Error) -> Error {
    Error::Engine(format!("failed to launch `{bin}` ({e}); is FFmpeg installed and on PATH?"))
}

/// The environment variables that make an OpenMP runtime bind its process's threads as it loads: removed from the environment of every
/// child built by [`command`], so that `av_cpu_count` in FFmpeg's process reads the affinity Kerf's plan read. Which of them bind, what
/// was measured, what is kept and why are in `engine-cli.md` ("The slice threads a graph runs on").
const OPENMP_BINDING_VARS: [&str; 4] = ["OMP_PROC_BIND", "OMP_PLACES", "GOMP_CPU_AFFINITY", "KMP_AFFINITY"];

/// Build a `Command` for an ffmpeg/ffprobe binary. On Windows this sets
/// `CREATE_NO_WINDOW` so spawning the console subprocess doesn't flash a
/// terminal window over the GUI; on other platforms it's a plain `Command`.
/// Every child the engine and the GPU compositor start is built here (and so
/// by [`ffmpeg_command`] and [`bg_command`]), which is why this is where the
/// environment is made the one the plan assumed: [`OPENMP_BINDING_VARS`] are
/// removed from the child's. The argv is not touched.
pub(super) fn command(bin: &str) -> Command {
    let mut cmd = Command::new(bin);
    for var in OPENMP_BINDING_VARS {
        cmd.env_remove(var);
    }
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = cmd;
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    };
    cmd
}

/// A `Command` for a **background** ffmpeg run: everything [`command`] does,
/// plus below-normal scheduling priority so a render or an analysis pass never
/// out-prioritizes the window the user is actually looking at. Interactive
/// decodes (a scrubbed frame, the preview stream) keep normal priority — they
/// are short, and the whole point of them is to land now.
pub(super) fn bg_command(bin: &str) -> Command {
    let mut cmd = command(bin);
    cpu::background(&mut cmd);
    cmd
}

// ---- probe -----------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ProbeJson {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    format: Option<ProbeFormat>,
}

#[derive(serde::Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

#[derive(serde::Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    /// The stream's tick, `"1/1000"` (ffprobe prints it for every stream).
    time_base: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    duration: Option<String>,
    start_time: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    color_space: Option<String>,
    color_range: Option<String>,
    pix_fmt: Option<String>,
    /// Container-level stream tags. Only `rotate` matters: FFmpeg before 6.0
    /// reported a phone's turn as this tag (clockwise) as well as in the
    /// `Display Matrix` side data (counter-clockwise).
    #[serde(default)]
    tags: Option<std::collections::HashMap<String, String>>,
    /// Stream-level side data, which is where a spherical mapping surfaces. The
    /// mov demuxer fills this from the `sv3d` box (and the legacy Google
    /// spatial-media `uuid` blob), and `-show_streams` already prints it — no
    /// extra ffprobe flag is needed. (`-export_side_data` is a *decoder* option
    /// for film grain / motion vectors and is unrelated.)
    #[serde(default)]
    side_data_list: Option<Vec<ProbeSideData>>,
}

#[derive(serde::Deserialize)]
struct ProbeSideData {
    side_data_type: Option<String>,
    projection: Option<String>,
    rotation: Option<f64>,
}

/// How long `ffprobe` may take over one file before it is killed. It reads headers, so this is a
/// file on a share that stopped answering, not a big file — but a share that is merely *slow to
/// wake* (a NAS spinning its disks up, a VPN coming back) takes a while to answer its first
/// read, and ffprobe prints nothing until it is done, so there is no progress to build a stall
/// bound on: five minutes, and a cancel ends the wait sooner.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the once-per-process capability probes (`-filters`, `-h full`, `-h filter=…`) may
/// take. They run inside a `OnceLock` every other thread waits on, and a cold first launch
/// under a virus scan is slow, so the bound is generous; a binary that does not answer reads
/// as the probe's "cannot run at all" case.
const CAPABILITY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Probe a media file via `ffprobe -of json`.
// In a full `ffmpeg` build the in-process libav probe is used instead.
#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
pub fn probe(path: &Path) -> Result<ProbeResult> {
    probe_with(path, None)
}

/// [`probe`] that gives up with [`Error::Cancelled`] once `cancel` is true (ffprobe is killed).
// In a full `ffmpeg` build the in-process libav probe is used instead.
#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
pub fn probe_cancellable(path: &Path, cancel: &dyn Fn() -> bool) -> Result<ProbeResult> {
    probe_with(path, Some(cancel))
}

#[cfg_attr(feature = "ffmpeg", allow(dead_code))]
fn probe_with(path: &Path, cancel: Option<&dyn Fn() -> bool>) -> Result<ProbeResult> {
    probe_formats(path, cancel, crate::paths::demuxer_whitelist())
}

/// The `ffprobe` arguments of a probe of `path`: `-format_whitelist` only when `formats` names some
/// (a confined run), so every other probe's argv is unchanged.
fn probe_args(path: &Path, formats: Option<&str>) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["-v", "error", "-show_format", "-show_streams", "-of", "json"]
        .into_iter()
        .map(Into::into)
        .collect();
    if let Some(formats) = formats {
        args.extend(["-format_whitelist".into(), formats.into()]);
    }
    args.push(path.into());
    args
}

/// [`probe_with`], held to the demuxers `formats` names when it names some.
fn probe_formats(path: &Path, cancel: Option<&dyn Fn() -> bool>, formats: Option<&str>) -> Result<ProbeResult> {
    let bin = ffprobe_bin();
    let mut cmd = command(&bin);
    cmd.args(probe_args(path, formats));
    let output = output_bounded_cancellable(cmd, PROBE_TIMEOUT, cancel)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "ffprobe did not answer for {} in {} s and was stopped (is the file on a share that went away?)",
                path.display(),
                PROBE_TIMEOUT.as_secs()
            )),
            _ => launch_err(&bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let parsed: ProbeJson =
        serde_json::from_slice(&output.stdout).map_err(|e| Error::Engine(format!("could not parse ffprobe output: {e}")))?;
    Ok(probe_from_json(parsed, Some(path)))
}

fn probe_from_json(parsed: ProbeJson, path: Option<&Path>) -> ProbeResult {
    let format_dur = parsed
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok());

    // A still image probes as a lone video stream in an image codec with no
    // playable duration and no audio. We mark that stream so the engine loops it
    // for the clip length on export and decodes its single frame without seeking.
    // A still has no real timeline; we treat a sub-second duration as "none" so the
    // probe is robust whether the demuxer reports N/A (ffprobe) or a single frame's
    // worth (libav). The image-codec guard keeps short *videos* from being misread.
    let video_count = parsed
        .streams
        .iter()
        .filter(|s| s.codec_type.as_deref() == Some("video"))
        .count();
    let has_audio = parsed.streams.iter().any(|s| s.codec_type.as_deref() == Some("audio"));
    let still = video_count == 1
        && !has_audio
        && format_dur.unwrap_or(0.0) < STILL_MAX_DURATION
        && parsed
            .streams
            .iter()
            .any(|s| s.codec_type.as_deref() == Some("video") && is_still_codec(s.codec_name.as_deref()));

    let secs = |t: Option<&String>| t.and_then(|t| t.trim().parse::<f64>().ok()).filter(|t| t.is_finite());
    // A video's lead is measured from the earliest picture or sound, not the container's start:
    // a subtitle or data track can start the container earlier, and the export rebases the
    // sound to its own first sample as it does the picture, so only the gap between them is
    // a lead (the libav probe measures the same).
    let av_start = parsed
        .streams
        .iter()
        .filter(|s| matches!(s.codec_type.as_deref(), Some("video" | "audio")))
        .filter_map(|s| secs(s.start_time.as_ref()))
        .reduce(f64::min);
    let mut streams = Vec::new();
    let mut max_stream_dur = 0.0_f64;
    for s in &parsed.streams {
        let kind = match s.codec_type.as_deref() {
            Some("video") => StreamKind::Video,
            Some("audio") => StreamKind::Audio,
            Some("subtitle") => StreamKind::Subtitle,
            _ => StreamKind::Data,
        };
        if let Some(d) = s.duration.as_deref().and_then(|d| d.parse::<f64>().ok()) {
            max_stream_dur = max_stream_dur.max(d);
        }
        let is_video = kind == StreamKind::Video;
        let rotation = if is_video { display_rotation(s) } else { 0 };
        let (width, height) = displayed_size(s.width, s.height, rotation);
        streams.push(StreamInfo {
            index: s.index,
            kind,
            codec: s.codec_name.clone().unwrap_or_default(),
            width,
            height,
            fps: nominal_fps(
                s.r_frame_rate.as_deref().and_then(parse_rational),
                s.avg_frame_rate.as_deref().and_then(parse_rational),
            ),
            sample_rate: s.sample_rate.as_deref().and_then(|r| r.parse().ok()),
            channels: s.channels,
            image: still && kind == StreamKind::Video,
            projection: if is_video { detect_projection(path, s) } else { None },
            rotation,
            color_transfer: if is_video {
                known_color(s.color_transfer.as_deref())
            } else {
                None
            },
            color_primaries: if is_video {
                known_color(s.color_primaries.as_deref())
            } else {
                None
            },
            pix_fmt: if is_video {
                s.pix_fmt.clone().filter(|p| !p.is_empty() && p != "none")
            } else {
                None
            },
            color_space: if is_video {
                known_color(s.color_space.as_deref())
            } else {
                None
            },
            color_range: if is_video { range_tag(s.color_range.as_deref()) } else { None },
            lead: if is_video && !still {
                video_lead(secs(s.start_time.as_ref()), av_start)
            } else {
                0.0
            },
            time_base: if is_video && !still {
                s.time_base.as_deref().and_then(parse_time_base)
            } else {
                None
            },
        });
    }
    let duration = format_dur.unwrap_or(max_stream_dur).max(0.0);
    for s in &mut streams {
        s.lead = lead_within(s.lead, duration);
    }
    ProbeResult { duration, streams }
}

/// How far a video stream is turned for display, in whole degrees
/// counter-clockwise in `(-180, 180]`, from its `Display Matrix` side data (or,
/// on an FFmpeg old enough to lack that, the clockwise `rotate` tag). 0 for an
/// unturned stream.
fn display_rotation(s: &ProbeStream) -> i16 {
    let raw = s.side_data_list.iter().flatten().find_map(|sd| sd.rotation).or_else(|| {
        let tag: f64 = s.tags.as_ref()?.get("rotate")?.trim().parse().ok()?;
        Some(-tag)
    });
    let Some(raw) = raw.filter(|r| r.is_finite()) else {
        return 0;
    };
    let mut deg = raw.round().rem_euclid(360.0) as i16;
    if deg > 180 {
        deg -= 360;
    }
    deg
}

/// The rotation a 3x3 display matrix (nine 16.16 fixed-point values in file
/// order, as libav hands them over) describes, in whole degrees counter-clockwise
/// in `(-180, 180]` — `av_display_rotation_get`, for the libav probe, which gets
/// the raw matrix where ffprobe gets the angle.
#[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
pub(crate) fn matrix_rotation(m: &[i32; 9]) -> i16 {
    let f = |i: usize| m[i] as f64 / 65536.0;
    let (s0, s1) = (f(0).hypot(f(3)), f(1).hypot(f(4)));
    if s0 == 0.0 || s1 == 0.0 {
        return 0;
    }
    let deg = -(f(1) / s1).atan2(f(0) / s0).to_degrees();
    let mut deg = deg.round().rem_euclid(360.0) as i16;
    if deg > 180 {
        deg -= 360;
    }
    deg
}

/// The size a stream is *shown* at: the coded size, swapped when the stream is
/// turned a quarter. Every FFmpeg decode autorotates, so this is the size of the
/// pixels the engine actually receives — the one every piece of frame geometry
/// (the project frame, fit, crop, smart crop) has to be done against.
pub(crate) fn displayed_size(width: Option<u32>, height: Option<u32>, rotation: i16) -> (Option<u32>, Option<u32>) {
    if rotation.rem_euclid(180) == 90 {
        (height, width)
    } else {
        (width, height)
    }
}

/// A colour tag as the engine stores it: `None` when the file does not say,
/// rather than ffprobe's literal `"unknown"` — or `"reserved"`, a value no matrix has, which
/// FFmpeg reads as it reads none.
pub(crate) fn known_color(tag: Option<&str>) -> Option<String> {
    tag.filter(|t| !t.is_empty() && !matches!(*t, "unknown" | "unspecified" | "reserved"))
        .map(str::to_string)
}

/// A range tag as the engine stores it: `"pc"` (full) or `"tv"` (limited), `None` when the file does
/// not say (ffprobe prints `unknown`, or nothing). ffprobe's own spellings are `tv` / `pc`; the
/// libav names (`mpeg` / `jpeg`) and the plain words are folded into them.
pub(crate) fn range_tag(tag: Option<&str>) -> Option<String> {
    match tag?.trim().to_ascii_lowercase().as_str() {
        "pc" | "jpeg" | "full" => Some("pc".to_string()),
        "tv" | "mpeg" | "limited" => Some("tv".to_string()),
        _ => None,
    }
}

/// The frame rate to build a project around. `r_frame_rate` is "the lowest rate
/// all timestamps are a multiple of", which for a variable-frame-rate phone clip
/// can be a multiple of the real one (a ~30 fps clip with jittery timestamps
/// probes as 120) and would turn the whole export into a 4x-duplicated render.
/// When it runs far past the stream's average the average, snapped to the
/// nearest standard rate, is the nominal rate; otherwise `r_frame_rate` stands,
/// since a clip that merely dropped a few frames still belongs to its nominal rate.
pub(crate) fn nominal_fps(r_frame_rate: Option<f64>, avg_frame_rate: Option<f64>) -> Option<f64> {
    const STANDARD: [f64; 12] = [23.976, 24.0, 25.0, 29.97, 30.0, 48.0, 50.0, 59.94, 60.0, 100.0, 119.88, 120.0];
    let r = r_frame_rate.filter(|r| *r > 0.0);
    let avg = avg_frame_rate.filter(|a| *a > 0.0);
    match (r, avg) {
        (Some(r), Some(avg)) if r > avg * 1.5 => Some(
            STANDARD
                .iter()
                .copied()
                .filter(|s| (s - avg).abs() / s < 0.03)
                .min_by(|a, b| (a - avg).abs().total_cmp(&(b - avg).abs()))
                .unwrap_or(avg),
        ),
        (Some(r), _) => Some(r),
        (None, avg) => avg,
    }
}

/// FFmpeg `codec_name`s for single-frame still images. Animated containers
/// (gif/webp/apng) only land here when they probe with no playable duration —
/// i.e. they really are one frame; an animated one keeps a real duration and is
/// treated as ordinary video.
/// A probed duration below this (seconds) counts as "no real duration" when
/// deciding whether an image-codec stream is a still vs. an animated/looping one.
pub(crate) const STILL_MAX_DURATION: f64 = 1.0;

pub(crate) fn is_still_codec(codec: Option<&str>) -> bool {
    matches!(
        codec,
        Some(
            "png"
                | "mjpeg"
                | "jpeg"
                | "jpegls"
                | "bmp"
                | "gif"
                | "webp"
                | "tiff"
                | "ppm"
                | "pgm"
                | "pgmyuv"
                | "pam"
                | "targa"
                | "tga"
                | "qoi"
                | "apng"
                | "jpeg2000"
                | "j2k"
                | "heif"
                | "heic"
        )
    )
}

/// Decide whether a probed video stream is 360 footage, and in which projection.
///
/// Two signals, strongest first:
///
/// 1. A `Spherical Mapping` side-data entry declaring an equirectangular
///    projection. This is authoritative — it is what a stitched export (Insta360
///    Studio, the Google spatial-media injector, YouTube-ready files) writes.
/// 2. An Insta360 `.insv` whose frame is two squares side by side — the raw
///    dual-fisheye shape (a 5.7K capture probes as 5760x2880, i.e. two 2880x2880
///    circular hemispheres).
///
/// Deliberately **no** bare aspect-ratio guess. A 2:1 frame is a real and common
/// shape for anamorphic masters, ultrawide edits and ordinary stitched panoramas,
/// and the costs are lopsided: a missed detection costs one click in the
/// Inspector, whereas a false positive silently reprojects ordinary footage and
/// changes the export resolution out from under the user. Everything past these
/// two signals is a manual override.
fn detect_projection(path: Option<&Path>, s: &ProbeStream) -> Option<Projection> {
    if let Some(list) = &s.side_data_list {
        for sd in list {
            if sd.side_data_type.as_deref() == Some("Spherical Mapping") {
                return spherical_side_data_projection(sd.projection.as_deref());
            }
        }
    }
    projection_from_shape(path, s.width, s.height)
}

/// Signal 1: map the `projection` field of a `Spherical Mapping` side-data entry.
/// `None` for the field covers older ffmpeg builds that name the side data but not
/// its projection — equirect is the only mono-360 projection Insta360 and the
/// spatial-media spec actually emit. Cubemap / mesh projections exist but are not
/// something we can reframe faithfully, so they stay flat rather than being
/// reprojected wrongly.
pub(crate) fn spherical_side_data_projection(projection: Option<&str>) -> Option<Projection> {
    match projection {
        Some("equirectangular") | Some("half equirectangular") | None => Some(Projection::Equirect),
        _ => None,
    }
}

/// Signal 2: an Insta360 capture (`.insv` / `.insp`) whose frame is exactly two
/// squares side by side — the raw dual-fisheye packing (a 5.7K capture probes as
/// 5760x2880, i.e. two 2880x2880 circular hemispheres). Shared with the libav
/// probe backend so both agree on the geometry rule.
pub(crate) fn projection_from_shape(path: Option<&Path>, width: Option<u32>, height: Option<u32>) -> Option<Projection> {
    let insta360 = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("insv") || e.eq_ignore_ascii_case("insp"));
    match (insta360, width, height) {
        (true, Some(w), Some(h)) if h > 0 && h.checked_mul(2) == Some(w) => Some(Projection::DualFisheye),
        _ => None,
    }
}

/// A stream's tick as ffprobe prints it (`"1/1000"`): both terms whole and above zero, else not known.
fn parse_time_base(s: &str) -> Option<(u32, u32)> {
    let (num, den) = s.split_once('/')?;
    time_base_of(num.trim().parse().ok()?, den.trim().parse().ok()?)
}

/// A stream's tick from the two terms a probe read (ffprobe's text, libav's `Rational`): both whole, above zero and in
/// range, else not known. The ffprobe and the libav probes share it.
pub(crate) fn time_base_of(num: i64, den: i64) -> Option<(u32, u32)> {
    let (num, den) = (u32::try_from(num).ok()?, u32::try_from(den).ok()?);
    (num > 0 && den > 0).then_some((num, den))
}

/// Parse an FFmpeg rational like `"30000/1001"` into an `f64`.
fn parse_rational(s: &str) -> Option<f64> {
    let (num, den) = s.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

// ---- HDR → SDR -------------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// What [`zscale_available`] answers on this thread while a test pins it.
    static ZSCALE_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg did (`true`) or did not (`false`) have `zscale`.
#[cfg(test)]
fn with_zscale<R>(available: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ZSCALE_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(ZSCALE_OVERRIDE.with(|c| c.replace(Some(available))));
    f()
}

/// Whether this ffmpeg can tone-map with `zscale` + `tonemap`, probed once per
/// process. `zscale` needs libzimg, which a minimal or hand-built ffmpeg often
/// lacks; `tonemap` itself is part of every build.
fn zscale_available() -> bool {
    // Tests pin the answer so an argv oracle does not depend on the ffmpeg
    // installed where it runs (see `golden`).
    #[cfg(test)]
    if let Some(forced) = ZSCALE_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let bin = ffmpeg_bin();
        let ok = output_bounded(
            command(&bin).args(["-hide_banner", "-loglevel", "quiet", "-filters"]),
            CAPABILITY_PROBE_TIMEOUT,
            0,
        )
        .map(|o| {
            let list = String::from_utf8_lossy(&o.stdout);
            let has = |name: &str| list.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
            has("zscale") && has("tonemap")
        })
        // A binary that will not even run renders nothing either way; only one
        // that runs and lacks the filters gets the fallback. It also keeps the
        // graph builders' output from depending on whether a test machine has
        // ffmpeg installed.
        .unwrap_or(true);
        tracing::debug!(available = ok, "probed ffmpeg for zscale + tonemap");
        ok
    })
}

// ---- the composite's colour policy ---------------------------------------------

/// How this ffmpeg picks the matrix of a composite — see [`CompositeColorPolicy`].
/// Probed by **running the still graph** twice on the same picture, once in a
/// clip tagged BT.709 and once in an untagged one, and seeing whether the
/// finished composites differ: FFmpeg 6 hands the encoder an untagged frame
/// (BT.601 whatever the clip was, so the two agree); FFmpeg 9 negotiates the
/// colourspace across the overlay chain, so the tagged clip's composite is
/// converted as BT.709 and the two differ. Measured, not read off a version
/// string — and the probe checks that the tag **survived** into the clip it
/// measures (an `ffprobe` of it), because a tag that was lost on the way would
/// read as "FFmpeg 6" on any build.
///
/// **A probe that could not tell is [`CompositeColorPolicy::Unknown`], not a
/// guess.** FFmpeg 6 and 9 disagree precisely on BT.709 and BT.2020 footage, so
/// no answer is safe for it: `Unknown` mirrors only the stacks both agree on
/// (all BT.601-class) and refuses the rest. It is not remembered — a measured
/// policy is kept for the process, a failed probe is retried on a later call, at
/// most once per backoff period (5 s, doubling to 5 min) so a broken ffmpeg is
/// not respawned for every frame. The whole probe is bounded
/// ([`POLICY_PROBE_TIMEOUT`]): a hung ffmpeg cannot hold a caller for longer.
///
/// **The first call blocks** — 70 to 200 ms where ffmpeg works (six short runs,
/// two at a time; 70 ms measured on a static build, 200 on a distro one) and up
/// to the timeout where it does not — and concurrent first callers wait behind it,
/// so call it from a blocking thread, not an async task or the UI's. Later calls
/// are a lock and a read.
pub fn composite_color_policy() -> CompositeColorPolicy {
    static PROBE: Mutex<PolicyProbe> = Mutex::new(PolicyProbe::new());
    cached_policy(&PROBE, Instant::now(), || {
        measure_composite_color_policy(&ffmpeg_bin(), &ffprobe_bin(), POLICY_PROBE_TIMEOUT)
    })
}

/// The longest the whole policy probe may take. It is six short runs of a
/// 64x64 clip, anything near this is a wedged ffmpeg, and every other caller of
/// [`composite_color_policy`] waits behind it.
const POLICY_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What the process knows about its ffmpeg's composite colour policy.
struct PolicyProbe {
    /// The measured policy, once there is one: kept for good.
    measured: Option<CompositeColorPolicy>,
    /// Failed probes in a row, and the earliest time the next one may run.
    failures: u32,
    retry_at: Option<Instant>,
}

impl PolicyProbe {
    const fn new() -> Self {
        Self {
            measured: None,
            failures: 0,
            retry_at: None,
        }
    }
}

/// The wait before the probe may run again after `failures` failures in a row:
/// 5 s, doubling, capped at 5 minutes.
fn policy_probe_backoff(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300))
}

/// The policy in `state`, measuring it with `measure` if nothing is known yet and
/// the backoff after the last failure has passed. Holds the lock while it
/// measures, so concurrent first callers wait for one probe rather than each
/// running their own; the probe is bounded, so the wait is.
fn cached_policy(
    state: &Mutex<PolicyProbe>,
    now: Instant,
    measure: impl FnOnce() -> Option<CompositeColorPolicy>,
) -> CompositeColorPolicy {
    let mut s = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(policy) = s.measured {
        return policy;
    }
    if s.retry_at.is_some_and(|t| now < t) {
        return CompositeColorPolicy::Unknown;
    }
    match measure() {
        Some(policy) => {
            tracing::debug!(?policy, "probed ffmpeg's composite colour policy");
            s.measured = Some(policy);
            policy
        }
        None => {
            s.failures += 1;
            let wait = policy_probe_backoff(s.failures);
            s.retry_at = Some(Instant::now() + wait);
            tracing::warn!(
                failures = s.failures,
                retry_in_secs = wait.as_secs(),
                "could not measure ffmpeg's composite colour policy; only BT.601-class stacks are drawn on the GPU until it can"
            );
            CompositeColorPolicy::Unknown
        }
    }
}

/// The probe picture: a flat YCbCr whose red the two matrices convert to values
/// well apart (about 20 levels on FFmpeg's converter).
const POLICY_PROBE_YUV: (u8, u8, u8) = (60, 128, 230);

/// Which policy two measured reds say: the same red from the tagged and the
/// untagged clip means the tag did not reach the conversion. A difference between
/// the two readings is not a result.
fn policy_from_reds(tagged: u8, untagged: u8) -> Option<CompositeColorPolicy> {
    match tagged.abs_diff(untagged) {
        0..=3 => Some(CompositeColorPolicy::FixedBt601),
        10.. => Some(CompositeColorPolicy::BottomLayerTag),
        _ => None,
    }
}

/// Measure the policy of `ffmpeg` (with `ffprobe` to check the probe clips), or
/// `None` when it cannot be read: a binary that will not run, a run that fails or
/// outlasts `timeout`, a tag that did not survive, readings that do not decide.
fn measure_composite_color_policy(ffmpeg: &str, ffprobe: &str, timeout: std::time::Duration) -> Option<CompositeColorPolicy> {
    let deadline = Instant::now() + timeout;
    // The two clips are independent, so they are measured side by side.
    let (tagged, untagged) = std::thread::scope(|scope| {
        let tagged = scope.spawn(|| probe_composite_red(ffmpeg, ffprobe, true, deadline));
        let untagged = probe_composite_red(ffmpeg, ffprobe, false, deadline);
        (tagged.join().ok().flatten(), untagged)
    });
    let (tagged, untagged) = (tagged?, untagged?);
    tracing::debug!(tagged, untagged, "composite colour probe");
    policy_from_reds(tagged, untagged)
}

/// Run `cmd` with `input` on its stdin and return its stdout if it exits
/// successfully before `deadline` — killed, and `None`, if it does not. The pipes
/// are served from side threads, so a child that stops reading or never exits
/// cannot wedge the caller. **Nothing here joins a thread that a child can keep
/// alive**: a wrapper script that spawned the real ffmpeg keeps the pipes open
/// past the kill (and past its own exit), so on a timeout the side threads are
/// left to finish on their own, and after a clean exit the output is waited
/// for no longer than the deadline (a short grace past it) either.
pub fn run_piped_until(cmd: &mut Command, input: Vec<u8>, deadline: Instant) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    // The input is a few kilobytes and the child may close the pipe before reading
    // it all, which is not the feeder's problem. (Not joined: a descendant that holds
    // the pipe open and does not read keeps a long input's write blocked.)
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let (sent, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        let _ = sent.send(out);
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(4)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // The child has exited, so the pipe is at its end unless a descendant of it holds the other side: the output is
    // there already, and a descendant that outlives the deadline is not waited for.
    let wait = deadline
        .saturating_duration_since(Instant::now())
        .max(std::time::Duration::from_millis(250));
    let out = output.recv_timeout(wait).ok()?;
    status.success().then_some(out)
}

/// How long a whole-file read ([`scene_pass`], [`contact_sheet`]) may go without a word from
/// ffmpeg before it is killed, at the least (the same floor the waveform decode and the
/// filmstrip give theirs). `-progress` reports every [`PROGRESS_ON_STDERR`] second, but **only
/// once a frame has reached the output**: a read that outputs nothing until its last frame is
/// silent for the whole decode, so it gets either a frame to report on early (the scene pass
/// selects frame 0) or a bound that grows with the work ([`stall_for_work`], the sheet's).
pub(super) const READ_STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// The most a read that is silent until it ends is given before it counts as wedged.
const READ_STALL_CEILING: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

/// Silence a read of `work_secs` of footage that outputs only at its end is allowed (pure): `per_sec`
/// per second of footage — a decode slower than `1 / per_sec` of real time is a wedge, not a slow
/// machine — between `floor`, which covers start-up and the first seek, and `ceiling`, which keeps
/// even a wedged hour-long read finite. Clamped in seconds first: `from_secs_f64` panics on
/// infinity, and a container can report an absurd duration.
fn stall_for_work(work_secs: f64, per_sec: f64, floor: std::time::Duration, ceiling: std::time::Duration) -> std::time::Duration {
    let secs = if work_secs.is_finite() {
        (work_secs * per_sec).clamp(0.0, ceiling.as_secs_f64())
    } else {
        0.0
    };
    std::time::Duration::from_secs_f64(secs).max(floor)
}

/// The stall bound of a contact sheet of a `window_secs` window: the whole window is decoded
/// before `tile` outputs the one frame, so the read says nothing until it ends. Thirty seconds of
/// silence per second of footage, at least [`READ_STALL`], at most [`READ_STALL_CEILING`].
fn sheet_stall(window_secs: f64) -> std::time::Duration {
    stall_for_work(window_secs, 30.0, READ_STALL, READ_STALL_CEILING)
}

/// The `-progress` flags a bounded read adds **at spawn time** (like the thread caps, never in
/// a pure builder): the report goes to stderr, which stdout-carrying reads (a contact sheet's
/// JPEG) leave free, and is what [`output_unless_stalled`] listens for.
pub(super) const PROGRESS_ON_STDERR: [&str; 4] = ["-progress", "pipe:2", "-stats_period", "1"];

/// Whether `line` is one of the `key=value` lines of ffmpeg's `-progress` report.
fn is_progress_line(line: &str) -> bool {
    let Some((key, _)) = line.trim().split_once('=') else {
        return false;
    };
    key.starts_with("stream_")
        || matches!(
            key,
            "frame"
                | "fps"
                | "bitrate"
                | "total_size"
                | "out_time_us"
                | "out_time_ms"
                | "out_time"
                | "dup_frames"
                | "drop_frames"
                | "speed"
                | "progress"
        )
}

/// What [`output_unless_stalled`] collected.
pub(super) struct Collected {
    pub(super) status: std::process::ExitStatus,
    pub(super) stdout: Vec<u8>,
    /// stderr without the `-progress` report lines (what had arrived, when the read stalled).
    pub(super) stderr: String,
    /// ffmpeg said nothing for the stall bound and was killed.
    pub(super) stalled: bool,
    /// The caller's cancel tripped and ffmpeg was killed ([`output_supervised`]).
    pub(super) cancelled: bool,
    /// The run outlasted its `total` bound and was killed.
    pub(super) timed_out: bool,
}

impl Collected {
    pub(super) fn succeeded(&self) -> bool {
        self.status.success() && !self.stalled && !self.cancelled && !self.timed_out
    }

    /// The last lines of stderr, for an error message.
    pub(super) fn stderr_tail(&self) -> String {
        let lines: Vec<&str> = self.stderr.lines().rev().take(12).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n").trim().to_string()
    }
}

/// [`Command::output`] that cannot wait forever: `cmd` is given `-progress pipe:2`
/// ([`PROGRESS_ON_STDERR`]) and is killed once its stderr has been silent for `stall`. Both
/// pipes are drained on side threads, so neither a flood of warnings nor a large stdout can fill
/// a pipe and wedge the child. A stalled read returns the stderr that had arrived and no stdout.
/// The child is always reaped, whatever fails on the way.
#[cfg(test)]
fn output_unless_stalled(cmd: Command, stall: std::time::Duration) -> std::io::Result<Collected> {
    output_supervised(cmd, stall, None, &|| false)
}

/// [`output_unless_stalled`] for a job someone can stop: `cancel` is polled about fifty times a
/// second and, once true, kills the child and returns what had arrived with `cancelled` set.
/// The caller maps that to [`Error::Cancelled`] (it is not a failure to report). `total`, when
/// given, is a wall-clock bound on the whole run (`timed_out`) for the reads that print no
/// `-progress` (pass `Duration::MAX` as `stall` for those: stderr silence then means nothing).
pub(super) fn output_supervised(
    cmd: Command,
    stall: std::time::Duration,
    total: Option<std::time::Duration>,
    cancel: &dyn Fn() -> bool,
) -> std::io::Result<Collected> {
    output_supervised_sized(cmd, stall, total, cancel, 0)
}

/// [`output_supervised`] that reads stdout into a buffer of `capacity` bytes allocated up front,
/// for a read whose size is known (the scopes' raw frames), instead of doubling through it.
pub(super) fn output_supervised_sized(
    mut cmd: Command,
    stall: std::time::Duration,
    total: Option<std::time::Duration>,
    cancel: &dyn Fn() -> bool,
    capacity: usize,
) -> std::io::Result<Collected> {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::{Arc, Mutex};

    /// When stderr was last heard from, and what it said so far.
    struct Heard {
        at: Instant,
        text: String,
    }

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let heard = Arc::new(Mutex::new(Heard {
        at: Instant::now(),
        text: String::new(),
    }));
    let out_handle = std::thread::spawn(move || {
        let mut bytes = Vec::with_capacity(capacity);
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err_handle = {
        let heard = Arc::clone(&heard);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).split(b'\n') {
                let Ok(line) = line else { break };
                let line = String::from_utf8_lossy(&line);
                if let Ok(mut heard) = heard.lock() {
                    heard.at = Instant::now();
                    if !is_progress_line(&line) {
                        heard.text.push_str(&line);
                        heard.text.push('\n');
                    }
                }
            }
        })
    };
    let started = Instant::now();
    let (mut stalled, mut cancelled, mut timed_out) = (false, false, false);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
        if cancel() {
            cancelled = true;
            let _ = child.kill();
            break child.wait()?;
        }
        if total.is_some_and(|total| started.elapsed() > total) {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        if heard.lock().is_ok_and(|heard| heard.at.elapsed() > stall) {
            stalled = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let text = |heard: &Mutex<Heard>| heard.lock().map(|heard| heard.text.clone()).unwrap_or_default();
    if stalled || cancelled || timed_out {
        // The side threads end on their own when the pipes close; a wrapper that spawned the real
        // ffmpeg keeps them open past the kill, and the caller must not wait for that.
        return Ok(Collected {
            status,
            stdout: Vec::new(),
            stderr: text(&heard),
            stalled,
            cancelled,
            timed_out,
        });
    }
    let stdout = out_handle.join().unwrap_or_default();
    let _ = err_handle.join();
    Ok(Collected {
        status,
        stdout,
        stderr: text(&heard),
        stalled,
        cancelled,
        timed_out,
    })
}

/// How long a child that has closed its pipes (or been killed) is given to exit before it is
/// killed: ffmpeg flushes a muxer on the way out, which on a dying network share can block.
const REAP_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Wait for `child` to exit — for [`REAP_GRACE`] at most, then kill it. The bounded `wait()`:
/// every place that reads a child's pipes to the end and then waits on it would otherwise hang
/// for good on one that closed stdout and never exited.
pub(super) fn reap(child: &mut std::process::Child) -> Result<std::process::ExitStatus> {
    reap_within(child, REAP_GRACE)
}

fn reap_within(child: &mut std::process::Child, grace: std::time::Duration) -> Result<std::process::ExitStatus> {
    let wait_failed = |e: std::io::Error| Error::Engine(format!("ffmpeg wait failed: {e}"));
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                return child.wait().map_err(wait_failed);
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(2)),
            Err(e) => return Err(wait_failed(e)),
        }
    }
}

/// The red of the middle pixel of the still graph's composite over a one-frame
/// clip of [`POLICY_PROBE_YUV`], with the clip tagged BT.709 or not.
fn probe_composite_red(ffmpeg: &str, ffprobe: &str, tag_bt709: bool, deadline: Instant) -> Option<u8> {
    let (y, u, v) = POLICY_PROBE_YUV;
    // 1. A one-frame clip of exactly that picture (raw planes, so no generator gets
    // to round it), tagged the way a camera's file is, or not.
    let mut raw = vec![y; 64 * 64];
    raw.extend(std::iter::repeat_n(u, 32 * 32));
    raw.extend(std::iter::repeat_n(v, 32 * 32));
    // The tag goes on the *input* (a decoder option): put on the output of a
    // raw-video encode it would make FFmpeg 9 convert the picture into the new
    // matrix on the way, and the two clips would no longer hold the same planes.
    let mut maker = command(ffmpeg);
    maker.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        "64x64",
    ]);
    if tag_bt709 {
        maker.args(["-colorspace", "bt709"]);
    }
    maker.args([
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
        "-c:v",
        "ffv1",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "matroska",
        "pipe:1",
    ]);
    let clip = run_piped_until(&mut maker, raw, deadline).filter(|c| !c.is_empty())?;

    // 2. The tag must have survived into the clip, as the probe the app imports
    // with reads it: a clip that lost it (a wrapper, a build that drops the
    // option) would make the two measurements identical on *any* FFmpeg, and the
    // probe would read "FFmpeg 6" off an FFmpeg 9.
    let mut checker = command(ffprobe);
    checker.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=color_space",
        "-of",
        "csv=p=0",
        "-i",
        "pipe:0",
    ]);
    let found = run_piped_until(&mut checker, clip.clone(), deadline)?;
    let found = String::from_utf8_lossy(&found);
    let found = found.trim();
    let survived = if tag_bt709 {
        found == "bt709"
    } else {
        !matches!(found, "bt709" | "bt2020nc" | "bt2020c")
    };
    if !survived {
        tracing::debug!(found, tag_bt709, "the probe clip's colour tag is not what was written");
        return None;
    }

    // 3. The still graph of a one-clip timeline over that clip, built by the very
    // builder the stills use, ending in raw RGB.
    let asset = Asset {
        id: uuid::Uuid::new_v4(),
        path: "pipe:0".to_string(),
        name: "probe".to_string(),
        duration: 1.0,
        streams: vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "ffv1".to_string(),
            width: Some(64),
            height: Some(64),
            fps: Some(1.0),
            sample_rate: None,
            channels: None,
            image: false,
            projection: None,
            rotation: 0,
            color_transfer: None,
            color_primaries: None,
            pix_fmt: Some("yuv420p".to_string()),
            color_space: tag_bt709.then(|| "bt709".to_string()),
            color_range: None,
            lead: 0.0,
            time_base: None,
        }],
        imported_at: chrono::Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let timeline = Timeline {
        tracks: vec![crate::model::Track {
            clips: vec![Clip::new(asset.id, 0.0, 1.0, 0.0)],
            ..crate::model::Track::new(StreamKind::Video, "V1")
        }],
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let args = build_still_args(
        &timeline,
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        0.0,
        64,
        None,
        &StillOutput::RgbPipe,
    )
    .ok()?;
    let mut render = command(ffmpeg);
    render.args(&args);
    let out = run_piped_until(&mut render, clip, deadline)?;
    if out.len() != 64 * 64 * 3 {
        return None;
    }
    Some(out[(32 * 64 + 32) * 3])
}

/// The filter chain that turns decoded HDR frames into 8-bit SDR BT.709 —
/// pure, so it is unit-tested, with the filter probe's answer passed in.
///
/// The `zscale` path is the standard one: light-linear float RGB in the BT.709
/// gamut, a **mobius** roll-off (linear up to 70% so the midtones — most of a
/// picture — are left alone, then a smooth shoulder rather than `clip`'s hard
/// edge or `hable`'s global contrast change), and back to BT.709 with error
/// diffusion so a sky does not band. The input's transfer, primaries and matrix
/// are stated rather than read from the frame, because a phone file with its
/// colour tags stripped would otherwise abort the whole graph with "no path
/// between colorspaces". `npl=100` makes light-linear 1.0 an SDR white. The
/// output frames are tagged BT.709, so an encoder downstream writes an honest
/// SDR file.
///
/// Without `zscale` the fallback is `colorspace`, which moves the BT.2020
/// primaries and matrix into BT.709 but cannot know either HDR curve. That is
/// near-right for HLG — designed to read acceptably on an SDR display — and
/// only approximate for PQ, which is why it is the fallback.
pub(crate) fn tonemap_chain(hdr: Hdr, zscale: bool) -> String {
    if zscale {
        format!(
            "zscale=tin={}:pin=bt2020:min=bt2020nc:t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,\
             tonemap=tonemap=mobius:param=0.7:desat=0,\
             zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p",
            hdr.zscale_name()
        )
    } else {
        "colorspace=all=bt709:iall=bt2020:fast=1,format=yuv420p".to_string()
    }
}

/// [`tonemap_chain`] for this machine's ffmpeg.
pub(super) fn tonemap_filter(hdr: Hdr) -> String {
    tonemap_chain(hdr, zscale_available())
}

/// The filter that labels a picture BT.709 without touching a pixel (`setparams`), for an untagged
/// HD input that shares a graph with one naming a matrix ([`crate::model::bt709_labels`]). Not the
/// `-colorspace` output option, which FFmpeg 9 takes as an order to convert into that matrix on the
/// way (up to 22 levels moved, measured).
const BT709_LABEL: &str = "setparams=colorspace=bt709";

/// What the decodes that are handed a bare path rather than an [`Asset`] need to
/// know about a file's first video stream, from one `ffprobe`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SourceTraits {
    /// The HDR transfer, if the stream is HLG or PQ.
    pub hdr: Option<Hdr>,
    /// Seconds between the container's start and the video's first frame (see
    /// [`head_lead`]); `0.0` for an ordinary file.
    pub lead: f64,
    /// The container is an MP4/MOV or Matroska/WebM file: a per-packet timestamp of its own
    /// and a seek index. Transport streams, AVI, program streams and bare elementary streams
    /// are not (their timestamps are measured or guessed and their seeks land on a later
    /// keyframe), and neither is a file whose format the probe did not name.
    pub indexed: bool,
    /// The stream is flagged full range (`color_range=pc`): what the file says, which an asset probed before the
    /// flag was recorded does not (`PlanStream::color_range`, `None`).
    pub full_range: bool,
    /// The probe's answer described a file (JSON with a `streams` list and a `format`): without that every other field is
    /// the neutral value of an ordinary file, not something the file said (`ffprobe` can exit 0 and print nothing usable). A
    /// file that has no video stream is described all the same (its `streams` list is empty).
    pub described: bool,
    /// The stream's codec and chroma format, for the one thing a decode that is handed a bare
    /// path has to ask of them: whether to request a hardware decoder (`decode_policy`).
    pub codec: Codec,
    pub chroma: Chroma,
    /// The bit depth of a 4:2:0 stream.
    pub bits: Option<u8>,
    /// The first plane of the decoded picture is the luma alone (see [`luma_plane_pix_fmt`]):
    /// what a yuv420p proxy's first plane is, and the only plane `select='scene'` scores.
    pub luma_plane: bool,
    /// The stream's tick (`stream.time_base`, `(numerator, denominator)` seconds); `None` when the probe did not print one.
    pub time_base: Option<(u32, u32)>,
}

/// Whether a picture of this `pix_fmt` keeps its luma alone in plane 0: planar or semi-planar
/// YCbCr of any subsampling or depth, and gray. `select='scene'` scores plane 0 only, so on an
/// RGB-family picture (`gbrp`: the G plane; `rgb24`, `bgr0`: every interleaved byte), a packed
/// YCbCr one (`yuyv422`: luma and chroma bytes interleaved), a palette, an alpha-carrying
/// format or a name nobody here knows, it scores something a yuv420p proxy cannot reproduce.
fn luma_plane_pix_fmt(name: &str) -> bool {
    let packed = matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "uyvy422" | "yuyv422" | "yvyu422" | "uyyvyy411"
    );
    !packed
        && matches!(
            pix_fmt_layout(name.trim()),
            Some(PixLayout::Yuv420 | PixLayout::Gray | PixLayout::OtherYuv)
        )
}

/// [`SourceTraits`] of the file at `path`, probed once per file and cached against the file's
/// size and modified time. One `ffprobe`, killed after [`SOURCE_PROBE_TIMEOUT`]; `None` when the
/// probe itself fails, outlasts it or exits 0 with an answer that describes no file (nothing usable on its
/// output), and the failure is remembered for a minute (a broken or hung `ffprobe` is not respawned by every
/// frame that asks), then retried.
///
/// It spawns a process: not under a lock.
pub(crate) fn source_traits(path: &Path) -> Option<SourceTraits> {
    source_traits_cache().get(path, || probe_source_traits(&ffprobe_bin(), path, SOURCE_PROBE_TIMEOUT))
}

/// Whether the first video stream of the file at `path` is flagged full range, **as the file says** (one cached `ffprobe`,
/// [`source_traits`]): what a plan that was made from an asset probed before the flag was recorded cannot know. `None`
/// when the probe could not tell: it failed, outlasted its timeout, or exited 0 with an answer that describes no file (nothing
/// usable on its output). A file the probe read that has no video stream is not full range: there is no picture to be. It
/// spawns a process: not under a lock.
pub fn source_is_full_range(path: &Path) -> Option<bool> {
    full_range_of(source_traits(path)?)
}

/// [`source_is_full_range`] **from what is already known**: the answer a probe of this file left (this one's, or any
/// other read of the file's [`SourceTraits`]: the HDR, the lead, the container), `None` while there is none (not asked,
/// being asked, failed, or no video stream). It never spawns and never waits, so a render may ask it; whoever
/// wants it answered asks [`source_is_full_range`] ahead, off the render's path.
pub fn source_is_full_range_known(path: &Path) -> Option<bool> {
    full_range_of(source_traits_known(path)?)
}

fn full_range_of(traits: SourceTraits) -> Option<bool> {
    traits.described.then_some(traits.full_range)
}

/// Make the cache hold `full_range` for `path` (a test seeds what a probe of a file would have left).
#[cfg(test)]
pub(crate) fn seed_source_range(path: &Path, full_range: bool) {
    let traits = SourceTraits {
        described: true,
        full_range,
        ..SourceTraits::default()
    };
    let _ = source_traits_cache().get(path, || Some(traits));
}

/// Make the cache hold a probe of `path` that found `time_base` (`None`: one that printed no tick), as one of the file would
/// have left.
#[cfg(test)]
pub(crate) fn seed_source_time_base(path: &Path, time_base: Option<(u32, u32)>) {
    let traits = SourceTraits {
        described: true,
        time_base,
        ..SourceTraits::default()
    };
    let _ = source_traits_cache().get(path, || Some(traits));
}

fn source_traits_cache() -> &'static ProbeCache<SourceTraits> {
    static CACHE: OnceLock<ProbeCache<SourceTraits>> = OnceLock::new();
    CACHE.get_or_init(ProbeCache::new)
}

/// What [`source_traits`] has remembered of the file, without probing it.
fn source_traits_known(path: &Path) -> Option<SourceTraits> {
    source_traits_cache().peek(path)
}

/// One `ffprobe` of `path`, killed after `timeout`.
fn probe_source_traits(ffprobe: &str, path: &Path, timeout: std::time::Duration) -> Option<SourceTraits> {
    let mut cmd = command(ffprobe);
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=color_transfer,color_range,start_time,codec_name,pix_fmt,time_base:format=start_time,format_name",
    ])
    // JSON rather than `csv`/`default`: the stream and the format both have a
    // `start_time`, and a file with side data (a phone's display matrix) makes
    // the csv writer append a stray empty field to the value.
    .args(["-of", "json"])
    .arg(path);
    let output = run_piped_until(&mut cmd, Vec::new(), Instant::now() + timeout)?;
    let traits = parse_source_traits(&String::from_utf8_lossy(&output));
    // (An `ffprobe` that exits 0 and prints nothing usable has told nothing: that is a failed probe, retried after the
    // failure backoff, and not the neutral traits of an ordinary file remembered for good.)
    traits.described.then_some(traits)
}

/// The pure half of [`source_traits`]: read the probe's JSON. Anything missing
/// or unparsable (`N/A` is simply absent) leaves the neutral value, so an odd
/// file is treated as an ordinary one.
fn parse_source_traits(json: &str) -> SourceTraits {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return SourceTraits::default();
    };
    let secs = |field: Option<&serde_json::Value>| -> Option<f64> {
        field?.as_str()?.trim().parse::<f64>().ok().filter(|t| t.is_finite())
    };
    let stream = v.get("streams").and_then(|s| s.get(0));
    let hdr = match stream
        .and_then(|s| s.get("color_transfer"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
    {
        "arib-std-b67" => Some(Hdr::Hlg),
        "smpte2084" => Some(Hdr::Pq),
        _ => None,
    };
    let format = v.get("format");
    // MPEG-TS (a camcorder's `.mts` / `.m2ts` included) cannot be fixed this way:
    // its demuxer measures the container's start over only the streams being read,
    // so a read with the audio discarded — which is what makes the proxy — rebases
    // the video to zero, the pad's clone and the first frame come out one tick
    // apart, and a seek into the proxy lands where a plain proxy's would. The lead
    // is reported as none, so the source keeps the plain proxy and its cache key.
    let transport_stream = format
        .and_then(|f| f.get("format_name"))
        .and_then(|n| n.as_str())
        .is_some_and(|n| n.split(',').any(|n| n == "mpegts"));
    let lead = if transport_stream {
        0.0
    } else {
        head_lead(
            secs(stream.and_then(|s| s.get("start_time"))),
            secs(format.and_then(|f| f.get("start_time"))),
        )
    };
    let indexed = format
        .and_then(|f| f.get("format_name"))
        .and_then(|n| n.as_str())
        .is_some_and(|n| n.split(',').any(|n| matches!(n, "mp4" | "matroska")));
    let text = |key: &str| stream.and_then(|s| s.get(key)).and_then(|v| v.as_str()).unwrap_or("");
    SourceTraits {
        hdr,
        lead,
        indexed,
        full_range: text("color_range") == "pc",
        described: v.get("streams").is_some_and(|s| s.is_array()) && v.get("format").is_some_and(|f| f.is_object()),
        codec: Codec::from_name(text("codec_name")),
        chroma: Chroma::from_pix_fmt(text("pix_fmt")),
        bits: pix_fmt_bits_420(text("pix_fmt")),
        luma_plane: luma_plane_pix_fmt(text("pix_fmt")),
        time_base: parse_time_base(text("time_base")),
    }
}

/// Whether the file at `path` is an MP4/MOV or Matroska/WebM file (see
/// [`SourceTraits::indexed`]): the only containers whose frame timestamps and seeks the
/// GPU frame source's decode runs are checked against. One cached `ffprobe` per file (it
/// spawns a process: not under a lock); `false` when the probe fails.
pub fn source_is_indexed_container(path: &Path) -> bool {
    source_traits(path).is_some_and(|t| t.indexed)
}

/// What a whole-file *analysis* read can take from the preview proxy of a file in place of the
/// file itself (see [`proxy_fidelity`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProxyFidelity {
    /// A frame sits at the time it does in the original, so a read that picks frames by time
    /// (a contact sheet) or reports a time (a cut) says the same: an MP4 / MOV or Matroska / WebM
    /// source, the containers the proxy's timing is verified for. A transport stream's proxy is
    /// `lead` seconds off whenever its video starts late (`engine-cli.md`), and the rest are
    /// unproven.
    pub times: bool,
    /// ...and a frame's *scene score* comes out within a few hundredths of the original's, so
    /// the cuts found are the same: also an SDR source whose luma sits alone in plane 0
    /// ([`SourceTraits::luma_plane`]; not RGB, not packed YCbCr). A proxy is tone-mapped at
    /// encode, and the original is scored in its HLG / PQ code values, which the tone curve moves
    /// by a fifth of the scale (0.20 to 0.21 measured); an RGB original is scored on its G plane
    /// or its interleaved bytes against the proxy's luma (up to 0.56 measured, cuts lost). The few
    /// hundredths are for the software (libx264) proxy; a hardware-encoded one is unmeasured.
    pub scores: bool,
}

/// [`ProxyFidelity`] of the file at `path`. One cached `ffprobe` (the one the proxy's own name
/// already cost); nothing is trusted when the probe fails. Spawns a process: not under a lock.
pub fn proxy_fidelity(path: &Path) -> ProxyFidelity {
    fidelity_of(source_traits(path).unwrap_or_default())
}

/// The pure half of [`proxy_fidelity`].
fn fidelity_of(traits: SourceTraits) -> ProxyFidelity {
    ProxyFidelity {
        times: traits.indexed,
        scores: traits.indexed && traits.hdr.is_none() && traits.luma_plane,
    }
}

/// The HDR transfer of the first video stream of the file at `path`, for the
/// decodes that are handed a bare path rather than an [`Asset`] (a scrubbed
/// frame, a contact sheet, a proxy). Probed once per file and cached against the
/// file's size and modified time. A generated proxy answers `None` — it was
/// tone-mapped when it was encoded, which is the point.
pub(crate) fn source_hdr(path: &Path) -> Option<Hdr> {
    source_traits(path)?.hdr
}

// ---- silence / scene analysis ---------------------------------------------

/// Detect silent spans using the `silencedetect` filter.
///
/// `noise_db` is the threshold in dBFS (e.g. `-30.0`); `min_silence` is the
/// shortest span to report, in seconds.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (while it waits for the heavy-job
/// slot or while ffmpeg runs), and fails — rather than answering "no silence" — when the decode
/// does: a run that exits non-zero, or says nothing for [`READ_STALL`], is an error with
/// ffmpeg's last words. A file with no audio stream is the one non-zero exit that is an answer
/// (no silence), as the `0:a:0?` map always meant.
pub fn detect_silence_cancellable(
    path: &Path,
    noise_db: f64,
    min_silence: f64,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<TimeRange>> {
    let bin = ffmpeg_bin();
    let filter = format!("silencedetect=noise={noise_db}dB:d={min_silence}");
    // A whole-file decode: takes the heavy-job slot and the budget's threads.
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    // A free slot is taken without a look at the cancel: do not start a pass that was stopped.
    if cancel() {
        return Err(Error::Cancelled);
    }
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu.budget());
    cmd.args(PROGRESS_ON_STDERR)
        .args(["-hide_banner", "-nostats"])
        .arg("-i")
        .arg(path)
        .args(["-map", "0:a:0?", "-af", &filter, "-f", "null", "-"]);
    let output = output_supervised(cmd, READ_STALL, None, cancel).map_err(|e| launch_err(&bin, e))?;
    silence_from_run(&output, path)
}

/// What a finished `silencedetect` run says (pure over [`Collected`], unit-tested): the spans
/// it printed, or why there are none.
fn silence_from_run(output: &Collected, path: &Path) -> Result<Vec<TimeRange>> {
    if output.cancelled {
        return Err(Error::Cancelled);
    }
    if output.stalled {
        return Err(Error::Engine(format!(
            "silence detection stalled: ffmpeg said nothing for {}s reading {}: {}",
            READ_STALL.as_secs(),
            path.display(),
            output.stderr_tail()
        )));
    }
    if !output.status.success() {
        if output.stderr.contains("does not contain any stream") {
            return Ok(Vec::new());
        }
        return Err(Error::Engine(format!(
            "silence detection failed ({}) reading {}: {}",
            output.status,
            path.display(),
            output.stderr_tail()
        )));
    }
    Ok(parse_silence(&output.stderr))
}

fn parse_silence(stderr: &str) -> Vec<TimeRange> {
    let mut ranges = Vec::new();
    let mut pending_start: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(v) = field_after(line, "silence_start:") {
            pending_start = Some(v);
        } else if let Some(end) = field_after(line, "silence_end:") {
            if let Some(start) = pending_start.take() {
                if end > start {
                    ranges.push(TimeRange { start, end });
                }
            }
        }
    }
    ranges
}

/// Width the scene detector runs at. The scene score is a mean absolute frame
/// difference normalized by pixel count, so it is stable under downscale — and
/// computing it on a 640px frame instead of 4K makes the per-frame diff ~20x
/// cheaper while the decode (hardware-accelerated when available) dominates.
const SCENE_DETECT_WIDTH: u32 = 640;

/// Detect scene-change timestamps using `select='gt(scene,threshold)'`.
///
/// Decodes with `-hwaccel` when configured (the whole file is decoded, which is
/// the expensive part for 4K sources); a failed accelerated run retries in
/// software, mirroring [`decode_frame`]'s fallback.
#[cfg(test)]
pub fn detect_scenes(path: &Path, threshold: f64) -> Result<Vec<f64>> {
    detect_scenes_cancellable(path, threshold, &|| false)
}

/// [`detect_scenes`] that gives up with [`Error::Cancelled`] once `cancel` is true, and that
/// fails — rather than answering "no cuts" — when the decode does (a file with no video stream
/// is the one non-zero exit that is an answer: no cuts).
pub fn detect_scenes_cancellable(path: &Path, threshold: f64, cancel: &dyn Fn() -> bool) -> Result<Vec<f64>> {
    let pass = scene_pass(path, threshold, cancel)?;
    if !pass.ok && !pass.no_video {
        return Err(Error::Engine(format!(
            "scene detection failed reading {}: {}",
            path.display(),
            pass.why
        )));
    }
    Ok(pass.times)
}

/// [`detect_scenes`] for the file at `original`, reading its preview `proxy` instead when one
/// is given: a small all-intra decode in place of a whole 4K long-GOP one.
///
/// The proxy is only ever a faster way to the *same answer*, so a proxy that does not read
/// (a truncated or foreign file, a decoder that dies on it) is not a failed analysis: the
/// original is read instead, as if there had been none. The cut times themselves come out
/// the same because a proxy answers `-ss T` with the frame the original does (see
/// [`build_proxy_args`]; a late-starting video is padded so its frames keep their
/// timestamps), and the detector scores at [`SCENE_DETECT_WIDTH`] either way.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (a stop on the proxy read does not
/// fall back to reading the original).
pub fn detect_scenes_preferring_cancellable(
    original: &Path,
    proxy: Option<&Path>,
    threshold: f64,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<f64>> {
    if let Some(proxy) = proxy {
        match scene_pass(proxy, threshold, cancel) {
            Ok(pass) if pass.ok => return Ok(pass.times),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            _ => {}
        }
        tracing::warn!(proxy = %proxy.display(), "scene detection could not read the proxy; reading the original");
    }
    detect_scenes_cancellable(original, threshold, cancel)
}

/// The `-vf` chain of a scene pass (pure). Frame 0 is selected too (`eq(n,0)`) and dropped again
/// by [`parse_scenes`]: a pass with no cut outputs nothing otherwise, and ffmpeg reports no
/// progress until a frame has reached the output, so a long single take would look wedged from
/// its first second.
fn scene_filter(threshold: f64) -> String {
    format!("scale='min({SCENE_DETECT_WIDTH},iw)':-2:flags=bilinear,select='gt(scene,{threshold})+eq(n,0)',showinfo")
}

/// What one scene-detection pass found.
struct ScenePass {
    /// The cut times in the log.
    times: Vec<f64>,
    /// ffmpeg ended cleanly.
    ok: bool,
    /// The input has no video stream to score (`-map 0:v:0?` left nothing to write).
    no_video: bool,
    /// The last of ffmpeg's words, for the error of a run that did not end cleanly.
    why: String,
}

/// One scene-detection pass over `path`: the cut times and whether ffmpeg ended cleanly; the
/// flag is what lets a caller with somewhere else to read (the original, behind a proxy) tell.
/// A run that goes [`READ_STALL`] without a word is killed and is an error, not a short list of
/// cuts.
fn scene_pass(path: &Path, threshold: f64, cancel: &dyn Fn() -> bool) -> Result<ScenePass> {
    use std::sync::atomic::Ordering;

    let bin = ffmpeg_bin();
    let filter = scene_filter(threshold);
    // Probed before queueing: a hung probe must not hold the one heavy-job slot.
    let hw = decode_hwaccel_for(path, Reading::Whole);
    // The whole file is decoded, so this queues behind any other heavy job.
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    if cancel() {
        return Err(Error::Cancelled);
    }
    let run = |hw: Option<&str>| {
        let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostats".into()];
        if let Some(hw) = hw {
            args.extend(["-hwaccel".into(), hw.into()]);
        }
        args.extend(["-i".into(), path.to_string_lossy().into_owned()]);
        args.extend(["-map", "0:v:0?", "-vf", &filter, "-f", "null", "-"].map(String::from));
        cpu::limit_args(&mut args, cpu.budget());
        let mut cmd = bg_command(&bin);
        cmd.args(PROGRESS_ON_STDERR).args(&args);
        let out = output_supervised(cmd, READ_STALL, None, cancel).map_err(|e| launch_err(&bin, e))?;
        if out.cancelled {
            return Err(Error::Cancelled);
        }
        Ok(out)
    };
    let output = match hw.as_deref() {
        Some(h) => {
            let out = run(Some(h))?;
            if out.succeeded() {
                out
            } else {
                let sw = run(None)?;
                if sw.succeeded() {
                    HWACCEL_OK.store(false, Ordering::Relaxed);
                    tracing::warn!("hardware decode failed during scene detection; using software decode");
                }
                sw
            }
        }
        None => run(None)?,
    };
    if output.stalled {
        return Err(Error::Engine(format!(
            "scene detection stalled: ffmpeg said nothing for {}s reading {}: {}",
            READ_STALL.as_secs(),
            path.display(),
            output.stderr_tail()
        )));
    }
    Ok(ScenePass {
        times: parse_scenes(&output.stderr),
        ok: output.status.success(),
        no_video: output.stderr.contains("does not contain any stream"),
        why: output.stderr_tail(),
    })
}

/// The cut times in a scene pass's `showinfo` log. Its first line (`n:0`, showinfo's own count of
/// the frames that reached it) is frame 0, which the pass lets through to keep ffmpeg reporting
/// and which never scores as a cut: not a cut.
fn parse_scenes(stderr: &str) -> Vec<f64> {
    let mut times = Vec::new();
    for line in stderr.lines() {
        if field_after(line, "] n:") == Some(0.0) {
            continue;
        }
        if let Some(t) = field_after(line, "pts_time:") {
            times.push(t);
        }
    }
    times.sort_by(f64::total_cmp);
    times.dedup();
    times
}

/// Parse the number that immediately follows `key` on a log line, tolerating a
/// leading space (`"silence_start: 12.5"`).
fn field_after(line: &str, key: &str) -> Option<f64> {
    let rest = line.split(key).nth(1)?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

// ---- salience sampling (smart crop) ----------------------------------------

/// Grid the salience sampler decodes at. Coarse on purpose: the answer is a
/// crop window, not a mask, and 64x36 gray pixels is 2.3 KB a frame — small
/// enough that hundreds of samples cost less than one preview still.
pub const SALIENCE_COLS: usize = 64;
pub const SALIENCE_ROWS: usize = 36;

/// Frames sampled across the window. Enough to average out a blink or a
/// handheld wobble without decoding the whole clip's worth of pictures.
const SALIENCE_SAMPLES: usize = 48;

/// Weight of *motion* against *detail* in a cell's score. Motion is the stronger
/// signal when there is any — a subject who moves is the subject — but a
/// locked-off talking head has none at all, which is why detail carries the
/// floor rather than being a tie-break.
const SALIENCE_MOTION_WEIGHT: f64 = 3.0;

/// Sample where the content of `path`'s `[start, end)` window sits, as a coarse
/// [`SalienceMap`].
///
/// One ffmpeg pass decodes a few dozen tiny grayscale frames; each cell scores
/// the picture's edge energy plus how much it changed since the last sample.
/// Hardware-accelerated like [`detect_scenes`], with the same software retry —
/// the decode is the expensive half on 4K footage, and the arithmetic here runs
/// on 2 KB frames.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (queued for the slot or decoding),
/// and is killed after [`READ_STALL`] without a word from ffmpeg.
pub fn salience_map_cancellable(path: &Path, start: f64, end: f64, cancel: &dyn Fn() -> bool) -> Result<SalienceMap> {
    use std::sync::atomic::Ordering;

    let bin = ffmpeg_bin();
    let hw = decode_hwaccel_for(path, Reading::Whole);
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    if cancel() {
        return Err(Error::Cancelled);
    }
    let mut args = build_salience_args(path, start, end);
    cpu::limit_args(&mut args, cpu.budget());
    let run = |hw: Option<&str>| -> Result<Collected> {
        let mut cmd = bg_command(&bin);
        cmd.args(PROGRESS_ON_STDERR);
        if let Some(hw) = hw {
            cmd.args(["-hwaccel", hw]);
        }
        cmd.args(&args);
        let out = output_supervised(cmd, READ_STALL, None, cancel).map_err(|e| launch_err(&bin, e))?;
        if out.cancelled {
            return Err(Error::Cancelled);
        }
        Ok(out)
    };
    let output = match hw.as_deref() {
        Some(h) => {
            let out = run(Some(h))?;
            if out.succeeded() && !out.stdout.is_empty() {
                out
            } else {
                let sw = run(None)?;
                if sw.succeeded() {
                    HWACCEL_OK.store(false, Ordering::Relaxed);
                    tracing::warn!("hardware decode failed while sampling salience; using software decode");
                }
                sw
            }
        }
        None => run(None)?,
    };
    if output.stalled {
        return Err(Error::Engine(format!(
            "could not sample the shot: ffmpeg said nothing for {}s: {}",
            READ_STALL.as_secs(),
            output.stderr_tail()
        )));
    }
    if !output.succeeded() {
        return Err(Error::Engine(format!("could not sample the shot: {}", output.stderr.trim())));
    }
    Ok(score_salience(&output.stdout))
}

/// Pure arg builder for [`salience_map_cancellable`] (no I/O, unit-tested). Fast-seeks to
/// the window, decodes at most [`SALIENCE_SAMPLES`] frames spread across it, and
/// writes them as raw gray at the analysis grid. `-an` because nothing here
/// looks at sound, and `fps` before `scale` so the scaler runs on the frames
/// that survive rather than on every one.
fn build_salience_args(path: &Path, start: f64, end: f64) -> Vec<String> {
    let start = start.max(0.0);
    let window = (end - start).max(0.04);
    // Spread the samples across the window, but never ask for more frames per
    // second than a sane source has — a 0.2s clip wants every frame it has, not
    // 240 duplicated ones.
    let fps = (SALIENCE_SAMPLES as f64 / window).clamp(0.2, 30.0);
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostats"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push("-ss".into());
    args.push(format!("{start:.3}"));
    args.push("-t".into());
    args.push(format!("{window:.3}"));
    args.push("-i".into());
    args.push(path.to_string_lossy().into_owned());
    args.push("-an".into());
    args.push("-map".into());
    args.push("0:v:0?".into());
    args.push("-vf".into());
    args.push(format!(
        "fps={fps:.4},scale={SALIENCE_COLS}:{SALIENCE_ROWS}:flags=bilinear,format=gray"
    ));
    args.push("-frames:v".into());
    args.push(SALIENCE_SAMPLES.to_string());
    args.push("-f".into());
    args.push("rawvideo".into());
    args.push("-pix_fmt".into());
    args.push("gray".into());
    args.push("pipe:1".into());
    args
}

/// Score a run of raw gray [`SALIENCE_COLS`]x[`SALIENCE_ROWS`] frames into a
/// [`SalienceMap`]: per cell, the local edge energy of every frame plus the
/// frame-to-frame change, averaged over the frames actually decoded. Pure over
/// the decoded bytes, so the scoring is unit-testable without ffmpeg.
fn score_salience(raw: &[u8]) -> SalienceMap {
    let (w, h) = (SALIENCE_COLS, SALIENCE_ROWS);
    let stride = w * h;
    let frames = raw.len() / stride;
    if frames == 0 {
        return SalienceMap::default();
    }
    let mut cells = vec![0.0f64; stride];
    let mut prev: Option<&[u8]> = None;
    for f in 0..frames {
        let frame = &raw[f * stride..(f + 1) * stride];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let p = frame[i] as f64;
                // Edge energy: how much this pixel differs from its right and
                // lower neighbours. Texture and outlines score, flat sky doesn't.
                let dx = if x + 1 < w { (frame[i + 1] as f64 - p).abs() } else { 0.0 };
                let dy = if y + 1 < h { (frame[i + w] as f64 - p).abs() } else { 0.0 };
                let motion = prev.map_or(0.0, |q| (q[i] as f64 - p).abs());
                cells[i] += dx + dy + SALIENCE_MOTION_WEIGHT * motion;
            }
        }
        prev = Some(frame);
    }
    let scale = 1.0 / (frames as f64 * 255.0);
    SalienceMap::new(w, h, cells.into_iter().map(|c| (c * scale) as f32).collect())
}

// ---- frame / waveform extraction ------------------------------------------

/// Decode a single frame at `time_secs` and return it as PNG bytes, scaled to
/// at most `max_width` pixels wide.
pub fn frame_at(path: &Path, time_secs: f64, max_width: u32) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "png", None, true, None)
}

/// A rectangle of a frame to look at more closely, as fractions of the full
/// frame: `left`/`top` place its corner, `width`/`height` size it.
///
/// This is the *zoom* half of "look, then look closer": a vision model spends
/// its image budget on whatever it is handed, so a quarter of the frame cropped
/// out and scaled to the same width shows four times the detail of the whole
/// frame for the same cost — a face, a caption, a mask edge. Nothing here is an
/// edit; it is only how a frame is presented.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl Region {
    /// The smallest side a region may have, as a fraction of the frame: below
    /// this a crop of a 640 px preview is a handful of pixels.
    pub const MIN_SIDE: f64 = 0.02;

    /// The whole frame, which is what "no region" means.
    pub const FULL: Region = Region {
        left: 0.0,
        top: 0.0,
        width: 1.0,
        height: 1.0,
    };

    /// Clamp into the frame: sides at least [`Region::MIN_SIDE`], the corner
    /// inside, and the far edge pulled back to 1.0 by shrinking the size, not
    /// by moving the corner — the corner is what the caller pointed at.
    pub fn normalized(self) -> Region {
        let n = |v: f64| if v.is_finite() { v } else { 0.0 };
        let left = n(self.left).clamp(0.0, 1.0 - Self::MIN_SIDE);
        let top = n(self.top).clamp(0.0, 1.0 - Self::MIN_SIDE);
        let width = n(self.width).clamp(Self::MIN_SIDE, 1.0 - left);
        let height = n(self.height).clamp(Self::MIN_SIDE, 1.0 - top);
        Region {
            left,
            top,
            width,
            height,
        }
    }

    /// Whether this is (within rounding) the whole frame — then no crop is
    /// added, so a caller passing the full region gets the byte-identical
    /// invocation it always got.
    pub fn is_full(&self) -> bool {
        let r = self.normalized();
        r.left < 1e-6 && r.top < 1e-6 && r.width > 1.0 - 1e-6 && r.height > 1.0 - 1e-6
    }

    /// The `crop` filter for this region against whatever frame it is applied
    /// to, in `iw`/`ih` terms so it needs no knowledge of the frame size. The
    /// width and height are kept even: MJPEG is 4:2:0 and an odd crop would be
    /// realigned by the filter to a size the scale after it cannot predict.
    fn crop_filter(&self) -> String {
        let r = self.normalized();
        format!(
            "crop=2*trunc(iw*{w:.4}/2):2*trunc(ih*{h:.4}/2):iw*{l:.4}:ih*{t:.4}",
            w = r.width,
            h = r.height,
            l = r.left,
            t = r.top
        )
    }
}

/// Decode a single frame at `time_secs` as **JPEG** bytes, scaled to at most
/// `max_width` pixels wide, at `quality` (ffmpeg `-q:v`, 2 = best … 31 = worst).
/// JPEG is dramatically smaller than the PNG of [`frame_at`], which matters when
/// the frame is handed to an LLM as an image content block rather than rendered
/// in the GUI. `accurate = false` snaps to the nearest keyframe (fast scrubbing);
/// see [`decode_frame`].
pub fn frame_jpeg(path: &Path, time_secs: f64, max_width: u32, quality: u8, accurate: bool) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "mjpeg", Some(quality), accurate, None)
}

/// [`frame_jpeg`] that gives up with [`Error::Cancelled`] once `cancel` is true.
pub fn frame_jpeg_cancellable(
    path: &Path,
    time_secs: f64,
    max_width: u32,
    quality: u8,
    accurate: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    let scale = format!("scale='min({max_width},iw)':-2");
    decode_frame(path, time_secs, &scale, "mjpeg", Some(quality), accurate, Some(cancel))
}

/// [`frame_jpeg`] of one `region` of the frame: the region is cropped out of
/// the decoded frame *first* and then scaled to at most `max_width`, so the
/// width budget is spent on the region rather than on the whole picture. Never
/// upscaled past the source's own pixels — a zoom shows real detail or none.
/// A full region is exactly [`frame_jpeg`].
pub fn frame_jpeg_region(
    path: &Path,
    time_secs: f64,
    region: Region,
    max_width: u32,
    quality: u8,
    accurate: bool,
) -> Result<Vec<u8>> {
    if region.is_full() {
        return frame_jpeg(path, time_secs, max_width, quality, accurate);
    }
    let vf = region_frame_filter(region, max_width);
    decode_frame(path, time_secs, &vf, "mjpeg", Some(quality), accurate, None)
}

/// [`frame_jpeg_region`] that gives up with [`Error::Cancelled`] once `cancel` is true.
pub fn frame_jpeg_region_cancellable(
    path: &Path,
    time_secs: f64,
    region: Region,
    max_width: u32,
    quality: u8,
    accurate: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    if region.is_full() {
        return frame_jpeg_cancellable(path, time_secs, max_width, quality, accurate, cancel);
    }
    let vf = region_frame_filter(region, max_width);
    decode_frame(path, time_secs, &vf, "mjpeg", Some(quality), accurate, Some(cancel))
}

/// Pure filter chain for [`frame_jpeg_region`] (unit-tested): crop, then scale
/// to an even width no wider than `max_width` or the crop itself.
fn region_frame_filter(region: Region, max_width: u32) -> String {
    format!(
        "{crop},scale='2*trunc(min({max_width},iw)/2)':-2",
        crop = region.crop_filter()
    )
}

/// Seek to `time_secs`, run the `-vf` chain on a single frame and pipe it out in
/// the given image codec (`png` / `mjpeg`); `quality`, when set, becomes `-q:v`.
/// Shared by [`frame_at`] and [`frame_jpeg`]. `-ss` is input-side (fast). With
/// `accurate` ffmpeg decodes forward from the keyframe to the exact frame; with
/// `accurate = false` it snaps to the keyframe (`-noaccurate_seek`, no forward
/// decode) — tens of ms even on long-GOP 4K, for responsive scrubbing. Decode is
/// hardware-accelerated per [`hwaccel`]. With a `cancel` the decode is killed when it goes true and
/// the answer is [`Error::Cancelled`] — never a reason to retry in software or to switch hardware
/// decode off; without one (the scrub, which is never cancelled, only superseded) the decode waits
/// on nothing but its own deadline.
fn decode_frame(
    path: &Path,
    time_secs: f64,
    vf: &str,
    vcodec: &str,
    quality: Option<u8>,
    accurate: bool,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    if cancel.is_some_and(|cancel| cancel()) {
        return Err(Error::Cancelled);
    }
    let time_secs = time_secs.max(0.0);
    // HDR footage is tone-mapped after the caller's own scale/crop, so the
    // expensive float stage runs on the small frame rather than the 4K one.
    let tonemapped;
    let vf = match source_hdr(path) {
        Some(hdr) => {
            tonemapped = format!("{vf},{}", tonemap_filter(hdr));
            tonemapped.as_str()
        }
        None => vf,
    };
    // Cache key captures everything that determines the bytes (path, time,
    // filter — which includes the target width — codec, quality and whether the
    // seek was exact or keyframe-snapped).
    let key = format!("{}|{:.3}|{vf}|{vcodec}|{quality:?}|{accurate}", path.display(), time_secs);
    if let Some(hit) = frame_cache().lock().ok().and_then(|mut c| c.get(&key)) {
        return Ok(hit);
    }

    use std::sync::atomic::Ordering;
    let bin = ffmpeg_bin();
    // A proxy is one intra frame of a small H.264: no hardware session, one thread.
    let proxy = is_cache_proxy(&path.to_string_lossy());
    let hw = decode_hwaccel_for(path, Reading::Frame);
    let use_hw = hw.is_some();

    let bytes = if use_hw {
        match run_frame_decode(
            &bin,
            path,
            time_secs,
            vf,
            vcodec,
            quality,
            accurate,
            hw.as_deref(),
            proxy,
            cancel,
        ) {
            Ok(b) => b,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(hw_err) => {
                // The accelerated decode failed — retry in software. If *that*
                // works, `-hwaccel` is the culprit on this machine, so disable it
                // for subsequent frames; if it fails too, the error is genuine.
                match run_frame_decode(&bin, path, time_secs, vf, vcodec, quality, accurate, None, proxy, cancel) {
                    Ok(b) => {
                        HWACCEL_OK.store(false, Ordering::Relaxed);
                        tracing::warn!("hardware decode failed ({hw_err}); using software decode for previews");
                        b
                    }
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(_) => return Err(hw_err),
                }
            }
        }
    } else {
        run_frame_decode(&bin, path, time_secs, vf, vcodec, quality, accurate, None, proxy, cancel)?
    };

    if let Ok(mut c) = frame_cache().lock() {
        c.put(key, bytes.clone());
    }
    Ok(bytes)
}

/// How long one single-frame decode may run before it is killed (a hardware decoder stuck in a
/// driver would otherwise hold the preview's one frame in flight for good). The software retry
/// that follows is a second chance, not a second wait.
const FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Run one `ffmpeg` single-frame decode (optionally with `-hwaccel hw`) and
/// return the encoded image bytes. Split out of [`decode_frame`] so the same
/// invocation can be retried in software when an accelerated attempt fails.
#[allow(clippy::too_many_arguments)]
fn run_frame_decode(
    bin: &str,
    path: &Path,
    time_secs: f64,
    vf: &str,
    vcodec: &str,
    quality: Option<u8>,
    accurate: bool,
    hw: Option<&str>,
    single_thread: bool,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    let mut cmd = command(bin);
    // Interactive: never gated and never de-prioritized — a scrubbed frame is
    // wanted now — but still held to the budget's threads, or to one when the
    // frame is read from a proxy (the thread pools would outlast the decode).
    if single_thread {
        cmd.args(["-threads", "1", "-filter_threads", "1", "-filter_complex_threads", "1"]);
    } else {
        cpu::limit_cmd(&mut cmd, cpu::budget());
    }
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if let Some(hw) = hw {
        cmd.args(["-hwaccel", hw]);
    }
    if !accurate {
        cmd.arg("-noaccurate_seek");
    }
    // No seek to the start: it reads the same first frame, and on a JPEG still (the `image2`
    // demuxer) any `-ss`, 0 included, makes ffmpeg exit 0 having decoded nothing (6.1 and 9.0).
    if let Some(at) = seek_unless_start(time_secs) {
        cmd.arg("-ss").arg(at);
    }
    cmd.arg("-i").arg(path).args(["-frames:v", "1", "-vf", vf]);
    if let Some(q) = quality {
        cmd.args(["-q:v", q.to_string().as_str()]);
    }
    cmd.args(["-f", "image2pipe", "-vcodec", vcodec]);
    if single_thread {
        cmd.args(["-threads", "1"]);
    }
    cmd.arg("pipe:1");
    let output = output_bounded_cancellable(cmd, FRAME_TIMEOUT, cancel)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "could not extract frame at {time_secs:.3}s: ffmpeg did not answer in {} s and was stopped",
                FRAME_TIMEOUT.as_secs()
            )),
            _ => launch_err(bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(Error::Engine(format!(
            "could not extract frame at {time_secs:.3}s: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// The `-ss` a single-frame read or a sheet seeks with (`{:.3}`), or `None` at the start, which
/// seeking to would only break a JPEG still: there `-ss 0` decodes no frame and ffmpeg still exits 0.
fn seek_unless_start(at: f64) -> Option<String> {
    let at = format!("{:.3}", at.max(0.0));
    (at != "0.000").then_some(at)
}

/// [`contact_sheet_cancellable`] with nothing to cancel it.
#[cfg(test)]
pub fn contact_sheet(
    path: &Path,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
) -> Result<(Vec<u8>, Vec<f64>)> {
    contact_sheet_cancellable(path, start, end, columns, rows, cell_width, quality, &|| false)
}

/// Build a **contact sheet** of `path`: `columns`×`rows` frames sampled evenly
/// across `[start, end)`, each cell scaled to `cell_width` px wide and tiled into
/// one JPEG (`quality` = `-q:v`). Returns the montage bytes plus the per-cell
/// timestamps in row-major order, so the caller can tell an LLM which moment each
/// cell shows. One ffmpeg pass — lets the model skim a long clip cheaply.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true (polled about fifty times a second; the
/// sampling ffmpeg is killed) — an agent that dropped its request does not leave a many-seek
/// decode running.
#[allow(clippy::too_many_arguments)]
pub fn contact_sheet_cancellable(
    path: &Path,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<(Vec<u8>, Vec<f64>)> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    let tonemap = source_hdr(Path::new(path)).map(tonemap_filter);
    let (mut args, times) = build_contact_sheet_args(path, start, end, columns, rows, cell_width, quality, tonemap.as_deref());
    let bin = ffmpeg_bin();
    // Deliberately ungated: this is how an agent *looks* at the footage, and
    // making it wait out a ten-minute render would read as a hung server. It
    // still runs thread-capped and at background priority.
    cpu::limit_args(&mut args, cpu::budget());
    let mut cmd = bg_command(&bin);
    cmd.args(PROGRESS_ON_STDERR).args(&args);
    // `tile` outputs the one frame when the window has been decoded, and ffmpeg reports nothing
    // before: the bound grows with the window (see `sheet_stall`).
    let stall = sheet_stall(end - start);
    let output = output_supervised(cmd, stall, None, cancel).map_err(|e| launch_err(&bin, e))?;
    if output.cancelled {
        return Err(Error::Cancelled);
    }
    if !output.succeeded() || output.stdout.is_empty() {
        let why = if output.stalled {
            format!("ffmpeg said nothing for {}s: {}", stall.as_secs(), output.stderr_tail())
        } else {
            output.stderr.trim().to_string()
        };
        return Err(Error::Engine(format!("could not build contact sheet: {why}")));
    }
    Ok((output.stdout, times))
}

/// The source timestamp each cell of a `columns`×`rows` [`contact_sheet`] over
/// `[start, end)` shows, row-major: the start of each of the equal slices the
/// window is cut into. Public so a caller holding a sheet can turn "cell 7"
/// back into a moment to look at more closely without re-sampling the sheet.
pub fn contact_sheet_times(start: f64, end: f64, columns: u32, rows: u32) -> Vec<f64> {
    let cells = (columns.max(1) * rows.max(1)) as usize;
    let start = start.max(0.0);
    let window = (end - start).max(0.0);
    let step = if window > 0.0 { window / cells as f64 } else { 0.0 };
    (0..cells).map(|k| start + step * k as f64).collect()
}

/// Pure arg builder for [`contact_sheet`] (no I/O, unit-tested): the ffmpeg
/// argument list and the row-major per-cell timestamps. Frames are sampled at
/// the start of each of `columns*rows` equal slices of the window via the `fps`
/// filter over an `-ss`/`-t` window, then `tile`d into the single output frame.
#[allow(clippy::too_many_arguments)]
fn build_contact_sheet_args(
    path: &str,
    start: f64,
    end: f64,
    columns: u32,
    rows: u32,
    cell_width: u32,
    quality: u8,
    tonemap: Option<&str>,
) -> (Vec<String>, Vec<f64>) {
    let columns = columns.max(1);
    let rows = rows.max(1);
    let cells = (columns * rows) as usize;
    let start = start.max(0.0);
    let window = (end - start).max(0.0);
    let times = contact_sheet_times(start, end, columns, rows);
    // `fps` = one frame per slice over the seeked window; `tile` packs them and
    // `-frames:v 1` emits the single sheet. A degenerate window falls back to 1.
    let rate = if window > 0.0 { cells as f64 / window } else { 1.0 };
    // HDR sources are tone-mapped per cell, after the downscale, before the tile.
    let tone = tonemap.map(|t| format!("{t},")).unwrap_or_default();
    // A head-padded proxy opens with a clone of its first frame, which the original has no
    // counterpart for: read from the very start (no seek, which keeps it) it would take a
    // slot of its own and push every cell one over. Any later seek has skipped it already.
    let unpad = if start < 0.0005 && is_head_padded_proxy(path) {
        "trim=start_frame=1,"
    } else {
        ""
    };
    let vf = format!("{unpad}fps={rate},scale={cell_width}:-2:flags=bilinear,{tone}tile={columns}x{rows}");
    let mut args = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string()];
    if let Some(at) = seek_unless_start(start) {
        args.extend(["-ss".to_string(), at]);
    }
    args.extend([
        "-t".to_string(),
        format!("{window:.3}"),
        "-i".to_string(),
        path.to_string(),
        "-frames:v".to_string(),
        "1".to_string(),
        "-vf".to_string(),
        vf,
        "-q:v".to_string(),
        quality.to_string(),
        "-f".to_string(),
        "image2pipe".to_string(),
        "-vcodec".to_string(),
        "mjpeg".to_string(),
        "pipe:1".to_string(),
    ]);
    (args, times)
}

/// Decode the first audio stream to mono f32 PCM at `sample_rate` Hz and reduce
/// it to `buckets` peak magnitudes in `0.0..=1.0` (for waveform rendering).
pub fn waveform(path: &Path, buckets: usize, sample_rate: u32) -> Result<Vec<f32>> {
    waveform_cancellable(path, buckets, sample_rate, &|| false)
}

/// [`waveform`] that gives up with [`Error::Cancelled`] once `cancel` is true (the decode is killed).
pub fn waveform_cancellable(path: &Path, buckets: usize, sample_rate: u32, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    let buckets = buckets.max(1);
    // Stream the decoded PCM through a bounded peak-downsampler instead of
    // buffering the whole signal: a 1h file at 8 kHz mono is ~115 MB of f32
    // otherwise (held twice — raw stdout then the parsed Vec). Here memory is
    // O(buckets) regardless of length, and ffmpeg's decode is the only cost.
    let bin = ffmpeg_bin();
    // Ungated like the other reads the timeline draws from — a clip's waveform
    // appearing is not worth queueing behind an export — but capped and niced.
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu::budget());
    let child = cmd
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let mut down = PeakDownsampler::new(buckets);
    // The pump kills a decode that stops producing for `READ_STALL`, which a bare read of its
    // stdout never did.
    super::peaks::pump_pcm_cancellable(child, 4, READ_STALL, cancel, &mut |samples| {
        for s in samples {
            down.push(s.abs());
        }
    })?;
    Ok(down.finish())
}

/// Folds an unbounded stream of sample magnitudes into exactly `buckets` peak
/// values, holding only `2*buckets` floats. Each incoming sample updates the
/// running peak of the current bucket; once the buffer fills it halves
/// resolution (merging adjacent buckets, doubling samples-per-bucket) and keeps
/// going — so the output is length-independent without knowing the total up
/// front. [`finish`] resamples the filled region down to `buckets`.
struct PeakDownsampler {
    buf: Vec<f32>,
    buckets: usize,
    write: usize,
    samples_per_bucket: u64,
    in_bucket: u64,
}

impl PeakDownsampler {
    fn new(buckets: usize) -> Self {
        let buckets = buckets.max(1);
        Self {
            buf: vec![0.0; buckets * 2],
            buckets,
            write: 0,
            samples_per_bucket: 1,
            in_bucket: 0,
        }
    }

    fn push(&mut self, magnitude: f32) {
        let a = magnitude.clamp(0.0, 1.0);
        self.buf[self.write] = self.buf[self.write].max(a);
        self.in_bucket += 1;
        if self.in_bucket < self.samples_per_bucket {
            return;
        }
        self.in_bucket = 0;
        self.write += 1;
        if self.write == self.buf.len() {
            // Buffer full: merge each adjacent pair into the first half (peak of
            // peaks), clear the rest, and halve the resolution.
            for i in 0..self.buckets {
                self.buf[i] = self.buf[2 * i].max(self.buf[2 * i + 1]);
            }
            for slot in self.buf[self.buckets..].iter_mut() {
                *slot = 0.0;
            }
            self.write = self.buckets;
            self.samples_per_bucket *= 2;
        }
    }

    fn finish(self) -> Vec<f32> {
        // `write` complete buckets plus an in-progress partial one, all at the
        // current resolution; collapse to exactly `buckets`.
        let len = self.write + if self.in_bucket > 0 { 1 } else { 0 };
        peaks(&self.buf[..len], self.buckets)
    }
}

fn peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; buckets];
    }
    let mut out = Vec::with_capacity(buckets);
    for b in 0..buckets {
        let lo = b * samples.len() / buckets;
        let hi = ((b + 1) * samples.len() / buckets).max(lo + 1).min(samples.len());
        let peak = samples[lo..hi].iter().fold(0.0_f32, |m, s| m.max(s.abs()));
        out.push(peak.clamp(0.0, 1.0));
    }
    out
}

/// Decode the first audio stream to 16 kHz mono f32 PCM (Whisper's input shape); gives up with
/// [`Error::Cancelled`] once `cancel` is true.
#[cfg(feature = "whisper")]
pub fn decode_audio_16k_mono_cancellable(path: &Path, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    decode_audio_mono_f32(path, 16_000, cancel)
}

/// Decode the first audio stream to mono f32 PCM at `sample_rate`, into memory. A whole-file
/// decode, so it takes the heavy-job slot (and says "cancelled" if `cancel` trips while it
/// waits for it), and it is killed after [`READ_STALL`] without a byte of audio.
pub(super) fn decode_audio_mono_f32(path: &Path, sample_rate: u32, cancel: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    let bin = ffmpeg_bin();
    // Decodes the whole stream *into memory* — eight of these at once is the
    // several gigabytes an unsupervised agent used to cost — so it is gated.
    let cpu = cpu::lease_waiting(&mut |_| {}, cancel)?;
    if cancel() {
        return Err(Error::Cancelled);
    }
    let mut cmd = bg_command(&bin);
    cpu::limit_cmd(&mut cmd, cpu.budget());
    let child = cmd
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let mut samples = Vec::new();
    super::peaks::pump_pcm_cancellable(child, 4, READ_STALL, cancel, &mut |frames| {
        samples.extend_from_slice(frames);
    })?;
    Ok(samples)
}

/// How long one [`audio_pcm`] window may take before it is killed: a window is seconds of sound
/// behind a fast seek, so a minute is a read that went wrong.
const AUDIO_PCM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Decode a window of the first audio stream to mono s16le PCM at
/// `sample_rate` — the GUI's Web Audio preview playback. Raw s16le (no
/// container) so the webview can build an `AudioBuffer` directly without
/// codec support; the input-side `-ss` fast-seek keeps a window deep in a
/// long source cheap to extract.
pub fn audio_pcm(path: &Path, start: f64, duration: f64, sample_rate: u32, filters: Option<&str>) -> Result<Vec<u8>> {
    audio_pcm_with(path, start, duration, sample_rate, filters, None)
}

/// [`audio_pcm`] that gives up with [`Error::Cancelled`] once `cancel` is true (the decode is
/// killed), for a caller that is itself a job someone can stop.
pub fn audio_pcm_cancellable(
    path: &Path,
    start: f64,
    duration: f64,
    sample_rate: u32,
    filters: Option<&str>,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    audio_pcm_with(path, start, duration, sample_rate, filters, Some(cancel))
}

fn audio_pcm_with(
    path: &Path,
    start: f64,
    duration: f64,
    sample_rate: u32,
    filters: Option<&str>,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Vec<u8>> {
    if cancel.is_some_and(|cancel| cancel()) {
        return Err(Error::Cancelled);
    }
    let bin = ffmpeg_bin();
    let mut cmd = command(&bin);
    // Interactive, like the frame decode: the preview's playback is waiting on it.
    cpu::limit_cmd(&mut cmd, cpu::budget());
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(["-ss", &start.max(0.0).to_string()])
        .arg("-i")
        .arg(path)
        .args([
            "-t",
            &duration.max(0.0).to_string(),
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            &sample_rate.to_string(),
        ]);
    // The clip's own effect chain, so the monitor hears the EQ / compressor /
    // gate the export will render rather than the dry source.
    if let Some(filters) = filters.filter(|f| !f.is_empty()) {
        cmd.args(["-af", filters]);
    }
    cmd.args(["-f", "s16le", "pipe:1"]);
    let output = output_bounded_cancellable(cmd, AUDIO_PCM_TIMEOUT, cancel)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "could not decode audio: ffmpeg did not answer in {} s and was stopped",
                AUDIO_PCM_TIMEOUT.as_secs()
            )),
            _ => launch_err(&bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "could not decode audio: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

// ---- preview proxies -------------------------------------------------------

/// Cap on the proxy's width, in pixels (`scale='min(W,iw)'`). ~720p: small
/// enough that one frame decodes in a few ms, large enough to preview framing.
const PROXY_MAX_WIDTH: u32 = 1280;

/// Cap on the proxy's width for spherical sources. A reframed preview crops
/// roughly a 100° window out of a 360° picture, so only about a quarter of the
/// proxy's width ever reaches the screen: at 1280 that leaves ~355 px of real
/// detail — visible mush. 3072 puts ~850 px across the shot while an all-intra
/// frame still decodes fast enough to scrub.
const PROXY_MAX_WIDTH_SPHERICAL: u32 = 3072;

/// The width a flat asset's proxy is rendered at: the default is
/// [`PROXY_MAX_WIDTH`], and Settings can pick a smaller one (or turn proxies off,
/// which is the caller's business — this is only the size). It is part of the cache
/// key, so a proxy made at another size is a different file and the default's
/// cache stays good.
static PROXY_BASE_WIDTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(PROXY_MAX_WIDTH);

/// The width a flat asset's proxy is rendered at right now (see [`proxy_width`]).
pub fn proxy_base_width() -> u32 {
    PROXY_BASE_WIDTH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Set the flat proxy width. Takes effect for every later lookup; an existing
/// proxy at the old size stays on disk, unused, until it is deleted.
pub fn set_proxy_base_width(width: u32) {
    PROXY_BASE_WIDTH.store(width.max(2), std::sync::atomic::Ordering::Relaxed);
}

/// The proxy width for an asset when flat proxies are `base` wide (pure,
/// unit-tested): `base` itself, or for a spherical source the same ratio the
/// default has (3072 for 1280), never past [`PROXY_MAX_WIDTH_SPHERICAL`]. (The cap dates from
/// proxies that were hardware-encoded, which stops at 4096 across; it stays.)
pub fn proxy_width_for(projection: Option<Projection>, base: u32) -> u32 {
    if projection.is_some_and(|p| p.is_spherical()) {
        (base.saturating_mul(PROXY_MAX_WIDTH_SPHERICAL) / PROXY_MAX_WIDTH).min(PROXY_MAX_WIDTH_SPHERICAL)
    } else {
        base
    }
}

/// The proxy width to render (and look up) an asset at. 360 footage is preserved
/// at a larger size because reframing throws most of the frame away.
pub fn proxy_width(projection: Option<Projection>) -> u32 {
    proxy_width_for(projection, proxy_base_width())
}

/// FNV-1a over `s`. A small, dependency-free, deterministic hash for naming a
/// source's proxy file — stability across sessions is what lets a re-import
/// reuse the cached proxy (a non-deterministic hasher would orphan it).
pub(super) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A content key for `src` that changes whenever the file is replaced: its path
/// plus size and modified time. Hashed into a cache file name so re-imports of
/// the same source reuse the cached artifact, while a swapped-out source
/// regenerates one. Shared by the proxy cache, the Insta360 stitch cache and
/// the waveform peak cache.
pub(crate) fn source_key(src: &Path) -> String {
    let meta = std::fs::metadata(src).ok();
    let len = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}|{len}|{mtime}", src.display())
}

/// Which file a cached decode came from, as one number: [`source_key`] (path, size and
/// modified time) hashed, so it changes when the file is replaced and a cache keyed by it
/// never serves a stale frame. A proxy and its original are different paths, so different
/// identities. A path that cannot be read hashes with size 0 and mtime 0: stable, and it
/// changes the moment the file appears.
pub fn source_identity(src: &Path) -> u64 {
    fnv1a(&source_key(src))
}

/// How long after the container's start the video's first frame lies, in
/// seconds: the video stream's start less the container's (the earliest of all
/// its streams — audio at 0 and video at 0.08 s gives 0.08).
///
/// The number matters because ffmpeg's input `-ss T` is **relative to the
/// container's start**, not to the video's. A source that starts its video late
/// is asked for source time `T` and answers with the frame `lead` seconds into
/// the video's own timeline; a proxy is video-only, so its container *is* the
/// video and starts where the video does — which would make the same `-ss T`
/// land `lead` seconds further into the footage (see [`build_proxy_args`]).
/// Negative or missing values are `0.0`.
fn head_lead(stream_start: Option<f64>, format_start: Option<f64>) -> f64 {
    match (stream_start, format_start) {
        (Some(stream), Some(format)) => (stream - format).max(0.0),
        _ => 0.0,
    }
}

/// The smallest lead worth a proxy of its own. Under a millisecond is a hair
/// inside the error of the container's own timestamps; anything above shifts a
/// seek by a whole frame for the stretch of each frame interval it covers.
const HEAD_PAD_MIN: f64 = 0.001;

/// A video stream's [`StreamInfo::lead`] from its start and the earliest start of the file's
/// picture and sound streams (pure): the [`head_lead`] when it is above [`HEAD_PAD_MIN`], else
/// `0.0`. Unlike [`SourceTraits::lead`] a transport stream is measured too: the export reads
/// its sound beside its picture.
pub(crate) fn video_lead(stream_start: Option<f64>, av_start: Option<f64>) -> f64 {
    Some(head_lead(stream_start, av_start))
        .filter(|&lead| needs_head_pad(lead))
        .unwrap_or(0.0)
}

/// A lead as the file's `duration` allows it (pure): one that reaches the end of the file is
/// a broken start time, not a picture that never comes, and reads as none — otherwise every
/// window would start inside it and no clip of the file would ever be seeked.
pub(crate) fn lead_within(lead: f64, duration: f64) -> f64 {
    if lead < duration {
        lead
    } else {
        0.0
    }
}

/// Whether a source with this [`head_lead`] needs its proxy's head filled in.
fn needs_head_pad(lead: f64) -> bool {
    lead > HEAD_PAD_MIN
}

/// The cache-key text of a proxy (pure, unit-tested): the source's identity and
/// the width, plus a suffix for each way the proxy was made differently from the
/// plain one. A suffix is only ever *added* to a source that needs it, so the
/// key of every ordinary SDR file with a normal start is what it always was and
/// its cached proxy stays good.
fn proxy_key(source_key: &str, width: u32, tonemapped: bool, head_padded: bool) -> String {
    let tone = if tonemapped { "|sdr" } else { "" };
    let pad = if head_padded { "|lead" } else { "" };
    format!("{source_key}|{width}{tone}{pad}")
}

/// The on-disk path of `src`'s preview proxy at `width` (whether or not it
/// exists yet): `<cache>/kerf/proxies/<hash>.mp4`. `None` when no OS cache
/// directory is resolvable (a proxy simply can't be cached — preview falls back
/// to the original).
///
/// The width is part of the key, so an asset that is later marked as 360 (or
/// stops being one) looks up a different file and regenerates instead of
/// silently reusing a proxy rendered at the wrong size.
///
/// An HDR source also keys on the fact that its proxy is **tone-mapped**: a
/// proxy cached before the engine did that is an untouched HDR picture squeezed
/// into BT.709, and must be rebuilt rather than trusted. SDR keys are unchanged.
///
/// So does a source whose video **starts after its container** (`|lead`): a proxy
/// cached before the engine filled that head in has the video starting at its own
/// time zero, and a seek into it lands on the wrong frame (see [`head_lead`]).
/// FFmpeg 9 wrote exactly such proxies for every build that had the default
/// frame-sync mode of its day. Only these sources are rebuilt, and their proxy is
/// named `<hash>.lead.mp4` (`is_head_padded_proxy`, in `clip_timing`).
pub fn proxy_path(src: &Path, width: u32) -> Option<PathBuf> {
    proxy_path_for(src, width, source_traits(src).unwrap_or_default())
}

/// [`proxy_path`] for traits already read: an encode takes **one** reading for its destination and
/// for its filters, because a probe that failed is remembered only for a minute and an encode that
/// waited for the machine's slot can read again, and answer differently, before it starts — a
/// tone-mapped or head-padded picture would then be written to the plain proxy's name.
fn proxy_path_for(src: &Path, width: u32, traits: SourceTraits) -> Option<PathBuf> {
    let dir = crate::paths::cache_dir()?.join("kerf").join("proxies");
    let padded = needs_head_pad(traits.lead);
    let key = proxy_key(&source_key(src), width, traits.hdr.is_some(), padded);
    Some(dir.join(proxy_file_name(fnv1a(&key), padded)))
}

/// Every file `src`'s proxy at `width` could be in the cache, with its sidecar: the four spellings
/// of the key (tone-mapped or not, head-padded or not), so it needs none of the probes
/// [`proxy_path`] runs and can be asked with a lock held. For the cache cap, which must not remove
/// what the open project plays from.
pub(crate) fn proxy_cache_files(src: &Path, width: u32) -> Vec<PathBuf> {
    let Some(dir) = crate::paths::cache_dir().map(|d| d.join("kerf").join("proxies")) else {
        return Vec::new();
    };
    let source = source_key(src);
    let mut files = Vec::with_capacity(8);
    for (tonemapped, padded) in [(false, false), (true, false), (false, true), (true, true)] {
        let proxy = dir.join(proxy_file_name(fnv1a(&proxy_key(&source, width, tonemapped, padded)), padded));
        files.push(proxy_sidecar_path(&proxy));
        files.push(proxy);
    }
    files
}

/// A proxy's file name (pure, unit-tested): `<hash>.mp4`, or `<hash>.lead.mp4` for
/// one made with a padded head.
fn proxy_file_name(hash: u64, head_padded: bool) -> String {
    if head_padded {
        format!("{hash:016x}{HEAD_PADDED_SUFFIX}")
    } else {
        format!("{hash:016x}.mp4")
    }
}

/// The proxy for `src` at `width` **if it has already been generated** (the file
/// exists), for a preview path to decode from instead of the original; `None`
/// otherwise. Preview must never block on generation, so this is a pure
/// existence check.
pub fn ready_proxy(src: &Path, width: u32) -> Option<PathBuf> {
    proxy_path(src, width).filter(|p| p.is_file())
}

/// Delete `src`'s proxy at `width` and the sidecar beside it, returning the bytes
/// freed (0 when there was none). A proxy that is being written is a `.part` file
/// and is left to the encode that owns it.
pub fn remove_proxy_files(src: &Path, width: u32) -> u64 {
    let Some(proxy) = proxy_path(src, width) else {
        return 0;
    };
    let freed: u64 = [proxy.clone(), proxy_sidecar_path(&proxy)]
        .iter()
        .filter_map(|file| {
            let bytes = std::fs::metadata(file).ok()?.len();
            std::fs::remove_file(file).ok().map(|()| bytes)
        })
        .sum();
    crate::plan_epoch::bump_plan_epoch();
    freed
}

/// How many CPU threads a single preview-proxy encode may use. Follows the
/// engine's CPU budget (see [`cpu`]), except that it never takes the whole
/// machine even at 100% — an uncapped `libx264` grabs every core, and a proxy
/// is background work the user did not ask to wait for. `KERF_PROXY_THREADS`
/// still overrides it outright (clamped to >= 1).
fn proxy_threads(budget: cpu::Budget) -> usize {
    if let Some(n) = std::env::var("KERF_PROXY_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.max(1);
    }
    budget.threads().min(budget.cores().saturating_sub(1).max(1)).max(1)
}

/// Constant-quality flags for encoder `vc` at software-CRF-scale `crf` — the
/// proxy / stitch analogue of the export's `push_video_opts`, spelling the same
/// intent per hardware family (each names its quality knob differently).
fn quality_args(vc: &str, crf: u32) -> Vec<String> {
    let s = |v: &str| v.to_string();
    match enc_family(vc) {
        EncFamily::Software => vec![s("-preset"), s("veryfast"), s("-crf"), crf.to_string()],
        EncFamily::Nvenc => vec![s("-rc"), s("vbr"), s("-cq"), crf.to_string(), s("-b:v"), s("0")],
        EncFamily::Qsv => vec![s("-global_quality"), crf.to_string()],
        EncFamily::VideoToolbox => vec![s("-q:v"), crf_to_vt_quality(crf).to_string()],
        // The QP is the CRF's number, not `hw_qp`'s: this spells the stitch, a source that is
        // decoded again, where the extra bits cost disk and not picture. The export is what
        // the offset is for.
        EncFamily::Amf => {
            let qp = crf.to_string();
            vec![s("-rc"), s("cqp"), s("-qp_i"), qp.clone(), s("-qp_p"), qp]
        }
        EncFamily::Vaapi => unreachable!("a stitch or a proxy has no upload for a VAAPI encoder"),
    }
}

/// Encoder input pixel format: the hardware encoders all take `nv12` (some take
/// nothing else), the software path keeps its historical `yuv420p`. Both are
/// 4:2:0, so the decoded proxy looks the same either way. (A VAAPI encoder is fed by an
/// upload the export graph ends in, which a stitch and a proxy do not have.)
fn encode_pix_fmt(vc: &str) -> &'static str {
    match enc_family(vc) {
        EncFamily::Software => "yuv420p",
        EncFamily::Vaapi => unreachable!("a stitch or a proxy has no upload for a VAAPI encoder"),
        _ => "nv12",
    }
}

/// The proxy's argv as [`generate_proxy_with`] runs it: [`build_proxy_args`] with the encoder
/// fixed to `libx264`. A proxy is never hardware-encoded, whatever encoders the machine has
/// (pinned by a test over every combination of the other inputs).
fn proxy_encode_args(
    src: &str,
    dst: &str,
    threads: usize,
    width: u32,
    hw_decode: Option<&str>,
    tonemap: Option<&str>,
    head_pad: Option<&str>,
) -> Vec<String> {
    build_proxy_args(src, dst, threads, width, "libx264", hw_decode, tonemap, head_pad)
}

/// Build the ffmpeg argument list (pure, unit-tested) that transcodes `src` into
/// an all-intra, audio-less preview proxy at `dst`, capped to `width` pixels
/// across (see [`proxy_width`]) and using at most `threads` CPU threads.
/// `encoder` is `libx264`: [`generate_proxy_with`] never hands a proxy to a hardware
/// encoder (the builder still spells their quality flags); `hw_decode` adds an
/// input-side `-hwaccel`. `-g 1` makes every frame a keyframe, so a seek decodes
/// exactly one frame (instant scrub even on long-GOP 4K/HEVC). fps and duration
/// are left untouched — no `-r`, no `-t`, no trim — so a source time maps 1:1
/// onto the proxy and a preview seek lands on the same frame the export (which
/// always reads the original) would. `tonemap`, for an HDR source, is the chain
/// that brings it down to SDR BT.709 after the downscale: the **proxy is where
/// that conversion happens**, so everything that decodes it afterwards — the
/// preview stream, scrubbed stills, the composited frame — sees ordinary SDR and
/// converts nothing a second time. (Upright rotation is the decoder's: ffmpeg
/// autorotates and writes the proxy without a matrix.)
///
/// `head_pad` is for a source whose video starts after its container (see
/// [`head_lead`]), and carries the spelling of the frame-sync flag
/// ([`fps_mode_flag`]). The proxy has no audio to start the container early, so
/// without help its video would start at its own zero and an input `-ss T` would
/// land `lead` seconds past where the original's does — which is what an
/// FFmpeg-9 proxy of a late-starting video did. The fix keeps every timestamp
/// exactly and fills the gap the way the original answers a seek into it, by
/// holding the first frame: one clone of it at time zero, merged in front of the
/// stream by `interleave` (which orders by timestamp, so nothing is regridded).
/// That is not `-fps_mode cfr`, the other way to fill a head: cfr snaps every
/// frame to a grid anchored at zero, which is half a frame off for most leads
/// (a seek then picks the neighbouring frame at every frame boundary) and turns
/// a variable-frame-rate source into a constant one. The mode is spelled out as
/// `vfr` because FFmpeg 6 and older default an mp4 to cfr — which would undo it —
/// and FFmpeg 7 and newer to vfr. An ordinary source passes `None` and gets the
/// plain `-vf` chain it always did. The clone is a frame the original has no
/// counterpart for, so a reader that starts the proxy from zero without a seek —
/// which the original answers with its real first frame — drops it again (see
/// [`video_clip_chain`]); the proxy's file name says it has one
/// (`is_head_padded_proxy`, in `clip_timing`).
#[allow(clippy::too_many_arguments)]
fn build_proxy_args(
    src: &str,
    dst: &str,
    threads: usize,
    width: u32,
    encoder: &str,
    hw_decode: Option<&str>,
    tonemap: Option<&str>,
    head_pad: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
    ];
    if let Some(hw) = hw_decode {
        args.push("-hwaccel".to_string());
        args.push(hw.to_string());
    }
    let chain = match tonemap {
        Some(t) => format!("scale='min({width},iw)':-2:flags=bilinear,{t}"),
        None => format!("scale='min({width},iw)':-2:flags=bilinear"),
    };
    args.extend(["-i".to_string(), src.to_string(), "-an".to_string()]);
    if head_pad.is_some() {
        args.extend([
            "-filter_complex".to_string(),
            format!("[0:v:0]{chain},split[a][b];[a]trim=end_frame=1,setpts=PTS-STARTPTS[h];[h][b]interleave[v]"),
            "-map".to_string(),
            "[v]".to_string(),
        ]);
    } else {
        args.extend(["-vf".to_string(), chain]);
    }
    args.extend(["-c:v".to_string(), encoder.to_string()]);
    args.extend(quality_args(encoder, 24));
    args.extend([
        "-g".to_string(),
        "1".to_string(),
        "-threads".to_string(),
        threads.max(1).to_string(),
        "-pix_fmt".to_string(),
        encode_pix_fmt(encoder).to_string(),
    ]);
    if let Some(flag) = head_pad {
        args.extend([flag.to_string(), "vfr".to_string()]);
    }
    args.extend([
        // The encode writes a `.part` temp file, whose extension tells ffmpeg
        // nothing — name the muxer instead of letting it guess, or it exits
        // with "unable to find a suitable output format" before decoding a frame.
        "-f".to_string(),
        "mp4".to_string(),
        dst.to_string(),
    ]);
    args
}

/// Generate the preview proxy for `src` if it isn't cached yet, returning its
/// path. A cache hit (the proxy already on disk) returns immediately — so this
/// is cheap to call for every asset on project open. The encode writes to a
/// per-process temp file and atomically renames it into place, so a partial /
/// interrupted / concurrent encode never leaves a half-written file that
/// [`ready_proxy`] would mistake for a finished proxy. Blocking; callers run it
/// off the project lock (e.g. on a background thread).
pub fn generate_proxy(src: &Path, width: u32) -> Result<PathBuf> {
    generate_proxy_with(
        src,
        width,
        ProxyRun {
            reservation: None,
            duration: None,
            progress: &mut |_| {},
            cancel: &|| false,
        },
    )
}

/// What a caller hooks into a proxy encode (see [`generate_proxy_with`]).
pub struct ProxyRun<'a> {
    /// The place in front of the heavy-job queue the caller took when it queued the
    /// proxy ([`cpu::reserve`]); without one the encode asks for the slot in the
    /// same lane when it starts.
    pub reservation: Option<cpu::Reservation>,
    /// The source's length in seconds — what the encoder's position is measured
    /// against to give `progress` a fraction. Without it nothing is reported.
    pub duration: Option<f64>,
    /// Called with how far along the encode is, `0.0..1.0`: once with `0.0` when it holds the
    /// slot and starts (so a proxy queued behind an export is not shown at 0%), then about twice
    /// a second.
    pub progress: &'a mut dyn FnMut(f64),
    /// Polled while queued and while encoding; once true the encode is killed, its
    /// partial file removed and [`Error::Cancelled`] returned.
    pub cancel: &'a dyn Fn() -> bool,
}

/// [`generate_proxy`] that reports progress and can be abandoned.
///
/// The encode is a whole-file job in the **proxy** lane of the heavy-job queue: a proxy
/// is what the preview is waiting for (it decodes the original until the proxy lands),
/// where analysis is only wanted eventually, so it goes before every analysis that is
/// waiting — but after an export or anything else a user is waiting on. A job already
/// running is not interrupted.
///
/// It runs with `-progress` on a pipe (written here at spawn time, never in
/// [`build_proxy_args`]) and is killed after [`EXPORT_STALL`] without a word, so a
/// wedged decoder cannot hold the machine's one heavy-job slot for good.
pub fn generate_proxy_with(src: &Path, width: u32, run: ProxyRun<'_>) -> Result<PathBuf> {
    let ProxyRun {
        reservation,
        duration,
        progress,
        cancel,
    } = run;
    // One reading of the source answers everything below: where the proxy goes, whether to tone-map
    // it and whether its video starts late.
    let traits = source_traits(src).unwrap_or_default();
    let dst = proxy_path_for(src, width, traits)
        .ok_or_else(|| Error::Engine("no cache directory available for preview proxies".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }
    if cancel() {
        return Err(Error::Cancelled);
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create proxy cache dir: {e}")))?;
    }
    let src_str = src
        .to_str()
        .ok_or_else(|| Error::Engine("asset path is not valid UTF-8".to_string()))?;
    // One name per encode: a rebuild that cancels the encode before it must not share a
    // temp file with the one that replaces it.
    static ENCODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = ENCODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dst.with_extension(format!("{}.{n}.part", std::process::id()));
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| Error::Engine("proxy temp path is not valid UTF-8".to_string()))?;
    let bin = ffmpeg_bin();
    // A full-file re-encode. Importing a folder queues them one behind the next
    // rather than starting one per file at once; they wait in the proxy lane, behind
    // anything a user is waiting on and ahead of analysis. A proxy that is deleted, rebuilt
    // or switched off while it waits gives up its place without having started.
    let cpu = match reservation {
        Some(reserved) => reserved.lease_waiting(&mut |_| {}, cancel)?,
        None => cpu::lease_priority_waiting(cpu::Priority::Proxy, &mut |_| {}, cancel)?,
    };
    // Cancelled in the instant it got the slot: do not start the encode.
    if cancel() {
        return Err(Error::Cancelled);
    }
    // The first report says the encode has begun (it held no slot until now).
    if duration.is_some() {
        progress(0.0);
    }
    let granted = cpu.budget();
    let threads = proxy_threads(granted);
    let tonemap = traits.hdr.map(tonemap_filter);
    let head_pad = needs_head_pad(traits.lead).then(fps_mode_flag);
    let mut run = |hw_decode: Option<&str>| -> Result<Streamed> {
        let mut args = proxy_encode_args(src_str, tmp_str, threads, width, hw_decode, tonemap.as_deref(), head_pad);
        cpu::limit_args(&mut args, granted.with_threads(threads));
        run_ffmpeg_streamed(&bin, &args, duration, EXPORT_STALL, &mut *progress, cancel)
    };
    // The decode is the GPU's when there is one; the encode never is. A hardware H.264 intra
    // pass costs the same wall time as x264's (1.85 s against 1.90 s for 8 s of 4K), saves
    // about 0.4 CPU-s, writes proxies a third larger, and one refusal used to turn hardware
    // encoding off for the whole process. A decode failure falls back to software once.
    let hw_dec = decode_hwaccel_for(src, Reading::Whole);
    let mut output = run(hw_dec.as_deref())?;
    if output.outcome == StreamOutcome::Cancelled {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Cancelled);
    }
    if retry_proxy_in_software(output.outcome, output.status.success(), hw_dec.is_some()) {
        let _ = std::fs::remove_file(&tmp);
        let err = if output.outcome == StreamOutcome::Stalled {
            // A hardware decoder that hangs is as broken as one that refuses, and costs five
            // minutes a proxy to find out: turn it off for the process, like any other
            // hardware failure, and go again in software.
            disable_decode_hwaccel();
            format!("ffmpeg stopped reporting progress for {}s", EXPORT_STALL.as_secs())
        } else {
            output.stderr.trim().to_string()
        };
        output = run(None)?;
        if output.outcome == StreamOutcome::Cancelled {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::Cancelled);
        }
        if output.status.success() {
            // The decoder is the culprit only if the failure reads as a hardware one: a named
            // accelerator such as `d3d11va` exits non-zero on a machine with no device, where
            // `auto` fell back quietly, so without this every proxy would pay a failed launch
            // first; a full disk or a bad file must not turn it off for the process. (A
            // *stall* is convicted before the rerun, as it always was.)
            if looks_like_hardware_decode_failure(&err) {
                convict_hardware_decode();
            }
            tracing::warn!(error = %err, "hardware-accelerated proxy decode failed; using software");
        }
    }
    if output.outcome == StreamOutcome::Stalled {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Engine(format!(
            "could not generate preview proxy: ffmpeg stopped reporting progress for {}s and was stopped",
            EXPORT_STALL.as_secs()
        )));
    }
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Engine(format!(
            "could not generate preview proxy: {}",
            output.stderr.trim()
        )));
    }
    // Another generator may have finished the same proxy while we encoded: theirs is in place
    // with its own sidecar, and ours is redundant.
    finalize_proxy(&tmp, &dst, || {
        probe(&tmp)
            .ok()
            .and_then(|p| p.streams.into_iter().find(|s| s.kind == StreamKind::Video))
    })
}

/// Whether a proxy encode that used hardware decode is run again in software (pure,
/// unit-tested): when it failed on its own **or stalled** — a cancelled one is not a failure,
/// and an encode that was software already has nothing to fall back to.
fn retry_proxy_in_software(outcome: StreamOutcome, success: bool, hw_decode: bool) -> bool {
    !success && matches!(outcome, StreamOutcome::Ended | StreamOutcome::Stalled) && hw_decode
}

/// How a streamed ffmpeg run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamOutcome {
    /// ffmpeg exited on its own (successfully or not — see the status).
    Ended,
    /// The cancel callback tripped and ffmpeg was killed.
    Cancelled,
    /// ffmpeg said nothing for [`EXPORT_STALL`] and was killed.
    Stalled,
}

/// The result of [`run_ffmpeg_streamed`].
pub(super) struct Streamed {
    pub(super) status: std::process::ExitStatus,
    pub(super) stderr: String,
    pub(super) outcome: StreamOutcome,
}

/// Run `ffmpeg` with `args`, reading its `-progress` stream: `progress` gets the
/// encoder's position as a fraction of `duration` (when it is known), `cancel` is
/// polled between reports and while ffmpeg is silent, and a run that says nothing
/// for `stall` ([`EXPORT_STALL`] in production) is killed. A bounded cousin of `Command::output()` for the
/// whole-file jobs that outlive a glance — stderr is drained on a side thread so a
/// warning flood cannot fill its pipe and wedge the child.
fn run_ffmpeg_streamed(
    bin: &str,
    args: &[String],
    duration: Option<f64>,
    stall: std::time::Duration,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<Streamed> {
    run_ffmpeg_streamed_in(bin, args, None, duration, stall, progress, cancel)
}

/// [`run_ffmpeg_streamed`] with ffmpeg's working directory set to `cwd` (the whisper filter
/// is handed bare file names so that no path has to survive filter-argument escaping).
pub(super) fn run_ffmpeg_streamed_in(
    bin: &str,
    args: &[String],
    cwd: Option<&Path>,
    duration: Option<f64>,
    stall: std::time::Duration,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> Result<Streamed> {
    use std::io::{BufRead, BufReader, Read};

    let mut command = bg_command(bin);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .args(["-progress", "pipe:1", "-stats_period", "0.5"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(bin, e))?;
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        text
    });
    let stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let total = duration.filter(|d| d.is_finite() && *d > 0.0);
    let mut outcome = StreamOutcome::Ended;
    let mut last_output = Instant::now();
    loop {
        match rx.recv_timeout(CANCEL_POLL) {
            Ok(line) => {
                last_output = Instant::now();
                if line == "progress=end" {
                    break;
                }
                if let (Some(total), Some(us)) = (
                    total,
                    line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<i64>().ok()),
                ) {
                    // Held under 1: the file is not in place until it is renamed.
                    progress((us.max(0) as f64 / 1_000_000.0 / total).clamp(0.0, 0.99));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if last_output.elapsed() > stall {
                    outcome = StreamOutcome::Stalled;
                    break;
                }
            }
        }
        if cancel() {
            outcome = StreamOutcome::Cancelled;
            break;
        }
    }
    if outcome != StreamOutcome::Ended {
        let _ = child.kill();
    }
    let status = reap(&mut child)?;
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(Streamed { status, stderr, outcome })
}

/// Put the finished encode `tmp` in place as `dst`, with its sidecar. If `dst` is already there
/// (a concurrent generator got there first) it is left as it is — **its sidecar stays too, no
/// new one is written** — and `tmp` is dropped. Otherwise `probe` describes `tmp` and the
/// sidecar is written *before* the rename, so a reader that finds the proxy finds its
/// description; a failed probe only costs the sidecar (the reader probes once and writes it).
fn finalize_proxy(tmp: &Path, dst: &Path, probe: impl FnOnce() -> Option<StreamInfo>) -> Result<PathBuf> {
    if dst.is_file() {
        let _ = std::fs::remove_file(tmp);
        return Ok(dst.to_path_buf());
    }
    if let Some(video) = probe() {
        write_proxy_sidecar(dst, tmp, video);
    }
    std::fs::rename(tmp, dst).map_err(|e| Error::Engine(format!("could not finalize preview proxy: {e}")))?;
    crate::plan_epoch::bump_plan_epoch();
    Ok(dst.to_path_buf())
}

/// What `generate_proxy` writes beside a proxy (`<hash>.json` next to `<hash>.mp4`, the
/// same name a `.lead.mp4` proxy's sidecar takes with its own ending): the video stream
/// **of the proxy file** — its size, pixel format and colour tags, not the original's.
///
/// A proxy is a smaller, always-`yuv420p`, upright picture, so what a renderer that decodes
/// it has to know about its layer (geometry, a format it refuses, a matrix) is a fact about
/// the proxy. Asking `ffprobe` for it would put a process spawn on the interactive path; the
/// sidecar makes it a small file read. It carries the proxy's byte size so a proxy that was
/// replaced under the same name does not keep describing the one that is gone.
#[derive(serde::Serialize, serde::Deserialize)]
struct ProxySidecar {
    version: u32,
    size: u64,
    video: StreamInfo,
}

const PROXY_SIDECAR_VERSION: u32 = 1;

/// Where the sidecar of `proxy` lives.
pub(crate) fn proxy_sidecar_path(proxy: &Path) -> PathBuf {
    proxy.with_extension("json")
}

/// A temp name for writing the sidecar at `dst`, **unique per writer** (the process and a
/// counter: two threads writing the same sidecar must not share one) and not the proxy's own
/// (`<hash>.<pid>.part`), which the write runs beside.
fn sidecar_temp(dst: &Path) -> PathBuf {
    static WRITERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dst.with_extension(format!("json.{}.{n}.part", std::process::id()))
}

/// Write `proxy`'s sidecar, describing `file` (the proxy itself, or the temp it is being
/// written as). Best effort and atomic (temp file, rename): the worst a failure does is make
/// the reader probe.
fn write_proxy_sidecar(proxy: &Path, file: &Path, video: StreamInfo) {
    let Ok(size) = std::fs::metadata(file).map(|m| m.len()) else {
        return;
    };
    let sidecar = ProxySidecar {
        version: PROXY_SIDECAR_VERSION,
        size,
        video,
    };
    let dst = proxy_sidecar_path(proxy);
    let tmp = sidecar_temp(&dst);
    let written = serde_json::to_vec(&sidecar)
        .ok()
        .is_some_and(|bytes| std::fs::write(&tmp, bytes).is_ok())
        && std::fs::rename(&tmp, &dst).is_ok();
    if !written {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The sidecar of `proxy` if it is there, current and describes this very file.
fn read_proxy_sidecar(proxy: &Path) -> Option<StreamInfo> {
    let sidecar: ProxySidecar = serde_json::from_slice(&std::fs::read(proxy_sidecar_path(proxy)).ok()?).ok()?;
    let size = std::fs::metadata(proxy).ok()?.len();
    (sidecar.version == PROXY_SIDECAR_VERSION && sidecar.size == size).then_some(sidecar.video)
}

/// How long a failed probe of a proxy is remembered before it is tried again.
const PROXY_PROBE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// The video stream of the proxy at `proxy`, as decoding it yields it: from its sidecar, and
/// for a proxy that has none (one made before sidecars existed) from **one** `ffprobe`,
/// remembered per file (size and modified time) and written back as the sidecar so no later
/// run asks again. `None` when the proxy cannot be read; the caller decodes the original.
///
/// The fallback spawns a process: call it off the project lock, never from a render loop.
/// A plan resolves it once per `Planner`.
pub(crate) fn proxy_video_info(proxy: &Path) -> Option<StreamInfo> {
    /// What one probe of a proxy found, and when.
    type Probed = (Instant, Option<StreamInfo>);
    static CACHE: OnceLock<Mutex<HashMap<String, Probed>>> = OnceLock::new();
    if let Some(video) = read_proxy_sidecar(proxy) {
        return Some(video);
    }
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = source_key(proxy);
    let remembered = cache.lock().ok().and_then(|c| c.get(&key).cloned());
    if let Some((at, found)) = remembered {
        if found.is_some() || at.elapsed() < PROXY_PROBE_RETRY {
            return found;
        }
    }
    let found = probe(proxy)
        .ok()
        .and_then(|p| p.streams.into_iter().find(|s| s.kind == StreamKind::Video));
    if let Some(video) = &found {
        write_proxy_sidecar(proxy, proxy, video.clone());
    }
    if let Ok(mut c) = cache.lock() {
        c.insert(key, (Instant::now(), found.clone()));
    }
    found
}

// ---- insta360 dual-lens stitching ------------------------------------------

/// The equirect frame a lens pair is stitched into. Each lens is a square
/// fisheye covering [`STITCH_FOV`] degrees, so a 3072x3072 lens carries about
/// 16 px per degree — roughly 5800 px around the full circle. 5760x2880 is
/// therefore the size that keeps the capture's detail without inventing any
/// (and is what Insta360 Studio itself exports a 5.7K capture at).
const STITCH_WIDTH: u32 = 5760;
const STITCH_HEIGHT: u32 = 2880;

/// Per-lens field of view, in degrees. Insta360's lenses overshoot the
/// hemisphere so the two circles overlap at the seam; `v360` needs the real
/// figure or the halves meet with a gap.
const STITCH_FOV: u32 = 190;

/// The lens token of an Insta360 capture file name and the name of its other
/// lens: a capture is written as a *pair* of files whose second-to-last
/// underscore-separated token is `00` (front) or `10` (rear) —
/// `VID_20220625_140410_00_008.mp4` / `..._10_008.mp4`.
///
/// Matched token-wise rather than by searching for `_00_`, so a capture whose
/// date or time happens to contain those digits can't be misread.
pub(crate) fn insta360_lens(file_name: &str) -> Option<(&'static str, String)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("mp4") || !stem.starts_with("VID_") {
        return None;
    }
    // `VID_<date>_<time>_<lens>_<sequence>` — anything shorter, or with an empty
    // trailing sequence, is not a capture file whatever its middle tokens say.
    let mut tokens: Vec<&str> = stem.split('_').collect();
    if tokens.len() < 4 || tokens.last().is_none_or(|t| t.is_empty()) {
        return None;
    }
    let lens_at = tokens.len() - 2;
    let (lens, other) = match tokens[lens_at] {
        "00" => ("00", "10"),
        "10" => ("10", "00"),
        _ => return None,
    };
    tokens[lens_at] = other;
    Some((lens, format!("{}.{ext}", tokens.join("_"))))
}

/// The display name for a stitched pair: the capture name with the lens token
/// dropped, so the bin shows one `VID_20220625_140410_008.mp4` rather than
/// whichever lens file happened to be imported.
pub(crate) fn insta360_pair_name(file_name: &str) -> Option<String> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    insta360_lens(file_name)?;
    let mut tokens: Vec<&str> = stem.split('_').collect();
    tokens.remove(tokens.len() - 2);
    Some(format!("{}.{ext}", tokens.join("_")))
}

/// The `(front, rear)` lens files of the Insta360 capture `path` belongs to, if
/// it is one: a square video frame (one fisheye circle per file) whose sibling
/// lens is on disk next to it. Either half resolves to the same canonical pair,
/// so importing either file stitches — and caches — the same sphere.
pub fn insta360_pair(path: &Path, width: Option<u32>, height: Option<u32>) -> Option<(PathBuf, PathBuf)> {
    let (w, h) = (width?, height?);
    if w == 0 || w != h {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    let (lens, sibling_name) = insta360_lens(name)?;
    let sibling = path.with_file_name(sibling_name);
    if !sibling.is_file() {
        return None;
    }
    let this = path.to_path_buf();
    Some(if lens == "00" { (this, sibling) } else { (sibling, this) })
}

/// Cache key for a stitched pair: both lens files' identity plus a version salt,
/// so changing the stitch recipe below invalidates everything stitched by an
/// older build instead of silently reusing it.
fn stitch_key(front: &Path, rear: &Path) -> String {
    format!("{}||{}||v1", source_key(front), source_key(rear))
}

/// Where the stitched equirect for a lens pair lives (whether or not it exists
/// yet): `<cache>/kerf/stitched/<hash>.mp4`. Alongside the proxy cache rather
/// than next to the originals — capture media is routinely on a read-only or
/// ejected SD card, and a deterministic key lets a re-import reuse the stitch
/// instead of spending minutes re-encoding it.
pub fn stitched_path(front: &Path, rear: &Path) -> Option<PathBuf> {
    let dir = crate::paths::cache_dir()?.join("kerf").join("stitched");
    Some(dir.join(format!("{:016x}.mp4", fnv1a(&stitch_key(front, rear)))))
}

/// Build the ffmpeg argument list (pure, unit-tested) that stitches the two
/// fisheye lens files into one equirectangular video at `dst`.
///
/// `hstack` packs the pair into the dual-fisheye layout `v360` expects (front
/// left), `roll=180` corrects the sensor orientation — Insta360 records both
/// lenses upside down — and `shortest` resolves the few-frames-different
/// durations the two files are written with. The front file's audio is copied
/// through untouched; its telemetry/subtitle stream is dropped.
///
/// CRF 15 rather than a lossless or visually-lossy setting: this file becomes
/// the effective source for every later reframe and export, so it must not be
/// the quality floor, while `veryfast` keeps a capture's import to minutes.
/// With a hardware `encoder` the same quality intent is mapped per family (see
/// [`quality_args`]) and the multi-minute re-encode moves onto the GPU;
/// `hw_decode` accelerates the two lens decodes the same way.
pub(crate) fn build_stitch_args(front: &str, rear: &str, dst: &str, encoder: &str, hw_decode: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
    ];
    // `-hwaccel` is an input option: emit it before each lens file's `-i`.
    for lens in [front, rear] {
        if let Some(hw) = hw_decode {
            args.push("-hwaccel".to_string());
            args.push(hw.to_string());
        }
        args.push("-i".to_string());
        args.push(lens.to_string());
    }
    args.extend([
        "-filter_complex".to_string(),
        format!(
            "[0:v][1:v]hstack=shortest=1,v360=dfisheye:e:ih_fov={STITCH_FOV}:iv_fov={STITCH_FOV}:roll=180:w={STITCH_WIDTH}:h={STITCH_HEIGHT}[v]"
        ),
        "-map".to_string(),
        "[v]".to_string(),
        "-map".to_string(),
        "0:a?".to_string(),
        "-c:v".to_string(),
        encoder.to_string(),
    ]);
    args.extend(quality_args(encoder, 15));
    args.extend([
        "-pix_fmt".to_string(),
        encode_pix_fmt(encoder).to_string(),
        "-c:a".to_string(),
        "copy".to_string(),
        "-shortest".to_string(),
        // The encode writes a `.part` temp file, whose extension tells ffmpeg
        // nothing — name the muxer instead of letting it guess.
        "-f".to_string(),
        "mp4".to_string(),
        dst.to_string(),
    ]);
    args
}

/// The hardware encoder for stitching, when available: an **HEVC** one, because
/// the 5760-wide equirect frame exceeds H.264 NVENC's 4096-wide ceiling while
/// every HEVC hardware encoder handles 8K. `None` falls back to libx264.
fn stitch_hw_encoder() -> Option<&'static str> {
    if !HW_ENCODE_OK.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    HW_ENCODER_CANDIDATES
        .iter()
        .find(|e| e.starts_with("hevc_") && hw_encoders().iter().any(|h| h == *e))
        .copied()
}

/// The stitches in flight, by the cache file each is making. Importing both lens files at once
/// (the obvious thing to do in a file dialog) would otherwise run the same multi-minute encode
/// twice and throw one away; the loser of the race waits for the claim and then finds the
/// finished file in the cache. A claim is a *name in a set*, not a lock held across the encode:
/// the mutex is taken for an insert or a remove, never while ffmpeg runs.
struct Stitching {
    running: Mutex<std::collections::HashSet<PathBuf>>,
    finished: std::sync::Condvar,
}

fn stitching() -> &'static Stitching {
    static STITCHING: OnceLock<Stitching> = OnceLock::new();
    STITCHING.get_or_init(|| Stitching {
        running: Mutex::new(std::collections::HashSet::new()),
        finished: std::sync::Condvar::new(),
    })
}

/// A pair's stitch claim; dropping it lets the next waiter go.
struct StitchClaim(PathBuf);

impl Drop for StitchClaim {
    fn drop(&mut self) {
        let stitching = stitching();
        stitching.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
        stitching.finished.notify_all();
    }
}

/// Claim the right to make `dst`, waiting (in slices, so `cancel` is heard) while another
/// stitch of the same pair holds it. `None`: it finished while this one waited, and `dst` is
/// there to use.
fn claim_stitch(dst: &Path, cancel: &dyn Fn() -> bool) -> Result<Option<StitchClaim>> {
    let stitching = stitching();
    let mut running = stitching.running.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if dst.is_file() {
            return Ok(None);
        }
        if running.insert(dst.to_path_buf()) {
            return Ok(Some(StitchClaim(dst.to_path_buf())));
        }
        if cancel() {
            return Err(Error::Cancelled);
        }
        running = stitching
            .finished
            .wait_timeout(running, std::time::Duration::from_millis(100))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Stitch an Insta360 lens pair into a single equirectangular file, returning
/// its cached path; a pair already stitched returns immediately. `duration_hint`
/// (the capture's length in seconds) scales the `progress` bar. Blocking and
/// slow — a full re-encode — so callers run it off the project lock.
///
/// Like the proxy cache, the encode writes a per-process temp file and renames
/// it into place, so an interrupted or concurrent stitch never leaves a
/// half-written file behind for the next import to mistake for a finished one.
pub fn stitch_insta360(
    front: &Path,
    rear: &Path,
    duration_hint: f64,
    progress: &mut dyn FnMut(ExportProgress),
) -> Result<PathBuf> {
    stitch_insta360_with(front, rear, duration_hint, progress, &|| false)
}

/// [`stitch_insta360`] that can be abandoned: `cancel` is polled while the stitch waits for the
/// same pair's other stitch, while it waits for the heavy-job slot, and while ffmpeg encodes
/// (about twice a second); once true the encode is killed, its temp file removed and
/// [`Error::Cancelled`] returned. A stalled encode (no `-progress` for [`EXPORT_STALL`]) is an
/// error, as in an export.
pub fn stitch_insta360_with(
    front: &Path,
    rear: &Path,
    duration_hint: f64,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    let dst = stitched_path(front, rear)
        .ok_or_else(|| Error::Engine("no cache directory available for stitched 360 media".to_string()))?;
    if dst.is_file() {
        return Ok(dst);
    }

    // The same pair's other stitch holds the claim for minutes: wait for it in slices, so a Stop
    // works on the one that is only waiting, and take its result when it lands.
    let Some(_claim) = claim_stitch(&dst, cancel)? else {
        return Ok(dst);
    };

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Engine(format!("could not create stitch cache dir: {e}")))?;
    }
    let (Some(front_str), Some(rear_str)) = (front.to_str(), rear.to_str()) else {
        return Err(Error::Engine("lens file path is not valid UTF-8".to_string()));
    };
    let tmp = dst.with_extension(format!("{}.part", std::process::id()));
    let tmp_str = tmp
        .to_str()
        .ok_or_else(|| Error::Engine("stitch temp path is not valid UTF-8".to_string()))?;

    tracing::info!(front = %front.display(), rear = %rear.display(), "stitching insta360 lens pair");
    let attempt = |encoder: &str, hw_decode: Option<&str>, progress: &mut dyn FnMut(ExportProgress)| {
        let args = build_stitch_args(front_str, rear_str, tmp_str, encoder, hw_decode);
        run_ffmpeg_progress(
            &args,
            &dst,
            Bar {
                total: duration_hint.max(1e-9),
                offset: 0.0,
                width: 1.0,
                start: std::time::Instant::now(),
            },
            progress,
            cancel,
        )
    };
    // GPU-encode the stitch when a verified HEVC hardware encoder exists (the
    // full re-encode drops from minutes towards realtime); one failure falls
    // back to the software pipeline and disables hardware encodes.
    let hw_enc = stitch_hw_encoder();
    let hw_dec = decode_hwaccel();
    let mut result = attempt(hw_enc.unwrap_or("libx264"), hw_dec.as_deref(), progress);
    if result.is_err() && (hw_enc.is_some() || hw_dec.is_some()) {
        tracing::warn!(error = ?result.as_ref().err(), "hardware-accelerated stitch failed; retrying in software");
        if hw_enc.is_some() {
            HW_ENCODE_OK.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        let _ = std::fs::remove_file(&tmp);
        result = attempt("libx264", None, progress);
    }
    // A cancelled run is not a finished one: `run_ffmpeg_progress` reports it as `Ok`, and its
    // half-written file must not be renamed into the cache for the next import to find.
    if !matches!(result, Ok(RenderStatus::Completed)) {
        let _ = std::fs::remove_file(&tmp);
    }
    if matches!(result, Ok(RenderStatus::Cancelled)) {
        return Err(Error::Cancelled);
    }
    result?;
    if dst.is_file() {
        let _ = std::fs::remove_file(&tmp);
        return Ok(dst);
    }
    std::fs::rename(&tmp, &dst).map_err(|e| Error::Engine(format!("could not finalize stitched 360 media: {e}")))?;
    Ok(dst)
}

// ---- export ----------------------------------------------------------------

/// Output container / muxer. Authoritative over the output path extension; it
/// gates the codec allow-lists, faststart, the gif palette pipeline and whether
/// a video / audio stream is produced at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    #[default]
    Mp4,
    Mov,
    Mkv,
    Webm,
    Gif,
    Mp3,
    M4a,
    Wav,
    Flac,
}

impl Container {
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
            Self::Gif => "gif",
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }
    /// mp4 / mov / m4a benefit from a front-loaded moov atom; nothing else does.
    pub fn supports_faststart(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov | Self::M4a)
    }
    /// Audio-only containers never carry a video stream.
    pub fn is_audio_only(self) -> bool {
        matches!(self, Self::Mp3 | Self::M4a | Self::Wav | Self::Flac)
    }
    /// Gif is the only video-only container (no audio stream).
    pub fn is_video_only(self) -> bool {
        matches!(self, Self::Gif)
    }
    pub fn video_codecs(self) -> &'static [&'static str] {
        // Hardware encoders sit alongside the software ones — they emit the same
        // h264 / hevc / av1 bitstreams, so a container accepts a codec's HW
        // variants wherever it accepts the software one.
        match self {
            Self::Mp4 => &[
                "libx264",
                "libx265",
                "libsvtav1",
                "h264_nvenc",
                "hevc_nvenc",
                "av1_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "av1_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
                "av1_amf",
                "h264_vaapi",
                "hevc_vaapi",
                "av1_vaapi",
            ],
            Self::Mov => &[
                "prores_ks",
                "libx264",
                "libx265",
                "h264_nvenc",
                "hevc_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
                "h264_vaapi",
                "hevc_vaapi",
            ],
            Self::Mkv => &[
                "libx264",
                "libx265",
                "libvpx-vp9",
                "libsvtav1",
                "h264_nvenc",
                "hevc_nvenc",
                "av1_nvenc",
                "h264_qsv",
                "hevc_qsv",
                "av1_qsv",
                "h264_videotoolbox",
                "hevc_videotoolbox",
                "h264_amf",
                "hevc_amf",
                "av1_amf",
                "h264_vaapi",
                "hevc_vaapi",
                "av1_vaapi",
            ],
            Self::Webm => &["libvpx-vp9", "libsvtav1", "av1_nvenc", "av1_qsv", "av1_amf", "av1_vaapi"],
            Self::Gif => &["gif"],
            _ => &[],
        }
    }
    pub fn audio_codecs(self) -> &'static [&'static str] {
        match self {
            Self::Mp4 => &["aac", "alac"],
            Self::Mov => &["aac", "alac", "pcm_s16le", "pcm_s24le"],
            Self::Mkv => &["aac", "libopus", "libmp3lame", "flac", "pcm_s16le"],
            Self::Webm => &["libopus"],
            Self::Mp3 => &["libmp3lame"],
            Self::M4a => &["aac", "alac"],
            Self::Wav => &["pcm_s16le", "pcm_s24le"],
            Self::Flac => &["flac"],
            Self::Gif => &[],
        }
    }
    pub fn video_ok(self, codec: &str) -> bool {
        self.video_codecs().contains(&codec)
    }
    pub fn audio_ok(self, codec: &str) -> bool {
        self.audio_codecs().contains(&codec)
    }
}

/// Which video rate-control branch [`build_export_args`] emits. Ignored for
/// `prores_ks` (driven by the ProRes profile) and `gif` (palette pipeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RateControl {
    /// Constant quality: `-crf N` (libvpx-vp9 also gets `-b:v 0`). The default.
    #[default]
    Crf,
    /// Single-pass average bitrate: `-b:v X` (+ optional `-maxrate`/`-bufsize`).
    Bitrate,
    /// Two-pass average bitrate (two ffmpeg runs sharing a passlog).
    TwoPass,
    /// Per-codec lossless: x264/x265/svt-av1 `-crf 0`; libvpx-vp9 `-lossless 1`.
    Lossless,
}

pub use crate::model::Fit;
use crate::model::FitOrder;

/// Which ffmpeg invocation [`build_export_args`] is emitting for. Injected so
/// the builder stays pure (no knowledge of the platform null device or the temp
/// passlog file — [`render_with`] supplies those).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PassPhase {
    /// One-shot encode (every mode except two-pass).
    #[default]
    Single,
    /// Two-pass analysis pass: `-pass 1`, video-only, discarded output.
    First,
    /// Two-pass final pass: `-pass 2`, real output.
    Second,
}

/// Everything the export menu can drive. `Default` reproduces the original
/// hard-coded behaviour byte-for-byte (no `-c:v`/`-c:a`/`-crf`, no faststart),
/// so the legacy [`render`] path and the existing unit tests are unaffected.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case", default)]
pub struct ExportOptions {
    /// Target container / muxer.
    pub container: Container,

    /// `-c:v` value. `None` lets ffmpeg pick the encoder from the container
    /// (legacy behaviour); audio-only containers ignore it.
    pub video_codec: Option<String>,
    /// `-c:a` value. `None` lets ffmpeg pick from the container.
    pub audio_codec: Option<String>,

    /// Video rate-control mode.
    pub rate_control: RateControl,
    /// `-crf N` (Crf / Lossless modes). `None` keeps the encoder default.
    pub crf: Option<u32>,
    /// `-b:v` token, e.g. "8M" / "2500k". Required for Bitrate / TwoPass.
    pub video_bitrate: Option<String>,
    /// `-maxrate` VBV cap (bitrate modes).
    pub max_rate: Option<String>,
    /// `-bufsize` VBV buffer (pairs with `max_rate`).
    pub buf_size: Option<String>,

    /// `-preset` for x264/x265 (named) and svt-av1 (numeric); reinterpreted as
    /// `-cpu-used` for libvpx-vp9.
    pub preset: Option<String>,
    /// ProRes quality `-profile:v 0..5` (prores_ks only).
    pub prores_profile: Option<u8>,
    /// `-tune` (x264 / x265 only).
    pub tune: Option<String>,
    /// `-profile:v` for h264 / hevc (not ProRes).
    pub profile_v: Option<String>,

    /// `-pix_fmt` AND the filtergraph terminal `format=` (dual-write). `None`
    /// keeps the yuv420p path. yuv420p requires even dimensions.
    pub pix_fmt: Option<String>,

    /// `-hwaccel` for input **decode** on export ("auto", "cuda", "vaapi",
    /// "videotoolbox", "qsv", "d3d11va", …). `None` / "none" decodes in software
    /// (the default, byte-for-byte as before). Independent of the encoder: GPU
    /// decode composes with a software encode. Frames are downloaded to system
    /// memory (no `-hwaccel_output_format`) so the CPU filtergraph still runs.
    pub hwaccel: Option<String>,

    /// Output WxH, baked into the filtergraph. Even-clamped.
    pub resolution: Option<(u32, u32)>,
    /// How footage of a different shape is fitted to that frame — letterboxed
    /// (the default), filled and cropped, or letterboxed over a blurred copy of
    /// itself. Only matters when the two differ.
    pub fit: Fit,
    /// Output fps, baked into the filtergraph; never emits `-r`. `None` is the project's
    /// delivery frame rate (`Delivery::fps`) when it has one, else the first clip with a
    /// picture's.
    pub fps: Option<f64>,
    /// `scale=…:flags=` scaler (bicubic / bilinear / lanczos / neighbor / spline).
    pub scaler: Option<String>,
    /// Forced audio sample rate, via the graph `aformat` (not `-ar`).
    pub audio_sample_rate: Option<u32>,
    /// Forced channel count, via the graph `aformat` (not `-ac`).
    pub audio_channels: Option<u16>,

    /// `-b:a` token (lossy codecs only).
    pub audio_bitrate: Option<String>,
    /// `-compression_level` for flac.
    pub flac_compression: Option<u8>,
    /// When false the audio map is dropped and `-an` emitted.
    pub include_audio: bool,

    /// `-movflags +faststart` (mp4 / mov / m4a only).
    pub faststart: bool,
    /// `paletteuse=dither=` for gif.
    pub gif_dither: Option<String>,
    /// gif `-loop 0` (true, infinite) vs `-loop -1` (false, play once).
    pub gif_loop: bool,
    /// `-metadata title=`.
    pub metadata_title: Option<String>,
    /// Render only this timeline span (seconds), e.g. the GUI's in/out marks;
    /// the output starts at `range.start`. `None` renders the whole timeline.
    pub range: Option<crate::model::TimeRange>,
    /// Normalize the final mix to -14 LUFS (single-pass `loudnorm`, the
    /// streaming-platform target) before encoding. The same as
    /// `loudness: Some(LoudnessPreset::Youtube)`.
    pub loudnorm: bool,
    /// Normalize the final mix to a platform's loudness target. Wins over `loudnorm`.
    pub loudness: Option<LoudnessPreset>,
    /// Reach the loudness target with one static gain instead of `loudnorm`'s moving one:
    /// the mix is measured first (the export's own graph, as `get_levels` reads it), then
    /// raised or lowered by the difference and held under the target's true peak by an
    /// oversampled limiter. The dynamics stay as mixed; only peaks the gain pushes over the
    /// ceiling are limited. Read only with a target (`loudness` or `loudnorm`).
    pub loudness_static: bool,
    /// The gain static normalization applies, in dB: measured by the render itself before
    /// it builds the graph, never taken from a caller.
    #[serde(skip)]
    #[schemars(skip)]
    pub loudness_gain_db: Option<f64>,
    /// Mux the cut's captions in as a soft subtitle stream (`mov_text` in mp4 / mov,
    /// `webvtt` in webm, `srt` in mkv). Refused for a container that carries none.
    pub embed_captions: bool,
    /// Write the cut's captions beside the export, one file per format
    /// (`cut.mp4` → `cut.srt` / `cut.vtt`).
    pub caption_sidecars: Vec<crate::captions_export::SubtitleFormat>,
    /// Replace caption files already beside the output (the MCP `export` tool's `overwrite`,
    /// or the GUI's confirmation); otherwise an export that would is refused.
    pub overwrite_sidecars: bool,
    /// Leave the generated captions out of the picture (typed titles are still drawn), for
    /// a delivery that carries them as a subtitle track or a file instead.
    pub skip_burned_captions: bool,
    /// Set by the render, never by a caller: the SubRip file of the cut's captions
    /// to mux in (see `side_files`).
    #[serde(skip)]
    pub subtitle_input: Option<String>,
    /// Set by the render, never by a caller: the FFMETADATA file of the cut's
    /// chapter markers to mux in.
    #[serde(skip)]
    pub chapters_input: Option<String>,
}

/// The most static loudness normalization raises or lowers a mix, in dB: a near-silent
/// cut is not lifted into a wall of noise.
pub const LOUDNESS_MAX_GAIN_DB: f64 = 30.0;

/// How far under the target's true-peak ceiling the static normalization's limiter is set:
/// resampling the 4x limiter's output back down rings a little over what it held.
const STATIC_TP_MARGIN_DB: f64 = 0.3;

/// A delivery's loudness target: integrated loudness and the true-peak ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoudnessPreset {
    /// YouTube: -14 LUFS, -1 dBTP.
    Youtube,
    /// Spotify: -14 LUFS, -1 dBTP.
    Spotify,
    /// Apple Music / Podcasts: -16 LUFS, -1 dBTP.
    Apple,
    /// EBU R128 broadcast: -23 LUFS, -1 dBTP.
    Broadcast,
}

impl LoudnessPreset {
    pub fn lufs(self) -> f64 {
        match self {
            Self::Youtube | Self::Spotify => -14.0,
            Self::Apple => -16.0,
            Self::Broadcast => -23.0,
        }
    }

    pub fn true_peak_db(self) -> f64 {
        -1.0
    }

    /// The `loudnorm` filter that meets this target. Its `TP` is half a decibel under
    /// the ceiling: single-pass `loudnorm` overshoots its own true-peak target by about
    /// that much (the reason the plain `loudnorm` option asks for -1.5).
    fn filter(self) -> String {
        format!("loudnorm=I={}:TP={}:LRA=11", self.lufs(), self.true_peak_db() - 0.5)
    }
}

impl ExportOptions {
    /// The caption files an export with these options writes beside `output` (`caption_sidecars`).
    pub fn caption_sidecar_paths(&self, output: &Path) -> Vec<PathBuf> {
        side_files::sidecar_paths(output, self).into_iter().map(|(_, p)| p).collect()
    }
}

impl Default for ExportOptions {
    // Reproduces the pre-existing argv exactly: no codecs, no crf, no faststart.
    fn default() -> Self {
        Self {
            container: Container::Mp4,
            video_codec: None,
            audio_codec: None,
            rate_control: RateControl::Crf,
            crf: None,
            video_bitrate: None,
            max_rate: None,
            buf_size: None,
            preset: None,
            prores_profile: None,
            tune: None,
            profile_v: None,
            pix_fmt: None,
            hwaccel: None,
            resolution: None,
            fit: Fit::Contain,
            fps: None,
            scaler: None,
            audio_sample_rate: None,
            audio_channels: None,
            audio_bitrate: None,
            flac_compression: None,
            include_audio: true,
            faststart: false,
            gif_dither: None,
            gif_loop: true,
            metadata_title: None,
            range: None,
            loudnorm: false,
            loudness: None,
            loudness_static: false,
            loudness_gain_db: None,
            embed_captions: false,
            caption_sidecars: Vec::new(),
            overwrite_sidecars: false,
            skip_burned_captions: false,
            subtitle_input: None,
            chapters_input: None,
        }
    }
}

/// The validated export range from `opts`, clamped to the timeline. `None`
/// when absent or empty after clamping (which falls back to a full export).
fn effective_range(timeline: &Timeline, opts: &ExportOptions) -> Option<(f64, f64)> {
    timeline.export_span(opts.range.map(|r| (r.start, r.end)))
}

/// Whether a bitrate token like "8M" / "2500k" / "800000" is well-formed.
fn valid_bitrate(s: &str) -> bool {
    let s = s.trim();
    let digits = match s.char_indices().find(|(_, c)| !(c.is_ascii_digit() || *c == '.')) {
        Some((i, c)) => {
            // The only allowed trailing char is a single k/K/M unit suffix.
            if !matches!(c, 'k' | 'K' | 'm' | 'M') || i + c.len_utf8() != s.len() {
                return false;
            }
            &s[..i]
        }
        None => s,
    };
    !digits.is_empty() && digits.parse::<f64>().map(|v| v > 0.0).unwrap_or(false)
}

/// The `-tune` values each encoder accepts. x265 notably lacks x264's `film` /
/// `stillimage`; feeding an unknown tune makes the encoder fail to initialise.
fn video_tunes(vc: &str) -> &'static [&'static str] {
    match vc {
        "libx264" => &[
            "film",
            "animation",
            "grain",
            "stillimage",
            "zerolatency",
            "fastdecode",
            "psnr",
            "ssim",
        ],
        "libx265" => &["psnr", "ssim", "grain", "zerolatency", "fastdecode", "animation"],
        _ => &[],
    }
}

/// `-pix_fmt` / filtergraph `format=` values the graph builders will emit. A
/// closed set, unlike a colour: `pix_fmt`/`scaler`/`gif_dither` reach the graph
/// unquoted (see `ExportFormat::scale_flags`, `export_format`, the gif
/// `paletteuse` chain), so an out-of-list value is dropped rather than passed
/// through — a `.kerf` file or an MCP call is untrusted the same as anything
/// else, and this must hold even when `validate_export` was never run.
const PIX_FMTS: &[&str] = &[
    "yuv420p",
    "yuv422p",
    "yuv444p",
    "yuva420p",
    "yuva422p",
    "yuva444p",
    "yuv420p10le",
    "yuv422p10le",
    "yuv444p10le",
    "yuva420p10le",
    "yuva422p10le",
    "yuva444p10le",
    "yuv420p12le",
    "yuv422p12le",
    "yuv444p12le",
    "yuvj420p",
    "yuvj422p",
    "yuvj444p",
    "nv12",
    "nv21",
    "p010le",
    "rgb24",
    "bgr24",
    "rgba",
    "bgra",
    "gray",
];

/// `scale=…:flags=` scalers `swscale` accepts, matching the export dialog's choices.
const SCALERS: &[&str] = &["bicubic", "bilinear", "lanczos", "neighbor", "spline"];

/// `paletteuse=dither=` modes for the gif pipeline.
const GIF_DITHERS: &[&str] = &[
    "bayer",
    "heckbert",
    "floyd_steinberg",
    "sierra2",
    "sierra2_4a",
    "sierra3",
    "burkes",
    "atkinson",
    "none",
];

/// The hardware-encoder family a `-c:v` value belongs to, read from its ffmpeg
/// suffix. Software encoders (libx264 / libx265 / libsvtav1 / libvpx-vp9) and
/// the prores / gif pipelines are [`EncFamily::Software`]. The family decides how
/// the constant-quality knob, VBV caps and speed preset are spelled — each HW
/// encoder names the same intent differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncFamily {
    Software,
    Nvenc,
    Qsv,
    VideoToolbox,
    Amf,
    Vaapi,
}

fn enc_family(vc: &str) -> EncFamily {
    if vc.ends_with("_nvenc") {
        EncFamily::Nvenc
    } else if vc.ends_with("_qsv") {
        EncFamily::Qsv
    } else if vc.ends_with("_videotoolbox") {
        EncFamily::VideoToolbox
    } else if vc.ends_with("_amf") {
        EncFamily::Amf
    } else if vc.ends_with("_vaapi") {
        EncFamily::Vaapi
    } else {
        EncFamily::Software
    }
}

/// Whether the export hands `opts`' picture to a VAAPI encoder, which takes only frames
/// uploaded to the device: the graph ends in `format=nv12,hwupload` and the argv names the
/// device. (A GIF is paletted, so a VAAPI codec on one is ignored like any other.)
fn uploads_to_vaapi(opts: &ExportOptions) -> bool {
    opts.container != Container::Gif
        && opts
            .video_codec
            .as_deref()
            .is_some_and(|vc| enc_family(vc) == EncFamily::Vaapi)
}

/// At the x264 CRF's number a hardware H.264 / HEVC encoder's constant QP spends about twice
/// the bits. Measured on a Radeon RX 7900 XT, 4K to 1080p, against a lossless render: x264
/// medium CRF 20 is 5.59 MB / 45.1 dB where `h264_amf` at constant QP 20 (the old 1:1
/// spelling) was 10.7 MB / 48.9 dB, and `h264_amf` / `hevc_amf` at QP 26 5.3-5.6 MB / 45.5-46.1 dB.
const HW_QP_OFFSET: u32 = 6;

/// AV1's quantiser index runs 0..255; the dialog's AV1 CRF scale (SVT-AV1's) 0..63.
const AV1_QP_PER_CRF: u32 = 4;

/// The constant QP an AMF or VAAPI encoder gets for the software-scale `crf` the options
/// carry. The one AV1 point measured (`av1_amf` QP 90: 6.41 MB / 46.4 dB) sits on the
/// high-quality side of x264 CRF 20; the rest of the AV1 line is the scale's slope.
fn hw_qp(vc: &str, crf: u32) -> u32 {
    if vc.starts_with("av1_") {
        crf.saturating_mul(AV1_QP_PER_CRF).min(255)
    } else {
        crf.saturating_add(HW_QP_OFFSET).min(51)
    }
}

/// Whether `vc` produces an H.264 bitstream (software or any HW family).
fn is_h264(vc: &str) -> bool {
    vc == "libx264" || vc.starts_with("h264_")
}

/// Whether `vc` produces an HEVC bitstream (software or any HW family) — these
/// need the `hvc1` tag in mp4 / mov so QuickTime / iOS will play them.
fn is_hevc(vc: &str) -> bool {
    vc == "libx265" || vc.starts_with("hevc_")
}

/// Map a 0..51 CRF (lower = better) onto VideoToolbox's 1..100 constant-quality
/// scale (higher = better). VideoToolbox has no CRF; this preserves the intent
/// so the same `crf` field drives every encoder.
fn crf_to_vt_quality(crf: u32) -> u32 {
    let crf = crf.min(51) as f64;
    (((1.0 - crf / 51.0) * 100.0).round() as u32).clamp(1, 100)
}

/// Validate an option set against the timeline's available streams, returning a
/// list of human-readable problems (empty = OK). Pure; called by the pre-launch
/// guard in [`render_with`] and mirrored client-side by the export dialog.
pub fn validate_export(opts: &ExportOptions, has_video: bool, has_audio: bool) -> Vec<String> {
    let mut issues = Vec::new();
    let c = opts.container;
    let want_video = has_video && !c.is_audio_only();
    let want_audio = has_audio && !c.is_video_only() && opts.include_audio;

    if c.is_audio_only() && !has_audio {
        issues.push(format!(
            "{} is audio-only, but the timeline has no audio.",
            c.ext().to_uppercase()
        ));
    }
    if c.is_video_only() && !has_video {
        issues.push("GIF export needs video, but the timeline has no video.".to_string());
    }
    if !want_video && !want_audio {
        issues.push("These settings would export nothing.".to_string());
    }
    if want_video {
        if let Some(vc) = opts.video_codec.as_deref() {
            if !c.video_ok(vc) {
                issues.push(format!("{vc} can't go in a .{} file.", c.ext()));
            }
        }
        let rate_mode = !matches!(opts.video_codec.as_deref(), Some("prores_ks") | Some("gif"));
        if rate_mode && matches!(opts.rate_control, RateControl::Bitrate | RateControl::TwoPass) && opts.video_bitrate.is_none() {
            issues.push("A target video bitrate is required for bitrate / two-pass.".to_string());
        }
        if let Some(vc) = opts.video_codec.as_deref() {
            if matches!(opts.rate_control, RateControl::TwoPass) && enc_family(vc) != EncFamily::Software {
                issues.push(format!(
                    "Two-pass encoding isn't supported for hardware encoder {vc}; use crf or bitrate."
                ));
            }
        }
        if let (Some(vc), Some(t)) = (opts.video_codec.as_deref(), opts.tune.as_deref()) {
            if matches!(vc, "libx264" | "libx265") && !t.is_empty() && !video_tunes(vc).contains(&t) {
                issues.push(format!("tune \"{t}\" is not valid for {vc}."));
            }
        }
    }
    if let Some(b) = opts.video_bitrate.as_deref() {
        if !valid_bitrate(b) {
            issues.push(format!("Invalid video bitrate \"{b}\"."));
        }
    }
    for (label, v) in [("max rate", &opts.max_rate), ("buffer size", &opts.buf_size)] {
        if let Some(b) = v.as_deref() {
            if !valid_bitrate(b) {
                issues.push(format!("Invalid {label} \"{b}\"."));
            }
        }
    }
    if let Some(pf) = opts.pix_fmt.as_deref() {
        if !PIX_FMTS.contains(&pf) {
            issues.push(format!("Unsupported pixel format \"{pf}\"."));
        }
    }
    if want_video && uploads_to_vaapi(opts) && !matches!(opts.pix_fmt.as_deref(), None | Some("yuv420p" | "nv12")) {
        issues.push("VAAPI encoders take 8-bit 4:2:0 (yuv420p) here.".to_string());
    }
    if let Some(s) = opts.scaler.as_deref() {
        if !SCALERS.contains(&s) {
            issues.push(format!("Unsupported scaler \"{s}\"."));
        }
    }
    if let Some(d) = opts.gif_dither.as_deref() {
        if !GIF_DITHERS.contains(&d) {
            issues.push(format!("Unsupported gif dither \"{d}\"."));
        }
    }
    if want_audio {
        if let Some(ac) = opts.audio_codec.as_deref() {
            if !c.audio_ok(ac) {
                issues.push(format!("{ac} can't go in a .{} file.", c.ext()));
            }
        }
        if let Some(b) = opts.audio_bitrate.as_deref() {
            if !valid_bitrate(b) {
                issues.push(format!("Invalid audio bitrate \"{b}\"."));
            }
        }
    }
    if opts.embed_captions && side_files::subtitle_codec(c).is_none() {
        issues.push(format!(
            "A .{} file can't carry subtitles; write them beside the export instead.",
            c.ext()
        ));
    }
    issues
}

/// The single output shape every clip is normalized to before `concat`. The
/// `concat` filter requires identical resolution / frame rate / sample format
/// across its inputs, and `concat`'s `a=1` requires every segment to carry
/// audio — so clips from a video-only asset get synthesized silence.
#[derive(Debug, Clone)]
struct ExportFormat {
    width: u32,
    height: u32,
    fps: f64,
    sample_rate: u32,
    channels: u16,
    /// Terminal pixel format: argv `-pix_fmt` and the filtergraph terminal
    /// `format=` are kept in sync through this single field.
    pix_fmt: String,
    /// Optional `scale=…:flags=` scaler.
    scaler: Option<String>,
    /// Letterbox or fill-and-crop when the footage and the frame differ in shape.
    fit: Fit,
}

impl Default for ExportFormat {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30.0,
            sample_rate: 48_000,
            channels: 2,
            pix_fmt: "yuv420p".to_string(),
            scaler: None,
            fit: Fit::Contain,
        }
    }
}

impl ExportFormat {
    fn channel_layout(&self) -> &'static str {
        if self.channels <= 1 {
            "mono"
        } else {
            "stereo"
        }
    }
    /// The `:flags=…` suffix to append to a `scale` filter, or empty. An
    /// out-of-list value (a stale `.kerf` file, a hand-crafted MCP call) is
    /// dropped rather than spliced in unquoted — see [`SCALERS`].
    fn scale_flags(&self) -> String {
        match self.scaler.as_deref() {
            Some(s) if SCALERS.contains(&s) => format!(":flags={s}"),
            _ => String::new(),
        }
    }
}

/// The frame this timeline actually renders at: the project's delivery format
/// when one is set, otherwise the shape its footage gives it. What a readiness
/// check has to compare a platform's expectations against.
pub fn delivery_frame(timeline: &Timeline, assets: &[Asset]) -> (u32, u32) {
    let f = export_format(timeline, assets, &ExportOptions::default());
    (f.width, f.height)
}

/// Where a render's frame rate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FpsSource {
    /// The export options named one (`ExportOptions::fps`).
    Options,
    /// The project's delivery frame states one (`Delivery::fps`).
    Delivery,
    /// Neither did, so it is the rate of the first clip with a picture — in footage of mixed
    /// rates, whichever comes first.
    Footage,
    /// Nothing to follow: no clip with a picture, so the 30 fps default.
    Default,
}

/// The frame rate the cut renders at when the export names none — the delivery frame's, else the
/// first picture's — and which of the two it was. What a stream plays at, a still is placed on and
/// an export writes by default; `timeline_summary` shows it so the default is not a surprise.
pub fn delivery_rate(timeline: &Timeline, assets: &[Asset]) -> (f64, FpsSource) {
    let (f, source) = export_format_sourced(&timeline.for_render(), assets, &ExportOptions::default());
    (f.fps, source)
}

/// The picture an export writes, as it will be rendered: what the options and the project's
/// delivery frame made of the footage's own, reported back so a caller sees the size and the rate
/// it got rather than the ones it hoped for.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct ExportedFrame {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Where `fps` came from. `footage` and `default` are the cases a caller who cares about the
    /// rate should set `options.fps`, or the delivery frame's `fps`, for.
    pub fps_source: FpsSource,
}

/// The picture `render_with_progress(timeline, assets, .., opts, ..)` writes: the frame of the cut as
/// the export renders it (a range sliced, muted tracks gone). `None` when it writes no picture (an
/// audio container, or a cut with no video clip).
pub fn exported_frame(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Option<ExportedFrame> {
    let cut = export_timeline(timeline, opts);
    let has_video = cut.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    if opts.container.is_audio_only() || !has_video {
        return None;
    }
    let (f, fps_source) = export_format_sourced(&cut, assets, opts);
    Some(ExportedFrame {
        width: f.width,
        height: f.height,
        fps: f.fps,
        fps_source,
    })
}

/// [`exported_frame`] of one file of a multi-format export: the cut delivered at `variant`'s frame
/// (which keeps the project's frame rate, [`Timeline::for_delivery`]) with `opts` as
/// `render_variants` takes them.
pub fn variant_frame(
    timeline: &Timeline,
    assets: &[Asset],
    variant: &ExportVariant,
    opts: &ExportOptions,
) -> Option<ExportedFrame> {
    let per_variant = ExportOptions {
        resolution: Some((variant.delivery.width, variant.delivery.height)),
        fit: variant.delivery.fit,
        ..opts.clone()
    };
    exported_frame(&timeline.for_delivery(variant.delivery), assets, &per_variant)
}

/// The slice of [`ExportFormat`] a renderer of one *frame* needs — the canvas
/// and how footage meets it — taken from the same `export_format` the still and
/// the export use, so a second renderer (the GPU compositor, through
/// [`crate::render_plan::RenderPlan`]) cannot drift from them on what the frame
/// is.
#[derive(Debug, Clone)]
pub(crate) struct RenderGeometry {
    pub width: u32,
    pub height: u32,
    pub fit: Fit,
    /// The export's `scaler` as chosen (`None` is swscale's bicubic default).
    pub scaler: Option<String>,
    /// The output rate, exactly as the graph's `fps=` / `color=r=` print it (`{}` of
    /// this `f64`, which FFmpeg parses back to a rational).
    pub fps: f64,
    /// The delivery's terminal pixel format (`format=` in the graph).
    pub pix_fmt: String,
}

pub(crate) fn render_geometry(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> RenderGeometry {
    let f = export_format(timeline, assets, opts);
    RenderGeometry {
        width: f.width,
        height: f.height,
        fit: f.fit,
        scaler: f.scaler,
        fps: f.fps,
        pix_fmt: f.pix_fmt,
    }
}

/// Derive the output shape from the first clip (across all tracks) that carries
/// a video stream and the first that carries audio, falling back to 1080p30
/// stereo defaults. The project's delivery frame (size, fit and frame rate) comes
/// next, and `opts` carrying resolution or fps overrides win over all of it.
fn export_format(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> ExportFormat {
    export_format_sourced(timeline, assets, opts).0
}

/// [`export_format`], and where its frame rate came from.
fn export_format_sourced(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> (ExportFormat, FpsSource) {
    let mut fps_source = FpsSource::Default;
    let stream_of = |clip: &crate::model::Clip, kind: StreamKind| {
        assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .and_then(|a| a.streams.iter().find(|s| s.kind == kind))
    };
    let clips = || timeline.tracks.iter().flat_map(|t| t.clips.iter());

    let mut fmt = ExportFormat::default();
    if let Some((clip, v)) = clips().find_map(|c| stream_of(c, StreamKind::Video).map(|s| (c, s))) {
        // A reframed clip's source dimensions describe the *sphere*, not the
        // deliverable — inheriting them would export a 16:9 reframe of a 5.7K
        // Insta360 capture at 5760x2880. Keep the 1080p default instead and let
        // `opts.resolution` override as usual. Keyed off the reframe rather than
        // the projection, so an un-reframed 360 clip still behaves as before.
        let reframes_to_flat = clip
            .reframe
            .as_ref()
            .is_some_and(|r| r.output == crate::model::Projection::Flat);
        if let (false, Some(w), Some(h)) = (reframes_to_flat, v.width, v.height) {
            fmt.width = w;
            fmt.height = h;
        }
        if let Some(f) = v.fps.filter(|f| *f > 0.0) {
            fmt.fps = f;
            fps_source = FpsSource::Footage;
        }
    }
    if let Some(a) = clips().find_map(|c| stream_of(c, StreamKind::Audio)) {
        if let Some(r) = a.sample_rate.filter(|r| *r > 0) {
            fmt.sample_rate = r;
        }
        if let Some(c) = a.channels.filter(|c| *c > 0) {
            fmt.channels = c;
        }
    }
    // The project's delivery frame sits between the footage and an explicit
    // export resolution: it overrides "whatever the first clip is" so the
    // preview, the still and the export agree on the shape being cut for, and
    // yields to a resolution typed into the export dialog so a one-off render
    // at a different size still works. Its frame rate, when it states one, is
    // the same: the footage's rate is only the default of a cut that never
    // chose (in mixed footage, whichever clip is first), and `opts.fps` below
    // still wins over it.
    if let Some(d) = timeline.format {
        fmt.width = d.width;
        fmt.height = d.height;
        if let Some(fps) = d.rate() {
            fmt.fps = fps;
            fps_source = FpsSource::Delivery;
        }
    }
    if let Some((w, h)) = opts.resolution {
        fmt.width = w;
        fmt.height = h;
    }
    if let Some(f) = opts.fps.filter(|f| *f > 0.0) {
        fmt.fps = f;
        fps_source = FpsSource::Options;
    }
    if let Some(r) = opts.audio_sample_rate.filter(|r| *r > 0) {
        fmt.sample_rate = r;
    }
    if let Some(c) = opts.audio_channels.filter(|c| *c > 0) {
        fmt.channels = c;
    }
    // libopus only encodes at 48 kHz; force it regardless of source / override.
    if opts.audio_codec.as_deref() == Some("libopus") {
        fmt.sample_rate = 48_000;
    }
    // yuv420p (and most 4:2:0 formats) require even dimensions, so clamp both
    // source-derived and custom sizes before they reach `scale=`/`color=s=`.
    fmt.width = (fmt.width & !1).max(2);
    fmt.height = (fmt.height & !1).max(2);
    // An out-of-list `pix_fmt` (see [`PIX_FMTS`]) is treated as if none were
    // given, falling through to the same ProRes default a bare `None` gets —
    // never spliced into the graph terminal `format=` unchecked.
    if let Some(pf) = opts.pix_fmt.as_deref().filter(|pf| PIX_FMTS.contains(pf)) {
        fmt.pix_fmt = pf.to_string();
    } else if opts.video_codec.as_deref() == Some("prores_ks") {
        // ProRes cannot encode 4:2:0; default a None pix_fmt to 10-bit 4:2:2 (or
        // 4:4:4 for the 4444 profiles) so the graph terminal doesn't silently
        // decimate a 10-bit / 4:2:2 source to 8-bit 4:2:0 before the encode.
        fmt.pix_fmt = if matches!(opts.prores_profile, Some(4) | Some(5)) {
            "yuva444p10le".to_string()
        } else {
            "yuv422p10le".to_string()
        };
    }
    fmt.scaler = opts.scaler.clone();
    // Same precedence for the fit: a Cover delivery crops in the preview exactly
    // as it will on export, but an explicit non-default `opts.fit` still wins.
    fmt.fit = match (timeline.format, opts.fit) {
        (Some(d), Fit::Contain) => d.fit,
        (_, f) => f,
    };
    (fmt, fps_source)
}

/// Build the complete argument list for `ffmpeg` (everything after the binary
/// name) that renders the whole `timeline` to `output_path` with `opts`.
///
/// One input (`-i`) is added per clip, in track-then-clip order; the filtergraph
/// (see [`build_filter_complex`]) references those inputs by the same index. The
/// `[outv]` / `[outa]` maps — and the codec / rate-control / muxer flags that
/// follow them — are emitted only for the streams the chosen container actually
/// carries, kept in lockstep with the graph so no produced pad is left unmapped.
///
/// The function is pure — it performs no I/O and does not spawn ffmpeg —
/// which makes it unit-testable without the binary being present. The actual
/// render call feeds the returned `Vec<String>` straight to `Command::args`.
pub fn build_export_args(timeline: &Timeline, assets: &[Asset], output_path: &str, opts: &ExportOptions) -> Result<Vec<String>> {
    build_export_args_phase(timeline, assets, output_path, opts, PassPhase::Single, "", "")
}

/// Maps each timeline clip (in track-then-storage order, the flat indexing the
/// graph and `fx` already use) onto an ffmpeg input, **deduplicating** clips that
/// would emit byte-identical `-i` arguments — the same asset fast-seeked to the
/// same point — so a source decoded for several clips (e.g. composited on two
/// tracks, or a duplicated clip) is opened and decoded **once** and fanned out
/// with `split`. Clips with *different* seeks deliberately stay separate: the
/// per-input `-ss` already decodes only each one's kept region, and collapsing
/// them would force decoding the whole span between the first and last cut. Still
/// images are never shared (their input encodes a per-clip `-t` window).
struct InputPlan {
    /// The representative clip flat-index for each unique input, in `-i` order.
    representatives: Vec<usize>,
    /// Per clip flat-index → its unique input index (position in `representatives`);
    /// [`NO_INPUT`] for a clip that takes no part in this plan.
    clip_input: Vec<usize>,
    /// Per clip flat-index, how a title clip's pictures are stamped onto the canvas' grid (its chain's
    /// first filters); `None` for a clip of footage. Filled by [`push_inputs`], which names the pictures.
    stamps: Vec<Option<svg_layer::ClipStamp>>,
    /// Added to every input index the graph prints (`[N:v]`, `[N:a]`): the inputs the argv
    /// has ahead of this plan's own. `0` for every graph that runs alone.
    base: usize,
}

/// A clip in no input of a plan ([`InputPlan::clip_input`]).
const NO_INPUT: usize = usize::MAX;

#[cfg(test)]
fn plan_inputs(timeline: &Timeline, assets: &[Asset], fx: &[ClipFx]) -> InputPlan {
    plan_inputs_for(timeline, assets, fx, &|_| true)
}

/// [`plan_inputs`] for the clips `include` names (by flat index): the inputs of one part of an
/// export in parts, or of its sound alone. The others get no input.
fn plan_inputs_for(timeline: &Timeline, assets: &[Asset], fx: &[ClipFx], include: &dyn Fn(usize) -> bool) -> InputPlan {
    let image_of = |id| assets.iter().find(|a| a.id == id).is_some_and(|a| a.is_image());
    let mut representatives: Vec<usize> = Vec::new();
    let mut clip_input: Vec<usize> = Vec::new();
    let mut seen: std::collections::HashMap<(uuid::Uuid, String), usize> = std::collections::HashMap::new();
    for (flat, clip) in timeline.tracks.iter().flat_map(|t| t.clips.iter()).enumerate() {
        if !include(flat) {
            clip_input.push(NO_INPUT);
            continue;
        }
        let input = if image_of(clip.asset_id) || clip.is_generated() {
            let i = representatives.len();
            representatives.push(flat);
            i
        } else {
            let (start, _) = clip_source_window(clip, &fx[flat]);
            // Key on the exact emitted seek string: two clips share an input iff
            // their `-i` arguments (path is implied by asset_id) are identical.
            let key = (clip.asset_id, format!("{}", clip_input_seek(clip, &fx[flat], start)));
            *seen.entry(key).or_insert_with(|| {
                let i = representatives.len();
                representatives.push(flat);
                i
            })
        };
        clip_input.push(input);
    }
    let stamps = vec![None; clip_input.len()];
    InputPlan {
        representatives,
        clip_input,
        stamps,
        base: 0,
    }
}

/// A still image an export in parts picks up at a cut instead of reading from its start: the output
/// frame its first picture lands on, and how many pictures it keeps (so that it ends where the
/// whole export's run of it does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Restart {
    frame: u64,
    count: u64,
}

/// Which inputs a run opens and how: all of them by default ([`push_inputs`]), or the part of an
/// export in parts ([`segments`]).
struct InputScope<'a> {
    /// The cut's transition table, whole: a clip's look does not depend on which part draws it.
    fx: &'a [ClipFx],
    /// The clips (by flat index) that get an input.
    include: &'a dyn Fn(usize) -> bool,
    /// Stills read from a frame onward (by flat index).
    restarts: &'a HashMap<usize, Restart>,
    /// Open the sound of each input only (`-vn`): the sound of an export in parts is one pass over
    /// the whole cut, from the clips that have any.
    sound_only: bool,
    /// The inputs ahead of these in the argv.
    base: usize,
}

/// Push the `-i` inputs for every clip in `timeline` and return the plan the
/// filtergraph indexes by.
///
/// Extracted so export and the live preview stream ([`build_preview_args`]) decode their sources
/// *identically* — same deduplication, same per-input fast-seek, same still-image looping — and
/// only differ at the two ends of the pipeline (which files go in, what muxer comes out).
fn push_inputs(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    opts: &ExportOptions,
    args: &mut Vec<String>,
) -> Result<InputPlan> {
    let fx = transition_fx(timeline, assets);
    let scope = InputScope {
        fx: &fx,
        include: &|_| true,
        restarts: &HashMap::new(),
        sound_only: false,
        base: 0,
    };
    push_inputs_scoped(timeline, assets, fmt, opts, &scope, args)
}

/// [`push_inputs`] for the clips and the way of reading them an [`InputScope`] names.
fn push_inputs_scoped(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    opts: &ExportOptions,
    scope: &InputScope<'_>,
    args: &mut Vec<String>,
) -> Result<InputPlan> {
    let path_of = |id: uuid::Uuid| assets.iter().find(|a| a.id == id).map(|a| a.path.as_str());
    let image_of = |id: uuid::Uuid| assets.iter().find(|a| a.id == id).is_some_and(|a| a.is_image());
    let fx = scope.fx;
    // Deduplicate inputs: clips whose `-i` args are identical (same asset, same
    // fast-seek) share one decoded input, fanned out in the graph with `split`.
    let mut plan = plan_inputs_for(timeline, assets, fx, scope.include);
    plan.base = scope.base;
    // A title clip's input is its pictures, drawn before ffmpeg is spawned (`svg_layer::prepare`).
    let titles = svg_layer::clip_sequences(timeline, fmt, fx);
    let clips: Vec<&crate::model::Clip> = timeline.tracks.iter().flat_map(|t| t.clips.iter()).collect();
    for &rep in &plan.representatives {
        let clip = clips[rep];
        if clip.is_generated() {
            let pictures = titles[rep]
                .as_ref()
                .ok_or_else(|| Error::Engine(format!("frame rate {} cannot draw a title on a track", fmt.fps)))?;
            svg_layer::push_inputs(std::slice::from_ref(&pictures.seq), fmt, args);
            plan.stamps[rep] = Some(pictures.stamp());
            continue;
        }
        let path = path_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        let (start, _) = picture_window(clip, &fx[rep], fmt.fps);
        if let Some(restart) = scope.restarts.get(&rep) {
            // A still picked up at a cut: the picture, held (`loop` in its chain), numbered onto the
            // frames it still has (the way a held title is), where the whole export reads it from
            // its own start.
            let rate = Rational::from_fps(fmt.fps)
                .ok_or_else(|| Error::Engine(format!("frame rate {} cannot restart a still", fmt.fps)))?;
            args.extend(["-f", "image2", "-pattern_type", "none", "-framerate"].map(String::from));
            args.push(format!("{}", fmt.fps));
            plan.stamps[rep] = Some(svg_layer::ClipStamp {
                num: rate.num,
                den: rate.den,
                first_frame: restart.frame,
                held: Some(restart.count),
            });
        } else if image_of(clip.asset_id) {
            // A still has no timeline of its own: its one frame is decoded once and held by the
            // chain's `loop` for the clip's whole source window (`still_pictures` of `end`), which
            // the in-graph trim (the seek forced to 0 for images) carves the clip's duration out
            // of. `-loop 1` decoded the file again for every frame: 0.8 s a frame of a 45 MP JPEG.
            // No `-ss` — seeking into a one-frame input decodes nothing.
            args.push("-framerate".to_string());
            args.push(format!("{}", fmt.fps));
        } else {
            // Hardware-accelerated decode for this input when requested. `-hwaccel`
            // is an input option (applies to the next `-i`), so it's emitted
            // per-input and only for real media — a still gains nothing. Frames
            // are downloaded to system memory (no `-hwaccel_output_format`), so the
            // per-input `-ss` fast-seek and the CPU filtergraph still work.
            if scope.sound_only {
                args.push("-vn".to_string());
            } else if let Some(hw) = opts
                .hwaccel
                .as_deref()
                .filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("none"))
            {
                args.push("-hwaccel".to_string());
                args.push(hw.to_string());
            }
            let seek = clip_input_seek(clip, &fx[rep], start);
            if seek > 0.0 {
                args.push("-ss".to_string());
                args.push(export_seek_arg(seek));
            }
        }
        args.push("-i".to_string());
        args.push(path.to_string());
    }
    Ok(plan)
}

/// The timeline an export draws: [`Timeline::for_render`] (muted / solo-shadowed tracks and
/// disabled clips gone), and for a range export the sliced sub-timeline, so trims, fades,
/// keyframes and overlays all see the same shifted geometry. The builder and whatever has to know
/// what the builder will ask for before it is spawned (the titles' pictures) both start here.
fn export_timeline(timeline: &Timeline, opts: &ExportOptions) -> Timeline {
    let rendered = timeline.for_render();
    let mut cut = match effective_range(&rendered, opts) {
        Some((s, e)) => rendered.slice(s, e),
        None => rendered,
    };
    if opts.container.is_audio_only() {
        drop_titles(&mut cut);
    }
    cut
}

/// [`export_timeline`] as the export builder has it: with the generated captions dropped when the
/// export carries them as a subtitle track or a file instead (`skip_burned_captions`).
fn prepared_cut(timeline: &Timeline, opts: &ExportOptions) -> Timeline {
    let mut cut = export_timeline(timeline, opts);
    if opts.skip_burned_captions {
        cut.overlays.retain(|o| !o.generated);
    }
    cut
}

/// Take the title clips out of a cut whose graph draws no picture (an audio-only export, the levels
/// and the mix passes): their input is pictures nobody drew, and they have no sound.
fn drop_titles(timeline: &mut Timeline) {
    for track in &mut timeline.tracks {
        track.clips.retain(|c| !c.is_generated());
    }
}

/// [`build_export_args`] parameterised by the two-pass [`PassPhase`]. `null_sink`
/// is the platform null device (`/dev/null` / `NUL`) used as the first-pass
/// output, and `passlog` is the shared `-passlogfile` prefix — both injected by
/// [`render_with`] so this builder stays pure. Single-pass callers pass
/// `(Single, "", "")`.
fn build_export_args_phase(
    timeline: &Timeline,
    assets: &[Asset],
    output_path: &str,
    opts: &ExportOptions,
    pass: PassPhase,
    null_sink: &str,
    passlog: &str,
) -> Result<Vec<String>> {
    // Drop muted / solo-shadowed tracks and disabled clips up front, and cut the range an export
    // of one span draws, so the rest of the builder never has to reason about either.
    let cut = prepared_cut(timeline, opts);
    let timeline = &cut;

    // Stream gating: decide what the graph emits and what we `-map`, in lockstep.
    let timeline_has_video = timeline
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let timeline_has_audio = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| clip_sounds(c, assets));
    let c = opts.container;
    let want_video = timeline_has_video && !c.is_audio_only();
    let want_audio = timeline_has_audio && !c.is_video_only() && opts.include_audio && pass != PassPhase::First;

    // `-hide_banner -nostats` keep the captured stderr to genuine warnings/errors
    // (matching the probe/frame calls); without `-nostats` the per-frame progress
    // lines would accumulate unbounded in memory for a long export.
    let mut args: Vec<String> = vec!["-y".to_string(), "-hide_banner".to_string(), "-nostats".to_string()];
    if want_video && uploads_to_vaapi(opts) {
        args.push("-vaapi_device".to_string());
        args.push(VAAPI_DEFAULT_DEVICE.to_string());
    }
    // Per-input fast-seek: an input-side `-ss` to each clip's source-window start
    // so ffmpeg decodes only the kept region instead of everything from t=0 — a
    // 20-subclip cut from a 1h source no longer decodes the hour 20 times over.
    // `-ss` before `-i` is keyframe-accurate-seek (decode+discard up to the point)
    // and resets timestamps to ~0, so the in-graph trim is expressed relative to
    // this same seek (see `video_clip_chain` / `audio_clip_chain`) and the two
    // stay frame-accurate. Inputs are added in storage order, matching how
    // `build_filter_complex` indexes them and how `fx` is indexed.
    let fmt = export_format(timeline, assets, opts);
    let plan = push_inputs(timeline, assets, &fmt, opts, &mut args)?;
    // The animated titles' picture sequences follow the clips' inputs (the graph indexes them from
    // there); a cut with none adds nothing.
    if want_video {
        svg_layer::push_inputs(&svg_layer::svg_sequences(timeline, &fmt), &fmt, &mut args);
    }
    let side = if pass == PassPhase::First {
        side_files::SideInputs::default()
    } else {
        side_files::push_inputs(opts, &mut args)
    };

    let total = timeline.duration();
    let graph = build_filter_complex(timeline, assets, &fmt, total, opts, want_video, want_audio, &plan);
    args.push("-filter_complex".to_string());
    args.push(graph.filter);
    if graph.has_video {
        args.push("-map".to_string());
        args.push("[outv]".to_string());
    }
    if graph.has_audio {
        args.push("-map".to_string());
        args.push("[outa]".to_string());
    }
    side_files::push_maps(&side, &mut args);

    let muted = timeline_has_audio && !want_audio && pass != PassPhase::First && !c.is_video_only();
    push_output_opts(
        &mut args,
        opts,
        &fmt,
        (graph.has_video, graph.has_audio),
        holds_peak_ceiling(&timeline.master, opts),
        muted,
        pass,
        passlog,
    );

    if pass == PassPhase::First {
        args.push("-an".to_string());
        args.push("-f".to_string());
        args.push("null".to_string());
        args.push(null_sink.to_string());
    } else {
        args.push(output_path.to_string());
    }
    Ok(args)
}

/// Whether the export's mix is held under a peak ceiling: the master limiter is on, or a loudness
/// target (`loudness` / `loudnorm`, moving or static) closes the mix at its true-peak limit.
fn holds_peak_ceiling(master: &crate::model::MasterBus, opts: &ExportOptions) -> bool {
    master.limiter || opts.loudness.is_some() || opts.loudnorm
}

/// What follows an output's maps: the encoders and their settings, an explicit mute (`muted`: the
/// cut has sound and the export drops it) and the muxer's flags. `(has_video, has_audio)` is
/// whether the graph feeds the output a picture and a sound; `peak_ceiling` is
/// [`holds_peak_ceiling`] for the mix it carries.
#[allow(clippy::too_many_arguments)]
fn push_output_opts(
    args: &mut Vec<String>,
    opts: &ExportOptions,
    fmt: &ExportFormat,
    (has_video, has_audio): (bool, bool),
    peak_ceiling: bool,
    muted: bool,
    pass: PassPhase,
    passlog: &str,
) {
    // ---- video output options (only when a codec is explicitly chosen; a bare
    // default still maps [outv] and lets ffmpeg pick the encoder, as before) ----
    if has_video {
        if let Some(vc) = opts.video_codec.as_deref() {
            args.push("-c:v".to_string());
            args.push(vc.to_string());
            push_video_opts(args, opts, vc, pass, passlog);
            // `-pix_fmt` must equal the graph terminal `format=`; gif is pal8, and a VAAPI
            // encoder's frames are on the device, formatted by the graph's tail.
            if vc != "gif" && enc_family(vc) != EncFamily::Vaapi {
                args.push("-pix_fmt".to_string());
                args.push(fmt.pix_fmt.clone());
            }
        }
    }

    // ---- audio output options ----
    if has_audio {
        if let Some(ac) = opts.audio_codec.as_deref() {
            args.push("-c:a".to_string());
            args.push(ac.to_string());
            // The native encoder's perceptual noise substitution adds isolated +3..+8 dBTP spikes
            // after click-like transients, through a mix the limiter holds under its ceiling.
            if ac == "aac" && peak_ceiling {
                args.push("-aac_pns".to_string());
                args.push("0".to_string());
            }
            match ac {
                "aac" | "libmp3lame" | "libopus" => {
                    if let Some(b) = &opts.audio_bitrate {
                        args.push("-b:a".to_string());
                        args.push(b.clone());
                    }
                }
                "flac" => {
                    if let Some(lvl) = opts.flac_compression {
                        args.push("-compression_level".to_string());
                        args.push(lvl.to_string());
                    }
                }
                _ => {}
            }
        }
    }
    // Explicit mute: the timeline has audio but the user dropped it (distinct
    // from a timeline that simply has no audio).
    if muted {
        args.push("-an".to_string());
    }

    // ---- muxer / misc (skipped on the two-pass analysis pass, whose output is
    // the null muxer — it rejects mov/gif muxer options like -movflags) ----
    if pass != PassPhase::First {
        if opts.faststart && opts.container.supports_faststart() {
            args.push("-movflags".to_string());
            args.push("+faststart".to_string());
        }
        if opts.container == Container::Gif {
            args.push("-loop".to_string());
            args.push(if opts.gif_loop { "0" } else { "-1" }.to_string());
        }
        if let Some(title) = opts.metadata_title.as_deref().filter(|t| !t.is_empty()) {
            // One argv token via Command::args — no shell quoting; spaces/= are safe.
            args.push("-metadata".to_string());
            args.push(format!("title={title}"));
        }
    }
}

/// Append the `-c:v`-private options for `vc`: rate control, speed preset,
/// tune / profile and the HEVC `hvc1` tag. Must run after `-c:v` is pushed or
/// ffmpeg silently drops these.
fn push_video_opts(args: &mut Vec<String>, opts: &ExportOptions, vc: &str, pass: PassPhase, passlog: &str) {
    // ProRes and gif drive quality elsewhere (profile / palette), not rate control.
    if vc == "prores_ks" {
        args.push("-profile:v".to_string());
        args.push(opts.prores_profile.unwrap_or(3).to_string());
        return;
    }
    if vc == "gif" {
        return;
    }

    let fam = enc_family(vc);

    // ---- rate control / quality (spelled per encoder family) ----
    match opts.rate_control {
        RateControl::Crf => match fam {
            EncFamily::Software => {
                if let Some(n) = opts.crf {
                    args.push("-crf".to_string());
                    args.push(n.to_string());
                }
                // VP9 constant-quality requires -crf paired with -b:v 0.
                if vc == "libvpx-vp9" {
                    args.push("-b:v".to_string());
                    args.push("0".to_string());
                }
            }
            // NVENC: VBR steered by a constant-quality target (`-cq`), with no
            // average-bitrate target so quality (not size) drives the encode.
            EncFamily::Nvenc => {
                args.push("-rc".to_string());
                args.push("vbr".to_string());
                if let Some(n) = opts.crf {
                    args.push("-cq".to_string());
                    args.push(n.to_string());
                }
                args.push("-b:v".to_string());
                args.push("0".to_string());
            }
            // QSV: `-global_quality` is its CRF analogue (ICQ mode).
            EncFamily::Qsv => {
                if let Some(n) = opts.crf {
                    args.push("-global_quality".to_string());
                    args.push(n.to_string());
                }
            }
            // VideoToolbox has no CRF — map onto its 1..100 quality scale.
            EncFamily::VideoToolbox => {
                if let Some(n) = opts.crf {
                    args.push("-q:v".to_string());
                    args.push(crf_to_vt_quality(n).to_string());
                }
            }
            // AMF: constant QP, on the scale `hw_qp` maps the CRF to.
            EncFamily::Amf => {
                args.push("-rc".to_string());
                args.push("cqp".to_string());
                if let Some(n) = opts.crf {
                    let qp = hw_qp(vc, n).to_string();
                    args.push("-qp_i".to_string());
                    args.push(qp.clone());
                    args.push("-qp_p".to_string());
                    args.push(qp);
                }
            }
            // VAAPI: constant QP too; `-global_quality` is what every VAAPI encoder
            // reads it from (`-qp` exists on H.264 and HEVC only).
            EncFamily::Vaapi => {
                args.push("-rc_mode".to_string());
                args.push("CQP".to_string());
                if let Some(n) = opts.crf {
                    args.push("-global_quality".to_string());
                    args.push(hw_qp(vc, n).to_string());
                }
            }
        },
        RateControl::Bitrate => {
            if let Some(b) = &opts.video_bitrate {
                args.push("-b:v".to_string());
                args.push(b.clone());
            }
            // VBV caps: x264 / x265 and NVENC honour -maxrate/-bufsize; the other
            // HW families ignore or reject them, so only emit where they apply.
            if matches!(fam, EncFamily::Software | EncFamily::Nvenc) {
                if let Some(m) = &opts.max_rate {
                    args.push("-maxrate".to_string());
                    args.push(m.clone());
                }
                if let Some(b) = &opts.buf_size {
                    args.push("-bufsize".to_string());
                    args.push(b.clone());
                }
            }
        }
        // Two-pass is gated to software encoders (validate_export rejects it for
        // HW families, whose multi-pass uses different flags).
        RateControl::TwoPass => {
            if let Some(b) = &opts.video_bitrate {
                args.push("-b:v".to_string());
                args.push(b.clone());
            }
            args.push("-pass".to_string());
            args.push(if pass == PassPhase::First { "1" } else { "2" }.to_string());
            if !passlog.is_empty() {
                args.push("-passlogfile".to_string());
                args.push(passlog.to_string());
            }
        }
        RateControl::Lossless => match fam {
            EncFamily::Software => match vc {
                "libx264" | "libx265" | "libsvtav1" => {
                    args.push("-crf".to_string());
                    args.push("0".to_string());
                }
                "libvpx-vp9" => {
                    args.push("-lossless".to_string());
                    args.push("1".to_string());
                }
                _ => {}
            },
            // The extreme of each HW family's quality knob (visually lossless,
            // not necessarily bit-exact).
            EncFamily::Nvenc => {
                args.push("-rc".to_string());
                args.push("constqp".to_string());
                args.push("-qp".to_string());
                args.push("0".to_string());
            }
            EncFamily::Qsv => {
                args.push("-global_quality".to_string());
                args.push("1".to_string());
            }
            EncFamily::VideoToolbox => {
                args.push("-q:v".to_string());
                args.push("100".to_string());
            }
            EncFamily::Amf => {
                args.push("-rc".to_string());
                args.push("cqp".to_string());
                args.push("-qp_i".to_string());
                args.push("0".to_string());
                args.push("-qp_p".to_string());
                args.push("0".to_string());
            }
            EncFamily::Vaapi => {
                args.push("-rc_mode".to_string());
                args.push("CQP".to_string());
                args.push("-global_quality".to_string());
                args.push("1".to_string());
            }
        },
    }

    // ---- speed preset ----
    match fam {
        EncFamily::Software => match vc {
            // Named for x264 / x265 / svt-av1, -cpu-used for libvpx-vp9.
            "libx264" | "libx265" | "libsvtav1" => {
                if let Some(p) = &opts.preset {
                    args.push("-preset".to_string());
                    args.push(p.clone());
                }
            }
            "libvpx-vp9" => {
                args.push("-cpu-used".to_string());
                args.push(opts.preset.clone().unwrap_or_else(|| "4".to_string()));
                args.push("-deadline".to_string());
                args.push("good".to_string());
                args.push("-row-mt".to_string());
                args.push("1".to_string());
            }
            _ => {}
        },
        // NVENC (p1..p7 / named) and QSV (veryfast..veryslow) take `-preset`.
        EncFamily::Nvenc | EncFamily::Qsv => {
            if let Some(p) = &opts.preset {
                args.push("-preset".to_string());
                args.push(p.clone());
            }
        }
        // At one QP `h264_amf` came out 2 % smaller and 0.06 dB better with it (RX 7900 XT).
        EncFamily::Amf => {
            args.push("-quality".to_string());
            args.push("quality".to_string());
        }
        // VideoToolbox and VAAPI have no `-preset` knob in this shape.
        EncFamily::VideoToolbox | EncFamily::Vaapi => {}
    }

    // -tune: software x264 / x265 only. Emit only a tune the encoder accepts
    // (x265 lacks film/stillimage) so a stale value never fails encoder open.
    if matches!(vc, "libx264" | "libx265") {
        if let Some(t) = opts.tune.as_deref().filter(|t| video_tunes(vc).contains(t)) {
            args.push("-tune".to_string());
            args.push(t.to_string());
        }
    }
    // -profile:v applies to every h264 / hevc encoder (software and hardware).
    if is_h264(vc) || is_hevc(vc) {
        if let Some(p) = &opts.profile_v {
            args.push("-profile:v".to_string());
            args.push(p.clone());
        }
    }
    // HEVC in mp4/mov needs the hvc1 tag or QuickTime / iOS refuse to play it.
    if is_hevc(vc) && matches!(opts.container, Container::Mp4 | Container::Mov) {
        args.push("-tag:v".to_string());
        args.push("hvc1".to_string());
    }
}

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
const PREVIEW_STREAM_WIDTH: u32 = 960;

/// The widest and narrowest stream asked for. The top is the MCP frame tools' own cap and the
/// preview's (`preview-size.ts`): the engine never scales a picture past its own width, so the
/// bound is only on cost.
pub const MAX_STREAM_WIDTH: u32 = 1920;
pub const MIN_STREAM_WIDTH: u32 = 64;

/// The fastest a stream plays. The webview paints at the display's rate, so frames past this
/// are composited and shipped for nothing.
pub const MAX_STREAM_FPS: f64 = 60.0;
pub const MIN_STREAM_FPS: f64 = 1.0;

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
const PREVIEW_STREAM_QUALITY: u8 = 6;

/// The preview's render size: the export geometry scaled down to at most
/// `max_width`, keeping the aspect and staying even (yuv420 needs it).
fn preview_resolution(timeline: &Timeline, assets: &[Asset], max_width: u32) -> (u32, u32) {
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
fn build_preview_args(
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
fn build_preview_args_with(
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
struct PreviewCut {
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

fn preview_cut(
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
fn preview_parts(cut: &PreviewCut, assets: &[Asset], limits: segments::SegmentLimits) -> Option<segments::PartsPlan> {
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
fn next_jpeg(buf: &[u8]) -> Option<(usize, usize)> {
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
fn stream_preview_in(
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
struct PreviewTimeouts {
    first: std::time::Duration,
    stall: std::time::Duration,
}

const PREVIEW_TIMEOUTS: PreviewTimeouts = PreviewTimeouts {
    first: std::time::Duration::from_secs(30),
    stall: std::time::Duration::from_secs(15),
};

/// One ffmpeg of a playback stream: its stdout read on a side thread so the pump can give up on a
/// silent one (`recv_timeout`) instead of blocking in `read`, its stderr drained on another so a
/// warning flood can't deadlock the frame read. Killed and reaped when dropped.
struct PreviewRun {
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
    fn start(mut child: std::process::Child, first: u64, hw: bool, graph: GraphScript) -> Self {
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
fn pump_preview(
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
fn pump_runs(
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

/// Render the timeline by driving the `ffmpeg` binary with a generated
/// `filter_complex` (trim + per-clip volume + normalize + concat).
// With the `libav-render` feature the in-process libav executor is used instead.
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render(timeline: &Timeline, assets: &[Asset], output: &Path, _format: &str) -> Result<()> {
    render_with(timeline, assets, output, &ExportOptions::default())
}

/// Progress emitted during an export: `fraction` in `0.0..=1.0` over the **whole** export (its
/// stages each own a share of the bar, see [`progress::StageSplit`]), wall-clock `elapsed_secs`
/// since the export began, and an `eta_secs` estimate once enough has rendered to extrapolate.
/// Derived from ffmpeg's `-progress` stream.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ExportProgress {
    pub fraction: f64,
    pub elapsed_secs: f64,
    pub eta_secs: Option<f64>,
    /// Set while the work has not started: the machine is busy with another heavy job
    /// (`waiting for the preview proxy`), or the animated titles are being drawn first
    /// (`rendering animated titles`) — so the export does not look hung at 0%. Wins over `stage`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<&'static str>,
    /// What the export is doing now, once it is working ([`ExportStage`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<ExportStage>,
    /// Set while an export that is rendered in parts ([`ExportPart`]) is under way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<ExportPart>,
}

/// Where an export that is rendered in parts has got to: "rendering part 2 of 5", then, with every
/// part done, "encoding" (the one pass that joins them, mixes the sound and writes the file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ExportPart {
    /// The part being rendered, from 1 (the last part, when `encoding`).
    pub index: u32,
    /// How many parts the export is rendered in.
    pub total: u32,
    /// Every part is rendered and the file is being written.
    pub encoding: bool,
}

impl std::fmt::Display for ExportPart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.encoding {
            write!(f, "encoding")
        } else {
            write!(f, "rendering part {} of {}", self.index, self.total)
        }
    }
}

impl ExportProgress {
    /// A report that waits (`why`), at `fraction` of the bar.
    fn queued(fraction: f64, elapsed_secs: f64, why: &'static str) -> Self {
        Self {
            fraction,
            elapsed_secs,
            eta_secs: None,
            waiting: Some(why),
            stage: None,
            part: None,
        }
    }
}

/// Whether an export ran to completion or was stopped by the cancel callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderStatus {
    Completed,
    Cancelled,
}

/// Like [`render`] but with explicit export options. Validates the options
/// against the timeline's available streams before launching, and runs ffmpeg
/// twice for [`RateControl::TwoPass`]. The no-op-callback wrapper over
/// [`render_with_progress`], so both share one code path.
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render_with(timeline: &Timeline, assets: &[Asset], output: &Path, opts: &ExportOptions) -> Result<()> {
    render_with_progress(timeline, assets, output, opts, &mut |_| {}, &|| false).map(|_| ())
}

/// When `path` was last written, or `None` when there is no such file: the "before" of a
/// render, taken ahead of it so [`discard_partial`] can tell whether the run touched the file.
pub fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Remove what a failed or cancelled render left at `path` — but only if this run touched it
/// (`before` is the [`file_mtime`] taken ahead of the run). A failure before ffmpeg opened the
/// output (bad options, an empty cut, a cancel while the export was still queued) must not
/// delete the earlier export still sitting at that path; an unconditional remove did.
pub fn discard_partial(path: &Path, before: Option<std::time::SystemTime>) {
    if file_mtime(path) != before {
        let _ = std::fs::remove_file(path);
    }
}

/// Run `render` (a render of `output`) and [`discard_partial`] what it left unless it completed.
fn discarding_partial(output: &Path, render: impl FnOnce() -> Result<RenderStatus>) -> Result<RenderStatus> {
    let before = file_mtime(output);
    let result = render();
    if !matches!(result, Ok(RenderStatus::Completed)) {
        discard_partial(output, before);
    }
    result
}

/// Like [`render_with`] but streams [`ExportProgress`] to `progress` and polls
/// `cancel` between updates — returning [`RenderStatus::Cancelled`] when it trips.
/// A render that fails or is cancelled **removes what it wrote** ([`discard_partial`]: only if
/// this run touched the file), so no caller has to remember to clean up after one.
///
/// When the options request hardware decode (`opts.hwaccel`), an input is
/// really handed it, and the render fails, it is retried once fully in software
/// — so defaulting exports to GPU decode can never lose a render that plain
/// software decoding would have produced (`-hwaccel auto` already falls back at
/// init, a named accelerator does not, and a decoder can fail mid-stream). A
/// retry that completes after a failure that reads as the hardware's turns
/// hardware decode off for the process, and from then on the symbolic `auto`
/// is dropped from an export's options, as it is under `KERF_HWACCEL=none` (a
/// named accelerator never is; nor does a rescue of a preview, which is not
/// classified, count).
#[cfg_attr(feature = "libav-render", allow(dead_code))]
pub fn render_with_progress(
    timeline: &Timeline,
    assets: &[Asset],
    output: &Path,
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    let mut opts = ExportOptions {
        hwaccel: export_hwaccel_request(
            opts.hwaccel.as_deref(),
            hwaccel().is_some(),
            HWACCEL_PROVEN_BAD.load(std::sync::atomic::Ordering::Relaxed),
        ),
        ..opts.clone()
    };
    // An asset stored before the tick was recorded is asked for it now, when a run of its blended clips could fold.
    let assets = &*backfilled(timeline, assets, &opts);
    let started = Instant::now();
    let status = discarding_partial(output, || {
        // A request that would be refused anyway is refused before the titles are drawn (and before
        // a static loudness normalization spends a pass metering the mix).
        let (has_video, has_audio) = export_preflight(timeline, assets, &opts)?;
        // The metering pass owns the first part of the bar, the render the rest. Each reports in its
        // own 0..1 and `within` puts it on the whole export's.
        let meter = metering_share(meters_loudness_first(&opts, has_audio), has_video);
        match measure_static_loudness(
            timeline,
            assets,
            &mut opts,
            has_audio,
            &mut |p| progress(p.within(0.0, meter, started)),
            cancel,
        ) {
            Err(Error::Cancelled) => return Ok(RenderStatus::Cancelled),
            r => r?,
        }
        let opts = &opts;
        let mut render_bar = |p: ExportProgress| progress(p.within(meter, 1.0, started));
        // The animated titles are pictures on disk before any ffmpeg is spawned: drawn once for this
        // export — a second pass and the software retry read the same files — and kept until it is over.
        let Some(_titles) = svg_layer::prepare_export(timeline, assets, opts, &mut render_bar, cancel)? else {
            return Ok(RenderStatus::Cancelled);
        };
        // The captions and chapters to mux in are files for the length of the export. The caption
        // files beside it are checked first and written only once it has completed, so a cancelled or
        // failed export never touches a file already there.
        let (opts, _side) = side_files::prepare(timeline, opts)?;
        side_files::check_sidecars(output, &opts)?;
        retry_export_in_software(
            &opts,
            |o, hw_used| render_attempt(timeline, assets, output, o, &mut render_bar, cancel, hw_used),
            convict_hardware_decode,
        )
    })?;
    // After the render is safe: a caption file that cannot be written is reported, but the
    // finished video is not thrown away for it.
    if status == RenderStatus::Completed {
        side_files::write_sidecars(timeline, output, &opts)?;
    }
    Ok(status)
}

/// Fill in `opts.loudness_gain_db` for a static loudness normalization that has not been
/// measured yet: one metering pass over the export's own mix, without the normalization
/// (see [`ExportOptions::loudness_static`]). Nothing for any other export, nor for one that
/// writes no sound (a cut without any, or a container that carries none).
///
/// The pass reports in its own `0.0..=1.0` (the length of the mix metered so far, which it walks
/// — a known duration — as [`ExportStage::Metering`]), after a `waiting` line while it is queued
/// for the slot; the caller puts that on the export's bar.
fn measure_static_loudness(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &mut ExportOptions,
    has_audio: bool,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<()> {
    if !meters_loudness_first(opts, has_audio) {
        return Ok(());
    }
    let started = Instant::now();
    let mut status = |metering: levels::Metering| {
        let elapsed = started.elapsed().as_secs_f64();
        progress(match metering {
            levels::Metering::Waiting(wait) => ExportProgress::queued(0.0, elapsed, wait.message()),
            levels::Metering::Running(fraction) => ExportProgress {
                fraction,
                elapsed_secs: elapsed,
                eta_secs: None,
                waiting: None,
                stage: Some(ExportStage::Metering),
                part: None,
            },
        });
    };
    // A mix too quiet to measure is left at its level (the limiter still holds the peaks).
    opts.loudness_gain_db = Some(levels::static_loudness_gain(timeline, assets, opts, &mut status, cancel)?.unwrap_or(0.0));
    tracing::debug!(gain_db = ?opts.loudness_gain_db, "measured the mix for static loudness normalization");
    Ok(())
}

/// Whether an export meters its mix before it renders (pure): a static loudness normalization
/// with a target, whose gain has not been measured yet, over a cut that writes sound.
fn meters_loudness_first(opts: &ExportOptions, has_audio: bool) -> bool {
    let target = opts.loudness.is_some() || opts.loudnorm;
    let writes_sound = has_audio && opts.include_audio && !opts.container.is_video_only();
    opts.loudness_static && target && writes_sound && opts.loudness_gain_db.is_none()
}

/// Run an export attempt, and once more in software when one that asked an input for hardware
/// decode failed. When the retry completes **and the first failure reads as the hardware's**
/// (`looks_like_hardware_decode_failure` over its error text: a named accelerator — `d3d11va`,
/// unlike `auto` — exits non-zero with no device behind it) `blame_hardware` is told, so later
/// exports stop asking (`export_hwaccel_request`). A failure that does not read that way (a
/// stall, a locked output) is still rerun, as it always was, but convicts nothing. A run that
/// asked no input for hardware is not rerun at all: its software rebuild would be the same argv.
fn retry_export_in_software(
    opts: &ExportOptions,
    mut attempt: impl FnMut(&ExportOptions, &std::cell::Cell<bool>) -> Result<RenderStatus>,
    blame_hardware: impl FnOnce(),
) -> Result<RenderStatus> {
    let hw_used = std::cell::Cell::new(false);
    match attempt(opts, &hw_used) {
        Err(e) if hw_used.get() => {
            tracing::warn!(error = %e, "export with hardware decode failed; retrying with software decode");
            let hardware_failure = looks_like_hardware_decode_failure(&e.to_string());
            let sw = ExportOptions {
                hwaccel: None,
                ..opts.clone()
            };
            let again = attempt(&sw, &std::cell::Cell::new(false));
            if hardware_failure && matches!(again, Ok(RenderStatus::Completed)) {
                blame_hardware();
            }
            again
        }
        result => result,
    }
}

/// Tune a built export argv for the process about to be spawned: each input's `-hwaccel` per
/// `decode_policy` (on Windows `auto` becomes `d3d11va` for H.264 and HEVC), a VAAPI export's
/// device swapped for the render node the probe verified, the threads left to
/// [`run_ffmpeg_progress`]. The builders emit the options as given. Returns whether any input
/// still asks for hardware decode.
fn tune_export_args(args: &mut Vec<String>, assets: &[Asset], os: HostOs) -> bool {
    if args.iter().any(|a| a == "-vaapi_device") {
        set_vaapi_device(args, vaapi_device_within(VAAPI_DEVICE_WAIT));
    }
    tune_inputs(args, os, Reading::Whole, &facts_from_assets(assets)).hw
}

/// The filters a graph may hold and still be hardware from end to end: the picture goes through
/// the cores only to be scaled, laid over a canvas and handed to the encoder (about 5 % of a
/// hardware export's wall time, measured; a VAAPI export's `format=nv12,hwupload` tail is a copy of
/// each frame to the device and belongs to the same 5 %), and the sound through filters that cost next to nothing
/// beside a video encode. Anything else (`zscale` and the tone-map, `geq`, `rotate`, `v360`,
/// `drawtext`, a blur, a grade, a chroma key, a keyed opacity) is per-pixel work on the cores, and
/// a name nobody here has judged is such work until it is.
const HARDWARE_GRAPH_FILTERS: [&str; 34] = [
    // picture
    "color",
    "format",
    "trim",
    "setpts",
    "scale",
    "pad",
    "setsar",
    "fps",
    "overlay",
    "null",
    "split",
    "crop",
    "settb",
    "fade",
    "hwupload",
    // sound
    "anullsrc",
    "atrim",
    "asetpts",
    "atempo",
    "volume",
    "afade",
    "aformat",
    "adelay",
    "amix",
    "apad",
    "pan",
    "alimiter",
    "aresample",
    "asplit",
    "anull",
    "loudnorm",
    "acompressor",
    "agate",
    "asetnsamples",
];

/// The filter names of a `-filter_complex` value, in order (pure, unit-tested): what follows each
/// run of `[label]`s at the start of a filter, up to its `=` or `[`. Commas and semicolons inside
/// single quotes or after a backslash are the filter's own (an expression), not the next filter.
fn filter_names(graph: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let bytes = graph.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Labels and spaces in front of a filter.
        while i < bytes.len() && (bytes[i] == b'[' || bytes[i].is_ascii_whitespace()) {
            if bytes[i] == b'[' {
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
            }
            i += 1;
        }
        let from = i;
        while i < bytes.len() && !matches!(bytes[i], b'=' | b'[' | b',' | b';') && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i > from {
            names.push(&graph[from..i]);
        }
        // The rest of this filter: to the next `,` or `;` that is not quoted or escaped.
        let mut quoted = false;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 1,
                b'\'' => quoted = !quoted,
                b',' | b';' if !quoted => break,
                _ => {}
            }
            i += 1;
        }
        i += 1;
    }
    names
}

/// Which resource a built export argv (after `tune_inputs`, before the graph is spilled to a file)
/// spends (pure, unit-tested): the hardware's slot ([`cpu::Resource::Gpu`]) only when the run is
/// hardware from end to end, and the cores' for everything else, which includes every doubt.
///
/// Hardware from end to end means all of: the lane is on; the video encoder is a hardware one
/// (`h264_amf`, `hevc_nvenc`, ...); every input is either a still image or a video that is
/// handed a **named** accelerator (`d3d11va`: it exits non-zero without a device where `auto`
/// falls back to software without a word, and a run that does that is a software run holding a
/// hardware slot) and that accelerator decodes (`InputFacts::hardware_decodes`: H.264 4:2:0 at 8 bit,
/// HEVC 4:2:0 at 8 or 10, by what the asset records);
/// and the graph holds only [`HARDWARE_GRAPH_FILTERS`]. A run asked for no hardware decode at all,
/// as the rerun after a failure is, fails the second test and is the cores'.
fn export_resource(args: &[String], facts_of: &dyn Fn(&str) -> InputFacts, lane_on: bool) -> cpu::Resource {
    if !lane_on {
        return cpu::Resource::Cpu;
    }
    let value_of = |flag: &str| args.iter().position(|a| a == flag).and_then(|at| args.get(at + 1));
    if !value_of("-c:v").is_some_and(|enc| enc_family(enc) != EncFamily::Software) {
        return cpu::Resource::Cpu;
    }
    // Inputs, in groups: everything up to and including each `-i <path>`.
    let mut group_hwaccel: Option<&str> = None;
    let mut side_input = false;
    let mut graph: Option<&str> = None;
    let mut inputs = 0;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-hwaccel" => group_hwaccel = rest.next().map(String::as_str),
            // The captions and chapters a render muxes in decode no picture.
            "-f" => side_input = rest.next().is_some_and(|f| side_files::is_side_format(f)),
            "-i" if side_input => {
                side_input = false;
                rest.next();
            }
            "-i" => {
                inputs += 1;
                let Some(path) = rest.next() else { return cpu::Resource::Cpu };
                let facts = facts_of(path);
                let video = facts.hardware_decodes();
                let named = group_hwaccel.is_some_and(|h| {
                    !h.trim().is_empty() && !h.trim().eq_ignore_ascii_case("auto") && !h.trim().eq_ignore_ascii_case("none")
                });
                if !(facts.image || (video && named)) {
                    return cpu::Resource::Cpu;
                }
                group_hwaccel = None;
            }
            "-filter_complex" => graph = rest.next().map(String::as_str),
            _ => {}
        }
    }
    let Some(graph) = graph.filter(|_| inputs > 0) else {
        return cpu::Resource::Cpu;
    };
    if filter_names(graph).iter().all(|name| HARDWARE_GRAPH_FILTERS.contains(name)) {
        cpu::Resource::Gpu
    } else {
        cpu::Resource::Cpu
    }
}

/// Point a built argv's `-vaapi_device` at `device`, when the probe found one (pure). Without
/// one the builder's stand-in stays, and the export fails with ffmpeg's own words for it.
fn set_vaapi_device(args: &mut [String], device: Option<&str>) {
    let (Some(device), Some(at)) = (device, args.iter().position(|a| a == "-vaapi_device")) else {
        return;
    };
    if let Some(value) = args.get_mut(at + 1) {
        *value = device.to_string();
    }
}

/// One delivery of a multi-format export: the frame and where its file goes.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportVariant {
    pub delivery: Delivery,
    pub output: PathBuf,
}

impl ExportVariant {
    /// The variant's file beside `base`, its shape spliced into the name:
    /// `cut.mp4` at 9:16 becomes `cut-9x16.mp4`. The `x` rather than a `:`
    /// because a colon is not a filename character on Windows.
    pub fn beside(base: &Path, delivery: Delivery) -> Self {
        let (w, h) = delivery.ratio();
        let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let name = match base.extension() {
            Some(ext) => format!("{stem}-{w}x{h}.{}", ext.to_string_lossy()),
            None => format!("{stem}-{w}x{h}"),
        };
        Self {
            delivery,
            output: base.with_file_name(name),
        }
    }
}

/// Check every variant of a multi-format export and measure what they share before any is
/// rendered: each variant's preflight first (one that would be refused fails before anything
/// runs), then a static loudness normalization's metering pass, once for all of them — the
/// mix is the same in every shape. Returns the options to render every variant with (the
/// measured gain filled in), or `None` when cancelled while metering. [`render_variants`]
/// runs it; a caller rendering the variants one call at a time runs it first and passes its
/// options on, so the mix is not measured once per file.
pub fn prepare_variants(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<Option<ExportOptions>> {
    prepare_variants_sharing(timeline, assets, variants, opts, Instant::now(), progress, cancel)
        .map(|prepared| prepared.map(|(opts, _)| opts))
}

/// [`prepare_variants`] on the bar of an export that began at `started`: the options, and the
/// share of the bar the metering pass took (0 when it measured nothing), which the renders that
/// follow start after.
fn prepare_variants_sharing(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    started: Instant,
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<Option<(ExportOptions, f64)>> {
    let (mut has_video, mut has_audio) = (false, false);
    for variant in variants {
        let per_variant = ExportOptions {
            resolution: Some((variant.delivery.width, variant.delivery.height)),
            fit: variant.delivery.fit,
            ..opts.clone()
        };
        let (video, audio) = export_preflight(&timeline.for_delivery(variant.delivery), assets, &per_variant)?;
        has_video |= video;
        has_audio |= audio;
    }
    let mut opts = opts.clone();
    let total = variants.len();
    let meter = metering_share(meters_loudness_first(&opts, has_audio), has_video);
    let mut on_meter = |p: ExportProgress| progress(VariantProgress::of(p.within(0.0, meter, started), 0, total, false));
    match measure_static_loudness(timeline, assets, &mut opts, has_audio, &mut on_meter, cancel) {
        Err(Error::Cancelled) => Ok(None),
        r => r.map(|()| Some((opts, meter))),
    }
}

/// Progress across a multi-format export: the overall [`ExportProgress`] plus
/// which variant is rendering, so a bar can say "2 of 3 · 9:16" — or, for variants
/// rendered in one run (`together`), that all of them are.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct VariantProgress {
    /// Zero-based index of the variant being rendered.
    pub variant: usize,
    pub total: usize,
    /// Progress through all of them: each variant owns an equal share.
    pub fraction: f64,
    pub elapsed_secs: f64,
    /// Time left across the remaining variants, estimated from how long the
    /// completed share has taken.
    pub eta_secs: Option<f64>,
    /// [`ExportProgress::waiting`]: queued for the slot, or drawing the animated titles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waiting: Option<&'static str>,
    /// [`ExportProgress::stage`]: what the export is doing now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<ExportStage>,
    /// [`ExportProgress::part`]: the file in flight is rendered in parts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<ExportPart>,
    /// Every variant is rendering in the one run ([`render_variants`] `together`): `variant`
    /// is then 0 and `fraction` is each file's as much as the whole.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub together: bool,
}

impl VariantProgress {
    /// `p` — already on the whole export's bar — as the report of variant `variant` of `total`.
    fn of(p: ExportProgress, variant: usize, total: usize, together: bool) -> Self {
        Self {
            variant,
            total,
            fraction: p.fraction,
            elapsed_secs: p.elapsed_secs,
            eta_secs: p.eta_secs,
            waiting: p.waiting,
            stage: p.stage,
            part: p.part,
            together,
        }
    }
}

/// Refuse an export whose caption files would replace ones already beside `output`, unless
/// `opts.overwrite_sidecars` allows it. A multi-format export asks this of every variant first.
pub fn check_caption_sidecars(output: &Path, opts: &ExportOptions) -> Result<()> {
    side_files::check_sidecars(output, opts)
}

/// Render the same cut once per variant — one file per delivery frame, each
/// shot wearing the crop it carries for that shape ([`Timeline::for_delivery`]).
///
/// With `together`, every variant that can is written by **one** ffmpeg run
/// ([`together::render_together`]): the sources are decoded once and `split` into each
/// frame's graph, the mix is built once and `asplit` to each file. A cancelled or failed run
/// then removes every file it touched (all of them were in flight) and reports none completed.
/// What one run cannot write (a two-pass or hardware encode, an audio-only container, two
/// variants named alike) renders one after another, as it does without `together`.
///
/// One after another, a cancelled batch is clean — the variant in flight is deleted like
/// a cancelled single export, the ones already finished are kept, and the ones
/// not started never existed. A variant that **fails** is removed the same way (its partial
/// file, if the run touched it; the finished ones stay). `opts` is the encode shared by all of
/// them; its `resolution` and `fit` are replaced per variant by the delivery. Returns the
/// status, how many files were completed and whether they rendered in one run.
pub fn render_variants(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    together: bool,
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<VariantsRun> {
    if variants.is_empty() {
        return Err(Error::InvalidArgument("no delivery formats to export".to_string()));
    }
    // (The ticks of the assets that lack one, once, for every variant and for a run of them together.)
    let assets = &*backfilled(timeline, assets, opts);
    let total = variants.len();
    let started = Instant::now();
    let run = |status, completed, together| VariantsRun {
        status,
        completed,
        together,
    };
    let Some((opts, meter)) = prepare_variants_sharing(timeline, assets, variants, opts, started, progress, cancel)? else {
        return Ok(run(
            RenderStatus::Cancelled,
            0,
            together && together::renders_together(variants, opts),
        ));
    };
    if together {
        match together::render_together(timeline, assets, variants, &opts, (meter, started), progress, cancel)? {
            Some(RenderStatus::Completed) => return Ok(run(RenderStatus::Completed, total, true)),
            Some(RenderStatus::Cancelled) => return Ok(run(RenderStatus::Cancelled, 0, true)),
            None => {}
        }
    }
    for (i, variant) in variants.iter().enumerate() {
        let framed = timeline.for_delivery(variant.delivery);
        let per_variant = ExportOptions {
            resolution: Some((variant.delivery.width, variant.delivery.height)),
            fit: variant.delivery.fit,
            ..opts.clone()
        };
        // Each variant owns an equal share of what the metering pass left of the bar.
        let (from, to) = (
            meter + (1.0 - meter) * i as f64 / total as f64,
            meter + (1.0 - meter) * (i + 1) as f64 / total as f64,
        );
        let mut on_progress = |p: ExportProgress| {
            let eta_secs = variants_eta(p.eta_secs, p.fraction, total - 1 - i);
            progress(VariantProgress::of(
                ExportProgress { eta_secs, ..p }.within(from, to, started),
                i,
                total,
                false,
            ));
        };
        // `render_with_progress` discards a failed or cancelled variant's partial file, but only
        // if this run touched it: a cancel while the batch was still queued leaves an earlier
        // export at that path alone, where the unconditional remove used here deleted it.
        let status = render_with_progress(&framed, assets, &variant.output, &per_variant, &mut on_progress, cancel)?;
        if status == RenderStatus::Cancelled {
            return Ok(run(RenderStatus::Cancelled, i, false));
        }
    }
    Ok(run(RenderStatus::Completed, total, false))
}

/// How a multi-format export ended ([`render_variants`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantsRun {
    pub status: RenderStatus,
    /// How many files were completed (none from a stopped single run: it removed them all).
    pub completed: usize,
    /// Whether the files were (or, stopped before it began, would have been) written by one run.
    pub together: bool,
}

/// [`render_with_progress`], and once the export has **completed**, a read-back of the file it wrote
/// ([`measure_export`]): its container, picture and sound as `ffprobe` finds them and, when it has
/// sound, its own integrated loudness, true peak and loudness range — what `get_levels` (the mix on
/// the timeline) cannot say once a lossy encode has moved it. The read-back is the last part of the
/// same job: it reports [`ExportStage::Measuring`] in the last [`VERIFY_SHARE`] of the bar and
/// stops on the same `cancel` — which leaves the finished file alone and says so in the report. A
/// measurement that fails never fails the export: the report says why. `None` when the export did
/// not complete (a cancelled one has removed its file).
pub fn render_measured(
    timeline: &Timeline,
    assets: &[Asset],
    output: &Path,
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<(RenderStatus, Option<ExportReport>)> {
    let started = Instant::now();
    let status = render_with_progress(
        timeline,
        assets,
        output,
        opts,
        &mut |p| progress(p.within(0.0, 1.0 - VERIFY_SHARE, started)),
        cancel,
    )?;
    if status != RenderStatus::Completed {
        return Ok((status, None));
    }
    let report = measure_export(
        output,
        written::loudness_target(opts),
        &mut |measured| {
            progress(measuring(measured, started));
        },
        cancel,
    );
    // The end of the bar is the end of the job, whatever the meter last said.
    progress(measuring(1.0, started));
    Ok((status, Some(report)))
}

/// The report of a read-back that is `measured` (0..1) through, on the bar of an export begun at `started`.
fn measuring(measured: f64, started: Instant) -> ExportProgress {
    ExportProgress {
        fraction: 1.0 - VERIFY_SHARE + VERIFY_SHARE * measured.clamp(0.0, 1.0),
        elapsed_secs: started.elapsed().as_secs_f64(),
        eta_secs: None,
        waiting: None,
        stage: Some(ExportStage::Measuring),
        part: None,
    }
}

/// [`render_variants`], and once every file has been written, a read-back of each
/// ([`render_measured`]'s, one report per file in `variants` order). Empty when the run did not
/// complete: a stopped one has removed what it touched or, one after another, keeps files that were
/// never measured.
pub fn render_variants_measured(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    together: bool,
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<(VariantsRun, Vec<ExportReport>)> {
    let started = Instant::now();
    let run = render_variants(
        timeline,
        assets,
        variants,
        opts,
        together,
        &mut |p| {
            progress(VariantProgress {
                fraction: p.fraction * (1.0 - VERIFY_SHARE),
                ..p
            });
        },
        cancel,
    )?;
    if run.status != RenderStatus::Completed {
        return Ok((run, Vec::new()));
    }
    let target = written::loudness_target(opts);
    let total = variants.len();
    let mut reports = Vec::with_capacity(total);
    for (k, variant) in variants.iter().enumerate().take(run.completed) {
        let report = measure_export(
            &variant.output,
            target,
            &mut |measured| {
                // The files take equal shares of the read-back's part of the bar.
                let of_all = (k as f64 + measured.clamp(0.0, 1.0)) / total as f64;
                progress(VariantProgress::of(measuring(of_all, started), k, total, run.together));
            },
            cancel,
        );
        progress(VariantProgress::of(
            measuring((k + 1) as f64 / total as f64, started),
            k,
            total,
            run.together,
        ));
        reports.push(report);
    }
    Ok((run, reports))
}

/// What an export is refused for before anything is drawn or spawned — an empty timeline, options
/// that would export nothing or that the container cannot hold — and whether it has picture and
/// sound. Asked ahead of the animated titles' drawing so a bad request does not spend seconds of the
/// cores on pictures nobody will use.
fn export_preflight(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Result<(bool, bool)> {
    if !timeline.tracks.iter().any(|t| !t.clips.is_empty()) {
        return Err(Error::InvalidArgument("timeline has no clips to export".to_string()));
    }

    let has_video = timeline
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let has_audio = timeline
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .any(|c| clip_sounds(c, assets));
    let issues = validate_export(opts, has_video, has_audio);
    if !issues.is_empty() {
        return Err(Error::InvalidArgument(issues.join(" ")));
    }
    check_export_luts(timeline)?;
    Ok((has_video, has_audio))
}

/// An export refuses a LUT it cannot apply (the preview and stills leave it out instead): the file
/// would otherwise be silently missing from the delivered picture. Reads the disk, off the lock.
fn check_export_luts(timeline: &Timeline) -> Result<()> {
    let clips = timeline
        .tracks
        .iter()
        .filter(|t| t.kind == StreamKind::Video)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.enabled);
    for lut in clips.filter_map(|c| c.grade.lut()) {
        let why = match crate::color_lut::inspect_lut(&lut.path) {
            Err(e) => e.to_string(),
            Ok(_) if crate::color_lut::lut_file(&lut).is_none() => "Kerf could not write its copy of it".to_string(),
            Ok(_) => continue,
        };
        return Err(Error::InvalidArgument(format!(
            "cannot export: the LUT {} cannot be applied ({why}); choose another file or clear it",
            lut.path
        )));
    }
    Ok(())
}

/// One export run with `opts` exactly as given (no fallback).
fn render_attempt(
    timeline: &Timeline,
    assets: &[Asset],
    output: &Path,
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    hw_used: &std::cell::Cell<bool>,
) -> Result<RenderStatus> {
    let (has_video, _) = export_preflight(timeline, assets, opts)?;

    // A span with more open at once than a run should hold is rendered in parts (`segments`);
    // everything under the limits keeps the single run below, argv for argv.
    if let Some(parts) = export_parts(timeline, assets, opts) {
        return segments::render_in_parts(&parts, assets, output, opts, progress, cancel, hw_used);
    }

    let output_str = output
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 output path: {}", output.display())))?;

    let two_pass = matches!(opts.rate_control, RateControl::TwoPass)
        && has_video
        && !opts.container.is_audio_only()
        && matches!(opts.video_codec.as_deref(), Some(vc) if vc != "prores_ks" && vc != "gif" && enc_family(vc) == EncFamily::Software);

    let total = match effective_range(timeline, opts) {
        Some((s, e)) => e - s,
        None => timeline.duration(),
    };
    let start = std::time::Instant::now();

    if two_pass {
        let null_sink = if cfg!(windows) { "NUL" } else { "/dev/null" };
        // ffmpeg appends "-N.log" to the passlog prefix; scope it to this process.
        let passlog = std::env::temp_dir()
            .join(format!("kerf-2pass-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        // Removed on every way out, a failed first pass and a stalled second one included.
        let _logs = PassLogs(passlog.clone());
        // Analysis pass fills the first half of the bar, the encode the second.
        let mut a1 = build_export_args_phase(timeline, assets, output_str, opts, PassPhase::First, null_sink, &passlog)?;
        hw_used.set(tune_export_args(&mut a1, assets, HostOs::CURRENT));
        let _g1 = externalize_filter_complex(&mut a1, "p1")?;
        let s1 = run_ffmpeg_progress(
            &a1,
            output,
            Bar {
                total,
                offset: 0.0,
                width: 0.5,
                start,
            },
            progress,
            cancel,
        )?;
        if s1 == RenderStatus::Cancelled {
            return Ok(RenderStatus::Cancelled);
        }
        let mut a2 = build_export_args_phase(timeline, assets, output_str, opts, PassPhase::Second, null_sink, &passlog)?;
        hw_used.set(hw_used.get() | tune_export_args(&mut a2, assets, HostOs::CURRENT));
        let _g2 = externalize_filter_complex(&mut a2, "p2")?;
        run_ffmpeg_progress(
            &a2,
            output,
            Bar {
                total,
                offset: 0.5,
                width: 0.5,
                start,
            },
            progress,
            cancel,
        )
    } else {
        let mut args = build_export_args(timeline, assets, output_str, opts)?;
        hw_used.set(tune_export_args(&mut args, assets, HostOs::CURRENT));
        // Judged on the argv as it will run, before a long graph is spilled to a file: a run
        // that asks no input for hardware (the rerun after a hardware failure) is the cores'.
        let resource = export_resource(&args, &facts_from_assets(assets), cpu::gpu_lane());
        let _g = externalize_filter_complex(&mut args, "s")?;
        run_ffmpeg_progress_as(
            &args,
            output,
            Bar {
                total,
                offset: 0.0,
                width: 1.0,
                start,
            },
            resource,
            progress,
            cancel,
        )
    }
}

/// The x264 two-pass stats files behind `prefix`, removed when dropped.
struct PassLogs(String);

impl Drop for PassLogs {
    fn drop(&mut self) {
        for suffix in ["-0.log", "-0.log.mbtree", ".log", ".log.mbtree"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

/// Longest `-filter_complex` we are willing to hand over as an argv string.
///
/// An animated reframe's `sendcmd` list dwarfs an ordinary graph — a single
/// channel at 30 fps runs to roughly 50 KB a minute — and it is *argv*, not
/// ffmpeg, that gives out first: Linux caps one argument at 128 KiB
/// (`MAX_ARG_STRLEN`) and Windows caps the whole command line at 32767
/// characters, which is about ten seconds of animation. The threshold sits well
/// below both, since spilling to a file costs nothing.
pub const GRAPH_ARG_MAX: usize = 8192;

/// Index of the `-filter_complex` *value* when it is too long to pass in argv.
fn oversized_graph_index(args: &[String]) -> Option<usize> {
    args.iter()
        .position(|a| a == "-filter_complex")
        .map(|i| i + 1)
        .filter(|&i| args.get(i).is_some_and(|g| g.len() > GRAPH_ARG_MAX))
}

/// A filtergraph spilled to a script file, removed when the render is done.
struct GraphScript(Option<PathBuf>);

impl Drop for GraphScript {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// The options this ffmpeg build knows that the engine's argv depends on, read
/// from `ffmpeg -h full` once per process. Only these booleans are kept — the help
/// text itself is over a megabyte. A binary that cannot run at all reads as
/// "knows none of them", which every probe below takes as the modern spelling: any
/// render on that binary is about to fail the same way regardless.
struct HelpFlags {
    filter_complex_script: bool,
    fps_mode: bool,
    vsync: bool,
}

fn help_flags() -> &'static HelpFlags {
    static FLAGS: OnceLock<HelpFlags> = OnceLock::new();
    FLAGS.get_or_init(|| {
        let help = output_bounded(
            command(&ffmpeg_bin()).args(["-hide_banner", "-loglevel", "quiet", "-h", "full"]),
            CAPABILITY_PROBE_TIMEOUT,
            0,
        )
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
        HelpFlags {
            filter_complex_script: help.contains("-filter_complex_script"),
            fps_mode: help.contains("-fps_mode"),
            vsync: help.contains("-vsync"),
        }
    })
}

/// The argv spelling that points *this* ffmpeg at a filtergraph file, probed
/// once per process: FFmpeg 8 removed `-filter_complex_script` (deprecated in
/// 7.0 as equivalent to the generic `-/filter_complex <file>` form), so on a
/// bundled FFmpeg 8 the old spelling aborts every spilled render with
/// `Unrecognized option` — while a pre-7.0 binary knows only the old one.
/// `-h full` still lists the option wherever it exists.
fn graph_script_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let legacy = help_flags().filter_complex_script;
        tracing::debug!(legacy, "probed ffmpeg for -filter_complex_script");
        if legacy {
            "-filter_complex_script"
        } else {
            "-/filter_complex"
        }
    })
}

/// The flag that sets how this ffmpeg paces output frames, given which of the two
/// spellings its `-h full` lists (pure, unit-tested): `-fps_mode` since 5.1, which
/// FFmpeg 9 is left with — it removed `-vsync`, `Unrecognized option` — and
/// `-vsync` before that.
fn fps_mode_flag_for(knows_fps_mode: bool, knows_vsync: bool) -> &'static str {
    if knows_fps_mode || !knows_vsync {
        "-fps_mode"
    } else {
        "-vsync"
    }
}

/// [`fps_mode_flag_for`] for this machine's ffmpeg, probed once per process: the spelling
/// every spawn outside the engine that passes `passthrough` uses.
pub fn fps_mode_flag() -> &'static str {
    static FLAG: OnceLock<&'static str> = OnceLock::new();
    FLAG.get_or_init(|| {
        let help = help_flags();
        fps_mode_flag_for(help.fps_mode, help.vsync)
    })
}

// ---- the threads of a wide graph --------------------------------------------------------------------

/// The most threads the swscale contexts of one `-filter_complex` graph may hold between them, counted as [`graph_contexts`]
/// counts, which is about twice the contexts a graph holds. A `scale` filter hands its graph's thread count
/// (`-filter_complex_threads`) to its swscale context, which starts a pool of that many threads of its own, and so does every
/// `scale` FFmpeg inserts where two filters' pixel formats differ; a slice-threaded filter that is no scaler shares the graph's one
/// pool (`hflip` 40 times: 2 threads more, `scale` 40 times: 160). A graph holds *contexts x threads*: 100 butted blended clips
/// 3,600 threads on a 4-core machine on 9.0.2 and 6.1 alike (about 10 contexts a clip), 300 plain clips of one file the same (3 a
/// clip). A process may not hold that many on macOS (`kern.num_taskthreads`, 2,560 as XNU ships it; not verified on a Mac): the
/// swscaler fails to initialise (`Resource temporarily unavailable`) and the encoder after it (`ff_frame_thread_encoder_init
/// failed`), reproduced with a cap of 2,560 threads a process. At 1,536 the pools hold about 800, and the rest of the process, its
/// decoders, its encoder and its muxer, has the room.
const GRAPH_POOL_THREADS: usize = 1536;

/// The filters of a graph that name or force a pixel format, where FFmpeg may put a scaler of its own: with the `scale` filters
/// themselves, an upper bound of the swscale contexts the graph holds (the 100 blended clips above: 1,700 against the 1,000 it
/// holds, 1.7 times; the 300 plain clips, 1.65). Nothing else is counted.
const FORMAT_FILTERS: [&str; 11] = [
    "scale",
    "format",
    "overlay",
    "blend",
    "lutyuv",
    "colorchannelmixer",
    "alphamerge",
    "alphaextract",
    "geq",
    "fade",
    "pad",
];

/// The filters of a graph, by name (pure): each piece between a `;` and a `,`, less the pads a link opens with. Quotes are not looked
/// into, so a piece of an expression is a "name" that no filter has, and a name inside one can be counted, which only overstates.
fn graph_filter_names(graph: &str) -> impl Iterator<Item = &str> {
    graph.split([';', ',']).map(|piece| {
        let mut rest = piece.trim_start();
        while let Some(after) = rest.strip_prefix('[') {
            rest = after.split_once(']').map_or("", |(_, tail)| tail).trim_start();
        }
        rest.split(['=', '[', ' ']).next().unwrap_or("")
    })
}

/// An upper bound of the swscale contexts `graph` holds (pure): its [`FORMAT_FILTERS`].
fn graph_contexts(graph: &str) -> usize {
    graph_filter_names(graph).filter(|name| FORMAT_FILTERS.contains(name)).count()
}

/// The filters whose result depends on the number of slice threads the graph runs on: `chromakey` starts its window over at each slice,
/// and `gblur`'s kernel is not the recursion on every plane shape at every count (`gblur::KernelHazard`). The GPU compositor draws the
/// one at `cpu::exact_graph_threads` and the plan judges the other over `cpu::graph_thread_range`, both the budget's count, so a graph
/// that runs either is handed exactly that count ([`cap_wide_graph`] leaves it).
const SLICE_DEPENDENT_FILTERS: [&str; 2] = ["chromakey", "gblur"];

/// Whether `graph` runs a filter whose result depends on its slice threads ([`SLICE_DEPENDENT_FILTERS`]).
fn graph_depends_on_slices(graph: &str) -> bool {
    graph_filter_names(graph).any(|name| SLICE_DEPENDENT_FILTERS.contains(&name))
}

/// The threads a graph of `contexts` swscale contexts may give each of them where the machine would give it `threads` (pure):
/// `threads` while the contexts' pools fit [`GRAPH_POOL_THREADS`], else what fits (down to 1, which starts no pool).
fn pool_threads_for(contexts: usize, threads: usize) -> usize {
    let threads = threads.max(1);
    (GRAPH_POOL_THREADS / contexts.max(1)).clamp(1, threads)
}

/// What the cap on a wide graph ([`cap_wide_graph`]) reads of the graph it is spawned with: its swscale contexts
/// ([`graph_contexts`]) and whether it runs a filter whose result depends on its slice threads ([`graph_depends_on_slices`]).
/// Bundled so that the spawn and the plan's judgement of the threads a frame is drawn on ([`wide_graph_cap`]) read the same two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphShape {
    pub contexts: usize,
    pub slice_dependent: bool,
}

/// The shape of `graph` (pure).
pub fn graph_shape(graph: &str) -> GraphShape {
    GraphShape {
        contexts: graph_contexts(graph),
        slice_dependent: graph_depends_on_slices(graph),
    }
}

/// The filter threads a graph of this `shape` is lowered to where the machine would give it `granted` (pure): `Some(n)` when it is
/// wide enough to hold more than [`GRAPH_POOL_THREADS`] between its contexts, and does not run a slice-dependent filter. The one
/// arithmetic of [`cap_wide_graph`] (the spawn) and of `RenderPlan::ten_bit_thread_range` (the plan's judgement), which is why a
/// 4:2:0 10-bit frame is drawn on the threads the export runs on and not the budget's.
pub fn wide_graph_cap(shape: GraphShape, granted: usize) -> Option<usize> {
    let granted = granted.max(1);
    let each = pool_threads_for(shape.contexts, granted);
    (each < granted && !shape.slice_dependent).then_some(each)
}

/// One run of ffmpeg that draws a stretch of the export's picture, as the plan needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphRun {
    /// The output frames the run draws (the whole export's: `0..u64::MAX` for a cut that runs whole).
    pub frames: std::ops::Range<u64>,
    /// Its `-filter_complex` graph.
    pub shape: GraphShape,
}

/// The parts an export of `timeline` is rendered in at `limits` (`None`: parts are off), else `None`: the one run. The **one** decision of
/// whole or parts, which [`render_attempt`] and the plan's account of the export ([`export_graph_runs`]) both ask, so the plan cannot
/// drift from the export.
fn export_parts_within(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    limits: Option<segments::SegmentLimits>,
) -> Option<segments::PartsPlan> {
    limits.and_then(|limits| segments::plan_export(timeline, assets, opts, limits))
}

/// [`export_parts_within`] at the process's limits (`KERF_EXPORT_PARTS`, which is constant for a process and read here as the export reads it).
fn export_parts(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Option<segments::PartsPlan> {
    export_parts_within(timeline, assets, opts, segments::limits())
}

/// The runs the export of `timeline` spawns its picture graph in, built by the export's own builders (pure): the parts [`export_parts`]
/// cuts it into, each with the graph [`segments::build_part_args`] writes, else the one run of [`build_export_args`]. What a part holds depends
/// on which clips share it: a long clip under many short ones is one wide run, the same clips in a cut that can be cut are several narrow ones.
/// `None` where the graphs are not known without more than a build: an asset whose tick is not recorded and whose blended clips could fold if
/// it were ([`time_base_inputs`]: the export probes it first), or a build that fails.
///
/// The threads of a run are the budget's, lowered to [`wide_graph_cap`] of its shape at spawn. These are the runs of an export that renders
/// this delivery alone: a multi-format export written in one run (`together`) has every variant's graph in one, and is not these.
pub fn export_graph_runs(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Option<Vec<GraphRun>> {
    export_graph_runs_within(timeline, assets, opts, segments::limits())
}

/// [`export_graph_runs`] for the part `limits` (`None`: parts are off), pure, for the tests.
fn export_graph_runs_within(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    limits: Option<segments::SegmentLimits>,
) -> Option<Vec<GraphRun>> {
    if !time_base_inputs(timeline, assets, opts).is_empty() {
        return None;
    }
    match export_parts_within(timeline, assets, opts, limits) {
        Some(parts) => {
            let floating = svg_layer::svg_sequences(&parts.cut, &parts.fmt);
            parts
                .parts
                .iter()
                .map(|part| {
                    let args = segments::build_part_args(&parts, assets, opts, part, &floating, "part.mkv", "-fps_mode").ok()?;
                    Some(GraphRun {
                        frames: part.first..part.end(),
                        shape: graph_shape(&filter_graph_text(&args)?),
                    })
                })
                .collect()
        }
        None => {
            let args = build_export_args(timeline, assets, "export.mkv", opts).ok()?;
            Some(vec![GraphRun {
                frames: 0..u64::MAX,
                shape: graph_shape(&filter_graph_text(&args)?),
            }])
        }
    }
}

/// The text of the `-filter_complex` graph an argv runs (inline, or the script it points to), `None` when it has none.
fn filter_graph_text(args: &[String]) -> Option<String> {
    let at = args
        .iter()
        .position(|a| matches!(a.as_str(), "-filter_complex" | "-filter_complex_script" | "-/filter_complex"))?;
    let value = args.get(at + 1)?;
    if args[at] == "-filter_complex" {
        Some(value.clone())
    } else {
        std::fs::read_to_string(value).ok()
    }
}

/// Cap the filter threads of a graph that is wide enough to hold more than [`GRAPH_POOL_THREADS`] between its swscale contexts
/// (see there) to what fits: `-filter_complex_threads` with the smaller of what the argv has and the cap, at the head of the argv.
/// Nothing for a graph that fits, so every ordinary export is handed what it always was, and nothing for one that runs a filter
/// whose result depends on the slice threads ([`graph_depends_on_slices`]): the plan and the compositor draw it at the budget's count,
/// and a lowered one would put the export's frame off theirs (such a wide graph is exposed to a process's thread limit as it was).
fn cap_wide_graph(args: &mut Vec<String>, budget: cpu::Budget) {
    let Some(graph) = filter_graph_text(args) else {
        return;
    };
    let granted = budget.threads().max(1);
    let Some(each) = wide_graph_cap(graph_shape(&graph), granted) else {
        return;
    };
    let first_input = args.iter().position(|a| a == "-i").unwrap_or(args.len());
    match args[..first_input].iter().position(|a| a == "-filter_complex_threads") {
        Some(at) if at + 1 < first_input => {
            let had = args[at + 1].parse::<usize>().unwrap_or(granted);
            args[at + 1] = had.min(each).to_string();
        }
        _ => {
            args.splice(0..0, ["-filter_complex_threads".to_string(), each.to_string()]);
        }
    }
}

/// The thread caps an ffmpeg run is spawned with at `budget` (`sinks`: the indices of its outputs, when it has several): the
/// budget's ([`cpu::limit_args`]), then a wide graph's ([`cap_wide_graph`]). The one place both are written, which a test that
/// renders a graph through ffmpeg uses too, so that it is handed what an export is.
fn spawn_caps(args: &mut Vec<String>, budget: cpu::Budget, sinks: &[usize]) {
    if sinks.len() > 1 {
        cpu::limit_outputs_args(args, budget, sinks);
    } else {
        cpu::limit_args(args, budget);
    }
    cap_wide_graph(args, budget);
}

/// Move an oversized filtergraph out of argv into a script file, pointing ffmpeg
/// at it with [`graph_script_flag`]'s spelling. Leaves ordinary exports
/// untouched, so their argv stays byte-identical (and every pure arg-builder
/// test with it).
fn externalize_filter_complex(args: &mut [String], tag: &str) -> Result<GraphScript> {
    spill_graph(args, tag, graph_script_flag())
}

/// The pure half of [`externalize_filter_complex`], with the flag decided.
///
/// Either flag takes the path as its own argv token, which is why this beats
/// the obvious alternative of `sendcmd=f=`: that would bury the path *inside* a
/// filtergraph value, where `\` escapes and `:` separates options, so a Windows
/// path would have to be mangled first.
fn spill_graph(args: &mut [String], tag: &str, flag: &str) -> Result<GraphScript> {
    let Some(i) = oversized_graph_index(args) else {
        return Ok(GraphScript(None));
    };
    // Unique per call: two renders in one process (the GUI previewing while an
    // agent exports, two playback streams) share a pid and a tag, and the
    // second to finish used to delete the script the other was still reading.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("kerf-graph-{}-{tag}-{seq}.txt", std::process::id()));
    std::fs::write(&path, &args[i]).map_err(|e| Error::Engine(format!("could not write the filtergraph script: {e}")))?;
    args[i] = path.to_string_lossy().into_owned();
    args[i - 1] = flag.to_string();
    Ok(GraphScript(Some(path)))
}

/// Where one ffmpeg invocation's reported `out_time` maps onto the overall
/// export bar: `[offset, offset+width]` of `[0,1]`, against an output of `total`
/// seconds, timed from `start`. (A single-pass export is the whole bar; a
/// two-pass export splits it into two halves.)
#[derive(Clone, Copy)]
struct Bar {
    total: f64,
    offset: f64,
    width: f64,
    start: std::time::Instant,
}

/// How often a running export checks whether it has been cancelled when ffmpeg
/// has said nothing in the meantime.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// How long an export may go without moving — not one frame more, not one later output position
/// in its `-progress` stream — before it is treated as hung ([`run_progress_stalling`]). A run that
/// moves is never stopped, so the bound is generous: killing a render that was merely slow is the
/// worse error.
/// (It reports nothing while it opens the footage and decodes up to a range's start, so this is
/// also the most a seek into a recording with no keyframes to land on may take.)
const EXPORT_STALL: std::time::Duration = std::time::Duration::from_secs(300);

/// How often a running export that has written nothing yet repeats that it is starting.
const STARTING_BEAT: std::time::Duration = std::time::Duration::from_secs(1);

/// Spawn the `ffmpeg` binary with `args`, streaming `-progress` from stdout to
/// map elapsed render time onto `bar`, and polling `cancel` between updates
/// (killing ffmpeg when it trips). stderr is drained on a side thread so a
/// warning flood can't deadlock the stdout read, and its tail still surfaces in
/// a failure's error.
fn run_ffmpeg_progress(
    args: &[String],
    output: &Path,
    bar: Bar,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_ffmpeg_progress_as(args, output, bar, cpu::Resource::Cpu, progress, cancel)
}

/// [`run_ffmpeg_progress`] for a run that spends `resource`: the cores' slot for any software
/// run, the hardware's for one that is hardware from end to end ([`export_resource`]).
fn run_ffmpeg_progress_as(
    args: &[String],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_ffmpeg_progress_into(args, &[], output, bar, resource, progress, cancel)
}

/// [`run_ffmpeg_progress_as`] for a run that writes several files: `sinks` is the argv index of
/// each output path, so each encoder is capped to its share of the budget
/// ([`cpu::limit_outputs_args`]). Empty is one output, the last argument ([`cpu::limit_args`]).
fn run_ffmpeg_progress_into(
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_progress_with(&ffmpeg_bin(), args, sinks, output, bar, resource, progress, cancel)
}

/// [`run_ffmpeg_progress_into`] that also says how many frames the run had written when it ended
/// (ffmpeg's `frame=` in the `-progress` stream), for a run whose count is part of what it promises.
#[allow(clippy::too_many_arguments)]
fn run_ffmpeg_progress_counted(
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    frames: &mut u64,
) -> Result<RenderStatus> {
    run_progress_counting(&ffmpeg_bin(), args, sinks, output, bar, resource, progress, cancel, frames)
}

/// [`run_ffmpeg_progress_into`] with the binary named (a test runs a script that talks like ffmpeg).
#[allow(clippy::too_many_arguments)]
fn run_progress_with(
    bin: &str,
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<RenderStatus> {
    run_progress_counting(bin, args, sinks, output, bar, resource, progress, cancel, &mut 0)
}

/// [`run_progress_with`] that also counts the frames written (`frames`).
#[allow(clippy::too_many_arguments)]
fn run_progress_counting(
    bin: &str,
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    frames: &mut u64,
) -> Result<RenderStatus> {
    run_progress_stalling(
        bin,
        args,
        sinks,
        output,
        bar,
        resource,
        progress,
        cancel,
        frames,
        EXPORT_STALL,
    )
}

/// [`run_progress_counting`] that gives up on a run that has not moved for `stall`
/// ([`EXPORT_STALL`] in production): **moved** is a frame more or a later output position in the
/// `-progress` stream, not a line. FFmpeg 7 and later print a block every `-stats_period` whatever
/// the graph is doing, so a run whose graph had stopped (a 45 MP still decoded and resampled per
/// frame under memory pressure) went on "reporting" the same frame for hours; a run that is slow
/// but moving is never stopped, however slow.
#[allow(clippy::too_many_arguments)]
fn run_progress_stalling(
    bin: &str,
    args: &[String],
    sinks: &[usize],
    output: &Path,
    bar: Bar,
    resource: cpu::Resource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    frames: &mut u64,
    stall: std::time::Duration,
) -> Result<RenderStatus> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;

    // The heaviest thing the engine does. The lease is reentrant, so an export's
    // second pass and a stitch inside an import do not queue behind themselves. It is the
    // foreground lane — ahead of any proxy still queued — and a render that has to wait says
    // so through `progress` and can be cancelled while it does.
    let mut on_wait = |wait: cpu::Wait| {
        progress(ExportProgress::queued(
            bar.offset,
            bar.start.elapsed().as_secs_f64(),
            wait.message(),
        ));
    };
    let leased = match resource {
        cpu::Resource::Cpu => cpu::lease_waiting(&mut on_wait, cancel),
        cpu::Resource::Gpu => cpu::lease_gpu_waiting(&mut on_wait, cancel),
    };
    let cpu = match leased {
        Ok(lease) => lease,
        Err(Error::Cancelled) => return Ok(RenderStatus::Cancelled),
        Err(e) => return Err(e),
    };
    let mut args = args.to_vec();
    spawn_caps(&mut args, cpu.budget(), sinks);
    let args = &args[..];
    tracing::info!(output = %output.display(), resource = ?cpu.resource(), "exporting timeline");
    tracing::debug!(command = %format!("{bin} {}", args.join(" ")), "ffmpeg export command");

    // `-progress pipe:1` writes machine-readable key=value blocks to stdout;
    // `-stats_period` bounds how often, and thus the cancel-poll latency.
    let mut child = bg_command(bin)
        .arg("-progress")
        .arg("pipe:1")
        .arg("-stats_period")
        .arg("0.5")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(bin, e))?;

    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    let mut cancelled = false;
    let mut stalled = false;
    let stdout = child.stdout.take().expect("stdout piped");
    // Progress lines are read on a side thread so the loop below keeps polling
    // `cancel` (and watching for a silent ffmpeg) even when none arrive: a
    // blocking `lines()` read meant a render that stopped reporting could neither
    // be cancelled nor ever be given up on.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    // ffmpeg opens the footage before it writes anything — and decodes up to a range's start,
    // which on a long-GOP source takes as long as it takes, with the output file empty. Say so
    // now: it also replaces whatever the stage before this one (a queue, a metering pass) last said.
    let say = |progress: &mut dyn FnMut(ExportProgress), fraction: f64, eta_secs: Option<f64>, stage: ExportStage| {
        progress(ExportProgress {
            fraction,
            elapsed_secs: bar.start.elapsed().as_secs_f64(),
            eta_secs,
            waiting: None,
            stage: Some(stage),
            part: None,
        });
    };
    say(progress, bar.offset, None, ExportStage::Starting);
    let mut reader = ProgressReader::default();
    // The wall clock and the place on the bar at the first output: the rate (and so the time
    // left) is judged from there, not from the spawn.
    let mut first_output: Option<(Instant, f64)> = None;
    // When the run last moved, and how far it had got: frames written, output seconds.
    let mut last_moved = Instant::now();
    let mut furthest = (0u64, f64::NEG_INFINITY);
    let mut last_beat = Instant::now();
    loop {
        if last_moved.elapsed() > stall {
            let _ = child.kill();
            stalled = true;
            break;
        }
        let line = match rx.recv_timeout(CANCEL_POLL) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel() {
                    let _ = child.kill();
                    cancelled = true;
                    break;
                }
                // ffmpeg prints no `-progress` block while it opens the footage and decodes up
                // to a range's start (measured on 4.4: the first block comes only once the first
                // packet is written), so the silence of that stretch is the engine's to keep
                // alive: a beat a second, which moves the elapsed time.
                if first_output.is_none() && last_beat.elapsed() >= STARTING_BEAT {
                    last_beat = Instant::now();
                    say(progress, bar.offset, None, ExportStage::Starting);
                }
                continue;
            }
        };
        if let Some(n) = line.strip_prefix("frame=").and_then(|v| v.trim().parse::<u64>().ok()) {
            *frames = n;
            if n > furthest.0 {
                furthest.0 = n;
                last_moved = Instant::now();
            }
        }
        let block = reader.feed(&line);
        if let Some(Block::Working(Some(position)) | Block::End(Some(position))) = block {
            if position > furthest.1 {
                furthest.1 = position;
                last_moved = Instant::now();
            }
        }
        match block {
            // The output has been written to: its position against the length of the cut (the
            // output starts at 0 whatever the range's offset — the input seek rebases the
            // timestamps — so a range is measured against its own length). The last block is told
            // too: it carries the end of the output, which no periodic one was late enough to.
            Some(Block::Working(Some(position)) | Block::End(Some(position))) => {
                let (pass, stage) = pass_position(position, bar.total);
                let fraction = (bar.offset + bar.width * pass).clamp(0.0, 1.0);
                let (at, from) = *first_output.get_or_insert((Instant::now(), fraction));
                let eta = eta_secs(at.elapsed().as_secs_f64(), fraction - from, 1.0 - fraction);
                say(progress, fraction, eta, stage);
            }
            // Nothing written yet: ffmpeg is still opening and seeking. A beat, so the elapsed
            // time moves and the stage stays honest.
            Some(Block::Working(None)) if first_output.is_none() => say(progress, bar.offset, None, ExportStage::Starting),
            _ => {}
        }
        // A cancel that arrives with the last block still counts (the file is removed, as for any
        // other): the check comes before the end of the stream ends the loop.
        if cancel() {
            let _ = child.kill();
            cancelled = true;
            break;
        }
        if matches!(block, Some(Block::End(_))) {
            break;
        }
    }

    let status = reap(&mut child)?;
    let stderr_text = stderr_handle.join().unwrap_or_default();

    if cancelled {
        tracing::info!(output = %output.display(), "export cancelled");
        return Ok(RenderStatus::Cancelled);
    }
    if stalled {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(20).collect();
        tail.reverse();
        let at = if furthest.1.is_finite() {
            format!(" (stuck at frame {}, {:.2} s of output)", furthest.0, furthest.1)
        } else {
            " (nothing written yet)".to_string()
        };
        return Err(Error::Engine(format!(
            "ffmpeg stopped reporting progress for {}s{at} and was stopped: {}",
            stall.as_secs(),
            tail.join("\n").trim()
        )));
    }
    if !status.success() {
        let mut tail: Vec<&str> = stderr_text.lines().rev().take(20).collect();
        tail.reverse();
        let tail = tail.join("\n");
        tracing::error!(status = %status, "ffmpeg export failed:\n{tail}");
        return Err(Error::Engine(format!("ffmpeg exited with {}: {}", status, tail.trim())));
    }
    tracing::info!(output = %output.display(), "export complete");
    Ok(RenderStatus::Completed)
}

/// Whether `clip` puts sound in the mix: its asset carries an audio stream **and**
/// the clip still plays it. A picture clip whose sound was detached
/// (`Clip::source_audio` false, see `Project::detach_audio`) is silent — its sound
/// is the linked audio clip's — and the export otherwise mixes the audio of *every*
/// clip with an audio stream, video tracks included, which is what made an asset
/// extracted onto an audio track while still on V1 sound twice. `source_audio`
/// defaults to true, so a clip that never heard of the flag is gated exactly as it
/// was.
fn clip_sounds(clip: &Clip, assets: &[Asset]) -> bool {
    clip.source_audio
        && assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
}

/// The result of [`build_filter_complex`]: the `-filter_complex` string plus
/// which output pads it produced, so the caller knows which `-map`s to add.
struct FilterGraph {
    filter: String,
    has_video: bool,
    has_audio: bool,
    /// The `ebur128` taps a [metered](build_filter_complex_metered) build added,
    /// each ending on a pad the caller `-map`s. Empty for every other build.
    meters: Vec<Meter>,
}

/// One level meter in a metered audio graph: an `ebur128` instance on a track's
/// finished strip or on the master, ending on a pad to `-map` (the filter passes
/// its audio through, so an output has to be consumed somewhere).
#[derive(Debug, Clone, PartialEq)]
struct Meter {
    /// The pad the meter ends on (`lv3`, `lvmaster`), without brackets.
    pad: String,
    /// The `ebur128@<name>` instance name its log lines carry (`t3`, `master`).
    name: String,
    /// The index of the timeline track it listens to, `None` for the master.
    track: Option<usize>,
}

/// One part of an export in parts, as its picture graph draws it ([`segments`]): the clips of the
/// plan (`InputPlan`: the ones that are on screen for these output frames) over a canvas that is
/// those frames of the whole export's. Times in the graph stay the whole export's (a part's frames carry the
/// timestamps they have in it, so every expression of a clip, a title or a caption is the one the
/// single run evaluates); the part is shifted to start at zero only as it leaves the graph.
struct Scope<'a> {
    /// The cut's transition table, whole: a clip's look is the export's, whichever part draws it.
    fx: &'a [ClipFx],
    /// The output frames the part covers: `frames` of them from `first_frame`.
    first_frame: u64,
    frames: u64,
}

#[cfg(test)]
thread_local! {
    /// The shortest run [`fold_min`] answers on this thread while a test pins it.
    static FOLD_MIN_OVERRIDE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with runs of at least `min` blended clips folded (`usize::MAX`: none), to hold the folded graph against the
/// nested one it replaces.
#[cfg(test)]
fn with_fold_min<R>(min: usize, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<usize>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FOLD_MIN_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(FOLD_MIN_OVERRIDE.with(|c| c.replace(Some(min))));
    f()
}

/// The fewest consecutive blended clips that are folded: [`FOLD_MIN`].
fn fold_min() -> usize {
    #[cfg(test)]
    if let Some(forced) = FOLD_MIN_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    FOLD_MIN
}

/// The runs of blended clips a graph lays onto the composite through one fork and join ([`fold_runs`]), as the flat
/// indices of each, in the order the graph composites them. The runs are the whole cut's; `include` names the clips the
/// graph has (an export in parts draws some), and a run keeps those of its clips, two at least (a part that holds a
/// run's tail folds it as the whole export does, instead of nesting the few, and one clip alone is the nested chain).
/// A clip folds when it is blended **footage** (a still's or a title's frames are made up, their ends set by other means
/// than the cut `drawn_slots` bounds) and the rate is one the canvas and the clips' `fps` agree on.
fn blend_folds(
    timeline: &Timeline,
    assets: &[Asset],
    fx: &[ClipFx],
    fps: f64,
    include: &dyn Fn(usize) -> bool,
) -> Vec<Vec<usize>> {
    let grid = match (Rational::from_fps(fps), Rational::from_fps_filter(fps)) {
        (Some(canvas), Some(clips)) if canvas == clips => Some(clips),
        _ => None,
    };
    let mut flats: Vec<usize> = Vec::new();
    let mut clips: Vec<FoldClip> = Vec::new();
    let mut base = 0;
    for (track_index, track) in timeline.tracks.iter().enumerate() {
        if track.kind == StreamKind::Video {
            let mut order: Vec<usize> = (0..track.clips.len()).collect();
            order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
            for cj in order {
                let (flat, clip) = (base + cj, &track.clips[cj]);
                let footage =
                    !clip.is_generated() && !assets.iter().find(|a| a.id == clip.asset_id).is_some_and(|a| a.is_image());
                let slots = grid
                    .filter(|_| footage && clip.blend_mode.ffmpeg().is_some())
                    .map(|rate| ClipTiming::new(clip, &fx[flat]).drawn_slots(rate));
                flats.push(flat);
                clips.push(FoldClip {
                    track: track_index,
                    slots,
                });
            }
        }
        base += track.clips.len();
    }
    fold_runs(&clips, fold_min())
        .into_iter()
        .map(|run| flats[run].iter().copied().filter(|&flat| include(flat)).collect::<Vec<_>>())
        .filter(|run| run.len() >= 2)
        .collect()
}

/// A tick a probe found for an asset's video stream: what [`Project::apply_time_bases`](crate::project::Project::apply_time_bases)
/// writes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTick {
    pub asset: uuid::Uuid,
    /// The file that was probed: the write-back is for this path and for no other (an asset relinked in between is not given
    /// its predecessor's tick).
    pub path: String,
    pub time_base: (u32, u32),
}

/// The first video stream of `asset` when it has no tick recorded: an asset probed before schema v21, which re-importing
/// does not probe again (`insert_or_get_asset` hands back the row it has).
fn lacking_time_base(asset: &Asset) -> bool {
    asset
        .streams
        .iter()
        .find(|s| s.kind == StreamKind::Video)
        .is_some_and(|s| !s.image && s.time_base.is_none())
}

/// The assets (`(id, path)`) an export of `timeline` has to learn the tick of before it is built: those with no tick recorded
/// ([`lacking_time_base`]) whose blended footage would fold if the tick allowed ([`blend_folds`] with every tick taken to be
/// fine), and so only when a fold is possible, which costs nothing otherwise. Pure.
///
/// The backfill is `time_base_inputs` → [`probe_time_bases`] (a bounded `ffprobe` each, **lock released**) →
/// [`with_time_bases`] for the build → [`Project::apply_time_bases`](crate::project::Project::apply_time_bases) (a write to
/// the asset's streams, not a timeline edit). The renders do the middle two themselves; whoever holds the project writes back.
pub fn time_base_inputs(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Vec<(uuid::Uuid, String)> {
    if !assets.iter().any(lacking_time_base) {
        return Vec::new();
    }
    let cut = prepared_cut(timeline, opts);
    let fmt = export_format(&cut, assets, opts);
    let mut fx = transition_fx(&cut, assets);
    for f in &mut fx {
        f.time_base.get_or_insert((1, 90_000));
    }
    let foldable: std::collections::HashSet<usize> = blend_folds(&cut, assets, &fx, fmt.fps, &|_| true)
        .into_iter()
        .flatten()
        .collect();
    let mut wanted: Vec<(uuid::Uuid, String)> = Vec::new();
    for (flat, clip) in cut.tracks.iter().flat_map(|t| t.clips.iter()).enumerate() {
        if !foldable.contains(&flat) || wanted.iter().any(|(id, _)| *id == clip.asset_id) {
            continue;
        }
        if let Some(asset) = assets.iter().find(|a| a.id == clip.asset_id).filter(|a| lacking_time_base(a)) {
            wanted.push((asset.id, asset.path.clone()));
        }
    }
    wanted
}

/// The ticks of `wanted` ([`time_base_inputs`]): one `ffprobe` each, killed at [`SOURCE_PROBE_TIMEOUT`] and remembered for the
/// process ([`source_traits`]), so a second export of the cut does not ask again. A probe that fails, outlasts it or prints no
/// tick leaves the asset out: its tick stays unknown, which folds nothing. Spawns processes: **not under the project lock**.
pub fn probe_time_bases(wanted: &[(uuid::Uuid, String)]) -> Vec<SourceTick> {
    wanted
        .iter()
        .filter_map(|(asset, path)| {
            let time_base = source_traits(Path::new(path))?.time_base?;
            Some(SourceTick {
                asset: *asset,
                path: path.clone(),
                time_base,
            })
        })
        .collect()
}

/// `assets` with the ticks `found` gives the assets that lack one (and are still at the path probed): what the graph is built
/// from. The list itself when there is nothing to give.
pub fn with_time_bases<'a>(assets: &'a [Asset], found: &[SourceTick]) -> std::borrow::Cow<'a, [Asset]> {
    if found.is_empty() {
        return std::borrow::Cow::Borrowed(assets);
    }
    let mut filled = assets.to_vec();
    for tick in found {
        let stream = filled
            .iter_mut()
            .find(|a| a.id == tick.asset && a.path == tick.path)
            .and_then(|a| a.streams.iter_mut().find(|s| s.kind == StreamKind::Video))
            .filter(|s| !s.image && s.time_base.is_none());
        if let Some(stream) = stream {
            stream.time_base = Some(tick.time_base);
        }
    }
    std::borrow::Cow::Owned(filled)
}

/// [`time_base_inputs`], [`probe_time_bases`] and [`with_time_bases`] in a row: the assets an export is built from.
fn backfilled<'a>(timeline: &Timeline, assets: &'a [Asset], opts: &ExportOptions) -> std::borrow::Cow<'a, [Asset]> {
    let wanted = time_base_inputs(timeline, assets, opts);
    if wanted.is_empty() {
        return std::borrow::Cow::Borrowed(assets);
    }
    with_time_bases(assets, &probe_time_bases(&wanted))
}

/// Build the positional, multi-track `filter_complex`.
///
/// Unlike a flat `concat`, this honors each clip's `timeline_start` and layers
/// the tracks:
///
/// * **Picture** — an opaque black canvas of the whole `total` duration, then
///   every video clip `overlay`'d onto it at its timeline position
///   (`setpts=…+start/TB`, gated by `enable='between(t,start,end)'`). Tracks are
///   composited in list order, so clips that appear later in the timeline's
///   track list (e.g. a B-roll lane added above the interview) render on top,
///   and gaps fall through to black.
/// * **Sound** — every clip that has a real audio stream is trimmed, gained,
///   faded, delayed to its timeline position (`adelay`), and summed with `amix`,
///   so audio from any track (video or audio) is mixed together.
///
/// Each clip indexes the ffmpeg input list by its track-then-clip order, which
/// matches how [`build_export_args`] adds the `-i` inputs. Kept pure (no I/O)
/// so it is unit-testable without the binary present.
///
/// `want_video` / `want_audio` gate stream emission so the graph never produces
/// a pad the caller won't `-map` (e.g. an mp3 export of a video timeline emits
/// no `[outv]`). For a gif container the picture pad is routed through a
/// `palettegen` / `paletteuse` pair and audio is always dropped.
#[allow(clippy::too_many_arguments)]
fn build_filter_complex(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    total: f64,
    opts: &ExportOptions,
    want_video: bool,
    want_audio: bool,
    plan: &InputPlan,
) -> FilterGraph {
    build_filter_complex_metered(timeline, assets, fmt, total, opts, want_video, want_audio, plan, false)
}

/// [`build_filter_complex`], optionally with **level meters** in the sound.
///
/// With `meter` off this *is* the export graph, byte for byte. With it on, the
/// sound is built the same way — same per-clip chains, same ducking, same master
/// bus and `loudnorm` — except that each track's clips are first summed into
/// that track's own submix, which is tapped (`asplit` → `ebur128`) before it
/// joins the bus, and the finished mix is tapped last. Summing a submix and then
/// the submixes is the same arithmetic as summing every clip at once
/// (`amix=normalize=0`), so what the master tap hears is what the export would
/// write, and the per-track taps cost one pass over the media rather than one
/// per track. A track's tap is its strip *output* — fader and pan applied, ahead
/// of the duck bus and the master — which is why a ducked track reads as loud as
/// it would play on its own.
#[allow(clippy::too_many_arguments)]
fn build_filter_complex_metered(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    total: f64,
    opts: &ExportOptions,
    want_video: bool,
    want_audio: bool,
    plan: &InputPlan,
    meter: bool,
) -> FilterGraph {
    build_filter_complex_with(
        timeline, assets, fmt, total, opts, want_video, want_audio, plan, meter, None, "", None,
    )
}

/// [`build_filter_complex_metered`] with the animated titles' sequences given: the playback stream
/// reads the ones named for a title, not for the playhead ([`svg_layer::stream_sequences`]), and the
/// graph must index the very inputs its argv carries. `None` is the cut's own
/// ([`svg_layer::svg_sequences`]). `instances` prefixes the names of the filter instances a
/// command targets (a reframe's `v360@c3`), so graphs that share one run keep theirs apart;
/// empty for every graph that runs alone.
#[allow(clippy::too_many_arguments)]
fn build_filter_complex_with(
    timeline: &Timeline,
    assets: &[Asset],
    fmt: &ExportFormat,
    total: f64,
    opts: &ExportOptions,
    want_video: bool,
    want_audio: bool,
    plan: &InputPlan,
    meter: bool,
    titles: Option<&[svg_layer::SvgSeq<'_>]>,
    instances: &str,
    scope: Option<&Scope<'_>>,
) -> FilterGraph {
    let has_audio = |clip: &crate::model::Clip| clip_sounds(clip, assets);
    let is_image = |clip: &crate::model::Clip| assets.iter().find(|a| a.id == clip.asset_id).is_some_and(|a| a.is_image());
    let picture_of = |clip: &crate::model::Clip| picture_size(assets.iter().find(|a| a.id == clip.asset_id));
    let layout = fmt.channel_layout();

    // Assign each clip its ffmpeg input index (track-then-storage order, matching
    // the `-i` order) and split into composited video clips and mixed audio clips.
    // Within a track the clips are visited in *timeline* order so video overlays
    // composite in timeline order (a later clip on top of an earlier one's tail,
    // e.g. during a crossfade); tracks keep their list order so a later track
    // still composites on top.
    // Each entry is `(flat, input, clip)`: `flat` is the storage-order clip index
    // (keys `fx` and the per-clip pad labels — unique per clip); `input` is the
    // deduplicated ffmpeg input index (may be shared, so the `[input:v]` source is
    // fanned out with `split` below).
    let mut video: Vec<(usize, usize, &crate::model::Clip)> = Vec::new();
    // Audio entries also carry the owning track's mix — the duck flag for the bus
    // split, the fader and the pan for the clip's own chain.
    let mut audio: Vec<(usize, usize, &crate::model::Clip, TrackMix)> = Vec::new();
    // ...and, parallel to it, the index of the track each audio clip is on (what a
    // metered build groups the submixes by).
    let mut audio_track: Vec<usize> = Vec::new();
    let mut base = 0;
    for (track_index, track) in timeline.tracks.iter().enumerate() {
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for &cj in &order {
            let clip = &track.clips[cj];
            let flat = base + cj;
            let input = plan.clip_input[flat];
            if input == NO_INPUT {
                continue;
            }
            if track.kind == StreamKind::Video {
                video.push((flat, input, clip));
            }
            if has_audio(clip) {
                audio.push((
                    flat,
                    input,
                    clip,
                    TrackMix {
                        duck: track.duck,
                        volume: track.volume,
                        pan: track.pan_gains(),
                    },
                ));
                audio_track.push(track_index);
            }
        }
        base += track.clips.len();
    }

    // Per-clip transition adjustments (crossfade tail / alpha, dip-to-black
    // fades), computed per track from each clip's `transition_in`.
    let fx_own;
    let fx: &[ClipFx] = match scope {
        Some(scope) => scope.fx,
        None => {
            fx_own = transition_fx(timeline, assets);
            &fx_own
        }
    };

    let gif = opts.container == Container::Gif;
    // A part with no clip on screen is still a picture: the canvas, and what is drawn on it.
    let has_video = want_video && (!video.is_empty() || scope.is_some());
    // The runs of blended clips laid onto the composite through one fork and join instead of one each.
    let folds = if has_video {
        blend_folds(timeline, assets, fx, fmt.fps, &|flat| plan.clip_input[flat] != NO_INPUT)
    } else {
        Vec::new()
    };
    let fold_of: HashMap<usize, usize> = folds
        .iter()
        .enumerate()
        .flat_map(|(run, flats)| flats.iter().map(move |&flat| (flat, run)))
        .collect();
    let has_audio_out = want_audio && !audio.is_empty();

    // How many clips consume each input as video / as audio. An input used by
    // more than one must be fanned out with `split` / `asplit` — ffmpeg forbids
    // reusing an input pad across filters. When nothing is shared (the common
    // case) every count is ≤ 1 and no split is emitted, so the graph is identical.
    let n_inputs = plan.representatives.len();
    let mut vcount = vec![0usize; n_inputs];
    let mut acount = vec![0usize; n_inputs];
    for (_, input, _) in &video {
        vcount[*input] += 1;
    }
    for (_, input, _, _) in &audio {
        acount[*input] += 1;
    }

    let mut chains: Vec<String> = Vec::new();
    if has_video {
        for (i, &cnt) in vcount.iter().enumerate() {
            if cnt > 1 {
                let outs: String = (0..cnt).map(|k| format!("[vsp{i}_{k}]")).collect();
                chains.push(format!("[{}:v]split={cnt}{outs}", plan.base + i));
            }
        }
    }
    if has_audio_out {
        for (i, &cnt) in acount.iter().enumerate() {
            if cnt > 1 {
                let outs: String = (0..cnt).map(|k| format!("[asp{i}_{k}]")).collect();
                chains.push(format!("[{}:a]asplit={cnt}{outs}", plan.base + i));
            }
        }
    }
    // A clip's source pad: its own input, or the next `split` output when that
    // input is shared. `vnext`/`anext` hand out the split outputs in clip order.
    let mut vnext = vec![0usize; n_inputs];
    let mut anext = vec![0usize; n_inputs];

    // ---- picture: black base + positioned overlays --------------------------
    if has_video {
        let draw = !timeline.overlays.is_empty();
        // Animated titles: PNG sequences read as inputs after the clips'. They go over the whole
        // composite and under the text.
        let own;
        let svg: &[svg_layer::SvgSeq<'_>] = match titles {
            Some(given) => given,
            None => {
                own = svg_layer::svg_sequences(timeline, fmt);
                &own
            }
        };
        // The picture ends on `[outv]`, or on `vhw` when a VAAPI encoder's upload follows. A part
        // of an export in parts ends on `vpart`, which is shifted to start at zero on its way out.
        let vaapi = uploads_to_vaapi(opts);
        let final_pad = match (vaapi, scope) {
            (_, Some(_)) => "vpart",
            (true, None) => "vhw",
            (false, None) => "outv",
        };
        // What comes after the titles: `vcomp` for gif (palettegen follows), `vtext`
        // when text overlays will be drawn on top, else directly on `final_pad`.
        let after_svg = if draw {
            "vtext"
        } else if gif {
            "vcomp"
        } else {
            final_pad
        };
        // The clips land on `vsvg` when titles follow, else where the titles would have gone.
        let composite_pad = if svg.is_empty() { after_svg } else { "vsvg" };
        // The canvas. A part has its own frames of it: `frames` from `first_frame`, numbered as the
        // whole export's (`setpts=N+k`, counted in frames like a title's), and — when no clip is
        // overlaid, which is where the graph would otherwise leave a picture in the delivery's
        // format — in the 8-bit 4:2:0 every `overlay` composes in, so that what is drawn on it
        // (titles, captions) is drawn as in the whole export.
        let canvas_pad = if video.is_empty() { composite_pad } else { "vbase" };
        match scope {
            None => chains.push(format!(
                "color=c=black:s={w}x{h}:r={fps}:d={total},format={pf}[{canvas_pad}]",
                w = fmt.width,
                h = fmt.height,
                fps = fmt.fps,
                total = total.max(0.0),
                pf = fmt.pix_fmt,
            )),
            Some(part) => {
                // Half a frame short of the last one's slot, so that no rounding of the duration
                // can add a frame or take one away.
                let d = (part.frames as f64 - 0.5) / fmt.fps;
                let composes = if video.is_empty() && svg.is_empty() && fmt.pix_fmt != "yuv420p" {
                    ",format=yuv420p"
                } else {
                    ""
                };
                chains.push(format!(
                    "color=c=black:s={w}x{h}:r={fps}:d={d},format={pf},setpts=N+{k}{composes}[{canvas_pad}]",
                    w = fmt.width,
                    h = fmt.height,
                    fps = fmt.fps,
                    pf = fmt.pix_fmt,
                    k = part.first_frame,
                ));
            }
        }
        let mut cur = "vbase".to_string();
        let mut fold_base = String::new();
        let mut fold_stages: Vec<FoldStage> = Vec::new();
        let last = video.len().wrapping_sub(1);
        // The matrix a blended clip's composite is in: the bottom layer's, the first picture of the lowest
        // video track that has one. Decided over the whole timeline, so a part names what the whole export does.
        let mut first = 0;
        let composite = timeline
            .tracks
            .iter()
            .map(|track| {
                let start = first;
                first += track.clips.len();
                (start, track)
            })
            .filter(|(_, track)| track.kind == StreamKind::Video)
            .find_map(|(start, track)| {
                track
                    .clips
                    .iter()
                    .enumerate()
                    .filter(|(_, clip)| picture_of(clip).is_some())
                    .min_by(|a, b| a.1.timeline_start.total_cmp(&b.1.timeline_start))
                    .map(|(j, _)| start + j)
            })
            .and_then(|flat| composite_matrix(fx[flat].label_bt709, fx[flat].frame_matrix));
        for (n, (flat, input, clip)) in video.iter().enumerate() {
            let src = if vcount[*input] > 1 {
                let k = vnext[*input];
                vnext[*input] += 1;
                format!("vsp{input}_{k}")
            } else {
                format!("{}:v", plan.base + input)
            };
            // A clip the blurred fill does not suit is placed with Contain's chain.
            let contained;
            let clip_fmt =
                if fmt.fit == Fit::Blur && !takes_blur_fill(clip, picture_of(clip), (fmt.width, fmt.height), fx[*flat].alpha) {
                    contained = ExportFormat {
                        fit: Fit::Contain,
                        ..fmt.clone()
                    };
                    &contained
                } else {
                    fmt
                };
            let stamp = plan.stamps.get(*flat).copied().flatten();
            let stab = (stamp.is_none() && !is_image(clip))
                .then(|| {
                    stabilizer(
                        clip,
                        assets.iter().find(|a| a.id == clip.asset_id),
                        picture_window(clip, &fx[*flat], clip_fmt.fps),
                    )
                })
                .flatten();
            // A moving still is drawn into the whole frame, its position inside it.
            let still_motion = stamp
                .is_none()
                .then(|| still_motion_for(clip, assets, fmt, &fx[*flat]))
                .flatten();
            chains.push(format!(
                "[{src}]{chain}[v{flat}]",
                chain = video_clip_chain_stamped(
                    clip,
                    clip_fmt,
                    &fx[*flat],
                    is_image(clip),
                    stamp,
                    picture_of(clip),
                    stab.as_ref(),
                    still_motion.as_ref(),
                    &format!("{instances}c{flat}")
                )
            ));
            let out = if n == last {
                composite_pad.to_string()
            } else {
                format!("vov{n}")
            };
            let timing = ClipTiming::new(clip, &fx[*flat]);
            let (start, end) = Rational::from_fps(fmt.fps).map_or_else(|| timing.window(), |rate| timing.enable_window(rate));
            // A slide / push adds its travel to whatever position the clip already
            // has, so a transition composes with a static offset or an animated one
            // instead of overriding it.
            let motion = motion_expr(clip, &fx[*flat]);
            // A position that varies over time is a piecewise expression, and a
            // piecewise expression contains commas — which the graph parser reads
            // as the end of the filter unless the value is quoted. A plain static
            // offset has none, and stays unquoted.
            let quote = |v: String, dynamic: bool| if dynamic { format!("'{v}'") } else { v };
            let overlay = if still_motion.is_some() {
                // The frame itself: only a slide or push moves it.
                match &motion {
                    Some((mx, my)) => format!(
                        "overlay=x='(W-w)/2+({mx})*W':y='(H-h)/2+({my})*H':eof_action=pass:enable='between(t,{start},{end})'"
                    ),
                    None => format!("overlay=eof_action=pass:enable='between(t,{start},{end})'"),
                }
            } else if clip.placement_animated() {
                // The chain's last stage placed the picture, travel included, on a frame of the
                // canvas' size (`placement_filters`).
                format!("overlay=eof_action=pass:enable='between(t,{start},{end})'")
            } else if clip.is_animated() {
                // Animated picture position: per-frame overlay x / y expressions.
                // The eased polyline `transform_at` reads too (one curve for every renderer);
                // an axis that is not keyed is its static offset.
                let px = driven_expr(clip, Property::PosX, "t");
                let py = driven_expr(clip, Property::PosY, "t");
                let (px, py) = match &motion {
                    Some((mx, my)) => (format!("({px})+({mx})"), format!("({py})+({my})")),
                    None => (px, py),
                };
                format!(
                    "overlay=x={px}:y={py}:eof_action=pass:enable='between(t,{start},{end})'",
                    px = quote(format!("(W-w)/2+({px})*W"), true),
                    py = quote(format!("(H-h)/2+({py})*H"), true),
                )
            } else if clip.transform.is_identity() && motion.is_none() {
                format!("overlay=eof_action=pass:enable='between(t,{start},{end})'")
            } else {
                let t = &clip.transform;
                let (px, py) = match &motion {
                    Some((mx, my)) => (format!("({})+({mx})", t.pos_x), format!("({})+({my})", t.pos_y)),
                    None => (t.pos_x.to_string(), t.pos_y.to_string()),
                };
                format!(
                    "overlay=x={px}:y={py}:eof_action=pass:enable='between(t,{start},{end})'",
                    px = quote(format!("(W-w)/2+({px})*W"), motion.is_some()),
                    py = quote(format!("(H-h)/2+({py})*H"), motion.is_some()),
                )
            };
            match clip.blend_mode.ffmpeg() {
                None => chains.push(format!("[{cur}][v{flat}]{overlay}[{out}]")),
                Some(mode) => {
                    let window = format!("between(t,{start},{end})");
                    match fold_of.get(flat) {
                        Some(&run) => {
                            // A run's clips are gathered and laid on together by its last (`blend_fold`), from the
                            // composite the first one finds.
                            if folds[run][0] == *flat {
                                fold_base = cur.clone();
                                fold_stages.clear();
                            }
                            fold_stages.push(FoldStage {
                                layer: format!("v{flat}"),
                                tag: flat.to_string(),
                                mode,
                                placement: overlay,
                                window,
                            });
                            if folds[run].last() == Some(flat) {
                                chains.extend(blend_fold(&fold_base, &fold_stages, composite, &fold_stages[0].tag, &out));
                            }
                        }
                        None => {
                            let blend = Blend {
                                mode,
                                placement: &overlay,
                                window: Some(&window),
                                matrix: composite,
                            };
                            chains.extend(blend_chains(&cur, &format!("v{flat}"), &blend, &flat.to_string(), &out));
                        }
                    }
                }
            }
            cur = out;
        }
        if !svg.is_empty() {
            svg_layer::push_chains(svg, plan.base + n_inputs, composite_pad, after_svg, &mut chains);
        }
        // Text overlays (titles / lower-thirds / captions) drawn on the composited
        // picture in order; the last produces the gif source `vcomp` or `[outv]`.
        if draw {
            let mut tcur = after_svg.to_string();
            let last_o = timeline.overlays.len() - 1;
            let text_final = if gif { "vcomp" } else { final_pad };
            for (oi, ov) in timeline.overlays.iter().enumerate() {
                let out = if oi == last_o {
                    text_final.to_string()
                } else {
                    format!("vtxt{oi}")
                };
                chains.push(format!("[{tcur}]{f}[{out}]", f = drawtext_export(ov, fmt)));
                tcur = out;
            }
        }
        if gif {
            // A two-stream palette gives far better color than the default 216-color
            // web palette: generate an optimized palette, then map onto it.
            // An out-of-list dither (see [`GIF_DITHERS`]) falls back to the
            // default rather than reaching `paletteuse=` unchecked.
            let dither = opts
                .gif_dither
                .as_deref()
                .filter(|d| GIF_DITHERS.contains(d))
                .unwrap_or("bayer");
            chains.push("[vcomp]split[gpsrc][gpuse]".to_string());
            chains.push("[gpsrc]palettegen=stats_mode=diff[gpal]".to_string());
            chains.push(format!("[gpuse][gpal]paletteuse=dither={dither}[outv]"));
        }
        if vaapi && scope.is_none() {
            chains.push("[vhw]format=nv12,hwupload[outv]".to_string());
        }
        if scope.is_some() {
            chains.push("[vpart]setpts=PTS-STARTPTS[outv]".to_string());
        }
    }

    // ---- sound: positioned per-clip audio summed with amix ------------------
    let mut meters: Vec<Meter> = Vec::new();
    if has_audio_out {
        for (flat, input, clip, mix) in &audio {
            let src = if acount[*input] > 1 {
                let k = anext[*input];
                anext[*input] += 1;
                format!("asp{input}_{k}")
            } else {
                format!("{}:a", plan.base + input)
            };
            chains.extend(audio_clip_chains(&src, *flat, clip, fmt, &fx[*flat], layout, *mix));
        }
        // What the final sum adds up, as `(pad, ducked)`: every clip's own pad —
        // or, when metering, each track's submix (tapped on its way in).
        let mut sources: Vec<(String, bool)> = audio.iter().map(|(f, _, _, m)| (format!("a{f}"), m.duck)).collect();
        if meter {
            sources.clear();
            let mut tracks: Vec<usize> = audio_track.clone();
            tracks.dedup();
            for ti in tracks {
                let flats: Vec<usize> = audio
                    .iter()
                    .zip(&audio_track)
                    .filter(|(_, t)| **t == ti)
                    .map(|((f, _, _, _), _)| *f)
                    .collect();
                let duck = timeline.tracks[ti].duck;
                let strip = if let [only] = flats[..] {
                    format!("a{only}")
                } else {
                    chains.push(format!(
                        "{ins}amix=inputs={n}:normalize=0:dropout_transition=0[tk{ti}]",
                        ins = flats.iter().map(|f| format!("[a{f}]")).collect::<String>(),
                        n = flats.len(),
                    ));
                    format!("tk{ti}")
                };
                chains.push(format!("[{strip}]asplit=2[tm{ti}][tl{ti}]"));
                // Sample and true peak both: which track is hot is the question a
                // reading answers, and the oversampling that costs (about 5 ms of
                // work per second of audio per tap) is small beside the decode.
                chains.push(format!("[tl{ti}]ebur128@t{ti}=peak=sample+true[lv{ti}]"));
                meters.push(Meter {
                    pad: format!("lv{ti}"),
                    name: format!("t{ti}"),
                    track: Some(ti),
                });
                sources.push((format!("tm{ti}"), duck));
            }
        }
        // The master bus (its fader, then the limiter) and the optional
        // single-pass loudness normalization close the final mix, in that
        // order; loudnorm upsamples to 192 kHz internally, so resample back to
        // the output rate. A neutral master adds nothing, so a graph from before
        // the master existed is unchanged.
        let loudness = opts.loudness.or(opts.loudnorm.then_some(LoudnessPreset::Youtube));
        let master = master_filters(&timeline.master, fmt.sample_rate);
        let mix_tail = match loudness {
            Some(target) if opts.loudness_static => format!(
                "{master}{norm}",
                norm = static_loudness_filters(target, opts.loudness_gain_db, fmt.sample_rate)
            ),
            Some(target) => format!("{master},{norm},aresample={sr}", norm = target.filter(), sr = fmt.sample_rate),
            None => master,
        };
        let pads = |names: &[&str]| names.iter().map(|n| format!("[{n}]")).collect::<String>();
        let ducked: Vec<&str> = sources.iter().filter(|(_, d)| *d).map(|(n, _)| n.as_str()).collect();
        let keyed: Vec<&str> = sources.iter().filter(|(_, d)| !*d).map(|(n, _)| n.as_str()).collect();
        if ducked.is_empty() || keyed.is_empty() {
            // No ducking in play (nothing flagged, or nothing to key from): one
            // flat sum of every clip, exactly as before.
            let all: Vec<&str> = sources.iter().map(|(n, _)| n.as_str()).collect();
            chains.push(format!(
                "{ins}amix=inputs={n}:normalize=0:dropout_transition=0{mix_tail}[outa]",
                ins = pads(&all),
                n = all.len(),
            ));
        } else {
            // Mix each group into a bus, dip the ducked bus under the keyed one
            // (sidechain compression — music falls when dialogue speaks), then
            // sum the two buses.
            chains.push(format!(
                "{ins}amix=inputs={n}:normalize=0:dropout_transition=0[akey]",
                ins = pads(&keyed),
                n = keyed.len(),
            ));
            chains.push(format!(
                "{ins}amix=inputs={n}:normalize=0:dropout_transition=0[aduck]",
                ins = pads(&ducked),
                n = ducked.len(),
            ));
            chains.push("[akey]asplit=2[akmix][akside]".to_string());
            match timeline.master.safe_duck_depth_db() {
                None => chains
                    .push("[aduck][akside]sidechaincompress=threshold=0.05:ratio=8:attack=20:release=400[aducked]".to_string()),
                Some(depth) => chains.extend(duck_gate_chains(depth, layout)),
            }
            chains.push(format!(
                "[akmix][aducked]amix=inputs=2:normalize=0:dropout_transition=0{mix_tail}[outa]"
            ));
        }
        if meter {
            // The finished mix, as the export would write it (the true peak of
            // this one is the number a delivery is judged on).
            chains.push("[outa]ebur128@master=peak=sample+true[lvmaster]".to_string());
            meters.push(Meter {
                pad: "lvmaster".to_string(),
                name: "master".to_string(),
                track: None,
            });
        }
    }

    FilterGraph {
        filter: chains.join(";"),
        has_video,
        has_audio: has_audio_out,
        meters,
    }
}

#[cfg(test)]
thread_local! {
    /// What [`alimiter_latency_available`] answers on this thread while a test pins it.
    static ALIMITER_LATENCY_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg's `alimiter` did (`true`) or did not (`false`) have `latency`.
#[cfg(test)]
fn with_alimiter_latency<R>(available: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            ALIMITER_LATENCY_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(ALIMITER_LATENCY_OVERRIDE.with(|c| c.replace(Some(available))));
    f()
}

/// Whether `ffmpeg -h filter=alimiter` lists a `latency` option (pure, unit-tested).
fn help_lists_latency(help: &str) -> bool {
    help.lines().any(|l| l.split_whitespace().next() == Some("latency"))
}

/// Whether this ffmpeg's `alimiter` takes `latency`, probed once per process. The
/// option arrived after FFmpeg 4.4 (Ubuntu 22.04's ffmpeg), which refuses the whole
/// graph with `Option 'latency' not found` — every limiter-on export and level
/// measurement. A binary that will not run reads as having it (the modern spelling,
/// like [`zscale_available`]: any render on it is about to fail anyway, and it keeps
/// the builders' output from depending on whether a test machine has ffmpeg).
fn alimiter_latency_available() -> bool {
    // Tests pin the answer so an argv oracle does not depend on the ffmpeg
    // installed where it runs (see `golden`).
    #[cfg(test)]
    if let Some(forced) = ALIMITER_LATENCY_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let ok = output_bounded(
            command(&ffmpeg_bin()).args(["-hide_banner", "-loglevel", "quiet", "-h", "filter=alimiter"]),
            CAPABILITY_PROBE_TIMEOUT,
            0,
        )
        .map(|o| help_lists_latency(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(true);
        tracing::debug!(available = ok, "probed ffmpeg for alimiter latency");
        ok
    })
}

/// The filters the master bus adds after the final sum — its fader, then its
/// limiter — each led by the comma that joins it to the `amix` before it, or
/// the empty string while the master is neutral (the graph exactly as it was
/// before there was a master).
///
/// The limiter is `alimiter` with the two options that matter spelled out:
/// `level=0`, because its default **auto-levels** — scales the output back up so
/// the peak lands at full scale, which would turn a ceiling into a makeup gain —
/// and `latency=1`, because without it the lookahead delays the whole mix by the
/// attack time and drops the last few milliseconds at the end, putting the sound
/// out of step with the picture. An ffmpeg whose `alimiter` has no `latency`
/// ([`alimiter_latency_available`]) is given the limiter without it: the mix then
/// trails the picture by the 5 ms attack and loses its last 5 ms, under a frame at
/// any rate. Attack 5 ms is the filter's own lookahead; the
/// 100 ms release is slow enough not to pump on low notes. The ceiling is
/// rounded to six decimals, which is far below anything audible and keeps the
/// text from depending on the last digit of a libm `pow`.
///
/// With `true_peak` the limiter runs on the mix upsampled four times (`aresample` there and
/// back around it), so it holds the peaks between the samples too: the true peak a
/// platform measures, not only the sample peak.
fn master_filters(master: &crate::model::MasterBus, sample_rate: u32) -> String {
    let mut out = String::new();
    let volume = master.safe_volume();
    if (volume - 1.0).abs() > f64::EPSILON {
        out += &format!(",volume={}", fnum(volume));
    }
    if master.limiter {
        let limiter = limiter_filter(master.limit_linear());
        if master.true_peak {
            out += &oversampled(&limiter, sample_rate);
        } else {
            out += &format!(",{limiter}");
        }
    }
    out
}

/// The master's `alimiter` holding `limit` (linear, rounded to six decimals), with
/// `latency=1` where this ffmpeg has it (see [`master_filters`]).
fn limiter_filter(limit: f64) -> String {
    let limit = (limit * 1e6).round() / 1e6;
    let latency = if alimiter_latency_available() { ":latency=1" } else { "" };
    format!("alimiter=limit={}:attack=5:release=100:level=0{latency}", fnum(limit))
}

/// `filter` run at four times `sample_rate` and resampled back, led by its joining comma: a
/// limiter there sees the peaks between the output's samples (BS.1770 reads the true peak
/// off the same 4x oversampling).
fn oversampled(filter: &str, sample_rate: u32) -> String {
    format!(
        ",aresample={},{filter},aresample={sample_rate}",
        sample_rate.saturating_mul(4)
    )
}

/// The static loudness normalization: the measured gain (omitted when there is none to
/// apply) then a true-peak limiter just under the target's ceiling, led by its comma.
fn static_loudness_filters(target: LoudnessPreset, gain_db: Option<f64>, sample_rate: u32) -> String {
    let mut out = String::new();
    if let Some(g) = gain_db.filter(|g| g.is_finite() && g.abs() >= 0.005) {
        out += &format!(",volume={:.2}dB", g.clamp(-LOUDNESS_MAX_GAIN_DB, LOUDNESS_MAX_GAIN_DB));
    }
    let ceiling = 10f64.powf((target.true_peak_db() - STATIC_TP_MARGIN_DB) / 20.0);
    out + &oversampled(&limiter_filter(ceiling), sample_rate)
}

/// The owning track's mix settings, carried alongside each of its audio clips.
/// The fader rides the *finished* clip — after its own gain and effect chain,
/// the way a channel strip does — so pulling a music bed down does not change
/// what its own compressor was reacting to.
#[derive(Clone, Copy)]
struct TrackMix {
    duck: bool,
    volume: f32,
    pan: (f64, f64),
}

/// Format an f64 for an ffmpeg filter argument / expression (Rust's default
/// `{}` avoids scientific notation for the ranges used here; `-0` is normalized).
fn fnum(v: f64) -> String {
    let s = format!("{v}");
    if s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

/// The overlay offset a motion transition puts on a clip, as `(x, y)`
/// expressions in frame widths and heights over **timeline** time — or `None`
/// when the clip does not move, which is what keeps every non-motion graph
/// byte-identical.
///
/// Both halves are ordinary keyframes ([`ClipTiming::motion_keys`] decides them),
/// so this is [`keyframe_expr`] twice over rather than a second expression
/// language.
fn motion_expr(clip: &Clip, fx: &ClipFx) -> Option<(String, String)> {
    let keys = ClipTiming::new(clip, fx).motion_keys()?;
    let start = clip.timeline_start;
    Some((keyframe_expr(&keys.x, "t", start), keyframe_expr(&keys.y, "t", start)))
}

/// The `geq` that cuts a clip to its [`Mask`]: the picture is passed through
/// untouched and only the alpha plane is rewritten, so what is outside the shape
/// (or inside it, `inverted`) becomes transparent and a lower track shows
/// through.
///
/// One expression covers both shapes. Each axis is scaled so the shape's edge
/// sits at distance 1, and the shapes differ only in how the two axes combine —
/// `max` gives a rectangle, `hypot` an ellipse. Feathering is then a ramp over
/// the last `feather` of that distance, measured *inside* the edge so a softened
/// mask never grows beyond the shape that was drawn.
///
/// `geq` is per-pixel and therefore slow, the same cost keyframed opacity
/// already pays; a mask is worth it and a full-frame one is simply not written.
fn mask_filter(mask: &Mask) -> String {
    format!(
        "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({keep})*alpha(X,Y)'",
        keep = mask_keep_expr(mask)
    )
}

/// The `geq` slot a keyed opacity beside a keyed mask is kept in (the mask's numbers take
/// `0..5`, in [`Property::MASK`] order).
const OPACITY_SLOT: usize = Property::MASK.len();

/// The alpha of a clip whose mask has a keyed number: [`mask_keep_expr`] with each keyed number
/// its curve over `geq`'s `T`, times a keyed opacity's curve when there is one (`opacity`), so a
/// frame of it is [`mask_keep_expr`] of [`Clip::mask_at`] that moment.
///
/// `geq` evaluates its expression for every pixel, and a curve is a tree of comparisons, so
/// the curves are evaluated **once a row**: at its first pixel (`X` 0) each is stored in a slot
/// (`st`) and every other pixel reads it back (`ld`). A slice thread has its own copy of the
/// expression and so its own slots, and walks its rows from `X` 0. A feather keyed to nothing is
/// the hard edge a static mask with no feather gets.
fn keyed_mask_alpha(clip: &Clip, mask: &Mask, opacity: Option<&str>) -> String {
    let m = mask.normalized();
    let mut stores: Vec<String> = Property::MASK
        .iter()
        .enumerate()
        .filter(|(_, p)| clip.is_keyed(**p))
        .map(|(slot, p)| {
            format!(
                "st({slot},{})",
                keyframe_expr(&clip.property_curve(*p), "T", clip.timeline_start)
            )
        })
        .collect();
    if let Some(op) = opacity {
        stores.push(format!("st({OPACITY_SLOT},{op})"));
    }
    let slot = |p: Property| Property::MASK.iter().position(|q| *q == p).unwrap_or(0);
    let n = |p: Property, v: f64| {
        if clip.is_keyed(p) {
            format!("ld({})", slot(p))
        } else {
            fnum(v)
        }
    };
    let half = |p: Property, v: f64| {
        if clip.is_keyed(p) {
            format!("ld({})/2", slot(p))
        } else {
            fnum(v / 2.0)
        }
    };
    let dx = format!(
        "(X-{cx}*W)/({rw}*W)",
        cx = n(Property::MaskX, m.x),
        rw = half(Property::MaskWidth, m.width)
    );
    let dy = format!(
        "(Y-{cy}*H)/({rh}*H)",
        cy = n(Property::MaskY, m.y),
        rh = half(Property::MaskHeight, m.height)
    );
    let d = match m.shape {
        MaskShape::Rect => format!("max(abs({dx})\\,abs({dy}))"),
        MaskShape::Ellipse => format!("hypot({dx}\\,{dy})"),
    };
    let inside = if clip.is_keyed(Property::MaskFeather) {
        let f = n(Property::MaskFeather, m.feather);
        format!("if(lte({f}\\,0.000001)\\,lte({d}\\,1)\\,clip((1-{d})/{f}\\,0\\,1))")
    } else if m.feather <= 1e-6 {
        format!("lte({d}\\,1)")
    } else {
        format!("clip((1-{d})/{f}\\,0\\,1)", f = fnum(m.feather))
    };
    let keep = if m.inverted { format!("(1-{inside})") } else { inside };
    let op = if opacity.is_some() {
        format!("*ld({OPACITY_SLOT})")
    } else {
        String::new()
    };
    format!(
        "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='if(eq(X,0),{stores},0);({keep}){op}*alpha(X,Y)'",
        stores = stores.join("+")
    )
}

/// The corners `perspective` (`sense=source`: the picture's points its output's corners `(0,0)` ..
/// `(W,H)` read from) is given for the animated crop's window ([`Clip::crop_window_at`]): `side` of
/// each side of the picture, placed by the pans in the room left over. Output pixel `x` reads the
/// picture at `left * W + (x + 0.5) * side - 0.5` — pixel centres, as a scale reads them, so the
/// neutral window reads every pixel where it is — which puts the top-left corner at
/// `((1 + pan) * W / 2 - 0.5) * (1 - side)` and the bottom-right one at
/// `W - ((1 - pan) * W / 2 + 0.5) * (1 - side)` (the same point plus `side * W`, written so each
/// corner holds one copy of each curve). The same text for the export's curves and the still's
/// numbers, so both evaluate one formula.
fn crop_corners(side: &str, pan_x: &str, pan_y: &str) -> String {
    let x0 = format!("((1+{pan_x})*W/2-0.5)*(1-{side})");
    let y0 = format!("((1+{pan_y})*H/2-0.5)*(1-{side})");
    let x1 = format!("W-((1-{pan_x})*W/2+0.5)*(1-{side})");
    let y1 = format!("H-((1-{pan_y})*H/2+0.5)*(1-{side})");
    format!("x0='{x0}':y0='{y0}':x1='{x1}':y1='{y0}':x2='{x0}':y2='{y1}':x3='{x1}':y3='{y1}'")
}

/// The `fps` ahead of a keyed crop: with its first frame's slot pinned (`start_time`, the time the
/// chain's `setpts` puts its first frame at), so that slot is known here — `perspective` has no
/// clock, only a count of its frames (`in`, from 1), and frame `in` is output frame
/// `first + in - 1`. Returns the filter and that first frame: `fps` rounds `start_time`, read as
/// whole microseconds, to its own ticks half away from zero.
fn keyed_crop_fps(fps: f64, first: f64) -> (String, Rational, i64) {
    let rate = Rational::from_fps_filter(fps).unwrap_or(Rational { num: 30, den: 1 });
    let micros = (first * 1e6) as i64;
    let slot = crate::frame_pick::rescale_near(i128::from(micros), i128::from(rate.num), 1_000_000 * i128::from(rate.den)) as i64;
    (format!("fps=fps={fps}:start_time={first}"), rate, slot)
}

/// The `perspective` that draws a keyed crop: [`crop_corners`] of the window's curves over the
/// time of the frame (`(first + in - 1) * den / num`, the time the graph evaluates output frame
/// `first + in - 1` at), re-evaluated every frame. It keeps the picture's size — that is why the
/// crop is keyed as a zoom and a pan — and reads it bilinearly, a fixed lookup per frame instead
/// of an expression per pixel.
fn crop_remap_filter(clip: &Clip, rate: Rational, first: i64) -> String {
    let t = perspective_time(rate, first);
    let n = |p: Property| {
        if clip.is_keyed(p) {
            format!("({})", keyframe_expr(&clip.property_curve(p), &t, clip.timeline_start))
        } else {
            fnum(p.neutral())
        }
    };
    format!(
        "perspective={}:interpolation=linear:eval=frame",
        crop_corners(
            &format!("(1/{})", n(Property::CropZoom)),
            &n(Property::CropX),
            &n(Property::CropY)
        )
    )
}

/// The time a `perspective` evaluates output frame `first + in - 1` at (it has no clock, only
/// `in`, its count of frames from 1; see [`keyed_crop_fps`]).
fn perspective_time(rate: Rational, first: i64) -> String {
    let frame = match first - 1 {
        0 => "in".to_string(),
        a if a > 0 => format!("in+{a}"),
        a => format!("in-{}", -a),
    };
    format!("(({frame})*{}/{})", rate.den, rate.num)
}

/// The last stage of a clip whose zoom or position moves ([`Clip::placement_animated`]): the
/// picture is laid on a canvas-sized frame, zoomed about its centre and moved, in **one bilinear
/// resample at sub-pixel positions**, and the overlay then puts that frame at `0:0`.
///
/// A per-frame `scale` makes whole-pixel sizes and the overlay's `x`/`y` truncate to even
/// pixels in 4:2:0, so a slow zoom-pan (a still at 1.10 to 1.17 over 3.5 s) moved in irregular
/// one- and two-pixel steps. Here every frame is a fixed lookup (`perspective`, `eval=frame`,
/// as a keyed crop is drawn) and the size never changes. The keyed numbers are curves over the
/// time of the frame `perspective` counts ([`perspective_time`]); the travel of a slide or push
/// is added to the position. `alpha`: the chain already has an alpha plane.
fn placement_filters(clip: &Clip, fx: &ClipFx, fmt: &ExportFormat, alpha: bool, rate: Rational, first: i64) -> Vec<String> {
    let t = perspective_time(rate, first);
    let start = clip.timeline_start;
    let curve = |pts: &[(f64, f64)]| keyframe_expr(pts, &t, start);
    let scale = clip.zoom_animated().then(|| driven_expr(clip, Property::Scale, &t));
    let (mut px, mut py) = (driven_expr(clip, Property::PosX, &t), driven_expr(clip, Property::PosY, &t));
    if let Some(keys) = ClipTiming::new(clip, fx).motion_keys() {
        px = format!("({px})+({})", curve(&keys.x));
        py = format!("({py})+({})", curve(&keys.y));
    }
    placement_chain(scale.as_deref(), (&px, &py), (fmt.width, fmt.height), alpha, true)
}

/// The filters of a placement ([`placement_filters`]; the scrubbed still's, with the numbers of its
/// moment): the picture zoomed by `scale` (`None`: not at all) about its centre, moved to the
/// canvas' centre plus `pos` (in canvas widths and heights), on a `canvas`-sized `yuva420p` frame.
///
/// The picture is padded with transparency (`black@0`), centred, to at least the canvas and two
/// pixels past itself on every side, so what `perspective` reads past the picture (it clamps to
/// the edge) is transparent; it is then cut to the canvas round the same centre. Both offsets are
/// even whole numbers (`2*ceil(..)`, `2*floor(..)`), so neither `pad` nor `crop` rounds them, and
/// `perspective` (which knows only its own frame size) computes the same window. Its corners map
/// pixel centres: canvas pixel `X` reads the picture at `(X + 0.5 - cx) / s + w/2 - 0.5`.
fn placement_chain(scale: Option<&str>, pos: (&str, &str), canvas: (u32, u32), alpha: bool, per_frame: bool) -> Vec<String> {
    let (cw, ch) = canvas;
    // One axis: the picture position its output pixel `edge` (`0` or the frame's `W` / `H`) reads.
    let corner = |edge: &str, dim: &str, canvas: u32, pos: &str| {
        let centre = format!("{}+({pos})*{canvas}", fnum(f64::from(canvas) / 2.0));
        let off = format!("{edge}+0.5-2*floor(({dim}-{canvas})/4)-({centre})");
        match scale {
            Some(s) => format!("({off})/({s})+{dim}/2-0.5"),
            None => format!("{off}+{dim}/2-0.5"),
        }
    };
    let (x0, x1) = (corner("0", "W", cw, pos.0), corner("W", "W", cw, pos.0));
    let (y0, y1) = (corner("0", "H", ch, pos.1), corner("H", "H", ch, pos.1));
    let mut p = Vec::new();
    if !alpha {
        p.push("format=yuva420p".to_string());
    }
    p.extend([
        format!(
            "pad=w='iw+4*ceil(max(0,{cw}-iw)/4)+4':h='ih+4*ceil(max(0,{ch}-ih)/4)+4':\
             x='2*ceil(max(0,{cw}-iw)/4)+2':y='2*ceil(max(0,{ch}-ih)/4)+2':color=black@0"
        ),
        format!(
            "perspective=x0='{x0}':y0='{y0}':x1='{x1}':y1='{y0}':x2='{x0}':y2='{y1}':x3='{x1}':y3='{y1}':\
             interpolation=linear{}",
            if per_frame { ":eval=frame" } else { "" }
        ),
        format!("crop=w={cw}:h={ch}:x='2*floor((iw-{cw})/4)':y='2*floor((ih-{ch})/4)'"),
    ]);
    p
}

/// The placement chain **as the export and the still write it** (`per_frame`: the export's `eval=frame`), for the
/// compositor's probe of what this machine's `pad`, `perspective` and `crop` do with it: the text is
/// [`placement_chain`]'s, joined, so a probe measures the filters the graph really holds and cannot drift from them.
pub fn placement_probe_chain(scale: Option<&str>, pos: (&str, &str), canvas: (u32, u32), per_frame: bool) -> String {
    placement_chain(scale, pos, canvas, false, per_frame).join(",")
}

/// A moving placement as the export writes it, and the numbers the plan evaluates for it ([`placement_probe_motion`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementProbeMotion {
    /// The zoom's expression over `in` (the frame count `perspective` has), the position's two.
    pub scale: String,
    pub pos: (String, String),
    /// What the plan evaluates for frame `in = k + 1` (index `k`): the zoom and the position.
    pub poses: Vec<(f64, (f64, f64))>,
}

/// A clip whose zoom and position are keyed and which breathes, drifts and shakes at 30 fps, as
/// [`placement_filters`] writes it (the same [`driven_expr`] over the same `in`-time) and as the plan evaluates it
/// ([`Clip::transform_at`]) for the first `frames` frames of the graph. What the compositor's probe measures
/// the filters against: the expression's `sin` and `cos` are the machine's libm's, the plan's the app's.
pub fn placement_probe_motion(frames: u32) -> PlacementProbeMotion {
    use crate::model::{Behavior, PropertyKey};
    let rate = Rational { num: 30, den: 1 };
    let mut clip = Clip::new(uuid::Uuid::nil(), 0.0, 10.0, 0.0);
    clip.set_property_keys(
        Property::Scale,
        vec![
            PropertyKey::new(0.0, 1.1),
            PropertyKey::new(0.2, 2.3),
            PropertyKey::new(0.4, 0.7),
        ],
    );
    clip.set_property_keys(Property::PosX, vec![PropertyKey::new(0.0, -0.2), PropertyKey::new(0.3, 0.15)]);
    clip.set_property_keys(Property::PosY, vec![PropertyKey::new(0.0, 0.1), PropertyKey::new(0.3, -0.25)]);
    clip.behaviors = vec![
        Behavior::Breathe {
            amount: 0.12,
            speed: 0.8,
            phase: 1.25,
        },
        Behavior::Drift {
            speed: 0.15,
            angle: -33.0,
            phase: 0.5,
        },
        Behavior::Shake {
            amount: 0.013,
            speed: 3.1,
            seed: 4_000_000_001,
            phase: 0.5,
        },
    ];
    let t = perspective_time(rate, 0);
    let poses = (0..frames)
        .map(|k| {
            // The graph's `((in-1)*den/num)`, in the filter's own double arithmetic.
            let local = (f64::from(k) * f64::from(rate.den)) / f64::from(rate.num);
            let at = clip.transform_at(local);
            (at.scale, (at.pos_x, at.pos_y))
        })
        .collect();
    PlacementProbeMotion {
        scale: driven_expr(&clip, Property::Scale, &t),
        pos: (driven_expr(&clip, Property::PosX, &t), driven_expr(&clip, Property::PosY, &t)),
        poses,
    }
}

/// The 0..1 "keep" expression at the heart of [`mask_filter`], separated out so
/// a clip with both a mask and keyframed opacity can fold the two into one
/// `geq` pass instead of paying the per-pixel cost twice.
fn mask_keep_expr(mask: &Mask) -> String {
    let m = mask.normalized();
    let dx = format!("(X-{cx}*W)/({rw}*W)", cx = fnum(m.x), rw = fnum(m.width / 2.0));
    let dy = format!("(Y-{cy}*H)/({rh}*H)", cy = fnum(m.y), rh = fnum(m.height / 2.0));
    let d = match m.shape {
        MaskShape::Rect => format!("max(abs({dx})\\,abs({dy}))"),
        MaskShape::Ellipse => format!("hypot({dx}\\,{dy})"),
    };
    let inside = if m.feather <= 1e-6 {
        format!("lte({d}\\,1)")
    } else {
        format!("clip((1-{d})/{f}\\,0\\,1)", f = fnum(m.feather))
    };
    if m.inverted {
        format!("(1-{inside})")
    } else {
        inside
    }
}

/// A scale below this is one a picture can be truncated to nothing by: `scale` reads a
/// width or height that evaluates to 0 as "unset" and keeps the *input's* size, so a clip
/// zoomed to 0.0004 of a frame would snap to full size instead of vanishing. The scale
/// expressions only say `max(1, ...)` below it, so every ordinary graph is as it was.
const TINY_SCALE: f64 = crate::layer_geometry::TINY_SCALE;

/// The `scale` that zooms a clip's picture by `factor`: a plain number for a constant zoom,
/// else (`keyed`) an expression in `t` that is re-evaluated every frame.
///
/// `tiny` (a factor that can reach [`TINY_SCALE`]) keeps both sides at 1 px or more, and
/// `even` (a tone-map follows: `zscale` refuses an odd size in 4:2:0) rounds them down to
/// an even number of pixels. Neither is written when it is not needed.
fn zoom_scale(factor: &str, keyed: bool, tiny: bool, even: bool, sf: &str) -> String {
    let side = |axis: &str| {
        let e = if keyed {
            format!("{axis}*({factor})")
        } else {
            format!("{axis}*{factor}")
        };
        match (even, tiny) {
            (true, _) => format!("max(2,2*trunc(({e})/2))"),
            (false, true) => format!("max(1,{e})"),
            (false, false) => e,
        }
    };
    let (w, h) = (side("iw"), side("ih"));
    if keyed {
        format!("scale=w='{w}':h='{h}':eval=frame{sf}")
    } else if even || tiny {
        format!("scale=w='{w}':h='{h}'{sf}")
    } else {
        format!("scale={w}:{h}{sf}")
    }
}

/// The one `scale` that fits a picture to a `w`x`h` frame and zooms it by `factor` in the same
/// resample ([`FitOrder::Folded`] with `decrease`, [`FitOrder::Uncropped`] with `increase`): the
/// frame times the zoom is the box the aspect-preserving fit is made in, so the picture is taken
/// from the source once instead of being fitted to the frame and the fitted picture enlarged. A
/// zoom of 1 is the frame's own box.
///
/// `tone_mapped` (a `zscale` follows, which refuses an odd size in 4:2:0) rounds the box and the
/// result to even sizes, and a zoom under [`TINY_SCALE`] keeps the box at a pixel or more, as
/// [`zoom_scale`] does. Neither is written when it is not needed.
fn fit_scale(w: u32, h: u32, factor: f64, aspect: &str, tone_mapped: bool, sf: &str) -> String {
    let zoomed = (factor - 1.0).abs() > 1e-9;
    let tiny = zoomed && factor < TINY_SCALE;
    let side = |n: u32| {
        let e = if zoomed { format!("{n}*{factor}") } else { n.to_string() };
        match (tone_mapped && zoomed, tiny) {
            (true, _) => format!("max(2,2*trunc(({e})/2))"),
            (false, true) => format!("max(1,{e})"),
            (false, false) => e,
        }
    };
    let (bw, bh) = (side(w), side(h));
    let even = if tone_mapped { ":force_divisible_by=2" } else { "" };
    if (tone_mapped && zoomed) || tiny {
        format!("scale=w='{bw}':h='{bh}':force_original_aspect_ratio={aspect}{even}{sf}")
    } else {
        format!("scale={bw}:{bh}:force_original_aspect_ratio={aspect}{even}{sf}")
    }
}

/// The `crop` that keeps an [`FitOrder::Uncropped`] picture — the cover of the frame times the
/// zoom, which can be several frames wide — to what any frame of the clip can show, so the
/// overlay is not handed a needlessly large picture (pure).
///
/// The window is **centred on the picture**, so the overlay's own `(W-w)/2` still puts the picture's
/// centre where it would have put the whole one, and it is cut in steps of four pixels
/// (`iw - 4q`), so an even picture stays even and each side loses an even amount, which the
/// overlay's rounding down to even then leaves alone. Its half-extent is how far the canvas
/// reaches from the picture's centre: half the frame plus the furthest the picture is ever moved
/// (`pan`: the position's greatest value and a slide or push's travel, in frame fractions), turned
/// with the picture (`rotation`: the angle of a static one, the circle round the frame when the
/// rotation is keyed) and given `margin` pixels for what reads past the edge of the window (a
/// blur). Where the picture is smaller than that, it is not cut at all.
fn bound_crop(frame: (u32, u32), pan: (f64, f64), rotation: Rotated, margin: f64) -> String {
    let ax = f64::from(frame.0) * (0.5 + pan.0);
    let ay = f64::from(frame.1) * (0.5 + pan.1);
    let (wx, wy) = match rotation {
        Rotated::No => (ax, ay),
        Rotated::By(angle) => (
            ax * angle.cos().abs() + ay * angle.sin().abs(),
            ax * angle.sin().abs() + ay * angle.cos().abs(),
        ),
        Rotated::Keyed => (ax.hypot(ay), ax.hypot(ay)),
    };
    // (A turn of exactly 90 degrees leaves a cosine of 1e-16; the epsilon keeps it from costing a pixel.)
    // (A number that is not one — a hand-edited file — keeps the whole picture, as it never cuts.)
    let need = |half: f64| {
        let v = 2.0 * half + 2.0 * margin - 1e-6;
        if v.is_finite() {
            v.ceil().clamp(2.0, 1e9) as u64
        } else {
            1_000_000_000
        }
    };
    format!(
        "crop=w='iw-4*floor(max(0,iw-{nw})/4)':h='ih-4*floor(max(0,ih-{nh})/4)'",
        nw = need(wx),
        nh = need(wy)
    )
}

/// How an [`FitOrder::Uncropped`] clip turns its picture, for [`bound_crop`].
#[derive(Clone, Copy)]
enum Rotated {
    No,
    /// A static rotation, in radians.
    By(f64),
    /// A keyed rotation that turns the picture: any angle.
    Keyed,
}

/// Polyline points above which [`keyframe_expr`] writes a balanced tree instead of a chain.
/// libavutil refuses an expression nested deeper than 100 levels (`Invalid argument`, from
/// the filter that holds it), a chain is a level per point, and an eased key is 12 points: ten
/// eased keys failed the export and the playback stream alike. Up to this many points the
/// chain's depth is harmless and its text is the one every earlier graph carried.
const KEYFRAME_TREE_POINTS: usize = 24;

/// Build a piecewise-linear ffmpeg expression over **clip-local time** for a
/// channel of keyframes. `points` are `(seconds_from_clip_start, value)` and are
/// sorted here. `tvar` is the time variable the target filter exposes (`t` for
/// overlay / scale / rotate, `T` for geq); `start` is the clip's `timeline_start`
/// so the expression reads time relative to the clip. Values hold flat before the
/// first and after the last keyframe.
///
/// Few points are nested `if(lt(..))`s walked from the first segment; many are a balanced
/// binary tree over the segments (`if(lt(t,t_mid),left,right)`, [`KEYFRAME_TREE_POINTS`]),
/// which is `log2(n)` levels deep and `log2(n)` comparisons per evaluation (a `geq` runs it per
/// pixel). Both pick the one segment whose span holds the time, so they evaluate alike.
fn keyframe_expr(points: &[(f64, f64)], tvar: &str, start: f64) -> String {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    if pts.is_empty() {
        return "0".to_string();
    }
    if pts.len() == 1 {
        return fnum(pts[0].1);
    }
    let lt = format!("({tvar}-{})", fnum(start));
    let last = pts[pts.len() - 1];
    // The part between the first and the last key; the ends hold their values.
    let inside = if pts.len() > KEYFRAME_TREE_POINTS {
        let tree = keyframe_tree(&pts, 0, pts.len() - 1, &lt);
        format!("if(lt({lt},{tl}),{tree},{vl})", tl = fnum(last.0), vl = fnum(last.1))
    } else {
        // Fold segments in from the end; `expr` starts as the value held after the
        // last keyframe.
        let mut expr = fnum(last.1);
        for w in (0..pts.len() - 1).rev() {
            expr = format!(
                "if(lt({lt},{t1}),{seg},{expr})",
                lt = lt,
                t1 = fnum(pts[w + 1].0),
                seg = keyframe_segment(&pts, w, &lt),
                expr = expr
            );
        }
        expr
    };
    // Hold the first value before the first keyframe.
    format!(
        "if(lt({lt},{t0}),{v0},{inside})",
        lt = lt,
        t0 = fnum(pts[0].0),
        v0 = fnum(pts[0].1),
        inside = inside
    )
}

/// What [`keyframe_expr`]`(points, _, start)` evaluates to at time `t`, in the expression's own
/// `double` arithmetic and order (`av_expr` parses the constants back to these `f64`s, subtracts
/// as it adds the negation and multiplies by its sign 1): the same segment, picked by the same
/// comparisons, and the same line through it. A filter that keys a number per frame (`geq`'s
/// opacity) can be followed to the last bit.
pub(crate) fn keyframe_value(points: &[(f64, f64)], t: f64, start: f64) -> f64 {
    // (A curve is sorted already; the stable sort of one that is changes nothing, so it is not copied.)
    let sorted;
    let pts: &[(f64, f64)] = if points.windows(2).all(|w| w[0].0.total_cmp(&w[1].0).is_le()) {
        points
    } else {
        let mut copy = points.to_vec();
        copy.sort_by(|a, b| a.0.total_cmp(&b.0));
        sorted = copy;
        &sorted
    };
    match pts.len() {
        0 => return 0.0,
        1 => return pts[0].1,
        _ => {}
    }
    let lt = t - start;
    let segment = |w: usize| {
        let (t0, v0) = pts[w];
        let (t1, v1) = pts[w + 1];
        if (t1 - t0).abs() < 1e-9 {
            v0
        } else {
            v0 + (v1 - v0) * (lt - t0) / (t1 - t0)
        }
    };
    if lt < pts[0].0 {
        return pts[0].1;
    }
    let last = pts[pts.len() - 1];
    if pts.len() > KEYFRAME_TREE_POINTS {
        if lt >= last.0 {
            return last.1;
        }
        let (mut lo, mut hi) = (0, pts.len() - 1);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if lt < pts[mid].0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        return segment(lo);
    }
    (0..pts.len() - 1).find(|&w| lt < pts[w + 1].0).map_or(last.1, segment)
}

/// The value inside segment `w` (`pts[w]` to `pts[w + 1]`): a straight line, or the first
/// value for a step (equal times).
fn keyframe_segment(pts: &[(f64, f64)], w: usize, lt: &str) -> String {
    let (t0, v0) = pts[w];
    let (t1, v1) = pts[w + 1];
    if (t1 - t0).abs() < 1e-9 {
        fnum(v0)
    } else {
        format!(
            "({v0}+({dv})*({lt}-{t0})/({dt}))",
            v0 = fnum(v0),
            dv = fnum(v1 - v0),
            lt = lt,
            t0 = fnum(t0),
            dt = fnum(t1 - t0),
        )
    }
}

/// Segments `lo..hi` of `pts` as a balanced tree, for a time known to lie in
/// `[pts[lo].0, pts[hi].0)`. A step's empty span is never reached.
fn keyframe_tree(pts: &[(f64, f64)], lo: usize, hi: usize, lt: &str) -> String {
    if hi - lo == 1 {
        return keyframe_segment(pts, lo, lt);
    }
    let mid = (lo + hi) / 2;
    format!(
        "if(lt({lt},{t}),{left},{right})",
        t = fnum(pts[mid].0),
        left = keyframe_tree(pts, lo, mid, lt),
        right = keyframe_tree(pts, mid, hi, lt),
    )
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// The `eq` filter for a clip's color correction, shared by the export and
/// still chains. `temperature` becomes opposing red/blue per-channel gammas —
/// `eq` has no white-balance knob, but shifting midtone gamma per channel
/// warms/cools convincingly; ±1.0 maps to a ±30% gamma split. The channel
/// gammas are omitted at 0 so a temperature-free clip's graph stays
/// byte-identical to before the field existed.
fn eq_filter(c: &Color) -> String {
    let mut f = format!(
        "eq=brightness={}:contrast={}:saturation={}:gamma={}",
        c.brightness, c.contrast, c.saturation, c.gamma
    );
    if let Some((gamma_r, gamma_b)) = c.temperature_gammas() {
        f.push_str(&format!(":gamma_r={}:gamma_b={}", fnum(gamma_r), fnum(gamma_b)));
    }
    f
}

/// A keyed number's curve, or — not keyed — its static value as a one-point curve (which
/// [`keyframe_expr`] writes as that number).
fn curve_or_static(clip: &Clip, prop: Property) -> Vec<(f64, f64)> {
    if clip.is_keyed(prop) {
        clip.property_curve(prop)
    } else {
        vec![(0.0, clip.static_value(prop))]
    }
}

/// The `eq` filter of a clip with a keyed colour number: `eval=frame`, and each keyed number
/// an expression of the frame's time `t` (the frame's timestamp, which `setpts` has put on the
/// timeline, so clip-local time is `t` minus the clip's start, as every other keyed expression
/// here). Numbers that are not keyed stay the static ones. The temperature is the
/// opposing gammas [`Color::temperature_gammas`] makes of it, `1 + 0.3 t` and `1 - 0.3 t`
/// (the curve's values are held to -1..=1, so the clamp that function applies is already
/// in them). Every expression is quoted — it has commas — and `eq` evaluates them per frame
/// on FFmpeg 4.4 through 9.0 alike.
fn eq_filter_keyed(clip: &Clip) -> String {
    let c = &clip.color;
    let expr = |prop: Property| keyframe_expr(&clip.property_curve(prop), "t", clip.timeline_start);
    let number = |prop: Property, fixed: f64| {
        if clip.is_keyed(prop) {
            format!("'{}'", expr(prop))
        } else {
            fixed.to_string()
        }
    };
    let mut f = format!(
        "eq=brightness={}:contrast={}:saturation={}:gamma={}",
        number(Property::Brightness, c.brightness),
        number(Property::Contrast, c.contrast),
        number(Property::Saturation, c.saturation),
        number(Property::Gamma, c.gamma)
    );
    if clip.is_keyed(Property::Temperature) {
        let e = expr(Property::Temperature);
        f.push_str(&format!(":gamma_r='1+0.3*({e})':gamma_b='1-0.3*({e})'"));
    } else if let Some((gamma_r, gamma_b)) = c.temperature_gammas() {
        f.push_str(&format!(":gamma_r={}:gamma_b={}", fnum(gamma_r), fnum(gamma_b)));
    }
    f.push_str(":eval=frame");
    f
}

/// The colour [`eq_filter_keyed`]`(clip)` evaluates to at the frame time `t` (`pts * av_q2d(tb)` of the
/// `fps` grid the filter sits behind): each keyed number its curve through [`keyframe_value`], the same
/// arithmetic as the quoted expression `av_expr` parses back, the unkeyed ones the clip's own. A renderer
/// that draws the filter's tables with no FFmpeg (`eq` rebuilds them every frame) is given these numbers;
/// `Clip::color_at` is the model's value of the same curve, to within a rounding, and is the still's.
pub(crate) fn keyed_color_value(clip: &Clip, t: f64) -> Color {
    let value = |prop: Property, fixed: f64| {
        if clip.is_keyed(prop) {
            keyframe_value(&clip.property_curve(prop), t, clip.timeline_start)
        } else {
            fixed
        }
    };
    let c = &clip.color;
    Color {
        brightness: value(Property::Brightness, c.brightness),
        contrast: value(Property::Contrast, c.contrast),
        saturation: value(Property::Saturation, c.saturation),
        gamma: value(Property::Gamma, c.gamma),
        temperature: value(Property::Temperature, c.temperature),
    }
}

/// The filter for a non-alpha video effect, or `None` for chroma key (which
/// establishes alpha and is emitted separately, after the alpha plane exists).
fn video_effect_filter(e: &VideoEffect) -> Option<String> {
    Some(match e {
        VideoEffect::Blur { sigma } => blur_filter(*sigma),
        VideoEffect::Sharpen { amount } => {
            format!("unsharp=luma_msize_x=5:luma_msize_y=5:luma_amount={}", fnum(*amount))
        }
        VideoEffect::Grayscale => "hue=s=0".to_string(),
        VideoEffect::Invert => "negate".to_string(),
        VideoEffect::Vignette => "vignette".to_string(),
        VideoEffect::ChromaKey { .. } => return None,
    })
}

/// The `chromakey` filter for a chroma-key effect, or `None` for any other.
fn chroma_filter(e: &VideoEffect) -> Option<String> {
    match e {
        VideoEffect::ChromaKey {
            color,
            similarity,
            blend,
        } => Some(chroma_key_filter(color, *similarity, *blend)),
        _ => None,
    }
}

/// A chroma key as the graph writes it (`chromakey`, the colour made safe). For a renderer's probe, which runs the very text.
pub fn chroma_key_filter(color: &str, similarity: f64, blend: f64) -> String {
    format!(
        "chromakey={}:{}:{}",
        safe_color(color, "green"),
        fnum(similarity),
        fnum(blend)
    )
}

/// A clip's whole audio effect chain as one comma-joined filter string, or `None`
/// when it has no effects.
///
/// Public because the GUI's preview monitor decodes clip audio through the same
/// chain the export renders — the chain is only *described* once, here, so the
/// two can't drift.
pub fn audio_effects_filter(effects: &[AudioEffect]) -> Option<String> {
    (!effects.is_empty()).then(|| {
        effects
            .iter()
            .map(|e| audio_effect_filter(e, None))
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// The filter for one audio effect. dB thresholds / make-up gain are converted to
/// the linear units ffmpeg's dynamics filters expect. `len` is the clip's length where the
/// chain is the export's (a denoise on an `afftdn` that does not delay is cut to it).
fn audio_effect_filter(e: &AudioEffect, len: Option<f64>) -> String {
    match e {
        AudioEffect::Highpass { hz } => format!("highpass=f={}", fnum(*hz)),
        AudioEffect::Lowpass { hz } => format!("lowpass=f={}", fnum(*hz)),
        AudioEffect::Equalizer { hz, width, gain_db } => {
            format!(
                "equalizer=f={}:width_type=h:width={}:g={}",
                fnum(*hz),
                fnum(*width),
                fnum(*gain_db)
            )
        }
        AudioEffect::Compressor {
            threshold_db,
            ratio,
            attack_ms,
            release_ms,
            makeup_db,
        } => format!(
            "acompressor=threshold={}:ratio={}:attack={}:release={}:makeup={}",
            fnum(db_to_linear(*threshold_db)),
            fnum(*ratio),
            fnum(*attack_ms),
            fnum(*release_ms),
            fnum(db_to_linear(*makeup_db)),
        ),
        AudioEffect::Gate { threshold_db } => format!("agate=threshold={}", fnum(db_to_linear(*threshold_db))),
        AudioEffect::Denoise {
            reduction_db,
            floor_db,
            track_noise,
        } => denoise_filter(*reduction_db, *floor_db, *track_noise, len),
    }
}

/// Escape a value for a single-quoted `drawtext` option inside a filtergraph passed as one argv
/// argument. The value is read by **three parsers, innermost first**, and each has its own escape:
///
/// 1. the *expansion* of `text=` (`expansion` here): a backslash escapes the next character, `%{...}`
///    is a function and a bare `%` a configuration error that silently blanks the whole overlay
///    (ffmpeg still exits 0). So `\` -> `\\` and `%` -> `\%`. `fontfile=` is a path, not expanded
///    text: escaping `%` there would corrupt a path that legitimately contains one;
/// 2. the filter's *option string* (`av_get_token` with `:` as the terminator): `:` ends a value, a
///    backslash escapes the next character, an apostrophe starts a quotation, and blanks at either end
///    are dropped. So `\` -> `\\`, `'` -> `\'`, `:` -> `\:`, and a leading or trailing blank gets a
///    backslash (which is also how a Windows font path, `C:\Fonts\a.ttf`, survives);
/// 3. the *filtergraph*: `'...'` protects `[],;` and is stripped, a backslash inside it is literal,
///    and an apostrophe is `'\''` (close, escaped quote, reopen).
///
/// Applying only the third level, as this once did, made an apostrophe swallow every option after it
/// (the text came out as garbage in black at the top left), a colon fail the whole render on FFmpeg 9
/// and cut the text short on 6.1, and a backslash vanish. `drawtext_text_is_what_the_user_typed`
/// renders each against the literal text on whichever FFmpeg runs it.
///
/// A line break (LF, CR, CRLF as one, the Unicode line and paragraph separators) collapses to a
/// space: the overlays are one line, and FFmpeg's `drawtext` would otherwise start another one at a
/// CR as well as at an LF. Control characters other than tab are dropped: a NUL cannot be passed in an
/// argv at all ("nul byte found in provided data" — every preview and export would fail to spawn until
/// the title was found and deleted) and the rest (ESC, the C1 block) draw as nothing useful.
fn escape_drawtext(s: &str, expansion: bool) -> String {
    escape_drawtext_with(s, expansion, false)
}

/// [`escape_drawtext`] that can keep line feeds as line breaks (`lines`), for the compositor's
/// measuring graphs, which draw one prefix of a title per line.
fn escape_drawtext_with(s: &str, expansion: bool, lines: bool) -> String {
    let blank = |c: char| matches!(c, ' ' | '\t');
    let clean = clean_drawtext(s, lines);
    let expanded = if expansion {
        clean.replace('\\', "\\\\").replace('%', "\\%")
    } else {
        clean
    };
    let start = expanded.len() - expanded.trim_start_matches(blank).len();
    let end = expanded.trim_end_matches(blank).len();
    let mut option = String::with_capacity(expanded.len() + 8);
    for (i, c) in expanded.char_indices() {
        match c {
            '\\' => option.push_str("\\\\"),
            '\'' => option.push_str("\\'"),
            ':' => option.push_str("\\:"),
            c => {
                if blank(c) && (i < start || i >= end) {
                    option.push('\\');
                }
                option.push(c);
            }
        }
    }
    option.replace('\'', "'\\''")
}

/// The text `drawtext` is given for `s`, before any escaping: what [`escape_drawtext`] keeps of it (a line
/// break is a blank, or a break of its own with `lines`; control characters other than tab are dropped).
fn clean_drawtext(s: &str, lines: bool) -> String {
    let blank = |c: char| matches!(c, ' ' | '\t');
    let mut clean = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' if lines => clean.push('\n'),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => clean.push(' '),
            c if c.is_control() && !blank(c) => {}
            c => clean.push(c),
        }
    }
    clean
}

/// The characters `drawtext` draws for an overlay's text, in order: the text the compositor measures
/// and rasterises piece by piece ([`text_piece_filter`]).
pub fn text_as_drawn(s: &str) -> String {
    clean_drawtext(s, false)
}

/// The most characters of a text the compositor takes apart bitmap by bitmap (the measuring graph holds
/// a prefix of it per line); more is FFmpeg's.
pub const MAX_SEQUENCE_CHARS: usize = 160;

/// The largest side, in pixels, of the canvas a text is measured on.
pub const MAX_MEASURE_SIDE: f64 = 16_384.0;

/// Why a text cannot be taken apart into the bitmaps `drawtext` blends, when that can be told before
/// anything is run: a tab (`drawtext` lays it out by its own tab stops), more than [`MAX_SEQUENCE_CHARS`]
/// characters, and, once the size is known (`px`, the font's pixel height), a text too wide to be measured
/// on a canvas of [`MAX_MEASURE_SIDE`] pixels. Everything else the compositor cannot take apart (a glyph that
/// does not shape one to a unit, a face whose bitmaps overhang the dictionary's cell) only the render can see.
pub fn text_sequence_limit(text: &str, px: Option<u32>) -> Option<String> {
    let drawn = text_as_drawn(text);
    if drawn.contains('\t') {
        return Some("a tab in the text".to_string());
    }
    let n = drawn.chars().count();
    if n > MAX_SEQUENCE_CHARS {
        return Some(format!("{n} characters of text (over {MAX_SEQUENCE_CHARS})"));
    }
    let px = f64::from(px?);
    if px * 1.8 * n as f64 + 64.0 > MAX_MEASURE_SIDE || px * 3.0 + 64.0 > MAX_MEASURE_SIDE {
        return Some(format!("{n} characters at {px} px are too wide to measure"));
    }
    None
}

/// A filtergraph given to `ffmpeg` in a script file, because it was too long for the command line; the
/// file is removed when this is dropped.
pub struct GraphFile(#[allow(dead_code)] GraphScript);

/// Move the `-filter_complex` graph of `args` into a script file if it is too long to pass in argv (the
/// engine's own rule, [`GRAPH_ARG_MAX`]), pointing `ffmpeg` at it with the spelling this build takes.
/// Nothing moves otherwise. For a long graph of a caller outside the engine (the compositor measures a text
/// with an instance of `drawtext` per prefix); a graph that holds a line feed in a quoted value is better left
/// in argv.
pub fn spill_long_graph(args: &mut [String], tag: &str) -> Result<GraphFile> {
    externalize_filter_complex(args, tag).map(GraphFile)
}

/// [`escape_drawtext`] for `text=`.
fn escape_drawtext_text(s: &str) -> String {
    escape_drawtext(s, true)
}

/// [`escape_drawtext`] for `fontfile=`, a raw filesystem path rather than drawtext-expanded text.
fn escape_drawtext_path(s: &str) -> String {
    escape_drawtext(s, false)
}

/// How an overlay looks, as `drawtext` is told it: what the export, the still and the
/// compositor's rasteriser ([`text_raster_graph`]) all spell the same way. Every field is already
/// safe to splice (`safe_color`, `valid_color`, a resolved font).
struct TextLook<'a> {
    text: &'a str,
    /// Font height as a fraction of the frame height.
    size: f64,
    color: &'a str,
    /// Whether there is a box, and its colour.
    bg: Option<&'a str>,
    /// The font file; `None` leaves `drawtext` its own default (the shipped font could not be written).
    font_file: Option<&'a Path>,
    /// No bold face was found for a bold request: a same-colour border thickens the glyphs.
    border: bool,
}

/// The width of the border [`TextLook::border`] draws, and the padding of a box, in pixels.
const TEXT_BORDER_W: u32 = 2;
const TEXT_BOX_BORDER_W: u32 = 12;

impl TextLook<'_> {
    /// `drawtext` options shared by the export and still paths (text, size, color,
    /// font, bold approximation, box) — everything except position / alpha /
    /// enable. `frame_h` is the target canvas height (export height, or the
    /// still's height).
    fn options(&self, frame_h: f64) -> Vec<String> {
        self.options_coloured(frame_h, self.color, self.color, self.bg)
    }

    /// [`TextLook::options`] with the colours of the glyphs, the border and the box named: the
    /// rasteriser draws each part white and the others invisible.
    fn options_coloured(&self, frame_h: f64, font: &str, border: &str, boxed: Option<&str>) -> Vec<String> {
        let fontsize = (frame_h * self.size).round().max(1.0);
        let mut parts = vec![
            format!("text='{}'", escape_drawtext_text(self.text)),
            format!("fontsize={}", fnum(fontsize)),
            format!("fontcolor={font}"),
        ];
        if let Some(path) = self.font_file {
            parts.push(format!("fontfile='{}'", escape_drawtext_path(&path.to_string_lossy())));
        }
        if self.border {
            parts.push(format!("borderw={TEXT_BORDER_W}"));
            parts.push(format!("bordercolor={border}"));
        }
        // An invalid box colour drops the box entirely rather than falling back to
        // a visible default — a caller that asked for no readable box colour gets
        // no box, not a random one.
        if let Some(bg) = self.bg.and(boxed) {
            parts.push("box=1".to_string());
            parts.push(format!("boxcolor={bg}"));
            parts.push(format!("boxborderw={TEXT_BOX_BORDER_W}"));
        }
        parts
    }
}

/// `drawtext` options for an overlay of the timeline (see [`TextLook::options`]). The font is the
/// family it names if that is installed, else the shipped default (`crate::fonts`); a bold
/// request the face cannot satisfy is a same-colour border.
fn drawtext_common(o: &TextOverlay, frame_h: f64) -> Vec<String> {
    let font = crate::fonts::resolve_text_font(o.font.as_deref(), o.bold);
    TextLook {
        text: &o.text,
        size: o.size,
        color: safe_color(&o.color, "white"),
        bg: o.bg.as_deref().filter(|bg| valid_color(bg)),
        font_file: font.as_ref().map(|f| f.path.as_path()),
        border: o.bold && !font.as_ref().is_some_and(|f| f.bold),
    }
    .options(frame_h)
}

/// The export-path `drawtext` for an overlay: `enable`-gated to its lifetime,
/// with per-frame position / alpha expressions when it is animated.
fn drawtext_export(o: &TextOverlay, fmt: &ExportFormat) -> String {
    let mut parts = drawtext_common(o, fmt.height as f64);
    if o.keyframes.is_empty() {
        parts.push(format!("x=(w*{}-text_w/2)", fnum(o.pos_x)));
        parts.push(format!("y=(h*{}-text_h/2)", fnum(o.pos_y)));
    } else {
        let xs: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.pos_x)).collect();
        let ys: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.pos_y)).collect();
        let al: Vec<(f64, f64)> = o.keyframes.iter().map(|k| (k.time, k.opacity)).collect();
        // Quoted: a piecewise expression contains commas, and an unquoted comma
        // ends the filter as far as the graph parser is concerned.
        parts.push(format!("x='(w*({})-text_w/2)'", keyframe_expr(&xs, "t", o.start)));
        parts.push(format!("y='(h*({})-text_h/2)'", keyframe_expr(&ys, "t", o.start)));
        parts.push(format!("alpha='{}'", keyframe_expr(&al, "t", o.start)));
    }
    parts.push(format!("enable='between(t,{},{})'", fnum(o.start), fnum(o.end)));
    format!("drawtext={}", parts.join(":"))
}

/// The still-path `drawtext` for an overlay active at time `t`: position / alpha
/// sampled to constants (the still pipeline has no timeline clock), no `enable`.
fn drawtext_still(o: &TextOverlay, frame_h: u32, t: f64) -> String {
    let mut parts = drawtext_common(o, frame_h as f64);
    let (px, py, op) = o.sample(t);
    parts.push(format!("x=(w*{}-text_w/2)", fnum(px)));
    parts.push(format!("y=(h*{}-text_h/2)", fnum(py)));
    if op < 1.0 {
        parts.push(format!("alpha={}", fnum(op)));
    }
    format!("drawtext={}", parts.join(":"))
}

/// The `drawtext` the still draws for `t`, which is what a plan's overlay stands for: the same
/// options, position and opacity as [`drawtext_still`], taken from the plan's own resolved fields
/// instead of the timeline's (a test holds the two equal).
pub fn text_still_filter(t: &crate::render_plan::PlanText, frame_h: u32) -> String {
    let mut parts = TextLook {
        text: &t.text,
        size: t.size,
        color: &t.color,
        bg: t.bg.as_deref(),
        font_file: t.font_file.as_deref(),
        border: t.synthetic_bold,
    }
    .options(f64::from(frame_h));
    parts.push(format!("x=(w*{}-text_w/2)", fnum(t.pos.0)));
    parts.push(format!("y=(h*{}-text_h/2)", fnum(t.pos.1)));
    if t.alpha < 1.0 {
        parts.push(format!("alpha={}", fnum(t.alpha)));
    }
    format!("drawtext={}", parts.join(":"))
}

/// One piece of an overlay's `drawtext`, drawn on its own: a string of it, in colours of the caller's
/// choosing. The compositor measures and rasterises text a piece at a time (the prefixes of a title, the
/// glyph units of its dictionary) and reads what `drawtext` makes of each; the options are the overlay's
/// own ([`TextLook`]: size, font, border, box) so a piece is laid out and drawn as the overlay is.
#[derive(Debug, Clone, Copy)]
pub struct TextPiece<'a> {
    /// What to draw (cleaned and escaped here, [`text_as_drawn`]).
    pub text: &'a str,
    /// Keep line feeds in `text` as the line breaks they are (one prefix per line), rather than blanks.
    pub lines: bool,
    /// The glyphs' colour.
    pub font: &'a str,
    /// The border's colour, drawn only when the overlay has a synthetic-bold border.
    pub border: Option<&'a str>,
    /// A box behind the text: its colour and its `boxborderw`.
    pub boxed: Option<(&'a str, u32)>,
}

/// The `drawtext` of `piece` in the look of the overlay `t` on a frame `frame_h` high, followed by
/// `extra` options (the position, `y_align`, ...): `drawtext=<options>`, to put in a graph.
pub fn text_piece_filter(t: &crate::render_plan::PlanText, frame_h: u32, piece: &TextPiece<'_>, extra: &[&str]) -> String {
    let fontsize = (f64::from(frame_h) * t.size).round().max(1.0);
    // The colours are spliced unquoted: every one goes through `safe_color`, which leaves a colour ffmpeg parses
    // as it is (the argv is the same) and cannot be made to carry an option of its own. The callers' are
    // constants and probed hex, so an invalid one is a bug: loud in a test, an invisible colour otherwise
    // (and the picture it makes is not the one `drawtext` draws, which the compositor's proof refuses).
    let colour = |c: &str| {
        debug_assert!(valid_color(c), "a text piece colour ffmpeg may not parse: {c:?}");
        safe_color(c, "white@0").to_string()
    };
    let mut parts = vec![
        format!("text='{}'", escape_drawtext_with(piece.text, true, piece.lines)),
        format!("fontsize={}", fnum(fontsize)),
        format!("fontcolor={}", colour(piece.font)),
    ];
    if let Some(path) = t.font_file.as_deref() {
        parts.push(format!("fontfile='{}'", escape_drawtext_path(&path.to_string_lossy())));
    }
    if let (true, Some(border)) = (t.synthetic_bold, piece.border) {
        parts.push(format!("borderw={TEXT_BORDER_W}"));
        parts.push(format!("bordercolor={}", colour(border)));
    }
    if let Some((box_colour, border)) = piece.boxed {
        parts.push("box=1".to_string());
        parts.push(format!("boxcolor={}", colour(box_colour)));
        parts.push(format!("boxborderw={border}"));
    }
    parts.extend(extra.iter().map(|e| (*e).to_string()));
    format!("drawtext={}", parts.join(":"))
}

/// The width of the border a synthetic-bold overlay draws, in pixels.
pub const TEXT_BORDER_PX: u32 = TEXT_BORDER_W;

/// The padding of the box behind an overlay's text, in pixels.
pub const TEXT_BOX_PX: u32 = TEXT_BOX_BORDER_W;

/// The parts [`text_raster_graph`] draws, in the order they are stacked in its one output picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextPart {
    /// The glyphs, as coverage.
    Glyphs,
    /// The box behind the text: 255 where it is.
    Box,
}

/// The `ffmpeg` graph that makes an overlay's coverage masks. The compositor draws text by
/// blending what FFmpeg's own `drawtext` rasterised (layout, hinting, shaping and sub-pixel
/// placement are FFmpeg's, on whatever build this machine has), so it cannot differ from the still.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRasterGraph {
    /// The `-filter_complex` text: its output `[o]` is the `parts` stacked top to bottom, each
    /// `size` big, as 8-bit gray (`rawvideo`).
    pub filter_complex: String,
    pub parts: Vec<TextPart>,
    pub size: (u32, u32),
}

/// The graph that rasterises `t` on a black `size` canvas, **at the position and in the pixel
/// format the still draws it**: the same `drawtext` options and the same `x` / `y` expressions
/// ([`drawtext_still`]), so every pixel of coverage is where the still puts it (glyph bitmaps are
/// rendered at the quarter-pixel phase of a fractional position, so the position is part of the
/// picture, not something to move afterwards).
///
/// Each part is drawn white on black in `rgb24` and its red plane kept: full coverage is 255 and
/// the blend of 255 over 0 at coverage `m` is exactly `m` (a gray or YUV canvas would write 235 on
/// an FFmpeg that ignores the range tag, as 6.1 does). What a part leaves out is drawn invisibly
/// (`@0`), never omitted, so `drawtext` clips to the region the still does: its clip rectangle
/// grows with the box and with the border. A synthetic-bold border is part of the glyphs' picture,
/// drawn white in the same pass `drawtext` draws it in; the compositor takes the picture of the passes
/// as the check of the bitmaps it blends one by one, not as the mask it blends.
pub fn text_raster_graph(t: &crate::render_plan::PlanText, size: (u32, u32)) -> TextRasterGraph {
    let (w, h) = size;
    let look = TextLook {
        text: &t.text,
        size: t.size,
        color: "white",
        bg: t.bg.as_deref(),
        font_file: t.font_file.as_deref(),
        border: t.synthetic_bold,
    };
    let at = format!("x=(w*{}-text_w/2):y=(h*{}-text_h/2)", fnum(t.pos.0), fnum(t.pos.1));
    let mut parts = vec![TextPart::Glyphs];
    if t.bg.is_some() {
        parts.push(TextPart::Box);
    }
    const SHOWN: &str = "white";
    const HIDDEN: &str = "white@0";
    let draws: Vec<String> = parts
        .iter()
        .map(|part| {
            let (font, border, boxed) = match part {
                TextPart::Glyphs => (SHOWN, SHOWN, HIDDEN),
                TextPart::Box => (HIDDEN, HIDDEN, SHOWN),
            };
            let options = look.options_coloured(f64::from(h), font, border, Some(boxed));
            format!("drawtext={}:{at},extractplanes=r", options.join(":"))
        })
        .collect();
    let base = format!("color=c=black:s={w}x{h}:r=1:d=1,format=rgb24");
    let filter_complex = if let [only] = draws.as_slice() {
        format!("{base},{only}[o]")
    } else {
        let n = draws.len();
        let outs: String = (0..n).map(|i| format!("[a{i}]")).collect();
        let branches: Vec<String> = draws.iter().enumerate().map(|(i, d)| format!("[a{i}]{d}[p{i}]")).collect();
        let stacked: String = (0..n).map(|i| format!("[p{i}]")).collect();
        format!("{base},split={n}{outs};{};{stacked}vstack=inputs={n}[o]", branches.join(";"))
    };
    TextRasterGraph {
        filter_complex,
        parts,
        size,
    }
}

/// `v360` resampling used for export (quality) and for stills / previews (speed).
/// The measured difference in reprojection cost between these is small; the
/// export one buys sharper edges on a wide reframe.
const EXPORT_INTERP: &str = "cubic";
const PREVIEW_INTERP: &str = "line";

/// How far a reframe channel must drift from its last emitted value before it is
/// worth another `sendcmd`, in degrees. Every command makes `v360` rebuild its
/// remap LUT, so this gate is what keeps a held camera from paying per frame.
const REFRAME_CMD_TOLERANCE: f64 = 0.05;

/// The `v360` filter realizing one sampled reframe at output size `w`x`h`.
///
/// `instance` names the filter (`v360@c3`) so a `sendcmd` can target this clip's
/// instance and no other — several reframed clips coexist in one graph. The
/// still path passes `None`: it has no commands to send, so it needs no name.
fn reframe_filter(r: &ResolvedReframe, w: u32, h: u32, interp: &str, instance: Option<&str>) -> String {
    let name = match instance {
        Some(id) => format!("v360@{id}"),
        None => "v360".to_string(),
    };
    // An equirect output must stay 2:1 or the sphere is squashed; size it to the
    // frame width and let the caller's fit/pad letterbox it. A flat output is
    // rendered straight at the frame size, which both skips an oversized
    // intermediate (an 8K source never materializes at 8K) and makes the fit
    // `scale` that follows a no-op.
    let (ow, oh) = match r.output {
        Projection::Equirect => (w, ((w / 2).max(2)) & !1),
        _ => (w, h),
    };
    let mut opts = vec![
        format!("input={}", r.input.v360_name()),
        format!("output={}", r.output.v360_name()),
    ];
    // The lens field of view only describes a physical fisheye; it means nothing
    // for an equirect source, which is already unwrapped.
    if r.input.is_fisheye() {
        opts.push(format!("ih_fov={}", fnum(r.lens_fov)));
        opts.push(format!("iv_fov={}", fnum(r.lens_fov)));
    }
    opts.push(format!("w={ow}"));
    opts.push(format!("h={oh}"));
    opts.push(format!("interp={interp}"));
    opts.push(format!("yaw={}", fnum(r.yaw)));
    opts.push(format!("pitch={}", fnum(r.pitch)));
    opts.push(format!("roll={}", fnum(r.roll)));
    // `d_fov` derives an aspect-correct horizontal/vertical pair on its own,
    // unlike `h_fov`, which needs `v_fov` set in lockstep or the picture
    // stretches. It is meaningless for an equirect output, which always covers
    // the whole sphere.
    if r.output == Projection::Flat {
        opts.push(format!("d_fov={}", fnum(r.fov)));
    }
    format!("{name}={}", opts.join(":"))
}

/// The `sendcmd` command list that drives `target`'s `v360` across an animated
/// clip, or `None` when the camera holds still — a static reframe bakes its pose
/// into the filter's own arguments and costs nothing extra.
///
/// This is deliberately stingy, because each command makes `v360` re-run
/// `config_output` and rebuild its remap LUT (~32 ms per command at 1080p, linear
/// in both command count and output pixels). Two economies do the work: channels
/// that never move are left as static arguments and never appear here at all, and
/// a channel re-emits only once it has drifted past [`REFRAME_CMD_TOLERANCE`],
/// which collapses a long hold to a single command.
fn reframe_commands(clip: &Clip, rf: &Reframe, target: &str, fps: f64, dur: f64) -> Option<String> {
    if !rf.is_animated() || !fps.is_finite() || fps <= 0.0 || !dur.is_finite() || dur <= 0.0 {
        return None;
    }
    let k = rf.sorted_keyframes();
    let moves = |get: fn(&ReframeKeyframe) -> f64| {
        let first = get(&k[0]);
        k.iter().any(|kf| get(kf) != first)
    };
    // (v360 option name, how to read it off a sampled pose, whether it moves)
    type Channel = (&'static str, fn(&ResolvedReframe) -> f64, bool);
    let channels: [Channel; 4] = [
        ("yaw", |r| r.yaw, moves(|kf| kf.yaw)),
        ("pitch", |r| r.pitch, moves(|kf| kf.pitch)),
        ("roll", |r| r.roll, moves(|kf| kf.roll)),
        ("d_fov", |r| r.fov, moves(|kf| kf.fov)),
    ];
    if !channels.iter().any(|(_, _, moving)| *moving) {
        return None;
    }

    let frames = (dur * fps).ceil().max(0.0) as u64;
    let mut cmds: Vec<String> = Vec::new();
    let mut last: [Option<f64>; 4] = [None; 4];
    for i in 0..=frames {
        let local = i as f64 / fps;
        let pose = rf.sample(local);
        // Fire half a frame early so the command is already pending when frame
        // `i` reaches `v360` and cannot be consumed by frame `i-1`. Without the
        // lead, float drift at rates like 29.97 lands commands a frame late.
        let at = (clip.timeline_start + local - 0.5 / fps).max(0.0);
        for (n, (name, get, moving)) in channels.iter().enumerate() {
            if !*moving {
                continue;
            }
            let v = get(&pose);
            if last[n].is_some_and(|prev| (v - prev).abs() < REFRAME_CMD_TOLERANCE) {
                continue;
            }
            last[n] = Some(v);
            // `{:.4}` rather than `fnum`, whose `{}` formatting can spell a
            // rounded value `0.30000000000000004` and triple the graph's size.
            cmds.push(format!("{at:.5} {target} {name} {v:.4}"));
        }
    }
    (!cmds.is_empty()).then(|| cmds.join(";"))
}

/// One `fade` of a clip's picture as ffmpeg spells it. For a renderer's probe, which runs the very text.
pub fn fade_filter(step: &FadeStep) -> String {
    let edge = match step.edge {
        FadeEdge::In => "in",
        FadeEdge::Out => "out",
    };
    let tint = match step.tint {
        FadeTint::Black => "",
        FadeTint::White => ":c=white",
        FadeTint::Alpha => ":alpha=1",
    };
    format!("fade=t={edge}:st={}:d={}{tint}", step.st, step.d)
}

/// The pad that letterboxes a picture fitted inside a `w`x`h` frame: opaque black, or transparent when `clear`
/// (a blended clip's, [`keeps_clear_bars`]).
fn letterbox_pad(w: u32, h: u32, clear: bool) -> String {
    if clear {
        format!("format=yuva420p,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0")
    } else {
        format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2")
    }
}

/// A constant rotation by `degrees`. A blended clip's corners are filled with transparent black (`blended`):
/// `none` leaves them as the zeroed buffer was allocated, with chroma 0, and the 2x2 chroma blocks across the
/// picture's edge carry it into the RGB the blend works in (33 levels of luma off the plain overlay at that edge).
fn static_rotate(degrees: f64, blended: bool) -> String {
    let rad = degrees.to_radians();
    let fill = if blended { "black@0" } else { "none" };
    format!("rotate={rad}:fillcolor={fill}:ow=rotw({rad}):oh=roth({rad})")
}

/// The [`Fit::Blur`] fit of a picture into a `w`x`h` frame (pure): split in two, the
/// picture scaled to fit (`decrease`, with `even` and the delivery's scaler flags `sf`, as
/// the Contain fit) over a copy scaled to cover a thumbnail an eighth of the frame on each
/// side (even, at least 4x4), cropped to it, box-blurred twice and scaled back up — a blur of
/// a few dozen pixels, at the cost of one on a thumbnail. `label` names the chain's inner pads,
/// unique within its graph. Its text joins a chain like a filter:
/// `...,split[..];...;[..][..]overlay=...,...`. Which clips take it is [`takes_blur_fill`]'s.
///
/// The `overlay` composes in 8-bit 4:2:0 (its default) unless `deep`: an HDR picture, fitted ahead
/// of its tone-map, composes in 10 bits (`format=yuv420p10`, FFmpeg 4.4 on) so the fill does not
/// band it; an SDR one keeps the default.
fn blur_fill(w: u32, h: u32, even: &str, sf: &str, deep: bool, label: &str) -> String {
    let (bw, bh) = ((w / 16 * 2).max(4), (h / 16 * 2).max(4));
    // A radius above half the chroma plane is refused; a sixteenth of the thumbnail's smaller
    // side, whole pixels (so the strength is only roughly the same from one frame size to another).
    let radius = (bw.min(bh) / 16).max(1);
    format!(
        "split[{label}b][{label}f];\
         [{label}b]scale={bw}:{bh}:force_original_aspect_ratio=increase:flags=bilinear,crop={bw}:{bh},\
         boxblur={radius}:2,scale={w}:{h}:flags=bilinear[{label}bb];\
         [{label}f]scale={w}:{h}:force_original_aspect_ratio=decrease{even}{sf}[{label}ff];\
         [{label}bb][{label}ff]overlay=(W-w)/2:(H-h)/2{format}",
        format = if deep { ":format=yuv420p10" } else { "" }
    )
}

/// Whether `clip` takes the [`Fit::Blur`] fill in a `frame`-sized delivery (pure), its picture
/// `picture` (the probed, displayed size) and `alpha` whether it carries an alpha channel. Only a
/// picture placed by the plain fit that leaves bars: not one moved, scaled, rotated, cropped or
/// animated by its transform, not a 360 reframe (which draws the frame itself), not one that is
/// see-through (an alpha source, a chroma key, a mask: the fill would show through it as a
/// blurred halo) and not one of the frame's own shape or of an unknown one (nothing to fill).
/// Anything else is placed with Contain's own chain, byte for byte.
fn takes_blur_fill(clip: &Clip, picture: Option<(u32, u32)>, frame: (u32, u32), alpha: bool) -> bool {
    blur_fill_clip(clip, alpha) && picture_has_other_shape(picture, frame)
}

/// The part of [`takes_blur_fill`] that is the clip's (pure, shared with the plan): placed by the plain fit, not
/// see-through.
pub(crate) fn blur_fill_clip(clip: &Clip, alpha: bool) -> bool {
    clip.transform.is_identity()
        && !clip.is_animated()
        && clip.reframe.is_none()
        && clip.mask.is_none()
        && !alpha
        && !clip.effects.iter().any(|e| e.produces_alpha())
}

/// Whether a probed `picture` is not the shape of the `frame` (an unprobed one is nothing to fill).
pub(crate) fn picture_has_other_shape(picture: Option<(u32, u32)>, frame: (u32, u32)) -> bool {
    picture.is_some_and(|(w, h)| w > 0 && h > 0 && u64::from(w) * u64::from(frame.1) != u64::from(h) * u64::from(frame.0))
}

/// The displayed size of `asset`'s picture, when it was probed.
fn picture_size(asset: Option<&Asset>) -> Option<(u32, u32)> {
    let stream = asset?.streams.iter().find(|s| s.kind == StreamKind::Video)?;
    Some((stream.width?, stream.height?))
}

/// How many pictures of a still the export reads: those `-loop 1 -framerate fps -t end` gave, the
/// ones before `end` on the `fps` grid.
fn still_pictures(end: f64, fps: f64) -> u64 {
    ((end.max(1.0 / fps) * fps - 1e-9).ceil() as u64).max(1)
}

/// The [`StillMotion`] plan of an export's clip (`None` for any clip it does not draw): a moving
/// still image judged at the delivery frame on every frame it is drawn on (its length and the tail
/// a transition keeps under the next clip), the curves' own points included.
fn still_motion_for(clip: &Clip, assets: &[Asset], fmt: &ExportFormat, fx: &ClipFx) -> Option<StillMotion> {
    let asset = assets.iter().find(|a| a.id == clip.asset_id);
    if !asset.is_some_and(Asset::is_image) || clip.is_generated() || fx.hdr.is_some() || !clip.picture_moves() {
        return None;
    }
    let timing = ClipTiming::new(clip, fx);
    let length = timing.duration().max(0.0);
    let step = 1.0 / fmt.fps;
    // Every frame, or 4000 evenly over a very long clip (a polyline's extremes are its points). A
    // behaviour's extremes are between the points, so there every frame is judged, or the clip is
    // left to the general chain.
    let frames = (length / step).ceil() as usize;
    let cap = if clip.has_behaviors() { 20_000 } else { 4000 };
    if frames > cap && clip.has_behaviors() {
        return None;
    }
    let count = frames.clamp(1, cap);
    let mut times: Vec<f64> = if clip.has_behaviors() {
        // The frames the export evaluates the pose at: output frame `slot + k` of the delivery's own
        // grid (`each_frame`), `slot` the one `fps` pins the clip's first picture to, at the clip's
        // clock (`t - timeline_start`) — not `length / count` apart from the clip's first instant, which
        // for a clip that does not start on a frame reads a fast shake's peaks a fraction of a frame off.
        let (_, rate, slot) = keyed_crop_fps(fmt.fps, clip.timeline_start + timing.picture_delay());
        let at = |k: usize| (slot + k as i64) as f64 * rate.den as f64 / rate.num as f64 - clip.timeline_start;
        (0..=count + 1).map(at).chain([0.0, length]).collect()
    } else {
        (0..=count).map(|k| length * k as f64 / count as f64).collect()
    };
    for p in Property::TRANSFORM
        .iter()
        .chain(&[Property::CropZoom, Property::CropX, Property::CropY])
    {
        times.extend(
            clip.property_curve(*p)
                .iter()
                .map(|pt| pt.0)
                .filter(|t| (0.0..=length).contains(t)),
        );
    }
    let frame = (fmt.width, fmt.height);
    StillMotion::new(clip, still_motion::displayed(asset), frame, frame, fmt.fit, &times)
}

/// The `-filter_complex` graph of the export of `timeline` as the engine builds it ([`build_export_args`]), and the
/// frame rate of its image inputs: what a compositor's probe runs on a toy cut, so that it measures the graph the export
/// really holds and cannot drift from it. `None` for a cut whose graph is not a single `-filter_complex` argument.
pub fn export_graph(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Result<Option<String>> {
    let args = build_export_args(timeline, assets, "probe.mkv", opts)?;
    Ok(graph_of(&args))
}

/// [`export_graph`] for the FFmpeg still at timeline time `t` ([`build_still_args`], at the delivery's own size).
pub fn still_graph(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions, t: f64) -> Result<Option<String>> {
    let out = StillOutput::File {
        path: "probe.png".to_string(),
        format: ImageFormat::Png,
        quality: 0,
    };
    let args = build_still_args(timeline, assets, opts, t, u32::MAX, None, &out)?;
    Ok(graph_of(&args))
}

/// The argv of the export of `timeline` to `output_path` as the engine builds it ([`build_export_args`]: pure, before the thread caps and the progress
/// flags [`render_with`] adds), for a test that runs the export's own graph with another sink than the encoder (the planes it is handed).
#[doc(hidden)]
pub fn export_args(timeline: &Timeline, assets: &[Asset], output_path: &str, opts: &ExportOptions) -> Result<Vec<String>> {
    build_export_args(timeline, assets, output_path, opts)
}

fn graph_of(args: &[String]) -> Option<String> {
    let at = args.iter().position(|a| a == "-filter_complex")?;
    args.get(at + 1).cloned()
}

/// The delivery frame a plan judges the clips' moving stills by: [`still_motion_for`] over the cut's own export
/// format, held so each clip is judged against the same one.
pub(crate) struct StillRoutes {
    fmt: ExportFormat,
}

impl StillRoutes {
    /// The export format of `timeline` under `opts`.
    pub(crate) fn new(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Self {
        Self {
            fmt: export_format(timeline, assets, opts),
        }
    }

    /// The route of `clip` if the export takes its moving still through [`StillMotion`] (the scaled copy and one
    /// `perspective`): the static part, the plan over every frame it is drawn on, and the delivery's scaler flags.
    pub(crate) fn of(
        &self,
        clip: &Clip,
        assets: &[Asset],
        fx: &ClipFx,
    ) -> Option<(crate::still_motion::StillStatic, StillMotion, String)> {
        let motion = still_motion_for(clip, assets, &self.fmt, fx)?;
        let asset = assets.iter().find(|a| a.id == clip.asset_id);
        let stat = crate::still_motion::StillStatic::of_clip(
            clip,
            still_motion::displayed(asset),
            (self.fmt.width, self.fmt.height),
            self.fmt.fit,
        )?;
        Some((stat, motion, self.fmt.scale_flags()))
    }
}

/// The video filter chain for one clip (everything between its `[i:v]` input
/// and its `[v{i}]` output): trim, optional reverse / crop / retime, 360
/// reprojection, fit or transform geometry, color correction, per-clip video
/// effects, keyframe animation, fades and transition alpha. With all properties
/// at their defaults this reduces to the original fit-and-letterbox chain.
///
/// **The retime is one `setpts`**: `PTS-STARTPTS` over the speed, plus the clip's start, or for a
/// speed ramp the quoted expression of `ramp::setpts` (the map from the frame's source seconds to
/// its place on the clip). Everything animated after it reads the timeline clock either way.
///
/// **A keyframed zoom is the one stage that changes the picture's size from frame
/// to frame**, and almost nothing downstream of it can follow: `format`
/// negotiation inserts a fixed-size converter, `eq` / `geq` / `rotate` /
/// `gblur` read the frame size once when the graph is configured, and a filter
/// that does is pinned to the *first* frame's size for the whole clip (a zoom
/// that never showed, an opacity ramp or rotation that stayed at the first
/// frame's geometry). So when [`Clip::zoom_animated`] the zoom `scale` is the
/// **last** stage of the chain — everything else runs at the constant fit size —
/// and it sits after `fps`, so it is evaluated at the *output* frame's time
/// rather than at the source frame's (a 10 fps clip in a 30 fps export zoomed in
/// three-frame steps). Only `overlay` follows, and it reads each picture's own
/// size every frame (`(W-w)/2` centres it as it grows). The chain then ends in
/// `yuva420p`, the format `overlay` takes natively, so no converter is inserted
/// between them. Every other clip keeps the chain it always had.
///
/// `instance` is the clip's unique flat index, used to name its `v360` so
/// `sendcmd` can address it and the inner pads of a [`Fit::Blur`] fill; it is unused
/// for clips that do neither. A `fmt` whose fit is `Blur` gives the clip the fill: the caller
/// passes Contain for a clip that does not take it ([`takes_blur_fill`]).
#[cfg(test)]
fn video_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, is_image: bool, instance: &str) -> String {
    video_clip_chain_stamped(clip, fmt, fx, is_image, None, None, None, None, instance)
}

/// [`video_clip_chain`] for a picture of a known size (`None`: not known, which a Cover clip's
/// chain reads as "the cover may hang over the frame").
#[cfg(test)]
fn video_clip_chain_of(
    clip: &Clip,
    fmt: &ExportFormat,
    fx: &ClipFx,
    is_image: bool,
    picture: Option<(u32, u32)>,
    instance: &str,
) -> String {
    video_clip_chain_stamped(clip, fmt, fx, is_image, None, picture, None, None, instance)
}

/// [`video_clip_chain`] for a clip whose input may be a title's pictures: `stamp` puts them on the
/// canvas' grid in place of the `trim` and `setpts` a decoded clip starts with (they are drawn for
/// exactly the frames the clip is on, tail included); everything after is the same chain. `picture`
/// is the displayed size of the clip's footage ([`picture_size`]) when it is known: under `Cover`
/// it decides whether the cover of the frame has anything past the frame to show. `stab` is what
/// steadies a stabilized clip ([`stabilizer`]): its frames are moved straight after the trim, which
/// vid.stab's starts between two frames of its analysis.
#[allow(clippy::too_many_arguments)]
fn video_clip_chain_stamped(
    clip: &Clip,
    fmt: &ExportFormat,
    fx: &ClipFx,
    is_image: bool,
    stamp: Option<svg_layer::ClipStamp>,
    picture: Option<(u32, u32)>,
    stab: Option<&Stabilizer>,
    still_motion: Option<&StillMotion>,
    instance: &str,
) -> String {
    let s = clip.speed_mag();
    let t = &clip.transform;
    let anim = clip.is_animated();
    // Which animated channels actually move (so alpha / per-frame geometry is only
    // forced when needed). A clip animated through the legacy bundle has every number of its
    // transform keyed (a fresh keyframe captures the static transform), so `anim` alone drove
    // the geometry; a clip with per-property channels keys only some, and the rest are built
    // from its static transform as an unkeyed clip's are. Rotation / opacity additionally
    // need an alpha plane.
    // (A live behaviour makes a number an expression of time as a key does: `Clip::is_driven`.)
    let scale_keyed = clip.is_driven(Property::Scale);
    let rotation_keyed = clip.is_driven(Property::Rotation);
    let opacity_keyed = clip.is_keyed(Property::Opacity);
    let anim_rotation = clip.rotation_turns();
    let anim_opacity = clip.property_keys(Property::Opacity).iter().any(|k| k.value < 1.0);
    // A zoom that really moves: the picture changes size per frame, so its `scale`
    // goes last (see above). A keyed clip whose scale holds still keeps the chain
    // it always had — its picture never changes size, so nothing can be pinned.
    let zoom_keyed = clip.zoom_animated();
    // A zoom or a position that moves is placed on the canvas by the chain's last stage, at
    // sub-pixel positions (`placement_filters`), which counts its frames from a pinned `fps`.
    // (A moving still is drawn into the frame by `still_motion` instead.)
    let placed = clip.placement_animated() && still_motion.is_none();
    let chroma = clip.effects.iter().any(|e| e.produces_alpha());
    // Alpha is needed for static opacity/rotation, animated opacity/rotation, a
    // chroma key, or a crossfade dissolve.
    let transform_alpha =
        (!rotation_keyed && t.rotation != 0.0) || (!opacity_keyed && t.opacity < 1.0) || anim_rotation || anim_opacity || chroma;
    let timing = ClipTiming::new(clip, fx);
    // A blended clip's fades are alpha fades (`ClipTiming::fades`).
    let needs_alpha =
        transform_alpha || fx.xfade_in > 0.0 || clip.mask.is_some() || timing.fades().iter().any(|s| s.tint == FadeTint::Alpha);
    let dur = timing.duration();
    // A crossfade tail borrows unused source: forward clips extend past source_out,
    // reversed clips extend below source_in (reverse plays high->low, so the visible
    // tail is at the low end).
    // The picture reads on past the window (see `picture_window`); `cut` ends its stream at the
    // clip's end once `fps` has put it on the output grid.
    let (trim_start, trim_end) = picture_window(clip, fx, fmt.fps);
    let cut = Rational::from_fps_filter(fmt.fps).map(|rate| format!("trim=end_pts={}", end_frame(timing.window().1, rate)));
    // Relative to the input-side `-ss` (see `build_export_args_phase`): when the
    // window is fast-seeked the trim starts at 0, otherwise it is unchanged. A still
    // image is never seeked (it is `-loop`ed from t=0), so its trim stays absolute.
    let seek = if is_image {
        0.0
    } else {
        clip_input_seek(clip, fx, trim_start)
    };

    let reframe = clip.reframe.as_ref();
    let crop = t.has_crop().then(|| {
        let cw = (1.0 - t.crop_left - t.crop_right).max(0.0);
        let ch = (1.0 - t.crop_top - t.crop_bottom).max(0.0);
        format!(
            "crop=w=iw*{cw}:h=ih*{ch}:x=iw*{cl}:y=ih*{ct}",
            cl = t.crop_left,
            ct = t.crop_top
        )
    });
    // A keyed crop is a per-frame `geq` after the static one, on the timeline clock (after
    // `setpts`), and after `fps` so it draws only the frames the export keeps.
    let crop_keyed = clip.crop_animated();

    let mut p: Vec<String> = Vec::new();
    // The label is the first filter of the chain, so that all of it reads what a file tagged BT.709
    // would give. Later, a scaler may have converted the picture already: the fit `scale` is the
    // filter FFmpeg has do the RGB conversion a dip to white's `fade` needs, as BT.601 in and
    // BT.709 out (8 to 15 levels off the footage tagged in the file).
    if fx.label_bt709 {
        p.push(BT709_LABEL.to_string());
    }
    // A still's one decoded picture is held for as many frames as `-loop 1 -t` used to decode,
    // or as a part that picks it up at a cut keeps; a moving still's is scaled and cut once first
    // (`still_motion`).
    if is_image {
        if let Some(sm) = still_motion {
            p.push(sm.once(&fmt.scale_flags()));
        }
        let pictures = match stamp {
            Some(stamp) => stamp.held.unwrap_or(1),
            None => still_pictures(trim_end, fmt.fps),
        };
        p.push(format!("loop=loop={}:size=1", pictures.max(1) - 1));
    }
    // A head-padded proxy opens with one clone of its first frame at time zero (see
    // [`build_proxy_args`]), which a seek skips — and a read from the start does
    // not. The original has no such frame: its first one is the real one, and the
    // `setpts` below puts *that* at the clip's start. Dropping the clone first makes
    // the proxy do the same, so a clip cut from the very head of a late-starting
    // source plays the frames the export renders instead of `lead` seconds of the
    // first frame held. Only a padded proxy gets the filter; every other input's
    // chain is exactly what it was.
    if let Some(stamp) = stamp {
        p.push(stamp.filters());
    } else {
        if fx.head_pad && !is_image && seek == 0.0 {
            p.push("trim=start_frame=1".to_string());
        }
        // A stabilized clip's correction is per source frame, so it goes before the frames are reversed
        // or retimed; vid.stab's trim opens on the first frame its corrections are for. An HDR clip is
        // tone-mapped first, as a still and an SDR proxy are, so every graph steadies the same picture.
        let start = stab.and_then(Stabilizer::trim_start).unwrap_or(trim_start);
        p.push(format!("trim=start={}:end={}", start - seek, trim_end - seek));
        if let Some(hdr) = fx.hdr.filter(|_| stab.is_some()) {
            p.push(tonemap_filter(hdr));
        }
        p.extend(stab.map(Stabilizer::filters));
        // (A moving still's frames are one picture: reversing them changes nothing but the memory.)
        if clip.is_reversed() && still_motion.is_none() {
            p.push("reverse".to_string());
        }
    }
    // A reframed clip crops *after* reprojection instead: edge fractions of a raw
    // dual-fisheye frame mean nothing, and the user set them against the flat
    // picture they were looking at. `crop` reads no timestamps (its `iw`/`ih`
    // fractions are constants), so moving it past `setpts` is safe.
    if reframe.is_none() && still_motion.is_none() {
        p.extend(crop.clone());
    }
    // A keyed crop's `fps` pins its first frame where `setpts` puts it (see `keyed_crop_fps`).
    let (crop_fps, crop_rate, crop_first) = keyed_crop_fps(fmt.fps, clip.timeline_start + timing.picture_delay());
    if stamp.is_none() {
        // `setpts` puts the first frame kept at the clip's start; a window that starts inside a
        // late-starting source's lead keeps no frame until the source's first picture, which
        // plays `picture_delay` into the clip, in sync with the sound beside it.
        let start = match timing.picture_delay() {
            d if d > 0.0 => format!("({}+{d})", clip.timeline_start),
            _ => clip.timeline_start.to_string(),
        };
        if let Some(retime) = ramp::setpts(clip, timing.ramp_offset(), clip.timeline_start) {
            // A ramp's map takes the source seconds the frame is in to its place on the clip's
            // timeline, the delay of a late first picture included (`ramp_offset`).
            p.push(retime);
        } else if (s - 1.0).abs() < 1e-9 {
            p.push(format!("setpts=PTS-STARTPTS+{start}/TB"));
        } else {
            p.push(format!("setpts=(PTS-STARTPTS)/{s}+{start}/TB"));
        }
    }
    if let Some(rf) = reframe {
        // Hoisted above `v360` so a 50 fps source exporting at 30 reprojects 30
        // frames a second rather than reprojecting 50 and discarding 20 — each
        // of which would have rebuilt the remap LUT.
        if crop_keyed || placed {
            p.push(crop_fps.clone());
        } else {
            p.push(format!("fps={}", fmt.fps));
        }
        p.extend(cut.clone());
        // `sendcmd` sits upstream of `v360`: a command takes effect as a frame
        // passes through, so downstream it would land one frame late. Both sit
        // after `setpts`, which puts the timestamps `sendcmd` matches on the
        // timeline clock — so a keyframe at clip-local `t` is a command at
        // `timeline_start + t`, and `speed` and `reverse` are already folded in.
        let target = format!("v360@{instance}");
        if let Some(cmds) = reframe_commands(clip, rf, &target, fmt.fps, dur) {
            p.push(format!("sendcmd=c='{cmds}'"));
        }
        p.push(reframe_filter(
            &rf.pose(),
            fmt.width,
            fmt.height,
            EXPORT_INTERP,
            Some(instance),
        ));
        p.extend(crop);
    }
    if crop_keyed || still_motion.is_some() {
        if reframe.is_none() {
            p.push(crop_fps.clone());
            p.extend(cut.clone());
        }
        // A moving still's whole geometry, its static crop and fit included, is one `perspective`.
        match still_motion {
            Some(sm) => p.push(sm.each_frame(clip, crop_rate, crop_first)),
            None => p.push(crop_remap_filter(clip, crop_rate, crop_first)),
        }
    }
    let sf = fmt.scale_flags();
    // `zscale` (the tone-map, which follows the geometry) refuses a picture whose size is
    // not a multiple of its chroma subsampling, and a Contain fit of 4:3 footage into a
    // 9:16 frame is 405 rows high. Every size ahead of it is made even for HDR footage.
    let tone_mapped = fx.hdr.is_some();
    // A keyframed clip is treated as non-identity so its picture is centered by
    // the overlay (not padded full-frame), and its zoom is re-evaluated per frame.
    let geom_identity = t.is_identity() && !anim;
    let grade = grade_filters(&clip.grade, composite_matrix(fx.label_bt709, fx.frame_matrix));
    // A blended clip's letterbox bars are transparent, so they leave the lower tracks alone.
    let clear_bars = fmt.fit == Fit::Contain
        && geom_identity
        && keeps_clear_bars(!clip.blend_mode.is_normal(), tone_mapped, &clip.effects, &grade);
    let needs_alpha = needs_alpha || clear_bars;
    // A reprojected picture and a title's pictures come out at the frame's own size: nothing hangs over it.
    // (A title is the clip that has no asset: a still an export in parts picks up at a cut is stamped too,
    // and keeps its own picture, whose size decides the order as it does in the whole export.)
    let picture = if reframe.is_some() || clip.is_generated() {
        Some((fmt.width, fmt.height))
    } else {
        picture
    };
    // Whether the clip's zoom is part of the fit (one resample from the source), or follows it.
    let order = clip.fit_order(
        fmt.fit,
        crate::layer_geometry::cover_overflows(picture, (fmt.width, fmt.height), t),
    );
    let held = clip.held_scale().unwrap_or(1.0);
    match fmt.fit {
        // A moving still comes out of its `perspective` fitted, zoomed and at the frame's size.
        _ if still_motion.is_some() => {}
        // The whole picture over a blurred copy of itself that covers the frame: a
        // full-size canvas, as the padded identity case is.
        Fit::Blur if geom_identity => p.push(blur_fill(
            fmt.width,
            fmt.height,
            if tone_mapped { ":force_divisible_by=2" } else { "" },
            &sf,
            tone_mapped,
            &format!("bf{instance}"),
        )),
        // The zoom in is the fit's own: the frame times the zoom is the box the picture is fitted
        // in, so it is taken from the source once rather than fitted to the frame and enlarged.
        Fit::Contain | Fit::Blur if order == FitOrder::Folded => {
            p.push(fit_scale(fmt.width, fmt.height, held, "decrease", tone_mapped, &sf));
        }
        // Fill the frame times the zoom and keep all of it: the overlay cuts what hangs over the
        // canvas, so a position pans across the picture (cropped to the frame first, there is
        // nothing past it to pan to). What no frame of the clip can show is cut here, round the
        // centre, so the overlay is not handed the whole cover of a wide shot in a tall frame.
        Fit::Cover if order == FitOrder::Uncropped => {
            p.push(fit_scale(fmt.width, fmt.height, held, "increase", tone_mapped, &sf));
            let rotation = if anim_rotation {
                Rotated::Keyed
            } else if !rotation_keyed && t.rotation != 0.0 {
                Rotated::By(t.rotation.to_radians())
            } else {
                Rotated::No
            };
            // (A drift keeps moving while the clip plays on under a transition: judge its whole window.)
            let (mut px, mut py) = clip.pan_extent_over(timing.window().1 - clip.timeline_start);
            if let Some(travel) = timing.motion_keys() {
                let reach = |pts: &[(f64, f64)]| pts.iter().fold(0.0_f64, |m, p| m.max(p.1.abs()));
                px += reach(&travel.x);
                py += reach(&travel.y);
            }
            // A blur reads about three sigma past a pixel, so the window keeps that much margin.
            let sigma = clip
                .effects
                .iter()
                .filter_map(|e| match e {
                    VideoEffect::Blur { sigma } if sigma.is_finite() => Some(sigma.clamp(0.0, 4096.0)),
                    _ => None,
                })
                .fold(0.0_f64, f64::max);
            p.push(bound_crop(
                (fmt.width, fmt.height),
                (px, py),
                rotation,
                8.0 + (3.0 * sigma).ceil(),
            ));
        }
        // Fit inside the frame; the identity case then pads out to full size so
        // the overlay lands on a complete canvas.
        Fit::Contain | Fit::Blur => {
            p.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease{even}{sf}",
                w = fmt.width,
                h = fmt.height,
                even = if tone_mapped { ":force_divisible_by=2" } else { "" }
            ));
            if geom_identity {
                p.push(letterbox_pad(fmt.width, fmt.height, clear_bars));
            }
        }
        // Fill the frame and cut the overflow, so 16:9 footage delivered at 9:16
        // is a usable vertical shot rather than a strip of picture in a black
        // field. `increase` overshoots on one axis; the crop takes the centre.
        Fit::Cover => {
            p.push(format!(
                "scale={w}:{h}:force_original_aspect_ratio=increase{sf}",
                w = fmt.width,
                h = fmt.height
            ));
            p.push(format!("crop={w}:{h}", w = fmt.width, h = fmt.height));
        }
    }
    // A transformed clip's own zoom rides on top of that base fit.
    if !geom_identity && order == FitOrder::Fitted && still_motion.is_none() {
        if scale_keyed {
            // A keyed zoom that holds still: re-evaluated every frame, at one size. One that
            // moves is the placement's, at the end of the chain.
            if !zoom_keyed {
                let expr = driven_expr(clip, Property::Scale, "t");
                let tiny = clip.property_keys(Property::Scale).iter().any(|k| k.value < TINY_SCALE);
                p.push(zoom_scale(&expr, true, tiny, tone_mapped, &sf));
            }
        } else if (t.scale - 1.0).abs() > 1e-9 {
            p.push(zoom_scale(
                &t.scale.to_string(),
                false,
                t.scale < TINY_SCALE,
                tone_mapped,
                &sf,
            ));
        }
    }
    p.push("setsar=1".to_string());
    if reframe.is_none() && !crop_keyed && still_motion.is_none() {
        if placed {
            p.push(crop_fps);
        } else {
            p.push(format!("fps={}", fmt.fps));
        }
        p.extend(cut);
    }
    // HDR → SDR, once per clip, as late as the geometry allows: after the fit
    // scale has cut a 4K frame to the delivery size and `fps` has dropped the
    // frames that will never be shown, because the float RGB stage is by far the
    // most expensive step in the chain — and before any colour work, which is
    // meant to act on the SDR picture.
    // (A stabilized clip was tone-mapped before it was steadied, above.)
    if let Some(hdr) = fx.hdr.filter(|_| stab.is_none() || stamp.is_some()) {
        p.push(tonemap_filter(hdr));
    }
    // Color correction must run BEFORE any alpha plane is established: ffmpeg's `eq`
    // has no alpha-capable input format, so the graph would otherwise auto-insert a
    // conversion that drops the alpha (silently disabling opacity / rotation). A blended clip's transparent
    // letterbox bars are the exception: measured on 6.1 and 9.0, the alpha the pad gives survives `eq`, the
    // grade's RGB filters and every effect but `vignette` (`keeps_clear_bars`).
    if clip.color_animated() {
        p.push(eq_filter_keyed(clip));
    } else if !clip.color.is_identity() {
        p.push(eq_filter(&clip.color));
    }
    // The LUT, the curves and the wheels, after the `eq` (see `grade`), before any alpha plane.
    p.extend(grade);
    // Color-space video effects (blur / sharpen / grayscale / invert / vignette),
    // applied in author order, before any alpha plane.
    for e in &clip.effects {
        if let Some(f) = video_effect_filter(e) {
            p.push(f);
        }
    }
    // Establish alpha once, before any alpha-producing step (chroma key, opacity,
    // rotation fill, crossfade dissolve).
    if needs_alpha {
        p.push("format=yuva420p".to_string());
    }
    // Chroma key (color → transparency) after alpha is available.
    for e in &clip.effects {
        if let Some(f) = chroma_filter(e) {
            p.push(f);
        }
    }
    // The shape mask (once alpha exists — it only rewrites the alpha plane, so
    // it composes with a chroma key above it) and opacity: animated opacity is a
    // per-frame geq alpha (geq's time var is `T`), else a constant alpha mix.
    // The mask is the same alpha-plane geq, so a clip that
    // has both shares one pass — geq is per-pixel and by far the most expensive
    // filter in the chain, and two back-to-back passes would double it.
    let opacity_expr = anim_opacity.then(|| keyframe_expr(&clip.property_curve(Property::Opacity), "T", clip.timeline_start));
    match (&clip.mask, opacity_expr) {
        (Some(mask), expr) if clip.mask_animated() => p.push(keyed_mask_alpha(clip, mask, expr.as_deref())),
        (Some(mask), Some(expr)) => p.push(format!(
            "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({keep})*({expr})*alpha(X,Y)'",
            keep = mask_keep_expr(mask)
        )),
        (Some(mask), None) => p.push(mask_filter(mask)),
        (None, Some(expr)) => p.push(format!(
            "geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='({expr})*alpha(X,Y)'"
        )),
        (None, None) => {}
    }
    if !opacity_keyed && t.opacity < 1.0 {
        p.push(opacity_filter(t.opacity));
    }
    // Rotation: animated angle expression (degrees → radians), else a constant
    // rotate. Animated rotation uses a fixed bounding box (the frame diagonal).
    //
    // The animated fill is `black@0`, not `none`: `none` means "do not fill", and
    // `rotate` then leaves whatever its output buffer last held outside the
    // picture. A constant angle rewrites the same footprint every frame, so the
    // corners stay as the zeroed buffer was allocated (transparent) and nobody
    // sees it; an angle that moves leaves each earlier frame's footprint behind
    // in every buffer it reuses, and the picture grows into the union of every
    // pose it has had (the "erratic" rotation, on 6.1 and 9.0 alike).
    if anim_rotation {
        let expr = driven_expr(clip, Property::Rotation, "t");
        p.push(format!(
            "rotate=a='({expr})*PI/180':fillcolor=black@0:ow='hypot(iw,ih)':oh='hypot(iw,ih)'"
        ));
    } else if !rotation_keyed && t.rotation != 0.0 {
        p.push(static_rotate(t.rotation, !clip.blend_mode.is_normal()));
    }
    // `fade` reads the frame's pts, which `setpts` above already moved onto
    // the timeline, so a fade starts at the clip's timeline position and not
    // at 0 — clip-local times blacked out a later clip's fade-out entirely and
    // made its fade-in / dip / dissolve land before the clip existed. A dip
    // through white lands on the frame itself rather than on the alpha plane, so
    // it needs no `format=yuva420p`; a dissolve's alpha ramp does, and
    // `needs_alpha` above has already established it.
    for step in timing.fades() {
        p.push(fade_filter(&step));
    }
    if placed {
        // After `fps` (output frames) and after every filter that acts on the picture at its fit
        // size; it ends in `yuva420p`, the format `overlay` takes, at the canvas' size. A fade on a
        // chain with no alpha plane runs in the delivery's own format, as the terminal one keeps it
        // (left to negotiation it would run in the `yuva420p` that follows, where black is 0).
        if !needs_alpha && !fx.alpha && !timing.fades().is_empty() {
            p.push(format!("format={}", fmt.pix_fmt));
        }
        p.extend(placement_filters(clip, fx, fmt, needs_alpha, crop_rate, crop_first));
    } else if fx.alpha && !needs_alpha {
        // A source with an alpha channel keeps it to the overlay: the terminal pixel
        // format below has none, and `overlay` takes a `yuva420p` picture natively, so
        // flattening here put a transparent sticker's cut-out on black while the still,
        // which has no terminal format, kept it.
        p.push("format=yuva420p".to_string());
    } else if !needs_alpha {
        // Terminal pixel format — kept equal to argv `-pix_fmt` so a 10-bit /
        // 4:2:2 selection isn't silently bottlenecked back through 8-bit.
        p.push(format!("format={}", fmt.pix_fmt));
    }
    p.join(",")
}

/// Composite a single still of the `timeline` at timeline time `t` and return
/// it as JPEG bytes (`quality` = `-q:v`), the canvas downscaled so it is at most
/// `max_width` px wide. Lets an LLM *see the cut it is assembling* (which footage
/// is on screen, framing, picture-in-picture placement, crop, color) rather than
/// reasoning about timestamps blind.
///
/// Gives up with [`Error::Cancelled`] once `cancel` is true.
pub fn timeline_frame_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    run_still(
        timeline,
        assets,
        opts,
        t,
        max_width,
        None,
        &StillOutput::JpegPipe { quality },
        cancel,
    )
}

/// The composited still at `t` as its Y'CbCr samples: limited-range 4:4:4 planes (Y, then Cb,
/// then Cr) of the frame [`timeline_frame_cancellable`] would encode, with its size. The scopes read this
/// rather than an RGB conversion, whose matrix is the composite's and not the delivery's.
/// Niced like every read the UI draws from, killed past [`STILL_TIMEOUT`], and gives up with
/// [`Error::Cancelled`] once `cancel` is true.
pub fn timeline_frame_yuv_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    cancel: &dyn Fn() -> bool,
) -> Result<(u32, u32, Vec<u8>)> {
    let fmt = export_format(&timeline.for_render(), assets, opts);
    let (w, h) = still_size(fmt.width, fmt.height, max_width);
    let planes = run_still(
        timeline,
        assets,
        opts,
        t,
        max_width,
        None,
        &StillOutput::Yuv444Pipe { width: w, height: h },
        cancel,
    )?;
    if planes.len() != (w as usize) * (h as usize) * 3 {
        return Err(Error::Engine(format!(
            "could not render timeline frame at {t:.3}s: {} bytes for a {w}x{h} frame",
            planes.len()
        )));
    }
    Ok((w, h, planes))
}

/// [`timeline_frame`] of one `region` of the composited canvas. The canvas is
/// rendered large enough that the region alone comes out `max_width` wide
/// (capped at the delivery frame, so nothing is invented), then cropped — a
/// zoom into the cut rather than a screenshot of it.
/// Gives up with [`Error::Cancelled`] once `cancel` is true.
#[allow(clippy::too_many_arguments)]
pub fn timeline_frame_region_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    region: Region,
    max_width: u32,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    let region = (!region.is_full()).then_some(region);
    run_still(
        timeline,
        assets,
        opts,
        t,
        max_width,
        region,
        &StillOutput::JpegPipe { quality },
        cancel,
    )
}

/// Write the composited still at timeline time `t` to `path` as a **cover
/// frame**: full delivery resolution (no preview downscale) in `format`.
///
/// A cover is the picture a platform shows before anyone presses play, and this
/// renders it through the very graph the export uses — so the cover is literally
/// a frame of the video it fronts, at the project's delivery shape, rather than
/// a screenshot that has to be cropped back into agreement.
pub fn export_still(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    path: &Path,
    format: ImageFormat,
    quality: u8,
) -> Result<PathBuf> {
    export_still_cancellable(timeline, assets, opts, t, path, format, quality, &|| false)
}

/// [`export_still`] that gives up with [`Error::Cancelled`] once `cancel` is true (polled about
/// fifty times a second while ffmpeg runs; the render is killed). A cover that fails or is
/// cancelled removes what it wrote ([`discard_partial`]), leaving an earlier file at `path`
/// alone when this run never touched it.
#[allow(clippy::too_many_arguments)]
pub fn export_still_cancellable(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    path: &Path,
    format: ImageFormat,
    quality: u8,
    cancel: &dyn Fn() -> bool,
) -> Result<PathBuf> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let before = file_mtime(path);
    let out = StillOutput::File {
        path: path.to_string_lossy().into_owned(),
        format,
        quality,
    };
    // `u32::MAX` asks for no cap: `build_still_args` clamps to the delivery
    // width, which is exactly the cover size.
    if let Err(e) = run_still(timeline, assets, opts, t, u32::MAX, None, &out, cancel) {
        discard_partial(path, before);
        return Err(e);
    }
    Ok(path.to_path_buf())
}

/// Write the picture file `input` again as `output`, encoded with `codec` (`-c:v mjpeg -q:v 2`, …): one
/// frame, killed past [`STILL_TIMEOUT`] or once `cancel` is true ([`Error::Cancelled`]). A run that fails
/// removes what it wrote ([`discard_partial`]).
pub fn transcode_image(input: &Path, output: &Path, codec: &[&str], cancel: &dyn Fn() -> bool) -> Result<()> {
    let before = file_mtime(output);
    let bin = ffmpeg_bin();
    let mut cmd = command(&bin);
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-frames:v", "1"])
        .args(codec)
        .args(["-f", "image2", "-update", "1", "-y"])
        .arg(output);
    let result = match output_supervised(cmd, std::time::Duration::MAX, Some(STILL_TIMEOUT), cancel) {
        Err(e) => Err(launch_err(&bin, e)),
        Ok(out) if out.cancelled => Err(Error::Cancelled),
        Ok(out) if out.timed_out => Err(Error::Engine(format!(
            "could not write {}: ffmpeg did not answer in {} s and was stopped",
            output.display(),
            STILL_TIMEOUT.as_secs()
        ))),
        Ok(out) if !out.status.success() => Err(Error::Engine(format!(
            "could not write {}: {}",
            output.display(),
            out.stderr.trim()
        ))),
        Ok(_) => Ok(()),
    };
    if result.is_err() {
        discard_partial(output, before);
    }
    result
}

/// Run a composited still through ffmpeg, retrying in software if hardware
/// decode was asked for and failed. Returns stdout (empty for a file sink).
#[allow(clippy::too_many_arguments)]
fn run_still(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    // The scopes are a measurement, not the frame the user is waiting to see: niced.
    let spawn = StillSpawn {
        share: 1,
        background: matches!(out, StillOutput::Yuv444Pipe { .. }),
    };
    run_still_as(timeline, assets, opts, t, max_width, region, out, spawn, cancel)
}

/// How a still's ffmpeg is spawned: the share of the budget it takes among stills drawn side by
/// side ([`tune_still_args_shared`]), and whether it runs niced.
#[derive(Debug, Clone, Copy)]
struct StillSpawn {
    share: usize,
    background: bool,
}

/// [`run_still`] spawned as `spawn` says.
#[allow(clippy::too_many_arguments)]
fn run_still_as(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
    spawn: StillSpawn,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<u8>> {
    let piping = matches!(out, StillOutput::JpegPipe { .. });
    // The scopes' output size is known, so the read buffer is allocated once instead of doubling
    // through ~3 MB.
    let capacity = match out {
        StillOutput::Yuv444Pipe { width, height } => (*width as usize) * (*height as usize) * 3,
        _ => 0,
    };
    let background = spawn.background;
    let hw_used = std::cell::Cell::new(false);
    // The titles' pictures, on disk and kept for the run (and its software retry).
    let _titles = still_titles(timeline, assets, opts, t, max_width, region)?;
    let run = |o: &ExportOptions| -> Result<Vec<u8>> {
        let mut args = build_still_args(timeline, assets, o, t, max_width, region, out)?;
        hw_used.set(tune_still_args_shared(&mut args, assets, HostOs::CURRENT, spawn.share));
        let _script = externalize_filter_complex(&mut args, "still")?;
        let bin = ffmpeg_bin();
        let mut cmd = if background { bg_command(&bin) } else { command(&bin) };
        cmd.args(&args);
        // No `-progress` on a one-frame render, so no stall bound: a wall-clock one and the cancel.
        let output = output_supervised_sized(cmd, std::time::Duration::MAX, Some(STILL_TIMEOUT), cancel, capacity)
            .map_err(|e| launch_err(&bin, e))?;
        if output.cancelled {
            return Err(Error::Cancelled);
        }
        if output.timed_out {
            return Err(Error::Engine(format!(
                "could not render timeline frame at {t:.3}s: ffmpeg did not answer in {} s and was stopped",
                STILL_TIMEOUT.as_secs()
            )));
        }
        if !output.status.success() || (piping && output.stdout.is_empty()) {
            return Err(Error::Engine(format!(
                "could not render timeline frame at {t:.3}s: {}",
                output.stderr.trim()
            )));
        }
        Ok(output.stdout)
    };
    match run(opts) {
        Err(Error::Cancelled) => Err(Error::Cancelled),
        // Mirror `decode_frame`'s fallback: a software retry that succeeds means
        // `-hwaccel` is the culprit here, so stop asking for it. Only a run that
        // still asked for hardware after the spawn-time tuning can blame it.
        Err(hw_err) if hw_used.get() => match run(&ExportOptions {
            hwaccel: None,
            ..opts.clone()
        }) {
            Ok(bytes) => {
                HWACCEL_OK.store(false, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!("hardware decode failed for the timeline still ({hw_err}); using software decode");
                Ok(bytes)
            }
            Err(_) => Err(hw_err),
        },
        result => result,
    }
}

/// How long one composited still may run before it is killed. A still of a 4K 4:2:2 original is
/// a second or two; this is for the one that hangs (a hardware decoder stuck in a driver), which
/// would otherwise hold the caller, and the Preview's one frame in flight, for good.
const STILL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// `Command::output()` with a deadline: run `cmd` with its stdin closed and both output pipes
/// read, and kill it if it has not finished in `limit` (`ErrorKind::TimedOut`). The calling thread
/// reads stdout to its end (into a buffer of `capacity` bytes up front), a watchdog kills the process at the deadline (which ends that read),
/// and nothing waits on the child while the watchdog could need it.
pub(super) fn output_bounded(
    cmd: &mut Command,
    limit: std::time::Duration,
    capacity: usize,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, RecvTimeoutError};
    use std::sync::Arc;

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let child = Arc::new(Mutex::new(child));
    let timed_out = Arc::new(AtomicBool::new(false));
    let (done, finished) = channel::<()>();
    let watchdog = {
        let child = Arc::clone(&child);
        let timed_out = Arc::clone(&timed_out);
        std::thread::spawn(move || {
            if matches!(finished.recv_timeout(limit), Err(RecvTimeoutError::Timeout)) {
                timed_out.store(true, Ordering::SeqCst);
                let _ = child.lock().unwrap_or_else(|e| e.into_inner()).kill();
            }
        })
    };
    let err_reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_end(&mut text);
        }
        text
    });
    let mut out = Vec::with_capacity(capacity);
    let read = match stdout.as_mut() {
        Some(o) => o.read_to_end(&mut out).map(|_| ()),
        None => Ok(()),
    };
    // The output has ended, so the process is exiting; poll rather than block in `wait`, which
    // would hold the child's lock against the watchdog's kill.
    let started = Instant::now();
    let status = loop {
        let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
        match c.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() > limit => {
                timed_out.store(true, Ordering::SeqCst);
                let _ = c.kill();
                break c.wait();
            }
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        drop(c);
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    let _ = done.send(());
    let _ = watchdog.join();
    let stderr = err_reader.join().unwrap_or_default();
    if timed_out.load(Ordering::SeqCst) {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the process timed out"));
    }
    read?;
    Ok(std::process::Output {
        status: status?,
        stdout: out,
        stderr,
    })
}

/// [`output_bounded`] that a caller's `cancel` can also stop: `Ok(None)` is a run killed because
/// `cancel` went true (the caller says [`Error::Cancelled`]); a run past `limit` is the same
/// `TimedOut` error. With no `cancel` it *is* [`output_bounded`] — the interactive reads (a
/// scrubbed frame) keep the path that has no polling interval in it.
pub(super) fn output_bounded_cancellable(
    mut cmd: Command,
    limit: std::time::Duration,
    cancel: Option<&dyn Fn() -> bool>,
) -> std::io::Result<Option<std::process::Output>> {
    let Some(cancel) = cancel else {
        return output_bounded(&mut cmd, limit, 0).map(Some);
    };
    // These reads print no `-progress`, so stderr silence means nothing: only the wall clock bounds them.
    let run = output_supervised(cmd, std::time::Duration::MAX, Some(limit), cancel)?;
    if run.cancelled {
        return Ok(None);
    }
    if run.timed_out {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the process timed out"));
    }
    Ok(Some(std::process::Output {
        status: run.status,
        stdout: run.stdout,
        stderr: run.stderr.into_bytes(),
    }))
}

/// Tune a built still argv for the process about to be spawned: each input's `-hwaccel` and
/// threads per `decode_policy` (a proxy is decoded on one thread without a hardware session; a
/// still whose every input is a proxy runs its graph and encoder on one thread too), the budget's
/// threads otherwise. Returns whether any input still asks for hardware decode. At spawn time,
/// not in [`build_still_args`], so the builder keeps describing exactly what was asked for.
#[cfg(test)]
fn tune_still_args(args: &mut Vec<String>, assets: &[Asset], os: HostOs) -> bool {
    tune_still_args_shared(args, assets, os, 1)
}

/// [`tune_still_args`] for one of `share` stills drawn side by side (a sheet of the cut): what is
/// not cheap takes its share of the budget's threads ([`cpu::limit_args_shared`]; a lone still is
/// [`cpu::limit_args`] at the budget exactly).
fn tune_still_args_shared(args: &mut Vec<String>, assets: &[Asset], os: HostOs, share: usize) -> bool {
    let tuning = tune_inputs(args, os, Reading::Frame, &facts_from_assets(assets));
    if tuning.all_cheap {
        cpu::single_frame_args(args);
    } else {
        cpu::limit_args_shared(args, share);
    }
    tuning.hw
}

/// The JPEG-pipe still args — [`timeline_frame`]'s shape, and what every still
/// test asserts against.
#[cfg(test)]
fn build_timeline_frame_args(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    quality: u8,
) -> Result<Vec<String>> {
    build_still_args(timeline, assets, opts, t, max_width, None, &StillOutput::JpegPipe { quality })
}

/// Where a composited still is written, and in what image format.
///
/// The still compositor serves two callers with the same graph: the preview /
/// agent path wants JPEG bytes on stdout, and a **cover frame** wants a real
/// file at full delivery resolution — so only the sink differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StillOutput {
    /// MJPEG on stdout (`quality` = `-q:v`) — the preview and agent path.
    JpegPipe { quality: u8 },
    /// An image file. JPEG honors `quality`; PNG is lossless and ignores it.
    File { path: String, format: ImageFormat, quality: u8 },
    /// Raw `rgb24` on stdout — only for [`composite_color_policy`]'s probe, which
    /// wants the converted pixels themselves.
    RgbPipe,
    /// Raw planar `yuv444p` on stdout — [`timeline_frame_yuv_cancellable`], for the scopes. The frame's size
    /// sizes the read buffer; the argv does not carry it (the graph scales to `max_width`).
    Yuv444Pipe { width: u32, height: u32 },
}

/// The image formats a cover frame can be written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Jpeg,
    Png,
}

impl ImageFormat {
    /// The customary file extension (no dot).
    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Png => "png",
        }
    }

    /// The format a path's extension asks for, defaulting to JPEG — the format
    /// every platform accepts as a cover.
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("png") => ImageFormat::Png,
            _ => ImageFormat::Jpeg,
        }
    }

    fn encoder(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "mjpeg",
            ImageFormat::Png => "png",
        }
    }
}

impl StillOutput {
    /// The trailing output arguments: one frame, encoded and sent to the sink.
    fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-frames:v".to_string(), "1".to_string()];
        match self {
            StillOutput::JpegPipe { quality } => {
                a.extend([
                    "-q:v".to_string(),
                    quality.to_string(),
                    "-f".to_string(),
                    "image2pipe".to_string(),
                    "-vcodec".to_string(),
                    "mjpeg".to_string(),
                    "pipe:1".to_string(),
                ]);
            }
            StillOutput::RgbPipe => {
                a.extend(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"].map(String::from));
            }
            StillOutput::Yuv444Pipe { .. } => {
                a.extend(["-f", "rawvideo", "-pix_fmt", "yuv444p", "pipe:1"].map(String::from));
            }
            StillOutput::File { path, format, quality } => {
                if *format == ImageFormat::Jpeg {
                    a.extend(["-q:v".to_string(), quality.to_string()]);
                }
                a.extend([
                    "-f".to_string(),
                    "image2".to_string(),
                    "-vcodec".to_string(),
                    format.encoder().to_string(),
                    "-y".to_string(),
                    path.clone(),
                ]);
            }
        }
        a
    }
}

/// How a seek that has to land on one particular frame is spelled: `-ss` to the
/// microsecond.
///
/// `-ss` is an exact tick on a fine time base, so milliseconds are not enough: with a
/// frame at 1.0006 s, `-ss 1.0006` returns it and the spelling `1.001` skips to the
/// next. The composited still and `kerf-gpu`'s decode of the same layer both call
/// this, so the two cannot drift apart and show different frames.
pub fn seek_arg(seconds: f64) -> String {
    format!("{seconds:.6}")
}

/// How the export spells a clip's input `-ss`: the shortest text of the `f64`, which FFmpeg
/// reads and truncates to whole microseconds. [`seek_arg`] rounds to the nearest instead, so
/// for a window start like 2/30 s on a one-microsecond time base the two land on frames a tick
/// apart; whatever has to see the frames the export sees (`FpsPick::seek_arg`, the frame pick's
/// model of the seek) takes this one.
pub fn export_seek_arg(seconds: f64) -> String {
    format!("{seconds}")
}

/// Pure arg builder for a composited still, parameterized by its sink (no I/O,
/// unit-tested).
///
/// Every video clip whose timeline span contains `t` is decoded at its
/// corresponding source time (`-ss` input seek), put through the same geometry /
/// color chain the export uses ([`still_clip_chain_graded`] mirrors [`video_clip_chain`]
/// minus the time-domain `trim`/`setpts`/`fps` steps and the fades not changing
/// the picture at `t`), then `overlay`d onto a black canvas in **track-then-timeline order** — so later tracks composite on
/// top and gaps fall through to black, matching export framing. The output canvas
/// keeps the export aspect ratio capped to `max_width`. A frame inside a fade, a
/// dip, a dissolve, a slide or a push is drawn as the export draws it
/// ([`still_fx`]): the outgoing clip on its tail, the export's own `fade` filters
/// at the output frame `t` falls in, the travel at that frame's time.
fn build_still_args(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
) -> Result<Vec<String>> {
    // Same gate as the export, so the still shows the cut that would render.
    let rendered = timeline.for_render();
    build_still_args_rendered(&rendered, assets, opts, t, max_width, region, out)
}

/// The frame a still is composited on: the export's format, the region normalised, and the output
/// canvas (`ow` x `oh`: the export's aspect capped to `max_width`, even). A region zoom composites a
/// canvas wide enough that the region alone is `max_width` — the delivery frame caps it, so a zoom
/// never upscales. The builder and whatever draws the still's titles first both ask this.
fn still_canvas(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    max_width: u32,
    region: Option<Region>,
) -> (ExportFormat, Option<Region>, u32, u32) {
    let fmt = export_format(timeline, assets, opts);
    let region = region.map(Region::normalized);
    let canvas_width = match region {
        Some(r) => ((max_width as f64 / r.width).ceil() as u32).min(fmt.width),
        None => max_width,
    };
    let (ow, oh) = still_size(fmt.width, fmt.height, canvas_width);
    (fmt, region, ow, oh)
}

/// Have the animated titles a still at `t` shows drawn and kept (until the pins drop), before its
/// ffmpeg is spawned. Nothing for a cut without any.
fn still_titles(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
) -> Result<Vec<crate::svg::raster::Pin>> {
    if timeline.svg_overlays.is_empty() && timeline.title_clips().next().is_none() {
        return Ok(Vec::new());
    }
    let rendered = timeline.for_render();
    let (fmt, _, ow, oh) = still_canvas(&rendered, assets, opts, max_width, region);
    let t = t.max(0.0);
    let mut pictures = svg_layer::still_svgs(&rendered, ow, oh, t);
    pictures.extend(
        still_clip_pictures(&still_layers(&rendered, assets, fmt.fps, t), ow, oh)
            .into_iter()
            .flatten(),
    );
    svg_layer::ensure_stills(&pictures)
}

/// The picture of each title clip among a still's `layers` (`None` for a layer of footage), drawn over
/// the whole `ow` x `oh` canvas at the clip's own clock: its input, as a still image's file is.
fn still_clip_pictures<'a>(layers: &[still_fx::StillLayer<'a>], ow: u32, oh: u32) -> Vec<Option<svg_layer::StillSvg<'a>>> {
    layers
        .iter()
        .map(|layer| {
            let clip = layer.active.clip;
            clip.generator
                .as_ref()
                .map(|g| svg_layer::still_clip_picture(g.svg(), ow, oh, layer.active.source_time))
        })
        .collect()
}

/// [`build_still_args`] over a timeline that has already been through [`Timeline::for_render`]:
/// what a caller asking for many frames of one cut (the scrub cache keys every frame of a
/// window) does once instead of per frame.
fn build_still_args_rendered(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    t: f64,
    max_width: u32,
    region: Option<Region>,
    out: &StillOutput,
) -> Result<Vec<String>> {
    let (fmt, region, ow, oh) = still_canvas(timeline, assets, opts, max_width, region);
    let t = t.max(0.0);
    let asset_of = |id| assets.iter().find(|a: &&Asset| a.id == id);
    // The animated titles on screen at `t`: one picture each (`run_still` draws them first).
    let svg = svg_layer::still_svgs(timeline, ow, oh, t);

    // Active video clips at `t`, in composite order (tracks in list order, clips
    // within a track in timeline order), paired with their source time, the clips a
    // transition keeps on screen past their end included. The `RenderPlan` a compositor
    // draws from is built from the same list.
    let active = still_layers(timeline, assets, fmt.fps, t);
    let drawn = still_clip_pictures(&active, ow, oh);

    let mut args: Vec<String> = vec!["-hide_banner".to_string(), "-loglevel".to_string(), "error".to_string()];
    // What steadies each stabilized clip on screen: the frame at the seek moved as the export moves it.
    let mut stabs: Vec<Option<Stabilizer>> = Vec::with_capacity(active.len());
    for (layer, picture) in active.iter().zip(&drawn) {
        if let Some(picture) = picture {
            stabs.push(None);
            svg_layer::push_still_inputs(std::slice::from_ref(picture), &mut args);
            continue;
        }
        let (clip, src) = (layer.active.clip, layer.active.source_time);
        let asset = asset_of(clip.asset_id).ok_or(Error::AssetNotFound(clip.asset_id))?;
        let stab = stabilizer(clip, Some(asset), (src, src));
        // vid.stab's corrections are numbered from the frame its seek lands on: between two frames.
        let src = stab.as_ref().and_then(Stabilizer::trim_start).unwrap_or(src);
        stabs.push(stab);
        // A still has a single frame at t=0 (`trim=end_frame=1` in the chain picks
        // it up); seeking into it decodes nothing, so skip the `-ss` (and any
        // decode acceleration) for images.
        if !asset.is_image() {
            if let Some(hw) = opts
                .hwaccel
                .as_deref()
                .filter(|h| !h.is_empty() && !h.eq_ignore_ascii_case("none"))
            {
                args.push("-hwaccel".to_string());
                args.push(hw.to_string());
            }
            args.push("-ss".to_string());
            args.push(seek_arg(src));
        }
        args.push("-i".to_string());
        args.push(asset.path.clone());
    }
    svg_layer::push_still_inputs(&svg, &mut args);

    // Black base + each active clip's still chain (its transform sampled at `t`,
    // so a keyframed clip shows its pose), overlaid in order, then the text
    // overlays live at `t`. A trailing `null` makes the final label always
    // `[outv]` (so an empty timeline still maps cleanly).
    let live: Vec<&TextOverlay> = timeline.overlays.iter().filter(|o| t >= o.start && t < o.end).collect();
    let canvas = StillCanvas {
        w: ow,
        h: oh,
        fit: fmt.fit,
        sf: fmt.scale_flags(),
    };
    let mut chains: Vec<String> = vec![format!("color=c=black:s={ow}x{oh}:d=0.1[base]")];
    let mut cur = "base".to_string();
    // The matrix a blended clip's composite is in: the bottom layer's, as in the export.
    let mut composite = None;
    let fx = transition_fx(timeline, assets);
    let clips: Vec<&Clip> = timeline.tracks.iter().flat_map(|t| t.clips.iter()).collect();
    for (n, layer) in active.iter().enumerate() {
        let (ac, clip) = (&layer.active, layer.active.clip);
        // A still the export draws through `still_motion` is drawn through it here, at this moment.
        let still_motion = clips
            .iter()
            .position(|c| std::ptr::eq(*c, clip))
            .and_then(|flat| still_motion_for(clip, assets, &fmt, &fx[flat]))
            .and_then(|_| {
                StillMotion::new(
                    clip,
                    still_motion::displayed(asset_of(clip.asset_id)),
                    (fmt.width, fmt.height),
                    (ow, oh),
                    fmt.fit,
                    &[ac.local_time],
                )
            })
            .map(|m| format!("{},{}", m.once(&canvas.sf), m.at(clip, ac.local_time)));
        let tf = ac.transform();
        let rf = clip.reframe_at(ac.local_time);
        let hdr = asset_of(clip.asset_id).and_then(|a| a.hdr());
        let tone = hdr.map(|h| format!("{},", tonemap_filter(h))).unwrap_or_default();
        let stream = asset_of(clip.asset_id).and_then(|a| a.streams.iter().find(|s| s.kind == StreamKind::Video));
        let matrix = composite_matrix(layer.label_bt709, crate::model::frame_matrix(stream, hdr.is_some()));
        if n == 0 {
            composite = matrix;
        }
        let steady = stabs[n].as_ref().map(|s| format!("{},", s.filters())).unwrap_or_default();
        chains.push(format!(
            "[{n}:v]{tone}{steady}{chain}[v{n}]",
            chain = still_clip_chain_graded(
                &tf,
                &ac.color(),
                &clip.grade,
                matrix,
                &clip.effects,
                rf.as_ref(),
                &canvas,
                clip.mask_at(ac.local_time).as_ref(),
                clip.crop_animated().then(|| {
                    let local = ac.local_time;
                    (
                        1.0 / clip.property_at(Property::CropZoom, local),
                        clip.property_at(Property::CropX, local),
                        clip.property_at(Property::CropY, local),
                    )
                }),
                clip.zoom_animated(),
                clip.fit_order(
                    fmt.fit,
                    // (Judged at the delivery frame, as the export judges it, not at the preview's size.)
                    crate::layer_geometry::cover_overflows(
                        if rf.is_some() {
                            Some((fmt.width, fmt.height))
                        } else {
                            picture_size(asset_of(clip.asset_id))
                        },
                        (fmt.width, fmt.height),
                        &tf,
                    ),
                ),
                &layer.fades,
                layer.label_bt709,
                (fmt.fit == Fit::Blur
                    && takes_blur_fill(
                        clip,
                        picture_size(asset_of(clip.asset_id)),
                        (fmt.width, fmt.height),
                        asset_of(clip.asset_id).is_some_and(Asset::has_alpha)
                    ))
                .then(|| format!("bf{n}"))
                .as_deref(),
                StillBlend {
                    blended: !clip.blend_mode.is_normal(),
                    tone_mapped: hdr.is_some(),
                },
                (still_motion.is_none() && clip.placement_animated()).then(|| {
                    (
                        clip.zoom_animated().then_some(tf.scale),
                        (tf.pos_x + layer.motion.0, tf.pos_y + layer.motion.1),
                    )
                }),
                still_motion.as_deref(),
            )
        ));
        let out = format!("ov{n}");
        let overlay = if still_motion.is_some() {
            still_overlay(&Transform::default(), layer.motion)
        } else if clip.placement_animated() {
            still_overlay(&Transform::default(), (0.0, 0.0))
        } else {
            still_overlay(&tf, layer.motion)
        };
        match clip.blend_mode.ffmpeg() {
            None => chains.push(format!("[{cur}][v{n}]{overlay}[{out}]")),
            Some(mode) => {
                let blend = Blend {
                    mode,
                    placement: &overlay,
                    window: None,
                    matrix: composite,
                };
                chains.extend(blend_chains(&cur, &format!("v{n}"), &blend, &n.to_string(), &out));
            }
        }
        cur = out;
    }
    // Titles over the clips, under the text, as in the export.
    cur = svg_layer::push_still_chains(&svg, active.len(), &cur, &mut chains);
    for (oi, ov) in live.iter().enumerate() {
        let out = format!("txt{oi}");
        chains.push(format!("[{cur}]{f}[{out}]", f = drawtext_still(ov, oh, t)));
        cur = out;
    }
    match region {
        Some(r) => chains.push(format!("[{cur}]{crop}[outv]", crop = r.crop_filter())),
        None => chains.push(format!("[{cur}]null[outv]")),
    }
    let filter = chains.join(";");

    args.extend([
        "-filter_complex".to_string(),
        filter,
        "-map".to_string(),
        "[outv]".to_string(),
    ]);
    args.extend(out.args());
    Ok(args)
}

/// The frame a [`timeline_frame`] composite renders into: the delivery aspect
/// capped to the caller's preview width, how footage of another shape meets it,
/// and the scaler flags. Bundled because these four always travel together —
/// every one of them comes from the same `ExportFormat`.
struct StillCanvas {
    w: u32,
    h: u32,
    fit: Fit,
    sf: String,
}

/// The still video chain for one clip in a [`timeline_frame`] composite: take a
/// single decoded frame, then apply the same 360 reprojection / crop /
/// fit-or-transform / color / opacity / rotation geometry as
/// [`video_clip_chain`], minus every time-domain step (trim/setpts/fps/fades)
/// since the `-ss` input seek already positioned it.
///
/// `reframe` is the clip's camera already **sampled** at the requested instant.
/// The still pipeline has no timeline clock to run `sendcmd` against, so an
/// animated reframe resolves to a constant here — which is exactly what the
/// export chain's commands will have set `v360` to at the same timestamp.
///
/// `zoom_last` is [`Clip::zoom_animated`]: the export runs a moving zoom as the **last**
/// stage of the chain (see [`video_clip_chain`]), so its blur, mask, rotation and grade
/// act on the picture at its fit size and are magnified with it. The still follows, or
/// the frame you scrub to would be sharper than the file at a zoom-in and softer at a
/// zoom-out. Every other clip zooms first, as it always did.
///
/// `order` is [`Clip::fit_order`] (a pose sampled to the identity is `Fitted` whatever it says): a
/// zoom in under Contain is taken in the fit's own `scale`, as the export's chain takes it.
///
/// `fades` are the clip's fades still changing the picture at the still's frame
/// ([`still_fx`]): where the export's chain applies its `fade` steps, after the rotation and
/// ahead of a moving zoom, the same filters run on the frame. None at all for a still
/// outside any fade, and then the chain is what it was.
///
/// `label_bt709` is the graph's BT.709 label for this layer ([`BT709_LABEL`]): the still decides
/// it over the layers on screen, and puts it where [`video_clip_chain`] does, ahead of everything.
///
/// `grade` is the clip's curves, wheels and LUT, right after the `eq`, as `video_clip_chain` places them.
///
/// `blur_label` names the inner pads of a [`Fit::Blur`] fill ([`blur_fill`]), unique in the graph,
/// and is `None` for a clip the export gives none ([`takes_blur_fill`]: an animated one, whose pose
/// is not the plain fit even where it samples to it, a see-through one, a reframe, one of the
/// frame's own shape), which is then placed as under Contain.
///
/// `blend` is what a blend mode changes in the chain, as in `video_clip_chain`: transparent letterbox bars
/// ([`keeps_clear_bars`]) and rotated corners ([`static_rotate`]).
///
/// `placement` is a clip whose zoom or position moves ([`Clip::placement_animated`]): the zoom that
/// moves (`None` when only the position does) and the position with any travel, at this moment. The
/// chain then ends in the export's placement ([`placement_chain`], with these numbers), and the
/// overlay puts its canvas-sized frame at the origin.
#[allow(clippy::too_many_arguments)]
fn still_clip_chain_graded(
    tf: &Transform,
    color: &Color,
    grade: &Grade,
    frame_matrix: Option<&str>,
    effects: &[VideoEffect],
    reframe: Option<&ResolvedReframe>,
    canvas: &StillCanvas,
    mask: Option<&Mask>,
    keyed_crop: Option<(f64, f64, f64)>,
    zoom_last: bool,
    order: FitOrder,
    fades: &StillFades,
    label_bt709: bool,
    blur_label: Option<&str>,
    blend: StillBlend,
    placement: Option<(Option<f64>, (f64, f64))>,
    still_motion: Option<&str>,
) -> String {
    let StillCanvas { w: ow, h: oh, fit, sf } = canvas;
    let (ow, oh, fit) = (*ow, *oh, *fit);
    let grade = grade_filters(grade, frame_matrix);
    let letterboxed =
        still_motion.is_none() && tf.is_identity() && (fit == Fit::Contain || (fit == Fit::Blur && blur_label.is_none()));
    let clear_bars = letterboxed && keeps_clear_bars(blend.blended, blend.tone_mapped, effects, &grade);
    let chroma = effects.iter().any(|e| e.produces_alpha());
    let needs_alpha = (!tf.is_identity() && tf.needs_alpha()) || chroma || mask.is_some() || fades.needs_alpha() || clear_bars;
    let mut p: Vec<String> = Vec::new();
    if label_bt709 {
        p.push(BT709_LABEL.to_string());
    }
    p.extend(["trim=end_frame=1".to_string(), "setpts=PTS-STARTPTS".to_string()]);
    let mut late_zoom: Option<String> = None;
    if let Some(geometry) = still_motion {
        // A moving still: its whole geometry at this moment, as the export draws that frame.
        p.push(geometry.to_string());
    } else {
        let crop = tf.has_crop().then(|| {
            let cw = (1.0 - tf.crop_left - tf.crop_right).max(0.0);
            let ch = (1.0 - tf.crop_top - tf.crop_bottom).max(0.0);
            format!(
                "crop=w=iw*{cw}:h=ih*{ch}:x=iw*{cl}:y=ih*{ct}",
                cl = tf.crop_left,
                ct = tf.crop_top
            )
        });
        // Mirrors `video_clip_chain`: crop follows reprojection. There is no `setpts`
        // to step around here, so one order serves both cases.
        if let Some(r) = reframe {
            p.push(reframe_filter(r, ow, oh, PREVIEW_INTERP, None));
        }
        p.extend(crop);
        // A keyed crop's window at this moment, through the very `perspective` the export runs it
        // with (`crop_remap_filter`), so the still is the file's frame.
        p.extend(keyed_crop.map(|(side, x, y)| {
            format!(
                "perspective={}:interpolation=linear",
                crop_corners(&fnum(side), &fnum(x), &fnum(y))
            )
        }));
        // The same base fit `video_clip_chain` applies, so a Cover delivery crops in
        // the scrubbed still exactly as it will in the file. Letterboxing here while
        // the export cropped meant the one frame you look at while cutting was the
        // one shape you were never going to ship.
        // A pose the still samples to the identity is the plain fit, whatever the clip's chain in the
        // export is (a keyed clip is never padded there).
        let order = if tf.is_identity() { FitOrder::Fitted } else { order };
        match fit {
            Fit::Blur if tf.is_identity() && blur_label.is_some() => {
                // The still tone-maps ahead of the chain: its picture is 8-bit SDR here.
                p.push(blur_fill(ow, oh, "", sf, false, blur_label.unwrap_or_default()));
            }
            // The zoom in is the fit's own scale, as the export draws it.
            Fit::Contain | Fit::Blur if order == FitOrder::Folded => {
                p.push(fit_scale(ow, oh, tf.scale, "decrease", false, sf));
            }
            Fit::Contain | Fit::Blur => p.push(format!("scale={ow}:{oh}:force_original_aspect_ratio=decrease{sf}")),
            // The cover of the frame times the zoom, whole: the overlay cuts what hangs over the canvas, as
            // the export's does. (It does not bound the picture with the export's centred `crop`; one frame
            // is cheap, and the crop shows nothing the canvas does not.)
            Fit::Cover if order == FitOrder::Uncropped => {
                p.push(fit_scale(ow, oh, tf.scale, "increase", false, sf));
            }
            Fit::Cover => {
                p.push(format!("scale={ow}:{oh}:force_original_aspect_ratio=increase{sf}"));
                p.push(format!("crop={ow}:{oh}"));
            }
        }
        if tf.is_identity() {
            // Cover already fills the frame; padding it would be a no-op that still
            // costs a filter, so only the letterboxed path needs it (a blurred fill is the
            // frame; a clip that does not take one is letterboxed as under Contain).
            if letterboxed {
                p.push(letterbox_pad(ow, oh, clear_bars));
            }
        } else if (tf.scale - 1.0).abs() > 1e-9 && order == FitOrder::Fitted {
            let zoom = zoom_scale(&tf.scale.to_string(), false, tf.scale < TINY_SCALE, false, sf);
            if !zoom_last {
                p.push(zoom);
            } else if placement.is_none() {
                late_zoom = Some(zoom);
            }
        }
    }
    p.push("setsar=1".to_string());
    if !color.is_identity() {
        p.push(eq_filter(color));
    }
    p.extend(grade);
    for e in effects {
        if let Some(f) = video_effect_filter(e) {
            p.push(f);
        }
    }
    if needs_alpha {
        p.push("format=yuva420p".to_string());
    }
    for e in effects {
        if let Some(f) = chroma_filter(e) {
            p.push(f);
        }
    }
    if let Some(mask) = mask {
        p.push(mask_filter(mask));
    }
    if tf.opacity < 1.0 {
        p.push(opacity_filter(tf.opacity));
    }
    if tf.rotation != 0.0 {
        p.push(static_rotate(tf.rotation, blend.blended));
    }
    p.extend(fades.filters());
    if !fades.is_empty() && !needs_alpha && late_zoom.is_none() {
        p.push(format!("format={}", fades.terminal_format()));
    }
    p.extend(late_zoom);
    if let Some((zoom, (px, py))) = placement {
        // Always into `yuva420p` first: the still's opacity (`colorchannelmixer`) leaves an RGB picture, which
        // the export's never is, and placing it in RGB put the conversion after the resample (4 levels off).
        let zoom = zoom.map(fnum);
        p.extend(placement_chain(
            zoom.as_deref(),
            (&fnum(px), &fnum(py)),
            (ow, oh),
            false,
            false,
        ));
    }
    p.join(",")
}

/// [`still_clip_chain_graded`] with no grade.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn still_clip_chain(
    tf: &Transform,
    color: &Color,
    effects: &[VideoEffect],
    reframe: Option<&ResolvedReframe>,
    canvas: &StillCanvas,
    mask: Option<&Mask>,
    zoom_last: bool,
    fades: &StillFades,
    label_bt709: bool,
) -> String {
    // The order a clip with this pose and no keys has (`Clip::fit_order`).
    let picture_relative = mask.is_some() || effects.iter().any(|e| matches!(e, VideoEffect::Vignette));
    let order = match canvas.fit {
        Fit::Contain | Fit::Blur if !zoom_last && tf.scale > 1.0 + 1e-9 => FitOrder::Folded,
        Fit::Cover
            if !zoom_last && !picture_relative && ((tf.scale - 1.0).abs() > 1e-9 || tf.pos_x != 0.0 || tf.pos_y != 0.0) =>
        {
            FitOrder::Uncropped
        }
        _ => FitOrder::Fitted,
    };
    still_clip_chain_graded(
        tf,
        color,
        &Grade::default(),
        None,
        effects,
        reframe,
        canvas,
        mask,
        None,
        zoom_last,
        order,
        fades,
        label_bt709,
        Some("bf0"),
        StillBlend::default(),
        None,
        None,
    )
}

/// What a blend mode changes in a still layer's chain ([`still_clip_chain_graded`]).
#[derive(Clone, Copy, Debug, Default)]
struct StillBlend {
    /// The clip has a blend mode other than normal.
    blended: bool,
    /// Its picture is tone-mapped from HDR (which, in the export's chain, drops the alpha of clear bars).
    tone_mapped: bool,
}

/// The `overlay` placement for a clip in a [`timeline_frame`] composite: a full
/// frame for an identity transform, else centered with the clip's fractional
/// `pos_x`/`pos_y` offset (matching the export overlay positions). `motion` is the
/// travel a slide or push gives the clip at this frame, added to that offset as the
/// export adds it; a clip that is not moving passes `(0, 0)` and is placed as ever.
fn still_overlay(t: &Transform, motion: (f64, f64)) -> String {
    let moving = motion != (0.0, 0.0);
    if t.is_identity() && !moving {
        "overlay=(W-w)/2:(H-h)/2".to_string()
    } else if moving {
        format!(
            "overlay=x=(W-w)/2+({px})*W:y=(H-h)/2+({py})*H",
            px = fnum(t.pos_x + motion.0),
            py = fnum(t.pos_y + motion.1)
        )
    } else {
        format!("overlay=x=(W-w)/2+({px})*W:y=(H-h)/2+({py})*H", px = t.pos_x, py = t.pos_y)
    }
}

/// The audio filter chain for one clip (between `[i:a]` and `[a{i}]`): trim,
/// optional reverse / tempo, gain, fades (including transition cross-fades) and
/// delay to the clip's timeline position. Defaults reduce to the original chain.
///
/// One chain, so one tempo: a clip with a speed ramp of more than one step goes through
/// [`audio_clip_chains`] instead (this plays it at its mean speed, which is no ramp).
fn audio_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, layout: &str, mix: TrackMix) -> String {
    let mut p = audio_clip_head(clip, fx);
    let s = if clip.is_ramped() {
        match ramp::audio_steps(clip, ClipTiming::new(clip, fx).duration()).as_slice() {
            [only] => only.tempo(),
            _ => clip.mean_speed(),
        }
    } else {
        clip.speed_mag()
    };
    if (s - 1.0).abs() > 1e-9 {
        p.push(atempo_chain(s));
    }
    p.extend(audio_clip_tail(clip, fmt, fx, layout, mix));
    p.join(",")
}

/// The sound of a clip as the chain starts it: the source window (with the crossfade tail, and
/// relative to the input-side `-ss`), restamped to zero, and played backwards when the clip is.
fn audio_clip_head(clip: &Clip, fx: &ClipFx) -> Vec<String> {
    // Mirror the video crossfade tail (extends below source_in when reversed) and
    // the same input-side `-ss` fast-seek, so the atrim is relative to the seek.
    let (trim_start, trim_end) = clip_source_window(clip, fx);
    let seek = clip_input_seek(clip, fx, trim_start);
    let mut p: Vec<String> = Vec::new();
    p.push(format!("atrim=start={}:end={}", trim_start - seek, trim_end - seek));
    p.push("asetpts=PTS-STARTPTS".to_string());
    if clip.is_reversed() {
        p.push("areverse".to_string());
    }
    p
}

/// What follows the tempo in a clip's sound: gain (keyed or not), effects, the fades, the track
/// fader, the delivery format and pan, and the delay to the clip's place on the timeline.
fn audio_clip_tail(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, layout: &str, mix: TrackMix) -> Vec<String> {
    let dur = ClipTiming::new(clip, fx).duration();
    let delay = audio_delay(clip.timeline_start, fmt.sample_rate);
    let fi = clip.fade_in + fx.black_in + fx.white_in + fx.afade_in;
    let fo = clip.fade_out + fx.black_out + fx.white_out + fx.tail;

    let mut p: Vec<String> = Vec::new();
    if clip.volume_animated() {
        // A keyed gain: `volume` evaluated per frame, after `atempo` so its `t` is the
        // clip's own playing time. It holds one gain for a whole frame, so the frames are
        // cut to a few milliseconds first (`asetnsamples`, without padding the last one) or
        // a fade would step at the decoder's frame size — 21 ms for AAC, audible as zipper noise.
        p.push(format!("asetnsamples=n={VOLUME_FRAME_SAMPLES}:p=0"));
        p.push(format!(
            "volume='{}':eval=frame",
            keyframe_expr(&clip.property_curve(Property::Volume), "t", 0.0)
        ));
    } else {
        p.push(format!("volume={}", clip.volume));
    }
    // Per-clip audio effects (EQ / compressor / gate / denoise / filters) in author order,
    // after the clip gain.
    for e in &clip.audio {
        p.push(audio_effect_filter(e, Some(dur)));
    }
    // A fade that is wholly one half of a real crossfade pair (`ClipFx::afade_in_power` /
    // `tail_power`) is constant-power (`qsin`: sin in, cos out, so the summed power stays
    // put) where linear ramps dip about 3 dB at the midpoint. A fade with a clip fade or a
    // dip in it, a fade up from black, and a pair playing the same sound on both sides keep
    // the linear ramp they always had.
    let crossfade = |fade: f64, own: f64, power: bool| {
        if fade > 0.0 && own <= 0.0 && power {
            ":curve=qsin"
        } else {
            ""
        }
    };
    if fi > 0.0 {
        let curve = crossfade(fx.afade_in, clip.fade_in + fx.black_in + fx.white_in, fx.afade_in_power);
        p.push(format!("afade=t=in:st=0:d={}{curve}", fi.clamp(0.0, dur)));
    }
    if fo > 0.0 {
        let curve = crossfade(fx.tail, clip.fade_out + fx.black_out + fx.white_out, fx.tail_power);
        p.push(format!(
            "afade=t=out:st={}:d={}{curve}",
            (dur - fo).max(0.0),
            fo.clamp(0.0, dur)
        ));
    }
    // The track fader, after the clip's own chain. Omitted at unity, so a
    // project that never touched a fader renders the graph it always did.
    if (mix.volume - 1.0).abs() > f32::EPSILON {
        p.push(format!("volume={}", mix.volume));
    }
    p.push(format!(
        "aformat=sample_rates={sr}:channel_layouts={layout}",
        sr = fmt.sample_rate
    ));
    // The track pan, after `aformat` has normalized the stream to the delivery
    // layout: `pan` indexes channels by number, and run before the upmix a mono
    // source has no c1 — the attenuated leg would be synthesized from silence,
    // so panning a mono voice right would mute it instead of leaning it.
    // Omitted at centre, so an untouched mix stays byte-identical.
    let (gl, gr) = mix.pan;
    if layout == "stereo" && ((gl - 1.0).abs() > 1e-9 || (gr - 1.0).abs() > 1e-9) {
        // A balance, not a mono re-pan: each side keeps its own channel and is
        // attenuated, so a stereo music bed leans without collapsing.
        p.push(format!("pan=stereo|c0={}*c0|c1={}*c1", fnum(gl), fnum(gr)));
    }
    p.push(format!("adelay={delay}:all=1"));
    p
}

/// The chains of a clip's sound, `[src]…[a{flat}]`: [`audio_clip_chain`]'s one chain, or for a speed
/// ramp of several steps the pieces of [`ramp::audio_pieces`] cut from the source, each through its own
/// `atempo` and trimmed to its share of the timeline, crossfaded end to end, and then the chain's usual
/// tail on the whole. The head is resampled to the delivery rate first, so the trims count its samples.
///
/// ```text
/// [src]atrim=…,asetpts=PTS-STARTPTS,aresample=48000,asplit=3[ar0s0][ar0s1][ar0s2]
/// [ar0s0]atrim=start=0:end=1.18,asetpts=PTS-STARTPTS,atempo=1.1,adelay=…S:all=1,apad=whole_len=…,atrim=start_sample=0:end_sample=…,asetpts=N/SR/TB[ar0p0]
/// … [ar0p0][ar0p1]acrossfade=d=0.002[ar0x1] … [ar0x2]volume=1,…,adelay=0:all=1,asetpts=N/SR/TB[a0]
/// ```
fn audio_clip_chains(
    src: &str,
    flat: usize,
    clip: &Clip,
    fmt: &ExportFormat,
    fx: &ClipFx,
    layout: &str,
    mix: TrackMix,
) -> Vec<String> {
    let steps = if clip.is_ramped() {
        ramp::audio_steps(clip, ClipTiming::new(clip, fx).duration())
    } else {
        Vec::new()
    };
    if steps.len() < 2 || fmt.sample_rate == 0 {
        return vec![format!(
            "[{src}]{chain}[a{flat}]",
            chain = audio_clip_chain(clip, fmt, fx, layout, mix)
        )];
    }
    let (pieces, overlaps) = ramp::audio_pieces(&steps, fmt.sample_rate);
    let n = pieces.len();
    let mut chains = Vec::with_capacity(2 * n + 1);
    let mut head = audio_clip_head(clip, fx);
    head.push(format!("aresample={}", fmt.sample_rate));
    head.push(format!(
        "asplit={n}{}",
        (0..n).map(|i| format!("[ar{flat}s{i}]")).collect::<String>()
    ));
    chains.push(format!("[{src}]{}", head.join(",")));
    for (i, piece) in pieces.iter().enumerate() {
        let mut p = vec![
            format!("atrim=start={}:end={}", piece.from, piece.to),
            "asetpts=PTS-STARTPTS".to_string(),
        ];
        if ramp::tempo_applies(piece.tempo) {
            p.push(ramp::atempo_stages(piece.tempo));
        }
        if piece.lead > 0 {
            p.push(format!("adelay={}S:all=1", piece.lead));
        }
        p.push(format!("apad=whole_len={}", piece.skip + piece.keep));
        p.push(format!(
            "atrim=start_sample={}:end_sample={}",
            piece.skip,
            piece.skip + piece.keep
        ));
        // Counted, not rebased: the silence an `adelay` of `lead` puts first has no stamp to rebase.
        p.push("asetpts=N/SR/TB".to_string());
        chains.push(format!("[ar{flat}s{i}]{}[ar{flat}p{i}]", p.join(",")));
    }
    let mut joined = format!("ar{flat}p0");
    for (i, overlap) in overlaps.iter().enumerate() {
        let out = format!("ar{flat}x{}", i + 1);
        chains.push(format!(
            "[{joined}][ar{flat}p{}]acrossfade=d={}[{out}]",
            i + 1,
            *overlap as f64 / f64::from(fmt.sample_rate)
        ));
        joined = out;
    }
    // `adelay` puts out its silence before the first sample reaches it when its input is late (this
    // chain's is: nothing leaves the crossfades until the first pieces are cut), and stamps it with no
    // time at all, which the muxer cannot take. The stream is whole samples from the clip's start, so
    // they are counted again.
    chains.push(format!(
        "[{joined}]{},asetpts=N/SR/TB[a{flat}]",
        audio_clip_tail(clip, fmt, fx, layout, mix).join(",")
    ));
    chains
}

/// Samples in a frame the keyed volume is evaluated on (2.7 ms at 48 kHz): the gain steps by
/// at most the curve's slope times that between two frames.
const VOLUME_FRAME_SAMPLES: usize = 128;

/// `adelay`'s delay for a clip starting at `start` seconds: whole milliseconds when the
/// start is one (every graph that predates this stays byte-identical), otherwise an
/// exact sample count at `sample_rate` (`…S`) — rounding a bar-aligned splice to the
/// millisecond would put it up to half a millisecond off its neighbour. Pure.
fn audio_delay(start: f64, sample_rate: u32) -> String {
    let ms = start * 1000.0;
    if (ms - ms.round()).abs() < 1e-6 || sample_rate == 0 {
        format!("{}", ms.round().max(0.0) as i64)
    } else {
        format!("{}S", (start * sample_rate as f64).round().max(0.0) as i64)
    }
}

/// The speech-gate duck: `[aduck]` (the ducked bus) times a gain that sits at `depth`
/// dB while `[akside]` (the rest of the mix) speaks and at unity otherwise, into
/// `[aducked]`. Pure.
///
/// The gain is *made* as a signal: a constant 1.0 run through a `sidechaingate` keyed by
/// the rest is 1 while it speaks and 0 while it does not (the gate's 300 ms release is
/// the hangover between words). A gate run on the music itself switches within ~10 ms,
/// which clicks; so the 0/1 curve is smoothed twice — a fast one-pole (~50 ms rise) and a
/// slow one (~300 ms) — and the larger of the two taken, which rises with the fast one
/// and falls with the slow one. Three ffmpeg traps: a sidechain filter **ends with the
/// shorter input**, so a key that stops before the bed would cut the music off (the key
/// is padded with `apad`; the compressor path above still has this); a key of digital silence
/// holds the gate **open** (so the key gets a 1e-6 floor); and `amix` drops a negative
/// weight (so the gain is `1 - (1 - g) * curve` in one `aeval`, and the bus is
/// `amultiply`'d by it).
fn duck_gate_chains(depth_db: f64, layout: &str) -> Vec<String> {
    let cut = 1.0 - 10f64.powf(depth_db / 20.0);
    let upmix = if layout == "mono" { "" } else { ",pan=stereo|c0=c0|c1=c0" };
    vec![
        "[akside]apad,aformat=channel_layouts=mono,aeval=val(0)+0.000001:c=same[akfloor]".to_string(),
        "[aduck]asplit=2[aduckdry][aduckdc]".to_string(),
        "[aduckdc]aeval=1:c=mono[adc]".to_string(),
        "[adc][akfloor]sidechaingate=threshold=0.03:range=0:ratio=9000:knee=1:attack=10:release=300,asplit=2[agfastin][agslowin]"
            .to_string(),
        "[agfastin]lowpass=f=7:p=1,aformat=sample_fmts=fltp:channel_layouts=mono[agfast]".to_string(),
        "[agslowin]lowpass=f=1.2:p=1,aformat=sample_fmts=fltp:channel_layouts=mono[agslow]".to_string(),
        format!("[agfast][agslow]amerge=inputs=2,aeval='1-{cut:.6}*max(val(0),val(1))':c=mono{upmix}[again]"),
        "[aduckdry][again]amultiply[aducked]".to_string(),
    ]
}

/// Decompose a tempo change into `atempo` steps each within ffmpeg's supported
/// `[0.5, 2.0]` range (e.g. 4× → `atempo=2.0,atempo=2.0`).
fn atempo_chain(speed: f64) -> String {
    let mut s = speed;
    let mut parts: Vec<String> = Vec::new();
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s *= 2.0;
    }
    parts.push(format!("atempo={s}"));
    parts.join(",")
}

/// The stages of an export, the share of the bar each owns, and ffmpeg's `-progress` stream read
/// into positions.
mod progress;
pub use progress::ExportStage;
use progress::{eta_secs, metering_share, pass_position, variants_eta, Block, ProgressReader, VERIFY_SHARE};

/// What an export wrote, read back from the file: its streams and its own loudness.
mod written;
pub use written::{measure_export, ExportReport, LoudnessTarget, WrittenAudio, WrittenVideo};

/// Measuring the mix (`ebur128` meters on the export's own audio graph): a child
/// module so it reaches the private graph builders.
mod levels;
pub use levels::{mix_levels, stream_mix_pcm};

/// A clip's noise reduction (`afftdn`), its delay taken back out where this ffmpeg has one.
mod denoise;
use denoise::denoise_filter;
#[cfg(test)]
use denoise::with_afftdn_delay;

/// A confined run's probe: its argv, and what the real `ffprobe` refuses (`#[ignore]`d).
#[cfg(test)]
mod confined_probe;

/// Noise reduction through the real `ffmpeg`: the sound keeps its sync and its length (`#[ignore]`d).
#[cfg(test)]
mod denoise_rendered;

/// The golden argv oracle (see its docs): a child module so it reaches the
/// private builders.
#[cfg(test)]
pub(crate) mod golden;

/// The BT.709 label on the four builders, and the plan's mirror of it.
#[cfg(test)]
mod bt709_label;

/// What the export graph draws, pinned against rendered pixels (`#[ignore]`d).
#[cfg(test)]
mod rendered;

#[cfg(test)]
mod export_progress_rendered;

/// A clip's curves, wheels and LUT as filters.
mod grade;
use grade::grade_filters;

/// Stabilization: a clip's frames moved against its camera's shake, and the passes that measure it.
mod stabilize;
pub(crate) use stabilize::measure_motion;
pub use stabilize::vidstab_available;
#[cfg(test)]
pub(crate) use stabilize::with_vidstab;
use stabilize::{stabilizer, Stabilizer};

/// Stabilization rendered by the real ffmpeg (`#[ignore]`d).
#[cfg(test)]
mod stabilize_rendered;

/// A clip composited with a blend mode instead of alpha-over.
mod blend;
/// The tag a blend chain names for its composite (the bottom layer's): the planner mirrors it in `PlanCanvas::blend_tag`.
pub(crate) use blend::composite_matrix as blend_composite_tag;
use blend::{blend_chains, blend_fold, composite_matrix, fold_runs, keeps_clear_bars, Blend, FoldClip, FoldStage, FOLD_MIN};
pub use blend::{blend_colour_graph, blend_probe_graph};

/// Blend modes through the real `ffmpeg`: the pixels of each mode, the composite outside the clip, and `normal`
/// through the blend graph against the plain `overlay` (`#[ignore]`d).
#[cfg(test)]
mod blend_rendered;

/// Which blended clips fold into one fork and join, and what the planner counts of the rest.
#[cfg(test)]
mod blend_fold_tests;

/// A run of blended clips through the real `ffmpeg`, folded and in parts (`#[ignore]`d).
#[cfg(test)]
mod blend_fold_rendered;

#[cfg(test)]
mod blend_fold_checker;

/// A blur as the graph writes it (`gblur`, one step, the same sigma both ways). For a renderer's probe, which runs the very text.
pub fn blur_filter(sigma: f64) -> String {
    format!("gblur=sigma={}", fnum(sigma))
}

/// The [`Fit::Blur`] fill of a picture in a `w`x`h` frame as the graph writes it with the delivery's default scaler (the export's and the
/// still's alike, `label` naming its inner pads), from the filter that takes the picture (`split`) to the `overlay` that lays it over the fill.
/// For a renderer's probe, which runs the very text.
pub fn blur_fill_chain(w: u32, h: u32, label: &str) -> String {
    blur_fill(w, h, "", "", false, label)
}

/// A constant opacity below 1 as the graph writes it (export and still alike): `colorchannelmixer` only takes RGB, so FFmpeg takes
/// the layer `yuva420p -> argb -> yuva420p` around it. For a renderer's probe, which runs the very text.
pub fn opacity_filter(opacity: f64) -> String {
    format!("colorchannelmixer=aa={opacity}")
}

/// The filters of a clip's grade as the graph writes them, in chain order, for frames tagged `frame_matrix`
/// (`None`: untagged): `lut3d`, `lutrgb`, `colorbalance`, and for a tagged picture the `scale` / `format` /
/// `setparams` that bring it back to YCbCr. For a renderer's tests, which run the very text.
/// Reads the disk for a LUT (its copy); empty for an identity grade.
pub fn grade_filter_chain(grade: &Grade, frame_matrix: Option<&str>) -> Vec<String> {
    grade_filters(grade, frame_matrix)
}

/// A clip's live behaviours (sway, breathe, spin, drift, shake) written onto the expressions of its
/// transform numbers.
mod behavior;
use behavior::driven_expr;

/// A still whose picture moves (keyed zoom, position, crop) drawn in one sub-pixel resample of a
/// copy scaled once.
mod still_motion;
use still_motion::StillMotion;

/// The still inside a fade, a dip, a dissolve, a slide or a push: its layers and their fades.
mod still_fx;
use still_fx::{still_layers, StillFades};

/// Animated SVG titles as PNG sequences and stills: drawn before ffmpeg is spawned, overlaid by it.
pub(crate) mod svg_layer;

/// The cut's captions and chapter markers muxed into an export, and the caption
/// files written beside it.
mod side_files;

/// Every delivery frame of a multi-format export in one ffmpeg run.
mod together;

/// A long export rendered in parts, so that what is open at once stays bounded.
mod segments;

#[cfg(test)]
mod segments_tests;

/// An export in parts through the real ffmpeg, the whole export's frames and samples (`#[ignore]`d).
#[cfg(test)]
mod segments_rendered;

/// The one run held to the variants rendered one by one, frame for frame (`#[ignore]`d).
#[cfg(test)]
mod together_rendered;

/// Titles through the real ffmpeg (`#[ignore]`d).
#[cfg(test)]
mod svg_rendered;

/// Titles on tracks: the argv, and through the real ffmpeg (`#[ignore]`d).
#[cfg(test)]
mod title_clips;

/// Which slot a job takes, the cores' or the hardware's.
#[cfg(test)]
mod lane_tests;

/// ffprobe's JSON and ffmpeg's logs, mutated: nothing panics.
#[cfg(test)]
mod hostile_tests;

/// The deadline on a one-shot still's process.
#[cfg(all(test, unix))]
mod output_bounded_tests;

/// The environment every child starts in (`command`): the OpenMP binding variables are removed, the rest is inherited.
#[cfg(all(test, unix))]
mod spawn_env_tests;

/// Children that cannot be waited on for ever: the supervised run, the bounded reap, the
/// partial-output cleanup and the way a failed `silencedetect` is read.
#[cfg(all(test, unix))]
mod supervised_tests;

/// A bounded cache of composited stills, keyed by the argv that would draw each.
mod still_cache;

/// The scrubbed frame: cached, with a window of its neighbours drawn by one process.
mod scrub;
pub use scrub::{ScrubConfig, ScrubFrames, ScrubStats};

/// A contact sheet of the composited cut: a still per moment, tiled.
mod timeline_sheet;
pub use timeline_sheet::{timeline_sheet_cancellable, timeline_sheet_cells, SheetCell, SheetClip};

/// The scrub window held to the export's frames and the still's (`#[ignore]`d).
#[cfg(test)]
mod scrub_rendered;

/// The scrubbed still held to the export's frame at the same moment (`#[ignore]`d).
#[cfg(test)]
mod still_rendered;

/// The sound of detached / linked clips: that `extract_audio` used to double, that a
/// detached clip's *own* chain is the chain it had, and that its **level** survives
/// the move between a picture track's fader and an audio track's (graph level, and
/// measured on a render under `#[ignore]`). Pan, duck and mute are the destination
/// track's afterwards — see `Timeline::detach_audio`.
#[cfg(test)]
mod linked_audio;

/// The export plan against the *evaluated* export graph, on a grid of frames.
#[cfg(test)]
mod sweep;

/// The `fps` pick against the frames the export renders (`#[ignore]`d).
#[cfg(test)]
mod picked;

/// Speed ramps through the real `ffmpeg`: the frames, the scrubbed still, the sound's sync (`#[ignore]`d).
#[cfg(test)]
mod ramp_rendered;

/// The blurred fill through the real `ffmpeg` (`#[ignore]`d).
#[cfg(test)]
mod blur_rendered;

/// A zoom taken from the source in the fit's own resample, and a `Cover` clip that pans across the
/// picture: the chains, and the real `ffmpeg`'s pixels (`#[ignore]`d).
#[cfg(test)]
mod zoom_fit;

/// A source whose video starts late keeps its sync in the export (`#[ignore]`d).
#[cfg(test)]
mod late_start;

/// The master limiter through the real `ffmpeg`, the corpus the preview's port replays (`#[ignore]`d).
#[cfg(test)]
mod alimiter_corpus;

/// A keyframed zoom with every other feature of a clip, measured per output frame
/// against `transform_at` and the scrubbed still (`#[ignore]`d).
#[cfg(test)]
mod keyed_zoom;

/// Live behaviours (sway, breathe, spin, drift, shake) against the pictures the export, the scrubbed still
/// and the playback stream render (`#[ignore]`d).
#[cfg(test)]
mod behavior_rendered;

/// Per-property channels — a keyed colour and a keyed volume — against the pictures and samples
/// the export renders (`#[ignore]`d).
#[cfg(test)]
mod keyed_channels;

/// The motion presets (B8a) through the real `ffmpeg`: every preset renders, and the export agrees
/// with the scrubbed still (`#[ignore]`d).
#[cfg(test)]
mod motion_presets_rendered;

/// A keyed mask and a keyed crop against the pictures the export and the still render (`#[ignore]`d).
#[cfg(test)]
mod keyed_mask_crop;

/// Curves, wheels and a LUT against the pictures the export and the still render (`#[ignore]`d).
#[cfg(test)]
mod grade_rendered;

/// Scene detection and contact sheets on a proxy against the original (`#[ignore]`d).
#[cfg(test)]
mod scenes_on_proxy;

/// A 45 MP still with a slow keyed zoom, pan and crop, measured sub-pixel per output frame at
/// 1080p50 and 2160p50, against time (`#[ignore]`d).
#[cfg(test)]
mod still_motion_rendered;

#[cfg(test)]
mod tests;
