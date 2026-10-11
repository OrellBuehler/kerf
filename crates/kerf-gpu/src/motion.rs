//! The frames of a [`PlanMode::Motion`](kerf_core::PlanMode) plan: one [`FrameCursor`] per clip.
//!
//! A still plan's layers are served by the [`FrameSource`]'s cache (`Pick::AtOrAfter`, the frame
//! `-ss` lands on). An export frame's layers pick with the `fps` filter ([`Pick::Fps`]), which
//! depends on the frames *around* the one shown and so on a clip's whole run: a [`FrameCursor`]
//! reads that run forward, once, and answers the pick of each output frame in order. This is the
//! per-clip bookkeeping around it — open a cursor the first time a clip is on screen, hand it the
//! pick of every later frame, drop it when the clip is gone — which is what an export (A7) and a
//! playback loop (A4) do, and what the Motion parity harness (`tests/motion_parity.rs`) runs.
//!
//! **Forward only**, like the cursors: frames must be asked for in ascending order per clip. A plan
//! earlier than the last one a clip was asked for (a seek) opens that clip's cursor afresh at the
//! clip's own start (a cursor decodes from there, as the export does) and reads forward to the frame.

use std::collections::HashMap;
use std::sync::Arc;

use kerf_core::{Pick, PlanLayer, RenderPlan};
use uuid::Uuid;

use crate::cursor::{CursorConfig, FrameCursor};
use crate::frame_source::FrameSource;
use crate::gpu::GpuError;
use crate::source::YuvFrame;

struct Open {
    cursor: FrameCursor,
    /// The output frame of the last pick it answered.
    at: u64,
}

/// See the [module](self).
pub struct MotionFrames {
    source: Arc<FrameSource>,
    config: CursorConfig,
    open: HashMap<Uuid, Open>,
}

impl MotionFrames {
    /// Cursors over what `source` trusts decode runs for, with the default limits.
    pub fn new(source: Arc<FrameSource>) -> Self {
        Self::with_config(source, CursorConfig::default())
    }

    pub fn with_config(source: Arc<FrameSource>, config: CursorConfig) -> Self {
        Self {
            source,
            config,
            open: HashMap::new(),
        }
    }

    /// The picture of every layer of `plan`, in layer order: `None` where the pick names no frame
    /// (FFmpeg draws nothing there). An error is for that frame only — `Unsupported` for a file the
    /// runs are not trusted for, a still plan's layer (its pick is not the export's) — and the caller
    /// renders it through FFmpeg. A clip that is not on this plan has its cursor dropped (its
    /// `ffmpeg` killed).
    pub fn frames(&mut self, plan: &RenderPlan) -> Result<Vec<Option<Arc<YuvFrame>>>, GpuError> {
        let live: Vec<Uuid> = plan.layers.iter().map(|l| l.clip_id).collect();
        self.open.retain(|id, _| live.contains(id));
        plan.layers.iter().map(|layer| self.frame(layer)).collect()
    }

    fn frame(&mut self, layer: &PlanLayer) -> Result<Option<Arc<YuvFrame>>, GpuError> {
        let Pick::Fps(pick) = layer.pick else {
            return Err(GpuError::Unsupported(format!(
                "{}: a still plan's pick is the still's `-ss`, not the export's `fps` filter",
                layer.name
            )));
        };
        // A frame earlier than one already answered: the cursor only reads forward.
        if self.open.get(&layer.clip_id).is_some_and(|o| pick.frame < o.at) {
            self.open.remove(&layer.clip_id);
        }
        if !self.open.contains_key(&layer.clip_id) {
            let cursor = self.source.cursor(layer, self.config.clone())?;
            self.open.insert(layer.clip_id, Open { cursor, at: 0 });
        }
        let Some(open) = self.open.get_mut(&layer.clip_id) else {
            return Err(GpuError::Gpu("a cursor that was just opened is gone".into()));
        };
        open.at = pick.frame;
        open.cursor.pick(&layer.pick)
    }

    /// Cursors alive (their `ffmpeg` runs).
    pub fn cursors(&self) -> usize {
        self.open.len()
    }
}
