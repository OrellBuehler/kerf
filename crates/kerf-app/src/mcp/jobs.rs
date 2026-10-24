//! Background jobs for the tools that take minutes (an export, an analysis, a voiceover, a model
//! download…).
//!
//! A client that cannot hold a request open that long — or would rather keep working while a
//! render runs — passes `background: true` and gets a `job_id` back at once; `job_status` reads
//! its progress and, when it ends, its result or error, `cancel_job` stops it and `list_jobs`
//! shows what is running. Without the flag a tool behaves as it always did: the call stays open,
//! progress rides the request's `progressToken` and cancelling the request cancels the work.
//!
//! Every such tool is written once, against [`Run`]: something to poll for "stop" and somewhere to
//! say how far along it is. A foreground call builds a `Run` from the request (its cancellation
//! token, its progress token); a background one from its [`Jobs`] record.
//!
//! The registry is bounded — a handful of jobs running at once, a few dozen records kept, a
//! finished one forgotten after an hour — so a client that starts jobs and never reads them
//! cannot grow it without limit.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::ProgressNotificationParam;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{schemars, tool, tool_router, ErrorData as McpError};
use serde::Serialize;

use super::{json, KerfMcp};

/// Jobs running at once. The machine does one heavy thing at a time anyway (`kerf_core::cpu`);
/// this only bounds the registry.
const MAX_RUNNING: usize = 8;
/// Records kept, running and finished together. The oldest finished one makes room.
const MAX_RECORDS: usize = 48;
/// How long a finished job can still be asked about.
const KEEP_FINISHED: Duration = Duration::from_secs(60 * 60);

/// What a long-running tool needs from whoever called it: whether to stop, and a way to say how
/// far along it is.
#[derive(Clone)]
pub struct Run {
    cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    report: Arc<dyn Fn(Option<f64>, String) + Send + Sync>,
}

impl Run {
    /// Whether the caller wants the work abandoned.
    pub fn cancelled(&self) -> bool {
        (self.cancel)()
    }

    /// Say how far along the work is (`0.0..=1.0` when it can tell) and what it is doing.
    pub fn progress(&self, fraction: Option<f64>, message: impl Into<String>) {
        (self.report)(fraction.map(|f| f.clamp(0.0, 1.0)), message.into());
    }

    /// A run that is also stopped by `also` — the GUI's own Stop flag, say.
    pub fn also_cancelled_by(self, also: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        let inner = self.cancel;
        Self {
            cancel: Arc::new(move || inner() || also()),
            report: self.report,
        }
    }

    /// A run nobody can cancel and nobody hears from.
    #[cfg(test)]
    pub fn quiet() -> Self {
        Self {
            cancel: Arc::new(|| false),
            report: Arc::new(|_, _| {}),
        }
    }

    #[cfg(test)]
    fn recording(cancelled: Arc<AtomicBool>, log: Arc<Mutex<ProgressLog>>) -> Self {
        Self {
            cancel: Arc::new(move || cancelled.load(Ordering::SeqCst)),
            report: Arc::new(move |f, m| log.lock().unwrap().push((f, m))),
        }
    }
}

/// What a test run recorded: each `(fraction, message)` it was told.
#[cfg(test)]
type ProgressLog = Vec<(Option<f64>, String)>;

/// Where a foreground call's progress goes: to the client, on the progress token it sent, if any.
pub struct Relay {
    task: tauri::async_runtime::JoinHandle<()>,
}

impl Relay {
    /// Wait until the last progress message has been sent. Call it once the work is over and every
    /// [`Run`] clone has been dropped.
    pub async fn finish(self) {
        let _ = self.task.await;
    }
}

