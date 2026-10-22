//! A long export rendered in parts, so that what is open at once stays bounded however long the cut is.
//!
//! One ffmpeg run opens **every** clip of the span as an input, and each started decoder holds its
//! frames in flight whether or not the cut has reached its clip: measured on 4K / 5.3K HEVC 10-bit
//! (FFmpeg 4.4.2, 16 threads) a decoder costs 0.8 to 1.5 GB (about 95 MB per megapixel, in proportion
//! to its threads), so four such inputs hold 4.4 GB before the first frame and a reel of forty-odd
//! never gets one out. This module plans the span into parts that each open only the clips on screen
//! for them, renders each to a lossless intermediate, and joins them in one last pass.
//!
//! * **When**: only when the whole span's inputs are over [`SEGMENT_LIMITS`] (decode area in
//!   megapixels, or a number of inputs). Below it nothing here runs and the argv is the single run's,
//!   byte for byte (the golden oracle's first 8000 cases). `KERF_EXPORT_PARTS=off` never splits;
//!   a number replaces the decode budget in megapixels.
//! * **Where to cut** ([`plan_parts`], pure): at the frame where a clip's last drawn frame has gone
//!   or the next one's first comes, as late as the limits allow, preferring a cut that no clip
//!   straddles. A clip that straddles a cut is an input of both parts, **read from its own start in
//!   both** with its own seek, trim and fades — so what its chain does is what the single run does,
//!   whatever the transition, keyframe or fade — and a cut that would make a footage clip decode more
//!   than [`SegmentLimits::preroll_s`] of source it does not use is not made. A still image is the
//!   exception that costs nothing: its frames are made up, so a part restarts it at the cut
//!   ([`Restart`], numbered onto the frames it still has) however long it has been on screen.
//! * **Time is the export's**: a part's canvas is frames `first..first + n` of the whole export's
//!   (`setpts=N+first`), every clip, title and caption expression is evaluated on the very timestamps
//!   it has in the single run, and only the finished picture is shifted to start at zero. Nothing is
//!   rebased, so nothing moves by an ulp.
//! * **The join** is lossless: each part is FFV1 (intra, in Matroska) at exactly its frame count, read
//!   back by the concat demuxer and re-stamped by frame number, and the **one last pass** encodes the
//!   file — the user's codec, rate control, two passes, faststart, hardware encoder, container
//!   options — exactly as a single run would. No GOP, rate-control or timestamp seam can arise, because
//!   there is one encoder over one frame sequence. It costs disk (a lossless intermediate of the span,
//!   under `<cache>/kerf/segments` or `KERF_SEGMENT_DIR`), deleted whatever happens.
//! * **Sound is not split.** The last pass also mixes the whole cut's sound from the clips that have
//!   any (`-vn` on each input, so no picture decoder is dragged along), through the export's own audio
//!   graph: ducking, the master bus, `loudnorm` and the static loudness gain see the whole
//!   program, so there is no click, gap or pumping to hunt for at a join.
//!
//! Left to the single run (and listed in `engine-cli.md`): a gif or an audio-only container, a VAAPI
//! encode, a multi-format export rendered in one run (`together`), and a cut that cannot be cut — a
//! footage clip that straddles every candidate cut for longer than the pre-roll allows.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::*;
use crate::frame_pick::{fps_span, FpsPick, SourceFrames};

/// What decides whether an export runs in parts, and how big a part may be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SegmentLimits {
    /// The most decode area one run holds open, in megapixels of source picture (a still image
    /// counts a quarter: it is decoded once).
    pub decode_mpx: f64,
    /// The most inputs one run opens.
    pub inputs: usize,
    /// The most source a cut may make a clip decode and drop: a clip that straddles a cut is read
    /// from its start again by the part after it, so a cut is not made where it would cost more than
    /// this many seconds of one clip's source.
    pub preroll_s: f64,
}

/// The one threshold. Measured on this build (FFmpeg 4.4.2, 16 threads, HEVC 10-bit 4K / 5.3K,
/// libx264 `veryfast` to 1080p50): one run holds 0.8 / 1.5 GB per 4K / 5.3K decoder with default
/// threads, so 32 megapixels (four UHD inputs, fifteen 1080p) is about 3 GB; eight 4K inputs went
/// over 6.6 GB before the first frame. A part opens its decoders with
/// [`PART_DECODER_THREADS`], a third of that per megapixel.
pub(super) const SEGMENT_LIMITS: SegmentLimits = SegmentLimits {
    decode_mpx: 32.0,
    inputs: 16,
    preroll_s: 20.0,
};

