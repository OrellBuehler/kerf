//! The planner and the graphs of an export in parts, as strings (the renders are
//! `segments_rendered.rs`, `#[ignore]`d).

use std::collections::HashSet;
use std::path::PathBuf;

use super::segments::*;
use super::*;
use crate::engine::test_support::{audio_stream, image_stream, make_clip, test_asset, timeline_of, video_stream, video_track};
use crate::model::{Transition, TransitionKind};

fn clip(first: u64, last: u64, input: usize, mpx: f64) -> PlanClip {
    PlanClip {
        flat: input,
        first,
        last,
        input,
        mpx,
        source_per_frame: 1.0 / 50.0,
        restart: false,
    }
}

fn uhd(path: &str) -> Asset {
    let mut asset = test_asset(vec![video_stream(3840, 2160, 30.0), audio_stream(48_000, 2)]);
    asset.path = path.to_string();
    asset
}

fn logo(path: &str) -> Asset {
    let mut asset = test_asset(vec![image_stream(640, 360)]);
    asset.path = path.to_string();
    asset.duration = crate::model::DEFAULT_IMAGE_DURATION;
    asset
}

fn rate(fps: u32) -> Rational {
    Rational::new(fps, 1).unwrap()
}

/// `n` clips of `each` seconds one after the other from `asset`, source windows apart.
fn sequence(asset: &Asset, n: usize, each: f64) -> Timeline {
    let clips = (0..n)
        .map(|i| make_clip(asset.id, 1.0 + i as f64 * 0.37, 1.0 + i as f64 * 0.37 + each, i as f64 * each))
        .collect();
    timeline_of(vec![video_track(clips)])
}

fn opts() -> ExportOptions {
    ExportOptions {
        fps: Some(50.0),
        resolution: Some((1920, 1080)),
        ..ExportOptions::default()
    }
}

/// Whatever the limits, the parts tile the span.
fn assert_tiles(parts: &[Part], frames: u64) {
    assert_eq!(parts.first().map(|p| p.first), Some(0));
    for pair in parts.windows(2) {
        assert_eq!(pair[0].end(), pair[1].first, "{pair:?}");
    }
    assert_eq!(parts.last().map(Part::end), Some(frames));
    assert!(parts.iter().all(|p| p.frames > 0));
}

pub(super) fn graph_of(args: &[String]) -> String {
    args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone()
}

#[test]
fn a_span_that_fits_runs_whole() {
    let clips: Vec<PlanClip> = (0..3).map(|i| clip(i * 70, i * 70 + 70, i as usize, 8.3)).collect();
    assert_eq!(plan_parts(&clips, 210, SEGMENT_LIMITS), None);
    // Nothing at all, and one frame.
    assert_eq!(plan_parts(&[], 100, SEGMENT_LIMITS), None);
    assert_eq!(plan_parts(&clips, 1, SEGMENT_LIMITS), None);
}

#[test]
fn a_reel_of_clips_is_cut_between_them_and_every_part_fits() {
    // Forty 4K clips of 1.4 s at 50 fps, one after the other: each can touch the frame its
    // successor starts on (the window is closed at its end), so a cut falls on the frame after.
    let clips: Vec<PlanClip> = (0..40).map(|i| clip(i * 70, i * 70 + 70, i as usize, 8.3)).collect();
    let frames = 40 * 70;
    let parts = plan_parts(&clips, frames, SEGMENT_LIMITS).expect("forty UHD inputs are cut");
    assert_tiles(&parts, frames);
    for part in &parts {
        let (mpx, inputs) = open_cost(clips.iter().filter(|c| c.first < part.end() && c.last >= part.first));
        assert!(
            mpx <= SEGMENT_LIMITS.decode_mpx && inputs <= SEGMENT_LIMITS.inputs,
            "{part:?} holds {mpx} / {inputs}"
        );
    }
    // 32 Mpx is three 4K clips and a hair: a part is two whole clips and the first frame of the
    // third. Each cut is the frame after a clip's last: where only one frame is read again.
    assert!(parts.len() >= 14 && parts.len() <= 21, "{} parts", parts.len());
    for part in &parts[1..] {
        assert_eq!(part.first % 70, 1, "{part:?} starts one frame after a clip boundary");
    }
}

