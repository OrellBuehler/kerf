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
//! A draft changes only what a drafted gesture writes, and each change goes through the checks of
//! the edit that writes it (kerf-core's own): a clip's transform (`set_transform`), colour
//! (`set_color`), mask (`set_mask`, normalized as it is), fades (`set_fade`), its edges (`trim`, alone:
//! held to its footage, never onto another clip) or where it is (`Timeline::move_clips`: a track of its
//! kind, none locked, never onto a clip that stays, never before 0); a title's position and size
//! (`update_overlay`) or keys (`set_overlay_keyframes`). Anything else about a clip or a title (its
//! footage above all: the snapshot's assets and proxies are the ones the plan is made with), a clip or
//! title the cut does not hold, or a cut left breaking an invariant it did not already break, is
//! refused, and the frame is the cut as it is. So the preview never shows a state the edit would not
//! write.

use std::collections::HashSet;

use kerf_core::{Asset, Clip, ClipMove, Color, Mask, TextOverlay, Timeline, Transform};
use serde::{Deserialize, Serialize};
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

/// `to` where it differs from `from`: what an edit is handed to change.
fn changed(from: f64, to: f64) -> Option<f64> {
    (from.to_bits() != to.to_bits()).then_some(to)
}

/// Two values that serialize alike (a NaN an older build left is `null` on both sides).
fn alike<T: Serialize>(a: &T, b: &T) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}

impl PreviewDraft {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && self.overlays.is_empty()
    }

    /// `timeline` with the draft's clips and titles in place of its own, as the edits that write them
    /// would leave it (see the module docs); `assets` hold the footage a trim is held to.
    pub fn apply(&self, timeline: &Timeline, assets: &[Asset]) -> Result<Timeline, String> {
        let mut out = timeline.clone();
        let mut named = HashSet::new();
        let mut moves = Vec::new();
        let mut trimmed = Vec::new();
        for d in &self.clips {
            let id = d.clip.id;
            if !named.insert(id) {
                return Err(format!("the drag names clip {id} twice"));
            }
            let (from, ci) = timeline
                .locate(id)
                .ok_or_else(|| format!("the dragged clip {id} is not in the cut"))?;
            let to = timeline
                .tracks
                .iter()
                .position(|t| t.id == d.track_id)
                .ok_or_else(|| format!("the track {} the drag lands on is not in the cut", d.track_id))?;
            let mut clip = edited(&timeline.tracks[from].clips[ci], &d.clip, assets)?;
            let held = &timeline.tracks[from].clips[ci];
            let trims = clip.source_in != held.source_in || clip.source_out != held.source_out;
            if trims {
                if to != from {
                    return Err("a trim does not take a clip to another track".into());
                }
                trimmed.push(id);
            } else {
                // Where it goes is the move's to say, all of the group at once.
                if to != from || changed(held.timeline_start, d.clip.timeline_start).is_some() {
                    moves.push(ClipMove {
                        clip_id: id,
                        timeline_start: d.clip.timeline_start,
                        track_id: Some(d.track_id),
                    });
                }
                clip.timeline_start = held.timeline_start;
            }
            let (ti, cj) = out.locate(id).expect("the clip was located in the cut it was copied from");
            let resort = out.tracks[ti].clips[cj].timeline_start != clip.timeline_start;
            out.tracks[ti].clips[cj] = clip;
            if resort {
                out.tracks[ti].sort_by_start();
            }
        }
        // What a trim leaves must not sit on another clip; a move is checked whole, as `move_clips` checks it.
        out.check_own_lanes(timeline, &trimmed).map_err(|e| e.to_string())?;
        if !moves.is_empty() {
            out.move_clips(&moves).map_err(|e| e.to_string())?;
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
            *slot = edited_title(slot, o)?;
        }
        out.check_new(&timeline.known_violations()).map_err(|e| e.to_string())?;
        Ok(out)
    }
}