/// The threads each decoder of a part is capped to (`-threads` ahead of its `-i`), the budget's
/// where that is fewer. A frame-threaded decoder holds a frame per thread, and in a part one clip
/// plays at a time: four threads decode 4K HEVC 10-bit at 70 fps against 86 with eight.
pub(super) const PART_DECODER_THREADS: usize = 4;

/// Half a millisecond: how far outside a clip's window a frame still counts as one it can touch,
/// more than any rounding of the graph's expressions and less than a frame at any rate.
const TOUCH_EPS: f64 = 5e-4;

/// The limits an export is planned with, or `None` when parts are turned off
/// (`KERF_EXPORT_PARTS=off`); a number there is the decode budget in megapixels.
pub(super) fn limits() -> Option<SegmentLimits> {
    match std::env::var("KERF_EXPORT_PARTS") {
        Ok(v) if matches!(v.trim().to_ascii_lowercase().as_str(), "off" | "0" | "no" | "false") => None,
        Ok(v) => Some(match v.trim().parse::<f64>() {
            Ok(mpx) if mpx.is_finite() && mpx > 0.0 => SegmentLimits {
                decode_mpx: mpx,
                ..SEGMENT_LIMITS
            },
            _ => SEGMENT_LIMITS,
        }),
        Err(_) => Some(SEGMENT_LIMITS),
    }
}

// ---- frames -------------------------------------------------------------------------------------

/// How many frames the whole export's canvas has, `color=d={total}` at `rate`: frames `k` while
/// `k` of them, in microseconds (`av_rescale_q`, to nearest), is less than the duration
/// (`av_parse_time` of the printed number, whole microseconds). Pure.
pub(super) fn canvas_frames(total: f64, rate: Rational) -> u64 {
    if !(total.is_finite() && total > 0.0) {
        return 0;
    }
    let duration = i128::from(crate::clip_timing::parse_micros(total));
    let micros = |k: i128| crate::frame_pick::rescale_near(k, i128::from(rate.den) * 1_000_000, i128::from(rate.num));
    let mut k = (duration * i128::from(rate.num) / (i128::from(rate.den) * 1_000_000) - 2).max(0);
    while micros(k) < duration {
        k += 1;
    }
    while k > 0 && micros(k - 1) >= duration {
        k -= 1;
    }
    u64::try_from(k).unwrap_or(0)
}

/// The frames `first..=last` of the export that a window `[start, end]` (seconds, closed at both
/// ends like the graph's `between`) can touch, a hair wide; `None` when it touches none of the
/// `frames`.
fn touch(start: f64, end: f64, rate: Rational, frames: u64) -> Option<(u64, u64)> {
    if frames == 0 || !(start.is_finite() && end.is_finite()) {
        return None;
    }
    let first = svg_layer::first_frame_from(rate, (start - TOUCH_EPS).max(0.0));
    let last = svg_layer::last_frame_to(rate, end + TOUCH_EPS, false)?.min(frames - 1);
    (first <= last).then_some((first, last))
}

// ---- the plan ------------------------------------------------------------------------------------

/// A picture clip as the planner sees it: the frames it can touch and what it costs to hold open.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PlanClip {
    /// The clip's flat index in the cut.
    pub flat: usize,
    /// The first and last (inclusive) output frame it can be drawn on.
    pub first: u64,
    pub last: u64,
    /// Clips with the same `input` share one `-i`.
    pub input: usize,
    /// Megapixels its decoder holds.
    pub mpx: f64,
    /// Seconds of source its input decodes per output frame (the clip's speed over the rate): what
    /// reading it again from its start costs.
    pub source_per_frame: f64,
    /// A still image a part can restart at a cut for nothing ([`restartable`]).
    pub restart: bool,
}

/// A part: `frames` of the export from output frame `first`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Part {
    pub first: u64,
    pub frames: u64,
}

impl Part {
    /// One past its last frame.
    pub fn end(&self) -> u64 {
        self.first + self.frames
    }
}

