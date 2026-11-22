//! Where a finished cut is going, and whether it is ready to go there.
//!
//! Everything else in Kerf ends at the rendered file. A creator's job does not:
//! the file still has to be accepted by a platform and then actually shown to
//! people. Those are two different bars, and the second one fails silently —
//! which is what this module exists to say out loud before the render, while the
//! cut can still be changed.

use serde::{Deserialize, Serialize};

use crate::engine::SizeBounds;
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
    /// What the platform caps the file's size at, by the length of the cut; empty where no
    /// cap is recorded. Ascending by `up_to_secs`, the open-ended one last. See [`SizeLimit`].
    /// A cap is set only where its figure can be stated with a source and a date in the comment
    /// beside it; an unknown one stays empty under a `TODO(size)` saying what to check (a wrong
    /// cap warns about files that fit, or passes ones that do not).
    #[serde(skip_serializing_if = "<[SizeLimit]>::is_empty")]
    pub size_limits: &'static [SizeLimit],
    /// Where the limits come from / what else to know.
    pub notes: &'static str,
}

/// A cap on the size of the file a platform accepts, for cuts up to a length.
///
/// Only the size estimate can tell whether a cut meets it (`export_size`); a check without
/// an estimate never mentions one. `max_bytes` is read as **decimal** megabytes and gigabytes
/// (`500 MB` is `500_000_000`), the stricter of the two readings a platform's page leaves open.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SizeLimit {
    /// The longest cut, in seconds and inclusive, this cap covers; `None` covers any length.
    pub up_to_secs: Option<f64>,
    pub max_bytes: u64,
}

impl PlatformTarget {
    /// The size cap that applies to a cut of `duration` seconds: the first recorded limit that
    /// covers it. `None` where this platform's cap is not recorded for a cut that long.
    pub fn size_limit_for(&self, duration: f64) -> Option<&SizeLimit> {
        self.size_limits
            .iter()
            .find(|l| l.up_to_secs.is_none_or(|up_to| duration <= up_to))
    }
}

const MB: u64 = 1_000_000;
const GB: u64 = 1_000_000_000;

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
        // TODO(size): Instagram's upload file-size cap for Reels is not recorded. Check the
        // current figure (and whether it depends on length) before setting one.
        size_limits: &[],
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
        // TODO(size): YouTube's upload file-size cap for Shorts is not recorded. Check the
        // current figure before setting one.
        size_limits: &[],
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
        // 500 MB for a cut under 3 min, 2 GB for 3-10 min: the figures this target's `notes` carry
        // from the 2026-08-25 web check recorded in the module docs, moved here unchanged so
        // the estimate can be held to them. Not re-checked since (no network when this was
        // written). TODO(size): the 10-60 min tier (the platform takes uploads up to 60 min) is
        // not recorded, so a cut longer than 10 min gets no size rule; check TikTok's current
        // upload limits for it, and whether they still read as above.
        size_limits: &[
            SizeLimit {
                up_to_secs: Some(3.0 * 60.0),
                max_bytes: 500 * MB,
            },
            SizeLimit {
                up_to_secs: Some(10.0 * 60.0),
                max_bytes: 2 * GB,
            },
        ],
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
        // TODO(size): Instagram's upload file-size cap for feed video is not recorded. Check
        // the current figure before setting one.
        size_limits: &[],
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
        // TODO(size): YouTube's upload file-size cap is not recorded. Check the current figure
        // before setting one.
        size_limits: &[],
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
    /// The file may be over the platform's size cap. Only said of a check that was given a size
    /// estimate ([`check_sized`]).
    Size,
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
/// (`Project::cut_summary_in`) so the check itself stays pure.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

/// `Instagram Reels'`, `TikTok's`: the possessive of a platform's name.
fn possessive(label: &str) -> String {
    if label.ends_with('s') {
        format!("{label}'")
    } else {
        format!("{label}'s")
    }
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

/// A size as a person says it: decimal units, three figures at most (`500 MB`, `1.4 GB`).
fn fmt_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    let (value, unit) = if b >= 1e9 {
        (b / 1e9, "GB")
    } else if b >= 1e6 {
        (b / 1e6, "MB")
    } else if b >= 1e3 {
        (b / 1e3, "kB")
    } else {
        return format!("{bytes} B");
    };
    let text = if value >= 10.0 {
        format!("{value:.0}")
    } else {
        let one = format!("{value:.1}");
        one.strip_suffix(".0").map_or(one.clone(), str::to_string)
    };
    format!("{text} {unit}")
}

