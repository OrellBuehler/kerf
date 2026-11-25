//! How long the proxy queue will take: the arithmetic, pure and unit-tested, and the one thin
//! piece of state (the smoothed rate) it is fed from.
//!
//! Everything is measured in **pixel-seconds**: the source's width x height x its length in
//! seconds. A proxy encode costs about as much as the picture it reads, so a 4K clip is four
//! 1080p clips of the same length and "seconds of footage" alone would call them equal. The
//! rate is pixel-seconds built per wall second, per worker, measured from this session's own
//! builds: the machine, its share of the cores and the footage differ too much for a constant.

use std::collections::HashMap;

use super::Pipeline;

/// A build is not extrapolated before this fraction of it is done: the first moments are the
/// decoder opening the file, not the speed of the encode.
pub(super) const ETA_FROM: f64 = 0.02;

/// The weight of the newest sample in the smoothed rate: a run of different footage moves it
/// within a few jobs without one odd clip (a tiny one dominated by ffmpeg's start-up, a 4K one
/// that hit a slow decoder) throwing every estimate after it.
pub(super) const SMOOTHING: f64 = 0.3;

/// A build faster than this is not a measurement (an encode that was already on disk, a clock
/// that did not move): its rate would be enormous and every estimate after it a lie.
const MIN_WALL_SECS: f64 = 0.05;

/// What a source costs: its picture size times its length; `0` when either is not known.
pub(super) fn pixel_seconds(width: u32, height: u32, duration: f64) -> f64 {
    let cost = f64::from(width) * f64::from(height) * duration;
    if cost.is_finite() && cost > 0.0 {
        cost
    } else {
        0.0
    }
}

/// The rate of a build that finished: `None` when the numbers say nothing.
pub(super) fn sample(pixel_seconds: f64, wall_secs: f64) -> Option<f64> {
    (pixel_seconds > 0.0 && wall_secs.is_finite() && wall_secs >= MIN_WALL_SECS).then(|| pixel_seconds / wall_secs)
}

/// `previous` moved a step toward `sample`.
pub(super) fn smooth(previous: Option<f64>, sample: f64) -> f64 {
    match previous {
        Some(rate) => rate + SMOOTHING * (sample - rate),
        None => sample,
    }
}

/// What a build still running says its rate is, with how much of it is done (its weight): `None`
/// until it is past [`ETA_FROM`].
pub(super) fn live_sample(pixel_seconds: f64, fraction: f64, elapsed_secs: f64) -> Option<(f64, f64)> {
    (pixel_seconds > 0.0 && fraction > ETA_FROM && fraction <= 1.0 && elapsed_secs > 0.0 && elapsed_secs.is_finite())
        .then(|| (pixel_seconds * fraction / elapsed_secs, fraction))
}

/// The rate the estimates use: the smoothed rate of the builds that finished, moved toward each
/// running build's own by as much of it as is done. The weight is what keeps it continuous: a
/// build 99% done moves it nearly as far as the finished build's sample will, so the estimate
/// does not jump when it ends. With nothing finished yet, a running build's rate is the rate.
pub(super) fn current_rate(finished: Option<f64>, live: &[(f64, f64)]) -> Option<f64> {
    let rate = match (finished, live) {
        (finished, []) => finished,
        (None, live) => Some(live.iter().map(|(rate, _)| rate).sum::<f64>() / live.len() as f64),
        (Some(finished), live) => Some(
            live.iter()
                .fold(finished, |rate, (sample, done)| rate + SMOOTHING * done * (sample - rate)),
        ),
    };
    rate.filter(|r| r.is_finite() && *r > 0.0)
}

/// One job as the estimator sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Load {
    /// What it costs (see [`pixel_seconds`]); `0` when the source size is not known.
    pub pixel_seconds: f64,
    /// How far along it is, `0.0..=1.0`.
    pub fraction: f64,
    /// Whether it holds a worker now (is being encoded).
    pub running: bool,
}

/// Seconds from now until each of `loads` is done, in the order given (`None`: not knowable).
///
/// `rate` is one worker's; `workers` encode at once, each taking the next job in the list as it
/// frees up, which is what the queue does. Builds already running keep a worker until they end;
/// the queued ones follow in list order. With `start_queued` false (the queue is paused) the
/// queued jobs have no time at all, only the running ones do. A job whose size is unknown costs
/// what the known ones cost on average; with no known size, or no rate yet, nothing is knowable.
pub(super) fn finish_times(loads: &[Load], rate: Option<f64>, workers: usize, start_queued: bool) -> Vec<Option<f64>> {
    let nothing = vec![None; loads.len()];
    let Some(rate) = rate.filter(|r| r.is_finite() && *r > 0.0) else {
        return nothing;
    };
    let known: Vec<f64> = loads
        .iter()
        .map(|l| l.pixel_seconds)
        .filter(|p| p.is_finite() && *p > 0.0)
        .collect();
    let average = (!known.is_empty()).then(|| known.iter().sum::<f64>() / known.len() as f64);
    let remaining = |load: &Load| -> Option<f64> {
        let cost = if load.pixel_seconds.is_finite() && load.pixel_seconds > 0.0 {
            load.pixel_seconds
        } else {
            average?
        };
        Some(cost * (1.0 - load.fraction.clamp(0.0, 1.0)) / rate)
    };
    let mut free = vec![0.0_f64; workers.max(1)];
    let mut out = vec![None; loads.len()];
    let order = loads
        .iter()
        .enumerate()
        .filter(|(_, l)| l.running)
        .chain(loads.iter().enumerate().filter(|(_, l)| !l.running && start_queued));
    for (at, load) in order {
        let Some(cost) = remaining(load) else {
            return nothing;
        };
        let worker = free
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(i, _)| i);
        free[worker] += cost;
        out[at] = Some(free[worker]);
    }
    out
}

