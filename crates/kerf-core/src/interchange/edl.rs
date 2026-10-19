//! CMX 3600 edit decision lists (`.edl`): one video track, frame-quantized at the
//! timeline rate, record time starting at `01:00:00:00` and source time at the
//! start of each file (Kerf does not read a file's own start timecode). Reels are
//! eight characters made from the file name, unique per file; `* FROM CLIP NAME`
//! / `* SOURCE FILE` comments name the media, dissolves are `D` events, speed is
//! an `M2` line, and 29.97 / 59.94 lists use drop-frame timecode.

use std::collections::HashMap;

use uuid::Uuid;

use super::{clip_extras, one_line, InterchangeOptions, Report, Timebase};
use crate::clip_timing::transition_fx;
use crate::error::{Error, Result};
use crate::model::{Asset, Clip, StreamKind, Timeline};

/// Strict CMX reel width.
const REEL_LEN: usize = 8;

/// The longest `TITLE:` a CMX list carries.
const TITLE_LEN: usize = 70;

/// Reel names, one per file, unique within the list.
struct Reels {
    by_asset: HashMap<Uuid, String>,
}

impl Reels {
    fn reel(&mut self, asset: Option<&Asset>) -> String {
        let Some(asset) = asset else { return "AX".to_string() };
        if let Some(r) = self.by_asset.get(&asset.id) {
            return r.clone();
        }
        let stem = std::path::Path::new(&asset.path.replace('\\', "/"))
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let base: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .take(REEL_LEN)
            .collect();
        let base = if base.is_empty() { "AX".to_string() } else { base };
        let mut reel = base.clone();
        let mut n = 1;
        while self.by_asset.values().any(|r| *r == reel) {
            n += 1;
            let suffix = n.to_string();
            let keep = REEL_LEN.saturating_sub(suffix.len()).min(base.len());
            reel = format!("{}{suffix}", &base[..keep]);
        }
        self.by_asset.insert(asset.id, reel.clone());
        reel
    }
}

/// One line of an event.
struct Line {
    reel: String,
    trans: String,
    dur: Option<i64>,
    src: (i64, i64),
    rec: (i64, i64),
}

struct Event {
    lines: Vec<Line>,
    m2: Option<(String, f64, i64)>,
    comments: Vec<String>,
}

/// The first source frame a clip plays: its in-point, or for a reversed clip the
/// out-point it plays down from.
fn source_entry(tb: &Timebase, clip: &Clip) -> i64 {
    tb.frames(clip.timeline_to_source(clip.timeline_start))
}

/// Where a clip's source run ends after `rec_frames` of record time: the speed's worth of
/// source frames on from the entry, counting down for a reversed clip.
fn source_exit(src_in: i64, rec_frames: i64, clip: &Clip) -> i64 {
    let span = (rec_frames as f64 * clip.speed_mag()).round() as i64;
    if clip.is_reversed() {
        src_in - span
    } else {
        src_in + span
    }
}