#[test]
fn a_still_that_is_always_on_screen_costs_no_cut() {
    // A logo over the whole span (restartable) and sixteen 4K clips under it.
    let mut clips: Vec<PlanClip> = (0..16).map(|i| clip(i * 100, i * 100 + 100, i as usize, 8.3)).collect();
    clips.push(PlanClip {
        restart: true,
        ..clip(0, 1599, 99, 0.1)
    });
    let parts = plan_parts(&clips, 1600, SEGMENT_LIMITS).expect("cut");
    assert_tiles(&parts, 1600);
    assert!(parts.len() > 4);
    // The same logo as footage would have to be decoded from the start by every part after the
    // first, which is not allowed for more than `preroll_s` of source: nothing is cut past 20 s
    // (frame 1000).
    clips.last_mut().unwrap().restart = false;
    let parts = plan_parts(&clips, 1600, SEGMENT_LIMITS).expect("cut");
    assert_tiles(&parts, 1600);
    assert!(parts.iter().skip(1).all(|p| p.first <= 1001), "{parts:?}");
}

#[test]
fn a_cut_that_would_make_a_long_clip_decode_its_start_again_is_not_made() {
    // One twenty-second clip across everything, and a 4K clip under each second.
    let mut clips: Vec<PlanClip> = (0..20).map(|i| clip(i * 50, i * 50 + 50, i as usize, 8.3)).collect();
    clips.push(clip(0, 999, 99, 8.3));
    let few = SegmentLimits {
        preroll_s: 5.0,
        ..SEGMENT_LIMITS
    };
    let parts = plan_parts(&clips, 1000, few).expect("cut");
    assert_tiles(&parts, 1000);
    // 5 s of source at 50 fps is frame 250: nothing is cut after it, so the last part holds the rest.
    assert!(parts.iter().skip(1).all(|p| p.first <= 251), "{parts:?}");
    // With no cut allowed at all the export runs whole.
    let clips = vec![clip(0, 999, 99, 40.0), clip(10, 999, 98, 40.0)];
    let none = SegmentLimits {
        preroll_s: 0.0,
        ..SEGMENT_LIMITS
    };
    assert_eq!(plan_parts(&clips, 1000, none), None);
}

#[test]
fn a_transition_keeps_both_clips_in_the_parts_it_crosses() {
    // Two clips dissolving over frames 100..150, then eight more: a cut inside the dissolve is
    // possible (both clips are read again, 25 frames of the pair) but a clean one is preferred.
    let mut clips = vec![clip(0, 149, 0, 20.0), clip(100, 300, 1, 20.0)];
    clips.extend((2..10).map(|i| clip(300 + (i - 2) * 100, 300 + (i - 2) * 100 + 100, i as usize, 20.0)));
    let parts = plan_parts(&clips, 1100, SEGMENT_LIMITS).expect("cut");
    assert_tiles(&parts, 1100);
    let first_cut = parts[0].end();
    assert!(!(101..=149).contains(&first_cut), "cut inside the dissolve at {first_cut}");
}

#[test]
fn the_plan_tiles_the_span_for_random_layouts() {
    // A seeded sweep over layouts: whatever they are, the parts tile and every clip that touches
    // a frame is in a part that covers it.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n.max(1)
    };
    let mut cut_some = 0;
    for _ in 0..300 {
        let frames = 200 + next(3000);
        let n = 2 + next(30);
        let clips: Vec<PlanClip> = (0..n)
            .map(|i| {
                let first = next(frames);
                let last = (first + next(frames / 3 + 1)).min(frames - 1);
                PlanClip {
                    restart: next(5) == 0,
                    ..clip(first, last, i as usize, 1.0 + next(12) as f64)
                }
            })
            .collect();
        let limits = SegmentLimits {
            decode_mpx: 10.0 + next(30) as f64,
            inputs: 4 + next(12) as usize,
            preroll_s: next(30) as f64,
        };
        if let Some(parts) = plan_parts(&clips, frames, limits) {
            cut_some += 1;
            assert_tiles(&parts, frames);
            for c in &clips {
                assert!(parts.iter().any(|p| c.first < p.end() && c.last >= p.first), "{c:?}");
            }
        }
    }
    assert!(cut_some > 30, "the sweep should cut many of its layouts, cut {cut_some}");
}

#[test]
fn the_canvas_has_the_frames_the_color_source_makes() {
    // `color=d=` makes frame k while k, in microseconds, is under the duration.
    assert_eq!(canvas_frames(60.0, rate(50)), 3000);
    assert_eq!(canvas_frames(59.999_999_9, rate(50)), 3000);
    assert_eq!(canvas_frames(1.0 / 3.0, rate(30)), 10);
    assert_eq!(canvas_frames(0.0, rate(30)), 0);
    assert_eq!(canvas_frames(f64::NAN, rate(30)), 0);
    assert_eq!(canvas_frames(-3.0, rate(30)), 0);
    let ntsc = Rational::from_fps(29.97).unwrap();
    assert_eq!(canvas_frames(10.0, ntsc), 300);
    assert_eq!(canvas_frames(10.01, ntsc), 300);
    assert_eq!(canvas_frames(10.02, ntsc), 301);
    // A duration a hair over a whole number of frames has that frame too.
    assert_eq!(canvas_frames(2.000_001, rate(25)), 51);
}

