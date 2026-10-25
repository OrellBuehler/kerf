//! A clip's noise reduction (`AudioEffect::Denoise`): FFmpeg's `afftdn`, with its delay taken back out
//! where this ffmpeg has one.
//!
//! `afftdn` works on windows of three hops of `sample_rate / 80` samples (integer division). Since
//! FFmpeg 5.1 it puts out a hop for every hop it takes in, stamped with the input's time, but what it
//! puts out is the window two hops back: the sound comes out `2 * (sample_rate / 80)` samples late
//! (25 ms at 48 kHz, 1102 samples at 44.1 kHz) and its last two hops never come out. Before 5.1 (4.4,
//! Ubuntu 22.04's, and 5.0) it waited for a whole window and put out its first hop, so the sound kept
//! its time. Where it delays ([`afftdn_delays`]) the chain rebases the stream to zero, pads its end by
//! the delay, denoises and cuts the delay off the head: the clip keeps its length and its sync.

use super::*;

/// The delay a delaying `afftdn` adds, as the `apad` / `atrim` duration that takes it back out:
/// `atrim` and `apad` round it to the nearest sample, which is exactly `2 * (sample_rate / 80)`
/// samples at 8, 12, 16, 24, 32, 44.1, 48 and 96 kHz (one sample off at 22.05, 88.2 and 192 kHz).
const DELAY: &str = "0.024995";

#[cfg(test)]
thread_local! {
    /// What [`afftdn_delays`] answers on this thread while a test pins it.
    static AFFTDN_DELAY_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `f` as if this ffmpeg's `afftdn` did (`true`) or did not (`false`) delay its output.
#[cfg(test)]
pub(super) fn with_afftdn_delay<R>(delays: bool, f: impl FnOnce() -> R) -> R {
    struct Reset(Option<bool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            AFFTDN_DELAY_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _reset = Reset(AFFTDN_DELAY_OVERRIDE.with(|c| c.replace(Some(delays))));
    f()
}

/// Whether `ffmpeg -h filter=afftdn` lists `noise_reduction` (pure, unit-tested): the long option
/// names came with the 5.1 rewrite that made the filter delay its output.
fn help_lists_noise_reduction(help: &str) -> bool {
    help.lines().any(|l| l.split_whitespace().next() == Some("noise_reduction"))
}

/// Whether this ffmpeg's `afftdn` delays its output (see the module docs), probed once per process.
/// A binary that will not run reads as delaying (the current filter, like
/// [`alimiter_latency_available`]: any render on it is about to fail anyway, and it keeps the
/// builders' output from depending on whether a test machine has ffmpeg).
pub(super) fn afftdn_delays() -> bool {
    #[cfg(test)]
    if let Some(forced) = AFFTDN_DELAY_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static DELAYS: OnceLock<bool> = OnceLock::new();
    *DELAYS.get_or_init(|| {
        let delays = output_bounded(
            command(&ffmpeg_bin()).args(["-hide_banner", "-loglevel", "quiet", "-h", "filter=afftdn"]),
            CAPABILITY_PROBE_TIMEOUT,
            0,
        )
        .map(|o| help_lists_noise_reduction(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or(true);
        tracing::debug!(delays, "probed ffmpeg for afftdn's delay");
        delays
    })
}

/// The filters of one `Denoise`: `afftdn` with the reduction and the floor (`tn=1` only when the
/// floor is tracked), wrapped in the rebase, pad and trim that take its delay back out where this
/// ffmpeg has one.
pub(super) fn denoise_filter(reduction_db: f64, floor_db: f64, track_noise: bool) -> String {
    let track = if track_noise { ":tn=1" } else { "" };
    let afftdn = format!("afftdn=nr={}:nf={}{track}", fnum(reduction_db), fnum(floor_db));
    if afftdn_delays() {
        format!("asetpts=PTS-STARTPTS,apad=pad_dur={DELAY},{afftdn},atrim=start={DELAY},asetpts=PTS-STARTPTS")
    } else {
        afftdn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AudioEffect;

    fn denoise(reduction_db: f64, floor_db: f64, track_noise: bool) -> AudioEffect {
        AudioEffect::Denoise {
            reduction_db,
            floor_db,
            track_noise,
        }
    }

    #[test]
    fn a_denoise_is_afftdn_with_its_delay_taken_out_where_the_filter_delays() {
        let chain = with_afftdn_delay(true, || audio_effects_filter(&[denoise(12.0, -50.0, false)]));
        assert_eq!(
            chain.as_deref(),
            Some(
                "asetpts=PTS-STARTPTS,apad=pad_dur=0.024995,afftdn=nr=12:nf=-50,atrim=start=0.024995,\
                 asetpts=PTS-STARTPTS"
            )
        );
        let chain = with_afftdn_delay(false, || audio_effects_filter(&[denoise(12.0, -50.0, false)]));
        assert_eq!(chain.as_deref(), Some("afftdn=nr=12:nf=-50"));
    }

    #[test]
    fn tracking_the_floor_is_spelled_only_when_it_is_on_and_the_chain_keeps_its_order() {
        let chain = with_afftdn_delay(false, || {
            audio_effects_filter(&[
                AudioEffect::Highpass { hz: 80.0 },
                denoise(20.5, -62.25, true),
                AudioEffect::Gate { threshold_db: -40.0 },
            ])
        });
        assert_eq!(
            chain.as_deref(),
            Some("highpass=f=80,afftdn=nr=20.5:nf=-62.25:tn=1,agate=threshold=0.01")
        );
    }

    #[test]
    fn the_track_flag_may_be_left_out_of_the_json() {
        let e: AudioEffect = serde_json::from_str(r#"{"type":"denoise","reduction_db":12,"floor_db":-50}"#).unwrap();
        assert_eq!(e, denoise(12.0, -50.0, false));
        assert!(
            e.is_dynamic(),
            "its floor is a level: a gain ahead of it changes what is noise"
        );
        assert_eq!(e.name(), "denoise");
    }

    #[test]
    fn the_delay_is_read_off_the_long_option_names_of_the_rewrite() {
        let modern = "afftdn AVOptions:\n   noise_reduction   <float>      ..F.A....T. set the noise reduction\n   \
                      nr                <float>      ..F.A....T. set the noise reduction";
        let old = "afftdn AVOptions:\n  nr                <float>      ..F.A...... set the noise reduction (from 0.01 to 97)\n  \
                   nf                <float>      ..F.A...... set the noise floor";
        assert!(help_lists_noise_reduction(modern));
        assert!(!help_lists_noise_reduction(old));
        assert!(!help_lists_noise_reduction("Unknown filter 'afftdn'."));
    }
}
