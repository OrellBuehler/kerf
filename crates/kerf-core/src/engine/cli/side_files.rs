//! What rides along with an export besides the picture and the mix: the cut's
//! captions as a **soft subtitle stream** (`ExportOptions::embed_captions`) and/or
//! as files **beside** the export (`caption_sidecars`), and its chapter markers as
//! the file's **chapters**.
//!
//! The builders stay pure: the render writes the captions (SubRip) and chapters
//! (FFMETADATA) to temporary files first ([`prepare`]) and hands their paths in
//! through the options' render-only fields, and the builder then adds one input
//! each (`-f srt -i …`, `-f ffmetadata -i …`) after the clips and titles, maps the
//! subtitle stream with the container's codec (`mov_text` / `webvtt` / `srt`) and
//! takes the chapters with `-map_chapters`. Nothing is added when the fields are
//! unset, so every other argv — the golden oracle's included — is unchanged.
//!
//! Captions and chapters come off the timeline **as rendered** (`for_render`, and
//! the range slice), so a range export's subtitles start at its own zero.

use std::path::{Path, PathBuf};

use super::{effective_range, Container, ExportOptions};
use crate::captions_export::{cut_captions, write_captions, SubtitleFormat};
use crate::error::{Error, Result};
use crate::interchange::markers::ffmetadata_chapters;
use crate::model::Timeline;

/// The subtitle codec a container carries, or `None` when it carries none.
pub(crate) fn subtitle_codec(c: Container) -> Option<&'static str> {
    match c {
        Container::Mp4 | Container::Mov => Some("mov_text"),
        Container::Webm => Some("webvtt"),
        Container::Mkv => Some("srt"),
        _ => None,
    }
}

/// Whether a container keeps chapters.
fn carries_chapters(c: Container) -> bool {
    matches!(
        c,
        Container::Mp4 | Container::Mov | Container::Mkv | Container::Webm | Container::M4a
    )
}

/// The `-f` of an input this module adds.
pub(super) fn is_side_format(f: &str) -> bool {
    matches!(f, "srt" | "ffmetadata")
}

/// The inputs [`push_inputs`] added, for [`push_maps`].
#[derive(Debug, Default)]
pub(super) struct SideInputs {
    subtitles: Option<(usize, &'static str)>,
    chapters: Option<usize>,
}

/// Add the captions and chapters inputs the options name, after every input already in `args`.
pub(super) fn push_inputs(opts: &ExportOptions, args: &mut Vec<String>) -> SideInputs {
    let mut next = args.iter().filter(|a| *a == "-i").count();
    let mut side = SideInputs::default();
    if let (Some(path), Some(codec)) = (opts.subtitle_input.as_deref(), subtitle_codec(opts.container)) {
        args.extend(["-f", "srt", "-i", path].map(String::from));
        side.subtitles = Some((next, codec));
        next += 1;
    }
    if let Some(path) = opts.chapters_input.as_deref().filter(|_| carries_chapters(opts.container)) {
        args.extend(["-f", "ffmetadata", "-i", path].map(String::from));
        side.chapters = Some(next);
    }
    side
}

/// Map what [`push_inputs`] added into the output.
pub(super) fn push_maps(side: &SideInputs, args: &mut Vec<String>) {
    if let Some((input, codec)) = side.subtitles {
        args.extend([
            "-map".to_string(),
            format!("{input}:s:0"),
            "-c:s".to_string(),
            codec.to_string(),
        ]);
    }
    if let Some(input) = side.chapters {
        args.extend(["-map_chapters".to_string(), input.to_string()]);
    }
}

/// The temporary files of one export, removed when it is over.
#[derive(Debug, Default)]
pub(super) struct SideFiles(Vec<PathBuf>);

impl Drop for SideFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl SideFiles {
    fn write(&mut self, ext: &str, text: &str) -> Result<String> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("kerf-side-{}-{seq}.{ext}", std::process::id()));
        std::fs::write(&path, text).map_err(|e| Error::Engine(format!("could not write {}: {e}", path.display())))?;
        self.0.push(path.clone());
        Ok(path.to_string_lossy().into_owned())
    }
}

/// The timeline the builder renders: `for_render`, sliced to the range.
fn rendered_view(timeline: &Timeline, opts: &ExportOptions) -> Timeline {
    let rendered = timeline.for_render();
    match effective_range(&rendered, opts) {
        Some((s, e)) => rendered.slice(s, e),
        None => rendered,
    }
}

