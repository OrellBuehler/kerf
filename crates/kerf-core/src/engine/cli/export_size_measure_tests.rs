//! The measured size estimate, held to its own arithmetic and its failure paths (no real encode:
//! `export_size_measure_rendered` holds the bands to real files).

use std::time::{Duration, Instant};

use uuid::Uuid;

use super::export_size_measure::*;
use super::*;
use crate::engine::test_support::{av_asset, make_clip, single};

fn span(start: f64, end: f64, source: Uuid) -> Span {
    Span { start, end, source }
}

// ---- the plan ------------------------------------------------------------------------------------

#[test]
fn windows_are_spread_over_the_span_and_speak_for_all_of_it() {
    let a = Uuid::new_v4();
    let spans = [span(0.0, 60.0, a)];
    let n = planned_windows(60.0);
    let windows = plan_windows(0.0, 60.0, &spans, n, WINDOW_SECS);
    assert_eq!(windows.len(), n);
    assert!(windows.windows(2).all(|w| w[0].start < w[1].start), "in time order");
    assert!(windows
        .iter()
        .all(|w| w.len == WINDOW_SECS && w.start >= 0.0 && w.start + w.len <= 60.0));
    let total: f64 = windows.iter().map(|w| w.weight).sum();
    assert!((total - 60.0).abs() < 1e-9, "{total}");
    // Evenly spaced: each speaks for an equal share.
    for w in &windows {
        assert!((w.weight - 60.0 / n as f64).abs() < 1e-9, "{w:?}");
    }
    // Deterministic.
    assert_eq!(windows, plan_windows(0.0, 60.0, &spans, n, WINDOW_SECS));
}

#[test]
fn a_short_span_is_one_whole_window_and_a_degenerate_one_is_none() {
    let spans = [span(0.0, 10.0, Uuid::new_v4())];
    for (start, end) in [(0.0, 4.0), (0.0, 5.0), (2.0, 6.5)] {
        let w = plan_windows(start, end, &spans, 3, WINDOW_SECS);
        assert_eq!(
            w,
            vec![Window {
                start,
                len: end - start,
                weight: end - start
            }],
            "{start}..{end}"
        );
    }
    // Windows that would cover half of it are the whole of it.
    assert_eq!(plan_windows(0.0, 6.0, &spans, 3, 1.0).len(), 1);
    assert!(plan_windows(0.0, 7.0, &spans, 3, 1.0).len() == 3);
    for (start, end) in [(3.0, 3.0), (5.0, 2.0), (0.0, f64::NAN)] {
        assert!(plan_windows(start, end, &spans, 3, 1.0).is_empty(), "{start}..{end}");
    }
}

#[test]
fn a_window_lies_inside_the_clip_it_lands_on() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    // Two clips, the boundary at 31: a window centred at 30.9 would straddle it.
    let spans = [span(0.0, 31.0, a), span(31.0, 60.0, b)];
    let windows = plan_windows(0.0, 60.0, &spans, 5, 1.0);
    for w in &windows {
        let c = w.start + w.len / 2.0;
        let clip = spans.iter().find(|s| s.start <= c && c < s.end).unwrap();
        assert!(
            w.start >= clip.start - 1e-9 && w.start + w.len <= clip.end + 1e-9,
            "{w:?} in {clip:?}"
        );
    }
}

#[test]
fn every_footage_gets_a_window_while_another_has_two() {
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    // Four evenly spaced centres land in `a` (7.5, 22.5, 37.5) and `b` (52.5); `c`, the last five seconds, none.
    let spans = [span(0.0, 50.0, a), span(50.0, 55.0, b), span(55.0, 60.0, c)];
    let windows = plan_windows(0.0, 60.0, &spans, 4, 1.0);
    assert_eq!(windows.len(), 4);
    let source_of = |w: &Window| {
        spans
            .iter()
            .find(|s| s.start <= w.start + 0.5 && w.start + 0.5 < s.end)
            .unwrap()
            .source
    };
    let sources: Vec<Uuid> = windows.iter().map(source_of).collect();
    for (id, name) in [(a, "a"), (b, "b"), (c, "c")] {
        assert!(sources.contains(&id), "{name} has a window: {windows:?}");
    }
    // The window given up is the one of `a` nearest `c`'s clip, and the weights still add up.
    assert_eq!(sources.iter().filter(|s| **s == a).count(), 2);
    let total: f64 = windows.iter().map(|w| w.weight).sum();
    assert!((total - 60.0).abs() < 1e-9);
    // With no window to spare for the footage that has none, nobody is moved.
    let many = [span(0.0, 20.0, a), span(20.0, 40.0, b), span(40.0, 60.0, c)];
    let two = plan_windows(0.0, 60.0, &many, 3, 1.0);
    let ids: Vec<Uuid> = two
        .iter()
        .map(|w| {
            many.iter()
                .find(|s| s.start <= w.start + 0.5 && w.start + 0.5 < s.end)
                .unwrap()
                .source
        })
        .collect();
    assert_eq!(ids, vec![a, b, c]);
}

