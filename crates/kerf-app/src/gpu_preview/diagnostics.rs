//! Local counters of what the GPU preview did with the frames it was asked for, and why the rest
//! went through FFmpeg: per session, in memory, shown by `gpu_preview_diagnostics` and logged. They
//! tell which refusals matter on real projects (a title font that cannot be placed, two cameras with
//! different colour tags) so priorities do not come from the archetypes the plan was designed on.
//! Nothing here leaves the machine.
//!
//! A reason is counted by its **kind** — [`kerf_core::Unsupported::kind`] for what the plan refuses,
//! a fixed word for what the surface or the backend decides — with the first message seen as an
//! example; the layer names and numbers in a message are not part of the key.

use std::collections::BTreeMap;

use serde::Serialize;

/// Why one frame did not come from the GPU: a stable key, and the sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub kind: &'static str,
    pub text: String,
}

impl Reason {
    pub fn new(kind: &'static str, text: impl Into<String>) -> Self {
        Self { kind, text: text.into() }
    }
}

/// How many characters of an example message are kept.
const EXAMPLE_CHARS: usize = 240;

#[derive(Debug, Default)]
pub struct Counters {
    gpu: u64,
    ffmpeg: u64,
    refusals: BTreeMap<&'static str, (u64, String)>,
}

/// One kind of refusal, as `gpu_preview_diagnostics` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusalCount {
    pub kind: String,
    pub count: u64,
    /// The first message of this kind seen this session.
    pub example: String,
}

/// The session's tallies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    /// Frames the GPU drew.
    pub gpu_frames: u64,
    /// Frames FFmpeg drew (a refusal or a failure; each reason of a frame is counted in `refusals`).
    pub ffmpeg_frames: u64,
    /// Frame requests replaced by a newer one before they were drawn.
    pub superseded: u64,
    /// Refusals, most frequent first.
    pub refusals: Vec<RefusalCount>,
}

impl Counters {
    pub fn presented(&mut self) {
        self.gpu += 1;
    }

    /// A frame went through FFmpeg for `reasons`. Returns the kinds seen for the first time, so the
    /// caller can log a new one once.
    pub fn refused(&mut self, reasons: &[Reason]) -> Vec<&'static str> {
        self.ffmpeg += 1;
        let mut new = Vec::new();
        for reason in reasons {
            let entry = self.refusals.entry(reason.kind).or_insert_with(|| {
                new.push(reason.kind);
                (0, reason.text.chars().take(EXAMPLE_CHARS).collect())
            });
            entry.0 += 1;
        }
        new
    }

    /// The tallies so far; `superseded` is the mailbox's count of requests replaced before they
    /// were drawn.
    pub fn tally(&self, superseded: u64) -> Tally {
        let mut refusals: Vec<RefusalCount> = self
            .refusals
            .iter()
            .map(|(kind, (count, example))| RefusalCount {
                kind: (*kind).to_string(),
                count: *count,
                example: example.clone(),
            })
            .collect();
        refusals.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.kind.cmp(&b.kind)));
        Tally {
            gpu_frames: self.gpu,
            ffmpeg_frames: self.ffmpeg,
            superseded,
            refusals,
        }
    }
}

impl Tally {
    /// One line for the log: `12 GPU, 40 FFmpeg (text 30, mixed_matrices 12), 5 superseded`.
    pub fn line(&self) -> String {
        let kinds: Vec<String> = self.refusals.iter().map(|r| format!("{} {}", r.kind, r.count)).collect();
        format!(
            "{} GPU, {} FFmpeg ({}), {} superseded",
            self.gpu_frames,
            self.ffmpeg_frames,
            if kinds.is_empty() {
                "no refusals".to_string()
            } else {
                kinds.join(", ")
            },
            self.superseded
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_counted_by_kind_with_the_first_message_as_the_example() {
        let mut c = Counters::default();
        assert_eq!(
            c.refused(&[Reason::new("mask", "layer 1 (a.mp4): mask")]),
            vec!["mask"],
            "a new kind is reported once"
        );
        assert!(c.refused(&[Reason::new("mask", "layer 3 (b.mov): mask")]).is_empty());
        c.refused(&[Reason::new("text_too_small", "8 px"), Reason::new("mask", "layer 0: mask")]);
        c.presented();
        c.presented();
        let t = c.tally(7);
        assert_eq!((t.gpu_frames, t.ffmpeg_frames, t.superseded), (2, 3, 7));
        assert_eq!(
            t.refusals,
            vec![
                RefusalCount {
                    kind: "mask".into(),
                    count: 3,
                    example: "layer 1 (a.mp4): mask".into()
                },
                RefusalCount {
                    kind: "text_too_small".into(),
                    count: 1,
                    example: "8 px".into()
                },
            ]
        );
        assert_eq!(t.line(), "2 GPU, 3 FFmpeg (mask 3, text_too_small 1), 7 superseded");
    }

    #[test]
    fn an_example_is_cut_and_a_quiet_session_says_so() {
        let mut c = Counters::default();
        c.refused(&[Reason::new("geometry", "x".repeat(1000))]);
        assert_eq!(c.tally(0).refusals[0].example.chars().count(), EXAMPLE_CHARS);
        assert_eq!(
            Counters::default().tally(0).line(),
            "0 GPU, 0 FFmpeg (no refusals), 0 superseded"
        );
    }
}
