//! Speed ramps through the edit modes: every op that moves an edge, splits, slips or slides a clip with
//! a ramp keeps each frame on the footage it played before (`assert_same_footage`), which is the whole
//! contract — a ramp is a map, and an edit that changes the clip's extent must not change the map where
//! the clip still exists.

use super::ramp_edit::ramp_changes;
use super::*;

const ASSET: Uuid = Uuid::nil();

/// Fast, then a hold at a crawl, then a push up to double and back: every kind of segment.
fn keys() -> Vec<SpeedKey> {
    vec![
        SpeedKey::new(0.5, 1.0),
        SpeedKey {
            time: 1.5,
            speed: 0.25,
            easing: RampEasing::Hold,
        },
        SpeedKey::new(2.5, 2.0),
        SpeedKey::new(3.5, 1.0),
    ]
}

fn ramped(source_in: f64, source_out: f64, start: f64) -> Clip {
    let mut c = Clip::new(ASSET, source_in, source_out, start);
    c.speed_ramp = keys();
    c
}

fn lane(clips: Vec<Clip>) -> Timeline {
    Timeline {
        tracks: vec![Track {
            clips,
            ..Track::new(StreamKind::Video, "V1")
        }],
        ..Timeline::new()
    }
}

fn footage(secs: f64) -> SourceLimits {
    HashMap::from([(ASSET, secs)])
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} is not {b}");
}

/// Every frame `new` shows (sampled across its length) plays the footage it played in `old`: the
/// timeline-to-source map agrees at the same timeline times, wherever the edit moved the edges.
fn assert_same_footage(old: &Clip, new: &Clip) {
    for i in 0..=48 {
        let t = new.timeline_start + new.duration() * i as f64 / 48.0;
        near(new.timeline_to_source(t), old.timeline_to_source(t));
    }
}

fn times(c: &Clip) -> Vec<(f64, f64)> {
    c.speed_ramp.iter().map(|k| (k.time, k.speed)).collect()
}

#[test]
fn keys_are_checked_and_put_in_order() {
    assert!(check_speed_ramp(Vec::new()).unwrap().is_empty());
    let sorted = check_speed_ramp(vec![SpeedKey::new(2.0, 1.0), SpeedKey::new(0.0, 3.0)]).unwrap();
    assert_eq!(sorted.iter().map(|k| k.time).collect::<Vec<_>>(), vec![0.0, 2.0]);
    let refused = |keys: Vec<SpeedKey>| check_speed_ramp(keys).unwrap_err().to_string();
    assert!(refused(vec![SpeedKey::new(f64::NAN, 1.0)]).contains("key 1"));
    assert!(refused(vec![SpeedKey::new(0.0, 1.0), SpeedKey::new(-1.0, 1.0)]).contains("key 2"));
    assert!(refused(vec![SpeedKey::new(MAX_RAMP_TIME * 2.0, 1.0)]).contains("time"));
    assert!(refused(vec![SpeedKey::new(0.0, 0.0)]).contains("speed"));
    assert!(refused(vec![SpeedKey::new(0.0, -1.0)]).contains("magnitude"));
    assert!(refused(vec![SpeedKey::new(0.0, MAX_SPEED * 2.0)]).contains("speed"));
    assert!(refused(vec![SpeedKey::new(1.0, 1.0), SpeedKey::new(1.0, 2.0)]).contains("two speed ramp keys"));
    assert!(refused((0..=MAX_SPEED_KEYS).map(|i| SpeedKey::new(i as f64, 1.0)).collect()).contains("at most"));
    // A negative zero is a zero: two keys at "0" are a duplicate whichever sign they wear.
    assert!(refused(vec![SpeedKey::new(-0.0, 1.0), SpeedKey::new(0.0, 2.0)]).contains("two speed ramp keys"));
}

#[test]
fn a_head_trim_pins_the_speed_the_clip_opens_at_and_shifts_the_rest_back() {
    let mut c = ramped(10.0, 22.0, 5.0);
    let old = c.clone();
    c.move_head(1.0, false);
    // 1.0 s is halfway down the first run, 1x to 0.25x: 0.625x. The hold's key keeps its easing.
    assert_eq!(times(&c), vec![(0.0, 0.625), (0.5, 0.25), (1.5, 2.0), (2.5, 1.0)]);
    assert_eq!(c.speed_ramp[1].easing, RampEasing::Hold);
    assert_eq!(c.timeline_start, 6.0);
    near(c.timeline_end(), old.timeline_end());
    assert_same_footage(&old, &c);
}