#[test]
fn a_window_in_a_gap_is_left_where_the_even_spacing_put_it() {
    let a = Uuid::new_v4();
    // Footage for the first 10 s of a minute: black after.
    let spans = [span(0.0, 10.0, a)];
    let windows = plan_windows(0.0, 60.0, &spans, 6, 1.0);
    let in_gap = windows.iter().filter(|w| w.start >= 10.0).count();
    assert!(in_gap >= 4, "{windows:?}");
}

#[test]
fn the_spread_order_is_the_middle_then_each_half() {
    for n in 0..=24 {
        let mut sorted = spread_order(n);
        sorted.sort_unstable();
        assert_eq!(sorted, (0..n).collect::<Vec<_>>(), "a permutation, n = {n}");
    }
    assert_eq!(spread_order(5), vec![2, 1, 4, 0, 3]);
    assert_eq!(spread_order(1), vec![0]);
    // Any prefix is spread: the first three of seven span the cut.
    let order = spread_order(7);
    let first: Vec<usize> = order[..3].to_vec();
    assert!(
        first.contains(&3) && first.iter().any(|i| *i < 3) && first.iter().any(|i| *i > 3),
        "{first:?}"
    );
}

#[test]
fn a_longer_span_asks_for_more_windows_up_to_a_limit() {
    assert_eq!(planned_windows(0.0), 3);
    assert_eq!(planned_windows(14.9), 3);
    assert_eq!(planned_windows(15.0), 4);
    assert_eq!(planned_windows(60.0), 7);
    assert_eq!(planned_windows(10_000.0), 12);
    assert_eq!(planned_windows(f64::NAN), 3);
}

// ---- reading a window ----------------------------------------------------------------------------

#[test]
fn a_stream_leaves_its_first_packet_out_of_its_rate() {
    let frame = 1.0 / 30.0;
    let packets = [
        (1000.0, Some(frame)),
        (100.0, Some(frame)),
        (100.0, Some(frame)),
        (100.0, Some(frame)),
    ];
    let s = stream_stats(&packets, frame).unwrap();
    assert_eq!(s.first_bytes, 1000.0);
    assert!((s.rate - 3000.0).abs() < 1e-6, "{}", s.rate);
    assert_eq!(s.total_bytes, 1300.0);
    // No duration stated: the fallback lasts each packet.
    let bare = [(1000.0, None), (100.0, None), (100.0, None)];
    assert!((stream_stats(&bare, 0.5).unwrap().rate - 200.0).abs() < 1e-9);
    // A single packet is all there is.
    let one = stream_stats(&[(500.0, Some(0.5))], 0.1).unwrap();
    assert_eq!((one.first_bytes, one.rate, one.total_bytes), (500.0, 1000.0, 500.0));
    assert!(stream_stats(&[], 0.1).is_none());
}

#[test]
fn ffprobes_packets_are_read_per_stream() {
    let json = br#"{"streams":[{"index":0,"codec_type":"video"},{"index":1,"codec_type":"audio"}],
        "packets":[
          {"stream_index":0,"duration_time":"0.033333","size":"9000"},
          {"stream_index":1,"duration_time":"0.021333","size":"400"},
          {"stream_index":0,"duration_time":"0.033333","size":"300"},
          {"stream_index":1,"duration_time":"0.021333","size":"380"},
          {"stream_index":0,"duration_time":"0.033333","size":"330"},
          {"stream_index":1,"duration_time":"0.021333","size":"420"}]}"#;
    let stats = window_stats(json, 1.0 / 30.0, 1.0).unwrap();
    let v = stats.video.unwrap();
    assert_eq!((v.first_bytes, v.total_bytes), (9000.0, 9630.0));
    assert!((v.rate - 630.0 / (2.0 / 30.0 * 1.000003)).abs() < 5.0, "{}", v.rate);
    let a = stats.audio.unwrap();
    assert_eq!(a.first_bytes, 400.0);
    assert!((a.rate - 800.0 / 0.042666).abs() < 1.0, "{}", a.rate);
    // A sound-only file, a picture-only file, an empty one and garbage.
    let sound = br#"{"streams":[{"index":0,"codec_type":"audio"}],"packets":[{"stream_index":0,"size":"10"},{"stream_index":0,"size":"10"}]}"#;
    let s = window_stats(sound, 0.033, 2.0).unwrap();
    assert!(s.video.is_none() && s.audio.is_some());
    assert_eq!(window_stats(b"{}", 0.033, 1.0).unwrap(), WindowStats::default());
    assert!(window_stats(b"not json", 0.033, 1.0).is_err());
}

// ---- putting them together -------------------------------------------------------------------------

fn sampled(start: f64, weight: f64, video: f64, key: f64, audio: f64) -> Sample {
    let stats = |first, rate| StreamStats {
        first_bytes: first,
        rate,
        total_bytes: rate,
    };
    Sample {
        start,
        len: 1.0,
        weight,
        stats: WindowStats {
            video: Some(stats(key, video)),
            audio: (audio > 0.0).then(|| stats(100.0, audio)),
        },
    }
}

const NO_KEYS: Keyframes = Keyframes {
    frames: 0.0,
    interval: None,
    cuts: 0,
    fps: 30.0,
};