// ---- the plan of a cut -----------------------------------------------------------------------

#[test]
fn a_small_export_is_not_planned_in_parts() {
    let a = uhd("/m/a.mp4");
    let timeline = sequence(&a, 3, 2.0);
    assert!(plan_export(&timeline, std::slice::from_ref(&a), &opts(), SEGMENT_LIMITS).is_none());
    // Nor one a container cannot be cut for, however long.
    let long = sequence(&a, 60, 1.0);
    let gif = ExportOptions {
        container: Container::Gif,
        ..opts()
    };
    assert!(plan_export(&long, std::slice::from_ref(&a), &gif, SEGMENT_LIMITS).is_none());
    let audio = ExportOptions {
        container: Container::Mp3,
        ..opts()
    };
    assert!(plan_export(&long, std::slice::from_ref(&a), &audio, SEGMENT_LIMITS).is_none());
    assert!(plan_export(&long, std::slice::from_ref(&a), &opts(), SEGMENT_LIMITS).is_some());
}

#[test]
fn the_parts_of_a_cut_hold_the_clips_on_screen_for_them() {
    let a = uhd("/m/a.mp4");
    let assets = [a.clone()];
    let timeline = sequence(&a, 30, 1.4);
    let plan = plan_export(&timeline, &assets, &opts(), SEGMENT_LIMITS).expect("parts");
    assert_eq!(plan.frames, 2100);
    assert_tiles(&plan.parts, plan.frames);
    let rate = plan.rate;
    for part in &plan.parts {
        let contents = part_contents(&plan, part);
        let held: Vec<usize> = (0..30).filter(|&f| contents.include[f]).collect();
        assert!(held.len() <= 4, "{part:?}: {held:?}");
        // Every clip with a frame in the part is held, and each held clip has one.
        for f in 0..30usize {
            let (ws, we) = (f as f64 * 1.4, (f + 1) as f64 * 1.4);
            let on = (part.first..part.end()).any(|k| rate.frame_time(k) >= ws - 1e-3 && rate.frame_time(k) <= we + 1e-3);
            assert_eq!(contents.include[f], on, "clip {f} in {part:?}");
        }
        assert!(contents.restarts.is_empty());
    }
}

#[test]
fn a_part_graph_draws_its_clips_as_the_whole_graph_does() {
    // Dissolves and a keyed zoom across the cuts: the chain of a clip, and the overlay that lays
    // it down, are the very text the single run writes, whichever part draws it.
    let a = uhd("/m/a.mp4");
    let assets = [a.clone()];
    let mut clips: Vec<Clip> = (0..24)
        .map(|i| make_clip(a.id, 1.0 + i as f64 * 0.37, 2.4 + i as f64 * 0.37, i as f64 * 1.4))
        .collect();
    for clip in clips.iter_mut().skip(2).step_by(3) {
        clip.transition_in = Some(Transition {
            kind: TransitionKind::Crossfade,
            duration: 0.5,
        });
    }
    clips[7].channels = vec![crate::model::PropertyTrack {
        prop: crate::model::Property::Scale,
        keys: vec![
            crate::model::PropertyKey::new(0.0, 1.0),
            crate::model::PropertyKey::new(1.0, 1.4),
        ],
    }];
    let timeline = timeline_of(vec![video_track(clips)]);
    let plan = plan_export(&timeline, &assets, &opts(), SEGMENT_LIMITS).expect("parts");
    let whole_graph = graph_of(&build_export_args(&timeline, &assets, "out.mp4", &opts()).unwrap());
    let mut seen = HashSet::new();
    for (i, part) in plan.parts.iter().enumerate() {
        let args = build_part_args(&plan, &assets, &opts(), part, &[], &format!("/tmp/part-{i}.mkv"), "-fps_mode").unwrap();
        let graph = graph_of(&args);
        assert!(graph.starts_with("color=c=black:s=1920x1080:r=50:d="), "{graph}");
        assert!(graph.contains(&format!(",setpts=N+{}[", part.first)), "{graph}");
        assert!(graph.ends_with("[vpart]setpts=PTS-STARTPTS[outv]"), "{graph}");
        let contents = part_contents(&plan, part);
        for flat in (0..24).filter(|&f| contents.include[f]) {
            // The chain between the input and `[v{flat}]` differs only in the input's number.
            let chain = |g: &str| {
                g.split(';')
                    .find(|l| l.ends_with(&format!("[v{flat}]")))
                    .map(|l| l[l.find(']').unwrap() + 1..].to_string())
            };
            assert!(chain(&graph).is_some());
            assert_eq!(chain(&graph), chain(&whole_graph), "clip {flat} in part {i}");
            // ...and its overlay is the same line, where the pads before it are the part's own.
            let overlay = |g: &str| {
                g.split(';')
                    .find(|l| l.contains(&format!("][v{flat}]overlay")))
                    .map(|l| l[l.find("overlay").unwrap()..l.rfind('[').unwrap()].to_string())
            };
            assert!(overlay(&graph).is_some());
            assert_eq!(overlay(&graph), overlay(&whole_graph), "overlay of clip {flat} in part {i}");
            seen.insert(flat);
        }
        // Exactly the frames of the part, as a lossless file.
        let at = args.iter().position(|a| a == "-frames:v").unwrap();
        assert_eq!(args[at + 1], part.frames.to_string());
        assert!(args.windows(2).any(|w| w == ["-c:v", "ffv1"]));
        assert!(args.contains(&"-an".to_string()));
        let inputs = args.iter().filter(|a| *a == "-i").count();
        assert_eq!(inputs, contents.include.iter().filter(|&&b| b).count());
    }
    assert_eq!(seen.len(), 24, "every clip is drawn by some part");
}

