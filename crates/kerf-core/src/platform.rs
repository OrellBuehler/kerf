//! Where a finished cut is going, and whether it is ready to go there.
//!
//! Everything else in Kerf ends at the rendered file. A creator's job does not:
//! the file still has to be accepted by a platform and then actually shown to
//! people. Those are two different bars, and the second one fails silently —
//! which is what this module exists to say out loud before the render, while the
//! cut can still be changed.

use serde::{Deserialize, Serialize};

use crate::model::Delivery;

/// A place a cut gets published, and what it asks of the file.
///
/// Two kinds of limit matter and they are not the same. A **hard** limit is what
/// the platform refuses to accept. A **reach** limit is what it accepts and then
/// stops distributing: a four-minute Reel uploads fine and is shown only to
/// people who already follow you. That is the worse outcome of the two, because
/// nothing tells you it happened.
///
/// The numbers were verified 2026-08-25. They are other companies' product
/// decisions and they move, so they are **advisory** — Kerf says what it thinks
/// and still exports whatever you ask for.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PlatformTarget {
    pub id: &'static str,
    pub label: &'static str,
    /// The delivery frame this platform is authored for.
    pub width: u32,
    pub height: u32,
    /// Aspect ratios that get full treatment, as `(w, h)`. Anything else is
    /// letterboxed or pillarboxed by the platform rather than rejected.
    pub accepts: &'static [(u32, u32)],
    /// Longest the platform will accept at all.
    pub max_secs: Option<f64>,
    /// Longest that still gets distributed to non-followers.
    pub reach_max_secs: Option<f64>,
    /// Shortest the platform will accept.
    pub min_secs: Option<f64>,
    /// Where the limits come from / what else to know.
    pub notes: &'static str,
}

const VERTICAL: &[(u32, u32)] = &[(9, 16)];
const VERTICAL_OR_SQUARE: &[(u32, u32)] = &[(9, 16), (4, 5), (1, 1)];
const LANDSCAPE: &[(u32, u32)] = &[(16, 9)];

/// The publishing targets Kerf knows about, in the order a small brand tends to
/// reach for them.
pub const TARGETS: &[PlatformTarget] = &[
    PlatformTarget {
        id: "reels",
        label: "Instagram Reels",
        width: 1080,
        height: 1920,
        accepts: VERTICAL,
        max_secs: Some(20.0 * 60.0),
        reach_max_secs: Some(3.0 * 60.0),
        min_secs: Some(3.0),
        notes: "Uploads accept up to 20 min, but past 3 min a Reel is only shown to existing followers.",
    },
    PlatformTarget {
        id: "shorts",
        label: "YouTube Shorts",
        width: 1080,
        height: 1920,
        accepts: VERTICAL_OR_SQUARE,
        max_secs: Some(3.0 * 60.0),
        reach_max_secs: None,
        min_secs: None,
        notes: "Hard 3 min cap since Oct 2024; anything longer is published as a regular video instead.",
    },
    PlatformTarget {
        id: "tiktok",
        label: "TikTok",
        width: 1080,
        height: 1920,
        accepts: VERTICAL,
        max_secs: Some(60.0 * 60.0),
        reach_max_secs: Some(10.0 * 60.0),
        min_secs: Some(3.0),
        notes: "Uploads accept up to 60 min. Under 3 min the file must stay below 500 MB, 3-10 min below 2 GB.",
    },
    PlatformTarget {
        id: "ig_feed",
        label: "Instagram feed",
        width: 1080,
        height: 1350,
        accepts: &[(4, 5), (1, 1)],
        max_secs: Some(20.0 * 60.0),
        reach_max_secs: Some(3.0 * 60.0),
        min_secs: Some(3.0),
        notes: "4:5 takes the most vertical space in the feed. Feed video is distributed as a Reel.",
    },
    PlatformTarget {
        id: "youtube",
        label: "YouTube",
        width: 1920,
        height: 1080,
        accepts: LANDSCAPE,
        max_secs: None,
        reach_max_secs: None,
        min_secs: None,
        notes: "No practical length limit; 16:9 fills the player without bars.",
    },
];

/// The target with this id.
pub fn target(id: &str) -> Option<&'static PlatformTarget> {
    TARGETS.iter().find(|t| t.id == id)
}

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The platform will not accept this.
    Error,
    /// It will be accepted and then under-distributed, or shown letterboxed.
    Warning,
    /// Advice — nothing is wrong.
    Tip,
}

/// What an issue is *about*, so a UI can group it. Four targets that all want a
/// vertical frame produce four near-identical shape complaints; grouped, that is
/// one line naming four platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueKind {
    /// Nothing to publish.
    Empty,
    /// Too long or too short — includes the reach limit.
    Length,
    /// The wrong aspect for this target.
    Shape,
    /// The right aspect, too few pixels.
    Resolution,
    /// Advice about reading muted.
    Captions,
}

/// One thing to know before publishing this cut to a target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryIssue {
    pub severity: Severity,
    pub kind: IssueKind,
    /// Already phrased for a person, with the actual numbers in it.
    pub message: String,
}