#[test]
fn flat_windows_extrapolate_to_their_rate_with_no_spread() {
    let samples: Vec<Sample> = (0..5)
        .map(|i| sampled(i as f64 * 12.0, 12.0, 10_000.0, 0.0, 2_000.0))
        .collect();
    let e = extrapolate(&samples, 60.0, &NO_KEYS).unwrap();
    assert!((e.bytes - 12_000.0 * 60.0).abs() < 1e-6);
    assert_eq!(e.relative_se, 0.0);
    // The weights are renormalised over the windows that were encoded: three of the five say the same.
    let e = extrapolate(&samples[..3], 60.0, &NO_KEYS).unwrap();
    assert!((e.bytes - 12_000.0 * 60.0).abs() < 1e-6);
    assert!(extrapolate(&[], 60.0, &NO_KEYS).is_none());
}

#[test]
fn windows_are_weighed_by_what_they_speak_for() {
    // A clip a tenth of the cut, ten times as costly as the rest: weighed 1:9 it is 1 + 0.9 of the rate, not half.
    let samples = [
        sampled(0.0, 6.0, 100_000.0, 0.0, 0.0),
        sampled(30.0, 54.0, 10_000.0, 0.0, 0.0),
    ];
    let e = extrapolate(&samples, 60.0, &NO_KEYS).unwrap();
    assert!((e.bytes - (100_000.0 * 6.0 + 10_000.0 * 54.0)).abs() < 1e-6, "{}", e.bytes);
}

#[test]
fn the_spread_between_windows_is_the_successive_difference_error_of_the_mean() {
    // Rates 1, 1, 7, 40, 40 (a soft card, plain, grainy sections): Wolter's variance of the mean.
    let rates = [1.0, 1.0, 7.0, 40.0, 40.0];
    let samples: Vec<Sample> = rates
        .iter()
        .enumerate()
        .map(|(i, r)| sampled(i as f64 * 12.0, 12.0, r * 1000.0, 0.0, 0.0))
        .collect();
    let e = extrapolate(&samples, 60.0, &NO_KEYS).unwrap();
    let mean = rates.iter().sum::<f64>() / 5.0 * 1000.0;
    let squares: f64 = rates.windows(2).map(|p| ((p[1] - p[0]) * 1000.0).powi(2)).sum();
    let se = (squares / (2.0 * 5.0 * 4.0)).sqrt();
    assert!((e.relative_se - se / mean).abs() < 1e-9, "{} vs {}", e.relative_se, se / mean);
    // Out of order in, in time order out: the same.
    let mut shuffled = samples;
    shuffled.reverse();
    assert_eq!(extrapolate(&shuffled, 60.0, &NO_KEYS).unwrap(), e);
}

#[test]
fn the_periodic_keyframes_of_the_whole_file_are_added_back() {
    // 30 fps, 250-frame interval, 600 frames, one cut: 3.4 keyframes, each 30 000 over a 1 000-byte picture.
    let keys = Keyframes {
        frames: 600.0,
        interval: Some(250.0),
        cuts: 1,
        fps: 30.0,
    };
    let samples: Vec<Sample> = (0..4)
        .map(|i| sampled(i as f64 * 5.0, 5.0, 30_000.0, 31_000.0, 0.0))
        .collect();
    let e = extrapolate(&samples, 20.0, &keys).unwrap();
    let plain = 30_000.0 * 20.0;
    let extra = (600.0 / 250.0 + 1.0) * (31_000.0 - 1_000.0);
    assert!((e.bytes - plain - extra).abs() < 1e-6, "{} {}", e.bytes - plain, extra);
    // An encoder that codes every picture alone has none to add.
    let intra = Keyframes { interval: None, ..keys };
    assert!((extrapolate(&samples, 20.0, &intra).unwrap().bytes - plain).abs() < 1e-6);
    // A keyframe smaller than a picture adds nothing (it never subtracts).
    let small: Vec<Sample> = (0..4).map(|i| sampled(i as f64 * 5.0, 5.0, 30_000.0, 100.0, 0.0)).collect();
    assert!((extrapolate(&small, 20.0, &keys).unwrap().bytes - plain).abs() < 1e-6);
}

#[test]
fn the_band_is_at_least_fifteen_percent_and_opens_with_the_spread_the_sample_size_and_the_time() {
    // Flat footage: the floor. A spread: Student's t for the windows there are, times the standard error. Stopped short: opened
    // by sqrt(planned / done).
    assert_eq!(band(0.0, 6, 6, false, false), 0.15);
    assert_eq!(band(0.05, 6, 6, false, false), 0.15);
    assert!((band(0.4, 6, 6, false, false) - 2.571 * 0.4).abs() < 1e-12, "t(5) = 2.571");
    // Two windows say little: t(1) = 12.7, so it opens to the cap; seven, t(6) = 2.447.
    assert_eq!(band(0.4, 2, 2, false, false), 3.0);
    assert!((band(0.1, 7, 7, false, false) - 2.447 * 0.1).abs() < 1e-12 || band(0.1, 7, 7, false, false) == 0.15);
    assert!((band(0.3, 7, 7, false, false) - 2.447 * 0.3).abs() < 1e-12);
    assert!(
        (band(0.2, 3, 12, false, false) - 4.303 * 0.2 * 2.0).abs() < 1e-12,
        "stopped at a quarter: sqrt(4)"
    );
    // One window alone says nothing of the rest; a whole encode is the file.
    assert_eq!(band(0.0, 1, 7, false, false), 0.5);
    assert_eq!(band(0.0, 1, 1, true, false), 0.05);
    // A hardware encoder gets twice each floor; nothing opens past 3.
    assert_eq!(band(0.0, 6, 6, false, true), 0.30);
    assert_eq!(band(0.0, 1, 1, true, true), 0.10);
    assert_eq!(band(50.0, 6, 6, false, false), 3.0);
}

