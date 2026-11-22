use super::*;
use crate::clip_timing::is_head_padded_proxy;
use crate::engine::test_support::{
    audio_stream, audio_track, av_asset, image_stream, img_asset, make_clip, remove_proxy, single, test_asset, timeline_of,
    video_stream, video_track, ProxyGuard, StatusBounded,
};
use crate::model::{Asset, Clip, Delivery, StreamInfo, StreamKind, Timeline, Track, TransitionKind};

/// A track mix that changes nothing — what every test that is not about the
/// mixer wants.
fn unity_mix() -> TrackMix {
    TrackMix {
        duck: false,
        volume: 1.0,
        pan: (1.0, 1.0),
    }
}
use chrono::Utc;
use uuid::Uuid;

// ---- salience sampling --------------------------------------------------

#[test]
fn salience_args_sample_the_window_at_the_analysis_grid() {
    let args = build_salience_args(Path::new("/m/a.mp4"), 12.0, 22.0);
    let joined = args.join(" ");
    assert!(joined.contains("-ss 12.000 -t 10.000 -i /m/a.mp4"), "{joined}");
    // 48 samples over 10s.
    assert!(
        joined.contains("fps=4.8000,scale=64:36:flags=bilinear,format=gray"),
        "{joined}"
    );
    assert!(joined.contains("-frames:v 48"), "{joined}");
    assert!(joined.contains("-f rawvideo -pix_fmt gray pipe:1"), "{joined}");
    assert!(joined.contains("-an"), "{joined}");
}

#[test]
fn salience_args_clamp_the_sample_rate_for_absurd_windows() {
    // A quarter-second clip must not ask for 192 fps...
    let fast = build_salience_args(Path::new("/m/a.mp4"), 0.0, 0.25).join(" ");
    assert!(fast.contains("fps=30.0000"), "{fast}");
    // ...nor an hour-long one for a frame every 75 seconds.
    let slow = build_salience_args(Path::new("/m/a.mp4"), 0.0, 3600.0).join(" ");
    assert!(slow.contains("fps=0.2000"), "{slow}");
    // A zero-length window still produces a runnable command.
    let empty = build_salience_args(Path::new("/m/a.mp4"), 5.0, 5.0).join(" ");
    assert!(empty.contains("-t 0.040"), "{empty}");
}

#[test]
fn scoring_no_frames_yields_an_empty_map() {
    assert_eq!(score_salience(&[]), SalienceMap::default());
    // A partial frame is not a frame.
    assert_eq!(score_salience(&[7u8; 128]), SalienceMap::default());
}

#[test]
fn scoring_finds_the_detailed_half_of_a_flat_frame() {
    let (w, h) = (SALIENCE_COLS, SALIENCE_ROWS);
    // Left half flat gray, right half a hard checkerboard.
    let mut frame = vec![40u8; w * h];
    for y in 0..h {
        for x in w / 2..w {
            frame[y * w + x] = if (x + y) % 2 == 0 { 0 } else { 255 };
        }
    }
    let map = score_salience(&frame);
    assert_eq!((map.cols, map.rows), (w, h));
    let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
    assert!(crop.offset > 0.0, "the textured half pulls the window right: {crop:?}");
}

#[test]
fn scoring_weighs_a_moving_subject_over_a_static_one() {
    let (w, h) = (SALIENCE_COLS, SALIENCE_ROWS);
    let block = |frame: &mut Vec<u8>, from: usize, to: usize, v: u8| {
        for y in h / 3..2 * h / 3 {
            for x in from..to {
                frame[y * w + x] = v;
            }
        }
    };
    // Two frames: a static block on the left, a block on the right that moves.
    let mut a = vec![0u8; w * h];
    block(&mut a, 4, 12, 200);
    block(&mut a, w - 16, w - 8, 200);
    let mut b = a.clone();
    block(&mut b, w - 16, w - 8, 0);
    block(&mut b, w - 20, w - 12, 200);
    let mut raw = a;
    raw.extend_from_slice(&b);
    let map = score_salience(&raw);
    let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
    assert!(crop.offset > 0.0, "motion wins over equal detail: {crop:?}");
}

#[test]
fn parses_silence_pairs() {
    let log = "\
[silencedetect @ 0x1] silence_start: 12.5
[silencedetect @ 0x1] silence_end: 14.0 | silence_duration: 1.5
[silencedetect @ 0x1] silence_start: 60
[silencedetect @ 0x1] silence_end: 63.2 | silence_duration: 3.2
";
    let ranges = parse_silence(log);
    assert_eq!(ranges.len(), 2);
    assert!((ranges[0].start - 12.5).abs() < 1e-9);
    assert!((ranges[0].end - 14.0).abs() < 1e-9);
    assert!((ranges[1].end - 63.2).abs() < 1e-9);
}

#[test]
fn unterminated_silence_is_dropped() {
    let ranges = parse_silence("silence_start: 5.0\n");
    assert!(ranges.is_empty());
}

#[test]
fn parses_scene_times() {
    // `n:0` is frame 0, which the pass lets through to keep ffmpeg reporting: not a cut.
    let log = "\
[Parsed_showinfo_1 @ 0x1] n:   0 pts:0 pts_time:0 duration_time:0.04
[Parsed_showinfo_1 @ 0x1] n:   1 pts:720 pts_time:30.0 duration_time:0.04
[Parsed_showinfo_1 @ 0x1] n:   2 pts:1800 pts_time:75.5 duration_time:0.04
";
    let scenes = parse_scenes(log);
    assert_eq!(scenes, vec![30.0, 75.5]);
    // A log of frame 0 alone (a single take) has no cut; a run that logged nothing none either.
    assert!(parse_scenes("[Parsed_showinfo_1 @ 0x1] n:0 pts:0 pts_time:0 duration_time:0.04\n").is_empty());
    assert!(parse_scenes("").is_empty());
}

#[test]
fn parses_rational_fps() {
    assert!((parse_rational("30000/1001").unwrap() - 29.97).abs() < 0.01);
    assert_eq!(parse_rational("30/1"), Some(30.0));
    assert_eq!(parse_rational("25/0"), None);
}

#[test]
fn peaks_have_requested_length_and_range() {
    let samples: Vec<f32> = (0..1000).map(|i| ((i as f32) / 1000.0) - 0.5).collect();
    let p = peaks(&samples, 16);
    assert_eq!(p.len(), 16);
    assert!(p.iter().all(|&v| (0.0..=1.0).contains(&v)));
}

#[test]
fn peak_downsampler_is_length_independent_and_keeps_the_peak() {
    // Stream far more samples than 2*buckets so the halving path runs many
    // times, with one clipping spike buried in the middle.
    let buckets = 16;
    let mut down = PeakDownsampler::new(buckets);
    for i in 0..100_000u32 {
        let s = if i == 40_000 { 2.0 } else { ((i % 7) as f32) / 50.0 };
        down.push(s);
    }
    let out = down.finish();
    // Exactly `buckets` long regardless of how many samples streamed through.
    assert_eq!(out.len(), buckets);
    assert!(out.iter().all(|&v| (0.0..=1.0).contains(&v)));
    // The clipping spike survives the downsample, clamped into range.
    let max = out.iter().copied().fold(0.0_f32, f32::max);
    assert!((max - 1.0).abs() < 1e-6, "peak should be preserved, got {max}");
}

#[test]
fn peak_downsampler_handles_fewer_samples_than_buckets() {
    let mut down = PeakDownsampler::new(16);
    for &s in &[0.1, 0.9, 0.3] {
        down.push(s);
    }
    let out = down.finish();
    assert_eq!(out.len(), 16);
    let max = out.iter().copied().fold(0.0_f32, f32::max);
    assert!((max - 0.9).abs() < 1e-6, "got {max}");
}

#[test]
fn proxy_args_are_all_intra_audioless_and_keep_timing() {
    let args = build_proxy_args("/in.mov", "/out.mp4", 3, PROXY_MAX_WIDTH, "libx264", None, None, None);
    // All-intra: every frame a keyframe, so a preview seek decodes one frame.
    let gop = args.iter().position(|a| a == "-g").expect("-g present");
    assert_eq!(args[gop + 1], "1");
    // Thread-capped so a background proxy encode leaves cores for the GUI/agent.
    let threads = args.iter().position(|a| a == "-threads").expect("-threads present");
    assert_eq!(args[threads + 1], "3");
    // Audio is dropped — the proxy is only ever decoded for video frames.
    assert!(args.contains(&"-an".to_string()));
    // Downscale only: the proxy must NOT retime, trim or seek, or a source
    // time would no longer map 1:1 onto it (the invariant preview seek math
    // and the shared clip source-window math both rely on).
    assert!(!args.contains(&"-r".to_string()), "proxy must not change fps");
    assert!(!args.contains(&"-t".to_string()), "proxy must not trim duration");
    assert!(!args.contains(&"-ss".to_string()), "proxy must not seek");
    assert!(args.iter().any(|a| a.contains("scale='min(1280,iw)':-2")));
    assert!(args.contains(&"libx264".to_string()));
    // The output is a `.part` temp file the muxer can't be inferred from, so
    // the format must be stated or ffmpeg refuses to start.
    assert!(
        args.windows(2).any(|w| w[0] == "-f" && w[1] == "mp4"),
        "muxer must be explicit"
    );
    // The source is the input; the proxy is the (final) output.
    let input = args.iter().position(|a| a == "-i").expect("-i present");
    assert_eq!(args[input + 1], "/in.mov");
    assert_eq!(args.last().unwrap(), "/out.mp4");
}

#[test]
fn proxy_args_with_hw_encoder_spell_quality_per_family_and_stay_all_intra() {
    // NVENC: CRF intent becomes -rc vbr -cq, input format nv12, and the
    // all-intra / no-retime invariants hold exactly as in software.
    let args = build_proxy_args(
        "/in.mov",
        "/out.mp4",
        3,
        PROXY_MAX_WIDTH,
        "h264_nvenc",
        Some("auto"),
        None,
        None,
    );
    assert!(args.windows(2).any(|w| w[0] == "-hwaccel" && w[1] == "auto"));
    assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "h264_nvenc"));
    assert!(args.windows(2).any(|w| w[0] == "-cq" && w[1] == "24"));
    assert!(!args.contains(&"-crf".to_string()), "hw encoders have no -crf");
    assert!(args.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "nv12"));
    assert!(args.windows(2).any(|w| w[0] == "-g" && w[1] == "1"));
    assert!(!args.contains(&"-ss".to_string()) && !args.contains(&"-r".to_string()));
    // The `-hwaccel` is an input option: it must precede the `-i`.
    let hw = args.iter().position(|a| a == "-hwaccel").unwrap();
    let input = args.iter().position(|a| a == "-i").unwrap();
    assert!(hw < input);
}

#[test]
fn an_ordinary_proxy_is_the_same_argv_it_always_was() {
    // Nothing here may change for a source that starts its video with its
    // container: that is every file whose proxy is already cached and right.
    assert_eq!(
        build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, None, None).join(" "),
        "-hide_banner -loglevel error -y -i /in.mov -an \
             -vf scale='min(1280,iw)':-2:flags=bilinear -c:v libx264 -preset veryfast -crf 24 \
             -g 1 -threads 3 -pix_fmt yuv420p -f mp4 /out.mp4"
    );
}

#[test]
fn a_late_video_start_is_filled_by_one_held_frame_and_keeps_every_timestamp() {
    let args = build_proxy_args(
        "/in.mov",
        "/out.mp4",
        3,
        1280,
        "libx264",
        Some("auto"),
        Some("TONEMAP"),
        Some("-fps_mode"),
    );
    let flag = |name: &str| args.iter().position(|a| a == name).map(|i| args[i + 1].as_str());
    // One clone of the first frame at time zero, merged in by timestamp.
    let graph = flag("-filter_complex").expect("a filter_complex");
    assert!(
        graph.contains("split[a][b]") && graph.contains("trim=end_frame=1,setpts=PTS-STARTPTS[h]"),
        "{graph}"
    );
    assert!(graph.ends_with("[h][b]interleave[v]"), "{graph}");
    // The downscale and the tone-map run once, before the split, so the clone is
    // a proxy-sized SDR frame like the rest.
    assert!(
        graph.starts_with("[0:v:0]scale='min(1280,iw)':-2:flags=bilinear,TONEMAP,split"),
        "{graph}"
    );
    assert_eq!(flag("-map"), Some("[v]"));
    assert!(!args.contains(&"-vf".to_string()), "one chain, not two");
    // Variable frame rate, spelled the way this ffmpeg takes it: cfr (every
    // FFmpeg-6 mp4's default) would snap the frames to a grid again.
    assert_eq!(flag("-fps_mode"), Some("vfr"));
    assert!(!args.contains(&"cfr".to_string()));
    // Everything else about the proxy is as ever: all-intra, no retime, no seek.
    assert_eq!(flag("-g"), Some("1"));
    assert!(args.contains(&"-an".to_string()));
    assert!(!args.contains(&"-ss".to_string()) && !args.contains(&"-r".to_string()));
    assert_eq!(args.last().unwrap(), "/out.mp4");
    // The flag's spelling is the caller's, for a pre-5.1 ffmpeg.
    let legacy = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, None, Some("-vsync"));
    assert!(legacy.windows(2).any(|w| w[0] == "-vsync" && w[1] == "vfr"));
    // A hardware encoder only changes the encoder's own flags.
    let nvenc = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "h264_nvenc", None, None, Some("-fps_mode"));
    assert!(nvenc.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "nv12"));
    assert!(nvenc.windows(2).any(|w| w[0] == "-fps_mode" && w[1] == "vfr"));
}

#[test]
fn head_lead_is_the_video_start_past_the_container_start() {
    // Audio at 0 and video at 0.08 s.
    assert!((head_lead(Some(0.08), Some(0.0)) - 0.08).abs() < 1e-12);
    // Relative to the container, whatever clock it started on (an MPEG-TS
    // capture begins at some arbitrary tens of seconds).
    assert!((head_lead(Some(603.5), Some(600.0)) - 3.5).abs() < 1e-9);
    // The video *is* the start, the video is early, or something is missing:
    // no lead.
    assert_eq!(head_lead(Some(0.0), Some(0.0)), 0.0);
    assert_eq!(head_lead(Some(0.0), Some(0.5)), 0.0);
    assert_eq!(head_lead(None, Some(0.0)), 0.0);
    assert_eq!(head_lead(Some(1.0), None), 0.0);
    assert!(needs_head_pad(0.08) && needs_head_pad(5.0));
    assert!(!needs_head_pad(0.0) && !needs_head_pad(HEAD_PAD_MIN) && !needs_head_pad(0.0004));
}

#[test]
fn a_files_identity_follows_its_path_size_and_modified_time() {
    let dir = std::env::temp_dir().join(format!("kerf-identity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (a, b) = (dir.join("a.bin"), dir.join("b.bin"));
    std::fs::write(&a, b"one").unwrap();
    std::fs::write(&b, b"one").unwrap();
    let first = source_identity(&a);
    assert_eq!(first, source_identity(&a), "stable while the file is untouched");
    assert_eq!(first, fnv1a(&source_key(&a)));
    assert_ne!(first, source_identity(&b), "the path is part of it");
    // A replaced file (another size) is another identity, even when written at once.
    std::fs::write(&a, b"three").unwrap();
    assert_ne!(first, source_identity(&a));
    // A path that is not there is still a stable key, and it changes when the file appears.
    let missing = dir.join("missing.bin");
    let before = source_identity(&missing);
    assert_eq!(before, source_identity(&missing));
    std::fs::write(&missing, b"x").unwrap();
    assert_ne!(before, source_identity(&missing));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The learned fallback is one-way and is what `decode_hwaccel()` reads, so a decoder
/// outside the engine that calls it silences the preview path's attempts too. (Leaves the
/// flag cleared: nothing else in the suite may depend on it being set — a run under
/// `KERF_HWACCEL=none` already decodes in software with the flag never consulted.)
#[test]
fn disabling_decode_hwaccel_is_shared_with_the_preview_path() {
    disable_decode_hwaccel();
    assert_eq!(decode_hwaccel(), None);
    disable_decode_hwaccel();
    assert_eq!(decode_hwaccel(), None);
}

/// A rescue of a still, a frame grab or the stream is not classified, so it clears the learned
/// fallback the previews read but not what an export's `auto` is dropped on.
#[test]
fn an_unclassified_preview_conviction_leaves_the_exports_auto_alone() {
    disable_decode_hwaccel();
    assert_eq!(decode_hwaccel(), None);
    let proven = HWACCEL_PROVEN_BAD.load(std::sync::atomic::Ordering::Relaxed);
    assert!(!proven, "only convict_hardware_decode sets it, and no test calls that");
    assert_eq!(export_hwaccel_request(Some("auto"), true, proven).as_deref(), Some("auto"));
}

#[test]
fn source_traits_reads_hdr_and_lead_from_one_probe() {
    let json = r#"{"programs":[],"streams":[{"color_transfer":"arib-std-b67","start_time":"0.080013"}],
                       "format":{"start_time":"0.000000"}}"#;
    let t = parse_source_traits(json);
    assert_eq!(t.hdr, Some(Hdr::Hlg));
    assert!((t.lead - 0.080013).abs() < 1e-9);
    let pq = parse_source_traits(r#"{"streams":[{"color_transfer":"smpte2084"}],"format":{"start_time":"0.0"}}"#);
    assert_eq!((pq.hdr, pq.lead), (Some(Hdr::Pq), 0.0));
    // `N/A` is left out by ffprobe's JSON writer; an unknown transfer is SDR.
    let plain = parse_source_traits(r#"{"streams":[{"color_transfer":"bt709"}],"format":{"start_time":"0.000000"}}"#);
    let described = SourceTraits {
        described: true,
        ..SourceTraits::default()
    };
    assert_eq!(plain, described);
    assert_eq!(parse_source_traits(r#"{"streams":[],"format":{}}"#), described);
    // The file's own range flag, which an asset probed before it was recorded does not hold.
    assert!(parse_source_traits(r#"{"streams":[{"color_range":"pc"}],"format":{}}"#).full_range);
    for tag in ["tv", "unknown", ""] {
        let json = format!(r#"{{"streams":[{{"color_range":"{tag}"}}],"format":{{}}}}"#);
        assert!(!parse_source_traits(&json).full_range, "{tag:?}");
    }
    assert_eq!(parse_source_traits("not json"), SourceTraits::default());
    // A transport stream (a camcorder's .mts / .m2ts) is reported as having no
    // lead whatever its streams' starts say: a padded proxy cannot fix it, so it
    // keeps the plain proxy and its key.
    let ts = r#"{"streams":[{"start_time":"1.521333"}],"format":{"format_name":"mpegts","start_time":"1.400000"}}"#;
    assert_eq!(parse_source_traits(ts).lead, 0.0);
    let mp4 = r#"{"streams":[{"start_time":"1.521333"}],"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","start_time":"1.4"}}"#;
    assert!((parse_source_traits(mp4).lead - 0.121333).abs() < 1e-9);
}

/// The range a file says, as the compositor asks for it: `None` for an answer that describes no file (an `ffprobe` that exits 0
/// and prints nothing usable is not "limited range"), the flag for one that does, and "not full range" for a file the probe
/// read that has no video stream at all.
#[test]
fn a_files_range_is_unknown_when_the_probe_said_nothing_usable() {
    let range = |json: &str| full_range_of(parse_source_traits(json));
    for unusable in [
        "",
        "\n",
        "not json",
        "{}",
        "[]",
        "null",
        r#"{"streams":null,"format":{}}"#,
        r#"{"streams":{},"format":{}}"#,
        r#"{"streams":[{"color_range":"pc"}]}"#,
        r#"{"streams":[{"color_range":"pc"}],"format":null}"#,
        r#"{"format":{"format_name":"matroska,webm"}}"#,
        r#"{"programs":[]}"#,
    ] {
        assert_eq!(range(unusable), None, "{unusable:?}");
    }
    assert_eq!(range(r#"{"streams":[{"color_range":"pc"}],"format":{}}"#), Some(true));
    assert_eq!(
        range(r#"{"streams":[{"color_range":"tv"}],"format":{"format_name":"mov,mp4"}}"#),
        Some(false)
    );
    assert_eq!(
        range(r#"{"streams":[{}],"format":{}}"#),
        Some(false),
        "a stream that says nothing of its range"
    );
    // A wav or an mp3: read, and with no picture in it (what the picture would be is not a question it has an answer to).
    assert_eq!(
        range(r#"{"programs":[],"streams":[],"format":{"format_name":"wav","start_time":"0.000000"}}"#),
        Some(false)
    );
    // (A probe cached for a file serves both questions: `peek` finds it, and a file never asked about has none.)
    assert_eq!(
        source_is_full_range_known(Path::new("/no/such/file/was/asked/about.mkv")),
        None
    );
}

/// A probe whose output describes no file is a failed probe (`None`: the cache retries it after the failure backoff), not the
/// neutral traits of an ordinary file remembered for good.
#[cfg(unix)]
#[test]
fn a_probe_that_said_nothing_usable_is_a_failure_and_not_remembered_as_an_answer() {
    let ask = |ffprobe: &str| probe_source_traits(ffprobe, Path::new("/no/such/file.mkv"), std::time::Duration::from_secs(10));
    // Exits 0 with nothing; exits 0 with its own arguments (which are no JSON); does not run at all.
    assert_eq!(ask("true"), None);
    assert_eq!(ask("echo"), None);
    assert_eq!(ask("/no/such/ffprobe"), None);
    let cache = ProbeCache::<SourceTraits>::retrying(std::time::Duration::from_secs(3600));
    let path = Path::new("/no/such/file.mkv");
    let mut asked = 0;
    for _ in 0..3 {
        assert_eq!(
            cache.get(path, || {
                asked += 1;
                ask("true")
            }),
            None
        );
    }
    assert_eq!(asked, 1, "a failure is remembered inside the backoff");
    let retrying = ProbeCache::<SourceTraits>::retrying(std::time::Duration::ZERO);
    for _ in 0..3 {
        let _ = retrying.get(path, || {
            asked += 1;
            ask("true")
        });
    }
    assert_eq!(asked, 4, "and asked again after it, not kept as an answer");
}

/// A hung `ffprobe` is killed at the timeout and a failing one is no answer, where
/// `Command::output()` waited for as long as the process cared to run.
#[cfg(unix)]
#[test]
fn probing_a_source_is_bounded() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("kerf-source-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    };
    let file = Path::new("/media/a.mp4");
    let limit = std::time::Duration::from_millis(400);
    let started = Instant::now();
    assert_eq!(probe_source_traits(&script("hang", "exec sleep 30"), file, limit), None);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(probe_source_traits(&script("fail", "exit 1"), file, limit), None);
    assert_eq!(probe_source_traits("/no/such/ffprobe", file, limit), None);
    let json = r#"{"streams":[{"color_transfer":"smpte2084"}],"format":{"format_name":"matroska,webm"}}"#;
    let ok = probe_source_traits(&script("ok", &format!("echo '{json}'")), file, limit).expect("an answer");
    assert_eq!((ok.hdr, ok.indexed, ok.time_base), (Some(Hdr::Pq), true, None));
    // The tick of the first video stream, asked for with the rest, is what a backfill reads.
    let json = r#"{"streams":[{"time_base":"1/1000"}],"format":{"format_name":"matroska,webm"}}"#;
    let ticked = probe_source_traits(&script("ticked", &format!("echo '{json}'")), file, limit).expect("an answer");
    assert_eq!(ticked.time_base, Some((1, 1000)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_proxy_stands_in_for_an_sdr_yuv_mp4_or_matroska_source_and_for_nothing_else() {
    let traits_of = |format: &str, transfer: &str, pix_fmt: &str| {
        parse_source_traits(&format!(
            r#"{{"streams":[{{"color_transfer":"{transfer}","pix_fmt":"{pix_fmt}"}}],"format":{{"format_name":"{format}"}}}}"#
        ))
    };
    let traits = |format: &str, transfer: &str| traits_of(format, transfer, "yuv420p");
    let (yes, picture) = (
        ProxyFidelity {
            times: true,
            scores: true,
        },
        ProxyFidelity {
            times: true,
            scores: false,
        },
    );
    assert_eq!(fidelity_of(traits("mov,mp4,m4a,3gp,3g2,mj2", "bt709")), yes);
    assert_eq!(fidelity_of(traits("matroska,webm", "")), yes);
    // HDR: times hold, but the scores are taken in HLG / PQ code values and the proxy is SDR.
    assert_eq!(fidelity_of(traits("mov,mp4,m4a,3gp,3g2,mj2", "arib-std-b67")), picture);
    assert_eq!(fidelity_of(traits("matroska,webm", "smpte2084")), picture);
    // Every YCbCr layout keeps its luma alone in plane 0, which is what the scene filter scores.
    for pix_fmt in [
        "yuv420p10le",
        "yuvj420p",
        "yuv422p10le",
        "yuv444p12le",
        "gray",
        "gray10le",
        "nv12",
    ] {
        assert_eq!(
            fidelity_of(traits_of("mov,mp4,m4a,3gp,3g2,mj2", "bt709", pix_fmt)),
            yes,
            "{pix_fmt}"
        );
    }
    // RGB (G plane, or every interleaved byte), packed YCbCr, a palette, alpha and a name nobody
    // knows are scored on something else than the proxy's luma.
    for pix_fmt in [
        "gbrp", "gbrp10le", "rgb24", "bgr24", "bgr0", "rgb48le", "yuyv422", "uyvy422", "pal8", "yuva420p", "rgba", "", "newfmt",
    ] {
        assert_eq!(
            fidelity_of(traits_of("mov,mp4,m4a,3gp,3g2,mj2", "bt709", pix_fmt)),
            picture,
            "{pix_fmt}"
        );
        assert_eq!(fidelity_of(traits_of("matroska,webm", "", pix_fmt)), picture, "{pix_fmt}");
    }
    // A transport stream's proxy is `lead` off when its video starts late; the others are unproven.
    for other in ["mpegts", "avi", "mpeg", "flv", "mxf", "h264"] {
        assert_eq!(fidelity_of(traits(other, "bt709")), ProxyFidelity::default(), "{other}");
    }
    // A failed probe trusts nothing.
    assert_eq!(fidelity_of(SourceTraits::default()), ProxyFidelity::default());
}

#[test]
fn the_progress_report_is_told_from_the_log() {
    for line in [
        "frame=120",
        "fps=0.0",
        "stream_0_0_q=-0.0",
        "out_time_us=4000000",
        "out_time=00:00:04.000000",
        "progress=end",
        "speed=N/A",
    ] {
        assert!(is_progress_line(line), "{line}");
    }
    for line in [
        "[Parsed_showinfo_1 @ 0x1] n:   4 pts:  4 pts_time:4 fmt:yuv420p",
        "Error opening input file x.mp4.",
        "x.mp4: Invalid data found when processing input",
        "key=value without being ffmpeg's",
        "",
    ] {
        assert!(!is_progress_line(line), "{line}");
    }
}

#[test]
fn the_silence_a_read_may_keep_grows_with_the_work_between_a_floor_and_a_ceiling() {
    use std::time::Duration as D;
    let secs = |window: f64| sheet_stall(window).as_secs_f64();
    // Start-up and a short window get the floor: a minute.
    assert_eq!((secs(0.0), secs(1.0), secs(2.0)), (60.0, 60.0, 60.0));
    // A longer window is decoded for longer before `tile` says anything: 30 s per second of footage.
    assert_eq!((secs(10.0), secs(60.0)), (300.0, 1800.0));
    // Wedged is still finite, and nonsense (a negative or unknown duration) falls back to the floor.
    assert_eq!(secs(1e9), 2.0 * 3600.0);
    assert_eq!((secs(-5.0), secs(f64::NAN)), (60.0, 60.0));
    assert_eq!(secs(f64::INFINITY), 60.0);
    assert_eq!(
        stall_for_work(4.0, 0.5, D::from_millis(100), D::from_secs(10)),
        D::from_secs(2)
    );
}

#[cfg(unix)]
#[test]
fn a_read_that_is_busy_and_silent_is_not_killed_within_its_bound() {
    use std::time::Duration as D;
    let busy = || {
        let mut cmd = Command::new("sh");
        // Silent for 1.2 s, then the whole answer on stdout: a contact sheet's shape.
        cmd.args(["-c", "sleep 1.2; printf sheet"]);
        cmd
    };
    // A 4 s window at half a second of silence per second of footage: two seconds is enough...
    let bound = stall_for_work(4.0, 0.5, std::time::Duration::from_millis(100), D::from_secs(10));
    let done = output_unless_stalled(busy(), bound).unwrap();
    assert!(done.succeeded() && !done.stalled, "{:?}", done.stderr);
    assert_eq!(done.stdout, b"sheet");
    // ...and with no window to scale by, the floor alone kills the same child.
    let floor_only = stall_for_work(0.0, 0.5, std::time::Duration::from_millis(100), D::from_secs(10));
    let killed = output_unless_stalled(busy(), floor_only).unwrap();
    assert!(killed.stalled && killed.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn a_read_that_goes_quiet_is_killed_and_one_that_reports_is_not() {
    let sh = |script: &str| {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    };
    // Quiet: a child that never writes (a wedged decoder) is killed at the bound, not waited for.
    let started = Instant::now();
    let quiet = output_unless_stalled(
        sh("echo warming up >&2; echo frame=1 >&2; sleep 30"),
        std::time::Duration::from_millis(500),
    )
    .unwrap();
    assert!(quiet.stalled && !quiet.succeeded());
    // What it had said is kept for the error message (without the progress lines).
    assert_eq!(quiet.stderr_tail(), "warming up");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    // Reporting: each line of stderr is a word, so a long run that keeps talking is not cut off,
    // and the progress lines are not in the text the caller reads errors from.
    let talking = output_unless_stalled(
        sh("for i in 1 2 3 4 5 6; do echo frame=$i >&2; sleep 0.1; done; echo oops >&2; printf out"),
        std::time::Duration::from_millis(400),
    )
    .unwrap();
    assert!(talking.succeeded(), "{:?}", talking.stderr);
    assert_eq!((talking.stdout.as_slice(), talking.stderr.as_str()), (&b"out"[..], "oops\n"));
    // A failure is a failure, not a stall.
    let failed = output_unless_stalled(sh("echo bad >&2; exit 3"), std::time::Duration::from_secs(5)).unwrap();
    assert!(!failed.stalled && !failed.succeeded());
    assert_eq!(failed.stderr, "bad\n");
}

#[test]
fn a_contact_sheet_drops_the_head_pad_clone_when_it_reads_from_the_start_only() {
    let padded = "/home/u/.cache/kerf/proxies/0123456789abcdef.lead.mp4";
    let plain = "/home/u/.cache/kerf/proxies/0123456789abcdef.mp4";
    let vf = |path: &str, start: f64| {
        let (args, _) = build_contact_sheet_args(path, start, start + 8.0, 4, 2, 160, 3, None);
        args[args.iter().position(|a| a == "-vf").unwrap() + 1].clone()
    };
    // A seek to 0.000 keeps pts 0, the clone with it: it would take a slot of its own.
    assert!(vf(padded, 0.0).starts_with("trim=start_frame=1,fps="), "{}", vf(padded, 0.0));
    assert!(vf(padded, 0.0004).starts_with("trim=start_frame=1,fps="));
    // Any later seek has skipped it already, and dropping a frame there would drop a real one.
    assert!(vf(padded, 0.5).starts_with("fps="));
    // An ordinary proxy and an original have no clone: their argv is what it always was.
    assert!(vf(plain, 0.0).starts_with("fps="));
    assert!(vf("/media/clip.lead.mp4", 0.0).starts_with("fps="));
}

#[test]
fn only_mp4_and_matroska_are_indexed_containers() {
    let indexed =
        |name: &str| parse_source_traits(&format!(r#"{{"streams":[{{}}],"format":{{"format_name":"{name}"}}}}"#)).indexed;
    assert!(indexed("mov,mp4,m4a,3gp,3g2,mj2"));
    assert!(indexed("matroska,webm"));
    for other in ["mpegts", "avi", "mpeg", "mpegvideo", "h264", "flv", "asf", "mxf", "image2"] {
        assert!(!indexed(other), "{other}");
    }
    // No format named (a failed or odd probe) is not indexed.
    assert!(!parse_source_traits(r#"{"streams":[{}],"format":{}}"#).indexed);
    assert!(!parse_source_traits("not json").indexed);
}

#[test]
fn a_proxy_is_rebuilt_only_for_the_sources_that_were_made_wrong() {
    let src = "/media/a.mov|1234|99";
    // The key of an ordinary SDR file with a normal start is what it has
    // always been, so its cached proxy is still found.
    assert_eq!(proxy_key(src, 1280, false, false), "/media/a.mov|1234|99|1280");
    // The `|sdr` precedent, and the new suffix stacked after it.
    assert_eq!(proxy_key(src, 1280, true, false), "/media/a.mov|1234|99|1280|sdr");
    assert_eq!(proxy_key(src, 1280, false, true), "/media/a.mov|1234|99|1280|lead");
    assert_eq!(proxy_key(src, 3072, true, true), "/media/a.mov|1234|99|3072|sdr|lead");
    let keys = [
        proxy_key(src, 1280, false, false),
        proxy_key(src, 1280, true, false),
        proxy_key(src, 1280, false, true),
        proxy_key(src, 1280, true, true),
    ];
    let hashes: std::collections::HashSet<u64> = keys.iter().map(|k| fnv1a(k)).collect();
    assert_eq!(hashes.len(), keys.len(), "every variant gets its own file");
}

#[test]
fn a_padded_proxy_is_told_by_its_name_and_nothing_else_is() {
    assert_eq!(proxy_file_name(0xabc, false), "0000000000000abc.mp4");
    assert_eq!(proxy_file_name(0xabc, true), "0000000000000abc.lead.mp4");
    let padded = is_head_padded_proxy;
    assert!(padded("/home/u/.cache/kerf/proxies/0000000000000abc.lead.mp4"));
    #[cfg(windows)]
    assert!(padded(r"C:\Users\u\AppData\Local\kerf\proxies\0123456789ABCDEF.lead.mp4"));
    // The plain proxy of the same source, an original, a lookalike elsewhere and a
    // name that is not a proxy's hash are none of them padded.
    assert!(!padded("/home/u/.cache/kerf/proxies/0000000000000abc.mp4"));
    assert!(!padded("/media/clip.mp4"));
    assert!(!padded("/media/0000000000000abc.lead.mp4"));
    assert!(!padded("/home/u/.cache/kerf/proxies/clip.lead.mp4"));
    assert!(!padded("/home/u/.cache/kerf/proxies/0000000000000abcd.lead.mp4"));
    assert!(!padded("/home/u/.cache/other/proxies/0000000000000abc.lead.mp4"));
    // And the names `generate_proxy` writes, in the directory it writes them to,
    // are the ones that read back.
    if let Some(dir) = crate::paths::cache_dir().map(|d| d.join("kerf").join("proxies")) {
        assert!(!is_head_padded_proxy(&dir.join(proxy_file_name(7, false)).to_string_lossy()));
        assert!(is_head_padded_proxy(&dir.join(proxy_file_name(7, true)).to_string_lossy()));
    }
}

/// A proxy's sidecar is its video stream, named after the proxy, and describes *that*
/// file: another size, a replaced file, another version or garbage reads as none.
#[test]
fn a_proxy_sidecar_describes_the_file_beside_it_and_nothing_else() {
    let dir = Scratch::new("sidecar");
    let video = StreamInfo {
        width: Some(1280),
        height: Some(720),
        pix_fmt: Some("yuv420p".into()),
        ..crate::engine::test_support::video_stream(1280, 720, 30.0)
    };
    // Named after the proxy, whatever its ending, and apart from its own temp file.
    let plain = dir.join("0000000000000abc.mp4");
    let padded = dir.join("0000000000000abc.lead.mp4");
    assert_eq!(proxy_sidecar_path(&plain), dir.join("0000000000000abc.json"));
    assert_eq!(proxy_sidecar_path(&padded), dir.join("0000000000000abc.lead.json"));
    for proxy in [&plain, &padded] {
        std::fs::write(proxy, b"not really a video").unwrap();
        assert_eq!(read_proxy_sidecar(proxy), None, "no sidecar yet");
        write_proxy_sidecar(proxy, proxy, video.clone(), None);
        assert_eq!(read_proxy_sidecar(proxy), Some(video.clone()));
        // The file it describes was replaced by another one: it no longer applies.
        std::fs::write(proxy, b"a different, longer encode").unwrap();
        assert_eq!(read_proxy_sidecar(proxy), None);
        // A sidecar from another version of the format, and a half-written one.
        write_proxy_sidecar(proxy, proxy, video.clone(), None);
        let path = proxy_sidecar_path(proxy);
        let current = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, current.replace("\"version\":1", "\"version\":99")).unwrap();
        assert_eq!(read_proxy_sidecar(proxy), None);
        std::fs::write(&path, &current[..current.len() / 2]).unwrap();
        assert_eq!(read_proxy_sidecar(proxy), None);
    }
    // Writing leaves nothing behind, and never touches the proxy's own temp name.
    let temps: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".part"))
        .collect();
    assert!(temps.is_empty(), "{temps:?}");
}

/// A sidecar says which encoder wrote the proxy, and says nothing when it was x264: every sidecar
/// written before the Fast (GPU) pipeline existed is valid as it is and reads as x264's.
#[test]
fn a_proxy_sidecar_records_a_hardware_encoder_and_none_for_x264() {
    let dir = Scratch::new("sidecar-encoder");
    let video = StreamInfo {
        width: Some(1280),
        height: Some(720),
        pix_fmt: Some("yuv420p".into()),
        ..crate::engine::test_support::video_stream(1280, 720, 30.0)
    };
    let proxy = dir.join("0000000000000abc.mp4");
    std::fs::write(&proxy, b"not really a video").unwrap();
    let on_disk =
        || -> serde_json::Value { serde_json::from_slice(&std::fs::read(proxy_sidecar_path(&proxy)).unwrap()).unwrap() };

    // x264: no `encoder` key at all (byte-compatible with a build that never knew the field).
    write_proxy_sidecar(&proxy, &proxy, video.clone(), None);
    assert!(on_disk().get("encoder").is_none(), "{}", on_disk());
    assert_eq!(read_proxy_sidecar(&proxy), Some(video.clone()));
    assert!(!proxy_hardware_encoded(&proxy));
    assert_eq!(stored_proxy_encoder(&proxy), None);

    // A hardware encoder: named, and still a sidecar every reader accepts.
    for encoder in ["h264_amf", "h264_nvenc", "h264_qsv", "h264_vaapi"] {
        write_proxy_sidecar(&proxy, &proxy, video.clone(), Some(encoder));
        assert_eq!(on_disk()["encoder"], encoder);
        assert_eq!(read_proxy_sidecar(&proxy), Some(video.clone()), "{encoder}");
        assert!(proxy_hardware_encoded(&proxy), "{encoder}");
        assert_eq!(stored_proxy_encoder(&proxy).as_deref(), Some(encoder));
    }
    // x264 named is x264 (a future build may write it); an unreadable or missing sidecar says nothing.
    write_proxy_sidecar(&proxy, &proxy, video.clone(), Some("libx264"));
    assert!(!proxy_hardware_encoded(&proxy));
    std::fs::remove_file(proxy_sidecar_path(&proxy)).unwrap();
    assert!(!proxy_hardware_encoded(&proxy) && stored_proxy_encoder(&proxy).is_none());
    std::fs::write(proxy_sidecar_path(&proxy), b"{ half a sidec").unwrap();
    assert!(!proxy_hardware_encoded(&proxy));

    // A sidecar an older build wrote (no field), and one a newer build wrote (a field this one does not know).
    write_proxy_sidecar(&proxy, &proxy, video.clone(), None);
    let mut older: serde_json::Value = on_disk();
    older.as_object_mut().unwrap().remove("encoder");
    std::fs::write(proxy_sidecar_path(&proxy), serde_json::to_vec(&older).unwrap()).unwrap();
    assert_eq!(read_proxy_sidecar(&proxy), Some(video.clone()));
    let mut newer = older.clone();
    newer["encoder"] = "h264_amf".into();
    newer["a_field_from_the_future"] = serde_json::json!({"x": [1, 2]});
    std::fs::write(proxy_sidecar_path(&proxy), serde_json::to_vec(&newer).unwrap()).unwrap();
    assert_eq!(read_proxy_sidecar(&proxy), Some(video));
    assert!(proxy_hardware_encoded(&proxy));

    // The proxy was replaced under its name (the size no longer matches): the description is stale and
    // is not used, but what wrote it is still said, so a fallback that writes the sidecar again does
    // not turn a hardware proxy into an x264 one.
    std::fs::write(&proxy, b"a different, longer encode").unwrap();
    assert_eq!(read_proxy_sidecar(&proxy), None);
    assert_eq!(stored_proxy_encoder(&proxy).as_deref(), Some("h264_amf"));
}

/// A finished encode lands with its sidecar — written before the proxy is visible — unless a
/// concurrent generator got there first, in which case theirs stays, untouched, and no
/// sidecar is written over it. Two writers never share a temp name.
#[test]
fn a_finished_proxy_lands_with_its_sidecar_unless_another_generator_got_there_first() {
    use std::cell::Cell;
    let dir = Scratch::new("finalize");
    let video = |w| StreamInfo {
        width: Some(w),
        height: Some(w / 2),
        pix_fmt: Some("yuv420p".into()),
        ..crate::engine::test_support::video_stream(w, w / 2, 30.0)
    };
    let (tmp, dst) = (dir.join("0000000000000abc.1.part"), dir.join("0000000000000abc.mp4"));
    std::fs::write(&tmp, b"ours").unwrap();
    // A hardware-encoded proxy lands with its encoder in the sidecar written before it is visible.
    let hardware = (dir.join("0000000000000bee.1.part"), dir.join("0000000000000bee.mp4"));
    std::fs::write(&hardware.0, b"fast").unwrap();
    finalize_proxy(&hardware.0, &hardware.1, || Some(video(640)), Some("h264_nvenc")).unwrap();
    assert!(proxy_hardware_encoded(&hardware.1) && read_proxy_sidecar(&hardware.1).is_some());
    let bumps = crate::plan_epoch::bumps_here();
    assert_eq!(finalize_proxy(&tmp, &dst, || Some(video(1280)), None).unwrap(), dst);
    assert!(
        crate::plan_epoch::bumps_here() > bumps,
        "a proxy that lands moves the plan epoch"
    );
    assert!(!tmp.exists());
    assert_eq!(std::fs::read(&dst).unwrap(), b"ours");
    assert_eq!(read_proxy_sidecar(&dst), Some(video(1280)));

    // Another generator's proxy is in place with its sidecar: a second encode is dropped, the
    // proxy and its sidecar stay as they are, and nobody probes.
    let before = std::fs::read(proxy_sidecar_path(&dst)).unwrap();
    std::fs::write(&tmp, b"a second, different encode").unwrap();
    let probed = Cell::new(false);
    finalize_proxy(
        &tmp,
        &dst,
        || {
            probed.set(true);
            Some(video(640))
        },
        None,
    )
    .unwrap();
    assert!(!probed.get() && !tmp.exists());
    assert_eq!(std::fs::read(&dst).unwrap(), b"ours");
    assert_eq!(std::fs::read(proxy_sidecar_path(&dst)).unwrap(), before);

    // A probe that fails still lands the proxy; it only has no sidecar yet.
    let other = dir.join("0000000000000def.mp4");
    std::fs::write(&tmp, b"third").unwrap();
    finalize_proxy(&tmp, &other, || None, None).unwrap();
    assert!(other.is_file() && !proxy_sidecar_path(&other).exists());

    // The temp names of two writers of one sidecar differ, and neither is the proxy's own.
    let sidecar = proxy_sidecar_path(&dst);
    let (a, b) = (sidecar_temp(&sidecar), sidecar_temp(&sidecar));
    assert_ne!(a, b);
    let proxy_own = dst.with_extension(format!("{}.part", std::process::id()));
    assert!(a != proxy_own && b != proxy_own && a.to_string_lossy().ends_with(".part"));
}

/// A clip read from the very start of a padded proxy (no `-ss`) opens with the
/// pad's clone of the first frame, which the original it stands for has no
/// frame for: the chain drops it before the trim. A read that is seeked skips
/// the clone already, and anything that is not a padded proxy is untouched.
#[test]
fn a_head_clip_of_a_padded_proxy_drops_the_pads_clone_and_nothing_else_does() {
    let proxy = "/home/u/.cache/kerf/proxies/0123456789abcdef.lead.mp4";
    let original = Asset {
        path: "/media/clip.mp4".into(),
        ..av_asset(Uuid::new_v4(), 30.0)
    };
    let padded = Asset {
        path: proxy.into(),
        ..original.clone()
    };
    let plain_proxy = Asset {
        path: "/home/u/.cache/kerf/proxies/0123456789abcdef.mp4".into(),
        ..original.clone()
    };
    let graph = |asset: &Asset, source_in: f64| {
        let timeline = single(vec![make_clip(asset.id, source_in, source_in + 4.0, 0.0)]);
        let args = build_preview_args(&timeline, std::slice::from_ref(asset), 0.0, 30.0, 960, 6).unwrap();
        flag_val(&args, "-filter_complex").unwrap().to_string()
    };
    // From the head: the clone goes, ahead of the trim that rebases the clip.
    let head = graph(&padded, 0.0);
    assert!(
        head.contains("trim=start_frame=1,trim=start=0:end=4.066666666666666,"),
        "{head}"
    );
    // `clip_seek` treats a millisecond as the head too; just past it is seeked.
    assert!(graph(&padded, 0.0005).contains("trim=start_frame=1,"));
    assert!(!graph(&padded, 0.002).contains("start_frame"), "a seek skips the clone");
    assert!(!graph(&padded, 1.5).contains("start_frame"));
    // The same clip on the original, or on a proxy that was not padded: no filter,
    // and the graph is the one it always was.
    for asset in [&original, &plain_proxy] {
        assert!(!graph(asset, 0.0).contains("start_frame"), "{asset:?}");
    }
    assert_eq!(graph(&original, 0.0), graph(&plain_proxy, 0.0));
    // An export reads the original, so it never has one either way.
    let timeline = single(vec![make_clip(original.id, 0.0, 4.0, 0.0)]);
    let export = build_export_args(&timeline, &[original], "out.mp4", &ExportOptions::default()).unwrap();
    assert!(!export.join(" ").contains("start_frame"));
}

#[test]
fn the_frame_sync_flag_is_spelled_the_way_this_ffmpeg_takes_it() {
    // (knows -fps_mode, knows -vsync): FFmpeg 6 knows both; 9 removed -vsync;
    // 4 predates -fps_mode.
    assert_eq!(fps_mode_flag_for(true, true), "-fps_mode");
    assert_eq!(fps_mode_flag_for(true, false), "-fps_mode");
    assert_eq!(fps_mode_flag_for(false, true), "-vsync");
    // A binary that would not run: the modern spelling, like every probe here.
    assert_eq!(fps_mode_flag_for(false, false), "-fps_mode");
}

#[test]
fn stitch_args_with_hw_encoder_accelerate_both_lens_decodes() {
    let args = build_stitch_args(
        "/dcim/f_00_.mp4",
        "/dcim/r_10_.mp4",
        "/cache/out.mp4",
        "hevc_nvenc",
        Some("auto"),
    );
    // One `-hwaccel` per lens input, each before its `-i`.
    assert_eq!(args.iter().filter(|a| *a == "-hwaccel").count(), 2);
    assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "hevc_nvenc"));
    assert!(args.windows(2).any(|w| w[0] == "-cq" && w[1] == "15"));
    assert!(args.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "nv12"));
    // The stitch geometry is untouched by the encoder choice.
    let graph = args
        .iter()
        .position(|a| a == "-filter_complex")
        .map(|i| args[i + 1].clone())
        .expect("-filter_complex present");
    assert!(graph.contains("hstack=shortest=1"));
    assert!(graph.contains("w=5760:h=2880"));
}

#[test]
fn quality_args_map_the_same_intent_per_family() {
    assert_eq!(quality_args("libx264", 24).join(" "), "-preset veryfast -crf 24");
    assert_eq!(quality_args("h264_qsv", 24).join(" "), "-global_quality 24");
    assert_eq!(quality_args("h264_amf", 24).join(" "), "-rc cqp -qp_i 24 -qp_p 24");
    // VideoToolbox flips the scale: lower CRF must become higher quality.
    let vt15: u32 = quality_args("hevc_videotoolbox", 15)[1].parse().unwrap();
    let vt28: u32 = quality_args("hevc_videotoolbox", 28)[1].parse().unwrap();
    assert!(vt15 > vt28);
}

#[test]
fn timeline_frame_hwaccel_is_per_input_and_opt_in() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    // Default stays byte-identical: no -hwaccel.
    let plain = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 1.0, 960, 4).unwrap();
    assert!(!plain.contains(&"-hwaccel".to_string()));
    // Requested: emitted as an input option (before the -i it applies to).
    let hw = ExportOptions {
        hwaccel: Some("auto".to_string()),
        ..ExportOptions::default()
    };
    let args = build_timeline_frame_args(&timeline, &assets, &hw, 1.0, 960, 4).unwrap();
    let at = args.iter().position(|a| a == "-hwaccel").expect("-hwaccel present");
    assert_eq!(args[at + 1], "auto");
    assert!(at < args.iter().position(|a| a == "-i").unwrap());
}

const PROXY_PATH: &str = "/cache/kerf/proxies/0123456789abcdef.mp4";

fn hw_still_opts() -> ExportOptions {
    ExportOptions {
        hwaccel: Some("auto".to_string()),
        ..ExportOptions::default()
    }
}

/// The still argv for `assets` (one clip each, on tracks of their own) the way the app asks
/// for it — decode acceleration requested — before and after the spawn-time tuning.
fn tuned_still(assets: &[Asset], os: HostOs) -> (Vec<String>, Vec<String>, bool) {
    let tracks = assets
        .iter()
        .map(|a| video_track(vec![make_clip(a.id, 0.0, 5.0, 0.0)]))
        .collect();
    let built = build_timeline_frame_args(&timeline_of(tracks), assets, &hw_still_opts(), 1.0, 960, 4).unwrap();
    let mut tuned = built.clone();
    let hw = tune_still_args(&mut tuned, assets, os);
    (built, tuned, hw)
}

fn at_path(path: &str, codec: &str, pix_fmt: &str) -> Asset {
    let mut asset = test_asset(vec![video_stream(3840, 2160, 30.0)]);
    asset.path = path.into();
    asset.streams[0].codec = codec.into();
    asset.streams[0].pix_fmt = Some(pix_fmt.into());
    asset
}

fn count(args: &[String], flag: &str) -> usize {
    args.iter().filter(|a| *a == flag).count()
}

#[test]
fn a_still_from_a_proxy_asks_for_no_hardware_and_one_thread() {
    let _serial = cpu::test_lock();
    let (built, tuned, hw) = tuned_still(&[at_path(PROXY_PATH, "h264", "yuv420p")], HostOs::Windows);
    // The builder still describes what was asked for (the golden oracle pins that) ...
    assert_eq!(count(&built, "-hwaccel"), 1);
    // ... and the spawn drops it for the proxy, which decodes a frame in milliseconds, and
    // keeps every pool in the process to one thread.
    assert!(!hw);
    assert_eq!(count(&tuned, "-hwaccel"), 0, "{tuned:?}");
    assert_eq!(&tuned[..4], ["-filter_threads", "1", "-filter_complex_threads", "1"]);
    assert_eq!(&tuned[tuned.len() - 3..], ["-threads", "1", "pipe:1"]);
    let input = tuned.iter().position(|a| a == "-i").unwrap();
    assert_eq!(&tuned[input - 2..input], ["-threads", "1"]);
    assert_eq!(tuned[input + 1], PROXY_PATH);
    // Nothing else moved: the tuned argv is the built one with those flags added and the
    // hardware one taken out.
    let mut rest = tuned;
    rest.drain(..4);
    rest.drain(rest.len() - 3..rest.len() - 1);
    rest.drain(input - 4 - 2..input - 4);
    let mut plain = built;
    let at = plain.iter().position(|a| a == "-hwaccel").unwrap();
    plain.drain(at..at + 2);
    assert_eq!(rest, plain);
}

#[test]
fn a_still_from_originals_keeps_the_budget_and_names_the_windows_decoder() {
    let _serial = cpu::test_lock();
    let original = at_path("/media/a.mp4", "hevc", "yuv420p10le");
    let (built, windows, hw) = tuned_still(std::slice::from_ref(&original), HostOs::Windows);
    assert!(hw);
    assert_eq!(flag_val(&windows, "-hwaccel"), Some("d3d11va"));
    let (_, linux, hw) = tuned_still(std::slice::from_ref(&original), HostOs::Other);
    assert!(hw);
    // Where `auto` is already right the argv is the built one, plus the budget's caps (none
    // at a full budget) — and never the one-thread form, which a 4K original decoding from
    // a long-GOP keyframe would pay for.
    let mut expected = built;
    cpu::limit_args(&mut expected, cpu::budget());
    assert_eq!(linux, expected);
    for args in [&windows, &linux] {
        assert_ne!(args[0], "-filter_threads");
        assert_eq!(count(args, "-hwaccel"), 1);
    }
}

#[test]
fn a_still_skips_hardware_for_a_chroma_format_the_decoders_lack() {
    let _serial = cpu::test_lock();
    let r5 = at_path("/media/r5.mp4", "hevc", "yuv422p10le");
    for os in [HostOs::Windows, HostOs::Other] {
        let (_, tuned, hw) = tuned_still(std::slice::from_ref(&r5), os);
        assert!(!hw, "{os:?}");
        assert_eq!(count(&tuned, "-hwaccel"), 0, "{os:?}");
    }
    let (_, mac, hw) = tuned_still(std::slice::from_ref(&r5), HostOs::MacOs);
    assert!(hw);
    assert_eq!(flag_val(&mac, "-hwaccel"), Some("auto"));
}

#[test]
fn a_still_mixing_a_proxy_and_an_original_tunes_each_input_alone() {
    let _serial = cpu::test_lock();
    let proxy = at_path(PROXY_PATH, "h264", "yuv420p");
    let original = at_path("/media/a.mp4", "h264", "yuv420p");
    let (_, tuned, hw) = tuned_still(&[proxy, original], HostOs::Windows);
    assert!(hw);
    let first = tuned.iter().position(|a| a == "-i").unwrap();
    let second = tuned.iter().rposition(|a| a == "-i").unwrap();
    assert_eq!(
        (tuned[first + 1].as_str(), tuned[second + 1].as_str()),
        (PROXY_PATH, "/media/a.mp4")
    );
    // The proxy's input: no hardware, one decoder thread. The original's: the decoder and
    // the thread caps of the budget, which the one-thread form of an all-proxy still is not.
    assert!(!tuned[..first].contains(&"-hwaccel".to_string()));
    assert_eq!(&tuned[first - 2..first], ["-threads", "1"]);
    let hw_at = tuned.iter().position(|a| a == "-hwaccel").unwrap();
    assert!(first < hw_at && hw_at < second, "{tuned:?}");
    assert_eq!(tuned[hw_at + 1], "d3d11va");
    assert_ne!(tuned[0], "-filter_threads");
}

#[test]
fn an_export_is_tuned_per_input_at_spawn_and_says_whether_hardware_is_left() {
    let build = |assets: &[Asset], hwaccel: Option<&str>| {
        let tracks = assets
            .iter()
            .map(|a| video_track(vec![make_clip(a.id, 0.0, 5.0, 0.0)]))
            .collect();
        let opts = ExportOptions {
            hwaccel: hwaccel.map(String::from),
            ..ExportOptions::default()
        };
        build_export_args(&timeline_of(tracks), assets, "/out.mp4", &opts).unwrap()
    };
    let export_args = |assets: &[Asset]| build(assets, Some("auto"));
    let hevc = at_path("/media/a.mp4", "hevc", "yuv420p10le");
    let built = export_args(std::slice::from_ref(&hevc));
    assert_eq!(flag_val(&built, "-hwaccel"), Some("auto"));

    let mut windows = built.clone();
    assert!(tune_export_args(&mut windows, std::slice::from_ref(&hevc), HostOs::Windows));
    assert_eq!(flag_val(&windows, "-hwaccel"), Some("d3d11va"));
    // Nothing else moved.
    let mut back = windows.clone();
    let at = back.iter().position(|a| a == "-hwaccel").unwrap();
    back[at + 1] = "auto".into();
    assert_eq!(back, built);

    let mut linux = built.clone();
    assert!(tune_export_args(&mut linux, std::slice::from_ref(&hevc), HostOs::Other));
    assert_eq!(linux, built);

    // A render is a long read: 4:2:2 still tries the hardware.
    let r5 = at_path("/media/r5.mp4", "hevc", "yuv422p10le");
    let mut render = export_args(std::slice::from_ref(&r5));
    assert!(tune_export_args(&mut render, std::slice::from_ref(&r5), HostOs::Other));
    assert_eq!(flag_val(&render, "-hwaccel"), Some("auto"));

    // A proxy is never decoded in hardware, whatever reads it: nothing is left to blame.
    let proxy = at_path(PROXY_PATH, "h264", "yuv420p");
    let mut from_proxy = export_args(std::slice::from_ref(&proxy));
    assert!(!tune_export_args(
        &mut from_proxy,
        std::slice::from_ref(&proxy),
        HostOs::Windows
    ));
    assert_eq!(count(&from_proxy, "-hwaccel"), 0);

    // So a run that was handed no hardware has no software rerun to make: the argv rebuilt
    // with no `hwaccel` is the tuned one, byte for byte (a proxy, and an image-only timeline).
    assert_eq!(from_proxy, build(std::slice::from_ref(&proxy), None));
    let card = img_asset(Uuid::new_v4());
    let mut from_image = export_args(std::slice::from_ref(&card));
    assert!(!tune_export_args(
        &mut from_image,
        std::slice::from_ref(&card),
        HostOs::Windows
    ));
    assert_eq!(from_image, build(std::slice::from_ref(&card), None));
    // And one that was handed some does have something to change.
    assert_ne!(windows, build(std::slice::from_ref(&hevc), None));
}

#[test]
fn a_whole_file_read_of_an_asset_is_resolved_from_what_the_import_recorded() {
    let auto = || Some("auto".to_string());
    let h264 = at_path("/media/a.mp4", "h264", "yuv420p");
    assert_eq!(hwaccel_for_asset(auto(), HostOs::Windows, &h264).as_deref(), Some("d3d11va"));
    assert_eq!(hwaccel_for_asset(auto(), HostOs::Other, &h264).as_deref(), Some("auto"));
    assert_eq!(hwaccel_for_asset(auto(), HostOs::MacOs, &h264).as_deref(), Some("auto"));
    // Windows keeps `auto` for a codec D3D11VA has no decoder for.
    let prores = at_path("/media/a.mov", "prores", "yuv422p10le");
    assert_eq!(hwaccel_for_asset(auto(), HostOs::Windows, &prores).as_deref(), Some("auto"));
    // 4:2:2 is read whole in hardware (the chroma gate is for one frame).
    let r5 = at_path("/media/r5.mp4", "hevc", "yuv422p10le");
    assert_eq!(hwaccel_for_asset(auto(), HostOs::Windows, &r5).as_deref(), Some("d3d11va"));
    // Named, nothing requested, and a proxy.
    assert_eq!(
        hwaccel_for_asset(Some("cuda".into()), HostOs::Windows, &h264).as_deref(),
        Some("cuda")
    );
    assert_eq!(hwaccel_for_asset(None, HostOs::Windows, &h264), None);
    assert_eq!(hwaccel_for_asset(Some("none".into()), HostOs::Windows, &h264), None);
    let proxy = at_path(PROXY_PATH, "h264", "yuv420p");
    assert_eq!(hwaccel_for_asset(auto(), HostOs::Windows, &proxy), None);
}

#[test]
fn an_export_that_fails_in_hardware_decode_is_rerun_in_software_and_blames_it() {
    let opts = ExportOptions {
        hwaccel: Some("d3d11va".to_string()),
        ..ExportOptions::default()
    };
    // What ffmpeg 9.0.2 says with no device behind a named accelerator.
    let no_device = || {
        Err(Error::Engine(
            "ffmpeg exited with exit status: 244: Device creation failed: -12.\nHardware device setup failed for decoder".into(),
        ))
    };
    let blamed = std::cell::Cell::new(0);
    let mut seen: Vec<Option<String>> = Vec::new();

    // Hardware was handed to an input, the run failed the way a missing device fails, and the
    // software run completes: rerun, and the hardware is blamed.
    let result = retry_export_in_software(
        &opts,
        |o, hw_used| {
            seen.push(o.hwaccel.clone());
            if o.hwaccel.is_some() {
                hw_used.set(true);
                no_device()
            } else {
                Ok(RenderStatus::Completed)
            }
        },
        || blamed.set(blamed.get() + 1),
    );
    assert_eq!(result.unwrap(), RenderStatus::Completed);
    assert_eq!(seen, [Some("d3d11va".to_string()), None]);
    assert_eq!(blamed.get(), 1);

    // The same run failing some other way (a stall, a locked output): still rerun, as ever,
    // and the rerun completes, but nothing says the hardware did it, so it is not blamed.
    for other in [
        "ffmpeg stopped reporting progress for 300s and was stopped: frame= 120",
        "ffmpeg exited with exit status: 1: Error opening output file /out/a.mp4\nPermission denied",
    ] {
        seen.clear();
        let result = retry_export_in_software(
            &opts,
            |o, hw_used| {
                seen.push(o.hwaccel.clone());
                if o.hwaccel.is_some() {
                    hw_used.set(true);
                    Err(Error::Engine(other.into()))
                } else {
                    Ok(RenderStatus::Completed)
                }
            },
            || blamed.set(blamed.get() + 1),
        );
        assert_eq!(result.unwrap(), RenderStatus::Completed);
        assert_eq!(seen.len(), 2, "{other}");
    }
    assert_eq!(blamed.get(), 1);

    // Hardware was requested but no input was handed it (a proxy, an image): the failure is
    // the render's own, so no rerun and no blame — the rerun would be the same argv.
    seen.clear();
    let result = retry_export_in_software(
        &opts,
        |o, _| {
            seen.push(o.hwaccel.clone());
            no_device()
        },
        || blamed.set(blamed.get() + 1),
    );
    assert!(result.is_err());
    assert_eq!(seen.len(), 1);
    assert_eq!(blamed.get(), 1);

    // The rerun fails too, or is cancelled: not the hardware's doing.
    for rerun in [Err(Error::Engine("disk full".into())), Ok(RenderStatus::Cancelled)] {
        let mut rerun = Some(rerun);
        let _ = retry_export_in_software(
            &opts,
            |o, hw_used| {
                if o.hwaccel.is_some() {
                    hw_used.set(true);
                    no_device()
                } else {
                    rerun.take().unwrap()
                }
            },
            || blamed.set(blamed.get() + 1),
        );
    }
    assert_eq!(blamed.get(), 1);

    // A run that works is left alone.
    let result = retry_export_in_software(
        &opts,
        |_, hw_used| {
            hw_used.set(true);
            Ok(RenderStatus::Completed)
        },
        || blamed.set(blamed.get() + 1),
    );
    assert_eq!(result.unwrap(), RenderStatus::Completed);
    assert_eq!(blamed.get(), 1);
}

/// Against the real binary: a named accelerator that cannot get a device exits non-zero with
/// text the classifier reads as the hardware's, where `auto` falls back and succeeds — the
/// reason a named `d3d11va` needs the learned fallback `auto` never did.
/// `cargo test -p kerf-core --no-default-features -- --ignored no_device`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_named_accelerator_with_no_device_fails_in_words_the_classifier_reads() {
    let dir = Scratch::new("no-device");
    let media = dir.join("a.mp4");
    run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=10:duration=1",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        media.to_str().unwrap(),
    ]);
    // At `info`, the level whose `auto` header the classifier must not mistake for a failure.
    let decode = |accel: &str| {
        command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "info", "-hwaccel", accel, "-i"])
            .arg(&media)
            .args(["-frames:v", "1", "-f", "null", "-"])
            .output()
            .expect("run ffmpeg")
    };
    let mut failed = 0;
    for accel in ["vaapi", "cuda", "vdpau", "d3d11va", "dxva2", "videotoolbox", "qsv"] {
        let out = decode(accel);
        // A machine that has the device (or an ffmpeg built with the name) has nothing to learn.
        if out.status.success() {
            continue;
        }
        failed += 1;
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(looks_like_hardware_decode_failure(&text), "{accel}: {text}");
    }
    // `auto` falls back quietly — except on a machine where the pinned build's shim for a
    // hardware library (`libva-drm.so.2`) cannot load it, which aborts the run: that must be
    // read as the hardware's too, and there is then no probing `auto` run left to check.
    let auto = decode("auto");
    if !auto.status.success() {
        let text = String::from_utf8_lossy(&auto.stderr);
        assert!(looks_like_hardware_decode_failure(&text), "auto: {text}");
        skip("skipped the output check: `auto` itself fails here, as the hardware's failure");
        return;
    }
    // The probing `auto` prints (`Device creation failed`, `Using auto hwaccel type ...`) is
    // not a failure: a run that then dies on its output is not the hardware's.
    let out = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "info", "-hwaccel", "auto", "-i"])
        .arg(&media)
        .args(["-frames:v", "1"])
        .arg(dir.join("no-such-dir").join("out.mp4"))
        .output()
        .expect("run ffmpeg");
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("Error opening output"), "{text}");
    assert!(!looks_like_hardware_decode_failure(&text), "{text}");
    if failed == 0 {
        skip("skipped: every accelerator worked here");
    }
}

/// A recording stand-in for ffmpeg: writes the argv it was given, one per line, and answers
/// with a byte of "image".
#[cfg(unix)]
fn argv_of_frame_decode(dir: &Path, proxy: bool, hw: Option<&str>) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let rec = dir.join("rec.sh");
    let out = dir.join("rec.out");
    std::fs::write(
        &rec,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {}\nprintf x\n",
            out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&rec, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bytes = run_frame_decode(
        rec.to_str().unwrap(),
        Path::new("/media/a.mp4"),
        3.0,
        "scale=320:-2",
        "mjpeg",
        Some(4),
        true,
        hw,
        proxy,
        None,
    )
    .unwrap();
    assert_eq!(bytes, b"x");
    std::fs::read_to_string(out).unwrap().lines().map(String::from).collect()
}

#[cfg(unix)]
#[test]
fn a_frame_grab_of_a_proxy_is_one_thread_and_of_an_original_the_budget() {
    let _serial = cpu::test_lock();
    let dir = std::env::temp_dir().join(format!("kerf-frame-argv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let proxy = argv_of_frame_decode(&dir, true, None);
    assert_eq!(
        proxy.join(" "),
        "-threads 1 -filter_threads 1 -filter_complex_threads 1 -hide_banner -loglevel error -ss 3.000 -i /media/a.mp4 \
             -frames:v 1 -vf scale=320:-2 -q:v 4 -f image2pipe -vcodec mjpeg -threads 1 pipe:1"
    );
    // An original keeps its hardware decoder, and no encoder thread cap of its own.
    let original = argv_of_frame_decode(&dir, false, Some("auto"));
    assert_eq!(count(&original, "-hwaccel"), 1);
    assert_eq!(&original[original.len() - 3..], ["-vcodec", "mjpeg", "pipe:1"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_frame_decode_a_caller_cancels_is_killed_and_is_cancelled_not_a_failure() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = cpu::test_lock();
    let dir = std::env::temp_dir().join(format!("kerf-frame-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let slow = dir.join("slow-ffmpeg");
    std::fs::write(&slow, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();

    let stop = std::sync::atomic::AtomicBool::new(false);
    let started = Instant::now();
    let err = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(250));
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let cancel = || stop.load(std::sync::atomic::Ordering::SeqCst);
        run_frame_decode(
            slow.to_str().unwrap(),
            Path::new("/media/a.mp4"),
            3.0,
            "scale=320:-2",
            "mjpeg",
            Some(4),
            true,
            None,
            false,
            Some(&cancel),
        )
    })
    .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );

    // Already cancelled: not even started, hit or miss.
    let err = decode_frame(
        Path::new("/media/a.mp4"),
        1.0,
        "scale=320:-2",
        "mjpeg",
        Some(4),
        true,
        Some(&|| true),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    let err = audio_pcm_cancellable(Path::new("/media/a.mp4"), 0.0, 1.0, 8_000, None, &|| true).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn source_traits_carry_the_codec_and_chroma_the_hardware_decision_reads() {
    let probe = |codec: &str, pix_fmt: &str| {
        parse_source_traits(&format!(
            r#"{{"streams":[{{"codec_name":"{codec}","pix_fmt":"{pix_fmt}"}}],"format":{{"format_name":"mov,mp4,m4a,3gp,3g2,mj2"}}}}"#
        ))
    };
    let iphone = probe("hevc", "yuv420p10le");
    assert_eq!((iphone.codec, iphone.chroma), (Codec::Hevc, Chroma::Yuv420));
    let r5 = probe("hevc", "yuv422p10le");
    assert_eq!((r5.codec, r5.chroma), (Codec::Hevc, Chroma::Other));
    assert_eq!(probe("prores", "yuv422p10le").codec, Codec::Other);
    // A stream that says nothing, and a probe that failed, assume nothing.
    let silent = parse_source_traits(r#"{"streams":[{}],"format":{}}"#);
    assert_eq!((silent.codec, silent.chroma), (Codec::Unknown, Chroma::Unknown));
    assert_eq!(parse_source_traits("not json"), SourceTraits::default());
}

#[test]
fn a_proxy_is_known_by_its_name_without_a_probe() {
    let facts = path_facts(Path::new(PROXY_PATH));
    assert!(facts.proxy);
    assert_eq!((facts.codec, facts.chroma), (Codec::Unknown, Chroma::Unknown));
}

/// Against the real binary: what the spawn-time tuning of a proxy read changes is the
/// cost of the run and not one byte of the picture — the scrubbed still, the frame grab
/// and the playback stream, all handed `-hwaccel auto` as the app does.
/// `cargo test -p kerf-core --no-default-features -- --ignored tuned_proxy_reads`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn tuned_proxy_reads_are_the_same_picture_as_the_untuned_ones() {
    let _serial = cpu::test_lock();
    let dir = Scratch::new("tuned-proxy");
    let proxies = dir.join("kerf").join("proxies");
    std::fs::create_dir_all(&proxies).unwrap();
    let proxy = proxies.join("0123456789abcdef.mp4");
    // What `build_proxy_args` makes: all-intra H.264, 4:2:0.
    run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=30:duration=3",
        "-c:v",
        "libx264",
        "-g",
        "1",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "mp4",
        proxy.to_str().unwrap(),
    ]);
    let mut asset = at_path(proxy.to_str().unwrap(), "h264", "yuv420p");
    asset.streams[0].width = Some(1280);
    asset.streams[0].height = Some(720);
    asset.duration = 3.0;
    let assets = [asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 0.0, 3.0, 0.0)]);
    let jpeg = |bytes: &[u8]| bytes.len() > 1000 && bytes[..2] == [0xFF, 0xD8];

    // The composited still: the argv as built plus the budget's caps (what ran before)
    // against the one that is spawned now.
    let built = build_timeline_frame_args(&timeline, &assets, &hw_still_opts(), 1.5, 640, 4).unwrap();
    let run = |args: &[String]| command(&ffmpeg_bin()).args(args).output().expect("run ffmpeg");
    let spawn = |args: &[String]| {
        let out = run(args);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    let mut untuned = built.clone();
    cpu::limit_args(&mut untuned, cpu::budget());
    let mut tuned = built;
    assert!(!tune_still_args(&mut tuned, &assets, HostOs::CURRENT));
    assert_ne!(untuned, tuned);
    // What ran before asked the proxy for `-hwaccel auto`. On a machine where that run dies
    // (the pinned build aborts when `libva-drm.so.2` is missing) the baseline is what the
    // software rerun of production produced: the same still with no hardware asked for.
    let first = run(&untuned);
    let old = if first.status.success() {
        first.stdout
    } else {
        let text = String::from_utf8_lossy(&first.stderr);
        assert!(looks_like_hardware_decode_failure(&text), "{text}");
        let mut software = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 1.5, 640, 4).unwrap();
        cpu::limit_args(&mut software, cpu::budget());
        spawn(&software)
    };
    let new = spawn(&tuned);
    assert!(jpeg(&new));
    assert_eq!(old, new, "the still differs");
    let options = hw_still_opts();
    assert_eq!(
        timeline_frame_cancellable(&timeline, &assets, &options, 1.5, 640, 4, &|| false).unwrap(),
        new
    );

    // The frame grab: `frame_jpeg` decides for itself from the path it is given.
    let vf = "scale='min(320,iw)':-2";
    let grab = frame_jpeg(&proxy, 1.5, 320, 4, true).unwrap();
    assert!(jpeg(&grab));
    let untuned = run_frame_decode(&ffmpeg_bin(), &proxy, 1.5, vf, "mjpeg", Some(4), true, None, false, None).unwrap();
    assert_eq!(grab, untuned, "the grab differs");

    // The stream: a second of frames, read from the proxy with the hardware dropped.
    let mut frames = 0;
    stream_preview(&timeline, &assets, 0.5, 24.0, &mut |f| {
        assert!(jpeg(&f.jpeg));
        frames += 1;
        frames < 12
    })
    .expect("stream");
    assert_eq!(frames, 12);
}

#[test]
fn cover_frame_renders_to_a_file_at_full_delivery_size() {
    let asset = test_asset(vec![video_stream(3840, 2160, 30.0)]);
    let assets = vec![asset.clone()];
    let mut timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    timeline.format = Some(crate::model::Delivery {
        width: 1080,
        height: 1920,
        fit: Fit::Cover,
        fps: None,
    });
    let out = StillOutput::File {
        path: "/covers/cover.jpg".to_string(),
        format: ImageFormat::Jpeg,
        quality: 2,
    };
    let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, u32::MAX, None, &out).unwrap();
    // The uncapped width resolves to the project's delivery frame, not to a
    // preview size — a cover is a delivered image.
    let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
    assert!(graph.contains("s=1080x1920"), "canvas is the delivery frame: {graph}");
    // Written as a real file, overwriting, with the muxer stated.
    assert!(args.windows(2).any(|w| w[0] == "-f" && w[1] == "image2"));
    assert!(args.windows(2).any(|w| w[0] == "-vcodec" && w[1] == "mjpeg"));
    assert!(args.windows(2).any(|w| w[0] == "-q:v" && w[1] == "2"));
    assert!(args.contains(&"-y".to_string()));
    assert!(!args.contains(&"pipe:1".to_string()));
    assert_eq!(args.last().unwrap(), "/covers/cover.jpg");
    // Exactly one frame, whichever sink it goes to.
    assert!(args.windows(2).any(|w| w[0] == "-frames:v" && w[1] == "1"));
}

#[test]
fn a_png_cover_is_lossless_and_carries_no_jpeg_quality() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    let out = StillOutput::File {
        path: "/covers/cover.png".to_string(),
        format: ImageFormat::Png,
        quality: 2,
    };
    let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, u32::MAX, None, &out).unwrap();
    assert!(args.windows(2).any(|w| w[0] == "-vcodec" && w[1] == "png"));
    assert!(!args.contains(&"-q:v".to_string()), "-q:v is meaningless for png: {args:?}");
}

#[test]
fn the_scopes_still_pipes_raw_yuv444_and_is_otherwise_the_jpeg_still() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    let o = ExportOptions::default();
    let yuv = StillOutput::Yuv444Pipe { width: 960, height: 540 };
    let args = build_still_args(&timeline, &assets, &o, 1.0, 960, None, &yuv).unwrap();
    let tail = ["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv444p", "pipe:1"];
    assert_eq!(&args[args.len() - tail.len()..], tail.map(String::from).as_slice());
    let jpeg = build_still_args(&timeline, &assets, &o, 1.0, 960, None, &StillOutput::JpegPipe { quality: 3 }).unwrap();
    let head = |a: &[String]| a[..a.iter().position(|x| x == "-frames:v").unwrap()].to_vec();
    assert_eq!(head(&args), head(&jpeg), "only the sink differs");
    // The size only sizes the read buffer.
    let other = StillOutput::Yuv444Pipe { width: 1, height: 1 };
    assert_eq!(
        build_still_args(&timeline, &assets, &o, 1.0, 960, None, &other).unwrap(),
        args
    );
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_cover_frame_is_really_written_at_the_delivery_size() {
    // The arg builder has unit tests, but nothing above ever ran the binary
    // — and a still sink that never produced a file would look identical.
    let dir = std::env::temp_dir().join(format!("kerf-cover-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30:duration=2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    let mut asset = av_asset(Uuid::new_v4(), 2.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(1920, 1080, 30.0)];
    let mut timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);
    timeline.format = Some(crate::model::Delivery {
        width: 1080,
        height: 1350,
        fit: Fit::Cover,
        fps: None,
    });

    for (name, format) in [("cover.jpg", ImageFormat::Jpeg), ("cover.png", ImageFormat::Png)] {
        let out = dir.join(name);
        export_still(&timeline, &[asset.clone()], &ExportOptions::default(), 1.0, &out, format, 2).expect("cover");
        let size = std::fs::metadata(&out).expect("cover written").len();
        assert!(size > 1024, "{name} should be a real image, got {size} bytes");
        // And it is the delivery frame, not the source shape.
        let probe = probe(&out).expect("probe the cover");
        let stream = probe.streams.first().expect("a video stream");
        assert_eq!((stream.width, stream.height), (Some(1080), Some(1350)), "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cover_format_follows_the_file_extension() {
    assert_eq!(ImageFormat::from_path(Path::new("/a/cover.png")), ImageFormat::Png);
    assert_eq!(ImageFormat::from_path(Path::new("/a/cover.PNG")), ImageFormat::Png);
    assert_eq!(ImageFormat::from_path(Path::new("/a/cover.jpg")), ImageFormat::Jpeg);
    // Anything else falls back to the format every platform accepts.
    assert_eq!(ImageFormat::from_path(Path::new("/a/cover")), ImageFormat::Jpeg);
}

#[test]
fn insta360_lens_pairs_the_two_capture_files() {
    // Either lens resolves to the other, and to one shared display name.
    let front = "VID_20220625_140410_00_008.mp4";
    let rear = "VID_20220625_140410_10_008.mp4";
    assert_eq!(insta360_lens(front), Some(("00", rear.to_string())));
    assert_eq!(insta360_lens(rear), Some(("10", front.to_string())));
    assert_eq!(insta360_pair_name(front).as_deref(), Some("VID_20220625_140410_008.mp4"));
    assert_eq!(insta360_pair_name(rear).as_deref(), Some("VID_20220625_140410_008.mp4"));
}

#[test]
fn insta360_lens_ignores_everything_else() {
    // The lens token is matched positionally, so digits elsewhere in the
    // name — a time ending in 10, a clip numbered 00 — are not lens tokens.
    for name in [
        "VID_20220625_141000_008.mp4",
        "VID_20220625_140410_20_008.mp4",
        "VID_20220625_140410_00_008.mov",
        "MVI_20220625_140410_00_008.mp4",
        "holiday.mp4",
        "VID_00_.mp4",
    ] {
        assert_eq!(insta360_lens(name), None, "{name} must not read as a lens file");
    }
}

#[test]
fn stitch_args_reproject_the_lens_pair_to_equirect() {
    let args = build_stitch_args("/dcim/front_00_.mp4", "/dcim/rear_10_.mp4", "/cache/out.mp4", "libx264", None);
    // Front lens is input 0 — hstack packs it into the left half, which is
    // the front hemisphere `v360=dfisheye` expects.
    let inputs: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, _)| *i > 0 && args[i - 1] == "-i")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(inputs, vec!["/dcim/front_00_.mp4", "/dcim/rear_10_.mp4"]);
    let graph = args
        .iter()
        .position(|a| a == "-filter_complex")
        .map(|i| args[i + 1].clone())
        .expect("-filter_complex present");
    assert!(graph.contains("hstack=shortest=1"));
    assert!(graph.contains("v360=dfisheye:e"));
    assert!(graph.contains("ih_fov=190"), "the lenses overshoot the hemisphere");
    assert!(graph.contains("roll=180"), "both lenses record upside down");
    assert!(graph.contains("w=5760:h=2880"));
    // The capture's audio rides along untouched; the output is the stitch.
    assert!(args.windows(2).any(|w| w[0] == "-map" && w[1] == "0:a?"));
    assert!(args.windows(2).any(|w| w[0] == "-c:a" && w[1] == "copy"));
    assert_eq!(args.last().unwrap(), "/cache/out.mp4");
}

#[test]
fn stitched_path_is_shared_by_both_lens_orders() {
    // The pair is keyed by both files, so whichever lens was imported the
    // cached stitch is the same file (the second import is a cache hit).
    if let (Some(a), Some(b), Some(other)) = (
        stitched_path(Path::new("/dcim/a_00_.mp4"), Path::new("/dcim/a_10_.mp4")),
        stitched_path(Path::new("/dcim/a_00_.mp4"), Path::new("/dcim/a_10_.mp4")),
        stitched_path(Path::new("/dcim/b_00_.mp4"), Path::new("/dcim/b_10_.mp4")),
    ) {
        assert_eq!(a, b);
        assert_ne!(a, other);
        assert!(a.to_string_lossy().contains("stitched"));
    }
}

#[test]
fn insta360_pair_needs_square_frames_and_a_sibling_on_disk() {
    // Non-square (already stitched, or ordinary video) is never a lens file,
    // and a lone lens file with no sibling next to it can't be stitched.
    assert_eq!(
        insta360_pair(Path::new("/dcim/VID_1_2_00_3.mp4"), Some(5760), Some(2880)),
        None
    );
    assert_eq!(
        insta360_pair(Path::new("/dcim/VID_1_2_00_3.mp4"), Some(3072), Some(3072)),
        None,
        "no sibling on disk"
    );
}

#[test]
fn proxy_path_is_deterministic_and_distinct_per_source() {
    // Same source → same proxy path on every call (so a re-import / new
    // session reuses the cached proxy); different sources → different files.
    // Skipped when the platform exposes no cache directory.
    if let (Some(a1), Some(a2), Some(b)) = (
        proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH),
        proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH),
        proxy_path(Path::new("/media/b.mov"), PROXY_MAX_WIDTH),
    ) {
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert!(a1.to_string_lossy().contains("proxies"));
        assert_eq!(a1.extension().and_then(|e| e.to_str()), Some("mp4"));
    }
}

#[test]
fn an_encode_names_its_proxy_from_the_traits_it_was_given() {
    // One reading answers the destination, the tone-map and the pad: what the destination is
    // follows from those traits alone, and a source that cannot be probed is the plain one.
    let src = Path::new("/media/never-imported.mov");
    let plain = SourceTraits::default();
    let hlg = SourceTraits {
        hdr: Some(Hdr::Hlg),
        ..plain
    };
    let late = SourceTraits { lead: 0.5, ..plain };
    let (Some(a), Some(b), Some(c), Some(unreadable)) = (
        proxy_path_for(src, PROXY_MAX_WIDTH, plain),
        proxy_path_for(src, PROXY_MAX_WIDTH, hlg),
        proxy_path_for(src, PROXY_MAX_WIDTH, late),
        proxy_path(src, PROXY_MAX_WIDTH),
    ) else {
        skip("skipped: this platform has no cache directory for proxies");
        return;
    };
    assert_eq!(a, unreadable);
    assert!(a != b && a != c && b != c);
    assert!(is_head_padded_proxy(&c.to_string_lossy()) && !is_head_padded_proxy(&a.to_string_lossy()));
    assert_eq!(Some(&b), proxy_path_for(src, PROXY_MAX_WIDTH, hlg).as_ref());
}

#[test]
fn the_cache_cap_knows_every_file_a_source_s_proxy_could_be_without_probing() {
    let src = Path::new("/media/never-imported.mov");
    let files = proxy_cache_files(src, PROXY_MAX_WIDTH);
    if files.is_empty() {
        skip("skipped: this platform has no cache directory for proxies");
        return;
    }
    let tonemapped = SourceTraits {
        hdr: Some(Hdr::Hlg),
        ..SourceTraits::default()
    };
    let late = SourceTraits {
        lead: 0.5,
        ..SourceTraits::default()
    };
    for traits in [SourceTraits::default(), tonemapped, late] {
        let proxy = proxy_path_for(src, PROXY_MAX_WIDTH, traits).unwrap();
        assert!(files.contains(&proxy), "{}", proxy.display());
        assert!(files.contains(&proxy_sidecar_path(&proxy)), "its sidecar too");
    }
    let unique: std::collections::HashSet<_> = files.iter().collect();
    assert_eq!(unique.len(), 8, "four spellings, a proxy and a sidecar each");
    assert!(
        !proxy_cache_files(src, PROXY_MAX_WIDTH_SPHERICAL)
            .iter()
            .any(|f| files.contains(f)),
        "another width is other files"
    );
}

#[test]
fn spherical_sources_proxy_larger_and_under_their_own_key() {
    // Reframing crops ~100° out of 360, so a 360 proxy keeps more pixels.
    assert_eq!(proxy_width(None), PROXY_MAX_WIDTH);
    assert_eq!(proxy_width(Some(Projection::Flat)), PROXY_MAX_WIDTH);
    assert_eq!(proxy_width(Some(Projection::Equirect)), PROXY_MAX_WIDTH_SPHERICAL);
    assert_eq!(proxy_width(Some(Projection::DualFisheye)), PROXY_MAX_WIDTH_SPHERICAL);
    assert!(build_proxy_args(
        "/in.mp4",
        "/out.mp4",
        1,
        PROXY_MAX_WIDTH_SPHERICAL,
        "libx264",
        None,
        None,
        None
    )
    .iter()
    .any(|a| a.contains("scale='min(3072,iw)':-2")));
    // Marking an asset as 360 must not silently reuse the small proxy that
    // was rendered while it looked flat, so the width is part of the key.
    if let (Some(flat), Some(sphere)) = (
        proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH),
        proxy_path(Path::new("/media/a.mov"), PROXY_MAX_WIDTH_SPHERICAL),
    ) {
        assert_ne!(flat, sphere);
    }
}

#[test]
fn a_smaller_proxy_size_scales_a_360_proxy_by_the_same_ratio_and_the_default_is_unchanged() {
    // The default is what Kerf always made, so its cache stays valid.
    assert_eq!(proxy_width_for(None, 1280), 1280);
    assert_eq!(proxy_width_for(Some(Projection::Equirect), 1280), PROXY_MAX_WIDTH_SPHERICAL);
    assert_eq!(proxy_width_for(Some(Projection::Flat), 720), 720);
    // 3072 / 1280 = 2.4: a 720 flat proxy is a 1728 equirect one, 1080 is 2592.
    assert_eq!(proxy_width_for(Some(Projection::Equirect), 720), 1728);
    assert_eq!(proxy_width_for(Some(Projection::DualFisheye), 1080), 2592);
    // …and never past what a hardware H.264 encoder takes.
    assert_eq!(proxy_width_for(Some(Projection::Equirect), 4000), PROXY_MAX_WIDTH_SPHERICAL);
    // Each size is its own file: the width is in the key.
    if let (Some(a), Some(b)) = (
        proxy_path(Path::new("/media/a.mov"), 720),
        proxy_path(Path::new("/media/a.mov"), 1280),
    ) {
        assert_ne!(a, b);
    }
}

#[test]
fn a_hardware_decoded_proxy_that_stalls_is_retried_in_software_like_one_that_fails() {
    use StreamOutcome::*;
    // Failed on its own or stalled, through hardware decode: go again in software.
    assert!(retry_proxy_in_software(Ended, false, true));
    assert!(
        retry_proxy_in_software(Stalled, false, true),
        "a hung hardware decoder is as broken as a refusing one"
    );
    // Not when it worked, when the user cancelled, or when it was software already.
    assert!(!retry_proxy_in_software(Ended, true, true));
    assert!(!retry_proxy_in_software(Cancelled, false, true));
    assert!(!retry_proxy_in_software(Stalled, false, false));
    assert!(!retry_proxy_in_software(Ended, false, false));
}

#[test]
fn a_failed_proxy_says_why_in_a_few_lines_however_much_ffmpeg_wrote() {
    // The log of a clip that is zeros after its first seconds: four lines a frame, 2800 frames.
    let mut log = String::new();
    for i in 0..2800 {
        log.push_str(&format!(
            "[hevc @ 000002b4a68062c0] Invalid NAL unit size (0 > {}).\r\n",
            300_000 + i
        ));
        log.push_str("[hevc @ 000002b4a68062c0] Error splitting the input into NAL units.\r\n");
        log.push_str("[vist#0:0/hevc @ 000002b4a4ad7880] [dec:hevc @ 000002b4a4ad8580] Error submitting packet to decoder: Invalid data found when processing input\r\n");
    }
    log.push_str("[vist#0:0/hevc @ 000002b4a4ad7880] [dec:hevc @ 000002b4a4ad8580] Decode error rate 0.851449 exceeds maximum 0.666667\r\n");
    let reason = super::proxy::proxy_failure_reason(&log);
    assert!(reason.starts_with("the file is damaged"), "{reason}");
    assert!(
        reason.contains("Decode error rate 0.851449"),
        "the end of the log is kept: {reason}"
    );
    assert!(reason.len() < 1600, "{} bytes", reason.len());
    assert!(!reason.contains('\r'));

    // Any other failure is the end of its log, as it was, and is not called a damaged file.
    let reason =
        super::proxy::proxy_failure_reason("  \nOutput file is empty\nav_interleaved_write_frame(): No space left on device\n");
    assert_eq!(
        reason,
        "Output file is empty\nav_interleaved_write_frame(): No space left on device"
    );
}

/// A child that says nothing is killed after the stall allowance, and a cancel kills one
/// that is still talking — neither waits for the process to finish by itself. (The child is
/// a script standing in for ffmpeg: it ignores the `-progress …` flags it is handed.)
#[cfg(unix)]
#[test]
fn a_silent_encode_is_killed_after_the_stall_allowance_and_a_cancel_kills_a_talker() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("stall");
    let script = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    };
    let silent = script("silent.sh", "exec sleep 30");
    let started = Instant::now();
    let stalled = run_ffmpeg_streamed(
        &silent,
        &[],
        None,
        std::time::Duration::from_millis(700),
        &mut |_| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(stalled.outcome, StreamOutcome::Stalled);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "it waited for the child to end by itself"
    );
    assert!(!stalled.status.success());

    let talker = script(
        "talker.sh",
        "i=0; while [ $i -lt 100 ]; do echo out_time_us=$((i*100000)); i=$((i+1)); sleep 0.1; done",
    );
    let started = Instant::now();
    let flag = std::sync::atomic::AtomicBool::new(false);
    let mut seen = Vec::new();
    let cancelled = run_ffmpeg_streamed(
        &talker,
        &[],
        Some(10.0),
        std::time::Duration::from_secs(60),
        &mut |f| {
            seen.push(f);
            if seen.len() >= 3 {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        },
        &|| flag.load(std::sync::atomic::Ordering::SeqCst),
    )
    .unwrap();
    assert_eq!(cancelled.outcome, StreamOutcome::Cancelled);
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]) && seen.iter().all(|f| (0.0..1.0).contains(f)),
        "{seen:?}"
    );
}

/// An export that has to wait for another heavy job says so in its progress, and Stop works
/// on it before ffmpeg is ever started — no binary needed.
#[test]
fn a_queued_export_says_it_is_waiting_and_can_be_cancelled_before_it_starts() {
    let _serial = cpu::test_lock();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    // A proxy encode is running: it holds the slot.
    let proxy = std::thread::spawn(move || {
        let _lease = cpu::lease_priority(cpu::Priority::Proxy);
        held_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    held_rx.recv().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut told: Vec<Option<&'static str>> = Vec::new();
    let bar = Bar {
        total: 10.0,
        offset: 0.0,
        width: 1.0,
        start: Instant::now(),
    };
    let status = run_ffmpeg_progress(
        &["-version".to_string()],
        Path::new("/nonexistent/out.mp4"),
        bar,
        &mut |p| {
            told.push(p.waiting);
            assert_eq!(p.fraction, 0.0);
            cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        },
        &|| cancel.load(std::sync::atomic::Ordering::SeqCst),
    )
    .unwrap();
    assert_eq!(status, RenderStatus::Cancelled);
    assert_eq!(told, [Some("waiting for the preview proxy")]);
    release_tx.send(()).unwrap();
    proxy.join().unwrap();
}

/// The run that has stopped moving is stopped even while it goes on printing blocks (FFmpeg 7+
/// prints one every `-stats_period` whatever its graph does: the same frame, over and over), and
/// the error says where it was stuck; the run that moves slowly (a frame every 0.4 s against a
/// 1 s bound, the same block printed between) is never stopped. No ffmpeg needed.
#[cfg(unix)]
#[test]
fn a_run_that_stops_moving_is_stopped_and_a_slow_one_that_moves_is_not() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = cpu::test_lock();
    let dir = Scratch::new("stalls-like-ffmpeg");
    let run = |name: &str, body: &str| {
        let script = dir.join(name);
        std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let bar = Bar {
            total: 10.0,
            offset: 0.0,
            width: 1.0,
            start: Instant::now(),
        };
        let started = Instant::now();
        let ran = run_progress_stalling(
            script.to_str().unwrap(),
            &[],
            &[],
            Path::new("/nonexistent/out.mp4"),
            bar,
            cpu::Resource::Cpu,
            &mut |_| {},
            &|| false,
            &mut 0,
            std::time::Duration::from_secs(1),
        );
        (ran, started.elapsed())
    };
    // Stuck at frame 12 (0.48 s written), saying so five times a second for 30 s.
    let (ran, took) = run(
        "stuck.sh",
        "printf 'frame=12\\nout_time_us=480000\\nprogress=continue\\n'\n\
             for i in $(seq 150); do printf 'frame=12\\nout_time_us=480000\\nprogress=continue\\n'; sleep 0.2; done\n",
    );
    let why = ran.unwrap_err().to_string();
    assert!(took < std::time::Duration::from_secs(10), "stopped after {took:?}");
    assert!(why.contains("stuck at frame 12, 0.48 s of output"), "{why}");
    // A frame every 0.4 s, 3 s in all, with a repeated block between each: slow, but moving.
    let (ran, _) = run(
        "slow.sh",
        "for n in 1 2 3 4 5 6 7; do\n\
               printf \"frame=$n\\nout_time_us=${n}00000\\nprogress=continue\\n\"; sleep 0.2\n\
               printf \"frame=$n\\nout_time_us=${n}00000\\nprogress=continue\\n\"; sleep 0.2\n\
             done\n\
             printf 'frame=8\\nout_time_us=800000\\nprogress=end\\n'\n",
    );
    assert_eq!(ran.unwrap(), RenderStatus::Completed);
}

/// A child that talks like `ffmpeg -progress` — a block with no output position yet (it is
/// opening and seeking the footage), then the output's position through a 10 s cut, the last of
/// it past the end while the muxer closes the file — as the export's progress tells it: a
/// stage that says so before any output, a rising fraction on the pass's share of the bar, and
/// 99 % of the pass until ffmpeg has exited. No ffmpeg needed.
#[cfg(unix)]
#[test]
fn a_pass_tells_it_is_starting_then_rendering_then_finishing_and_never_runs_the_bar_backwards() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = cpu::test_lock();
    let dir = Scratch::new("talks-like-ffmpeg");
    let script = dir.join("ffmpeg.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
             printf 'frame=0\\nout_time_ms=N/A\\nout_time=N/A\\nprogress=continue\\n'; sleep 0.3\n\
             printf 'out_time_us=N/A\\nout_time_ms=N/A\\nprogress=continue\\n'; sleep 0.3\n\
             for us in 1000000 5000000 9500000 10000000 10120000; do\n\
               printf \"out_time_us=$us\\nprogress=continue\\n\"; sleep 0.1\n\
             done\n\
             printf 'out_time_us=10150000\\nprogress=end\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The second pass of a two-pass export: the second half of the bar, over a 10 s cut.
    let bar = Bar {
        total: 10.0,
        offset: 0.5,
        width: 0.5,
        start: Instant::now(),
    };
    let mut seen: Vec<ExportProgress> = Vec::new();
    let status = run_progress_with(
        script.to_str().unwrap(),
        &[],
        &[],
        Path::new("/nonexistent/out.mp4"),
        bar,
        cpu::Resource::Cpu,
        &mut |p| seen.push(p),
        &|| false,
    )
    .unwrap();
    assert_eq!(status, RenderStatus::Completed);
    let stages: Vec<_> = seen.iter().filter_map(|p| p.stage).collect();
    let mut distinct = stages.clone();
    distinct.dedup();
    assert_eq!(
        distinct,
        [ExportStage::Starting, ExportStage::Rendering, ExportStage::Finishing],
        "{stages:?}"
    );
    assert!(seen.iter().all(|p| p.waiting.is_none()));
    // The spawn says it, and so does each block before the first output: three beats of
    // "starting" at the pass's own start — not 0, not stuck on a stale word.
    let starting: Vec<f64> = seen
        .iter()
        .filter(|p| p.stage == Some(ExportStage::Starting))
        .map(|p| p.fraction)
        .collect();
    assert_eq!(starting, [0.5, 0.5, 0.5]);
    // The position is measured against the cut's length: 1 s of 10 is a tenth of the half.
    let rendering: Vec<f64> = seen
        .iter()
        .filter(|p| p.stage == Some(ExportStage::Rendering))
        .map(|p| p.fraction)
        .collect();
    assert!(
        (rendering[0] - 0.55).abs() < 1e-9 && (rendering[1] - 0.75).abs() < 1e-9,
        "{rendering:?}"
    );
    // Held under the end of the pass until ffmpeg exits, however far past the cut it says it is.
    let finishing: Vec<f64> = seen
        .iter()
        .filter(|p| p.stage == Some(ExportStage::Finishing))
        .map(|p| p.fraction)
        .collect();
    assert!(
        !finishing.is_empty() && finishing.iter().all(|f| (*f - 0.995).abs() < 1e-9),
        "{finishing:?}"
    );
    let all: Vec<f64> = seen.iter().map(|p| p.fraction).collect();
    assert!(all.windows(2).all(|w| w[0] <= w[1]), "the bar went backwards: {all:?}");
    assert!(all.iter().all(|f| *f < 1.0), "{all:?}");
    // Time left appears once a rate does, and never before the first output.
    assert!(seen
        .iter()
        .filter(|p| p.stage == Some(ExportStage::Starting))
        .all(|p| p.eta_secs.is_none()));
}

/// The streamed encode reports a rising fraction under 1 and can be abandoned: a cancel
/// kills the encode, leaves no `.part` file and returns `Cancelled`.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored proxy_encode_reports`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn proxy_encode_reports_progress_and_can_be_cancelled() {
    let dir = Scratch::new("proxy-progress");
    let media = dir.join("clip.mp4");
    run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1920x1080:r=30:d=8",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        media.to_str().unwrap(),
    ]);
    let mut seen: Vec<f64> = Vec::new();
    let proxy = generate_proxy_with(
        &media,
        PROXY_MAX_WIDTH,
        ProxyRun {
            reservation: Some(cpu::reserve()),
            duration: Some(8.0),
            progress: &mut |f| seen.push(f),
            cancel: &|| false,
            pipeline: &mut |_| {},
        },
    )
    .expect("proxy");
    let _cleanup = ProxyGuard(proxy.clone());
    assert!(proxy.is_file());
    assert!(!seen.is_empty(), "no progress was reported");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress went backwards: {seen:?}");
    assert!(
        seen.iter().all(|f| (0.0..1.0).contains(f)),
        "a build is not done until it is renamed: {seen:?}"
    );

    // A second source, abandoned the moment it starts.
    let media2 = dir.join("clip2.mp4");
    run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=1920x1080:r=30:d=8",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        media2.to_str().unwrap(),
    ]);
    let cancelled = generate_proxy_with(
        &media2,
        PROXY_MAX_WIDTH,
        ProxyRun {
            reservation: None,
            duration: Some(8.0),
            progress: &mut |_| {},
            cancel: &|| true,
            pipeline: &mut |_| {},
        },
    );
    assert!(matches!(cancelled, Err(Error::Cancelled)), "{cancelled:?}");
    let dst = proxy_path(&media2, PROXY_MAX_WIDTH).expect("path");
    assert!(!dst.exists(), "a cancelled build left a proxy behind");
    let strays: Vec<_> = std::fs::read_dir(dst.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name().to_string_lossy().ends_with(".part")
                && e.file_name().to_string_lossy().starts_with(&format!(
                    "{:016x}",
                    fnv1a(&proxy_key(&source_key(&media2), PROXY_MAX_WIDTH, false, false))
                ))
        })
        .collect();
    assert!(strays.is_empty(), "a cancelled build left a temp file: {strays:?}");
}

#[test]
fn filter_complex_positions_and_overlays_clips() {
    // One clip with audio, one from a video-only asset, on one video track.
    let with_audio = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    let video_only = test_asset(vec![video_stream(3840, 2160, 24.0)]);
    let assets = vec![with_audio.clone(), video_only.clone()];

    let mut first = make_clip(with_audio.id, 0.0, 5.0, 0.0);
    first.volume = 0.5;
    let timeline = single(vec![first, make_clip(video_only.id, 2.0, 4.0, 5.0)]);

    let opts = ExportOptions::default();
    let fmt = export_format(&timeline, &assets, &opts);
    // Output shape comes from the first video/audio-bearing clips.
    assert_eq!((fmt.width, fmt.height), (1920, 1080));
    assert_eq!(fmt.sample_rate, 48_000);

    let g = build_filter_complex(
        &timeline,
        &assets,
        &fmt,
        timeline.duration(),
        &ExportOptions::default(),
        true,
        true,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    );
    assert!(g.has_video && g.has_audio);
    let f = g.filter;
    // A black canvas spanning the whole timeline, then one positioned
    // overlay per video clip; the last overlay writes [outv].
    assert!(f.contains("color=c=black:s=1920x1080"));
    assert!(f.contains("overlay=eof_action=pass:enable='between(t,0,5)'"));
    // Second clip: start 5, dur 2, its window opened a frame early.
    assert!(f.contains("enable='between(t,4.966666666666667,7)'"));
    assert!(f.contains("[outv]"));
    assert!(f.contains("volume=0.5"));
    assert!(f.contains("[0:v]trim=start=0:end=5"));
    assert!(f.contains("setpts=PTS-STARTPTS+5/TB")); // second clip positioned at 5s
                                                     // Every video segment is scaled/padded to the common resolution.
    assert_eq!(f.matches("scale=1920:1080").count(), 2);
    assert!(f.contains("format=yuv420p"));
    // Only the audio-bearing clip contributes audio; it is summed via amix
    // (no synthesized silence for the video-only clip any more).
    assert!(f.contains("[0:a]atrim=start=0:end=5"));
    assert!(f.contains("aformat=sample_rates=48000:channel_layouts=stereo"));
    assert!(f.contains("amix=inputs=1:normalize=0"));
    assert!(!f.contains("[1:a]"));
    assert!(!f.contains("anullsrc"));
}

#[test]
fn filter_complex_layers_multiple_tracks() {
    // Interview on V1 (video+audio), B-roll over it on V2 (video only).
    let interview = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    let broll = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![interview.clone(), broll.clone()];

    let timeline = timeline_of(vec![
        video_track(vec![make_clip(interview.id, 0.0, 20.0, 0.0)]),
        video_track(vec![make_clip(broll.id, 0.0, 6.0, 4.0)]), // overlaps 4..10
    ]);
    let fmt = export_format(&timeline, &assets, &ExportOptions::default());
    let g = build_filter_complex(
        &timeline,
        &assets,
        &fmt,
        timeline.duration(),
        &ExportOptions::default(),
        true,
        true,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    );
    let f = g.filter;
    // Two overlays: B-roll (input 1) composites on top of the interview.
    assert_eq!(f.matches("overlay=eof_action=pass").count(), 2);
    assert!(f.contains("[1:v]trim=start=0:end=6"));
    assert!(f.contains("enable='between(t,3.966666666666667,10)'"));
    // Only the interview carries audio, so the mix has one input.
    assert!(g.has_audio);
    assert!(f.contains("amix=inputs=1"));
    assert!(!f.contains("[1:a]"));
}

#[test]
fn filter_complex_audio_only_timeline_has_no_video() {
    let audio_asset = test_asset(vec![audio_stream(44_100, 2)]);
    let assets = vec![audio_asset.clone()];
    let timeline = timeline_of(vec![audio_track(vec![make_clip(audio_asset.id, 0.0, 10.0, 3.0)])]);
    let fmt = export_format(&timeline, &assets, &ExportOptions::default());
    let g = build_filter_complex(
        &timeline,
        &assets,
        &fmt,
        timeline.duration(),
        &ExportOptions::default(),
        true,
        true,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    );
    assert!(!g.has_video);
    assert!(g.has_audio);
    // Positioned at 3s on the timeline via adelay; no picture canvas.
    assert!(g.filter.contains("adelay=3000:all=1"));
    assert!(!g.filter.contains("color=c=black"));
}

#[test]
fn filter_complex_applies_fades_to_picture_and_audio() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    let assets = vec![asset.clone()];
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.fade_in = 0.5;
    clip.fade_out = 1.0;
    let timeline = single(vec![clip]);

    let fmt = export_format(&timeline, &assets, &ExportOptions::default());
    let f = build_filter_complex(
        &timeline,
        &assets,
        &fmt,
        timeline.duration(),
        &ExportOptions::default(),
        true,
        true,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    )
    .filter;
    // Picture fades sit just before the pixel-format normalize.
    assert!(f.contains("fade=t=in:st=0:d=0.5,fade=t=out:st=9:d=1,format=yuv420p"));
    // Audio fades sit just before the audio-format normalize. The out fade
    // starts at (duration - fade_out) = 9s.
    assert!(f.contains("afade=t=in:st=0:d=0.5,afade=t=out:st=9:d=1,aformat"));
}

#[test]
fn filter_complex_omits_fades_when_zero() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    let assets = vec![asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    let fmt = export_format(&timeline, &assets, &ExportOptions::default());
    let f = build_filter_complex(
        &timeline,
        &assets,
        &fmt,
        timeline.duration(),
        &ExportOptions::default(),
        true,
        true,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    )
    .filter;
    assert!(!f.contains("fade="), "no fade filter when fades are zero");
    assert!(!f.contains("afade="), "no afade filter when fades are zero");
}

#[test]
fn export_format_falls_back_to_defaults() {
    let timeline = Timeline {
        tracks: Vec::new(),
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: None,
        master: Default::default(),
        thumbnails: Vec::new(),
    };
    let fmt = export_format(&timeline, &[], &ExportOptions::default());
    assert_eq!((fmt.width, fmt.height), (1920, 1080));
    assert_eq!(fmt.channel_layout(), "stereo");
}

#[test]
fn export_args_reference_the_original_source_not_a_proxy() {
    // Hard invariant: export always reads the original asset; preview proxies
    // are a preview-only optimisation. The export builder has no proxy
    // knowledge, so its `-i` inputs are the asset paths verbatim and never a
    // cached file under .../proxies/.
    let asset = test_asset(vec![video_stream(3840, 2160, 60.0), audio_stream(48_000, 2)]);
    let assets = vec![asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 1.0, 5.0, 0.0)]);
    let args = build_export_args(&timeline, &assets, "/out.mp4", &ExportOptions::default()).unwrap();
    let input = args.iter().position(|a| a == "-i").expect("-i present");
    assert_eq!(args[input + 1], asset.path);
    assert!(
        !args.iter().any(|a| a.contains("proxies")),
        "export must not reference a proxy"
    );
}

// ---- mute / solo / clip-enable gating ----------------------------------

/// The graph builders take the timeline through `Timeline::for_render`, so a
/// muted track or a disabled clip must never reach argv. Tested here rather
/// than only on the model, because the failure mode is a *missing call*.
#[test]
fn muted_tracks_and_disabled_clips_never_reach_the_export_args() {
    let keep = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    // Distinct paths: `test_asset` reuses one, which would make the assertion vacuous.
    let drop = Asset {
        path: "/gated.mp4".into(),
        ..test_asset(vec![video_stream(1920, 1080, 30.0)])
    };
    let assets = vec![keep.clone(), drop.clone()];

    let mut disabled = make_clip(drop.id, 0.0, 4.0, 10.0);
    disabled.enabled = false;
    let timeline = timeline_of(vec![
        video_track(vec![make_clip(keep.id, 0.0, 5.0, 0.0), disabled]),
        Track {
            muted: true,
            ..video_track(vec![make_clip(drop.id, 0.0, 6.0, 0.0)])
        },
    ]);

    let args = build_export_args(&timeline, &assets, "/out.mp4", &ExportOptions::default()).unwrap();
    let argv = args.join(" ");
    assert!(argv.contains(&keep.path), "the kept clip's input is missing");
    assert!(
        !argv.contains(&drop.path),
        "a muted track and a disabled clip both still reached argv: {argv}"
    );
}

/// Soloing gates by kind, and the still path must agree with the export.
#[test]
fn solo_gates_the_timeline_still_by_kind() {
    let a = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let b = Asset {
        path: "/soloed.mp4".into(),
        ..test_asset(vec![video_stream(1920, 1080, 30.0)])
    };
    let assets = vec![a.clone(), b.clone()];
    let timeline = timeline_of(vec![
        video_track(vec![make_clip(a.id, 0.0, 5.0, 0.0)]),
        Track {
            solo: true,
            ..video_track(vec![make_clip(b.id, 0.0, 5.0, 0.0)])
        },
    ]);

    let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 2.0, 960, 4).unwrap();
    let argv = args.join(" ");
    assert!(argv.contains(&b.path), "the soloed track should be the one shown");
    assert!(!argv.contains(&a.path), "an unsoloed track leaked into the still: {argv}");
}

// ---- per-clip video / audio effects, keyframes, text overlays -----------

#[test]
fn video_effects_render_with_chroma_keeping_alpha() {
    let fmt = ExportFormat::default();
    let mut clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0);
    clip.effects = vec![
        VideoEffect::Blur { sigma: 8.0 },
        VideoEffect::ChromaKey {
            color: "green".into(),
            similarity: 0.1,
            blend: 0.0,
        },
    ];
    let chain = video_clip_chain(&clip, &fmt, &ClipFx::default(), false, "c0");
    // Color-space blur runs before the alpha plane is established, chroma key
    // after it; the terminal yuv420p flatten is suppressed so alpha survives.
    let gi = chain.find("gblur=sigma=8").expect("blur");
    let yi = chain.find("format=yuva420p").expect("alpha plane");
    let ci = chain.find("chromakey=green:0.1:0").expect("chroma key");
    assert!(gi < yi && yi < ci, "order: blur < yuva < chroma in {chain}");
    assert!(!chain.contains("format=yuv420p"), "alpha must not be flattened: {chain}");
}

#[test]
fn audio_effects_chain_after_gain_in_order() {
    let fmt = ExportFormat::default();
    let mut clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 1.0);
    clip.audio = vec![
        AudioEffect::Highpass { hz: 80.0 },
        AudioEffect::Compressor {
            threshold_db: -18.0,
            ratio: 3.0,
            attack_ms: 20.0,
            release_ms: 250.0,
            makeup_db: 6.0,
        },
    ];
    let chain = audio_clip_chain(&clip, &fmt, &ClipFx::default(), "stereo", unity_mix());
    let vi = chain.find("volume=").expect("gain");
    let hi = chain.find("highpass=f=80").expect("highpass");
    let ai = chain.find("acompressor=").expect("compressor");
    assert!(vi < hi && hi < ai, "effects follow the gain in author order: {chain}");
    assert!(chain.contains("ratio=3"));
}

/// The pads a chain reads (the labels it starts with) and writes (the labels it ends with).
fn pads_of(chain: &str) -> (Vec<String>, Vec<String>) {
    let (mut ins, mut rest) = (Vec::new(), chain);
    while let Some(inner) = rest.strip_prefix('[') {
        let end = inner.find(']').expect("a closing bracket");
        ins.push(inner[..end].to_string());
        rest = &inner[end + 1..];
    }
    let (mut outs, mut rest) = (Vec::new(), rest);
    while rest.ends_with(']') {
        let open = rest.rfind('[').expect("an opening bracket");
        outs.push(rest[open + 1..rest.len() - 1].to_string());
        rest = &rest[..open];
    }
    outs.reverse();
    (ins, outs)
}

/// Split `text` at the commas outside single quotes and brackets: the filters of a chain.
fn filters_of(text: &str) -> Vec<&str> {
    let (mut parts, mut start, mut quoted, mut depth) = (Vec::new(), 0, false, 0i32);
    for (i, c) in text.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn ramp_keys() -> Vec<crate::model::SpeedKey> {
    use crate::model::{RampEasing, SpeedKey};
    vec![
        SpeedKey::new(0.0, 1.0),
        SpeedKey {
            time: 1.5,
            speed: 2.5,
            easing: RampEasing::Hold,
        },
        SpeedKey::new(2.5, 0.5),
        SpeedKey::new(3.5, 0.5),
    ]
}

#[test]
fn a_ramped_clips_picture_is_retimed_by_one_quoted_expression() {
    let fmt = fmt_1080p();
    let mut clip = make_clip(Uuid::new_v4(), 1.0, 8.0, 2.0);
    clip.speed_ramp = ramp_keys();
    let chain = video_clip_chain(&clip, &fmt, &ClipFx::default(), false, "c0");
    let filters = filters_of(&chain);
    // The window is trimmed as ever, then the one `setpts` (commas and a `;` inside its quotes).
    let setpts = filters.iter().find(|f| f.starts_with("setpts=")).expect("a setpts");
    assert!(
        setpts.starts_with("setpts='st(0,(PTS-STARTPTS)*TB);(") && setpts.ends_with("+2)/TB'"),
        "{setpts}"
    );
    assert_eq!(filters.iter().filter(|f| f.starts_with("setpts")).count(), 1, "{chain}");
    assert!(!chain.contains("/TB+") && !chain.contains(")/2.5+"), "{chain}");
    let (trim, retime, fps) = (
        filters.iter().position(|f| f.starts_with("trim=")).unwrap(),
        filters.iter().position(|f| f.starts_with("setpts=")).unwrap(),
        filters.iter().position(|f| f.starts_with("fps=")).unwrap(),
    );
    assert!(trim < retime && retime < fps, "{chain}");
    // Reversed, the frames are put back in playing order first and the retime is the same text.
    let mut back = clip;
    back.speed = -1.0;
    let reversed = video_clip_chain(&back, &fmt, &ClipFx::default(), false, "c0");
    let filters = filters_of(&reversed);
    assert!(
        filters.iter().position(|f| *f == "reverse").unwrap() < filters.iter().position(|f| f.starts_with("setpts=")).unwrap()
    );
    assert!(filters.contains(setpts), "{reversed}");
    // Sixteen keys nest five `if`s and a bracket level per `(`, far from libavutil's hundred.
    let mut many = make_clip(Uuid::new_v4(), 0.0, 400.0, 0.0);
    many.speed_ramp = (0..16)
        .map(|i| crate::model::SpeedKey::new(f64::from(i) * 0.5, 0.5 + f64::from(i % 4)))
        .collect();
    let chain = video_clip_chain(&many, &fmt, &ClipFx::default(), false, "c0");
    let setpts = filters_of(&chain).into_iter().find(|f| f.starts_with("setpts=")).unwrap();
    let (ifs, brackets) = nesting(setpts);
    assert_eq!(ifs, 5, "{setpts}");
    assert!(brackets < 40, "{brackets} levels in {setpts}");
    // A clip at a constant speed keeps the divide it always had.
    let mut fast = make_clip(Uuid::new_v4(), 1.0, 8.0, 2.0);
    fast.speed = 2.0;
    let chain = video_clip_chain(&fast, &fmt, &ClipFx::default(), false, "c0");
    assert!(chain.contains("setpts=(PTS-STARTPTS)/2+2/TB"), "{chain}");
}

#[test]
fn a_ramp_leaves_the_animation_on_the_timeline_clock() {
    // Keyframes and channels are clip-local seconds of the output, read after `setpts` has put the
    // frames on the timeline: the same text with a ramp as without one, at any speed.
    let fmt = fmt_1080p();
    let mut plain = make_clip(Uuid::new_v4(), 1.0, 8.0, 2.0);
    let keys = |pairs: &[(f64, f64)]| -> Vec<crate::model::PropertyKey> {
        pairs.iter().map(|&(t, v)| crate::model::PropertyKey::new(t, v)).collect()
    };
    plain.set_property_keys(Property::Opacity, keys(&[(0.0, 0.2), (2.0, 1.0)]));
    plain.set_property_keys(Property::Scale, keys(&[(0.0, 1.0), (3.0, 1.6)]));
    plain.set_property_keys(Property::Rotation, keys(&[(0.0, 0.0), (3.0, 20.0)]));
    let mut ramped = plain.clone();
    ramped.speed_ramp = ramp_keys();
    let (a, b) = (
        video_clip_chain(&plain, &fmt, &ClipFx::default(), false, "c0"),
        video_clip_chain(&ramped, &fmt, &ClipFx::default(), false, "c0"),
    );
    let animated = |chain: &str| -> Vec<String> {
        filters_of(chain)
            .into_iter()
            .filter(|f| !f.starts_with("setpts") && !f.starts_with("trim") && !f.starts_with("fps"))
            .map(String::from)
            .collect()
    };
    assert!(a.contains("geq=") && a.contains("rotate=a="));
    assert_eq!(animated(&a), animated(&b));
    // And the sound's volume curve, evaluated after the steps are joined, is clip-local too.
    let mut keyed = ramped;
    keyed.set_property_keys(Property::Volume, keys(&[(0.0, 1.0), (2.0, 0.25)]));
    let audio = audio_clip_chains("0:a", 0, &keyed, &fmt, &ClipFx::default(), "stereo", unity_mix());
    let tail = audio.last().unwrap();
    assert!(
        tail.starts_with("[ar0x") && tail.contains("asetnsamples=n=128:p=0,volume='"),
        "{tail}"
    );
    assert!(tail.ends_with("adelay=2000:all=1,asetpts=N/SR/TB[a0]"), "{tail}");
}

#[test]
fn a_ramped_clips_sound_is_cut_into_steps_and_joined_end_to_end() {
    let fmt = ExportFormat::default();
    let (src, flat) = ("0:a", 3);
    let mut clip = make_clip(Uuid::new_v4(), 1.0, 8.0, 2.0);
    clip.speed_ramp = ramp_keys();
    let fx = ClipFx::default();
    let chains = audio_clip_chains(src, flat, &clip, &fmt, &fx, "stereo", unity_mix());
    let steps = ramp::audio_steps(&clip, clip.duration());
    let n = steps.len();
    assert!(n > 4, "{n} steps");
    // The head splits the stream into one pad per step; one chain per step; one `acrossfade` per seam.
    assert_eq!(chains.len(), 1 + n + (n - 1) + 1, "{chains:#?}");
    assert!(
        chains[0].starts_with("[0:a]atrim=start=0:end=7,asetpts=PTS-STARTPTS,aresample=48000,asplit="),
        "{}",
        chains[0]
    );
    let crossfades = chains.iter().filter(|c| c.contains("acrossfade=d=0.002")).count();
    assert_eq!(crossfades, n - 1);
    // Every pad is written once and read once, from the source to the clip's own `[a3]`.
    let mut written = vec![src.to_string()];
    let mut read: Vec<String> = Vec::new();
    for chain in &chains {
        let (ins, outs) = pads_of(chain);
        read.extend(ins);
        written.extend(outs);
    }
    let (mut written_sorted, mut read_sorted) = (written.clone(), read.clone());
    written_sorted.sort();
    read_sorted.sort();
    let mut want = written_sorted.clone();
    want.retain(|p| p != "a3");
    assert_eq!(read_sorted, want, "written {written:?}\nread {read:?}");
    assert_eq!(written.iter().filter(|p| *p == "a3").count(), 1);
    assert!(
        chains.last().unwrap().ends_with("adelay=2000:all=1,asetpts=N/SR/TB[a3]"),
        "{}",
        chains.last().unwrap()
    );
    // A step is its own `atempo` stream trimmed to whole samples, padded if the source ran short.
    let hold = chains
        .iter()
        .find(|c| c.contains("atempo=1.5811388300841898,atempo=1.5811388300841898"))
        .expect("the hold at 2.5x: two stages of its square root");
    assert!(hold.contains("apad=whole_len="), "{hold}");
    for piece in &chains[1..=n] {
        assert!(
            piece.contains(",apad=whole_len=") && piece.contains("atrim=start_sample="),
            "{piece}"
        );
    }
    // The two ends of the clip's sound are the ramp's two ends: the first step's tempo is the slope's start.
    assert!(chains[1].contains("atempo="), "{}", chains[1]);
    // A ramp of one step, and a clip without one, are the single chain they always were.
    let mut flat_ramp = make_clip(Uuid::new_v4(), 0.0, 6.0, 0.0);
    flat_ramp.speed_ramp = vec![crate::model::SpeedKey::new(0.0, 2.0)];
    let one = audio_clip_chains(src, flat, &flat_ramp, &fmt, &fx, "stereo", unity_mix());
    assert_eq!(one.len(), 1);
    assert!(
        one[0].starts_with("[0:a]atrim=start=0:end=6,asetpts=PTS-STARTPTS,atempo=2,volume=1,"),
        "{}",
        one[0]
    );
    let mut fast = flat_ramp;
    (fast.speed_ramp, fast.speed) = (Vec::new(), 2.0);
    assert_eq!(
        audio_clip_chains(src, flat, &fast, &fmt, &fx, "stereo", unity_mix()),
        one,
        "a constant speed and a one-key ramp at it play the same"
    );
    assert_eq!(
        one[0],
        format!("[0:a]{}[a3]", audio_clip_chain(&fast, &fmt, &fx, "stereo", unity_mix()))
    );
}

#[test]
fn a_ramped_clips_reversed_sound_is_reversed_before_it_is_cut_and_its_tail_is_stepped_too() {
    let fmt = ExportFormat::default();
    let mut clip = make_clip(Uuid::new_v4(), 10.0, 16.0, 1.0);
    clip.speed = -1.0;
    clip.speed_ramp = ramp_keys();
    let fx = ClipFx {
        tail: 0.75,
        ..ClipFx::default()
    };
    let chains = audio_clip_chains("1:a", 0, &clip, &fmt, &fx, "stereo", unity_mix());
    // The window reaches below the in-point by the tail at the last key's speed (0.5x: 0.375 s).
    assert!(
        chains[0].starts_with("[1:a]atrim=start=0:end=6.375,asetpts=PTS-STARTPTS,areverse,aresample=48000,asplit="),
        "{}",
        chains[0]
    );
    // The steps cover the clip and its tail: the sound fades out over the tail as ever.
    let total = clip.duration() + 0.75;
    let last = chains.last().unwrap();
    assert!(last.contains(&format!("afade=t=out:st={}:d=0.75", clip.duration())), "{last}");
    let steps = ramp::audio_steps(&clip, total);
    assert_eq!(steps.last().unwrap().t1, total);
}

#[test]
fn hostile_ramps_never_panic_a_builder_and_a_malformed_one_is_a_constant_speed() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    let numbers = [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -1.0,
        0.0,
        1e-300,
        0.01,
        0.37,
        1.0,
        2.5,
        100.0,
        101.0,
        1e300,
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut pick = |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as usize % n
    };
    for round in 0..400 {
        let keys: Vec<crate::model::SpeedKey> = (0..pick(20))
            .map(|_| crate::model::SpeedKey {
                time: numbers[pick(numbers.len())],
                speed: numbers[pick(numbers.len())],
                easing: if pick(2) == 0 {
                    crate::model::RampEasing::Hold
                } else {
                    crate::model::RampEasing::Linear
                },
            })
            .collect();
        let mut clip = make_clip(
            asset.id,
            numbers[pick(8)].abs().min(50.0),
            60.0,
            numbers[pick(8)].abs().min(9.0),
        );
        clip.speed = [1.0, -1.0, 2.0, 0.0][pick(4)];
        clip.speed_ramp = keys.clone();
        let tail = [0.0, 0.5][pick(2)];
        let fx = ClipFx {
            tail,
            ..ClipFx::default()
        };
        let fmt = ExportFormat::default();
        let _ = video_clip_chain(&clip, &fmt, &fx, false, "c0");
        let chains = audio_clip_chains("0:a", 0, &clip, &fmt, &fx, "stereo", unity_mix());
        assert!(!chains.is_empty());
        assert!(!chains.concat().contains("NaN"), "round {round}: {chains:?}");
        let silent = ExportFormat {
            sample_rate: 0,
            ..ExportFormat::default()
        };
        assert!(!audio_clip_chains("0:a", 0, &clip, &silent, &fx, "stereo", unity_mix())
            .concat()
            .contains("NaN"));
        let timeline = crate::engine::test_support::single(vec![clip.clone()]);
        let _ = build_export_args(&timeline, std::slice::from_ref(&asset), "/o.mp4", &ExportOptions::default());
        if !crate::model::ramp_is_valid(&keys) {
            // Not read: the same graph as the clip at its constant speed.
            let mut plain = clip.clone();
            plain.speed_ramp = Vec::new();
            assert_eq!(
                video_clip_chain(&clip, &fmt, &fx, false, "c0"),
                video_clip_chain(&plain, &fmt, &fx, false, "c0"),
                "round {round}: {keys:?}"
            );
            assert_eq!(
                audio_clip_chains("0:a", 0, &clip, &fmt, &fx, "stereo", unity_mix()),
                audio_clip_chains("0:a", 0, &plain, &fmt, &fx, "stereo", unity_mix()),
                "round {round}: {keys:?}"
            );
        }
    }
}

#[test]
fn keyframe_expr_is_piecewise_linear_and_clamped() {
    let e = keyframe_expr(&[(0.0, 10.0), (4.0, 20.0)], "t", 2.0);
    // Local time is (t - start); the first value is held before the first key.
    assert!(e.contains("(t-2)"), "local time relative to clip start: {e}");
    assert!(e.starts_with("if(lt((t-2),0),10,"), "holds first value before t0: {e}");
    assert!(e.contains("10+(10)*"), "linear segment v0 + dv*…: {e}");
    // A single keyframe degenerates to a constant.
    assert_eq!(keyframe_expr(&[(1.0, 0.5)], "t", 0.0), "0.5");
}

/// The deepest `if(` nesting of an expression, and its deepest nesting of any kind of
/// bracket: what libavutil counts (a level per `(`, a function call's included).
fn nesting(expr: &str) -> (usize, usize) {
    let (mut brackets, mut ifs, mut open_ifs) = (0usize, 0usize, Vec::new());
    let (mut deepest_brackets, mut deepest_ifs) = (0, 0);
    let bytes = expr.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        match c {
            b'(' => {
                brackets += 1;
                let is_if = i >= 2 && &bytes[i - 2..i] == b"if" && (i == 2 || !bytes[i - 3].is_ascii_alphanumeric());
                open_ifs.push(is_if);
                if is_if {
                    ifs += 1;
                }
            }
            b')' => {
                brackets -= 1;
                if open_ifs.pop() == Some(true) {
                    ifs -= 1;
                }
            }
            _ => {}
        }
        deepest_brackets = deepest_brackets.max(brackets);
        deepest_ifs = deepest_ifs.max(ifs);
    }
    (deepest_ifs, deepest_brackets)
}

/// `n` points on a line with time = value = index.
fn ramp(n: usize) -> Vec<(f64, f64)> {
    (0..n).map(|i| (i as f64, i as f64)).collect()
}

#[test]
fn a_short_keyframe_expression_is_the_chain_it_always_was() {
    assert_eq!(
        keyframe_expr(&[(0.0, 0.0), (1.0, 10.0), (3.0, -2.0)], "t", 1.5),
        "if(lt((t-1.5),0),0,if(lt((t-1.5),1),(0+(10)*((t-1.5)-0)/(1)),\
             if(lt((t-1.5),3),(10+(-12)*((t-1.5)-1)/(2)),-2)))"
    );
    // Right up to the threshold every point is a level; one past it, the tree.
    assert_eq!(
        nesting(&keyframe_expr(&ramp(KEYFRAME_TREE_POINTS), "t", 0.0)).0,
        KEYFRAME_TREE_POINTS
    );
    assert!(nesting(&keyframe_expr(&ramp(KEYFRAME_TREE_POINTS + 1), "t", 0.0)).0 < 8);
}

#[test]
fn a_long_keyframe_expression_is_a_balanced_tree_over_its_segments() {
    // Five points, four segments: halve twice. A step (equal times) is a leaf like any other.
    let pts = ramp(5);
    assert_eq!(
        keyframe_tree(&pts, 0, 4, "L"),
        "if(lt(L,2),if(lt(L,1),(0+(1)*(L-0)/(1)),(1+(1)*(L-1)/(1))),\
             if(lt(L,3),(2+(1)*(L-2)/(1)),(3+(1)*(L-3)/(1))))"
    );
    let step = [(0.0, 1.0), (2.0, 1.0), (2.0, 5.0), (4.0, 7.0)];
    assert_eq!(
        keyframe_tree(&step, 0, 3, "L"),
        "if(lt(L,2),(1+(0)*(L-0)/(2)),if(lt(L,2),1,(5+(2)*(L-2)/(2))))"
    );
}

#[test]
fn many_eased_keys_stay_far_inside_the_expression_nesting_limit() {
    // libavutil refuses an expression nested past 100 levels. 40 keys eased into each other
    // are 12 pieces a segment: the chain was a level per piece (ten eased keys already failed
    // the export and the playback stream with `Invalid argument`).
    let mut clip = make_clip(Uuid::nil(), 0.0, 40.0, 0.0);
    use crate::model::Easing;
    let easings = [Easing::EaseInOut, Easing::Hold, Easing::EaseOut, Easing::Linear];
    clip.keyframes = (0..40)
        .map(|i| crate::model::Keyframe {
            time: f64::from(i),
            scale: 1.0 + f64::from(i % 3) * 0.1,
            pos_x: f64::from(i % 5) * 0.05,
            pos_y: 0.0,
            rotation: f64::from(i % 7),
            opacity: 1.0 - f64::from(i % 4) * 0.1,
            easing: easings[i as usize % easings.len()],
        })
        .collect();
    for channel in [
        clip.keyframe_channel(|k| k.scale),
        clip.keyframe_channel(|k| k.pos_x),
        clip.keyframe_channel(|k| k.rotation),
        clip.keyframe_channel(|k| k.opacity),
    ] {
        assert!(channel.len() > 200, "{} points", channel.len());
        let (ifs, brackets) = nesting(&keyframe_expr(&channel, "t", 0.0));
        assert!(
            ifs <= 12 && brackets <= 20,
            "{} points nest {ifs} ifs / {brackets} brackets",
            channel.len()
        );
    }
    // ... and inside the filters that wrap it, which add a few levels of their own.
    let graph = video_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), false, "c0");
    assert!(nesting(&graph).1 < 40, "{}", nesting(&graph).1);
}

#[test]
fn keyframed_clip_animates_scale_and_position() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let mut clip = make_clip(asset.id, 0.0, 10.0, 2.0);
    clip.keyframes = vec![
        crate::model::Keyframe {
            time: 0.0,
            scale: 1.0,
            pos_x: -0.3,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        },
        crate::model::Keyframe {
            time: 4.0,
            scale: 1.5,
            pos_x: 0.3,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        },
    ];
    // The zoom and the position are the chain's last stage, a per-frame `perspective` that
    // lays the picture on a frame of the canvas' size…
    let chain = video_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), false, "c0");
    assert!(
        chain.contains(",perspective=x0='(") && chain.contains(":interpolation=linear:eval=frame,crop=w=1920:h=1080:"),
        "animated zoom: {chain}"
    );
    assert!(!chain.contains("scale=w='iw*("), "{chain}");
    // …which the overlay puts at the canvas' origin.
    let timeline = single(vec![clip]);
    let g = build_filter_complex(
        &timeline,
        &assets,
        &ExportFormat::default(),
        timeline.duration(),
        &ExportOptions::default(),
        true,
        false,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    );
    assert!(
        g.filter
            .contains("[vbase][v0]overlay=eof_action=pass:enable='between(t,1.9666666666666666,12)'"),
        "placed overlay: {}",
        g.filter
    );
}

/// A key at clip-local `time` carrying the given channels.
fn key(time: f64, scale: f64, pos_x: f64, rotation: f64, opacity: f64) -> crate::model::Keyframe {
    crate::model::Keyframe {
        time,
        scale,
        pos_x,
        pos_y: 0.0,
        rotation,
        opacity,
        easing: Default::default(),
    }
}

/// Everything after the placement's `perspective ... eval=frame` in a chain.
fn after_zoom(chain: &str) -> &str {
    let at = chain.find("eval=frame").expect("a moving zoom has an eval=frame perspective");
    chain[at..].split_once(',').map_or("", |(_, rest)| rest)
}

/// The structural half of the keyed-zoom fix, over every feature a clip's chain can
/// carry: the zoom is the **last** thing in the chain, after `fps`: the placement's
/// `perspective` and the cut to the canvas, in the format `overlay` takes. Every filter
/// ahead of it acts on the picture at its fit size, so a blur or a mask is magnified with
/// the zoom, as it was when the zoom was a size-changing `scale`. The rendered tests
/// (`cli/keyed_zoom.rs`) are what show ffmpeg agrees; this is what keeps a later
/// edit of the chain from putting a filter back behind the zoom.
#[test]
fn a_keyed_zoom_is_the_last_stage_of_its_chain() {
    use crate::model::{Mask, MaskShape, VideoEffect};
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let base = || {
        let mut clip = make_clip(asset.id, 0.0, 8.0, 2.0);
        clip.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 0.0, 1.0)];
        clip
    };
    let mask = Mask {
        shape: MaskShape::Ellipse,
        ..Mask::default()
    };
    let cases: Vec<(&str, Clip, ExportFormat, ClipFx, bool)> = {
        let plain = ExportFormat::default();
        let mut out = vec![("scale only", base(), plain.clone(), ClipFx::default(), false)];
        let mut c = base();
        c.keyframes = vec![key(0.0, 0.4, -0.2, 0.0, 1.0), key(3.0, 1.6, 0.2, 0.0, 1.0)];
        out.push(("scale + position", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 0.0, 0.5)];
        out.push(("scale + opacity", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 40.0, 1.0)];
        out.push(("scale + rotation", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.keyframes = vec![key(0.0, 0.4, -0.2, 0.0, 1.0), key(3.0, 1.6, 0.2, 40.0, 0.5)];
        out.push(("everything keyed", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.transform.crop_left = 0.1;
        c.transform.crop_top = 0.2;
        out.push(("crop", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.mask = Some(mask);
        out.push(("mask", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.mask = Some(mask);
        c.keyframes = vec![key(0.0, 0.4, 0.0, 0.0, 1.0), key(3.0, 1.6, 0.0, 0.0, 0.5)];
        out.push(("mask + opacity", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.effects = vec![
            VideoEffect::Blur { sigma: 3.0 },
            VideoEffect::Vignette,
            VideoEffect::ChromaKey {
                color: "green".into(),
                similarity: 0.3,
                blend: 0.1,
            },
        ];
        out.push(("effects + chroma key", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.color.brightness = 0.1;
        c.color.temperature = 0.3;
        out.push(("colour", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.speed = 2.0;
        out.push(("speed", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.speed = -0.5;
        out.push(("reverse", c, plain.clone(), ClipFx::default(), false));
        let mut c = base();
        c.fade_in = 0.5;
        c.fade_out = 0.5;
        out.push(("fades", c, plain.clone(), ClipFx::default(), false));
        let fx = ClipFx {
            xfade_in: 0.5,
            tail: 0.5,
            black_out: 0.3,
            ..ClipFx::default()
        };
        out.push(("dissolve + dip", base(), plain.clone(), fx, false));
        let hdr = ClipFx {
            hdr: Some(crate::model::Hdr::Hlg),
            ..ClipFx::default()
        };
        out.push(("hdr", base(), plain.clone(), hdr, false));
        let cover = ExportFormat {
            fit: Fit::Cover,
            scaler: Some("lanczos".into()),
            ..ExportFormat::default()
        };
        out.push(("cover + scaler flag", base(), cover, ClipFx::default(), false));
        out.push(("still image", base(), plain.clone(), ClipFx::default(), true));
        let mut c = base();
        c.reframe = Some(crate::model::Reframe::new(crate::model::Projection::Equirect));
        out.push(("360 reframe", c, plain, ClipFx::default(), false));
        out
    };
    for (name, clip, fmt, fx, image) in cases {
        assert!(clip.zoom_animated(), "{name}: the case has to zoom");
        let chain = video_clip_chain(&clip, &fmt, &fx, image, "c0");
        assert_eq!(chain.matches("eval=frame").count(), 1, "{name}: one zoom: {chain}");
        // Nothing follows the zoom but the cut to the canvas.
        assert_eq!(
            after_zoom(&chain),
            "crop=w=1920:h=1080:x='2*floor((iw-1920)/4)':y='2*floor((ih-1080)/4)'",
            "{name}: {chain}"
        );
        assert!(!chain.contains("scale=w='iw*("), "{name}: {chain}");
        // Every filter that reads the frame's size or its time is ahead of it, at
        // the constant size, and `fps` is ahead of all of them so they and the zoom
        // run on *output* frames.
        let zoom = chain.find("eval=frame").unwrap();
        let fps = chain.find("fps=").unwrap_or_else(|| panic!("{name}: no fps: {chain}"));
        assert!(fps < zoom, "{name}: the zoom is evaluated after fps: {chain}");
        for needle in [
            "geq=",
            "rotate=",
            "eq=",
            "gblur=",
            "vignette",
            "chromakey=",
            "fade=",
            "zscale",
            "colorspace=",
        ] {
            if let Some(at) = chain.find(needle) {
                assert!(at < zoom, "{name}: `{needle}` runs behind the zoom: {chain}");
                // ...and after the output rate is set (the 360 path hoists `fps`
                // above `v360`, which is earlier still).
                assert!(fps < at, "{name}: `{needle}` before fps: {chain}");
            }
        }
        // The old terminal pixel format is what pinned the size.
        assert!(!chain.ends_with("format=yuv420p"), "{name}: {chain}");
    }
}

/// `rotate`'s `fillcolor=none` means "do not fill": the corners keep whatever the
/// buffer last held, which a rotation that moves turns into every earlier pose. A
/// keyed rotation fills with transparent black; a constant one, whose footprint
/// never changes, keeps the chain it always had.
#[test]
fn a_keyed_rotation_fills_transparent_and_a_constant_one_keeps_its_chain() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let fmt = ExportFormat::default();
    let mut keyed = make_clip(asset.id, 0.0, 8.0, 2.0);
    keyed.keyframes = vec![key(0.0, 1.0, 0.0, 0.0, 1.0), key(3.0, 1.0, 0.0, 40.0, 1.0)];
    assert!(!keyed.zoom_animated(), "the rotation alone moves");
    let chain = video_clip_chain(&keyed, &fmt, &ClipFx::default(), false, "c0");
    assert!(chain.contains(":fillcolor=black@0:ow='hypot(iw,ih)'"), "{chain}");
    assert!(!chain.contains("fillcolor=none"), "{chain}");
    let mut zoomed = keyed;
    zoomed.keyframes = vec![key(0.0, 0.5, 0.0, 0.0, 1.0), key(3.0, 1.5, 0.0, 40.0, 1.0)];
    let chain = video_clip_chain(&zoomed, &fmt, &ClipFx::default(), false, "c0");
    assert!(chain.contains(":fillcolor=black@0:ow='hypot(iw,ih)'"), "{chain}");
    // A constant rotation is untouched.
    let mut fixed = make_clip(asset.id, 0.0, 8.0, 2.0);
    fixed.transform.rotation = 30.0;
    let chain = video_clip_chain(&fixed, &fmt, &ClipFx::default(), false, "c0");
    assert!(chain.contains(":fillcolor=none:ow=rotw("), "{chain}");
}

/// A keyed clip whose scale holds still never changes the picture's size, so it
/// keeps the chain it always had — and so does every unkeyed clip.
#[test]
fn a_keyed_clip_whose_scale_holds_keeps_the_chain_it_always_had() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let mut clip = make_clip(asset.id, 0.0, 8.0, 2.0);
    clip.keyframes = vec![key(0.0, 0.5, -0.2, 0.0, 1.0), key(3.0, 0.5, -0.2, 0.0, 1.0)];
    assert!(clip.is_animated() && !clip.zoom_animated() && !clip.placement_animated());
    let chain = video_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), false, "c0");
    // The (constant-valued) per-frame scale stays where it was: before `setsar`
    // and `fps`, with the clip's own pixel format at the end.
    let zoom = chain.find("eval=frame").unwrap();
    assert!(
        zoom < chain.find("setsar=1").unwrap() && zoom < chain.find("fps=").unwrap(),
        "{chain}"
    );
    assert!(chain.ends_with(",format=yuv420p"), "{chain}");
    assert!(!chain.contains("format=yuva420p"), "{chain}");
    // An unkeyed zoom is a plain `scale=iw*..` and gains nothing.
    let mut plain = make_clip(asset.id, 0.0, 8.0, 2.0);
    plain.transform.scale = 0.5;
    let chain = video_clip_chain(&plain, &ExportFormat::default(), &ClipFx::default(), false, "c0");
    assert!(!chain.contains("eval=frame") && chain.ends_with(",format=yuv420p"), "{chain}");
}

fn still_canvas(fit: Fit) -> StillCanvas {
    StillCanvas {
        w: 640,
        h: 360,
        fit,
        sf: String::new(),
    }
}

/// The still follows the export for a moving zoom: its zoom is the last stage, behind the
/// grade, the effects, the mask, the opacity and the rotation, which then act on the
/// picture at its fit size. Every other clip's still zooms first, as it always did.
#[test]
fn the_still_of_a_moving_zoom_runs_the_zoom_last_and_every_other_still_zooms_first() {
    use crate::model::{Mask, VideoEffect};
    let tf = Transform {
        scale: 0.6,
        pos_x: 0.1,
        rotation: 20.0,
        opacity: 0.7,
        ..Transform::default()
    };
    let color = Color {
        contrast: 1.2,
        ..Color::default()
    };
    let effects = [VideoEffect::Blur { sigma: 3.0 }];
    let mask = Mask::default();
    let canvas = still_canvas(Fit::Contain);
    let last = still_clip_chain(
        &tf,
        &color,
        &effects,
        None,
        &canvas,
        Some(&mask),
        true,
        &StillFades::default(),
        false,
    );
    let first = still_clip_chain(
        &tf,
        &color,
        &effects,
        None,
        &canvas,
        Some(&mask),
        false,
        &StillFades::default(),
        false,
    );
    let zoom = "scale=iw*0.6:ih*0.6";
    // Last: after the rotation, with nothing behind it, and only once.
    assert!(last.ends_with(zoom), "{last}");
    assert_eq!(last.matches(zoom).count(), 1, "{last}");
    for before in ["eq=", "gblur=", "geq=", "colorchannelmixer=", "rotate="] {
        assert!(last.find(before).unwrap() < last.find(zoom).unwrap(), "{before}: {last}");
    }
    // First: ahead of `setsar` and all of them, and the chain otherwise the same.
    assert!(first.find(zoom).unwrap() < first.find("setsar=1").unwrap(), "{first}");
    assert!(!first.ends_with(zoom), "{first}");
    let strip = |c: &str| c.replace(&format!(",{zoom}"), "");
    assert_eq!(strip(&last), strip(&first), "only the zoom's place differs");
    // A zoom in under Contain that does not move is the fit's own scale (taken from the source once),
    // where a moving one is still fitted and then zoomed last.
    let tf_in = Transform { scale: 1.4, ..tf };
    let chain = |zoom_last: bool| {
        still_clip_chain(
            &tf_in,
            &color,
            &effects,
            None,
            &canvas,
            Some(&mask),
            zoom_last,
            &StillFades::default(),
            false,
        )
    };
    let (folded, moving) = (chain(false), chain(true));
    let fold = "scale=640*1.4:360*1.4:force_original_aspect_ratio=decrease";
    assert!(
        folded.contains(&format!("{fold},setsar=1")) && !folded.contains("iw*1.4"),
        "{folded}"
    );
    assert_eq!(folded.matches("scale=").count(), 1, "{folded}");
    assert!(moving.starts_with("trim=end_frame=1,setpts=PTS-STARTPTS,scale=640:360:") && moving.ends_with("scale=iw*1.4:ih*1.4"));
    // A moving zoom that has no zoom at the sampled instant has none to move.
    let at_rest = Transform { scale: 1.0, ..tf };
    assert_eq!(
        still_clip_chain(
            &at_rest,
            &color,
            &effects,
            None,
            &canvas,
            None,
            true,
            &StillFades::default(),
            false
        ),
        still_clip_chain(
            &at_rest,
            &color,
            &effects,
            None,
            &canvas,
            None,
            false,
            &StillFades::default(),
            false
        )
    );
}

/// Through the whole still graph: the moving-zoom clip's chain ends in the zoom, the same
/// keys at a constant scale zoom first.
#[test]
fn a_still_of_a_moving_zoom_clip_ends_its_chain_in_the_zoom() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let blurred = |first: f64, last: f64| {
        let mut c = make_clip(asset.id, 0.0, 4.0, -0.5); // the still at 1 s is 1.5 s in
        c.keyframes = vec![key(0.0, first, 0.0, 30.0, 1.0), key(2.0, last, 0.0, 30.0, 1.0)];
        c.effects = vec![crate::model::VideoEffect::Blur { sigma: 4.0 }];
        c
    };
    let chain_of = |clip: Clip| {
        let tl = single(vec![clip]);
        let args = build_timeline_frame_args(&tl, &assets, &ExportOptions::default(), 1.0, 640, 4).unwrap();
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        graph.split(';').find(|c| c.ends_with("[v0]")).unwrap().to_string()
    };
    // 0.5 to 1.5 over two seconds is 1.25 at 1.5 s: the export's placement, at that zoom.
    let moving = chain_of(blurred(0.5, 1.5));
    assert!(
        moving.ends_with(",crop=w=640:h=360:x='2*floor((iw-640)/4)':y='2*floor((ih-360)/4)'[v0]"),
        "{moving}"
    );
    assert!(
        moving.contains(")/(1.25)+W/2-0.5'") && !moving.contains("iw*1.25"),
        "{moving}"
    );
    assert!(
        moving.find("gblur=").unwrap() < moving.find("perspective=").unwrap(),
        "{moving}"
    );
    // Held at 1.25 the whole way: zoom first, as ever, and a zoom in under Contain is the fit's own scale.
    let held = chain_of(blurred(1.25, 1.25));
    assert!(!held.contains("iw*1.25"), "{held}");
    assert!(
        held.find("scale=640*1.25:360*1.25:force_original_aspect_ratio=decrease")
            .unwrap()
            < held.find("gblur=").unwrap(),
        "{held}"
    );
}

/// `scale` reads a size that evaluates to 0 as "keep the input's", so a picture zoomed
/// to a fraction of a pixel snapped to full size. Below `TINY_SCALE` the sizes are held
/// at 1 px, in the export, the playback and the still, for a constant zoom and a keyed
/// one; above it, no graph has changed by a character.
#[test]
fn a_tiny_scale_keeps_a_pixel_instead_of_snapping_to_full_size() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let fmt = ExportFormat::default();
    let chain = |clip: &Clip| video_clip_chain(clip, &fmt, &ClipFx::default(), false, "c0");
    // A constant scale.
    let mut tiny = make_clip(asset.id, 0.0, 4.0, 0.0);
    tiny.transform.scale = 0.0004;
    let c = chain(&tiny);
    assert!(c.contains("scale=w='max(1,iw*0.0004)':h='max(1,ih*0.0004)'"), "{c}");
    let mut small = tiny;
    small.transform.scale = 0.5;
    assert!(chain(&small).contains(",scale=iw*0.5:ih*0.5,"), "{}", chain(&small));
    // A keyed one that holds clamps when its key is tiny; one that moves is the placement,
    // which divides by the zoom and never sizes a picture by it.
    let mut keyed = make_clip(asset.id, 0.0, 4.0, 0.0);
    keyed.keyframes = vec![key(0.0, 0.0004, 0.0, 0.0, 1.0), key(2.0, 0.0004, 0.0, 0.0, 1.0)];
    let c = chain(&keyed);
    assert!(c.contains("scale=w='max(1,iw*(") && c.contains("eval=frame"), "{c}");
    assert!(c.contains("':h='max(1,ih*("), "{c}");
    keyed.keyframes = vec![key(0.0, 0.0004, 0.0, 0.0, 1.0), key(2.0, 1.0, 0.0, 0.0, 1.0)];
    let c = chain(&keyed);
    assert!(c.contains(",perspective=") && !c.contains("scale=w='"), "{c}");
    let mut keyed = make_clip(asset.id, 0.0, 4.0, 0.0);
    keyed.keyframes = vec![key(0.0, 0.3, 0.0, 0.0, 1.0), key(2.0, 1.6, 0.0, 0.0, 1.0)];
    assert!(!chain(&keyed).contains("max(1,"), "{}", chain(&keyed));
    // The still.
    let canvas = still_canvas(Fit::Contain);
    let tf = Transform {
        scale: 0.0004,
        ..Transform::default()
    };
    let c = still_clip_chain(
        &tf,
        &Color::default(),
        &[],
        None,
        &canvas,
        None,
        false,
        &StillFades::default(),
        false,
    );
    assert!(c.contains("scale=w='max(1,iw*0.0004)':h='max(1,ih*0.0004)'"), "{c}");
    let c = still_clip_chain(
        &Transform { scale: 0.5, ..tf },
        &Color::default(),
        &[],
        None,
        &canvas,
        None,
        false,
        &StillFades::default(),
        false,
    );
    assert!(c.contains("scale=iw*0.5:ih*0.5"), "{c}");
}

/// `zscale` refuses an odd size in 4:2:0, so for HDR footage (tone-mapped after the
/// geometry) the fit and a constant zoom come out even; a moving zoom runs after the
/// tone-map and needs nothing; and SDR footage is byte-for-byte what it was.
#[test]
fn the_sizes_ahead_of_a_tone_map_are_even() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let hlg = ClipFx {
        hdr: Some(crate::model::Hdr::Hlg),
        ..ClipFx::default()
    };
    let fmt = ExportFormat::default();
    let mut clip = make_clip(asset.id, 0.0, 4.0, 0.0);
    clip.transform.scale = 0.45;
    let sdr = video_clip_chain(&clip, &fmt, &ClipFx::default(), false, "c0");
    let hdr = video_clip_chain(&clip, &fmt, &hlg, false, "c0");
    assert!(!sdr.contains("force_divisible_by") && !sdr.contains("trunc("), "{sdr}");
    assert!(
        hdr.contains(":force_original_aspect_ratio=decrease:force_divisible_by=2"),
        "the fit: {hdr}"
    );
    assert!(
        hdr.contains("scale=w='max(2,2*trunc((iw*0.45)/2))':h='max(2,2*trunc((ih*0.45)/2))'"),
        "the zoom: {hdr}"
    );
    // Both sit ahead of the tone-map.
    let tone = hdr.find("zscale").or_else(|| hdr.find("colorspace")).unwrap();
    assert!(
        hdr.find("force_divisible_by").unwrap() < tone && hdr.find("trunc(").unwrap() < tone,
        "{hdr}"
    );
    // Cover crops the plain fit to the (even) frame; its intermediate size never reaches `zscale`.
    let cover = ExportFormat {
        fit: Fit::Cover,
        ..ExportFormat::default()
    };
    let plain = make_clip(asset.id, 0.0, 4.0, 0.0);
    assert!(!video_clip_chain(&plain, &cover, &hlg, false, "c0").contains("force_divisible_by"));
    // ... but a Cover clip that zooms is not cropped ahead of the tone-map: the cover of the frame
    // times the zoom reaches it, so that is made even (rounded up, as `increase` rounds).
    let zoomed = video_clip_chain(&clip, &cover, &hlg, false, "c0");
    assert!(
            zoomed.contains(
                "scale=w='max(2,2*trunc((1920*0.45)/2))':h='max(2,2*trunc((1080*0.45)/2))':force_original_aspect_ratio=increase:force_divisible_by=2"
            ),
            "{zoomed}"
        );
    // A moving zoom is the last stage, after the tone-map: its sizes are not made even.
    clip.keyframes = vec![key(0.0, 0.3, 0.0, 0.0, 1.0), key(2.0, 1.6, 0.0, 0.0, 1.0)];
    let moving = video_clip_chain(&clip, &fmt, &hlg, false, "c0");
    assert!(!moving.contains("trunc("), "{moving}");
    assert!(moving.contains("force_divisible_by=2"), "the fit is still even: {moving}");
    // The still tone-maps the source before any geometry: nothing to do there.
    let still = still_clip_chain(
        &clip.transform,
        &Color::default(),
        &[],
        None,
        &still_canvas(Fit::Contain),
        None,
        false,
        &StillFades::default(),
        false,
    );
    assert!(!still.contains("force_divisible_by") && !still.contains("trunc("), "{still}");
}

/// A source with an alpha channel keeps it to the overlay (the terminal `format=` of a
/// non-alpha chain flattened a transparent sticker onto black), and an unrecorded or
/// opaque pixel format keeps the chain it always had.
#[test]
fn a_source_with_alpha_ends_its_chain_in_an_alpha_format() {
    let mut sticker = img_asset(Uuid::new_v4());
    sticker.streams[0].pix_fmt = Some("rgba".into());
    let mut clip = make_clip(sticker.id, 0.0, 3.0, 0.0);
    clip.transform.scale = 0.5;
    let timeline = single(vec![clip.clone()]);
    let fx = transition_fx(&timeline, std::slice::from_ref(&sticker));
    assert!(fx[0].alpha, "the probed format says rgba");
    let fmt = ExportFormat::default();
    let chain = video_clip_chain(&clip, &fmt, &fx[0], true, "c0");
    assert!(chain.ends_with(",format=yuva420p"), "{chain}");
    assert!(!chain.contains("format=yuv420p"), "{chain}");
    // Opaque, and never recorded: today's chain.
    for pix_fmt in [Some("rgb24"), Some("yuv420p"), None] {
        sticker.streams[0].pix_fmt = pix_fmt.map(String::from);
        let fx = transition_fx(&timeline, std::slice::from_ref(&sticker));
        assert!(!fx[0].alpha, "{pix_fmt:?}");
        let chain = video_clip_chain(&clip, &fmt, &fx[0], true, "c0");
        assert!(chain.ends_with(",format=yuv420p"), "{pix_fmt:?}: {chain}");
    }
    // The names that carry alpha, by `pix_fmt_has_alpha`.
    for pix_fmt in ["rgba", "bgra", "yuva420p", "yuva444p10le", "gbrap", "pal8", "ya8"] {
        sticker.streams[0].pix_fmt = Some(pix_fmt.into());
        assert!(transition_fx(&timeline, std::slice::from_ref(&sticker))[0].alpha, "{pix_fmt}");
    }
    // A chain that is already alpha keeps it; a moving zoom adds no second `format=yuva420p`.
    let alpha = ClipFx {
        alpha: true,
        ..ClipFx::default()
    };
    let mut faded = clip.clone();
    faded.transform.opacity = 0.5;
    let chain = video_clip_chain(&faded, &fmt, &alpha, true, "c0");
    assert!(chain.ends_with("colorchannelmixer=aa=0.5"), "{chain}");
    let mut moving = clip;
    moving.keyframes = vec![key(0.0, 0.3, 0.0, 0.0, 1.0), key(2.0, 1.6, 0.0, 0.0, 1.0)];
    let chain = video_clip_chain(&moving, &fmt, &alpha, true, "c0");
    assert!(
        chain.contains(",format=yuva420p,pad=") && chain.matches("format=yuva420p").count() == 1,
        "{chain}"
    );
}

/// A moving zoom and position reach `overlay` as a `yuva420p` frame of the canvas' size,
/// the picture already placed on it at sub-pixel positions, and the overlay puts it at the
/// origin: a per-frame `scale` and `overlay` offsets moved it in whole, and even, pixels.
#[test]
fn a_moving_zoom_and_position_are_placed_at_sub_pixel_positions_on_a_frame_of_the_canvas() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let mut clip = make_clip(asset.id, 0.0, 4.0, 1.0);
    clip.keyframes = vec![key(0.0, 0.4, -0.2, 0.0, 1.0), key(2.0, 1.6, 0.2, 0.0, 1.0)];
    let timeline = single(vec![clip]);
    let g = build_filter_complex(
        &timeline,
        &assets,
        &ExportFormat::default(),
        timeline.duration(),
        &ExportOptions::default(),
        true,
        false,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    );
    let zoomed = g
        .filter
        .split(';')
        .find(|c| c.starts_with("[0:v]"))
        .expect("the clip's chain");
    assert!(
        zoomed.ends_with(",crop=w=1920:h=1080:x='2*floor((iw-1920)/4)':y='2*floor((ih-1080)/4)'[v0]"),
        "{zoomed}"
    );
    // `perspective` has no clock: the frame it draws is counted from a pinned `fps`.
    assert!(zoomed.contains(",fps=fps=30:start_time=1,"), "{zoomed}");
    assert!(
        zoomed.contains("perspective=x0='(0+0.5-2*floor((W-1920)/4)-(960+(if(lt((((in+29)*1/30)-1),0),-0.2,"),
        "{zoomed}"
    );
    assert!(
        g.filter
            .contains("[vbase][v0]overlay=eof_action=pass:enable='between(t,0.9666666666666667,5)'"),
        "{}",
        g.filter
    );
}

#[test]
fn text_overlay_drawn_over_composite() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let mut timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    timeline.overlays = vec![TextOverlay::new("Hello", 1.0, 4.0)];
    let g = build_filter_complex(
        &timeline,
        &assets,
        &ExportFormat::default(),
        timeline.duration(),
        &ExportOptions::default(),
        true,
        false,
        &plan_inputs(&timeline, &assets, &transition_fx(&timeline, &assets)),
    );
    let f = g.filter;
    // The composite lands on `vtext`, then drawtext writes the final `[outv]`.
    assert!(f.contains("[vtext]"), "composite pad before text: {f}");
    assert!(f.contains("drawtext=") && f.contains("text='Hello'"), "{f}");
    assert!(f.contains("enable='between(t,1,4)'"), "gated to its lifetime: {f}");
    assert!(f.contains("[outv]"));
}

#[test]
fn drawtext_escapes_each_of_the_three_levels_a_value_is_read_at() {
    // Apostrophe: escaped for the option string (`\'`), then the graph (`'\''`).
    assert_eq!(escape_drawtext_text("a'b"), "a\\'\\''b");
    // Colon: the option string would end the value there.
    assert_eq!(escape_drawtext_text("Note: this"), "Note\\: this");
    // Backslash: once for the expansion, once for the option string.
    assert_eq!(escape_drawtext_text("a\\b"), "a\\\\\\\\b");
    // Percent: the expansion's `\%`, whose backslash the option string doubles.
    assert_eq!(escape_drawtext_text("50% off"), "50\\\\% off");
    // Blanks at the ends would be dropped by the option string; inside they are fine.
    assert_eq!(escape_drawtext_text("  a b \t"), "\\ \\ a b\\ \\\t");
    assert_eq!(escape_drawtext_text("   "), "\\ \\ \\ ");
    // Characters only the graph level knows about stay inside the quotes.
    assert_eq!(escape_drawtext_text("a, b; [c]"), "a, b; [c]");
}

#[test]
fn drawtext_drops_control_characters_and_changes_nothing_else() {
    // A NUL makes `Command::spawn` fail for the whole filtergraph; ESC and the
    // C1 block are noise. All of them go.
    assert_eq!(escape_drawtext_text("a\0b\u{1b}[0mc\u{7f}d\u{85}e"), "ab[0mcde");
    assert_eq!(escape_drawtext_path("/fonts/\0x.ttf"), "/fonts/x.ttf");
    // Plain text is untouched: punctuation that needs no escape, a tab inside, accents, emoji, CJK.
    for plain in ["Hello, world", "50 OFF - it is \"live\"", "tab\there", "Café ☕ 日本語 🎬"] {
        assert_eq!(escape_drawtext_text(plain), plain);
    }
    // Every kind of line break is one space: drawtext starts a new line at a CR as well as an LF.
    assert_eq!(escape_drawtext_text("two\nlines"), "two lines");
    assert_eq!(escape_drawtext_text("crlf\r\nline"), "crlf line");
    assert_eq!(
        escape_drawtext_text("cr\rline\u{2028}sep\u{2029}par\n\rx"),
        "cr line sep par  x"
    );
}

#[test]
fn drawtext_text_escapes_percent_but_path_does_not() {
    // A bare `%` is a drawtext configuration error that silently blanks the
    // whole overlay. drawtext's own literal-`%` escape is `\%`, and its backslash is
    // doubled for the option string. `fontfile=` is a path, not drawtext-expanded text,
    // so a `%` in it must not be touched.
    assert_eq!(escape_drawtext_text("50% OFF"), "50\\\\% OFF");
    assert_eq!(
        escape_drawtext_path("C:\\fonts\\100% Arial.ttf"),
        "C\\:\\\\fonts\\\\100% Arial.ttf"
    );
    assert_eq!(escape_drawtext_path("/Users/o'neil/a.ttf"), "/Users/o\\'\\''neil/a.ttf");
}

#[test]
fn drawtext_export_escapes_percent_in_text() {
    let o = TextOverlay::new("Battery: 82%", 0.0, 1.0);
    let f = drawtext_export(&o, &ExportFormat::default());
    assert!(f.contains("text='Battery\\: 82\\\\%'"), "{f}");
}

/// What the user typed is what is drawn: each text through the real `drawtext` against the same
/// text read literally from a file, on whichever FFmpeg runs this (the apostrophe, the colon and
/// the backslash each used to draw something else: see [`escape_drawtext`]).
#[test]
#[ignore = "needs ffmpeg"]
fn drawtext_text_is_what_the_user_typed() {
    let font = crate::fonts::default_font_file(false).expect("the default font");
    let dir = std::env::temp_dir().join(format!("kerf-drawtext-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let render = |options: Vec<String>| -> Vec<u8> {
        let graph = format!("color=c=black:s=500x80:d=1:r=1,drawtext={}:x=5:y=20[o]", options.join(":"));
        let out = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-filter_complex", &graph])
            .args(["-map", "[o]", "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdin(Stdio::null())
            .output()
            .expect("run ffmpeg");
        assert!(out.status.success(), "{graph}: {}", String::from_utf8_lossy(&out.stderr));
        out.stdout
    };
    let blank = render(vec!["text=' '".into(), "fontsize=30".into()]);
    for text in [
        "Hello",
        "a: b",
        "it's",
        "'quoted'",
        "50% off",
        "100%",
        "%{pts}",
        "back\\slash",
        "C:\\dir\\file.txt",
        "trailing\\",
        "a, b; [c] {d}",
        "x:y:z",
        "it's: a \"test\", 50% \\ done; [ok]",
        "  spaced out  ",
        "Café 日本語 Дп",
    ] {
        let look = TextLook {
            text,
            size: 0.375,
            color: "white",
            bg: None,
            font_file: Some(&font),
            border: false,
        };
        let ours = render(look.options(80.0));
        let file = dir.join("t.txt");
        std::fs::write(&file, text).unwrap();
        let mut literal = look.options(80.0);
        literal[0] = format!("textfile='{}'", escape_drawtext_path(&file.to_string_lossy()));
        literal.push("expansion=none".into());
        assert_ne!(ours, blank, "{text:?} drew nothing");
        assert!(ours == render(literal), "{text:?} is not what was typed");
    }
    // A line break of any kind is the one space it is turned into, never a second line.
    let look = |text| TextLook {
        text,
        size: 0.375,
        color: "white",
        bg: None,
        font_file: Some(&font),
        border: false,
    };
    let one_line = render(look("one two").options(80.0));
    for broken in ["one\ntwo", "one\rtwo", "one\r\ntwo", "one\u{2028}two"] {
        assert!(render(look(broken).options(80.0)) == one_line, "{broken:?} is not one line");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malicious_colour_never_reaches_the_graph() {
    let mut overlay = TextOverlay::new("Hello", 0.0, 1.0);
    let injection = "white,drawtext=textfile='/etc/passwd'";
    overlay.color = injection.to_string();
    overlay.bg = Some(injection.to_string());
    let drawn = drawtext_export(&overlay, &ExportFormat::default());
    assert!(!drawn.contains("/etc/passwd") && !drawn.contains("textfile"), "{drawn}");
    assert!(drawn.contains("fontcolor=white"), "invalid colour falls back: {drawn}");
    assert!(!drawn.contains("box=1"), "invalid box colour drops the box: {drawn}");

    let chroma = chroma_filter(&VideoEffect::ChromaKey {
        color: injection.to_string(),
        similarity: 0.1,
        blend: 0.0,
    })
    .unwrap();
    assert!(!chroma.contains("/etc/passwd"), "{chroma}");
    assert_eq!(chroma, "chromakey=green:0.1:0", "invalid colour falls back: {chroma}");
    // The renderer's probe asks for the same text without an effect to hold it.
    assert_eq!(chroma_key_filter(injection, 0.1, 0.0), chroma);
    assert_eq!(chroma_key_filter("0x1a8cd8", 0.3, 0.1), "chromakey=0x1a8cd8:0.3:0.1");
}

#[test]
fn malicious_pix_fmt_scaler_and_gif_dither_never_reach_the_graph() {
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        pix_fmt: Some("yuv420p,drawtext=textfile='/etc/passwd'".into()),
        scaler: Some("bilinear:x=1".into()),
        ..Default::default()
    };
    let args = args_of(&opts);
    let filter = flag_val(&args, "-filter_complex").unwrap();
    assert!(!filter.contains("/etc/passwd") && !filter.contains(":x=1"), "{filter}");
    // Invalid pix_fmt is dropped, never passed through raw.
    assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv420p"));

    let gif_opts = ExportOptions {
        container: Container::Gif,
        video_codec: Some("gif".into()),
        include_audio: false,
        gif_dither: Some("bayer,drawtext=textfile='/etc/passwd'".into()),
        ..Default::default()
    };
    let gif_filter = flag_val(&args_of(&gif_opts), "-filter_complex").unwrap().to_string();
    assert!(gif_filter.contains("paletteuse=dither=bayer["), "{gif_filter}");
    assert!(!gif_filter.contains("/etc/passwd"), "{gif_filter}");
}

#[test]
fn validate_export_flags_unsupported_pix_fmt_scaler_and_dither() {
    let opts = ExportOptions {
        pix_fmt: Some("nope".into()),
        scaler: Some("nope".into()),
        gif_dither: Some("nope".into()),
        ..Default::default()
    };
    let issues = validate_export(&opts, true, true);
    assert_eq!(issues.len(), 3, "{issues:?}");
}

#[test]
fn drawtext_always_names_a_font_file() {
    // Every overlay is drawn with a file the plan can name: with no family, or one that is not
    // installed, the shipped default (a bare `drawtext` asks its own build for "Sans", which a
    // build without fontconfig does not have).
    let regular = crate::fonts::default_font_file(false).expect("a writable cache directory");
    for family in [None, Some("Definitely Not An Installed Font XYZ123")] {
        let mut o = TextOverlay::new("Hi", 0.0, 1.0);
        o.font = family.map(String::from);
        let f = drawtext_export(&o, &ExportFormat::default());
        assert!(
            f.contains(&format!("fontfile='{}'", escape_drawtext_path(&regular.to_string_lossy()))),
            "{family:?}: {f}"
        );
        assert!(!f.contains("borderw"), "{f}");
    }
}

#[test]
fn drawtext_bold_uses_the_bold_face_and_only_a_face_without_one_gets_the_border() {
    let mut o = TextOverlay::new("Hi", 0.0, 1.0);
    o.bold = true;
    let bold = crate::fonts::default_font_file(true).expect("a writable cache directory");
    let f = drawtext_export(&o, &ExportFormat::default());
    assert!(
        f.contains(&format!("fontfile='{}'", escape_drawtext_path(&bold.to_string_lossy()))) && !f.contains("borderw"),
        "the default has a bold face: {f}"
    );
    // A family with no bold face is thickened with a same-colour border instead.
    o.font = Some(crate::fonts::PINNED_FAMILY.into());
    let f = crate::fonts::with_pinned_default_font(|| drawtext_export(&o, &ExportFormat::default()));
    assert!(
        f.contains("fontfile='/kerf-test/fonts/PinnedSans-Regular.ttf'") && f.contains("borderw=2:bordercolor=white"),
        "{f}"
    );
}

#[test]
fn a_plans_text_is_the_drawtext_the_still_draws() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let mut tl = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let mut a = TextOverlay::new("Fish: 50% off, it's \\ \"fine\"", 0.0, 10.0);
    a.bold = true;
    a.bg = Some("black@0.5".into());
    a.color = "yellow@0.9".into();
    let mut b = TextOverlay::new("Moving", 0.0, 10.0);
    b.font = Some(crate::fonts::PINNED_FAMILY.into());
    b.bold = true;
    b.keyframes = vec![
        crate::model::TextKeyframe {
            time: 0.0,
            pos_x: 0.2,
            pos_y: 0.3,
            opacity: 0.2,
        },
        crate::model::TextKeyframe {
            time: 8.0,
            pos_x: 0.8,
            pos_y: 0.6,
            opacity: 1.0,
        },
    ];
    tl.overlays = vec![a, b];
    for t in [0.0, 3.3, 9.0] {
        let plan = crate::render_plan::RenderPlan::at(
            &tl,
            &assets,
            &ExportOptions::default(),
            t,
            crate::render_plan::CompositeColorPolicy::FixedBt601,
        )
        .unwrap();
        assert_eq!(plan.overlays.len(), 2);
        for (text, overlay) in plan.overlays.iter().zip(&tl.overlays) {
            assert_eq!(text_still_filter(text, 540), drawtext_still(overlay, 540, t), "t = {t}");
        }
    }
}

#[test]
fn a_piece_of_a_text_is_drawn_in_the_look_of_its_overlay() {
    use crate::render_plan::PlanText;
    let t = PlanText {
        id: uuid::Uuid::nil(),
        text: "ignored".into(),
        size: 0.06,
        pos: (0.5, 0.82),
        alpha: 1.0,
        color: "yellow".into(),
        bg: None,
        font_file: Some("/fonts/A b.ttf".into()),
        synthetic_bold: true,
    };
    let piece = |text, lines, border, boxed| TextPiece {
        text,
        lines,
        font: "white",
        border,
        boxed,
    };
    // The overlay's size, font and border; the piece's text and colours; then the caller's options.
    assert_eq!(
        text_piece_filter(&t, 960, &piece("a:b", false, Some("white@0"), None), &["x=0", "y=ascent"]),
        "drawtext=text='a\\:b':fontsize=58:fontcolor=white:fontfile='/fonts/A b.ttf':borderw=2:bordercolor=white@0:x=0:y=ascent"
    );
    // The border is the overlay's: none to draw, none drawn, whatever the piece asks.
    let mut plain = t.clone();
    plain.synthetic_bold = false;
    let f = text_piece_filter(&plain, 960, &piece("ab", false, Some("white"), None), &[]);
    assert!(!f.contains("borderw"), "{f}");
    let f = text_piece_filter(&t, 960, &piece("ab", false, None, None), &[]);
    assert!(!f.contains("borderw"), "a piece may leave the border out: {f}");
    let f = text_piece_filter(&t, 960, &piece("ab", false, None, Some(("white@0", 24))), &[]);
    assert!(f.ends_with("box=1:boxcolor=white@0:boxborderw=24"), "{f}");
    // A line break is a blank, unless the piece is lines (a prefix per line, as the measure asks).
    let f = text_piece_filter(&t, 960, &piece("a\nab", false, None, None), &[]);
    assert!(f.contains("text='a ab'"), "{f}");
    let f = text_piece_filter(&t, 960, &piece("a\nab", true, None, None), &[]);
    assert!(f.contains("text='a\nab'"), "{f}");
    // ...and what `drawtext` draws of a text is what the escape keeps of it.
    assert_eq!(text_as_drawn("a\r\nb\u{2028}c\u{7}d\te"), "a b cd\te");
    assert_eq!(text_as_drawn("50% off: it's"), "50% off: it's", "escaping is not cleaning");
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "a text piece colour")]
fn a_piece_colour_that_could_carry_an_option_is_a_bug_in_the_caller() {
    use crate::render_plan::PlanText;
    let t = PlanText {
        id: uuid::Uuid::nil(),
        text: "x".into(),
        size: 0.06,
        pos: (0.5, 0.5),
        alpha: 1.0,
        color: "white".into(),
        bg: None,
        font_file: None,
        synthetic_bold: false,
    };
    let piece = TextPiece {
        text: "x",
        lines: false,
        font: "white:fontsize=1",
        border: None,
        boxed: None,
    };
    let _ = text_piece_filter(&t, 960, &piece, &[]);
}

#[test]
fn the_raster_graph_is_the_stills_drawtext_with_each_part_in_white() {
    use crate::render_plan::PlanText;
    let mut t = PlanText {
        id: uuid::Uuid::nil(),
        text: "50% it's".into(),
        size: 0.06,
        pos: (0.5, 0.82),
        alpha: 0.4,
        color: "yellow@0.9".into(),
        bg: None,
        font_file: Some("/fonts/A b.ttf".into()),
        synthetic_bold: false,
    };
    // The still's own text for the same overlay, to compare the shared options with.
    let mut o = TextOverlay::new(t.text.clone(), 0.0, 1.0);
    o.size = t.size;
    o.color = t.color.clone();
    let still = drawtext_still(&o, 960, 0.5);

    let plain = text_raster_graph(&t, (540, 960));
    assert_eq!(plain.parts, vec![TextPart::Glyphs]);
    assert_eq!(
            plain.filter_complex,
            format!(
                "color=c=black:s=540x960:r=1:d=1,format=rgb24,drawtext=text='{}':fontsize=58:fontcolor=white:fontfile='/fonts/A b.ttf':x=(w*0.5-text_w/2):y=(h*0.82-text_h/2),extractplanes=r[o]",
                escape_drawtext_text("50% it's")
            )
        );
    assert!(
        still.contains(&format!("text='{}':fontsize=58", escape_drawtext_text("50% it's"))),
        "{still}"
    );
    // A box adds a part; what a part does not draw is invisible, not left out (drawtext's clip
    // rectangle grows with the box and the border). A synthetic border is part of the glyphs' picture.
    t.bg = Some("black@0.5".into());
    t.synthetic_bold = true;
    let full = text_raster_graph(&t, (540, 960));
    assert_eq!(full.parts, vec![TextPart::Glyphs, TextPart::Box]);
    let f = &full.filter_complex;
    assert!(
        f.contains("split=2[a0][a1];") && f.ends_with("[p0][p1]vstack=inputs=2[o]"),
        "{f}"
    );
    for (branch, font, border, boxed) in [("[a0]", "white", "white", "white@0"), ("[a1]", "white@0", "white@0", "white")] {
        let want = format!(
            "fontcolor={font}:fontfile='/fonts/A b.ttf':borderw=2:bordercolor={border}:box=1:boxcolor={boxed}:boxborderw=12:x="
        );
        let part = f.split(';').find(|p| p.starts_with(branch)).unwrap();
        assert!(part.contains(&want), "{part}");
    }
}

#[test]
fn drawtext_resolves_installed_font_to_fontfile() {
    // Environment-dependent: skip if this machine has no fonts installed
    // at all, rather than hardcoding a family that may be absent on some
    // CI runner OS.
    let Some(family) = crate::fonts::list_system_fonts().into_iter().next() else {
        return;
    };
    let mut o = TextOverlay::new("Hi", 0.0, 1.0);
    o.font = Some(family);
    let f = drawtext_export(&o, &ExportFormat::default());
    assert!(f.contains("fontfile='"), "installed font resolves to a fontfile: {f}");
}

#[test]
fn still_frame_samples_keyframes_and_draws_overlays() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let mut timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    timeline.overlays = vec![TextOverlay::new("Caption", 0.0, 10.0)];
    let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 3.0, 640, 4).unwrap();
    let joined = args.join(" ");
    assert!(
        joined.contains("drawtext=") && joined.contains("text='Caption'"),
        "still draws overlays: {joined}"
    );
    assert!(joined.contains("null[outv]"), "still terminates on [outv]: {joined}");
}

/// Two adjacent 10 s clips of one 100 s, 30 fps asset (source 0..10 and 20..30), the second
/// entering through `kind` over `secs`: the cut the transition stills are built from.
fn transition_cut(kind: TransitionKind, secs: f64) -> (Timeline, Vec<Asset>) {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 20.0, 30.0, 10.0);
    b.transition_in = Some(crate::model::Transition { kind, duration: secs });
    (single(vec![a, b]), vec![asset])
}

fn still_args_at(timeline: &Timeline, assets: &[Asset], t: f64) -> Vec<String> {
    build_timeline_frame_args(timeline, assets, &ExportOptions::default(), t, 640, 4).unwrap()
}

/// The `-ss` of every input of a still's argv.
fn seeks(args: &[String]) -> Vec<&str> {
    args.windows(2).filter(|w| w[0] == "-ss").map(|w| w[1].as_str()).collect()
}

/// The still of a frame inside a dissolve is the export's: the outgoing clip on its tail,
/// from the source it borrows, under the incoming clip's alpha ramp, run through the
/// export's own `fade` filter at the output frame the time falls in.
#[test]
fn a_still_inside_a_dissolve_draws_the_tail_and_the_ramp() {
    let (tl, assets) = transition_cut(TransitionKind::Crossfade, 1.0);
    let args = still_args_at(&tl, &assets, 10.5);
    assert_eq!(
        seeks(&args),
        ["10.500000", "20.500000"],
        "the outgoing clip plays on past its end"
    );
    let graph = args.join(" ");
    assert!(
        graph.contains("format=yuva420p,settb=expr=1/30,setpts=315,fade=t=in:st=10:d=1:alpha=1,setpts=PTS-STARTPTS[v1]"),
        "the ramp is the export's fade, at frame 315: {graph}"
    );
    assert!(
        !graph.contains("[0:v]") || !graph.split("[v0]").next().unwrap().contains("fade"),
        "{graph}"
    );
    // Before the dissolve and once it is over there is one clip and no fade.
    for t in [5.0, 9.99, 11.2, 19.9] {
        let plain = still_args_at(&tl, &assets, t).join(" ");
        assert!(!plain.contains("fade") && !plain.contains("settb"), "t = {t}: {plain}");
        assert_eq!(seeks(&still_args_at(&tl, &assets, t)).len(), 1, "t = {t}");
    }
}

/// A dip fades the picture itself, to black or through white, either side of the cut; the
/// chain then ends in the format the export's does, which is what the fade runs on.
#[test]
fn a_still_inside_a_dip_fades_the_picture_in_the_exports_pixel_format() {
    let (tl, assets) = transition_cut(TransitionKind::DipToBlack, 2.0);
    let before = still_args_at(&tl, &assets, 9.5).join(" ");
    assert!(
        before.contains("settb=expr=1/30,setpts=285,fade=t=out:st=9:d=1,setpts=PTS-STARTPTS,format=yuv420p[v0]"),
        "{before}"
    );
    let after = still_args_at(&tl, &assets, 10.5).join(" ");
    assert!(
        after.contains("setpts=315,fade=t=in:st=10:d=1,setpts=PTS-STARTPTS,format=yuv420p[v0]"),
        "{after}"
    );
    assert!(
        !after.contains("alpha=1") && !after.contains("yuva420p"),
        "a dip needs no alpha plane: {after}"
    );
    let (white, assets) = transition_cut(TransitionKind::DipToWhite, 2.0);
    assert!(still_args_at(&white, &assets, 10.5)
        .join(" ")
        .contains("fade=t=in:st=10:d=1:c=white"));
    // A picture that has an alpha channel of its own keeps it through the fade.
    let (tl, mut assets) = transition_cut(TransitionKind::DipToBlack, 2.0);
    assets[0].streams[0].pix_fmt = Some("yuva420p".into());
    assert!(still_args_at(&tl, &assets, 9.5)
        .join(" ")
        .contains("fade=t=out:st=9:d=1,setpts=PTS-STARTPTS,format=yuva420p[v0]"));
}

/// A clip that entered through a dissolve has its whole chain on an alpha plane in the export,
/// so a fade of its own after the ramp is over runs on `yuva420p` (where black is 0), and the
/// still's chain ends in no `yuv420p` that would run it on another format.
#[test]
fn a_fade_after_a_dissolve_runs_on_yuva420p_like_the_exports() {
    let (mut tl, assets) = transition_cut(TransitionKind::Crossfade, 1.0);
    tl.tracks[0].clips[1].fade_out = 1.0;
    let graph = still_args_at(&tl, &assets, 19.5).join(" ");
    assert!(
        graph.contains("setsar=1,format=yuva420p,settb=expr=1/30,setpts=585,fade=t=out:st=19:d=1,setpts=PTS-STARTPTS[v0]"),
        "{graph}"
    );
    assert!(!graph.contains("format=yuv420p"), "{graph}");
    // Outside the fade it is the plain chain.
    let plain = still_args_at(&tl, &assets, 15.0).join(" ");
    assert!(!plain.contains("yuva420p") && !plain.contains("fade"), "{plain}");
}

/// A clip's own fade-in is drawn until it is done, and then the still is what it always was.
#[test]
fn a_still_inside_a_clips_own_fade_runs_the_fade_until_it_is_over() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.fade_in = 1.0;
    clip.fade_out = 1.0;
    let tl = single(vec![clip]);
    let assets = [asset];
    assert!(still_args_at(&tl, &assets, 0.0)
        .join(" ")
        .contains("setpts=0,fade=t=in:st=0:d=1,"));
    assert!(still_args_at(&tl, &assets, 0.5)
        .join(" ")
        .contains("setpts=15,fade=t=in:st=0:d=1,"));
    assert!(still_args_at(&tl, &assets, 9.5)
        .join(" ")
        .contains("setpts=285,fade=t=out:st=9:d=1,"));
    for t in [1.0, 5.0, 8.99] {
        assert!(!still_args_at(&tl, &assets, t).join(" ").contains("fade"), "t = {t}");
    }
}

/// A slide adds its travel at this frame to the overlay position, a push moves both clips,
/// and a clip that is not travelling is placed as ever.
#[test]
fn a_still_inside_a_slide_or_a_push_places_the_clips_by_their_travel() {
    let (tl, assets) = transition_cut(TransitionKind::SlideLeft, 2.0);
    let graph = still_args_at(&tl, &assets, 11.0).join(" ");
    assert!(
        graph.contains("[ov0][v1]overlay=x=(W-w)/2+(0.5)*W:y=(H-h)/2+(0)*H[ov1]"),
        "{graph}"
    );
    assert!(
        graph.contains("[base][v0]overlay=(W-w)/2:(H-h)/2[ov0]"),
        "a slide covers the outgoing clip where it sits: {graph}"
    );
    assert!(!graph.contains("fade"), "{graph}");
    let (push, assets) = transition_cut(TransitionKind::PushUp, 2.0);
    let graph = still_args_at(&push, &assets, 11.0).join(" ");
    assert!(
        graph.contains("[base][v0]overlay=x=(W-w)/2+(0)*W:y=(H-h)/2+(-0.5)*H[ov0]"),
        "{graph}"
    );
    assert!(
        graph.contains("[ov0][v1]overlay=x=(W-w)/2+(0)*W:y=(H-h)/2+(0.5)*H[ov1]"),
        "{graph}"
    );
}

/// Nothing about a still changes outside a transition: the same cut without its transition
/// gives the same argv at every time a transition does not reach.
#[test]
fn a_still_outside_every_transition_is_what_it_was() {
    for kind in [
        TransitionKind::Crossfade,
        TransitionKind::DipToBlack,
        TransitionKind::SlideLeft,
        TransitionKind::PushDown,
    ] {
        let (tl, assets) = transition_cut(kind, 2.0);
        let mut bare = tl.clone();
        bare.tracks[0].clips[1].transition_in = None;
        for t in [0.0, 4.0, 7.9, 12.1, 19.9] {
            assert_eq!(
                still_args_at(&tl, &assets, t),
                still_args_at(&bare, &assets, t),
                "{kind:?} at {t}"
            );
        }
    }
}

/// A moving zoom is the last stage of the export's chain, after the fades; the still follows.
#[test]
fn a_fade_runs_ahead_of_a_moving_zoom() {
    let (mut tl, assets) = transition_cut(TransitionKind::Crossfade, 1.0);
    let key = |time, scale| crate::model::Keyframe {
        time,
        scale,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity: 1.0,
        easing: Default::default(),
    };
    tl.tracks[0].clips[1].keyframes = vec![key(0.0, 0.5), key(2.0, 1.0)];
    let graph = still_args_at(&tl, &assets, 10.5).join(" ");
    let chain = graph.split("[v1]").next().unwrap().rsplit(';').next().unwrap().to_string();
    let (fade, zoom) = (chain.find("fade=").unwrap(), chain.rfind("perspective=").unwrap());
    assert!(fade < zoom, "{chain}");
    assert!(!chain.contains("format=yuv420p"), "{chain}");
}

#[test]
fn srt_export_formats_timecodes() {
    use crate::model::TranscriptSegment;
    let srt = crate::model::transcript_to_srt(&[
        TranscriptSegment {
            start: 1.5,
            end: 3.0,
            text: "first".into(),
            ..Default::default()
        },
        TranscriptSegment {
            start: 3.0,
            end: 4.25,
            text: "second".into(),
            ..Default::default()
        },
    ]);
    assert!(srt.contains("1\n00:00:01,500 --> 00:00:03,000\nfirst"), "{srt}");
    assert!(srt.contains("2\n00:00:03,000 --> 00:00:04,250\nsecond"), "{srt}");
}

#[test]
fn contact_sheet_samples_evenly_and_tiles() {
    let (args, times) = build_contact_sheet_args("/media/clip.mp4", 0.0, 40.0, 4, 4, 240, 5, None);
    let joined = args.join(" ");
    // 16 cells across 40s -> one frame every 2.5s, row-major.
    assert_eq!(times.len(), 16);
    assert!((times[0] - 0.0).abs() < 1e-9);
    assert!((times[1] - 2.5).abs() < 1e-9);
    assert!((times[15] - 37.5).abs() < 1e-9);
    // Seek/limit to the window, sample with fps, scale cells, tile to one sheet.
    // A sheet from the start does not seek: on a JPEG still any `-ss` decodes nothing.
    assert!(!joined.contains("-ss"), "{joined}");
    assert!(joined.contains("-t 40.000"));
    assert!(joined.contains("fps=0.4")); // 16 / 40
    assert!(joined.contains("scale=240:-2"));
    assert!(joined.contains("tile=4x4"));
    assert!(joined.contains("-vcodec mjpeg"));
    assert!(joined.contains("-q:v 5"));
    assert!(joined.ends_with("pipe:1"));
}

/// A region zoom crops before it scales, so the width budget lands on the
/// region; the whole frame is left byte-identical to the plain decode.
#[test]
fn region_frame_crops_then_scales_and_a_full_region_is_no_crop() {
    let r = Region {
        left: 0.25,
        top: 0.1,
        width: 0.5,
        height: 0.3,
    };
    let vf = region_frame_filter(r, 640);
    assert_eq!(
        vf,
        "crop=2*trunc(iw*0.5000/2):2*trunc(ih*0.3000/2):iw*0.2500:ih*0.1000,scale='2*trunc(min(640,iw)/2)':-2"
    );
    assert!(Region::FULL.is_full());
    assert!(!r.is_full());
}

/// Whatever the model asks for is pulled into the frame: a corner past the
/// far edge comes back in, and a region hanging over the edge is shrunk
/// rather than moved — the corner is the thing that was pointed at.
#[test]
fn region_normalizes_into_the_frame() {
    let r = Region {
        left: 0.8,
        top: -0.5,
        width: 0.6,
        height: f64::NAN,
    }
    .normalized();
    assert!((r.left - 0.8).abs() < 1e-9);
    assert_eq!(r.top, 0.0);
    assert!((r.width - 0.2).abs() < 1e-9, "shrunk to the edge: {r:?}");
    assert_eq!(r.height, Region::MIN_SIDE);
    let far = Region {
        left: 2.0,
        top: 0.0,
        width: 1.0,
        height: 1.0,
    }
    .normalized();
    assert!((far.left - (1.0 - Region::MIN_SIDE)).abs() < 1e-9);
    assert!((far.width - Region::MIN_SIDE).abs() < 1e-9);
}

/// Zooming the composite renders a canvas large enough for the region to
/// fill the requested width, then crops it — never past the delivery frame.
#[test]
fn timeline_region_widens_the_canvas_and_crops_the_composite() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    let r = Region {
        left: 0.5,
        top: 0.5,
        width: 0.25,
        height: 0.25,
    };
    let out = StillOutput::JpegPipe { quality: 2 };
    let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, 320, Some(r), &out).unwrap();
    let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
    // 320 / 0.25 = 1280 canvas, so the crop comes out 320 wide.
    assert!(graph.contains("s=1280x720"), "{graph}");
    assert!(
        graph.ends_with("crop=2*trunc(iw*0.2500/2):2*trunc(ih*0.2500/2):iw*0.5000:ih*0.5000[outv]"),
        "{graph}"
    );
    // Capped at the delivery frame: a zoom invents no pixels.
    let args = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, 1920, Some(r), &out).unwrap();
    let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
    assert!(graph.contains("s=1920x1080"), "{graph}");
    // Without a region the graph is what it always was.
    let plain = build_still_args(&timeline, &assets, &ExportOptions::default(), 1.0, 320, None, &out).unwrap();
    let graph = plain[plain.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
    assert!(graph.contains("s=320x180") && graph.ends_with("null[outv]"), "{graph}");
}

#[test]
fn contact_sheet_times_match_the_sheet() {
    let (_, times) = build_contact_sheet_args("/x.mp4", 10.0, 20.0, 2, 2, 160, 3, None);
    assert_eq!(times, contact_sheet_times(10.0, 20.0, 2, 2));
}

#[test]
fn contact_sheet_respects_a_subrange() {
    let (args, times) = build_contact_sheet_args("/x.mp4", 10.0, 20.0, 2, 2, 160, 3, None);
    let joined = args.join(" ");
    assert_eq!(times.len(), 4);
    assert!((times[0] - 10.0).abs() < 1e-9); // window starts at `start`
    assert!((times[3] - 17.5).abs() < 1e-9); // step = 10 / 4 = 2.5
    assert!(joined.contains("-ss 10.000"));
    assert!(joined.contains("-t 10.000"));
    assert!(joined.contains("tile=2x2"));
}

#[test]
fn timeline_frame_composites_the_active_clip() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![asset.clone()];
    // Source 5..15 at timeline 0; at t=2 the mapped source time is 7.
    let timeline = single(vec![make_clip(asset.id, 5.0, 15.0, 0.0)]);
    let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 2.0, 640, 4).unwrap();
    let joined = args.join(" ");
    assert_eq!(joined.matches("-i /x.mp4").count(), 1);
    assert!(joined.contains("-ss 7.000000 -i /x.mp4"), "{joined}");
    // 16:9 export shape capped to max_width 640 -> 640x360.
    assert!(joined.contains("color=c=black:s=640x360"));
    assert!(joined.contains("[0:v]trim=end_frame=1"));
    assert!(joined.contains("scale=640:360:force_original_aspect_ratio=decrease"));
    // The composite overlays onto a pad, then a trailing `null` names [outv].
    assert!(joined.contains("overlay=(W-w)/2:(H-h)/2[ov0]"));
    assert!(joined.contains("null[outv]"));
    assert!(joined.contains("-vcodec mjpeg"));
}

#[test]
fn timeline_frame_renders_black_on_a_gap() {
    let asset = test_asset(vec![video_stream(1280, 720, 30.0)]);
    let assets = vec![asset.clone()];
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]); // covers 0..5
    let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 8.0, 640, 4).unwrap();
    let joined = args.join(" ");
    // Nothing visible at t=8 -> no inputs, bare black canvas renamed to [outv].
    assert!(!joined.contains("-i "));
    assert!(joined.contains("color=c=black:s=640x360:d=0.1[base]"));
    assert!(joined.contains("[base]null[outv]"));
}

#[test]
fn timeline_frame_layers_tracks_with_the_last_on_top() {
    let base = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let pip = test_asset(vec![video_stream(1920, 1080, 30.0)]);
    let assets = vec![base.clone(), pip.clone()];
    let mut top = make_clip(pip.id, 0.0, 10.0, 0.0);
    top.transform.scale = 0.5;
    top.transform.pos_x = 0.25;
    let timeline = timeline_of(vec![
        video_track(vec![make_clip(base.id, 0.0, 10.0, 0.0)]),
        video_track(vec![top]),
    ]);
    let args = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 1.0, 960, 4).unwrap();
    let joined = args.join(" ");
    // Both clips visible at t=1 -> two inputs; the V2 picture-in-picture is the
    // second input, scaled down, offset, and overlaid last onto [outv].
    assert_eq!(joined.matches("-i /x.mp4").count(), 2);
    assert!(joined.contains("scale=iw*0.5:ih*0.5"));
    assert!(joined.contains("[base][v0]overlay=(W-w)/2:(H-h)/2[ov0]"));
    assert!(joined.contains("[v1]overlay=x=(W-w)/2+(0.25)*W:y=(H-h)/2+(0)*H[ov1]"));
    assert!(joined.contains("[ov1]null[outv]"));
}

/// The FFmpeg still and the GPU's `RenderPlan` are built from one active-clip
/// list, but the argv is still the thing that decides what FFmpeg decodes —
/// so pin that the two describe the same frame: same inputs in the same
/// order at the same source times, same canvas size.
#[test]
fn the_render_plan_and_the_still_args_agree_on_layers_timing_and_canvas() {
    use crate::render_plan::RenderPlan;
    let mk = |path: &str| {
        let mut a = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        a.path = path.into();
        a
    };
    let (a, b, c) = (mk("/m/a.mp4"), mk("/m/b.mp4"), mk("/m/c.mp4"));
    let mut fast = make_clip(a.id, 10.0, 30.0, 0.0);
    fast.speed = 2.0;
    let mut reversed = make_clip(b.id, 0.0, 20.0, 1.0);
    reversed.speed = -1.0;
    let mut late = make_clip(c.id, 0.0, 5.0, 40.0);
    late.transform.scale = 0.5;
    let timeline = timeline_of(vec![
        video_track(vec![fast, late]),
        video_track(vec![reversed]),
        // A muted track appears in neither.
        Track {
            muted: true,
            ..video_track(vec![make_clip(c.id, 0.0, 9.0, 0.0)])
        },
    ]);
    let assets = vec![a, b, c];
    let opts = ExportOptions::default();
    for (t, max_width) in [(2.5, 640), (0.5, 640), (6.0, 1920), (45.0, 320)] {
        let args = build_timeline_frame_args(&timeline, &assets, &opts, t, max_width, 4).unwrap();
        let plan = RenderPlan::at(
            &timeline,
            &assets,
            &opts,
            t,
            crate::render_plan::CompositeColorPolicy::FixedBt601,
        )
        .unwrap();

        // `-ss <t> -i <path>` pairs, in input order.
        let mut from_args = Vec::new();
        let mut ss: Option<String> = None;
        for w in args.windows(2) {
            match w[0].as_str() {
                "-ss" => ss = Some(w[1].clone()),
                "-i" => from_args.push((w[1].clone(), ss.take().unwrap_or_default())),
                _ => {}
            }
        }
        let from_plan: Vec<(String, String)> = plan
            .layers
            .iter()
            .map(|l| (l.path.clone(), format!("{:.6}", l.source_time)))
            .collect();
        assert_eq!(from_args, from_plan, "t={t}");

        let (w, h) = plan.size(max_width);
        let canvas = format!("color=c=black:s={w}x{h}:d=0.1[base]");
        assert!(args.iter().any(|a| a.starts_with(&canvas)), "t={t}: {canvas} not in {args:?}");
    }
}

#[test]
fn build_export_args_single_video_clip() {
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/media/clip.mp4".into(),
        name: "clip.mp4".into(),
        duration: 10.0,
        streams: vec![video_stream(1280, 720, 25.0), audio_stream(44_100, 2)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let assets = vec![asset];
    let opts = ExportOptions::default();

    let args = build_export_args(&timeline, &assets, "/out/result.mp4", &opts).unwrap();

    assert_eq!(args[0], "-y");
    assert!(
        args.contains(&"-nostats".to_string()),
        "progress stats suppressed so stderr stays bounded"
    );
    let i_pos = args.iter().position(|a| a == "-i").expect("an input flag");
    assert_eq!(args[i_pos + 1], "/media/clip.mp4");
    assert!(args.contains(&"-filter_complex".to_string()));
    let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
    let filter = &args[fc_pos + 1];
    assert!(filter.contains("trim=start=0:end=10"));
    assert!(filter.contains("overlay=eof_action=pass"));
    assert!(filter.contains("[outv]"));
    assert!(filter.contains("[outa]"));
    assert!(args.contains(&"-map".to_string()));
    assert!(args.contains(&"[outv]".to_string()));
    assert!(args.contains(&"[outa]".to_string()));
    assert_eq!(args.last().unwrap(), "/out/result.mp4");
    // Default opts: no explicit codec or crf flags.
    assert!(!args.contains(&"-c:v".to_string()));
    assert!(!args.contains(&"-c:a".to_string()));
    assert!(!args.contains(&"-crf".to_string()));
}

#[test]
fn build_export_args_two_clips_two_inputs() {
    let a1 = Asset {
        id: Uuid::new_v4(),
        path: "/media/a.mp4".into(),
        name: "a.mp4".into(),
        duration: 20.0,
        streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let a2 = Asset {
        id: Uuid::new_v4(),
        path: "/media/b.mp4".into(),
        name: "b.mp4".into(),
        duration: 10.0,
        streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let timeline = single(vec![make_clip(a1.id, 0.0, 20.0, 0.0), make_clip(a2.id, 0.0, 10.0, 20.0)]);
    let assets = vec![a1, a2];
    let opts = ExportOptions::default();

    let args = build_export_args(&timeline, &assets, "/out/out.mp4", &opts).unwrap();

    // Two -i flags for the two clips.
    let input_count = args.windows(2).filter(|w| w[0] == "-i").count();
    assert_eq!(input_count, 2);
    let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
    let filter = &args[fc_pos + 1];
    // One overlay per clip, both audio streams summed.
    assert_eq!(filter.matches("overlay=eof_action=pass").count(), 2);
    assert!(filter.contains("amix=inputs=2"));
    assert_eq!(args.last().unwrap(), "/out/out.mp4");
}

#[test]
fn build_export_args_video_only_has_no_audio_map() {
    let video_only = Asset {
        id: Uuid::new_v4(),
        path: "/media/vo.mp4".into(),
        name: "vo.mp4".into(),
        duration: 5.0,
        streams: vec![video_stream(1920, 1080, 30.0)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let timeline = single(vec![make_clip(video_only.id, 0.0, 5.0, 0.0)]);
    let assets = vec![video_only];
    let opts = ExportOptions::default();

    let args = build_export_args(&timeline, &assets, "/out/vo.mp4", &opts).unwrap();

    let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
    let filter = &args[fc_pos + 1];
    assert!(filter.contains("overlay=eof_action=pass"));
    assert!(!filter.contains("[0:a]"), "no real audio stream should be trimmed");
    assert!(!filter.contains("amix"), "nothing to mix with no audio");
    // A timeline with no audio yields a video map but no [outa] map.
    assert!(args.contains(&"[outv]".to_string()));
    assert!(!args.contains(&"[outa]".to_string()));
}

#[test]
fn build_export_args_with_codec_and_crf_options() {
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/media/clip.mp4".into(),
        name: "clip.mp4".into(),
        duration: 10.0,
        streams: vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let assets = vec![asset];
    let opts = ExportOptions {
        video_codec: Some("libx264".to_string()),
        audio_codec: Some("aac".to_string()),
        crf: Some(23),
        ..Default::default()
    };

    let args = build_export_args(&timeline, &assets, "/out/result.mp4", &opts).unwrap();

    let cv_pos = args.iter().position(|a| a == "-c:v").expect("-c:v must be present");
    assert_eq!(args[cv_pos + 1], "libx264");
    let ca_pos = args.iter().position(|a| a == "-c:a").expect("-c:a must be present");
    assert_eq!(args[ca_pos + 1], "aac");
    let crf_pos = args.iter().position(|a| a == "-crf").expect("-crf must be present");
    assert_eq!(args[crf_pos + 1], "23");
}

#[test]
fn build_export_args_resolution_override() {
    let asset = Asset {
        id: Uuid::new_v4(),
        path: "/media/4k.mp4".into(),
        name: "4k.mp4".into(),
        duration: 10.0,
        streams: vec![video_stream(3840, 2160, 60.0), audio_stream(48_000, 2)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    };
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let assets = vec![asset];
    let opts = ExportOptions {
        resolution: Some((1920, 1080)),
        fps: Some(30.0),
        ..Default::default()
    };

    let args = build_export_args(&timeline, &assets, "/out/downscaled.mp4", &opts).unwrap();

    let fc_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
    let filter = &args[fc_pos + 1];
    // Override forces 1920x1080 even though the source is 4K.
    assert!(filter.contains("scale=1920:1080"), "resolution override must apply");
    assert!(filter.contains("fps=30"), "fps override must apply");
}

#[test]
fn build_export_args_error_on_missing_asset() {
    let timeline = single(vec![make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0)]);
    let result = build_export_args(&timeline, &[], "/out/result.mp4", &ExportOptions::default());
    assert!(matches!(result, Err(Error::AssetNotFound(_))));
}

/// A raw Insta360 5.7K capture: one dual-fisheye video stream plus audio.
fn insv_asset(id: Uuid, duration: f64) -> Asset {
    let mut v = video_stream(5760, 2880, 30.0);
    v.codec = "hevc".into();
    v.projection = Some(Projection::DualFisheye);
    Asset {
        id,
        path: "/media/VID_20260801_120000_10_001.insv".into(),
        name: "VID_20260801_120000_10_001.insv".into(),
        duration,
        streams: vec![v, audio_stream(48_000, 2)],
        imported_at: Utc::now(),
        source_paths: Vec::new(),
        voiceover: None,
        music_score: None,
        sound_effect: None,
        tags: Vec::new(),
    }
}

/// A clip of `asset` that reframes to flat, optionally animated.
fn reframed_clip(asset: &Asset, keyframes: Vec<crate::model::ReframeKeyframe>) -> Clip {
    let mut clip = Clip::for_asset(asset, 0.0, 8.0, 2.0);
    let rf = clip.reframe.as_mut().expect("a 360 asset reframes by default");
    rf.pitch = -8.0;
    rf.keyframes = keyframes;
    clip
}

fn rkf(time: f64, yaw: f64) -> crate::model::ReframeKeyframe {
    crate::model::ReframeKeyframe {
        time,
        yaw,
        pitch: -8.0,
        roll: 0.0,
        fov: 100.0,
    }
}

fn fmt_1080p() -> ExportFormat {
    ExportFormat {
        width: 1920,
        height: 1080,
        fps: 30.0,
        sample_rate: 48_000,
        channels: 2,
        pix_fmt: "yuv420p".to_string(),
        scaler: None,
        fit: Fit::Contain,
    }
}

fn graph_of(timeline: &Timeline, assets: &[Asset]) -> String {
    build_filter_complex(
        timeline,
        assets,
        &fmt_1080p(),
        timeline.duration(),
        &ExportOptions::default(),
        true,
        true,
        &plan_inputs(timeline, assets, &transition_fx(timeline, assets)),
    )
    .filter
}

// ---- 360 / reframe -----------------------------------------------------

#[test]
fn reframe_chain_inserts_v360_after_setpts_before_the_fit() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![]);
    let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c3");

    let setpts = chain.find("setpts=").expect("setpts");
    let v360 = chain.find("v360@c3=").expect("v360");
    let fit = chain.find("scale=1920:1080:force_original_aspect_ratio").expect("fit");
    assert!(setpts < v360 && v360 < fit, "order: setpts < v360 < fit in {chain}");
    // Reprojected straight to the export frame, so the fit that follows is a
    // no-op and an 8K sphere never materializes at 8K.
    assert!(chain.contains("w=1920:h=1080"), "v360 renders at frame size: {chain}");
    assert!(chain.contains("input=dfisheye"), "dual-fisheye source: {chain}");
    assert!(chain.contains("output=flat"), "flat deliverable: {chain}");
    assert!(chain.contains("d_fov=100"), "aspect-correct fov knob: {chain}");
    // `:h_fov=`, not `h_fov=` — `ih_fov` (the input lens) contains it.
    assert!(!chain.contains(":h_fov="), "h_fov would stretch the picture: {chain}");
    assert!(chain.contains("ih_fov=190"), "lens fov for a fisheye source: {chain}");
}

#[test]
fn reframe_hoists_fps_above_v360_and_does_not_repeat_it() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![]);
    let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
    assert_eq!(chain.matches("fps=30").count(), 1, "exactly one fps: {chain}");
    assert!(
        chain.find("fps=30").unwrap() < chain.find("v360@c0=").unwrap(),
        "reproject at the output rate, not the source rate: {chain}"
    );
}

#[test]
fn reframe_crops_after_reprojection() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let mut clip = reframed_clip(&asset, vec![]);
    clip.transform.crop_left = 0.1;
    let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
    assert!(
        chain.find("v360@c0=").unwrap() < chain.find("crop=").unwrap(),
        "edge crops mean nothing on a raw fisheye frame: {chain}"
    );
}

#[test]
fn an_ordinary_clip_keeps_its_original_chain() {
    // The reframe branch reorders crop and fps; a non-360 clip must not move.
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let mut clip = make_clip(asset.id, 0.0, 8.0, 2.0);
    clip.transform.crop_left = 0.1;
    let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
    assert!(!chain.contains("v360"), "no reprojection: {chain}");
    assert!(
        chain.find("crop=").unwrap() < chain.find("setpts=").unwrap(),
        "crop stays ahead of setpts: {chain}"
    );
    assert!(
        chain.find("setsar=1").unwrap() < chain.find("fps=30").unwrap(),
        "fps stays after setsar: {chain}"
    );
}

#[test]
fn a_static_reframe_emits_no_sendcmd() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![]);
    let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
    assert!(
        !chain.contains("sendcmd"),
        "a held camera must not pay a LUT rebuild per frame: {chain}"
    );
    assert!(chain.contains("yaw=0"), "the pose is baked into the args: {chain}");
}

#[test]
fn an_animated_reframe_sends_commands_upstream_of_v360() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(4.0, 60.0)]);
    let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c3");

    let send = chain.find("sendcmd").expect("sendcmd");
    let v360 = chain.find("v360@c3=").expect("v360");
    assert!(send < v360, "a command must reach v360 with its own frame: {chain}");
    assert!(chain.contains("v360@c3 yaw"), "commands target this clip's instance");

    // Timestamps are on the timeline clock (the clip starts at 2.0) and lead
    // by half a frame so frame `i` cannot be served by frame `i-1`'s value.
    let first = chain.split("sendcmd=c='").nth(1).unwrap().split(' ').next().unwrap();
    let expected = 2.0 - 0.5 / 30.0;
    assert!(
        (first.parse::<f64>().unwrap() - expected).abs() < 1e-4,
        "first command at {first}, want {expected}"
    );
}

#[test]
fn reframe_commands_skip_channels_that_never_move() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(4.0, 60.0)]);
    let rf = clip.reframe.as_ref().unwrap();
    let cmds = reframe_commands(&clip, rf, "v360@c0", 30.0, clip.duration()).expect("commands");
    assert!(cmds.contains("yaw"), "yaw moves: {cmds}");
    for still in ["pitch", "roll", "d_fov"] {
        assert!(!cmds.contains(still), "{still} is static and must stay an arg: {cmds}");
    }
    // …and the static pitch is still applied, via the filter's own arguments.
    let chain = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
    assert!(chain.contains("pitch=-8"), "static pitch survives: {chain}");
}

#[test]
fn reframe_commands_wrap_yaw_into_range() {
    // A pan across the ±180 seam. `v360` *silently discards* an out-of-range
    // command — the frames render as if uncommanded — so every emitted value
    // has to land inside [-180, 180].
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![rkf(0.0, 170.0), rkf(4.0, -170.0)]);
    let rf = clip.reframe.as_ref().unwrap();
    let cmds = reframe_commands(&clip, rf, "v360@c0", 30.0, clip.duration()).expect("commands");

    let mut seen = 0;
    for c in cmds.split(';') {
        let v: f64 = c.rsplit(' ').next().unwrap().parse().unwrap();
        assert!((-180.0..=180.0).contains(&v), "{v} is out of v360's range: {c}");
        seen += 1;
    }
    assert!(seen > 1, "the pan should emit more than one command");
    // Shortest arc: 170 -> -170 travels 20° forward through 180, never back
    // through 0. Halfway is therefore 180/-180, not 0.
    let mid = rf.sample(2.0).yaw;
    assert!(mid.abs() > 179.0, "midpoint {mid} should be at the seam, not near 0");
}

#[test]
fn reframe_commands_collapse_a_held_camera() {
    // Keyframes that pin the same pose twice: nothing moves after the first
    // sample, so the tolerance gate should leave a single command at most.
    let asset = insv_asset(Uuid::new_v4(), 60.0);
    let mut clip = reframed_clip(&asset, vec![rkf(0.0, 30.0), rkf(50.0, 30.0)]);
    clip.source_out = 50.0;
    let rf = clip.reframe.as_ref().unwrap();
    let cmds = reframe_commands(&clip, rf, "v360@c0", 30.0, clip.duration());
    assert!(cmds.is_none(), "a motionless camera needs no commands at all: {cmds:?}");
}

#[test]
fn export_format_ignores_a_reframed_clips_source_size() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![]);
    let tl = single(vec![clip]);
    let fmt = export_format(&tl, std::slice::from_ref(&asset), &ExportOptions::default());
    assert_eq!(
        (fmt.width, fmt.height),
        (1920, 1080),
        "a 16:9 reframe must not inherit the sphere's 5760x2880"
    );
    assert_eq!(fmt.fps, 30.0, "frame rate still comes from the source");

    // An explicit override still wins.
    let opts = ExportOptions {
        resolution: Some((3840, 2160)),
        ..Default::default()
    };
    assert_eq!(export_format(&tl, &[asset], &opts).width, 3840);
}

#[test]
fn the_still_path_samples_the_reframe_statically() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(4.0, 60.0)]);
    let tl = single(vec![clip]);
    // t = 4.0 is 2.0s into a clip starting at 2.0, i.e. halfway through the pan.
    let args = build_timeline_frame_args(&tl, &[asset], &ExportOptions::default(), 4.0, 960, 4).expect("args");
    let graph = args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1].clone();
    assert!(!graph.contains("sendcmd"), "a still has no clock to command against: {graph}");
    assert!(graph.contains("v360=input=dfisheye"), "unnamed instance: {graph}");
    assert!(graph.contains("yaw=30"), "the pose is sampled to a constant: {graph}");
    assert!(graph.contains("w=960"), "reproject at the preview size: {graph}");
}

#[test]
fn slice_resamples_reframe_keyframes() {
    let asset = insv_asset(Uuid::new_v4(), 30.0);
    // Clip spans timeline 2..10, panning 0 -> 60 over its 8 seconds.
    let clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(8.0, 60.0)]);
    let pose_at_cut = clip.reframe_at(2.0).unwrap();
    let sliced = single(vec![clip]).slice(4.0, 10.0);

    let c = &sliced.tracks[0].clips[0];
    assert_eq!(c.timeline_start, 0.0);
    let kfs = &c.reframe.as_ref().unwrap().keyframes;
    assert_eq!(kfs[0].time, 0.0, "a pinned keyframe opens the sliced clip");
    assert!(
        (kfs[0].yaw - pose_at_cut.yaw).abs() < 1e-9,
        "the pin carries the pose the cut landed on: {} vs {}",
        kfs[0].yaw,
        pose_at_cut.yaw
    );
}

#[test]
fn an_ordinary_graph_stays_in_argv_but_a_long_pan_spills_to_a_script() {
    let asset = insv_asset(Uuid::new_v4(), 300.0);

    // A static reframe: no commands, so the graph stays small.
    let still = single(vec![reframed_clip(&asset, vec![])]);
    let args = build_export_args(&still, std::slice::from_ref(&asset), "/out.mp4", &ExportOptions::default()).unwrap();
    assert_eq!(oversized_graph_index(&args), None, "a normal graph rides in argv");

    // A four-minute pan: the sendcmd list alone outgrows what a single argv
    // string may hold on Windows, and eventually on Linux too.
    let mut clip = reframed_clip(&asset, vec![rkf(0.0, 0.0), rkf(240.0, 170.0)]);
    clip.source_out = 240.0;
    let long = single(vec![clip]);
    let args = build_export_args(&long, &[asset], "/out.mp4", &ExportOptions::default()).unwrap();
    let i = oversized_graph_index(&args).expect("a long pan must spill out of argv");
    assert!(args[i].len() > GRAPH_ARG_MAX);

    // spill_graph rather than externalize_filter_complex: the wrapper
    // probes the real binary for which flag spelling it takes, and the
    // default test run must stay binary-free.
    let mut spilled = args.clone();
    let guard = spill_graph(&mut spilled, "test", "-/filter_complex").unwrap();
    assert_eq!(spilled[i - 1], "-/filter_complex");
    let path = std::path::PathBuf::from(&spilled[i]);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        args[i],
        "the graph is written verbatim"
    );
    drop(guard);
    assert!(!path.exists(), "the script is cleaned up after the render");
}

#[test]
fn probe_reads_a_spherical_mapping() {
    let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":1920,"r_frame_rate":"30/1","side_data_list":[{"side_data_type":"Spherical Mapping","projection":"equirectangular"}]}],"format":{"duration":"20.0"}}"#;
    let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
    assert_eq!(r.streams[0].projection, Some(Projection::Equirect));
}

/// The tick of a video stream, as the probe records it (`StreamInfo::time_base`): what `setpts` truncates a clip's start to.
#[test]
fn probe_records_the_tick_of_a_video_stream() {
    let probe = |codec: &str, tick: &str| {
        let json = format!(
            r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"{codec}","width":64,"height":36,"r_frame_rate":"25/1"{tick}}},{{"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"time_base":"1/48000"}}],"format":{{"duration":"20.0"}}}}"#
        );
        probe_from_json(serde_json::from_str(&json).unwrap(), None)
    };
    let tick = |r: &ProbeResult| r.streams[0].time_base;
    assert_eq!(tick(&probe("h264", r#","time_base":"1/1000""#)), Some((1, 1000)));
    assert_eq!(tick(&probe("h264", r#","time_base":"1/15360""#)), Some((1, 15360)));
    assert_eq!(tick(&probe("h264", r#","time_base":"1001/30000""#)), Some((1001, 30000)));
    // Not a tick: a term of zero, a negative or fractional or overlarge one, text, or none printed.
    for bad in [
        r#","time_base":"0/1""#,
        r#","time_base":"1/0""#,
        r#","time_base":"-1/1000""#,
        r#","time_base":"1/1000.5""#,
        r#","time_base":"1/99999999999""#,
        r#","time_base":"garbage""#,
        r#","time_base":"""#,
        "",
    ] {
        assert_eq!(tick(&probe("h264", bad)), None, "{bad}");
    }
    // A still is looped from one picture: it has no tick to truncate to. Nor does the sound.
    let still = probe_from_json(
            serde_json::from_str(
                r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"png","width":64,"height":64,"r_frame_rate":"25/1","time_base":"1/25"}],"format":{}}"#,
            )
            .unwrap(),
            None,
        );
    assert!(still.streams[0].image);
    assert_eq!(still.streams[0].time_base, None);
    let both = probe("h264", r#","time_base":"1/1000""#);
    assert_eq!(both.streams[1].time_base, None, "the sound's tick is not recorded");
    // The libav probe reads the same two terms through the same rule.
    assert_eq!(time_base_of(1, 600), Some((1, 600)));
    assert_eq!(time_base_of(0, 600), None);
    assert_eq!(time_base_of(1, 0), None);
    assert_eq!(time_base_of(-1, 600), None);
    assert_eq!(time_base_of(1, i64::from(u32::MAX) + 1), None);
}

#[test]
fn probe_reads_insta360_dual_fisheye_geometry() {
    let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":5760,"height":2880,"r_frame_rate":"30/1"}],"format":{"duration":"20.0"}}"#;
    let r = probe_from_json(
        serde_json::from_str(json).unwrap(),
        Some(Path::new("/media/VID_20260801_120000_10_001.insv")),
    );
    assert_eq!(r.streams[0].projection, Some(Projection::DualFisheye));
}

#[test]
fn probe_does_not_guess_360_from_aspect_alone() {
    // 2:1 at 4K is an ordinary shape (anamorphic, ultrawide, panoramas). A
    // false positive would silently reproject real footage, so only an
    // Insta360 extension unlocks the geometry signal.
    let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":5760,"height":2880,"r_frame_rate":"30/1"}],"format":{"duration":"20.0"}}"#;
    let r = probe_from_json(serde_json::from_str(json).unwrap(), Some(Path::new("/media/ultrawide.mp4")));
    assert_eq!(r.streams[0].projection, None);
}

#[test]
fn probe_records_how_late_the_video_starts() {
    let probe = |video: &str, audio: &str| {
        let json = format!(
            r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"h264","width":64,"height":36,"r_frame_rate":"25/1"{video}}},
                    {{"index":1,"codec_type":"audio","codec_name":"aac"{audio}}}],"format":{{"duration":"5.0","start_time":"0.000000"}}}}"#
        );
        let r = probe_from_json(serde_json::from_str(&json).unwrap(), None);
        (r.streams[0].lead, r.streams[1].lead)
    };
    let at = |t: &str| format!(r#","start_time":"{t}""#);
    assert_eq!(probe(&at("0.500000"), &at("0.000000")), (0.5, 0.0));
    assert_eq!(probe(&at("1.521333"), &at("1.400000")).0, 1.521333 - 1.4);
    // A millisecond or less, an early video and a missing start are an ordinary file.
    assert_eq!(probe(&at("0.000500"), &at("0.000000")).0, 0.0);
    assert_eq!(probe(&at("0.000000"), &at("0.200000")).0, 0.0);
    assert_eq!(probe("", &at("0.000000")).0, 0.0);
    assert_eq!(probe(&at("0.5"), "").0, 0.0);
    // A start at or past the end of the file is a broken one, not a lead.
    assert_eq!(probe(&at("5.000000"), &at("0.000000")).0, 0.0);
    assert_eq!(probe(&at("9.000000"), &at("0.000000")).0, 0.0);
}

#[test]
fn a_text_track_that_starts_the_container_earlier_is_no_lead() {
    // Picture and sound both at 40 ms, a subtitle and a data track at 0: the export rebases
    // the picture and the sound to the clip's start alike, so there is nothing to delay.
    let json = |video: &str| {
        format!(
            r#"{{"streams":[
                    {{"index":0,"codec_type":"video","codec_name":"h264","width":64,"height":36,"r_frame_rate":"25/1","start_time":"{video}"}},
                    {{"index":1,"codec_type":"audio","codec_name":"aac","start_time":"0.040000"}},
                    {{"index":2,"codec_type":"subtitle","codec_name":"subrip","start_time":"0.000000"}},
                    {{"index":3,"codec_type":"data","codec_name":"bin_data","start_time":"0.000000"}}],
                    "format":{{"duration":"5.0","start_time":"0.000000"}}}}"#
        )
    };
    let leads = |video: &str| -> Vec<f64> {
        probe_from_json(serde_json::from_str(&json(video)).unwrap(), None)
            .streams
            .iter()
            .map(|s| s.lead)
            .collect()
    };
    assert_eq!(leads("0.040000"), vec![0.0; 4]);
    // A video 80 ms behind its sound, with the same text track: 80 ms.
    let late = leads("0.120000");
    assert!((late[0] - 0.08).abs() < 1e-12, "{late:?}");
    assert_eq!(late[1..], [0.0; 3]);
}

#[test]
fn a_window_inside_a_late_starting_sources_lead_is_unseeked_and_its_picture_placed_late() {
    let mut asset = av_asset(Uuid::new_v4(), 20.0);
    asset.streams[0].lead = 0.5;
    let assets = vec![asset.clone()];
    let graph = |clip: Clip| {
        let args = build_export_args(&single(vec![clip]), &assets, "out.mp4", &ExportOptions::default()).unwrap();
        let at = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
        (args.iter().any(|a| a == "-ss"), args[at].clone())
    };
    // Inside the lead: no `-ss` for the picture or the sound, and the picture placed
    // `(lead - source_in) / speed` after the clip's start, spelled beside it.
    let (seeked, g) = graph(make_clip(asset.id, 0.2, 5.0, 3.0));
    assert!(
        !seeked && g.contains("trim=start=0.2:end=5.066666666666666,setpts=PTS-STARTPTS+(3+0.3)/TB"),
        "{g}"
    );
    assert!(g.contains("atrim=start=0.2:end=5,asetpts=PTS-STARTPTS"), "{g}");
    let mut fast = make_clip(asset.id, 0.0, 10.0, 0.0);
    fast.speed = 2.0;
    let (_, g) = graph(fast);
    assert!(g.contains("setpts=(PTS-STARTPTS)/2+(0+0.25)/TB"), "{g}");
    // Past the lead, and played backwards: the argv an ordinary file gets.
    let (seeked, g) = graph(make_clip(asset.id, 0.5, 5.0, 3.0));
    assert!(seeked && g.contains("setpts=PTS-STARTPTS+3/TB"), "{g}");
    let mut back = make_clip(asset.id, 0.0, 5.0, 3.0);
    back.speed = -1.0;
    let (_, g) = graph(back);
    assert!(g.contains("reverse,setpts=PTS-STARTPTS+3/TB"), "{g}");
    // A fade-in starts on the first picture.
    let mut faded = make_clip(asset.id, 0.0, 5.0, 3.0);
    faded.fade_in = 0.4;
    let (_, g) = graph(faded);
    assert!(g.contains("fade=t=in:st=3.5:d=0.4"), "{g}");
}

#[test]
fn probe_leaves_ordinary_video_flat() {
    let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"r_frame_rate":"30/1"}],"format":{"duration":"12.0"}}"#;
    let r = probe_from_json(serde_json::from_str(json).unwrap(), Some(Path::new("/media/a.mp4")));
    assert_eq!(r.streams[0].projection, None);
}

#[test]
fn speed_retimes_picture_and_sound() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.speed = 2.0;
    // Source span 10s at 2x => 5s on the timeline.
    assert!((clip.duration() - 5.0).abs() < 1e-9);
    let g = graph_of(&single(vec![clip]), &[asset]);
    assert!(g.contains("setpts=(PTS-STARTPTS)/2+0/TB"), "{g}");
    assert!(g.contains("atempo=2"), "{g}");
}

#[test]
fn negative_speed_reverses() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.speed = -1.0;
    let g = graph_of(&single(vec![clip]), &[asset]);
    assert!(g.contains(",reverse,"), "{g}");
    assert!(g.contains("areverse"), "{g}");
    // |speed| == 1, so the picture is not retimed.
    assert!(g.contains("setpts=PTS-STARTPTS+0/TB"), "{g}");
}

#[test]
fn transform_pip_positions_a_scaled_overlay() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.transform = crate::model::Transform {
        scale: 0.5,
        pos_x: 0.25,
        ..Default::default()
    };
    let g = graph_of(&single(vec![clip]), &[asset]);
    assert!(g.contains("scale=iw*0.5:ih*0.5"), "{g}");
    assert!(g.contains("overlay=x=(W-w)/2+(0.25)*W:y=(H-h)/2+(0)*H"), "{g}");
    // A transformed clip is positioned by overlay, not letterbox-padded.
    assert!(!g.contains("pad=1920:1080"), "{g}");
}

#[test]
fn a_keyed_mask_reads_its_numbers_per_frame_and_a_static_one_is_as_it_was() {
    use crate::model::{Property, PropertyKey};
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 2.0);
    clip.mask = Some(Mask::default());
    let still_graph = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&asset));
    assert!(still_graph.contains(&mask_filter(&Mask::default())), "{still_graph}");
    clip.set_property_keys(Property::MaskX, vec![PropertyKey::new(0.0, 0.2), PropertyKey::new(4.0, 0.8)]);
    clip.set_property_keys(Property::MaskFeather, vec![PropertyKey::new(1.0, 0.0)]);
    let g = graph_of(&single(vec![clip.clone()]), &[asset]);
    // The curves once a row, at its first pixel, over the frame's own time (from the clip's
    // start): the centre in slot 0, the feather (keyed to a hard edge here) in slot 4, read
    // back by every pixel; the width still static; the feather behind a guard against
    // dividing by nothing.
    assert!(g.contains("a='if(eq(X,0),st(0,if(lt((T-2),0),0.2,"), "{g}");
    assert!(g.contains("+st(4,0),0);"), "{g}");
    assert!(g.contains("(X-ld(0)*W)/(0.25*W)"), "{g}");
    assert!(g.contains("if(lte(ld(4)\\,0.000001)\\,lte("), "{g}");
    assert!(!g.contains(&mask_filter(&Mask::default())), "{g}");
    // Beside a keyed opacity, the opacity is in slot 5.
    clip.set_property_keys(
        Property::Opacity,
        vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(2.0, 0.5)],
    );
    let g = graph_of(&single(vec![clip]), &[av_asset(Uuid::new_v4(), 20.0)]);
    assert!(
        g.contains("+st(5,if(lt((T-2),0),1,") && g.contains("*ld(5)*alpha(X,Y)'"),
        "{g}"
    );
}

#[test]
fn a_keyed_crop_reads_the_picture_through_its_window_after_fps_and_the_static_crop() {
    use crate::model::{Property, PropertyKey};
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 1.0);
    clip.transform.crop_left = 0.1;
    clip.set_property_keys(
        Property::CropZoom,
        vec![PropertyKey::new(0.0, 1.0), PropertyKey::new(2.0, 2.0)],
    );
    let g = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&asset));
    let crop = g.find("crop=w=iw*0.9").expect("the static crop");
    let setpts = g.find("setpts=").unwrap();
    // The first frame's slot pinned at the clip's start: frame 30 of 30 fps.
    let fps = g.find("fps=fps=30:start_time=1,").expect("the pinned fps");
    // `perspective` counts its frames from 1: frame `in` is output frame `29 + in`, at
    // `(in+29)/30` s, from the clip's start; a pan unkeyed is centred.
    let window = g
        .find("perspective=x0='((1+0)*W/2-0.5)*(1-(1/(if(lt((((in+29)*1/30)-1),0),1,")
        .expect("the window");
    assert!(crop < setpts && setpts < fps && fps < window, "{g}");
    assert_eq!(g.matches(",fps=").count(), 1, "one fps: {g}");
    assert!(g.contains(":interpolation=linear:eval=frame"), "{g}");
    assert!(!g.contains("geq="), "{g}");
    assert!(window < g.find("scale=").unwrap(), "the window is cut before the fit: {g}");
    // A clip off the frame grid: `start_time` (1.02 s, slot 30.6) rounded to slot 31.
    let mut late = clip.clone();
    late.timeline_start = 1.02;
    let g = graph_of(&single(vec![late]), std::slice::from_ref(&asset));
    assert!(g.contains("((in+30)*1/30)"), "{g}");
    // A zoom held at 1 is no crop: the chain is the static one.
    let mut held = clip.clone();
    held.set_property_keys(Property::CropZoom, vec![PropertyKey::new(0.0, 1.0)]);
    held.set_property_keys(Property::CropX, vec![PropertyKey::new(0.0, 0.5)]);
    let g = graph_of(&single(vec![held]), &[asset]);
    assert!(!g.contains("perspective") && !g.contains("start_time"), "{g}");
}

#[test]
fn opacity_uses_an_alpha_channel() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.transform = crate::model::Transform {
        opacity: 0.5,
        ..Default::default()
    };
    let g = graph_of(&single(vec![clip]), &[asset]);
    assert!(g.contains("format=yuva420p"), "{g}");
    assert!(g.contains("colorchannelmixer=aa=0.5"), "{g}");
}

#[test]
fn color_correction_adds_an_eq_filter() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.color = crate::model::Color {
        brightness: 0.1,
        contrast: 1.2,
        ..Default::default()
    };
    let g = graph_of(&single(vec![clip]), &[asset]);
    assert!(g.contains("eq=brightness=0.1:contrast=1.2:saturation=1:gamma=1"), "{g}");
    // No temperature → no channel gammas, so pre-temperature graphs are
    // reproduced byte-for-byte.
    assert!(!g.contains("gamma_r"), "{g}");
}

#[test]
fn color_temperature_warms_via_opposing_channel_gammas() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.color = crate::model::Color {
        temperature: 0.5,
        ..Default::default()
    };
    let g = graph_of(&single(vec![clip]), &[asset]);
    assert!(g.contains("gamma_r=1.15"), "{g}");
    assert!(g.contains("gamma_b=0.85"), "{g}");
}

/// The video chain of the first clip of `tl`.
fn first_video_chain(tl: &Timeline, assets: &[Asset]) -> String {
    let g = graph_of(tl, assets);
    g.split(';')
        .find(|c| c.starts_with("[0:v]"))
        .expect("a video chain")
        .to_string()
}

fn pkey(time: f64, value: f64) -> crate::model::PropertyKey {
    crate::model::PropertyKey::new(time, value)
}

#[test]
fn a_keyed_colour_writes_the_eq_per_frame_and_leaves_the_rest_of_the_chain_as_it_was() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut still_graded = make_clip(asset.id, 2.0, 8.0, 1.5);
    still_graded.color.contrast = 1.2;
    let mut keyed = still_graded.clone();
    keyed.set_property_keys(Property::Brightness, vec![pkey(0.0, -0.3), pkey(2.0, 0.3)]);
    keyed.set_property_keys(Property::Temperature, vec![pkey(0.0, -1.0), pkey(2.0, 0.5)]);
    let chain = first_video_chain(&single(vec![keyed.clone()]), std::slice::from_ref(&asset));
    // The keyed numbers are quoted expressions of `t` minus the clip's start; the others stay
    // the numbers they were; the temperature is the gammas of its curve; one `eval=frame`.
    assert!(
            chain.contains("eq=brightness='if(lt((t-1.5),0),-0.3,if(lt((t-1.5),2),(-0.3+(0.6)*((t-1.5)-0)/(2)),0.3))':contrast=1.2:saturation=1:gamma=1:gamma_r='1+0.3*(if(lt((t-1.5),0),-1,"),
            "{chain}"
        );
    assert!(
        chain.contains(":gamma_b='1-0.3*(") && chain.ends_with(":eval=frame,format=yuv420p[v0]"),
        "{chain}"
    );
    // Nothing but the `eq` differs from the clip with the same grade held still.
    let still = first_video_chain(&single(vec![still_graded]), std::slice::from_ref(&asset));
    let strip = |c: &str| c.split(",eq=").next().unwrap().to_string();
    assert_eq!(strip(&chain), strip(&still), "the picture is placed as a static clip's is");
    let after = |c: &str| c.rsplit_once(",format=").unwrap().1.to_string();
    assert_eq!(after(&chain), after(&still));
    // A keyed clip is graded even when its static grade is the identity.
    keyed.color = crate::model::Color::default();
    let chain = first_video_chain(&single(vec![keyed]), std::slice::from_ref(&asset));
    assert!(chain.contains("contrast=1:saturation=1:gamma=1:gamma_r="), "{chain}");
}

#[test]
fn a_transform_with_some_numbers_keyed_builds_the_rest_from_the_static_transform() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let assets = std::slice::from_ref(&asset);
    let mut fading = make_clip(asset.id, 0.0, 10.0, 0.0);
    fading.transform.scale = 1.5;
    fading.transform.rotation = 20.0;
    fading.set_property_keys(Property::Opacity, vec![pkey(0.0, 0.2), pkey(2.0, 1.0)]);
    let chain = first_video_chain(&single(vec![fading.clone()]), assets);
    // The opacity is a `geq` alpha; the zoom and the turn are the constant ones.
    assert!(chain.contains("geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='("), "{chain}");
    assert!(
        chain.contains("scale=1920*1.5:1080*1.5:force_original_aspect_ratio=decrease,setsar=1"),
        "{chain}"
    );
    assert!(!chain.contains("eval=frame"), "no zoom is keyed: {chain}");
    assert!(chain.contains("rotate=0.3490658503988659:fillcolor=none"), "{chain}");
    assert!(!chain.contains("colorchannelmixer"), "{chain}");
    // A moving position beside a static opacity: that opacity is the constant mix.
    let mut moving = make_clip(asset.id, 0.0, 10.0, 0.0);
    moving.transform.opacity = 0.6;
    moving.set_property_keys(Property::PosX, vec![pkey(0.0, -0.2), pkey(3.0, 0.2)]);
    let g = graph_of(&single(vec![moving.clone()]), assets);
    assert!(g.contains("colorchannelmixer=aa=0.6"), "{g}");
    assert!(
        g.contains("perspective=x0='0+0.5-2*floor((W-1920)/4)-(960+(if(")
            && g.contains(":y0='0+0.5-2*floor((H-1080)/4)-(540+(0)*1080)+H/2-0.5'"),
        "{g}"
    );
    // Taking a number off the bundle: the bundle's other numbers still drive the rest.
    let mut bundle = make_clip(asset.id, 0.0, 10.0, 0.0);
    bundle.keyframes = vec![
        crate::model::Keyframe {
            time: 0.0,
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        },
        crate::model::Keyframe {
            time: 2.0,
            scale: 2.0,
            pos_x: 0.1,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 0.5,
            easing: Default::default(),
        },
    ];
    let keyed_chain = first_video_chain(&single(vec![bundle.clone()]), assets);
    assert!(
        keyed_chain.contains("geq=") && keyed_chain.contains("perspective="),
        "{keyed_chain}"
    );
    bundle.set_property_keys(Property::Opacity, vec![]);
    bundle.transform.opacity = 0.7;
    let off = first_video_chain(&single(vec![bundle]), assets);
    assert!(
        !off.contains("geq=") && off.contains("colorchannelmixer=aa=0.7") && off.contains("perspective="),
        "{off}"
    );
}

#[test]
fn a_keyed_volume_is_a_per_frame_gain_in_small_frames_and_an_unkeyed_one_is_not() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 3.0);
    clip.volume = 0.5;
    let plain = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&asset));
    assert!(plain.contains("volume=0.5,") && !plain.contains("asetnsamples"), "{plain}");
    clip.set_property_keys(Property::Volume, vec![pkey(0.0, 0.0), pkey(2.0, 1.0)]);
    let keyed = graph_of(&single(vec![clip]), std::slice::from_ref(&asset));
    assert!(
        keyed.contains(",asetnsamples=n=128:p=0,volume='if(lt((t-0),0),0,if(lt((t-0),2),(0+(1)*((t-0)-0)/(2)),1))':eval=frame,"),
        "{keyed}"
    );
    assert!(!keyed.contains("volume=0.5"), "the static gain is replaced: {keyed}");
}

#[test]
fn slice_cuts_clips_and_shifts_to_zero() {
    let asset_id = Uuid::new_v4();
    let a = make_clip(asset_id, 0.0, 10.0, 0.0);
    let b = make_clip(asset_id, 0.0, 10.0, 10.0);
    let s = single(vec![a, b]).slice(8.0, 12.0);
    let clips = &s.tracks[0].clips;
    assert_eq!(clips.len(), 2);
    // A keeps its last 2 source seconds, landing at t=0.
    assert!((clips[0].source_in - 8.0).abs() < 1e-9, "{}", clips[0].source_in);
    assert!((clips[0].source_out - 10.0).abs() < 1e-9);
    assert!(clips[0].timeline_start.abs() < 1e-9);
    // B keeps its first 2 source seconds, landing right after.
    assert!(clips[1].source_in.abs() < 1e-9);
    assert!((clips[1].source_out - 2.0).abs() < 1e-9);
    assert!((clips[1].timeline_start - 2.0).abs() < 1e-9);
}

#[test]
fn slice_drops_outside_clips_and_honors_speed() {
    let asset_id = Uuid::new_v4();
    let mut a = make_clip(asset_id, 0.0, 4.0, 0.0);
    a.speed = 2.0; // 2 timeline seconds
    let b = make_clip(asset_id, 0.0, 4.0, 6.0);
    let s = single(vec![a, b]).slice(1.0, 3.0);
    let clips = &s.tracks[0].clips;
    assert_eq!(clips.len(), 1);
    // One timeline second cut from the front = two source seconds at 2×.
    assert!((clips[0].source_in - 2.0).abs() < 1e-9, "{}", clips[0].source_in);
    assert!(clips[0].timeline_start.abs() < 1e-9);
}

#[test]
fn range_export_builds_the_graph_from_the_sliced_timeline() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    let tl = single(vec![clip]);
    let opts = ExportOptions {
        range: Some(crate::model::TimeRange { start: 2.0, end: 6.0 }),
        ..Default::default()
    };
    let args = build_export_args(&tl, &[asset], "out.mp4", &opts).unwrap();
    let joined = args.join(" ");
    // The kept span is source 2..6 fast-sought to 2, so the in-graph trim
    // is seek-relative 0..4 — the graph really was built from the slice.
    assert!(joined.contains("trim=start=0:end=4"), "{joined}");
}

#[test]
fn a_mask_cuts_the_clip_to_its_shape() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.mask = Some(crate::model::Mask {
        shape: crate::model::MaskShape::Ellipse,
        x: 0.25,
        y: 0.5,
        width: 0.4,
        height: 0.6,
        feather: 0.2,
        inverted: false,
    });
    let g = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&asset));
    // A mask needs an alpha plane, and it must be established before the geq.
    let alpha = g.find("format=yuva420p").expect("alpha");
    let geq = g.find("geq=lum=").expect("mask");
    assert!(alpha < geq, "alpha must precede the mask: {g}");
    // An ellipse combines the axes with hypot; the edge sits at distance 1.
    assert!(g.contains("hypot((X-0.25*W)/(0.2*W)"), "{g}");
    assert!(g.contains("clip((1-hypot"), "feathered edge: {g}");

    // A rectangle is the same expression with max instead of hypot…
    let mut rect = clip;
    rect.mask = Some(crate::model::Mask {
        shape: crate::model::MaskShape::Rect,
        feather: 0.0,
        ..Default::default()
    });
    let g = graph_of(&single(vec![rect.clone()]), std::slice::from_ref(&asset));
    assert!(g.contains("max(abs("), "{g}");
    // …and no feather is a hard test rather than a ramp.
    assert!(g.contains("lte(max(abs("), "{g}");
    assert!(!g.contains("clip((1-"), "{g}");

    // Inverted keeps what is outside the shape.
    let mut inv = rect;
    inv.mask = Some(crate::model::Mask {
        inverted: true,
        feather: 0.0,
        ..Default::default()
    });
    let g = graph_of(&single(vec![inv]), &[asset]);
    assert!(g.contains("a='((1-lte("), "{g}");
}

#[test]
fn a_mask_and_keyframed_opacity_share_one_geq_pass() {
    // Both rewrite only the alpha plane, and geq is the most expensive
    // filter in the chain — a clip with both must fold them into one pass.
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.mask = Some(crate::model::Mask::default());
    let key = |time: f64, opacity: f64| crate::model::Keyframe {
        easing: Default::default(),
        time,
        scale: 1.0,
        pos_x: 0.0,
        pos_y: 0.0,
        rotation: 0.0,
        opacity,
    };
    clip.keyframes = vec![key(0.0, 0.0), key(5.0, 1.0)];
    let g = graph_of(&single(vec![clip]), &[asset]);
    assert_eq!(g.matches("geq=").count(), 1, "one pass for both: {g}");
    // The mask's keep expression and the opacity ramp share the alpha term.
    assert!(g.contains("clip((1-max(abs("), "the mask survives: {g}");
    assert!(g.contains(")*(if(lt((T"), "the opacity ramp survives: {g}");
}

#[test]
fn an_unmasked_clip_writes_no_mask() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let g = graph_of(&single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]), &[asset]);
    assert!(!g.contains("geq="), "{g}");
    assert!(!g.contains("format=yuva420p"), "no mask, no alpha: {g}");
}

#[test]
fn a_blended_clip_fades_its_alpha_clears_its_bars_and_fills_its_corners() {
    let square = test_asset(vec![video_stream(1080, 1080, 25.0)]);
    let mut clip = make_clip(square.id, 0.0, 4.0, 0.0);
    clip.fade_in = 1.0;
    let plain = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&square));
    assert!(plain.contains("pad=1920:1080:(ow-iw)/2:(oh-ih)/2,") && plain.contains("fade=t=in:st=0:d=1,"));
    clip.blend_mode = crate::model::BlendMode::Multiply;
    let g = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&square));
    assert!(
        g.contains("format=yuva420p,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:color=black@0,"),
        "{g}"
    );
    assert!(g.contains("fade=t=in:st=0:d=1:alpha=1"), "{g}");
    assert!(
        g.contains("select='between(t,0,4)'"),
        "only the clip's window is blended: {g}"
    );
    // A vignette drops the alpha after the pad: the bars stay black.
    clip.effects = vec![VideoEffect::Vignette];
    let g = graph_of(&single(vec![clip.clone()]), std::slice::from_ref(&square));
    assert!(
        g.contains("pad=1920:1080:(ow-iw)/2:(oh-ih)/2,") && !g.contains("black@0"),
        "{g}"
    );
    clip.effects.clear();
    clip.transform.rotation = 10.0;
    let g = graph_of(&single(vec![clip]), std::slice::from_ref(&square));
    assert!(g.contains(":fillcolor=black@0:ow=rotw("), "{g}");
}

/// What the blend chains name for the composite is what the plan says they do (`PlanCanvas::blend_tag`), which the GPU draws the
/// blend in: the bottom layer's tag (the first picture of the lowest video track, wherever the cut is looked at from), BT.709 for
/// untagged HD beside a tagged input, nothing for an untagged one.
#[test]
fn the_matrix_a_blend_names_is_the_plans_blend_tag() {
    use crate::planner::{PlanRequest, Planner};
    use crate::render_plan::{CompositeColorPolicy, PlanMode};
    let tagged = |w: u32, h: u32, tag: Option<&str>| {
        let mut stream = video_stream(w, h, 25.0);
        stream.pix_fmt = Some("yuv420p".into());
        stream.color_space = tag.map(str::to_string);
        test_asset(vec![stream])
    };
    let blended = |asset: &Asset, start: f64| {
        let mut c = make_clip(asset.id, 0.0, 4.0, start);
        c.blend_mode = crate::model::BlendMode::Screen;
        c
    };
    let named = |graph: &str| -> Option<String> {
        graph
            .split("in_color_matrix=")
            .nth(1)
            .map(|rest| rest.split(':').next().unwrap_or_default().to_string())
    };
    let (untagged_sd, bt709, ntsc, untagged_hd) = (
        tagged(640, 480, None),
        tagged(1920, 1080, Some("bt709")),
        tagged(640, 480, Some("smpte170m")),
        tagged(1920, 1080, None),
    );
    for (name, bottom, top, tag, graph_names) in [
        ("untagged SD", &untagged_sd, &untagged_sd, None, None),
        ("bt709", &bt709, &bt709, Some("bt709"), Some("bt709")),
        ("smpte170m", &ntsc, &ntsc, Some("smpte170m"), Some("smpte170m")),
        // Untagged HD beside a clip that names a matrix is labelled BT.709 by the graph.
        (
            "an untagged HD bottom beside a tagged one",
            &untagged_hd,
            &bt709,
            Some("bt709"),
            Some("bt709"),
        ),
        (
            "a tagged bottom under an untagged one",
            &bt709,
            &untagged_sd,
            Some("bt709"),
            Some("bt709"),
        ),
    ] {
        let tl = timeline_of(vec![
            video_track(vec![make_clip(bottom.id, 0.0, 4.0, 0.0)]),
            video_track(vec![blended(top, 0.0)]),
        ]);
        let assets = [bottom.clone(), top.clone()];
        let graph = graph_of(&tl, &assets);
        assert_eq!(named(&graph).as_deref(), graph_names, "{name}: {graph}");
        for mode in [PlanMode::Motion, PlanMode::Still] {
            let planner = Planner::new(
                &tl,
                &assets,
                &ExportOptions::default(),
                PlanRequest {
                    mode,
                    ..PlanRequest::still(CompositeColorPolicy::FixedBt601)
                },
            )
            .expect("planner");
            let plan = planner.at(1.0).expect("plan");
            assert_eq!(plan.canvas.blend_tag, tag, "{name} {mode:?}");
        }
    }
    // The first picture of the lowest video track: a clip that starts later in the track's list but earlier on the timeline.
    let tl = timeline_of(vec![video_track(vec![
        blended(&untagged_sd, 5.0),
        make_clip(bt709.id, 0.0, 4.0, 0.0),
    ])]);
    let assets = [untagged_sd, bt709];
    let graph = graph_of(&tl, &assets);
    assert_eq!(named(&graph).as_deref(), Some("bt709"), "{graph}");
    let plan = Planner::new(
        &tl,
        &assets,
        &ExportOptions::default(),
        PlanRequest {
            mode: PlanMode::Motion,
            ..PlanRequest::still(CompositeColorPolicy::FixedBt601)
        },
    )
    .expect("planner");
    assert_eq!(plan.canvas().blend_tag, Some("bt709"));
}

#[test]
fn a_degenerate_mask_is_clamped_rather_than_blanking_the_clip() {
    // A zero-width shape would make the whole clip transparent, which is
    // never what was meant by dragging a handle too far.
    let m = crate::model::Mask {
        width: 0.0,
        height: -3.0,
        x: 9.0,
        feather: f64::NAN,
        ..Default::default()
    }
    .normalized();
    assert!(m.width >= 0.01 && m.height >= 0.01);
    assert_eq!(m.x, 1.0);
    assert!((0.0..=1.0).contains(&m.feather));
}

#[test]
fn the_track_fader_and_pan_ride_the_finished_clip() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.volume = 0.8;
    clip.audio = vec![crate::model::AudioEffect::Highpass { hz: 80.0 }];
    let mut timeline = single(vec![clip]);
    timeline.tracks[0].volume = 0.5;
    timeline.tracks[0].pan = -1.0;
    let g = graph_of(&timeline, &[asset]);
    // The clip's own gain, then its effects, then the fader: a channel strip,
    // so the compressor upstream never sees the fader move.
    let clip_gain = g.find("volume=0.8").expect("clip gain");
    let effect = g.find("highpass").expect("clip effect");
    let fader = g.find("volume=0.5").expect("track fader");
    assert!(clip_gain < effect && effect < fader, "fader must come last: {g}");
    // Hard left is the right channel gone and the left untouched.
    assert!(g.contains("pan=stereo|c0=1*c0|c1=0*c1"), "{g}");
    // And the pan runs after the aformat upmix: run before it, a mono
    // source has no c1 and the attenuated leg would be pure silence.
    let af = g.find("aformat=").expect("aformat");
    let pan = g.find("pan=stereo").expect("pan");
    assert!(af < pan, "pan must follow the layout normalize: {g}");
}

#[test]
fn an_untouched_track_mix_emits_nothing() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let g = graph_of(&timeline, &[asset]);
    // Unity and centre are the historical graph exactly — no fader, no pan.
    assert!(!g.contains("pan=stereo"), "{g}");
    assert_eq!(g.matches("volume=").count(), 1, "only the clip's own gain: {g}");
}

#[test]
fn a_pan_is_dropped_when_there_is_nowhere_to_pan_to() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    timeline.tracks[0].pan = 1.0;
    let opts = ExportOptions {
        audio_channels: Some(1),
        ..Default::default()
    };
    let args = build_export_args(&timeline, &[asset], "out.mp4", &opts).unwrap();
    let g = args.join(" ");
    assert!(!g.contains("pan=stereo"), "a mono delivery has no stereo field: {g}");
}

#[test]
fn ducked_track_sidechains_under_the_rest() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let voice = make_clip(asset.id, 0.0, 10.0, 0.0);
    let music = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut music_track = audio_track(vec![music]);
    music_track.duck = true;
    let tl = timeline_of(vec![video_track(vec![voice]), music_track]);
    let g = graph_of(&tl, &[asset]);
    assert!(g.contains("sidechaincompress"), "{g}");
    assert!(g.contains("[akmix][aducked]amix=inputs=2"), "{g}");
}

#[test]
fn a_duck_depth_gates_the_bus_by_exactly_that_much() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let voice = make_clip(asset.id, 0.0, 10.0, 0.0);
    let music = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut music_track = audio_track(vec![music]);
    music_track.duck = true;
    let mut tl = timeline_of(vec![video_track(vec![voice]), music_track]);
    tl.master.duck_depth_db = Some(-12.0);
    let g = graph_of(&tl, std::slice::from_ref(&asset));
    assert!(!g.contains("sidechaincompress"), "{g}");
    assert!(
        g.contains("[adc][akfloor]sidechaingate=threshold=0.03:range=0:ratio=9000"),
        "{g}"
    );
    assert!(
        g.contains("aeval='1-0.748811*max(val(0),val(1))':c=mono,pan=stereo|c0=c0|c1=c0[again]"),
        "{g}"
    );
    assert!(g.contains("[aduckdry][again]amultiply[aducked]"), "{g}");
    assert!(g.contains("[akmix][aducked]amix=inputs=2"), "{g}");
    let mono = ExportOptions {
        audio_channels: Some(1),
        ..Default::default()
    };
    let args = build_export_args(&tl, &[asset], "out.mp4", &mono).unwrap();
    assert!(args.join(" ").contains(":c=mono[again]"), "a mono delivery needs no upmix");
}

#[test]
fn the_duck_depth_is_clamped_and_absent_by_default() {
    let mut m = crate::model::MasterBus::default();
    assert_eq!(m.safe_duck_depth_db(), None);
    assert!(m.is_default());
    m.duck_depth_db = Some(-90.0);
    assert_eq!(m.safe_duck_depth_db(), Some(crate::model::DUCK_MIN_DEPTH_DB));
    m.duck_depth_db = Some(3.0);
    assert_eq!(m.safe_duck_depth_db(), Some(-1.0));
    m.duck_depth_db = Some(f64::NAN);
    assert_eq!(m.safe_duck_depth_db(), Some(crate::model::DUCK_DEFAULT_DEPTH_DB));
}

#[test]
#[ignore = "drives the ffmpeg binary"]
fn the_speech_gate_ducks_by_its_depth_and_recovers() {
    use crate::engine::{decode_audio, write_wav, AudioBuffer};
    let dir = std::env::temp_dir().join(format!("kerf-duck-gate-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let sr = 48_000u32;
    let tone = |hz: f64, secs: f64| -> Vec<f32> {
        (0..(secs * sr as f64) as usize)
            .map(|n| (0.2 * (2.0 * std::f64::consts::PI * hz * n as f64 / sr as f64).sin()) as f32)
            .collect()
    };
    let wav = |name: &str, samples: Vec<f32>| {
        let path = dir.join(name);
        write_wav(&path, &AudioBuffer::new(sr, 1, samples)).unwrap();
        let mut a = test_asset(vec![audio_stream(sr, 1)]);
        a.path = path.to_string_lossy().into_owned();
        a.duration = 6.0;
        a
    };
    let music = wav("music.wav", tone(220.0, 6.0));
    let mut voice = wav("voice.wav", tone(1000.0, 2.0));
    voice.duration = 2.0;
    let mut bed = audio_track(vec![make_clip(music.id, 0.0, 6.0, 0.0)]);
    bed.duck = true;
    let mut tl = timeline_of(vec![audio_track(vec![make_clip(voice.id, 0.0, 2.0, 2.0)]), bed]);
    tl.master.duck_depth_db = Some(-12.0);

    for channels in [1u16, 2] {
        let out = dir.join(format!("mix-{channels}.wav"));
        let opts = ExportOptions {
            container: Container::Wav,
            video_codec: None,
            audio_codec: Some("pcm_s16le".into()),
            audio_sample_rate: Some(sr),
            audio_channels: Some(channels),
            ..Default::default()
        };
        render_with(&tl, &[voice.clone(), music.clone()], &out, &opts).unwrap();
        // Probe the written channel count by decoding as stereo only when it is.
        let mix = decode_audio(&out, sr, channels).unwrap();
        assert_eq!(mix.frames(), 6 * sr as usize, "{channels} ch: the bed plays to its end");
        for ch in 0..channels as usize {
            let samples: Vec<f32> = mix.samples.iter().skip(ch).step_by(channels as usize).copied().collect();
            // The 220 Hz bed's amplitude over a window, by correlating with it.
            let level = |from: f64, to: f64| -> f64 {
                let (a, b) = ((from * sr as f64) as usize, (to * sr as f64) as usize);
                let (mut re, mut im) = (0.0, 0.0);
                for (n, x) in samples.iter().enumerate().take(b).skip(a) {
                    let w = 2.0 * std::f64::consts::PI * 220.0 * n as f64 / sr as f64;
                    re += *x as f64 * w.cos();
                    im += *x as f64 * w.sin();
                }
                2.0 * (re * re + im * im).sqrt() / (b - a) as f64
            };
            let db = |x: f64| 20.0 * x.log10();
            let (before, during, after) = (level(0.5, 1.5), level(2.5, 3.5), level(5.0, 5.9));
            assert!(
                (db(during / before) - -12.0).abs() < 0.5,
                "{channels} ch, ch {ch}: ducked by {} dB",
                db(during / before)
            );
            assert!(
                db(after / before).abs() < 0.5,
                "{channels} ch, ch {ch}: recovered to {} dB",
                db(after / before)
            );
            assert!(before > 0.1, "{channels} ch, ch {ch}: the bed is there ({before})");
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn duck_flag_without_other_audio_keeps_the_flat_mix() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let music = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut t = audio_track(vec![music]);
    t.duck = true;
    let g = graph_of(&timeline_of(vec![t]), &[asset]);
    assert!(!g.contains("sidechaincompress"), "{g}");
    assert!(g.contains("amix=inputs=1"), "{g}");
}

#[test]
fn a_loudness_preset_sets_the_target_and_youtube_is_the_old_loudnorm() {
    let args = |loudnorm, loudness| {
        let opts = ExportOptions {
            loudnorm,
            loudness,
            ..Default::default()
        };
        args_of(&opts).join(" ")
    };
    assert_eq!(args(false, Some(LoudnessPreset::Youtube)), args(true, None));
    assert!(args(false, Some(LoudnessPreset::Apple)).contains("loudnorm=I=-16:TP=-1.5:LRA=11,aresample="));
    assert!(args(true, Some(LoudnessPreset::Broadcast)).contains("loudnorm=I=-23:TP=-1.5:LRA=11"));
    assert!(!args(false, None).contains("loudnorm"));
}

#[test]
fn loudnorm_option_appends_to_the_final_mix() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    let opts = ExportOptions {
        loudnorm: true,
        ..Default::default()
    };
    let args = build_export_args(&single(vec![clip]), &[asset], "out.mp4", &opts).unwrap();
    let joined = args.join(" ");
    assert!(joined.contains("loudnorm=I=-14:TP=-1.5:LRA=11,aresample="), "{joined}");
}

// ---- the master bus ----------------------------------------------------

use crate::model::MasterBus;

/// A video track with one clip of sound, delivered through a master `bus`.
fn master_cut(bus: MasterBus) -> (Timeline, Vec<Asset>) {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut tl = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    tl.master = bus;
    (tl, vec![asset])
}

fn mix_of(tl: &Timeline, assets: &[Asset], opts: &ExportOptions) -> String {
    build_export_args(tl, assets, "out.mp4", opts).unwrap().join(" ")
}

#[test]
fn a_neutral_master_adds_nothing_to_the_mix() {
    let (tl, assets) = master_cut(MasterBus::default());
    let opts = ExportOptions::default();
    let g = mix_of(&tl, &assets, &opts);
    assert!(g.contains("amix=inputs=1:normalize=0:dropout_transition=0[outa]"), "{g}");
    assert!(!g.contains("alimiter"), "{g}");
    // A ceiling kept while the limiter is off is neutral too: the same argv.
    let mut parked = tl;
    parked.master.ceiling_db = -6.0;
    assert_eq!(mix_of(&parked, &assets, &opts), g);
}

#[test]
fn the_master_fader_follows_the_final_sum() {
    let (tl, assets) = master_cut(MasterBus {
        volume: 0.5,
        ..MasterBus::default()
    });
    let g = mix_of(&tl, &assets, &ExportOptions::default());
    assert!(
        g.contains("amix=inputs=1:normalize=0:dropout_transition=0,volume=0.5[outa]"),
        "{g}"
    );
    assert!(!g.contains("alimiter"), "{g}");
}

#[test]
fn the_limiter_holds_the_ceiling_without_levelling_or_delaying_the_mix() {
    let (tl, assets) = master_cut(MasterBus {
        limiter: true,
        ..MasterBus::default()
    });
    let g = with_alimiter_latency(true, || mix_of(&tl, &assets, &ExportOptions::default()));
    // -1.5 dB is 0.841395 linear. `level=0` because the default auto-levels the
    // output back up to full scale; `latency=1` because without it the lookahead
    // delays the sound against the picture and drops its tail.
    assert!(
        g.contains("dropout_transition=0,alimiter=limit=0.841395:attack=5:release=100:level=0:latency=1[outa]"),
        "{g}"
    );
    assert!(!g.contains("dropout_transition=0,volume"), "no fader at unity: {g}");
}

#[test]
fn an_alimiter_without_latency_is_given_the_limiter_without_it() {
    // FFmpeg 4.4 refuses the option (`Option 'latency' not found`) and with it the
    // whole graph; the rest of the limiter is spelled exactly as before.
    let (tl, assets) = master_cut(MasterBus {
        limiter: true,
        ..MasterBus::default()
    });
    let g = with_alimiter_latency(false, || mix_of(&tl, &assets, &ExportOptions::default()));
    assert!(
        g.contains("dropout_transition=0,alimiter=limit=0.841395:attack=5:release=100:level=0[outa]"),
        "{g}"
    );
    assert!(!g.contains("latency"), "{g}");
    // A fader-only master has no limiter to spell.
    let (tl, assets) = master_cut(MasterBus {
        volume: 0.5,
        ..MasterBus::default()
    });
    let g = with_alimiter_latency(false, || mix_of(&tl, &assets, &ExportOptions::default()));
    assert!(g.contains("dropout_transition=0,volume=0.5[outa]"), "{g}");
}

#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_latency_probe_agrees_with_whether_this_ffmpeg_accepts_the_option() {
    // The probe reads help text; what matters is that a graph spelled by the answer
    // runs. FFmpeg 4.4 exits 1 on `latency=1` ("Option 'latency' not found").
    let accepts = |opts: &str| {
        command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "quiet", "-f", "lavfi", "-i", "sine=d=0.1"])
            .args(["-af", &format!("alimiter={opts}"), "-f", "null", "-"])
            .status_bounded()
            .expect("run ffmpeg")
            .success()
    };
    assert!(accepts("limit=0.8:attack=5:release=100:level=0"), "the limiter itself");
    assert_eq!(
        accepts("limit=0.8:attack=5:release=100:level=0:latency=1"),
        alimiter_latency_available()
    );
    let tl = master_cut(MasterBus {
        limiter: true,
        ..MasterBus::default()
    });
    let g = mix_of(&tl.0, &tl.1, &ExportOptions::default());
    assert_eq!(g.contains(":latency=1"), alimiter_latency_available(), "{g}");
}

#[test]
fn the_latency_option_is_read_off_the_filters_own_help() {
    // `ffmpeg -h filter=alimiter` as FFmpeg 4.4.2 (Ubuntu 22.04) prints it, and 9.0.2.
    let old = "Filter alimiter\n  Audio lookahead limiter.\nalimiter AVOptions:\n  \
                   level_in          <double>     ..F.A...... set input level (from 0.015625 to 64) (default 1)\n  \
                   release           <double>     ..F.A...... set release (from 1 to 8000) (default 50)\n  \
                   level             <boolean>    ..F.A...... auto level (default true)\n";
    let new = format!(
        "{old}   latency           <boolean>    ..F.A....T. compensate delay (default false)\n\n\
             This filter has support for timeline through the 'enable' option.\n"
    );
    assert!(!help_lists_latency(old));
    assert!(help_lists_latency(&new));
    // Only an option of that name: a description that mentions it is not one.
    assert!(!help_lists_latency("  asc <boolean> set the latency of the asc\n"));
    assert!(!help_lists_latency(""));
}

#[test]
fn the_master_runs_after_the_sum_and_before_loudnorm() {
    let (tl, assets) = master_cut(MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume: 0.8,
        limiter: true,
        ceiling_db: -3.0,
    });
    let opts = ExportOptions {
        loudnorm: true,
        ..ExportOptions::default()
    };
    let g = mix_of(&tl, &assets, &opts);
    let amix = g.find("amix=inputs=1").expect(&g);
    let fader = g.find(",volume=0.8,").expect(&g);
    let limiter = g.find(",alimiter=limit=0.707946:").expect(&g);
    let norm = g.find(",loudnorm=I=-14").expect(&g);
    assert!(amix < fader && fader < limiter && limiter < norm, "{g}");
    assert!(g.contains("loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000[outa]"), "{g}");
}

#[test]
fn the_master_closes_the_duck_bus_sum() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut music = audio_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    music.duck = true;
    let mut tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]), music]);
    tl.master.volume = 0.6;
    let g = mix_of(&tl, &[asset], &ExportOptions::default());
    assert!(
        g.contains("[akmix][aducked]amix=inputs=2:normalize=0:dropout_transition=0,volume=0.6[outa]"),
        "{g}"
    );
}

#[test]
fn a_range_export_keeps_the_master() {
    let (tl, assets) = master_cut(MasterBus {
        limiter: true,
        ..MasterBus::default()
    });
    let opts = ExportOptions {
        range: Some(crate::model::TimeRange { start: 2.0, end: 6.0 }),
        ..ExportOptions::default()
    };
    assert!(mix_of(&tl, &assets, &opts).contains("alimiter=limit=0.841395"));
}

#[test]
fn a_delivery_variant_keeps_the_master() {
    // `render_variants` builds every file from `for_delivery`: the same mix at
    // every frame, only the picture changes.
    let (tl, assets) = master_cut(MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume: 0.5,
        limiter: true,
        ceiling_db: -2.0,
    });
    let variant = tl.for_delivery(Delivery::new(1080, 1920, Fit::Cover));
    let g = mix_of(&variant, &assets, &ExportOptions::default());
    assert!(g.contains(",volume=0.5,alimiter=limit=0.794328:"), "{g}");
}

#[test]
fn the_playback_stream_has_no_sound_for_the_master_to_touch() {
    let (tl, assets) = master_cut(MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume: 0.5,
        limiter: true,
        ceiling_db: -1.0,
    });
    let args = build_preview_args_with(&tl, &assets, 0.0, 30.0, 640, 4, None)
        .unwrap()
        .join(" ");
    assert!(!args.contains("alimiter") && !args.contains("volume=0.5"), "{args}");
}

#[test]
fn master_filters_are_safe_for_a_file_that_never_went_through_the_op() {
    let bus = |volume, ceiling_db| MasterBus {
        duck_depth_db: None,
        true_peak: false,
        volume,
        limiter: true,
        ceiling_db,
    };
    let filters = |bus| with_alimiter_latency(true, || master_filters(&bus, 48_000));
    // Not a number: no fader, the default ceiling.
    assert_eq!(
        filters(bus(f64::NAN, f64::NAN)),
        ",alimiter=limit=0.841395:attack=5:release=100:level=0:latency=1"
    );
    // Beyond the ends: clamped to what `alimiter` accepts (its `limit` is 0.0625..=1).
    assert_eq!(
        filters(bus(9.0, 12.0)),
        ",volume=4,alimiter=limit=1:attack=5:release=100:level=0:latency=1"
    );
    assert_eq!(
        filters(bus(-1.0, -90.0)),
        ",volume=0,alimiter=limit=0.063096:attack=5:release=100:level=0:latency=1"
    );
    assert_eq!(filters(MasterBus::default()), "");
}

#[test]
fn a_true_peak_limiter_runs_oversampled_and_is_omitted_when_off() {
    let (tl, assets) = master_cut(MasterBus {
        limiter: true,
        true_peak: true,
        ..MasterBus::default()
    });
    let g = with_alimiter_latency(true, || mix_of(&tl, &assets, &ExportOptions::default()));
    assert!(
            g.contains(
                "dropout_transition=0,aresample=192000,alimiter=limit=0.841395:attack=5:release=100:level=0:latency=1,aresample=48000[outa]"
            ),
            "{g}"
        );
    let opts = ExportOptions {
        audio_sample_rate: Some(44_100),
        ..ExportOptions::default()
    };
    let g = with_alimiter_latency(false, || mix_of(&tl, &assets, &opts));
    assert!(
        g.contains(",aresample=176400,alimiter=limit=0.841395:attack=5:release=100:level=0,aresample=44100[outa]"),
        "{g}"
    );

    // Off, or with the limiter off, the graph is the sample-peak one / the neutral one.
    let with = |master: MasterBus| {
        let mut t = tl.clone();
        t.master = master;
        mix_of(&t, &assets, &ExportOptions::default())
    };
    let g = with(MasterBus {
        limiter: true,
        ..MasterBus::default()
    });
    assert!(!g.contains("aresample"), "{g}");
    assert_eq!(
        with(MasterBus {
            true_peak: true,
            ..MasterBus::default()
        }),
        with(MasterBus::default())
    );
}

#[test]
fn static_loudness_applies_the_measured_gain_and_a_true_peak_limiter() {
    let (tl, assets) = master_cut(MasterBus {
        volume: 0.8,
        ..MasterBus::default()
    });
    let opts = ExportOptions {
        loudness: Some(LoudnessPreset::Apple),
        loudness_static: true,
        loudness_gain_db: Some(3.456),
        ..ExportOptions::default()
    };
    let g = with_alimiter_latency(true, || mix_of(&tl, &assets, &opts));
    // -1 dBTP less the margin, 4x oversampled; no loudnorm.
    assert!(
            g.contains(
                "dropout_transition=0,volume=0.8,volume=3.46dB,aresample=192000,alimiter=limit=0.860994:attack=5:release=100:level=0:latency=1,aresample=48000[outa]"
            ),
            "{g}"
        );
    assert!(!g.contains("loudnorm"), "{g}");

    // An unmeasured or negligible gain adds no `volume`; a wild one is clamped.
    for (gain, volume) in [(None, None), (Some(0.001), None), (Some(80.0), Some("volume=30.00dB"))] {
        let g = mix_of(
            &tl,
            &assets,
            &ExportOptions {
                loudness_gain_db: gain,
                ..opts.clone()
            },
        );
        assert_eq!(g.contains("dB,"), volume.is_some(), "{g}");
        if let Some(v) = volume {
            assert!(g.contains(v), "{g}");
        }
    }

    // Without a target the flag changes nothing, and the dynamic path is the old one.
    let plain = |static_: bool, loudness| {
        mix_of(
            &tl,
            &assets,
            &ExportOptions {
                loudness,
                loudness_static: static_,
                loudness_gain_db: Some(2.0),
                ..ExportOptions::default()
            },
        )
    };
    assert_eq!(plain(true, None), plain(false, None));
    assert_eq!(
        plain(false, Some(LoudnessPreset::Youtube)),
        mix_of(
            &tl,
            &assets,
            &ExportOptions {
                loudnorm: true,
                ..ExportOptions::default()
            }
        )
    );
}

#[test]
fn the_static_gain_is_the_distance_to_the_target_within_bounds() {
    assert_eq!(levels::static_gain_db(LoudnessPreset::Youtube, Some(-20.0)), Some(6.0));
    assert_eq!(levels::static_gain_db(LoudnessPreset::Broadcast, Some(-14.0)), Some(-9.0));
    assert_eq!(
        levels::static_gain_db(LoudnessPreset::Apple, Some(-60.0)),
        Some(LOUDNESS_MAX_GAIN_DB)
    );
    assert_eq!(levels::static_gain_db(LoudnessPreset::Apple, None), None);
    assert_eq!(levels::static_gain_db(LoudnessPreset::Apple, Some(f64::NEG_INFINITY)), None);
}

#[test]
fn crossfade_extends_the_outgoing_tail_and_dissolves_the_incoming() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    // Outgoing clip A renders one extra second of source under the dissolve.
    assert!(g.contains("trim=start=0:end=11"), "{g}");
    // Incoming clip B fades up via alpha, from where it sits on the timeline.
    assert!(g.contains("fade=t=in:st=10:d=1:alpha=1"), "{g}");
}

#[test]
fn a_crossfade_is_constant_power_and_a_clip_fade_stays_linear() {
    let (asset, other) = (av_asset(Uuid::new_v4(), 20.0), av_asset(Uuid::new_v4(), 20.0));
    let both = [asset.clone(), other.clone()];
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(other.id, 0.0, 10.0, 10.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a.clone(), b.clone()]), &both);
    assert!(g.contains("afade=t=out:st=10:d=1:curve=qsin,"), "{g}");
    assert!(g.contains("afade=t=in:st=0:d=1:curve=qsin,"), "{g}");

    // A cut inside one take plays the same samples both sides: the linear pair keeps
    // that correlated sound at unity. Elsewhere in the same take is two sounds again.
    let mut same = b.clone();
    same.asset_id = asset.id;
    (same.source_in, same.source_out) = (10.0, 20.0);
    let mut both_takes = vec![asset];
    both_takes[0].duration = 40.0;
    let g = graph_of(&single(vec![a.clone(), same.clone()]), &both_takes);
    assert!(g.contains("afade=t=out:st=10:d=1,"), "{g}");
    assert!(g.contains("afade=t=in:st=0:d=1,"), "{g}");
    assert!(!g.contains("qsin"), "{g}");
    (same.source_in, same.source_out) = (25.0, 35.0);
    let g = graph_of(&single(vec![a.clone(), same]), &both_takes);
    assert_eq!(g.matches("curve=qsin").count(), 2, "{g}");

    // A dissolve up from black has no partner: a linear fade.
    let g = graph_of(&single(vec![b.clone()]), &both);
    assert!(g.contains("afade=t=in:st=0:d=1,"), "{g}");
    assert!(!g.contains("qsin"), "{g}");

    // A clip fade beside the transition keeps the one linear ramp it always had.
    let (mut a2, mut b2) = (a.clone(), b.clone());
    a2.fade_out = 0.5;
    b2.fade_in = 0.25;
    let g = graph_of(&single(vec![a2, b2]), &both);
    assert!(g.contains("afade=t=out:st=9.5:d=1.5,"), "{g}");
    assert!(g.contains("afade=t=in:st=0:d=1.25,"), "{g}");
    assert!(!g.contains("qsin"), "{g}");

    // A dip is not a crossfade: the sound dips through silence linearly.
    let mut dip = b;
    dip.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::DipToBlack,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, dip]), &both);
    assert!(g.contains("afade=t=in:st=0:d=0.5,"), "{g}");
    assert!(!g.contains("qsin"), "{g}");
}

#[test]
fn color_eq_runs_before_alpha_is_established() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let mut clip = make_clip(asset.id, 0.0, 10.0, 0.0);
    clip.transform = crate::model::Transform {
        opacity: 0.5,
        ..Default::default()
    };
    clip.color = crate::model::Color {
        brightness: 0.1,
        ..Default::default()
    };
    let g = graph_of(&single(vec![clip]), &[asset]);
    let eq = g.find("eq=").expect("eq present");
    let alpha = g.find("format=yuva420p").expect("alpha present");
    // eq cannot carry alpha, so it must precede the alpha conversion or the
    // opacity (colorchannelmixer) would be silently dropped.
    assert!(eq < alpha, "eq must come before alpha: {g}");
    assert!(g.contains("colorchannelmixer=aa=0.5"), "{g}");
}

#[test]
fn crossfade_without_source_handle_is_a_hard_cut() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 20.0, 0.0); // uses the whole asset — no handle to borrow
    let mut b = make_clip(asset.id, 0.0, 10.0, 20.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    assert!(!g.contains(":alpha=1"), "no fade-from-black when there is no handle: {g}");
    assert!(g.contains("trim=start=0:end=20"), "outgoing tail must not be extended: {g}");
}

#[test]
fn crossfade_across_a_gap_dissolves_from_black_without_bleeding_the_partner() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 10.0, 15.0); // 5s gap after a
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    assert!(
        g.contains("trim=start=0:end=10"),
        "outgoing clip must not bleed across the gap: {g}"
    );
    assert!(
        g.contains("fade=t=in:st=15:d=1:alpha=1"),
        "incoming dissolves from black: {g}"
    );
}

#[test]
fn reversed_crossfade_extends_the_low_source_end() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let mut a = make_clip(asset.id, 5.0, 15.0, 0.0);
    a.speed = -1.0; // reversed, with 5s of handle below source_in
    let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    // Window [4,15] is fast-seeked: the input gets `-ss 4` and the trim is
    // expressed relative to it (start 0, 11s long).
    assert!(
        g.contains("trim=start=0:end=11"),
        "reversed tail extends below source_in: {g}"
    );
    assert!(g.contains(",reverse,"), "{g}");
}

#[test]
fn fast_seek_emits_input_ss_and_makes_the_trim_relative() {
    let asset = av_asset(Uuid::new_v4(), 60.0);
    // A subclip deep into the source: 30s..33s.
    let timeline = single(vec![make_clip(asset.id, 30.0, 33.0, 0.0)]);
    let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
    // `-ss 30` precedes the input so ffmpeg decodes from ~30s, not from 0.
    let ss = args.iter().position(|a| a == "-ss").expect("a fast-seek -ss");
    assert_eq!(args[ss + 1], "30");
    assert_eq!(args[ss + 2], "-i", "the -ss must immediately precede its input");
    // The graph trim/atrim are relative to the seek: a 3s window from 0.
    let filter = flag_val(&args, "-filter_complex").unwrap();
    assert!(filter.contains("trim=start=0:end=3"), "video trim is seek-relative: {filter}");
    assert!(
        filter.contains("atrim=start=0:end=3"),
        "audio trim is seek-relative: {filter}"
    );
}

#[test]
fn head_clips_emit_no_fast_seek() {
    // A clip that starts at the source head must not gain an -ss (decoding
    // from 0 is free) — args stay byte-identical to the pre-fast-seek build.
    let asset = av_asset(Uuid::new_v4(), 10.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
    assert!(!args.contains(&"-ss".to_string()), "no seek for a head clip: {args:?}");
    let filter = flag_val(&args, "-filter_complex").unwrap();
    assert!(filter.contains("trim=start=0:end=10"), "{filter}");
}

#[test]
fn a_still_image_is_decoded_once_and_held_not_seeked() {
    let asset = img_asset(Uuid::new_v4());
    // A full-length still placed deep in the timeline (t=12) — a non-image
    // clip there would gain an `-ss`; a still must not.
    let timeline = single(vec![make_clip(asset.id, 0.0, crate::model::DEFAULT_IMAGE_DURATION, 12.0)]);
    let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();

    assert!(
        !args.contains(&"-ss".to_string()),
        "a still is looped, never seeked: {args:?}"
    );
    // One frame decoded, not `-loop 1` decoding the file again for every frame.
    assert!(
        !args.contains(&"-loop".to_string()) && !args.contains(&"-t".to_string()),
        "{args:?}"
    );
    assert!(
        args.contains(&"-framerate".to_string()),
        "the held still gets an input framerate"
    );
    let i_pos = args.iter().position(|a| a == "-i").unwrap();
    assert_eq!(args[i_pos + 1], "/media/title.png");
    // The chain holds it for as long as its trim reads: its source window end and the read past
    // it (`picture_window`: a still's rate is unknown, a tenth of a second, and an output frame),
    // 5.1333 s at 30 fps, the 154 pictures `-loop 1 -t` gave.
    let filter = flag_val(&args, "-filter_complex").unwrap();
    assert!(filter.contains("[0:v]loop=loop=153:size=1,trim="), "{filter}");
    // No seek to subtract, so the trim window stays absolute, positioned at t=12.
    assert!(filter.contains("trim=start=0:end=5"), "{filter}");
    assert!(filter.contains("+12/TB"), "still composited at its timeline start: {filter}");
}

#[test]
fn a_trimmed_still_keeps_an_absolute_trim() {
    let asset = img_asset(Uuid::new_v4());
    // The user trimmed the still to its 1s..4s window — for a real video that
    // window would be fast-seeked; for a still the trim stays absolute.
    let timeline = single(vec![make_clip(asset.id, 1.0, 4.0, 0.0)]);
    let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
    assert!(!args.contains(&"-ss".to_string()));
    let filter = flag_val(&args, "-filter_complex").unwrap();
    // Held past source_out: 4.1333 s at 30 fps.
    assert!(filter.contains("loop=loop=123:size=1,"), "{filter}");
    assert!(filter.contains("trim=start=1:end=4"), "absolute, not seek-relative: {filter}");
}

#[test]
fn timeline_frame_does_not_seek_a_still() {
    let asset = img_asset(Uuid::new_v4());
    let timeline = single(vec![make_clip(asset.id, 0.0, crate::model::DEFAULT_IMAGE_DURATION, 0.0)]);
    // Composite at t=2: a still has one frame, so it must be read without `-ss`.
    let args = build_timeline_frame_args(&timeline, &[asset], &ExportOptions::default(), 2.0, 640, 4).unwrap();
    assert!(
        !args.contains(&"-ss".to_string()),
        "a still has one frame; don't seek it: {args:?}"
    );
    let i_pos = args.iter().position(|a| a == "-i").unwrap();
    assert_eq!(args[i_pos + 1], "/media/title.png");
}

#[test]
fn probe_flags_a_lone_still_image() {
    let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"png","width":1920,"height":1080,"r_frame_rate":"25/1"}],"format":{}}"#;
    let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
    assert_eq!(r.duration, 0.0, "a still probes with no duration");
    assert!(r.streams[0].image, "a lone, audio-less png is a still");
}

#[test]
fn probe_does_not_flag_ordinary_video() {
    let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"r_frame_rate":"30/1","duration":"12.0"}],"format":{"duration":"12.0"}}"#;
    let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
    assert!(!r.streams[0].image);
}

#[test]
fn probe_does_not_flag_an_animated_gif() {
    // A multi-frame gif probes with a real duration, so it is treated as video.
    let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"gif","width":480,"height":270,"r_frame_rate":"10/1"}],"format":{"duration":"3.0"}}"#;
    let r = probe_from_json(serde_json::from_str(json).unwrap(), None);
    assert!(!r.streams[0].image);
}

#[test]
fn dip_to_white_fades_both_sides_through_white() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::DipToWhite,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    assert!(g.contains("fade=t=out:st=9.5:d=0.5:c=white"), "{g}");
    assert!(g.contains("fade=t=in:st=10:d=0.5:c=white"), "{g}");
    // A dip never borrows a handle: neither clip is extended.
    assert!(g.contains("trim=start=0:end=10"), "{g}");
    assert!(!g.contains(":alpha=1"), "a dip is not a dissolve: {g}");
}

#[test]
fn slide_travels_the_incoming_clip_in_over_the_held_outgoing_one() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::SlideLeft,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    // The outgoing clip keeps playing under the incoming one, as for a dissolve.
    assert!(g.contains("trim=start=0:end=11"), "outgoing holds under the slide: {g}");
    // The incoming clip starts a whole frame to the right and travels to 0 over
    // the transition; local time is measured from its timeline start.
    assert!(g.contains("(t-10)"), "motion is expressed in clip-local time: {g}");
    assert!(g.contains("1+(-1)*((t-10)-0)/(1)"), "one frame of travel over 1s: {g}");
    // A slide is not a dissolve — the picture stays hard-edged.
    assert!(!g.contains(":alpha=1"), "{g}");
    // …but the sound still crossfades.
    assert!(g.contains("afade=t=in:st=0:d=1"), "{g}");
}

#[test]
fn push_carries_the_outgoing_clip_out_of_frame_too() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::PushUp,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    // The outgoing clip sits still until its own end, then leaves upwards over
    // its tail: y travels 0 → -1 between local 10s and 11s.
    assert!(g.contains("0+(-1)*((t-0)-10)/(1)"), "outgoing is pushed out: {g}");
    // The incoming one arrives from below over the same second.
    assert!(g.contains("1+(-1)*((t-10)-0)/(1)"), "incoming arrives: {g}");
}

#[test]
fn a_slide_composes_with_the_clip_position_it_already_has() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
    b.transform = crate::model::Transform {
        pos_x: 0.25,
        ..Default::default()
    };
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::SlideLeft,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    // The travel is added to the offset the clip already has, not substituted
    // for it — a picture-in-picture slides in to where it lives.
    assert!(g.contains("x='(W-w)/2+((0.25)+(if(lt((t-10)"), "{g}");
}

#[test]
fn slide_without_source_handle_is_a_hard_cut() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 20.0, 0.0); // no handle left to borrow
    let mut b = make_clip(asset.id, 0.0, 10.0, 20.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::PushLeft,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    assert!(g.contains("trim=start=0:end=20"), "outgoing tail must not be extended: {g}");
    assert!(
        !g.contains("overlay=x="),
        "nothing moves when there is nothing to move over: {g}"
    );
}

#[test]
fn a_transition_out_of_a_still_keeps_its_transition() {
    // A still loops, so it never runs out of source: a dissolve (or slide /
    // push) out of a title card must not degrade to a hard cut just because
    // the clip already spans the asset's nominal duration.
    let still = img_asset(Uuid::new_v4());
    let footage = av_asset(Uuid::new_v4(), 20.0);
    let d = crate::model::DEFAULT_IMAGE_DURATION;
    let a = make_clip(still.id, 0.0, d, 0.0);
    let mut b = make_clip(footage.id, 0.0, 10.0, d);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::Crossfade,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[still, footage]);
    // The still is held for the tail and the incoming clip dissolves in.
    assert!(g.contains(&format!("trim=start=0:end={}", d + 1.0)), "{g}");
    assert!(g.contains(":alpha=1"), "the dissolve must survive: {g}");
}

#[test]
fn dip_to_black_fades_both_sides_of_the_cut() {
    let asset = av_asset(Uuid::new_v4(), 20.0);
    let a = make_clip(asset.id, 0.0, 10.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 10.0, 10.0);
    b.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::DipToBlack,
        duration: 1.0,
    });
    let g = graph_of(&single(vec![a, b]), &[asset]);
    // Outgoing A fades out to black at its end, incoming B fades up from black.
    assert!(g.contains("fade=t=out:st=9.5:d=0.5"), "{g}");
    assert!(g.contains("fade=t=in:st=0:d=0.5"), "{g}");
}

// ---- export option mapping -------------------------------------------

/// The token following `flag` in `args`, if present.
fn flag_val<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// Build the argv for `opts` against a single 1080p video+audio clip.
fn args_of(opts: &ExportOptions) -> Vec<String> {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    build_export_args(&timeline, &[asset], "/out/x", opts).unwrap()
}

#[test]
fn build_export_args_default_unchanged() {
    // The bare default must reproduce the legacy argv: maps, but no codec /
    // crf / pix_fmt / faststart flags.
    let args = args_of(&ExportOptions::default());
    assert!(args.contains(&"[outv]".to_string()) && args.contains(&"[outa]".to_string()));
    assert!(!args.contains(&"-c:v".to_string()));
    assert!(!args.contains(&"-c:a".to_string()));
    assert!(!args.contains(&"-crf".to_string()));
    assert!(!args.contains(&"-pix_fmt".to_string()));
    assert!(!args.contains(&"-movflags".to_string()));
    assert_eq!(args.last().unwrap(), "/out/x");
}

#[test]
fn build_export_args_h264_crf_in_order() {
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        audio_codec: Some("aac".into()),
        crf: Some(20),
        preset: Some("medium".into()),
        pix_fmt: Some("yuv420p".into()),
        audio_bitrate: Some("192k".into()),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert_eq!(flag_val(&args, "-c:v"), Some("libx264"));
    assert_eq!(flag_val(&args, "-crf"), Some("20"));
    assert_eq!(flag_val(&args, "-preset"), Some("medium"));
    assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv420p"));
    assert_eq!(flag_val(&args, "-c:a"), Some("aac"));
    assert_eq!(flag_val(&args, "-b:a"), Some("192k"));
    // The maps precede -c:v, which precedes its private -crf.
    let map = args.iter().position(|a| a == "[outv]").unwrap();
    let cv = args.iter().position(|a| a == "-c:v").unwrap();
    let crf = args.iter().position(|a| a == "-crf").unwrap();
    assert!(map < cv && cv < crf);
}

/// The argv of `opts` over a 1080p clip whose master bus is `master`.
fn args_with_master(master: MasterBus, opts: &ExportOptions) -> Vec<String> {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let mut timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    timeline.master = master;
    build_export_args(&timeline, &[asset], "/out/x", opts).unwrap()
}

fn limited() -> MasterBus {
    MasterBus {
        limiter: true,
        ..MasterBus::default()
    }
}

fn with_audio(codec: &str) -> ExportOptions {
    ExportOptions {
        audio_codec: Some(codec.into()),
        audio_bitrate: Some("192k".into()),
        ..ExportOptions::default()
    }
}

#[test]
fn aac_under_a_peak_ceiling_has_perceptual_noise_substitution_off() {
    // The master limiter alone, then each way of asking for a loudness target.
    let targets = [
        ExportOptions::default(),
        ExportOptions {
            loudnorm: true,
            ..ExportOptions::default()
        },
        ExportOptions {
            loudness: Some(LoudnessPreset::Apple),
            ..ExportOptions::default()
        },
        ExportOptions {
            loudness: Some(LoudnessPreset::Youtube),
            loudness_static: true,
            loudness_gain_db: Some(3.0),
            ..ExportOptions::default()
        },
    ];
    for (n, extra) in targets.into_iter().enumerate() {
        let master = if n == 0 { limited() } else { MasterBus::default() };
        let opts = ExportOptions {
            audio_codec: Some("aac".into()),
            audio_bitrate: Some("192k".into()),
            ..extra
        };
        let args = args_with_master(master, &opts);
        let at = args.iter().position(|a| a == "-c:a").expect("-c:a");
        assert_eq!(
            args[at..at + 5],
            ["-c:a", "aac", "-aac_pns", "0", "-b:a"],
            "case {n}: {args:?}"
        );
        assert_eq!(args[at + 5], "192k");
    }
    // The audio-only container takes the same path.
    let opts = ExportOptions {
        container: Container::M4a,
        ..with_audio("aac")
    };
    assert_eq!(flag_val(&args_with_master(limited(), &opts), "-aac_pns"), Some("0"));
}

#[test]
fn aac_without_a_peak_ceiling_keeps_its_argv() {
    let opts = with_audio("aac");
    let args = args_with_master(MasterBus::default(), &opts);
    assert!(!args.contains(&"-aac_pns".to_string()), "{args:?}");
    assert_eq!(flag_val(&args, "-c:a"), Some("aac"));
    assert_eq!(flag_val(&args, "-b:a"), Some("192k"));
    // A stored ceiling beside the limiter turned off, and a fader, hold nothing.
    let quiet = MasterBus {
        ceiling_db: -3.0,
        volume: 0.8,
        true_peak: true,
        ..MasterBus::default()
    };
    assert!(!args_with_master(quiet, &opts).contains(&"-aac_pns".to_string()));
    // The limiter on beside a silent export: no sound, no encoder to tune.
    let muted = ExportOptions {
        include_audio: false,
        ..with_audio("aac")
    };
    let args = args_with_master(limited(), &muted);
    assert!(
        !args.contains(&"-aac_pns".to_string()) && !args.contains(&"-c:a".to_string()),
        "{args:?}"
    );
}

#[test]
fn no_other_audio_codec_is_given_the_aac_option() {
    let normalized = ExportOptions {
        loudnorm: true,
        ..ExportOptions::default()
    };
    for codec in ["libmp3lame", "libopus", "flac", "pcm_s16le", "alac", "bogus"] {
        let opts = ExportOptions {
            audio_codec: Some(codec.into()),
            ..normalized.clone()
        };
        let args = args_with_master(limited(), &opts);
        assert_eq!(flag_val(&args, "-c:a"), Some(codec));
        assert!(!args.contains(&"-aac_pns".to_string()), "{codec}: {args:?}");
    }
    // No codec named: the container picks it, and nothing is said about the encoder.
    let args = args_with_master(limited(), &normalized);
    assert!(!args.contains(&"-aac_pns".to_string()), "{args:?}");
}

#[test]
fn a_peak_ceiling_is_the_limiter_or_a_loudness_target() {
    let off = ExportOptions::default();
    assert!(!holds_peak_ceiling(&MasterBus::default(), &off));
    assert!(holds_peak_ceiling(&limited(), &off));
    for opts in [
        ExportOptions {
            loudnorm: true,
            ..off.clone()
        },
        ExportOptions {
            loudness: Some(LoudnessPreset::Broadcast),
            ..off.clone()
        },
    ] {
        assert!(holds_peak_ceiling(&MasterBus::default(), &opts));
    }
    // Nothing else the master carries is a ceiling.
    let others = MasterBus {
        volume: 1.5,
        ceiling_db: -6.0,
        true_peak: true,
        ..MasterBus::default()
    };
    assert!(!holds_peak_ceiling(&others, &off));
}

#[test]
fn build_export_args_vp9_crf_pairs_bv0_and_cpu_used() {
    let opts = ExportOptions {
        container: Container::Webm,
        video_codec: Some("libvpx-vp9".into()),
        audio_codec: Some("libopus".into()),
        crf: Some(31),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert_eq!(flag_val(&args, "-crf"), Some("31"));
    assert_eq!(flag_val(&args, "-b:v"), Some("0"));
    assert_eq!(flag_val(&args, "-cpu-used"), Some("4"));
    assert!(!args.contains(&"-preset".to_string()));
}

#[test]
fn build_export_args_nvenc_crf_uses_cq_not_crf() {
    let opts = ExportOptions {
        video_codec: Some("h264_nvenc".into()),
        crf: Some(20),
        preset: Some("p5".into()),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert_eq!(flag_val(&args, "-c:v"), Some("h264_nvenc"));
    // NVENC: VBR steered by -cq with -b:v 0; never the software -crf.
    assert!(!args.contains(&"-crf".to_string()));
    assert_eq!(flag_val(&args, "-rc"), Some("vbr"));
    assert_eq!(flag_val(&args, "-cq"), Some("20"));
    assert_eq!(flag_val(&args, "-b:v"), Some("0"));
    assert_eq!(flag_val(&args, "-preset"), Some("p5"));
}

#[test]
fn build_export_args_qsv_crf_uses_global_quality() {
    let opts = ExportOptions {
        video_codec: Some("h264_qsv".into()),
        crf: Some(23),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert!(!args.contains(&"-crf".to_string()));
    assert_eq!(flag_val(&args, "-global_quality"), Some("23"));
}

#[test]
fn build_export_args_videotoolbox_crf_maps_to_quality() {
    let opts = ExportOptions {
        video_codec: Some("h264_videotoolbox".into()),
        crf: Some(23),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert!(!args.contains(&"-crf".to_string()));
    // crf 23 → round((1 - 23/51) * 100) = 55 on the 1..100 scale.
    assert_eq!(flag_val(&args, "-q:v"), Some("55"));
    assert!(!args.contains(&"-preset".to_string()), "videotoolbox has no -preset");
}

#[test]
fn amf_crf_is_offset_onto_the_qp_scale_and_asks_for_quality() {
    // x264 CRF 20 measured equal to constant QP 26 on h264_amf and hevc_amf, not QP 20.
    for codec in ["h264_amf", "hevc_amf"] {
        let args = args_of(&ExportOptions {
            video_codec: Some(codec.into()),
            crf: Some(20),
            ..Default::default()
        });
        assert_eq!(flag_val(&args, "-rc"), Some("cqp"), "{codec}");
        assert_eq!(flag_val(&args, "-qp_i"), Some("26"), "{codec}");
        assert_eq!(flag_val(&args, "-qp_p"), Some("26"), "{codec}");
        assert_eq!(flag_val(&args, "-quality"), Some("quality"), "{codec}");
        assert!(!args.contains(&"-crf".to_string()));
    }
    // The AMF ranges are 0..51 (H.264, HEVC) and 0..255 (AV1): neither is run past.
    assert_eq!(hw_qp("h264_amf", 20), 26);
    assert_eq!(hw_qp("hevc_amf", 0), 6);
    assert_eq!(hw_qp("hevc_amf", 51), 51);
    assert_eq!(hw_qp("h264_amf", u32::MAX), 51);
    assert_eq!(hw_qp("av1_amf", 30), 120);
    assert_eq!(hw_qp("av1_amf", 63), 252);
    assert_eq!(hw_qp("av1_amf", 200), 255);
    assert_eq!(hw_qp("av1_vaapi", u32::MAX), 255);
}

#[test]
fn av1_amf_goes_in_mp4_mkv_and_webm_and_takes_the_av1_qp_scale() {
    let args = args_of(&ExportOptions {
        video_codec: Some("av1_amf".into()),
        crf: Some(30),
        ..Default::default()
    });
    assert_eq!(flag_val(&args, "-c:v"), Some("av1_amf"));
    assert_eq!(flag_val(&args, "-qp_i"), Some("120"));
    assert_eq!(flag_val(&args, "-quality"), Some("quality"));
    assert_eq!(flag_val(&args, "-tag:v"), None, "AV1 takes no hvc1 tag");
    for container in [Container::Mp4, Container::Mkv, Container::Webm] {
        assert!(container.video_ok("av1_amf"), "{container:?}");
    }
    assert!(!Container::Mov.video_ok("av1_amf"));
}

#[test]
fn amf_lossless_and_bitrate_keep_their_spelling_and_gain_only_the_quality_preset() {
    let lossless = args_of(&ExportOptions {
        video_codec: Some("h264_amf".into()),
        rate_control: RateControl::Lossless,
        ..Default::default()
    });
    assert_eq!(flag_val(&lossless, "-qp_i"), Some("0"));
    assert_eq!(flag_val(&lossless, "-quality"), Some("quality"));
    let bitrate = args_of(&ExportOptions {
        video_codec: Some("hevc_amf".into()),
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("8M".into()),
        ..Default::default()
    });
    assert_eq!(flag_val(&bitrate, "-b:v"), Some("8M"));
    assert!(!bitrate.contains(&"-qp_i".to_string()));
}

#[test]
fn the_other_hardware_families_keep_their_one_to_one_crf() {
    for (codec, flag) in [("h264_nvenc", "-cq"), ("hevc_qsv", "-global_quality")] {
        let args = args_of(&ExportOptions {
            video_codec: Some(codec.into()),
            crf: Some(20),
            ..Default::default()
        });
        assert_eq!(flag_val(&args, flag), Some("20"), "{codec}");
        assert!(!args.contains(&"-quality".to_string()), "{codec}");
    }
}

#[test]
fn a_vaapi_export_names_the_device_and_uploads_the_picture() {
    let args = args_of(&ExportOptions {
        video_codec: Some("h264_vaapi".into()),
        crf: Some(20),
        pix_fmt: Some("yuv420p".into()),
        ..Default::default()
    });
    assert_eq!(flag_val(&args, "-vaapi_device"), Some(VAAPI_DEFAULT_DEVICE));
    assert!(
        args.iter().position(|a| a == "-vaapi_device").unwrap() < args.iter().position(|a| a == "-i").unwrap(),
        "a global option, named ahead of the inputs"
    );
    let graph = flag_val(&args, "-filter_complex").unwrap();
    assert!(
        graph.contains("[vbase][v0]overlay=eof_action=pass:enable='between(t,0,10)'[vhw];[vhw]format=nv12,hwupload[outv];"),
        "{graph}"
    );
    assert_eq!(graph.matches("[outv]").count(), 1, "one producer of the mapped pad: {graph}");
    assert_eq!(flag_val(&args, "-c:v"), Some("h264_vaapi"));
    assert_eq!(flag_val(&args, "-rc_mode"), Some("CQP"));
    assert_eq!(flag_val(&args, "-global_quality"), Some("26"));
    assert!(
        !args.contains(&"-pix_fmt".to_string()),
        "the device's frames are formatted by the graph"
    );
    assert!(!args.contains(&"-crf".to_string()) && !args.contains(&"-preset".to_string()));
}

#[test]
fn a_vaapi_export_with_text_uploads_after_the_titles() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let mut timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    timeline.overlays = vec![TextOverlay::new("Hello", 1.0, 4.0)];
    let args = build_export_args(
        &timeline,
        &[asset],
        "/out/x",
        &ExportOptions {
            video_codec: Some("hevc_vaapi".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let graph = flag_val(&args, "-filter_complex").unwrap();
    assert!(graph.contains("[vtext]"), "{graph}");
    assert!(graph.contains("drawtext=") && graph.contains("[vhw];"), "{graph}");
    assert!(graph.contains("[vhw]format=nv12,hwupload[outv]"), "{graph}");
    assert_eq!(flag_val(&args, "-tag:v"), Some("hvc1"), "HEVC in mp4 keeps its tag");
}

#[test]
fn a_vaapi_codec_with_no_picture_to_upload_adds_nothing() {
    for container in [Container::Gif, Container::Mp3] {
        let args = args_of(&ExportOptions {
            container,
            video_codec: Some("h264_vaapi".into()),
            ..Default::default()
        });
        assert!(!args.contains(&"-vaapi_device".to_string()), "{container:?}");
        assert!(
            !flag_val(&args, "-filter_complex").unwrap_or("").contains("hwupload"),
            "{container:?}"
        );
    }
}

#[test]
fn exports_without_a_vaapi_codec_never_mention_it() {
    for codec in [
        None,
        Some("libx264"),
        Some("h264_amf"),
        Some("h264_nvenc"),
        Some("hevc_videotoolbox"),
    ] {
        let args = args_of(&ExportOptions {
            video_codec: codec.map(String::from),
            ..Default::default()
        });
        assert!(!args.contains(&"-vaapi_device".to_string()), "{codec:?}");
        assert!(
            !args.iter().any(|a| a.contains("hwupload") || a.contains("[vhw]")),
            "{codec:?}"
        );
    }
}

#[test]
fn vaapi_lossless_and_av1_use_the_device_quality_scale() {
    let lossless = args_of(&ExportOptions {
        video_codec: Some("hevc_vaapi".into()),
        rate_control: RateControl::Lossless,
        ..Default::default()
    });
    assert_eq!(flag_val(&lossless, "-global_quality"), Some("1"));
    let av1 = args_of(&ExportOptions {
        video_codec: Some("av1_vaapi".into()),
        crf: Some(30),
        ..Default::default()
    });
    assert_eq!(flag_val(&av1, "-global_quality"), Some("120"));
    assert!(!av1.contains(&"-tag:v".to_string()));
}

#[test]
fn vaapi_is_validated_against_the_container_and_the_pixel_format() {
    let ok = ExportOptions {
        video_codec: Some("h264_vaapi".into()),
        ..Default::default()
    };
    assert!(validate_export(&ok, true, true).is_empty());
    for pix_fmt in ["yuv420p", "nv12"] {
        let with = ExportOptions {
            pix_fmt: Some(pix_fmt.into()),
            ..ok.clone()
        };
        assert!(validate_export(&with, true, true).is_empty(), "{pix_fmt}");
    }
    let ten_bit = ExportOptions {
        pix_fmt: Some("yuv420p10le".into()),
        ..ok.clone()
    };
    assert_eq!(
        validate_export(&ten_bit, true, true),
        ["VAAPI encoders take 8-bit 4:2:0 (yuv420p) here."]
    );
    // Not a VAAPI export, so a 10-bit software one is left alone.
    let software = ExportOptions {
        video_codec: Some("libx265".into()),
        pix_fmt: Some("yuv420p10le".into()),
        ..Default::default()
    };
    assert!(validate_export(&software, true, true).is_empty());
    let two_pass = ExportOptions {
        rate_control: RateControl::TwoPass,
        video_bitrate: Some("8M".into()),
        ..ok
    };
    assert!(validate_export(&two_pass, true, true).iter().any(|i| i.contains("Two-pass")));
    // H.264 and HEVC go in mov, AV1 does not; WebM takes AV1 only.
    assert!(Container::Mov.video_ok("hevc_vaapi") && !Container::Mov.video_ok("av1_vaapi"));
    assert!(Container::Webm.video_ok("av1_vaapi") && !Container::Webm.video_ok("h264_vaapi"));
    assert!(Container::Mkv.video_ok("av1_vaapi") && Container::Mp4.video_ok("h264_vaapi"));
}

#[test]
fn the_probe_encode_matches_how_each_family_is_fed() {
    let system = encoder_probe_args("h264_amf", None).join(" ");
    assert_eq!(
        system,
        "-hide_banner -v error -f lavfi -i color=black:s=256x256:r=30:d=0.2 -frames:v 1 -pix_fmt nv12 -c:v h264_amf -f null -"
    );
    let vaapi = encoder_probe_args("hevc_vaapi", Some("/dev/dri/renderD129")).join(" ");
    assert_eq!(
        vaapi,
        "-hide_banner -v error -vaapi_device /dev/dri/renderD129 -f lavfi -i color=black:s=256x256:r=30:d=0.2 \
             -vf format=nv12,hwupload -frames:v 1 -c:v hevc_vaapi -f null -"
    );
}

#[test]
fn render_nodes_are_what_dri_holds_unless_one_is_named() {
    let scratch = Scratch::new("dri");
    let dir = scratch.join("dri");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["card0", "renderD129", "renderD128", "controlD64"] {
        std::fs::write(dir.join(name), b"").unwrap();
    }
    let nodes = render_nodes(&dir, None);
    let names: Vec<_> = nodes
        .iter()
        .map(|n| Path::new(n).file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["renderD128", "renderD129"], "render nodes only, in order");
    assert_eq!(
        render_nodes(&dir, Some("/dev/dri/renderD130".into())),
        ["/dev/dri/renderD130"]
    );
    assert_eq!(
        render_nodes(&dir, Some(String::new())).len(),
        2,
        "an empty override is no override"
    );
    for i in 0..8 {
        std::fs::write(dir.join(format!("renderD{}", 200 + i)), b"").unwrap();
    }
    assert_eq!(render_nodes(&dir, None).len(), VAAPI_DEVICES_TRIED);
    assert!(render_nodes(&dir.join("missing"), None).is_empty(), "no /dev/dri, no VAAPI");
}

/// Scripts standing in for `ffmpeg`, which decide by their argv alone: the probe is
/// handed a binary, a device and a limit, and nothing else.
#[cfg(unix)]
fn probe_script(scratch: &Scratch, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = scratch.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

#[cfg(unix)]
#[test]
fn a_probe_that_refuses_aborts_or_hangs_is_told_apart_and_a_hang_is_killed() {
    let scratch = Scratch::new("probe");
    let limit = std::time::Duration::from_secs(3);
    let probe = |body: &str| run_probe(&probe_script(&scratch, "p.sh", body), &[], limit);
    assert_eq!(probe("exit 0"), Probe::Works);
    assert_eq!(probe("exit 234"), Probe::Refused);
    // The pinned build aborts when a hardware library is missing: not a success.
    assert_eq!(probe("kill -ABRT $$"), Probe::Refused);
    let started = Instant::now();
    assert_eq!(probe("exec sleep 30"), Probe::Hung);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "it waited for the driver"
    );
    assert_eq!(
        run_probe(&scratch.join("missing").to_string_lossy(), &[], limit),
        Probe::Refused,
        "no such binary"
    );
}

#[cfg(unix)]
#[test]
fn vaapi_takes_the_first_render_node_that_encodes_and_gives_up_a_hung_one() {
    let scratch = Scratch::new("vaapi-nodes");
    let limit = std::time::Duration::from_secs(3);
    let all = ["h264_vaapi", "hevc_vaapi", "av1_vaapi", "h264_amf"];
    let compiled: std::collections::HashSet<&str> = all.into_iter().collect();
    let nodes = |names: &[&str]| names.iter().map(|n| format!("/dev/dri/{n}")).collect::<Vec<_>>();
    // renderD128 is somebody else's GPU (every encode refused), renderD129 encodes H.264
    // and HEVC but not AV1.
    let bin = probe_script(
        &scratch,
        "ffmpeg.sh",
        "case \"$*\" in *renderD129*av1_vaapi*) exit 1;; *renderD129*) exit 0;; *) exit 1;; esac",
    );
    let (found, device) = probe_vaapi(&bin, &compiled, &nodes(&["renderD128", "renderD129"]), limit);
    assert_eq!(found, ["h264_vaapi", "hevc_vaapi"]);
    assert_eq!(device.as_deref(), Some("/dev/dri/renderD129"));
    // Nothing works anywhere: no encoders, no device.
    let refused = probe_script(&scratch, "refuse.sh", "exit 1");
    assert_eq!(
        probe_vaapi(&refused, &compiled, &nodes(&["renderD128"]), limit),
        (Vec::new(), None)
    );
    // An ffmpeg without VAAPI is not probed at all (a script that would hang proves it).
    let hang = probe_script(&scratch, "hang.sh", "exec sleep 30");
    let started = Instant::now();
    let without: std::collections::HashSet<&str> = ["h264_amf"].into_iter().collect();
    assert_eq!(
        probe_vaapi(&hang, &without, &nodes(&["renderD128"]), limit),
        (Vec::new(), None)
    );
    // A wedged node costs one limit, not one per encoder, and the next node is still tried.
    let wedged = probe_script(
        &scratch,
        "wedged.sh",
        "case \"$*\" in *renderD128*) exec sleep 30;; *) exit 0;; esac",
    );
    let (found, device) = probe_vaapi(&wedged, &compiled, &nodes(&["renderD128", "renderD129"]), limit);
    assert_eq!(device.as_deref(), Some("/dev/dri/renderD129"));
    assert_eq!(found.len(), 3);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the hung node held the probe"
    );
}

#[cfg(unix)]
#[test]
fn a_family_is_asked_one_encoder_at_a_time_and_stops_at_a_hang() {
    let scratch = Scratch::new("family");
    let limit = std::time::Duration::from_secs(3);
    let compiled: std::collections::HashSet<&str> = ["h264_amf", "hevc_amf", "av1_amf", "h264_nvenc"].into_iter().collect();
    let works_but_hevc = probe_script(&scratch, "amf.sh", "case \"$*\" in *hevc_amf*) exit 1;; *) exit 0;; esac");
    // Only AMF's own encoders are asked, in candidate order.
    assert_eq!(
        probe_family(&works_but_hevc, EncFamily::Amf, &compiled, limit),
        ["h264_amf", "av1_amf"]
    );
    let started = Instant::now();
    let hangs = probe_script(&scratch, "hang.sh", "exec sleep 30");
    assert!(probe_family(&hangs, EncFamily::Amf, &compiled, limit).is_empty());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(7),
        "three hung probes would take three limits"
    );
}

#[test]
fn a_wait_for_a_probe_that_is_still_running_gives_up_and_one_that_finishes_is_read() {
    let late: OnceLock<u32> = OnceLock::new();
    let never: OnceLock<u32> = OnceLock::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = late.set(7);
        });
        assert_eq!(wait_for(&late, std::time::Duration::from_secs(30)), Some(&7));
    });
    assert_eq!(
        wait_for(&late, std::time::Duration::ZERO),
        Some(&7),
        "already set: no waiting"
    );
    let started = Instant::now();
    assert_eq!(wait_for(&never, std::time::Duration::from_millis(100)), None);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn only_encoders_that_take_system_memory_frames_are_candidates_for_a_stitch() {
    for encoder in HW_ENCODER_CANDIDATES {
        assert_ne!(enc_family(encoder), EncFamily::Vaapi, "{encoder}");
        assert_ne!(enc_family(encoder), EncFamily::Software, "{encoder}");
    }
    assert!(VAAPI_ENCODER_CANDIDATES.iter().all(|e| enc_family(e) == EncFamily::Vaapi));
    assert!(!HW_ENCODER_CANDIDATES.iter().any(|e| VAAPI_ENCODER_CANDIDATES.contains(e)));
}

#[test]
fn a_proxy_is_x264_unless_the_fast_gpu_pipeline_was_given_it_whatever_else_it_asks_for() {
    let hardware = ["_nvenc", "_qsv", "_videotoolbox", "_amf", "_vaapi"];
    for hw_decode in [None, Some("auto"), Some("d3d11va"), Some("vaapi")] {
        for tonemap in [None, Some("zscale=t=linear")] {
            for head_pad in [None, Some("-fps_mode")] {
                // No plan (the setting off, or a source the pipeline refused): x264 with hardware decode only.
                let args = proxy_encode_args(None, "/in.mov", "/out.mp4", 3, 1280, hw_decode, tonemap, head_pad);
                let at = args.iter().position(|a| a == "-c:v").expect("an encoder");
                assert_eq!(args[at + 1], "libx264", "{hw_decode:?} {tonemap:?} {head_pad:?}");
                assert!(!args.iter().any(|a| hardware.iter().any(|h| a.ends_with(h))), "{args:?}");
                assert_eq!(args.contains(&"-hwaccel".to_string()), hw_decode.is_some(), "decode stays");
                assert_eq!(flag_val(&args, "-g"), Some("1"));
                // ...and the argv is the one every build has always issued.
                assert_eq!(
                    args,
                    build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", hw_decode, tonemap, head_pad)
                );
            }
        }
    }
    // A plan (the opt-in Fast (GPU) pipeline, which refused every source that needs a tone-map, a
    // head pad or the software decode path) is the vendor's own: the encoder changes and nothing
    // software-only is in it.
    for vendor in FastVendor::ALL {
        let plan = FastPlan {
            vendor,
            device: None,
            width: 1280,
            height: 720,
            fps_flag: "-fps_mode",
        };
        let args = proxy_encode_args(Some(&plan), "/in.mov", "/out.mp4", 3, 1280, Some("auto"), None, None);
        assert_eq!(flag_val(&args, "-c:v"), Some(vendor.encoder()));
        assert_eq!(flag_val(&args, "-g"), Some("1"));
        assert_eq!(flag_val(&args, "-hwaccel"), flag_val(&args, "-hwaccel_output_format"));
        assert_ne!(
            flag_val(&args, "-hwaccel"),
            Some("auto"),
            "{vendor:?} names its own device type"
        );
        assert!(!args
            .iter()
            .any(|a| a.contains("libx264") || a == "-pix_fmt" || a == "-threads"));
    }
}

#[test]
fn the_probed_device_replaces_the_builders_stand_in() {
    let mut args: Vec<String> = ["-y", "-vaapi_device", VAAPI_DEFAULT_DEVICE, "-i", "a.mp4"]
        .map(String::from)
        .into();
    set_vaapi_device(&mut args, None);
    assert_eq!(args[2], VAAPI_DEFAULT_DEVICE, "nothing probed, nothing swapped");
    set_vaapi_device(&mut args, Some("/dev/dri/renderD129"));
    assert_eq!(args[2], "/dev/dri/renderD129");
    let mut plain: Vec<String> = ["-y", "-i", "a.mp4"].map(String::from).into();
    set_vaapi_device(&mut plain, Some("/dev/dri/renderD129"));
    assert_eq!(plain, ["-y", "-i", "a.mp4"]);
}

#[test]
fn build_export_args_hevc_hw_gets_hvc1_tag() {
    let opts = ExportOptions {
        video_codec: Some("hevc_nvenc".into()),
        crf: Some(24),
        ..Default::default()
    };
    let args = args_of(&opts);
    // The hvc1 tag must follow HEVC into mp4 for every encoder, not just libx265.
    assert_eq!(flag_val(&args, "-tag:v"), Some("hvc1"));
}

#[test]
fn build_export_args_hwaccel_decode_is_per_input_and_opt_in() {
    // Default: no -hwaccel at all (byte-for-byte legacy decode).
    let plain = args_of(&ExportOptions::default());
    assert!(!plain.contains(&"-hwaccel".to_string()));

    let opts = ExportOptions {
        hwaccel: Some("cuda".into()),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert_eq!(flag_val(&args, "-hwaccel"), Some("cuda"));
    // It's an input option: must precede the `-i` it accelerates.
    let hw = args.iter().position(|a| a == "-hwaccel").unwrap();
    let input = args.iter().position(|a| a == "-i").unwrap();
    assert!(hw < input);

    // "none" is treated as software (no flag emitted).
    let none = args_of(&ExportOptions {
        hwaccel: Some("none".into()),
        ..Default::default()
    });
    assert!(!none.contains(&"-hwaccel".to_string()));
}

#[test]
fn build_export_args_dedupes_identical_inputs_with_split() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    // The same source window on two video tracks (e.g. a composite) must decode
    // once: one `-i`, fanned out to both consumers with split / asplit.
    let timeline = timeline_of(vec![
        video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]),
        video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]),
    ]);
    let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
    assert_eq!(
        args.iter().filter(|a| a.as_str() == "-i").count(),
        1,
        "the shared source must be one input"
    );
    let f = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
    assert!(f.contains("[0:v]split=2[vsp0_0][vsp0_1]"), "{f}");
    assert!(f.contains("[0:a]asplit=2[asp0_0][asp0_1]"), "{f}");
    // Each clip's chain reads its own fan-out pad — never the input pad twice.
    assert!(f.contains("[vsp0_0]trim") && f.contains("[vsp0_1]trim"), "{f}");
}

#[test]
fn build_export_args_keeps_distinct_seeks_separate() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    // Same asset but different source_in → different fast-seek: two inputs, no
    // fan-out, so each still decodes only its own kept region.
    let timeline = timeline_of(vec![
        video_track(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]),
        video_track(vec![make_clip(asset.id, 10.0, 15.0, 0.0)]),
    ]);
    let args = build_export_args(&timeline, &[asset], "/out/x.mp4", &ExportOptions::default()).unwrap();
    assert_eq!(args.iter().filter(|a| a.as_str() == "-i").count(), 2);
    let f = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
    assert!(!f.contains("split="), "distinct seeks must not be fanned out: {f}");
}

#[test]
fn validate_export_rejects_two_pass_for_hardware_encoders() {
    let opts = ExportOptions {
        video_codec: Some("h264_nvenc".into()),
        rate_control: RateControl::TwoPass,
        video_bitrate: Some("8M".into()),
        ..Default::default()
    };
    let issues = validate_export(&opts, true, true);
    assert!(issues.iter().any(|i| i.contains("Two-pass")), "{issues:?}");
}

#[test]
fn build_export_args_x265_mp4_tags_hvc1_but_mkv_does_not() {
    let mp4 = ExportOptions {
        video_codec: Some("libx265".into()),
        container: Container::Mp4,
        ..Default::default()
    };
    assert_eq!(flag_val(&args_of(&mp4), "-tag:v"), Some("hvc1"));
    let mkv = ExportOptions {
        video_codec: Some("libx265".into()),
        container: Container::Mkv,
        ..Default::default()
    };
    assert!(!args_of(&mkv).contains(&"-tag:v".to_string()));
}

#[test]
fn build_export_args_prores_uses_profile_not_crf() {
    let opts = ExportOptions {
        container: Container::Mov,
        video_codec: Some("prores_ks".into()),
        prores_profile: Some(3),
        crf: Some(18), // ignored for prores
        pix_fmt: Some("yuv422p10le".into()),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert_eq!(flag_val(&args, "-profile:v"), Some("3"));
    assert!(!args.contains(&"-crf".to_string()));
    assert!(!args.contains(&"-preset".to_string()));
    assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv422p10le"));
}

#[test]
fn build_export_args_faststart_only_for_mp4_mov() {
    let mp4 = ExportOptions {
        video_codec: Some("libx264".into()),
        faststart: true,
        ..Default::default()
    };
    assert_eq!(flag_val(&args_of(&mp4), "-movflags"), Some("+faststart"));
    let mkv = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        faststart: true,
        ..Default::default()
    };
    assert!(!args_of(&mkv).contains(&"-movflags".to_string()));
}

#[test]
fn build_export_args_audio_only_drops_video() {
    let opts = ExportOptions {
        container: Container::Mp3,
        audio_codec: Some("libmp3lame".into()),
        audio_bitrate: Some("320k".into()),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert!(
        !args.contains(&"[outv]".to_string()),
        "no video map for an audio-only container"
    );
    assert!(!args.contains(&"-c:v".to_string()));
    assert!(!args.contains(&"-pix_fmt".to_string()));
    assert!(args.contains(&"[outa]".to_string()));
    assert_eq!(flag_val(&args, "-c:a"), Some("libmp3lame"));
    assert_eq!(flag_val(&args, "-b:a"), Some("320k"));
}

#[test]
fn build_export_args_include_audio_false_emits_an() {
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        include_audio: false,
        ..Default::default()
    };
    let args = args_of(&opts);
    assert!(!args.contains(&"[outa]".to_string()));
    assert!(args.contains(&"-an".to_string()));
    assert!(!args.contains(&"-c:a".to_string()));
}

#[test]
fn build_export_args_lossless_per_codec() {
    let x264 = ExportOptions {
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        ..Default::default()
    };
    assert_eq!(flag_val(&args_of(&x264), "-crf"), Some("0"));
    let vp9 = ExportOptions {
        container: Container::Webm,
        video_codec: Some("libvpx-vp9".into()),
        audio_codec: Some("libopus".into()),
        rate_control: RateControl::Lossless,
        ..Default::default()
    };
    assert_eq!(flag_val(&args_of(&vp9), "-lossless"), Some("1"));
}

#[test]
fn svt_av1_lossless_is_its_own_parameter_not_crf_zero() {
    // ffmpeg's libsvtav1 reads `-crf 0` as "no CRF given" and writes its default quality (35): measured at
    // the same bytes as no `-crf` at all, and not bit-exact. `lossless=1` is, on the pinned FFmpeg 9.0.2.
    let svt = |rate_control, crf| ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libsvtav1".into()),
        rate_control,
        crf,
        ..Default::default()
    };
    for (rate_control, crf) in [
        (RateControl::Lossless, None),
        (RateControl::Lossless, Some(30)),
        (RateControl::Crf, Some(0)),
    ] {
        let args = args_of(&svt(rate_control, crf));
        assert_eq!(
            flag_val(&args, "-svtav1-params"),
            Some("lossless=1"),
            "{rate_control:?} {crf:?}"
        );
        assert!(!args.contains(&"-crf".to_string()), "{rate_control:?} {crf:?}: {args:?}");
    }
    // Any other CRF is the CRF, and takes no parameter.
    for crf in [1, 30, 63] {
        let args = args_of(&svt(RateControl::Crf, Some(crf)));
        assert_eq!(flag_val(&args, "-crf"), Some(crf.to_string().as_str()));
        assert!(!args.contains(&"-svtav1-params".to_string()), "{args:?}");
    }
    // Neither does a bitrate mode, nor the other encoders' CRF 0.
    let bitrate = args_of(&ExportOptions {
        rate_control: RateControl::Bitrate,
        video_bitrate: Some("2M".into()),
        crf: Some(0),
        ..svt(RateControl::Bitrate, None)
    });
    assert!(!bitrate.contains(&"-svtav1-params".to_string()) && !bitrate.contains(&"-crf".to_string()));
    for codec in ["libx264", "libx265"] {
        let args = args_of(&ExportOptions {
            video_codec: Some(codec.into()),
            crf: Some(0),
            ..Default::default()
        });
        assert_eq!(flag_val(&args, "-crf"), Some("0"), "{codec}");
        assert!(!args.contains(&"-svtav1-params".to_string()));
    }
    // The lossless mode of x264 / x265 is untouched.
    for codec in ["libx264", "libx265"] {
        let lossless = ExportOptions {
            video_codec: Some(codec.into()),
            rate_control: RateControl::Lossless,
            ..Default::default()
        };
        assert_eq!(flag_val(&args_of(&lossless), "-crf"), Some("0"), "{codec}");
    }
}

/// The pinned FFmpeg 9.0.2 (SVT-AV1 4.2.0) writes a bit-exact picture for `lossless=1`; `-crf 0` was the
/// default quality. Compared the way a lossless claim is: the decoded pixels of the export, against the
/// export of the same cut by an encoder whose lossless is not in doubt (x264), through the engine's own graph.
#[test]
#[ignore = "needs the ffmpeg binary, with SVT-AV1 4.x"]
#[allow(clippy::print_stderr)]
fn svt_av1_lossless_export_decodes_to_the_same_pixels_as_x264_lossless() {
    // An ffmpeg without SVT-AV1's lossless refuses such an export (`svt_av1_lossless_supported`): nothing to compare there.
    if !svt_av1_lossless_supported() {
        eprintln!("this ffmpeg's SVT-AV1 has no lossless: the export is refused, nothing to compare");
        return;
    }
    let dir = std::env::temp_dir().join(format!("kerf-svt-lossless-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _remove = Remove(dir.clone());
    let source = dir.join("source.mp4");
    let made = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc2=s=320x180:r=30:d=1.5")
        .args(["-c:v", "libx264", "-crf", "0", "-pix_fmt", "yuv420p"])
        .arg(&source)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(made.success());
    let mut asset = test_asset(vec![video_stream(320, 180, 30.0)]);
    asset.path = source.to_string_lossy().into_owned();
    asset.duration = 1.5;
    let timeline = single(vec![make_clip(asset.id, 0.0, 1.5, 0.0)]);
    let assets = [asset];
    let md5_of = |codec: &str, rate_control: RateControl, crf: Option<u32>| -> String {
        let out = dir.join(format!("{codec}-{rate_control:?}-{crf:?}.mkv"));
        let opts = ExportOptions {
            container: Container::Mkv,
            video_codec: Some(codec.into()),
            rate_control,
            crf,
            preset: Some(if codec == "libsvtav1" { "8" } else { "medium" }.into()),
            pix_fmt: Some("yuv420p".into()),
            include_audio: false,
            ..Default::default()
        };
        let status = render_with_progress(&timeline, &assets, &out, &opts, &mut |_| {}, &|| false).expect("export");
        assert_eq!(status, RenderStatus::Completed);
        let decoded = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&out)
            .args(["-pix_fmt", "yuv420p", "-f", "md5", "-"])
            .output()
            .expect("decode");
        String::from_utf8_lossy(&decoded.stdout).trim().to_string()
    };
    let reference = md5_of("libx264", RateControl::Lossless, None);
    assert!(reference.starts_with("MD5="), "{reference}");
    assert_eq!(md5_of("libsvtav1", RateControl::Lossless, None), reference, "Lossless");
    assert_eq!(md5_of("libsvtav1", RateControl::Crf, Some(0)), reference, "CRF 0");
    assert_ne!(
        md5_of("libsvtav1", RateControl::Crf, Some(1)),
        reference,
        "CRF 1 is lossy: the comparison can fail"
    );
}

#[test]
fn build_export_args_two_pass_first_and_second() {
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        audio_codec: Some("aac".into()),
        rate_control: RateControl::TwoPass,
        video_bitrate: Some("8M".into()),
        faststart: true,
        metadata_title: Some("Cut".into()),
        ..Default::default()
    };
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let assets = [asset];
    let p1 = build_export_args_phase(
        &timeline,
        &assets,
        "/out/x.mp4",
        &opts,
        PassPhase::First,
        "/dev/null",
        "/tmp/pl",
    )
    .unwrap();
    assert_eq!(flag_val(&p1, "-b:v"), Some("8M"));
    assert_eq!(flag_val(&p1, "-pass"), Some("1"));
    assert_eq!(flag_val(&p1, "-passlogfile"), Some("/tmp/pl"));
    assert!(!p1.contains(&"[outa]".to_string()), "the analysis pass is video-only");
    assert!(p1.contains(&"-f".to_string()) && p1.contains(&"null".to_string()));
    assert_eq!(p1.last().unwrap(), "/dev/null");
    // The null muxer rejects mov/metadata options — they belong to pass 2 only.
    assert!(!p1.contains(&"-movflags".to_string()) && !p1.contains(&"-metadata".to_string()));
    let p2 = build_export_args_phase(
        &timeline,
        &assets,
        "/out/x.mp4",
        &opts,
        PassPhase::Second,
        "/dev/null",
        "/tmp/pl",
    )
    .unwrap();
    assert_eq!(flag_val(&p2, "-pass"), Some("2"));
    assert!(p2.contains(&"[outa]".to_string()));
    assert_eq!(flag_val(&p2, "-movflags"), Some("+faststart"));
    assert_eq!(p2.last().unwrap(), "/out/x.mp4");
}

#[test]
fn filter_pix_fmt_is_threaded_through_the_graph() {
    let opts = ExportOptions {
        video_codec: Some("libx265".into()),
        pix_fmt: Some("yuv420p10le".into()),
        ..Default::default()
    };
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let args = build_export_args(&timeline, &[asset], "/out/x", &opts).unwrap();
    let filter = flag_val(&args, "-filter_complex").unwrap();
    // Both the black base and the per-clip terminal track pix_fmt — no 8-bit
    // bottleneck before the 10-bit encode.
    assert!(filter.matches("yuv420p10le").count() >= 2, "{filter}");
    assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv420p10le"));
}

#[test]
fn export_format_even_clamps_and_forces_opus_48k() {
    let asset = test_asset(vec![video_stream(1921, 1081, 30.0), audio_stream(44_100, 2)]);
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    let opts = ExportOptions {
        resolution: Some((1921, 1081)),
        audio_codec: Some("libopus".into()),
        ..Default::default()
    };
    let fmt = export_format(&timeline, &[asset], &opts);
    assert_eq!((fmt.width, fmt.height), (1920, 1080));
    assert_eq!(fmt.sample_rate, 48_000);
}

#[test]
fn gif_uses_a_palette_and_drops_audio() {
    let opts = ExportOptions {
        container: Container::Gif,
        video_codec: Some("gif".into()),
        include_audio: false,
        ..Default::default()
    };
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    let args = build_export_args(&timeline, &[asset], "/out/x.gif", &opts).unwrap();
    let filter = flag_val(&args, "-filter_complex").unwrap();
    assert!(filter.contains("palettegen=stats_mode=diff"), "{filter}");
    assert!(filter.contains("paletteuse=dither=bayer"), "{filter}");
    assert!(!args.contains(&"[outa]".to_string()), "gif carries no audio");
    assert!(!args.contains(&"-pix_fmt".to_string()), "gif is pal8");
    assert_eq!(flag_val(&args, "-loop"), Some("0"));
}

#[test]
fn audio_bitrate_omitted_for_lossless_codecs() {
    let flac = ExportOptions {
        container: Container::Flac,
        audio_codec: Some("flac".into()),
        flac_compression: Some(8),
        ..Default::default()
    };
    let a = args_of(&flac);
    assert!(!a.contains(&"-b:a".to_string()));
    assert_eq!(flag_val(&a, "-compression_level"), Some("8"));
    let wav = ExportOptions {
        container: Container::Wav,
        audio_codec: Some("pcm_s16le".into()),
        audio_bitrate: Some("192k".into()),
        ..Default::default()
    };
    assert!(!args_of(&wav).contains(&"-b:a".to_string()), "pcm ignores a bitrate");
}

#[test]
fn metadata_title_is_a_single_token() {
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        metadata_title: Some("My Cut = v2".into()),
        ..Default::default()
    };
    let args = args_of(&opts);
    let i = args.iter().position(|a| a == "-metadata").unwrap();
    assert_eq!(args[i + 1], "title=My Cut = v2");
}

#[test]
fn fps_never_emits_dash_r() {
    let opts = ExportOptions {
        video_codec: Some("libx264".into()),
        fps: Some(24.0),
        ..Default::default()
    };
    let args = args_of(&opts);
    assert!(!args.contains(&"-r".to_string()), "fps lives only in the filtergraph");
    assert!(flag_val(&args, "-filter_complex").unwrap().contains("fps=24"));
}

#[test]
fn validate_export_flags_bad_combinations() {
    let webm_x264 = ExportOptions {
        container: Container::Webm,
        video_codec: Some("libx264".into()),
        ..Default::default()
    };
    assert!(!validate_export(&webm_x264, true, true).is_empty());
    let mp4_opus = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        audio_codec: Some("libopus".into()),
        ..Default::default()
    };
    assert!(!validate_export(&mp4_opus, true, true).is_empty());
    let two_pass_no_bitrate = ExportOptions {
        video_codec: Some("libx264".into()),
        rate_control: RateControl::TwoPass,
        ..Default::default()
    };
    assert!(!validate_export(&two_pass_no_bitrate, true, true).is_empty());
    let mp3_no_audio = ExportOptions {
        container: Container::Mp3,
        audio_codec: Some("libmp3lame".into()),
        ..Default::default()
    };
    assert!(!validate_export(&mp3_no_audio, true, false).is_empty());
    let ok = ExportOptions {
        video_codec: Some("libx264".into()),
        audio_codec: Some("aac".into()),
        crf: Some(20),
        ..Default::default()
    };
    assert!(validate_export(&ok, true, true).is_empty());
}

#[test]
fn prores_without_pix_fmt_defaults_to_10bit_422() {
    let opts = ExportOptions {
        container: Container::Mov,
        video_codec: Some("prores_ks".into()),
        prores_profile: Some(3),
        ..Default::default()
    };
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let args = build_export_args(&timeline, &[asset], "/out/x.mov", &opts).unwrap();
    // Both the argv and the graph terminal use 4:2:2 10-bit — no 4:2:0 bottleneck.
    assert_eq!(flag_val(&args, "-pix_fmt"), Some("yuv422p10le"));
    assert!(flag_val(&args, "-filter_complex").unwrap().contains("yuv422p10le"));
    assert!(!args.contains(&"yuv420p".to_string()));
    // The 4444 profiles upgrade to 4:4:4 with alpha.
    let xq = ExportOptions {
        container: Container::Mov,
        video_codec: Some("prores_ks".into()),
        prores_profile: Some(5),
        ..Default::default()
    };
    assert_eq!(flag_val(&args_of_for(&timeline_mov(), &xq), "-pix_fmt"), Some("yuva444p10le"));
}

fn timeline_mov() -> (Timeline, Vec<Asset>) {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    (single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]), vec![asset])
}
fn args_of_for(tl: &(Timeline, Vec<Asset>), opts: &ExportOptions) -> Vec<String> {
    build_export_args(&tl.0, &tl.1, "/out/x.mov", opts).unwrap()
}

#[test]
fn x265_invalid_tune_is_dropped_and_flagged() {
    // `film` is an x264-only tune; x265 would fail to open the encoder.
    let opts = ExportOptions {
        video_codec: Some("libx265".into()),
        tune: Some("film".into()),
        ..Default::default()
    };
    assert!(
        !args_of(&opts).contains(&"-tune".to_string()),
        "an invalid tune must not reach ffmpeg"
    );
    assert!(!validate_export(&opts, true, true).is_empty(), "validation must flag it");
    // A valid x265 tune is kept.
    let ok = ExportOptions {
        video_codec: Some("libx265".into()),
        tune: Some("grain".into()),
        ..Default::default()
    };
    assert_eq!(flag_val(&args_of(&ok), "-tune"), Some("grain"));
    assert!(validate_export(&ok, true, true).is_empty());
}

#[test]
fn cover_fills_the_frame_where_contain_letterboxes_it() {
    let clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0);
    // A vertical delivery of landscape footage: the whole point of `cover`.
    let vertical = ExportFormat {
        width: 1080,
        height: 1920,
        ..ExportFormat::default()
    };

    let contained = video_clip_chain(&clip, &vertical, &ClipFx::default(), false, "c0");
    assert!(contained.contains("force_original_aspect_ratio=decrease"));
    assert!(contained.contains("pad=1080:1920"), "contain must letterbox: {contained}");
    assert!(!contained.contains("crop=1080:1920"));

    let covered = video_clip_chain(
        &clip,
        &ExportFormat {
            fit: Fit::Cover,
            ..vertical
        },
        &ClipFx::default(),
        false,
        "c0",
    );
    assert!(covered.contains("force_original_aspect_ratio=increase"));
    assert!(covered.contains("crop=1080:1920"), "cover must crop: {covered}");
    assert!(!covered.contains("pad="), "cover must not letterbox: {covered}");
}

#[test]
fn the_delivery_format_sets_the_frame_every_render_path_uses() {
    let mut timeline = Timeline::default();
    // No delivery frame: the shape still follows the footage / the 1080p default.
    let fmt = export_format(&timeline, &[], &ExportOptions::default());
    assert_eq!((fmt.width, fmt.height), (1920, 1080));
    assert_eq!(fmt.fit, Fit::Contain);

    timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));
    let fmt = export_format(&timeline, &[], &ExportOptions::default());
    assert_eq!(
        (fmt.width, fmt.height),
        (1080, 1920),
        "the project frame wins over the footage"
    );
    assert_eq!(
        fmt.fit,
        Fit::Cover,
        "and brings its fit, so the preview crops like the export"
    );
}

#[test]
fn an_explicit_export_resolution_still_overrides_the_delivery_format() {
    let timeline = Timeline {
        format: Some(Delivery::new(1080, 1920, Fit::Cover)),
        ..Timeline::default()
    };
    let opts = ExportOptions {
        resolution: Some((3840, 2160)),
        ..Default::default()
    };
    let fmt = export_format(&timeline, &[], &opts);
    assert_eq!((fmt.width, fmt.height), (3840, 2160));
    // The fit is not overridden by a *default* Contain — only an explicit
    // Cover would differ, and Contain is what a one-off resize wants anyway.
    assert_eq!(fmt.fit, Fit::Cover);
}

#[test]
fn a_delivery_frame_is_even_clamped_and_never_zero() {
    let d = Delivery::new(1081, 0, Fit::Cover);
    assert_eq!((d.width, d.height), (1080, 2));
}

/// Footage of 100, 24 and 29.97 fps on one track: the cut an agent reported, whose export with no
/// `fps` came out at whatever rate the first clip happened to have.
fn mixed_rates() -> (Timeline, Vec<Asset>) {
    let assets: Vec<Asset> = [100.0, 24.0, 29.97]
        .iter()
        .map(|fps| test_asset(vec![video_stream(1920, 1080, *fps)]))
        .collect();
    let clips = assets
        .iter()
        .enumerate()
        .map(|(i, a)| make_clip(a.id, 0.0, 2.0, i as f64 * 2.0))
        .collect();
    (single(clips), assets)
}

fn delivered_at(fps: Option<f64>) -> Delivery {
    Delivery::new(1080, 1920, Fit::Cover).with_fps(fps).unwrap()
}

#[test]
fn with_no_delivery_rate_the_export_follows_the_first_clip_as_it_always_did() {
    let (mut timeline, assets) = mixed_rates();
    let opts = ExportOptions::default();
    assert_eq!(export_format(&timeline, &assets, &opts).fps, 100.0);
    assert_eq!(delivery_rate(&timeline, &assets), (100.0, FpsSource::Footage));
    // "First" is the stored order, which is why it is no answer: reorder and the rate moves.
    timeline.tracks[0].clips.reverse();
    assert_eq!(export_format(&timeline, &assets, &opts).fps, 29.97);

    // A delivery frame with no rate of its own leaves it alone; so does no clip with a picture.
    timeline.format = Some(delivered_at(None));
    assert_eq!(export_format(&timeline, &assets, &opts).fps, 29.97);
    let none = export_format_sourced(&Timeline::default(), &[], &opts);
    assert_eq!((none.0.fps, none.1), (30.0, FpsSource::Default));
}

#[test]
fn the_delivery_rate_is_the_default_of_every_render_path_and_an_explicit_one_wins() {
    let (mut timeline, assets) = mixed_rates();
    timeline.format = Some(delivered_at(Some(50.0)));
    let opts = ExportOptions::default();
    assert_eq!(export_format(&timeline, &assets, &opts).fps, 50.0);
    assert_eq!(delivery_rate(&timeline, &assets), (50.0, FpsSource::Delivery));
    // Only the rate moved: the delivery's size and fit are what they were.
    let fmt = export_format(&timeline, &assets, &opts);
    assert_eq!((fmt.width, fmt.height, fmt.fit), (1080, 1920, Fit::Cover));

    // The export options still have the last word.
    let explicit = ExportOptions {
        fps: Some(25.0),
        ..Default::default()
    };
    let (fmt, source) = export_format_sourced(&timeline, &assets, &explicit);
    assert_eq!((fmt.fps, source), (25.0, FpsSource::Options));

    // The export graph's canvas and every clip's `fps` follow it, not a clip's own rate.
    let args = build_export_args(&timeline, &assets, "out.mp4", &opts).unwrap();
    let graph = flag_val(&args, "-filter_complex").unwrap();
    assert!(graph.contains(":r=50:"), "{graph}");
    assert_eq!(graph.matches("fps=50").count(), 3, "{graph}");
    assert!(
        !graph.contains("fps=100") && !graph.contains("fps=24") && !graph.contains("29.97"),
        "{graph}"
    );

    // The same graph is what plays and what a still is placed on.
    let params = playback_params(&timeline, &assets, PlaybackRequest::default());
    assert_eq!(params.fps, 50.0);
    let preview = build_preview_args(&timeline, &assets, 0.0, params.fps, params.max_width, PREVIEW_STREAM_QUALITY).unwrap();
    assert!(flag_val(&preview, "-filter_complex").unwrap().contains("fps=50"));
    assert_eq!(render_geometry(&timeline, &assets, &opts).fps, 50.0);
    // The playback clamp is the stream's own: a 240 fps delivery plays at what a display paints.
    timeline.format = Some(delivered_at(Some(240.0)));
    assert_eq!(
        playback_params(&timeline, &assets, PlaybackRequest::default()).fps,
        MAX_STREAM_FPS
    );
}

#[test]
fn ntsc_delivery_rates_reach_the_graph_as_the_exact_rate() {
    let (mut timeline, assets) = mixed_rates();
    for (typed, exact) in [
        (23.976, 24_000.0 / 1001.0),
        (29.97, 30_000.0 / 1001.0),
        (59.94, 60_000.0 / 1001.0),
    ] {
        timeline.format = Some(delivered_at(Some(typed)));
        let fps = export_format(&timeline, &assets, &ExportOptions::default()).fps;
        assert_eq!(fps, exact, "{typed}");
        // What FFmpeg parses `fps=` and `color=r=` to is the NTSC rational, not 2997/100.
        let rational = crate::clip_timing::Rational::from_fps(fps).unwrap();
        assert_eq!(
            (rational.num, rational.den),
            ((typed * 1001.0 / 1000.0).round() as u32 * 1000, 1001)
        );
    }
}

#[test]
fn a_stored_delivery_rate_out_of_range_is_ignored() {
    let (mut timeline, assets) = mixed_rates();
    for bad in [0.0, -30.0, 0.5, 1e9, f64::NAN, f64::INFINITY] {
        timeline.format = Some(Delivery {
            fps: Some(bad),
            ..delivered_at(None)
        });
        assert_eq!(
            export_format_sourced(&timeline, &assets, &ExportOptions::default()).1,
            FpsSource::Footage,
            "{bad} is not a rate"
        );
        assert_eq!(export_format(&timeline, &assets, &ExportOptions::default()).fps, 100.0);
    }
}

#[test]
fn a_variant_keeps_the_project_rate_unless_it_names_its_own() {
    let (mut timeline, assets) = mixed_rates();
    timeline.format = Some(delivered_at(Some(50.0)));
    let square = Delivery::new(1080, 1080, Fit::Cover);
    assert_eq!(timeline.for_delivery(square).format.unwrap().fps, Some(50.0));
    let own = square.with_fps(Some(24.0)).unwrap();
    assert_eq!(timeline.for_delivery(own).format.unwrap().fps, Some(24.0));
    // A project with no rate hands none on.
    timeline.format = Some(delivered_at(None));
    assert_eq!(timeline.for_delivery(square).format.unwrap().fps, None);

    timeline.format = Some(delivered_at(Some(50.0)));
    let variant = ExportVariant::beside(Path::new("/out/cut.mp4"), square);
    let frame = variant_frame(&timeline, &assets, &variant, &ExportOptions::default()).unwrap();
    assert_eq!(
        (frame.width, frame.height, frame.fps, frame.fps_source),
        (1080, 1080, 50.0, FpsSource::Delivery)
    );
}

#[test]
fn an_export_reports_the_frame_it_writes() {
    let (mut timeline, assets) = mixed_rates();
    let opts = ExportOptions::default();
    let frame = exported_frame(&timeline, &assets, &opts).unwrap();
    assert_eq!(
        (frame.width, frame.height, frame.fps, frame.fps_source),
        (1920, 1080, 100.0, FpsSource::Footage)
    );

    timeline.format = Some(delivered_at(Some(29.97)));
    let frame = exported_frame(&timeline, &assets, &opts).unwrap();
    assert_eq!((frame.width, frame.height), (1080, 1920));
    assert_eq!((frame.fps, frame.fps_source), (30_000.0 / 1001.0, FpsSource::Delivery));

    // What the options changed is what is reported, not what the project asked for.
    let resized = ExportOptions {
        resolution: Some((720, 1280)),
        fps: Some(24.0),
        ..Default::default()
    };
    let frame = exported_frame(&timeline, &assets, &resized).unwrap();
    assert_eq!(
        (frame.width, frame.height, frame.fps, frame.fps_source),
        (720, 1280, 24.0, FpsSource::Options)
    );

    // No picture, no frame to report.
    let audio = ExportOptions {
        container: Container::Mp3,
        ..Default::default()
    };
    assert_eq!(exported_frame(&timeline, &assets, &audio), None);
    assert_eq!(exported_frame(&Timeline::default(), &[], &opts), None);
}

#[test]
fn the_preview_canvas_follows_the_delivery_frame() {
    let timeline = Timeline {
        format: Some(Delivery::new(1080, 1920, Fit::Cover)),
        ..Timeline::default()
    };
    // Capped to max_width, but at the delivery aspect — not the footage's.
    assert_eq!(preview_resolution(&timeline, &[], 540), (540, 960));
    // Already inside the cap: kept as-is.
    assert_eq!(preview_resolution(&timeline, &[], 2000), (1080, 1920));
}

#[test]
fn the_scrubbed_still_crops_like_the_export_when_the_delivery_covers() {
    let canvas = |fit| StillCanvas {
        w: 1080,
        h: 1920,
        fit,
        sf: String::new(),
    };
    let contained = still_clip_chain(
        &Transform::default(),
        &Color::default(),
        &[],
        None,
        &canvas(Fit::Contain),
        None,
        false,
        &StillFades::default(),
        false,
    );
    assert!(contained.contains("force_original_aspect_ratio=decrease"));
    assert!(contained.contains("pad=1080:1920"), "contain letterboxes: {contained}");

    let covered = still_clip_chain(
        &Transform::default(),
        &Color::default(),
        &[],
        None,
        &canvas(Fit::Cover),
        None,
        false,
        &StillFades::default(),
        false,
    );
    assert!(covered.contains("force_original_aspect_ratio=increase"));
    assert!(covered.contains("crop=1080:1920"), "cover crops: {covered}");
    assert!(!covered.contains("pad="), "and never letterboxes: {covered}");
}

#[test]
fn fit_defaults_to_the_historical_letterbox() {
    assert_eq!(ExportOptions::default().fit, Fit::Contain);
    let fmt = export_format(&Timeline::default(), &[], &ExportOptions::default());
    assert_eq!(fmt.fit, Fit::Contain);
    // And an explicit choice reaches the format the graph is built from.
    let fmt = export_format(
        &Timeline::default(),
        &[],
        &ExportOptions {
            fit: Fit::Cover,
            ..Default::default()
        },
    );
    assert_eq!(fmt.fit, Fit::Cover);
}

#[test]
fn the_monitors_effect_chain_is_the_export_chain() {
    assert_eq!(audio_effects_filter(&[]), None);
    let effects = vec![AudioEffect::Highpass { hz: 80.0 }, AudioEffect::Gate { threshold_db: -40.0 }];
    let chain = audio_effects_filter(&effects).expect("a chain");
    assert_eq!(chain, "highpass=f=80,agate=threshold=0.01");
    // The preview decodes through exactly what the export renders, so a clip
    // whose chain is auralized cannot drift from the mix it will become.
    let mut clip = make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0);
    clip.audio = effects;
    let exported = audio_clip_chain(&clip, &ExportFormat::default(), &ClipFx::default(), "stereo", unity_mix());
    assert!(exported.contains(&chain), "export chain {exported} must contain {chain}");
}

// ---- live preview streaming --------------------------------------------

#[test]
fn preview_args_stream_mjpeg_from_the_playhead() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let args = build_preview_args(&timeline, &[asset], 4.0, 24.0, 960, 6).unwrap();

    assert_eq!(args.last().unwrap(), "pipe:1");
    assert_eq!(flag_val(&args, "-f"), Some("image2pipe"));
    assert_eq!(flag_val(&args, "-c:v"), Some("mjpeg"));
    assert_eq!(flag_val(&args, "-q:v"), Some("6"));
    // Video only: audio would just compete with the Web Audio engine that
    // already owns playback sound.
    assert!(args.contains(&"-an".to_string()));
    assert!(args.contains(&"[outv]".to_string()));
    assert!(!args.contains(&"[outa]".to_string()));
    // The graph is the export's, so the composite is identical.
    assert!(args.contains(&"-filter_complex".to_string()));
    // Starting at 4s seeks into the source rather than decoding from zero.
    assert_eq!(flag_val(&args, "-ss"), Some("4"));
}

#[test]
fn preview_args_scale_down_but_keep_aspect() {
    let asset = av_asset(Uuid::new_v4(), 30.0); // 1920x1080
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let assets = [asset];
    assert_eq!(preview_resolution(&timeline, &assets, 960), (960, 540));
    // Already small enough: left alone rather than upscaled.
    assert_eq!(preview_resolution(&timeline, &assets, 4096), (1920, 1080));
}

#[test]
fn preview_honors_mute_and_solo() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = timeline_of(vec![Track {
        muted: true,
        ..video_track(vec![make_clip(asset.id, 0.0, 10.0, 0.0)])
    }]);
    // The only picture is muted, so there is nothing left to composite —
    // the same gate the export applies, so playback matches the render.
    assert!(build_preview_args(&timeline, &[asset], 0.0, 24.0, 960, 6).is_err());
}

#[test]
fn preview_refuses_when_there_is_nothing_to_play() {
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let assets = std::slice::from_ref(&asset);
    // Past the end of the timeline.
    assert!(build_preview_args(&timeline, assets, 10.0, 24.0, 960, 6).is_err());
    // No video at all.
    assert!(build_preview_args(&Timeline::default(), assets, 0.0, 24.0, 960, 6).is_err());
}

#[test]
fn jpeg_frames_split_on_their_markers() {
    let frame = |body: &[u8]| {
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(body);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    };
    let a = frame(&[1, 2, 3]);
    let b = frame(&[4, 5]);

    // One complete frame, consumed exactly.
    assert_eq!(next_jpeg(&a), Some((0, a.len())));

    // Two back to back: the first is returned, the rest left for later.
    let mut both = a.clone();
    both.extend_from_slice(&b);
    let (s, e) = next_jpeg(&both).unwrap();
    assert_eq!(&both[s..e], &a[..]);
    assert_eq!(next_jpeg(&both[e..]), Some((0, b.len())));

    // A partial tail is not a frame yet.
    assert_eq!(next_jpeg(&a[..a.len() - 1]), None);
    assert_eq!(next_jpeg(&[]), None);

    // Leading junk before the start marker is skipped, not mistaken for data.
    let mut noisy = vec![0x00, 0xAB, 0xFF];
    noisy.extend_from_slice(&a);
    let (s, e) = next_jpeg(&noisy).unwrap();
    assert_eq!(&noisy[s..e], &a[..]);
}

/// A stand-in for ffmpeg: `script` run by `sh` with piped stdout / stderr.
/// `exec` in the script so a kill reaches the process holding the pipes.
#[cfg(unix)]
fn fake_stream(script: &str) -> std::process::Child {
    Command::new("sh")
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh")
}

#[cfg(unix)]
fn short_timeouts() -> PreviewTimeouts {
    PreviewTimeouts {
        first: std::time::Duration::from_millis(400),
        stall: std::time::Duration::from_millis(400),
    }
}

/// An ffmpeg that opens and then never produces a frame must fail the
/// stream, not freeze it: the read used to wait on it forever.
#[cfg(unix)]
#[test]
fn a_preview_stream_that_never_produces_a_frame_times_out() {
    let started = std::time::Instant::now();
    let mut sent = Some(0);
    let result = pump_preview(
        fake_stream("echo no frames today >&2; exec sleep 60"),
        0.0,
        24.0,
        short_timeouts(),
        &mut |_| true,
        &mut sent,
    );
    let err = result.expect_err("a silent stream must fail").to_string();
    assert!(err.contains("stalled") && err.contains("first frame"), "{err}");
    assert!(err.contains("no frames today"), "stderr tail is surfaced: {err}");
    assert_eq!(sent, Some(0), "nothing was shown, so a software retry is safe");
    assert!(started.elapsed().as_secs() < 10, "killed promptly: {:?}", started.elapsed());
}

/// Frames that did arrive are delivered, then a stall fails the stream with
/// the count intact (so the caller knows replaying would repeat them).
#[cfg(unix)]
#[test]
fn a_preview_stream_that_stalls_midway_delivers_what_it_had_then_fails() {
    let mut frames = 0;
    let mut sent = Some(0);
    let result = pump_preview(
        fake_stream(r"printf '\377\330one\377\331\377\330two\377\331'; exec sleep 60"),
        1.0,
        1000.0,
        short_timeouts(),
        &mut |f| {
            assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8]);
            frames += 1;
            true
        },
        &mut sent,
    );
    let err = result.expect_err("stall").to_string();
    assert!(err.contains("stalled") && !err.contains("first frame"), "{err}");
    assert_eq!((frames, sent), (2, Some(2)));
}

/// A callback that declines a frame still stops playback cleanly, and a
/// stream that simply ends is not an error.
#[cfg(unix)]
#[test]
fn a_preview_stream_stops_on_request_and_ends_cleanly() {
    let mut sent = Some(0);
    let mut frames = 0;
    pump_preview(
        fake_stream(r"printf '\377\330a\377\331\377\330b\377\331'; exec sleep 60"),
        0.0,
        1000.0,
        short_timeouts(),
        &mut |_| {
            frames += 1;
            false
        },
        &mut sent,
    )
    .expect("a requested stop is not a failure");
    assert_eq!(frames, 1);

    let mut frames = 0;
    pump_preview(
        fake_stream(r"printf '\377\330a\377\331'"),
        0.0,
        1000.0,
        short_timeouts(),
        &mut |_| {
            frames += 1;
            true
        },
        &mut sent,
    )
    .expect("a stream that finishes is not a failure");
    assert_eq!(frames, 1);
}

/// A frame that arrives behind the wall-clock schedule says by how much; one that is early or
/// on time says nothing. A caller tells a stream that cannot keep up from a hiccup by whether
/// that number keeps growing.
#[cfg(unix)]
#[test]
fn a_frame_reports_how_far_behind_its_slot_it_arrived() {
    let timeouts = PreviewTimeouts {
        first: std::time::Duration::from_secs(5),
        stall: std::time::Duration::from_secs(5),
    };
    let mut delays = Vec::new();
    let mut sent = Some(0);
    pump_preview(
        fake_stream(r"printf '\377\330a\377\331'; sleep 0.3; printf '\377\330b\377\331'"),
        0.0,
        1000.0,
        timeouts,
        &mut |f| {
            delays.push(f.late);
            true
        },
        &mut sent,
    )
    .expect("ends cleanly");
    assert_eq!(delays.len(), 2);
    assert_eq!(delays[0], 0.0, "the first frame sets the clock: {delays:?}");
    assert!(
        delays[1] > 0.2 && delays[1] < 2.0,
        "the second was a third of a second behind a 1 ms slot: {delays:?}"
    );

    // Paced faster than it renders is not late: the pacing sleeps until the slot. The slots are
    // long enough that a loaded runner oversleeping one cannot reach the next.
    let mut delays = Vec::new();
    pump_preview(
        fake_stream(r"printf '\377\330a\377\331\377\330b\377\331\377\330c\377\331'"),
        0.0,
        4.0,
        timeouts,
        &mut |f| {
            delays.push(f.late);
            true
        },
        &mut sent,
    )
    .expect("ends cleanly");
    assert!(delays.iter().all(|l| *l < 0.05), "{delays:?}");
}

/// A stream in parts plays them one after the other on one clock: each frame is on the frame of
/// the stream its part starts at, and the next part is started once this one has shown its first
/// frame, not before.
#[cfg(unix)]
#[test]
fn a_stream_in_parts_plays_them_in_turn_and_starts_each_next_one_ahead() {
    let scripts = [
        r"printf '\377\330a\377\331\377\330b\377\331'",
        r"printf '\377\330c\377\331'",
        r"printf '\377\330d\377\331\377\330e\377\331'",
    ];
    let firsts = [0u64, 2, 3];
    let spawned = std::cell::RefCell::new(Vec::new());
    let shown = std::cell::Cell::new(0u64);
    let mut spawn = |i: usize| -> Result<Option<PreviewRun>> {
        spawned.borrow_mut().push((i, shown.get()));
        Ok(scripts
            .get(i)
            .map(|s| PreviewRun::start(fake_stream(s), firsts[i], false, GraphScript(None))))
    };
    let mut frames = Vec::new();
    let mut sent = None;
    pump_runs(
        &mut spawn,
        10.0,
        1000.0,
        short_timeouts(),
        &mut |f| {
            frames.push((f.time, f.jpeg[2]));
            shown.set(shown.get() + 1);
            true
        },
        &mut sent,
    )
    .expect("ends cleanly");
    assert_eq!(
        frames,
        vec![(10.0, b'a'), (10.001, b'b'), (10.002, b'c'), (10.003, b'd'), (10.004, b'e')]
    );
    assert_eq!(sent, Some(5));
    // Part 1 after part 0's first frame, part 2 after part 1's (which is the stream's third), and
    // the end asked for once.
    assert_eq!(*spawned.borrow(), vec![(0, 0), (1, 1), (2, 3), (3, 4)]);
}

/// A part that dies before its first frame having asked for hardware decode is started again in
/// software (nothing of it was shown, so nothing is replayed); one that asked for none fails the
/// stream, as does one that dies after showing frames.
#[cfg(unix)]
#[test]
fn a_part_whose_hardware_decode_fails_starts_again_and_any_other_failure_ends_the_stream() {
    let before = HWACCEL_OK.load(std::sync::atomic::Ordering::Relaxed);
    let attempts = std::cell::Cell::new(0);
    let mut spawn = |i: usize| -> Result<Option<PreviewRun>> {
        Ok(match i {
            0 => Some(PreviewRun::start(
                fake_stream(r"printf '\377\330a\377\331'"),
                0,
                false,
                GraphScript(None),
            )),
            1 => {
                attempts.set(attempts.get() + 1);
                let hw = attempts.get() == 1;
                let script = if hw {
                    "echo no device >&2; exit 1"
                } else {
                    r"printf '\377\330b\377\331'"
                };
                Some(PreviewRun::start(fake_stream(script), 1, hw, GraphScript(None)))
            }
            _ => None,
        })
    };
    let mut frames = Vec::new();
    let mut sent = None;
    pump_runs(
        &mut spawn,
        0.0,
        1000.0,
        short_timeouts(),
        &mut |f| {
            frames.push(f.jpeg[2]);
            true
        },
        &mut sent,
    )
    .expect("the software run plays");
    HWACCEL_OK.store(before, std::sync::atomic::Ordering::Relaxed);
    assert_eq!((frames, attempts.get()), (vec![b'a', b'b'], 2));

    let mut spawn = |i: usize| -> Result<Option<PreviewRun>> {
        Ok(match i {
            0 => Some(PreviewRun::start(
                fake_stream(r"printf '\377\330a\377\331'"),
                0,
                false,
                GraphScript(None),
            )),
            1 => Some(PreviewRun::start(
                fake_stream("echo bad graph >&2; exit 1"),
                1,
                false,
                GraphScript(None),
            )),
            _ => None,
        })
    };
    let err = pump_runs(&mut spawn, 0.0, 1000.0, short_timeouts(), &mut |_| true, &mut sent)
        .expect_err("a software part that fails ends the stream")
        .to_string();
    assert!(err.contains("preview stream failed") && err.contains("bad graph"), "{err}");
}

fn params_for(fps: f64, request: PlaybackRequest) -> PlaybackParams {
    let asset = test_asset(vec![video_stream(1920, 1080, fps)]);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    playback_params(&timeline, &[asset], request)
}

#[test]
fn playback_runs_at_the_timelines_own_rate() {
    for fps in [12.0, 23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0] {
        let p = params_for(fps, PlaybackRequest::default());
        assert_eq!(p.fps, fps, "a {fps} fps cut plays at {fps}");
    }
    // Past what a display paints there is nothing to gain: the cap.
    assert_eq!(params_for(120.0, PlaybackRequest::default()).fps, MAX_STREAM_FPS);
    assert_eq!(params_for(240.0, PlaybackRequest::default()).fps, MAX_STREAM_FPS);
    // A cut with no footage to read a rate from plays at the engine's default, not at nothing.
    let empty = timeline_of(vec![video_track(vec![])]);
    assert_eq!(playback_params(&empty, &[], PlaybackRequest::default()).fps, 30.0);
}

#[test]
fn a_stated_rate_wins_within_bounds_and_an_unusable_one_is_not_stated() {
    let say = |fps| {
        params_for(
            60.0,
            PlaybackRequest {
                fps: Some(fps),
                max_width: None,
            },
        )
        .fps
    };
    assert_eq!(say(30.0), 30.0);
    assert_eq!(say(29.97), 29.97);
    assert_eq!(say(15.0), 15.0);
    assert_eq!(say(0.2), MIN_STREAM_FPS);
    assert_eq!(say(500.0), MAX_STREAM_FPS);
    // The cut's own rate stands in for a number that is not one.
    let own = params_for(25.0, PlaybackRequest::default()).fps;
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -24.0] {
        assert_eq!(
            params_for(
                25.0,
                PlaybackRequest {
                    fps: Some(bad),
                    max_width: None
                }
            )
            .fps,
            own,
            "{bad}"
        );
    }
}

#[test]
fn the_rate_is_the_cut_as_it_renders_not_a_muted_tracks_footage() {
    let slow = test_asset(vec![video_stream(1920, 1080, 24.0)]);
    let fast = test_asset(vec![video_stream(1920, 1080, 60.0)]);
    let mut muted = video_track(vec![make_clip(slow.id, 0.0, 5.0, 0.0)]);
    muted.muted = true;
    let timeline = timeline_of(vec![muted, video_track(vec![make_clip(fast.id, 0.0, 5.0, 0.0)])]);
    let p = playback_params(&timeline, &[slow, fast], PlaybackRequest::default());
    assert_eq!(p.fps, 60.0);
}

#[test]
fn the_stream_width_is_the_one_asked_for_within_bounds() {
    let width = |w| params_for(30.0, PlaybackRequest { fps: None, max_width: w }).max_width;
    assert_eq!(width(None), PREVIEW_STREAM_WIDTH);
    for w in [480, 960, 1280, 1440, 1920] {
        assert_eq!(width(Some(w)), w);
    }
    assert_eq!(width(Some(0)), MIN_STREAM_WIDTH);
    assert_eq!(width(Some(10)), MIN_STREAM_WIDTH);
    assert_eq!(width(Some(100_000)), MAX_STREAM_WIDTH);
    // What the page asks for is bucketed to the steps of `preview-size.ts`, and the top of
    // them is the engine's own bound.
    assert_eq!(MAX_STREAM_WIDTH, 1920);
}

#[test]
fn the_requested_width_and_rate_reach_the_stream_graph() {
    let asset = av_asset(Uuid::new_v4(), 10.0);
    let timeline = single(vec![make_clip(asset.id, 0.0, 10.0, 0.0)]);
    let assets = [asset];
    let params = playback_params(
        &timeline,
        &assets,
        PlaybackRequest {
            fps: None,
            max_width: Some(1440),
        },
    );
    assert_eq!((params.fps, params.max_width), (30.0, 1440));
    let args = build_preview_args(&timeline, &assets, 0.0, params.fps, params.max_width, PREVIEW_STREAM_QUALITY).unwrap();
    let graph = flag_val(&args, "-filter_complex").unwrap();
    assert!(graph.contains("1440") && graph.contains("fps=30"), "{graph}");
    let slim = build_preview_args(&timeline, &assets, 0.0, 60.0, 960, PREVIEW_STREAM_QUALITY).unwrap();
    let graph = flag_val(&slim, "-filter_complex").unwrap();
    assert!(graph.contains("fps=60") && !graph.contains("1440"), "{graph}");
    // A source narrower than the request is not scaled up to it.
    let narrow = test_asset(vec![video_stream(640, 360, 30.0)]);
    let tl = single(vec![make_clip(narrow.id, 0.0, 5.0, 0.0)]);
    let args = build_preview_args(&tl, &[narrow], 0.0, 30.0, 1920, PREVIEW_STREAM_QUALITY).unwrap();
    let graph = flag_val(&args, "-filter_complex").unwrap();
    assert!(
        graph.contains("640") && !graph.contains("1920"),
        "narrow source keeps its width: {graph}"
    );
}

#[test]
fn a_smart_cropped_clip_crops_before_the_fit_so_cover_has_nothing_left_to_take() {
    let asset = av_asset(Uuid::new_v4(), 30.0); // 1920x1080
    let mut clip = make_clip(asset.id, 0.0, 5.0, 0.0);
    // What `smart_crop` writes for a 9:16 delivery, pulled left of centre.
    let map = SalienceMap::new(4, 2, vec![1.0, 1.0, 0.01, 0.01, 1.0, 1.0, 0.01, 0.01]);
    let crop = map.crop_for(1920, 1080, 1080.0 / 1920.0).expect("crops");
    clip.transform.crop_left = crop.left;
    clip.transform.crop_right = crop.right;

    let fmt = ExportFormat {
        width: 1080,
        height: 1920,
        fit: Fit::Cover,
        ..ExportFormat::default()
    };
    let chain = video_clip_chain(&clip, &fmt, &ClipFx::default(), false, "c0");
    let cropped = chain.find("crop=w=iw*").expect("the smart crop is in the graph");
    let scaled = chain.find("scale=1080:1920").expect("the fit is in the graph");
    assert!(
        cropped < scaled,
        "the crop must pick the window before the fit scales it: {chain}"
    );
    // The window is off-centre — a plain Cover would have taken the middle.
    assert!(chain.contains(&format!("x=iw*{}", crop.left)), "{chain}");
    assert!(crop.left < 0.3, "the subject is left of centre: {crop:?}");
}

#[test]
fn a_blurred_fill_puts_the_fitted_picture_over_a_blurred_cover_copy() {
    assert_eq!(
        blur_fill(1080, 1920, "", ":flags=bicubic", false, "bf0"),
        "split[bf0b][bf0f];\
             [bf0b]scale=134:240:force_original_aspect_ratio=increase:flags=bilinear,crop=134:240,\
             boxblur=8:2,scale=1080:1920:flags=bilinear[bf0bb];\
             [bf0f]scale=1080:1920:force_original_aspect_ratio=decrease:flags=bicubic[bf0ff];\
             [bf0bb][bf0ff]overlay=(W-w)/2:(H-h)/2"
    );
    // A tiny frame keeps a thumbnail whose chroma plane takes a radius of 1.
    assert!(blur_fill(40, 24, ":force_divisible_by=2", "", false, "x")
        .contains("scale=4:4:force_original_aspect_ratio=increase:flags=bilinear,crop=4:4,boxblur=1:2"));
    // An HDR picture composes in 10 bits ahead of its tone-map, on even sizes.
    let fmt = ExportFormat {
        width: 1080,
        height: 1920,
        fit: Fit::Blur,
        ..ExportFormat::default()
    };
    let hlg = ClipFx {
        hdr: Some(Hdr::Hlg),
        ..ClipFx::default()
    };
    let deep = video_clip_chain(&make_clip(Uuid::new_v4(), 0.0, 5.0, 0.0), &fmt, &hlg, false, "c0");
    assert!(
        deep.contains("decrease:force_divisible_by=2[bfc0ff];[bfc0bb][bfc0ff]overlay=(W-w)/2:(H-h)/2:format=yuv420p10,"),
        "{deep}"
    );
    assert!(
        deep.find(":format=yuv420p10").unwrap() < deep.find("zscale").unwrap(),
        "{deep}"
    );
    let asset = av_asset(Uuid::new_v4(), 30.0);
    let graph = |clip: Clip, fit: Fit| {
        let mut tl = single(vec![clip]);
        tl.format = Some(Delivery::new(1080, 1920, fit));
        let args = build_export_args(&tl, std::slice::from_ref(&asset), "out.mp4", &ExportOptions::default()).unwrap();
        let at = args.iter().position(|a| a == "-filter_complex").unwrap() + 1;
        args[at].clone()
    };
    let plain = make_clip(asset.id, 0.0, 5.0, 0.0);
    let g = graph(plain.clone(), Fit::Blur);
    assert!(
        g.contains("[0:v]trim=start=0:end=5.066666666666666,setpts=PTS-STARTPTS+0/TB,split[bfc0b][bfc0f];"),
        "{g}"
    );
    assert!(g.contains("[bfc0bb][bfc0ff]overlay=(W-w)/2:(H-h)/2,setsar=1,"), "{g}");
    assert!(!g.contains("pad="), "the fill is the frame: {g}");
    // Contain is what it was, and a clip placed by its transform gets Contain's chain.
    let contain = graph(plain.clone(), Fit::Contain);
    assert!(contain.contains("force_original_aspect_ratio=decrease,pad=1080:1920") && !contain.contains("boxblur"));
    let mut pip = plain.clone();
    pip.transform.scale = 0.5;
    let (blurred, contained) = (graph(pip.clone(), Fit::Blur), graph(pip, Fit::Contain));
    assert_eq!(blurred, contained);
    // Nor does a see-through picture (the fill would show through it as a halo), a 360
    // reframe, or a picture of the frame's own shape: Contain's graph, byte for byte.
    let mut keyed = plain.clone();
    keyed.effects.push(VideoEffect::ChromaKey {
        color: "green".into(),
        similarity: 0.1,
        blend: 0.0,
    });
    let mut masked = plain.clone();
    masked.mask = Some(crate::model::Mask::default());
    let mut reframed = plain.clone();
    reframed.reframe = Some(crate::model::Reframe::new(crate::model::Projection::Flat));
    for clip in [keyed, masked, reframed] {
        assert_eq!(graph(clip.clone(), Fit::Blur), graph(clip, Fit::Contain));
    }
    let fill_of = |picture, alpha| takes_blur_fill(&plain, picture, (1080, 1920), alpha);
    assert!(fill_of(Some((1920, 1080)), false));
    assert!(!fill_of(Some((1920, 1080)), true), "an alpha source");
    assert!(!fill_of(Some((540, 960)), false), "the frame's own shape");
    assert!(!fill_of(None, false) && !fill_of(Some((0, 0)), false), "an unknown shape");
    let wide = |fit| {
        let mut tl = single(vec![plain.clone()]);
        tl.format = Some(Delivery::new(1920, 1080, fit));
        build_export_args(&tl, std::slice::from_ref(&asset), "out.mp4", &ExportOptions::default()).unwrap()
    };
    assert_eq!(wide(Fit::Blur), wide(Fit::Contain), "nothing to fill");
    // The scrubbed still: the same fill, labelled per layer.
    let mut tl = single(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]);
    tl.tracks.push(video_track(vec![make_clip(asset.id, 0.0, 5.0, 0.0)]));
    tl.format = Some(Delivery::new(1080, 1920, Fit::Blur));
    let out = StillOutput::JpegPipe { quality: 2 };
    let args = build_still_args(
        &tl,
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        1.0,
        540,
        None,
        &out,
    )
    .unwrap();
    let g = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
    assert!(g.contains("split[bf0b][bf0f]") && g.contains("split[bf1b][bf1f]"), "{g}");
    assert!(
        g.contains("scale=66:120:force_original_aspect_ratio=increase:flags=bilinear,crop=66:120,boxblur=4:2,scale=540:960"),
        "{g}"
    );
    // An alpha source takes no fill, in the export or the still: Contain's graphs.
    let mut sticker = asset.clone();
    sticker.streams[0].pix_fmt = Some("yuva420p".into());
    let both = |fit| {
        let mut tl = single(vec![make_clip(sticker.id, 0.0, 5.0, 0.0)]);
        tl.format = Some(Delivery::new(1080, 1920, fit));
        let assets = std::slice::from_ref(&sticker);
        let opts = ExportOptions::default();
        (
            build_export_args(&tl, assets, "out.mp4", &opts).unwrap(),
            build_still_args(&tl, assets, &opts, 1.0, 540, None, &out).unwrap(),
        )
    };
    assert_eq!(both(Fit::Blur), both(Fit::Contain));
}

#[test]
fn variant_files_land_beside_the_base_named_by_shape() {
    let v = ExportVariant::beside(Path::new("/renders/cut.mp4"), Delivery::new(1080, 1920, Fit::Cover));
    assert_eq!(v.output, PathBuf::from("/renders/cut-9x16.mp4"));
    let v = ExportVariant::beside(Path::new("cut"), Delivery::new(1080, 1080, Fit::Cover));
    assert_eq!(v.output, PathBuf::from("cut-1x1"));
    let v = ExportVariant::beside(Path::new("/r/my.cut.mov"), Delivery::new(1920, 1080, Fit::Contain));
    assert_eq!(v.output, PathBuf::from("/r/my.cut-16x9.mov"));
}

#[test]
fn each_variant_renders_its_own_frame_and_its_own_crop() {
    let asset = av_asset(Uuid::new_v4(), 30.0); // 1920x1080
    let mut clip = make_clip(asset.id, 0.0, 5.0, 0.0);
    // Cut 9:16 with a left-leaning smart crop in the transform, and a
    // different framing kept for 1:1.
    clip.transform.crop_left = 0.05;
    clip.transform.crop_right = 0.6336;
    clip.framings.push(crate::model::Framing {
        aspect_w: 1,
        aspect_h: 1,
        crop_left: 0.3,
        crop_right: 0.2625,
        crop_top: 0.0,
        crop_bottom: 0.0,
    });
    let mut timeline = timeline_of(vec![video_track(vec![clip])]);
    timeline.format = Some(Delivery::new(1080, 1920, Fit::Cover));

    let graph_for = |d: Delivery| {
        let framed = timeline.for_delivery(d);
        let opts = ExportOptions {
            resolution: Some((d.width, d.height)),
            fit: d.fit,
            ..ExportOptions::default()
        };
        build_export_args(&framed, std::slice::from_ref(&asset), "out.mp4", &opts)
            .unwrap()
            .join(" ")
    };
    let vertical = graph_for(Delivery::new(1080, 1920, Fit::Cover));
    assert!(vertical.contains("scale=1080:1920"), "{vertical}");
    assert!(
        vertical.contains("x=iw*0.05"),
        "the project frame keeps its own crop: {vertical}"
    );

    let square = graph_for(Delivery::new(1080, 1080, Fit::Cover));
    assert!(square.contains("scale=1080:1080"), "{square}");
    assert!(
        square.contains("x=iw*0.3"),
        "the 1:1 delivery wears the 1:1 framing: {square}"
    );
    assert!(!square.contains("x=iw*0.05"), "{square}");

    // A shape nothing was framed for keeps the crop it has, and the
    // delivery's own fit.
    let wide = graph_for(Delivery::new(1920, 1080, Fit::Contain));
    assert!(wide.contains("scale=1920:1080"), "{wide}");
    assert!(wide.contains("x=iw*0.05"), "{wide}");
}

/// End to end: two files from one cut, each at its delivery frame. Run with
/// `cargo test -p kerf-core --no-default-features -- --ignored renders_every`.
#[test]
#[ignore = "needs the ffmpeg binary"]
fn renders_every_variant_to_its_own_file() {
    let dir = std::env::temp_dir().join(format!("kerf-variants-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=640x360:rate=30:duration=2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success(), "could not synthesize test media");

    let mut asset = av_asset(Uuid::new_v4(), 2.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(640, 360, 30.0)];
    let timeline = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 2.0, 0.0)])]);

    let base = dir.join("cut.mp4");
    let variants = vec![
        ExportVariant::beside(&base, Delivery::new(180, 320, Fit::Cover)),
        ExportVariant::beside(&base, Delivery::new(200, 200, Fit::Cover)),
    ];
    let mut ticks = Vec::new();
    let VariantsRun {
        status, completed: done, ..
    } = render_variants(
        &timeline,
        &[asset],
        &variants,
        &ExportOptions::default(),
        false,
        &mut |p| ticks.push(p),
        &|| false,
    )
    .expect("render");
    assert_eq!((status, done), (RenderStatus::Completed, 2));
    for (v, (w, h)) in variants.iter().zip([(180, 320), (200, 200)]) {
        let probed = probe(&v.output).expect("probe the variant");
        let video = probed.streams.iter().find(|s| s.kind == StreamKind::Video).expect("video");
        assert_eq!((video.width, video.height), (Some(w), Some(h)), "{}", v.output.display());
    }
    assert!(ticks.iter().any(|p| p.variant == 1), "progress names the second variant");
    assert!(ticks.iter().all(|p| p.total == 2 && (0.0..=1.0).contains(&p.fraction)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// End to end against the real `ffmpeg` binary: synthesize a 16:9 shot whose
/// only content sits in the left third, and check the sampler finds it and
/// the 9:16 crop keeps it — the case a centre crop gets wrong, and the whole
/// reason smart crop exists. Not part of the normal (binary-free) run:
/// `cargo test -p kerf-core --no-default-features -- --ignored samples_a_real`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn samples_a_real_shot_and_frames_its_subject() {
    let dir = std::env::temp_dir().join(format!("kerf-salience-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("left.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=360x240:rate=30:duration=3"])
        .args(["-f", "lavfi", "-i", "color=c=black:s=1280x720:rate=30:duration=3"])
        .args(["-filter_complex", "[1][0]overlay=x=80:y=240"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success(), "could not synthesize test media");

    let map = salience_map_cancellable(&media, 0.0, 3.0, &|| false).expect("sample");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!((map.cols, map.rows), (SALIENCE_COLS, SALIENCE_ROWS));
    assert!(map.cells.iter().any(|c| *c > 0.0), "the map is empty");

    let crop = map.crop_for(1280, 720, 1080.0 / 1920.0).expect("crops");
    assert!(crop.offset < 0.0, "the subject is on the left: {crop:?}");
    // The subject spans x = 80..440 of 1280, i.e. 0.0625..0.344 — a centre
    // crop (0.342..0.658) would miss it entirely.
    assert!(crop.left < 0.0625 && 1.0 - crop.right > 0.344, "{crop:?}");
}

/// End to end against the real `ffmpeg` binary: synthesize a clip, play two
/// seconds of a two-track timeline out of it, and check real JPEGs arrive at
/// roughly the requested rate. Not part of the normal (binary-free) run:
/// `cargo test -p kerf-core --no-default-features -- --ignored streams_real_frames`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn streams_real_frames_from_a_composited_timeline() {
    let dir = std::env::temp_dir().join(format!("kerf-preview-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=640x360:rate=30:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success(), "could not synthesize test media");

    let mut asset = av_asset(Uuid::new_v4(), 6.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(640, 360, 30.0), audio_stream(48_000, 2)];
    // Two video tracks, so the run exercises real compositing (overlay of a
    // second layer over the first) rather than a single passthrough.
    let base = make_clip(asset.id, 0.0, 6.0, 0.0);
    let mut over = make_clip(asset.id, 2.0, 4.0, 1.0);
    over.transform.scale = 0.5;
    let timeline = timeline_of(vec![video_track(vec![base]), video_track(vec![over])]);

    let mut frames: Vec<PreviewFrame> = Vec::new();
    let started = std::time::Instant::now();
    let result = stream_preview(&timeline, &[asset], 1.0, 24.0, &mut |f| {
        frames.push(f);
        frames.len() < 24 // stop after a second's worth
    });
    let elapsed = started.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    result.expect("stream");

    assert_eq!(frames.len(), 24);
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8], "frame {i} is not a JPEG");
        assert_eq!(&f.jpeg[f.jpeg.len() - 2..], &[0xFF, 0xD9], "frame {i} is truncated");
        assert!(f.jpeg.len() > 1000, "frame {i} is suspiciously small");
        // Timeline times advance from the requested start at the frame rate.
        assert!((f.time - (1.0 + i as f64 / 24.0)).abs() < 1e-9);
    }
    // Paced to real time rather than dumped as fast as it renders: a second
    // of frames takes about a second (with slack for the graph starting up).
    assert!(elapsed.as_secs_f64() > 0.8, "playback ran ahead of real time: {elapsed:?}");
}

/// The size a JPEG says it is: the first start-of-frame segment's height and width.
fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    while at + 9 < jpeg.len() {
        if jpeg[at] != 0xFF {
            return None;
        }
        let marker = jpeg[at + 1];
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        if matches!(marker, 0xC0..=0xC2) {
            let h = u16::from_be_bytes([jpeg[at + 5], jpeg[at + 6]]) as u32;
            let w = u16::from_be_bytes([jpeg[at + 7], jpeg[at + 8]]) as u32;
            return Some((w, h));
        }
        at += 2 + len;
    }
    None
}

/// End to end against the real `ffmpeg` binary: a 60 fps cut plays at 60 fps and at the width
/// the preview asked for, a 30 fps cut at 30 and, with nothing asked, the default width.
/// `cargo test -p kerf-core --no-default-features -- --ignored streams_at_the_timelines_rate`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn streams_at_the_timelines_rate_and_the_requested_width() {
    let dir = std::env::temp_dir().join(format!("kerf-preview-rate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let play = |fps: u32, request: PlaybackRequest, frames_wanted: usize| {
        let media = dir.join(format!("src{fps}.mp4"));
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", &format!("testsrc2=size=1280x720:rate={fps}:duration=4")])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-g", "1", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success(), "could not synthesize test media");
        let mut asset = av_asset(Uuid::new_v4(), 4.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(1280, 720, fps as f64)];
        let timeline = single(vec![make_clip(asset.id, 0.0, 4.0, 0.0)]);
        let mut frames: Vec<PreviewFrame> = Vec::new();
        let began = std::time::Instant::now();
        let mut first = None;
        stream_preview_with(&timeline, &[asset], 0.5, request, &mut |f| {
            first.get_or_insert_with(|| began.elapsed());
            frames.push(f);
            frames.len() < frames_wanted
        })
        .expect("stream");
        let span = (began.elapsed() - first.unwrap()).as_secs_f64();
        (frames, span)
    };

    let (frames, span) = play(
        60,
        PlaybackRequest {
            fps: None,
            max_width: Some(640),
        },
        60,
    );
    assert_eq!(frames.len(), 60);
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(jpeg_size(&f.jpeg), Some((640, 360)), "frame {i}");
        assert!((f.time - (0.5 + i as f64 / 60.0)).abs() < 1e-9, "frame {i} is at {}", f.time);
    }
    // 59 gaps of 1/60 s: a second of video takes about a second, not the 2.5 s it took at 24.
    assert!(span > 0.85 && span < 1.4, "60 frames at 60 fps took {span:.2}s");

    let (frames, span) = play(30, PlaybackRequest::default(), 30);
    assert_eq!(frames.len(), 30);
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(jpeg_size(&f.jpeg), Some((960, 540)), "frame {i} at the default width");
        assert!((f.time - (0.5 + i as f64 / 30.0)).abs() < 1e-9, "frame {i} is at {}", f.time);
    }
    assert!(span > 0.85 && span < 1.4, "30 frames at 30 fps took {span:.2}s");

    // A request past the source's own width is not scaled up to it.
    let (frames, _) = play(
        30,
        PlaybackRequest {
            fps: Some(24.0),
            max_width: Some(1920),
        },
        4,
    );
    assert_eq!(jpeg_size(&frames[0].jpeg), Some((1280, 720)));
    assert!(
        (frames[1].time - frames[0].time - 1.0 / 24.0).abs() < 1e-9,
        "a stated rate overrides the cut's"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The graph features most likely to break only at runtime — a looped still
/// input, a crossfade, a colour grade, a video effect and a `drawtext`
/// overlay — all in one playable timeline. Unit tests can only check the
/// argv; this checks ffmpeg actually accepts it.
/// `cargo test -p kerf-core --no-default-features -- --ignored plays_a_timeline_with_everything`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn plays_a_timeline_with_everything_on_it() {
    let dir = std::env::temp_dir().join(format!("kerf-preview-rich-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let still = dir.join("card.png");
    let run = |args: Vec<String>| {
        let ok = command(&ffmpeg_bin()).args(&args).status_bounded().expect("run ffmpeg");
        assert!(ok.success(), "ffmpeg failed for {args:?}");
    };
    let s = |v: &str| v.to_string();
    run(vec![
        s("-hide_banner"),
        s("-loglevel"),
        s("error"),
        s("-y"),
        s("-f"),
        s("lavfi"),
        s("-i"),
        s("testsrc=size=640x360:rate=30:duration=6"),
        s("-f"),
        s("lavfi"),
        s("-i"),
        s("sine=frequency=440:duration=6"),
        s("-c:v"),
        s("libx264"),
        s("-pix_fmt"),
        s("yuv420p"),
        s("-c:a"),
        s("aac"),
        s("-shortest"),
        media.to_string_lossy().into_owned(),
    ]);
    run(vec![
        s("-hide_banner"),
        s("-loglevel"),
        s("error"),
        s("-y"),
        s("-f"),
        s("lavfi"),
        s("-i"),
        s("color=c=red:size=640x360"),
        s("-frames:v"),
        s("1"),
        still.to_string_lossy().into_owned(),
    ]);

    let mut video = av_asset(Uuid::new_v4(), 6.0);
    video.path = media.to_string_lossy().into_owned();
    video.streams = vec![video_stream(640, 360, 30.0), audio_stream(48_000, 2)];
    let mut card = av_asset(Uuid::new_v4(), 5.0);
    card.path = still.to_string_lossy().into_owned();
    card.streams = vec![image_stream(640, 360)];

    // A graded, blurred clip; a still crossfading in over it; a title on top.
    let mut base = make_clip(video.id, 0.0, 6.0, 0.0);
    base.color = Color {
        brightness: 0.05,
        contrast: 1.2,
        saturation: 0.8,
        gamma: 1.0,
        temperature: 0.3,
    };
    base.effects = vec![VideoEffect::Blur { sigma: 2.0 }];
    let mut over = make_clip(card.id, 0.0, 3.0, 1.5);
    over.transition_in = Some(crate::model::Transition {
        kind: TransitionKind::Crossfade,
        duration: 0.5,
    });
    over.transform.scale = 0.6;
    let mut timeline = timeline_of(vec![video_track(vec![base]), video_track(vec![over])]);
    timeline.overlays = vec![TextOverlay::new("Rough cut", 0.0, 6.0)];

    let mut frames = 0usize;
    let result = stream_preview(&timeline, &[video, card], 1.0, 24.0, &mut |f| {
        assert_eq!(&f.jpeg[..2], &[0xFF, 0xD8]);
        frames += 1;
        frames < 12
    });
    let _ = std::fs::remove_dir_all(&dir);
    result.expect("stream");
    assert_eq!(frames, 12);
}

/// A real vertical export of landscape footage, both ways. `Cover` is only
/// worth anything if the delivered frame actually has picture at the top and
/// bottom instead of black, which no argv assertion can tell you.
/// `cargo test -p kerf-core --no-default-features -- --ignored cover_really_fills`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn cover_really_fills_a_vertical_frame() {
    let dir = std::env::temp_dir().join(format!("kerf-fit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30:duration=2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    let mut asset = av_asset(Uuid::new_v4(), 2.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(1920, 1080, 30.0)];
    let timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);

    // Mean luma of the top 100 rows of the first frame: black bars sit near
    // 16 (limited-range black), real picture well above it.
    let top_luma = |file: &Path| -> f64 {
        let raw = dir.join("top.raw");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(file)
            .args([
                "-vf",
                "crop=1080:100:0:0",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
            ])
            .arg(&raw)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        let bytes = std::fs::read(&raw).expect("raw");
        bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
    };

    let opts = |fit| ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        resolution: Some((1080, 1920)),
        fit,
        include_audio: false,
        ..Default::default()
    };
    let contained = dir.join("contain.mp4");
    let covered = dir.join("cover.mp4");
    render_with(&timeline, &[asset.clone()], &contained, &opts(Fit::Contain)).expect("contain export");
    render_with(&timeline, &[asset], &covered, &opts(Fit::Cover)).expect("cover export");

    let (dark, bright) = (top_luma(&contained), top_luma(&covered));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(dark < 30.0, "contain should letterbox the top, got luma {dark}");
    assert!(bright > 60.0, "cover should fill the top with picture, got luma {bright}");
}

/// A piecewise position expression contains commas, and the filtergraph
/// parser treats an unquoted comma as the end of the filter — so a clip with
/// position keyframes, or an *animated text overlay*, made ffmpeg fail the
/// whole render with `No such filter`. Nothing caught it: every unit test
/// above asserts on the graph string, and the graph string looked right.
///
/// Both are on the default path — the Text overlays style chips animate an
/// overlay's opacity, which puts it on the keyframed branch — so this renders
/// one of each and only asks that ffmpeg accept the graph.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored animated_positions`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn animated_positions_survive_the_filtergraph_parser() {
    let dir = std::env::temp_dir().join(format!("kerf-anim-parse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=320x180:rate=30:duration=2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    let mut asset = av_asset(Uuid::new_v4(), 2.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(320, 180, 30.0)];

    let mut clip = make_clip(asset.id, 0.0, 2.0, 0.0);
    clip.keyframes = vec![
        crate::model::Keyframe {
            time: 0.0,
            scale: 1.0,
            pos_x: -0.2,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        },
        crate::model::Keyframe {
            time: 2.0,
            scale: 1.0,
            pos_x: 0.2,
            pos_y: 0.0,
            rotation: 0.0,
            opacity: 1.0,
            easing: Default::default(),
        },
    ];
    let mut timeline = single(vec![clip]);
    // An overlay animated the way the Text overlays style chips animate one.
    let mut overlay = TextOverlay::new("Kerf".to_string(), 0.0, 2.0);
    overlay.keyframes = vec![
        crate::model::TextKeyframe {
            time: 0.0,
            pos_x: 0.5,
            pos_y: 0.5,
            opacity: 0.0,
        },
        crate::model::TextKeyframe {
            time: 1.0,
            pos_x: 0.5,
            pos_y: 0.8,
            opacity: 1.0,
        },
    ];
    timeline.overlays = vec![overlay];

    let out = dir.join("anim.mp4");
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        include_audio: false,
        ..Default::default()
    };
    let rendered = render_with(&timeline, &[asset], &out, &opts);
    let ok = rendered.is_ok() && out.exists();
    let err = rendered.err().map(|e| e.to_string()).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "an animated clip and overlay must render: {err}");
}

/// A mask is only worth anything if a lower track really shows through it,
/// which no assertion on the graph string can establish. Black over white,
/// masked to a hard-edged ellipse: the middle must come out black and the
/// corners white.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored a_mask_really`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_mask_really_lets_the_track_below_show_through() {
    let dir = std::env::temp_dir().join(format!("kerf-mask-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mk = |name: &str, color: &str| -> PathBuf {
        let out = dir.join(name);
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", &format!("color=c={color}:s=320x180:r=30:d=2")])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&out)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        out
    };
    let (white, black) = (mk("white.mp4", "white"), mk("black.mp4", "black"));
    let mut lower = av_asset(Uuid::new_v4(), 2.0);
    lower.path = white.to_string_lossy().into_owned();
    lower.streams = vec![video_stream(320, 180, 30.0)];
    let mut upper = av_asset(Uuid::new_v4(), 2.0);
    upper.path = black.to_string_lossy().into_owned();
    upper.streams = vec![video_stream(320, 180, 30.0)];

    let mut top = make_clip(upper.id, 0.0, 2.0, 0.0);
    top.mask = Some(crate::model::Mask {
        shape: crate::model::MaskShape::Ellipse,
        x: 0.5,
        y: 0.5,
        width: 0.5,
        height: 0.5,
        feather: 0.0,
        inverted: false,
    });
    let timeline = timeline_of(vec![
        video_track(vec![make_clip(lower.id, 0.0, 2.0, 0.0)]),
        video_track(vec![top]),
    ]);

    let out = dir.join("masked.mp4");
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        include_audio: false,
        ..Default::default()
    };
    render_with(&timeline, &[lower, upper], &out, &opts).expect("export");

    // Mean luma of a 20x20 patch at (x, y) of the first frame.
    let patch = |x: u32, y: u32| -> f64 {
        let raw = dir.join("patch.raw");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(&out)
            .args(["-vf", &format!("crop=20:20:{x}:{y}"), "-frames:v", "1"])
            .args(["-f", "rawvideo", "-pix_fmt", "gray"])
            .arg(&raw)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        let bytes = std::fs::read(&raw).expect("raw");
        bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
    };
    let (middle, corner) = (patch(150, 80), patch(0, 0));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(middle < 60.0, "inside the mask the upper clip is kept, got {middle}");
    assert!(corner > 180.0, "outside it the lower track shows through, got {corner}");
}

#[test]
fn a_seek_is_spelled_to_the_microsecond() {
    assert_eq!(seek_arg(1.0006), "1.000600");
    assert_eq!(seek_arg(7.0), "7.000000");
    assert_eq!(seek_arg(0.0), "0.000000");
    assert_eq!(seek_arg(12.3456789), "12.345679");
}

/// A frame at 1.0006 s is returned by `-ss 1.0006` and skipped by the
/// spelling `1.001`: on a fine time base the still has to say the time to the
/// microsecond. The clip is a 10 fps ramp (frame `n` is luma `16 + 8n`) in
/// which every frame from the 10th on sits 0.6 ms late, written to an mp4
/// with a 1/10000 time base (the encoder's too, which would otherwise round
/// every timestamp to a tenth of a second) so the offset survives the container.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored fine_time_base`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_still_picks_the_frame_at_a_fine_time_base_second() {
    let dir = std::env::temp_dir().join(format!("kerf-fine-ss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("ramp.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=64x64:r=10:d=2,format=yuv420p,geq=lum='16+8*N':cb=128:cr=128,\
                 settb=1/10000,setpts='PTS+if(gte(N,10),6,0)'",
        ])
        .args(["-c:v", "libx264", "-crf", "8", "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p"])
        .args([
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
            "1/10000",
            "-video_track_timescale",
            "10000",
        ])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    // Which ramp frame a decode at `-ss <ss>` returns, read off its first luma sample.
    let frame_at = |ss: &str| -> u32 {
        let out = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-ss", ss, "-i"])
            .arg(&media)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"])
            .stdin(Stdio::null())
            .output()
            .expect("run ffmpeg");
        assert!(out.status.success() && !out.stdout.is_empty());
        ((f64::from(out.stdout[0]) - 16.0) / 8.0).round() as u32
    };
    // The premise, on this ffmpeg: the exact time lands on frame 10, the
    // millisecond rounding of it skips to frame 11.
    assert_eq!(frame_at("1.000600"), 10, "the fixture has no frame at 1.0006");
    assert_eq!(frame_at("1.001"), 11, "this ffmpeg does not skip a frame on `-ss 1.001`");

    let mut asset = av_asset(Uuid::new_v4(), 2.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(64, 64, 10.0)];
    // Source 1.0006 at timeline 0: the still at t=0 is that source second.
    let timeline = single(vec![make_clip(asset.id, 1.0006, 1.9, 0.0)]);
    let args = build_still_args(
        &timeline,
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        0.0,
        64,
        None,
        &StillOutput::RgbPipe,
    )
    .expect("still args");
    let rgb = run_still(
        &timeline,
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        0.0,
        64,
        None,
        &StillOutput::RgbPipe,
        &|| false,
    )
    .expect("render the still");
    let _ = std::fs::remove_dir_all(&dir);
    // Full-range grey of luma 16 + 8n is 255 * 8n / 219: 93 for frame 10, 102 for 11.
    let grey = f64::from(rgb[0]);
    let frame = (grey * 219.0 / 255.0 / 8.0).round() as u32;
    assert_eq!(
        frame, 10,
        "the still showed ramp frame {frame} (grey {grey}), not the one at 1.0006 s"
    );
    assert!(args.join(" ").contains("-ss 1.000600 -i "), "{args:?}");
}

/// A VAAPI export's argv with the device taken out: `-vaapi_device` dropped, the upload
/// left as a plain `format=nv12`, and the encoder swapped for lossless FFV1 into `out`.
/// What is left is the graph ffmpeg would be handed, shape for shape, minus the one filter
/// that needs a GPU.
fn vaapi_argv_without_the_device(args: &[String], out: &str) -> Vec<String> {
    let mut rewritten = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-vaapi_device" => i += 2,
            "-c:v" => {
                i += 1;
                while i + 1 < args.len() && !matches!(args[i].as_str(), "-c:a" | "-an" | "-movflags") {
                    i += 1;
                }
                rewritten.extend(["-c:v", "ffv1"].map(String::from));
            }
            _ => {
                rewritten.push(args[i].replace(",hwupload", ""));
                i += 1;
            }
        }
    }
    *rewritten.last_mut().unwrap() = out.to_string();
    rewritten
}

/// MD5 of every decoded video frame of `path`, as 8-bit planar YUV.
fn frames_md5(path: &Path) -> String {
    let out = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-pix_fmt", "yuv420p", "-f", "md5", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The VAAPI export's graph, rendered: the `vhw` pad and `format=nv12` tail it ends in are
/// accepted by ffmpeg and carry the same picture as the graph without them, with and
/// without titles on top. (The upload itself needs a GPU; see `needs a real machine`.)
///
/// `cargo test -p kerf-core --no-default-features -- --ignored vaapi_graph`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_vaapi_graph_carries_the_same_picture_as_the_plain_one() {
    let scratch = Scratch::new("vaapi-graph");
    let media = scratch.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30:duration=1"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());
    let mut asset = av_asset(Uuid::new_v4(), 1.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(640, 360, 30.0)];
    for titled in [false, true] {
        let mut timeline = single(vec![make_clip(asset.id, 0.0, 1.0, 0.0)]);
        if titled {
            let mut title = TextOverlay::new("VAAPI", 0.0, 1.0);
            title.size = 0.3;
            timeline.overlays = vec![title];
        }
        let render = |codec: &str, name: &str| {
            let opts = ExportOptions {
                video_codec: Some(codec.into()),
                include_audio: false,
                ..Default::default()
            };
            let args = build_export_args(&timeline, std::slice::from_ref(&asset), "/out/x.mp4", &opts).unwrap();
            let out = scratch.join(name);
            let args = vaapi_argv_without_the_device(&args, &out.to_string_lossy());
            assert_eq!(
                args.iter().any(|a| a.contains("[vhw]format=nv12[outv]")),
                codec == "h264_vaapi"
            );
            let run = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error"])
                .args(&args)
                .output()
                .expect("run ffmpeg");
            assert!(
                run.status.success(),
                "{codec} titled={titled}: {}",
                String::from_utf8_lossy(&run.stderr)
            );
            out
        };
        let vaapi = render("h264_vaapi", &format!("vaapi-{titled}.mkv"));
        let plain = render("libx264", &format!("plain-{titled}.mkv"));
        assert_eq!(frames_md5(&vaapi), frames_md5(&plain), "titled={titled}");
    }
}

/// The options each AMF and VAAPI export spells are ones the encoder knows, with values in
/// its range — checked where ffmpeg checks them, when the encoder is opened, which is
/// before the missing GPU is noticed. A QP past `qp_i`'s 51 (or an AV1 one past 255) is the
/// failure this guards: the old 1:1 mapping put `-qp_i 60` on the line for a CRF of 60.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored encoder_options`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_hardware_encoder_options_are_ones_the_encoder_accepts() {
    let scratch = Scratch::new("hw-options");
    let listed = command(&ffmpeg_bin())
        .args(["-hide_banner", "-encoders"])
        .output()
        .expect("run ffmpeg");
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    let mut asset = av_asset(Uuid::new_v4(), 1.0);
    asset.path = scratch.join("src.mp4").to_string_lossy().into_owned();
    asset.streams = vec![video_stream(640, 360, 30.0)];
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=30:d=1"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&asset.path)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());
    let timeline = single(vec![make_clip(asset.id, 0.0, 1.0, 0.0)]);
    let mut tried = 0;
    for codec in ["h264_amf", "hevc_amf", "av1_amf", "h264_vaapi", "hevc_vaapi", "av1_vaapi"] {
        if !listed.lines().any(|l| l.split_whitespace().nth(1) == Some(codec)) {
            continue;
        }
        for (rate_control, crf) in [
            (RateControl::Crf, Some(0)),
            (RateControl::Crf, Some(20)),
            (RateControl::Crf, Some(51)),
            (RateControl::Crf, Some(63)),
            (RateControl::Crf, Some(u32::MAX)),
            (RateControl::Lossless, None),
        ] {
            let opts = ExportOptions {
                video_codec: Some(codec.into()),
                rate_control,
                crf,
                include_audio: false,
                ..Default::default()
            };
            let args = build_export_args(&timeline, std::slice::from_ref(&asset), "/out/x.mp4", &opts).unwrap();
            let args: Vec<String> = args
                .iter()
                .enumerate()
                .filter(|(i, a)| a.as_str() != "-vaapi_device" && (*i == 0 || args[i - 1] != "-vaapi_device"))
                .map(|(_, a)| a.replace(",hwupload", ""))
                .collect();
            let out = scratch.join("out.mp4");
            let run = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error"])
                .args(&args[..args.len() - 1])
                .arg(&out)
                .output()
                .expect("run ffmpeg");
            let stderr = String::from_utf8_lossy(&run.stderr);
            for refusal in [
                "Unrecognized option",
                "not found",
                "out of range",
                "Error setting option",
                "Error applying",
            ] {
                assert!(!stderr.contains(refusal), "{codec} {rate_control:?} crf {crf:?}: {stderr}");
            }
            tried += 1;
        }
    }
    // The pinned Linux and Windows builds carry them; the macOS one has neither family.
    assert!(
        tried > 0 || cfg!(target_os = "macos"),
        "this ffmpeg has none of the AMF or VAAPI encoders"
    );
}

/// With no GPU the probe still finishes, names only encoders it knows, and finds no VAAPI
/// device; with one, whatever it names it verified. Never hangs.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored hw_encoders_probe`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_hw_encoders_probe_finishes_and_names_only_what_it_knows() {
    let started = Instant::now();
    let found = hw_encoders();
    assert!(started.elapsed() < std::time::Duration::from_secs(60));
    for encoder in found {
        assert!(
            HW_ENCODER_CANDIDATES.contains(&encoder.as_str()) || VAAPI_ENCODER_CANDIDATES.contains(&encoder.as_str()),
            "{encoder}"
        );
    }
    let vaapi = found.iter().any(|e| enc_family(e) == EncFamily::Vaapi);
    assert_eq!(
        vaapi,
        hw_probe().vaapi_device.is_some(),
        "a VAAPI encoder is verified on a device, and only then"
    );
    if !Path::new("/dev/dri").exists() {
        assert!(!vaapi);
    }
}

/// A bare `%` used to be a drawtext configuration error that silently
/// blanked the whole overlay — ffmpeg still exits 0, so nothing above can
/// tell a rendered "50% OFF" from a plain black frame. Render both and
/// compare mean luma.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored percent_overlay`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn percent_overlay_is_not_silently_blanked() {
    let dir = std::env::temp_dir().join(format!("kerf-percent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=1"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    let mut asset = av_asset(Uuid::new_v4(), 1.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(640, 360, 30.0)];

    let mut timeline = single(vec![make_clip(asset.id, 0.0, 1.0, 0.0)]);
    let mut overlay = TextOverlay::new("50% OFF", 0.0, 1.0);
    overlay.size = 0.3;
    timeline.overlays = vec![overlay];

    let out = dir.join("percent.mp4");
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        include_audio: false,
        ..Default::default()
    };
    render_with(&timeline, &[asset], &out, &opts).expect("export");

    // Mean luma of the first frame.
    let mean_luma = |path: &Path| -> f64 {
        let raw = dir.join("frame.raw");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .arg("-i")
            .arg(path)
            .args(["-frames:v", "1"])
            .args(["-f", "rawvideo", "-pix_fmt", "gray"])
            .arg(&raw)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        let bytes = std::fs::read(&raw).expect("raw");
        bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
    };
    let (blank, text) = (mean_luma(&media), mean_luma(&out));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(blank < 2.0, "control should be plain black, got {blank}");
    assert!(
        text > blank + 5.0,
        "\"50% OFF\" should be visible over black: {text} vs blank {blank}"
    );
}

/// A fade on a clip that does not start the timeline. The chain's `setpts`
/// has already put the frames on the timeline, so a fade timed from 0
/// blacked out the whole of a later clip that had a fade-out and played a
/// later clip's fade-in before the clip existed.
#[test]
fn a_later_clips_fades_are_timed_from_where_it_sits() {
    let asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    let a = make_clip(asset.id, 0.0, 5.0, 0.0);
    let mut b = make_clip(asset.id, 0.0, 4.0, 5.0);
    b.fade_in = 0.5;
    b.fade_out = 1.0;
    let g = graph_of(&single(vec![a, b]), &[asset]);
    assert!(g.contains("fade=t=in:st=5:d=0.5,fade=t=out:st=8:d=1,"), "{g}");
    // Audio is re-based to the clip before `adelay` moves it, so its fades stay clip-local.
    assert!(g.contains("afade=t=in:st=0:d=0.5,afade=t=out:st=3:d=1,"), "{g}");
}

/// The same, rendered: the middle of a later clip with a fade-out must be
/// picture, not black.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored later_clip_fade`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_later_clip_fade_out_does_not_black_out_the_clip() {
    let dir = std::env::temp_dir().join(format!("kerf-fade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("gray.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=320x180:r=25:d=10"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    let mut asset = av_asset(Uuid::new_v4(), 10.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(320, 180, 25.0)];
    let a = make_clip(asset.id, 0.0, 3.0, 0.0);
    let mut b = make_clip(asset.id, 3.0, 7.0, 3.0);
    b.fade_in = 0.5;
    b.fade_out = 1.0;
    let timeline = single(vec![a, b]);

    let out = dir.join("faded.mp4");
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        include_audio: false,
        ..Default::default()
    };
    render_with(&timeline, &[asset], &out, &opts).expect("export");

    let luma_at = |t: f64| -> f64 {
        let raw = dir.join(format!("frame-{t}.raw"));
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-ss", &t.to_string()])
            .arg("-i")
            .arg(&out)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray"])
            .arg(&raw)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        let bytes = std::fs::read(&raw).expect("raw");
        bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
    };
    let (first, fading_in, middle, tail) = (luma_at(1.0), luma_at(3.1), luma_at(4.5), luma_at(6.9));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(first > 100.0, "first clip is plain gray: {first}");
    assert!(middle > 100.0, "a later clip's fade-out must not black it out: {middle}");
    assert!(
        fading_in < middle - 30.0,
        "the fade-in starts dark at the cut: {fading_in} vs {middle}"
    );
    assert!(tail < middle - 30.0, "the fade-out darkens the tail: {tail} vs {middle}");
}

/// A slide and a push look identical in the graph builder's assertions —
/// both put the incoming clip a frame away and walk it home — and differ
/// only in whether the *outgoing* clip moves. Nothing above can tell them
/// apart, so this renders both and looks at the pixels.
///
/// The outgoing source carries a white stripe down its left edge. Halfway
/// through the transition a slide has left it where it was (the stripe is
/// still on screen); a push has carried it half a frame left (the stripe has
/// gone with it).
///
/// `cargo test -p kerf-core --no-default-features -- --ignored slide_and_push`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn slide_and_push_really_move_the_picture() {
    let dir = std::env::temp_dir().join(format!("kerf-motion-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    // Outgoing: black with a white stripe down the left edge. Incoming: gray.
    let striped = dir.join("striped.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=4"])
        .args(["-vf", "drawbox=x=0:y=0:w=64:h=360:color=white:t=fill"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&striped)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());
    let plain = dir.join("gray.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=c=gray:s=640x360:r=30:d=4"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&plain)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    let mut out_asset = av_asset(Uuid::new_v4(), 4.0);
    out_asset.path = striped.to_string_lossy().into_owned();
    out_asset.streams = vec![video_stream(640, 360, 30.0)];
    let mut in_asset = av_asset(Uuid::new_v4(), 4.0);
    in_asset.path = plain.to_string_lossy().into_owned();
    in_asset.streams = vec![video_stream(640, 360, 30.0)];

    // Mean luma of the leftmost 32 px, one frame into the middle of the
    // transition (the cut is at 2.0s, the transition runs a second).
    let stripe_luma = |file: &Path| -> f64 {
        let raw = dir.join("stripe.raw");
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-ss", "2.5"])
            .arg("-i")
            .arg(file)
            .args(["-vf", "crop=32:360:0:0", "-frames:v", "1"])
            .args(["-f", "rawvideo", "-pix_fmt", "gray"])
            .arg(&raw)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        let bytes = std::fs::read(&raw).expect("raw");
        bytes.iter().map(|b| *b as f64).sum::<f64>() / bytes.len() as f64
    };

    let render = |kind: TransitionKind, name: &str| -> PathBuf {
        let a = make_clip(out_asset.id, 0.0, 2.0, 0.0);
        let mut b = make_clip(in_asset.id, 0.0, 2.0, 2.0);
        b.transition_in = Some(crate::model::Transition { kind, duration: 1.0 });
        let timeline = single(vec![a, b]);
        let out = dir.join(name);
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            include_audio: false,
            ..Default::default()
        };
        render_with(&timeline, &[out_asset.clone(), in_asset.clone()], &out, &opts).expect("export");
        out
    };
    let slid = stripe_luma(&render(TransitionKind::SlideLeft, "slide.mp4"));
    let pushed = stripe_luma(&render(TransitionKind::PushLeft, "push.mp4"));
    let _ = std::fs::remove_dir_all(&dir);

    assert!(slid > 180.0, "a slide leaves the outgoing clip where it was, got luma {slid}");
    assert!(
        pushed < 60.0,
        "a push carries the outgoing clip out of frame, got luma {pushed}"
    );
}

/// The delivery frame is the point of the whole feature: with it set and
/// **no** export resolution at all, a 16:9 source must still render a filled
/// 1080x1920 file — the same thing the preview showed while cutting.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored delivery_format_renders`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn delivery_format_renders_the_project_frame_without_an_export_resolution() {
    let dir = std::env::temp_dir().join(format!("kerf-delivery-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let media = dir.join("src.mp4");
    let ok = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc=size=1920x1080:rate=30:duration=2"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&media)
        .status_bounded()
        .expect("run ffmpeg");
    assert!(ok.success());

    let mut asset = av_asset(Uuid::new_v4(), 2.0);
    asset.path = media.to_string_lossy().into_owned();
    asset.streams = vec![video_stream(1920, 1080, 30.0)];
    let timeline = Timeline {
        format: Some(Delivery::new(1080, 1920, Fit::Cover)),
        ..single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)])
    };

    let out = dir.join("vertical.mp4");
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        include_audio: false,
        ..Default::default()
    };
    render_with(&timeline, &[asset], &out, &opts).expect("delivery export");

    let probed = command(&ffprobe_bin())
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(&out)
        .output()
        .expect("run ffprobe");
    let dims = String::from_utf8_lossy(&probed.stdout).trim().to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(dims, "1080,1920", "the project frame decides the file's shape");
}

/// The file's own frame rate, as `ffprobe` reads it: `50/1`, `30000/1001`.
fn probed_rate(path: &Path) -> String {
    let probed = command(&ffprobe_bin())
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=r_frame_rate", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&probed.stdout).trim().to_string()
}

/// Footage of 24 and 25 fps in one cut: with no rate on the delivery frame the file takes the first
/// clip's, a delivery rate decides it instead (an NTSC one as the exact `30000/1001`, which FFmpeg
/// would otherwise read as `2997/100`), and the export's own `fps` still wins.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored delivery_rate_decides`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn the_delivery_rate_decides_the_files_frame_rate_in_mixed_footage() {
    let dir = std::env::temp_dir().join(format!("kerf-delivery-rate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = |name: &str, rate: u32| {
        let media = dir.join(name);
        let ok = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "lavfi", "-i", &format!("testsrc=size=640x360:rate={rate}:duration=2")])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&media)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(ok.success());
        let mut asset = av_asset(Uuid::new_v4(), 2.0);
        asset.path = media.to_string_lossy().into_owned();
        asset.streams = vec![video_stream(640, 360, f64::from(rate))];
        asset
    };
    let (slow, quick) = (source("slow.mp4", 24), source("quick.mp4", 25));
    let cut = |format: Option<Delivery>| Timeline {
        format,
        ..single(vec![make_clip(slow.id, 0.0, 1.0, 0.0), make_clip(quick.id, 0.0, 1.0, 1.0)])
    };
    let assets = [slow.clone(), quick.clone()];
    let opts = ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        include_audio: false,
        ..Default::default()
    };
    let rate_of = |name: &str, format: Option<Delivery>, opts: &ExportOptions| {
        let out = dir.join(name);
        render_with(&cut(format), &assets, &out, opts).expect("export");
        probed_rate(&out)
    };
    let frame = Delivery::new(640, 360, Fit::Contain);
    let at = |fps: f64| Some(frame.with_fps(Some(fps)).unwrap());

    let explicit = ExportOptions {
        fps: Some(25.0),
        ..opts.clone()
    };
    let seen = [
        ("footage", rate_of("footage.mp4", Some(frame), &opts)),
        ("none", rate_of("none.mp4", None, &opts)),
        ("50", rate_of("fifty.mp4", at(50.0), &opts)),
        ("29.97", rate_of("ntsc.mp4", at(29.97), &opts)),
        ("explicit", rate_of("explicit.mp4", at(50.0), &explicit)),
    ];
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        seen,
        [
            ("footage", "24/1".to_string()),
            ("none", "24/1".to_string()),
            ("50", "50/1".to_string()),
            ("29.97", "30000/1001".to_string()),
            ("explicit", "25/1".to_string()),
        ]
    );
}

// ---- rotation, colour and frame rate at probe time ----------------------

fn probe_json(video: &str) -> ProbeResult {
    let json = format!(
        r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"hevc",{video}}}],"format":{{"duration":"10.0"}}}}"#
    );
    probe_from_json(serde_json::from_str(&json).expect("json"), None)
}

#[test]
fn the_pixel_format_is_recorded_for_video_and_tells_alpha_apart() {
    let plain = probe_json(r#""width":1280,"height":720,"pix_fmt":"yuv420p""#);
    assert_eq!(plain.streams[0].pix_fmt.as_deref(), Some("yuv420p"));
    assert_eq!(plain.streams[0].has_alpha(), Some(false));
    let alpha = probe_json(r#""width":1280,"height":720,"pix_fmt":"yuva420p""#);
    assert_eq!(alpha.streams[0].has_alpha(), Some(true));
    // Absent from the JSON (an old ffprobe, an audio-only file): not known,
    // which is not the same as "no alpha".
    let unknown = probe_json(r#""width":1280,"height":720"#);
    assert_eq!(unknown.streams[0].pix_fmt, None);
    assert_eq!(unknown.streams[0].has_alpha(), None);
}

#[test]
fn the_probe_reads_a_policy_off_two_reds() {
    // The tag did not reach the conversion: the same red either way.
    assert_eq!(policy_from_reds(193, 193), Some(CompositeColorPolicy::FixedBt601));
    assert_eq!(policy_from_reds(194, 192), Some(CompositeColorPolicy::FixedBt601));
    // It did: BT.709 reads this picture's red about 18 levels higher.
    assert_eq!(policy_from_reds(211, 193), Some(CompositeColorPolicy::BottomLayerTag));
    // Neither: not a difference the probe knows how to read.
    assert_eq!(policy_from_reds(200, 193), None);
}

#[test]
#[ignore = "needs ffmpeg"]
#[allow(clippy::print_stderr)]
fn the_composite_policy_is_measured_from_the_graph_not_guessed() {
    // Whatever this ffmpeg is, the probe must read *something* off the real
    // graph (a `None` would mean it fell back to `Unknown`).
    let (ffmpeg, ffprobe) = (ffmpeg_bin(), ffprobe_bin());
    let deadline = Instant::now() + POLICY_PROBE_TIMEOUT;
    let started = Instant::now();
    let measured = measure_composite_color_policy(&ffmpeg, &ffprobe, POLICY_PROBE_TIMEOUT);
    eprintln!(
        "composite colour policy of {ffmpeg}: {measured:?} in {:?} (reds tagged {:?} / untagged {:?})",
        started.elapsed(),
        probe_composite_red(&ffmpeg, &ffprobe, true, deadline),
        probe_composite_red(&ffmpeg, &ffprobe, false, deadline)
    );
    assert!(measured.is_some(), "the probe could not read the still graph's matrix");
    assert_eq!(composite_color_policy(), measured.unwrap());
}

#[test]
#[ignore = "needs ffmpeg"]
#[cfg(unix)]
fn a_probe_whose_tag_does_not_survive_measures_nothing() {
    // Standing in for an `ffprobe` that reports no matrix for the tagged clip
    // (`echo` prints its arguments, which is not `bt709`): the measurement is
    // refused rather than read as "FFmpeg 6", which is what two identical
    // clips would say on any build.
    let ffmpeg = ffmpeg_bin();
    assert!(measure_composite_color_policy(&ffmpeg, "echo", POLICY_PROBE_TIMEOUT).is_none());
    assert!(measure_composite_color_policy(&ffmpeg, &ffprobe_bin(), POLICY_PROBE_TIMEOUT).is_some());
}

/// `crop` rounds its window to the chroma grid of the pixel format it is given,
/// and [`crate::model::pix_fmt_subsampling`] says which grid that is: measured
/// here by cropping a 33x35 window out of a picture in each format and reading
/// the size ffmpeg reports for what came out.
#[test]
#[ignore = "needs ffmpeg"]
fn the_subsampling_table_matches_ffmpegs_crop() {
    let bin = ffmpeg_bin();
    let supported = command(&bin)
        .args(["-hide_banner", "-v", "error", "-pix_fmts"])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let has = |fmt: &str| supported.lines().any(|l| l.split_whitespace().nth(1) == Some(fmt));
    let mut checked = 0;
    for fmt in [
        "yuv420p",
        "yuvj420p",
        "yuv420p9le",
        "yuv420p10le",
        "yuv420p12le",
        "yuv420p16le",
        "nv12",
        "nv21",
        "p010le",
        "p016le",
        "yuv422p",
        "yuvj422p",
        "yuv422p10le",
        "yuv422p16le",
        "nv16",
        "p210le",
        "yuv440p",
        "yuvj440p",
        "yuv440p10le",
        "yuv411p",
        "yuv410p",
        "yuv444p",
        "yuvj444p",
        "yuv444p9le",
        "yuv444p16le",
        "nv24",
        "nv42",
        "p410le",
        "gray",
        "gray10le",
        "gray16le",
        "gbrp",
        "gbrp10le",
        "gbrp16le",
        "rgb24",
        "bgr24",
        "rgb0",
        "bgr0",
        "0rgb",
        "0bgr",
        "rgb48le",
        "bgr48le",
    ] {
        if !has(fmt) {
            continue;
        }
        let want = crate::model::pix_fmt_subsampling(fmt).unwrap_or_else(|| panic!("{fmt} is not in the table"));
        let output = command(&bin)
            .args(["-hide_banner", "-v", "info", "-f", "lavfi", "-i"])
            .arg(format!("color=c=gray:s=128x128:r=1,format={fmt}"))
            .args(["-vf", "crop=w=33:h=35:x=1:y=1,showinfo", "-frames:v", "1", "-f", "null", "-"])
            .stdin(Stdio::null())
            .output()
            .expect("run ffmpeg");
        let log = String::from_utf8_lossy(&output.stderr);
        let size = log
            .lines()
            .filter(|l| l.contains("Parsed_showinfo"))
            .find_map(|l| {
                let rest = l.split(" s:").nth(1)?;
                let dims = rest.split_whitespace().next()?;
                let (w, h) = dims.split_once('x')?;
                Some((w.parse::<i64>().ok()?, h.parse::<i64>().ok()?))
            })
            .unwrap_or_else(|| panic!("{fmt}: no showinfo size in {log}"));
        assert_eq!(
            size,
            (want.round_w(33), want.round_h(35)),
            "{fmt}: crop gave {size:?}, the table says {want:?}"
        );
        // ...and the origin is rounded to the same grid: a 32x32 window at (1, 1)
        // is the window at the rounded origin, and where an axis is not rounded
        // it is *not* the window one pixel back.
        let md5_at = |x: i64, y: i64| -> String {
            let output = command(&bin)
                .args(["-hide_banner", "-v", "error", "-f", "lavfi", "-i"])
                .arg(format!("testsrc2=s=128x128:r=1,format={fmt}"))
                .args([
                    "-vf",
                    &format!("crop=w=32:h=32:x={x}:y={y}"),
                    "-frames:v",
                    "1",
                    "-f",
                    "framemd5",
                    "-",
                ])
                .stdin(Stdio::null())
                .output()
                .expect("run ffmpeg");
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .find(|l| !l.starts_with('#') && !l.trim().is_empty())
                .and_then(|l| l.split(',').next_back())
                .map(|m| m.trim().to_string())
                .unwrap_or_else(|| panic!("{fmt}: no frame md5 for crop at ({x}, {y})"))
        };
        assert_eq!(
            md5_at(1, 1),
            md5_at(want.round_w(1), want.round_h(1)),
            "{fmt}: the origin (1, 1) is not rounded to {want:?}"
        );
        if want.log2_w == 0 {
            assert_ne!(md5_at(1, 1), md5_at(0, 1), "{fmt}: x was rounded, the table says it is not");
        }
        if want.log2_h == 0 {
            assert_ne!(md5_at(1, 1), md5_at(1, 0), "{fmt}: y was rounded, the table says it is not");
        }
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} formats were available to check");
}

#[test]
fn a_probe_that_cannot_run_measures_nothing() {
    let t = std::time::Duration::from_secs(5);
    assert!(measure_composite_color_policy("/nonexistent/ffmpeg", "/nonexistent/ffprobe", t).is_none());
    // A binary that runs and fails.
    #[cfg(unix)]
    assert!(measure_composite_color_policy("false", "false", t).is_none());
}

#[test]
#[cfg(unix)]
fn a_child_that_never_exits_is_killed_at_the_deadline() {
    let started = Instant::now();
    let out = run_piped_until(
        Command::new("sh").args(["-c", "sleep 30"]),
        b"input".to_vec(),
        Instant::now() + std::time::Duration::from_millis(300),
    );
    assert!(out.is_none());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    // One that finishes in time hands back its output, with the input fed in.
    let out = run_piped_until(
        &mut Command::new("cat"),
        b"input".to_vec(),
        Instant::now() + std::time::Duration::from_secs(10),
    );
    assert_eq!(out.as_deref(), Some(&b"input"[..]));
    // ...and one that fails hands back nothing.
    let out = run_piped_until(
        &mut Command::new("false"),
        Vec::new(),
        Instant::now() + std::time::Duration::from_secs(10),
    );
    assert!(out.is_none());
}

/// A wrapper that does not `exec` leaves the real program holding the pipes past the kill, and past its own exit: the
/// run must hand the caller back at its deadline all the same, never join a thread a descendant keeps alive.
#[test]
#[cfg(unix)]
fn a_descendant_holding_the_pipes_does_not_hold_the_caller() {
    let held = std::time::Duration::from_secs(6);
    let bound = std::time::Duration::from_millis(300);
    // Killed at the deadline: the shell dies, its `sleep` keeps the stdout pipe open for `held`.
    let started = Instant::now();
    let out = run_piped_until(
        Command::new("sh").args(["-c", "sleep 6; echo late"]),
        Vec::new(),
        Instant::now() + bound,
    );
    assert!(out.is_none());
    assert!(started.elapsed() < held / 2, "waited {:?} for a pipe", started.elapsed());
    // Exited cleanly before the deadline with a background `sleep` still holding the pipe: nothing is waited on past it.
    let started = Instant::now();
    let out = run_piped_until(
        Command::new("sh").args(["-c", "sleep 6 & echo early"]),
        Vec::new(),
        Instant::now() + bound,
    );
    assert!(out.is_none(), "an output cut short by a held pipe is no answer: {out:?}");
    assert!(started.elapsed() < held / 2, "waited {:?} for a pipe", started.elapsed());
}

#[test]
fn a_measured_policy_is_kept_and_a_failed_probe_is_not() {
    let state = Mutex::new(PolicyProbe::new());
    let now = Instant::now();
    let runs = std::cell::Cell::new(0);
    let counter = &runs;
    let measure = |answer: Option<CompositeColorPolicy>| {
        move || {
            counter.set(counter.get() + 1);
            answer
        }
    };
    // A probe that fails answers `Unknown` and is not remembered...
    assert_eq!(cached_policy(&state, now, measure(None)), CompositeColorPolicy::Unknown);
    assert_eq!(runs.get(), 1);
    // ...nor run again before its backoff has passed (a broken ffmpeg is not
    // respawned for every frame)...
    assert_eq!(
        cached_policy(&state, now, measure(Some(CompositeColorPolicy::FixedBt601))),
        CompositeColorPolicy::Unknown
    );
    assert_eq!(runs.get(), 1, "the backoff holds the probe off");
    // ...but it is retried once the backoff is over, and a policy it then
    // measures is the answer for good.
    state.lock().unwrap().retry_at = Some(Instant::now());
    assert_eq!(
        cached_policy(
            &state,
            Instant::now() + std::time::Duration::from_secs(1),
            measure(Some(CompositeColorPolicy::BottomLayerTag))
        ),
        CompositeColorPolicy::BottomLayerTag
    );
    assert_eq!(runs.get(), 2);
    assert_eq!(
        cached_policy(&state, now, measure(Some(CompositeColorPolicy::FixedBt601))),
        CompositeColorPolicy::BottomLayerTag
    );
    assert_eq!(runs.get(), 2, "a measured policy is never measured again");
}

#[test]
fn the_backoff_after_a_failed_probe_doubles_up_to_five_minutes() {
    let secs = |n| policy_probe_backoff(n).as_secs();
    assert_eq!((secs(1), secs(2), secs(3), secs(4)), (5, 10, 20, 40));
    assert_eq!((secs(7), secs(8), secs(100)), (300, 300, 300));
}

#[test]
fn a_failed_probe_leaves_bt709_footage_to_ffmpeg() {
    // The reviewer's repro: an ffmpeg wrapper that fails the probe. The answer
    // used to be `BottomLayerTag`, which draws a BT.709 clip as BT.709 and is
    // 30 dB / 31 levels wrong on every FFmpeg 6.
    let state = Mutex::new(PolicyProbe::new());
    let policy = cached_policy(&state, Instant::now(), || {
        measure_composite_color_policy(
            "/nonexistent/ffmpeg",
            "/nonexistent/ffprobe",
            std::time::Duration::from_secs(5),
        )
    });
    assert_eq!(policy, CompositeColorPolicy::Unknown);
    let clip_of = |color_space: Option<&str>| {
        let asset = Asset {
            id: uuid::Uuid::new_v4(),
            path: "/media/a.mp4".to_string(),
            name: "a".to_string(),
            duration: 5.0,
            streams: vec![StreamInfo {
                index: 0,
                kind: StreamKind::Video,
                codec: "h264".to_string(),
                width: Some(640),
                height: Some(360),
                fps: Some(30.0),
                sample_rate: None,
                channels: None,
                image: false,
                projection: None,
                rotation: 0,
                color_transfer: None,
                color_primaries: None,
                pix_fmt: Some("yuv420p".to_string()),
                color_space: color_space.map(str::to_string),
                color_range: None,
                lead: 0.0,
                time_base: None,
            }],
            imported_at: chrono::Utc::now(),
            source_paths: Vec::new(),
            voiceover: None,
            music_score: None,
            sound_effect: None,
            tags: Vec::new(),
        };
        let timeline = Timeline {
            tracks: vec![crate::model::Track {
                clips: vec![Clip::new(asset.id, 0.0, 5.0, 0.0)],
                ..crate::model::Track::new(StreamKind::Video, "V1")
            }],
            overlays: Vec::new(),
            svg_overlays: Vec::new(),
            markers: Vec::new(),
            format: None,
            master: Default::default(),
            thumbnails: Vec::new(),
        };
        crate::render_plan::RenderPlan::at(&timeline, &[asset], &ExportOptions::default(), 1.0, policy).unwrap()
    };
    let plan = clip_of(Some("bt709"));
    assert!(!plan.gpu_supported(), "a BT.709 clip must go to FFmpeg");
    assert!(plan.unsupported_reasons().iter().any(|r| r.contains("could not be measured")));
    // What every FFmpeg draws the same way is still drawn.
    let plan = clip_of(None);
    assert!(plan.gpu_supported(), "{:?}", plan.unsupported_reasons());
    assert_eq!(plan.canvas.matrix, crate::render_plan::YuvMatrix::Bt601);
}

#[test]
fn the_declared_ycbcr_matrix_is_recorded_when_there_is_one() {
    let tagged = probe_json(r#""width":1280,"height":720,"color_space":"bt709""#);
    assert_eq!(tagged.streams[0].color_space.as_deref(), Some("bt709"));
    // ffprobe says "unknown" for none; that is no tag, not a tag named unknown.
    let untagged = probe_json(r#""width":1280,"height":720,"color_space":"unknown""#);
    assert_eq!(untagged.streams[0].color_space, None);
    assert_eq!(probe_json(r#""width":1280,"height":720"#).streams[0].color_space, None);
    // `reserved` (a value no matrix has) and `unspecified` are read by FFmpeg as no tag at all.
    for none in ["reserved", "unspecified", ""] {
        let probed = probe_json(&format!(r#""width":1280,"height":720,"color_space":"{none}""#));
        assert_eq!(probed.streams[0].color_space, None, "{none:?}");
    }
}

#[test]
fn the_declared_sample_range_is_recorded_when_there_is_one() {
    // A VP9 / HEVC / AV1 file flagged full range probes as plain `yuv420p` with `color_range=pc`.
    let full = probe_json(r#""width":1280,"height":720,"pix_fmt":"yuv420p","color_range":"pc""#);
    assert_eq!(full.streams[0].pix_fmt.as_deref(), Some("yuv420p"));
    assert_eq!(full.streams[0].color_range.as_deref(), Some("pc"));
    let limited = probe_json(r#""width":1280,"height":720,"color_range":"tv""#);
    assert_eq!(limited.streams[0].color_range.as_deref(), Some("tv"));
    // ffprobe 9 prints `unknown` for none and 6 prints nothing: no flag, not a flag named unknown.
    for none in ["unknown", "unspecified", ""] {
        let probed = probe_json(&format!(r#""width":1280,"height":720,"color_range":"{none}""#));
        assert_eq!(probed.streams[0].color_range, None, "{none:?}");
    }
    assert_eq!(probe_json(r#""width":1280,"height":720"#).streams[0].color_range, None);
    // libav's names and plain words are folded into ffprobe's two.
    for (said, kept) in [
        ("jpeg", "pc"),
        ("full", "pc"),
        ("PC", "pc"),
        ("mpeg", "tv"),
        ("limited", "tv"),
    ] {
        let probed = probe_json(&format!(r#""width":1280,"height":720,"color_range":"{said}""#));
        assert_eq!(probed.streams[0].color_range.as_deref(), Some(kept), "{said:?}");
    }
    // A picture has a range; sound and subtitles do not.
    let parsed: ProbeJson = serde_json::from_str(
            r#"{"format":{"duration":"2.0"},"streams":[
                {"index":0,"codec_type":"video","codec_name":"vp9","width":640,"height":360,"pix_fmt":"yuv420p","color_range":"pc"},
                {"index":1,"codec_type":"audio","codec_name":"opus","sample_rate":"48000","channels":2,"color_range":"pc"}]}"#,
        )
        .unwrap();
    let probed = probe_from_json(parsed, None);
    assert_eq!(probed.streams[0].color_range.as_deref(), Some("pc"));
    assert_eq!(probed.streams[1].color_range, None);
}

#[test]
fn a_turned_phone_clip_probes_at_its_displayed_size() {
    for rotation in [90, -90, 270] {
        let probed = probe_json(&format!(
            r#""width":1920,"height":1080,"side_data_list":[{{"side_data_type":"Display Matrix","rotation":{rotation}}}]"#
        ));
        let v = &probed.streams[0];
        assert_eq!((v.width, v.height), (Some(1080), Some(1920)), "rotation {rotation}");
        assert_eq!(v.rotation, if rotation == 270 { -90 } else { rotation as i16 });
    }
    // Upside-down is still landscape; no rotation is no rotation.
    let flipped =
        probe_json(r#""width":1920,"height":1080,"side_data_list":[{"side_data_type":"Display Matrix","rotation":180}]"#);
    assert_eq!(
        (flipped.streams[0].width, flipped.streams[0].height),
        (Some(1920), Some(1080))
    );
    assert_eq!(flipped.streams[0].rotation, 180);
    let plain = probe_json(r#""width":1920,"height":1080"#);
    assert_eq!((plain.streams[0].width, plain.streams[0].height), (Some(1920), Some(1080)));
    assert_eq!(plain.streams[0].rotation, 0);
}

#[test]
fn an_old_ffprobe_reports_the_turn_as_a_clockwise_tag() {
    let probed = probe_json(r#""width":1920,"height":1080,"tags":{"rotate":"90"}"#);
    let v = &probed.streams[0];
    assert_eq!((v.width, v.height), (Some(1080), Some(1920)));
    assert_eq!(v.rotation, -90, "clockwise 90 is -90 in the counter-clockwise convention");
    // The side data wins when both are present.
    let both = probe_json(
        r#""width":1920,"height":1080,"tags":{"rotate":"90"},"side_data_list":[{"side_data_type":"Display Matrix","rotation":90}]"#,
    );
    assert_eq!(both.streams[0].rotation, 90);
}

#[test]
fn a_display_matrix_reads_back_as_the_angle_ffprobe_reports() {
    const ONE: i32 = 65536;
    let w = 1 << 30;
    // The matrix of a real phone clip, which ffprobe reports as rotation 90.
    assert_eq!(matrix_rotation(&[0, -ONE, 0, ONE, 0, 0, 0, 0, w]), 90);
    assert_eq!(matrix_rotation(&[0, ONE, 0, -ONE, 0, 0, 0, 0, w]), -90);
    assert_eq!(matrix_rotation(&[ONE, 0, 0, 0, ONE, 0, 0, 0, w]), 0);
    assert_eq!(matrix_rotation(&[-ONE, 0, 0, 0, -ONE, 0, 0, 0, w]), 180);
    assert_eq!(matrix_rotation(&[0; 9]), 0, "a degenerate matrix is no rotation");
}

#[test]
fn displayed_size_swaps_only_for_quarter_turns() {
    assert_eq!(displayed_size(Some(4), Some(3), 90), (Some(3), Some(4)));
    assert_eq!(displayed_size(Some(4), Some(3), -90), (Some(3), Some(4)));
    assert_eq!(displayed_size(Some(4), Some(3), 180), (Some(4), Some(3)));
    assert_eq!(displayed_size(Some(4), Some(3), 0), (Some(4), Some(3)));
    assert_eq!(displayed_size(None, Some(3), 90), (Some(3), None));
}

#[test]
fn probe_records_the_hdr_transfer_and_ignores_unknown_tags() {
    let hlg = probe_json(r#""width":3840,"height":2160,"color_transfer":"arib-std-b67","color_primaries":"bt2020""#);
    let v = &hlg.streams[0];
    assert_eq!(v.color_transfer.as_deref(), Some("arib-std-b67"));
    assert_eq!(v.color_primaries.as_deref(), Some("bt2020"));
    assert_eq!(v.hdr(), Some(crate::model::Hdr::Hlg));
    let pq = probe_json(r#""width":3840,"height":2160,"color_transfer":"smpte2084","color_primaries":"bt2020""#);
    assert_eq!(pq.streams[0].hdr(), Some(crate::model::Hdr::Pq));
    let sdr = probe_json(r#""width":1920,"height":1080,"color_transfer":"bt709","color_primaries":"bt709""#);
    assert_eq!(sdr.streams[0].hdr(), None);
    let untagged = probe_json(r#""width":1920,"height":1080,"color_transfer":"unknown""#);
    assert_eq!(untagged.streams[0].color_transfer, None);
    assert_eq!(untagged.streams[0].hdr(), None);
}

#[test]
fn a_jittery_vfr_clip_keeps_its_nominal_frame_rate() {
    // A phone clip that dropped frames keeps its nominal rate...
    assert_eq!(nominal_fps(Some(30.0), Some(25.75)), Some(30.0));
    assert_eq!(nominal_fps(Some(30000.0 / 1001.0), Some(29.9)), Some(30000.0 / 1001.0));
    // ...but a timestamp grid far finer than the footage does not become the
    // project's frame rate.
    assert_eq!(nominal_fps(Some(120.0), Some(30.07)), Some(30.0));
    assert_eq!(nominal_fps(Some(600.0), Some(29.97)), Some(29.97));
    assert_eq!(nominal_fps(Some(600.0), Some(41.3)), Some(41.3));
    assert_eq!(nominal_fps(Some(25.0), None), Some(25.0));
    assert_eq!(nominal_fps(None, Some(24.0)), Some(24.0));
    assert_eq!(nominal_fps(None, None), None);
    assert_eq!(nominal_fps(Some(0.0), Some(0.0)), None);
}

// ---- HDR → SDR -----------------------------------------------------------

fn hlg_asset() -> Asset {
    let mut asset = test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)]);
    asset.path = "/media/hdr.mov".into();
    asset.streams[0].color_transfer = Some("arib-std-b67".into());
    asset.streams[0].color_primaries = Some("bt2020".into());
    asset
}

#[test]
fn the_tonemap_chain_names_the_input_transfer_and_ends_in_sdr() {
    let hlg = tonemap_chain(Hdr::Hlg, true);
    assert!(
        hlg.starts_with("zscale=tin=arib-std-b67:pin=bt2020:min=bt2020nc:t=linear:npl=100,"),
        "{hlg}"
    );
    assert!(hlg.contains("tonemap=tonemap=mobius"), "{hlg}");
    assert!(
        hlg.ends_with("zscale=t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"),
        "{hlg}"
    );
    assert!(tonemap_chain(Hdr::Pq, true).starts_with("zscale=tin=smpte2084:"));
    // No libzimg: still leaves BT.2020 and lands on 8-bit 4:2:0.
    let fallback = tonemap_chain(Hdr::Hlg, false);
    assert!(
        !fallback.contains("zscale") && fallback.contains("colorspace=all=bt709:iall=bt2020"),
        "{fallback}"
    );
    assert!(fallback.ends_with("format=yuv420p"));
}

#[test]
fn an_hdr_clip_is_tonemapped_once_after_fps_and_before_colour() {
    let asset = hlg_asset();
    let mut clip = make_clip(asset.id, 0.0, 5.0, 0.0);
    clip.color.saturation = 1.3;
    let timeline = single(vec![clip.clone()]);
    let fx = transition_fx(&timeline, std::slice::from_ref(&asset));
    assert_eq!(fx[0].hdr, Some(Hdr::Hlg));
    let chain = video_clip_chain(&clip, &fmt_1080p(), &fx[0], false, "c0");
    assert_eq!(chain.matches("tonemap=tonemap").count(), 1, "{chain}");
    let (fps, tone, eq) = (
        chain.find("fps=").unwrap(),
        chain.find("zscale=tin=").unwrap(),
        chain.find("eq=").unwrap(),
    );
    assert!(
        chain.find("scale=1920").unwrap() < tone,
        "geometry first, on the cheap pixels: {chain}"
    );
    assert!(fps < tone && tone < eq, "{chain}");
    // SDR is untouched.
    let sdr = video_clip_chain(&clip, &fmt_1080p(), &ClipFx::default(), false, "c0");
    assert!(
        !sdr.contains("zscale") && !sdr.contains("tonemap") && !sdr.contains("colorspace"),
        "{sdr}"
    );
}

#[test]
fn hdr_reaches_export_preview_stream_and_still_graphs_but_sdr_never_does() {
    let hdr = hlg_asset();
    let sdr = Asset {
        id: Uuid::new_v4(),
        path: "/media/sdr.mp4".into(),
        ..test_asset(vec![video_stream(1920, 1080, 30.0), audio_stream(48_000, 2)])
    };
    let assets = vec![hdr.clone(), sdr.clone()];
    let timeline = single(vec![make_clip(hdr.id, 0.0, 5.0, 0.0), make_clip(sdr.id, 0.0, 5.0, 5.0)]);
    let count = |args: &[String]| args.join(" ").matches("tonemap=tonemap").count();

    let export = build_export_args(&timeline, &assets, "out.mp4", &ExportOptions::default()).unwrap();
    assert_eq!(count(&export), 1, "only the HDR clip: {export:?}");
    let stream = build_preview_args(&timeline, &assets, 1.0, 24.0, 960, 6).unwrap();
    assert_eq!(count(&stream), 1);
    let still = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 2.0, 640, 4).unwrap();
    assert_eq!(count(&still), 1);
    let sdr_still = build_timeline_frame_args(&timeline, &assets, &ExportOptions::default(), 7.0, 640, 4).unwrap();
    assert_eq!(count(&sdr_still), 0);

    // Through a proxy the asset is SDR, and nothing converts a second time.
    let proxied = vec![hdr.as_sdr_proxy(), sdr];
    let export = build_export_args(&timeline, &proxied, "out.mp4", &ExportOptions::default()).unwrap();
    assert_eq!(count(&export), 0);
}

#[test]
fn single_frame_contact_sheet_and_proxy_decodes_tonemap_after_the_downscale() {
    let (args, _) = build_contact_sheet_args("/m/a.mov", 0.0, 8.0, 2, 2, 240, 5, Some("TONEMAP"));
    assert!(
        args.join(" ").contains("scale=240:-2:flags=bilinear,TONEMAP,tile=2x2"),
        "{args:?}"
    );
    let proxy = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, Some("TONEMAP"), None).join(" ");
    assert!(
        proxy.contains("scale='min(1280,iw)':-2:flags=bilinear,TONEMAP -c:v libx264"),
        "{proxy}"
    );
    // SDR stays byte-identical.
    let plain = build_proxy_args("/in.mov", "/out.mp4", 3, 1280, "libx264", None, None, None).join(" ");
    assert!(plain.contains("flags=bilinear -c:v libx264"), "{plain}");
}

// ---- phone / mirrorless footage, end to end ------------------------------

/// Say why an end-to-end test has nothing to check on this ffmpeg.
#[allow(clippy::print_stderr)]
fn skip(why: &str) {
    eprintln!("{why}");
}

/// A scratch directory that is removed when the test ends, pass or fail.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kerf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Scratch(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `ffmpeg` with `args` (after the usual quiet flags); true when it exits 0.
fn try_ffmpeg(args: &[&str]) -> bool {
    command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .status_bounded()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_ffmpeg(args: &[&str]) {
    assert!(try_ffmpeg(args), "ffmpeg {args:?} failed");
}

/// The first frame of `path` (optionally through `vf`) as `(width, height, rgb24)`,
/// decoded by ffmpeg with autorotation on — the ground truth for "upright".
fn rgb_frame(path: &Path, vf: Option<&str>) -> (usize, usize, Vec<u8>) {
    let mut cmd = command(&ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-i"]).arg(path);
    if let Some(vf) = vf {
        cmd.args(["-vf", vf]);
    }
    let out = cmd
        .args([
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "image2pipe",
            "-vcodec",
            "ppm",
            "pipe:1",
        ])
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let b = &out.stdout;
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while b[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !b[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&b[start..at]).into_owned());
    }
    let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    (w, h, b[at + 1..].to_vec())
}

/// Which edge of the picture the pure-red bar sits on (`top` / `bottom` /
/// `left` / `right`), judged by where the red pixels' centroid falls.
fn red_bar_edge(w: usize, h: usize, rgb: &[u8]) -> &'static str {
    let (mut n, mut sx, mut sy) = (0.0, 0.0, 0.0);
    for y in 0..h {
        for x in 0..w {
            let p = &rgb[(y * w + x) * 3..][..3];
            if p[0] > 180 && p[1] < 90 && p[2] < 90 {
                n += 1.0;
                sx += x as f64;
                sy += y as f64;
            }
        }
    }
    if n == 0.0 {
        return "none";
    }
    let (cx, cy) = (sx / n / w as f64 - 0.5, sy / n / h as f64 - 0.5);
    if cx.abs() > cy.abs() {
        if cx > 0.0 {
            "right"
        } else {
            "left"
        }
    } else if cy > 0.0 {
        "bottom"
    } else {
        "top"
    }
}

/// The probed asset for a file, as the import path would build it.
fn probed_asset(path: &Path) -> Asset {
    let probed = probe(path).expect("probe");
    let mut asset = av_asset(Uuid::new_v4(), probed.duration);
    asset.path = path.to_string_lossy().into_owned();
    asset.streams = probed.streams;
    asset
}

fn ffprobe_field(path: &Path, stream: &str, entries: &str) -> String {
    let out = command(&ffprobe_bin())
        .args(["-v", "error", "-select_streams", stream, "-show_entries", entries])
        .args(["-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout).lines().collect::<Vec<_>>().join(",")
}

fn mp4_export_opts() -> ExportOptions {
    ExportOptions {
        container: Container::Mp4,
        video_codec: Some("libx264".into()),
        ..Default::default()
    }
}

/// `src` remuxed with a display matrix turning it by `rotation` degrees, the
/// way a phone marks a portrait recording. `None` when this ffmpeg can not
/// write a display matrix at all (the test then has nothing to say).
fn turned(dir: &Scratch, src: &Path, rotation: i32, name: &str) -> Option<PathBuf> {
    let out = dir.join(name);
    let (src, outs) = (src.to_str().unwrap(), out.to_str().unwrap());
    let rot = rotation.to_string();
    // FFmpeg 6+ takes the angle as an input option; older builds write the
    // matrix from a `rotate` tag (clockwise, the opposite sign).
    let neg = (-rotation).to_string();
    let wrote = try_ffmpeg(&["-display_rotation", &rot, "-i", src, "-c", "copy", outs])
        || try_ffmpeg(&["-i", src, "-c", "copy", "-metadata:s:v:0", &format!("rotate={neg}"), outs]);
    let dump = command(&ffprobe_bin())
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&out)
        .output()
        .expect("run ffprobe");
    let dump = String::from_utf8_lossy(&dump.stdout);
    (wrote && (dump.contains("\"rotation\"") || dump.contains("\"rotate\""))).then_some(out)
}

/// A landscape clip with a red bar along the top of its coded frame, turned
/// by [`turned`].
fn rotated_clip(dir: &Scratch, rotation: i32) -> Option<PathBuf> {
    let base = dir.join("landscape.mp4");
    run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=gray:s=640x360:r=30:d=2,drawbox=x=0:y=0:w=640:h=60:color=red:t=fill",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        base.to_str().unwrap(),
    ]);
    turned(dir, &base, rotation, "portrait.mp4")
}

/// A phone clip is a landscape sensor frame plus a rotation. Every ffmpeg
/// decode turns it upright, so the probe must report the *displayed* size or
/// the project frame, the fit and the crop maths all describe a picture that
/// is not the one being rendered.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored rotated_phone_clip`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn rotated_phone_clip_probes_display_dims_and_exports_upright() {
    let dir = Scratch::new("rotated");
    let Some(media) = rotated_clip(&dir, -90) else {
        skip("skipped: this ffmpeg cannot write a display matrix");
        return;
    };
    let (rw, rh, ref_rgb) = rgb_frame(&media, None);
    assert_eq!((rw, rh), (360, 640), "the reference decode is upright");
    let want_edge = red_bar_edge(rw, rh, &ref_rgb);
    assert_ne!(want_edge, "top", "the rotation moved the bar off the top");

    let asset = probed_asset(&media);
    let video = asset.streams.iter().find(|s| s.kind == StreamKind::Video).unwrap();
    assert_eq!(
        (video.width, video.height),
        (Some(360), Some(640)),
        "probe reports display dims"
    );
    assert_ne!(video.rotation, 0);

    let timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);
    assert_eq!(delivery_frame(&timeline, std::slice::from_ref(&asset)), (360, 640));

    let out = dir.join("export.mp4");
    render_with(&timeline, std::slice::from_ref(&asset), &out, &mp4_export_opts()).expect("export");
    let (w, h, rgb) = rgb_frame(&out, None);
    assert_eq!((w, h), (360, 640), "the file is portrait, not a letterboxed landscape");
    assert_eq!(red_bar_edge(w, h, &rgb), want_edge, "and upright");

    let jpeg = timeline_frame_cancellable(
        &timeline,
        std::slice::from_ref(&asset),
        &ExportOptions::default(),
        0.5,
        360,
        3,
        &|| false,
    )
    .expect("composited still");
    let still = dir.join("still.jpg");
    std::fs::write(&still, jpeg).unwrap();
    let (w, h, rgb) = rgb_frame(&still, None);
    assert_eq!((w, h), (360, 640));
    assert_eq!(red_bar_edge(w, h, &rgb), want_edge);

    let jpeg = frame_jpeg(&media, 0.5, 360, 3, true).expect("frame");
    std::fs::write(&still, jpeg).unwrap();
    let (w, h, rgb) = rgb_frame(&still, None);
    assert_eq!((w, h), (360, 640));
    assert_eq!(red_bar_edge(w, h, &rgb), want_edge);
}

/// A file flagged full range probes as plain `yuv420p` where its codec's decoder does not call the
/// format `yuvj420p` (FFV1 here, VP9 and AV1 as well): the format cannot tell it from a limited one,
/// the range flag can. Real ffprobe, so the spelling `range_tag` reads is the one it prints.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored flagged_full_range`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn a_file_flagged_full_range_probes_its_range_beside_a_plain_format() {
    let dir = Scratch::new("range-flag");
    // (An unflagged FFV1 file reads `tv` from FFmpeg 9's muxer and nothing from 6.1.1's.)
    for (name, codec, pix_fmt, flag, want) in [
        ("pc.mkv", "ffv1", "yuv420p", Some("pc"), vec![Some("pc")]),
        ("tv.mkv", "ffv1", "yuv420p", Some("tv"), vec![Some("tv")]),
        ("unflagged.mkv", "ffv1", "yuv420p", None, vec![None, Some("tv")]),
        ("jpeg.mkv", "mjpeg", "yuvj420p", None, vec![Some("pc")]),
    ] {
        let file = dir.join(name);
        let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=64x64:rate=5:duration=0.4", "-c:v", codec];
        args.extend(["-pix_fmt", pix_fmt]);
        if let Some(flag) = flag {
            args.extend(["-color_range", flag]);
        }
        args.push(file.to_str().unwrap());
        run_ffmpeg(&args);
        let probed = probe(&file).expect("probe");
        let video = &probed.streams[0];
        assert_eq!(video.pix_fmt.as_deref(), Some(pix_fmt), "{name}");
        assert!(
            want.contains(&video.color_range.as_deref()),
            "{name}: {:?}",
            video.color_range
        );
    }
}

/// The proxy is what the preview actually decodes; it must come out upright
/// and carry no matrix of its own that would turn it a second time.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored rotated_proxy`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn rotated_proxy_is_upright_and_carries_no_matrix() {
    let dir = Scratch::new("rotated-proxy");
    let Some(media) = rotated_clip(&dir, -90) else {
        skip("skipped: this ffmpeg cannot write a display matrix");
        return;
    };
    let (rw, rh, ref_rgb) = rgb_frame(&media, None);
    let proxy = generate_proxy(&media, PROXY_MAX_WIDTH).expect("proxy");
    let dump = command(&ffprobe_bin())
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&proxy)
        .output()
        .expect("run ffprobe");
    let dump = String::from_utf8_lossy(&dump.stdout).into_owned();
    let (w, h, rgb) = rgb_frame(&proxy, None);
    remove_proxy(&proxy);
    assert!(!dump.contains("Display Matrix") && !dump.contains("\"rotate\""), "{dump}");
    assert_eq!((w, h), (rw, rh));
    assert_eq!(red_bar_edge(w, h, &rgb), red_bar_edge(rw, rh, &ref_rgb));
}

/// Six seconds of 30 fps video whose frames number themselves — bit `k` of the
/// frame number is the `k`-th eighth of the picture's width, white for 1 — so a
/// decoded frame says which source frame it is, however it was scaled or
/// compressed. The video starts `lead` seconds after the audio (an empty edit
/// in the mp4, the way a camera that opens its mic first writes it). `None`
/// when this ffmpeg cannot make the file.
fn late_barcode_clip(dir: &Scratch, name: &str, lead: f64) -> Option<PathBuf> {
    let (video, audio, out) = (dir.join(&format!("v-{name}")), dir.join(&format!("a-{name}")), dir.join(name));
    let bars = "color=c=black:s=640x360:r=30:d=6,format=yuv420p,\
                    geq=lum='if(bitand(trunc(N/pow(2,trunc(X*8/W))),1),235,16)':cb=128:cr=128";
    let delay = lead.to_string();
    let ok = try_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        bars,
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "30",
        "-pix_fmt",
        "yuv420p",
        video.to_str().unwrap(),
    ]) && try_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000:duration=8",
        "-c:a",
        "aac",
        audio.to_str().unwrap(),
    ]) && try_ffmpeg(&[
        "-itsoffset",
        &delay,
        "-i",
        video.to_str().unwrap(),
        "-i",
        audio.to_str().unwrap(),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c",
        "copy",
        out.to_str().unwrap(),
    ]);
    ok.then_some(out)
}

/// Which numbered frame of [`late_barcode_clip`] `path` answers a seek to `t` with,
/// through the same single-frame decode the preview scrubs with.
fn barcode_at(dir: &Scratch, path: &Path, t: f64) -> u32 {
    let jpeg = frame_jpeg(path, t, 640, 2, true).expect("frame");
    let file = dir.join("barcode.jpg");
    std::fs::write(&file, jpeg).unwrap();
    let (w, h, rgb) = rgb_frame(&file, None);
    (0..8)
        .filter(|k| rgb[((h / 2) * w + (2 * k + 1) * w / 16) * 3] > 125)
        .map(|k| 1u32 << k)
        .sum()
}

/// The presentation times of the first `n` video packets in `path`'s own clock.
fn first_packet_times(path: &Path, n: usize) -> Vec<f64> {
    let out = command(&ffprobe_bin())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("run ffprobe");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .take(n)
        .map(|l| l.trim().parse().expect("a packet time"))
        .collect()
}

/// A source whose video starts after its audio. FFmpeg 7+ no longer pads such
/// a head when it writes the audio-less proxy, so the proxy's container began
/// at the video and `-ss T` on it landed `lead` seconds deeper into the footage
/// than `-ss T` on the original — preview and export showed different frames
/// (two at half a second in, a five-second slip for a five-second lead). The
/// leads are chosen to break each cheaper fix: 0.064 s is 1.92 frames, which
/// `-fps_mode cfr` rounds *up* and then seeks a frame early at every frame
/// boundary; 1.5 s is a gap long enough to see; 0 is the control — an ordinary
/// file whose proxy must come out exactly as before.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored late_starting_video`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn proxy_of_a_late_starting_video_answers_every_seek_like_the_original() {
    let dir = Scratch::new("late-proxy");
    let times = [
        0.0, 0.01, 0.03, 0.05, 0.07, 0.1, 0.2, 0.5, 0.51, 1.0, 1.49, 1.51, 1.7, 3.333, 4.0, 5.9,
    ];
    for (i, lead) in [0.0, 0.064, 0.08, 1.5].into_iter().enumerate() {
        let Some(media) = late_barcode_clip(&dir, &format!("late{i}.mp4"), lead) else {
            skip("skipped: this ffmpeg cannot make the late-starting test clip");
            return;
        };
        let traits = source_traits(&media).expect("probe");
        assert_eq!(
            needs_head_pad(traits.lead),
            lead > 0.0,
            "lead {lead}: the probe saw {}",
            traits.lead
        );
        if lead > 0.0 {
            assert!(
                (traits.lead - lead).abs() < 0.005,
                "lead {lead}: the probe saw {}",
                traits.lead
            );
        }

        let proxy = generate_proxy(&media, PROXY_MAX_WIDTH).expect("proxy");
        let _cleanup = ProxyGuard(proxy.clone());
        assert_eq!(
            is_head_padded_proxy(&proxy.to_string_lossy()),
            lead > 0.0,
            "lead {lead}: the name says whether the head was padded: {proxy:?}"
        );
        let mut seen = Vec::new();
        for t in times {
            let (want, got) = (barcode_at(&dir, &media, t), barcode_at(&dir, &proxy, t));
            assert_eq!(
                got, want,
                "lead {lead}: a seek to {t}s lands on frame {got} of the proxy, {want} of the original"
            );
            seen.push(want);
        }
        assert!(seen.first() < seen.last(), "lead {lead}: the clip never moved: {seen:?}");

        // The structure behind it: the proxy starts with its container, holding
        // the first frame until the footage begins where the original does.
        let (orig, prox) = (first_packet_times(&media, 1), first_packet_times(&proxy, 2));
        let after = source_traits(&proxy).map(|t| t.lead).unwrap_or(f64::NAN);
        assert_eq!(after, 0.0, "lead {lead}: the proxy's video starts with its container");
        if lead > 0.0 {
            assert_eq!(prox[0], 0.0);
            assert!((prox[1] - orig[0]).abs() < 0.002, "lead {lead}: {prox:?} against {orig:?}");
        } else {
            assert_eq!(prox[0], orig[0], "an ordinary source is not given a pad");
        }
    }

    // A transport stream cannot be fixed this way (its demuxer rebases a read with
    // the audio discarded), so it is left alone: no lead, the plain proxy and key.
    let ts = dir.join("late.ts");
    let late = dir.join("late1.mp4");
    if try_ffmpeg(&[
        "-i",
        late.to_str().unwrap(),
        "-c",
        "copy",
        "-f",
        "mpegts",
        ts.to_str().unwrap(),
    ]) {
        let traits = source_traits(&ts).expect("probe");
        assert_eq!(traits.lead, 0.0, "a .ts is never padded");
        let proxy = generate_proxy(&ts, PROXY_MAX_WIDTH).expect("proxy");
        let _cleanup = ProxyGuard(proxy.clone());
        assert!(!is_head_padded_proxy(&proxy.to_string_lossy()));
    }
}

/// 10-bit HLG BT.2020 footage, made by re-encoding a tagged SDR test card, so
/// the SDR original is the answer key. (The tags matter: an untagged source
/// has no colourspace for `zscale` to start from on older FFmpegs.) `None`
/// without libx265 / zscale.
fn hlg_clip(dir: &Scratch) -> Option<(PathBuf, PathBuf)> {
    let sdr = dir.join("sdr.mp4");
    run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=30:duration=2,format=yuv420p",
        "-c:v",
        "libx264",
        "-crf",
        "12",
        "-pix_fmt",
        "yuv420p",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-colorspace",
        "bt709",
        "-color_range",
        "tv",
        sdr.to_str().unwrap(),
    ]);
    let hlg = dir.join("hlg.mp4");
    let convert = |input: &[&str]| {
        let mut args: Vec<&str> = input.to_vec();
        args.extend([
            "-vf",
            "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt2020:t=arib-std-b67:m=bt2020nc:r=tv,format=yuv420p10le",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error",
            "-color_primaries",
            "bt2020",
            "-color_trc",
            "arib-std-b67",
            "-colorspace",
            "bt2020nc",
            hlg.to_str().unwrap(),
        ]);
        try_ffmpeg(&args)
    };
    // FFmpeg 9 wants the card's colourspace stated in the graph, FFmpeg 4
    // wants it on a file: one of the two starts `zscale` from a known place.
    let ok = convert(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=30:duration=2,format=yuv420p,\
             setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv",
    ]) || convert(&["-i", sdr.to_str().unwrap()]);
    ok.then_some((sdr, hlg))
}

/// Mean absolute difference and mean colourfulness (`max-min` over RGB) of two
/// same-sized frames.
fn compare_frames(a: &[u8], b: &[u8]) -> (f64, f64, f64) {
    let mad = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / a.len() as f64;
    let colourful = |f: &[u8]| {
        f.as_chunks::<3>()
            .0
            .iter()
            .map(|p| (p.iter().max().unwrap() - p.iter().min().unwrap()) as f64)
            .sum::<f64>()
            / (f.len() / 3) as f64
    };
    (mad, colourful(a), colourful(b))
}

/// An iPhone's HLG BT.2020 footage squeezed into BT.709 untouched is washed
/// out and mis-tagged. Exported next to its SDR original it must come back
/// as the same picture, in an SDR file.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored hlg_footage`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn hlg_footage_exports_as_sdr_with_matching_colour() {
    let dir = Scratch::new("hlg");
    let Some((sdr, hlg)) = hlg_clip(&dir) else {
        skip("skipped: this ffmpeg cannot make the HLG test clip");
        return;
    };
    let hlg_asset = probed_asset(&hlg);
    assert_eq!(hlg_asset.hdr(), Some(Hdr::Hlg), "probe reads the transfer");
    assert_eq!(probed_asset(&sdr).hdr(), None);

    let export = |asset: &Asset, name: &str, opts: &ExportOptions| {
        let timeline = single(vec![make_clip(asset.id, 0.0, 2.0, 0.0)]);
        let out = dir.join(name);
        render_with(&timeline, std::slice::from_ref(asset), &out, opts).expect("export");
        out
    };
    let reference = export(&probed_asset(&sdr), "ref.mp4", &mp4_export_opts());
    let (rw, rh, ref_rgb) = rgb_frame(&reference, None);
    // Both the plain export and the GUI default (hardware decode if any).
    for (name, opts) in [
        ("plain.mp4", mp4_export_opts()),
        (
            "hw.mp4",
            ExportOptions {
                hwaccel: Some("auto".into()),
                ..mp4_export_opts()
            },
        ),
    ] {
        let out = export(&hlg_asset, name, &opts);
        let (w, h, rgb) = rgb_frame(&out, None);
        assert_eq!((w, h), (rw, rh));
        let (mad, colour, ref_colour) = compare_frames(&rgb, &ref_rgb);
        let trc = ffprobe_field(&out, "v:0", "stream=color_transfer");
        assert!(
            !matches!(trc.as_str(), "arib-std-b67" | "smpte2084"),
            "{name} is still tagged {trc}"
        );
        assert!(
            (0.8..1.25).contains(&(colour / ref_colour)),
            "{name}: colourfulness {colour:.1} vs the SDR original's {ref_colour:.1}"
        );
        assert!(
            mad < 25.0,
            "{name}: differs from the SDR original by {mad:.1} levels on average"
        );
    }

    // A phone clip is both at once. The side data in the file must not hide
    // the colour tags from the probe, the proxy has to be converted *and*
    // upright, and the export portrait and SDR.
    if let Some(rotated) = turned(&dir, &hlg, -90, "hlg-portrait.mp4") {
        assert_eq!(source_hdr(&rotated), Some(Hdr::Hlg));
        let asset = probed_asset(&rotated);
        assert_eq!(
            (asset.hdr(), asset.streams[0].width, asset.streams[0].height),
            (Some(Hdr::Hlg), Some(360), Some(640))
        );
        let proxy = generate_proxy(&rotated, PROXY_MAX_WIDTH).expect("proxy");
        let tags = ffprobe_field(&proxy, "v:0", "stream=width,height,pix_fmt,color_transfer");
        remove_proxy(&proxy);
        assert_eq!(tags, "360,640,yuv420p,bt709", "the proxy is upright SDR");
        let out = export(&asset, "portrait.mp4", &mp4_export_opts());
        assert_eq!(ffprobe_field(&out, "v:0", "stream=width,height"), "360,640");
        let (_, _, rgb) = rgb_frame(&out, None);
        let (_, colour, ref_colour) = compare_frames(&rgb, &ref_rgb);
        assert!(
            (0.8..1.25).contains(&(colour / ref_colour)),
            "colourfulness {colour:.1} vs the SDR original's {ref_colour:.1}"
        );
    }

    // The single-frame decodes tone-map too: a scrubbed frame and a contact
    // sheet cell look like the SDR original, not like a washed-out copy.
    let jpeg_rgb = |name: &str, bytes: Vec<u8>| {
        let file = dir.join(name);
        std::fs::write(&file, bytes).unwrap();
        rgb_frame(&file, Some("scale=640:360"))
    };
    let (.., sdr_frame) = jpeg_rgb("sdr-frame.jpg", frame_jpeg(&sdr, 0.5, 640, 2, true).unwrap());
    let (.., hlg_frame) = jpeg_rgb("hlg-frame.jpg", frame_jpeg(&hlg, 0.5, 640, 2, true).unwrap());
    let (.., hlg_sheet) = jpeg_rgb("hlg-sheet.jpg", contact_sheet(&hlg, 0.5, 1.5, 1, 1, 640, 2).unwrap().0);
    for (what, rgb) in [("frame", &hlg_frame), ("contact sheet", &hlg_sheet)] {
        let (mad, colour, ref_colour) = compare_frames(rgb, &sdr_frame);
        assert!(
            (0.8..1.25).contains(&(colour / ref_colour)),
            "{what}: colourfulness {colour:.1} vs {ref_colour:.1}"
        );
        // Looser than the export: the sheet cell is a neighbouring frame of a
        // moving test card, bilinear-scaled. A washed-out copy is ~70 off.
        assert!(mad < 35.0, "{what}: differs from the SDR original by {mad:.1}");
    }
}

/// A Canon R5 II records 10-bit 4:2:2 HEVC with 24-bit PCM audio in an MP4.
/// Hardware decoders often cannot do 4:2:2, so the GUI's `auto` hwaccel has
/// to fall back rather than fail, and the PCM must survive every audio path.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored mirrorless_422`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn mirrorless_422_10bit_hevc_with_24bit_pcm_exports_with_audio() {
    let dir = Scratch::new("r5");
    // PCM in an MP4 is what the camera writes; older FFmpegs refuse it there
    // and take it in a MOV, which is the same audio for every reader below.
    let make = |name: &str| {
        let media = dir.join(name);
        try_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=25:duration=3,format=yuv422p10le",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error",
            "-pix_fmt",
            "yuv422p10le",
            "-c:a",
            "pcm_s24le",
            "-ac",
            "2",
            media.to_str().unwrap(),
        ])
        .then_some(media)
    };
    let Some(media) = make("r5.mp4").or_else(|| make("r5.mov")) else {
        skip("skipped: this ffmpeg cannot make the 4:2:2 test clip");
        return;
    };
    let asset = probed_asset(&media);
    assert!(asset
        .streams
        .iter()
        // FFmpeg 6 reads the MP4 `ipcm` box back as 32-bit; any PCM is the point.
        .any(|s| s.kind == StreamKind::Audio && s.codec.starts_with("pcm_s")));

    let timeline = single(vec![make_clip(asset.id, 0.0, 3.0, 0.0)]);
    // `vaapi` is a hardware decode that is requested by name: where it cannot
    // do 4:2:2 (or is not there at all) the export has to retry in software
    // rather than fail.
    for (name, hw) in [
        ("plain.mp4", None),
        ("hw.mp4", Some("auto".to_string())),
        ("named.mp4", Some("vaapi".to_string())),
    ] {
        let out = dir.join(name);
        let opts = ExportOptions {
            hwaccel: hw,
            ..mp4_export_opts()
        };
        render_with(&timeline, std::slice::from_ref(&asset), &out, &opts).expect("export");
        assert_eq!(
            ffprobe_field(&out, "a:0", "stream=codec_type"),
            "audio",
            "{name} lost its audio"
        );
        let probe = command(&ffmpeg_bin())
            .args(["-hide_banner", "-i"])
            .arg(&out)
            .args(["-af", "volumedetect", "-vn", "-f", "null", "-"])
            .output()
            .expect("run ffmpeg");
        let log = String::from_utf8_lossy(&probe.stderr);
        let mean: f64 = log
            .lines()
            .find_map(|l| l.split("mean_volume:").nth(1))
            .and_then(|v| v.trim().trim_end_matches(" dB").parse().ok())
            .unwrap_or(-91.0);
        assert!(mean > -40.0, "{name}: exported audio is silent ({mean} dB)\n{log}");
        let (w, h, _) = rgb_frame(&out, None);
        assert_eq!((w, h), (640, 360));
    }

    // The other audio readers: preview PCM, waveform, loudness analysis.
    let pcm = audio_pcm(&media, 0.5, 0.5, 8_000, None).expect("preview audio");
    // FFmpeg 6 seeks PCM-in-MP4 to the nearest packet, so allow a short read.
    assert!(
        (5_000..=8_000).contains(&pcm.len()),
        "about half a second of mono s16le: {}",
        pcm.len()
    );
    assert!(pcm.as_chunks::<2>().0.iter().any(|b| i16::from_le_bytes(*b).abs() > 1000));
    let wave = waveform(&media, 50, 8_000).expect("waveform");
    assert!(wave.iter().copied().fold(0.0, f32::max) > 0.1);
    let loud = super::super::audio::measure_loudness_cancellable(&media, &|| false).expect("loudness");
    assert!(loud.integrated_lufs > -40.0, "{loud:?}");

    // Preview frames decode (with hardware decode attempted first).
    assert!(!frame_jpeg(&media, 1.0, 320, 4, true).expect("frame").is_empty());
    assert!(!frame_at(&media, 1.0, 320).expect("png frame").is_empty());
}

/// Cut 3.5s..8.5s out of a variable-frame-rate phone clip (every seventh
/// frame missing) and butt a constant-rate clip against it. A white flash and
/// a beep land together at every whole second of the source, so after the cut
/// they must still land together, on the right half-seconds.
///
/// `cargo test -p kerf-core --no-default-features -- --ignored vfr_cut`
#[test]
#[ignore = "needs the ffmpeg binary"]
fn vfr_cut_keeps_duration_and_av_sync() {
    let dir = Scratch::new("vfr");
    let vfr = dir.join("vfr.mp4");
    let video = "color=c=black:s=640x360:r=30:d=12,\
                     drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='lt(mod(t,1),0.1)',\
                     select='mod(n,7)'";
    let beeps = "aevalsrc='sin(2*PI*1000*t)*lt(mod(t,1),0.1)':s=48000:d=12";
    let made = ["-fps_mode", "-vsync"].iter().any(|flag| {
        try_ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            video,
            "-f",
            "lavfi",
            "-i",
            beeps,
            flag,
            "vfr",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            vfr.to_str().unwrap(),
        ])
    });
    assert!(made, "could not make the VFR clip");
    let tail = dir.join("tail.mp4");
    run_ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=gray:s=640x360:r=25:d=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=220:sample_rate=48000:duration=3",
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        tail.to_str().unwrap(),
    ]);

    let (a, b) = (probed_asset(&vfr), probed_asset(&tail));
    let timeline = single(vec![make_clip(a.id, 3.5, 8.5, 0.0), make_clip(b.id, 0.0, 3.0, 5.0)]);
    let out = dir.join("cut.mp4");
    render_with(&timeline, &[a, b], &out, &mp4_export_opts()).expect("export");

    let total: f64 = {
        let o = command(&ffprobe_bin())
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(&out)
            .output()
            .expect("run ffprobe");
        String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
    };
    assert!((total - 8.0).abs() < 0.15, "expected an 8s file, got {total}");

    // Flash times: per-frame mean luma of the picture sampled at 100 Hz.
    let luma = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&out)
        .args(["-an", "-vf", "fps=100,scale=1:1,format=gray", "-f", "rawvideo", "pipe:1"])
        .output()
        .expect("run ffmpeg")
        .stdout;
    // Beep times: RMS of the mono audio in 10 ms windows.
    let pcm = command(&ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&out)
        .args(["-vn", "-ac", "1", "-ar", "8000", "-f", "s16le", "pipe:1"])
        .output()
        .expect("run ffmpeg")
        .stdout;
    let onsets = |active: Vec<bool>| -> Vec<f64> {
        let mut times = Vec::new();
        let mut prev = false;
        for (i, on) in active.into_iter().enumerate() {
            if on && !prev {
                times.push(i as f64 / 100.0);
            }
            prev = on;
        }
        times
    };
    let flashes = onsets(luma.iter().map(|l| *l > 128).collect());
    let samples: Vec<f64> = pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b) as f64).collect();
    let beeps = onsets(
        samples
            .chunks(80)
            .map(|w| (w.iter().map(|s| s * s).sum::<f64>() / w.len() as f64).sqrt() > 12000.0)
            .collect(),
    );
    // Source seconds 4..8 land at output 0.5, 1.5, 2.5, 3.5 — the clip's own
    // 3.5 s head start is gone, the 1-second beat is not.
    let want = [0.5, 1.5, 2.5, 3.5, 4.5];
    let near = |times: &[f64]| want.iter().filter(|w| times.iter().any(|t| (*t - **w).abs() < 0.08)).count();
    assert!(near(&flashes) >= 4, "flashes at {flashes:?}, wanted {want:?}");
    assert!(near(&beeps) >= 4, "beeps at {beeps:?}, wanted {want:?}");
    for f in flashes.iter().filter(|f| **f < 4.9) {
        let nearest = beeps.iter().map(|b| (b - f).abs()).fold(f64::MAX, f64::min);
        assert!(nearest < 0.08, "flash at {f} has no beep within 80ms (beeps {beeps:?})");
    }
}