/// What a set of clips holds open: the megapixels of its distinct inputs, and how many.
pub(super) fn open_cost<'a>(clips: impl Iterator<Item = &'a PlanClip>) -> (f64, usize) {
    let mut inputs: HashMap<usize, f64> = HashMap::new();
    for c in clips {
        let held = inputs.entry(c.input).or_insert(0.0);
        *held = held.max(c.mpx);
    }
    (inputs.values().sum(), inputs.len())
}

/// Plan the cuts (pure): the parts of an export of `frames` frames whose picture clips are `clips`,
/// or `None` when it should run whole — everything fits the limits, nothing helps (no cut is
/// allowed, or the parts are no smaller), or there is nothing to cut.
///
/// Greedy, from the front: each part reaches as far as the limits allow, to the latest cut that is
/// *allowed* (every clip that straddles it either restarts for free or has no more than
/// `preroll_s` of source to decode again), and among the later half of the cuts that reach that
/// far prefers one no footage straddles. Cuts are made only where the set of clips on screen changes
/// (a clip's first frame; the frame after its last), because that is the only place a cut buys
/// anything. A stretch that is over the limits by itself (more is on screen at one moment than the
/// limits allow) is cut where it can be and is over them, the best that can be done.
pub(super) fn plan_parts(clips: &[PlanClip], frames: u64, limits: SegmentLimits) -> Option<Vec<Part>> {
    let clips: Vec<&PlanClip> = clips.iter().filter(|c| c.first < frames && c.first <= c.last).collect();
    let within = |(mpx, inputs): (f64, usize)| mpx <= limits.decode_mpx && inputs <= limits.inputs;
    let open = |from: u64, to: u64| open_cost(clips.iter().copied().filter(|c| c.first < to && c.last >= from));
    if frames < 2 || within(open(0, frames)) {
        return None;
    }
    let mut cuts: Vec<u64> = clips
        .iter()
        .flat_map(|c| [c.first, c.last + 1])
        .filter(|&k| k > 0 && k < frames)
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let straddling = |cut: u64| clips.iter().copied().filter(move |c| c.first < cut && c.last >= cut);
    let preroll = |c: &PlanClip, cut: u64| {
        if c.restart {
            0.0
        } else {
            (cut - c.first) as f64 * c.source_per_frame
        }
    };
    let allowed = |cut: u64| straddling(cut).all(|c| preroll(c, cut) <= limits.preroll_s);
    let cost = |cut: u64| straddling(cut).map(|c| preroll(c, cut)).sum::<f64>();

    let mut parts = Vec::new();
    let mut a = 0u64;
    loop {
        if within(open(a, frames)) {
            parts.push(Part {
                first: a,
                frames: frames - a,
            });
            break;
        }
        let ahead = &cuts[cuts.partition_point(|&c| c <= a)..];
        // The cuts the limits reach: what a part from `a` holds only grows with its end.
        let reach = ahead.partition_point(|&c| within(open(a, c)));
        let feasible: Vec<u64> = ahead[..reach].iter().copied().filter(|&c| allowed(c)).collect();
        let cut = match feasible.last() {
            Some(&far) => {
                let half = a + (far - a) / 2;
                let late: Vec<u64> = feasible.iter().copied().filter(|&c| c >= half).collect();
                // The latest cut nothing straddles; failing that the cheapest, the latest of equals.
                late.iter()
                    .rev()
                    .copied()
                    .find(|&c| cost(c) == 0.0)
                    .or_else(|| late.iter().rev().copied().min_by(|&x, &y| cost(x).total_cmp(&cost(y))))
                    .unwrap_or(far)
            }
            // Over the limits already at the first cut: take the first one that is allowed.
            None => match ahead.iter().copied().find(|&c| allowed(c)) {
                Some(c) => c,
                None => {
                    parts.push(Part {
                        first: a,
                        frames: frames - a,
                    });
                    break;
                }
            },
        };
        parts.push(Part {
            first: a,
            frames: cut - a,
        });
        a = cut;
    }
    (parts.len() > 1).then_some(parts)
}

