//! What `ffprobe` says of a source beyond its streams (range, container, HDR, time base), and the tone map that follows.

use super::*;

// ---- HDR → SDR -------------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// What [`zscale_available`] answers on this thread while a test pins it.
    static ZSCALE_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg did (`true`) or did not (`false`) have `zscale`.
#[cfg(test)]
pub(super) fn with_zscale<R>(available: bool, f: impl FnOnce() -> R) -> R {
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
pub(in crate::engine) fn tonemap_filter(hdr: Hdr) -> String {
    tonemap_chain(hdr, zscale_available())
}

/// The filter that labels a picture BT.709 without touching a pixel (`setparams`), for an untagged
/// HD input that shares a graph with one naming a matrix ([`crate::model::bt709_labels`]). Not the
/// `-colorspace` output option, which FFmpeg 9 takes as an order to convert into that matrix on the
/// way (up to 22 levels moved, measured).
pub(super) const BT709_LABEL: &str = "setparams=colorspace=bt709";

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

pub(super) fn full_range_of(traits: SourceTraits) -> Option<bool> {
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
pub(super) fn probe_source_traits(ffprobe: &str, path: &Path, timeout: std::time::Duration) -> Option<SourceTraits> {
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
pub(super) fn parse_source_traits(json: &str) -> SourceTraits {
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
pub(super) fn fidelity_of(traits: SourceTraits) -> ProxyFidelity {
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