#[test]
fn students_t_is_the_two_sided_ninety_five_percent_value() {
    let known = [
        (1, 12.706),
        (2, 4.303),
        (3, 3.182),
        (5, 2.571),
        (6, 2.447),
        (10, 2.228),
        (12, 2.179),
    ];
    for (df, t) in known {
        assert_eq!(student_t95(df), t, "df {df}");
    }
    assert_eq!(student_t95(0), 12.706);
    assert_eq!(student_t95(13), 1.96);
    assert!((1..=12).all(|d| student_t95(d) > student_t95(d + 1)), "falls with the sample");
}

#[test]
fn a_band_is_wider_over_than_under_unless_the_whole_cut_was_encoded() {
    // A burst the windows missed costs more than a lull they missed saves.
    let (low, high) = banded(1000.0, 0.2, false);
    assert!((low - 800.0).abs() < 1e-9 && (high - 1300.0).abs() < 1e-9, "{low} {high}");
    assert!(high - 1000.0 > 1000.0 - low);
    // A whole encode has nothing missed.
    let (low, high) = banded(1000.0, 0.05, true);
    assert!((low - 950.0).abs() < 1e-9 && (high - 1050.0).abs() < 1e-9);
    // The low side never goes under a tenth.
    assert!((banded(1000.0, 2.5, false).0 - 100.0).abs() < 1e-9);
}

#[test]
fn an_encoders_default_keyframe_interval_is_the_one_measured() {
    let opts = |codec: &str| ExportOptions {
        video_codec: Some(codec.into()),
        ..ExportOptions::default()
    };
    assert_eq!(keyframe_interval(&opts("libx264"), 30.0), Some(250.0));
    assert_eq!(keyframe_interval(&opts("libx265"), 30.0), Some(250.0));
    assert_eq!(keyframe_interval(&opts("libvpx-vp9"), 30.0), Some(128.0));
    assert_eq!(keyframe_interval(&opts("libsvtav1"), 30.0), Some(161.0));
    assert_eq!(keyframe_interval(&opts("h264_nvenc"), 30.0), Some(250.0));
    assert_eq!(keyframe_interval(&opts("prores_ks"), 30.0), None);
    assert_eq!(keyframe_interval(&opts("gif"), 30.0), None);
    assert_eq!(
        keyframe_interval(&ExportOptions::default(), 30.0),
        Some(250.0),
        "mp4's own x264"
    );
}

fn facts(whole: bool, done: usize, planned: usize) -> MeasuredFacts {
    MeasuredFacts {
        whole,
        done,
        planned,
        sampled_secs: done as f64,
        total_secs: 60.0,
        took_secs: 3.25,
        half: 0.18,
        left_out: LeftOut::default(),
    }
}

#[test]
fn the_note_says_what_it_holds_if_and_that_a_missed_stretch_makes_it_larger() {
    let n = measured_note(facts(false, 7, 7));
    assert!(
        n.contains("7 samples of 1 s (7 of 60 s of the cut)") && n.contains("3.2 s"),
        "{n}"
    );
    assert!(n.contains("-18 % to +27 %"), "the band is wider over than under: {n}");
    assert!(n.contains("holds if the sampled seconds are typical of the cut"), "{n}");
    assert!(
        n.contains("a busy stretch the samples missed would make the file larger"),
        "{n}"
    );
    assert!(!n.contains("time limit") && !n.contains("Left out"), "{n}");
    let one = measured_note(facts(false, 1, 7));
    assert!(
        one.contains("1 sample of 1 s") && one.contains("stopped it after 1 of 7 samples"),
        "{one}"
    );
    let whole = measured_note(facts(true, 1, 1));
    assert!(whole.contains("whole 60 s cut") && whole.contains("a few percent"), "{whole}");
    assert!(!whole.contains("typical"), "a whole encode needs no such caveat: {whole}");
}

#[test]
fn the_note_names_everything_the_samples_leave_out_when_the_cut_has_it() {
    let all = LeftOut {
        titles: true,
        muxed_captions: true,
        chapters: true,
        loudness: true,
    };
    assert_eq!(
        all.sentence().as_deref(),
        Some("Left out of the samples: title clips and SVG overlays, captions muxed in, chapters and loudness normalisation.")
    );
    assert_eq!(
        LeftOut {
            loudness: true,
            ..LeftOut::default()
        }
        .sentence()
        .as_deref(),
        Some("Left out of the samples: loudness normalisation.")
    );
    assert_eq!(
        LeftOut {
            titles: true,
            chapters: true,
            ..LeftOut::default()
        }
        .sentence()
        .as_deref(),
        Some("Left out of the samples: title clips and SVG overlays and chapters.")
    );
    assert_eq!(LeftOut::default().sentence(), None);
    let note = measured_note(MeasuredFacts {
        left_out: all,
        ..facts(false, 7, 7)
    });
    assert!(note.ends_with("and loudness normalisation."), "{note}");
}