#[test]
fn a_cut_inside_a_hold_keeps_holding_and_one_on_a_key_keeps_that_keys_segment() {
    let old = ramped(10.0, 22.0, 5.0);
    let mut inside = old.clone();
    inside.move_head(2.0, false);
    assert_eq!(times(&inside), vec![(0.0, 0.25), (0.5, 2.0), (1.5, 1.0)]);
    assert_eq!(
        inside.speed_ramp[0].easing,
        RampEasing::Hold,
        "the rest of the hold is a hold"
    );
    assert_same_footage(&old, &inside);

    let mut on_key = old.clone();
    on_key.move_head(1.5, false);
    assert_eq!(times(&on_key), vec![(0.0, 0.25), (1.0, 2.0), (2.0, 1.0)]);
    assert_eq!(on_key.speed_ramp[0].easing, RampEasing::Hold);
    assert_same_footage(&old, &on_key);
}

#[test]
fn a_cut_before_the_first_key_pins_nothing_and_one_past_the_last_leaves_its_speed() {
    let old = ramped(10.0, 22.0, 5.0);
    let mut early = old.clone();
    early.move_head(0.25, false);
    assert_eq!(times(&early), vec![(0.25, 1.0), (1.25, 0.25), (2.25, 2.0), (3.25, 1.0)]);
    assert_same_footage(&old, &early);

    let mut late = old.clone();
    late.move_head(6.0, false);
    assert_eq!(times(&late), vec![(0.0, 1.0)]);
    assert_eq!(late.speed_ramp[0].easing, RampEasing::Linear);
    assert_same_footage(&old, &late);
}

#[test]
fn pulling_the_head_out_shifts_every_key_and_the_new_head_runs_at_the_first_speed() {
    let old = ramped(10.0, 22.0, 5.0);
    let mut c = old.clone();
    c.move_head(-1.0, false);
    assert_eq!(times(&c), vec![(1.5, 1.0), (2.5, 0.25), (3.5, 2.0), (4.5, 1.0)]);
    assert_eq!(c.timeline_start, 4.0);
    near(c.source_in, 9.0);
    near(c.timeline_end(), old.timeline_end());
    assert_same_footage(&old, &c);
}

#[test]
fn the_key_cap_holds_through_any_head_trim_and_colliding_keys_are_dropped_not_the_ramp() {
    let mut full = ramped(0.0, 100.0, 0.0);
    full.speed_ramp = (0..MAX_SPEED_KEYS)
        .map(|i| SpeedKey::new(i as f64 * 0.5, 1.0 + i as f64 * 0.1))
        .collect();
    for by in [0.0, 0.1, 0.5, 0.75, 3.0, 7.5, 9.0, -2.0] {
        let mut c = full.clone();
        c.rebase_ramp(by);
        assert!(c.is_ramped() && c.speed_ramp.len() <= MAX_SPEED_KEYS, "by {by}");
    }
    let mut close = ramped(0.0, 100.0, 0.0);
    close.speed_ramp = vec![SpeedKey::new(1.0, 1.0), SpeedKey::new(1.0 + f64::EPSILON, 2.0)];
    assert!(close.is_ramped());
    close.rebase_ramp(-1000.0);
    assert!(
        close.is_ramped(),
        "two keys that round to one leave one, not an unreadable ramp"
    );
    assert_eq!(close.speed_ramp.len(), 1);
}

#[test]
fn a_tail_move_leaves_the_keys_and_runs_on_at_the_last_speed() {
    let old = ramped(10.0, 20.0, 5.0);
    let mut shorter = old.clone();
    shorter.move_tail(-1.0, false);
    assert_eq!(shorter.speed_ramp, old.speed_ramp);
    near(shorter.duration(), old.duration() - 1.0);
    assert_same_footage(&old, &shorter);

    let mut longer = old.clone();
    longer.move_tail(2.0, false);
    near(longer.duration(), old.duration() + 2.0);
    // Past the last key (3.5 s in) the ramp holds 1x, and the extra seconds are source seconds.
    assert!(old.duration() > 3.5);
    near(longer.source_out - old.source_out, 2.0);
    assert_same_footage(&longer, &old);
}

