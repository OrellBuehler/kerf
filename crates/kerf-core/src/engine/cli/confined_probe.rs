//! A confined run (`paths::confine_demuxers`) holds its probes to the media demuxers: the argv says
//! so only then, and the real `ffprobe` refuses a playlist or a manifest before it opens anything it
//! names, wherever in the file the manifest's tell sits.

use std::path::{Path, PathBuf};

use super::{ffmpeg_bin, probe_args, probe_formats};
use crate::paths::MEDIA_DEMUXERS;

#[test]
fn only_a_confined_probe_names_the_demuxers() {
    let path = Path::new("/media/clip.mp4");
    let plain: Vec<String> = probe_args(path, None)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        plain,
        [
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
            "/media/clip.mp4"
        ],
        "the argv of every other probe is what it was"
    );
    let confined: Vec<String> = probe_args(path, Some(MEDIA_DEMUXERS))
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let at = confined
        .iter()
        .position(|a| a == "-format_whitelist")
        .expect("held to the demuxers");
    assert_eq!(confined[at + 1], MEDIA_DEMUXERS);
    assert_eq!(
        confined.last().map(String::as_str),
        Some("/media/clip.mp4"),
        "before the input"
    );
    for reference in ["hls", "dash", "concat", "sdp", "imf", "mxf", "svg_pipe", "applehttp"] {
        assert!(!MEDIA_DEMUXERS.split(',').any(|d| d == reference), "{reference}");
    }
    assert!(crate::paths::demuxer_whitelist().is_none(), "off unless a host turns it on");
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kerf-confined-probe-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn make(dir: &Path, name: &str, args: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let status = std::process::Command::new(ffmpeg_bin())
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(&out)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "{name}");
    out
}

fn confined(path: &Path) -> crate::error::Result<super::ProbeResult> {
    probe_formats(path, None, Some(MEDIA_DEMUXERS))
}

#[test]
#[ignore = "runs the ffmpeg and ffprobe binaries"]
fn a_confined_probe_takes_media_and_refuses_playlists_and_manifests() {
    let dir = scratch();
    let outside = scratch();
    let secret = make(&outside, "secret.wav", &["-f", "lavfi", "-i", "sine=d=1"]);
    let mp4 = make(
        &dir,
        "clip.mp4",
        &["-f", "lavfi", "-i", "testsrc=d=1:s=64x64", "-pix_fmt", "yuv420p"],
    );
    let wav = make(&dir, "tone.wav", &["-f", "lavfi", "-i", "sine=d=1"]);
    let png = make(
        &dir,
        "still.png",
        &["-f", "lavfi", "-i", "color=red:s=16x16", "-frames:v", "1"],
    );
    for ok in [&mp4, &wav, &png] {
        assert!(confined(ok).is_ok(), "{}: {:?}", ok.display(), confined(ok).err());
    }
    let secret = secret.display();
    let hls = dir.join("list.m3u8");
    std::fs::write(
        &hls,
        format!("#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.0,\n{secret}\n#EXT-X-ENDLIST\n"),
    )
    .unwrap();
    // A DASH manifest named like a video, its tell past the first 4 KiB.
    let dash = dir.join("padded.mp4");
    let padding = "x".repeat(6000);
    std::fs::write(
        &dash,
        format!(
            "<?xml version=\"1.0\"?><!-- {padding} --><MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" \
             mediaPresentationDuration=\"PT1S\" profiles=\"urn:mpeg:dash:profile:isoff-on-demand:2011\"><Period>\
             <AdaptationSet mimeType=\"audio/wav\"><Representation id=\"a\" bandwidth=\"1\"><BaseURL>file://{secret}</BaseURL>\
             </Representation></AdaptationSet></Period></MPD>"
        ),
    )
    .unwrap();
    let concat = dir.join("list.mp4");
    std::fs::write(&concat, format!("ffconcat version 1.0\nfile '{secret}'\n")).unwrap();
    for bad in [&hls, &dash, &concat] {
        let why = confined(bad).map(|_| ()).unwrap_err().to_string();
        assert!(why.contains("whitelist"), "{}: {why}", bad.display());
    }
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&outside).ok();
}
