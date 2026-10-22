//! One cut at every delivery frame in **one** ffmpeg run.
//!
//! A multi-format export used to render each delivery frame on its own: the same sources
//! decoded, and the same mix built, once per file. Here each variant's graph is the export's
//! own (`build_filter_complex_with` over `for_delivery`, byte for byte), its labels moved into a
//! namespace of its own (`d<k>_…`) so the graphs can stand side by side in one `-filter_complex`.
//! What they share is shared: the clips' inputs are decoded once and `split` into every frame's
//! graph, and the mix is built once (the first variant's) and `asplit` into every file, so each
//! file carries the very samples. Every variant's own inputs (its animated titles, drawn at its
//! size) follow the clips', and the captions and chapters are read once and mapped into each.
//!
//! The run is an export like any other: the foreground lease, a stall bound, a cancel that kills
//! ffmpeg, the software rerun after a hardware decode failure. Its encoders run side by side, so
//! each is given its share of the budget's threads (`cpu::limit_outputs_args`). A run that stops
//! removes every file it touched, since all of them were in flight.
//!
//! What one run does not write is left to the variants rendered one after another: a two-pass
//! encode (one stats file per encoder), a hardware encoder (one session per file, a refusal of
//! the second unproven here), an audio-only container (nothing to share but the mix), and two
//! variants that would land on one file name.

use super::*;

/// Every link label of a filtergraph passed through `f` (pure): a `[` that ffmpeg's own parser
/// reads as one — not inside single quotes, not after a backslash (what `av_get_token` honours
/// in a filter's arguments, which end at an unquoted `[`) — up to its `]`.
pub(super) fn relabel(graph: &str, mut f: impl FnMut(&str) -> String) -> String {
    let bytes = graph.as_bytes();
    let mut out = String::with_capacity(graph.len() + graph.len() / 4);
    let (mut i, mut from, mut quoted) = (0, 0, false);
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if !quoted => i += 2,
            b'\'' => {
                quoted = !quoted;
                i += 1;
            }
            b'[' if !quoted => {
                let Some(len) = graph[i + 1..].find(']') else { break };
                out.push_str(&graph[from..i]);
                out.push('[');
                out.push_str(&f(&graph[i + 1..i + 1 + len]));
                out.push(']');
                i += len + 2;
                from = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&graph[from.min(graph.len())..]);
    out
}

/// An input pad label (`3:v`, `0:a`): its input index and stream letter.
fn input_pad(label: &str) -> Option<(usize, &str)> {
    let (n, kind) = label.split_once(':')?;
    Some((n.parse().ok()?, kind))
}

/// The options variant `delivery` is rendered with: the shared encode at its frame.
fn variant_opts(opts: &ExportOptions, delivery: Delivery) -> ExportOptions {
    ExportOptions {
        resolution: Some((delivery.width, delivery.height)),
        fit: delivery.fit,
        ..opts.clone()
    }
}

/// Whether one run may write every variant (pure): more than one, a single-pass encode in
/// software, a container with a picture, and a file each.
pub(super) fn renders_together(variants: &[ExportVariant], opts: &ExportOptions) -> bool {
    let software = opts
        .video_codec
        .as_deref()
        .is_none_or(|vc| enc_family(vc) == EncFamily::Software);
    let distinct = variants
        .iter()
        .enumerate()
        .all(|(i, v)| variants[..i].iter().all(|w| w.output != v.output));
    variants.len() > 1
        && distinct
        && software
        && !matches!(opts.rate_control, RateControl::TwoPass)
        && !opts.container.is_audio_only()
}

