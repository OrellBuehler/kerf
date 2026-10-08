//! The **differential corpus** for the browser harness's linked-clip edits.
//!
//! The harness (`frontend/src/lib/link-ops.ts` over `links.ts` / `ripple.ts` /
//! `link-groups.ts`) is a hand port of this crate's link-aware ops, and the sync lock in
//! particular is too much arithmetic to trust a port of by reading. So this crate **writes
//! the answers down**: a few dozen edits — each a timeline, the call it is made under
//! (ripple, links), one op, and what `Project` did (the timeline it left, canonicalized, and
//! the revision label — or the exact refusal) — into
//! `frontend/src/lib/fixtures/links-corpus.json`, and `links-corpus.test.ts` replays every
//! case through the TS port and demands the same answer. A rule changed here without the
//! port following fails *that* test, naming the case; a corpus that no longer says what the
//! engine does fails the freshness test below.
//!
//! Ids are fixed in the cases that are written (so the file is stable) and never compared in
//! the answers: a clip an edit creates has a random id, so a timeline is canonicalized — clips
//! in order, assets by index, link groups by first appearance — before it is written.
//!
//! Regenerate after an intended change:
//! `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- links_corpus`

use serde_json::{json, Value};

use super::linked_tests::{asset, stream, Rng};
use super::*;

const CORPUS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../frontend/src/lib/fixtures/links-corpus.json"
);

/// How many random cases are written beside the hand-made ones.
const RANDOM_CASES: usize = 48;

fn uid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The assets every case may name: `AV` (video + audio, 60 s), `VID` (video only, 30 s) and
/// `MUS` (audio only, 90 s). Fixed ids, so the corpus does not move between runs.
const AV: u128 = 0xA1;
const VID: u128 = 0xA2;
const MUS: u128 = 0xA3;

fn assets() -> Vec<Asset> {
    let mut out = vec![
        asset("/av.mp4", 60.0, vec![stream(StreamKind::Video), stream(StreamKind::Audio)]),
        asset("/vid.mp4", 30.0, vec![stream(StreamKind::Video)]),
        asset("/mus.wav", 90.0, vec![stream(StreamKind::Audio)]),
    ];
    for (a, n) in out.iter_mut().zip([AV, VID, MUS]) {
        a.id = uid(n);
    }
    out
}

/// Hands out the fixed ids of one case's clips and link groups.
struct Ids {
    next: u128,
}

impl Ids {
    fn new() -> Self {
        Ids { next: 0x1000 }
    }

    fn id(&mut self) -> Uuid {
        self.next += 1;
        uid(self.next)
    }

    /// A clip with a fixed id, its numbers rounded to the millisecond so the file stays short.
    fn clip(&mut self, asset: u128, source_in: f64, source_out: f64, at: f64) -> Clip {
        let mut c = Clip::new(uid(asset), round3(source_in), round3(source_out), round3(at));
        c.id = self.id();
        c
    }
}

fn lane(kind: StreamKind, name: &str, n: u128, clips: Vec<Clip>) -> Track {
    let mut t = Track::new(kind, name);
    t.id = uid(0x10 + n);
    t.clips = clips;
    t
}

fn timeline(tracks: Vec<Track>) -> Timeline {
    Timeline {
        tracks,
        overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
    }
}

fn link(t: &mut Timeline, ids: &mut Ids, members: &[Uuid]) {
    let group = ids.id();
    for track in &mut t.tracks {
        for c in &mut track.clips {
            if members.contains(&c.id) {
                c.link_id = Some(group);
            }
        }
    }
}

// ---- the cuts the random cases start from ------------------------------------------

/// Three shots on V1, each picture linked to a sound copy on A1 — mirrored lanes — with, when
/// `titles`, an unlinked silent clip between two of them on V1 only.
fn mirrored(rng: &mut Rng, ids: &mut Ids, titles: bool) -> Timeline {
    let (mut v, mut a) = (Vec::new(), Vec::new());
    let mut at = round3(1.0 + rng.unit() * 2.0);
    let mut pairs = Vec::new();
    for i in 0..3 {
        if titles && i == 1 {
            let len = 0.5 + rng.unit() * 2.0;
            v.push(ids.clip(VID, 0.0, len, at));
            at = round3(at + round3(len));
        }
        let len = round3(2.0 + rng.unit() * 5.0);
        let source_in = round3(rng.unit() * 40.0);
        let pic = ids.clip(AV, source_in, source_in + len, at);
        let mut snd = pic.clone();
        snd.id = ids.id();
        pairs.push((pic.id, snd.id));
        v.push(pic);
        a.push(snd);
        at = round3(at + len + if rng.below(3) == 0 { round3(rng.unit() * 2.0) } else { 0.0 });
    }
    let mut t = timeline(vec![lane(StreamKind::Video, "V1", 0, v), lane(StreamKind::Audio, "A1", 1, a)]);
    for (p, s) in pairs {
        link(&mut t, ids, &[p, s]);
    }
    t
}