/// `held` as the edits a drag writes would leave it with `drafted`'s values, each checked as its edit
/// checks it; the clip's start is the draft's (a move is checked whole by the caller).
fn edited(held: &Clip, drafted: &Clip, assets: &[Asset]) -> Result<Clip, String> {
    let mut only = held.clone();
    only.timeline_start = drafted.timeline_start;
    only.source_in = drafted.source_in;
    only.source_out = drafted.source_out;
    only.transform = drafted.transform;
    only.color = drafted.color;
    only.mask = drafted.mask;
    only.fade_in = drafted.fade_in;
    only.fade_out = drafted.fade_out;
    if !alike(&only, drafted) {
        return Err("a drag changes only a clip's place, edges, transform, colour, mask and fades (never its footage)".into());
    }
    let fail = |e: kerf_core::Error| e.to_string();
    let mut clip = held.clone();
    clip.timeline_start = drafted.timeline_start;
    let (a, b) = (&held.transform, &drafted.transform);
    if a != b {
        Transform::check_values(
            changed(a.scale, b.scale),
            changed(a.pos_x, b.pos_x),
            changed(a.pos_y, b.pos_y),
            changed(a.rotation, b.rotation),
            changed(a.opacity, b.opacity),
            changed(a.crop_left, b.crop_left),
            changed(a.crop_right, b.crop_right),
            changed(a.crop_top, b.crop_top),
            changed(a.crop_bottom, b.crop_bottom),
        )
        .map_err(fail)?;
        b.check_crop().map_err(fail)?;
        clip.transform = *b;
    }
    let (a, b) = (&held.color, &drafted.color);
    if a != b {
        Color::check_values(
            changed(a.brightness, b.brightness),
            changed(a.contrast, b.contrast),
            changed(a.saturation, b.saturation),
            changed(a.gamma, b.gamma),
            changed(a.temperature, b.temperature),
        )
        .map_err(fail)?;
        clip.color = *b;
    }
    if held.mask != drafted.mask {
        clip.mask = drafted.mask.map(Mask::normalized);
    }
    let (fade_in, fade_out) = (
        changed(held.fade_in, drafted.fade_in),
        changed(held.fade_out, drafted.fade_out),
    );
    if fade_in.is_some() || fade_out.is_some() {
        Clip::check_fades(fade_in, fade_out).map_err(fail)?;
        clip.fade_in = drafted.fade_in;
        clip.fade_out = drafted.fade_out;
    }
    let (source_in, source_out) = (
        changed(held.source_in, drafted.source_in),
        changed(held.source_out, drafted.source_out),
    );
    if source_in.is_some() || source_out.is_some() {
        let start = changed(held.timeline_start, drafted.timeline_start);
        Clip::check_trim(source_in, source_out, start).map_err(fail)?;
        if let (Some(out), Some(asset)) = (source_out, assets.iter().find(|a| a.id == held.asset_id)) {
            asset.check_source_out(out).map_err(fail)?;
        }
        clip.source_in = drafted.source_in;
        clip.source_out = drafted.source_out;
        if clip.source_out <= clip.source_in {
            return Err("source_out must be greater than source_in".into());
        }
        clip.timeline_start = start.map_or(held.timeline_start, |s| s.max(0.0));
    }
    Ok(clip)
}

