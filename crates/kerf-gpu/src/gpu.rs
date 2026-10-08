//! The headless device: an instance, an adapter, a device and its queue — no
//! window, no surface.
//!
//! Tests and CI run on a **software adapter** (Mesa lavapipe on Linux, WARP on
//! Windows), and the app must keep working with no usable GPU at all, so asking
//! for an adapter is a fallible, explicit thing: [`Gpu::new`] returns an error
//! rather than panicking when there is none, and [`GpuOptions`] can insist on
//! the software one.
//!
//! wgpu reports a bad call (an over-large texture, a validation failure, an
//! out-of-memory) through an error handler whose default is to **panic**. Nothing
//! here may panic on a GPU failure — the caller's answer to one is "render the
//! frame through FFmpeg" — so every unit of GPU work runs inside error scopes
//! ([`Gpu::guarded`]), anything uncaptured is logged and remembered, and a lost
//! device is tracked: once it is lost every later call returns
//! [`GpuError::DeviceLost`], and the owner builds a new [`Gpu`] (and a new
//! compositor on it).

use std::sync::{Arc, Mutex, PoisonError};

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
    /// The device is gone (a driver reset, a removed GPU, a destroyed device).
    /// Every later call fails the same way; create a new [`Gpu`].
    #[error("the GPU device was lost: {0}")]
    DeviceLost(String),
    /// The GPU ran out of memory (a 4K frame is 33 MB, a layer several textures).
    #[error("the GPU ran out of memory")]
    OutOfMemory,
    /// wgpu reported a validation or internal error for the work submitted.
    #[error("the GPU rejected the work: {0}")]
    Gpu(String),
    /// Getting the source picture out of FFmpeg failed.
    #[error("could not decode a source frame: {0}")]
    Decode(String),
    /// The plan (or a layer in it) is something the compositor does not render;
    /// the caller should use the FFmpeg path.
    #[error("not rendered on the GPU: {0}")]
    Unsupported(String),
    /// The frame source cannot keep up (it keeps restarting decodes): the caller renders this
    /// frame, or the span, through FFmpeg's own stream.
    #[error("the frame source is busy: {0}")]
    Busy(String),
    /// The finished frame could not be copied back.
    #[error("could not read the frame back: {0}")]
    Readback(String),
    /// A window surface could not be created, configured or drawn to (an unsupported
    /// window system, a surface the adapter cannot present to, a window that is
    /// occluded or gone). The device itself is fine: the caller shows the frame through
    /// FFmpeg and may build a new surface.
    #[error("the presentation surface is not usable: {0}")]
    Surface(String),
}

impl From<crate::router::Busy> for GpuError {
    fn from(busy: crate::router::Busy) -> Self {
        GpuError::Busy(busy.to_string())
    }
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

/// What the device's callbacks learned, shared with them.
#[derive(Default)]
struct Health {
    /// Set once, by the device-lost callback.
    lost: Mutex<Option<String>>,
    /// The last error nobody was scoped to catch.
    uncaptured: Mutex<Option<String>>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A device and queue, shared between compositors.
pub struct Gpu {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    /// Kept so a surface can be created on the instance the device came from and asked
    /// what the adapter can present (a surface belongs to one instance).
    instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    info: wgpu::AdapterInfo,
    health: Arc<Health>,
}

impl Gpu {
    /// Open a device, blocking the calling thread until the adapter answers.
    pub fn new(options: GpuOptions) -> Result<Arc<Gpu>, GpuError> {
        pollster::block_on(Self::new_async(options, None)).map(|(gpu, _)| gpu)
    }

    /// Open a device on an adapter that can present to `target` (a window, in the app),
    /// and the surface made from it, blocking the calling thread until the adapter answers.
    ///
    /// The surface is created first and the adapter asked to be compatible with it: on a
    /// machine with an integrated and a discrete GPU, the one that drives the window is the
    /// one that can show it. It is the caller's [`crate::Presenter`] to configure.
    ///
    /// A surface that cannot be created is a [`GpuError::Surface`] — an unsupported window
    /// system (a Wayland handle on a build without it, a handle kind no backend takes) or a
    /// window that is already gone — and never a panic; the caller keeps its FFmpeg preview.
    pub fn new_for_surface(
        options: GpuOptions,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
    ) -> Result<(Arc<Gpu>, wgpu::Surface<'static>), GpuError> {
        let target = target.into();
        pollster::block_on(Self::new_async(options, Some(target))).and_then(|(gpu, surface)| {
            surface
                .map(|s| (gpu, s))
                .ok_or_else(|| GpuError::Surface("no surface was created".into()))
        })
    }

    async fn new_async(
        options: GpuOptions,
        target: Option<wgpu::SurfaceTarget<'static>>,
    ) -> Result<(Arc<Gpu>, Option<wgpu::Surface<'static>>), GpuError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = match target {
            Some(target) => Some(
                instance
                    .create_surface(target)
                    .map_err(|e| GpuError::Surface(e.to_string()))?,
            ),
            None => None,
        };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: options.force_fallback_adapter,
                compatible_surface: surface.as_ref(),
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

