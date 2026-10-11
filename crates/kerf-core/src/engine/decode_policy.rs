//! Which hardware decoder an input of an ffmpeg run is asked for, and how many threads a
//! one-frame read of a preview proxy takes.
//!
//! Decided where the argv is **spawned**, never in the graph builders: those keep emitting
//! `-hwaccel` exactly as the options say (the golden oracle pins that), and the run that is
//! about to start rewrites the flags per input from what is known of the file behind each `-i`.
//! The numbers behind the rules are measured, on an AMD GPU under Windows and without any GPU
//! here:
//!
//! * A hardware decoder is a *session*: creating one costs 70 to 80 ms a process (DXVA2 and
//!   D3D11VA alike) and, with none to be had, `-hwaccel auto` still probes for devices. A
//!   preview proxy is a 720p all-intra H.264 whose frame decodes in under 3 ms, so a scrubbed
//!   still that asks for hardware spends most of its time starting it, and a sustained 240-frame
//!   stream from one is faster in software too. **A proxy is never decoded in hardware.**
//! * The decoder and filter thread pools are created per process: for one frame from a proxy
//!   they are a quarter of the run. **A one-frame read of proxies runs on one thread.**
//! * `-hwaccel auto` on Windows resolves to DXVA2, the first of the decoder's hardware
//!   configurations. **D3D11VA is faster** (4K stream 1.89 s against 1.47 s, decode-only CPU
//!   1.53 s against 0.39 s, export 8 to 10 % shorter), so it is asked for by name where the
//!   codec is one it decodes everywhere.
//! * A device that cannot decode the stream's chroma format falls back to software *after* the
//!   session was created, on every spawn: 4:2:2 is not decoded by AMD's or (before the newest
//!   generation) NVIDIA's decoders, and a one-frame scrub of 4K 4:2:2 was 43 % slower for the
//!   attempt (1.5 s against 1.05 s). A one-frame read does not make it; a long read still does,
//!   because the devices that can (Intel's recent iGPUs, Apple's media engines) gain minutes
//!   there and the failed attempt costs 80 ms once.

use crate::clip_timing::is_cache_proxy;
use crate::model::{pix_fmt_has_alpha, pix_fmt_layout, Asset, PixLayout, StreamKind};

use super::cpu;

/// The operating system the engine runs on, as far as decoding is concerned. A value, not a
/// `cfg`, so every rule is tested on every platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostOs {
    Windows,
    MacOs,
    /// Linux and everything else: `-hwaccel auto` picks among VAAPI, CUDA and VDPAU.
    Other,
}

impl HostOs {
    pub(crate) const CURRENT: HostOs = if cfg!(windows) {
        HostOs::Windows
    } else if cfg!(target_os = "macos") {
        HostOs::MacOs
    } else {
        HostOs::Other
    };
}

/// The video codec of an input, coarsely: only whether a platform decoder is known to exist for
/// it everywhere it matters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Codec {
    /// Not recorded or not probed.
    #[default]
    Unknown,
    H264,
    Hevc,
    Other,
}

impl Codec {
    /// From ffprobe's `codec_name`.
    pub(crate) fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "" => Codec::Unknown,
            "h264" => Codec::H264,
            "hevc" | "h265" => Codec::Hevc,
            _ => Codec::Other,
        }
    }
}

/// The chroma format of an input, coarsely: 4:2:0 (8 or 10 bit) is what every GPU decodes,
/// anything else (4:2:2, 4:4:4, 12 bit and up, alpha) is what most do not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Chroma {
    /// Not recorded, not probed, or a name nobody here knows: nothing is assumed.
    #[default]
    Unknown,
    Yuv420,
    Other,
}

impl Chroma {
    /// From ffprobe's `pix_fmt`.
    pub(crate) fn from_pix_fmt(name: &str) -> Self {
        if pix_fmt_has_alpha(name) {
            return Chroma::Other;
        }
        match pix_fmt_layout(name) {
            Some(PixLayout::Yuv420) => Chroma::Yuv420,
            Some(_) => Chroma::Other,
            None => Chroma::Unknown,
        }
    }
}

/// What a run reads from an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reading {
    /// One frame, then the process ends: a scrubbed still, a frame grab, a cover.
    Frame,
    /// A long-lived process feeding the preview.
    Stream,
    /// The whole file: an export, a proxy, an analysis pass.
    Whole,
}

/// What the argv alone does not say about the file behind an input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputFacts {
    /// A preview proxy (`is_cache_proxy`): small and all-intra.
    pub proxy: bool,
    /// A still image: no decoding to speak of.
    pub image: bool,
    pub codec: Codec,
    pub chroma: Chroma,
}

