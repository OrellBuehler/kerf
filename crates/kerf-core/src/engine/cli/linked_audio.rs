//! The sound of detached and linked clips.
//!
//! **The doubling.** The export mixes the audio of *every* clip whose asset carries
//! an audio stream — a video clip's own sound included — so `extract_audio`, which
//! used to append the asset's audio to an audio track without touching the picture
//! clip, made anything already cut onto V1 sound twice. The pure tests pin that at
//! graph level (`amix=inputs=2`, two chains reading the same window) and the fix
//! (`amix=inputs=1`); the `#[ignore]`d one *measures* it on a real render: the old
//! recipe comes out twice the level (+6 dB), the fixed one at the level of the clip
//! alone, and once detached the audio track's own fader rules the sound.
//!
//! `cargo test -p kerf-core --no-default-features -- --ignored linked_audio`

use std::path::{Path, PathBuf};

use super::*;
use crate::engine::test_support::{
    audio_track, av_asset, make_clip, single, test_asset, timeline_of, video_stream, video_track, StatusBounded,
};
use crate::model::{AudioEffect, Clip, Transition, TransitionKind};
use crate::project::Project;
use uuid::Uuid;

/// The `-filter_complex` of the export of `timeline`.
fn graph_of(timeline: &Timeline, assets: &[Asset]) -> String {
    let args = build_export_args(timeline, assets, "out.mp4", &ExportOptions::default()).unwrap();
    let at = args.iter().position(|a| a == "-filter_complex").expect("a graph") + 1;
    args[at].clone()
}

/// Every per-clip audio chain in a graph — the filters between a clip's source pad
/// (`[N:a]`, or one output of the `asplit` that fans a shared input out) and its
/// `[aK]` output — without the pad names, so two graphs' chains compare on what
/// they *do*.
fn audio_chains(graph: &str) -> Vec<String> {
    graph
        .split(';')
        .filter_map(|c| {
            let (head, rest) = c.strip_prefix('[')?.split_once(']')?;
            let (body, out) = rest.rsplit_once('[')?;
            let is_source = head.ends_with(":a") || head.starts_with("asp");
            let is_clip_out = out
                .strip_prefix('a')
                .is_some_and(|n| n.trim_end_matches(']').chars().all(|d| d.is_ascii_digit()));
            (is_source && is_clip_out && !body.starts_with("asplit")).then(|| body.to_string())
        })
        .collect()
}

/// An A/V asset with 10 s of footage.
fn av(duration: f64) -> Asset {
    av_asset(Uuid::new_v4(), duration)
}

#[test]
fn doubled_audio_is_what_extract_audio_used_to_make() {
    let asset = av(10.0);
    let assets = vec![asset.clone()];
    // The old recipe: the picture on V1 with its own sound, and the asset's whole
    // audio appended to A1 (the track was empty, so at 0) — nothing muted.
    let old = timeline_of(vec![
        video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]),
        audio_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]),
    ]);
    let graph = graph_of(&old, &assets);
    assert!(graph.contains("amix=inputs=2:normalize=0"), "{graph}");
    let chains = audio_chains(&graph);
    assert_eq!(chains.len(), 2, "{graph}");
    assert_eq!(
        chains[0], chains[1],
        "both chains read the same window, at the same moment: the sound is summed with itself"
    );
    assert!(chains[0].contains("atrim=start=0:end=10"), "{graph}");

    // Detached: the same picture, the sound on A1 once.
    let mut fixed = old;
    fixed.tracks[0].clips[0].source_audio = false;
    let graph = graph_of(&fixed, &assets);
    assert!(graph.contains("amix=inputs=1:normalize=0"), "{graph}");
    assert_eq!(audio_chains(&graph).len(), 1, "{graph}");
    // ...and the picture is still there.
    assert!(graph.contains("[0:v]") && graph.contains("[outv]"), "{graph}");
}

#[test]
fn extract_audio_of_a_cut_asset_detaches_rather_than_doubles() {
    let asset = av(10.0);
    let project = Project::open_in_memory().unwrap();
    project.insert_asset(&asset).unwrap();
    let mut timeline = project.timeline().unwrap();
    timeline.tracks[0].clips.push(Clip::for_asset(&asset, 0.0, 10.0, 0.0));
    project.save_timeline(&timeline).unwrap();

    let audio = project.extract_audio(asset.id).unwrap();
    let timeline = project.timeline().unwrap();
    let picture = &timeline.tracks[0].clips[0];
    assert!(!picture.source_audio, "the picture no longer plays its own sound");
    assert_eq!(audio.timeline_start, 0.0);
    assert_eq!((audio.source_in, audio.source_out), (0.0, 10.0));
    assert_eq!(picture.link_id, audio.link_id, "and the pair is linked");
    assert!(picture.link_id.is_some());
    let graph = graph_of(&timeline, &[asset]);
    assert!(graph.contains("amix=inputs=1:normalize=0"), "{graph}");
}