fn overlay(text: &str, generated: bool) -> crate::model::TextOverlay {
    crate::model::TextOverlay {
        generated,
        ..crate::model::TextOverlay::new(text, 1.0, 3.0)
    }
}

#[test]
fn the_samples_view_drops_titles_and_keeps_every_picture_the_export_draws() {
    let asset = av_asset(Uuid::new_v4(), 60.0);
    let mut cut = single(vec![make_clip(asset.id, 0.0, 30.0, 0.0)]);
    // Burned captions (generated overlays) and a typed title are drawn by the graph itself: they stay, as the real export has them.
    cut.overlays.push(overlay("a caption", true));
    cut.overlays.push(overlay("a typed title", false));
    let opts = ExportOptions::default();
    let (view, left_out) = sample_view(&cut, &opts);
    assert_eq!(left_out, LeftOut::default(), "nothing to say when the cut has none of it");
    assert_eq!(view.overlays.len(), 2);
    assert_eq!(view.tracks[0].clips.len(), 1);
    // A title clip and an SVG overlay are left out, said, and gone from the view.
    let mut titled = cut.clone();
    titled.svg_overlays.push(crate::model::SvgOverlay {
        id: Uuid::new_v4(),
        name: String::new(),
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\"/>".into(),
        start: 0.0,
        end: 2.0,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        opacity: 1.0,
        offset: 0.0,
        preset: None,
        template: None,
    });
    let (view, left_out) = sample_view(&titled, &opts);
    assert!(left_out.titles && !left_out.muxed_captions && !left_out.chapters && !left_out.loudness);
    assert!(view.svg_overlays.is_empty());
    assert_eq!(view.overlays.len(), 2, "burned captions stay");
    // What the options carry: captions muxed in, loudness, chapters.
    let asked = ExportOptions {
        embed_captions: true,
        loudness: Some(LoudnessPreset::Youtube),
        ..opts.clone()
    };
    let (_, left_out) = sample_view(&cut, &asked);
    assert!(left_out.muxed_captions && left_out.loudness && !left_out.chapters);
    assert!(
        sample_view(
            &cut,
            &ExportOptions {
                loudnorm: true,
                ..opts.clone()
            }
        )
        .1
        .loudness
    );
    // A container with no subtitle track mutes nothing (the export refuses it); a chapter marker in a container that carries them is named.
    let mut chaptered = cut.clone();
    chaptered.markers.push(crate::model::Marker {
        kind: crate::model::MarkerKind::Chapter,
        ..crate::model::Marker::default()
    });
    assert!(sample_view(&chaptered, &opts).1.chapters);
    assert!(
        !sample_view(
            &chaptered,
            &ExportOptions {
                container: Container::Gif,
                ..opts
            }
        )
        .1
        .chapters
    );
}

#[test]
fn the_options_of_a_sample_are_the_exports_without_what_is_not_the_size() {
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        crf: Some(20),
        skip_burned_captions: true,
        range: Some(TimeRange { start: 3.0, end: 9.0 }),
        loudness: Some(LoudnessPreset::Youtube),
        loudness_static: true,
        loudness_gain_db: Some(4.0),
        loudnorm: true,
        faststart: true,
        embed_captions: true,
        caption_sidecars: vec![crate::captions_export::SubtitleFormat::Srt],
        overwrite_sidecars: true,
        metadata_title: Some("t".into()),
        ..ExportOptions::default()
    };
    let s = sample_options(&opts);
    assert_eq!(
        (s.video_codec.as_deref(), s.crf),
        (Some("libx264"), Some(20)),
        "the encoder is the export's own"
    );
    assert!(s.skip_burned_captions, "captions are burned as the export burns them");
    assert!(s.range.is_none() && s.loudness.is_none() && !s.loudness_static && s.loudness_gain_db.is_none() && !s.loudnorm);
    assert!(
        !s.faststart && !s.embed_captions && s.caption_sidecars.is_empty() && !s.overwrite_sidecars && s.metadata_title.is_none()
    );
}

// ---- several deliveries ----------------------------------------------------------------------------

fn crf_cut(secs: f64) -> (Timeline, Vec<Asset>, ExportOptions) {
    let asset = av_asset(Uuid::new_v4(), secs + 10.0);
    let tl = single(vec![make_clip(asset.id, 0.0, secs, 0.0)]);
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        crf: Some(23),
        include_audio: false,
        ..ExportOptions::default()
    };
    (tl, vec![asset], opts)
}