/// Whether a clip is a still image a part can restart at a cut (pure): its frames are made up, one
/// picture at the export's rate, so picking it up at a frame is the same as having read it from its
/// start — as long as nothing in its chain keeps a count across frames (a keyed crop's
/// `perspective`, `vignette`'s dither) or plays it other than once through at its own pace.
fn restartable(clip: &Clip, asset: Option<&Asset>, fx: &ClipFx) -> bool {
    asset.is_some_and(|a| a.is_image())
        && !clip.is_generated()
        && !clip.is_reversed()
        && (clip.speed_mag() - 1.0).abs() < 1e-9
        && !clip.crop_animated()
        && clip.effects.is_empty()
        && clip.reframe.is_none()
        && !fx.head_pad
}

/// The picture clips of `cut` (video-track clips that are drawn on at least one frame) as the
/// planner sees them, in flat order. `fx` is the cut's whole transition table.
pub(super) fn picture_clips(
    cut: &Timeline,
    assets: &[Asset],
    fx: &[ClipFx],
    fmt: &ExportFormat,
    rate: Rational,
    frames: u64,
) -> Vec<PlanClip> {
    let mut out = Vec::new();
    let mut shared: HashMap<(uuid::Uuid, String), usize> = HashMap::new();
    let mut inputs = 0usize;
    let mut flat = 0usize;
    for track in &cut.tracks {
        for clip in &track.clips {
            let this = flat;
            flat += 1;
            if track.kind != StreamKind::Video {
                continue;
            }
            let (start, end) = ClipTiming::new(clip, &fx[this]).window();
            let Some((first, last)) = touch(start, end, rate, frames) else {
                continue;
            };
            let asset = assets.iter().find(|a| a.id == clip.asset_id);
            let image = asset.is_some_and(|a| a.is_image());
            let stream = asset.and_then(|a| a.streams.iter().find(|s| s.kind == StreamKind::Video));
            let area = stream
                .and_then(|s| Some(f64::from(s.width?) * f64::from(s.height?)))
                .unwrap_or(1920.0 * 1080.0);
            let (input, mpx) = if clip.is_generated() {
                inputs += 1;
                (inputs - 1, f64::from(fmt.width) * f64::from(fmt.height) / 4.0 / 1e6)
            } else if image {
                inputs += 1;
                (inputs - 1, area / 4.0 / 1e6)
            } else {
                let (window_start, _) = clip_source_window(clip, &fx[this]);
                let key = (clip.asset_id, format!("{}", clip_seek(window_start, fx[this].lead)));
                let input = *shared.entry(key).or_insert_with(|| {
                    inputs += 1;
                    inputs - 1
                });
                (input, area / 1e6)
            };
            out.push(PlanClip {
                flat: this,
                first,
                last,
                input,
                mpx,
                source_per_frame: clip.speed_mag() * f64::from(rate.den) / f64::from(rate.num),
                restart: restartable(clip, asset, &fx[this]),
            });
        }
    }
    out
}

/// An export planned in parts.
#[derive(Debug)]
pub(super) struct PartsPlan {
    /// The cut as the builders have it (`prepared_cut`).
    pub cut: Timeline,
    pub fmt: ExportFormat,
    /// The cut's whole transition table.
    pub fx: Vec<ClipFx>,
    pub rate: Rational,
    /// The frames of the whole export.
    pub frames: u64,
    pub clips: Vec<PlanClip>,
    pub parts: Vec<Part>,
    /// Where the parts' files go instead of the cache (a test's own directory).
    pub scratch: Option<PathBuf>,
}

/// Plan `timeline`'s export in parts under `limits` (pure), or `None` to run it whole.
pub(super) fn plan_export(
    timeline: &Timeline,
    assets: &[Asset],
    opts: &ExportOptions,
    limits: SegmentLimits,
) -> Option<PartsPlan> {
    if opts.container.is_audio_only() || opts.container == Container::Gif || uploads_to_vaapi(opts) {
        return None;
    }
    let cut = prepared_cut(timeline, opts);
    if !cut.tracks.iter().any(|t| t.kind == StreamKind::Video && !t.clips.is_empty()) {
        return None;
    }
    let fmt = export_format(&cut, assets, opts);
    let rate = Rational::from_fps(fmt.fps)?;
    let frames = canvas_frames(cut.duration(), rate);
    let fx = transition_fx(&cut, assets);
    let clips = picture_clips(&cut, assets, &fx, &fmt, rate, frames);
    let parts = plan_parts(&clips, frames, limits)?;
    Some(PartsPlan {
        cut,
        fmt,
        fx,
        rate,
        frames,
        clips,
        parts,
        scratch: None,
    })
}

