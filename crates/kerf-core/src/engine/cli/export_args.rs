//! The argv of an export: its inputs, its output and its encoder.

use super::*;

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
pub(super) struct InputPlan {
    /// The representative clip flat-index for each unique input, in `-i` order.
    pub(super) representatives: Vec<usize>,
    /// Per clip flat-index → its unique input index (position in `representatives`);
    /// [`NO_INPUT`] for a clip that takes no part in this plan.
    pub(super) clip_input: Vec<usize>,
    /// Per clip flat-index, how a title clip's pictures are stamped onto the canvas' grid (its chain's
    /// first filters); `None` for a clip of footage. Filled by [`push_inputs`], which names the pictures.
    pub(super) stamps: Vec<Option<svg_layer::ClipStamp>>,
    /// Added to every input index the graph prints (`[N:v]`, `[N:a]`): the inputs the argv
    /// has ahead of this plan's own. `0` for every graph that runs alone.
    pub(super) base: usize,
}

/// A clip in no input of a plan ([`InputPlan::clip_input`]).
pub(super) const NO_INPUT: usize = usize::MAX;

#[cfg(test)]
pub(super) fn plan_inputs(timeline: &Timeline, assets: &[Asset], fx: &[ClipFx]) -> InputPlan {
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
pub(super) struct Restart {
    pub(super) frame: u64,
    pub(super) count: u64,
}

/// Which inputs a run opens and how: all of them by default ([`push_inputs`]), or the part of an
/// export in parts ([`segments`]).
pub(super) struct InputScope<'a> {
    /// The cut's transition table, whole: a clip's look does not depend on which part draws it.
    pub(super) fx: &'a [ClipFx],
    /// The clips (by flat index) that get an input.
    pub(super) include: &'a dyn Fn(usize) -> bool,
    /// Stills read from a frame onward (by flat index).
    pub(super) restarts: &'a HashMap<usize, Restart>,
    /// Open the sound of each input only (`-vn`): the sound of an export in parts is one pass over
    /// the whole cut, from the clips that have any.
    pub(super) sound_only: bool,
    /// The inputs ahead of these in the argv.
    pub(super) base: usize,
}

/// Push the `-i` inputs for every clip in `timeline` and return the plan the
/// filtergraph indexes by.
///
/// Extracted so export and the live preview stream ([`build_preview_args`]) decode their sources
/// *identically* — same deduplication, same per-input fast-seek, same still-image looping — and
/// only differ at the two ends of the pipeline (which files go in, what muxer comes out).
pub(super) fn push_inputs(
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
pub(super) fn push_inputs_scoped(
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
pub(super) fn export_timeline(timeline: &Timeline, opts: &ExportOptions) -> Timeline {
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
pub(super) fn prepared_cut(timeline: &Timeline, opts: &ExportOptions) -> Timeline {
    let mut cut = export_timeline(timeline, opts);
    if opts.skip_burned_captions {
        cut.overlays.retain(|o| !o.generated);
    }
    cut
}

/// Take the title clips out of a cut whose graph draws no picture (an audio-only export, the levels
/// and the mix passes): their input is pictures nobody drew, and they have no sound.
pub(super) fn drop_titles(timeline: &mut Timeline) {
    for track in &mut timeline.tracks {
        track.clips.retain(|c| !c.is_generated());
    }
}

/// [`build_export_args`] parameterised by the two-pass [`PassPhase`]. `null_sink`
/// is the platform null device (`/dev/null` / `NUL`) used as the first-pass
/// output, and `passlog` is the shared `-passlogfile` prefix — both injected by
/// [`render_with`] so this builder stays pure. Single-pass callers pass
/// `(Single, "", "")`.
pub(super) fn build_export_args_phase(
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
pub(super) fn holds_peak_ceiling(master: &crate::model::MasterBus, opts: &ExportOptions) -> bool {
    master.limiter || opts.loudness.is_some() || opts.loudnorm
}

/// What follows an output's maps: the encoders and their settings, an explicit mute (`muted`: the
/// cut has sound and the export drops it) and the muxer's flags. `(has_video, has_audio)` is
/// whether the graph feeds the output a picture and a sound; `peak_ceiling` is
/// [`holds_peak_ceiling`] for the mix it carries.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_output_opts(
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
