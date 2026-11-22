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

fn sampled(start: f64, weight: f64, video: f64, key: f64, audio: f64) -> Sampled {
    let stats = |first, rate| StreamStats {
        first_bytes: first,
        rate,
        total_bytes: rate,
    };
    Sampled {
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
    let samples: Vec<Sampled> = (0..5)
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
    let samples: Vec<Sampled> = rates
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
    let samples: Vec<Sampled> = (0..4)
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
    let small: Vec<Sampled> = (0..4).map(|i| sampled(i as f64 * 5.0, 5.0, 30_000.0, 100.0, 0.0)).collect();
    assert!((extrapolate(&small, 20.0, &keys).unwrap().bytes - plain).abs() < 1e-6);
}

#[test]
fn the_band_is_at_least_fifteen_percent_and_opens_with_the_spread_and_the_time() {
    // Flat footage: the floor. A spread: twice the standard error. Stopped short: opened by sqrt(planned / done).
    assert_eq!(band(0.0, 6, 6, false, false), 0.15);
    assert_eq!(band(0.05, 6, 6, false, false), 0.15);
    assert!((band(0.4, 6, 6, false, false) - 0.8).abs() < 1e-12);
    assert!((band(0.4, 3, 12, false, false) - 0.8 * 2.0).abs() < 1e-12);
    // One window alone says nothing of the rest; a whole encode is the file.
    assert_eq!(band(0.0, 1, 7, false, false), 0.5);
    assert_eq!(band(0.0, 1, 1, true, false), 0.05);
    // A hardware encoder gets twice each floor; nothing opens past 3.
    assert_eq!(band(0.0, 6, 6, false, true), 0.30);
    assert_eq!(band(0.0, 1, 1, true, true), 0.10);
    assert_eq!(band(50.0, 6, 6, false, false), 3.0);
    let (low, high) = banded(1000.0, 0.15);
    assert!((low - 850.0).abs() < 1e-9 && (high - 1150.0).abs() < 1e-9);
    // The low side never goes under a tenth.
    assert!((banded(1000.0, 2.5).0 - 100.0).abs() < 1e-9);
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

#[test]
fn the_note_says_how_it_was_measured_and_what_it_leaves_out() {
    let facts = |whole, done, planned| MeasuredFacts {
        whole,
        done,
        planned,
        sampled_secs: done as f64,
        total_secs: 60.0,
        took_secs: 3.25,
        half: 0.18,
        skips_titles: false,
    };
    let n = measured_note(facts(false, 6, 6));
    assert!(
        n.contains("6 samples of 1 s (6 s of the 60 s cut)") && n.contains("3.2 s") && n.contains("+/- 18 %"),
        "{n}"
    );
    assert!(!n.contains("time limit"), "{n}");
    let one = measured_note(facts(false, 1, 7));
    assert!(
        one.contains("1 sample of 1 s") && one.contains("stopped it after 1 of 7 samples"),
        "{one}"
    );
    let whole = measured_note(facts(true, 1, 1));
    assert!(whole.contains("whole 60 s cut") && whole.contains("a few percent"), "{whole}");
    let titles = measured_note(MeasuredFacts {
        skips_titles: true,
        ..facts(false, 6, 6)
    });
    assert!(titles.contains("Animated titles are not in the samples"), "{titles}");
}

// ---- running it (no real encode) ---------------------------------------------------------------------

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

#[test]
fn what_the_model_can_tell_exactly_is_not_measured() {
    // A bitrate target, and PCM: arithmetic. Nothing is spawned, the answer is the model's own.
    let (tl, assets, mut opts) = crf_cut(60.0);
    opts.rate_control = RateControl::Bitrate;
    opts.video_bitrate = Some("4M".into());
    let model = estimate_export(&tl, &assets, &opts, &[]);
    assert_eq!(model.files[0].estimate.basis, SizeBasis::Targeted);
    let started = Instant::now();
    let got = measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false).unwrap();
    assert_eq!(got, model);
    assert!(started.elapsed() < Duration::from_secs(1));
    // An empty cut cannot be sized, measured or not.
    let empty = timeline_of_nothing();
    let got = measure_export_size(&empty, &[], &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false).unwrap();
    assert_eq!(got.files[0].estimate.basis, SizeBasis::Unknown);
}

fn timeline_of_nothing() -> Timeline {
    crate::engine::test_support::timeline_of(vec![crate::engine::test_support::video_track(vec![])])
}

#[cfg(unix)]
#[test]
fn an_encoder_that_fails_is_the_models_estimate_with_the_reason() {
    let dir = scratch("fails");
    let bin = stand_in(&dir, "ffmpeg", "echo 'boom: no such encoder' >&2\nexit 1");
    let (tl, assets, opts) = crf_cut(60.0);
    let model = estimate_export(&tl, &assets, &opts, &[]);
    let got = with_ffmpeg(&bin, || {
        measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| false).unwrap()
    });
    let (m, g) = (&model.files[0].estimate, &got.files[0].estimate);
    assert_eq!(
        (g.basis, g.low_bytes, g.high_bytes),
        (SizeBasis::Modelled, m.low_bytes, m.high_bytes)
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
fn a_hung_encoder_is_killed_at_the_budget_and_nothing_is_left_behind() {
    let dir = scratch("hangs");
    let log = dir.join("out.log");
    // Writes where it was told to write (the last argument), then hangs.
    let body = format!(
        "for last; do :; done\necho \"$last\" > {}\n: > \"$last\"\nsleep 60",
        log.display()
    );
    let bin = stand_in(&dir, "ffmpeg", &body);
    let (tl, assets, opts) = crf_cut(60.0);
    let started = Instant::now();
    let got = with_ffmpeg(&bin, || {
        measure_export_size(&tl, &assets, &opts, &[], Duration::from_secs(1), &mut never, &|| false).unwrap()
    });
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(10),
        "killed at the budget plus its grace, not at 60 s: {took:?}"
    );
    let e = &got.files[0].estimate;
    assert_eq!(e.basis, SizeBasis::Modelled);
    assert!(e.note.contains("time limit"), "{}", e.note);
    // The file it was writing, and the folder, are gone.
    let out = std::fs::read_to_string(&log).expect("the stand-in ran").trim().to_string();
    assert!(!Path::new(&out).exists(), "{out}");
    assert!(!Path::new(&out).parent().unwrap().exists(), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_cancel_stops_a_running_sample_and_is_not_an_estimate() {
    let dir = scratch("cancels");
    let bin = stand_in(&dir, "ffmpeg", "sleep 60");
    let (tl, assets, opts) = crf_cut(60.0);
    let started = Instant::now();
    let asked = std::cell::Cell::new(0u32);
    let cancel = || {
        // True from the second look: the run is already under way.
        asked.set(asked.get() + 1);
        asked.get() > 2
    };
    let got = with_ffmpeg(&bin, || {
        measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &cancel)
    });
    assert!(matches!(got, Err(Error::Cancelled)), "{got:?}");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cancel_before_the_first_sample_never_spawns() {
    let (tl, assets, opts) = crf_cut(60.0);
    let got = measure_export_size(&tl, &assets, &opts, &[], DEFAULT_MEASURE_BUDGET, &mut never, &|| true);
    assert!(matches!(got, Err(Error::Cancelled)), "{got:?}");
}

#[test]
fn the_shape_to_measure_is_the_one_with_the_most_pixels_and_the_others_are_scaled() {
    let (tl, assets, opts) = crf_cut(60.0);
    let tall = Delivery::new(1080, 1920, Fit::Cover);
    let square = Delivery::new(1080, 1080, Fit::Cover);
    let wide = Delivery::new(1920, 1080, Fit::Contain);
    let model = estimate_export(&tl, &assets, &opts, &[square, tall, wide]);
    // 1080x1920 and 1920x1080 tie on pixels: the first of them is the one measured.
    assert_eq!(reference_file(&model.files), Some(1));
    let measured = SizeEstimate {
        basis: SizeBasis::Measured,
        low_bytes: 8_000_000,
        high_bytes: 12_000_000,
        note: "Measured.".into(),
    };
    let scaled = scale_from(
        &measured,
        &model.files[1].estimate,
        &model.files[0].estimate,
        model.files[1].frame,
    );
    assert_eq!(scaled.basis, SizeBasis::Measured);
    // 1080x1080 has 0.56 of the pixels: smaller, and with room on both sides.
    let ratio = (model.files[0].estimate.low_bytes as f64 * model.files[0].estimate.high_bytes as f64).sqrt()
        / (model.files[1].estimate.low_bytes as f64 * model.files[1].estimate.high_bytes as f64).sqrt();
    assert!(ratio < 0.85 && ratio > 0.4, "{ratio}");
    assert_eq!(scaled.low_bytes, (8_000_000.0 * ratio / 1.3) as u64);
    assert_eq!(scaled.high_bytes, (12_000_000.0 * ratio * 1.3) as u64);
    assert!(
        scaled.note.starts_with("Measured.") && scaled.note.contains("Scaled from the 1080x1920 file"),
        "{}",
        scaled.note
    );
    // Nothing modelled to measure: no reference.
    let bitrate = ExportOptions {
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("4M".into()),
        ..opts
    };
    assert_eq!(
        reference_file(&estimate_export(&tl, &assets, &bitrate, &[tall, wide]).files),
        None
    );
}
