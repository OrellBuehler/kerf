//! Measuring the mix: how loud is the cut, and which track is doing it.
//!
//! One `ffmpeg` pass over the audio the export would render, with an `ebur128`
//! meter on the finished mix and one on every track's strip (see
//! [`build_filter_complex_metered`], which is the export's own graph plus the
//! taps — the master meter therefore hears what the file would contain, master
//! fader, limiter and `loudnorm` included). ffmpeg writes what the meters read
//! to stderr; [`MeterParser`] reads it back, line by line, so a long timeline's
//! frame log never has to be held in memory.
//!
//! **Why this shape.** The alternative to taps is one render per track — N
//! decodes of the same footage. The taps cost one pass, and the only extra work
//! is the meters themselves (about a hundredth of real time each; a *true* peak
//! roughly doubles that, so only the master is given one).
//!
//! It reads whole files, so it takes the heavy-job lease like every other
//! whole-file pass, and ffmpeg runs under the CPU budget's thread caps and
//! priority. A watchdog stops a run that has gone silent: a meter logs ten
//! lines per second of audio, so silence from ffmpeg is a hang, not a long
//! render.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::model::LevelReading;

/// What a metered pass read: the finished mix and each track that fed it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MixLevels {
    /// Seconds of the cut that were measured.
    pub duration: f64,
    /// The finished mix. `None` when the cut had no audio at all.
    pub master: Option<LevelReading>,
    /// `(index in the timeline's track list, reading)` for every track that had
    /// audio in the render, in track order.
    pub tracks: Vec<(usize, LevelReading)>,
}

/// How long a metering pass may go without ffmpeg writing a line before it is
/// given up on. The meters log ten lines per second of audio processed, so this
/// is a hang (or a source that has stopped delivering), not a heavy render.
const LEVELS_STALL: Duration = Duration::from_secs(120);

/// How often a running pass checks whether it has been cancelled while ffmpeg
/// is quiet.
const LEVELS_POLL: Duration = Duration::from_millis(250);

/// What the metering build of an export is run with: the argv, the meters it
/// asks for and how long the measured span is.
#[derive(Debug)]
struct LevelsRun {
    args: Vec<String>,
    meters: Vec<Meter>,
    duration: f64,
}

/// The argv for one metering pass over `timeline`, or `None` when the render has
/// no audio. Pure, like [`build_export_args`]: the same muted-track and
/// disabled-clip gate, the same range slice, the same inputs and the same
/// graph, ending in `-f null -` instead of a file.
///
/// `opts` is what the pass is *as if* exported with — `range` and `loudnorm`
/// are the ones that matter here.
fn build_levels_args(timeline: &Timeline, assets: &[Asset], opts: &ExportOptions) -> Result<Option<LevelsRun>> {
    let rendered = timeline.for_render();
    let sliced;
    let timeline = match effective_range(&rendered, opts) {
        Some((s, e)) => {
            sliced = rendered.slice(s, e);
            &sliced
        }
        None => &rendered,
    };

    let fmt = export_format(timeline, assets, opts);
    let mut args: Vec<String> = vec!["-hide_banner".to_string(), "-nostats".to_string()];
    let plan = push_inputs(timeline, assets, &fmt, opts, &mut args)?;
    let duration = timeline.duration();
    let graph = build_filter_complex_metered(timeline, assets, &fmt, duration, opts, false, true, &plan, true);
    if !graph.has_audio {
        return Ok(None);
    }
    args.push("-filter_complex".to_string());
    args.push(graph.filter);
    for meter in &graph.meters {
        args.push("-map".to_string());
        args.push(format!("[{}]", meter.pad));
    }
    args.extend(["-f".to_string(), "null".to_string(), "-".to_string()]);
    Ok(Some(LevelsRun {
        args,
        meters: graph.meters,
        duration,
    }))
}

