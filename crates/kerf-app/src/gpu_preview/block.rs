//! The GPU preview turning itself off on a machine, and staying off.
//!
//! The preview may be on without the user ever having asked for it ([`crate::settings::GPU_PREVIEW_DEFAULT_ON`]),
//! so every way it can fail on a machine has to end, with nothing done by the user, in FFmpeg's picture, and
//! the way that is *not* a per-frame fallback has to be remembered across launches or the same failure
//! greets the user on every start. What is remembered is a [`GpuBlock`] in the settings file (the machine's
//! preferences, not a project's):
//!
//! * [`BlockKind::Crash`]: a run ended while the preview was starting (the attempt marker, `gpu_preview.rs`),
//!   or two runs in a row ended while it was up ([`launch_verdict`], the session marker). Nothing is built
//!   until the block lapses: the adapter is not known, and the one thing known about it is that touching it
//!   took the process down.
//! * [`BlockKind::Mismatch`]: the parity self-test found the adapter drawing something else than this FFmpeg
//!   does. It names the adapter, driver and FFmpeg it was found on ([`super::check::AdapterKey`]); the next
//!   launch starts with that key closed ([`super::check::Check::refuse`]) and a driver update, another FFmpeg
//!   or a new Kerf is checked again.
//!
//! A block lapses with the app's version (a fix may be in the new one) and with the user's own word: any
//! write of the `gpu_preview` setting (on or off) drops it, because a deliberate choice outranks an automatic
//! one. A check that *could not be made* is not a block: what stopped it may not be there next time.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The version of the running app: what a block is held against.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why the preview was turned off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    /// A run ended while the preview was starting or up: nothing is built.
    Crash,
    /// The self-test found an adapter drawing something else than this FFmpeg: that adapter is closed.
    Mismatch,
}

/// The preview turned itself off on this machine (stored under `gpu_preview_block`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuBlock {
    pub kind: BlockKind,
    /// What the user is told, in a sentence.
    pub reason: String,
    /// The app version that wrote it: another version forgets it.
    pub app_version: String,
    /// `Mismatch`: the adapter, driver and FFmpeg it was found on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    /// Seconds since the epoch, for the log.
    #[serde(default)]
    pub at: u64,
    /// The page has said so (once, quietly).
    #[serde(default)]
    pub told: bool,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl GpuBlock {
    pub fn crash(reason: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Crash,
            reason: reason.into(),
            app_version: APP_VERSION.to_string(),
            adapter: None,
            at: now_secs(),
            told: false,
        }
    }

    pub fn mismatch(adapter: &str, reason: impl Into<String>) -> Self {
        Self {
            kind: BlockKind::Mismatch,
            reason: reason.into(),
            app_version: APP_VERSION.to_string(),
            adapter: Some(adapter.to_string()),
            at: now_secs(),
            told: false,
        }
    }

    /// Whether this block was written by `version`: another version of the app forgets it.
    pub fn holds_in(&self, version: &str) -> bool {
        self.app_version == version
    }

    /// Whether this block keeps the preview from being built at all (a crash does; a mismatch closes one
    /// adapter's gate and leaves the rest to the check).
    pub fn disables_in(&self, version: &str) -> bool {
        self.kind == BlockKind::Crash && self.holds_in(version)
    }
}

/// The note a crash found at launch leaves.
pub const CRASH_NOTE: &str = "turned itself off: the last run ended while the GPU preview was starting (a driver crash?); \
     switching it on again tries once more";

/// The note two runs in a row ending while it was up leave.
pub const SESSION_NOTE: &str = "turned itself off: the last two runs ended while the GPU preview was on (a driver crash?); \
     switching it on again tries once more";

/// Runs in a row that must end while the preview is up before it is turned off for it. One would put it
/// at the mercy of a power cut or a task manager; a crash in a driver comes back with the next frame.
pub const STRIKES_TO_BLOCK: u32 = 2;

/// The file whose presence says a run ended while the preview was up (and how many in a row did).
pub const SESSION_MARKER: &str = "gpu-preview-session";

/// What a launch makes of the markers the last run left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchVerdict {
    /// Turn the preview off, for this reason.
    pub block: Option<GpuBlock>,
    /// Runs in a row so far that ended while the preview was up; carried into this run's session marker.
    pub strikes: u32,
}

/// What to do about the markers found at launch. `crashed`: the attempt marker was down (the run ended
/// while a backend was being made or before a frame reached the screen). `unclean`: the session marker
/// was down, with the runs in a row it counted.
pub fn launch_verdict(crashed: bool, unclean: Option<u32>) -> LaunchVerdict {
    if crashed {
        return LaunchVerdict {
            block: Some(GpuBlock::crash(CRASH_NOTE)),
            strikes: 0,
        };
    }
    let Some(before) = unclean else {
        return LaunchVerdict { block: None, strikes: 0 };
    };
    let strikes = before.saturating_add(1);
    if strikes >= STRIKES_TO_BLOCK {
        LaunchVerdict {
            block: Some(GpuBlock::crash(SESSION_NOTE)),
            strikes: 0,
        }
    } else {
        LaunchVerdict { block: None, strikes }
    }
}