/// A part that fails says what it was drawing, the most pixels a frame first, so a clip that is
/// slow to draw has a name: here a 45 MP still that moves (`still_motion` scales it once) over 4K
/// footage, and a small logo the general chain resamples for every frame.
#[test]
fn a_part_that_fails_names_what_it_draws_heaviest_first() {
    use crate::model::{Easing, Keyframe};
    let mut a = uhd("/m/a.mp4");
    a.name = "C086.MP4".into();
    let mut l = logo("/m/logo.png");
    l.name = "logo.png".into();
    let mut photo = test_asset(vec![image_stream(8192, 5464)]);
    photo.path = "/m/ORP_8469.jpg".into();
    photo.name = "ORP_8469.jpg".into();
    let assets = [a.clone(), l.clone(), photo.clone()];
    let mut seq = sequence(&a, 30, 1.4);
    let mut still = make_clip(photo.id, 0.0, 3.5, 0.5);
    let key = |time, scale| Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Easing::EaseInOut,
    };
    still.keyframes = vec![key(0.0, 1.06), key(3.5, 1.14)];
    let mut overlay = make_clip(l.id, 0.0, 42.0, 0.0);
    overlay.transform.scale = 0.2;
    for (name, clip) in [("V2", still), ("V3", overlay)] {
        seq.tracks.push(crate::model::Track {
            clips: vec![clip],
            ..crate::model::Track::new(StreamKind::Video, name)
        });
    }
    let cover = ExportOptions {
        fit: Fit::Cover,
        ..opts()
    };
    let plan = plan_export(&seq, &assets, &cover, SEGMENT_LIMITS).expect("parts");
    let said = part_drawing(&plan, &assets, &plan.parts[0]);
    let at = |what: &str| said.find(what).unwrap_or_else(|| panic!("{what} in {said}"));
    assert!(said.starts_with("0.00-"), "{said}");
    assert!(
        at("ORP_8469.jpg (44.8 MP still, scaled once) at 0.50-4.00 s") < at("C086.MP4 (3840x2160)"),
        "{said}"
    );
    assert!(
        at("C086.MP4") < at("logo.png (0.2 MP still, resampled every frame)"),
        "{said}"
    );
}

#[test]
fn a_still_across_a_cut_is_picked_up_there() {
    let a = uhd("/m/a.mp4");
    let l = logo("/m/logo.png");
    let assets = [a.clone(), l.clone()];
    let mut seq = sequence(&a, 30, 1.4);
    let mut overlay = make_clip(l.id, 0.0, 42.0, 0.0);
    overlay.transform.scale = 0.2;
    seq.tracks.push(crate::model::Track {
        clips: vec![overlay],
        ..crate::model::Track::new(StreamKind::Video, "V2")
    });
    let plan = plan_export(&seq, &assets, &opts(), SEGMENT_LIMITS).expect("parts");
    assert!(plan.parts.len() > 3);
    // The first part reads the logo from its start, as the whole export does.
    let first = build_part_args(&plan, &assets, &opts(), &plan.parts[0], &[], "/tmp/p0.mkv", "-fps_mode").unwrap();
    // (Decoded once, held by the chain's `loop`: no `-loop 1` decoding it again for every frame.)
    assert!(
        !first.contains(&"-loop".to_string()) && !first.contains(&"-t".to_string()),
        "{first:?}"
    );
    assert!(graph_of(&first).contains(":size=1,trim=start=0:end="), "{}", graph_of(&first));
    // A later one restarts it: the picture held, numbered onto the frames it still has.
    let part = plan.parts[2];
    let contents = part_contents(&plan, &part);
    let restart = contents.restarts.values().next().copied().expect("the logo is restarted");
    assert_eq!(restart.frame, part.first);
    assert_eq!(
        restart.frame + restart.count,
        2100,
        "it ends where the whole export's run of it does"
    );
    let args = build_part_args(&plan, &assets, &opts(), &part, &[], "/tmp/p2.mkv", "-fps_mode").unwrap();
    assert!(
        args.windows(3).any(|w| w == ["-pattern_type", "none", "-framerate"]),
        "{args:?}"
    );
    let graph = graph_of(&args);
    assert!(
        graph.contains(&format!(
            "loop=loop={}:size=1,settb=expr=1/50,setpts=N+{},trim=end_frame={}",
            restart.count - 1,
            restart.frame,
            restart.count
        )),
        "{graph}"
    );
    // The restarted input carries no `-t`, which is the whole export's way of ending it.
    let at = args.iter().position(|a| a.ends_with("logo.png")).unwrap();
    assert!(!args[..at].iter().rev().take(8).any(|a| a == "-t"), "{args:?}");
}