/// Whether the cut has more open at once than one run should hold, so that each variant is rendered
/// in parts on its own ([`segments`]) — one run over every delivery frame's inputs would open them
/// all again, times the number of frames (pure). What a delivery frame changes (crops, captions) is
/// not what [`segments::plan_export`] decides on, so the first variant speaks for all.
pub(super) fn export_in_parts(timeline: &Timeline, assets: &[Asset], variants: &[ExportVariant], opts: &ExportOptions) -> bool {
    let (Some(first), Some(limits)) = (variants.first(), segments::limits()) else {
        return false;
    };
    let framed = timeline.for_delivery(first.delivery);
    segments::plan_export(&framed, assets, &variant_opts(opts, first.delivery), limits).is_some()
}

/// The argv of the one run, and where its outputs sit.
#[derive(Debug)]
pub(super) struct TogetherArgs {
    pub args: Vec<String>,
    /// Each output path's distance from the end of `args`: the tuning of the inputs and the
    /// spilling of the graph happen ahead of them, so these hold while the indices move.
    pub sinks_from_end: Vec<usize>,
}

/// Build the one run that writes every variant (pure): `None` when the variants cannot share it —
/// their clips would not open the same inputs (never, from [`Timeline::for_delivery`], which only
/// moves crops and captions), or the cut has no picture to export. `opts` is the encode shared
/// by all of them, as [`render_variants`] takes it.
pub(super) fn build_together_args(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
) -> Result<Option<TogetherArgs>> {
    if variants.is_empty() {
        return Ok(None);
    }
    let per: Vec<ExportOptions> = variants.iter().map(|v| variant_opts(opts, v.delivery)).collect();
    let cuts: Vec<Timeline> = variants
        .iter()
        .zip(&per)
        .map(|(v, o)| {
            let mut cut = export_timeline(&timeline.for_delivery(v.delivery), o);
            if o.skip_burned_captions {
                cut.overlays.retain(|ov| !ov.generated);
            }
            cut
        })
        .collect();
    let cut = &cuts[0];
    let has_video = cut.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty());
    let has_audio = cut.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| clip_sounds(c, assets));
    let c = opts.container;
    if !has_video || c.is_audio_only() {
        return Ok(None);
    }
    let want_audio = has_audio && !c.is_video_only() && opts.include_audio;

    let fmts: Vec<ExportFormat> = cuts.iter().zip(&per).map(|(t, o)| export_format(t, assets, o)).collect();
    let mut inputs: Vec<String> = Vec::new();
    let plan = push_inputs(cut, assets, &fmts[0], &per[0], &mut inputs)?;
    for (k, (t, o)) in cuts.iter().zip(&per).enumerate().skip(1) {
        let mut own = Vec::new();
        push_inputs(t, assets, &fmts[k], o, &mut own)?;
        if own != inputs {
            return Ok(None);
        }
    }
    let n_inputs = plan.representatives.len();
    let titles: Vec<Vec<svg_layer::SvgSeq<'_>>> = cuts.iter().zip(&fmts).map(|(t, f)| svg_layer::svg_sequences(t, f)).collect();

    let mut args: Vec<String> = vec!["-y".to_string(), "-hide_banner".to_string(), "-nostats".to_string()];
    args.extend(inputs);
    let mut title_input = Vec::with_capacity(variants.len());
    let mut next = n_inputs;
    for (seqs, f) in titles.iter().zip(&fmts) {
        title_input.push(next);
        next += seqs.len();
        svg_layer::push_inputs(seqs, f, &mut args);
    }
    let side = side_files::push_inputs(opts, &mut args);

    let total = cut.duration();
    let graphs: Vec<FilterGraph> = cuts
        .iter()
        .enumerate()
        .map(|(k, t)| {
            build_filter_complex_with(
                t,
                assets,
                &fmts[k],
                total,
                &per[k],
                true,
                want_audio && k == 0,
                &plan,
                false,
                Some(&titles[k]),
                &if k == 0 { String::new() } else { format!("d{k}") },
                None,
            )
        })
        .collect();
    // How many of the graphs read each clip input's picture: one read stays the input's own pad,
    // more are one decode `split` among them.
    let mut reads = vec![0usize; n_inputs];
    for g in &graphs {
        relabel(&g.filter, |label| {
            if let Some((n, "v")) = input_pad(label).filter(|(n, _)| *n < n_inputs) {
                reads[n] += 1;
            }
            label.to_string()
        });
    }
    let mut chains: Vec<String> = Vec::new();
    for (n, &count) in reads.iter().enumerate().filter(|(_, c)| **c > 1) {
        let outs: String = (0..count).map(|j| format!("[in{n}_{j}]")).collect();
        chains.push(format!("[{n}:v]split={count}{outs}"));
    }
    let mut handed = vec![0usize; n_inputs];
    for (k, g) in graphs.iter().enumerate() {
        chains.push(relabel(&g.filter, |label| match input_pad(label) {
            Some((n, "v")) if n < n_inputs && reads[n] > 1 => {
                handed[n] += 1;
                format!("in{n}_{}", handed[n] - 1)
            }
            Some((n, kind)) if n >= n_inputs => format!("{}:{kind}", n - n_inputs + title_input[k]),
            Some(_) => label.to_string(),
            None => format!("d{k}_{label}"),
        }));
    }
    let mixed = graphs[0].has_audio;
    if mixed {
        let outs: String = (0..variants.len()).map(|k| format!("[mix{k}]")).collect();
        chains.push(format!("[d0_outa]asplit={}{outs}", variants.len()));
    }
    args.push("-filter_complex".to_string());
    args.push(chains.join(";"));

    let muted = has_audio && !want_audio && !c.is_video_only();
    // Every file carries the first variant's mix, so its ceiling is every file's.
    let peak_ceiling = holds_peak_ceiling(&cut.master, &per[0]);
    let mut sinks = Vec::with_capacity(variants.len());
    for (k, v) in variants.iter().enumerate() {
        let output = v
            .output
            .to_str()
            .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 output path: {}", v.output.display())))?;
        if graphs[k].has_video {
            args.extend(["-map".to_string(), format!("[d{k}_outv]")]);
        }
        if mixed {
            args.extend(["-map".to_string(), format!("[mix{k}]")]);
        }
        side_files::push_maps(&side, &mut args);
        push_output_opts(
            &mut args,
            &per[k],
            &fmts[k],
            (graphs[k].has_video, mixed),
            peak_ceiling,
            muted,
            PassPhase::Single,
            "",
        );
        sinks.push(args.len());
        args.push(output.to_string());
    }
    let last = args.len() - 1;
    Ok(Some(TogetherArgs {
        sinks_from_end: sinks.iter().map(|s| last - s).collect(),
        args,
    }))
}

