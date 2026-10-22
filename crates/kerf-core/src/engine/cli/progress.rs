//! How an export tells how far it is (pure, unit-tested): the stages it goes through, the share of
//! the bar each owns, and ffmpeg's `-progress` stream read into positions.
//!
//! **Why stages.** An export is not one process. A static loudness normalization meters the mix
//! first, ffmpeg may then spend a long time opening a long recording and decoding up to a range's
//! start before it writes a single frame (the output file stays empty), the encode runs, and an
//! mp4's `faststart` closes with a rewrite of the whole file. Reporting only `out_time` against
//! the cut's length leaves every part but the encode at 0% — an export that looks hung — so each
//! part says what it is and moves the bar through its own share.

use super::*;

/// The part of an export that is running. Each reports its own position inside its share of the
/// overall bar, so the bar only moves forward and the words say what is moving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportStage {
    /// A static loudness normalization is metering the mix (one pass over the export's own
    /// audio, before the render). Its fraction walks the mix's length.
    Metering,
    /// ffmpeg is running but has written no picture yet: it opens the footage and, for a range
    /// that starts deep in a long recording, decodes up to the start of the range — which can take
    /// minutes on a long-GOP source, with the output still empty and nothing to count.
    Starting,
    /// Encoding: the fraction follows the output's position through the cut.
    Rendering,
    /// Everything is encoded and the muxer is closing the file (an mp4's `faststart` rewrites the
    /// whole file to move its index to the front).
    Finishing,
    /// The written file is being read back: its streams and its loudness.
    Measuring,
}

impl ExportStage {
    /// The words for a progress line (`metering loudness`, `rendering` …).
    pub fn label(self) -> &'static str {
        match self {
            Self::Metering => "metering loudness",
            Self::Starting => "starting ffmpeg and seeking the footage",
            Self::Rendering => "rendering",
            Self::Finishing => "finishing the file",
            Self::Measuring => "measuring the written file",
        }
    }
}

impl ExportProgress {
    /// This report as the **whole export** sees it: a stage that owns `[from, to]` of the bar
    /// reports `0.0..=1.0` of its own, and the elapsed time is counted from `since` — the export's
    /// start, not the stage's (the time must not run backwards into the render).
    pub(super) fn within(self, from: f64, to: f64, since: Instant) -> Self {
        Self {
            fraction: (from + (to - from) * self.fraction.clamp(0.0, 1.0)).clamp(0.0, 1.0),
            elapsed_secs: since.elapsed().as_secs_f64(),
            ..self
        }
    }
}

/// What a static loudness normalization's metering pass takes of an export that encodes picture.
const METER_SHARE_WITH_PICTURE: f64 = 0.08;
/// ... and of one that writes sound only, where metering is about as much work as the encode.
const METER_SHARE_SOUND_ONLY: f64 = 0.4;

/// What reading the written file back takes of the bar, at the end of it ([`render_measured`]).
pub(super) const VERIFY_SHARE: f64 = 0.03;

/// The share of the bar a metering pass takes ahead of the render (pure): nothing when nothing is
/// metered. An estimate of cost, not a measurement: a pass that meters the mix decodes only the
/// sound and runs a meter, a small part of a picture encode, but most of an export of sound alone.
pub(super) fn metering_share(metering: bool, has_picture: bool) -> f64 {
    match (metering, has_picture) {
        (false, _) => 0.0,
        (true, true) => METER_SHARE_WITH_PICTURE,
        (true, false) => METER_SHARE_SOUND_ONLY,
    }
}

/// The render never reports more than this of its own pass before ffmpeg has exited: the file is
/// not finished until the muxer has closed it.
const HOLD: f64 = 0.99;
/// The position at which the encode is as good as written and what remains is the muxer closing
/// the file: the last packets of the encoder's flush and, for an mp4 with `faststart`, the rewrite.
const FINISHING_FROM: f64 = 0.998;