/// The smoothed rate of the builds this session has finished, per pipeline: the only state the
/// estimates keep. A pipeline with no finished build has none, and its estimates are `None`
/// until the first lands (or one running passes [`ETA_FROM`]).
#[derive(Debug, Default)]
pub(super) struct Rates {
    finished: HashMap<Pipeline, f64>,
}

impl Rates {
    /// Count a finished build of `pixel_seconds` that took `wall_secs` of the slot (not of
    /// waiting for it). Returns the new rate, or `None` when the build says nothing.
    pub fn record(&mut self, pipeline: Pipeline, pixel_seconds: f64, wall_secs: f64) -> Option<f64> {
        let sample = sample(pixel_seconds, wall_secs)?;
        let rate = smooth(self.finished.get(&pipeline).copied(), sample);
        self.finished.insert(pipeline, rate);
        Some(rate)
    }

    /// The smoothed rate of the builds that finished.
    pub fn finished(&self, pipeline: Pipeline) -> Option<f64> {
        self.finished.get(&pipeline).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: f64 = 1920.0 * 1080.0;
    const UHD: f64 = 3840.0 * 2160.0;

    fn queued(pixel_seconds: f64) -> Load {
        Load {
            pixel_seconds,
            fraction: 0.0,
            running: false,
        }
    }

    fn running(pixel_seconds: f64, fraction: f64) -> Load {
        Load {
            pixel_seconds,
            fraction,
            running: true,
        }
    }

    fn near(got: Option<f64>, want: f64) {
        let got = got.expect("an estimate");
        assert!((got - want).abs() < 1e-6, "{got} is not {want}");
    }

    #[test]
    fn a_4k_clip_costs_four_times_a_1080p_clip_of_the_same_length() {
        assert_eq!(pixel_seconds(3840, 2160, 10.0), 4.0 * pixel_seconds(1920, 1080, 10.0));
        assert_eq!(pixel_seconds(0, 1080, 10.0), 0.0);
        assert_eq!(pixel_seconds(1920, 1080, 0.0), 0.0);
        assert_eq!(pixel_seconds(1920, 1080, f64::NAN), 0.0);
        assert_eq!(pixel_seconds(1920, 1080, -3.0), 0.0);
    }

    #[test]
    fn one_worker_finishes_the_jobs_one_after_the_other() {
        // 100 pixel-seconds per second: 1000, 500 and 2000 take 10, 5 and 20 s.
        let loads = [queued(1000.0), queued(500.0), queued(2000.0)];
        let done = finish_times(&loads, Some(100.0), 1, true);
        near(done[0], 10.0);
        near(done[1], 15.0);
        near(done[2], 35.0);
    }

    #[test]
    fn a_running_build_counts_only_what_is_left_of_it() {
        let loads = [running(1000.0, 0.75), queued(1000.0)];
        let done = finish_times(&loads, Some(100.0), 1, true);
        near(done[0], 2.5);
        near(done[1], 12.5);
        // A fraction past the end is clamped, not negative time.
        near(finish_times(&[running(1000.0, 1.5)], Some(100.0), 1, true)[0], 0.0);
    }

    #[test]
    fn a_running_build_goes_first_whatever_its_place_in_the_list() {
        let loads = [queued(1000.0), running(1000.0, 0.5)];
        let done = finish_times(&loads, Some(100.0), 1, true);
        near(done[1], 5.0);
        near(done[0], 15.0);
    }

    #[test]
    fn two_workers_take_the_next_job_as_one_frees_up() {
        // Each worker encodes at 100: jobs of 10, 10, 10 and 20 s.
        let loads = [queued(1000.0), queued(1000.0), queued(1000.0), queued(2000.0)];
        let done = finish_times(&loads, Some(100.0), 2, true);
        near(done[0], 10.0);
        near(done[1], 10.0);
        near(done[2], 20.0);
        near(done[3], 30.0);
        // The same list on one worker is the sum.
        near(finish_times(&loads, Some(100.0), 1, true)[3], 50.0);
        // A running build keeps its worker until it ends.
        let loads = [running(1000.0, 0.0), queued(500.0), queued(500.0)];
        let done = finish_times(&loads, Some(100.0), 2, true);
        near(done[0], 10.0);
        near(done[1], 5.0);
        near(done[2], 10.0);
        // Zero workers is one: the queue cannot be built by nobody.
        near(finish_times(&loads, Some(100.0), 0, true)[0], 10.0);
    }

    #[test]
    fn a_paused_queue_has_a_time_for_what_is_running_and_none_for_the_rest() {
        let loads = [running(1000.0, 0.5), queued(1000.0)];
        let done = finish_times(&loads, Some(100.0), 1, false);
        near(done[0], 5.0);
        assert_eq!(done[1], None);
    }

    #[test]
    fn no_rate_yet_is_no_estimate() {
        let loads = [running(1000.0, 0.5), queued(1000.0)];
        assert_eq!(finish_times(&loads, None, 1, true), [None, None]);
        assert_eq!(finish_times(&loads, Some(0.0), 1, true), [None, None]);
        assert_eq!(finish_times(&loads, Some(f64::NAN), 1, true), [None, None]);
        assert!(finish_times(&[], Some(1.0), 1, true).is_empty());
    }

    #[test]
    fn a_source_of_unknown_size_costs_what_the_known_ones_cost_on_average() {
        let loads = [queued(1000.0), queued(0.0), queued(3000.0)];
        let done = finish_times(&loads, Some(100.0), 1, true);
        near(done[0], 10.0);
        near(done[1], 10.0 + 20.0);
        near(done[2], 10.0 + 20.0 + 30.0);
        // With nothing known there is nothing to average.
        assert_eq!(
            finish_times(&[queued(0.0), queued(f64::NAN)], Some(100.0), 1, true),
            [None, None]
        );
    }

    #[test]
    fn a_build_is_extrapolated_only_once_it_is_past_two_percent() {
        assert_eq!(live_sample(HD * 10.0, 0.02, 5.0), None);
        assert_eq!(live_sample(HD * 10.0, 0.0, 5.0), None);
        assert_eq!(live_sample(0.0, 0.5, 5.0), None);
        assert_eq!(live_sample(HD * 10.0, 0.5, 0.0), None);
        let (rate, weight) = live_sample(1000.0, 0.5, 5.0).unwrap();
        assert_eq!((rate, weight), (100.0, 0.5));
        // The same threshold the status's own ETA uses.
        assert_eq!(super::super::eta(0.02, std::time::Duration::from_secs(1)), None);
        assert!(super::super::eta(0.021, std::time::Duration::from_secs(1)).is_some());
    }

    #[test]
    fn the_smoothed_rate_follows_the_footage_without_jumping() {
        assert_eq!(smooth(None, 80.0), 80.0);
        assert!((smooth(Some(100.0), 200.0) - 130.0).abs() < 1e-9);
        let mut rates = Rates::default();
        assert_eq!(rates.finished(Pipeline::Software), None);
        assert_eq!(rates.record(Pipeline::Software, UHD * 10.0, 20.0), Some(UHD * 10.0 / 20.0));
        let first = rates.finished(Pipeline::Software).unwrap();
        // A quarter of the pixels in a quarter of the time is the same rate: it moves nothing.
        rates.record(Pipeline::Software, HD * 10.0, 5.0);
        assert!((rates.finished(Pipeline::Software).unwrap() - first).abs() < 1e-6);
        // A slower one pulls it down by the smoothing weight.
        rates.record(Pipeline::Software, UHD * 10.0, 40.0);
        let expected = first + SMOOTHING * (first / 2.0 - first);
        assert!((rates.finished(Pipeline::Software).unwrap() - expected).abs() < 1e-6);
    }

    #[test]
    fn a_build_that_says_nothing_is_not_counted() {
        let mut rates = Rates::default();
        for (cost, wall) in [
            (0.0, 5.0),
            (HD, 0.0),
            (HD, 0.01),
            (HD, f64::NAN),
            (HD, f64::INFINITY),
            (-1.0, 5.0),
        ] {
            assert_eq!(rates.record(Pipeline::Software, cost, wall), None, "{cost} in {wall}");
        }
        assert_eq!(rates.finished(Pipeline::Software), None);
    }

    #[test]
    fn the_rate_is_the_finished_ones_moved_toward_the_running_by_as_much_as_is_done() {
        // Nothing finished: a running build's own rate is the rate (what the status's ETA said).
        assert_eq!(current_rate(None, &[]), None);
        assert_eq!(current_rate(None, &[(100.0, 0.5)]), Some(100.0));
        assert_eq!(current_rate(None, &[(100.0, 0.5), (200.0, 0.1)]), Some(150.0));
        // Something finished, nothing past 2% running: the finished rate.
        assert_eq!(current_rate(Some(80.0), &[]), Some(80.0));
        // A running one nudges it by its weight...
        let nudged = current_rate(Some(100.0), &[(200.0, 0.1)]).unwrap();
        assert!((nudged - (100.0 + SMOOTHING * 0.1 * 100.0)).abs() < 1e-9);
        // ...and a build that is all but done nudges it exactly as far as finishing will.
        let almost = current_rate(Some(100.0), &[(200.0, 1.0)]).unwrap();
        assert!((almost - smooth(Some(100.0), 200.0)).abs() < 1e-9);
        // A rate that is not a rate is none.
        assert_eq!(current_rate(Some(f64::NAN), &[]), None);
    }
}
