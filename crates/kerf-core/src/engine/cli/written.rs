//! What an export wrote, read back from the file.
//!
//! `get_levels` measures the timeline's mix; the file is what a platform judges, and a lossy
//! encode moves it (an AAC re-encode raised a limited peak back over its ceiling — an agent had to
//! run `ffmpeg -af ebur128=peak=true` itself to find out). After a successful export
//! [`measure_export`] probes the file (container, picture, sound, sizes, rates) and, when it has
//! sound, meters its first audio stream for integrated loudness, true peak and loudness range
//! with the same `ebur128` machinery the levels pass uses — `-vn`, so only the sound is decoded.
//!
//! **A measurement never fails an export**: the file is written and valid, so whatever could not
//! be read says why in [`ExportReport::problem`] and the rest stands. It is part of the export's job
//! (it reports `ExportStage::Measuring` and stops on the same cancel — which leaves the finished file
//! alone and says the measurement was stopped), reads the file the export just wrote (hot in the page
//! cache) and is ungated like the other reads an agent looks at: thread-capped and niced, not taking
//! the heavy-job slot, so a proxy that started meanwhile cannot hold a finished export's answer for
//! minutes.

use serde::{Deserialize, Serialize};

use super::levels::run_meters;
use super::*;
use crate::model::LevelReading;

/// How long `ffprobe` may take over the file just written. Headers of a local file: an answer in
/// milliseconds, so this is a share that went away.
const WRITTEN_PROBE_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The instance name the file's meter logs under.
const FILE_METER: &str = "file";

/// What the report says when the measurement was stopped.
const STOPPED: &str = "the measurement was stopped before it finished (the file is complete)";

/// What an export wrote — see the module docs. Every field is `None` where it could not be read or
/// does not apply (a picture-only file has no `audio` and no `loudness`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportReport {
    /// The file.
    pub path: String,
    pub size_bytes: Option<u64>,
    /// ffprobe's format name (`mov,mp4,m4a,3gp,3g2,mj2`, `matroska,webm`, `wav` …).
    pub container: Option<String>,
    pub duration_secs: Option<f64>,
    /// The whole file's bit rate, bits per second.
    pub bit_rate: Option<u64>,
    pub video: Option<WrittenVideo>,
    pub audio: Option<WrittenAudio>,
    /// The file's own audio, metered: integrated loudness, loudness range, sample and true peak.
    pub loudness: Option<LevelReading>,
    /// The loudness target the export was asked for, if it was asked for one.
    pub target: Option<LoudnessTarget>,
    /// How far the file's true peak is over the target's ceiling, in dB to a tenth — only when it
    /// is over it (what a lossy encode can do to a limited mix).
    pub true_peak_over_db: Option<f64>,
    /// Why part of the measurement is missing: the file could not be probed or metered, or the
    /// measurement was stopped. The export itself succeeded.
    pub problem: Option<String>,
}

/// The first picture stream of the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WrittenVideo {
    pub codec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub pix_fmt: Option<String>,
    /// The stream's bit rate, bits per second, when the container records it.
    pub bit_rate: Option<u64>,
}

/// The first sound stream of the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WrittenAudio {
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// The stream's bit rate, bits per second, as the packets add up (not the `-b:a` asked for).
    pub bit_rate: Option<u64>,
}

/// A delivery's loudness target (a [`LoudnessPreset`]'s numbers).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LoudnessTarget {
    pub lufs: f64,
    /// The true-peak ceiling, dBTP.
    pub true_peak_dbtp: f64,
}

impl LoudnessTarget {
    pub fn of(preset: LoudnessPreset) -> Self {
        Self {
            lufs: preset.lufs(),
            true_peak_dbtp: preset.true_peak_db(),
        }
    }
}

/// The loudness target an export asked for: `loudness` wins over the `loudnorm` bool.
pub fn loudness_target(opts: &ExportOptions) -> Option<LoudnessPreset> {
    opts.loudness.or(opts.loudnorm.then_some(LoudnessPreset::Youtube))
}

