//! The preview-proxy line, read and steered, for both surfaces: the queue a panel shows, an asset
//! asked for next, one job or the whole queue cancelled, the queue paused and resumed. The Tauri
//! commands and the MCP tool call the same `*_now` functions here, so the project lock and the events
//! are handled once. `kerf_core::proxy` keeps the line itself.
//!
//! It also keeps the line **ranked**. The core orders the jobs by what the user asked for next, then
//! what a preview is waiting on (Proxy only), then what the cut uses, then the rest, and takes the two
//! sets from the adapter ([`kerf_core::proxy::rerank`], which *replaces* both). This module remembers
//! the pair and passes both every time: the waited-on set from where a preview asks for a frame
//! ([`note_waits`], with the project lock released), the used set from a task that follows the
//! project's changes ([`start`]), so one cheap place covers every edit, undo, open and applied proposal.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use kerf_core::project_activity as areas;
use kerf_core::{ProxyQueue, ProxyStatus, ProxyWait};

use crate::*;

/// What the line is ranked by, as the adapter last told the core.
#[derive(Default)]
struct Ranking {
    /// The assets a preview is waiting on.
    waits: HashSet<Uuid>,
    /// The assets the cut uses on its video tracks.
    used: HashSet<Uuid>,
}

impl Ranking {
    /// Take the assets a preview waits on; whether that changed anything.
    fn wait_on(&mut self, waits: HashSet<Uuid>) -> bool {
        if self.waits == waits {
            return false;
        }
        self.waits = waits;
        true
    }

    /// Take the assets the cut uses; whether that changed anything.
    fn use_assets(&mut self, used: HashSet<Uuid>) -> bool {
        if self.used == used {
            return false;
        }
        self.used = used;
        true
    }

    fn apply(&self) {
        kerf_core::proxy::rerank(&self.waits, &self.used);
    }
}

fn ranking() -> std::sync::MutexGuard<'static, Ranking> {
    static RANKING: OnceLock<Mutex<Ranking>> = OnceLock::new();
    RANKING.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Tell the line what a preview is waiting on (`Project::proxy_waits`, or the snapshot's
/// `proxy_waits_in`): the assets of `waits` go before the rest of the cut's. Call it with the project
/// lock **released**; it takes only this module's own short lock and the line's. Nothing waited on
/// (always the case unless the preview source is Proxy only) is a comparison, so a frame may ask
/// every time.
pub(crate) fn note_waits(waits: &[ProxyWait]) {
    let mut ranking = ranking();
    if waits.is_empty() && ranking.waits.is_empty() {
        return;
    }
    if ranking.wait_on(kerf_core::proxy::waiting_assets(waits)) {
        ranking.apply();
    }
}

/// Tell the line what the cut uses (`Project::proxy_used_assets`), resolved under the lock and
/// handed over with it released.
fn note_used(used: HashSet<Uuid>) {
    let mut ranking = ranking();
    if ranking.use_assets(used) {
        ranking.apply();
    }
}

/// What the cut uses, read with the lock taken for that alone.
fn read_used(app: &impl Host) -> Option<HashSet<Uuid>> {
    let state = app.managed::<AppState>()?;
    let used = lock_user(&state.project).proxy_used_assets();
    used.map_err(|e| tracing::debug!(error = %e, "could not read what the cut uses to rank the proxy queue"))
        .ok()
}

/// Read what the cut uses and tell the line.
fn rank_used(app: &impl Host) {
    if let Some(used) = read_used(app) {
        note_used(used);
    }
}

/// The areas whose writes can change what the cut uses: the cut itself, and the project being replaced
/// by another. Read as the user (`lock_user`), so it is the live cut: a clip an agent has only staged
/// counts once the user applies it, which is a write to the cut.
const RANKS_BY: u32 = areas::TIMELINE | areas::PROJECT;

/// Once at startup, in the desktop app and headless alike: have the page told when the queue
/// changes (the `proxy-queue-changed` event, no payload — the page reads the queue), and keep the
/// line ranked by what the cut uses as the project changes.
pub(crate) fn start(app: &impl Host) {
    let emitter = app.clone();
    kerf_core::proxy::set_queue_listener(Arc::new(move || {
        if let Err(e) = emitter.send("proxy-queue-changed", ()) {
            tracing::warn!(error = %e, "failed to emit proxy-queue-changed");
        }
    }));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut changes = crate::mcp::project_changes();
        let first = app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || rank_used(&first)).await;
        loop {
            match changes.recv().await {
                Ok(crate::mcp::ActivityEvent::Project(changed)) if changed & RANKS_BY == 0 => continue,
                Ok(crate::mcp::ActivityEvent::Job(_)) => continue,
                // Too many to follow: read the project afresh, as for any change.
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
            let app = app.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || rank_used(&app)).await;
        }
    });
}