/// Three shots whose sound leads or trails its picture at each cut (a J/L-cut): every pair in
/// step, covering different stretches.
fn jl(rng: &mut Rng, ids: &mut Ids) -> Timeline {
    let n = 3;
    let mut cuts = vec![round3(2.0 + rng.unit())];
    for _ in 0..n {
        let last = *cuts.last().expect("a cut");
        cuts.push(round3(last + 2.0 + rng.unit() * 5.0));
    }
    let mut shifts: Vec<f64> = (0..=n).map(|_| round3(-1.0 + rng.unit() * 2.5)).collect();
    for i in 0..n {
        shifts[i + 1] = round3(shifts[i + 1].max(1.0 + cuts[i] + shifts[i] - cuts[i + 1]));
    }
    let (mut v, mut a, mut pairs) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..n {
        let source_in = round3(5.0 + rng.unit() * 30.0);
        let len = round3(cuts[i + 1] - cuts[i]);
        let pic = ids.clip(AV, source_in, source_in + len, cuts[i]);
        let snd = ids.clip(
            AV,
            source_in + shifts[i],
            source_in + len + shifts[i + 1],
            cuts[i] + shifts[i],
        );
        pairs.push((pic.id, snd.id));
        v.push(pic);
        a.push(snd);
    }
    let mut t = timeline(vec![lane(StreamKind::Video, "V1", 0, v), lane(StreamKind::Audio, "A1", 1, a)]);
    for (p, s) in pairs {
        link(&mut t, ids, &[p, s]);
    }
    t
}

fn all_clips(t: &Timeline) -> Vec<Clip> {
    t.tracks.iter().flat_map(|tr| tr.clips.clone()).collect()
}

fn pick(rng: &mut Rng, t: &Timeline) -> Clip {
    let all = all_clips(t);
    all[rng.below(all.len())].clone()
}

/// One op on `t`, chosen by `which` (cycling through the menu), with arguments from `rng`.
fn random_op(rng: &mut Rng, t: &Timeline, which: usize) -> Value {
    let c = pick(rng, t);
    let frac = round3(0.15 + rng.unit() * 0.7);
    let at = round3(c.timeline_start + frac * c.duration());
    let src = |f: f64| round3(c.source_in + f * (c.source_out - c.source_in));
    let speed = [0.5, 2.0, -1.0, 1.0][rng.below(4)];
    let side = if rng.below(2) == 0 { "left" } else { "right" };
    match which % 12 {
        0 => json!({"kind": "split_at", "clip_id": c.id, "at": at}),
        1 => json!({"kind": "trim", "clip_id": c.id, "source_out": src(frac)}),
        2 => json!({"kind": "trim", "clip_id": c.id, "source_in": src(frac), "timeline_start": at}),
        3 => json!({"kind": "cut_clip_range", "clip_id": c.id, "from": src(frac * 0.5), "to": src(frac)}),
        4 => json!({"kind": "ripple_delete", "clip_id": c.id}),
        5 => json!({"kind": "remove", "clip_id": c.id}),
        6 => json!({"kind": "set_speed", "clip_id": c.id, "speed": speed}),
        7 => json!({"kind": "split_remove", "clip_id": c.id, "at": at, "side": side}),
        8 => {
            let d = pick(rng, t);
            if d.id == c.id {
                json!({"kind": "remove", "clip_id": c.id})
            } else {
                json!({"kind": "remove_clips", "clip_ids": [c.id, d.id]})
            }
        }
        9 => json!({"kind": "move_clips", "moves": [{"clip_id": c.id, "timeline_start": round3(rng.unit() * 30.0)}]}),
        10 => {
            // What the UI's trim to the playhead does: a click selects the partners too, so both are named.
            let partner = t
                .link_partners(c.id)
                .into_iter()
                .filter_map(|id| t.clip(id))
                .find(|p| p.timeline_start + 1e-3 < at && at < p.timeline_end() - 1e-3)
                .map(|p| p.id);
            match partner {
                Some(p) => {
                    json!({"kind": "split_remove_clips", "cuts": [{"clip_id": c.id, "at": at}, {"clip_id": p, "at": at}], "side": side})
                }
                None => json!({"kind": "split_remove", "clip_id": c.id, "at": at, "side": side}),
            }
        }
        _ => {
            let track = t
                .tracks
                .iter()
                .find(|tr| tr.clips.iter().any(|x| x.id == c.id))
                .expect("on a track");
            json!({"kind": "reorder", "track_id": track.id, "clip_id": c.id, "new_index": rng.below(3)})
        }
    }
}

// ---- hand-made cases ---------------------------------------------------------------

struct Case {
    name: String,
    ripple: bool,
    links: bool,
    before: Timeline,
    op: Value,
}

fn case(name: &str, before: Timeline, op: Value) -> Case {
    Case {
        name: name.to_string(),
        ripple: false,
        links: true,
        before,
        op,
    }
}

