//! What a preview frame is planned from, kept between frames.
//!
//! Resolving the inputs of one frame took the project lock, parsed the working timeline's JSON,
//! read every asset and asked the disk whether each had a proxy — per frame, on every playhead
//! move of a drag. The inputs only change when the project or the proxies do, so they are held as
//! an `Arc` snapshot ([`PlanSnapshots`]) and a frame takes none of that: it compares two numbers.
//!
//! A snapshot is **fresh** while the process-wide plan epoch ([`kerf_core::plan_epoch`]) is the one
//! it was taken at (every row written to the project, an opened project, a proxy landing or
//! deleted, a proxy-size or preview-source change move it, so the GUI's own edits and an agent's
//! alike) and for at most [`MAX_AGE`]: the age bound is for what no code path knows about (a proxy
//! file deleted by hand), not for anything that is expected to happen. The epoch is read **under
//! the project lock, before the project is read**, so a snapshot is never newer than its tag.
//!
//! The snapshot is of `working_timeline()` as the GUI sees it (the user's view: staged agent edits
//! are not in it until they are accepted, which writes the timeline and moves the epoch), exactly
//! what the per-frame resolution read before.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use kerf_core::{Asset, Error, Project, ProxyInput, Timeline};
use uuid::Uuid;

/// The longest a snapshot is served without being taken again, whatever the epoch says.
pub const MAX_AGE: Duration = Duration::from_secs(2);

/// What needs the project lock: the working timeline and every asset with its proxy input. Read in
/// one pass under the lock ([`Taken::from`]), resolved without it ([`Taken::resolve`]).
pub struct Taken {
    epoch: u64,
    timeline: Timeline,
    inputs: Vec<ProxyInput>,
}

impl Taken {
    /// Take the inputs. Call under the project lock; nothing slow happens here.
    pub fn from(project: &Project) -> Result<Self, String> {
        // First: a write after this is a change this snapshot may not have seen.
        Self::tagged(project, kerf_core::plan_epoch())
    }

    /// [`Taken::from`] tagged with `epoch`, which the caller read first (a test's own).
    pub fn tagged(project: &Project, epoch: u64) -> Result<Self, String> {
        let timeline = project.working_timeline().map_err(|e| e.to_string())?;
        let inputs = project.proxy_inputs().map_err(|e| e.to_string())?;
        Ok(Self { epoch, timeline, inputs })
    }

    /// The rest, off the lock: which file each asset previews from.
    pub fn resolve(self) -> PlanInputs {
        let assets: Vec<Asset> = self.inputs.iter().map(|i| i.asset.clone()).collect();
        let preview_assets = Project::preview_assets_of(assets.clone());
        PlanInputs {
            epoch: self.epoch,
            taken_at: Instant::now(),
            timeline: self.timeline,
            assets,
            preview_assets,
            proxy_inputs: self.inputs.into_iter().map(|i| (i.asset.id, i)).collect(),
        }
    }
}

/// Everything a frame is planned from, shared by every frame until something changes.
pub struct PlanInputs {
    epoch: u64,
    taken_at: Instant,
    pub(super) timeline: Timeline,
    /// The assets as imported: the plan resolves the proxies itself.
    pub(super) assets: Vec<Asset>,
    /// The proxy-swapped assets the FFmpeg preview graph reads.
    pub(super) preview_assets: Vec<Asset>,
    proxy_inputs: HashMap<Uuid, ProxyInput>,
}

impl PlanInputs {
    /// Take and resolve in one go (the project lock is the caller's, held across both).
    #[cfg(test)]
    pub fn of(project: &Project) -> Result<Self, String> {
        Taken::from(project).map(Taken::resolve)
    }

    /// The plan epoch these inputs were taken at: a session planned from them is stale once
    /// [`kerf_core::plan_epoch`] has moved.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Under **Proxy only**, what the preview says about a frame at `t` whose clips have no proxy
    /// yet (`None`: nothing to wait for, and always `None` under any other source). Answered from
    /// the snapshot: the status is read from the proxy queue now, the project not at all.
    pub fn proxy_wait(&self, t: f64) -> Result<Option<String>, String> {
        self.waits(&self.timeline, t, Some(t))
    }

