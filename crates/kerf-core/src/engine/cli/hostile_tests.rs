//! What ffprobe and ffmpeg print, mutated: the probe's JSON, the source-traits probe, and the
//! silence and scene logs never panic, whatever a damaged file or a different build makes them say.

use super::*;
use crate::hostile::assert_no_panic;

const ROUNDS: usize = 1500;

const PHONE: &str = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":1920,"height":1080,"r_frame_rate":"30000/1001","avg_frame_rate":"29970/1000","duration":"12.345","pix_fmt":"yuv420p10le","color_transfer":"arib-std-b67","color_primaries":"bt2020","color_space":"bt2020nc","tags":{"rotate":"90"},"side_data_list":[{"side_data_type":"Display Matrix","rotation":-90.0}]},{"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"duration":"12.3"}],"format":{"duration":"12.345000"}}"#;
const INSV: &str = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc","width":5760,"height":2880,"r_frame_rate":"30/1","side_data_list":[{"side_data_type":"Spherical Mapping","projection":"equirectangular"}]}],"format":{"duration":"20.0"}}"#;
const STILL: &str = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"png","width":640,"height":480,"r_frame_rate":"25/1","avg_frame_rate":"0/0"}],"format":{"duration":"0.040000"}}"#;

fn probe_bytes(bytes: &[u8]) {
    if let Ok(parsed) = serde_json::from_slice::<ProbeJson>(bytes) {
        let r = probe_from_json(parsed, Some(Path::new("/media/VID_0001.insv")));
        for s in &r.streams {
            let _ = s.has_alpha();
        }
    }
    if let Ok(parsed) = serde_json::from_slice::<ProbeJson>(bytes) {
        let _ = probe_from_json(parsed, None);
    }
}

#[test]
fn hostile_probe_json_never_panics() {
    assert_no_panic(
        "ffprobe -show_streams",
        &[PHONE.as_bytes(), INSV.as_bytes(), STILL.as_bytes()],
        ROUNDS,
        probe_bytes,
    );
}

/// `h * 2` overflowed `u32` (a panic in a debug build) for an Insta360 file whose probe said a height
/// of 2^31 or more.
#[test]
fn a_huge_insta360_height_is_flat_not_an_overflow() {
    for (w, h) in [(0u32, 2_147_483_648u32), (u32::MAX, u32::MAX), (4_294_967_294, 2_147_483_647)] {
        let json = format!(
            r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"hevc","width":{w},"height":{h}}}],"format":{{"duration":"1.0"}}}}"#
        );
        let r = probe_from_json(serde_json::from_str(&json).unwrap(), Some(Path::new("/media/a.insv")));
        let want = (u64::from(w) == u64::from(h) * 2).then_some(Projection::DualFisheye);
        assert_eq!(r.streams[0].projection, want, "{w}x{h}");
    }
}

#[test]
fn hostile_source_traits_never_panic() {
    let json = r#"{"programs":[],"streams":[{"codec_name":"h264","pix_fmt":"yuv420p","color_transfer":"smpte2084","start_time":"0.080013"}],"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","start_time":"0.000000"}}"#;
    assert_no_panic("ffprobe source traits", &[json.as_bytes()], ROUNDS, |b| {
        let _ = parse_source_traits(&String::from_utf8_lossy(b));
    });
}

#[test]
fn hostile_silence_and_scene_logs_never_panic() {
    let silence = "[silencedetect @ 0x55] silence_start: 1.25\n[silencedetect @ 0x55] silence_end: 2.5 | silence_duration: 1.25\n[silencedetect @ 0x55] silence_start: -0.001\n";
    let scenes = "[Parsed_showinfo_1 @ 0x1] n:   0 pts:      0 pts_time:0       duration:1\n[Parsed_showinfo_1 @ 0x1] n:   1 pts:  90000 pts_time:3.0     duration:1\n[Parsed_showinfo_1 @ 0x1] n:   2 pts: 180000 pts_time:6.25\n";
    assert_no_panic(
        "silence and scene logs",
        &[silence.as_bytes(), scenes.as_bytes()],
        ROUNDS,
        |b| {
            let text = String::from_utf8_lossy(b);
            let _ = parse_silence(&text);
            let _ = parse_scenes(&text);
        },
    );
}

/// What the stabilization passes read back (the detect pass's `showinfo` log, vid.stab's transforms) and
/// the analysis kept on disk, mutated: no panic, and an analysis that reads is either refused or sliced.
#[test]
fn hostile_stabilization_logs_and_analyses_never_panic() {
    let log = "[Parsed_showinfo_2 @ 0x1] config in time_base: 1/15360, frame_rate: 30/1\n[Parsed_showinfo_2 @ 0x1] n:   0 pts:      0 pts_time:0 duration:512 fmt:yuv420p sar:1/1 s:560x300 i:P\n[Parsed_showinfo_2 @ 0x1] n:   1 pts:    512 pts_time:0.0333 s:560x300\n";
    let transforms = "0 0 0 0 0 1\n# no fields\n0 1.938609 -3.997462 0.003857 0.509595 0\n#\t\t 1.78 2\n";
    assert_no_panic("stabilization logs", &[log.as_bytes(), transforms.as_bytes()], ROUNDS, |b| {
        let text = String::from_utf8_lossy(b);
        let _ = stabilize::parse_frames(&text, 1.5);
        let _ = stabilize::parse_transforms(&text);
    });
    let analysis = serde_json::to_vec(&crate::stabilize::pinned::shaky(1.0, 30.0, 560, 300)).unwrap();
    let asset = uuid::Uuid::from_u128(5);
    assert_no_panic("stabilization analysis", &[&analysis], ROUNDS / 3, |b| {
        let Ok(a) = serde_json::from_slice::<crate::stabilize::MotionAnalysis>(b) else {
            return;
        };
        if !a.is_sound() {
            return;
        }
        let mut clip = Clip::new(asset, 0.1, 0.9, 0.0);
        clip.stabilize = Some(crate::model::Stabilize::default());
        let analyses = std::collections::HashMap::from([(asset, std::sync::Arc::new(a))]);
        crate::stabilize::pinned::with(analyses, || {
            for window in [(0.1, 0.9), (0.0, 0.0), (0.5, 0.4), (-1.0, 99.0)] {
                let setting = crate::model::Stabilize::default();
                if let Some(s) = crate::stabilize::slice_for(&clip, &setting, window, None, None) {
                    assert!(s.start.is_finite() && s.zoom.is_finite(), "{s:?}");
                }
            }
        });
    });
}