/// Whether the last run left its session marker (and remove it): the runs in a row that ended while the
/// preview was up, or `None`. A marker that cannot be read is one run.
pub fn take_session_marker(dir: &Path) -> Option<u32> {
    let path = dir.join(SESSION_MARKER);
    let text = std::fs::read_to_string(&path);
    if !path.exists() {
        return None;
    }
    let _ = std::fs::remove_file(&path);
    Some(text.ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0))
}

/// The session marker of this run: down while a backend is up, gone when it is dropped or the app exits
/// cleanly.
#[derive(Debug, Clone)]
pub struct SessionMarker {
    path: PathBuf,
    carried: u32,
}

impl SessionMarker {
    pub fn new(dir: &Path, carried: u32) -> Self {
        Self {
            path: dir.join(SESSION_MARKER),
            carried,
        }
    }

    /// A backend is up: a run that ends from here on without [`SessionMarker::clear`] is a strike.
    pub fn write(&self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&self.path, self.carried.to_string());
    }

    /// The backend is gone, or the app is closing: this run is not a strike.
    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kerf-gpu-block-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_launch_that_found_nothing_turns_nothing_off() {
        assert_eq!(launch_verdict(false, None), LaunchVerdict { block: None, strikes: 0 });
    }

    #[test]
    fn a_run_that_ended_while_the_preview_was_starting_blocks_it_at_once() {
        let v = launch_verdict(true, None);
        let block = v.block.expect("a block");
        assert_eq!(block.kind, BlockKind::Crash);
        assert_eq!(block.reason, CRASH_NOTE);
        assert!(block.disables_in(APP_VERSION));
        assert_eq!(v.strikes, 0);
        // Whatever else the session marker said, the crash is the reason.
        assert_eq!(launch_verdict(true, Some(1)).block.unwrap().reason, CRASH_NOTE);
    }

    #[test]
    fn one_run_that_ended_while_it_was_up_is_a_strike_and_two_in_a_row_block_it() {
        let first = launch_verdict(false, Some(0));
        assert_eq!(first.block, None, "one unclean run could be a power cut");
        assert_eq!(first.strikes, 1, "carried into the next session marker");
        let second = launch_verdict(false, Some(first.strikes));
        let block = second.block.expect("the second in a row blocks");
        assert_eq!(block.kind, BlockKind::Crash);
        assert_eq!(block.reason, SESSION_NOTE);
        assert_eq!(second.strikes, 0, "a block starts the count again");
    }

    #[test]
    fn the_session_marker_carries_the_count_and_a_clean_exit_removes_it() {
        let dir = scratch();
        assert_eq!(take_session_marker(&dir), None, "no marker, no strike");
        let marker = SessionMarker::new(&dir, 1);
        marker.write();
        assert!(dir.join(SESSION_MARKER).exists());
        marker.clear();
        assert_eq!(take_session_marker(&dir), None, "a run that closed cleanly leaves nothing");
        marker.write();
        assert_eq!(take_session_marker(&dir), Some(1), "the next launch finds the count");
        assert!(!dir.join(SESSION_MARKER).exists(), "and takes the marker");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_marker_that_cannot_be_read_is_one_run() {
        let dir = scratch();
        std::fs::write(dir.join(SESSION_MARKER), "not a number").unwrap();
        assert_eq!(take_session_marker(&dir), Some(0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_block_lapses_with_the_version_and_only_a_crash_keeps_the_device_from_being_built() {
        let crash = GpuBlock::crash("c");
        assert!(crash.holds_in(APP_VERSION) && crash.disables_in(APP_VERSION));
        assert!(
            !crash.holds_in("0.0.0") && !crash.disables_in("0.0.0"),
            "a new version tries again"
        );
        let mismatch = GpuBlock::mismatch("a gpu | Vulkan | driver 1 | ffmpeg", "m");
        assert!(mismatch.holds_in(APP_VERSION));
        assert!(
            !mismatch.disables_in(APP_VERSION),
            "a mismatch closes one adapter's gate, the check does the rest"
        );
        assert_eq!(mismatch.adapter.as_deref(), Some("a gpu | Vulkan | driver 1 | ffmpeg"));
    }

    #[test]
    fn a_block_round_trips_and_a_stored_one_without_the_optional_fields_reads() {
        let block = GpuBlock::mismatch("x", "y");
        let back: GpuBlock = serde_json::from_value(serde_json::to_value(&block).unwrap()).unwrap();
        assert_eq!(back, block);
        let bare: GpuBlock = serde_json::from_str(r#"{"kind":"crash","reason":"r","app_version":"1.0.0"}"#).unwrap();
        assert!(!bare.told && bare.adapter.is_none() && bare.at == 0);
        assert!(serde_json::from_str::<GpuBlock>(r#"{"kind":"nope","reason":"r","app_version":"1"}"#).is_err());
    }
}
