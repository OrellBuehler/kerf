//! OpenTimelineIO JSON (`.otio`): every track, with its clips, gaps and
//! transitions, speed as a `LinearTimeWarp`, media references to the original
//! files and the timeline's markers. What OTIO has no field for travels in a
//! `metadata.kerf` blob — each clip's full Kerf state, each track's mixer
//! settings, the titles, the delivery frame and the master bus — so nothing is
//! lost on the way out, only (for another application) unread.

use serde_json::{json, Value};

use super::{clip_extras, export_speed, file_url, one_line, InterchangeOptions, Report, Timebase};
use crate::clip_timing::transition_fx;
use crate::error::{Error, Result};
use crate::model::{Asset, Clip, Marker, StreamKind, Timeline, Track};

/// Frames at `tb`, to a millionth of a frame: an off-frame time keeps its
/// fraction and an on-frame one is a whole number however it was summed.
fn value(tb: &Timebase, seconds: f64) -> f64 {
    let v = (seconds * tb.fps * 1e6).round() / 1e6;
    v + 0.0
}

fn rt(tb: &Timebase, seconds: f64) -> Value {
    json!({"OTIO_SCHEMA": "RationalTime.1", "rate": tb.fps, "value": value(tb, seconds)})
}

fn range(tb: &Timebase, start: f64, duration: f64) -> Value {
    json!({"OTIO_SCHEMA": "TimeRange.1", "start_time": rt(tb, start), "duration": rt(tb, duration)})
}

fn gap(tb: &Timebase, duration: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "name": "",
        "source_range": range(tb, 0.0, duration),
        "effects": [],
        "markers": [],
        "enabled": true,
        "metadata": {},
    })
}

/// OTIO's marker colours, nearest to a CSS hex colour by hue (greys by lightness).
fn marker_color(css: Option<&str>) -> &'static str {
    const NAMED: [&str; 11] = [
        "RED", "PINK", "ORANGE", "YELLOW", "GREEN", "CYAN", "BLUE", "PURPLE", "MAGENTA", "BLACK", "WHITE",
    ];
    let Some(css) = css.map(str::trim).filter(|c| !c.is_empty()) else {
        return "RED";
    };
    if let Some(named) = NAMED.iter().find(|n| n.eq_ignore_ascii_case(css)) {
        return named;
    }
    let hex = css.trim_start_matches('#');
    let digits: Option<Vec<u8>> = match hex.len() {
        3 => hex.chars().map(|c| c.to_digit(16).map(|d| (d * 17) as u8)).collect(),
        6 | 8 => (0..3)
            .map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
            .collect(),
        _ => None,
    };
    let Some([r, g, b]) = digits.as_deref().and_then(|d| <[u8; 3]>::try_from(d).ok()) else {
        return "RED";
    };
    let (r, g, b) = (f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 0.1 {
        return if max < 0.5 { "BLACK" } else { "WHITE" };
    }
    let hue = if max == r {
        60.0 * ((g - b) / (max - min)).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / (max - min) + 2.0)
    } else {
        60.0 * ((r - g) / (max - min) + 4.0)
    };
    match hue {
        h if h < 15.0 => "RED",
        h if h < 45.0 => "ORANGE",
        h if h < 70.0 => "YELLOW",
        h if h < 160.0 => "GREEN",
        h if h < 200.0 => "CYAN",
        h if h < 255.0 => "BLUE",
        h if h < 290.0 => "PURPLE",
        h if h < 330.0 => "MAGENTA",
        h if h < 345.0 => "PINK",
        _ => "RED",
    }
}

fn marker_json(tb: &Timebase, m: &Marker) -> Value {
    json!({
        "OTIO_SCHEMA": "Marker.2",
        "name": m.name,
        "comment": m.comment,
        "color": marker_color(m.color.as_deref()),
        "marked_range": range(tb, m.time, m.duration),
        "metadata": {"kerf": m},
    })
}