/// What the estimate says about the size, in a sentence: a range, or a single figure when the
/// estimate is as tight as an exact one.
fn fmt_range(size: SizeBounds) -> String {
    if size.high_bytes <= size.low_bytes.saturating_add(size.low_bytes / 20) {
        format!("about {}", fmt_bytes(size.high_bytes))
    } else {
        format!("{} to {}", fmt_bytes(size.low_bytes), fmt_bytes(size.high_bytes))
    }
}

/// The finding about a file estimated at `size` against `limit`, or none when all of it fits.
///
/// An **error** ("it will not fit") stands only on `size.floor_bytes`: the model's own floor, or what a whole encode wrote.
/// A range that sampled seconds produced (`SizeBasis::Measured`) is a statistic, so even its low end over the cap is a
/// **warning**; its high end over the cap is the same warning: neither end is a promise.
fn size_issue(target: &PlatformTarget, limit: &SizeLimit, size: SizeBounds) -> Option<DeliveryIssue> {
    let cap = fmt_bytes(limit.max_bytes);
    let covers = limit
        .up_to_secs
        .map_or(String::new(), |s| format!(" for a cut up to {}", fmt_dur(s)));
    let of = possessive(target.label);
    let fix = "Lower the quality or the resolution, or trim the cut.";
    if size.floor_bytes > limit.max_bytes {
        Some(DeliveryIssue {
            severity: Severity::Error,
            kind: IssueKind::Size,
            message: format!(
                "This export is estimated at {}, and at least {} of it is certain: over {of} {cap} limit{covers}. {fix}",
                fmt_range(size),
                fmt_bytes(size.floor_bytes)
            ),
        })
    } else if size.low_bytes > limit.max_bytes {
        Some(DeliveryIssue {
            severity: Severity::Warning,
            kind: IssueKind::Size,
            message: format!(
                "This export is estimated at {}, over {of} {cap} limit{covers}; the estimate is not firm enough to say it will be refused. {fix}",
                fmt_range(size)
            ),
        })
    } else if size.high_bytes > limit.max_bytes {
        Some(DeliveryIssue {
            severity: Severity::Warning,
            kind: IssueKind::Size,
            message: format!(
                "This export is estimated at {}, and {of} limit is {cap}{covers}. It may not fit; lowering the quality or the resolution makes it safer.",
                fmt_range(size)
            ),
        })
    } else {
        None
    }
}

/// Check one cut against one target.
///
/// Aspect is compared as a ratio within 1%, not as exact pixels: a 1080x1920
/// cut and a 720x1280 one are the same shape and the platform treats them the
/// same way.
pub fn check(target: &PlatformTarget, cut: &CutSummary) -> DeliveryCheck {
    check_sized(target, cut, None)
}