/// Read back the file `path` an export wrote: probe it and meter its sound. `target` is what the
/// export was asked to reach (to judge the true peak against its ceiling). `progress` hears the
/// fraction of the sound metered, `0.0..=1.0`; `cancel` stops the measurement (not the file). Never
/// fails — see the module docs.
pub fn measure_export(
    path: &Path,
    target: Option<LoudnessPreset>,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> bool,
) -> ExportReport {
    let mut report = ExportReport {
        path: path.to_string_lossy().into_owned(),
        size_bytes: std::fs::metadata(path).ok().map(|m| m.len()),
        container: None,
        duration_secs: None,
        bit_rate: None,
        video: None,
        audio: None,
        loudness: None,
        target: target.map(LoudnessTarget::of),
        true_peak_over_db: None,
        problem: None,
    };
    if cancel() {
        report.problem = Some(STOPPED.to_string());
        return report;
    }
    match probe_written(path, cancel) {
        Ok(json) => json.describe(&mut report),
        Err(Error::Cancelled) => {
            report.problem = Some(STOPPED.to_string());
            return report;
        }
        Err(e) => {
            tracing::warn!(error = %e, file = %path.display(), "could not probe the file an export wrote");
            report.problem = Some(format!("the file could not be probed: {e}"));
            return report;
        }
    }
    if report.audio.is_some() {
        let seconds = report.duration_secs.unwrap_or(0.0);
        match meter_written(path, seconds, progress, cancel) {
            Ok(reading) => report.loudness = Some(reading),
            Err(Error::Cancelled) => report.problem = Some(STOPPED.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, file = %path.display(), "could not meter the file an export wrote");
                report.problem = Some(format!("the file's loudness could not be measured: {e}"));
            }
        }
    }
    report.true_peak_over_db = true_peak_over(
        report.loudness.and_then(|l| l.true_peak_dbtp),
        report.target.map(|t| t.true_peak_dbtp),
    );
    report
}

/// How far a true peak is over a ceiling, in dB to a tenth, or `None` when it is not over it or
/// either is unknown (pure). Compared as it is shown — one decimal — so a reading that prints as the
/// ceiling is not flagged over it.
pub(crate) fn true_peak_over(true_peak_dbtp: Option<f64>, ceiling_dbtp: Option<f64>) -> Option<f64> {
    let tenth = |v: f64| (v * 10.0).round() / 10.0;
    let (peak, ceiling) = (tenth(true_peak_dbtp?), tenth(ceiling_dbtp?));
    (peak > ceiling).then(|| tenth(peak - ceiling))
}

// ---- the probe --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct WrittenJson {
    #[serde(default)]
    streams: Vec<WrittenStream>,
    #[serde(default)]
    format: Option<WrittenFormat>,
}

#[derive(Deserialize)]
struct WrittenFormat {
    format_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Deserialize)]
struct WrittenStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    pix_fmt: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    bit_rate: Option<String>,
    duration: Option<String>,
}

