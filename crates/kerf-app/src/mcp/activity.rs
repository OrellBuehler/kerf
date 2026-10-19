//! The live activity stream: what an agent is told without asking.
//!
//! A background job moving along or ending, and the project changing — the user cutting, a proposal
//! applied or discarded, a task queued, media analysed, another project opened — are pushed to the
//! sessions that subscribed, as MCP **resource-updated notifications**: the four [`resources`]
//! (`kerf://project`, `kerf://tasks`, `kerf://jobs`, `kerf://jobs/{job_id}`) say what changed, and
//! reading one (`resources/read`, the same answers as the polling tools) says how. A client
//! subscribes per resource with `resources/subscribe` (the notification rides the session's
//! standalone SSE stream, the `GET /mcp`), or, at protocol `2026-07-28`, with `subscriptions/listen`
//! (it rides that request's stream). The polling tools stay as they were.
//!
//! **Nothing upstream waits for a client.** Producers [`publish`] into one bounded broadcast
//! channel (a send never blocks and fails quietly with nobody listening): a job's registry after
//! its own lock is released, and the project's areas from `kerf_core::project_activity`, woken by
//! its listener (under the project lock — it only wakes [`pump_project`], which takes the marks a
//! moment later and publishes with no lock held). Each session reads the channel on a task of its
//! own (started by its first subscription), gathers what arrived in a short window (a job's progress
//! ticks collapse to one), and sends. A session too slow to keep up falls behind the channel's
//! capacity and is told every resource it subscribed changed (it re-reads), so it costs neither
//! memory nor the engine anything. A session's task ends with the session — its [`Listener`] is
//! dropped with the last handle of the session's server, which cancels the task — or once the client
//! has asked nothing for [`CLIENT_QUIET`], so that our own sends do not keep a gone client's session
//! alive.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use kerf_core::project_activity as areas;
use rmcp::model::{Resource, ResourceTemplate, ResourceUpdatedNotificationParam};
use rmcp::service::{Peer, RoleServer};
use rmcp::ErrorData as McpError;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

pub const PROJECT_URI: &str = "kerf://project";
pub const TASKS_URI: &str = "kerf://tasks";
pub const JOBS_URI: &str = "kerf://jobs";
pub const JOB_URI_PREFIX: &str = "kerf://jobs/";

/// Events the channel holds before a reader that has not kept up is told it lagged.
const CAPACITY: usize = 256;
/// How long a session gathers events before it sends: a burst of edits or of progress ticks is one
/// notification per resource.
const GATHER: Duration = Duration::from_millis(200);
/// How long one notification may take to reach a session's stream before the batch is given up on
/// (and the next one tells everything again).
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a client may ask nothing before its session is sent nothing more and its subscriptions
/// go: rmcp's own idle limit for a session.
const CLIENT_QUIET: Duration = Duration::from_secs(300);
/// The most resources one session may subscribe to.
const MAX_SUBSCRIPTIONS: usize = 64;

/// What happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A background job started, moved or ended.
    Job(String),
    /// These areas of the project changed (`kerf_core::project_activity` bits).
    Project(u32),
}

fn channel() -> &'static broadcast::Sender<Event> {
    static CHANNEL: OnceLock<broadcast::Sender<Event>> = OnceLock::new();
    CHANNEL.get_or_init(|| broadcast::channel(CAPACITY).0)
}

/// Tell every listening session. Never blocks; with nobody listening it does nothing.
pub fn publish(event: Event) {
    let _ = channel().send(event);
}

/// Feed the project's changes into the channel, for the life of the process. The core's listener
/// only wakes this task (it runs under the project lock); the task takes the marks once the burst
/// has had [`GATHER`] to settle, with no lock held.
pub fn pump_project() {
    let wake = Arc::new(tokio::sync::Notify::new());
    let woken = wake.clone();
    if !areas::set_listener(move || woken.notify_one()) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            wake.notified().await;
            tokio::time::sleep(GATHER).await;
            let changed = areas::take();
            if changed != 0 {
                publish(Event::Project(changed));
            }
        }
    });
}