/// Render every variant in one run, or `None` (having done nothing) when they cannot share one
/// ([`renders_together`], [`build_together_args`]) — the caller then renders them one after
/// another. `opts` is what [`prepare_variants`] returned (checked, the mix measured). The steps
/// are [`render_with_progress`]'s, once for the lot: the titles drawn for each frame, the
/// captions and chapters written once, the caption files beside each output checked first and
/// written once the run has completed. A run that does not complete removes every output it
/// touched ([`discard_partial`]).
pub(super) fn render_together(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    (from, since): (f64, Instant),
    progress: &mut dyn FnMut(VariantProgress),
    cancel: &dyn Fn() -> bool,
) -> Result<Option<RenderStatus>> {
    if !renders_together(variants, opts) || export_in_parts(timeline, assets, variants, opts) {
        return Ok(None);
    }
    let opts = ExportOptions {
        hwaccel: export_hwaccel_request(
            opts.hwaccel.as_deref(),
            hwaccel().is_some(),
            HWACCEL_PROVEN_BAD.load(std::sync::atomic::Ordering::Relaxed),
        ),
        ..opts.clone()
    };
    if build_together_args(timeline, assets, variants, &opts)?.is_none() {
        return Ok(None);
    }
    let total = variants.len();
    // `from` is where the metering pass (shared by every file) left the bar, `since` when it began.
    let mut progress = |p: ExportProgress| progress(VariantProgress::of(p.within(from, 1.0, since), 0, total, true));
    let framed: Vec<Timeline> = variants.iter().map(|v| timeline.for_delivery(v.delivery)).collect();
    let before: Vec<_> = variants.iter().map(|v| file_mtime(&v.output)).collect();
    let status = (|| {
        let mut titles = Vec::with_capacity(total);
        for (v, t) in variants.iter().zip(&framed) {
            let Some(pins) = svg_layer::prepare_export(t, assets, &variant_opts(&opts, v.delivery), &mut progress, cancel)?
            else {
                return Ok(RenderStatus::Cancelled);
            };
            titles.push(pins);
        }
        let (opts, _side) = side_files::prepare(&framed[0], &opts)?;
        for v in variants {
            side_files::check_sidecars(&v.output, &opts)?;
        }
        retry_export_in_software(
            &opts,
            |o, hw_used| render_attempt(timeline, assets, variants, o, &mut progress, cancel, hw_used),
            convict_hardware_decode,
        )
    })();
    if !matches!(status, Ok(RenderStatus::Completed)) {
        for (v, before) in variants.iter().zip(before) {
            discard_partial(&v.output, before);
        }
    }
    let status = status?;
    if status == RenderStatus::Completed {
        for (v, t) in variants.iter().zip(&framed) {
            side_files::write_sidecars(t, &v.output, &opts)?;
        }
    }
    Ok(Some(status))
}