impl WrittenJson {
    /// Fill `report` from what ffprobe said (pure, unit-tested over its JSON).
    fn describe(&self, report: &mut ExportReport) {
        let number = |t: &Option<String>| {
            t.as_deref()
                .and_then(|t| t.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite())
        };
        let whole = |t: &Option<String>| number(t).filter(|v| *v > 0.0).map(|v| v.round() as u64);
        let first = |kind: &str| self.streams.iter().find(|s| s.codec_type.as_deref() == Some(kind));
        if let Some(format) = &self.format {
            report.container = format.format_name.clone().filter(|n| !n.is_empty());
            report.duration_secs = number(&format.duration).filter(|d| *d >= 0.0);
            report.bit_rate = whole(&format.bit_rate);
            report.size_bytes = whole(&format.size).or(report.size_bytes);
        }
        if let Some(v) = first("video") {
            let rate = |t: &Option<String>| t.as_deref().and_then(parse_rational).filter(|r| r.is_finite() && *r > 0.0);
            report.video = Some(WrittenVideo {
                codec: v.codec_name.clone().unwrap_or_default(),
                width: v.width,
                height: v.height,
                // A constant-rate file says it twice; a variable one has only the average.
                fps: rate(&v.avg_frame_rate).or_else(|| rate(&v.r_frame_rate)),
                pix_fmt: v.pix_fmt.clone().filter(|p| !p.is_empty() && p != "none"),
                bit_rate: whole(&v.bit_rate),
            });
        }
        if let Some(a) = first("audio") {
            report.audio = Some(WrittenAudio {
                codec: a.codec_name.clone().unwrap_or_default(),
                sample_rate: a.sample_rate.as_deref().and_then(|r| r.trim().parse().ok()),
                channels: a.channels,
                bit_rate: whole(&a.bit_rate),
            });
            // Some containers (a raw stream) have no format duration: the sound's own will do.
            if report.duration_secs.is_none() {
                report.duration_secs = number(&a.duration).filter(|d| *d >= 0.0);
            }
        }
        // A format without a bit rate of its own: what the size says over the length.
        if report.bit_rate.is_none() {
            if let (Some(bytes), Some(secs)) = (report.size_bytes, report.duration_secs.filter(|d| *d > 0.0)) {
                report.bit_rate = Some((bytes as f64 * 8.0 / secs).round() as u64);
            }
        }
    }
}