fn special_cases() -> Vec<Case> {
    let mut out = Vec::new();

    // V1 holds a clip of AV with its sound still on it; A1 empty.
    let plain = |faders: (f32, f32), volume: f32| {
        let mut ids = Ids::new();
        let mut c = ids.clip(AV, 5.0, 15.0, 2.0);
        c.volume = volume;
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![c.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![]),
        ]);
        t.tracks[0].volume = faders.0;
        t.tracks[1].volume = faders.1;
        (t, c.id)
    };
    for (name, faders, volume) in [
        ("detach folds the picture track's fader into the new clip", (0.5, 1.0), 0.8),
        ("detach divides the audio track's fader back out", (0.5, 2.0), 0.8),
        ("detach onto a silent audio track makes a new one", (1.0, 0.0), 1.0),
    ] {
        let (t, c) = plain(faders, volume);
        out.push(case(name, t, json!({"kind": "detach_audio", "clip_id": c})));
    }

    // Extract: two uses of AV, one on a locked V2.
    {
        let mut ids = Ids::new();
        let (a, b) = (ids.clip(AV, 0.0, 10.0, 0.0), ids.clip(AV, 20.0, 26.0, 10.0));
        let c = ids.clip(AV, 30.0, 36.0, 0.0);
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![a, b]),
            lane(StreamKind::Video, "V2", 2, vec![c]),
            lane(StreamKind::Audio, "A1", 1, vec![]),
        ]);
        t.tracks[1].locked = true;
        out.push(case(
            "extract skips a clip on a locked track and reports it",
            t.clone(),
            json!({"kind": "extract_audio", "asset": uid(AV)}),
        ));
        let ids_all: Vec<Uuid> = all_clips(&t).iter().map(|c| c.id).collect();
        out.push(case(
            "detach several skips what cannot be detached",
            t,
            json!({"kind": "detach_audio_clips", "clip_ids": ids_all}),
        ));
    }
    {
        let mut ids = Ids::new();
        let s = ids.clip(VID, 0.0, 5.0, 0.0);
        let a = ids.clip(AV, 0.0, 5.0, 5.0);
        let t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![s.clone(), a.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![]),
        ]);
        out.push(case(
            "detach several skips an asset with no sound",
            t.clone(),
            json!({"kind": "detach_audio_clips", "clip_ids": [s.id, a.id]}),
        ));
        out.push(case(
            "extract with nothing of the asset on a video track is an error",
            t.clone(),
            json!({"kind": "extract_audio", "asset": uid(MUS)}),
        ));
        out.push(case(
            "add asset audio appends the whole audio",
            t.clone(),
            json!({"kind": "add_asset_audio", "asset": uid(MUS)}),
        ));
        out.push(case(
            "detach with no sound is refused",
            t,
            json!({"kind": "detach_audio", "clip_id": s.id}),
        ));
    }

    // Reattach: a muted picture and (a) its linked sound, (b) an unlinked in-step clip, (c) nothing.
    {
        let build = |kind: &str| {
            let mut ids = Ids::new();
            let mut pic = ids.clip(AV, 0.0, 10.0, 0.0);
            pic.source_audio = false;
            let snd = ids.clip(AV, 0.0, 10.0, 0.0);
            let mut t = timeline(vec![
                lane(StreamKind::Video, "V1", 0, vec![pic.clone()]),
                lane(
                    StreamKind::Audio,
                    "A1",
                    1,
                    if kind == "none" { vec![] } else { vec![snd.clone()] },
                ),
            ]);
            if kind == "linked" {
                link(&mut t, &mut ids, &[pic.id, snd.id]);
            }
            (t, pic.id, snd.id)
        };
        for (name, kind) in [
            ("reattach deletes the linked sound and unmutes the picture", "linked"),
            (
                "reattach is refused while an unlinked clip plays the same footage in step",
                "unlinked",
            ),
            ("reattach unmutes a picture whose sound is gone", "none"),
        ] {
            let (t, pic, _) = build(kind);
            out.push(case(name, t, json!({"kind": "reattach_audio", "clip_id": pic})));
        }
    }

    // Reattach several: one revision for the lot, all or nothing, a pair named by both its clips counted once.
    {
        let mut ids = Ids::new();
        let (mut p1, mut p2) = (ids.clip(AV, 0.0, 10.0, 0.0), ids.clip(AV, 20.0, 26.0, 10.0));
        p1.source_audio = false;
        p2.source_audio = false;
        let (mut s1, mut s2) = (p1.clone(), p2.clone());
        (s1.id, s2.id) = (ids.id(), ids.id());
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![p1.clone(), p2.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![s1.clone(), s2.clone()]),
        ]);
        link(&mut t, &mut ids, &[p1.id, s1.id]);
        link(&mut t, &mut ids, &[p2.id, s2.id]);
        out.push(case(
            "reattach several is one revision and counts a pair named twice once",
            t.clone(),
            json!({"kind": "reattach_audio_clips", "clip_ids": [s2.id, p2.id, p1.id]}),
        ));
        let mut unlinked = t.clone();
        for c in unlinked.tracks.iter_mut().flat_map(|tr| tr.clips.iter_mut()) {
            if c.id == p2.id || c.id == s2.id {
                c.link_id = None;
            }
        }
        out.push(case(
            "reattach several is all or nothing: one pair that would double refuses the lot",
            unlinked,
            json!({"kind": "reattach_audio_clips", "clip_ids": [p1.id, p2.id]}),
        ));
        out.push(case(
            "reattach several on an empty list is an error",
            t.clone(),
            json!({"kind": "reattach_audio_clips", "clip_ids": []}),
        ));
        let mut locked = t;
        locked.tracks[1].locked = true;
        out.push(case(
            "reattach several refuses a locked sound track",
            locked,
            json!({"kind": "reattach_audio_clips", "clip_ids": [p1.id]}),
        ));
    }

    // A trim carries its edge to the sound; the sound may not land on a clip outside its group, once
    // the ripple (which can push that clip away) has run.
    {
        let mut ids = Ids::new();
        let mut pic = ids.clip(AV, 0.0, 5.0, 0.0);
        pic.source_audio = false;
        let mut snd = pic.clone();
        snd.id = ids.id();
        let vo = ids.clip(MUS, 0.0, 3.0, 6.0);
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![pic.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![snd.clone(), vo]),
        ]);
        link(&mut t, &mut ids, &[pic.id, snd.id]);
        for (name, ripple, op) in [
            (
                "a trim that extends the sound over an unlinked clip is refused",
                false,
                json!({"kind": "trim", "clip_id": pic.id, "source_out": 6.5}),
            ),
            (
                "a move by trim that lands the sound on an unlinked clip is refused",
                false,
                json!({"kind": "trim", "clip_id": pic.id, "timeline_start": 1.5}),
            ),
            (
                "a ripple pushes the unlinked clip ahead of the extended sound",
                true,
                json!({"kind": "trim", "clip_id": pic.id, "source_out": 6.5}),
            ),
            (
                "a move by trim is refused under ripple too, nothing rippling",
                true,
                json!({"kind": "trim", "clip_id": pic.id, "timeline_start": 1.5}),
            ),
            (
                "a trim that stops short of the unlinked clip is carried",
                false,
                json!({"kind": "trim", "clip_id": pic.id, "source_out": 5.9}),
            ),
        ] {
            let mut c = case(name, t.clone(), op);
            c.ripple = ripple;
            out.push(c);
        }
    }

    // Paste: a lone muted picture gets its sound back; a muted picture pasted with its sound stays muted.
    {
        let mut ids = Ids::new();
        let mut pic = ids.clip(AV, 0.0, 10.0, 0.0);
        pic.source_audio = false;
        let snd = ids.clip(AV, 0.0, 10.0, 0.0);
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![pic.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![snd.clone()]),
        ]);
        link(&mut t, &mut ids, &[pic.id, snd.id]);
        out.push(case(
            "duplicating a lone muted picture gives the copy its own sound",
            t.clone(),
            json!({"kind": "duplicate_clips", "clip_ids": [pic.id], "at": 20.0}),
        ));
        out.push(case(
            "duplicating a muted picture with its sound keeps the copy muted and linked",
            t.clone(),
            json!({"kind": "duplicate_clips", "clip_ids": [pic.id, snd.id], "at": 20.0}),
        ));
        out.push(case(
            "duplicating a sound alone leaves it a plain clip",
            t.clone(),
            json!({"kind": "duplicate_clips", "clip_ids": [snd.id], "at": 20.0}),
        ));
        out.push(case(
            "unlinking either half of a pair unlinks the pair",
            t.clone(),
            json!({"kind": "unlink_clips", "clip_ids": [snd.id]}),
        ));
        out.push(case(
            "already linked is not an edit",
            t,
            json!({"kind": "link_clips", "clip_ids": [pic.id, snd.id]}),
        ));
    }

    // Splits and cuts that have to hand a partner to a side.
    {
        let mut ids = Ids::new();
        let pic = ids.clip(AV, 0.0, 10.0, 0.0);
        let snd = ids.clip(AV, 5.0, 10.0, 5.0);
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![pic.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![snd.clone()]),
        ]);
        link(&mut t, &mut ids, &[pic.id, snd.id]);
        out.push(case(
            "a split before the sound hands the sound to the right half",
            t.clone(),
            json!({"kind": "split_at", "clip_id": pic.id, "at": 3.0}),
        ));
        out.push(case(
            "a split inside both cuts both",
            t.clone(),
            json!({"kind": "split_at", "clip_id": pic.id, "at": 7.0}),
        ));
        out.push(case(
            "a cut whose stretch swallows the sound's head resumes it at the cut",
            t.clone(),
            json!({"kind": "cut_clip_range", "clip_id": pic.id, "from": 3.0, "to": 8.0}),
        ));
        out.push(case(
            "a cut after the sound begins cuts both",
            t,
            json!({"kind": "cut_clip_range", "clip_id": pic.id, "from": 6.0, "to": 8.0}),
        ));
    }

    // Both lanes pushed by different amounts: a ripple trim that the partner can only follow part
    // way (its footage runs out), so the track of the clip that was named has to speak for the group.
    {
        let mut ids = Ids::new();
        let (x, x2) = (ids.clip(AV, 45.0, 55.0, 5.0), ids.clip(AV, 0.0, 10.0, 15.0));
        let (y, y2) = (ids.clip(AV, 45.0, 55.0, 5.0), ids.clip(AV, 0.0, 10.0, 15.0));
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![x.clone(), x2.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![y.clone(), y2.clone()]),
        ]);
        link(&mut t, &mut ids, &[x.id, y.id]);
        link(&mut t, &mut ids, &[x2.id, y2.id]);
        for (name, named) in [
            ("a ripple trim named by the picture: its track speaks for the group", x.id),
            ("a ripple trim named by the sound: its track speaks for the group", y.id),
        ] {
            let mut c = case(name, t.clone(), json!({"kind": "trim", "clip_id": named, "source_out": 65.0}));
            c.ripple = true;
            out.push(c);
        }
    }

    // The second review: named partners, leftovers of a cut, victims, faders.
    {
        let build = || {
            let mut ids = Ids::new();
            let (x1, x2) = (ids.clip(AV, 5.0, 15.0, 5.0), ids.clip(AV, 15.0, 25.0, 15.0));
            let (y1, y2) = (ids.clip(AV, 0.0, 13.0, 0.0), ids.clip(AV, 13.0, 25.0, 13.0));
            let mut t = timeline(vec![
                lane(StreamKind::Video, "V1", 0, vec![x1.clone(), x2.clone()]),
                lane(StreamKind::Audio, "A1", 1, vec![y1.clone(), y2.clone()]),
            ]);
            link(&mut t, &mut ids, &[x1.id, y1.id]);
            link(&mut t, &mut ids, &[x2.id, y2.id]);
            (t, [x1.id, x2.id, y1.id, y2.id])
        };
        let (t, [x1, _, y1, _]) = build();
        for side in ["left", "right"] {
            let mut c = case(
                &format!("trim to the playhead naming both partners of a J-cut ripples them in step ({side})"),
                t.clone(),
                json!({"kind": "split_remove_clips", "cuts": [{"clip_id": x1, "at": 8.0}, {"clip_id": y1, "at": 8.0}], "side": side}),
            );
            c.ripple = true;
            out.push(c);
        }
        let mut c = case(
            "a ripple trim named by the sound pulls its picture onto the one before: refused, never cut",
            t.clone(),
            json!({"kind": "trim", "clip_id": y1, "source_out": 10.0}),
        );
        c.ripple = true;
        out.push(c);
        // A cut of a whole clip: the partner keeps its head, which a later pair then comes up to.
        let mut t2 = t;
        t2.tracks[1].clips[1].source_in = 23.0;
        t2.tracks[1].clips[1].source_out = 35.0;
        t2.tracks[0].clips[1].source_in = 25.0;
        t2.tracks[0].clips[1].source_out = 35.0;
        out.push(case(
            "cutting a whole clip leaves its partner's head to give way to the pair after it",
            t2,
            json!({"kind": "cut_clip_range", "clip_id": x1, "from": 5.0, "to": 15.0}),
        ));
    }
    {
        let mut ids = Ids::new();
        let x = ids.clip(AV, 0.0, 10.0, 0.0);
        let s = ids.clip(AV, 9.0, 15.0, 9.0);
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![x.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![s.clone()]),
        ]);
        link(&mut t, &mut ids, &[x.id, s.id]);
        {
            // A1 holds an unlinked clip, then the sound of the picture on V1 (which starts later than
            // the sound's picture would let it): deleting the unlinked clip pulls the sound up by 3 s,
            // and the picture follows it — before 0, where a picture is never trimmed.
            let mut ids = Ids::new();
            let x = ids.clip(AV, 0.0, 10.0, 0.0);
            let gap = ids.clip(MUS, 0.0, 3.0, 0.0);
            let y = ids.clip(AV, 3.0, 10.0, 3.0);
            let mut t = timeline(vec![
                lane(StreamKind::Video, "V1", 0, vec![x.clone()]),
                lane(StreamKind::Audio, "A1", 1, vec![gap.clone(), y.clone()]),
            ]);
            link(&mut t, &mut ids, &[x.id, y.id]);
            out.push(case(
                "a picture pulled before 0 by its sound is refused, never trimmed",
                t,
                json!({"kind": "ripple_delete", "clip_id": gap.id}),
            ));
        }
        out.push(case(
            "a lone leftover of a partner still resumes at the cut",
            t,
            json!({"kind": "cut_clip_range", "clip_id": x.id, "from": 8.0, "to": 10.0}),
        ));
        let mut ids = Ids::new();
        let x = ids.clip(AV, 0.0, 10.0, 0.0);
        let s = ids.clip(AV, 12.0, 20.0, 12.0);
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![x.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![s.clone()]),
        ]);
        link(&mut t, &mut ids, &[x.id, s.id]);
        out.push(case(
            "a partner after the cut comes up by it even when the named clip keeps nothing after",
            t,
            json!({"kind": "cut_clip_range", "clip_id": x.id, "from": 8.0, "to": 10.0}),
        ));
    }
    {
        // A sound pulled before 0 is trimmed and the label says so; a floor under 0.05 s refuses.
        let mut ids = Ids::new();
        let (x1, x2) = (ids.clip(AV, 0.0, 10.0, 0.0), ids.clip(AV, 10.0, 20.0, 10.0));
        let (y1, y2) = (ids.clip(AV, 0.0, 10.0, 0.0), ids.clip(AV, 8.0, 20.0, 8.0));
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![x1.clone(), x2.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![y1.clone(), y2.clone()]),
        ]);
        link(&mut t, &mut ids, &[x1.id, y1.id]);
        link(&mut t, &mut ids, &[x2.id, y2.id]);
        out.push(case(
            "a J-cut's lead pulled before 0 is trimmed and the label says so",
            t,
            json!({"kind": "ripple_delete", "clip_id": x1.id}),
        ));

        let mut ids = Ids::new();
        let (x1, x2) = (ids.clip(AV, 0.0, 10.0, 0.0), ids.clip(AV, 10.0, 20.0, 10.0));
        let (y1, y2) = (ids.clip(AV, 9.0, 12.0, 9.0), ids.clip(AV, 12.0, 20.0, 12.0));
        let mut t = timeline(vec![
            lane(StreamKind::Video, "V1", 0, vec![x1.clone(), x2.clone()]),
            lane(StreamKind::Audio, "A1", 1, vec![y1.clone(), y2.clone()]),
        ]);
        link(&mut t, &mut ids, &[x1.id, y1.id]);
        link(&mut t, &mut ids, &[x2.id, y2.id]);
        let mut c = case(
            "a clip that would be left under 0.05 s is refused, not stubbed",
            t.clone(),
            json!({"kind": "trim", "clip_id": x1.id, "source_out": 1.97}),
        );
        c.ripple = true;
        out.push(c);
        let mut c = case(
            "a sound trimmed to make room is said in the label",
            t,
            json!({"kind": "trim", "clip_id": x1.id, "source_out": 9.0}),
        );
        c.ripple = true;
        out.push(c);
    }
    {
        // Faders and dynamics.
        let build = |lanes: &[f32], effect: Option<AudioEffect>| {
            let mut ids = Ids::new();
            let mut c = ids.clip(AV, 5.0, 15.0, 2.0);
            c.volume = 0.8;
            if let Some(e) = effect {
                c.audio = vec![e];
            }
            let mut tracks = vec![lane(StreamKind::Video, "V1", 0, vec![c.clone()])];
            for (i, f) in lanes.iter().enumerate() {
                tracks.push(lane(StreamKind::Audio, &format!("A{}", i + 1), 1 + i as u128, vec![]));
                tracks.last_mut().expect("a lane").volume = *f;
            }
            let mut t = timeline(tracks);
            t.tracks[0].volume = 0.5;
            (t, c.id)
        };
        let compressor = AudioEffect::Compressor {
            threshold_db: -18.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
        };
        let (t, c) = build(&[2.0], Some(compressor));
        out.push(case(
            "a compressor on the clip: detach makes a lane at the picture track's fader",
            t,
            json!({"kind": "detach_audio", "clip_id": c}),
        ));
        let (t, c) = build(&[2.0, 0.5], Some(AudioEffect::Gate { threshold_db: -40.0 }));
        out.push(case(
            "a gate on the clip: detach takes the lane that is already at the picture track's fader",
            t,
            json!({"kind": "detach_audio", "clip_id": c}),
        ));
        let (t, c) = build(&[2.0], Some(AudioEffect::Highpass { hz: 80.0 }));
        out.push(case(
            "a linear chain still folds the fader",
            t,
            json!({"kind": "detach_audio", "clip_id": c}),
        ));
    }

    // A J/L-cut under the sync lock.
    {
        let build = || {
            let mut ids = Ids::new();
            let (x1, x2) = (
                ids.clip(AV, 105.0 - 100.0, 115.0 - 100.0, 5.0),
                ids.clip(AV, 15.0, 25.0, 15.0),
            );
            let (y1, y2) = (ids.clip(AV, 0.0, 13.0, 0.0), ids.clip(AV, 13.0, 25.0, 13.0));
            let mut t = timeline(vec![
                lane(StreamKind::Video, "V1", 0, vec![x1.clone(), x2.clone()]),
                lane(StreamKind::Audio, "A1", 1, vec![y1.clone(), y2.clone()]),
            ]);
            link(&mut t, &mut ids, &[x1.id, y1.id]);
            link(&mut t, &mut ids, &[x2.id, y2.id]);
            (t, ids, [x1.id, x2.id, y1.id, y2.id])
        };
        let (t, _, [x1, x2, y1, _]) = build();
        out.push(case(
            "ripple delete of a J-cut closes by the picture removed",
            t.clone(),
            json!({"kind": "ripple_delete", "clip_id": x1}),
        ));
        out.push(case(
            "ripple delete named by the sound closes by the sound",
            t.clone(),
            json!({"kind": "ripple_delete", "clip_id": y1}),
        ));
        let mut c = case(
            "a ripple trim of a J-cut trims the earlier sound back",
            t.clone(),
            json!({"kind": "trim", "clip_id": x1, "source_out": 12.0}),
        );
        c.ripple = true;
        out.push(c);
        let mut c = case(
            "a speed change re-places the sound about the picture",
            t.clone(),
            json!({"kind": "set_speed", "clip_id": x1, "speed": 2.0}),
        );
        c.ripple = true;
        out.push(c);
        out.push(case(
            "a cut range through a J-cut cuts the sound in two",
            t.clone(),
            json!({"kind": "cut_clip_range", "clip_id": x1, "from": 8.0, "to": 11.0}),
        ));
        out.push(case("two partners named and moved apart are refused as out of step", t.clone(), json!({"kind": "move_clips", "moves": [{"clip_id": x2, "timeline_start": 21.0}, {"clip_id": t.tracks[1].clips[1].id, "timeline_start": 17.0}]})));
        out.push(case(
            "a reorder carries the sound",
            t.clone(),
            json!({"kind": "reorder", "track_id": t.tracks[0].id, "clip_id": x2, "new_index": 0}),
        ));
        let mut blocked = t.clone();
        let mut ids = Ids::new();
        ids.next = 0x2000;
        blocked.tracks[1].clips.push(ids.clip(AV, 0.0, 2.0, 2.0));
        out.push(case(
            "an unlinked clip in the follower's way refuses, naming the lane",
            blocked,
            json!({"kind": "ripple_delete", "clip_id": x1}),
        ));
        let mut locked = t;
        locked.tracks[1].locked = true;
        out.push(case(
            "a locked partner refuses",
            locked,
            json!({"kind": "ripple_delete", "clip_id": x1}),
        ));
    }
    out
}

