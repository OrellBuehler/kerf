//! Speed ramps on linked clips: a picture and its sound keep the same ramp on the timeline through
//! every edit that is carried to the group, and the sync lock and guard judge them by the footage they
//! play, not by an offset a ramp cannot have.

use super::*;

const ASSET: Uuid = Uuid::nil();

fn keys() -> Vec<SpeedKey> {
    vec![
        SpeedKey::new(0.5, 1.0),
        SpeedKey::new(1.5, 0.4),
        SpeedKey {
            time: 2.5,
            speed: 0.4,
            easing: RampEasing::Hold,
        },
        SpeedKey::new(3.5, 1.5),
    ]
}

fn footage(secs: f64) -> SourceLimits {
    HashMap::from([(ASSET, secs)])
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} is not {b}");
}

/// V1 holds the picture `(src 10..16 at 2.0)` and A1 its sound `(src 8..16 at 0.0)`: the sound
/// leads the picture by two seconds (an L-cut), both at 1x, linked.
fn leading_sound() -> (Timeline, Uuid, Uuid) {
    let picture = Clip::new(ASSET, 10.0, 16.0, 2.0);
    let sound = Clip::new(ASSET, 8.0, 16.0, 0.0);
    let (p, s) = (picture.id, sound.id);
    let mut t = Timeline {
        tracks: vec![
            Track {
                clips: vec![picture],
                ..Track::new(StreamKind::Video, "V1")
            },
            Track {
                clips: vec![sound],
                ..Track::new(StreamKind::Audio, "A1")
            },
        ],
        ..Timeline::new()
    };
    t.link_clips(&[p, s]).unwrap();
    (t, p, s)
}

/// V1 and A1 hold the same window at the same place, linked.
fn aligned_pair() -> (Timeline, Uuid, Uuid) {
    let (mut t, p, s) = leading_sound();
    {
        let c = clip_mut(&mut t, p);
        (c.source_in, c.source_out, c.timeline_start) = (10.0, 16.0, 0.0);
        let a = clip_mut(&mut t, s);
        (a.source_in, a.source_out, a.timeline_start) = (10.0, 16.0, 0.0);
    }
    (t, p, s)
}

fn clip(t: &Timeline, id: Uuid) -> &Clip {
    t.clip(id).unwrap()
}

fn clip_mut(t: &mut Timeline, id: Uuid) -> &mut Clip {
    let (ti, ci) = t.locate(id).unwrap();
    &mut t.tracks[ti].clips[ci]
}

/// The footage the lane's clip at `time` plays there, if one does.
fn playing(t: &Timeline, track: usize, time: f64) -> Option<f64> {
    t.tracks[track]
        .clips
        .iter()
        .find(|c| c.timeline_start <= time && time < c.timeline_end())
        .map(|c| c.timeline_to_source(time))
}

/// The picture's footage and the sound's agree at every sampled moment both play.
fn assert_in_step(t: &Timeline) {
    let end = t.duration();
    let mut both = 0;
    for i in 0..=200 {
        let at = end * i as f64 / 200.0 + 1e-7;
        if let (Some(v), Some(a)) = (playing(t, 0, at), playing(t, 1, at)) {
            near(v, a);
            both += 1;
        }
    }
    assert!(both > 20, "the pair shares almost no time ({both} samples)");
}

#[test]
fn a_ramp_lands_at_the_same_moment_of_the_timeline_in_a_partner_that_starts_elsewhere() {
    let (mut t, p, s) = leading_sound();
    let before = t.clone();
    let set = t.set_speed_ramp_linked(p, keys()).unwrap();
    assert_eq!(set.speed_ramp, keys());
    // The sound starts two seconds before the picture, so its keys are two seconds later.
    let times: Vec<f64> = clip(&t, s).speed_ramp.iter().map(|k| k.time).collect();
    assert_eq!(times, vec![2.5, 3.5, 4.5, 5.5]);
    assert_eq!(clip(&t, s).speed_ramp[2].easing, RampEasing::Hold);
    for i in 0..=60 {
        let at = 2.0 + 0.1 * i as f64;
        near(clip(&t, p).speed_at(at - 2.0), clip(&t, s).speed_at(at));
    }
    // The first key's speed is the clip's old one (1x), so the lead-in is where it was: still in step.
    let anchors = HashSet::from([p]);
    t.conform_links(&before, &anchors, &HashMap::new()).unwrap();
    assert!(t.first_sync_break(&before).is_none());
    near(clip(&t, s).timeline_start, 0.0);
    assert_in_step(&t);
}