#[test]
fn a_part_with_no_clip_is_a_canvas_with_what_is_drawn_on_it() {
    let a = uhd("/m/a.mp4");
    let assets = [a.clone()];
    // Two groups of clips with a long gap between them and a caption over the gap.
    let mut clips: Vec<Clip> = (0..20)
        .map(|i| make_clip(a.id, 1.0 + i as f64 * 0.37, 2.4 + i as f64 * 0.37, i as f64 * 1.4))
        .collect();
    clips.extend((0..20).map(|i| make_clip(a.id, 1.0 + i as f64 * 0.37, 2.4 + i as f64 * 0.37, 60.0 + i as f64 * 1.4)));
    let mut timeline = timeline_of(vec![video_track(clips)]);
    timeline.overlays = vec![crate::model::TextOverlay::new("hello", 30.0, 40.0)];
    let ten_bit = ExportOptions {
        pix_fmt: Some("yuv420p10le".into()),
        ..opts()
    };
    let plan = plan_export(&timeline, &assets, &ten_bit, SEGMENT_LIMITS).expect("parts");
    // The plan joins the gap to the parts beside it; a part of the gap alone is as good a part.
    let gap = &Part {
        first: 1600,
        frames: 100,
    };
    assert!(part_contents(&plan, gap).include.iter().all(|&b| !b));
    let args = build_part_args(&plan, &assets, &ten_bit, gap, &[], "/tmp/gap.mkv", "-fps_mode").unwrap();
    let graph = graph_of(&args);
    assert_eq!(args.iter().filter(|a| *a == "-i").count(), 0);
    // The canvas, in the 8-bit 4:2:0 the overlays compose in, then the caption, then out.
    assert!(
        graph.contains(&format!(",format=yuv420p10le,setpts=N+{},format=yuv420p[vtext]", gap.first)),
        "{graph}"
    );
    assert!(graph.contains("drawtext="), "{graph}");
    assert!(graph.ends_with("[vpart]setpts=PTS-STARTPTS[outv]"));
}

#[test]
fn the_last_pass_reads_the_parts_and_mixes_the_sound_of_the_whole_cut() {
    let a = uhd("/m/a.mp4");
    let mut silent = uhd("/m/silent.mp4");
    silent.streams.truncate(1);
    let assets = [a.clone(), silent.clone()];
    let mut clips: Vec<Clip> = (0..24)
        .map(|i| make_clip(a.id, 1.0 + i as f64 * 0.37, 2.4 + i as f64 * 0.37, i as f64 * 1.4))
        .collect();
    clips[3] = make_clip(silent.id, 1.0, 2.4, 3.0 * 1.4);
    let timeline = timeline_of(vec![video_track(clips)]);
    let aac = ExportOptions {
        video_codec: Some("libx264".into()),
        audio_codec: Some("aac".into()),
        faststart: true,
        loudness: Some(LoudnessPreset::Youtube),
        ..opts()
    };
    let plan = plan_export(&timeline, &assets, &aac, SEGMENT_LIMITS).expect("parts");
    let args = build_final_args(
        &plan,
        &assets,
        "/out/cut.mp4",
        &aac,
        PassPhase::Single,
        "",
        "",
        "/tmp/parts.ffconcat",
    )
    .unwrap();
    assert_eq!(&args[3..9], ["-f", "concat", "-safe", "0", "-i", "/tmp/parts.ffconcat"]);
    // The sound comes from the 23 clips that have any: each opened for its sound alone.
    assert_eq!(args.iter().filter(|a| *a == "-vn").count(), 23);
    assert_eq!(args.iter().filter(|a| *a == "-i").count(), 24);
    let graph = graph_of(&args);
    assert!(graph.starts_with("[0:v]settb=expr=1/50,setpts=N[outv];"), "{graph}");
    assert!(graph.contains("[1:a]atrim=") && graph.contains("[23:a]atrim="), "{graph}");
    assert!(!graph.contains("[0:a]"), "{graph}");
    assert!(graph.contains("loudnorm=I=-14"), "{graph}");
    assert!(graph.ends_with("[outa]"), "{graph}");
    // The encode is the user's, to the file.
    assert!(args.windows(2).any(|w| w == ["-c:v", "libx264"]));
    assert!(args.windows(2).any(|w| w == ["-movflags", "+faststart"]));
    assert_eq!(args.last().unwrap(), "/out/cut.mp4");
    // The first pass of a two-pass encode reads the picture alone.
    let first = build_final_args(
        &plan,
        &assets,
        "/out/cut.mp4",
        &aac,
        PassPhase::First,
        "/dev/null",
        "/tmp/log",
        "/tmp/p.ffconcat",
    )
    .unwrap();
    assert_eq!(first.iter().filter(|a| *a == "-i").count(), 1);
    assert!(first.contains(&"-an".to_string()) && first.last().unwrap() == "/dev/null");
}