/// The [`Run`] of a call that stays open: stopped by the request's cancellation, reporting on its
/// progress token. The forwarder drains the channel even when the client sent no token, so
/// ticks never pile up.
pub fn foreground(context: &RequestContext<RoleServer>) -> (Run, Relay) {
    let ct = context.ct.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(Option<f64>, String)>();
    let peer = context.peer.clone();
    let token = context.meta.get_progress_token();
    let task = tauri::async_runtime::spawn(async move {
        let mut last = 0.0f64;
        while let Some((fraction, message)) = rx.recv().await {
            let Some(token) = token.clone() else { continue };
            // Progress must not go backwards, and a step with no fraction repeats the last one.
            last = fraction.unwrap_or(last).max(last);
            let param = ProgressNotificationParam::new(token, last)
                .with_total(1.0)
                .with_message(message);
            let _ = peer.notify_progress(param).await;
        }
    });
    let run = Run {
        cancel: Arc::new(move || ct.is_cancelled()),
        report: Arc::new(move |fraction, message| {
            let _ = tx.send((fraction, message));
        }),
    };
    (run, Relay { task })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

struct Job {
    id: String,
    tool: &'static str,
    state: JobState,
    started: Instant,
    finished: Option<Instant>,
    progress: Option<f64>,
    message: Option<String>,
    cancel: Arc<AtomicBool>,
    result: Option<String>,
    error: Option<String>,
}

/// One job as a client reads it.
#[derive(Debug, Serialize, PartialEq)]
pub struct JobView {
    pub job_id: String,
    pub tool: &'static str,
    pub state: JobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub elapsed_secs: f64,
    /// What the tool returned, once `state` is `done`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Why it ended, once `state` is `failed` or `cancelled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct Jobs {
    jobs: Mutex<Vec<Job>>,
    max_running: usize,
    max_records: usize,
    keep_finished: Duration,
}

impl Jobs {
    fn new() -> Self {
        Self::with_limits(MAX_RUNNING, MAX_RECORDS, KEEP_FINISHED)
    }

    fn with_limits(max_running: usize, max_records: usize, keep_finished: Duration) -> Self {
        Self {
            jobs: Mutex::new(Vec::new()),
            max_running,
            max_records,
            keep_finished,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forget the jobs that finished too long ago, and the oldest finished ones beyond the cap.
    fn prune(jobs: &mut Vec<Job>, now: Instant, keep: Duration, max_records: usize) {
        jobs.retain(|j| j.finished.is_none_or(|at| now.duration_since(at) < keep));
        while jobs.len() >= max_records {
            match jobs.iter().position(|j| j.state != JobState::Running) {
                Some(oldest_finished) => {
                    jobs.remove(oldest_finished);
                }
                None => break,
            }
        }
    }

    /// Register a job for `tool` and hand back its id and the [`Run`] it works against.
    pub fn begin(self: &Arc<Self>, tool: &'static str) -> Result<(String, Run), McpError> {
        let mut jobs = self.lock();
        Self::prune(&mut jobs, Instant::now(), self.keep_finished, self.max_records);
        let running = jobs.iter().filter(|j| j.state == JobState::Running).count();
        if running >= self.max_running {
            return Err(McpError::invalid_params(
                format!(
                    "{running} background jobs are already running; wait for one to finish (list_jobs, job_status) or cancel one (cancel_job)"
                ),
                None,
            ));
        }
        let id = format!("job-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.push(Job {
            id: id.clone(),
            tool,
            state: JobState::Running,
            started: Instant::now(),
            finished: None,
            progress: None,
            message: None,
            cancel: cancel.clone(),
            result: None,
            error: None,
        });
        drop(jobs);
        super::activity::publish(super::activity::Event::Job(id.clone()));
        let registry = self.clone();
        let reporting = id.clone();
        let run = Run {
            cancel: Arc::new(move || cancel.load(Ordering::SeqCst)),
            report: Arc::new(move |fraction, message| registry.report(&reporting, fraction, message)),
        };
        Ok((id, run))
    }

    fn report(&self, id: &str, fraction: Option<f64>, message: String) {
        let found = match self.lock().iter_mut().find(|j| j.id == id) {
            Some(job) => {
                if let Some(f) = fraction {
                    job.progress = Some(f);
                }
                job.message = Some(message);
                true
            }
            None => false,
        };
        if found {
            super::activity::publish(super::activity::Event::Job(id.to_string()));
        }
    }

    /// Record how the job ended. A job that was asked to stop and did not succeed is `cancelled`.
    pub fn finish(&self, id: &str, outcome: Result<String, McpError>) {
        self.record_end(id, outcome);
        super::activity::publish(super::activity::Event::Job(id.to_string()));
    }

    fn record_end(&self, id: &str, outcome: Result<String, McpError>) {
        let mut jobs = self.lock();
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return;
        };
        job.finished = Some(Instant::now());
        match outcome {
            Ok(result) => {
                job.state = JobState::Done;
                job.progress = Some(1.0);
                job.result = Some(result);
            }
            Err(e) => {
                job.state = if job.cancel.load(Ordering::SeqCst) {
                    JobState::Cancelled
                } else {
                    JobState::Failed
                };
                job.error = Some(e.message.to_string());
            }
        }
    }

    fn view_of(job: &Job, with_result: bool) -> JobView {
        JobView {
            job_id: job.id.clone(),
            tool: job.tool,
            state: job.state,
            progress: job.progress,
            message: job.message.clone(),
            elapsed_secs: job
                .finished
                .unwrap_or_else(Instant::now)
                .duration_since(job.started)
                .as_secs_f64(),
            result: job
                .result
                .as_deref()
                .filter(|_| with_result)
                .map(|text| serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.to_string()))),
            error: job.error.clone(),
        }
    }

    pub fn view(&self, id: &str) -> Option<JobView> {
        self.lock().iter().find(|j| j.id == id).map(|j| Self::view_of(j, true))
    }

    /// Every job, oldest first, without their results (ask `job_status` for one).
    pub fn list(&self) -> Vec<JobView> {
        self.lock().iter().map(|j| Self::view_of(j, false)).collect()
    }

    /// Ask a running job to stop; whether it was running.
    pub fn cancel(&self, id: &str) -> Option<bool> {
        let jobs = self.lock();
        let job = jobs.iter().find(|j| j.id == id)?;
        let running = job.state == JobState::Running;
        if running {
            job.cancel.store(true, Ordering::SeqCst);
        }
        Some(running)
    }

    /// Ask every running job to stop.
    pub fn cancel_all(&self) {
        for job in self.lock().iter().filter(|j| j.state == JobState::Running) {
            job.cancel.store(true, Ordering::SeqCst);
        }
    }

    /// How many jobs are running.
    pub fn running(&self) -> usize {
        self.lock().iter().filter(|j| j.state == JobState::Running).count()
    }
}