// ---- running a case, and writing down the answer -----------------------------------

fn uuid_of(v: &Value) -> Uuid {
    v.as_str().and_then(|s| s.parse().ok()).expect("an id")
}

fn ids_of(v: &Value) -> Vec<Uuid> {
    v.as_array().expect("ids").iter().map(uuid_of).collect()
}

/// What the op returned beyond the timeline, where the harness is held to it too.
fn apply(p: &Project, op: &Value) -> Result<Value> {
    let f = |k: &str| op[k].as_f64().expect("a number");
    let opt = |k: &str| op[k].as_f64();
    let report = |d: crate::model::DetachedMany| json!({"detached": d.detached.len(), "skipped": d.skipped.iter().map(|s| s.reason.clone()).collect::<Vec<_>>()});
    match op["kind"].as_str().expect("a kind") {
        "split_at" => p.split_at(uuid_of(&op["clip_id"]), f("at")).map(|_| Value::Null),
        "trim" => p
            .trim(
                uuid_of(&op["clip_id"]),
                opt("source_in"),
                opt("source_out"),
                opt("timeline_start"),
            )
            .map(|_| Value::Null),
        "cut_clip_range" => p
            .cut_clip_range(uuid_of(&op["clip_id"]), f("from"), f("to"))
            .map(|_| Value::Null),
        "ripple_delete" => p.ripple_delete(uuid_of(&op["clip_id"])).map(|_| Value::Null),
        "remove" => p.remove(uuid_of(&op["clip_id"])).map(|_| Value::Null),
        "remove_clips" => p.remove_clips(&ids_of(&op["clip_ids"])).map(|n| json!({"removed": n})),
        "set_speed" => p.set_speed(uuid_of(&op["clip_id"]), f("speed")).map(|_| Value::Null),
        "split_remove" => {
            let side = if op["side"] == "left" {
                SplitSide::Left
            } else {
                SplitSide::Right
            };
            p.split_remove(uuid_of(&op["clip_id"]), f("at"), side).map(|_| Value::Null)
        }
        "split_remove_clips" => {
            let side = if op["side"] == "left" {
                SplitSide::Left
            } else {
                SplitSide::Right
            };
            let cuts: Vec<ClipCut> = op["cuts"]
                .as_array()
                .expect("cuts")
                .iter()
                .map(|c| ClipCut {
                    clip_id: uuid_of(&c["clip_id"]),
                    at: c["at"].as_f64().expect("a time"),
                })
                .collect();
            p.split_remove_clips(&cuts, side).map(|_| Value::Null)
        }
        "move_clips" => {
            let moves: Vec<ClipMove> = op["moves"]
                .as_array()
                .expect("moves")
                .iter()
                .map(|m| ClipMove {
                    clip_id: uuid_of(&m["clip_id"]),
                    timeline_start: m["timeline_start"].as_f64().expect("a start"),
                    track_id: None,
                })
                .collect();
            p.move_clips(&moves).map(|_| Value::Null)
        }
        "reorder" => p
            .reorder(
                uuid_of(&op["track_id"]),
                uuid_of(&op["clip_id"]),
                op["new_index"].as_u64().expect("an index") as usize,
            )
            .map(|_| Value::Null),
        "detach_audio" => p.detach_audio(uuid_of(&op["clip_id"])).map(|_| Value::Null),
        "detach_audio_clips" => p.detach_audio_clips(&ids_of(&op["clip_ids"])).map(report),
        "reattach_audio" => p.reattach_audio(uuid_of(&op["clip_id"])).map(|_| Value::Null),
        "reattach_audio_clips" => p
            .reattach_audio_clips(&ids_of(&op["clip_ids"]))
            .map(|done| json!({"reattached": done.len()})),
        "extract_audio" => p.extract_audio(uuid_of(&op["asset"])).map(report),
        "add_asset_audio" => p.add_asset_audio(uuid_of(&op["asset"])).map(|_| Value::Null),
        "link_clips" => p.link_clips(&ids_of(&op["clip_ids"])).map(|_| Value::Null),
        "unlink_clips" => p.unlink_clips(&ids_of(&op["clip_ids"])).map(|n| json!({"unlinked": n})),
        "duplicate_clips" => p.duplicate_clips(&ids_of(&op["clip_ids"]), f("at")).map(|_| Value::Null),
        other => panic!("unknown op {other}"),
    }
}