/// [`check`], and with the size an export of the cut is estimated at (`SizeEstimate::bounds`) the
/// file's size held to the target's cap for a cut that long.
///
/// **What may be an error.** Only `floor_bytes` over the cap is: it is the model's own floor, or what a
/// *whole* encode wrote, the part of an estimate that is not a statistic. The low end of a **measured**
/// estimate from samples is a statistic (its note says it holds if the sampled seconds are typical), so
/// even a measured band wholly over the cap is a warning, "not firm enough to say it will be refused", and
/// its high end is never presented as a guarantee either. A warning is also what the high end over the cap
/// is. Without an estimate, with one that could not be made, or for a target with no cap recorded, the answer is
/// exactly [`check`]'s.
pub fn check_sized(target: &PlatformTarget, cut: &CutSummary, size: Option<SizeBounds>) -> DeliveryCheck {
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
                    "{} is shorter than {} {:.0}s minimum.",
                    fmt_dur(cut.duration),
                    possessive(target.label),
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
                    "{} is over {} {} limit — trim {} to fit.",
                    fmt_dur(cut.duration),
                    possessive(target.label),
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

    // The file's size, when there is an estimate to hold to a cap.
    if let (Some(size), Some(limit)) = (size, target.size_limit_for(cut.duration)) {
        if cut.duration > 0.0 {
            issues.extend(size_issue(target, limit, size));
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
                "{}×{} is below {} {}×{}; the platform will upscale it and it will look soft.",
                cut.width,
                cut.height,
                possessive(target.label),
                target.width,
                target.height
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
    check_all_sized(cut, None)
}

/// [`check_all`] with the size the export is estimated at ([`check_sized`]).
pub fn check_all_sized(cut: &CutSummary, size: Option<SizeBounds>) -> Vec<DeliveryCheck> {
    TARGETS.iter().map(|t| check_sized(t, cut, size)).collect()
}

/// A cut's readiness for every target, with the cut it was judged on: the frame and length a UI
/// states in words have to be the ones the checks used, not a second reading of the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformReport {
    pub cut: CutSummary,
    pub targets: Vec<DeliveryCheck>,
}

/// Check a cut against every known target and keep the cut beside the answer.
pub fn report(cut: &CutSummary) -> PlatformReport {
    report_sized(cut, None)
}

/// [`report`] with the size the export is estimated at ([`check_sized`]).
pub fn report_sized(cut: &CutSummary, size: Option<SizeBounds>) -> PlatformReport {
    PlatformReport {
        cut: *cut,
        targets: check_all_sized(cut, size),
    }
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
    fn a_name_ending_in_s_takes_a_bare_apostrophe() {
        assert_eq!(possessive("YouTube Shorts"), "YouTube Shorts'");
        assert_eq!(possessive("Instagram Reels"), "Instagram Reels'");
        assert_eq!(possessive("TikTok"), "TikTok's");
        let c = check(target("shorts").unwrap(), &vertical_cut(200.0));
        assert!(
            c.issues[0].message.contains("YouTube Shorts' 3:00 limit"),
            "{}",
            c.issues[0].message
        );
        let c = check(target("reels").unwrap(), &vertical_cut(2.0));
        assert!(
            c.issues[0].message.contains("Instagram Reels' 3s minimum"),
            "{}",
            c.issues[0].message
        );
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
    fn the_report_carries_the_cut_it_judged() {
        let r = report(&vertical_cut(45.0));
        assert_eq!((r.cut.width, r.cut.height, r.cut.duration), (1080, 1920, 45.0));
        assert_eq!(r.targets.len(), TARGETS.len());
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["cut"]["width"], 1080);
        assert_eq!(json["targets"][0]["target"], "reels");
    }

    #[test]
    fn a_delivery_frame_resolves_to_the_target_it_was_chosen_for() {
        let d = Delivery {
            width: 1080,
            height: 1350,
            fit: crate::model::Fit::Cover,
            fps: None,
        };
        assert_eq!(target_for(Some(&d)).map(|t| t.id), Some("ig_feed"));
        assert_eq!(target_for(None), None);
    }

    // ---- the size of the file ----------------------------------------------------------------

    /// A platform that exists only here, so the mechanism is tested on numbers nobody has to
    /// believe: 100 MB for a cut up to a minute, 500 MB past it.
    const CAPPED: PlatformTarget = PlatformTarget {
        id: "test_capped",
        label: "Test Platform",
        width: 1080,
        height: 1920,
        accepts: VERTICAL,
        max_secs: None,
        reach_max_secs: None,
        min_secs: None,
        size_limits: &[
            SizeLimit {
                up_to_secs: Some(60.0),
                max_bytes: 100 * MB,
            },
            SizeLimit {
                up_to_secs: None,
                max_bytes: 500 * MB,
            },
        ],
        notes: "test only",
    };

    /// An estimate of the model's: its low end is a floor.
    fn bounds(low_mb: u64, high_mb: u64) -> Option<SizeBounds> {
        Some(SizeBounds {
            low_bytes: low_mb * MB,
            high_bytes: high_mb * MB,
            floor_bytes: low_mb * MB,
        })
    }

    /// A measured estimate from samples: a low end and a high end, and a floor that is not the band's.
    fn sampled(low_mb: u64, high_mb: u64, floor_mb: u64) -> Option<SizeBounds> {
        Some(SizeBounds {
            low_bytes: low_mb * MB,
            high_bytes: high_mb * MB,
            floor_bytes: floor_mb * MB,
        })
    }

    fn size_issues(c: &DeliveryCheck) -> Vec<&DeliveryIssue> {
        c.issues.iter().filter(|i| i.kind == IssueKind::Size).collect()
    }

    /// What `check` said, target by target, of these cuts before sizes existed: recorded by running `platform.rs` as it
    /// stood on main (`name|target|ok|severity|kind|message`, or `name|target|ok|none`), not derived from this file.
    const RECORDED: &str = r#"vertical 45s|reels|true|none
vertical 45s|shorts|true|none
vertical 45s|tiktok|true|none
vertical 45s|ig_feed|true|Warning|Shape|This cut is 9:16 (1080×1920); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
vertical 45s|youtube|true|Warning|Shape|This cut is 9:16 (1080×1920); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080.
vertical 4min|reels|true|Warning|Length|Over 3:00, Instagram Reels stops showing this to people who don't already follow you. Cutting 1:00 would keep it in the feed.
vertical 4min|shorts|false|Error|Length|4:00 is over YouTube Shorts' 3:00 limit — trim 1:00 to fit.
vertical 4min|tiktok|true|none
vertical 4min|ig_feed|true|Warning|Length|Over 3:00, Instagram feed stops showing this to people who don't already follow you. Cutting 1:00 would keep it in the feed.
vertical 4min|ig_feed|true|Warning|Shape|This cut is 9:16 (1080×1920); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
vertical 4min|youtube|true|Warning|Shape|This cut is 9:16 (1080×1920); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080.
vertical 200s|reels|true|Warning|Length|Over 3:00, Instagram Reels stops showing this to people who don't already follow you. Cutting 0:20 would keep it in the feed.
vertical 200s|shorts|false|Error|Length|3:20 is over YouTube Shorts' 3:00 limit — trim 0:20 to fit.
vertical 200s|tiktok|true|none
vertical 200s|ig_feed|true|Warning|Length|Over 3:00, Instagram feed stops showing this to people who don't already follow you. Cutting 0:20 would keep it in the feed.
vertical 200s|ig_feed|true|Warning|Shape|This cut is 9:16 (1080×1920); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
vertical 200s|youtube|true|Warning|Shape|This cut is 9:16 (1080×1920); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080.
vertical 2s|reels|false|Error|Length|0:02 is shorter than Instagram Reels' 3s minimum.
vertical 2s|shorts|true|none
vertical 2s|tiktok|false|Error|Length|0:02 is shorter than TikTok's 3s minimum.
vertical 2s|ig_feed|false|Error|Length|0:02 is shorter than Instagram feed's 3s minimum.
vertical 2s|ig_feed|false|Warning|Shape|This cut is 9:16 (1080×1920); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
vertical 2s|youtube|true|Warning|Shape|This cut is 9:16 (1080×1920); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080.
vertical 70min|reels|false|Error|Length|70:00 is over Instagram Reels' 20:00 limit — trim 50:00 to fit.
vertical 70min|shorts|false|Error|Length|70:00 is over YouTube Shorts' 3:00 limit — trim 67:00 to fit.
vertical 70min|tiktok|false|Error|Length|70:00 is over TikTok's 60:00 limit — trim 10:00 to fit.
vertical 70min|ig_feed|false|Error|Length|70:00 is over Instagram feed's 20:00 limit — trim 50:00 to fit.
vertical 70min|ig_feed|false|Warning|Shape|This cut is 9:16 (1080×1920); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
vertical 70min|youtube|true|Warning|Shape|This cut is 9:16 (1080×1920); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080.
landscape 30s|reels|true|Warning|Shape|This cut is 16:9 (1920×1080); Instagram Reels shows 9:16, so it will be letterboxed. Set the delivery frame to 1080×1920.
landscape 30s|reels|true|Tip|Captions|The feed autoplays muted. Captions or a title would carry this for the people who never turn sound on.
landscape 30s|shorts|true|Warning|Shape|This cut is 16:9 (1920×1080); YouTube Shorts shows 9:16 or 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1920.
landscape 30s|shorts|true|Tip|Captions|The feed autoplays muted. Captions or a title would carry this for the people who never turn sound on.
landscape 30s|tiktok|true|Warning|Shape|This cut is 16:9 (1920×1080); TikTok shows 9:16, so it will be letterboxed. Set the delivery frame to 1080×1920.
landscape 30s|tiktok|true|Tip|Captions|The feed autoplays muted. Captions or a title would carry this for the people who never turn sound on.
landscape 30s|ig_feed|true|Warning|Shape|This cut is 16:9 (1920×1080); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
landscape 30s|ig_feed|true|Tip|Captions|The feed autoplays muted. Captions or a title would carry this for the people who never turn sound on.
landscape 30s|youtube|true|Tip|Captions|The feed autoplays muted. Captions or a title would carry this for the people who never turn sound on.
720p vertical 30s|reels|true|Warning|Resolution|720×1280 is below Instagram Reels' 1080×1920; the platform will upscale it and it will look soft.
720p vertical 30s|shorts|true|Warning|Resolution|720×1280 is below YouTube Shorts' 1080×1920; the platform will upscale it and it will look soft.
720p vertical 30s|tiktok|true|Warning|Resolution|720×1280 is below TikTok's 1080×1920; the platform will upscale it and it will look soft.
720p vertical 30s|ig_feed|true|Warning|Shape|This cut is 9:16 (720×1280); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
720p vertical 30s|youtube|true|Warning|Shape|This cut is 9:16 (720×1280); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080.
square 30s silent|reels|true|Warning|Shape|This cut is 1:1 (1080×1080); Instagram Reels shows 9:16, so it will be letterboxed. Set the delivery frame to 1080×1920.
square 30s silent|shorts|true|none
square 30s silent|tiktok|true|Warning|Shape|This cut is 1:1 (1080×1080); TikTok shows 9:16, so it will be letterboxed. Set the delivery frame to 1080×1920.
square 30s silent|ig_feed|true|none
square 30s silent|youtube|true|Warning|Shape|This cut is 1:1 (1080×1080); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080.
empty|reels|false|Error|Empty|The timeline is empty — there is nothing to publish.
empty|shorts|false|Error|Empty|The timeline is empty — there is nothing to publish.
empty|tiktok|false|Error|Empty|The timeline is empty — there is nothing to publish.
empty|ig_feed|false|Error|Empty|The timeline is empty — there is nothing to publish.
empty|ig_feed|false|Warning|Shape|This cut is 9:16 (1080×1920); Instagram feed shows 4:5 or 1:1, so it will be letterboxed. Set the delivery frame to 1080×1350.
empty|youtube|false|Error|Empty|The timeline is empty — there is nothing to publish.
empty|youtube|false|Warning|Shape|This cut is 9:16 (1080×1920); YouTube shows 16:9, so it will be letterboxed. Set the delivery frame to 1920×1080."#;

    /// The cuts of [`RECORDED`], by name.
    fn recorded_cut(name: &str) -> CutSummary {
        let cut = |duration, width, height, has_audio, has_text| CutSummary {
            duration,
            width,
            height,
            has_audio,
            has_text,
        };
        match name {
            "vertical 45s" => cut(45.0, 1080, 1920, true, true),
            "vertical 4min" => cut(240.0, 1080, 1920, true, true),
            "vertical 200s" => cut(200.0, 1080, 1920, true, true),
            "vertical 2s" => cut(2.0, 1080, 1920, true, true),
            "vertical 70min" => cut(4200.0, 1080, 1920, true, true),
            "landscape 30s" => cut(30.0, 1920, 1080, true, false),
            "720p vertical 30s" => cut(30.0, 720, 1280, true, true),
            "square 30s silent" => cut(30.0, 1080, 1080, false, false),
            "empty" => cut(0.0, 1080, 1920, true, true),
            other => panic!("a cut the record does not name: {other}"),
        }
    }

    /// The findings of `check` for every recorded cut and target, in the recorded form.
    fn findings(check: &dyn Fn(&PlatformTarget, &CutSummary) -> DeliveryCheck) -> String {
        let names = [
            "vertical 45s",
            "vertical 4min",
            "vertical 200s",
            "vertical 2s",
            "vertical 70min",
            "landscape 30s",
            "720p vertical 30s",
            "square 30s silent",
            "empty",
        ];
        let mut lines = Vec::new();
        for name in names {
            for t in TARGETS {
                let c = check(t, &recorded_cut(name));
                for i in &c.issues {
                    lines.push(format!(
                        "{name}|{}|{}|{:?}|{:?}|{}",
                        t.id, c.ok, i.severity, i.kind, i.message
                    ));
                }
                if c.issues.is_empty() {
                    lines.push(format!("{name}|{}|{}|none", t.id, c.ok));
                }
            }
        }
        lines.join("\n")
    }

    #[test]
    fn without_an_estimate_the_findings_and_their_words_are_the_recorded_ones() {
        assert_eq!(findings(&|t, c| check(t, c)), RECORDED.trim());
        assert_eq!(findings(&|t, c| check_sized(t, c, None)), RECORDED.trim());
        // With any estimate, a cut no cap is recorded for says the same words.
        let huge = bounds(50_000, 90_000);
        let with_estimate = |t: &PlatformTarget, c: &CutSummary| {
            let got = check_sized(t, c, huge);
            if t.size_limit_for(c.duration).is_none() {
                assert_eq!(got, check(t, c), "{} {}", t.id, c.duration);
            }
            check(t, c)
        };
        assert_eq!(findings(&with_estimate), RECORDED.trim());
        let cut = vertical_cut(45.0);
        assert_eq!(report_sized(&cut, None), report(&cut));
        assert_eq!(check_all_sized(&cut, None), check_all(&cut));
    }

    #[test]
    fn a_file_that_fits_is_not_mentioned() {
        let c = check_sized(&CAPPED, &vertical_cut(45.0), bounds(10, 100));
        assert!(size_issues(&c).is_empty(), "{:?}", c.issues);
        assert!(c.ok);
    }

    #[test]
    fn a_top_over_the_cap_is_a_warning_and_the_file_may_still_be_sent() {
        let c = check_sized(&CAPPED, &vertical_cut(45.0), bounds(20, 160));
        let issues = size_issues(&c);
        assert_eq!(issues.len(), 1, "{:?}", c.issues);
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(c.ok, "may not fit is not will not fit");
        let m = &issues[0].message;
        assert!(m.contains("20 MB to 160 MB"), "{m}");
        assert!(m.contains("Test Platform's limit is 100 MB for a cut up to 1:00"), "{m}");
    }

    #[test]
    fn a_bottom_over_the_cap_is_an_error() {
        let c = check_sized(&CAPPED, &vertical_cut(45.0), bounds(120, 900));
        let issues = size_issues(&c);
        assert_eq!(issues.len(), 1, "{:?}", c.issues);
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(!c.ok);
        let m = &issues[0].message;
        assert!(
            m.contains(
                "120 MB to 900 MB, and at least 120 MB of it is certain: over Test Platform's 100 MB limit for a cut up to 1:00"
            ),
            "{m}"
        );
        assert!(m.contains("trim"), "says what to do: {m}");
    }

    #[test]
    fn a_range_that_sampled_seconds_made_is_never_an_error_unless_the_whole_cut_was_encoded() {
        // 120..900 MB over a 100 MB cap, but the floor under it is 80 MB: a warning, not "it will not fit".
        let c = check_sized(&CAPPED, &vertical_cut(45.0), sampled(120, 900, 80));
        let issues = size_issues(&c);
        assert_eq!(issues.len(), 1, "{:?}", c.issues);
        assert_eq!(issues[0].severity, Severity::Warning, "{:?}", issues[0]);
        assert!(c.ok, "a sampled estimate never makes a cut unpublishable");
        assert!(
            issues[0].message.contains("not firm enough to say it will be refused"),
            "{}",
            issues[0].message
        );
        // A whole encode (or the model's own floor beside it) is what an error may stand on.
        let whole = check_sized(&CAPPED, &vertical_cut(45.0), sampled(120, 130, 120));
        assert_eq!(size_issues(&whole)[0].severity, Severity::Error);
        assert!(!whole.ok);
        // The high end over the cap is only ever a warning, floor or none.
        let high = check_sized(&CAPPED, &vertical_cut(45.0), sampled(40, 900, 40));
        assert_eq!(size_issues(&high)[0].severity, Severity::Warning);
        assert!(high.ok);
        // A model floor that is over the cap stands even when the band that sampled seconds made is not.
        let floor_over = check_sized(&CAPPED, &vertical_cut(45.0), sampled(60, 400, 110));
        assert_eq!(size_issues(&floor_over)[0].severity, Severity::Error);
    }

    #[test]
    fn the_cap_is_the_one_for_a_cut_that_long() {
        // 100 MB up to a minute, inclusive; 500 MB past it.
        let size = bounds(150, 300);
        let at = |secs| size_issues(&check_sized(&CAPPED, &vertical_cut(secs), size)).len();
        let severity = |secs| {
            check_sized(&CAPPED, &vertical_cut(secs), size)
                .issues
                .iter()
                .find(|i| i.kind == IssueKind::Size)
                .map(|i| i.severity)
        };
        assert_eq!(severity(60.0), Some(Severity::Error), "60 s is still the 100 MB tier");
        assert_eq!(at(60.5), 0, "past it the 500 MB cap holds 300 MB");
        assert_eq!(CAPPED.size_limit_for(60.0).map(|l| l.max_bytes), Some(100 * MB));
        assert_eq!(CAPPED.size_limit_for(60.1).map(|l| l.max_bytes), Some(500 * MB));
        assert_eq!(CAPPED.size_limit_for(1.0e6).map(|l| l.max_bytes), Some(500 * MB));
    }

    #[test]
    fn a_size_that_is_as_tight_as_an_exact_one_is_one_figure() {
        let c = check_sized(
            &CAPPED,
            &vertical_cut(45.0),
            Some(SizeBounds {
                low_bytes: 300 * MB,
                high_bytes: 304 * MB,
                floor_bytes: 300 * MB,
            }),
        );
        assert!(size_issues(&c)[0].message.contains("about 304 MB"), "{:?}", c.issues);
    }

    #[test]
    fn an_empty_cut_is_the_empty_findings_alone() {
        let c = check_sized(&CAPPED, &vertical_cut(0.0), bounds(1_000, 2_000));
        assert!(size_issues(&c).is_empty());
        assert!(!c.ok);
    }

    #[test]
    fn a_size_is_said_in_decimal_units() {
        for (bytes, text) in [
            (0, "0 B"),
            (950, "950 B"),
            (1_500, "1.5 kB"),
            (980_000, "980 kB"),
            (1_200_000, "1.2 MB"),
            (500_000_000, "500 MB"),
            (1_400_000_000, "1.4 GB"),
            (2_000_000_000, "2 GB"),
            (36_000_000_000, "36 GB"),
        ] {
            assert_eq!(fmt_bytes(bytes), text, "{bytes}");
        }
    }

    #[test]
    fn tiktoks_recorded_caps_are_held_to_the_estimate() {
        let tiktok = target("tiktok").unwrap();
        // 500 MB under 3 min, 2 GB for 3-10 min (the figures its notes carry), nothing recorded past.
        assert_eq!(tiktok.size_limit_for(90.0).map(|l| l.max_bytes), Some(500_000_000));
        assert_eq!(tiktok.size_limit_for(180.0).map(|l| l.max_bytes), Some(500_000_000));
        assert_eq!(tiktok.size_limit_for(181.0).map(|l| l.max_bytes), Some(2_000_000_000));
        assert_eq!(tiktok.size_limit_for(600.0).map(|l| l.max_bytes), Some(2_000_000_000));
        assert!(tiktok.size_limit_for(601.0).is_none());
        let warned = check_sized(tiktok, &vertical_cut(90.0), bounds(40, 900));
        assert_eq!(size_issues(&warned)[0].severity, Severity::Warning);
        assert!(size_issues(&warned)[0]
            .message
            .contains("TikTok's limit is 500 MB for a cut up to 3:00"));
        let refused = check_sized(tiktok, &vertical_cut(300.0), bounds(2_500, 6_000));
        assert_eq!(size_issues(&refused)[0].severity, Severity::Error);
        assert!(size_issues(&refused)[0].message.contains("TikTok's 2 GB limit"));
        // Past the last recorded tier there is no rule to hold it to.
        assert!(size_issues(&check_sized(tiktok, &vertical_cut(900.0), bounds(9_000, 90_000))).is_empty());
        // Only TikTok has one recorded so far: the rest say so in the table, not in a guess.
        let capped: Vec<&str> = TARGETS.iter().filter(|t| !t.size_limits.is_empty()).map(|t| t.id).collect();
        assert_eq!(capped, ["tiktok"]);
    }

    #[test]
    fn the_size_kind_and_the_caps_serialize_for_the_frontend() {
        assert_eq!(serde_json::to_value(IssueKind::Size).unwrap(), "size");
        let tiktok = serde_json::to_value(target("tiktok").unwrap()).unwrap();
        assert_eq!(tiktok["size_limits"][0]["up_to_secs"], 180.0);
        assert_eq!(tiktok["size_limits"][0]["max_bytes"], 500_000_000u64);
        // A target without a cap serializes exactly as it did before there were any.
        let reels = serde_json::to_value(target("reels").unwrap()).unwrap();
        assert!(reels.get("size_limits").is_none(), "{reels}");
        let r = report_sized(&vertical_cut(90.0), bounds(40, 900));
        let json = serde_json::to_value(&r).unwrap();
        let tiktok_issues = json["targets"][2]["issues"].as_array().unwrap();
        assert!(
            tiktok_issues
                .iter()
                .any(|i| i["kind"] == "size" && i["severity"] == "warning"),
            "{tiktok_issues:?}"
        );
    }
}