#[test]
fn an_asset_not_in_the_cut_is_still_just_appended() {
    let asset = av(10.0);
    let project = Project::open_in_memory().unwrap();
    project.insert_asset(&asset).unwrap();
    let audio = project.extract_audio(asset.id).unwrap();
    let timeline = project.timeline().unwrap();
    let a1 = timeline.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap();
    assert_eq!(a1.clips.len(), 1);
    assert_eq!(a1.clips[0].id, audio.id);
    assert!(audio.link_id.is_none(), "nothing to link it to");
    assert!(timeline.tracks.iter().all(|t| t.clips.iter().all(|c| c.source_audio)));
}

#[test]
fn a_detached_clips_sound_chain_is_the_one_it_had() {
    // Everything that shapes the sound — speed, volume, effects, fades, the dip's
    // sound fade — must survive the move to the audio track unchanged: detaching
    // changes where the sound lives, not what it is.
    let asset = av(30.0);
    let assets = vec![asset.clone()];
    let mut first = make_clip(asset.id, 2.0, 8.0, 0.0);
    first.volume = 0.6;
    first.fade_out = 0.5;
    let mut second = make_clip(asset.id, 10.0, 16.0, 6.0);
    second.speed = 1.25;
    second.volume = 0.8;
    second.fade_in = 0.4;
    second.audio = vec![AudioEffect::Highpass { hz: 120.0 }];
    second.transition_in = Some(Transition {
        kind: TransitionKind::DipToBlack,
        duration: 1.0,
    });
    let attached = single(vec![first.clone(), second.clone()]);
    let want = audio_chains(&graph_of(&attached, &assets));
    assert_eq!(want.len(), 2);

    let mut detached = attached;
    for id in [first.id, second.id] {
        detached.detach_audio(id, true).unwrap();
    }
    let got = audio_chains(&graph_of(&detached, &assets));
    // The pictures are muted, so only the two audio-track chains remain — and they
    // are the chains the pictures had.
    assert_eq!(got.len(), 2, "{got:?}");
    let (mut got, mut want) = (got, want);
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn a_muted_picture_is_not_part_of_the_mix_and_a_timeline_of_only_those_has_no_audio() {
    let asset = av(10.0);
    let assets = vec![asset.clone()];
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.source_audio = false;
    let args = build_export_args(&single(vec![clip]), &assets, "out.mp4", &ExportOptions::default()).unwrap();
    assert!(!args.iter().any(|a| a == "[outa]"), "nothing to map: {args:?}");
    let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
    assert!(!graph.contains("amix"), "{graph}");
    assert!(graph.contains("[outv]"), "the picture still renders: {graph}");
    // It is not an explicit mute either: no `-an` for a cut that simply has no sound.
    assert!(!args.iter().any(|a| a == "-an"), "{args:?}");
}

#[test]
fn links_never_reach_the_graph() {
    let asset = av(10.0);
    let assets = vec![asset.clone()];
    let plain = timeline_of(vec![
        video_track(vec![make_clip(asset.id, 0.0, 5.0, 0.0), make_clip(asset.id, 5.0, 10.0, 5.0)]),
        audio_track(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]),
    ]);
    let mut linked = plain.clone();
    let group = Uuid::new_v4();
    linked.tracks[0].clips[0].link_id = Some(group);
    linked.tracks[1].clips[0].link_id = Some(group);
    let opts = ExportOptions::default();
    assert_eq!(
        build_export_args(&plain, &assets, "out.mp4", &opts).unwrap(),
        build_export_args(&linked, &assets, "out.mp4", &opts).unwrap()
    );
    // And the field is invisible in a saved project until it is used.
    let json = serde_json::to_string(&plain).unwrap();
    assert!(!json.contains("link_id") && !json.contains("source_audio"), "{json}");
    let json = serde_json::to_string(&linked).unwrap();
    assert!(json.contains("link_id") && !json.contains("source_audio"), "{json}");
}

#[test]
fn a_silent_asset_cannot_be_detached() {
    let silent = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let mut timeline = single(vec![make_clip(silent.id, 0.0, 5.0, 0.0)]);
    let id = timeline.tracks[0].clips[0].id;
    // The timeline cannot know the asset has no sound — `Project` tells it.
    assert!(timeline.detach_audio(id, false).is_err());
    assert!(timeline.tracks[0].clips[0].source_audio, "a refused detach changes nothing");
}