/// A timeline as the corpus compares it: tracks in order, clips in start order, assets by
/// index, link groups by first appearance — no ids, so the clips an edit creates compare.
fn canonical(t: &Timeline, assets: &[Asset]) -> Value {
    let mut groups: HashMap<Uuid, usize> = HashMap::new();
    let tracks: Vec<Value> = t
        .tracks
        .iter()
        .map(|track| {
            let mut clips: Vec<&Clip> = track.clips.iter().collect();
            clips.sort_by(|a, b| {
                a.timeline_start
                    .total_cmp(&b.timeline_start)
                    .then(a.source_in.total_cmp(&b.source_in))
                    .then(a.source_out.total_cmp(&b.source_out))
            });
            let clips: Vec<Value> = clips
                .iter()
                .map(|c| {
                    let link = c.link_id.map(|l| {
                        let n = groups.len();
                        format!("g{}", *groups.entry(l).or_insert(n))
                    });
                    json!({
                        "asset": assets.iter().position(|a| a.id == c.asset_id),
                        "start": c.timeline_start,
                        "in": c.source_in,
                        "out": c.source_out,
                        "speed": c.speed,
                        "volume": c.volume,
                        "fade_in": c.fade_in,
                        "fade_out": c.fade_out,
                        "own_sound": c.source_audio,
                        "link": link,
                    })
                })
                .collect();
            json!({
                "name": track.name,
                "kind": track.kind,
                "volume": track.volume,
                "locked": track.locked,
                "clips": clips,
            })
        })
        .collect();
    json!({ "tracks": tracks })
}