        let health = Arc::new(Health::default());
        let h = Arc::clone(&health);
        device.set_device_lost_callback(move |reason, message| {
            tracing::error!(?reason, %message, "kerf-gpu device lost");
            *locked(&h.lost) = Some(format!("{reason:?}: {message}"));
        });
        let h = Arc::clone(&health);
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            // The default handler panics; this one keeps the process alive and
            // leaves the evidence for the next `guarded` call to report.
            tracing::error!(%error, "kerf-gpu uncaptured error");
            *locked(&h.uncaptured) = Some(error.to_string());
        }));

        tracing::info!(
            adapter = %info.name,
            backend = ?info.backend,
            device_type = ?info.device_type,
            "kerf-gpu device ready"
        );
        let gpu = Arc::new(Gpu {
            device,
            queue,
            instance,
            adapter,
            info,
            health,
        });
        Ok((gpu, surface))
    }

    /// A surface for another window on this device's instance (the first one came with
    /// [`Gpu::new_for_surface`]). Whether the adapter can present to it is the
    /// [`crate::Presenter`]'s to find out.
    pub fn create_surface(&self, target: impl Into<wgpu::SurfaceTarget<'static>>) -> Result<wgpu::Surface<'static>, GpuError> {
        self.check_alive()?;
        self.instance
            .create_surface(target)
            .map_err(|e| GpuError::Surface(e.to_string()))
    }

    /// The adapter this device is on.
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.info
    }

    /// Whether the adapter is a CPU rasterizer (lavapipe, WARP, ...).
    pub fn is_software(&self) -> bool {
        self.info.device_type == wgpu::DeviceType::Cpu
    }

    /// Why the device is gone, once it is. A lost device does not come back: the
    /// owner creates a new [`Gpu`].
    pub fn lost(&self) -> Option<String> {
        locked(&self.health.lost).clone()
    }

    /// The last error wgpu raised that no error scope was there to catch.
    pub fn last_uncaptured_error(&self) -> Option<String> {
        locked(&self.health.uncaptured).clone()
    }

    /// Destroy the device now (a controlled shutdown, and the way to rehearse a
    /// loss): every later call fails with [`GpuError::DeviceLost`]. wgpu reports a
    /// destroy through the same callback as a driver reset, once polled.
    pub fn destroy(&self) {
        self.device.destroy();
        // The callback fires from the device's maintenance; give it the chance.
        let _ = self.device.poll(wgpu::PollType::Poll);
    }

    /// `Err(DeviceLost)` once the device is lost.
    pub(crate) fn check_alive(&self) -> Result<(), GpuError> {
        match self.lost() {
            Some(why) => Err(GpuError::DeviceLost(why)),
            None => Ok(()),
        }
    }

    /// Run `work` — which creates resources, records and submits commands and
    /// reads results back — inside out-of-memory, validation and internal error
    /// scopes, and turn whatever they caught (or a device loss) into a
    /// [`GpuError`] instead of the panic wgpu defaults to. Call it, and let
    /// `work` run, on one thread: wgpu's scope stack is per thread.
    pub(crate) fn guarded<T>(&self, what: &str, work: impl FnOnce() -> Result<T, GpuError>) -> Result<T, GpuError> {
        self.check_alive()?;
        let oom = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let internal = self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        let result = work();
        // Innermost first.
        let internal = pollster::block_on(internal.pop());
        let validation = pollster::block_on(validation.pop());
        let oom = pollster::block_on(oom.pop());
        self.check_alive()?;
        if oom.is_some() {
            return Err(GpuError::OutOfMemory);
        }
        if let Some(e) = validation.or(internal) {
            return Err(GpuError::Gpu(format!("{what}: {e}")));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bad_texture(gpu: &Gpu) {
        // A zero-width texture is a validation error.
        let _ = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("zero wide"),
            size: wgpu::Extent3d {
                width: 0,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
    }

    /// A window no native backend can draw to: a browser canvas handle.
    struct NoWindow;

    impl wgpu::rwh::HasWindowHandle for NoWindow {
        fn window_handle(&self) -> Result<wgpu::rwh::WindowHandle<'_>, wgpu::rwh::HandleError> {
            let raw = wgpu::rwh::RawWindowHandle::Web(wgpu::rwh::WebWindowHandle::new(1));
            // SAFETY: the handle names nothing at all; no backend dereferences a web handle.
            Ok(unsafe { wgpu::rwh::WindowHandle::borrow_raw(raw) })
        }
    }

    impl wgpu::rwh::HasDisplayHandle for NoWindow {
        fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
            Ok(wgpu::rwh::DisplayHandle::web())
        }
    }

    #[test]
    fn a_window_no_backend_can_draw_to_is_an_error_and_not_a_panic() {
        // Needs no adapter: the surface is refused before one is asked for, with or without a
        // Vulkan loader on the machine.
        let err = Gpu::new_for_surface(GpuOptions::for_tests(), NoWindow)
            .err()
            .expect("an error");
        assert!(matches!(err, GpuError::Surface(_)), "{err:?}");
        assert!(err.to_string().contains("surface"), "{err}");
    }

    #[test]
    #[ignore = "needs a GPU adapter (lavapipe is enough)"]
    fn a_validation_error_is_returned_and_the_device_stays_usable() {
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
        let err = gpu
            .guarded("a bad texture", || {
                bad_texture(&gpu);
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(&err, GpuError::Gpu(why) if why.contains("a bad texture")), "{err}");
        assert_eq!(gpu.guarded("fine", || Ok(7)).unwrap(), 7);
        assert!(gpu.lost().is_none());
    }

    #[test]
    #[ignore = "needs a GPU adapter (lavapipe is enough)"]
    fn an_error_nobody_scoped_is_remembered_instead_of_panicking() {
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter");
        assert!(gpu.last_uncaptured_error().is_none());
        // wgpu's default handler would panic here.
        bad_texture(&gpu);
        assert!(gpu.last_uncaptured_error().is_some());
    }
}