/// One run with `opts` exactly as given: built, tuned for this machine, judged for its slot and
/// run with every encoder capped to its share.
fn render_attempt(
    timeline: &Timeline,
    assets: &[Asset],
    variants: &[ExportVariant],
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    hw_used: &std::cell::Cell<bool>,
) -> Result<RenderStatus> {
    let Some(TogetherArgs {
        mut args,
        sinks_from_end,
    }) = build_together_args(timeline, assets, variants, opts)?
    else {
        return Err(Error::Engine(
            "the delivery frames can no longer share one render".to_string(),
        ));
    };
    hw_used.set(tune_export_args(&mut args, assets, HostOs::CURRENT));
    let resource = export_resource(&args, &facts_from_assets(assets), cpu::gpu_lane());
    let _graph = externalize_filter_complex(&mut args, "v")?;
    let last = args.len() - 1;
    let sinks: Vec<usize> = sinks_from_end.iter().map(|d| last - d).collect();
    let total = match effective_range(timeline, opts) {
        Some((s, e)) => e - s,
        None => timeline.duration(),
    };
    run_ffmpeg_progress_into(
        &args,
        &sinks,
        &variants[0].output,
        Bar {
            total,
            offset: 0.0,
            width: 1.0,
            start: Instant::now(),
        },
        resource,
        progress,
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{audio_stream, av_asset, make_clip, timeline_of, video_stream, video_track};

    /// A picture-only 16:9 clip carrying a 9:16 crop.
    fn cut() -> (Timeline, Vec<Asset>) {
        let mut asset = av_asset(uuid::Uuid::new_v4(), 10.0);
        asset.streams = vec![video_stream(1920, 1080, 30.0)];
        let mut clip = make_clip(asset.id, 0.0, 6.0, 0.0);
        clip.framings.push(crate::model::Framing {
            aspect_w: 9,
            aspect_h: 16,
            crop_left: 0.25,
            crop_right: 0.25,
            crop_top: 0.0,
            crop_bottom: 0.0,
        });
        (timeline_of(vec![video_track(vec![clip])]), vec![asset])
    }

    /// Named as `ExportVariant::beside` names them, spelled out so the argv is the same on every OS.
    fn variants() -> Vec<ExportVariant> {
        [(1080, 1920, "9x16"), (1080, 1080, "1x1")]
            .into_iter()
            .map(|(w, h, shape)| ExportVariant {
                delivery: Delivery::new(w, h, Fit::Cover),
                output: PathBuf::from(format!("/out/cut-{shape}.mp4")),
            })
            .collect()
    }

    #[test]
    fn relabel_reads_the_labels_ffmpeg_reads_and_nothing_quoted_or_escaped() {
        let graph = "[0:v]drawtext=text='a [b] c':x=1\\[2\\],null[o1];[o1][1:v]overlay[outv]";
        let mut seen = Vec::new();
        let same = relabel(graph, |l| {
            seen.push(l.to_string());
            l.to_string()
        });
        assert_eq!(same, graph);
        assert_eq!(seen, ["0:v", "o1", "o1", "1:v", "outv"]);
        assert_eq!(
            relabel(graph, |l| format!("x{l}")),
            "[x0:v]drawtext=text='a [b] c':x=1\\[2\\],null[xo1];[xo1][x1:v]overlay[xoutv]"
        );
    }

    #[test]
    fn one_run_decodes_each_source_once_and_mixes_once() {
        let (timeline, mut assets) = cut();
        assets[0].streams = vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)];
        let built = build_together_args(&timeline, &assets, &variants(), &ExportOptions::default())
            .unwrap()
            .expect("mergeable");
        let args = built.args;
        assert_eq!(args.iter().filter(|a| *a == "-i").count(), 1, "{args:?}");
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.starts_with("[0:v]split=2[in0_0][in0_1];"), "{graph}");
        assert!(graph.contains("[in0_0]") && graph.contains("[in0_1]"));
        assert!(graph.contains("color=c=black:s=1080x1920") && graph.contains("color=c=black:s=1080x1080"));
        assert_eq!(graph.matches("[0:a]").count(), 1, "one mix: {graph}");
        assert!(graph.ends_with("[d0_outa]asplit=2[mix0][mix1]"), "{graph}");
        // The 9:16 file wears the clip's 9:16 crop, the square one the crop it has (none).
        let (nine, square) = graph.split_once("[d1_").unwrap();
        assert!(nine.contains("crop=w=iw*"), "{nine}");
        assert!(!square.contains("crop=w=iw*"), "{square}");
        let tail: Vec<&str> = args
            .iter()
            .skip_while(|a| *a != "-filter_complex")
            .skip(2)
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "-map",
                "[d0_outv]",
                "-map",
                "[mix0]",
                "/out/cut-9x16.mp4",
                "-map",
                "[d1_outv]",
                "-map",
                "[mix1]",
                "/out/cut-1x1.mp4"
            ]
        );
        let last = args.len() - 1;
        let sinks: Vec<&str> = built.sinks_from_end.iter().map(|d| args[last - d].as_str()).collect();
        assert_eq!(sinks, ["/out/cut-9x16.mp4", "/out/cut-1x1.mp4"]);
    }

    #[test]
    fn every_file_of_the_run_keeps_aac_noise_substitution_off_under_a_ceiling() {
        let (mut timeline, mut assets) = cut();
        assets[0].streams = vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)];
        let opts = ExportOptions {
            audio_codec: Some("aac".into()),
            ..ExportOptions::default()
        };
        let pns = |timeline: &Timeline, opts: &ExportOptions| {
            let args = build_together_args(timeline, &assets, &variants(), opts)
                .unwrap()
                .expect("mergeable")
                .args;
            args.windows(4).filter(|w| w == &["-c:a", "aac", "-aac_pns", "0"]).count()
        };
        assert_eq!(pns(&timeline, &opts), 0);
        timeline.master.limiter = true;
        assert_eq!(pns(&timeline, &opts), 2);
        timeline.master.limiter = false;
        let normalized = ExportOptions { loudnorm: true, ..opts };
        assert_eq!(pns(&timeline, &normalized), 2);
    }

    #[test]
    fn each_graph_is_the_single_exports_own() {
        let (timeline, assets) = cut();
        let variants = variants();
        let opts = ExportOptions::default();
        let built = build_together_args(&timeline, &assets, &variants, &opts)
            .unwrap()
            .unwrap()
            .args;
        let graph = &built[built.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        for (k, v) in variants.iter().enumerate() {
            let single = build_export_args(
                &timeline.for_delivery(v.delivery),
                &assets,
                "/x.mp4",
                &variant_opts(&opts, v.delivery),
            )
            .unwrap();
            let own = &single[single.iter().position(|a| a == "-filter_complex").unwrap() + 1];
            let moved = relabel(own, |l| match input_pad(l) {
                Some(_) => format!("in0_{k}"),
                None => format!("d{k}_{l}"),
            });
            assert!(graph.contains(&moved), "variant {k}: {moved}\nnot in {graph}");
        }
    }

    #[test]
    fn each_frame_commands_its_own_reframe() {
        let mut stream = video_stream(1024, 512, 30.0);
        stream.projection = Some(crate::model::Projection::Equirect);
        let mut asset = av_asset(uuid::Uuid::new_v4(), 10.0);
        asset.streams = vec![stream];
        let mut clip = Clip::for_asset(&asset, 0.0, 4.0, 0.0);
        let key = |time, yaw| crate::model::ReframeKeyframe {
            time,
            yaw,
            pitch: 0.0,
            roll: 0.0,
            fov: 100.0,
        };
        clip.reframe.as_mut().expect("a 360 clip reframes").keyframes = vec![key(0.0, 0.0), key(4.0, 90.0)];
        let timeline = timeline_of(vec![video_track(vec![clip])]);
        let assets = vec![asset];
        let variants = variants();
        let built = build_together_args(&timeline, &assets, &variants, &ExportOptions::default())
            .unwrap()
            .unwrap()
            .args;
        let graph = &built[built.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        for name in ["v360@c0", "v360@d1c0"] {
            assert_eq!(graph.matches(&format!("{name}=")).count(), 1, "{name}: {graph}");
            assert!(graph.contains(&format!("{name} yaw")), "{name} is commanded: {graph}");
        }
        // The first frame's graph is the one it exports alone, instance names and all.
        let single = build_export_args(
            &timeline.for_delivery(variants[0].delivery),
            &assets,
            "/x.mp4",
            &variant_opts(&ExportOptions::default(), variants[0].delivery),
        )
        .unwrap();
        let own = &single[single.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        let moved = relabel(own, |l| match input_pad(l) {
            Some(_) => "in0_0".to_string(),
            None => format!("d0_{l}"),
        });
        assert!(graph.contains(&moved), "{moved}\nnot in {graph}");
    }

    #[test]
    fn what_one_run_cannot_write_is_left_to_the_files_one_by_one() {
        let v = variants();
        let opts = ExportOptions::default();
        assert!(renders_together(&v, &opts));
        assert!(!renders_together(&v[..1], &opts));
        let twice = vec![
            v[0].clone(),
            ExportVariant {
                delivery: Delivery::new(720, 1280, Fit::Cover),
                ..v[0].clone()
            },
        ];
        assert!(!renders_together(&twice, &opts), "two variants on one file name");
        for o in [
            ExportOptions {
                rate_control: RateControl::TwoPass,
                ..opts.clone()
            },
            ExportOptions {
                video_codec: Some("h264_nvenc".into()),
                ..opts.clone()
            },
            ExportOptions {
                container: Container::Mp3,
                ..opts.clone()
            },
        ] {
            assert!(!renders_together(&v, &o), "{o:?}");
        }
        assert!(renders_together(
            &v,
            &ExportOptions {
                video_codec: Some("libx264".into()),
                ..opts
            }
        ));
    }

    #[test]
    fn a_cut_without_a_picture_is_not_built_together() {
        let (mut timeline, mut assets) = cut();
        assets[0].streams = vec![audio_stream(48_000, 2)];
        timeline.tracks[0].kind = StreamKind::Audio;
        assert!(
            build_together_args(&timeline, &assets, &variants(), &ExportOptions::default())
                .unwrap()
                .is_none()
        );
    }
}
