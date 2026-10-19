//! Whether frames are being asked for like a drag or like a click, from how fast the requests
//! arrive. A drag wants each frame on the screen as soon as it is drawn (no wait for the next
//! refresh: `AutoNoVsync`, where the surface has `Immediate` or `Mailbox`); a settled frame or an
//! occasional click is better on vsync, and a surface is reconfigured to change that, so the
//! decision has to be steady.
//!
//! A gap between two requests under [`DRAG_GAP`] is a drag and turns it on at once. It stays on
//! until the requests stop being close: two gaps of [`LULL_GAP`] or more in a row turn it off, so a
//! drag that pauses for a moment, or a click and then a drag, does not reconfigure the surface at
//! every start and stop. The page's own cadence is a request per animation frame (16 ms) while a
//! playhead moves, and one per click otherwise.
//!
//! [`Trend`] reads the same requests for their direction: a drag that walks forward is decoded like
//! playback, reading ahead.

use std::time::{Duration, Instant};

/// Requests this close together are a drag.
pub const DRAG_GAP: Duration = Duration::from_millis(120);
/// Requests this far apart are not.
pub const LULL_GAP: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, Default)]
pub struct Pace {
    last: Option<Instant>,
    lulls: u8,
    dragging: bool,
}

impl Pace {
    /// A request arrived at `now`; whether frames are being dragged.
    pub fn observe(&mut self, now: Instant) -> bool {
        if let Some(last) = self.last.replace(now) {
            let gap = now.saturating_duration_since(last);
            if gap < DRAG_GAP {
                self.dragging = true;
                self.lulls = 0;
            } else if gap >= LULL_GAP {
                self.lulls = self.lulls.saturating_add(1);
                if self.lulls >= 2 {
                    self.dragging = false;
                }
            }
        }
        self.dragging
    }

    #[cfg(test)]
    pub fn dragging(&self) -> bool {
        self.dragging
    }
}

/// A frame this far after the one before it (seconds of the cut) is the next step of a forward walk.
pub const FORWARD_STEP_MAX: f64 = 0.5;
/// Steps forward in a row before a drag is read as walking forward.
pub const FORWARD_RUN: u8 = 3;

/// Whether the frames of a drag walk forward through the cut (a slow drag to the right, shuttle,
/// a held arrow key): each a little later than the one before. Such a drag is decoded like playback
/// (`Hint::Forward`: the run reads ahead of the frame asked for, so the next one is in the cache
/// when it is asked for); any other is a scrub. Frames of play sessions are not observed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Trend {
    last: Option<f64>,
    run: u8,
}

impl Trend {
    /// A frame at `t` was asked for, in a drag or not; whether it is read forward.
    pub fn observe(&mut self, t: f64, dragging: bool) -> bool {
        let step = self.last.replace(t).map(|last| t - last);
        if step.is_some_and(|d| d > 0.0 && d <= FORWARD_STEP_MAX) {
            self.run = self.run.saturating_add(1);
        } else {
            self.run = 0;
        }
        dragging && self.run >= FORWARD_RUN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_first_request_or_a_click_is_not_a_drag_and_close_requests_are() {
        let t0 = Instant::now();
        let mut pace = Pace::default();
        assert!(!pace.observe(t0), "the first request has no gap");
        assert!(!pace.observe(t0 + ms(2000)), "a click a while later");
        assert!(pace.observe(t0 + ms(2016)), "the next request 16 ms on is a drag");
        assert!(pace.observe(t0 + ms(2032)));
        assert!(pace.dragging());
    }

    #[test]
    fn a_drag_that_steps_forward_a_few_times_in_a_row_is_read_forward() {
        let mut trend = Trend::default();
        let frame = 1.0 / 30.0;
        assert!(!trend.observe(1.0, true), "a first frame has no direction");
        assert!(!trend.observe(1.0 + frame, true));
        assert!(!trend.observe(1.0 + 2.0 * frame, true));
        assert!(trend.observe(1.0 + 3.0 * frame, true), "the third step forward in a row");
        assert!(trend.observe(1.4, true), "a longer step is still a step");
        assert!(
            !trend.observe(1.4, true),
            "the same time again (a slider dragged at a resting playhead)"
        );
        assert!(!trend.observe(1.5, true));
        assert!(!trend.observe(1.6, true));
        assert!(trend.observe(1.7, true));
        assert!(!trend.observe(1.6, true), "a step back ends it");
        for t in [1.7, 1.8, 1.9] {
            trend.observe(t, true);
        }
        assert!(!trend.observe(3.0, true), "a jump is not a step");
        for t in [3.1, 3.2] {
            trend.observe(t, true);
        }
        assert!(!trend.observe(3.3, false), "clicks a frame apart are not a drag");
        assert!(trend.observe(3.4, true));
    }

    #[test]
    fn a_drag_that_pauses_stays_a_drag_and_two_lulls_in_a_row_end_it() {
        let t0 = Instant::now();
        let mut pace = Pace::default();
        pace.observe(t0);
        pace.observe(t0 + ms(16));
        assert!(pace.dragging());
        // A pause of a second, then moving again: one lull is not the end.
        assert!(pace.observe(t0 + ms(1016)));
        assert!(pace.observe(t0 + ms(1032)), "and the drag goes on without a change of mode");
        // Gaps between a drag's pace and a lull change nothing.
        assert!(pace.observe(t0 + ms(1032 + 300)));
        // Two lulls with nothing close between them.
        assert!(pace.observe(t0 + ms(3000)));
        assert!(!pace.observe(t0 + ms(5000)), "the second lull in a row ends it");
        // Isolated requests stay off; a close pair turns it on again at once.
        assert!(!pace.observe(t0 + ms(8000)));
        assert!(pace.observe(t0 + ms(8050)));
    }
}
