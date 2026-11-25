//! The argv of a composited still and the chains of its layers.

use super::*;

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
pub(super) fn build_still_args(
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
pub(super) fn still_titles(
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
pub(super) fn build_still_args_rendered(
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
pub(super) struct StillCanvas {
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) fit: Fit,
    pub(super) sf: String,
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
pub(super) fn still_clip_chain(
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
