//! One export per delivery frame.

use super::*;

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
    pub(super) fn of(p: ExportProgress, variant: usize, total: usize, together: bool) -> Self {
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