/// Write the captions and chapters an export muxes in, returning the options that
/// name them and the files (removed on drop). Refuses captions asked for when the
/// exported cut has none.
pub(super) fn prepare(timeline: &Timeline, opts: &ExportOptions) -> Result<(ExportOptions, SideFiles)> {
    let mut out = ExportOptions {
        subtitle_input: None,
        chapters_input: None,
        ..opts.clone()
    };
    let mut files = SideFiles::default();
    let wants_captions = opts.embed_captions || !opts.caption_sidecars.is_empty();
    let chapters = carries_chapters(opts.container);
    if !wants_captions && !chapters {
        return Ok((out, files));
    }
    let view = rendered_view(timeline, opts);
    if wants_captions {
        let cues = cut_captions(&view);
        if cues.is_empty() {
            return Err(Error::InvalidArgument(
                "the exported cut has no captions; generate or import captions first".to_string(),
            ));
        }
        if opts.embed_captions && subtitle_codec(opts.container).is_some() {
            out.subtitle_input = Some(files.write("srt", &write_captions(&cues, SubtitleFormat::Srt))?);
        }
    }
    if chapters {
        if let Some(meta) = ffmetadata_chapters(&view.markers, view.duration()) {
            out.chapters_input = Some(files.write("txt", &meta)?);
        }
    }
    Ok((out, files))
}

/// Where a caption file of `format` goes beside `output`.
fn sidecar_path(output: &Path, format: SubtitleFormat) -> PathBuf {
    output.with_extension(format.ext())
}