#[test]
fn a_ramp_set_on_the_sound_reaches_the_picture_the_other_way_round() {
    let (mut t, p, s) = leading_sound();
    t.set_speed_ramp_linked(s, keys()).unwrap();
    // The picture starts two seconds after the sound: its keys are two seconds earlier (one is before 0).
    let times: Vec<f64> = clip(&t, p).speed_ramp.iter().map(|k| k.time).collect();
    assert_eq!(times, vec![-1.5, -0.5, 0.5, 1.5]);
    for i in 0..=60 {
        let at = 2.0 + 0.1 * i as f64;
        near(clip(&t, p).speed_at(at - 2.0), clip(&t, s).speed_at(at));
    }
}

#[test]
fn a_partner_at_another_pace_keeps_its_ratio_and_one_that_would_leave_the_range_refuses() {
    let (mut t, p, s) = aligned_pair();
    clip_mut(&mut t, s).speed = 2.0;
    t.set_speed_ramp_linked(p, vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(1.0, 0.5)])
        .unwrap();
    let doubled: Vec<(f64, f64)> = clip(&t, s).speed_ramp.iter().map(|k| (k.time, k.speed)).collect();
    assert_eq!(doubled, vec![(0.0, 2.0), (1.0, 1.0)]);

    let (mut t, p, s) = aligned_pair();
    clip_mut(&mut t, s).speed = 50.0;
    let before = t.clone();
    let err = t
        .set_speed_ramp_linked(p, vec![SpeedKey::new(0.0, 4.0)])
        .unwrap_err()
        .to_string();
    assert!(err.contains("200.000×"), "{err}");
    assert_eq!(
        serde_json::to_string(&t).unwrap(),
        serde_json::to_string(&before).unwrap(),
        "a refusal changes nothing"
    );
}

#[test]
fn a_clear_is_carried_and_a_constant_speed_replaces_the_ramp_on_every_clip() {
    let (mut t, p, s) = aligned_pair();
    t.set_speed_ramp_linked(p, keys()).unwrap();
    t.set_speed_ramp_linked(p, Vec::new()).unwrap();
    assert!(clip(&t, p).speed_ramp.is_empty() && clip(&t, s).speed_ramp.is_empty());
    assert!(t.set_speed_ramp_linked(p, Vec::new()).is_err());

    t.set_speed_ramp_linked(p, keys()).unwrap();
    t.set_speed_linked(s, 2.0).unwrap();
    assert!(clip(&t, p).speed_ramp.is_empty() && clip(&t, s).speed_ramp.is_empty());
    near(clip(&t, p).speed, 2.0);
    near(clip(&t, s).speed, 2.0);
}

#[test]
fn a_trim_is_carried_to_a_partner_that_shares_the_edge_with_the_same_ramp_rebase() {
    let (mut t, p, s) = aligned_pair();
    t.set_speed_ramp_linked(p, keys()).unwrap();
    let was = clip(&t, p).clone();
    // What `Project::trim` does to the picture: the in-point moves, the ramp follows, the start moves.
    {
        let c = clip_mut(&mut t, p);
        c.source_in = 11.1;
        c.follow_head(&was);
        c.timeline_start = was.timeline_start + was.local_at_along(1.1);
    }
    let carried = t.carry_extent_edit(p, &was, &footage(100.0)).unwrap();
    assert_eq!(carried.len(), 1);
    let (c, a) = (clip(&t, p), clip(&t, s));
    near(a.timeline_start, c.timeline_start);
    near(a.source_in, c.source_in);
    assert_eq!(a.speed_ramp.len(), c.speed_ramp.len());
    for (x, y) in a.speed_ramp.iter().zip(&c.speed_ramp) {
        near(x.time, y.time);
        near(x.speed, y.speed);
    }
    assert_in_step(&t);
    // And the tail the other way: both lengthen by the same timeline seconds.
    let was = clip(&t, p).clone();
    {
        let c = clip_mut(&mut t, p);
        c.source_out = 17.0;
    }
    t.carry_extent_edit(p, &was, &footage(100.0)).unwrap();
    near(clip(&t, s).source_out, 17.0);
    near(clip(&t, s).timeline_end(), clip(&t, p).timeline_end());
}

