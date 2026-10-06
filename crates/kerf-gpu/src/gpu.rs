//! The headless device: an instance, an adapter, a device and its queue — no
//! window, no surface.
//!
//! Tests and CI run on a **software adapter** (Mesa lavapipe on Linux, WARP on
//! Windows), and the app must keep working with no usable GPU at all, so asking
//! for an adapter is a fallible, explicit thing: [`Gpu::new`] returns an error
//! rather than panicking when there is none, and [`GpuOptions`] can insist on
//! the software one.

use std::sync::Arc;

/// What can go wrong between "I want a frame" and "here are its pixels".
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// No adapter satisfied the request (no driver, no software rasterizer, or
    /// the software one was requested on a platform that has none).
    #[error("no usable GPU adapter: {0}")]
    NoAdapter(String),
    /// The adapter would not give a device.
    #[error("could not create a GPU device: {0}")]
    Device(String),
    /// Getting the source picture out of FFmpeg failed.
    #[error("could not decode a source frame: {0}")]
    Decode(String),
    /// The plan (or a layer in it) is something the compositor does not render;
    /// the caller should use the FFmpeg path.
    #[error("not rendered on the GPU: {0}")]
    Unsupported(String),
    /// The finished frame could not be copied back.
    #[error("could not read the frame back: {0}")]
    Readback(String),
}

/// How to pick an adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuOptions {
    /// Take only a software ("fallback") adapter — lavapipe / llvmpipe, WARP.
    /// What the parity harness uses, so its numbers do not depend on which GPU
    /// (or driver) the machine happens to have.
    pub force_fallback_adapter: bool,
}

impl GpuOptions {
    /// The software adapter, unless `KERF_GPU_ADAPTER` says `hardware`
    /// (use the machine's own GPU) or `auto` (whatever wgpu prefers). macOS has no
    /// software adapter — Metal is the only one — so run the tests there with
    /// `KERF_GPU_ADAPTER=hardware`.
    pub fn for_tests() -> Self {
        match std::env::var("KERF_GPU_ADAPTER").as_deref() {
            Ok("hardware") | Ok("auto") => Self::default(),
            _ => Self {
                force_fallback_adapter: true,
            },
        }
    }
}

/// A device and queue, shared between compositors.
pub struct Gpu {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
}

impl Gpu {
    /// Open a device, blocking the calling thread until the adapter answers.
    pub fn new(options: GpuOptions) -> Result<Arc<Gpu>, GpuError> {
        pollster::block_on(Self::new_async(options))
    }

    async fn new_async(options: GpuOptions) -> Result<Arc<Gpu>, GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: options.force_fallback_adapter,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| GpuError::NoAdapter(e.to_string()))?;
        let info = adapter.get_info();
        // Defaults, not the adapter's maximums: a compositor that needs only the
        // baseline limits (and no optional feature) runs on every adapter, which
        // is what makes the software one a faithful test target.
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kerf-gpu"),
                ..Default::default()
            })
            .await
            .map_err(|e| GpuError::Device(e.to_string()))?;
        tracing::info!(
            adapter = %info.name,
            backend = ?info.backend,
            device_type = ?info.device_type,
            "kerf-gpu device ready"
        );
        Ok(Arc::new(Gpu { device, queue, info }))
    }

    /// The adapter this device is on.
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.info
    }

    /// Whether the adapter is a CPU rasterizer (lavapipe, WARP, ...).
    pub fn is_software(&self) -> bool {
        self.info.device_type == wgpu::DeviceType::Cpu
    }
}