// ---- what one part draws -------------------------------------------------------------------------

/// What a part opens and draws, out of the plan.
#[derive(Debug)]
pub(super) struct PartContents {
    /// The clips (by flat index) with an input in the part.
    pub include: Vec<bool>,
    /// The still images the part picks up at its first frame.
    pub restarts: HashMap<usize, Restart>,
}

/// Which clips `part` draws (those that can touch one of its frames) and which stills it restarts.
pub(super) fn part_contents(plan: &PartsPlan, part: &Part) -> PartContents {
    let total: usize = plan.cut.tracks.iter().map(|t| t.clips.len()).sum();
    let mut include = vec![false; total];
    let mut restarts = HashMap::new();
    let clips: Vec<&Clip> = plan.cut.tracks.iter().flat_map(|t| t.clips.iter()).collect();
    for c in plan.clips.iter().filter(|c| c.first < part.end() && c.last >= part.first) {
        include[c.flat] = true;
        if !(c.restart && c.first < part.first) {
            continue;
        }
        // A still that was on screen before this part: read from here on, if the whole export's
        // run of it has a picture on this part's first frame (else it is not drawn in the part).
        let clip = clips[c.flat];
        let Some(fps) = Rational::from_fps_filter(plan.fmt.fps) else {
            continue;
        };
        let pick = FpsPick {
            speed: 1.0,
            reverse: false,
            window: clip_source_window(clip, &plan.fx[c.flat]),
            start: clip.timeline_start,
            frame: part.first,
            fps,
            drop_first: false,
            lead: 0.0,
            image: Some(plan.rate),
        };
        let Some((first, end)) = fps_span(&pick, &SourceFrames::NONE) else {
            continue;
        };
        let (first, end) = (first.max(0) as u64, end.max(0) as u64);
        if first >= part.first {
            // It starts inside the part after all: read from its own start.
        } else if end <= part.first {
            include[c.flat] = false;
        } else {
            restarts.insert(
                c.flat,
                Restart {
                    frame: part.first,
                    count: end - part.first,
                },
            );
        }
    }
    PartContents { include, restarts }
}

/// The floating SVG titles `part` draws, each cut to the pictures on its frames (pure).
pub(super) fn part_titles<'a>(all: &[svg_layer::SvgSeq<'a>], rate: Rational, part: &Part) -> Vec<svg_layer::SvgSeq<'a>> {
    let (a, b) = (part.first, part.end());
    all.iter()
        .filter_map(|s| {
            let mut s = s.clone();
            match s.kind {
                svg_layer::Kind::Frames => {
                    let (lo, hi) = (s.first_frame.max(a), (s.first_frame + s.pictures).min(b));
                    if lo >= hi {
                        return None;
                    }
                    s.first_picture += lo - s.first_frame;
                    s.pictures = hi - lo;
                    s.first_frame = lo;
                }
                // One picture, looped; on screen between its start and end like a caption.
                svg_layer::Kind::Held => {
                    let (first, last) = touch(s.start, s.end, rate, u64::MAX / 2)?;
                    if first >= b || last < a {
                        return None;
                    }
                    s.first_frame = s.first_frame.max(a);
                }
            }
            Some(s)
        })
        .collect()
}

/// The text overlays `part` can draw (those whose window touches one of its frames).
fn part_overlays(cut: &Timeline, rate: Rational, part: &Part) -> Vec<crate::model::TextOverlay> {
    cut.overlays
        .iter()
        .filter(|o| {
            touch(o.start, o.end, rate, u64::MAX / 2).is_some_and(|(first, last)| first < part.end() && last >= part.first)
        })
        .cloned()
        .collect()
}

// ---- the argv ------------------------------------------------------------------------------------

/// The mezzanine: lossless, intra, in Matroska (the container FFV1 is specified for). Its frames are
/// what the graph produced, in whatever pixel format that was.
const MEZZANINE: [&str; 10] = ["-c:v", "ffv1", "-level", "3", "-coder", "1", "-context", "1", "-g", "1"];