/// Tell the webview about an asset's proxy: the `proxy-progress` event, as the job's own notifier
/// does. A cancel does not call the job's notifier, so the caller sends the fresh status itself.
fn announce(app: &impl Host, status: &ProxyStatus) {
    if let Err(e) = app.send("proxy-progress", status) {
        tracing::warn!(error = %e, "failed to emit proxy-progress");
    }
}

/// The queue now: every job in the order it runs, how long it should take, whether it is paused
/// and what it waits for, and the proxies that failed. The inputs are taken under the lock and the
/// core is asked with it released.
pub(crate) fn queue_now<'a>(lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>) -> CmdResult<ProxyQueue> {
    let inputs = lock().proxy_inputs().map_err(|e| e.to_string())?;
    Ok(kerf_core::proxy::queue(&inputs))
}

/// Build an asset's proxy before everything else that is waiting, without discarding what there is:
/// a proxy that is ready is left alone and one being built is not restarted. A proxy the user had
/// deleted is wanted again, once the core accepted the ask (as for a rebuild). Returns where the
/// asset stands; `proxy-progress` follows it.
pub(crate) fn prioritize_proxy_now<'a>(
    app: &impl Host,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
) -> CmdResult<ProxyStatus> {
    crate::build_proxy_now(
        app,
        lock,
        asset_id,
        kerf_core::proxy::build_next,
        "its proxy was being put first",
    )
}

/// Cancel one asset's proxy job, queued or building: it goes back to *not built* (nothing queues it
/// again by itself this session until it is rebuilt or asked for next). Answers the asset's fresh
/// status, which it also sends as `proxy-progress` (a cancel does not call the job's notifier).
pub(crate) fn cancel_proxy_now<'a>(
    app: &impl Host,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
    asset_id: Uuid,
) -> CmdResult<ProxyStatus> {
    let input = lock().proxy_input(asset_id).map_err(|e| e.to_string())?;
    kerf_core::proxy::cancel(&input.asset);
    let status = kerf_core::proxy::status(&input);
    announce(app, &status);
    Ok(status)
}

/// Cancel every queued or building proxy job. Answers the fresh status of each asset it cancelled
/// (all of which it also sends as `proxy-progress`); failures stay as they are and a pause is not lifted.
pub(crate) fn cancel_queue_now<'a>(
    app: &impl Host,
    lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>,
) -> CmdResult<Vec<ProxyStatus>> {
    // Read first, so a project that cannot be read fails before anything is cancelled.
    let mut inputs = lock().proxy_inputs().map_err(|e| e.to_string())?;
    let cancelled: HashSet<Uuid> = kerf_core::proxy::cancel_all().into_iter().collect();
    inputs.retain(|input| cancelled.contains(&input.asset.id));
    let statuses = kerf_core::proxy::statuses(&inputs);
    for status in &statuses {
        announce(app, status);
    }
    Ok(statuses)
}

/// Pause the queue: the build under way finishes and nothing new starts until it is resumed. Answers
/// the queue as it now stands.
pub(crate) fn pause_queue_now<'a>(lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>) -> CmdResult<ProxyQueue> {
    kerf_core::proxy::pause();
    queue_now(lock)
}

/// Lift a pause. Answers the queue as it now stands.
pub(crate) fn resume_queue_now<'a>(lock: &dyn Fn() -> std::sync::MutexGuard<'a, Project>) -> CmdResult<ProxyQueue> {
    kerf_core::proxy::resume();
    queue_now(lock)
}