impl InputFacts {
    /// Facts of a path and nothing else: all that is known of an input that is not in the
    /// project's asset list.
    pub(crate) fn of_path(path: &str) -> Self {
        InputFacts {
            proxy: is_cache_proxy(path),
            ..InputFacts::default()
        }
    }

    /// An input that takes next to nothing to decode.
    fn cheap(self) -> bool {
        self.proxy || self.image
    }
}

/// The facts of an input by its path, from the asset list the argv was built from. A path that
/// is in no asset has the facts of its name alone.
pub(crate) fn facts_from_assets(assets: &[Asset]) -> impl Fn(&str) -> InputFacts + '_ {
    move |path| {
        let Some(asset) = assets.iter().find(|a| a.path == path) else {
            return InputFacts::of_path(path);
        };
        let video = asset.streams.iter().find(|s| s.kind == StreamKind::Video);
        InputFacts {
            proxy: is_cache_proxy(path),
            image: asset.is_image(),
            codec: video.map_or(Codec::Unknown, |s| Codec::from_name(&s.codec)),
            chroma: video
                .and_then(|s| s.pix_fmt.as_deref())
                .map_or(Chroma::Unknown, Chroma::from_pix_fmt),
        }
    }
}

/// The `-hwaccel` to hand ffmpeg for an input, given the one that was asked for (`None` for
/// none). Pure, so every rule is a test.
///
/// * A proxy or an image gets none, whatever was asked.
/// * A name other than `auto` (`KERF_HWACCEL=cuda`, an MCP export's `vaapi`) is the user's
///   choice and goes through as it is.
/// * `auto` on Windows becomes `d3d11va` for H.264 and HEVC, which every DirectX 11 GPU decodes
///   and for which `auto` would have chosen DXVA2; for any other codec it stays `auto`, so a
///   codec with no D3D11VA decoder (ProRes, libdav1d's AV1) is not handed one and failed.
/// * `auto` for a one-frame read of a chroma format the platform's decoders mostly lack (every
///   platform but macOS, see the module docs) is none.
pub(crate) fn resolve_hwaccel(os: HostOs, requested: &str, facts: InputFacts, reading: Reading) -> Option<String> {
    let requested = requested.trim();
    if requested.is_empty() || requested.eq_ignore_ascii_case("none") || facts.cheap() {
        return None;
    }
    if !requested.eq_ignore_ascii_case("auto") {
        return Some(requested.to_string());
    }
    if reading == Reading::Frame && facts.chroma == Chroma::Other && os != HostOs::MacOs {
        return None;
    }
    match (os, facts.codec) {
        (HostOs::Windows, Codec::H264 | Codec::Hevc) => Some("d3d11va".to_string()),
        _ => Some("auto".to_string()),
    }
}

/// Whether anything beyond an input's name can change what [`resolve_hwaccel`] says, so that a
/// decode handed a bare path needs to learn its codec and chroma format (a probe) at all: only
/// for `auto`, on Windows (the codec picks `d3d11va`) and on a one-frame read everywhere but macOS
/// (the chroma format can skip the hardware). Anywhere else the answer is the same unprobed.
pub(crate) fn facts_matter(os: HostOs, requested: &str, reading: Reading) -> bool {
    requested.trim().eq_ignore_ascii_case("auto") && (os == HostOs::Windows || (reading == Reading::Frame && os != HostOs::MacOs))
}

