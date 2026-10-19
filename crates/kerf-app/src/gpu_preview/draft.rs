//! A drag's frame: the cut as the gesture under way would leave it, drawn before anything is written.
//!
//! While a gesture runs (an Inspector slider, a title moved or resized in the Preview, clips moved
//! or an edge trimmed on the timeline) the page sends what it would write on release as a
//! **draft**: the clips and titles it changes, each whole, as the edit would leave them, and the
//! track each clip would be on. The frame is planned from the snapshot's timeline with those in
//! place of its own ([`PreviewDraft::apply`]), so the GPU draws the drag frame as the cut will be
//! once the gesture is let go. Nothing is written: the project, the snapshot and the plan epoch are
//! untouched, and the gesture still writes one edit on release.
//!
//! A draft only replaces what the cut holds. It may move a clip in time and to another track of
//! its kind (and the tracks it lands on are put in order by start, as `move_clips` does), and change
//! anything else about a clip or a title, but never the footage a clip reads: the snapshot's assets
//! and proxies are the ones the plan is made with. A draft that names a clip or a title the cut does
//! not hold, or leaves the cut breaking an invariant it did not already break, is refused, and the
//! frame is the cut as it is.

use std::collections::HashSet;

use kerf_core::{Clip, TextOverlay, Timeline};
use serde::Deserialize;
use uuid::Uuid;

/// What a gesture would write, sent with each frame of the gesture (`get_preview_frame`'s `draft`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PreviewDraft {
    #[serde(default)]
    pub clips: Vec<DraftClip>,
    #[serde(default)]
    pub overlays: Vec<TextOverlay>,
}

/// A clip as the gesture would leave it, on the track it would be on.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftClip {
    pub track_id: Uuid,
    pub clip: Clip,
}