#[test]
fn a_tail_move_inside_the_ramp_takes_the_footage_the_ramp_plays_there() {
    let mut c = ramped(10.0, 11.0, 0.0);
    let before = c.duration();
    c.move_tail(1.0, false);
    near(c.duration(), before + 1.0);
    near(c.along(c.duration()), c.source_duration());
    let mut back = c.clone();
    back.move_tail(-1.0, false);
    near(back.source_out, 11.0);
}

#[test]
fn the_handles_are_the_timeline_the_footage_either_side_plays_for() {
    for reversed in [false, true] {
        let mut old = ramped(4.0, 9.0, 3.0);
        if reversed {
            old.speed = -1.0;
        }
        let (head, tail) = old.handles(30.0);
        let mut pulled = old.clone();
        pulled.move_head(-head, false);
        let mut extended = old.clone();
        extended.move_tail(tail, false);
        if reversed {
            near(pulled.source_out, 30.0);
            near(extended.source_in, 0.0);
        } else {
            near(pulled.source_in, 0.0);
            near(extended.source_out, 30.0);
        }
        assert_same_footage(&old, &pulled);
        assert_same_footage(&extended, &old);
    }
    assert_eq!(ramped(4.0, 9.0, 3.0).handles(f64::INFINITY), (f64::INFINITY, f64::INFINITY));
}

#[test]
fn a_split_keeps_every_frames_source_time_on_both_halves() {
    for reversed in [false, true] {
        for at in [5.2, 6.0, 6.7, 7.0, 7.9, 9.5, 12.0] {
            let mut c = ramped(10.0, 22.0, 5.0);
            if reversed {
                c.speed = -2.0;
            }
            let end = c.timeline_end();
            let mut t = lane(vec![c.clone()]);
            let (left, right) = t.split_clip(c.id, at).unwrap();
            near(left.timeline_end(), at);
            near(right.timeline_end(), end);
            near(right.timeline_start, left.timeline_end());
            assert_same_footage(&c, &left);
            assert_same_footage(&c, &right);
            near(right.speed_at(0.0), c.speed_at(at - c.timeline_start));
            // The halves tile the window: nothing played twice, nothing dropped.
            let (lo, hi) = (left.source_in.min(right.source_in), left.source_out.max(right.source_out));
            near(lo, c.source_in);
            near(hi, c.source_out);
            near(left.source_duration() + right.source_duration(), c.source_duration());
        }
    }
}

#[test]
fn split_and_remove_keeps_the_half_that_stays_exactly() {
    let old = ramped(10.0, 22.0, 5.0);
    for at in [5.6, 6.5, 7.2, 9.0] {
        for side in [SplitSide::Left, SplitSide::Right] {
            let mut t = lane(vec![old.clone()]);
            let kept = t.split_remove(old.id, at, side).unwrap();
            assert_same_footage(&old, &kept);
            match side {
                SplitSide::Left => {
                    near(kept.timeline_start, at);
                    near(kept.timeline_end(), old.timeline_end());
                }
                SplitSide::Right => {
                    near(kept.timeline_start, old.timeline_start);
                    near(kept.timeline_end(), at);
                }
            }
        }
    }
}

#[test]
fn a_range_export_cuts_a_ramped_clip_to_the_footage_the_range_shows() {
    let old = ramped(10.0, 22.0, 5.0);
    let t = lane(vec![old.clone()]);
    for (start, end) in [(6.0, 9.0), (5.0, 7.5), (8.0, 100.0), (6.25, 6.75)] {
        let cut = t.slice(start, end);
        let c = &cut.tracks[0].clips[0];
        // The slice is shifted to open at 0: its time `x` is the original's `x + start`.
        for i in 0..=24 {
            let x = c.timeline_start + c.duration() * i as f64 / 24.0;
            near(c.timeline_to_source(x), old.timeline_to_source(x + start));
        }
        near(
            c.duration(),
            (end.min(old.timeline_end()) - start.max(old.timeline_start)).max(0.0),
        );
    }
}

/// `a` (src 10..16) and `b` (src 40..46), both on the ramp, abutting at `a`'s end.
fn abutting() -> (Clip, Clip) {
    let a = ramped(10.0, 16.0, 0.0);
    let mut b = ramped(40.0, 46.0, 0.0);
    b.timeline_start = a.timeline_end();
    (a, b)
}