#[test]
fn the_list_of_parts_names_each_file_and_its_length() {
    let text = concat_list(
        &[("/c/a'b/p0.mkv".to_string(), 150), ("/c/p1.mkv".to_string(), 75)],
        Rational::from_fps(29.97).unwrap(),
    );
    assert_eq!(
        text,
        "ffconcat version 1.0\nfile '/c/a'\\''b/p0.mkv'\nduration 5.005005\nfile '/c/p1.mkv'\nduration 2.502503\n"
    );
}

#[test]
fn titles_are_cut_to_the_pictures_a_part_shows() {
    fn seq(first_frame: u64, pictures: u64) -> svg_layer::SvgSeq<'static> {
        svg_layer::SvgSeq {
            svg: "",
            dir: PathBuf::from("/c/k"),
            input: "/c/k/%06d.png".into(),
            kind: svg_layer::Kind::Frames,
            first_frame,
            frames: pictures,
            first_picture: 0,
            pictures,
            doc_t0: 0.0,
            fps: Rational::new(50, 1).unwrap(),
            rect: svg_layer::Rect {
                x: 0,
                y: 0,
                w: 10,
                h: 10,
            },
            opacity: 1.0,
            start: first_frame as f64 / 50.0,
            end: (first_frame + pictures - 1) as f64 / 50.0,
            fonts: crate::fonts::FontSet::none(),
        }
    }
    let r = rate(50);
    let all = [seq(100, 200)]; // frames 100..300
    let cut = |first, frames| part_titles(&all, r, &Part { first, frames });
    assert!(cut(0, 100).is_empty() && cut(300, 50).is_empty());
    let t = &cut(50, 100)[0]; // frames 50..150: the title's first 50 pictures
    assert_eq!((t.first_frame, t.first_picture, t.pictures), (100, 0, 50));
    let t = &cut(150, 100)[0]; // frames 150..250: pictures 50..150
    assert_eq!((t.first_frame, t.first_picture, t.pictures), (150, 50, 100));
    let t = &cut(250, 100)[0]; // frames 250..350: the last 50
    assert_eq!((t.first_frame, t.first_picture, t.pictures), (250, 150, 50));
}

#[test]
fn a_multi_format_export_of_a_long_cut_is_rendered_one_delivery_at_a_time() {
    let a = uhd("/m/a.mp4");
    let assets = [a.clone()];
    let variants = [
        ExportVariant {
            delivery: Delivery {
                width: 1080,
                height: 1920,
                fit: Fit::Cover,
                fps: None,
            },
            output: PathBuf::from("/o/cut-9x16.mp4"),
        },
        ExportVariant {
            delivery: Delivery {
                width: 1920,
                height: 1080,
                fit: Fit::Contain,
                fps: None,
            },
            output: PathBuf::from("/o/cut-16x9.mp4"),
        },
    ];
    let long = sequence(&a, 40, 1.4);
    let short = sequence(&a, 3, 1.4);
    assert!(together::export_in_parts(&long, &assets, &variants, &opts()));
    assert!(!together::export_in_parts(&short, &assets, &variants, &opts()));
}

// ---- a part draws a clip as the whole export does ------------------------------------------------

/// `s` split at `sep` where ffmpeg's graph parser would: not inside single quotes, not after a
/// backslash (a filter's expression holds commas and a `sendcmd` list holds semicolons).
pub(super) fn split_top(s: &str, sep: char) -> Vec<&str> {
    let (mut parts, mut from, mut quoted, mut escaped) = (Vec::new(), 0, false, false);
    for (i, c) in s.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '\'' => quoted = !quoted,
            c if c == sep && !quoted => {
                parts.push(&s[from..i]);
                from = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[from..]);
    parts
}