#[test]
fn a_head_trim_the_partner_does_not_share_moves_no_footage_and_so_displaces_nothing() {
    let (mut t, p, s) = leading_sound();
    t.set_speed_ramp_linked(p, keys()).unwrap();
    let before = t.clone();
    let was = clip(&t, p).clone();
    {
        let c = clip_mut(&mut t, p);
        c.source_in = 11.0;
        c.follow_head(&was);
        c.timeline_start = was.timeline_start + was.local_at_along(1.0);
    }
    // The picture's start is two seconds from the sound's: the trim is not carried to it...
    t.carry_extent_edit(p, &was, &footage(100.0)).unwrap();
    let sound = clip(&t, s);
    assert_eq!((sound.timeline_start, sound.source_in), (0.0, 8.0));
    assert_eq!(sound.speed_ramp, clip(&before, s).speed_ramp);
    // ...and the sync lock, judging footage and not an offset beyond the window, leaves it where it was.
    t.conform_links(&before, &HashSet::from([p]), &HashMap::new()).unwrap();
    near(clip(&t, s).timeline_start, 0.0);
    assert!(t.first_sync_break(&before).is_none());
    assert_in_step(&t);
}

#[test]
fn a_split_cuts_both_and_the_halves_stay_in_step() {
    let (mut t, p, s) = aligned_pair();
    t.set_speed_ramp_linked(p, keys()).unwrap();
    let (left, right) = t.split_clip_linked(p, 2.2).unwrap();
    assert_eq!(t.tracks[0].clips.len(), 2);
    assert_eq!(t.tracks[1].clips.len(), 2);
    assert_in_step(&t);
    near(left.timeline_end(), 2.2);
    near(right.timeline_start, 2.2);
    assert_eq!(t.link_partners(right.id).len(), 1);
    assert!(t.clip(s).is_some());
}

/// Three pairs side by side: the first and last on the ramp, the middle one plain. Returns the timeline
/// and each pair's `(picture, sound)`.
fn three_pairs() -> (Timeline, Vec<(Uuid, Uuid)>) {
    let mut t = Timeline::new();
    let mut ids = vec![(Uuid::nil(), Uuid::nil()); 3];
    for (track, lane) in [0, 1].into_iter().enumerate() {
        let mut at = 0.0;
        for (i, (si, so, ramp)) in [(10.0, 16.0, true), (50.0, 53.0, false), (40.0, 46.0, true)]
            .into_iter()
            .enumerate()
        {
            let mut c = Clip::new(ASSET, si, so, at);
            if ramp {
                c.speed_ramp = keys();
            }
            at = c.timeline_end();
            if lane == 0 {
                ids[i].0 = c.id;
            } else {
                ids[i].1 = c.id;
            }
            t.tracks[track].clips.push(c);
        }
    }
    for (p, s) in &ids {
        t.link_clips(&[*p, *s]).unwrap();
    }
    (t, ids)
}

#[test]
fn a_roll_is_the_same_edit_on_both_pairs_of_a_cut() {
    let (mut t, ids) = three_pairs();
    let before = t.clone();
    let out = t.roll_edit_linked(ids[0].0, ids[1].0, 0.6, &footage(100.0)).unwrap();
    assert_eq!(out.clips.len(), 4);
    assert_in_step(&t);
    near(
        clip(&t, ids[0].0).timeline_end(),
        clip(&before, ids[0].0).timeline_end() + 0.6,
    );
    near(clip(&t, ids[0].0).timeline_end(), clip(&t, ids[0].1).timeline_end());
    near(clip(&t, ids[1].0).timeline_start, clip(&t, ids[1].1).timeline_start);
}

