//! Hardware decode and encode: what this machine offers and what the runs are asked for.

use super::*;

/// The decode hardware-acceleration to request for preview frames. Defaults to
/// ffmpeg's `auto` (VAAPI / CUDA / VideoToolbox, falls back to software when none
/// is usable), which offloads 4K decode off the CPU; on Windows `auto` would pick
/// DXVA2, so a run turns it into `d3d11va` per input where the codec allows
/// (`decode_policy`). Set `KERF_HWACCEL=none` (or empty) to force software
/// decoding, or a name (`cuda`, `qsv`, ...) to use exactly that.
pub(super) fn hwaccel() -> Option<String> {
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
pub(super) static HWACCEL_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Cleared the first time a hardware *encode* (the Insta360 stitch) fails while the
/// software retry succeeds, so a broken GPU encoder doesn't double every later
/// stitch. Proxies never encode in hardware, and an export's encoder is the user's.
pub(super) static HW_ENCODE_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Set only when a failure that read as the hardware's (`looks_like_hardware_decode_failure`) was
/// cured by a software rerun of a proxy or an export, where [`HWACCEL_OK`] is cleared by *any*
/// rescue of a still, a frame grab, the stream, an analysis or a filmstrip. What an export's `auto`
/// is dropped on: one spurious preview rescue must not send every later export to software.
pub(super) static HWACCEL_PROVEN_BAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Turn hardware decode off for the process on a conviction that read as the hardware's: the
/// preview paths' learned fallback ([`disable_decode_hwaccel`]) and the export's
/// ([`HWACCEL_PROVEN_BAD`]).
pub(super) fn convict_hardware_decode() {
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
pub(super) fn path_facts(path: &Path) -> InputFacts {
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
pub(super) fn decode_hwaccel_for(path: &Path, reading: Reading) -> Option<String> {
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
pub(in crate::engine) fn decode_hwaccel_for_asset(asset: &Asset) -> Option<String> {
    hwaccel_for_asset(decode_hwaccel(), HostOs::CURRENT, asset)
}

/// [`decode_hwaccel_for_asset`] for a given request and operating system (pure).
pub(super) fn hwaccel_for_asset(requested: Option<String>, os: HostOs, asset: &Asset) -> Option<String> {
    let facts = facts_from_assets(std::slice::from_ref(asset))(&asset.path);
    resolve_hwaccel(os, &requested?, facts, Reading::Whole)
}

/// Record that an accelerated decode failed where the software retry of the very
/// same input worked, so every later preview-class decode skips the accelerated
/// attempt (see [`decode_hwaccel`]). For background decodes living outside this
/// file that run their own retry.
pub(in crate::engine) fn disable_hwaccel() {
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
pub(super) const HW_ENCODER_CANDIDATES: [&str; 11] = [
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
pub(super) const VAAPI_ENCODER_CANDIDATES: [&str; 3] = ["h264_vaapi", "hevc_vaapi", "av1_vaapi"];

/// The render node a VAAPI export names in its argv. The builder is pure, so this
/// is a stand-in: the process that spawns it swaps in the node the probe found
/// (`set_vaapi_device`, from `tune_export_args`).
pub(super) const VAAPI_DEFAULT_DEVICE: &str = "/dev/dri/renderD128";

/// Render nodes tried for VAAPI before giving up (a laptop has two, a workstation
/// rarely more); each costs up to [`HW_PROBE_TIMEOUT`] should its driver hang.
pub(super) const VAAPI_DEVICES_TRIED: usize = 4;

/// How long one candidate gets to encode a single frame.
const HW_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// How long the `-encoders` listing gets. Longer than a test encode: the first launch of a
/// freshly installed ffmpeg can sit under a virus scan, and an answer given up on is cached for
/// the whole process as "no hardware encoders".
const HW_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long an export waits for the probe to name the VAAPI device when nothing has probed yet.
pub(super) const VAAPI_DEVICE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What the one-per-process hardware encoder probe found.
pub(super) struct HwProbe {
    encoders: Vec<String>,
    pub(super) vaapi_device: Option<String>,
}

static HW_PROBE: OnceLock<HwProbe> = OnceLock::new();

pub(super) fn hw_probe() -> &'static HwProbe {
    HW_PROBE.get_or_init(detect_hw_encoders)
}

/// The value of `cell` once something has set it, waiting up to `limit` (unit-tested over a
/// local cell).
pub(super) fn wait_for<T>(cell: &OnceLock<T>, limit: std::time::Duration) -> Option<&T> {
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
pub(super) fn vaapi_device_within(limit: std::time::Duration) -> Option<&'static str> {
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
pub(super) enum Probe {
    Works,
    Refused,
    /// Still running at [`HW_PROBE_TIMEOUT`] and killed: the driver wedged.
    Hung,
}

/// The test encode for `encoder` (pure): one 256x256 frame — enough to clear every
/// family's minimum-dimension floor — to the null muxer. System-memory encoders
/// take `nv12`, which they all accept; a VAAPI one is uploaded to `vaapi_device`
/// first, the way an export hands it frames.
pub(super) fn encoder_probe_args(encoder: &str, vaapi_device: Option<&str>) -> Vec<String> {
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
pub(super) fn run_probe(bin: &str, args: &[String], limit: std::time::Duration) -> Probe {
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
pub(super) fn probe_family(
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
pub(super) fn render_nodes(dir: &Path, forced: Option<String>) -> Vec<String> {
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
pub(super) fn probe_vaapi(
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