#[test]
fn shapes_share_a_measurement_only_when_they_share_a_fit_and_an_aspect() {
    let (tl, assets, opts) = crf_cut(60.0);
    let tall = Delivery::new(1080, 1920, Fit::Cover);
    let tall_small = Delivery::new(720, 1280, Fit::Cover);
    let tall_bars = Delivery::new(1080, 1920, Fit::Contain);
    let tall_blur = Delivery::new(1080, 1920, Fit::Blur);
    let square = Delivery::new(1080, 1080, Fit::Cover);
    let wide = Delivery::new(1920, 1080, Fit::Contain);
    let fast = Delivery::new(1080, 1920, Fit::Cover).with_fps(Some(60.0)).unwrap();
    let model = estimate_export(
        &tl,
        &assets,
        &opts,
        &[tall, tall_small, tall_bars, tall_blur, square, wide, fast],
    );
    let groups = measure_groups(&model.files);
    // 1080x1920 and 720x1280 cover are one picture at two sizes; the same pixels contained with bars, blur-filled, a square, a landscape and a
    // 60 fps one are each another picture.
    assert_eq!(groups, vec![vec![0, 1], vec![2], vec![3], vec![4], vec![5], vec![6]]);
    assert_eq!(
        reference_of(&model.files, &groups[0]),
        0,
        "the larger of the pair is the one encoded"
    );
    assert_eq!(reference_of(&model.files, &[1, 0]), 0);
    // No deliveries: one file, one group. Nothing modelled: none.
    assert_eq!(
        measure_groups(&estimate_export(&tl, &assets, &opts, &[]).files),
        vec![vec![0]]
    );
    let bitrate = ExportOptions {
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("4M".into()),
        ..opts
    };
    assert!(measure_groups(&estimate_export(&tl, &assets, &bitrate, &[tall, wide]).files).is_empty());
}

fn measured(low: u64, high: u64) -> SizeEstimate {
    SizeEstimate {
        basis: SizeBasis::Measured,
        low_bytes: low,
        high_bytes: high,
        floor_bytes: low / 2,
        note: "Measured.".into(),
        sampled: Some(Sampled {
            sampled_secs: 7.0,
            total_secs: 60.0,
            whole: false,
        }),
    }
}

#[test]
fn a_scaled_shape_carries_the_measurement_by_the_models_ratio_with_more_room() {
    let (tl, assets, opts) = crf_cut(60.0);
    let big = Delivery::new(1080, 1920, Fit::Cover);
    let small = Delivery::new(720, 1280, Fit::Cover);
    let model = estimate_export(&tl, &assets, &opts, &[big, small]);
    let (a, b) = (&model.files[0], &model.files[1]);
    let scaled = scale_from(&measured(8_000_000, 12_000_000), &a.estimate, &b.estimate, a.frame);
    let middle = |e: &SizeEstimate| (e.low_bytes as f64 * e.high_bytes as f64).sqrt();
    let ratio = middle(&b.estimate) / middle(&a.estimate);
    assert!(ratio < 0.7 && ratio > 0.3, "720x1280 is 0.44 of the pixels: {ratio}");
    assert_eq!(scaled.basis, SizeBasis::Measured);
    assert_eq!(scaled.low_bytes, (8_000_000.0 * ratio / 1.3) as u64);
    assert_eq!(scaled.high_bytes, (12_000_000.0 * ratio * 1.3) as u64);
    assert_eq!(
        scaled.floor_bytes,
        b.estimate.low_bytes.min(scaled.low_bytes),
        "the model's floor, under the scaled low end"
    );
    assert!(
        scaled.note.starts_with("Measured.")
            && scaled
                .note
                .contains("Scaled from the 1080x1920 file (the same fit and shape)"),
        "{}",
        scaled.note
    );
    assert!(!scaled.sampled.unwrap().whole);
}

// ---- running it (stand-in binaries, no real encode) ----------------------------------------------------

/// Tests that run a measurement take turns: a measurement that starts supersedes the one before it, and these run in one process.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

fn never(_: SizeMeasureProgress) {}

#[cfg(unix)]
fn stand_in(dir: &Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-measure-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether process `pid` is still there (not merely a zombie left unreaped by whoever started it).
#[cfg(unix)]
fn alive(pid: &str) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        return !stat.contains(") Z");
    }
    if Path::new("/proc/self").exists() {
        return false;
    }
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn what_the_model_can_tell_exactly_is_not_measured() {
    let _turn = one_at_a_time();
    // A bitrate target is arithmetic: no sample encode. The answer is the model's own.
    let (tl, assets, mut opts) = crf_cut(60.0);
    opts.rate_control = RateControl::Bitrate;
    opts.video_bitrate = Some("4M".into());
    let model = estimate_export(&tl, &assets, &opts, &[]);
    assert_eq!(model.files[0].estimate.basis, SizeBasis::Targeted);
    let started = Instant::now();
    let got = measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false).unwrap();
    assert_eq!(got, model);
    assert!(started.elapsed() < Duration::from_secs(5));
    // An empty cut cannot be sized, measured or not.
    let empty = crate::engine::test_support::timeline_of(vec![crate::engine::test_support::video_track(vec![])]);
    let got = measure_export_size(&empty, &[], &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false).unwrap();
    assert_eq!(got.files[0].estimate.basis, SizeBasis::Unknown);
}

#[test]
fn a_cancel_before_the_first_sample_never_spawns_and_does_not_supersede() {
    let _turn = one_at_a_time();
    let (tl, assets, opts) = crf_cut(60.0);
    let before = latest_measurement();
    let got = measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| true);
    assert!(matches!(got, Err(Error::Cancelled)), "{got:?}");
    assert_eq!(
        latest_measurement(),
        before,
        "a call that never started does not take another's place"
    );
}

