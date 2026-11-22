//! A video clip's filter chain: reframe, fades, fill, rotation and the fit.

use super::*;

/// `v360` resampling used for export (quality) and for stills / previews (speed).
/// The measured difference in reprojection cost between these is small; the
/// export one buys sharper edges on a wide reframe.
const EXPORT_INTERP: &str = "cubic";
pub(super) const PREVIEW_INTERP: &str = "line";

/// How far a reframe channel must drift from its last emitted value before it is
/// worth another `sendcmd`, in degrees. Every command makes `v360` rebuild its
/// remap LUT, so this gate is what keeps a held camera from paying per frame.
const REFRAME_CMD_TOLERANCE: f64 = 0.05;

/// The `v360` filter realizing one sampled reframe at output size `w`x`h`.
///
/// `instance` names the filter (`v360@c3`) so a `sendcmd` can target this clip's
/// instance and no other — several reframed clips coexist in one graph. The
/// still path passes `None`: it has no commands to send, so it needs no name.
pub(super) fn reframe_filter(r: &ResolvedReframe, w: u32, h: u32, interp: &str, instance: Option<&str>) -> String {
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
pub(super) fn reframe_commands(clip: &Clip, rf: &Reframe, target: &str, fps: f64, dur: f64) -> Option<String> {
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
pub(super) fn letterbox_pad(w: u32, h: u32, clear: bool) -> String {
    if clear {
        format!("format=yuva420p,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0")
    } else {
        format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2")
    }
}

/// A constant rotation by `degrees`. A blended clip's corners are filled with transparent black (`blended`):
/// `none` leaves them as the zeroed buffer was allocated, with chroma 0, and the 2x2 chroma blocks across the
/// picture's edge carry it into the RGB the blend works in (33 levels of luma off the plain overlay at that edge).
pub(super) fn static_rotate(degrees: f64, blended: bool) -> String {
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
pub(super) fn blur_fill(w: u32, h: u32, even: &str, sf: &str, deep: bool, label: &str) -> String {
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
pub(super) fn takes_blur_fill(clip: &Clip, picture: Option<(u32, u32)>, frame: (u32, u32), alpha: bool) -> bool {
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
pub(super) fn picture_size(asset: Option<&Asset>) -> Option<(u32, u32)> {
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
pub(super) fn still_motion_for(clip: &Clip, assets: &[Asset], fmt: &ExportFormat, fx: &ClipFx) -> Option<StillMotion> {
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
pub(super) fn video_clip_chain(clip: &Clip, fmt: &ExportFormat, fx: &ClipFx, is_image: bool, instance: &str) -> String {
    video_clip_chain_stamped(clip, fmt, fx, is_image, None, None, None, None, instance)
}

/// [`video_clip_chain`] for a picture of a known size (`None`: not known, which a Cover clip's
/// chain reads as "the cover may hang over the frame").
#[cfg(test)]
pub(super) fn video_clip_chain_of(
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
pub(super) fn video_clip_chain_stamped(
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