/// A cut's readiness for one target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeliveryCheck {
    pub target: String,
    pub label: String,
    /// True when nothing would be rejected — warnings and tips can still stand.
    pub ok: bool,
    pub issues: Vec<DeliveryIssue>,
}

/// What a readiness check needs to know about a cut. Resolved by the caller
/// (`Project::platform_check`) so the check itself stays pure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CutSummary {
    pub duration: f64,
    /// The frame the cut renders at — the project's delivery format, or the
    /// shape the footage gives it when none is set.
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    /// Whether any text overlay is on screen — a proxy for "this reads muted".
    pub has_text: bool,
}

impl CutSummary {
    /// The cut's frame as a [`Delivery`]-shaped pair, for comparing aspects.
    fn aspect(&self) -> f64 {
        if self.height == 0 {
            return 0.0;
        }
        self.width as f64 / self.height as f64
    }
}

/// `m:ss`, how a length is spoken about.
fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0).round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn ratio_label(w: u32, h: u32) -> String {
    let gcd = |mut a: u32, mut b: u32| {
        while b != 0 {
            let t = b;
            b = a % b;
            a = t;
        }
        a.max(1)
    };
    let d = gcd(w, h);
    format!("{}:{}", w / d, h / d)
}

/// Check one cut against one target.
///
/// Aspect is compared as a ratio within 1%, not as exact pixels: a 1080x1920
/// cut and a 720x1280 one are the same shape and the platform treats them the
/// same way.
pub fn check(target: &PlatformTarget, cut: &CutSummary) -> DeliveryCheck {
    let mut issues = Vec::new();

    if cut.duration <= 0.0 {
        issues.push(DeliveryIssue {
            severity: Severity::Error,
            kind: IssueKind::Empty,
            message: "The timeline is empty — there is nothing to publish.".to_string(),
        });
    }
    if let Some(min) = target.min_secs {
        if cut.duration > 0.0 && cut.duration < min {
            issues.push(DeliveryIssue {
                severity: Severity::Error,
                kind: IssueKind::Length,
                message: format!(
                    "{} is shorter than {}'s {:.0}s minimum.",
                    fmt_dur(cut.duration),
                    target.label,
                    min
                ),
            });
        }
    }
    if let Some(max) = target.max_secs {
        if cut.duration > max {
            issues.push(DeliveryIssue {
                severity: Severity::Error,
                kind: IssueKind::Length,
                message: format!(
                    "{} is over {}'s {} limit — trim {} to fit.",
                    fmt_dur(cut.duration),
                    target.label,
                    fmt_dur(max),
                    fmt_dur(cut.duration - max)
                ),
            });
        }
    }
    // The quiet one: accepted, then not shown to anyone new.
    if let Some(reach) = target.reach_max_secs {
        let within_hard = target.max_secs.is_none_or(|m| cut.duration <= m);
        if cut.duration > reach && within_hard {
            issues.push(DeliveryIssue {
                severity: Severity::Warning,
                kind: IssueKind::Length,
                message: format!(
                    "Over {}, {} stops showing this to people who don't already follow you. Cutting {} would keep it in the feed.",
                    fmt_dur(reach),
                    target.label,
                    fmt_dur(cut.duration - reach)
                ),
            });
        }
    }

    // Shape.
    let want = target.width as f64 / target.height.max(1) as f64;
    let have = cut.aspect();
    let matches = |(w, h): &(u32, u32)| {
        let r = *w as f64 / *h as f64;
        (have - r).abs() <= r * 0.01
    };
    if have > 0.0 && !target.accepts.iter().any(matches) {
        let accepted = target
            .accepts
            .iter()
            .map(|(w, h)| ratio_label(*w, *h))
            .collect::<Vec<_>>()
            .join(" or ");
        issues.push(DeliveryIssue {
            severity: Severity::Warning,
            kind: IssueKind::Shape,
            message: format!(
                "This cut is {} ({}×{}); {} shows {}, so it will be letterboxed. Set the delivery frame to {}×{}.",
                ratio_label(cut.width, cut.height),
                cut.width,
                cut.height,
                target.label,
                accepted,
                target.width,
                target.height
            ),
        });
    } else if have > 0.0 && cut.height < target.height && (have - want).abs() <= want * 0.01 {
        // Right shape, not enough pixels — the platform will upscale it.
        issues.push(DeliveryIssue {
            severity: Severity::Warning,
            kind: IssueKind::Resolution,
            message: format!(
                "{}×{} is below {}'s {}×{}; the platform will upscale it and it will look soft.",
                cut.width, cut.height, target.label, target.width, target.height
            ),
        });
    }

    if !cut.has_text && cut.has_audio {
        issues.push(DeliveryIssue {
            severity: Severity::Tip,
            kind: IssueKind::Captions,
            message: "The feed autoplays muted. Captions or a title would carry this for the people who never turn sound on."
                .to_string(),
        });
    }

    DeliveryCheck {
        target: target.id.to_string(),
        label: target.label.to_string(),
        ok: !issues.iter().any(|i| i.severity == Severity::Error),
        issues,
    }
}