fn run(c: &Case, assets: &[Asset]) -> Value {
    let project = Project::open_in_memory().expect("a project");
    for a in assets {
        project.insert_asset(a).expect("an asset");
    }
    project.save_timeline(&c.before).expect("a timeline");
    project.set_ripple_mode(c.ripple).expect("ripple");
    match project.with_links(Some(c.links), |p| apply(p, &c.op)) {
        Ok(report) => json!({
            "ok": true,
            "label": project.history().expect("history").last().expect("a revision").label,
            "report": report,
            "timeline": canonical(&project.timeline().expect("a timeline"), assets),
        }),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

/// A timeline as the corpus stores its starting point: what `Clip::new` fills in on its own
/// (the neutral colour grade and transform, no transition) left out, so the file stays short and
/// the TS side's loader is exercised on the same sparse JSON an old project file is.
fn slim(t: &Timeline) -> Value {
    let mut v = serde_json::to_value(t).expect("serializes");
    let plain = serde_json::to_value(Clip::new(uid(0), 0.0, 1.0, 0.0)).expect("serializes");
    for track in v["tracks"].as_array_mut().expect("tracks") {
        for clip in track["clips"].as_array_mut().expect("clips") {
            let clip = clip.as_object_mut().expect("a clip");
            for key in ["color", "transform", "transition_in"] {
                if clip.get(key) == plain.get(key) {
                    clip.remove(key);
                }
            }
        }
    }
    v
}

fn corpus() -> Value {
    let assets = assets();
    let mut cases: Vec<Case> = special_cases();
    let mut seed = 0u64;
    let mut random = 0;
    while random < RANDOM_CASES {
        seed += 1;
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut ids = Ids::new();
        let (kind, before) = match seed % 3 {
            0 => ("mirrored", mirrored(&mut rng, &mut ids, false)),
            1 => ("a titled cut", mirrored(&mut rng, &mut ids, true)),
            _ => ("a J/L-cut", jl(&mut rng, &mut ids)),
        };
        let which = seed as usize;
        let op = random_op(&mut rng, &before, which);
        let ripple = rng.below(2) == 0;
        // Mostly links on; a few with the escape hatch, which must agree too.
        let links = !seed.is_multiple_of(9);
        cases.push(Case {
            name: format!("{} on {kind} (seed {seed})", op["kind"].as_str().expect("a kind")),
            ripple,
            links,
            before,
            op,
        });
        random += 1;
    }
    let results: Vec<Value> = cases
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "ripple": c.ripple,
                "links": c.links,
                "before": slim(&c.before),
                "op": c.op,
                "expect": run(c, &assets),
            })
        })
        .collect();
    json!({
        "about": "Written by kerf-core (project/linked_corpus.rs); replayed by links-corpus.test.ts. Regenerate: KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- links_corpus",
        "assets": assets.iter().map(|a| json!({
            "id": a.id,
            "duration": a.duration,
            "audio": a.has_audio(),
        })).collect::<Vec<_>>(),
        "cases": results,
    })
}

