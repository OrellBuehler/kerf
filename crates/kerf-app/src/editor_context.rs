//! What the user is looking at in the editor — the playhead, the selection, the
//! in / out marks, the stretch of timeline on screen, the panel in use — so an
//! agent asked about "this clip" or "here" knows which one. The webview reports it
//! (`report_ui_context`, debounced, re-sent as a heartbeat) and the MCP tool
//! `get_editor_context` reads it back with its age. It is a view of the window,
//! not part of the cut, so it lives in app state and never in the `Project`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The webview re-sends its context every 30 s while it is open (`editor-context.ts`),
/// so a report older than this means the window stopped reporting.
const STALE_AFTER: Duration = Duration::from_secs(90);

/// A span of timeline seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

/// What the webview reports. Timeline seconds throughout; ids are the live
/// timeline's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UiContext {
    pub playhead: f64,
    #[serde(default)]
    pub selected_clip_ids: Vec<String>,
    #[serde(default)]
    pub mark_in: Option<f64>,
    #[serde(default)]
    pub mark_out: Option<f64>,
    #[serde(default)]
    pub visible_range: Option<Span>,
    #[serde(default)]
    pub focused_panel: Option<String>,
    #[serde(default)]
    pub selected_asset_id: Option<String>,
}

/// The last report and when it came.
#[derive(Default)]
pub struct EditorContextState(Mutex<Option<(UiContext, Instant)>>);

impl EditorContextState {
    pub fn report(&self, context: UiContext) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some((context, Instant::now()));
    }

    pub fn read(&self) -> EditorContext {
        let last = self.0.lock().unwrap_or_else(|p| p.into_inner()).clone();
        EditorContext::new(last.map(|(context, at)| (context, at.elapsed())))
    }
}

/// What `get_editor_context` answers.
#[derive(Debug, Serialize)]
pub struct EditorContext {
    /// False until the window has reported anything (Kerf headless, or still loading).
    pub reported: bool,
    #[serde(flatten)]
    pub context: Option<UiContext>,
    /// Seconds since the window last reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_secs: Option<f64>,
    /// The window has stopped reporting: treat the rest as a hint, not as what is on screen.
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

impl EditorContext {
    fn new(last: Option<(UiContext, Duration)>) -> Self {
        match last {
            None => Self {
                reported: false,
                context: None,
                age_secs: None,
                stale: true,
                note: Some(
                    "No editor window has reported what it shows (Kerf is running without one, or it is still \
                     loading). There is no selection or playhead to go by: ask the user, or read the timeline.",
                ),
            },
            Some((context, age)) => {
                let stale = age > STALE_AFTER;
                Self {
                    reported: true,
                    context: Some(context),
                    age_secs: Some((age.as_secs_f64() * 10.0).round() / 10.0),
                    stale,
                    note: stale.then_some(
                        "The editor window has stopped reporting (closed or not responding); this is what it \
                         showed last.",
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EditorContext, EditorContextState, Span, UiContext};
    use std::time::Duration;

    #[test]
    fn nothing_reported_is_a_clear_empty_state() {
        let v = serde_json::to_value(EditorContextState::default().read()).unwrap();
        assert_eq!(v["reported"], false);
        assert_eq!(v["stale"], true);
        assert!(v.get("playhead").is_none() && v.get("age_secs").is_none(), "{v}");
        assert!(v["note"].as_str().unwrap().contains("No editor window"), "{v}");
    }

    #[test]
    fn a_report_comes_back_flat_with_its_age() {
        let state = EditorContextState::default();
        state.report(UiContext {
            playhead: 12.5,
            selected_clip_ids: vec!["c1".into()],
            mark_in: Some(10.0),
            visible_range: Some(Span { start: 0.0, end: 30.0 }),
            focused_panel: Some("timeline".into()),
            ..UiContext::default()
        });
        let v = serde_json::to_value(state.read()).unwrap();
        assert_eq!(v["reported"], true);
        assert_eq!(v["stale"], false);
        assert_eq!(v["playhead"], 12.5);
        assert_eq!(v["selected_clip_ids"], serde_json::json!(["c1"]));
        assert_eq!(v["mark_in"], 10.0);
        assert!(v["mark_out"].is_null());
        assert_eq!(v["visible_range"]["end"], 30.0);
        assert_eq!(v["focused_panel"], "timeline");
        assert!(v["age_secs"].as_f64().unwrap() < 5.0);
        assert!(v.get("note").is_none(), "{v}");
    }

    #[test]
    fn an_old_report_is_stale() {
        let fresh = EditorContext::new(Some((UiContext::default(), Duration::from_secs(30))));
        assert!(!fresh.stale);
        let old = EditorContext::new(Some((UiContext::default(), Duration::from_secs(600))));
        assert!(old.stale && old.note.is_some());
        assert_eq!(old.age_secs, Some(600.0));
    }

    #[test]
    fn the_webview_may_leave_the_optional_fields_out() {
        let c: UiContext = serde_json::from_value(serde_json::json!({ "playhead": 3.0 })).unwrap();
        assert_eq!(
            c,
            UiContext {
                playhead: 3.0,
                ..UiContext::default()
            }
        );
    }
}
