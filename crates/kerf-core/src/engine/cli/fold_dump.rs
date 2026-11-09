//! Independent check of the blend fold, part 1: seeded timelines and the argv they build (compiles at the base commit too).
//!
//! `KERF_DUMP_OUT=file cargo test -p kerf-core --no-default-features fold_dump -- --ignored` writes one JSON line per seed:
//! the whole export's argv and the parts the planner cuts it into, for a diff between commits.

#![allow(clippy::print_stderr)]

use super::*;
use crate::engine::test_support::{image_stream, make_clip, test_asset, timeline_of, video_stream, video_track};
use crate::model::{BlendMode, Mask, Transition, TransitionKind, VideoEffect};

pub(super) struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
    pub fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.next(of.len() as u64) as usize]
    }
}

pub(super) fn fixture_assets() -> Vec<Asset> {
    let mut base = test_asset(vec![video_stream(640, 360, 30.0)]);
    base.path = "/media/base.mp4".into();
    let mut src = test_asset(vec![video_stream(640, 360, 30.0)]);
    src.path = "/media/src.mp4".into();
    let mut src25 = test_asset(vec![video_stream(1280, 720, 25.0)]);
    src25.path = "/media/src25.mp4".into();
    let mut logo = test_asset(vec![image_stream(64, 64)]);
    logo.path = "/media/logo.png".into();
    vec![base, src, src25, logo]
}

pub(super) const RATES: [f64; 9] = [24.0, 25.0, 30000.0 / 1001.0, 30.0, 50.0, 60.0, 23.976, 29.97, 59.94];

/// A seeded timeline: a base clip and one to three tracks of footage, stills and blended clips of every kind, on and between
/// the frames; `max_run` caps how many blended footage clips follow one another on a track (so a corpus can be kept under the
/// fold's minimum).
pub(super) fn seeded(seed: u64, max_blended: Option<usize>) -> (Timeline, Vec<Asset>, ExportOptions) {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    for _ in 0..8 {
        rng.next(2);
    }
    let assets = fixture_assets();
    let fps = rng.pick(&RATES);
    let mut tracks = vec![video_track(vec![make_clip(assets[0].id, 0.0, 30.0, 0.0)])];
    let mut blended = 0usize;
    for _ in 0..1 + rng.next(3) {
        let n = rng.next(16);
        let aligned = rng.next(3) != 0;
        let mut at = 0.0;
        let mut clips = Vec::new();
        for _ in 0..n {
            let frames = 1.0 + rng.next(6) as f64 + if aligned { 0.0 } else { rng.next(100) as f64 / 100.0 };
            let gap = if rng.next(8) == 0 { rng.next(4) as f64 } else { 0.0 };
            let overlap = if rng.next(10) == 0 { rng.next(3) as f64 } else { 0.0 };
            let start = ((at * fps + gap - overlap).max(0.0)) / fps;
            let kind = rng.next(14);
            let mut clip = if kind == 0 {
                make_clip(assets[3].id, 0.0, frames / fps, start)
            } else if kind < 4 {
                make_clip(assets[2].id, 0.5, 0.5 + frames / fps, start)
            } else {
                make_clip(assets[1].id, 0.5, 0.5 + frames / fps, start)
            };
            let blend = rng.next(5) != 0;
            if blend && max_blended.is_none_or(|m| blended < m) {
                clip.blend_mode = BlendMode::ALL[1 + rng.next(BlendMode::ALL.len() as u64 - 1) as usize];
                blended += 1;
            }
            match rng.next(16) {
                0 => clip.speed = 2.0,
                1 => clip.speed = 0.5,
                2 => {
                    clip.transition_in = Some(Transition {
                        kind: TransitionKind::Crossfade,
                        duration: 1.0 / fps,
                    });
                }
                3 => {
                    clip.transition_in = Some(Transition {
                        kind: TransitionKind::DipToBlack,
                        duration: 2.0 / fps,
                    });
                }
                4 => {
                    clip.transition_in = Some(Transition {
                        kind: TransitionKind::SlideLeft,
                        duration: 2.0 / fps,
                    });
                }
                5 => clip.fade_in = 1.0 / fps,
                6 => clip.fade_out = 1.0 / fps,
                7 => clip.transform.opacity = 0.5,
                8 => clip.transform.scale = 0.6,
                9 => clip.mask = Some(Mask::default()),
                10 => clip.effects.push(VideoEffect::Blur { sigma: 2.0 }),
                _ => {}
            }
            at = clip.timeline_end();
            clips.push(clip);
        }
        tracks.push(video_track(clips));
    }
    let opts = ExportOptions {
        fps: Some(fps),
        resolution: Some((640, 360)),
        ..ExportOptions::default()
    };
    (timeline_of(tracks), assets, opts)
}

#[test]
#[ignore = "writes the argv of seeded timelines to $KERF_DUMP_OUT"]
fn fold_dump() {
    let Some(out) = std::env::var_os("KERF_DUMP_OUT") else {
        eprintln!("KERF_DUMP_OUT not set; nothing to write");
        return;
    };
    let seeds: u64 = std::env::var("KERF_DUMP_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let mut lines = String::new();
    for seed in 0..seeds {
        let (tl, assets, opts) = seeded(seed, None);
        let args = build_export_args(&tl, &assets, "out.mkv", &opts).map_err(|e| e.to_string());
        let parts = segments::plan_export(&tl, &assets, &opts, segments::SEGMENT_LIMITS).map(|p| format!("{:?}", p.parts));
        lines.push_str(&serde_json::to_string(&serde_json::json!({ "seed": seed, "args": args.ok(), "parts": parts })).unwrap());
        lines.push('\n');
    }
    std::fs::write(out, lines).unwrap();
}