/// Measure the mix of `timeline` — its loudness, peaks and short-term maximum,
/// overall and per track — over `range` (the whole cut when `None`), with
/// `loudnorm` on or off as an export would have it.
///
/// Whole-file work: holds the heavy-job lease for the pass. `cancel` is polled
/// while ffmpeg runs and kills it, returning [`Error::Cancelled`].
pub fn mix_levels(
    timeline: &Timeline,
    assets: &[Asset],
    range: Option<TimeRange>,
    loudnorm: bool,
    cancel: &dyn Fn() -> bool,
) -> Result<MixLevels> {
    let opts = ExportOptions {
        range,
        loudnorm,
        ..ExportOptions::default()
    };
    let Some(run) = build_levels_args(timeline, assets, &opts)? else {
        return Ok(MixLevels::default());
    };

    let bin = ffmpeg_bin();
    // Foreground, and stoppable while it waits for the slot (the Mixer's Stop works on a queued
    // measurement as on a running one).
    let lease = cpu::lease_waiting(&mut |_| {}, cancel)?;
    let mut args = run.args;
    cpu::limit_args(&mut args, lease.threads());
    let _script = externalize_filter_complex(&mut args, "lv")?;
    tracing::debug!(command = %format!("{bin} {}", args.join(" ")), "ffmpeg levels command");

    let mut child = bg_command(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| launch_err(&bin, e))?;
    let stderr = child.stderr.take().expect("stderr piped");

    // Lines are read on a side thread so the loop below keeps polling `cancel`
    // and the watchdog even while ffmpeg is quiet. Raw bytes, decoded lossily:
    // a path in a warning must not end the read at the first invalid UTF-8.
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&raw).trim_end_matches(['\n', '\r']).to_string();
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let mut parser = MeterParser::default();
    // The last lines that are not meter frames, for the error of a run that fails.
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut last_line = Instant::now();
    let (mut cancelled, mut stalled) = (false, false);
    loop {
        match rx.recv_timeout(LEVELS_POLL) {
            Ok(line) => {
                last_line = Instant::now();
                if !parser.feed(&line) {
                    if tail.len() == 20 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
                if cancel() {
                    let _ = child.kill();
                    cancelled = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if cancel() {
                    let _ = child.kill();
                    cancelled = true;
                    break;
                }
                if last_line.elapsed() > LEVELS_STALL {
                    let _ = child.kill();
                    stalled = true;
                    break;
                }
            }
        }
    }
    let status = child.wait().map_err(|e| Error::Engine(format!("ffmpeg wait failed: {e}")))?;
    let tail = tail.into_iter().collect::<Vec<_>>().join("\n");
    if cancelled {
        return Err(Error::Cancelled);
    }
    if stalled {
        return Err(Error::Engine(format!(
            "ffmpeg stopped reporting levels for {}s and was stopped: {}",
            LEVELS_STALL.as_secs(),
            tail.trim()
        )));
    }
    if !status.success() {
        tracing::error!(status = %status, "ffmpeg levels pass failed:\n{tail}");
        return Err(Error::Engine(format!("ffmpeg exited with {status}: {}", tail.trim())));
    }

    let mut master = None;
    let mut tracks = Vec::new();
    for meter in &run.meters {
        let reading = parser.reading(&meter.name).ok_or_else(|| {
            Error::Engine(format!(
                "ffmpeg finished without reporting the `{}` meter: {}",
                meter.name,
                tail.trim()
            ))
        })?;
        match meter.track {
            Some(track) => tracks.push((track, reading)),
            None => master = Some(reading),
        }
    }
    Ok(MixLevels {
        duration: run.duration,
        master,
        tracks,
    })
}

/// A stretch of `ebur128` output, read one line at a time.
///
/// Each meter is an `ebur128@<name>` instance, so its lines carry that name:
/// `[ebur128@t0 @ 0x55…] t: 0.1 … S:-23.4 …` ten times a second (the short-term
/// loudness is the one number only the frame log has), and a `Summary:` block
/// when the stream ends. The summary is *one* log call, so only its first line
/// is prefixed — the rest follow bare, and belong to whichever meter spoke last.
#[derive(Debug, Default)]
pub(super) struct MeterParser {
    meters: BTreeMap<String, Acc>,
    /// The meter whose summary is being read, if one is.
    summary: Option<String>,
    section: Section,
}

#[derive(Debug, Default)]
struct Acc {
    reading: LevelReading,
    /// Set once the meter's summary has been read.
    done: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Section {
    #[default]
    None,
    Integrated,
    Range,
    SamplePeak,
    TruePeak,
}

/// What the short-term meter reads before its first 3 s window has closed.
const SHORT_TERM_FLOOR: f64 = -120.0;

/// The absolute gate: with nothing above it, `ebur128` reports -70.0.
const INTEGRATED_FLOOR: f64 = -70.0;

impl MeterParser {
    /// Read one line. Returns whether it belonged to a meter (a frame or a line
    /// of a summary) — the caller keeps the lines that did not, for an error.
    pub(super) fn feed(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix("[ebur128@") {
            let Some((name, message)) = rest.split_once(" @ ").and_then(|(n, m)| Some((n, m.split_once(']')?.1))) else {
                return false;
            };
            self.summary = None;
            self.section = Section::None;
            let acc = self.meters.entry(name.to_string()).or_default();
            let message = message.trim();
            if message.starts_with("t:") {
                // `t: 0.1  TARGET:-23 LUFS    M:-120.7 S:-120.7     I: -70.0 LUFS …`
                if let Some(s) = message.find(" S:").and_then(|i| number(&message[i + 3..])) {
                    if s > SHORT_TERM_FLOOR && acc.reading.short_term_max_lufs.is_none_or(|m| s > m) {
                        acc.reading.short_term_max_lufs = Some(s);
                    }
                }
            } else if message == "Summary:" {
                self.summary = Some(name.to_string());
            }
            return true;
        }
        if line.starts_with('[') {
            // Another filter, or the muxer: whatever summary was open is over.
            self.summary = None;
            return false;
        }
        let Some(name) = self.summary.clone() else {
            return false;
        };
        let text = line.trim();
        if text.is_empty() {
            return true;
        }
        let acc = self.meters.entry(name).or_default();
        match text {
            "Integrated loudness:" => self.section = Section::Integrated,
            "Loudness range:" => self.section = Section::Range,
            "Sample peak:" => self.section = Section::SamplePeak,
            "True peak:" => self.section = Section::TruePeak,
            _ => match (self.section, text) {
                (Section::Integrated, t) if t.starts_with("I:") => {
                    acc.reading.integrated_lufs = number(&t[2..]).filter(|v| *v > INTEGRATED_FLOOR);
                    acc.done = true;
                }
                (Section::Range, t) if t.starts_with("LRA:") => acc.reading.loudness_range_lu = number(&t[4..]),
                (Section::SamplePeak, t) if t.starts_with("Peak:") => acc.reading.peak_dbfs = number(&t[5..]),
                (Section::TruePeak, t) if t.starts_with("Peak:") => acc.reading.true_peak_dbtp = number(&t[5..]),
                _ => {}
            },
        }
        true
    }

    /// What `name` read, once its summary has arrived.
    pub(super) fn reading(&self, name: &str) -> Option<LevelReading> {
        self.meters.get(name).filter(|a| a.done).map(|a| a.reading)
    }
}

/// The number at the start of `text` (`" -15.0 LUFS"` → -15), `None` for
/// `-inf` or anything else that is not a finite number.
fn number(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::test_support::{audio_track, av_asset, make_clip, test_asset, timeline_of, video_stream, video_track};
    use crate::model::MasterBus;
    use uuid::Uuid;

    /// What ffmpeg 6.1 and 9.0 print for a two-meter pass over 4 s of audio
    /// (frame lines trimmed to the ones that matter), as the engine reads it.
    const OUTPUT: &str = "\
Input #0, lavfi, from 'sine=f=1000:d=4:sample_rate=48000,volume=4':
  Stream #0:0: Audio: pcm_f32le, 48000 Hz, mono, flt, 1536 kb/s
[ebur128@t0 @ 0x560cc4f74d80] t: 0.0999792  TARGET:-23 LUFS    M:-120.7 S:-120.7     I: -70.0 LUFS       LRA:   0.0 LU  FTPK:  -6.0 dBFS  TPK:  -6.0 dBFS
[ebur128@master @ 0x560cc4f75280] t: 0.0999792  TARGET:-23 LUFS    M:-120.7 S:-120.7     I: -70.0 LUFS       LRA:   0.0 LU  FTPK: -12.0 dBFS  TPK: -12.0 dBFS
[ebur128@t0 @ 0x560cc4f74d80] t: 3.09998  TARGET:-23 LUFS    M:  -9.0 S: -10.2     I:  -9.0 LUFS       LRA:   0.0 LU  FTPK:  -6.0 dBFS  TPK:  -6.0 dBFS
[ebur128@master @ 0x560cc4f75280] t: 3.09998  TARGET:-23 LUFS    M: -15.0 S: -16.3     I: -15.0 LUFS       LRA:   0.0 LU  FTPK: -12.0 dBFS  TPK: -12.0 dBFS
[ebur128@t0 @ 0x560cc4f74d80] t: 3.89998  TARGET:-23 LUFS    M:  -9.0 S:  -9.4     I:  -9.0 LUFS       LRA:   0.0 LU  FTPK:  -6.0 dBFS  TPK:  -6.0 dBFS
[ebur128@master @ 0x560cc4f75280] t: 3.89998  TARGET:-23 LUFS    M: -15.0 S: -15.4     I: -15.0 LUFS       LRA:   0.0 LU  FTPK: -12.0 dBFS  TPK: -12.0 dBFS
Output #0, null, to 'pipe:':
  Metadata:
[out#0/null @ 0x55bd352aea40] video:0kB audio:750kB subtitle:0kB other streams:0kB global headers:0kB muxing overhead: unknown
size=N/A time=00:00:03.90 bitrate=N/A speed=  53x
[ebur128@master @ 0x55bd352b3280] Summary:

  Integrated loudness:
    I:         -15.0 LUFS
    Threshold: -25.0 LUFS

  Loudness range:
    LRA:         1.5 LU
    Threshold: -35.0 LUFS
    LRA low:   -15.0 LUFS
    LRA high:  -15.0 LUFS

  Sample peak:
    Peak:      -12.0 dBFS

  True peak:
    Peak:      -11.7 dBFS
[ebur128@t0 @ 0x55bd352b2d80] Summary:

  Integrated loudness:
    I:          -9.0 LUFS
    Threshold: -19.0 LUFS

  Loudness range:
    LRA:         0.0 LU
    Threshold: -29.0 LUFS
    LRA low:    -9.0 LUFS
    LRA high:   -9.0 LUFS

  Sample peak:
    Peak:       -6.0 dBFS

  True peak:
    Peak:       -5.8 dBFS
";

    fn parse(text: &str) -> MeterParser {
        let mut p = MeterParser::default();
        for line in text.lines() {
            p.feed(line);
        }
        p
    }

    #[test]
    fn a_summary_is_read_per_meter() {
        let p = parse(OUTPUT);
        let master = p.reading("master").unwrap();
        assert_eq!(master.integrated_lufs, Some(-15.0));
        assert_eq!(master.loudness_range_lu, Some(1.5));
        assert_eq!(master.peak_dbfs, Some(-12.0));
        assert_eq!(master.true_peak_dbtp, Some(-11.7));
        let t0 = p.reading("t0").unwrap();
        assert_eq!(t0.integrated_lufs, Some(-9.0));
        assert_eq!(t0.peak_dbfs, Some(-6.0));
        // A track is metered for its true peak too: which one is hot is the question.
        assert_eq!(t0.true_peak_dbtp, Some(-5.8));
        assert!(p.reading("t1").is_none());
    }

    #[test]
    fn the_short_term_maximum_comes_from_the_frame_log() {
        let p = parse(OUTPUT);
        // The -120.7 of the first, unfilled window is not a loudness; the maximum
        // of the real ones is.
        assert_eq!(p.reading("master").unwrap().short_term_max_lufs, Some(-15.4));
        assert_eq!(p.reading("t0").unwrap().short_term_max_lufs, Some(-9.4));
    }

    #[test]
    fn a_span_under_three_seconds_has_no_short_term_maximum() {
        let text = "\
[ebur128@master @ 0x1] t: 1.9  TARGET:-23 LUFS    M: -15.0 S:-120.7     I: -15.0 LUFS       LRA:   0.0 LU
[ebur128@master @ 0x1] Summary:

  Integrated loudness:
    I:         -15.0 LUFS
";
        let r = parse(text).reading("master").unwrap();
        assert_eq!(r.short_term_max_lufs, None);
        assert_eq!(r.integrated_lufs, Some(-15.0));
    }

    #[test]
    fn silence_reads_as_nothing_rather_than_minus_infinity() {
        let text = "\
[ebur128@master @ 0x1] Summary:

  Integrated loudness:
    I:         -70.0 LUFS
    Threshold:   0.0 LUFS

  Loudness range:
    LRA:         0.0 LU

  Sample peak:
    Peak:       -inf dBFS

  True peak:
    Peak:       -inf dBFS
";
        let r = parse(text).reading("master").unwrap();
        assert_eq!(r.integrated_lufs, None);
        assert_eq!(r.peak_dbfs, None);
        assert_eq!(r.true_peak_dbtp, None);
        assert_eq!(r.loudness_range_lu, Some(0.0));
    }

    #[test]
    fn a_meter_that_never_summarised_has_no_reading() {
        // A pass killed mid-way: frames but no summary must not read as a result.
        let text = "[ebur128@master @ 0x1] t: 3.5  TARGET:-23 LUFS    M: -15.0 S: -15.0     I: -15.0 LUFS\n";
        assert!(parse(text).reading("master").is_none());
    }

    #[test]
    fn lines_that_are_not_a_meters_are_left_for_the_error() {
        let mut p = MeterParser::default();
        assert!(!p.feed("Input #0, lavfi, from 'x':"));
        assert!(!p.feed("[out#0/null @ 0x5] video:0kB audio:750kB"));
        assert!(!p.feed("size=N/A time=00:00:03.90 bitrate=N/A speed=  53x"));
        assert!(!p.feed("Error opening input file /nope.mp4."));
        assert!(p.feed("[ebur128@t1 @ 0x1] t: 0.1  TARGET:-23 LUFS    M:-120.7 S:-120.7"));
        // A summary ends where the next filter's own line begins.
        assert!(p.feed("[ebur128@t1 @ 0x1] Summary:"));
        assert!(p.feed("  Integrated loudness:"));
        assert!(!p.feed("[out#0/null @ 0x5] video:0kB"));
        assert!(!p.feed("    I:  -9.0 LUFS"));
        assert!(p.reading("t1").is_none());
    }

    // ---- the graph -------------------------------------------------------

    fn two_track_cut() -> (Timeline, Vec<Asset>) {
        let asset = av_asset(Uuid::new_v4(), 10.0);
        let voice = audio_track(vec![make_clip(asset.id, 0.0, 4.0, 0.0), make_clip(asset.id, 0.0, 3.0, 5.0)]);
        let mut music = audio_track(vec![make_clip(asset.id, 0.0, 8.0, 0.0)]);
        music.name = "A2".into();
        (timeline_of(vec![video_track(Vec::new()), voice, music]), vec![asset])
    }

    fn graph_of(tl: &Timeline, assets: &[Asset], opts: &ExportOptions, meter: bool) -> FilterGraph {
        let tl = tl.for_render();
        let fmt = export_format(&tl, assets, opts);
        let mut args = Vec::new();
        let plan = push_inputs(&tl, assets, &fmt, opts, &mut args).unwrap();
        build_filter_complex_metered(&tl, assets, &fmt, tl.duration(), opts, false, true, &plan, meter)
    }

    #[test]
    fn an_unmetered_build_has_no_meters_and_the_export_graph() {
        let (tl, assets) = two_track_cut();
        let opts = ExportOptions::default();
        let plain = graph_of(&tl, &assets, &opts, false);
        assert!(plain.meters.is_empty());
        assert!(!plain.filter.contains("ebur128"), "{}", plain.filter);
        assert!(
            plain.filter.ends_with("amix=inputs=3:normalize=0:dropout_transition=0[outa]"),
            "{}",
            plain.filter
        );
    }

    #[test]
    fn a_metered_build_taps_every_strip_and_the_finished_mix() {
        let (tl, assets) = two_track_cut();
        let g = graph_of(&tl, &assets, &ExportOptions::default(), true);
        // Tracks 1 and 2 (the video track carries no audio clips): the voice
        // track sums its two clips first, the music track has one.
        assert_eq!(
            g.meters,
            vec![
                Meter {
                    pad: "lv1".into(),
                    name: "t1".into(),
                    track: Some(1)
                },
                Meter {
                    pad: "lv2".into(),
                    name: "t2".into(),
                    track: Some(2)
                },
                Meter {
                    pad: "lvmaster".into(),
                    name: "master".into(),
                    track: None
                },
            ]
        );
        let f = &g.filter;
        assert!(
            f.contains("[a0][a1]amix=inputs=2:normalize=0:dropout_transition=0[tk1]"),
            "{f}"
        );
        assert!(f.contains("[tk1]asplit=2[tm1][tl1]"), "{f}");
        assert!(f.contains("[tl1]ebur128@t1=peak=sample+true[lv1]"), "{f}");
        assert!(f.contains("[a2]asplit=2[tm2][tl2]"), "{f}");
        assert!(
            f.contains("[tm1][tm2]amix=inputs=2:normalize=0:dropout_transition=0[outa]"),
            "{f}"
        );
        assert!(f.ends_with("[outa]ebur128@master=peak=sample+true[lvmaster]"), "{f}");
    }

    #[test]
    fn a_metered_build_keeps_the_duck_bus() {
        let (mut tl, assets) = two_track_cut();
        tl.tracks[2].duck = true;
        let g = graph_of(&tl, &assets, &ExportOptions::default(), true);
        assert!(
            g.filter.contains("[tm1]amix=inputs=1:normalize=0:dropout_transition=0[akey]"),
            "{}",
            g.filter
        );
        assert!(
            g.filter
                .contains("[tm2]amix=inputs=1:normalize=0:dropout_transition=0[aduck]"),
            "{}",
            g.filter
        );
        assert!(g.filter.contains("sidechaincompress"), "{}", g.filter);
    }

    #[test]
    fn the_master_bus_and_loudnorm_sit_on_the_tapped_mix() {
        let (mut tl, assets) = two_track_cut();
        tl.master = MasterBus {
            duck_depth_db: None,
            volume: 0.5,
            limiter: true,
            ceiling_db: -1.0,
        };
        let opts = ExportOptions {
            loudnorm: true,
            ..ExportOptions::default()
        };
        let g = graph_of(&tl, &assets, &opts, true);
        let f = &g.filter;
        let master = f.find(",volume=0.5,alimiter=").expect(f);
        let norm = f.find(",loudnorm=").expect(f);
        let tap = f.find("[outa]ebur128@master").expect(f);
        assert!(master < norm && norm < tap, "{f}");
    }

    #[test]
    fn the_levels_argv_maps_every_meter_to_the_null_muxer() {
        let (tl, assets) = two_track_cut();
        let run = build_levels_args(&tl, &assets, &ExportOptions::default()).unwrap().unwrap();
        assert_eq!(run.args[..2], ["-hide_banner", "-nostats"]);
        assert_eq!(run.args[run.args.len() - 3..], ["-f", "null", "-"]);
        let maps: Vec<&str> = run
            .args
            .windows(2)
            .filter(|w| w[0] == "-map")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(maps, ["[lv1]", "[lv2]", "[lvmaster]"]);
        assert!((run.duration - 8.0).abs() < 1e-9, "{}", run.duration);
    }

    #[test]
    fn a_range_measures_only_that_span() {
        let (tl, assets) = two_track_cut();
        let opts = ExportOptions {
            range: Some(TimeRange { start: 1.0, end: 3.0 }),
            ..ExportOptions::default()
        };
        let run = build_levels_args(&tl, &assets, &opts).unwrap().unwrap();
        assert!((run.duration - 2.0).abs() < 1e-9, "{}", run.duration);
    }

    #[test]
    fn a_muted_track_is_not_metered_and_a_cut_without_audio_is_not_measured() {
        let (mut tl, assets) = two_track_cut();
        tl.tracks[2].muted = true;
        let run = build_levels_args(&tl, &assets, &ExportOptions::default()).unwrap().unwrap();
        assert_eq!(run.meters.iter().filter_map(|m| m.track).collect::<Vec<_>>(), [1]);
        tl.tracks[1].muted = true;
        assert!(build_levels_args(&tl, &assets, &ExportOptions::default()).unwrap().is_none());
    }

    #[test]
    fn a_silent_asset_is_not_measured() {
        let silent = test_asset(vec![video_stream(1920, 1080, 30.0)]);
        let tl = timeline_of(vec![video_track(vec![make_clip(silent.id, 0.0, 5.0, 0.0)])]);
        assert!(build_levels_args(&tl, &[silent], &ExportOptions::default())
            .unwrap()
            .is_none());
    }

    // ---- the real thing (`#[ignore]`d: they drive the ffmpeg binary, 4.4, 6.1 and 9.0 alike) ---
    //
    // `cargo test -p kerf-core --no-default-features -- --ignored levels`

    use crate::engine::test_support::{audio_stream, StatusBounded};
    use crate::model::{Asset, StreamKind};
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-levels-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// `secs` of a stereo 1 kHz sine whose **peak** is `peak_db` dBFS, at 24 bits
    /// (no quantisation to argue about). A stereo tone with equal channels reads the
    /// same number of LUFS as its peak (EBU Tech 3341, case 1), which is what makes
    /// the expected values below plain arithmetic.
    fn tone(dir: &Path, peak_db: f64, secs: f64) -> Asset {
        // FLAC, not a float wav: FFmpeg 9's format probe takes some repeating
        // float-PCM byte patterns for MPEG-TS and refuses the file.
        let path = dir.join(format!("tone{peak_db}-{secs}.flac"));
        if !path.exists() {
            // `sine` peaks at 1/8, and is mono: `pan` copies it into both channels at
            // full level (an `aformat` upmix would take 3 dB off each).
            let gain = 10f64.powf(peak_db / 20.0) / 0.125;
            let made = command(&ffmpeg_bin())
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg(format!("sine=f=1000:d={secs}:sample_rate=48000"))
                .arg("-af")
                .arg(format!("pan=stereo|c0=c0|c1=c0,volume={gain:.9}"))
                .args(["-c:a", "flac", "-sample_fmt", "s32"])
                .arg(&path)
                .status_bounded()
                .expect("run ffmpeg");
            assert!(made.success());
        }
        let mut asset = test_asset(vec![audio_stream(48_000, 2)]);
        asset.path = path.to_string_lossy().into_owned();
        asset.duration = secs;
        asset
    }

    fn near(what: &str, got: Option<f64>, want: f64, within: f64) {
        let got = got.unwrap_or_else(|| panic!("{what}: no reading"));
        assert!((got - want).abs() <= within, "{what}: {got} is not {want} +/- {within}");
    }

    fn measure(tl: &Timeline, assets: &[Asset]) -> MixLevels {
        mix_levels(tl, assets, None, false, &|| false).expect("levels")
    }

    /// One 8 s clip of `asset` on each of `tracks` audio tracks.
    fn tracks_of(asset: &Asset, tracks: usize) -> Timeline {
        timeline_of(
            (0..tracks)
                .map(|n| {
                    let mut t = audio_track(vec![make_clip(asset.id, 0.0, 8.0, 0.0)]);
                    t.name = format!("A{}", n + 1);
                    t
                })
                .collect(),
        )
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_known_tone_reads_its_known_level() {
        let dir = scratch("tone");
        let asset = tone(&dir, -20.0, 8.0);
        let m = measure(&tracks_of(&asset, 1), std::slice::from_ref(&asset));
        let master = m.master.expect("master");
        near("integrated", master.integrated_lufs, -20.0, 0.5);
        near("sample peak", master.peak_dbfs, -20.0, 0.1);
        near("true peak", master.true_peak_dbtp, -20.0, 0.2);
        // 8 s is long enough for 3 s windows to close.
        near("short-term max", master.short_term_max_lufs, -20.0, 0.5);
        near("range", master.loudness_range_lu, 0.0, 0.5);
        assert!((m.duration - 8.0).abs() < 1e-6);
        // The one track reads what the master does: nothing but it feeds the sum.
        let [(0, strip)] = m.tracks[..] else {
            panic!("{:?}", m.tracks)
        };
        near("track integrated", strip.integrated_lufs, -20.0, 0.5);
        near("track peak", strip.peak_dbfs, -20.0, 0.1);
        near("track true peak", strip.true_peak_dbtp, -20.0, 0.2);
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn the_strip_and_the_master_move_the_readings_as_the_mixer_says() {
        let dir = scratch("mixer");
        let asset = tone(&dir, -20.0, 8.0);
        let assets = std::slice::from_ref(&asset);

        // A fader at 0.5 is -6.02 dB on the strip and so on the master.
        let mut tl = tracks_of(&asset, 1);
        tl.tracks[0].volume = 0.5;
        let m = measure(&tl, assets);
        near("fader: strip", m.tracks[0].1.integrated_lufs, -26.02, 0.5);
        near("fader: master", m.master.unwrap().integrated_lufs, -26.02, 0.5);

        // Hard left is a balance: the left channel keeps unity, the right is gone,
        // so the loudness drops by one channel's share (3.01 dB), not to the floor.
        let mut tl = tracks_of(&asset, 1);
        tl.tracks[0].pan = -1.0;
        let m = measure(&tl, assets);
        near("pan: master", m.master.unwrap().integrated_lufs, -23.01, 0.5);

        // Two coherent tracks sum: +6.02 dB on the master, each strip as it was.
        let m = measure(&tracks_of(&asset, 2), assets);
        near("sum: master", m.master.unwrap().integrated_lufs, -13.98, 0.5);
        near("sum: strip 1", m.tracks[0].1.integrated_lufs, -20.0, 0.5);
        near("sum: strip 2", m.tracks[1].1.integrated_lufs, -20.0, 0.5);
        assert_eq!(m.tracks.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1]);

        // The master fader moves only the master.
        let mut tl = tracks_of(&asset, 1);
        tl.master.volume = 0.5;
        let m = measure(&tl, assets);
        near("master fader: master", m.master.unwrap().integrated_lufs, -26.02, 0.5);
        near("master fader: strip", m.tracks[0].1.integrated_lufs, -20.0, 0.5);

        // A ducked track is read before the duck, as loud as it is on its own: the
        // strip is the track, the duck is the bus.
        let mut tl = tracks_of(&asset, 2);
        tl.tracks[1].duck = true;
        let m = measure(&tl, assets);
        near("duck: strip", m.tracks[1].1.integrated_lufs, -20.0, 0.5);
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn the_limiter_holds_its_ceiling_and_does_not_level_the_mix_back_up() {
        let dir = scratch("limiter");
        let asset = tone(&dir, -20.0, 8.0);
        let assets = std::slice::from_ref(&asset);

        // +12 dB of master fader puts the tone at -8 dBFS; a -12 dB ceiling has to
        // take it down to -12 and not a hair above.
        let mut tl = tracks_of(&asset, 1);
        tl.master = MasterBus {
            duck_depth_db: None,
            volume: 4.0,
            limiter: true,
            ceiling_db: -12.0,
        };
        let master = measure(&tl, assets).master.unwrap();
        let peak = master.peak_dbfs.unwrap();
        assert!((-12.1..=-11.9).contains(&peak), "peak {peak} not held at -12");
        assert!(master.true_peak_dbtp.unwrap() <= -11.0, "{master:?}");

        // The lookahead is compensated: the first 4 ms already carry the tone. Left
        // at its default the limiter delays the whole mix by its attack time (the
        // sound would trail the picture) and the first milliseconds are silence. Only
        // where `alimiter` has `latency` to compensate with: FFmpeg 4.4's does not,
        // and its graph carries the 5 ms delay.
        if alimiter_latency_available() {
            let head = mix_levels(&tl, assets, Some(TimeRange { start: 0.0, end: 0.004 }), false, &|| false)
                .unwrap()
                .master
                .unwrap();
            assert!(
                head.peak_dbfs.is_some_and(|p| p > -30.0),
                "the head of the mix is silent: {head:?}"
            );
        }

        // A limiter that has nothing to limit leaves the mix alone. (Its default
        // auto-level would scale -20 dBFS up to the ceiling: this is the regression
        // `level=0` is there for.)
        let mut quiet = tracks_of(&asset, 1);
        quiet.master.limiter = true;
        let master = measure(&quiet, assets).master.unwrap();
        near("untouched peak", master.peak_dbfs, -20.0, 0.1);
        near("untouched loudness", master.integrated_lufs, -20.0, 0.5);
    }

    /// The loudness and peaks an independent `ebur128` pass reads off a rendered file.
    fn read_back(file: &Path) -> LevelReading {
        let out = command(&ffmpeg_bin())
            .args(["-hide_banner", "-nostats", "-f", "wav", "-i"])
            .arg(file)
            .args(["-af", "ebur128@x=peak=sample+true", "-f", "null", "-"])
            .stdin(Stdio::null())
            .output()
            .expect("run ffmpeg");
        let mut parser = MeterParser::default();
        for line in String::from_utf8_lossy(&out.stderr).lines() {
            parser.feed(line);
        }
        parser.reading("x").expect("the read-back pass reported")
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn the_levels_are_what_the_export_writes() {
        let dir = scratch("export");
        let loud = tone(&dir, -9.0, 8.0);
        let soft = tone(&dir, -22.0, 8.0);
        let assets = [loud.clone(), soft.clone()];
        let mut tl = timeline_of(vec![
            audio_track(vec![make_clip(loud.id, 0.0, 8.0, 0.0)]),
            audio_track(vec![make_clip(soft.id, 0.0, 8.0, 0.0)]),
        ]);
        tl.tracks[1].volume = 0.8;
        tl.tracks[1].duck = true;
        tl.master = MasterBus {
            duck_depth_db: None,
            volume: 0.9,
            limiter: true,
            ceiling_db: -3.0,
        };
        for loudnorm in [false, true] {
            let measured = mix_levels(&tl, &assets, None, loudnorm, &|| false).unwrap().master.unwrap();
            let file = dir.join(format!("out-{loudnorm}.wav"));
            let opts = ExportOptions {
                container: Container::Wav,
                loudnorm,
                ..ExportOptions::default()
            };
            let status = render_with_progress(&tl, &assets, &file, &opts, &mut |_| {}, &|| false).expect("export");
            assert_eq!(status, RenderStatus::Completed);
            let written = read_back(&file);
            // The same graph, so the same signal; only the wav's 16-bit rounding differs.
            near(
                &format!("loudnorm={loudnorm}: integrated"),
                written.integrated_lufs,
                measured.integrated_lufs.unwrap(),
                0.15,
            );
            near(
                &format!("loudnorm={loudnorm}: peak"),
                written.peak_dbfs,
                measured.peak_dbfs.unwrap(),
                0.1,
            );
            near(
                &format!("loudnorm={loudnorm}: true peak"),
                written.true_peak_dbtp,
                measured.true_peak_dbtp.unwrap(),
                0.2,
            );
            if loudnorm {
                near("normalised to the target", measured.integrated_lufs, -14.0, 1.5);
            }
        }
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_range_measures_the_span_and_a_muted_track_is_not_heard() {
        let dir = scratch("range");
        let asset = tone(&dir, -20.0, 8.0);
        let assets = std::slice::from_ref(&asset);
        let mut tl = tracks_of(&asset, 2);
        tl.tracks[1].muted = true;

        // Through the project API: names, the heard flag, the span.
        let levels = crate::Project::measure_levels(&tl, assets, Some(TimeRange { start: 2.0, end: 6.0 }), false, &|| false)
            .expect("levels");
        assert!((levels.duration - 4.0).abs() < 1e-6, "{}", levels.duration);
        near("range: master", levels.master.unwrap().integrated_lufs, -20.0, 0.5);
        assert_eq!(levels.tracks.len(), 2);
        assert_eq!((levels.tracks[0].name.as_str(), levels.tracks[0].heard), ("A1", true));
        near("range: A1", levels.tracks[0].level.unwrap().integrated_lufs, -20.0, 0.5);
        assert_eq!((levels.tracks[1].name.as_str(), levels.tracks[1].heard), ("A2", false));
        assert!(levels.tracks[1].level.is_none(), "a muted track has no reading");
        assert_eq!(levels.tracks[1].kind, StreamKind::Audio);
        assert!(
            levels.notes.iter().any(|n| n.contains("under") || n.contains("close")),
            "{:?}",
            levels.notes
        );
    }

    #[test]
    #[ignore = "needs the ffmpeg binary"]
    fn a_cancelled_pass_stops_ffmpeg_and_says_so() {
        let dir = scratch("cancel");
        let asset = tone(&dir, -20.0, 8.0);
        let tl = tracks_of(&asset, 1);
        let got = mix_levels(&tl, std::slice::from_ref(&asset), None, false, &|| true);
        assert!(matches!(got, Err(Error::Cancelled)), "{got:?}");
    }
}