    /// [`PlanInputs::proxy_wait`] for `timeline` in place of the snapshot's (a drag's draft of it, which
    /// reads the same assets).
    pub fn proxy_wait_in(&self, timeline: &Timeline, t: f64) -> Result<Option<String>, String> {
        self.waits(timeline, t, Some(t))
    }

    /// [`PlanInputs::proxy_wait`] for playing from `t` to the end of the cut: the stream refuses
    /// the same way, since a clip without a proxy would be read from its original.
    pub fn proxy_wait_from(&self, t: f64) -> Result<Option<String>, String> {
        self.waits(&self.timeline, t, None)
    }

    fn waits(&self, timeline: &Timeline, from: f64, to: Option<f64>) -> Result<Option<String>, String> {
        let waits = Project::proxy_waits_in(timeline, kerf_core::proxy::effective_preview_source(), from, to, |id| {
            self.proxy_inputs.get(&id).cloned().ok_or(Error::AssetNotFound(id))
        })
        .map_err(|e| e.to_string())?;
        Ok(kerf_core::proxy::waits_message(&waits))
    }
}

/// The inputs of the frames being drawn, and how often they had to be taken again.
pub struct PlanSnapshots {
    held: Mutex<Option<Arc<PlanInputs>>>,
    max_age: Duration,
    epoch: fn() -> u64,
    served: AtomicU64,
    taken: AtomicU64,
}

/// What the snapshot cache did, for the diagnostics.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct SnapshotStats {
    /// Frames that were planned from the held snapshot.
    pub served: u64,
    /// Times the inputs were taken from the project.
    pub taken: u64,
}

impl Default for PlanSnapshots {
    fn default() -> Self {
        Self::new(MAX_AGE, kerf_core::plan_epoch)
    }
}

impl PlanSnapshots {
    pub fn new(max_age: Duration, epoch: fn() -> u64) -> Self {
        Self {
            held: Mutex::new(None),
            max_age,
            epoch,
            served: AtomicU64::new(0),
            taken: AtomicU64::new(0),
        }
    }

