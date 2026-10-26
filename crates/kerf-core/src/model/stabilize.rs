//! Video stabilization: the shake of a hand-held camera taken out of a clip's picture.
//!
//! A clip carries the setting ([`Clip::stabilize`]); what the camera did is measured once, by an
//! analysis of the clip's footage (`crate::stabilize`), and the export and the preview move each
//! source frame against it. The setting is all the timeline holds: the measurement is a cache
//! beside the footage, so a stabilized clip is a few bytes in the history, and a clip with none is
//! absent from the file (every stored timeline is byte-identical).
//!
//! **Per source frame.** The correction belongs to a frame of the file, not to a moment of the
//! timeline, so speed, reverse and speed ramps change nothing about it: the graph moves the frames
//! straight after it trims them, before they are retimed.

use super::*;

/// The most a stabilized picture may be enlarged to hide the edges it moved, in percent.
pub const MAX_STABILIZE_ZOOM: f64 = 30.0;

/// How strongly a clip is stabilized ([`Clip::stabilize`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Stabilize {
    /// How much of the camera's movement is smoothed away, `0..=1`: the radius of the smoothing of
    /// the camera's path, up to a second of it (vid.stab), or how far the motion search looks
    /// (`deshake`, where vid.stab is missing). `0.5` is a hand-held walk made steady; `1` holds a
    /// slow pan nearly still.
    #[serde(default = "default_strength")]
    pub strength: f64,
    /// How much the picture is enlarged to hide the edges the correction moves into view, in
    /// percent (`0..=MAX_STABILIZE_ZOOM`). `None` is automatic: the least zoom that keeps the edges
    /// out of the clip's frames (vid.stab, from the analysis), or one sized to the motion search
    /// (`deshake`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f64>,
}

fn default_strength() -> f64 {
    0.5
}

impl Default for Stabilize {
    fn default() -> Self {
        Self {
            strength: default_strength(),
            zoom: None,
        }
    }
}

impl Stabilize {
    /// Refuse numbers the graph cannot use: a strength outside `0..=1` or a zoom outside
    /// `0..=MAX_STABILIZE_ZOOM` (or either not finite).
    pub fn check(&self) -> Result<()> {
        if !self.strength.is_finite() || !(0.0..=1.0).contains(&self.strength) {
            return Err(Error::InvalidArgument(format!(
                "stabilization strength must be between 0 and 1, not {}",
                self.strength
            )));
        }
        if let Some(zoom) = self.zoom {
            if !zoom.is_finite() || !(0.0..=MAX_STABILIZE_ZOOM).contains(&zoom) {
                return Err(Error::InvalidArgument(format!(
                    "stabilization zoom must be between 0 and {MAX_STABILIZE_ZOOM} percent, not {zoom}"
                )));
            }
        }
        Ok(())
    }

    /// The setting held to its ranges, for a reader that must not fail on a stored one.
    pub fn normalized(&self) -> Self {
        let clamp = |v: f64, hi: f64| if v.is_finite() { v.clamp(0.0, hi) } else { 0.0 };
        Self {
            strength: if self.strength.is_finite() {
                self.strength.clamp(0.0, 1.0)
            } else {
                default_strength()
            },
            zoom: self.zoom.map(|z| clamp(z, MAX_STABILIZE_ZOOM)),
        }
    }
}

impl Clip {
    /// Whether this clip's picture is stabilized: it asks to be, and it is footage that moves (a
    /// title draws its own picture and has none to steady).
    pub fn is_stabilized(&self) -> bool {
        self.stabilize.is_some() && !self.is_generated()
    }
}

/// What a stabilization edit changed, for the review diff.
pub(super) fn stabilize_changes(before: &Clip, after: &Clip) -> Option<String> {
    if before.stabilize == after.stabilize {
        return None;
    }
    Some(match (&before.stabilize, &after.stabilize) {
        (_, None) => "stabilization off".to_string(),
        (None, Some(s)) => format!("stabilized ({:.0}%)", s.strength * 100.0),
        (Some(_), Some(s)) => format!("stabilization {:.0}%", s.strength * 100.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_outside_its_ranges_is_refused_and_a_stored_one_is_held_to_them() {
        assert!(Stabilize::default().check().is_ok());
        for bad in [
            Stabilize {
                strength: 1.5,
                zoom: None,
            },
            Stabilize {
                strength: f64::NAN,
                zoom: None,
            },
            Stabilize {
                strength: 0.5,
                zoom: Some(-1.0),
            },
            Stabilize {
                strength: 0.5,
                zoom: Some(MAX_STABILIZE_ZOOM + 1.0),
            },
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
            let n = bad.normalized();
            assert!(n.check().is_ok(), "{n:?}");
        }
    }

    #[test]
    fn the_json_names_only_what_is_set() {
        let json = serde_json::to_string(&Stabilize::default()).unwrap();
        assert_eq!(json, r#"{"strength":0.5}"#);
        let read: Stabilize = serde_json::from_str("{}").unwrap();
        assert_eq!(read, Stabilize::default());
        let zoomed: Stabilize = serde_json::from_str(r#"{"strength":1,"zoom":8}"#).unwrap();
        assert_eq!(zoomed.zoom, Some(8.0));
    }

    #[test]
    fn a_clip_without_the_setting_stores_what_it_always_did() {
        let clip = Clip::new(Uuid::nil(), 0.0, 1.0, 0.0);
        assert!(!serde_json::to_string(&clip).unwrap().contains("stabilize"));
        assert!(!clip.is_stabilized());
        let mut steady = clip.clone();
        steady.stabilize = Some(Stabilize::default());
        assert!(steady.is_stabilized());
        assert_eq!(stabilize_changes(&clip, &steady).as_deref(), Some("stabilized (50%)"));
        assert_eq!(stabilize_changes(&steady, &clip).as_deref(), Some("stabilization off"));
        assert_eq!(stabilize_changes(&clip, &clip), None);
    }
}