/// The statements of a graph (split once) that make clip `flat`'s picture: the chain ending `[v{flat}]`,
/// and the inner statements of a blurred fill before it, the source label of the first taken off. What
/// is left is the clip's chain, which is the same whichever input it reads.
pub(super) fn clip_chain(statements: &[&str], flat: usize) -> Option<Vec<String>> {
    let end = format!("[v{flat}]");
    let inner = ["b", "f", "bb", "ff"].map(|k| format!("[bfc{flat}{k}]"));
    let at = statements.iter().position(|s| s.ends_with(&end))?;
    let mut mine: Vec<String> = (0..=at)
        .filter(|&i| i == at || inner.iter().any(|label| statements[i].contains(label.as_str())))
        .map(|i| statements[i].to_string())
        .collect();
    let first = &mut mine[0];
    *first = first[first.find(']')? + 1..].to_string();
    Some(mine)
}

/// A restarted still's chain starts `settb,setpts=N+k,trim=end_frame=n` where the whole export's
/// starts `trim,setpts=PTS-STARTPTS+…` (a crop sits between those two): the same chain once those
/// stamps are taken off both.
pub(super) fn without_stamp(mut chain: Vec<String>, restarted: bool) -> Vec<String> {
    let filters: Vec<String> = split_top(&chain[0], ',').iter().map(ToString::to_string).collect();
    let kept: Vec<&str> = filters
        .iter()
        .map(String::as_str)
        .filter(|f| {
            // A still's hold counts the pictures it keeps: the whole run's from its start, a part's
            // from the cut.
            let stamp = if restarted {
                f.starts_with("settb=expr=") || f.starts_with("setpts=N+") || f.starts_with("trim=end_frame=")
            } else {
                f.starts_with("trim=start=") || f.starts_with("setpts=PTS-STARTPTS")
            } || f.starts_with("loop=loop=");
            !stamp
        })
        .collect();
    chain[0] = kept.join(",");
    chain
}

/// What a statement list says about how clip `flat` is laid over the canvas: its `overlay` filter.
fn overlay_of(statements: &[&str], flat: usize) -> Option<String> {
    let filter = |at: &str, name: &str| {
        statements
            .iter()
            .find(|l| l.contains(at))
            .map(|l| l[l.find(name).unwrap()..l.rfind('[').unwrap()].to_string())
    };
    // A blended clip is placed by the `overlay` of its colour stage (the alpha stage's is the same) and mixed by
    // its `blend`.
    filter(&format!("][v{flat}]overlay"), "overlay").or_else(|| {
        let placed = filter(&format!("[bld{flat}lo]overlay"), "overlay")?;
        Some(format!("{placed} {}", filter(&format!("[bld{flat}u]blend"), "blend")?))
    })
}

/// Whether every clip a part draws has the chain, and the overlay, it has in the whole export's graph
/// (`whole_args` is the whole export's argv). `Err` names the first that does not; `Ok` counts the clips
/// compared.
pub(super) fn parts_draw_as_whole(
    plan: &PartsPlan,
    assets: &[Asset],
    opts: &ExportOptions,
    whole_args: &[String],
) -> std::result::Result<usize, String> {
    let whole_graph = graph_of(whole_args);
    let whole = split_top(&whole_graph, ';');
    let floating = svg_layer::svg_sequences(&plan.cut, &plan.fmt);
    let mut compared = 0;
    for (i, part) in plan.parts.iter().enumerate() {
        let args = build_part_args(
            plan,
            assets,
            opts,
            part,
            &floating,
            &format!("/tmp/part-{i}.mkv"),
            "-fps_mode",
        )
        .map_err(|e| format!("part {i}: {e}"))?;
        let graph = graph_of(&args);
        let mine = split_top(&graph, ';');
        let contents = part_contents(plan, part);
        for flat in (0..contents.include.len()).filter(|&f| contents.include[f]) {
            let restarted = contents.restarts.contains_key(&flat);
            let (Some(drawn), Some(expected)) = (clip_chain(&mine, flat), clip_chain(&whole, flat)) else {
                return Err(format!("clip {flat} has no chain in part {i} or in the whole graph"));
            };
            let (drawn, expected) = if restarted {
                (without_stamp(drawn, true), without_stamp(expected, false))
            } else {
                (drawn, expected)
            };
            if drawn != expected {
                return Err(format!(
                    "clip {flat} in part {i}{}:\n part  {drawn:?}\n whole {expected:?}",
                    if restarted { " (restarted)" } else { "" }
                ));
            }
            let (lay, whole_lay) = (overlay_of(&mine, flat), overlay_of(&whole, flat));
            if lay != whole_lay || lay.is_none() {
                return Err(format!(
                    "the overlay of clip {flat} in part {i}:\n part  {lay:?}\n whole {whole_lay:?}"
                ));
            }
            compared += 1;
        }
    }
    Ok(compared)
}