/// Whether the text of a failed run (the tail of its stderr, which an error carries) says the
/// hardware decoder could not be had, rather than the render failing for its own reasons: a named
/// accelerator with no device behind it exits non-zero at start where `auto` fell back quietly.
/// Used to decide whether a software rerun that works convicts the hardware (turns it off for the
/// process). Only phrases that are fatal on their own count: an `auto` run that goes on to succeed
/// prints `Device creation failed` for each device it probed, `Using auto hwaccel type vulkan` and
/// `Failed setup for format ...: hwaccel initialisation returned error`, so a run that then fails
/// for another reason (an output directory that is not there, a full disk) carries all three and
/// is not the hardware's. ffmpeg 9.0.2 ends a run with no device for a named accelerator with
/// `No device available for decoder: device type d3d11va needed for codec h264.` and `Hardware
/// device setup failed for decoder: ...`.
///
/// The pinned build also loads the hardware libraries lazily through `implib-gen` shims, which
/// **abort the process** (SIGABRT) when one is missing: on a Linux machine without `libva-drm.so.2`
/// every `-hwaccel auto` and `-hwaccel vaapi` run dies with `implib-gen: libva-drm.so.2: failed to
/// load library ... dlopen: ...` and an assertion. That is fatal and the hardware's, but only for
/// the hardware libraries (a missing `libpulse` is not) — and unlike `Cannot load libcuda.so.1`,
/// which a working `auto` run prints, it ends the run.
pub(crate) fn looks_like_hardware_decode_failure(text: &str) -> bool {
    const MARKS: [&str; 8] = [
        "no device available for decoder",
        "hardware device setup failed",
        "unrecognized hwaccel",
        "unrecognized option 'hwaccel'",
        "for option 'hwaccel'",
        "invalid hwaccel device",
        "requires hwaccel",
        "doesn't support hardware accelerated",
    ];
    const HARDWARE_LIBRARIES: [&str; 8] = [
        "libva",
        "libvdpau",
        "libdrm",
        "libcuda",
        "libnvcuvid",
        "libmfx",
        "libvpl",
        "libamf",
    ];
    let text = text.to_ascii_lowercase();
    MARKS.iter().any(|m| text.contains(m))
        || text.lines().any(|l| {
            l.contains("implib-gen:")
                && l.contains("failed to load library")
                && HARDWARE_LIBRARIES.iter().any(|lib| l.contains(lib))
        })
}

/// The `-hwaccel` an export is asked for, given what the process knows about hardware decode:
/// `env_allows` is false under `KERF_HWACCEL=none` (or empty), `proven_bad` once a failure that
/// read as the hardware's (`looks_like_hardware_decode_failure`) was cured by a software rerun.
/// Either way the symbolic `auto` — the GUI's default, which names no device — is dropped, so a
/// machine whose decoder is broken does not launch and fail again for every export. A named
/// accelerator is the caller's choice and is never touched. Deliberately **not** keyed on the
/// preview paths' learned flag (`decode_hwaccel()`), which any rescue of a still or a stream
/// turns off, whatever failed: one spurious preview rescue must not send every export to software.
pub(crate) fn export_hwaccel_request(requested: Option<&str>, env_allows: bool, proven_bad: bool) -> Option<String> {
    match requested {
        Some(r) if (!env_allows || proven_bad) && r.trim().eq_ignore_ascii_case("auto") => None,
        other => other.map(str::to_string),
    }
}

/// What [`tune_inputs`] left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tuning {
    /// Some input still carries a `-hwaccel`: a failure of the run may be the hardware's.
    pub hw: bool,
    /// Every input is a proxy or an image (or there are none): nothing in the run needs
    /// more than one thread.
    pub all_cheap: bool,
}

/// Rewrite the per-input options of a built argv for the run that is about to spawn it: each
/// input's `-hwaccel` becomes what [`resolve_hwaccel`] says (or goes), and a proxy read for one
/// frame gets its decoder `-threads 1`. `facts_of` says what is behind each `-i`.
///
/// Options belong to the input they precede, so the argv is walked in groups: everything up to
/// and including a `-i <path>`. What follows the last input (the graph, the sink) is untouched.
pub(crate) fn tune_inputs(args: &mut Vec<String>, os: HostOs, reading: Reading, facts_of: &dyn Fn(&str) -> InputFacts) -> Tuning {
    let mut tuning = Tuning {
        hw: false,
        all_cheap: true,
    };
    let mut out: Vec<String> = Vec::with_capacity(args.len() + 4);
    let mut group: Vec<String> = Vec::new();
    let mut rest = std::mem::take(args).into_iter();
    while let Some(arg) = rest.next() {
        if arg != "-i" {
            group.push(arg);
            continue;
        }
        let Some(path) = rest.next() else {
            group.push(arg);
            break;
        };
        let facts = facts_of(&path);
        tuning.all_cheap &= facts.cheap();
        if let Some(at) = group.iter().rposition(|a| a == "-hwaccel").filter(|at| at + 1 < group.len()) {
            match resolve_hwaccel(os, &group[at + 1], facts, reading) {
                Some(hw) => {
                    group[at + 1] = hw;
                    tuning.hw = true;
                }
                None => {
                    group.drain(at..at + 2);
                }
            }
        }
        if reading == Reading::Frame && facts.proxy {
            group.extend(["-threads".to_string(), "1".to_string()]);
        }
        out.append(&mut group);
        out.push(arg);
        out.push(path);
    }
    out.append(&mut group);
    out.extend(rest);
    *args = out;
    tuning
}