// ---- measured on a render ---------------------------------------------------

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-linked-audio-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Two seconds of a tiny test picture and a 440 Hz sine (lossless PCM, so the level
/// measured is the level made), probed like an import.
fn media(dir: &Path) -> Asset {
    let path = dir.join("av.mkv");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=s=32x18:r=25:d=2")
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "pcm_s16le", "-ac", "2"])
        .arg(&path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    Project::probe_asset(&path).expect("probe the test media")
}

/// The audio the export of `timeline` renders, as mono f32 at 48 kHz.
fn export_audio(timeline: &Timeline, assets: &[Asset], dir: &Path, tag: &str) -> Vec<f32> {
    // An audio container: the graph carries only the mix, so nothing is left unmapped.
    let opts = ExportOptions {
        container: Container::Wav,
        ..ExportOptions::default()
    };
    let out = dir.join(format!("{tag}.f32"));
    let mut args = build_export_args(timeline, assets, "unused.mkv", &opts).unwrap();
    let graph = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
    args.truncate(graph + 1);
    args.extend(["-map", "[outa]", "-f", "f32le", "-ac", "1", "-ar", "48000", "-y"].map(String::from));
    args.push(out.to_string_lossy().into_owned());
    let run = command(&ffmpeg_bin()).args(&args).stdin(Stdio::null()).output().unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&run.stderr)
    );
    let bytes = std::fs::read(&out).unwrap();
    bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// RMS of the middle of the render — clear of the edges, where the mix ramps.
fn level(samples: &[f32]) -> f64 {
    let (from, to) = (48_000 * 3 / 10, 48_000 * 17 / 10);
    assert!(samples.len() > to, "{} samples", samples.len());
    let window = &samples[from..to];
    (window.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / window.len() as f64).sqrt()
}

#[test]
#[ignore = "needs the ffmpeg binary"]
#[allow(clippy::print_stderr)]
fn the_old_extract_audio_doubled_the_level_and_detaching_does_not() {
    let dir = scratch("doubling");
    let asset = media(&dir);
    let assets = vec![asset.clone()];
    let dur = asset.duration.min(2.0);

    // The clip alone: the level every other render is judged against.
    let alone = timeline_of(vec![
        video_track(vec![make_clip(asset.id, 0.0, dur, 0.0)]),
        audio_track(vec![]),
    ]);
    let base = level(&export_audio(&alone, &assets, &dir, "alone"));
    assert!(base > 0.02, "the sine rendered: {base}");

    // The old recipe: the same asset's audio appended to A1, the picture's own
    // sound left on — the level doubles (+6.02 dB).
    let old = timeline_of(vec![
        video_track(vec![make_clip(asset.id, 0.0, dur, 0.0)]),
        audio_track(vec![make_clip(asset.id, 0.0, dur, 0.0)]),
    ]);
    let doubled = level(&export_audio(&old, &assets, &dir, "old"));
    let gain_db = 20.0 * (doubled / base).log10();
    eprintln!("old extract_audio: {gain_db:+.2} dB over the clip alone (rms {doubled:.4} vs {base:.4})");
    assert!(
        (gain_db - 6.02).abs() < 0.3,
        "the old extract_audio sums the sound with itself: {gain_db:.2} dB over the clip alone"
    );

    // The fix, through the real operation: the level is the clip's own again.
    let project = Project::open_in_memory().unwrap();
    project.insert_asset(&asset).unwrap();
    let mut timeline = project.timeline().unwrap();
    timeline.tracks[0].clips.push(Clip::for_asset(&asset, 0.0, dur, 0.0));
    project.save_timeline(&timeline).unwrap();
    project.extract_audio(asset.id).unwrap();
    let timeline = project.timeline().unwrap();
    let fixed = level(&export_audio(&timeline, &assets, &dir, "fixed"));
    let gain_db = 20.0 * (fixed / base).log10();
    eprintln!("fixed extract_audio: {gain_db:+.2} dB");
    assert!(
        gain_db.abs() < 0.3,
        "extract_audio now leaves the sound where it was: {gain_db:.2} dB"
    );

    // And the sound is now the audio track's: pulling A1's fader down 6 dB lowers it
    // by 6 dB, which no fader could do while it rode the picture.
    let a1 = timeline.tracks.iter().find(|t| t.kind == StreamKind::Audio).unwrap().id;
    project.set_track_volume(a1, 0.5).unwrap();
    let timeline = project.timeline().unwrap();
    let ducked = level(&export_audio(&timeline, &assets, &dir, "fader"));
    let gain_db = 20.0 * (ducked / base).log10();
    eprintln!("detached audio, A1 fader at 0.5: {gain_db:+.2} dB");
    assert!(
        (gain_db + 6.02).abs() < 0.3,
        "the fader rides the detached sound: {gain_db:.2} dB"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