fn probe_written(path: &Path, cancel: &dyn Fn() -> bool) -> Result<WrittenJson> {
    let bin = ffprobe_bin();
    let mut cmd = command(&bin);
    cmd.args(["-v", "error", "-show_format", "-show_streams", "-of", "json"])
        .arg(path);
    let output = output_bounded_cancellable(cmd, WRITTEN_PROBE_LIMIT, Some(cancel))
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::TimedOut => Error::Engine(format!(
                "ffprobe did not answer in {} s and was stopped",
                WRITTEN_PROBE_LIMIT.as_secs()
            )),
            _ => launch_err(&bin, e),
        })?
        .ok_or(Error::Cancelled)?;
    if !output.status.success() {
        return Err(Error::Engine(format!(
            "ffprobe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| Error::Engine(format!("could not parse ffprobe output: {e}")))
}

// ---- the meter --------------------------------------------------------------------------------

/// The argv that meters the first sound stream of `path` (pure): `-vn`, so no picture is decoded, and
/// one `ebur128` tap with sample and true peak, to a null sink.
fn build_meter_args(path: &str) -> Vec<String> {
    [
        "-hide_banner",
        "-nostats",
        "-vn",
        "-sn",
        "-dn",
        "-i",
        path,
        "-map",
        "0:a:0",
        "-af",
        "ebur128@file=peak=sample+true",
        "-f",
        "null",
        "-",
    ]
    .map(String::from)
    .to_vec()
}

fn meter_written(path: &Path, seconds: f64, progress: &mut dyn FnMut(f64), cancel: &dyn Fn() -> bool) -> Result<LevelReading> {
    let path = path
        .to_str()
        .ok_or_else(|| Error::InvalidArgument(format!("non-UTF-8 output path: {}", path.display())))?;
    let mut args = build_meter_args(path);
    // Ungated, as `stream_mix_pcm` is: what an agent looks at, not a whole-file background job.
    cpu::cap_args(&mut args, cpu::shared_threads(cpu::budget_threads(), 2));
    let (parser, tail) = run_meters(args, "wr", seconds, progress, cancel)?;
    parser.reading(FILE_METER).ok_or_else(|| {
        Error::Engine(format!(
            "ffmpeg finished without reporting the file's loudness: {}",
            tail.trim()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What ffprobe 4.4.2 printed for the mp4 of the reported export (trimmed to what is read).
    const MP4: &str = r#"{
        "streams": [
            {"index": 0, "codec_name": "h264", "codec_type": "video", "width": 1920, "height": 1080,
             "pix_fmt": "yuv420p", "r_frame_rate": "30/1", "avg_frame_rate": "30/1", "bit_rate": "10293593", "duration": "31.500000"},
            {"index": 1, "codec_name": "aac", "codec_type": "audio", "sample_rate": "48000", "channels": 2,
             "bit_rate": "317375", "duration": "31.509333"}
        ],
        "format": {"format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "31.509000", "size": "40788249", "bit_rate": "10356000"}
    }"#;

    fn blank() -> ExportReport {
        ExportReport {
            path: "/out.mp4".into(),
            size_bytes: Some(1),
            container: None,
            duration_secs: None,
            bit_rate: None,
            video: None,
            audio: None,
            loudness: None,
            target: None,
            true_peak_over_db: None,
            problem: None,
        }
    }

    fn described(json: &str) -> ExportReport {
        let mut report = blank();
        serde_json::from_str::<WrittenJson>(json).unwrap().describe(&mut report);
        report
    }

    #[test]
    fn the_probe_reads_container_picture_sound_and_rates() {
        let r = described(MP4);
        assert_eq!(r.container.as_deref(), Some("mov,mp4,m4a,3gp,3g2,mj2"));
        assert_eq!(r.duration_secs, Some(31.509));
        assert_eq!(r.bit_rate, Some(10_356_000));
        assert_eq!(r.size_bytes, Some(40_788_249));
        let v = r.video.unwrap();
        assert_eq!(
            (v.codec.as_str(), v.width, v.height, v.fps, v.pix_fmt.as_deref(), v.bit_rate),
            ("h264", Some(1920), Some(1080), Some(30.0), Some("yuv420p"), Some(10_293_593))
        );
        let a = r.audio.unwrap();
        assert_eq!(
            (a.codec.as_str(), a.sample_rate, a.channels, a.bit_rate),
            ("aac", Some(48_000), Some(2), Some(317_375))
        );
    }

    #[test]
    fn a_sound_only_file_has_no_picture_and_a_picture_only_one_no_sound() {
        let wav = described(
            r#"{"streams":[{"codec_name":"pcm_s16le","codec_type":"audio","sample_rate":"44100","channels":1,"bit_rate":"705600"}],
                "format":{"format_name":"wav","duration":"4.000000","size":"352844","bit_rate":"705688"}}"#,
        );
        assert!(wav.video.is_none());
        assert_eq!(wav.audio.unwrap().channels, Some(1));
        assert_eq!(wav.container.as_deref(), Some("wav"));
        let silent = described(
            r#"{"streams":[{"codec_name":"h264","codec_type":"video","width":640,"height":360,"avg_frame_rate":"25/1","pix_fmt":"yuv420p"}],
                "format":{"format_name":"matroska,webm","duration":"2.0","size":"1000"}}"#,
        );
        assert!(silent.audio.is_none());
        let v = silent.video.unwrap();
        assert_eq!((v.fps, v.bit_rate), (Some(25.0), None));
        // No bit rate of its own: what the size says over the length (1000 B over 2 s).
        assert_eq!(silent.bit_rate, Some(4_000));
    }

    #[test]
    fn a_variable_rate_file_reports_its_average_and_nonsense_is_nothing() {
        let r = described(
            r#"{"streams":[{"codec_name":"h264","codec_type":"video","avg_frame_rate":"0/0","r_frame_rate":"60000/1001","pix_fmt":"none"},
                           {"codec_name":"aac","codec_type":"audio","sample_rate":"banana","bit_rate":"0"}],
                "format":{"format_name":"","duration":"N/A","size":"N/A","bit_rate":"N/A"}}"#,
        );
        let v = r.video.unwrap();
        assert!((v.fps.unwrap() - 59.94).abs() < 0.01);
        assert_eq!(v.pix_fmt, None);
        let a = r.audio.unwrap();
        assert_eq!((a.sample_rate, a.bit_rate), (None, None));
        assert_eq!((r.container, r.duration_secs, r.bit_rate), (None, None, None));
        // The size on disk stands when the container does not say.
        assert_eq!(r.size_bytes, Some(1));
    }

    #[test]
    fn the_true_peak_is_flagged_over_the_ceiling_by_what_it_shows() {
        // -0.4 dBTP against a -1.0 ceiling: 0.6 over.
        assert_eq!(true_peak_over(Some(-0.4), Some(-1.0)), Some(0.6));
        // Within, on the line as shown (-0.96 prints as -1.0), or unknown: not over.
        assert_eq!(true_peak_over(Some(-1.3), Some(-1.0)), None);
        assert_eq!(true_peak_over(Some(-0.96), Some(-1.0)), None);
        assert_eq!(true_peak_over(Some(-1.0), Some(-1.0)), None);
        assert_eq!(true_peak_over(None, Some(-1.0)), None);
        assert_eq!(true_peak_over(Some(0.2), None), None);
        // Just over, as shown.
        assert_eq!(true_peak_over(Some(-0.94), Some(-1.0)), Some(0.1));
    }

    #[test]
    fn the_meter_reads_only_the_sound_of_the_file() {
        let args = build_meter_args("/tmp/out.mp4");
        let at = |flag: &str| args.iter().position(|a| a == flag).unwrap();
        assert!(at("-vn") < at("-i"), "no picture is decoded: {args:?}");
        assert_eq!(args[at("-i") + 1], "/tmp/out.mp4");
        assert_eq!(args[at("-map") + 1], "0:a:0");
        assert!(args[at("-af") + 1].starts_with("ebur128@file="));
        assert!(args[at("-af") + 1].contains("peak=sample+true"));
        assert_eq!(args.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn the_target_is_the_presets_numbers_and_loudness_wins_over_the_bool() {
        let t = LoudnessTarget::of(LoudnessPreset::Broadcast);
        assert_eq!((t.lufs, t.true_peak_dbtp), (-23.0, -1.0));
        let opts = ExportOptions::default();
        assert_eq!(loudness_target(&opts), None);
        let norm = ExportOptions {
            loudnorm: true,
            ..ExportOptions::default()
        };
        assert_eq!(loudness_target(&norm), Some(LoudnessPreset::Youtube));
        let apple = ExportOptions {
            loudnorm: true,
            loudness: Some(LoudnessPreset::Apple),
            ..ExportOptions::default()
        };
        assert_eq!(loudness_target(&apple), Some(LoudnessPreset::Apple));
    }

    #[test]
    fn a_stopped_measurement_says_so_and_a_missing_file_says_why() {
        // Stopped before it began: nothing run, the finished file untouched.
        let r = measure_export(Path::new("/nonexistent/x.mp4"), None, &mut |_| {}, &|| true);
        assert_eq!(r.problem.as_deref(), Some(STOPPED));
        assert!(r.video.is_none() && r.loudness.is_none());
    }

    #[test]
    fn the_report_is_the_json_the_surfaces_read() {
        let mut r = described(MP4);
        r.target = Some(LoudnessTarget::of(LoudnessPreset::Youtube));
        r.loudness = Some(LevelReading {
            integrated_lufs: Some(-14.2),
            true_peak_dbtp: Some(-0.4),
            ..LevelReading::default()
        });
        r.true_peak_over_db = true_peak_over(Some(-0.4), Some(-1.0));
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["video"]["width"], 1920);
        assert_eq!(json["audio"]["sample_rate"], 48_000);
        assert_eq!(json["loudness"]["integrated_lufs"], -14.2);
        assert_eq!(json["target"]["true_peak_dbtp"], -1.0);
        assert_eq!(json["true_peak_over_db"], 0.6);
        assert!(json["problem"].is_null());
        assert_eq!(serde_json::from_value::<ExportReport>(json).unwrap(), r);
    }

    // ---- through the real ffmpeg ------------------------------------------------------------

    use crate::engine::test_support::{
        audio_stream, make_clip, test_asset, timeline_of, video_stream, video_track, StatusBounded,
    };

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-written-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// `secs` of a moving 640x360 / 30 fps picture with a stereo 1 kHz tone peaking at `peak_db`.
    fn source(dir: &Path, peak_db: f64, secs: f64) -> Asset {
        let path = dir.join("source.mkv");
        // `sine` peaks at 1/8 and is mono: `pan` copies it into both channels at full level.
        let gain = 10f64.powf(peak_db / 20.0) / 0.125;
        let made = command(&ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=s=640x360:r=30:d={secs}"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=f=1000:d={secs}:sample_rate=48000"))
            .args(["-af"])
            .arg(format!("pan=stereo|c0=c0|c1=c0,volume={gain:.9}"))
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "flac",
                "-shortest",
            ])
            .arg(&path)
            .status_bounded()
            .expect("run ffmpeg");
        assert!(made.success());
        let mut asset = test_asset(vec![video_stream(640, 360, 30.0), audio_stream(48_000, 2)]);
        asset.path = path.to_string_lossy().into_owned();
        asset.duration = secs;
        asset
    }

    fn export(dir: &Path, name: &str, asset: &Asset, opts: &ExportOptions) -> PathBuf {
        let tl = timeline_of(vec![video_track(vec![make_clip(asset.id, 0.0, asset.duration, 0.0)])]);
        let out = dir.join(name);
        let status = render_with_progress(&tl, std::slice::from_ref(asset), &out, opts, &mut |_| {}, &|| false).expect("export");
        assert_eq!(status, RenderStatus::Completed);
        out
    }

    /// The export of the report's story: a static -14 LUFS target into an AAC mp4. The file itself
    /// is measured — its streams as the probe finds them, its loudness on the target (the mix was
    /// 8 dB under it) — and the progress of the measurement rises to its end.
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn an_exported_file_is_probed_and_metered_from_the_file() {
        let dir = scratch("mp4");
        let asset = source(&dir, -22.0, 8.0);
        let opts = ExportOptions {
            container: Container::Mp4,
            video_codec: Some("libx264".into()),
            preset: Some("ultrafast".into()),
            audio_codec: Some("aac".into()),
            audio_bitrate: Some("192k".into()),
            faststart: true,
            loudness: Some(LoudnessPreset::Youtube),
            loudness_static: true,
            ..ExportOptions::default()
        };
        let out = export(&dir, "cut.mp4", &asset, &opts);
        let mut told: Vec<f64> = Vec::new();
        let r = measure_export(&out, loudness_target(&opts), &mut |f| told.push(f), &|| false);
        assert_eq!(r.problem, None, "{r:?}");
        assert!(r.container.as_deref().unwrap().contains("mp4"), "{:?}", r.container);
        let v = r.video.as_ref().expect("video");
        assert_eq!(
            (v.codec.as_str(), v.width, v.height, v.pix_fmt.as_deref()),
            ("h264", Some(640), Some(360), Some("yuv420p"))
        );
        assert!((v.fps.unwrap() - 30.0).abs() < 0.01);
        let a = r.audio.as_ref().expect("audio");
        assert_eq!((a.codec.as_str(), a.sample_rate, a.channels), ("aac", Some(48_000), Some(2)));
        // What the packets add up to, not the 192k asked for (a pure tone compresses far under it).
        assert!(
            a.bit_rate.is_some_and(|b| (10_000..=220_000).contains(&b)),
            "{:?}",
            a.bit_rate
        );
        assert!((r.duration_secs.unwrap() - 8.0).abs() < 0.2, "{:?}", r.duration_secs);
        assert_eq!(r.size_bytes, Some(std::fs::metadata(&out).unwrap().len()));
        assert!(r.bit_rate.is_some());
        let l = r.loudness.expect("loudness");
        let near = |what: &str, got: Option<f64>, want: f64, within: f64| {
            let got = got.unwrap_or_else(|| panic!("{what}: no reading"));
            assert!((got - want).abs() <= within, "{what}: {got} is not {want} +/- {within}");
        };
        near("integrated", l.integrated_lufs, -14.0, 1.0);
        let tp = l.true_peak_dbtp.expect("true peak");
        assert!(tp <= -0.5, "the limited, normalized mix ends under its ceiling: {tp}");
        assert_eq!(r.target, Some(LoudnessTarget::of(LoudnessPreset::Youtube)));
        assert_eq!(r.true_peak_over_db, true_peak_over(Some(tp), Some(-1.0)));
        // The meter's progress rises, never past 1.
        assert!(
            told.windows(2).all(|w| w[0] <= w[1]) && told.iter().all(|f| (0.0..=1.0).contains(f)),
            "{told:?}"
        );
    }

    /// A picture with no sound has nothing to meter; a sound alone has no picture.
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_silent_cut_has_no_loudness_and_a_wav_has_no_picture() {
        let dir = scratch("kinds");
        let asset = source(&dir, -20.0, 4.0);
        let silent = export(
            &dir,
            "silent.mp4",
            &asset,
            &ExportOptions {
                video_codec: Some("libx264".into()),
                preset: Some("ultrafast".into()),
                include_audio: false,
                ..ExportOptions::default()
            },
        );
        let r = measure_export(&silent, None, &mut |_| {}, &|| false);
        assert_eq!(r.problem, None);
        assert!(r.video.is_some() && r.audio.is_none() && r.loudness.is_none() && r.target.is_none());
        let wav = export(
            &dir,
            "mix.wav",
            &asset,
            &ExportOptions {
                container: Container::Wav,
                ..ExportOptions::default()
            },
        );
        let r = measure_export(&wav, None, &mut |_| {}, &|| false);
        assert_eq!(r.problem, None);
        assert!(r.video.is_none());
        assert_eq!(r.container.as_deref(), Some("wav"));
        let a = r.audio.unwrap();
        assert_eq!((a.sample_rate, a.channels), (Some(48_000), Some(2)));
        // 20 dB under full scale, equal channels: the number a tone reads (EBU Tech 3341).
        let integrated = r.loudness.unwrap().integrated_lufs.unwrap();
        assert!((integrated + 20.0).abs() < 0.5, "{integrated}");
        assert_eq!(r.true_peak_over_db, None, "no target, nothing to be over");
    }

    /// A file that is not there says why, and a stop mid-way leaves the file and says it was stopped.
    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_missing_file_and_a_stopped_measurement_are_reported_not_raised() {
        let r = measure_export(Path::new("/nonexistent/dir/x.mp4"), None, &mut |_| {}, &|| false);
        assert!(
            r.problem.as_deref().unwrap().starts_with("the file could not be probed"),
            "{:?}",
            r.problem
        );
        assert!(r.video.is_none() && r.audio.is_none() && r.loudness.is_none());

        let dir = scratch("stop");
        let asset = source(&dir, -20.0, 4.0);
        let wav = export(
            &dir,
            "mix.wav",
            &asset,
            &ExportOptions {
                container: Container::Wav,
                ..ExportOptions::default()
            },
        );
        // Stopped as soon as the meter is running (it says so with its first report): the probe is
        // kept, the loudness is not.
        let stopped = std::cell::Cell::new(false);
        let r = measure_export(&wav, None, &mut |_| stopped.set(true), &|| stopped.get());
        assert_eq!(r.problem.as_deref(), Some(STOPPED));
        assert!(r.audio.is_some() && r.loudness.is_none());
        assert!(wav.is_file(), "a stopped measurement leaves the finished file");
    }
}