/// `held` as `update_overlay { pos_x, pos_y, size }` or `set_overlay_keyframes` would leave it with
/// `drafted`'s values, each checked as that edit checks it.
fn edited_title(held: &TextOverlay, drafted: &TextOverlay) -> Result<TextOverlay, String> {
    let mut only = held.clone();
    only.pos_x = drafted.pos_x;
    only.pos_y = drafted.pos_y;
    only.size = drafted.size;
    only.keyframes = drafted.keyframes.clone();
    if !alike(&only, drafted) {
        return Err("a drag changes only a title's position, size and keys".into());
    }
    let fail = |e: kerf_core::Error| e.to_string();
    let mut title = held.clone();
    let (pos_x, pos_y, size) = (
        changed(held.pos_x, drafted.pos_x),
        changed(held.pos_y, drafted.pos_y),
        changed(held.size, drafted.size),
    );
    if pos_x.is_some() || pos_y.is_some() || size.is_some() {
        TextOverlay::check_values(None, None, pos_x, pos_y, size).map_err(fail)?;
        title.pos_x = drafted.pos_x;
        title.pos_y = drafted.pos_y;
        title.size = drafted.size;
        title.check_span().map_err(fail)?;
    }
    if !alike(&held.keyframes, &drafted.keyframes) {
        for k in &drafted.keyframes {
            k.check().map_err(fail)?;
        }
        let mut keys = drafted.keyframes.clone();
        keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        title.keyframes = keys;
    }
    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerf_core::{MaskShape, StreamKind, TextKeyframe, Track};

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

    fn title(o: TextOverlay) -> PreviewDraft {
        PreviewDraft {
            clips: Vec::new(),
            overlays: vec![o],
        }
    }

    #[test]
    fn a_clip_changed_in_place_stays_where_it_is_and_nothing_else_moves() {
        let tl = cut();
        let mut c = tl.tracks[0].clips[1].clone();
        c.transform.scale = 0.5;
        c.color.brightness = 0.2;
        let out = draft(&tl.tracks[0], c).apply(&tl, &[]).expect("a draft");
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
        let out = draft(&tl.tracks[1], c.clone()).apply(&tl, &[]).expect("a draft");
        assert_eq!(out.tracks[0].clips.len(), 1);
        let starts: Vec<f64> = out.tracks[1].clips.iter().map(|c| c.timeline_start).collect();
        assert_eq!(starts, vec![2.5, 5.0]);
        assert_eq!(out.tracks[1].clips[0].id, c.id);

        let mut first = tl.tracks[0].clips[0].clone();
        first.timeline_start = 6.0;
        let out = draft(&tl.tracks[0], first.clone()).apply(&tl, &[]).expect("a draft");
        let ids: Vec<Uuid> = out.tracks[0].clips.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![tl.tracks[0].clips[1].id, first.id]);
    }

    #[test]
    fn a_move_is_refused_where_move_clips_refuses_it() {
        let tl = cut();
        let mut onto = tl.tracks[0].clips[1].clone();
        onto.timeline_start = 1.0;
        let err = draft(&tl.tracks[0], onto).apply(&tl, &[]).unwrap_err();
        assert!(err.contains("overlap"), "onto a clip that stays: {err}");

        let mut early = tl.tracks[1].clips[0].clone();
        early.timeline_start = -1.0;
        let err = draft(&tl.tracks[1], early).apply(&tl, &[]).unwrap_err();
        assert!(err.contains("before the beginning"), "{err}");

        let mut locked = tl.clone();
        locked.tracks[1].locked = true;
        let mut c = tl.tracks[0].clips[1].clone();
        c.timeline_start = 2.5;
        let err = draft(&locked.tracks[1], c.clone()).apply(&locked, &[]).unwrap_err();
        assert!(err.contains("V2 is locked"), "onto a locked track: {err}");
        let mut source_locked = tl.clone();
        source_locked.tracks[0].locked = true;
        let err = draft(&source_locked.tracks[1], c).apply(&source_locked, &[]).unwrap_err();
        assert!(err.contains("V1 is locked"), "off a locked track: {err}");

        // The group moves together: abutting clips nudged by a second do not trip over each other.
        let mut a = tl.tracks[0].clips[0].clone();
        let mut b = tl.tracks[0].clips[1].clone();
        a.timeline_start += 1.0;
        b.timeline_start += 1.0;
        let group = PreviewDraft {
            clips: vec![
                DraftClip {
                    track_id: tl.tracks[0].id,
                    clip: a,
                },
                DraftClip {
                    track_id: tl.tracks[0].id,
                    clip: b,
                },
            ],
            overlays: Vec::new(),
        };
        let out = group.apply(&tl, &[]).expect("a group move");
        let starts: Vec<f64> = out.tracks[0].clips.iter().map(|c| c.timeline_start).collect();
        assert_eq!(starts, vec![1.0, 3.0]);
    }

    #[test]
    fn transform_colour_and_fades_are_held_to_the_ranges_their_edits_hold_them_to() {
        let tl = cut();
        let held = tl.tracks[0].clips[0].clone();
        let refused = |change: fn(&mut Clip)| {
            let mut c = held.clone();
            change(&mut c);
            draft(&tl.tracks[0], c).apply(&tl, &[]).is_err()
        };
        assert!(refused(|c| c.transform.scale = 0.0), "scale > 0");
        assert!(refused(|c| c.transform.scale = f64::NAN), "a finite scale");
        assert!(refused(|c| c.transform.pos_x = f64::INFINITY), "a finite position");
        assert!(refused(|c| c.transform.opacity = 1.5), "opacity within 0..=1");
        assert!(refused(|c| c.transform.crop_left = -0.1), "a crop within 0..1");
        assert!(refused(|c| c.transform.crop_top = 1.0), "a crop within 0..1");
        assert!(
            refused(|c| {
                c.transform.crop_left = 0.6;
                c.transform.crop_right = 0.4;
            }),
            "a crop that leaves nothing"
        );
        assert!(refused(|c| c.color.brightness = 1.5), "brightness within -1..=1");
        assert!(refused(|c| c.color.contrast = 5.0), "contrast within 0..=4");
        assert!(refused(|c| c.color.saturation = -0.1), "saturation within 0..=3");
        assert!(refused(|c| c.color.gamma = 0.05), "gamma within 0.1..=10");
        assert!(refused(|c| c.color.temperature = -2.0), "temperature within -1..=1");
        assert!(refused(|c| c.fade_in = -1.0), "a fade >= 0");
        assert!(refused(|c| c.speed = 2.0), "a drag does not change the speed");
        assert!(refused(|c| c.volume = 0.5), "nor the volume");
        assert!(!refused(|c| {
            c.transform.crop_left = 0.3;
            c.transform.opacity = 0.5;
            c.color.gamma = 2.0;
            c.fade_out = 1.0;
        }));
    }

    #[test]
    fn a_mask_is_drawn_normalized_as_set_mask_writes_it() {
        let tl = cut();
        let mut c = tl.tracks[0].clips[0].clone();
        let mask = Mask {
            shape: MaskShape::Ellipse,
            x: 0.5,
            y: 0.5,
            width: 0.0,
            height: 3.0,
            feather: -1.0,
            inverted: false,
        };
        c.mask = Some(mask);
        let out = draft(&tl.tracks[0], c).apply(&tl, &[]).expect("a draft");
        assert_eq!(out.tracks[0].clips[0].mask, Some(mask.normalized()));
        assert_ne!(Some(mask), Some(mask.normalized()), "the mask was one to clamp");
    }

    #[test]
    fn a_trim_is_held_to_the_footage_and_kept_off_other_clips() {
        let tl = cut();
        let asset = Asset {
            id: tl.tracks[0].clips[0].asset_id,
            duration: 4.5,
            ..serde_json::from_value(serde_json::json!({
                "id": Uuid::nil(), "path": "/x.mp4", "name": "x.mp4", "duration": 0.0,
                "streams": [], "imported_at": "2026-01-01T00:00:00Z",
                "source_paths": [], "voiceover": null, "music_score": null
            }))
            .expect("an asset")
        };
        let assets = [asset];
        let mut longer = tl.tracks[0].clips[1].clone();
        longer.source_out = 4.4;
        let out = draft(&tl.tracks[0], longer.clone()).apply(&tl, &assets).expect("a trim");
        assert_eq!(out.tracks[0].clips[1].source_out, 4.4);
        longer.source_out = 5.0;
        let err = draft(&tl.tracks[0], longer).apply(&tl, &assets).unwrap_err();
        assert!(err.contains("past the end"), "{err}");

        let mut onto = tl.tracks[0].clips[0].clone();
        onto.source_out = 3.0;
        let err = draft(&tl.tracks[0], onto).apply(&tl, &assets).unwrap_err();
        assert!(err.contains("overlap"), "onto the next clip: {err}");

        let mut left = tl.tracks[0].clips[1].clone();
        left.source_in = -1.0;
        left.timeline_start = -1.0;
        assert!(draft(&tl.tracks[0], left).apply(&tl, &assets).is_err(), "source_in >= 0");

        let mut elsewhere = tl.tracks[0].clips[1].clone();
        elsewhere.source_out = 3.5;
        assert!(
            draft(&tl.tracks[1], elsewhere).apply(&tl, &assets).is_err(),
            "a trim stays on its track"
        );
    }

    #[test]
    fn a_title_takes_a_position_a_size_or_keys_as_its_edits_check_them() {
        let tl = cut();
        let mut o = tl.overlays[0].clone();
        o.pos_x = 0.2;
        o.size = 0.1;
        let out = title(o).apply(&tl, &[]).expect("a draft");
        assert_eq!((out.overlays[0].pos_x, out.overlays[0].size), (0.2, 0.1));

        let mut o = tl.overlays[0].clone();
        o.size = 0.0;
        assert!(title(o).apply(&tl, &[]).is_err(), "size > 0");
        let mut o = tl.overlays[0].clone();
        o.pos_y = f64::NAN;
        assert!(title(o).apply(&tl, &[]).is_err(), "a finite position");
        let mut o = tl.overlays[0].clone();
        o.text = "Changed".into();
        assert!(title(o).apply(&tl, &[]).is_err(), "a drag does not change the text");
        let mut o = tl.overlays[0].clone();
        o.color = "white,format=gray".into();
        assert!(title(o).apply(&tl, &[]).is_err(), "nor the colour");

        let key = |time: f64, opacity: f64| TextKeyframe {
            time,
            pos_x: 0.5,
            pos_y: 0.5,
            opacity,
        };
        let mut o = tl.overlays[0].clone();
        o.keyframes = vec![key(1.0, 1.0), key(0.0, 0.5)];
        let out = title(o).apply(&tl, &[]).expect("keys");
        let times: Vec<f64> = out.overlays[0].keyframes.iter().map(|k| k.time).collect();
        assert_eq!(times, vec![0.0, 1.0], "sorted, as set_overlay_keyframes stores them");
        let mut o = tl.overlays[0].clone();
        o.keyframes = vec![key(0.0, 1.5)];
        assert!(title(o).apply(&tl, &[]).is_err(), "a key's opacity within 0..=1");
        let mut o = tl.overlays[0].clone();
        o.keyframes = vec![key(-1.0, 1.0)];
        assert!(title(o).apply(&tl, &[]).is_err(), "a key at a time >= 0");
    }

    #[test]
    fn a_draft_that_is_not_of_this_cut_or_breaks_it_is_refused() {
        let tl = cut();
        let stranger = Clip::new(Uuid::new_v4(), 0.0, 1.0, 0.0);
        assert!(
            draft(&tl.tracks[0], stranger).apply(&tl, &[]).is_err(),
            "a clip the cut does not hold"
        );

        let held = tl.tracks[0].clips[0].clone();
        let elsewhere = Track::new(StreamKind::Video, "V9");
        assert!(
            draft(&elsewhere, held.clone()).apply(&tl, &[]).is_err(),
            "a track the cut does not hold"
        );
        assert!(
            draft(&tl.tracks[2], held.clone()).apply(&tl, &[]).is_err(),
            "a track of another kind"
        );

        let mut other_footage = held.clone();
        other_footage.asset_id = Uuid::new_v4();
        assert!(draft(&tl.tracks[0], other_footage).apply(&tl, &[]).is_err(), "other footage");

        let mut empty = held.clone();
        empty.source_out = empty.source_in;
        assert!(
            draft(&tl.tracks[0], empty).apply(&tl, &[]).is_err(),
            "a clip with nothing in it"
        );

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
        assert!(twice.apply(&tl, &[]).is_err(), "a clip named twice");

        let lost = title(TextOverlay::new("Lost", 0.0, 1.0));
        assert!(lost.apply(&tl, &[]).is_err(), "a title the cut does not hold");
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