/// The resources a client can read and subscribe to.
pub fn resources() -> Vec<Resource> {
    [
        (
            PROJECT_URI,
            "project",
            "The open project: its generation (moves when another project is opened), the cut as the agent sees it (a brief) and whether a proposal is staged. Updated when the cut, the proposal, the media or the project itself changes, whoever changed it.",
        ),
        (
            TASKS_URI,
            "tasks",
            "The task queue (the first page of list_tasks). Updated when a task is added, claimed, finished or removed.",
        ),
        (
            JOBS_URI,
            "jobs",
            "The background jobs (list_jobs). Updated when any job starts, moves along or ends.",
        ),
    ]
    .into_iter()
    .map(|(uri, name, description)| {
        Resource::new(uri, name)
            .with_description(description)
            .with_mime_type("application/json")
    })
    .collect()
}

/// The resource templates: one background job.
pub fn resource_templates() -> Vec<ResourceTemplate> {
    vec![ResourceTemplate::new(format!("{JOB_URI_PREFIX}{{job_id}}"), "job")
        .with_description(
            "One background job (job_status): its progress and, once it ends, its result or error. Updated as it moves and when it ends.",
        )
        .with_mime_type("application/json")]
}

/// Whether `uri` names one of [`resources`] or a job.
pub fn known(uri: &str) -> bool {
    matches!(uri, PROJECT_URI | TASKS_URI | JOBS_URI) || job_of(uri).is_some()
}

/// The job id a `kerf://jobs/{job_id}` names.
pub fn job_of(uri: &str) -> Option<&str> {
    uri.strip_prefix(JOB_URI_PREFIX).filter(|id| is_job_id(id))
}

/// Whether `id` has the shape the jobs registry gives one: `job-` and eight lowercase hex digits.
fn is_job_id(id: &str) -> bool {
    id.strip_prefix("job-")
        .is_some_and(|hex| hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// What arrived in one gathering window.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    jobs: BTreeSet<String>,
    areas: u32,
    /// The reader fell behind the channel: anything may have changed.
    lagged: bool,
}

impl Batch {
    fn add(&mut self, event: Event) {
        match event {
            Event::Job(id) => {
                self.jobs.insert(id);
            }
            Event::Project(a) => self.areas |= a,
        }
    }

    /// The resources of `subscribed` this batch changed, in a fixed order.
    pub fn updated<'a>(&self, subscribed: impl IntoIterator<Item = &'a String>) -> Vec<String> {
        subscribed
            .into_iter()
            .filter(|uri| {
                self.lagged
                    || match uri.as_str() {
                        PROJECT_URI => self.areas & !areas::TASKS != 0,
                        TASKS_URI => self.areas & areas::TASKS != 0,
                        JOBS_URI => !self.jobs.is_empty(),
                        other => job_of(other).is_some_and(|id| self.jobs.contains(id)),
                    }
            })
            .cloned()
            .collect()
    }
}

/// The next batch from `rx`: the first event, and whatever else arrives within [`GATHER`]. `None`
/// once `ended` is cancelled or the channel is gone.
async fn next_batch(rx: &mut broadcast::Receiver<Event>, ended: &CancellationToken) -> Option<Batch> {
    let mut batch = Batch::default();
    let first = tokio::select! {
        _ = ended.cancelled() => return None,
        event = rx.recv() => event,
    };
    match first {
        Ok(event) => batch.add(event),
        Err(broadcast::error::RecvError::Lagged(_)) => batch.lagged = true,
        Err(broadcast::error::RecvError::Closed) => return None,
    }
    tokio::select! {
        _ = ended.cancelled() => return None,
        _ = tokio::time::sleep(GATHER) => {}
    }
    loop {
        match rx.try_recv() {
            Ok(event) => batch.add(event),
            Err(broadcast::error::TryRecvError::Lagged(_)) => batch.lagged = true,
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Closed) => return None,
        }
    }
    Some(batch)
}