/// The process-wide registry.
pub fn jobs() -> &'static Arc<Jobs> {
    static JOBS: OnceLock<Arc<Jobs>> = OnceLock::new();
    JOBS.get_or_init(|| Arc::new(Jobs::new()))
}

/// Start `work` as a job of `registry` and answer its id at once. The work runs as a task of its
/// own, and a second task records how it ended, so a panic in it ends the job as a failure instead
/// of leaving it `running` for good.
fn spawn_job<F, Fut>(registry: &Arc<Jobs>, tool: &'static str, work: F) -> Result<String, McpError>
where
    F: FnOnce(Run) -> Fut + Send + 'static,
    Fut: Future<Output = Result<String, McpError>> + Send + 'static,
{
    let (id, run) = registry.begin(tool)?;
    let (registry, started) = (registry.clone(), id.clone());
    let working = tauri::async_runtime::spawn(work(run));
    tauri::async_runtime::spawn(async move {
        let outcome = working
            .await
            .unwrap_or_else(|e| Err(McpError::internal_error(format!("the job stopped unexpectedly: {e}"), None)));
        registry.finish(&started, outcome);
    });
    Ok(id)
}

impl KerfMcp {
    /// Run `work` the way the caller asked: held open on the request (progress on its token,
    /// cancelled with it), or — `background` — as a job that answers `{job_id}` at once. The
    /// caller validates everything it can *before* this, so a bad argument is an error on the
    /// call, not a job that fails a moment later.
    pub(super) async fn long_tool<F, Fut>(
        &self,
        tool: &'static str,
        background: Option<bool>,
        context: &RequestContext<RoleServer>,
        work: F,
    ) -> Result<String, McpError>
    where
        F: FnOnce(Run) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, McpError>> + Send + 'static,
    {
        if background.unwrap_or(false) {
            let id = spawn_job(jobs(), tool, work)?;
            return json(&serde_json::json!({
                "job_id": id,
                "tool": tool,
                "state": JobState::Running,
                "next": format!(
                    "poll job_status with this job_id (cancel_job stops it), or subscribe to the resource \
                     kerf://jobs/{id} to be told when it moves and read it once right after subscribing \
                     (nothing before the subscription is replayed); the result arrives there"
                ),
            }));
        }
        let (run, relay) = foreground(context);
        let outcome = work(run).await;
        relay.finish().await;
        outcome
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub(super) struct JobIdParams {
    #[schemars(description = "The job_id a tool answered when it was called with background=true (see list_jobs)")]
    job_id: String,
}

#[tool_router(router = tool_router_jobs, vis = "pub(super)")]
impl KerfMcp {
    #[tool(
        description = "The state of a background job (one started with background=true): `running` with `progress` \
                       (0..1, when the tool can tell) and `message`, or `done` with its `result` (what the tool \
                       would have returned), or `failed` / `cancelled` with an `error`. Jobs are forgotten an hour \
                       after they end."
    )]
    fn job_status(&self, Parameters(p): Parameters<JobIdParams>) -> Result<String, McpError> {
        match jobs().view(&p.job_id) {
            Some(view) => json(&view),
            None => Err(unknown_job(&p.job_id)),
        }
    }

    #[tool(
        description = "Stop a running background job. The work stops at its next check (an export between ffmpeg \
                       progress ticks, an analysis within about a second) and the job ends `cancelled`; a job that \
                       has already finished is left as it is. Poll job_status to see it end."
    )]
    fn cancel_job(&self, Parameters(p): Parameters<JobIdParams>) -> Result<String, McpError> {
        match jobs().cancel(&p.job_id) {
            Some(was_running) => json(&serde_json::json!({
                "job_id": p.job_id,
                "cancel_requested": was_running,
                "note": if was_running { "stopping; poll job_status" } else { "it had already finished" },
            })),
            None => Err(unknown_job(&p.job_id)),
        }
    }

    #[tool(
        description = "List the background jobs — running ones and recently finished ones — with their state and \
                       progress (not their results; job_status has those)."
    )]
    fn list_jobs(&self) -> Result<String, McpError> {
        json(&jobs().list())
    }
}