/// The argv of one part (pure): the clips and titles it draws as inputs, the picture graph over a
/// canvas of its frames, and the lossless file of exactly `part.frames` frames at `out`. `fps_mode`
/// is the spelling of the frame pacing flag this ffmpeg knows ([`fps_mode_flag`]).
pub(super) fn build_part_args(
    plan: &PartsPlan,
    assets: &[Asset],
    opts: &ExportOptions,
    part: &Part,
    floating_titles: &[svg_layer::SvgSeq<'_>],
    out: &str,
    fps_mode: &str,
) -> Result<Vec<String>> {
    let contents = part_contents(plan, part);
    let mut timeline = plan.cut.clone();
    timeline.overlays = part_overlays(&plan.cut, plan.rate, part);
    let titles = part_titles(floating_titles, plan.rate, part);

    let mut args: Vec<String> = vec!["-y".to_string(), "-hide_banner".to_string(), "-nostats".to_string()];
    let scope = InputScope {
        fx: &plan.fx,
        include: &|flat| contents.include.get(flat).copied().unwrap_or(false),
        restarts: &contents.restarts,
        sound_only: false,
        base: 0,
    };
    let inputs = push_inputs_scoped(&timeline, assets, &plan.fmt, opts, &scope, &mut args)?;
    svg_layer::push_inputs(&titles, &plan.fmt, &mut args);
    let part_scope = Scope {
        fx: &plan.fx,
        first_frame: part.first,
        frames: part.frames,
    };
    let graph = build_filter_complex_with(
        &timeline,
        assets,
        &plan.fmt,
        0.0,
        opts,
        true,
        false,
        &inputs,
        false,
        Some(&titles),
        "",
        Some(&part_scope),
    );
    args.push("-filter_complex".to_string());
    args.push(graph.filter);
    args.extend(["-map", "[outv]", "-an", fps_mode, "passthrough", "-pix_fmt", "yuv420p"].map(String::from));
    args.extend(MEZZANINE.map(String::from));
    args.extend(["-frames:v".to_string(), part.frames.to_string(), out.to_string()]);
    Ok(args)
}

/// The text of the concat list that reads the parts back as one picture: each file with its exact
/// duration, so that the timeline the demuxer lays them on is the export's (the last pass restamps
/// by frame number besides). Single quotes are the format's own quoting.
pub(super) fn concat_list(files: &[(String, u64)], rate: Rational) -> String {
    let mut text = String::from("ffconcat version 1.0\n");
    for (path, frames) in files {
        text.push_str(&format!("file '{}'\n", path.replace('\'', "'\\''")));
        text.push_str(&format!(
            "duration {:.6}\n",
            *frames as f64 * f64::from(rate.den) / f64::from(rate.num)
        ));
    }
    text
}

/// The argv of the last pass (pure): the parts read back as the picture, the sound of the whole cut
/// mixed through the export's own audio graph, and the file written with the user's options. `pass`
/// is the two-pass encode's phase as in a single run.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_final_args(
    plan: &PartsPlan,
    assets: &[Asset],
    output_path: &str,
    opts: &ExportOptions,
    pass: PassPhase,
    null_sink: &str,
    passlog: &str,
    list_path: &str,
) -> Result<Vec<String>> {
    let cut = &plan.cut;
    let timeline_has_audio = cut.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| clip_sounds(c, assets));
    let c = opts.container;
    let want_audio = timeline_has_audio && !c.is_video_only() && opts.include_audio && pass != PassPhase::First;

    let mut args: Vec<String> = vec!["-y".to_string(), "-hide_banner".to_string(), "-nostats".to_string()];
    args.extend(["-f", "concat", "-safe", "0", "-i", list_path].map(String::from));
    let mut sounds: HashSet<usize> = HashSet::new();
    if want_audio {
        let mut flat = 0usize;
        for track in &cut.tracks {
            for clip in &track.clips {
                if clip_sounds(clip, assets) {
                    sounds.insert(flat);
                }
                flat += 1;
            }
        }
    }
    let scope = InputScope {
        fx: &plan.fx,
        include: &|flat| sounds.contains(&flat),
        restarts: &HashMap::new(),
        sound_only: true,
        base: 1,
    };
    let inputs = push_inputs_scoped(cut, assets, &plan.fmt, opts, &scope, &mut args)?;
    let side = if pass == PassPhase::First {
        side_files::SideInputs::default()
    } else {
        side_files::push_inputs(opts, &mut args)
    };

    let sound = build_filter_complex(cut, assets, &plan.fmt, cut.duration(), opts, false, want_audio, &inputs);
    let picture = format!(
        "[0:v]settb=expr={den}/{num},setpts=N[outv]",
        den = plan.rate.den,
        num = plan.rate.num
    );
    args.push("-filter_complex".to_string());
    args.push(if sound.filter.is_empty() {
        picture
    } else {
        format!("{picture};{}", sound.filter)
    });
    args.extend(["-map", "[outv]"].map(String::from));
    if sound.has_audio {
        args.extend(["-map", "[outa]"].map(String::from));
    }
    side_files::push_maps(&side, &mut args);
    let muted = timeline_has_audio && !want_audio && pass != PassPhase::First && !c.is_video_only();
    push_output_opts(
        &mut args,
        opts,
        &plan.fmt,
        (true, sound.has_audio),
        holds_peak_ceiling(&cut.master, opts),
        muted,
        pass,
        passlog,
    );
    if pass == PassPhase::First {
        args.extend(["-an", "-f", "null"].map(String::from));
        args.push(null_sink.to_string());
    } else {
        args.push(output_path.to_string());
    }
    Ok(args)
}

