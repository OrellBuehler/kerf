//! The export's `filter_complex`: its inputs, its tracks and how they join.

use super::*;

/// Whether `clip` puts sound in the mix: its asset carries an audio stream **and**
/// the clip still plays it. A picture clip whose sound was detached
/// (`Clip::source_audio` false, see `Project::detach_audio`) is silent — its sound
/// is the linked audio clip's — and the export otherwise mixes the audio of *every*
/// clip with an audio stream, video tracks included, which is what made an asset
/// extracted onto an audio track while still on V1 sound twice. `source_audio`
/// defaults to true, so a clip that never heard of the flag is gated exactly as it
/// was.
pub(super) fn clip_sounds(clip: &Clip, assets: &[Asset]) -> bool {
    clip.source_audio
        && assets
            .iter()
            .find(|a| a.id == clip.asset_id)
            .is_some_and(|a| a.streams.iter().any(|s| s.kind == StreamKind::Audio))
}

/// The result of [`build_filter_complex`]: the `-filter_complex` string plus
/// which output pads it produced, so the caller knows which `-map`s to add.
pub(super) struct FilterGraph {
    pub(super) filter: String,
    pub(super) has_video: bool,
    pub(super) has_audio: bool,
    /// The `ebur128` taps a [metered](build_filter_complex_metered) build added,
    /// each ending on a pad the caller `-map`s. Empty for every other build.
    pub(super) meters: Vec<Meter>,
}

/// One level meter in a metered audio graph: an `ebur128` instance on a track's
/// finished strip or on the master, ending on a pad to `-map` (the filter passes
/// its audio through, so an output has to be consumed somewhere).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Meter {
    /// The pad the meter ends on (`lv3`, `lvmaster`), without brackets.
    pub(super) pad: String,
    /// The `ebur128@<name>` instance name its log lines carry (`t3`, `master`).
    pub(super) name: String,
    /// The index of the timeline track it listens to, `None` for the master.
    pub(super) track: Option<usize>,
}

/// One part of an export in parts, as its picture graph draws it ([`segments`]): the clips of the
/// plan (`InputPlan`: the ones that are on screen for these output frames) over a canvas that is
/// those frames of the whole export's. Times in the graph stay the whole export's (a part's frames carry the
/// timestamps they have in it, so every expression of a clip, a title or a caption is the one the
/// single run evaluates); the part is shifted to start at zero only as it leaves the graph.
pub(super) struct Scope<'a> {
    /// The cut's transition table, whole: a clip's look is the export's, whichever part draws it.
    pub(super) fx: &'a [ClipFx],
    /// The output frames the part covers: `frames` of them from `first_frame`.
    pub(super) first_frame: u64,
    pub(super) frames: u64,
}

#[cfg(test)]
thread_local! {
    /// The shortest run [`fold_min`] answers on this thread while a test pins it.
    static FOLD_MIN_OVERRIDE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Run `f` with runs of at least `min` blended clips folded (`usize::MAX`: none), to hold the folded graph against the
/// nested one it replaces.
#[cfg(test)]
pub(super) fn with_fold_min<R>(min: usize, f: impl FnOnce() -> R) -> R {
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
pub(super) fn blend_folds(
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
pub(super) fn backfilled<'a>(timeline: &Timeline, assets: &'a [Asset], opts: &ExportOptions) -> std::borrow::Cow<'a, [Asset]> {
    let wanted = time_base_inputs(timeline, assets, opts);
    if wanted.is_empty() {
        return std::borrow::Cow::Borrowed(assets);
    }
    with_time_bases(assets, &probe_time_bases(&wanted))
}

/// The compressor duck: `[aduck]` (the ducked bus) dipped by `sidechaincompress` against `[akside]` (the
/// rest of the mix) into `[aducked]`. Pure.
///
/// A sidechain filter **ends with the shorter of its two inputs**, and the key ends where the last clip
/// that is not ducked does: a six-second bed under two seconds of dialogue came out two seconds long. The
/// key is padded with `apad` so the filter ends with the bus, which is the only input with an end the
/// mix cares about (every clip's chain is trimmed, so the bus is finite and the pad stops being read the
/// moment it ends). Unbounded on purpose: the bus's own last sample is the end, not a rounded copy of the
/// timeline's length. A silent key is a key that is not speaking, so the bus plays at its own level.
pub(super) fn duck_compressor_chains() -> Vec<String> {
    vec![
        "[akside]apad[akpad]".to_string(),
        "[aduck][akpad]sidechaincompress=threshold=0.05:ratio=8:attack=20:release=400[aducked]".to_string(),
    ]
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
pub(super) fn build_filter_complex(
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
pub(super) fn build_filter_complex_metered(
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
pub(super) fn build_filter_complex_with(
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
                None => chains.extend(duck_compressor_chains()),
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