fn media_reference(tb: &Timebase, asset: Option<&Asset>, report: &mut Report) -> Value {
    match asset {
        Some(a) => json!({
            "OTIO_SCHEMA": "ExternalReference.1",
            "name": a.name,
            "target_url": file_url(&a.path),
            "available_range": if a.is_image() { Value::Null } else { range(tb, 0.0, a.duration) },
            "available_image_bounds": null,
            "metadata": {"kerf": {"asset_id": a.id}},
        }),
        None => {
            report.warn("clips whose media is not in the project were written with a missing media reference");
            json!({"OTIO_SCHEMA": "MissingReference.1", "name": "", "available_range": null, "metadata": {}})
        }
    }
}

fn clip_json(tb: &Timebase, clip: &Clip, asset: Option<&Asset>, report: &mut Report) -> Result<Value> {
    let mut effects = Vec::new();
    let speed = export_speed(clip);
    if (speed - 1.0).abs() > f64::EPSILON {
        effects.push(json!({
            "OTIO_SCHEMA": "LinearTimeWarp.1",
            "name": "",
            "effect_name": "LinearTimeWarp",
            "time_scalar": speed,
            "metadata": {},
        }));
    }
    if clip.is_ramped() {
        report.warn(
            "speed ramps were written at their average speed (a LinearTimeWarp is one rate); the ramp itself is kept in metadata.kerf",
        );
    }
    for extra in clip_extras(clip) {
        report.info(format!(
            "clip {extra} have no OTIO equivalent; they are kept in metadata.kerf"
        ));
    }
    let state = serde_json::to_value(clip).map_err(|e| Error::Other(e.to_string()))?;
    if let Some(generator) = &clip.generator {
        report.info("titles on tracks are written as generator references; their artwork is kept in metadata.kerf");
        return Ok(json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": generator.display_name(),
            "source_range": range(tb, clip.source_in, clip.duration()),
            "effects": effects,
            "markers": [],
            "enabled": clip.enabled,
            "media_references": {"DEFAULT_MEDIA": {
                "OTIO_SCHEMA": "GeneratorReference.1",
                "name": generator.display_name(),
                "generator_kind": "KerfTitle",
                "parameters": {},
                "available_range": null,
                "available_image_bounds": null,
                "metadata": {},
            }},
            "active_media_reference_key": "DEFAULT_MEDIA",
            "metadata": {"kerf": state},
        }));
    }
    Ok(json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": asset.map(|a| a.name.as_str()).unwrap_or(""),
        "source_range": range(tb, clip.source_in, clip.duration()),
        "effects": effects,
        "markers": [],
        "enabled": clip.enabled,
        "media_references": {"DEFAULT_MEDIA": media_reference(tb, asset, report)},
        "active_media_reference_key": "DEFAULT_MEDIA",
        "metadata": {"kerf": state},
    }))
}