// ---- rendering -----------------------------------------------------------------------------------

/// The share of the progress bar the parts have; the last pass has the rest.
const PARTS_SHARE: f64 = 0.55;

/// The directory of one export's parts, removed when dropped: on success, on a failure, on a
/// cancel and on a panic.
struct PartsDir(PathBuf);

impl PartsDir {
    /// `<KERF_SEGMENT_DIR | cache>/kerf/segments/<pid>-<n>`, else the temp directory's. The
    /// directories of processes that are gone are swept first.
    fn create(root: Option<&Path>) -> Result<Self> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = root
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("KERF_SEGMENT_DIR").map(PathBuf::from))
            .or_else(|| dirs::cache_dir().map(|d| d.join("kerf").join("segments")))
            .unwrap_or_else(|| std::env::temp_dir().join("kerf-segments"));
        prune_stale(&root, std::process::id());
        let dir = root.join(format!("{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| {
            Error::Engine(format!(
                "could not make a directory for the parts of the export at {}: {e}",
                dir.display()
            ))
        })?;
        Ok(Self(dir))
    }
}

/// Remove the directories of `root` (`<pid>-<n>`) that a process which is gone left behind: where
/// there is a cheap liveness check, the pid's; elsewhere a directory nothing has written to for a day.
fn prune_stale(root: &Path, own: u32) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.split('-').next())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        let stale = pid != own
            && if cfg!(unix) {
                !crate::svg::raster::process_alive(pid)
            } else {
                entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600))
            };
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