    /// The held snapshot if it is still the inputs in force at `now`.
    pub fn fresh(&self, now: Instant) -> Option<Arc<PlanInputs>> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
        let fresh = held.epoch == (self.epoch)() && now.saturating_duration_since(held.taken_at) < self.max_age;
        if fresh {
            self.served.fetch_add(1, Ordering::Relaxed);
        }
        fresh.then_some(held)
    }

    /// Hold `inputs` for the frames that follow.
    pub fn keep(&self, inputs: PlanInputs) -> Arc<PlanInputs> {
        let inputs = Arc::new(inputs);
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&inputs));
        self.taken.fetch_add(1, Ordering::Relaxed);
        inputs
    }

    /// Let go of the snapshot (the next frame takes the inputs again).
    pub fn forget(&self) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    #[cfg(test)]
    pub fn stats(&self) -> SnapshotStats {
        SnapshotStats {
            served: self.served.load(Ordering::Relaxed),
            taken: self.taken.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn inputs_at(epoch: u64, taken_at: Instant) -> PlanInputs {
        PlanInputs {
            epoch,
            taken_at,
            timeline: Timeline::new(),
            assets: Vec::new(),
            preview_assets: Vec::new(),
            proxy_inputs: HashMap::new(),
        }
    }

    #[test]
    fn a_snapshot_serves_every_frame_until_the_epoch_moves_or_it_gets_old() {
        static EPOCH: AtomicU64 = AtomicU64::new(1);
        fn epoch() -> u64 {
            EPOCH.load(Ordering::SeqCst)
        }
        // Its own epoch, so the other tests in this process (which bump the real one) cannot move it.
        EPOCH.store(10, Ordering::SeqCst);
        let snapshots = PlanSnapshots::new(Duration::from_secs(2), epoch);
        let t0 = Instant::now();
        assert!(snapshots.fresh(t0).is_none(), "nothing held yet");

        let kept = snapshots.keep(inputs_at(10, t0));
        for n in 0..100 {
            let got = snapshots.fresh(t0 + Duration::from_millis(n)).expect("fresh");
            assert!(Arc::ptr_eq(&got, &kept), "every frame shares the one snapshot");
        }
        assert_eq!(snapshots.stats(), SnapshotStats { served: 100, taken: 1 });

        // Old.
        assert!(snapshots.fresh(t0 + Duration::from_millis(2000)).is_none());
        let again = snapshots.keep(inputs_at(10, t0 + Duration::from_secs(5)));
        assert!(Arc::ptr_eq(&snapshots.fresh(t0 + Duration::from_secs(5)).unwrap(), &again));

        // An edit, a proxy, a new project: the epoch moved.
        EPOCH.store(11, Ordering::SeqCst);
        assert!(snapshots.fresh(t0 + Duration::from_secs(5)).is_none());
        // A snapshot tagged with the epoch it was taken at is fresh again.
        snapshots.keep(inputs_at(11, t0 + Duration::from_secs(5)));
        assert!(snapshots.fresh(t0 + Duration::from_secs(5)).is_some());
        // And forgetting drops it.
        snapshots.forget();
        assert!(snapshots.fresh(t0 + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn a_snapshot_tagged_before_a_change_is_never_fresh_after_it() {
        static EPOCH: AtomicU64 = AtomicU64::new(1);
        fn epoch() -> u64 {
            EPOCH.load(Ordering::SeqCst)
        }
        EPOCH.store(20, Ordering::SeqCst);
        let snapshots = PlanSnapshots::new(Duration::from_secs(60), epoch);
        let now = Instant::now();
        // The epoch was read, then a writer bumped it, then the snapshot was stored.
        snapshots.keep(inputs_at(20, now));
        EPOCH.store(21, Ordering::SeqCst);
        assert!(snapshots.fresh(now).is_none());
    }

    #[test]
    fn the_snapshot_is_the_working_timeline_and_follows_the_projects_edits_through_the_real_epoch() {
        let project = Project::sample().expect("the sample project");
        let snapshots = PlanSnapshots::default();
        let first = snapshots.keep(PlanInputs::of(&project).expect("inputs"));
        assert_eq!(
            serde_json::to_string(&first.timeline).unwrap(),
            serde_json::to_string(&project.working_timeline().unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&first.assets).unwrap(),
            serde_json::to_string(&project.list_assets().unwrap()).unwrap()
        );
        assert_eq!(first.assets.len(), first.preview_assets.len());
        assert_eq!(first.proxy_inputs.len(), first.assets.len());

        // An edit through the project (any write moves the epoch)...
        let clip = first.timeline.tracks.iter().flat_map(|t| &t.clips).next().expect("a clip").id;
        project.set_clip_enabled(clip, false).expect("an edit");
        assert!(snapshots.fresh(Instant::now()).is_none(), "the edit made the snapshot stale");
        // ...and the next snapshot has it.
        let second = snapshots.keep(PlanInputs::of(&project).expect("inputs"));
        let disabled = second
            .timeline
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == clip)
            .expect("the clip");
        assert!(!disabled.enabled);
        assert_ne!(
            serde_json::to_string(&first.timeline).unwrap(),
            serde_json::to_string(&second.timeline).unwrap()
        );
    }

    #[test]
    fn nothing_waits_for_a_proxy_unless_the_preview_source_is_proxy_only() {
        let project = Project::sample().expect("the sample project");
        let inputs = PlanInputs::of(&project).expect("inputs");
        // The process default is Auto: no waiting, with any clip anywhere.
        assert_eq!(inputs.proxy_wait(0.5).unwrap(), None);
        assert_eq!(inputs.proxy_wait(1e9).unwrap(), None);
    }
}