fn unknown_job(id: &str) -> McpError {
    McpError::invalid_params(
        format!("no job {id:?}: it never existed, or it finished over an hour ago (list_jobs)"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(max_running: usize, max_records: usize) -> Arc<Jobs> {
        Arc::new(Jobs::with_limits(max_running, max_records, Duration::from_secs(3600)))
    }

    #[test]
    fn a_job_runs_reports_and_finishes_with_its_result() {
        let jobs = registry(4, 10);
        let (id, run) = jobs.begin("export").unwrap();
        assert!(id.starts_with("job-"));
        let view = jobs.view(&id).unwrap();
        assert_eq!(view.state, JobState::Running);
        assert_eq!(view.tool, "export");
        assert!(view.result.is_none() && view.error.is_none());

        run.progress(Some(0.25), "rendering");
        let view = jobs.view(&id).unwrap();
        assert_eq!(view.progress, Some(0.25));
        assert_eq!(view.message.as_deref(), Some("rendering"));
        run.progress(None, "still rendering");
        assert_eq!(jobs.view(&id).unwrap().progress, Some(0.25), "no fraction keeps the last");
        run.progress(Some(7.0), "clamped");
        assert_eq!(jobs.view(&id).unwrap().progress, Some(1.0));

        jobs.finish(&id, Ok(r#"{"output":"/tmp/a.mp4"}"#.to_string()));
        let view = jobs.view(&id).unwrap();
        assert_eq!(view.state, JobState::Done);
        assert_eq!(view.progress, Some(1.0));
        assert_eq!(
            view.result.unwrap()["output"],
            "/tmp/a.mp4",
            "the result comes back as JSON, not as a string"
        );
    }

    #[test]
    fn a_result_that_is_not_json_comes_back_as_text() {
        let jobs = registry(4, 10);
        let (id, _) = jobs.begin("download_speech_model").unwrap();
        jobs.finish(&id, Ok("Speech model ready at /x".to_string()));
        assert_eq!(
            jobs.view(&id).unwrap().result,
            Some(serde_json::Value::String("Speech model ready at /x".to_string()))
        );
    }

    #[test]
    fn a_failed_job_keeps_its_error_and_a_stopped_one_is_cancelled() {
        let jobs = registry(4, 10);
        let (failed, _) = jobs.begin("analyze_asset").unwrap();
        jobs.finish(&failed, Err(McpError::internal_error("boom", None)));
        let view = jobs.view(&failed).unwrap();
        assert_eq!((view.state, view.error.as_deref()), (JobState::Failed, Some("boom")));

        let (stopped, run) = jobs.begin("export").unwrap();
        assert!(!run.cancelled());
        assert_eq!(jobs.cancel(&stopped), Some(true));
        assert!(run.cancelled(), "the work sees the request to stop");
        jobs.finish(&stopped, Err(McpError::internal_error("export cancelled", None)));
        assert_eq!(jobs.view(&stopped).unwrap().state, JobState::Cancelled);
        assert_eq!(jobs.cancel(&stopped), Some(false), "a finished job has nothing to stop");
        assert_eq!(jobs.cancel("job-nope"), None);

        // A job asked to stop that finished anyway is still done.
        let (late, _) = jobs.begin("export").unwrap();
        jobs.cancel(&late);
        jobs.finish(&late, Ok("{}".to_string()));
        assert_eq!(jobs.view(&late).unwrap().state, JobState::Done);
    }

    #[test]
    fn cancel_all_reaches_every_running_job() {
        let jobs = registry(4, 10);
        let (_a, a) = jobs.begin("export").unwrap();
        let (_b, b) = jobs.begin("analyze_asset").unwrap();
        let (done, c) = jobs.begin("export").unwrap();
        jobs.finish(&done, Ok("{}".to_string()));
        assert_eq!(jobs.running(), 2);
        jobs.cancel_all();
        assert!(a.cancelled() && b.cancelled());
        assert!(!c.cancelled(), "a finished job is left alone");
    }

    #[test]
    fn only_so_many_jobs_run_at_once() {
        let jobs = registry(2, 10);
        let (a, _) = jobs.begin("export").unwrap();
        let _b = jobs.begin("export").unwrap();
        let err = jobs.begin("export").err().expect("the third is refused");
        assert!(err.message.contains("already running"), "{}", err.message);
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        jobs.finish(&a, Ok("{}".to_string()));
        assert!(jobs.begin("export").is_ok(), "a slot freed up");
    }

    #[test]
    fn the_registry_forgets_the_oldest_finished_job_and_never_a_running_one() {
        let jobs = registry(10, 3);
        let (first, _) = jobs.begin("a").unwrap();
        let (second, _) = jobs.begin("b").unwrap();
        let (third, _) = jobs.begin("c").unwrap();
        jobs.finish(&second, Ok("{}".to_string()));
        let (fourth, _) = jobs.begin("d").unwrap();
        assert!(jobs.view(&second).is_none(), "the oldest finished one made room");
        for running in [&first, &third, &fourth] {
            assert_eq!(jobs.view(running).unwrap().state, JobState::Running);
        }
        assert_eq!(jobs.list().len(), 3);
    }

    #[test]
    fn a_finished_job_expires() {
        let jobs = registry(10, 10);
        let (id, _) = jobs.begin("a").unwrap();
        jobs.finish(&id, Ok("{}".to_string()));
        {
            let mut all = jobs.lock();
            let later = Instant::now() + Duration::from_secs(2 * 3600);
            Jobs::prune(&mut all, later, Duration::from_secs(3600), 10);
        }
        assert!(jobs.view(&id).is_none());
    }

    #[test]
    fn the_list_leaves_out_results() {
        let jobs = registry(10, 10);
        let (id, _) = jobs.begin("a").unwrap();
        jobs.finish(&id, Ok(r#"{"big":"result"}"#.to_string()));
        let listed = jobs.list();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].result.is_none());
        assert!(jobs.view(&id).unwrap().result.is_some());
    }

    #[test]
    fn a_run_is_cancelled_by_either_flag() {
        let mine = Arc::new(AtomicBool::new(false));
        let gui = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let flag = gui.clone();
        let run = Run::recording(mine.clone(), log.clone()).also_cancelled_by(move || flag.load(Ordering::SeqCst));
        assert!(!run.cancelled());
        gui.store(true, Ordering::SeqCst);
        assert!(run.cancelled(), "the GUI's Stop reaches the run");
        gui.store(false, Ordering::SeqCst);
        mine.store(true, Ordering::SeqCst);
        assert!(run.cancelled());
        run.progress(Some(0.5), "half");
        assert_eq!(log.lock().unwrap().as_slice(), &[(Some(0.5), "half".to_string())]);
        assert!(!Run::quiet().cancelled());
    }

    async fn ended(registry: &Arc<Jobs>, id: &str) -> JobView {
        for _ in 0..200 {
            let view = registry.view(id).unwrap();
            if view.state != JobState::Running {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("job {id} never ended");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_spawned_job_reports_and_ends_with_its_result() {
        let registry = registry(4, 10);
        let id = spawn_job(&registry, "export", |run| async move {
            run.progress(Some(0.5), "halfway");
            Ok(r#"{"output":"/x.mp4"}"#.to_string())
        })
        .unwrap();
        let view = ended(&registry, &id).await;
        assert_eq!(view.state, JobState::Done);
        assert_eq!(view.result.unwrap()["output"], "/x.mp4");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_job_that_panics_ends_failed_not_running_for_good() {
        let registry = registry(4, 10);
        let id = spawn_job(&registry, "analyze_asset", |_run| async move {
            if true {
                panic!("boom");
            }
            Ok(String::new())
        })
        .unwrap();
        let view = ended(&registry, &id).await;
        assert_eq!(view.state, JobState::Failed);
        assert!(view.error.unwrap().contains("stopped unexpectedly"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelling_a_job_reaches_the_work_and_ends_it_cancelled() {
        let registry = registry(4, 10);
        let id = spawn_job(&registry, "export", |run| async move {
            while !run.cancelled() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(McpError::internal_error("export cancelled", None))
        })
        .unwrap();
        assert_eq!(registry.view(&id).unwrap().state, JobState::Running);
        assert_eq!(registry.cancel(&id), Some(true));
        let view = ended(&registry, &id).await;
        assert_eq!(view.state, JobState::Cancelled);
        assert_eq!(view.error.as_deref(), Some("export cancelled"));
    }
}