/// Cap an argv that reads one frame and was built from paths alone — the GPU compositor's
/// per-layer decodes: proxies only, and the run is one thread everywhere; otherwise the
/// budget's threads shared among `share` such processes ([`cpu::limit_args_shared`]).
pub fn limit_frame_args(args: &mut Vec<String>, share: usize) {
    let tuning = tune_inputs(args, HostOs::CURRENT, Reading::Frame, &InputFacts::of_path);
    if tuning.all_cheap {
        cpu::single_frame_args(args);
    } else {
        cpu::limit_args_shared(args, share);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{image_stream, test_asset, video_stream};

    const PROXY: &str = "/cache/kerf/proxies/0123456789abcdef.mp4";

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn facts(codec: Codec, chroma: Chroma) -> InputFacts {
        InputFacts {
            codec,
            chroma,
            ..InputFacts::default()
        }
    }

    const OSES: [HostOs; 3] = [HostOs::Windows, HostOs::MacOs, HostOs::Other];
    const READINGS: [Reading; 3] = [Reading::Frame, Reading::Stream, Reading::Whole];

    #[test]
    fn nothing_requested_is_nothing_asked_for() {
        for os in OSES {
            for reading in READINGS {
                for requested in ["", "  ", "none", "NONE", "None"] {
                    assert_eq!(
                        resolve_hwaccel(os, requested, facts(Codec::H264, Chroma::Yuv420), reading),
                        None,
                        "{requested:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_proxy_is_never_decoded_in_hardware_however_it_is_asked_for() {
        let proxy = InputFacts {
            proxy: true,
            ..facts(Codec::H264, Chroma::Yuv420)
        };
        for os in OSES {
            for reading in READINGS {
                for requested in ["auto", "d3d11va", "cuda", "vaapi", "videotoolbox"] {
                    assert_eq!(resolve_hwaccel(os, requested, proxy, reading), None, "{os:?} {requested}");
                }
            }
        }
        let image = InputFacts {
            image: true,
            ..InputFacts::default()
        };
        assert_eq!(resolve_hwaccel(HostOs::Other, "auto", image, Reading::Frame), None);
    }

    #[test]
    fn a_named_decoder_is_the_users_choice_and_goes_through() {
        let odd = facts(Codec::Other, Chroma::Other);
        for os in OSES {
            for reading in READINGS {
                assert_eq!(resolve_hwaccel(os, "cuda", odd, reading), Some("cuda".to_string()));
                assert_eq!(resolve_hwaccel(os, " qsv ", odd, reading), Some("qsv".to_string()));
                assert_eq!(resolve_hwaccel(os, "dxva2", odd, reading), Some("dxva2".to_string()));
            }
        }
    }

    #[test]
    fn windows_asks_for_d3d11va_by_name_where_it_is_known_to_decode_the_codec() {
        for reading in READINGS {
            for codec in [Codec::H264, Codec::Hevc] {
                assert_eq!(
                    resolve_hwaccel(HostOs::Windows, "auto", facts(codec, Chroma::Yuv420), reading),
                    Some("d3d11va".to_string())
                );
                assert_eq!(
                    resolve_hwaccel(HostOs::Windows, "AUTO", facts(codec, Chroma::Unknown), reading),
                    Some("d3d11va".to_string())
                );
            }
            // A codec D3D11VA has no decoder for, or one nobody probed, stays on `auto`, which
            // falls back to software without failing the run.
            for codec in [Codec::Other, Codec::Unknown] {
                assert_eq!(
                    resolve_hwaccel(HostOs::Windows, "auto", facts(codec, Chroma::Yuv420), reading),
                    Some("auto".to_string())
                );
            }
        }
        // Everywhere else `auto` is already the right choice.
        for os in [HostOs::MacOs, HostOs::Other] {
            assert_eq!(
                resolve_hwaccel(os, "auto", facts(Codec::Hevc, Chroma::Yuv420), Reading::Whole),
                Some("auto".to_string())
            );
        }
    }

    #[test]
    fn a_one_frame_read_skips_a_chroma_format_the_decoders_mostly_lack() {
        // 4:2:2 10-bit HEVC (a mirrorless camera): no AMD or NVIDIA decoder, so the attempt is
        // all cost on Windows and Linux.
        let c422 = facts(Codec::Hevc, Chroma::Other);
        for os in [HostOs::Windows, HostOs::Other] {
            assert_eq!(resolve_hwaccel(os, "auto", c422, Reading::Frame), None, "{os:?}");
        }
        // Apple's media engines decode it, and the macOS `auto` is VideoToolbox.
        assert_eq!(
            resolve_hwaccel(HostOs::MacOs, "auto", c422, Reading::Frame),
            Some("auto".to_string())
        );
        // A long read still tries, where Intel's iGPUs and the newest GPUs gain minutes.
        for os in OSES {
            for reading in [Reading::Stream, Reading::Whole] {
                assert!(resolve_hwaccel(os, "auto", c422, reading).is_some(), "{os:?} {reading:?}");
            }
        }
        // 4:2:0 and an unrecorded format are tried.
        for chroma in [Chroma::Yuv420, Chroma::Unknown] {
            assert!(resolve_hwaccel(HostOs::Other, "auto", facts(Codec::H264, chroma), Reading::Frame).is_some());
        }
    }

    /// ffmpeg 9.0.2 at `-loglevel info`, captured here (the `[vi..#0:0/h264 @ ..]` stream tag
    /// trimmed): `-hwaccel auto` probes devices that are not there, picks one, and the run then
    /// dies on an output directory that does not exist.
    const AUTO_THEN_OUTPUT_FAILS: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'a.mp4':\n\
        [CUDA @ 0x55fafc43d500] Cannot load libcuda.so.1\n\
        [CUDA @ 0x55fafc43d500] Could not dynamically load CUDA\n\
        Device creation failed: -1.\n\
        Device creation failed: -542398533.\n\
        [dec:h264 @ 0x55fafc43dcc0] Using auto hwaccel type vulkan with new default device.\n\
        [out#0/mp4 @ 0x55fafc44a840] Error opening output /nonexistent-dir/out.mp4: No such file or directory\n\
        Error opening output file /nonexistent-dir/out.mp4.\n\
        Error opening output files: No such file or directory";

    /// The same, at `-loglevel info`, for `-hwaccel d3d11va` with no device behind it.
    const NAMED_WITHOUT_A_DEVICE: &str = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'a.mp4':\n\
        Device creation failed: -12.\n\
        [dec:h264 @ 0x55de1abe1440] No device available for decoder: device type d3d11va needed for codec h264.\n\
        [dec:h264 @ 0x55de1abe1440] Hardware device setup failed for decoder: Cannot allocate memory\n\
        Error opening output file -.\n\
        Error opening output files: Cannot allocate memory";

    /// The pinned ffmpeg 9.0.2 on a GitHub runner with no `libva-drm.so.2` (CI, `-hwaccel auto` and
    /// `-hwaccel vaapi` alike): the shim aborts the process.
    const MISSING_HARDWARE_LIBRARY: &str = "[CUDA @ 0x55e5dfa45a40] Cannot load libcuda.so.1\n\
        [CUDA @ 0x55e5dfa45a40] Could not dynamically load CUDA\n\
        Device creation failed: -1.\n\
        implib-gen: libva-drm.so.2: failed to load library 'libva-drm.so.2' via dlopen: libva-drm.so.2: cannot open shared object file: No such file or directory\n\
        ffmpeg-x86_64-unknown-linux-gnu: libva-drm.so.2.init.c:122: load_library: Assertion `0 && \"Assertion in generated code\"' failed.";

    #[test]
    fn a_missing_device_reads_as_the_hardwares_failure_and_nothing_else_does() {
        for text in [
            NAMED_WITHOUT_A_DEVICE,
            MISSING_HARDWARE_LIBRARY,
            "ffmpeg exited with signal: 6 (SIGABRT): implib-gen: libva.so.2: failed to load library 'libva.so.2'",
            "implib-gen: libvdpau.so.1: failed to load library 'libvdpau.so.1' via dlopen: not found",
            "ffmpeg exited with exit status: 244: Device creation failed: -12.\n\
             [dec:h264 @ 0x55] No device available for decoder: device type vaapi needed for codec h264.\n\
             [dec:h264 @ 0x55] Hardware device setup failed for decoder: Generic error in an external library",
            "Unrecognized hwaccel: nonesuch.\nSupported hwaccels: cuda vaapi qsv",
            "Failed to set value 'd3d11va' for option 'hwaccel': Option not found",
            "Unrecognized option 'hwaccel'.",
            "Format vulkan requires hwaccel vulkan initialisation.",
            "Your platform doesn't support hardware accelerated AV1 decoding.",
            "NO DEVICE AVAILABLE FOR DECODER: device type cuda",
        ] {
            assert!(looks_like_hardware_decode_failure(text), "{text}");
        }
        // The render's own trouble, or a hang: rerunning in software may cure it, but the
        // hardware was not what failed. The first is a real run: an `auto` that probed devices
        // that are not there, then could not open its output.
        for text in [
            AUTO_THEN_OUTPUT_FAILS,
            "[CUDA @ 0x55] Cannot load libcuda.so.1\nDevice creation failed: -1.",
            "[h264 @ 0x55] Failed setup for format vulkan: hwaccel initialisation returned error.\n\
             Error opening output file /out/a.mp4.\nNo space left on device",
            "",
            "ffmpeg stopped reporting progress for 300s and was stopped: frame= 120",
            "ffmpeg exited with exit status: 1: Error opening output file /out/a.mp4.\nPermission denied",
            "[mov,mp4 @ 0x55] moov atom not found\nInvalid data found when processing input",
            "Error while decoding stream #0:0: Invalid data found when processing input",
            "Conversion failed!",
            "ffmpeg exited with exit status: 234: No space left on device",
            "timeline has no clips to export",
            // A shim for a library that is not the hardware's, and the benign `auto` probing line
            // that is not a shim at all.
            "implib-gen: libpulse.so.0: failed to load library 'libpulse.so.0' via dlopen: not found",
            "[CUDA @ 0x55e5dfa45a40] Cannot load libcuda.so.1\nCould not dynamically load CUDA",
        ] {
            assert!(!looks_like_hardware_decode_failure(text), "{text}");
        }
    }

    #[test]
    fn an_export_stops_asking_for_auto_when_hardware_is_off_or_proven_broken_but_not_for_a_name() {
        // Hardware allowed and not proven broken: the request goes through as it is.
        for requested in [None, Some("auto"), Some("cuda"), Some("none"), Some("")] {
            assert_eq!(export_hwaccel_request(requested, true, false).as_deref(), requested);
        }
        // `KERF_HWACCEL=none`, or a conviction: only the symbolic `auto` (in any spelling) goes.
        for (env_allows, proven_bad) in [(false, false), (true, true), (false, true)] {
            let request = |r| export_hwaccel_request(r, env_allows, proven_bad);
            assert_eq!(request(Some("auto")), None);
            assert_eq!(request(Some(" AUTO ")), None);
            assert_eq!(request(Some("d3d11va")).as_deref(), Some("d3d11va"));
            assert_eq!(request(Some("vaapi")).as_deref(), Some("vaapi"));
            assert_eq!(request(None), None);
        }
    }

    #[test]
    fn a_file_is_probed_only_where_what_it_holds_changes_the_answer() {
        let probed = |os, requested, reading| facts_matter(os, requested, reading);
        // Windows names the decoder by codec, whatever is read.
        for reading in READINGS {
            assert!(probed(HostOs::Windows, "auto", reading), "{reading:?}");
        }
        // Linux only skips hardware for a chroma format, and only on one frame.
        assert!(probed(HostOs::Other, "auto", Reading::Frame));
        assert!(!probed(HostOs::Other, "auto", Reading::Stream));
        assert!(!probed(HostOs::Other, "auto", Reading::Whole));
        // macOS decides on neither.
        for reading in READINGS {
            assert!(!probed(HostOs::MacOs, "auto", reading), "{reading:?}");
        }
        // A named decoder goes through as it is, and nothing asked for needs nothing.
        for os in OSES {
            for reading in READINGS {
                for requested in ["cuda", "d3d11va", "none", ""] {
                    assert!(!probed(os, requested, reading), "{os:?} {requested:?}");
                }
            }
        }
        assert!(probed(HostOs::Windows, " AUTO ", Reading::Whole));
    }

    #[test]
    fn codec_and_chroma_come_from_ffprobes_names() {
        assert_eq!(Codec::from_name("h264"), Codec::H264);
        assert_eq!(Codec::from_name("HEVC"), Codec::Hevc);
        assert_eq!(Codec::from_name("h265"), Codec::Hevc);
        assert_eq!(Codec::from_name("prores"), Codec::Other);
        assert_eq!(Codec::from_name("av1"), Codec::Other);
        assert_eq!(Codec::from_name(""), Codec::Unknown);

        for name in ["yuv420p", "yuvj420p", "yuv420p10le", "nv12", "p010le"] {
            assert_eq!(Chroma::from_pix_fmt(name), Chroma::Yuv420, "{name}");
        }
        for name in [
            "yuv422p",
            "yuv422p10le",
            "yuv444p",
            "yuv444p10le",
            "yuv420p12le",
            "yuva420p",
            "rgb24",
            "gray",
        ] {
            assert_eq!(Chroma::from_pix_fmt(name), Chroma::Other, "{name}");
        }
        assert_eq!(Chroma::from_pix_fmt("something_new"), Chroma::Unknown);
        assert_eq!(Chroma::from_pix_fmt(""), Chroma::Unknown);
    }

    #[test]
    fn facts_come_from_the_asset_behind_the_path() {
        let mut video = test_asset(vec![video_stream(3840, 2160, 30.0)]);
        video.path = "/media/a.mp4".into();
        video.streams[0].codec = "hevc".into();
        video.streams[0].pix_fmt = Some("yuv422p10le".into());
        let mut still = test_asset(vec![image_stream(100, 100)]);
        still.path = "/media/a.png".into();
        let assets = [video, still];
        let of = facts_from_assets(&assets);
        assert_eq!(
            of("/media/a.mp4"),
            InputFacts {
                codec: Codec::Hevc,
                chroma: Chroma::Other,
                ..InputFacts::default()
            }
        );
        assert!(of("/media/a.png").image);
        // A path no asset has is read by its name: a proxy is one.
        assert!(of(PROXY).proxy);
        assert_eq!(of("/media/b.mp4"), InputFacts::default());
        // And a proxy swapped in for an asset keeps the original's metadata but is still a proxy.
        let mut swapped = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        swapped.path = PROXY.into();
        let swapped = [swapped];
        assert!(facts_from_assets(&swapped)(PROXY).proxy);
    }

    fn opts(hw: &str, input: &str, flags: &str) -> Vec<String> {
        args(&format!(
            "-hide_banner -loglevel error -hwaccel {hw} {flags} -i {input} -filter_complex [0:v]null[outv] -map [outv] -frames:v 1 pipe:1"
        ))
    }

    #[test]
    fn a_proxy_still_loses_its_hwaccel_and_decodes_on_one_thread() {
        let mut a = opts("auto", PROXY, "-ss 3.000000");
        let t = tune_inputs(&mut a, HostOs::Other, Reading::Frame, &InputFacts::of_path);
        assert_eq!(
            a.join(" "),
            format!(
                "-hide_banner -loglevel error -ss 3.000000 -threads 1 -i {PROXY} -filter_complex [0:v]null[outv] -map [outv] -frames:v 1 pipe:1"
            )
        );
        assert_eq!(
            t,
            Tuning {
                hw: false,
                all_cheap: true
            }
        );
    }

    #[test]
    fn a_proxy_in_the_stream_loses_its_hwaccel_but_keeps_its_threads() {
        let mut a = opts("auto", PROXY, "");
        let t = tune_inputs(&mut a, HostOs::Windows, Reading::Stream, &InputFacts::of_path);
        assert!(
            !a.contains(&"-hwaccel".to_string()) && !a.contains(&"-threads".to_string()),
            "{a:?}"
        );
        assert!(!t.hw);
    }

    #[test]
    fn an_original_keeps_its_hwaccel_and_windows_names_the_decoder() {
        let original = |_: &str| facts(Codec::Hevc, Chroma::Yuv420);
        let mut a = opts("auto", "/media/a.mp4", "-ss 3.000000");
        let t = tune_inputs(&mut a, HostOs::Windows, Reading::Frame, &original);
        assert_eq!(
            a.join(" "),
            "-hide_banner -loglevel error -hwaccel d3d11va -ss 3.000000 -i /media/a.mp4 -filter_complex [0:v]null[outv] -map [outv] -frames:v 1 pipe:1"
        );
        assert_eq!(
            t,
            Tuning {
                hw: true,
                all_cheap: false
            }
        );
        let mut b = opts("auto", "/media/a.mp4", "");
        tune_inputs(&mut b, HostOs::Other, Reading::Frame, &original);
        assert_eq!(b, opts("auto", "/media/a.mp4", ""));
    }

    #[test]
    fn a_422_original_loses_its_hwaccel_for_one_frame_and_keeps_it_for_a_render() {
        let c422 = |_: &str| facts(Codec::Hevc, Chroma::Other);
        let mut still = opts("auto", "/media/r5.mp4", "-ss 1.5");
        let t = tune_inputs(&mut still, HostOs::Windows, Reading::Frame, &c422);
        assert!(!still.contains(&"-hwaccel".to_string()), "{still:?}");
        assert_eq!(
            t,
            Tuning {
                hw: false,
                all_cheap: false
            }
        );
        let mut export = opts("auto", "/media/r5.mp4", "-ss 1.5");
        let t = tune_inputs(&mut export, HostOs::Windows, Reading::Whole, &c422);
        assert_eq!(flag_after(&export, "-hwaccel"), Some("d3d11va"));
        assert!(t.hw);
    }

    fn flag_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    }

    #[test]
    fn options_stay_with_the_input_they_precede() {
        // A proxy and an original side by side: only the proxy is stripped and threaded, the
        // original keeps its seek and its decoder, and the graph after the last input is
        // untouched.
        let mut a = args(&format!(
            "-hide_banner -hwaccel auto -ss 1.0 -i {PROXY} -hwaccel auto -ss 2.0 -i /media/a.mp4 -filter_complex [0:v][1:v]overlay[outv] -map [outv] -frames:v 1 pipe:1"
        ));
        let of = |p: &str| {
            if p == PROXY {
                InputFacts::of_path(p)
            } else {
                facts(Codec::H264, Chroma::Yuv420)
            }
        };
        let t = tune_inputs(&mut a, HostOs::Other, Reading::Frame, &of);
        assert_eq!(
            a.join(" "),
            format!(
                "-hide_banner -ss 1.0 -threads 1 -i {PROXY} -hwaccel auto -ss 2.0 -i /media/a.mp4 -filter_complex [0:v][1:v]overlay[outv] -map [outv] -frames:v 1 pipe:1"
            )
        );
        assert_eq!(
            t,
            Tuning {
                hw: true,
                all_cheap: false
            }
        );
    }

    #[test]
    fn an_image_input_is_cheap_and_untouched() {
        let mut a =
            args("-hide_banner -loop 1 -framerate 25 -t 5 -i /media/t.png -hwaccel auto -ss 1 -i /media/a.mp4 -map 0 out.mp4");
        let of = |p: &str| {
            if p.ends_with(".png") {
                InputFacts {
                    image: true,
                    ..InputFacts::default()
                }
            } else {
                facts(Codec::H264, Chroma::Yuv420)
            }
        };
        let t = tune_inputs(&mut a, HostOs::Other, Reading::Frame, &of);
        assert_eq!(
            a.join(" "),
            "-hide_banner -loop 1 -framerate 25 -t 5 -i /media/t.png -hwaccel auto -ss 1 -i /media/a.mp4 -map 0 out.mp4"
        );
        assert_eq!(
            t,
            Tuning {
                hw: true,
                all_cheap: false
            }
        );
        let mut only = args("-loop 1 -i /media/t.png -map 0 out.png");
        let t = tune_inputs(&mut only, HostOs::Other, Reading::Frame, &of);
        assert!(t.all_cheap && !t.hw);
        assert_eq!(only.join(" "), "-loop 1 -i /media/t.png -map 0 out.png");
    }

    #[test]
    fn an_argv_without_inputs_or_with_a_loose_flag_is_left_alone() {
        let none = "-f lavfi -t 1 -map 0 out.mp4";
        let mut a = args(none);
        let t = tune_inputs(&mut a, HostOs::Other, Reading::Frame, &InputFacts::of_path);
        assert_eq!(a.join(" "), none);
        assert!(t.all_cheap && !t.hw);
        // A `-i` with nothing after it, and a `-hwaccel` with nothing after it, are not ours to
        // repair: the run fails as it would have.
        let mut dangling = args("-hwaccel auto -i");
        tune_inputs(&mut dangling, HostOs::Other, Reading::Frame, &InputFacts::of_path);
        assert_eq!(dangling.join(" "), "-hwaccel auto -i");
        let mut bare = args("-i /media/a.mp4 -hwaccel");
        tune_inputs(&mut bare, HostOs::Other, Reading::Frame, &InputFacts::of_path);
        assert_eq!(bare.join(" "), "-i /media/a.mp4 -hwaccel");
    }

    #[test]
    fn frame_args_over_proxies_are_one_thread_and_over_originals_the_budget() {
        let _serial = cpu::test_lock();
        let restore = cpu::cpu_percent();
        cpu::set_cpu_percent(100);
        let mut proxies = opts("auto", PROXY, "-ss 1");
        limit_frame_args(&mut proxies, 1);
        assert_eq!(&proxies[..4], ["-filter_threads", "1", "-filter_complex_threads", "1"]);
        assert_eq!(&proxies[proxies.len() - 3..], ["-threads", "1", "pipe:1"]);
        assert!(!proxies.contains(&"-hwaccel".to_string()));

        // An original is capped like any other process: at a full budget a lone one is not.
        let original = opts("auto", "/media/a.mp4", "-ss 1");
        let mut lone = original.clone();
        limit_frame_args(&mut lone, 1);
        assert!(lone.iter().all(|a| !a.contains("threads")), "{lone:?}");
        // A shared one splits the budget, even at 100% (on a machine with a core to split).
        let mut shared = original;
        limit_frame_args(&mut shared, 4);
        if cpu::cores() > 1 {
            assert!(shared.contains(&"-filter_threads".to_string()), "{shared:?}");
        }
        cpu::set_cpu_percent(restore);
    }
}