#[test]
fn a_budget_that_cannot_be_added_to_the_clock_is_clamped_and_still_works() {
    let _turn = one_at_a_time();
    let (tl, assets, mut opts) = crf_cut(60.0);
    opts.rate_control = RateControl::Bitrate;
    opts.video_bitrate = Some("4M".into());
    for budget in [Duration::MAX, Duration::from_secs(u64::MAX / 2), Duration::ZERO] {
        assert!(
            measure_export_size(&tl, &assets, &opts, &[], budget, &mut never, &|| false).is_ok(),
            "{budget:?}"
        );
    }
    assert_eq!(MAX_MEASURE_BUDGET, Duration::from_secs(600));
}

#[cfg(unix)]
#[test]
fn an_encoder_that_fails_is_the_models_estimate_with_the_reason() {
    let _turn = one_at_a_time();
    let dir = scratch("fails");
    let bin = stand_in(&dir, "ffmpeg", "echo 'boom: no such encoder' >&2\nexit 1");
    let (tl, assets, opts) = crf_cut(60.0);
    let model = estimate_export(&tl, &assets, &opts, &[]);
    let got = with_ffmpeg(&bin, || {
        measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false).unwrap()
    });
    let (m, g) = (&model.files[0].estimate, &got.files[0].estimate);
    assert_eq!(
        (g.basis, g.low_bytes, g.high_bytes, g.floor_bytes),
        (SizeBasis::Modelled, m.low_bytes, m.high_bytes, m.floor_bytes)
    );
    assert!(
        g.note.starts_with(&m.note) && g.note.contains("could not be made") && g.note.contains("boom"),
        "{}",
        g.note
    );
    assert_eq!(got.total, model.total);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_hung_encoder_is_killed_at_the_budget_and_really_gone_and_nothing_is_left_behind() {
    let _turn = one_at_a_time();
    let dir = scratch("hangs");
    let log = dir.join("out.log");
    let pid = dir.join("pid");
    // Writes where it was told to write (the last argument), says who it is, and hangs as the process it is.
    let body = format!(
        "for last; do :; done\necho \"$last\" > {}\necho $$ > {}\n: > \"$last\"\nexec sleep 60",
        log.display(),
        pid.display()
    );
    let bin = stand_in(&dir, "ffmpeg", &body);
    let (tl, assets, opts) = crf_cut(60.0);
    let started = Instant::now();
    let got = with_ffmpeg(&bin, || {
        measure_export_size(&tl, &assets, &opts, &[], Duration::from_secs(1), &mut never, &|| false).unwrap()
    });
    let took = started.elapsed();
    // The first window is given its budget plus the grace, three seconds at least: killed there, not at 60 s.
    assert!(took >= Duration::from_secs(2) && took < Duration::from_secs(12), "{took:?}");
    let e = &got.files[0].estimate;
    assert_eq!(e.basis, SizeBasis::Modelled);
    assert!(e.note.contains("time limit"), "{}", e.note);
    let pid = std::fs::read_to_string(&pid).expect("the stand-in ran").trim().to_string();
    assert!(!alive(&pid), "the hung encoder (pid {pid}) is gone");
    // The file it was writing, and the folder, are gone.
    let out = std::fs::read_to_string(&log).unwrap().trim().to_string();
    assert!(
        !Path::new(&out).exists() && !Path::new(&out).parent().unwrap().exists(),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_cancel_stops_a_running_sample_and_it_is_gone() {
    let _turn = one_at_a_time();
    let dir = scratch("cancels");
    let pid = dir.join("pid");
    let bin = stand_in(&dir, "ffmpeg", &format!("echo $$ > {}\nexec sleep 60", pid.display()));
    let (tl, assets, opts) = crf_cut(60.0);
    let started = Instant::now();
    // Cancel once the encode has said who it is: it is already running.
    let cancel = || pid.exists();
    let got = with_ffmpeg(&bin, || {
        measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &cancel)
    });
    assert!(matches!(got, Err(Error::Cancelled)), "{got:?}");
    assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());
    let pid = std::fs::read_to_string(&pid).expect("the stand-in ran").trim().to_string();
    assert!(!alive(&pid), "pid {pid} is gone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in `ffmpeg` that writes nothing and succeeds, beside a stand-in `ffprobe` that does `probe`.
#[cfg(unix)]
fn with_stand_ins<R>(tag: &str, probe: &str, f: impl FnOnce(&Path) -> R) -> R {
    let dir = scratch(tag);
    let ffmpeg = stand_in(&dir, "ffmpeg", "exit 0");
    let ffprobe = stand_in(&dir, "ffprobe", probe);
    let out = with_ffmpeg(&ffmpeg, || with_ffprobe(&ffprobe, || f(&dir)));
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[cfg(unix)]
#[test]
fn a_hung_ffprobe_is_killed_and_the_model_answers() {
    let _turn = one_at_a_time();
    let (tl, assets, opts) = crf_cut(60.0);
    with_stand_ins("probe-hangs", "echo $$ > \"$(dirname \"$0\")/pid\"\nexec sleep 60", |dir| {
        let started = Instant::now();
        let got = measure_export_size(&tl, &assets, &opts, &[], Duration::from_secs(1), &mut never, &|| false).unwrap();
        // The probe is held to what is left of the window's time (at least 2 s), not to its own 20.
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        let e = &got.files[0].estimate;
        assert_eq!(e.basis, SizeBasis::Modelled);
        assert!(e.note.contains("time limit"), "{}", e.note);
        let pid = std::fs::read_to_string(dir.join("pid"))
            .expect("the probe ran")
            .trim()
            .to_string();
        assert!(!alive(&pid), "the hung ffprobe (pid {pid}) is gone");
    });
}

#[cfg(unix)]
#[test]
fn a_cancel_during_the_probe_stops_it() {
    let _turn = one_at_a_time();
    let (tl, assets, opts) = crf_cut(60.0);
    with_stand_ins("probe-cancels", "echo $$ > \"$(dirname \"$0\")/pid\"\nexec sleep 60", |dir| {
        let started = Instant::now();
        let flag = dir.join("pid");
        // Cancel once the probe has said who it is.
        let cancel = || flag.exists();
        let got = measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &cancel);
        assert!(matches!(got, Err(Error::Cancelled)), "{got:?}");
        assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());
        let pid = std::fs::read_to_string(&flag).unwrap().trim().to_string();
        assert!(!alive(&pid), "pid {pid} is gone");
    });
}

#[cfg(unix)]
#[test]
fn a_newer_measurement_supersedes_an_older_one_still_running() {
    let _turn = one_at_a_time();
    let dir = scratch("latest");
    let pid = dir.join("pid");
    let hangs = stand_in(&dir, "hangs", &format!("echo $$ > {}\nexec sleep 60", pid.display()));
    let fails = stand_in(&dir, "fails", "echo nope >&2\nexit 1");
    let (tl, assets, opts) = crf_cut(60.0);
    let started = Instant::now();
    std::thread::scope(|s| {
        let older = s.spawn(|| {
            with_ffmpeg(&hangs, || {
                measure_export_size(&tl, &assets, &opts, &[], Duration::from_secs(30), &mut never, &|| false)
            })
        });
        // The older one is running when the newer one starts.
        while !pid.exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let newer = with_ffmpeg(&fails, || {
            measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false)
        });
        assert!(newer.is_ok(), "the latest runs to its own end: {newer:?}");
        let older = older.join().unwrap();
        assert!(matches!(older, Err(Error::Cancelled)), "{older:?}");
    });
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    let hung = std::fs::read_to_string(&pid).unwrap().trim().to_string();
    assert!(!alive(&hung), "the superseded encode (pid {hung}) is gone");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_sample_that_could_fill_the_temp_folder_is_not_made() {
    let _turn = one_at_a_time();
    // A year of 8K ProRes 4444 at its high end is a thousand terabytes: no window of it is encoded.
    let asset = av_asset(Uuid::new_v4(), 40_000.0);
    let tl = single(vec![make_clip(asset.id, 0.0, 30_000.0, 0.0)]);
    let opts = ExportOptions {
        container: Container::Mov,
        video_codec: Some("prores_ks".into()),
        prores_profile: Some(5),
        resolution: Some((7680, 4320)),
        include_audio: false,
        ..ExportOptions::default()
    };
    let dir = scratch("disk");
    let asked = dir.join("asked");
    let bin = stand_in(&dir, "ffmpeg", &format!("echo run >> {}\nexit 1", asked.display()));
    let got = with_ffmpeg(&bin, || {
        measure_export_size(
            &tl,
            &assets_of(&asset),
            &opts,
            &[],
            DEFAULT_MEASURE_BUDGET,
            &mut never,
            &|| false,
        )
        .unwrap()
    });
    let e = &got.files[0].estimate;
    assert_eq!(e.basis, SizeBasis::Modelled);
    assert!(e.note.contains("temp folder"), "{}", e.note);
    assert!(!asked.exists(), "nothing was spawned");
    let _ = std::fs::remove_dir_all(&dir);
}

fn assets_of(asset: &Asset) -> Vec<Asset> {
    vec![asset.clone()]
}

#[cfg(unix)]
#[test]
fn free_space_is_read_where_it_can_be() {
    let free = free_space(&std::env::temp_dir()).expect("the temp folder is on a filesystem");
    assert!(free > 0);
    assert_eq!(free_space(Path::new("/definitely/not/here")), None);
}

#[test]
fn a_hardware_encoder_is_not_sampled_beside_a_running_export() {
    let _turn = one_at_a_time();
    let _gate = crate::engine::cpu::test_lock();
    let (tl, assets, mut opts) = crf_cut(60.0);
    opts.video_codec = Some("h264_nvenc".into());
    // An export holds the gate while the measurement is asked.
    let _export = crate::engine::cpu::lease();
    assert!(crate::engine::cpu::foreground_busy());
    let model = estimate_export(&tl, &assets, &opts, &[]);
    let got = measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false).unwrap();
    let e = &got.files[0].estimate;
    assert_eq!(
        (e.basis, e.low_bytes, e.high_bytes),
        (
            SizeBasis::Modelled,
            model.files[0].estimate.low_bytes,
            model.files[0].estimate.high_bytes
        )
    );
    assert!(
        e.note.contains("not run for a sample while an export is running"),
        "{}",
        e.note
    );
}
