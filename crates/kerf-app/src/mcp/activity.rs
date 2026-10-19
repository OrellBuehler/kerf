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
//! own, gathers what arrived in a short window (a job's progress ticks collapse to one), and sends.
//! A session too slow to keep up falls behind the channel's capacity and is told every resource it
//! subscribed changed (it re-reads), so it costs neither memory nor the engine anything. A session's
//! task ends with the session: its [`Listener`] is dropped with the last handle of the session's
//! server, which cancels the task.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

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
    uri.strip_prefix(JOB_URI_PREFIX)
        .filter(|id| !id.is_empty() && !id.contains('/'))
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

#[derive(Default)]
struct Shared {
    subscribed: Mutex<BTreeSet<String>>,
    started: AtomicBool,
    ended: CancellationToken,
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.ended.cancel();
    }
}

impl Listener {
    /// Start sending this session what it subscribes to, through `peer` (once; a second call does
    /// nothing).
    pub fn start(&self, peer: Peer<RoleServer>) {
        if self.0.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let rx = channel().subscribe();
        let shared = Arc::downgrade(&self.0);
        let ended = self.0.ended.clone();
        tauri::async_runtime::spawn(forward(rx, shared, ended, peer));
    }

    /// Subscribe to `uri` ([`known`] only).
    pub fn subscribe(&self, uri: &str) -> Result<(), McpError> {
        if !known(uri) {
            return Err(unknown_resource(uri));
        }
        self.subscriptions().insert(uri.to_string());
        Ok(())
    }

    pub fn unsubscribe(&self, uri: &str) {
        self.subscriptions().remove(uri);
    }

    fn subscriptions(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.0.subscribed.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A `resources/*` request naming no resource of this server.
pub fn unknown_resource(uri: &str) -> McpError {
    McpError::resource_not_found(
        format!("no resource {uri:?}: kerf://project, kerf://tasks, kerf://jobs and kerf://jobs/{{job_id}} exist"),
        None,
    )
}

/// The task of one session: batches in, a notification per subscribed resource that changed out.
async fn forward(mut rx: broadcast::Receiver<Event>, shared: Weak<Shared>, ended: CancellationToken, peer: Peer<RoleServer>) {
    let mut behind = false;
    while let Some(mut batch) = next_batch(&mut rx, &ended).await {
        batch.lagged |= std::mem::take(&mut behind);
        let subscribed = match shared.upgrade() {
            Some(shared) => shared.subscribed.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            None => return,
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
/// own sink, for the resources it accepted, until the request is cancelled.
pub async fn listen(subscribed: Vec<String>, sink: rmcp::service::SubscriptionSink, ended: CancellationToken) {
    let mut rx = channel().subscribe();
    while let Some(batch) = next_batch(&mut rx, &ended).await {
        for uri in batch.updated(&subscribed) {
            if sink.notify_resource_updated(uri).await.is_err() {
                return;
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
        let all = uris(&[PROJECT_URI, TASKS_URI, JOBS_URI, "kerf://jobs/job-1", "kerf://jobs/job-2"]);
        let b = batch(&[Event::Job("job-1".into()), Event::Job("job-1".into())]);
        assert_eq!(
            b.updated(&all),
            uris(&[JOBS_URI, "kerf://jobs/job-1"]),
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
        for uri in ["kerf://jobs/", "kerf://jobs/a/b", "kerf://timeline", "file:///etc/passwd", ""] {
            assert!(!known(uri), "{uri}");
        }
        let listener = Listener::default();
        assert!(listener.subscribe("kerf://nope").is_err());
        listener.subscribe(TASKS_URI).unwrap();
        listener.subscribe(TASKS_URI).unwrap();
        assert_eq!(listener.subscriptions().len(), 1);
        listener.unsubscribe(TASKS_URI);
        assert!(listener.subscriptions().is_empty());
        assert_eq!(resources().len(), 3);
        assert_eq!(resource_templates().len(), 1);
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