#[test]
fn a_roll_trades_footage_at_the_cut_and_both_clips_keep_what_they_show() {
    let (a, b) = abutting();
    let (ia, ib) = (a.id, b.id);
    let span = (a.timeline_start, b.timeline_end());
    for delta in [0.3, -0.4, 1.2, -1.0] {
        let mut t = lane(vec![a.clone(), b.clone()]);
        let out = t.roll_edit(ia, ib, delta, &footage(100.0)).unwrap();
        assert!(!out.clamped);
        let (na, nb) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1]);
        near(na.timeline_end(), a.timeline_end() + delta);
        near(nb.timeline_start, na.timeline_end());
        near(nb.timeline_end(), span.1);
        // Each shows what it did before (and, past an edge that moved out, the footage that follows).
        assert_same_footage(&a, na);
        assert_same_footage(&b, nb);
    }
}

#[test]
fn a_roll_stops_where_a_ramped_clip_runs_out_of_footage() {
    // `a` has 1 source second after its out-point, played at the last key's 1x: 1 timeline second.
    let (a, b) = abutting();
    let (ia, ib) = (a.id, b.id);
    let t = lane(vec![a.clone(), b]);
    let range = t.roll_range(ia, ib, &footage(17.0)).unwrap();
    near(range.max, 1.0);
    let mut rolled = t.clone();
    let out = rolled.roll_edit(ia, ib, 5.0, &footage(17.0)).unwrap();
    assert!(out.clamped);
    near(out.applied, 1.0);
    near(rolled.tracks[0].clips[0].source_out, 17.0);
    // Earlier, `a` may shrink to the floor, and the footage before `b` is 40 s at the first key's 1x.
    near(range.min, -(a.duration() - MIN_EDIT_CLIP));
}

#[test]
fn a_slip_keeps_the_length_and_moves_every_frame_by_the_same_footage() {
    for reversed in [false, true] {
        let mut c = ramped(10.0, 22.0, 5.0);
        if reversed {
            c.speed = -1.0;
        }
        let (start, len) = (c.timeline_start, c.duration());
        let mut t = lane(vec![c.clone()]);
        let out = t.slip_clip(c.id, 3.0, &footage(100.0)).unwrap();
        assert!(!out.clamped);
        let slipped = &t.tracks[0].clips[0];
        near(slipped.duration(), len);
        assert_eq!(slipped.timeline_start, start);
        assert_eq!(slipped.speed_ramp, c.speed_ramp);
        let shift = if reversed { -3.0 } else { 3.0 };
        for i in 0..=24 {
            let x = start + len * i as f64 / 24.0;
            near(slipped.timeline_to_source(x), c.timeline_to_source(x) + shift);
        }
    }
}

#[test]
fn a_slide_trades_footage_with_ramped_neighbours_and_keeps_the_middle_clip() {
    let prev = ramped(10.0, 16.0, 0.0);
    let mid = Clip::new(ASSET, 50.0, 53.0, prev.timeline_end());
    let mut next = ramped(40.0, 46.0, 0.0);
    next.timeline_start = mid.timeline_end();
    let im = mid.id;
    for delta in [0.5, -0.5, 1.5] {
        let mut t = lane(vec![prev.clone(), mid.clone(), next.clone()]);
        let out = t.slide_clip(im, delta, &footage(100.0)).unwrap();
        assert!(!out.clamped, "{delta}");
        let (p, m, n) = (&t.tracks[0].clips[0], &t.tracks[0].clips[1], &t.tracks[0].clips[2]);
        near(m.timeline_start, mid.timeline_start + delta);
        near(p.timeline_end(), m.timeline_start);
        near(n.timeline_start, m.timeline_end());
        near(n.timeline_end(), next.timeline_end());
        assert_same_footage(&prev, p);
        assert_same_footage(&next, n);
    }
}

#[test]
fn a_beat_snap_ends_a_ramped_clip_on_the_beat_with_the_footage_the_ramp_plays_there() {
    let old = ramped(10.0, 16.0, 0.0);
    let mut track = Track {
        clips: vec![old.clone()],
        ..Track::new(StreamKind::Video, "V1")
    };
    let target = old.timeline_end() + 0.7;
    let moved = track.align_cuts_to_beats(&[0.0, target], 1.0, &footage(100.0));
    assert_eq!(moved, 1);
    let c = &track.clips[0];
    near(c.timeline_end(), target);
    assert_same_footage(c, &old);
    assert_eq!(c.speed_ramp, old.speed_ramp);
}