/// Twenty-four 4K clips under a logo that stays on screen, with every kind of zoom and pan the fit
/// order has an opinion on: zoomed in and out, a keyed zoom that never moves and one that does, a
/// pan, reversed, cropped, under a vignette.
fn zoomed_reel() -> (Timeline, Vec<Asset>) {
    use crate::model::{Property, PropertyKey, PropertyTrack};
    let a = uhd("/m/a.mp4");
    let l = logo("/m/logo.png");
    let mut clips: Vec<Clip> = (0..24)
        .map(|i| make_clip(a.id, 1.0 + i as f64 * 0.37, 2.4 + i as f64 * 0.37, i as f64 * 1.4))
        .collect();
    let keyed = |keys: [(f64, f64); 2], prop| PropertyTrack {
        prop,
        keys: keys.iter().map(|&(t, v)| PropertyKey::new(t, v)).collect(),
    };
    clips[3].transform.scale = 1.3;
    clips[4].channels = vec![keyed([(0.0, 1.3), (1.0, 1.3)], Property::Scale)];
    clips[5].channels = vec![keyed([(0.0, 1.0), (1.0, 1.5)], Property::Scale)];
    clips[6].transform.scale = 0.8;
    clips[7].transform.pos_x = 0.3;
    clips[8].speed = -1.0;
    clips[8].transform.scale = 1.3;
    clips[9].transform.scale = 1.1;
    clips[9].channels = vec![keyed([(0.0, 0.0), (1.0, 0.4)], Property::PosX)];
    clips[10].transform.crop_left = 0.1;
    clips[10].transform.scale = 1.2;
    clips[11].effects = vec![crate::model::VideoEffect::Vignette];
    clips[11].transform.scale = 1.3;
    clips[12].channels = vec![
        keyed([(0.0, 1.2), (1.0, 1.2)], Property::Scale),
        keyed([(0.0, 0.2), (1.0, 0.2)], Property::PosX),
    ];
    let mut overlay = make_clip(l.id, 0.0, 34.0, 0.0);
    overlay.transform.scale = 1.3;
    overlay.transform.pos_x = 0.2;
    let mut timeline = timeline_of(vec![video_track(clips)]);
    timeline.tracks.push(crate::model::Track {
        clips: vec![overlay],
        ..crate::model::Track::new(StreamKind::Video, "V2")
    });
    // A second logo that is shrunk and moved: under Cover it reveals what the frame's crop would hide.
    let l2 = logo("/m/logo2.png");
    let mut small = make_clip(l2.id, 0.0, 34.0, 0.0);
    small.transform.scale = 0.8;
    small.transform.pos_x = 0.2;
    timeline.tracks.push(crate::model::Track {
        clips: vec![small],
        ..crate::model::Track::new(StreamKind::Video, "V3")
    });
    (timeline, vec![a, l, l2])
}

#[test]
fn a_part_fits_and_zooms_a_clip_in_the_order_the_whole_export_does() {
    // Under Contain a zoom in is folded into the fit's own scale, under Cover it is cut from the whole
    // picture rather than the frame's crop of it, and a zoom that holds still counts as one that is
    // static: the order is the clip's and the frame's, so a part must not judge it otherwise — not for
    // a keyed zoom that holds still, not for a Cover clip whose picture overhangs the frame, and not
    // for a still the part picks up at a cut, which has a picture of its own size too.
    let (timeline, assets) = zoomed_reel();
    let mut restarted = 0;
    for fit in [Fit::Contain, Fit::Cover, Fit::Blur] {
        for resolution in [(1920, 1080), (1080, 1920), (1080, 1080)] {
            let opts = ExportOptions {
                fit,
                resolution: Some(resolution),
                ..opts()
            };
            let plan = plan_export(&timeline, &assets, &opts, SEGMENT_LIMITS).expect("parts");
            assert!(plan.parts.len() > 3);
            let whole = build_export_args(&timeline, &assets, "out.mp4", &opts).unwrap();
            let compared =
                parts_draw_as_whole(&plan, &assets, &opts, &whole).unwrap_or_else(|e| panic!("{fit:?} {resolution:?}: {e}"));
            assert!(compared >= 26, "{compared} clips compared");
            restarted += plan
                .parts
                .iter()
                .filter(|p| !part_contents(&plan, p).restarts.is_empty())
                .count();
        }
    }
    assert!(restarted >= 54, "the logo is picked up at a cut in later parts: {restarted}");
}