/// Where a pass is, from the output's position (pure): the fraction of the pass (held at
/// [`HOLD`] until ffmpeg exits) and the stage it is in.
pub(super) fn pass_position(position_secs: f64, total_secs: f64) -> (f64, ExportStage) {
    let pass = (position_secs.max(0.0) / total_secs.max(1e-9)).clamp(0.0, 1.0);
    let stage = if pass >= FINISHING_FROM {
        ExportStage::Finishing
    } else {
        ExportStage::Rendering
    };
    (pass.min(HOLD), stage)
}

/// The least a pass must have moved, and the least time it must have run since its first output,
/// before it estimates the time left: a first report of 1% after the seconds ffmpeg spent opening
/// the footage is not a rate.
const ETA_MIN_ADVANCE: f64 = 0.005;
const ETA_MIN_SECS: f64 = 1.0;

/// The time left (pure), from how far the pass moved (`advanced`, fraction of the whole bar) in
/// the `since_first` seconds since its **first output** — the dead time before it (opening the
/// footage, decoding up to a range's start) is not part of the rate — with `remaining` of the bar
/// still to go. `None` until there is enough of a rate to extrapolate.
pub(super) fn eta_secs(since_first: f64, advanced: f64, remaining: f64) -> Option<f64> {
    (advanced >= ETA_MIN_ADVANCE && since_first >= ETA_MIN_SECS && remaining >= 0.0).then(|| since_first * remaining / advanced)
}

/// The time left across variants rendered one after another (pure): the variant under way has
/// `this` seconds left at `fraction` of itself, and `remaining` more follow it, each as long as
/// this one takes — which is `this` over what is left of it. Near its end that is too small a
/// remainder to scale from, so it says nothing until the next variant has a rate of its own.
pub(super) fn variants_eta(this: Option<f64>, fraction: f64, remaining: usize) -> Option<f64> {
    let this = this?;
    if remaining == 0 {
        return Some(this);
    }
    let left = 1.0 - fraction.clamp(0.0, 1.0);
    (left > 0.02).then(|| this + remaining as f64 * this / left)
}

/// One finished block of ffmpeg's `-progress` stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Block {
    /// Another block: ffmpeg is working. `Some(seconds)` is how far the output has been written;
    /// `None` that it has not been written to yet (`out_time=N/A`).
    Working(Option<f64>),
    /// The last block (`progress=end`): the file is closed, and this is the position it ended at —
    /// a run's final block carries the end of the output, which no earlier block was late enough to.
    End(Option<f64>),
}

/// Reads ffmpeg's `-progress` key=value lines (`-progress pipe:1`) into [`Block`]s. A block is
/// the lines up to and including its `progress=continue` / `progress=end`.
///
/// The output's position is `out_time_us`; builds that predate it print `out_time_ms` — which is
/// microseconds too, whatever its name — and `out_time` as a timestamp, and all three read `N/A`
/// (or are left out) until the first packet is written. A negative position (a stream whose first
/// timestamp is a hair before zero) is the start.
#[derive(Debug, Default)]
pub(super) struct ProgressReader {
    us: Option<i64>,
    ms: Option<i64>,
    clock: Option<f64>,
}

impl ProgressReader {
    pub(super) fn feed(&mut self, line: &str) -> Option<Block> {
        let (key, value) = line.trim().split_once('=')?;
        let value = value.trim();
        match key {
            "out_time_us" => self.us = value.parse().ok(),
            "out_time_ms" => self.ms = value.parse().ok(),
            "out_time" => self.clock = clock_secs(value),
            "progress" => {
                let position = self
                    .us
                    .or(self.ms)
                    .map(|us| us as f64 / 1_000_000.0)
                    .or(self.clock)
                    .map(|secs| secs.max(0.0));
                *self = Self::default();
                return Some(match value {
                    "end" => Block::End(position),
                    _ => Block::Working(position),
                });
            }
            _ => {}
        }
        None
    }
}