#[test]
fn links_corpus_is_what_this_engine_does() {
    let fresh = format!("{}\n", serde_json::to_string(&corpus()).expect("serializes"));
    if std::env::var_os("KERF_BLESS_CORPUS").is_some() {
        std::fs::create_dir_all(std::path::Path::new(CORPUS_PATH).parent().expect("a directory")).expect("fixtures dir");
        std::fs::write(CORPUS_PATH, &fresh).expect("writes the corpus");
        return;
    }
    // A Windows checkout may hand the file back with CRLF line endings (`.gitattributes` asks for
    // LF, but a stale clone predates it): the content is what is compared, not the newline.
    let on_disk = std::fs::read_to_string(CORPUS_PATH)
        .expect("frontend/src/lib/fixtures/links-corpus.json — generate it with KERF_BLESS_CORPUS=1")
        .replace("\r\n", "\n");
    assert!(
        on_disk == fresh,
        "the linked-clip corpus is stale: the engine now does something the checked-in answers do not say. \
         Regenerate with KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- links_corpus, \
         then run `bun test` — the replay says what the port has to follow."
    );
}

/// The corpus says something about both outcomes: edits that went through and edits that
/// were refused, with the reason — and covers every op the harness mirrors.
#[test]
fn the_corpus_covers_the_ops_and_both_outcomes() {
    let c = corpus();
    let cases = c["cases"].as_array().expect("cases");
    let ok = cases.iter().filter(|x| x["expect"]["ok"] == true).count();
    assert!(
        ok > 40 && cases.len() - ok > 6,
        "{ok} edits applied, {} refused",
        cases.len() - ok
    );
    let kinds: HashSet<&str> = cases.iter().map(|x| x["op"]["kind"].as_str().expect("a kind")).collect();
    for kind in [
        "split_at",
        "trim",
        "cut_clip_range",
        "ripple_delete",
        "remove",
        "remove_clips",
        "set_speed",
        "split_remove",
        "split_remove_clips",
        "move_clips",
        "reorder",
        "detach_audio",
        "detach_audio_clips",
        "reattach_audio",
        "reattach_audio_clips",
        "extract_audio",
        "add_asset_audio",
        "link_clips",
        "unlink_clips",
        "duplicate_clips",
    ] {
        assert!(kinds.contains(kind), "no case for `{kind}`");
    }
}
