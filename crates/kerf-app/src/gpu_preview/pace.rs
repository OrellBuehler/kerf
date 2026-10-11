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