fn track_json(
    tb: &Timebase,
    track: &Track,
    fx: &[crate::clip_timing::ClipFx],
    assets: &[Asset],
    report: &mut Report,
) -> Result<Value> {
    let mut order: Vec<usize> = (0..track.clips.len()).collect();
    order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
    let mut children = Vec::new();
    let mut at = 0.0_f64;
    for j in order {
        let clip = &track.clips[j];
        if clip.timeline_start < at - 1e-6 {
            report.warn("overlapping clips on a track were written one after another");
        }
        if clip.timeline_start > at + 1e-9 {
            children.push(gap(tb, clip.timeline_start - at));
        }
        if let Some(tr) = clip.transition_in {
            let f = fx[j];
            let (before, after) = match tr.kind.dip_color() {
                Some(_) => {
                    let before = children
                        .last()
                        .filter(|c| c["OTIO_SCHEMA"] == "Clip.2")
                        .map_or(0.0, |_| tr.duration / 2.0);
                    (before.min(tr.duration / 2.0), f.black_in.max(f.white_in))
                }
                None => (0.0, f.afade_in),
            };
            if before + after > 0.0 {
                let dissolve = tr.kind.slide_from().is_none() && tr.kind.dip_color().is_none();
                if !dissolve {
                    report.info(format!(
                        "{} transitions are written as OTIO custom transitions; their kind is in metadata.kerf",
                        tr.kind.label().to_lowercase()
                    ));
                }
                children.push(json!({
                    "OTIO_SCHEMA": "Transition.1",
                    "name": tr.kind.label(),
                    "transition_type": if dissolve { "SMPTE_Dissolve" } else { "Custom_Transition" },
                    "in_offset": rt(tb, before),
                    "out_offset": rt(tb, after),
                    "metadata": {"kerf": {"kind": tr.kind, "duration": tr.duration}},
                }));
            } else {
                report.info("transitions with no source handle play as cuts and were left out");
            }
        }
        let asset = assets.iter().find(|a| a.id == clip.asset_id);
        children.push(clip_json(tb, clip, asset, report)?);
        at = at.max(clip.timeline_end());
    }
    if track.duck || (track.volume - 1.0).abs() > f32::EPSILON || track.pan.abs() > f32::EPSILON || track.solo || track.locked {
        report.info("track mixer, solo and lock settings are kept in metadata.kerf");
    }
    Ok(json!({
        "OTIO_SCHEMA": "Track.1",
        "name": track.name,
        "kind": if track.kind == StreamKind::Audio { "Audio" } else { "Video" },
        "children": children,
        "source_range": null,
        "effects": [],
        "markers": [],
        "enabled": !track.muted,
        "metadata": {"kerf": {
            "id": track.id,
            "duck": track.duck,
            "muted": track.muted,
            "solo": track.solo,
            "locked": track.locked,
            "volume": track.volume,
            "pan": track.pan,
        }},
    }))
}

pub(super) fn export(timeline: &Timeline, assets: &[Asset], opts: &InterchangeOptions, report: &mut Report) -> Result<String> {
    let tb = Timebase::new(opts.rate)?;
    let fx = transition_fx(timeline, assets);
    let mut tracks = Vec::new();
    let mut base = 0;
    for track in &timeline.tracks {
        tracks.push(track_json(&tb, track, &fx[base..base + track.clips.len()], assets, report)?);
        base += track.clips.len();
    }
    if !timeline.overlays.is_empty() {
        report.info(format!(
            "{} titles and captions have no OTIO equivalent; they are kept in metadata.kerf",
            timeline.overlays.len()
        ));
    }
    if !timeline.svg_overlays.is_empty() {
        report.info(format!(
            "{} animated titles have no OTIO equivalent; they are kept in metadata.kerf",
            timeline.svg_overlays.len()
        ));
    }
    let doc = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": one_line(&opts.title),
        "global_start_time": null,
        "metadata": {"kerf": {
            "overlays": timeline.overlays,
            "svg_overlays": timeline.svg_overlays,
            "format": timeline.format,
            "master": timeline.master,
            "rate": opts.rate,
        }},
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "children": tracks,
            "source_range": null,
            "effects": [],
            "markers": timeline.markers.iter().map(|m| marker_json(&tb, m)).collect::<Vec<_>>(),
            "enabled": true,
            "metadata": {},
        },
    });
    let mut text = serde_json::to_string_pretty(&doc).map_err(|e| Error::Other(e.to_string()))?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_colours_map_to_otio_names() {
        assert_eq!(marker_color(None), "RED");
        assert_eq!(marker_color(Some("cyan")), "CYAN");
        assert_eq!(marker_color(Some("#00f")), "BLUE");
        assert_eq!(marker_color(Some("#22c55e")), "GREEN");
        assert_eq!(marker_color(Some("#f59e0b")), "ORANGE");
        assert_eq!(marker_color(Some("#a855f7")), "PURPLE");
        assert_eq!(marker_color(Some("#ffffff")), "WHITE");
        assert_eq!(marker_color(Some("#101010")), "BLACK");
        assert_eq!(marker_color(Some("var(--x)")), "RED");
    }
}
