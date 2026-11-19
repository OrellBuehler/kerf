//! Running an export: its progress, its cancel, the hardware fallback and the passes.

use super::*;

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
    pub(super) fn queued(fraction: f64, elapsed_secs: f64, why: &'static str) -> Self {
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
pub(super) fn discarding_partial(output: &Path, render: impl FnOnce() -> Result<RenderStatus>) -> Result<RenderStatus> {
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
pub(super) fn measure_static_loudness(
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
pub(super) fn meters_loudness_first(opts: &ExportOptions, has_audio: bool) -> bool {
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
pub(super) fn retry_export_in_software(
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
pub(super) fn tune_export_args(args: &mut Vec<String>, assets: &[Asset], os: HostOs) -> bool {
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
pub(super) fn filter_names(graph: &str) -> Vec<&str> {
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
pub(super) fn export_resource(args: &[String], facts_of: &dyn Fn(&str) -> InputFacts, lane_on: bool) -> cpu::Resource {
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
pub(super) fn set_vaapi_device(args: &mut [String], device: Option<&str>) {
    let (Some(device), Some(at)) = (device, args.iter().position(|a| a == "-vaapi_device")) else {
        return;
    };
    if let Some(value) = args.get_mut(at + 1) {
        *value = device.to_string();
    }
}

/// What an export is refused for before anything is drawn or spawned — an empty timeline, options
/// that would export nothing or that the container cannot hold — and whether it has picture and
/// sound. Asked ahead of the animated titles' drawing so a bad request does not spend seconds of the
/// cores on pictures nobody will use.
pub(super) fn export_preflight(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Result<(bool, bool)> {
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
pub(super) fn check_export_luts(timeline: &Timeline) -> Result<()> {
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
pub(super) struct PassLogs(pub(super) String);

impl Drop for PassLogs {
    fn drop(&mut self) {
        for suffix in ["-0.log", "-0.log.mbtree", ".log", ".log.mbtree"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}