pub(super) fn export(timeline: &Timeline, assets: &[Asset], opts: &InterchangeOptions, report: &mut Report) -> Result<String> {
    let tb = Timebase::new(opts.rate)?;
    let video: Vec<usize> = (0..timeline.tracks.len())
        .filter(|&i| timeline.tracks[i].kind == StreamKind::Video)
        .collect();
    let &track_index = video.get(opts.video_track).ok_or_else(|| {
        Error::InvalidArgument(format!(
            "video track {} does not exist; the cut has {} video track(s)",
            opts.video_track + 1,
            video.len()
        ))
    })?;
    let track = &timeline.tracks[track_index];
    let others = video
        .iter()
        .filter(|&&i| i != track_index && !timeline.tracks[i].clips.is_empty())
        .count();
    if others > 0 {
        report.info(format!(
            "an EDL holds one video track; {others} other video track(s) were not written"
        ));
    }
    if timeline
        .tracks
        .iter()
        .any(|t| t.kind == StreamKind::Audio && !t.clips.is_empty())
    {
        report.info("audio tracks are not written to the EDL");
    }
    if !timeline.markers.is_empty() {
        report.info("markers are not written to the EDL");
    }
    if !timeline.overlays.is_empty() || !timeline.svg_overlays.is_empty() {
        report.warn("titles and captions are not carried by an EDL");
    }
    if track.muted {
        report.warn(format!("{} is muted in Kerf but written to the EDL", one_line(&track.name)));
    }

    let fx = transition_fx(timeline, assets);
    let offset: usize = timeline.tracks[..track_index].iter().map(|t| t.clips.len()).sum();
    let record_start = tb.frames_of(1, 0, 0, 0);
    let asset_of = |clip: &Clip| assets.iter().find(|a| a.id == clip.asset_id);
    let name_of = |clip: &Clip| asset_of(clip).map(|a| one_line(&a.name)).unwrap_or_default();
    let file_of = |clip: &Clip| asset_of(clip).map(|a| one_line(&a.path));
    let mut reels = Reels {
        by_asset: HashMap::new(),
    };

    let mut order: Vec<usize> = (0..track.clips.len()).collect();
    order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
    let mut events: Vec<Event> = Vec::new();
    let mut previous: Option<usize> = None;
    for j in order {
        let clip = &track.clips[j];
        if !clip.enabled {
            report.warn("disabled clips were left out of the EDL");
            continue;
        }
        if clip.is_generated() {
            report.warn("titles and captions are not carried by an EDL");
            continue;
        }
        if !tb.on_frame(clip.timeline_start) || !tb.on_frame(clip.timeline_end()) {
            report.warn("clip edges between frames were rounded to the nearest frame");
        }
        let (rec_in, rec_out) = (tb.frames(clip.timeline_start), tb.frames(clip.timeline_end()));
        if rec_out <= rec_in {
            report.warn("clips shorter than a frame were left out of the EDL");
            continue;
        }
        if asset_of(clip).is_none() {
            report.warn("clips whose media is not in the project were written as reel AX");
        }
        for extra in clip_extras(clip) {
            report.warn(format!("clip {extra} are not carried by an EDL"));
        }
        if clip.source_audio && asset_of(clip).is_some_and(|a| a.has_audio()) {
            report.warn("the sound of video clips is not written to the EDL (events are picture only, channel V)");
        }
        let reel = reels.reel(asset_of(clip));
        let src_in = source_entry(&tb, clip);
        let main = Line {
            reel: reel.clone(),
            trans: "C".to_string(),
            dur: None,
            src: (src_in, source_exit(src_in, rec_out - rec_in, clip)),
            rec: (record_start + rec_in, record_start + rec_out),
        };
        let prev = previous
            .map(|p| &track.clips[p])
            .filter(|p| (p.timeline_end() - clip.timeline_start).abs() < 1e-3);
        let mut lines = Vec::new();
        let mut comments = Vec::new();
        let dissolve = clip.transition_in.and_then(|tr| match tr.kind.dip_color() {
            Some(_) => {
                report.warn(format!("{} transitions were written as cuts", tr.kind.label().to_lowercase()));
                None
            }
            None => {
                if tr.kind.slide_from().is_some() {
                    report.warn(format!(
                        "{} transitions were written as dissolves",
                        tr.kind.label().to_lowercase()
                    ));
                }
                let frames = tb.frames(fx[offset + j].afade_in);
                (frames > 0).then_some(frames)
            }
        });
        match dissolve {
            Some(frames) => {
                let (from_reel, from_src) = match prev {
                    Some(p) => (reels.reel(asset_of(p)), tb.frames(p.timeline_to_source(p.timeline_end()))),
                    None => ("BL".to_string(), 0),
                };
                lines.push(Line {
                    reel: from_reel,
                    trans: "C".to_string(),
                    dur: None,
                    src: (from_src, from_src),
                    rec: (main.rec.0, main.rec.0),
                });
                lines.push(Line {
                    trans: "D".to_string(),
                    dur: Some(frames),
                    ..main
                });
                if let Some(p) = prev {
                    comments.push(format!("FROM CLIP NAME: {}", name_of(p)));
                    comments.push(format!("TO CLIP NAME: {}", name_of(clip)));
                    comments.extend(file_of(p).map(|f| format!("SOURCE FILE: {f}")));
                } else {
                    comments.push(format!("TO CLIP NAME: {}", name_of(clip)));
                }
                comments.extend(file_of(clip).map(|f| format!("SOURCE FILE: {f}")));
            }
            None => {
                lines.push(main);
                comments.push(format!("FROM CLIP NAME: {}", name_of(clip)));
                comments.extend(file_of(clip).map(|f| format!("SOURCE FILE: {f}")));
            }
        }
        let m2 = ((clip.speed - 1.0).abs() > f64::EPSILON).then(|| (reel.clone(), clip.speed * tb.base as f64, src_in));
        events.push(Event { lines, m2, comments });
        previous = Some(j);
    }

    let title: String = one_line(&opts.title).chars().take(TITLE_LEN).collect();
    let mut out = format!(
        "TITLE: {title}\nFCM: {}\n",
        if tb.drop { "DROP FRAME" } else { "NON-DROP FRAME" }
    );
    for (i, ev) in events.iter().enumerate() {
        out.push('\n');
        let num = i + 1;
        for l in &ev.lines {
            let dur = l.dur.map(|d| format!("{d:03}")).unwrap_or_default();
            out.push_str(&format!(
                "{num:03}  {:<REEL_LEN$} V     {:<4} {:>3} {} {} {} {}\n",
                l.reel,
                l.trans,
                dur,
                tb.timecode(l.src.0),
                tb.timecode(l.src.1),
                tb.timecode(l.rec.0),
                tb.timecode(l.rec.1),
            ));
        }
        if let Some((reel, fps, entry)) = &ev.m2 {
            let sign = if *fps < 0.0 { "-" } else { "" };
            out.push_str(&format!(
                "M2   {reel:<REEL_LEN$}       {sign}{:05.1}                {}\n",
                fps.abs(),
                tb.timecode(*entry)
            ));
        }
        for c in &ev.comments {
            out.push_str(&format!("* {c}\n"));
        }
    }
    Ok(out)
}