/// One session's subscriptions. Cloned into every handle of the session's server; when the last
/// is dropped the session's task ends.
#[derive(Clone, Default)]
pub struct Listener(Arc<Shared>);

struct Shared {
    /// What the session subscribed to, and whether its task is running: one lock, so a task that
    /// stops and a subscribe that starts one cannot pass each other.
    state: Mutex<Subscriptions>,
    /// When the client last asked the server anything.
    last_request: Mutex<Instant>,
    ended: CancellationToken,
}

#[derive(Default)]
struct Subscriptions {
    uris: BTreeSet<String>,
    running: bool,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            last_request: Mutex::new(Instant::now()),
            ended: CancellationToken::new(),
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.ended.cancel();
    }
}

impl Listener {
    /// The client asked something: it is there. A session whose client has asked nothing for
    /// [`CLIENT_QUIET`] is sent nothing more (see [`forward`]).
    pub fn touch(&self) {
        *self.0.last_request.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// Subscribe to `uri` ([`known`] only, at most [`MAX_SUBSCRIPTIONS`]) and, on the first, start
    /// the session's task, which sends through `peer`.
    pub fn subscribe(&self, uri: &str, peer: Peer<RoleServer>) -> Result<(), McpError> {
        if self.add(uri)? {
            let rx = channel().subscribe();
            let shared = Arc::downgrade(&self.0);
            let ended = self.0.ended.clone();
            tauri::async_runtime::spawn(forward(rx, shared, ended, peer));
        }
        Ok(())
    }

    /// Record a subscription to `uri`: whether the session's task has to be started for it.
    fn add(&self, uri: &str) -> Result<bool, McpError> {
        if !known(uri) {
            return Err(unknown_resource(uri));
        }
        self.touch();
        let mut state = self.state();
        if !state.uris.contains(uri) && state.uris.len() >= MAX_SUBSCRIPTIONS {
            return Err(McpError::invalid_params(
                format!("a session may subscribe to {MAX_SUBSCRIPTIONS} resources; unsubscribe from one first"),
                None,
            ));
        }
        state.uris.insert(uri.to_string());
        Ok(!std::mem::replace(&mut state.running, true))
    }

    pub fn unsubscribe(&self, uri: &str) {
        self.touch();
        self.state().uris.remove(uri);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, Subscriptions> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Shared {
    /// The resources to tell about, or `None` when the client has been quiet too long: its
    /// subscriptions are dropped and the task is to stop.
    fn subscribed_unless_quiet(&self) -> Option<BTreeSet<String>> {
        let quiet = self.last_request.lock().unwrap_or_else(|e| e.into_inner()).elapsed() >= CLIENT_QUIET;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if quiet {
            state.uris.clear();
            state.running = false;
            return None;
        }
        Some(state.uris.clone())
    }
}

/// A `resources/*` request naming no resource of this server.
pub fn unknown_resource(uri: &str) -> McpError {
    McpError::resource_not_found(
        format!(
            "no resource {uri:?}: kerf://project, kerf://tasks, kerf://jobs and kerf://jobs/{{job_id}} (a job_id as a tool answered it, job-xxxxxxxx) exist"
        ),
        None,
    )
}

/// The task of one session: batches in, a notification per subscribed resource that changed out.
///
/// A notification counts as activity on the session for rmcp's idle expiry, so a task that sent for
/// ever would keep the session of a client that is gone alive, and with it the proposal it owns.
/// So the task stops, and the subscriptions go, once the client has asked nothing for
/// [`CLIENT_QUIET`] (rmcp's own idle limit): liveness follows the client, not our sends.
async fn forward(mut rx: broadcast::Receiver<Event>, shared: Weak<Shared>, ended: CancellationToken, peer: Peer<RoleServer>) {
    let mut behind = false;
    while let Some(mut batch) = next_batch(&mut rx, &ended).await {
        batch.lagged |= std::mem::take(&mut behind);
        let Some(subscribed) = shared.upgrade().and_then(|s| s.subscribed_unless_quiet()) else {
            return;
        };
        for uri in batch.updated(&subscribed) {
            let sent = tokio::time::timeout(
                SEND_TIMEOUT,
                peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri)),
            )
            .await;
            match sent {
                Ok(Ok(())) => {}
                Ok(Err(_)) if peer.is_transport_closed() => return,
                _ => {
                    behind = true;
                    break;
                }
            }
        }
    }
}

/// `subscriptions/listen` (protocol `2026-07-28`): the same batches, sent through the subscription's
/// own sink, for the resources it accepted, until the request is cancelled. A send that takes over
/// [`SEND_TIMEOUT`] ends the subscription.
pub async fn listen(subscribed: Vec<String>, sink: rmcp::service::SubscriptionSink, ended: CancellationToken) {
    let mut rx = channel().subscribe();
    while let Some(batch) = next_batch(&mut rx, &ended).await {
        for uri in batch.updated(&subscribed) {
            match tokio::time::timeout(SEND_TIMEOUT, sink.notify_resource_updated(uri)).await {
                Ok(Ok(())) => {}
                _ => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(events: &[Event]) -> Batch {
        let mut b = Batch::default();
        for e in events {
            b.add(e.clone());
        }
        b
    }

    fn uris(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_batch_names_only_the_subscribed_resources_it_changed() {
        let all = uris(&[
            PROJECT_URI,
            TASKS_URI,
            JOBS_URI,
            "kerf://jobs/job-0000000a",
            "kerf://jobs/job-0000000b",
        ]);
        let b = batch(&[Event::Job("job-0000000a".into()), Event::Job("job-0000000a".into())]);
        assert_eq!(
            b.updated(&all),
            uris(&[JOBS_URI, "kerf://jobs/job-0000000a"]),
            "a job's ticks are one"
        );
        let b = batch(&[Event::Project(areas::TASKS)]);
        assert_eq!(b.updated(&all), uris(&[TASKS_URI]));
        let b = batch(&[Event::Project(areas::TIMELINE), Event::Project(areas::PROPOSAL)]);
        assert_eq!(b.updated(&all), uris(&[PROJECT_URI]));
        assert!(
            b.updated(&uris(&[TASKS_URI, JOBS_URI])).is_empty(),
            "not subscribed, not told"
        );
        let lagged = Batch {
            lagged: true,
            ..Batch::default()
        };
        assert_eq!(
            lagged.updated(&all),
            all,
            "a reader that fell behind re-reads everything it holds"
        );
        assert!(Batch::default().updated(&all).is_empty());
    }

    #[test]
    fn only_the_servers_resources_can_be_subscribed() {
        for uri in [PROJECT_URI, TASKS_URI, JOBS_URI, "kerf://jobs/job-1a2b3c4d"] {
            assert!(known(uri), "{uri}");
        }
        for uri in [
            "kerf://jobs/",
            "kerf://jobs/a/b",
            "kerf://jobs/job-1",
            "kerf://jobs/job-1A2B3C4D",
            "kerf://jobs/job-1a2b3c4d5",
            "kerf://jobs/job-1a2b3c4g",
            "kerf://timeline",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!known(uri), "{uri}");
        }
        let listener = Listener::default();
        assert!(listener.add("kerf://nope").is_err());
        assert!(listener.add(TASKS_URI).unwrap(), "the first subscription starts the task");
        assert!(!listener.add(TASKS_URI).unwrap(), "and only the first");
        assert_eq!(listener.state().uris.len(), 1);
        listener.unsubscribe(TASKS_URI);
        assert!(listener.state().uris.is_empty());
        for i in 0..MAX_SUBSCRIPTIONS {
            listener.add(&format!("{JOB_URI_PREFIX}job-{i:08x}")).unwrap();
        }
        assert!(listener.add(PROJECT_URI).is_err(), "a session holds a bounded number");
        assert!(
            listener.add(&format!("{JOB_URI_PREFIX}job-{:08x}", 0)).is_ok(),
            "one it holds is no new one"
        );
        assert_eq!(resources().len(), 3);
        assert_eq!(resource_templates().len(), 1);
    }

    /// A client that has asked nothing for the idle limit is sent nothing more: its subscriptions go
    /// and the task stops, so its session can expire. A request in time keeps them.
    #[test]
    fn a_quiet_client_loses_its_subscriptions() {
        let listener = Listener::default();
        assert!(listener.add(PROJECT_URI).unwrap());
        assert_eq!(listener.0.subscribed_unless_quiet().unwrap().len(), 1);
        let Some(long_ago) = Instant::now().checked_sub(CLIENT_QUIET) else {
            return;
        };
        *listener.0.last_request.lock().unwrap() = long_ago;
        assert!(listener.0.subscribed_unless_quiet().is_none());
        assert!(listener.state().uris.is_empty() && !listener.state().running);
        assert!(listener.add(PROJECT_URI).unwrap(), "subscribing again starts a task again");
        assert_eq!(listener.0.subscribed_unless_quiet().unwrap().len(), 1, "it was a request");
    }

    /// Events are gathered for a moment and then come as one batch; a reader that was not reading
    /// while the channel overflowed is told it lagged instead of being handed the backlog.
    #[tokio::test]
    async fn a_burst_is_one_batch_and_a_reader_left_behind_lags() {
        let (tx, mut rx) = broadcast::channel(4);
        let ended = CancellationToken::new();
        tx.send(Event::Job("job-1".into())).unwrap();
        tx.send(Event::Project(areas::TASKS)).unwrap();
        let b = next_batch(&mut rx, &ended).await.unwrap();
        assert_eq!(b.updated(&uris(&[TASKS_URI, JOBS_URI])), uris(&[TASKS_URI, JOBS_URI]));
        assert!(!b.lagged);
        for i in 0..10 {
            tx.send(Event::Job(format!("job-{i}"))).unwrap();
        }
        let b = next_batch(&mut rx, &ended).await.unwrap();
        assert!(b.lagged, "{b:?}");
        ended.cancel();
        assert!(next_batch(&mut rx, &ended).await.is_none(), "an ended session stops reading");
    }

    /// The project's writes reach the channel. A project opened and written before the pump started
    /// (as the app's is at launch) left its areas marked; the pump takes those at once, so a later
    /// write to an area that was already marked is still announced. The listener is the process's
    /// one, so only this test starts the pump.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_projects_writes_are_published_once_the_pump_runs() {
        async fn tasks_published(mut rx: broadcast::Receiver<Event>) -> bool {
            let heard = tokio::time::timeout(Duration::from_secs(5), async move {
                loop {
                    match rx.recv().await {
                        Ok(Event::Project(changed)) if changed & areas::TASKS != 0 => return true,
                        Err(broadcast::error::RecvError::Closed) => return false,
                        _ => {}
                    }
                }
            })
            .await;
            heard == Ok(true)
        }
        let project = kerf_core::Project::open_in_memory().unwrap();
        project.add_task("queued before anyone listened").unwrap();
        let stale = tasks_published(channel().subscribe());
        pump_project();
        assert!(stale.await, "the marks made before the pump are taken and published");
        let fresh = tasks_published(channel().subscribe());
        project.add_task("queued after the pump started").unwrap();
        assert!(fresh.await, "a write to an area marked before is announced");
    }

    /// The session's task ends when the last handle of the session's listener is dropped.
    #[test]
    fn dropping_the_listener_ends_its_session() {
        let listener = Listener::default();
        let ended = listener.0.ended.clone();
        let other = listener.clone();
        drop(listener);
        assert!(!ended.is_cancelled());
        drop(other);
        assert!(ended.is_cancelled());
    }
}