/// The proxy queue is the process's, shared by every test that queues a proxy: one takes it at a time,
/// paused so that no worker starts the (nonexistent) files while it looks, and resumed when it is done.
#[cfg(test)]
pub(crate) struct PausedQueue(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

#[cfg(test)]
impl PausedQueue {
    pub(crate) fn take() -> Self {
        static TURN: Mutex<()> = Mutex::new(());
        let turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
        kerf_core::proxy::pause();
        Self(turn)
    }
}

#[cfg(test)]
impl Drop for PausedQueue {
    fn drop(&mut self) {
        kerf_core::proxy::resume();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> crate::shell::Shell {
        headless_with(Arc::new(Mutex::new(Project::open_in_memory().unwrap())))
    }

    fn headless_with(project: Arc<Mutex<Project>>) -> crate::shell::Shell {
        let state = AppState::new(
            project,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            crate::LaunchSlot::default(),
            Arc::new(kerf_core::ScrubFrames::new(kerf_core::ScrubConfig::default())),
        );
        crate::shell::Shell::Headless(Arc::new(crate::shell::Headless::new(state, None, None)))
    }

    fn ids(n: usize) -> Vec<Uuid> {
        (0..n).map(|_| Uuid::new_v4()).collect()
    }

    /// The core *replaces* both sets on every call, so the pair it is handed must always carry the
    /// one that did not just change: a wait must not wipe what the cut uses, nor an edit what a
    /// preview waits on.
    #[test]
    fn each_set_is_replaced_and_the_other_is_kept() {
        let [a, b, c] = ids(3)[..] else { unreachable!() };
        let mut ranking = Ranking::default();
        assert!(ranking.use_assets([a, b].into()), "the first cut");
        assert!(ranking.wait_on([b].into()), "a preview waits on b");
        assert_eq!(ranking.used, HashSet::from([a, b]), "waiting does not forget the cut");
        assert!(ranking.use_assets([a, b, c].into()), "an edit adds a clip");
        assert_eq!(ranking.waits, HashSet::from([b]), "an edit does not forget the wait");
        assert!(ranking.wait_on(HashSet::new()), "the proxy landed");
        assert_eq!(ranking.used, HashSet::from([a, b, c]));
    }

    #[test]
    fn a_pair_that_did_not_change_is_not_sent_again() {
        let [a, b] = ids(2)[..] else { unreachable!() };
        let mut ranking = Ranking::default();
        assert!(!ranking.wait_on(HashSet::new()), "nothing waited on is nothing to say");
        assert!(!ranking.use_assets(HashSet::new()), "an empty cut is nothing to say");
        assert!(ranking.use_assets([a, b].into()));
        assert!(!ranking.use_assets([b, a].into()), "the same assets in another order");
        assert!(ranking.wait_on([a].into()));
        assert!(!ranking.wait_on([a].into()));
    }

    fn video(id: Uuid, name: &str) -> kerf_core::Asset {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "path": format!("/proxy-queue-test/{id}.mp4"),
            "name": name,
            "duration": 10.0,
            "streams": [{ "index": 0, "kind": "video", "codec": "h264", "width": 1920, "height": 1080, "fps": 30.0 }],
            "imported_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    /// An asset asked for next goes to the front of the line, cancelling it takes it out again and
    /// says so in its fresh status, and a deleted proxy's mark is cleared by the ask. The queue is
    /// paused throughout, so no worker starts the (nonexistent) files while the test looks.
    #[test]
    fn an_asset_asked_for_next_is_first_and_a_cancel_takes_it_out_and_says_so() {
        let shared = Mutex::new(Project::open_in_memory().unwrap());
        let (first, second) = (video(Uuid::new_v4(), "first.mp4"), video(Uuid::new_v4(), "second.mp4"));
        for asset in [&first, &second] {
            shared.lock().unwrap().insert_asset(asset).unwrap();
        }
        let shell = headless();
        let heard = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let listening = {
            let heard = heard.clone();
            shell.listen("proxy-progress", move |payload| {
                heard.lock().unwrap().push(serde_json::from_str(payload).unwrap());
            })
        };
        let lock = || lock_user(&shared);
        let _paused = PausedQueue::take();
        shared.lock().unwrap().set_proxy_declined(second.id, true).unwrap();

        prioritize_proxy_now(&shell, &lock, first.id).unwrap();
        prioritize_proxy_now(&shell, &lock, second.id).unwrap();
        assert!(
            !shared.lock().unwrap().proxy_declined(second.id).unwrap(),
            "asking for a deleted proxy wants it back"
        );
        let queue = queue_now(&lock).unwrap();
        assert!(queue.paused);
        let order: Vec<Uuid> = queue
            .jobs
            .iter()
            .map(|j| j.asset_id)
            .filter(|id| *id == first.id || *id == second.id)
            .collect();
        assert_eq!(order, [second.id, first.id], "the latest ask goes first");
        let job = queue.jobs.iter().find(|j| j.asset_id == second.id).unwrap();
        assert_eq!((job.name.as_str(), job.width, job.height), ("second.mp4", 1920, 1080));
        assert_eq!(
            job.position,
            queue.jobs.iter().position(|j| j.asset_id == second.id).unwrap() + 1
        );

        let cancelled = cancel_proxy_now(&shell, &lock, second.id).unwrap();
        assert_ne!(cancelled.state, kerf_core::ProxyPhase::Queued, "{cancelled:?}");
        assert!(
            heard
                .lock()
                .unwrap()
                .iter()
                .any(|s| s["asset_id"] == second.id.to_string() && s["state"] == serde_json::to_value(cancelled.state).unwrap()),
            "a cancel is announced with the fresh status: {:?}",
            heard.lock().unwrap()
        );
        let queue = queue_now(&lock).unwrap();
        assert!(
            queue.jobs.iter().all(|j| j.asset_id != second.id),
            "the cancelled job is gone"
        );
        assert!(queue.jobs.iter().any(|j| j.asset_id == first.id), "the other stays");

        cancel_proxy_now(&shell, &lock, first.id).unwrap();
        shell.unlisten(listening);
    }

    #[test]
    fn an_asset_that_is_not_in_the_project_is_refused_by_name() {
        let shared = Mutex::new(Project::open_in_memory().unwrap());
        let shell = headless();
        let lock = || lock_user(&shared);
        let missing = Uuid::new_v4();
        assert!(prioritize_proxy_now(&shell, &lock, missing).is_err());
        assert!(cancel_proxy_now(&shell, &lock, missing).is_err());
    }
    /// What the line is ranked by is the cut's pictures: the assets with a clip on a video track, however many
    /// clips, and not the library's others nor a sound.
    #[test]
    fn the_line_is_ranked_by_the_assets_the_cut_puts_on_video_tracks() {
        let project = Arc::new(Mutex::new(Project::open_in_memory().unwrap()));
        let shell = headless_with(project.clone());
        assert_eq!(read_used(&shell), Some(HashSet::new()), "an empty cut uses nothing");
        let (used, unused) = (video(Uuid::new_v4(), "used.mp4"), video(Uuid::new_v4(), "unused.mp4"));
        {
            let project = project.lock().unwrap();
            for asset in [&used, &unused] {
                project.insert_asset(asset).unwrap();
            }
            project.add_clip_to_timeline(used.id, None, 0.0, 4.0, None).unwrap();
            project.add_clip_to_timeline(used.id, None, 5.0, 8.0, None).unwrap();
        }
        assert_eq!(read_used(&shell), Some(HashSet::from([used.id])));
    }

    /// The page hears of a change in the queue, with no payload, once `start` has registered the listener
    /// (the core calls it from a thread of its own, so the test waits for it).
    #[test]
    fn the_page_is_told_when_the_queue_changes() {
        let shell = headless();
        let heard = Arc::new(Mutex::new(Vec::<String>::new()));
        let listening = {
            let heard = heard.clone();
            shell.listen("proxy-queue-changed", move |payload| {
                heard.lock().unwrap().push(payload.to_string());
            })
        };
        start(&shell);
        let _paused = PausedQueue::take();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while heard.lock().unwrap().is_empty() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            heard.lock().unwrap().first().map(String::as_str),
            Some("null"),
            "the event carries nothing"
        );
        shell.unlisten(listening);
    }
}