impl PreviewDraft {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && self.overlays.is_empty()
    }

    /// `timeline` with the draft's clips and titles in place of its own.
    pub fn apply(&self, timeline: &Timeline) -> Result<Timeline, String> {
        let mut out = timeline.clone();
        let mut named = HashSet::new();
        let mut lifted: Vec<(usize, Clip)> = Vec::new();
        for d in &self.clips {
            let id = d.clip.id;
            if !named.insert(id) {
                return Err(format!("the drag names clip {id} twice"));
            }
            let (from, ci) = out
                .locate(id)
                .ok_or_else(|| format!("the dragged clip {id} is not in the cut"))?;
            let to = out
                .tracks
                .iter()
                .position(|t| t.id == d.track_id)
                .ok_or_else(|| format!("the track {} the drag lands on is not in the cut", d.track_id))?;
            let held = &out.tracks[from].clips[ci];
            if held.asset_id != d.clip.asset_id {
                return Err("a drag does not change the footage a clip reads".into());
            }
            if out.tracks[to].kind != out.tracks[from].kind {
                return Err("a clip cannot be dragged to a track of another kind".into());
            }
            if to == from && held.timeline_start == d.clip.timeline_start {
                out.tracks[from].clips[ci] = d.clip.clone();
            } else {
                out.tracks[from].clips.remove(ci);
                lifted.push((to, d.clip.clone()));
            }
        }
        let landed: Vec<usize> = lifted.iter().map(|(to, _)| *to).collect();
        for (to, clip) in lifted {
            out.tracks[to].clips.push(clip);
        }
        for to in landed {
            out.tracks[to].sort_by_start();
        }
        let mut titles = HashSet::new();
        for o in &self.overlays {
            if !titles.insert(o.id) {
                return Err(format!("the drag names title {} twice", o.id));
            }
            let slot = out
                .overlays
                .iter_mut()
                .find(|held| held.id == o.id)
                .ok_or_else(|| format!("the dragged title {} is not in the cut", o.id))?;
            *slot = o.clone();
        }
        out.check_new(&timeline.known_violations()).map_err(|e| e.to_string())?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::{StreamKind, Track};

    fn cut() -> Timeline {
        let asset = Uuid::new_v4();
        let mut v1 = Track::new(StreamKind::Video, "V1");
        v1.clips = vec![Clip::new(asset, 0.0, 2.0, 0.0), Clip::new(asset, 2.0, 4.0, 2.0)];
        let mut v2 = Track::new(StreamKind::Video, "V2");
        v2.clips = vec![Clip::new(asset, 0.0, 1.0, 5.0)];
        let a1 = Track::new(StreamKind::Audio, "A1");
        Timeline {
            tracks: vec![v1, v2, a1],
            overlays: vec![TextOverlay::new("Hello", 0.0, 3.0)],
            ..Timeline::new()
        }
    }

    fn draft(track: &Track, clip: Clip) -> PreviewDraft {
        PreviewDraft {
            clips: vec![DraftClip {
                track_id: track.id,
                clip,
            }],
            overlays: Vec::new(),
        }
    }

    #[test]
    fn a_clip_changed_in_place_stays_where_it_is_and_nothing_else_moves() {
        let tl = cut();
        let mut c = tl.tracks[0].clips[1].clone();
        c.transform.scale = 0.5;
        c.color.brightness = 0.2;
        let out = draft(&tl.tracks[0], c.clone()).apply(&tl).expect("a draft");
        assert_eq!(out.tracks[0].clips[1].transform.scale, 0.5);
        assert_eq!(out.tracks[0].clips[1].color.brightness, 0.2);
        assert_eq!(out.tracks[0].clips[0].id, tl.tracks[0].clips[0].id);
        assert_eq!(
            tl.tracks[0].clips[1].transform.scale, 1.0,
            "the snapshot's own cut is untouched"
        );
    }

    #[test]
    fn a_clip_moved_lands_on_its_track_in_order_of_start_as_a_move_puts_it() {
        let tl = cut();
        let mut c = tl.tracks[0].clips[1].clone();
        c.timeline_start = 2.5;
        let out = draft(&tl.tracks[1], c.clone()).apply(&tl).expect("a draft");
        assert_eq!(out.tracks[0].clips.len(), 1);
        let starts: Vec<f64> = out.tracks[1].clips.iter().map(|c| c.timeline_start).collect();
        assert_eq!(starts, vec![2.5, 5.0]);
        assert_eq!(out.tracks[1].clips[0].id, c.id);

        let mut first = tl.tracks[0].clips[0].clone();
        first.timeline_start = 6.0;
        let out = draft(&tl.tracks[0], first.clone()).apply(&tl).expect("a draft");
        let ids: Vec<Uuid> = out.tracks[0].clips.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![tl.tracks[0].clips[1].id, first.id]);
    }

    #[test]
    fn a_title_is_replaced_whole() {
        let tl = cut();
        let mut o = tl.overlays[0].clone();
        o.pos_x = 0.2;
        o.size = 0.1;
        let out = PreviewDraft {
            clips: Vec::new(),
            overlays: vec![o],
        }
        .apply(&tl)
        .expect("a draft");
        assert_eq!((out.overlays[0].pos_x, out.overlays[0].size), (0.2, 0.1));
    }

    #[test]
    fn a_draft_that_is_not_of_this_cut_or_breaks_it_is_refused() {
        let tl = cut();
        let stranger = Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0);
        assert!(
            draft(&tl.tracks[0], stranger).apply(&tl).is_err(),
            "a clip the cut does not hold"
        );

        let held = tl.tracks[0].clips[0].clone();
        let elsewhere = Track::new(StreamKind::Video, "V9");
        assert!(
            draft(&elsewhere, held.clone()).apply(&tl).is_err(),
            "a track the cut does not hold"
        );
        assert!(
            draft(&tl.tracks[2], held.clone()).apply(&tl).is_err(),
            "a track of another kind"
        );

        let mut other_footage = held.clone();
        other_footage.asset_id = Uuid::new_v4();
        assert!(draft(&tl.tracks[0], other_footage).apply(&tl).is_err(), "other footage");

        let mut empty = held.clone();
        empty.source_out = empty.source_in;
        assert!(draft(&tl.tracks[0], empty).apply(&tl).is_err(), "a clip with nothing in it");

        let twice = PreviewDraft {
            clips: vec![
                DraftClip {
                    track_id: tl.tracks[0].id,
                    clip: held.clone(),
                },
                DraftClip {
                    track_id: tl.tracks[0].id,
                    clip: held,
                },
            ],
            overlays: Vec::new(),
        };
        assert!(twice.apply(&tl).is_err(), "a clip named twice");

        let lost = PreviewDraft {
            clips: Vec::new(),
            overlays: vec![TextOverlay::new("Lost", 0.0, 1.0)],
        };
        assert!(lost.apply(&tl).is_err(), "a title the cut does not hold");
    }

    #[test]
    fn a_draft_is_read_from_the_json_the_page_sends() {
        let tl = cut();
        let clip = serde_json::to_value(&tl.tracks[0].clips[0]).unwrap();
        let json = serde_json::json!({ "clips": [{ "track_id": tl.tracks[1].id, "clip": clip }] });
        let d: PreviewDraft = serde_json::from_value(json).expect("a draft");
        assert_eq!(d.clips.len(), 1);
        assert!(d.overlays.is_empty());
        let none: PreviewDraft = serde_json::from_value(serde_json::json!({})).expect("an empty draft");
        assert!(none.is_empty());
    }
}