/// Check a cut against every known target.
pub fn check_all(cut: &CutSummary) -> Vec<DeliveryCheck> {
    TARGETS.iter().map(|t| check(t, cut)).collect()
}

/// The target a delivery frame is evidently aimed at, if any — used to lead the
/// UI with the one the user already chose to cut for.
pub fn target_for(format: Option<&Delivery>) -> Option<&'static PlatformTarget> {
    let f = format?;
    TARGETS.iter().find(|t| t.width == f.width && t.height == f.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertical_cut(duration: f64) -> CutSummary {
        CutSummary {
            duration,
            width: 1080,
            height: 1920,
            has_audio: true,
            has_text: true,
        }
    }

    #[test]
    fn a_well_shaped_cut_passes_clean() {
        let c = check(target("reels").unwrap(), &vertical_cut(45.0));
        assert!(c.ok);
        assert!(c.issues.is_empty(), "{:?}", c.issues);
    }

    #[test]
    fn over_the_hard_limit_is_an_error_and_says_how_much_to_cut() {
        let c = check(target("shorts").unwrap(), &vertical_cut(200.0));
        assert!(!c.ok);
        let m = &c.issues[0].message;
        assert_eq!(c.issues[0].severity, Severity::Error);
        assert!(m.contains("3:00"), "names the limit: {m}");
        assert!(m.contains("0:20"), "names the overshoot: {m}");
    }

    #[test]
    fn over_the_reach_limit_is_a_warning_not_a_rejection() {
        // The whole point: this uploads fine, and then nobody new sees it.
        let c = check(target("reels").unwrap(), &vertical_cut(4.0 * 60.0));
        assert!(c.ok, "a long Reel is still accepted");
        let issue = c.issues.iter().find(|i| i.severity == Severity::Warning).expect("warned");
        assert!(issue.message.contains("follow you"), "{}", issue.message);
        assert!(issue.message.contains("1:00"), "says how much to cut: {}", issue.message);
    }

    #[test]
    fn a_reach_warning_is_not_repeated_once_it_is_already_rejected() {
        // TikTok's reach limit is 10 min and its hard limit 60; past 60 the only
        // useful thing to say is that it will not upload.
        let c = check(target("tiktok").unwrap(), &vertical_cut(70.0 * 60.0));
        assert_eq!(c.issues.iter().filter(|i| i.severity == Severity::Warning).count(), 0);
        assert_eq!(c.issues.iter().filter(|i| i.severity == Severity::Error).count(), 1);
    }

    #[test]
    fn a_landscape_cut_is_flagged_for_a_vertical_feed() {
        let cut = CutSummary {
            width: 1920,
            height: 1080,
            ..vertical_cut(30.0)
        };
        let c = check(target("reels").unwrap(), &cut);
        assert!(c.ok, "wrong shape is not a rejection");
        let m = &c.issues[0].message;
        assert!(m.contains("16:9") && m.contains("9:16"), "{m}");
        assert!(m.contains("1080×1920"), "names the fix: {m}");
    }

    #[test]
    fn aspect_is_compared_as_a_ratio_not_as_pixels() {
        // 720x1280 is the same shape as 1080x1920; only the softness is worth a word.
        let cut = CutSummary {
            width: 720,
            height: 1280,
            ..vertical_cut(30.0)
        };
        let c = check(target("reels").unwrap(), &cut);
        assert_eq!(c.issues.len(), 1);
        assert!(c.issues[0].message.contains("upscale"), "{:?}", c.issues[0]);
    }

    #[test]
    fn square_passes_shorts_but_not_reels() {
        let cut = CutSummary {
            width: 1080,
            height: 1080,
            ..vertical_cut(30.0)
        };
        assert!(check(target("shorts").unwrap(), &cut)
            .issues
            .iter()
            .all(|i| i.severity != Severity::Warning));
        assert!(check(target("reels").unwrap(), &cut)
            .issues
            .iter()
            .any(|i| i.severity == Severity::Warning));
    }

    #[test]
    fn a_silent_feed_gets_a_captions_tip_only_when_there_is_sound_to_miss() {
        let mut cut = vertical_cut(30.0);
        cut.has_text = false;
        assert!(check(target("reels").unwrap(), &cut)
            .issues
            .iter()
            .any(|i| i.severity == Severity::Tip));
        // No audio at all: nothing is lost by muting, so the tip would be noise.
        cut.has_audio = false;
        assert!(check(target("reels").unwrap(), &cut)
            .issues
            .iter()
            .all(|i| i.severity != Severity::Tip));
    }

    #[test]
    fn an_empty_timeline_is_rejected_everywhere() {
        let cut = CutSummary {
            duration: 0.0,
            ..vertical_cut(0.0)
        };
        assert!(check_all(&cut).iter().all(|c| !c.ok));
    }

    #[test]
    fn a_delivery_frame_resolves_to_the_target_it_was_chosen_for() {
        let d = Delivery {
            width: 1080,
            height: 1350,
            fit: crate::model::Fit::Cover,
        };
        assert_eq!(target_for(Some(&d)).map(|t| t.id), Some("ig_feed"));
        assert_eq!(target_for(None), None);
    }
}