#[test]
fn a_slide_moves_both_and_the_ramped_neighbours_trade_footage_with_it() {
    let (mut t, ids) = three_pairs();
    let before = t.clone();
    let out = t.slide_clip_linked(ids[1].0, 0.4, &footage(100.0)).unwrap();
    assert_eq!(out.clips.len(), 6);
    assert_in_step(&t);
    for (p, s) in &ids {
        near(clip(&t, *p).timeline_start, clip(&t, *s).timeline_start);
        near(clip(&t, *p).timeline_end(), clip(&t, *s).timeline_end());
    }
    near(
        clip(&t, ids[1].0).timeline_start,
        clip(&before, ids[1].0).timeline_start + 0.4,
    );
    near(clip(&t, ids[2].0).timeline_end(), clip(&before, ids[2].0).timeline_end());
}

#[test]
fn a_slip_moves_the_footage_of_both_by_the_same_source_seconds() {
    let (mut t, ids) = three_pairs();
    let before = t.clone();
    t.slip_clip_linked(ids[0].0, 1.5, &footage(100.0)).unwrap();
    assert_in_step(&t);
    near(clip(&t, ids[0].0).duration(), clip(&before, ids[0].0).duration());
    near(clip(&t, ids[0].0).source_in, 11.5);
    near(clip(&t, ids[0].1).source_in, 11.5);
}

#[test]
fn a_slip_against_a_partner_on_another_ramp_has_no_one_ratio_and_refuses() {
    let (mut t, p, s) = aligned_pair();
    clip_mut(&mut t, p).speed_ramp = keys();
    // The sound is plain 1x: the picture's slip in source seconds is not the sound's.
    let err = t.slip_clip_linked(p, 1.0, &footage(100.0)).unwrap_err().to_string();
    assert!(err.contains("another pace"), "{err}");
    assert!(t.clip(s).unwrap().speed_ramp.is_empty());
    // On the same ramp it is the same slip.
    clip_mut(&mut t, s).speed_ramp = keys();
    t.slip_clip_linked(p, 1.0, &footage(100.0)).unwrap();
    near(clip(&t, p).source_in, clip(&t, s).source_in);
}

#[test]
fn cutting_a_source_range_takes_the_same_stretch_from_both_and_keeps_them_in_step() {
    let (mut t, p, _) = aligned_pair();
    t.set_speed_ramp_linked(p, keys()).unwrap();
    let whole = clip(&t, p).duration();
    // Footage 11.2..12.4 plays 1.2 source seconds of the ramp, somewhere inside the first segments.
    let removed = clip(&t, p).source_span_to_timeline(11.2, 12.4);
    t.cut_clip_range_linked(p, 11.2, 12.4).unwrap();
    assert_eq!(t.tracks[0].clips.len(), 2);
    assert_eq!(t.tracks[1].clips.len(), 2);
    near(t.duration(), whole - (removed.end - removed.start));
    for track in &t.tracks {
        near(track.clips[1].timeline_start, track.clips[0].timeline_end());
    }
    assert_in_step(&t);
}

#[test]
fn the_guard_reads_a_pair_on_the_same_ramp_as_in_step_and_one_a_beat_apart_as_broken() {
    let (mut t, p, s) = aligned_pair();
    t.set_speed_ramp_linked(p, keys()).unwrap();
    let before = t.clone();
    assert!(t.first_sync_break(&before).is_none());
    // Move the sound alone: the pair was in step and now is not.
    clip_mut(&mut t, s).timeline_start += 0.5;
    assert!(t.first_sync_break(&before).is_some());
    // A pair on different ramps is never "in step" to begin with, so it is not the edit's doing.
    let mut apart = before;
    clip_mut(&mut apart, s).speed_ramp.truncate(2);
    let mut moved = apart.clone();
    clip_mut(&mut moved, s).timeline_start += 0.5;
    assert!(moved.first_sync_break(&apart).is_none());
}

#[test]
fn a_detached_sound_carries_the_ramp() {
    let asset = Uuid::new_v4();
    let mut picture = Clip::new(asset, 0.0, 6.0, 1.0);
    picture.speed_ramp = keys();
    let id = picture.id;
    let mut t = Timeline {
        tracks: vec![
            Track {
                clips: vec![picture],
                ..Track::new(StreamKind::Video, "V1")
            },
            Track::new(StreamKind::Audio, "A1"),
        ],
        ..Timeline::new()
    };
    let detached = t.detach_audio(id, true).unwrap();
    assert_eq!(detached.clip.speed_ramp, keys());
    near(detached.clip.duration(), clip(&t, id).duration());
}
