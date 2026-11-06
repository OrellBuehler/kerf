//! The text the two probes run is the text the export and the still build: `kerf_core::opacity_filter` and `kerf_core::blur_filter` are
//! found, byte for byte, in the argv `render_with` and `export_still` hand to FFmpeg, and the filters around them are the conversions the
//! probes put on either side. Its own binary: it points `KERF_FFMPEG` at a wrapper that records every argv (and the contents of a
//! spilled filter graph) before running the real build.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_z_text -- --ignored --nocapture
//! ```

#![cfg(unix)]
#![allow(clippy::print_stderr)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use kerf_core::{
    Clip, Container, Delivery, ExportOptions, Fit, ImageFormat, Project, RateControl, StreamKind, Timeline, Track, VideoEffect,
};

fn make(real: &str, path: &Path, lavfi: &str) {
    let out = Command::new(real)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            lavfi,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

fn timeline(tracks: Vec<Vec<Clip>>) -> Timeline {
    Timeline {
        tracks: tracks
            .into_iter()
            .enumerate()
            .map(|(i, clips)| Track {
                clips,
                ..Track::new(StreamKind::Video, format!("V{}", i + 1))
            })
            .collect(),
        overlays: Vec::new(),
        svg_overlays: Vec::new(),
        markers: Vec::new(),
        format: Some(Delivery::new(320, 180, Fit::Cover)),
        master: Default::default(),
        thumbnails: Vec::new(),
    }
}

/// `context` characters either side of the first `needle` in `text`.
fn around(text: &str, needle: &str, context: usize) -> String {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is not in the graph:\n{text}"));
    let from = text[..at].char_indices().rev().nth(context).map_or(0, |(i, _)| i);
    let to = text[at + needle.len()..]
        .char_indices()
        .nth(context)
        .map_or(text.len(), |(i, _)| at + needle.len() + i);
    text[from..to].to_string()
}

#[test]
#[ignore = "needs ffmpeg"]
fn z_the_probes_text_is_the_text_the_export_and_the_still_run() {
    let real = kerf_core::ffmpeg_path();
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-z-text");
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("argv.log");
    let _ = std::fs::remove_file(&log);
    let wrapper = dir.join("ffmpeg-logging.sh");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n{{ printf 'ARGV\\n'; prev=; for a in \"$@\"; do printf '%s\\n' \"$a\"; case \"$prev\" in -filter_complex_script|-/filter_complex) printf 'SCRIPT\\n'; cat \"$a\"; printf '\\nENDSCRIPT\\n';; esac; prev=$a; done; printf 'END\\n'; }} >> '{}'\nexec '{real}' \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    let a = dir.join("a.mp4");
    let b = dir.join("b.mp4");
    make(&real, &a, "testsrc2=size=320x180:rate=30:duration=1");
    make(&real, &b, "smptehdbars=size=320x180:rate=30:duration=1");
    let (pa, pb) = (Project::probe_asset(&a).unwrap(), Project::probe_asset(&b).unwrap());
    std::env::set_var("KERF_FFMPEG", &wrapper);

    let mut top = Clip::new(pa.id, 0.0, 0.5, 0.0);
    top.transform.opacity = 0.6;
    let mut blurred = Clip::new(pa.id, 0.0, 0.5, 0.0);
    blurred.effects = vec![VideoEffect::Blur { sigma: 3.0 }];
    let assets = [pb.clone(), pa];
    let translucent = timeline(vec![vec![Clip::new(pb.id, 0.0, 0.5, 0.0)], vec![top]]);
    let blur = timeline(vec![vec![Clip::new(pb.id, 0.0, 0.5, 0.0)], vec![blurred]]);
    let out = dir.join("out.mkv");
    let lossless = ExportOptions {
        container: Container::Mkv,
        video_codec: Some("libx264".into()),
        rate_control: RateControl::Lossless,
        fps: Some(30.0),
        ..ExportOptions::default()
    };
    kerf_core::render_with(&translucent, &assets, &out, &lossless).expect("export");
    let export_log = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_file(&log);
    kerf_core::export_still(
        &translucent,
        &assets,
        &ExportOptions::default(),
        0.2,
        &dir.join("still.png"),
        ImageFormat::Png,
        0,
    )
    .expect("still");
    let still_log = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_file(&log);
    kerf_core::render_with(&blur, &assets, &out, &lossless).expect("export");
    let blur_export = std::fs::read_to_string(&log).unwrap();
    let _ = std::fs::remove_file(&log);
    kerf_core::export_still(
        &blur,
        &assets,
        &ExportOptions::default(),
        0.2,
        &dir.join("still.png"),
        ImageFormat::Png,
        0,
    )
    .expect("still");
    let blur_still = std::fs::read_to_string(&log).unwrap();

    let opacity = kerf_core::opacity_filter(0.6);
    let gblur = kerf_core::blur_filter(3.0);
    assert_eq!(opacity, "colorchannelmixer=aa=0.6");
    assert_eq!(gblur, "gblur=sigma=3");
    for (what, text, needle) in [
        ("export", &export_log, &opacity),
        ("still", &still_log, &opacity),
        ("export", &blur_export, &gblur),
        ("still", &blur_still, &gblur),
    ] {
        let seen = around(text, needle, 90);
        eprintln!(
            "Z text: the {what} graph around `{needle}`:\n    ...{}...",
            seen.replace('\n', "\\n")
        );
    }
    // The conversion into the filter: the graph establishes alpha (`format=yuva420p`) right before it, as the probe's chain does.
    for (what, text) in [("export", &export_log), ("still", &still_log)] {
        assert!(
            text.contains(&format!("format=yuva420p,{opacity}")),
            "the {what} graph does not put `format=yuva420p` right before `{opacity}`"
        );
    }
    // And the way back: the next thing to see the picture is `overlay`, which takes `yuva420p` (the probe's closing `format=yuva420p`).
    for (what, text) in [("export", &export_log), ("still", &still_log)] {
        let after = &text[text.find(&opacity).unwrap() + opacity.len()..];
        let chain: String = after.chars().take_while(|c| *c != ';' && *c != '\n').collect();
        eprintln!("Z text: after the opacity filter in the {what} graph: `{chain}`");
        assert!(
            after.contains("overlay"),
            "the {what} graph has no overlay after the opacity filter"
        );
    }
}