/// Write the caption files beside `output` the options ask for, returning their paths.
pub(super) fn write_sidecars(timeline: &Timeline, output: &Path, opts: &ExportOptions) -> Result<Vec<PathBuf>> {
    let mut written: Vec<PathBuf> = Vec::new();
    if opts.caption_sidecars.is_empty() {
        return Ok(written);
    }
    let cues = cut_captions(&rendered_view(timeline, opts));
    for &format in &opts.caption_sidecars {
        let path = sidecar_path(output, format);
        if written.contains(&path) || path == output {
            continue;
        }
        if let Err(e) = std::fs::write(&path, write_captions(&cues, format)) {
            for p in &written {
                let _ = std::fs::remove_file(p);
            }
            return Err(Error::InvalidArgument(format!("cannot write {}: {e}", path.display())));
        }
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::super::{build_export_args, validate_export};
    use super::*;
    use crate::engine::test_support::{av_asset, make_clip, timeline_of, video_track};
    use crate::model::{Marker, MarkerKind, TextOverlay};
    use uuid::Uuid;

    fn captioned() -> (Timeline, Vec<crate::model::Asset>) {
        let asset = av_asset(Uuid::new_v4(), 10.0);
        let mut tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 4.0, 0.0)])]);
        tl.overlays.push(TextOverlay {
            generated: true,
            ..TextOverlay::new("hello", 0.5, 1.5)
        });
        tl.markers.push(Marker {
            time: 1.0,
            name: "Part one".into(),
            kind: MarkerKind::Chapter,
            ..Default::default()
        });
        (tl, vec![asset])
    }

    #[test]
    fn nothing_is_added_while_the_render_names_no_side_file() {
        let (tl, assets) = captioned();
        let opts = ExportOptions {
            embed_captions: true,
            caption_sidecars: vec![SubtitleFormat::Vtt],
            ..ExportOptions::default()
        };
        let plain = build_export_args(&tl, &assets, "out.mp4", &ExportOptions::default()).unwrap();
        assert_eq!(build_export_args(&tl, &assets, "out.mp4", &opts).unwrap(), plain);
    }

    #[test]
    fn subtitles_and_chapters_follow_the_inputs_with_the_containers_codec() {
        let (tl, assets) = captioned();
        for (container, codec) in [
            (Container::Mp4, "mov_text"),
            (Container::Webm, "webvtt"),
            (Container::Mkv, "srt"),
        ] {
            let opts = ExportOptions {
                container,
                subtitle_input: Some("/t/subs.srt".into()),
                chapters_input: Some("/t/chapters.txt".into()),
                ..ExportOptions::default()
            };
            let args = build_export_args(&tl, &assets, "out", &opts).unwrap().join(" ");
            assert!(
                args.contains("-i /media/clip.mp4 -f srt -i /t/subs.srt -f ffmetadata -i /t/chapters.txt -filter_complex"),
                "{args}"
            );
            assert!(
                args.contains(&format!("-map [outa] -map 1:s:0 -c:s {codec} -map_chapters 2 ")),
                "{args}"
            );
        }
        let gif = ExportOptions {
            container: Container::Gif,
            subtitle_input: Some("/t/subs.srt".into()),
            chapters_input: Some("/t/chapters.txt".into()),
            ..ExportOptions::default()
        };
        let args = build_export_args(&tl, &assets, "out.gif", &gif).unwrap().join(" ");
        assert!(!args.contains("subs.srt") && !args.contains("chapters.txt"), "{args}");
    }

    #[test]
    fn prepare_writes_what_the_options_ask_for_and_cleans_up() {
        let (tl, _) = captioned();
        let opts = ExportOptions {
            embed_captions: true,
            ..ExportOptions::default()
        };
        let (out, files) = prepare(&tl, &opts).unwrap();
        let subs = out.subtitle_input.clone().unwrap();
        let chapters = out.chapters_input.unwrap();
        assert_eq!(
            std::fs::read_to_string(&subs).unwrap(),
            "1\n00:00:00,500 --> 00:00:01,500\nhello\n\n"
        );
        assert!(std::fs::read_to_string(&chapters)
            .unwrap()
            .contains("START=1000\nEND=4000\ntitle=Part one"));
        drop(files);
        assert!(!Path::new(&subs).exists() && !Path::new(&chapters).exists());

        // A cut with no chapter marker and no captions asked for adds nothing.
        let mut plain = tl;
        plain.markers.clear();
        let (out, _files) = prepare(&plain, &ExportOptions::default()).unwrap();
        assert_eq!((out.subtitle_input, out.chapters_input), (None, None));

        // Captions asked for and none in the cut is refused.
        plain.overlays.clear();
        assert!(prepare(&plain, &opts).is_err());
        let gif = ExportOptions {
            container: Container::Gif,
            embed_captions: true,
            ..ExportOptions::default()
        };
        assert!(validate_export(&gif, true, false)
            .iter()
            .any(|i| i.contains("can't carry subtitles")));
    }

    #[test]
    fn sidecars_sit_beside_the_export() {
        let (tl, _) = captioned();
        let dir = std::env::temp_dir().join(format!("kerf-sidecars-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let opts = ExportOptions {
            caption_sidecars: vec![SubtitleFormat::Srt, SubtitleFormat::Vtt, SubtitleFormat::Srt],
            ..ExportOptions::default()
        };
        let written = write_sidecars(&tl, &dir.join("cut.mp4"), &opts).unwrap();
        assert_eq!(written, vec![dir.join("cut.srt"), dir.join("cut.vtt")]);
        assert!(std::fs::read_to_string(dir.join("cut.vtt"))
            .unwrap()
            .starts_with("WEBVTT\n\n00:00:00.500"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Renders a caption and a chapter into mp4, mkv and webm and asks ffprobe what came out.
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn ffprobe_sees_the_subtitle_stream_and_the_chapters() {
        use super::super::{command, ffmpeg_bin, ffprobe_bin, render_with};
        use crate::engine::test_support::StatusBounded;
        use crate::project::Project;

        let dir = std::env::temp_dir().join(format!("kerf-side-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("av.mkv");
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc=s=64x36:r=25:d=3")
            .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "pcm_s16le"])
            .arg(&src)
            .status_bounded()
            .unwrap();
        assert!(made.success());
        let asset = Project::probe_asset(&src).unwrap();
        let mut tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, 3.0, 0.0)])]);
        tl.overlays.push(TextOverlay {
            generated: true,
            ..TextOverlay::new("hello there", 0.5, 1.5)
        });
        for (time, name) in [(0.0, "Intro"), (1.5, "Part two")] {
            tl.markers.push(Marker {
                time,
                name: name.into(),
                kind: MarkerKind::Chapter,
                ..Default::default()
            });
        }
        for (container, codec) in [
            (Container::Mp4, "mov_text"),
            (Container::Mkv, "subrip"),
            (Container::Webm, "webvtt"),
        ] {
            let out = dir.join(format!("cut.{}", container.ext()));
            let opts = ExportOptions {
                container,
                embed_captions: true,
                caption_sidecars: vec![SubtitleFormat::Srt],
                ..ExportOptions::default()
            };
            render_with(&tl, std::slice::from_ref(&asset), &out, &opts).unwrap();
            let probe = command(&ffprobe_bin())
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "stream=codec_type,codec_name:chapter=start_time:chapter_tags=title",
                ])
                .args(["-of", "compact"])
                .arg(&out)
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&probe.stdout);
            assert!(
                text.contains(&format!("codec_name={codec}|codec_type=subtitle")),
                "{container:?}: {text}"
            );
            assert!(
                text.contains("tag:title=Intro") && text.contains("tag:title=Part two"),
                "{container:?}: {text}"
            );
            let srt = std::fs::read_to_string(dir.join("cut.srt")).unwrap();
            assert!(srt.contains("00:00:00,500 --> 00:00:01,500\nhello there"), "{srt}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