/// `HH:MM:SS.micro` in seconds (`N/A` and anything else that is not one is `None`).
fn clock_secs(text: &str) -> Option<f64> {
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut parts = text.split(':');
    let (h, m, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let secs = h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + s.parse::<f64>().ok()?;
    secs.is_finite().then_some(if negative { -secs } else { secs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(lines: &[&str]) -> Option<Block> {
        let mut reader = ProgressReader::default();
        lines.iter().filter_map(|l| reader.feed(l)).last()
    }

    /// What ffmpeg 4.4.2 printed first, before the first packet was written, and a block after it.
    #[test]
    fn a_block_before_the_first_packet_has_no_position() {
        let before = [
            "frame=0",
            "fps=0.00",
            "stream_0_0_q=0.0",
            "bitrate=N/A",
            "total_size=N/A",
            "out_time_ms=N/A",
            "out_time=N/A",
            "dup_frames=0",
            "drop_frames=0",
            "speed=N/A",
            "progress=continue",
        ];
        assert_eq!(block(&before), Some(Block::Working(None)));
        // 5.x and later also print out_time_us as N/A.
        let later = ["out_time_us=N/A", "out_time_ms=N/A", "out_time=N/A", "progress=continue"];
        assert_eq!(block(&later), Some(Block::Working(None)));
    }

    #[test]
    fn a_block_reads_the_outputs_position_in_seconds() {
        let b = block(&[
            "frame=135",
            "out_time_us=4500000",
            "out_time_ms=4500000",
            "out_time=00:00:04.500000",
            "progress=continue",
        ]);
        assert_eq!(b, Some(Block::Working(Some(4.5))));
        assert_eq!(block(&["progress=end"]), Some(Block::End(None)));
        // The last block carries the end of the output: it is not dropped for being the last.
        assert_eq!(
            block(&["out_time_us=31488000", "out_time=00:00:31.488000", "progress=end"]),
            Some(Block::End(Some(31.488)))
        );
        // The lines of a block are not a result until it closes.
        let mut r = ProgressReader::default();
        assert_eq!(r.feed("out_time_us=1000000"), None);
        assert_eq!(r.feed("speed=1.2x"), None);
        assert_eq!(r.feed("progress=continue"), Some(Block::Working(Some(1.0))));
        // ... and one block's position is not the next one's.
        assert_eq!(r.feed("out_time=N/A"), None);
        assert_eq!(r.feed("progress=continue"), Some(Block::Working(None)));
    }

    #[test]
    fn older_builds_report_the_position_under_their_other_names() {
        // `out_time_ms` is microseconds, as it always has been.
        assert_eq!(
            block(&["out_time_ms=2500000", "progress=continue"]),
            Some(Block::Working(Some(2.5)))
        );
        assert_eq!(
            block(&["out_time=00:01:02.250000", "progress=continue"]),
            Some(Block::Working(Some(62.25)))
        );
        // out_time_us wins when it is there.
        assert_eq!(
            block(&["out_time=00:00:09.000000", "out_time_us=3000000", "progress=continue"]),
            Some(Block::Working(Some(3.0)))
        );
    }

    #[test]
    fn a_position_before_zero_is_the_start_and_nonsense_is_no_position() {
        assert_eq!(
            block(&["out_time_us=-20000", "progress=continue"]),
            Some(Block::Working(Some(0.0)))
        );
        assert_eq!(
            block(&["out_time=-00:00:00.020000", "progress=continue"]),
            Some(Block::Working(Some(0.0)))
        );
        assert_eq!(
            block(&["out_time_us=banana", "progress=continue"]),
            Some(Block::Working(None))
        );
        assert_eq!(block(&["out_time=1:2", "progress=continue"]), Some(Block::Working(None)));
        // Lines that are not key=value at all are not progress.
        let mut r = ProgressReader::default();
        assert_eq!(r.feed("Error opening input file x.mp4."), None);
        assert_eq!(r.feed(""), None);
    }

    #[test]
    fn a_range_is_measured_against_its_own_length_and_held_until_ffmpeg_exits() {
        // The cut is the 31.5 s range; the output starts at 0 whatever the range's offset, so the
        // position is compared with the length alone.
        let total = 31.5;
        assert_eq!(pass_position(0.0, total), (0.0, ExportStage::Rendering));
        let (half, stage) = pass_position(15.75, total);
        assert!((half - 0.5).abs() < 1e-12 && stage == ExportStage::Rendering);
        // Everything but the muxer's closing is written: the bar waits at 99 % and says so.
        assert_eq!(pass_position(31.48, total), (HOLD, ExportStage::Finishing));
        assert_eq!(pass_position(40.0, total), (HOLD, ExportStage::Finishing));
        assert_eq!(pass_position(31.0, total).1, ExportStage::Rendering);
        // A zero-length cut cannot divide by zero.
        assert_eq!(pass_position(1.0, 0.0).0, HOLD);
    }

    #[test]
    fn the_time_left_ignores_the_dead_time_before_the_first_output() {
        // 10 % in 5 s of output: 45 s to go — however long ffmpeg took to open the footage.
        assert_eq!(eta_secs(5.0, 0.1, 0.9), Some(45.0));
        // Not a rate yet: too little moved, or too soon.
        assert_eq!(eta_secs(5.0, 0.001, 0.999), None);
        assert_eq!(eta_secs(0.2, 0.1, 0.9), None);
        assert_eq!(eta_secs(5.0, 0.0, 1.0), None);
    }

    #[test]
    fn the_time_left_across_variants_counts_the_ones_still_to_come() {
        // The last (or only) variant: what it says it has left.
        assert_eq!(variants_eta(Some(30.0), 0.5, 0), Some(30.0));
        // Halfway through the first of three, 30 s left in it: it takes 60 s, two more follow.
        assert_eq!(variants_eta(Some(30.0), 0.5, 2), Some(150.0));
        // No rate yet, or too little left of this one to scale from.
        assert_eq!(variants_eta(None, 0.5, 2), None);
        assert_eq!(variants_eta(Some(0.4), 0.99, 2), None);
        assert_eq!(variants_eta(Some(0.4), 0.99, 0), Some(0.4));
    }

    #[test]
    fn a_stage_owns_a_share_of_the_bar_and_the_elapsed_time_is_the_exports() {
        let since = Instant::now() - std::time::Duration::from_secs(30);
        let p = ExportProgress {
            fraction: 0.5,
            elapsed_secs: 1.0,
            eta_secs: Some(9.0),
            waiting: None,
            stage: Some(ExportStage::Rendering),
        };
        let seen = p.within(0.08, 1.0, since);
        assert!((seen.fraction - 0.54).abs() < 1e-12, "{}", seen.fraction);
        assert!(seen.elapsed_secs >= 30.0);
        assert_eq!((seen.eta_secs, seen.stage), (Some(9.0), Some(ExportStage::Rendering)));
        // The metering pass fills its share and no more, however it rounds.
        let meter = ExportProgress { fraction: 1.7, ..p }.within(0.0, 0.08, since);
        assert!((meter.fraction - 0.08).abs() < 1e-12);
        // Nothing owns a share: the stage's 0 is where it starts.
        assert_eq!(ExportProgress { fraction: 0.0, ..p }.within(0.08, 1.0, since).fraction, 0.08);
    }

    #[test]
    fn the_shares_are_nothing_without_a_metering_pass_and_never_the_whole_bar() {
        assert_eq!(metering_share(false, true), 0.0);
        assert_eq!(metering_share(false, false), 0.0);
        for picture in [true, false] {
            let m = metering_share(true, picture);
            assert!(m > 0.0 && m + VERIFY_SHARE < 1.0, "{picture}: {m}");
        }
        // Sound alone: metering is most of the work of the encode.
        assert!(metering_share(true, false) > metering_share(true, true));
    }

    #[test]
    fn a_stage_reads_as_words_and_serializes_in_snake_case() {
        assert_eq!(ExportStage::Metering.label(), "metering loudness");
        assert_eq!(ExportStage::Rendering.label(), "rendering");
        assert_eq!(serde_json::to_string(&ExportStage::Starting).unwrap(), "\"starting\"");
        assert_eq!(serde_json::to_string(&ExportStage::Measuring).unwrap(), "\"measuring\"");
        let p = ExportProgress {
            fraction: 0.5,
            elapsed_secs: 1.0,
            eta_secs: None,
            waiting: None,
            stage: Some(ExportStage::Finishing),
        };
        let json = serde_json::to_value(p).unwrap();
        assert_eq!(json["stage"], "finishing");
        assert!(json.get("waiting").is_none(), "an absent waiting stays off the wire");
    }
}