impl Drop for PartsDir {
    fn drop(&mut self) {
        if std::env::var_os("KERF_KEEP_PARTS").is_none() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Render `plan`'s export part by part and write `output` (the single entry the render takes when
/// [`plan_export`] says to). Holds the heavy-job lease from the first part to the last pass, so
/// nothing queued gets in between. Returns [`RenderStatus::Cancelled`] when `cancel` trips.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_in_parts(
    plan: &PartsPlan,
    assets: &[Asset],
    output: &Path,
    opts: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &dyn Fn() -> bool,
    hw_used: &std::cell::Cell<bool>,
) -> Result<RenderStatus> {
    let started = Instant::now();
    let output_str = output
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 output path: {}", output.display())))?;
    let mut on_wait = |wait: cpu::Wait| {
        progress(ExportProgress::queued(0.0, started.elapsed().as_secs_f64(), wait.message()));
    };
    let lease = match cpu::lease_waiting(&mut on_wait, cancel) {
        Ok(lease) => lease,
        Err(Error::Cancelled) => return Ok(RenderStatus::Cancelled),
        Err(e) => return Err(e),
    };
    let dir = PartsDir::create(plan.scratch.as_deref())?;
    let fps = plan.rate.as_f64();
    let total = plan.parts.len();
    let decoders = PART_DECODER_THREADS.min(lease.threads()).max(1);
    tracing::info!(parts = total, frames = plan.frames, "exporting in parts");

    let floating = svg_layer::svg_sequences(&plan.cut, &plan.fmt);
    let mut files: Vec<(String, u64)> = Vec::with_capacity(total);
    let mut offset = 0.0;
    for (i, part) in plan.parts.iter().enumerate() {
        let file = dir.0.join(format!("part-{i:04}.mkv"));
        let file_str = file.to_string_lossy().into_owned();
        let mut args = build_part_args(plan, assets, opts, part, &floating, &file_str, fps_mode_flag())?;
        hw_used.set(hw_used.get() | tune_export_args(&mut args, assets, HostOs::CURRENT));
        cpu::cap_input_threads(&mut args, decoders);
        let _graph = externalize_filter_complex(&mut args, &format!("part{i}"))?;
        let width = PARTS_SHARE * part.frames as f64 / plan.frames as f64;
        let bar = Bar {
            total: part.frames as f64 / fps,
            offset,
            width,
            start: started,
        };
        offset += width;
        let info = ExportPart {
            index: (i + 1) as u32,
            total: total as u32,
            encoding: false,
        };
        let mut rendered = 0u64;
        let status = run_ffmpeg_progress_counted(
            &args,
            &[],
            &file,
            bar,
            cpu::Resource::Cpu,
            // Each part is its own ffmpeg run, whose last packets are not the file closing: only the
            // last pass finishes the file.
            &mut |p| {
                let stage = p.stage.map(|s| {
                    if s == ExportStage::Finishing {
                        ExportStage::Rendering
                    } else {
                        s
                    }
                });
                progress(ExportProgress {
                    part: Some(info),
                    stage,
                    ..p
                });
            },
            cancel,
            &mut rendered,
        )?;
        if status == RenderStatus::Cancelled {
            return Ok(RenderStatus::Cancelled);
        }
        if rendered != part.frames {
            return Err(Error::Engine(format!(
                "part {} of {} came out with {rendered} frames where {} were planned",
                i + 1,
                total,
                part.frames
            )));
        }
        files.push((file_str, part.frames));
    }

    let list = dir.0.join("parts.ffconcat");
    std::fs::write(&list, concat_list(&files, plan.rate))
        .map_err(|e| Error::Engine(format!("could not write the list of parts: {e}")))?;
    let list_str = list.to_string_lossy().into_owned();
    let info = ExportPart {
        index: total as u32,
        total: total as u32,
        encoding: true,
    };
    let mut encoding = |p: ExportProgress| progress(ExportProgress { part: Some(info), ..p });
    let span = plan.frames as f64 / fps;
    let two_pass = matches!(opts.rate_control, RateControl::TwoPass)
        && matches!(opts.video_codec.as_deref(), Some(vc) if vc != "prores_ks" && vc != "gif" && enc_family(vc) == EncFamily::Software);
    if two_pass {
        let null_sink = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let passlog = std::env::temp_dir()
            .join(format!("kerf-2pass-{}", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _logs = PassLogs(passlog.clone());
        let half = (1.0 - PARTS_SHARE) / 2.0;
        for (pass, at) in [(PassPhase::First, PARTS_SHARE), (PassPhase::Second, PARTS_SHARE + half)] {
            let mut args = build_final_args(plan, assets, output_str, opts, pass, null_sink, &passlog, &list_str)?;
            let _graph = externalize_filter_complex(&mut args, "final")?;
            let status = run_ffmpeg_progress_as(
                &args,
                output,
                Bar {
                    total: span,
                    offset: at,
                    width: half,
                    start: started,
                },
                cpu::Resource::Cpu,
                &mut encoding,
                cancel,
            )?;
            if status == RenderStatus::Cancelled {
                return Ok(status);
            }
        }
        Ok(RenderStatus::Completed)
    } else {
        let mut args = build_final_args(plan, assets, output_str, opts, PassPhase::Single, "", "", &list_str)?;
        let _graph = externalize_filter_complex(&mut args, "final")?;
        run_ffmpeg_progress_as(
            &args,
            output,
            Bar {
                total: span,
                offset: PARTS_SHARE,
                width: 1.0 - PARTS_SHARE,
                start: started,
            },
            cpu::Resource::Cpu,
            &mut encoding,
            cancel,
        )
    }
}