#[test]
fn a_trim_that_wrote_the_window_directly_takes_the_ramp_along() {
    let old = ramped(10.0, 22.0, 5.0);
    let mut c = old.clone();
    // What `Project::trim` does: the in-point moves 1.2 source seconds (to 11.2).
    c.source_in = 11.2;
    c.follow_head(&old);
    let head = old.local_at_along(1.2);
    near(c.speed_at(0.0), old.speed_at(head));
    near(c.duration() + head, old.duration());

    let mut reversed = old.clone();
    reversed.speed = -1.0;
    let mut r = reversed.clone();
    r.source_out = 20.8;
    r.follow_head(&reversed);
    near(r.speed_at(0.0), reversed.speed_at(reversed.local_at_along(1.2)));

    // A tail trim, or a clip without a ramp, is none of its business.
    let mut tail = old.clone();
    tail.source_out = 18.0;
    tail.follow_head(&old);
    assert_eq!(tail.speed_ramp, old.speed_ramp);
    let mut plain = Clip::new(ASSET, 0.0, 4.0, 0.0);
    plain.speed = 2.0;
    let was = plain.clone();
    plain.source_in = 1.0;
    plain.follow_head(&was);
    assert!(plain.speed_ramp.is_empty());
}

#[test]
fn setting_a_ramp_keeps_the_window_changes_the_length_and_a_clear_needs_a_ramp() {
    let c = Clip::new(ASSET, 10.0, 18.0, 2.0);
    let mut t = lane(vec![c.clone()]);
    let set = t
        .set_speed_ramp(c.id, vec![SpeedKey::new(2.0, 1.0), SpeedKey::new(0.0, 4.0)])
        .unwrap();
    assert_eq!((set.source_in, set.source_out, set.speed), (10.0, 18.0, 1.0));
    assert_eq!(times(&set), vec![(0.0, 4.0), (2.0, 1.0)], "sorted");
    // 4x down to 1x over 2 s covers 5 source seconds, the last 3 at 1x: 2 + 3 s.
    near(set.duration(), 5.0);
    let cleared = t.set_speed_ramp(c.id, Vec::new()).unwrap();
    assert!(cleared.speed_ramp.is_empty());
    near(cleared.duration(), 8.0);
    let again = t.set_speed_ramp(c.id, Vec::new()).unwrap_err().to_string();
    assert!(again.contains("no speed ramp to clear"), "{again}");
    assert!(t.set_speed_ramp(c.id, vec![SpeedKey::new(0.0, 0.0)]).is_err());
    assert!(t.set_speed_ramp(Uuid::new_v4(), vec![SpeedKey::new(0.0, 1.0)]).is_err());
    t.tracks[0].locked = true;
    let locked = t.set_speed_ramp(c.id, vec![SpeedKey::new(0.0, 1.0)]).unwrap_err();
    assert!(locked.to_string().contains("locked"), "{locked}");
}

#[test]
fn the_diff_says_what_happened_to_the_ramp() {
    let plain = Clip::new(ASSET, 0.0, 8.0, 0.0);
    let mut set = plain.clone();
    set.speed_ramp = vec![SpeedKey::new(0.0, 2.0), SpeedKey::new(1.0, 0.3), SpeedKey::new(2.0, 1.0)];
    assert_eq!(ramp_changes(&plain, &set).unwrap(), "speed ramp 3 keys, 0.30×–2.00×");
    assert_eq!(ramp_changes(&set, &plain).unwrap(), "speed ramp cleared");
    assert!(ramp_changes(&set, &set).is_none());
    let mut more = set.clone();
    more.speed_ramp.push(SpeedKey::new(3.0, 1.0));
    assert_eq!(ramp_changes(&set, &more).unwrap(), "speed ramp 3 → 4 keys, 0.30×–2.00×");
    let mut moved = set.clone();
    moved.speed_ramp[1].time = 1.5;
    assert_eq!(ramp_changes(&set, &moved).unwrap(), "speed ramp retimed");
    let mut faster = set.clone();
    faster.speed_ramp[1].speed = 0.5;
    assert_eq!(ramp_changes(&set, &faster).unwrap(), "speed ramp changed");
    // And it reaches the timeline diff as a clip change.
    let (before, after) = (lane(vec![plain]), lane(vec![set]));
    let diff = before.diff(&after);
    assert!(
        diff.entries
            .iter()
            .any(|e| e.detail.as_deref() == Some("speed ramp 3 keys, 0.30×–2.00×")),
        "{diff:?}"
    );
}
