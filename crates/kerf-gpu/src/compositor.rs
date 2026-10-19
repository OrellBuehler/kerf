//! Compositor v0: draw a [`RenderPlan`] onto a black canvas on the GPU and read
//! it back as RGBA.
//!
//! Per layer, the passes mirror what the FFmpeg still graph does to the same
//! pictures, in the same order and at the same precision — that is what makes a
//! pixel comparison against FFmpeg meaningful rather than aspirational:
//!
//! 1. **Scale each plane** (`resample_h` + `resample_v` in `composite.wgsl`) with
//!    swscale's bicubic: its own filter tables ([`crate::sws`]) and its integer
//!    arithmetic. Luma and the two chroma planes are scaled *independently at
//!    their own size*, like swscale does, and the result is an 8-bit plane. One
//!    stage for the fit, a second when the clip's transform has its own scale —
//!    FFmpeg runs those as two scalers too. A stage that changes nothing is
//!    skipped, as FFmpeg's `scale` skips it.
//! 2. **`eq` tables** (colour correction) are applied to the last stage's 8-bit
//!    planes — in YUV, before any RGB exists, as the filter does.
//! 3. **The RGB round trip** of a translucent layer (`roundtrip.wgsl`,
//!    [`crate::roundtrip`]): FFmpeg's `colorchannelmixer` only takes RGB, so a clip
//!    below full opacity is converted `yuva420p -> argb -> yuva420p` before it is
//!    overlaid, clipping out-of-gamut colour and rebuilding the chroma.
//! 4. **Compose**: rotate (bilinear, as `rotate`), run the clip's `fade` filters on the
//!    8-bit picture (their own integer arithmetic, [`kerf_core::fade_luma`]; a dip to
//!    white goes through the RGB round trip of step 3 instead), place at FFmpeg's integer
//!    position and blend onto the canvas — which holds **Y, U, V and alpha**, not
//!    RGB, because `overlay` blends the encoded YUV planes. (Converting each layer
//!    to RGB first would clamp it before the blend, and legal-but-out-of-gamut
//!    footage then composites differently.) A layer whose alpha is below 255 (an
//!    opacity, a dissolve's ramp) is blended by `overlay`'s own integer formula over a
//!    copy of the canvas, not by the hardware's blend. A letterboxed layer is the whole frame
//!    with black bars (`pad`'s output), and an odd layer's last chroma block
//!    reaches the pixel past it — both reproduced.
//! 5. **Convert once**: the finished composite goes YUV -> RGB with the plan's
//!    matrix (limited range, chroma replicated 2x2) and is read back.
//!
//! Every GPU call runs in error scopes ([`Gpu::guarded`]): a wgpu failure comes
//! back as a [`GpuError`], never a panic.
//!
//! Everything is 8-bit between stages, in encoded gamma — FFmpeg's own working
//! space — so there is no linear-light round trip to disagree about.

use std::borrow::Borrow;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use kerf_core::{
    EffectKinds, FadeEdge, FadeTint, GeqEdge, GpuCaps, Mask, MaskShape, RenderPlan, VideoEffect, YuvMatrix, FADE_FULL,
};
use wgpu::util::DeviceExt;

use crate::effects::{self, Gaussian};
use crate::eq;
use crate::frame_source::{FrameSource, Hint};
use crate::gpu::{Gpu, GpuError};
use crate::motion::MotionFrames;
use crate::roundtrip::{Rgb2Yuv, Yuv2Rgb};
use crate::source::{chroma_size, decode_layers, YuvFrame};
use crate::sws::{self, Filter};
use crate::text::{self, Blend, Raster, Source, TextAtlas};
use kerf_core::layer_geometry::{LayerGeometry, Rect, ScaleStage};

/// A rendered frame: tightly packed 8-bit RGBA, top row first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A rendered frame still on the GPU: the finished composite as an `Rgba8Unorm` texture of
/// the device it was drawn on (encoded values, as [`RgbaFrame`] would hold them), for a
/// [`crate::Presenter`] to draw to a window without a trip through the CPU. Dropping it
/// frees the texture; submitted work that reads it keeps it alive until it is done.
pub struct RenderedFrame {
    pub(crate) texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
}

impl RenderedFrame {
    /// Upload `frame` (RGBA, top row first) as a texture of `gpu`: a picture the compositor did not
    /// draw (a placeholder, a screenshot) that a [`crate::Presenter`] can show all the same.
    pub fn from_rgba(gpu: &Gpu, frame: &RgbaFrame) -> Result<RenderedFrame, GpuError> {
        let (w, h) = (frame.width, frame.height);
        if w == 0 || h == 0 || frame.data.len() != (w as usize) * (h as usize) * 4 {
            return Err(GpuError::Unsupported(format!(
                "{} bytes are not a {w}x{h} RGBA picture",
                frame.data.len()
            )));
        }
        let limit = gpu.device.limits().max_texture_dimension_2d;
        if w > limit || h > limit {
            return Err(GpuError::Unsupported(format!(
                "a {w}x{h} picture is over this device's {limit}px texture limit"
            )));
        }
        gpu.guarded("uploading a frame", || {
            let texture = gpu.device.create_texture_with_data(
                &gpu.queue,
                &wgpu::TextureDescriptor {
                    label: Some("uploaded frame"),
                    size: extent(w, h),
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: CANVAS_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &frame.data,
            );
            Ok(RenderedFrame {
                texture,
                width: w,
                height: h,
            })
        })
    }

    /// The frame's pixels, copied back to the CPU (for a screenshot or a test; the app presents
    /// it without this). `gpu` must be the device it was rendered on.
    pub fn read_back(&self, gpu: &Gpu) -> Result<RgbaFrame, GpuError> {
        gpu.guarded("reading a rendered frame back", || {
            let enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kerf read back"),
            });
            read_texture(gpu, enc, &self.texture, self.width, self.height)
        })
    }
}

/// Where the time of a [`Compositor::render_plan`] went. The decode is FFmpeg's
/// (and v0 spawns one process per layer), the composite is the GPU's upload,
/// passes and readback.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderTimings {
    pub decode: Duration,
    pub composite: Duration,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ResampleParams {
    src_off: [i32; 2],
    src_size: [i32; 2],
    dst_off: [i32; 2],
    out_size: [i32; 2],
    apply_lut: u32,
    plane: u32,
    taps: i32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ComposeParams {
    opacity: [f32; 4],
    matte: [f32; 4],
    origin: [i32; 2],
    layer: [i32; 2],
    pic: [i32; 2],
    cpic: [i32; 2],
    clayer: [i32; 2],
    rot: [f32; 2],
    pic_at: [i32; 2],
    pic_shows: [i32; 2],
    rotate: u32,
    tail: u32,
    has_alpha: u32,
    edge: u32,
    fade_in: u32,
    fade_out: u32,
    black: u32,
    alpha: u32,
    ramp: u32,
    pad2: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ConvertParams {
    coef: [f32; 4],
    coef2: [f32; 4],
}

/// `roundtrip.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RoundTripParams {
    k: [i32; 4],
    m: [i32; 4],
    ky: [i32; 4],
    ku: [i32; 4],
    kv: [i32; 4],
    size: [i32; 2],
    csize: [i32; 2],
    channel: i32,
    taps: i32,
    /// A dip to white's `fade` factors, run in the RGB between the conversions.
    white: [i32; 2],
}

/// `text.wgsl`'s uniform: one blend of an overlay.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TextParams {
    colour: [u32; 4],
    area: [i32; 4],
    alpha: u32,
    kind: u32,
    rgb: u32,
    pad: u32,
}

/// `planeops.wgsl`'s uniform: one table lookup, `fade` or `unsharp` over a plane.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PlaneOpParams {
    size: [i32; 2],
    row: i32,
    amount: i32,
    kind: u32,
    black: u32,
    f_in: u32,
    f_out: u32,
}

/// `blur.wgsl`'s uniform: `gblur`'s numbers for one plane.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlurParams {
    size: [i32; 2],
    nu: f32,
    boundary: f32,
    post: f32,
    pad: [f32; 3],
}

/// `vignette.wgsl`'s uniform: one plane (its dither is in [`effects::vignette_steps`]).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VignetteParams {
    size: [u32; 2],
    map_w: u32,
    chroma: u32,
}

/// `chromakey.wgsl`'s uniform.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct KeyParams {
    size: [u32; 2],
    key: [u32; 2],
    slices: u32,
    pad: [u32; 3],
}

/// One plane on the GPU. A handle: cloning shares the texture.
#[derive(Clone)]
struct Plane {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

/// How long a frame may take to come back from the GPU before the render gives up.
const READBACK_TIMEOUT: Duration = Duration::from_secs(30);

const PLANE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const MID_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const FILTER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// U and V together, as 15-bit integers: the round trip's chroma intermediate.
const CHROMA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg32Float;
const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The GPU compositor: pipelines created once, textures per frame.
///
/// v0 allocates its textures and buffers per call and keeps no cache but the mask planes' — simple
/// to reason about while the numbers are what matters; pooling is A1.
pub struct Compositor {
    gpu: Arc<Gpu>,
    resample_layout: wgpu::BindGroupLayout,
    compose_layout: wgpu::BindGroupLayout,
    convert_layout: wgpu::BindGroupLayout,
    resample_h: wgpu::RenderPipeline,
    resample_v: wgpu::RenderPipeline,
    compose: wgpu::RenderPipeline,
    /// The same shader writing only U and V: the chroma an odd layer's last
    /// half block puts on the pixel just past it.
    compose_chroma: wgpu::RenderPipeline,
    /// A layer with an alpha below 255 (opacity, a dissolve's ramp): FFmpeg's blend in integers,
    /// over a copy of the canvas, instead of the hardware's.
    compose_exact_layout: wgpu::BindGroupLayout,
    compose_exact: wgpu::RenderPipeline,
    compose_exact_chroma: wgpu::RenderPipeline,
    convert: wgpu::RenderPipeline,
    /// The RGB round trip of a translucent layer (`roundtrip.wgsl`).
    rt_rgb: wgpu::RenderPipeline,
    rt_luma: wgpu::RenderPipeline,
    rt_chroma_h: wgpu::RenderPipeline,
    rt_chroma_v: wgpu::RenderPipeline,
    rt_v_layout: wgpu::BindGroupLayout,
    /// A 256x3 table that changes nothing, bound when a layer has no `eq`.
    identity_lut: wgpu::TextureView,
    text: TextPass,
    ops: PlaneOps,
    masks: MaskCache,
    vignettes: VignetteMaps,
    /// What FFmpeg's `geq` does at a plane's last column and row, when the host knows it: else probed.
    geq_edge: Option<GeqEdge>,
    /// `chromakey`'s distance terms ([`effects::chroma_key_terms`]), uploaded by the first keyed frame.
    key_terms: OnceLock<wgpu::Buffer>,
    /// The threads FFmpeg's filter graph slices a frame into: `chromakey` starts its window over at
    /// each slice, so its alpha at the first sample of a slice depends on them.
    graph_threads: u32,
}

/// The passes over a layer's planes between the scaler and the blend (`planeops.wgsl`, `blur.wgsl`):
/// a table, `fade`, `unsharp`, and `gblur`'s two recursions and the pass that writes them back.
struct PlaneOps {
    layout: wgpu::BindGroupLayout,
    lut: wgpu::RenderPipeline,
    fade: wgpu::RenderPipeline,
    unsharp: wgpu::RenderPipeline,
    edge: wgpu::RenderPipeline,
    blur_layout: wgpu::BindGroupLayout,
    blur_h: wgpu::ComputePipeline,
    blur_v: wgpu::ComputePipeline,
    blur_out_layout: wgpu::BindGroupLayout,
    blur_out: wgpu::RenderPipeline,
    key_layout: wgpu::BindGroupLayout,
    key: wgpu::RenderPipeline,
    vignette_layout: wgpu::BindGroupLayout,
    vignette: wgpu::RenderPipeline,
}

/// `vignette`'s factor map for one picture size ([`effects::vignette_map`], as `float` bits) on the GPU,
/// made once because it does not change (the filter's `eval=init`).
struct VignetteMap {
    w: u32,
    h: u32,
    gpu: wgpu::Buffer,
}

/// The vignette maps in use, newest first: a few sizes at once (a cut's clips at their fit sizes).
#[derive(Default)]
struct VignetteMaps(Mutex<Vec<Arc<VignetteMap>>>);

/// How many sizes of vignette map are kept.
const VIGNETTE_MAPS: usize = 4;

impl VignetteMaps {
    fn held(&self, w: u32, h: u32) -> Option<Arc<VignetteMap>> {
        let mut maps = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let at = maps.iter().position(|m| (m.w, m.h) == (w, h))?;
        let map = maps.remove(at);
        maps.insert(0, Arc::clone(&map));
        Some(map)
    }

    /// The map for `w` x `h`, made (tens of milliseconds at 1080p) outside the lock when it is not held.
    fn get(&self, device: &wgpu::Device, w: u32, h: u32) -> Arc<VignetteMap> {
        if let Some(map) = self.held(w, h) {
            return map;
        }
        let bits: Vec<u32> = effects::vignette_map(w, h).into_iter().map(f32::to_bits).collect();
        let gpu = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vignette map"),
            contents: bytemuck::cast_slice(&bits),
            usage: wgpu::BufferUsages::STORAGE,
        });
        if let Some(map) = self.held(w, h) {
            return map;
        }
        let map = Arc::new(VignetteMap { w, h, gpu });
        let mut maps = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        maps.insert(0, Arc::clone(&map));
        maps.truncate(VIGNETTE_MAPS);
        map
    }
}

/// Where the frames a layer's chain has passed on so far come from, for a filter that counts them
/// (`vignette`'s dither): a still graph runs the one frame it shows through the chain (0); an export
/// frame needs the clip's own count, which only the cursor that reads it knows ([`MotionFrames::counts`]).
#[derive(Clone, Copy)]
enum Counts<'a> {
    /// No counts: a Still plan's layers are 0, an export frame that needs one is refused.
    None,
    /// One per layer of the plan, in layer order.
    Of(&'a [Option<u64>]),
}

/// A mask's alpha plane for one picture size (`kerf_core::mask_alpha_plane`), made once because a
/// mask does not move — `geq` evaluates it again for every frame — and uploaded by the first frame
/// that draws it.
struct MaskPlane {
    w: u32,
    h: u32,
    data: Vec<u8>,
    gpu: OnceLock<Plane>,
}

/// The mask planes in use, newest first: a few, under a byte cap that counts each plane twice (the
/// CPU copy and the one uploaded for it).
struct MaskCache {
    entries: Mutex<Vec<(MaskKey, Arc<MaskPlane>)>>,
    max_entries: usize,
    max_bytes: usize,
}

type MaskKey = ([u64; 6], u32, u32);

const MASK_CACHE_ENTRIES: usize = 8;
const MASK_CACHE_BYTES: usize = 96 << 20;

impl Default for MaskCache {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
            max_entries: MASK_CACHE_ENTRIES,
            max_bytes: MASK_CACHE_BYTES,
        }
    }
}

impl MaskCache {
    fn get(&self, mask: &Mask, w: u32, h: u32) -> Arc<MaskPlane> {
        let m = mask.normalized();
        let shape = match m.shape {
            MaskShape::Rect => 0,
            MaskShape::Ellipse => 1,
        };
        let key: MaskKey = (
            [
                shape | (u64::from(m.inverted) << 1),
                m.x.to_bits(),
                m.y.to_bits(),
                m.width.to_bits(),
                m.height.to_bits(),
                m.feather.to_bits(),
            ],
            w,
            h,
        );
        let held = |entries: &mut Vec<(MaskKey, Arc<MaskPlane>)>| {
            entries.iter().position(|(k, _)| *k == key).map(|at| {
                let hit = entries.remove(at);
                let plane = Arc::clone(&hit.1);
                entries.insert(0, hit);
                plane
            })
        };
        if let Some(plane) = held(&mut self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner)) {
            return plane;
        }
        // (Made with no lock held: a 1080p ellipse is a few milliseconds.)
        let plane = Arc::new(MaskPlane {
            w,
            h,
            data: kerf_core::mask_alpha_plane(&m, w, h),
            gpu: OnceLock::new(),
        });
        let mut entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(found) = held(&mut entries) {
            return found;
        }
        entries.insert(0, (key, Arc::clone(&plane)));
        let (mut bytes, mut keep) = (0usize, 0usize);
        for (_, p) in entries.iter() {
            // (The texture a plane is uploaded to is as many bytes again: one 8-bit sample each.)
            bytes += 2 * p.data.len();
            if keep > 0 && (keep >= self.max_entries || bytes > self.max_bytes) {
                break;
            }
            keep += 1;
        }
        entries.truncate(keep);
        plane
    }
}

/// What happens to a layer's planes between the scaler and the blend, beyond the fades: worked out
/// before the GPU is touched.
struct LayerLook {
    /// `eq` and every pointwise effect (grayscale, invert) ahead of the first blur or sharpen,
    /// composed into one table per plane and applied in the scaler's last pass before the zoom.
    leading: Option<[[u8; 256]; 3]>,
    /// What follows the first blur or sharpen, in the clip's order.
    steps: Vec<Step>,
    /// The mask's alpha, at the size of the picture the mask is cut from.
    mask: Option<Arc<MaskPlane>>,
    /// `geq` copies the planes through `lum` / `cb` / `cr` and, on a build that reads its last
    /// column and row one in, changes them: the planes' own last column and row are their neighbours'.
    mask_edge: bool,
    /// A moving zoom comes last in FFmpeg's chain: everything above runs on the fit-size picture,
    /// and the second scale stage zooms the result.
    zoom_last: bool,
    /// `chromakey` after the effects: the last one's, as each writes the whole alpha plane from the
    /// chroma, which none of them changes.
    key: Option<effects::ChromaKeyOptions>,
}

enum Step {
    /// Pointwise effects that follow a blur or sharpen.
    Table(Box<[[u8; 256]; 3]>),
    Blur(Gaussian),
    Sharpen(i32),
    /// `vignette`, dithered from the layer's frame count ([`Drawn::count`]).
    Vignette,
}

/// What drawing overlays needs: `text.wgsl`'s pipeline, a mask for the blends that have none (the
/// box), and the rasterised overlays ([`TextAtlas`]): the cut's, and a drag's poses apart from them.
struct TextPass {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    blank: wgpu::TextureView,
    atlas: TextAtlas,
    drafts: TextAtlas,
}

/// Whose frame a composite draws, for where its titles are kept.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Texts {
    /// The cut's: titles are rasterised into the atlas, to be served again.
    Cut,
    /// A drag's (the cut as a gesture under way would leave it): a title the atlas does not hold is
    /// a pose of the drag, kept apart ([`TextAtlas::get_drafted`]).
    Draft,
}

/// An overlay of a frame, rasterised: what to blend and the masks to blend it with.
struct PreparedText {
    raster: Arc<Raster>,
    blends: Vec<Blend>,
}

/// Limited-range YUV -> full-range RGB coefficients for a matrix, `([ky, rv, bu,
/// gu], gv)`, in the 0..255 domain the final conversion works in.
fn matrix_coefs(matrix: YuvMatrix) -> ([f32; 4], f32) {
    let (kr, kb) = matrix.weights();
    let kg = 1.0 - kr - kb;
    let chroma_gain = 255.0 / 224.0;
    let coef = [
        (255.0 / 219.0) as f32,
        (2.0 * (1.0 - kr) * chroma_gain) as f32,
        (2.0 * (1.0 - kb) * chroma_gain) as f32,
        (-(2.0 * (1.0 - kb) * kb / kg) * chroma_gain) as f32,
    ];
    let gv = (-(2.0 * (1.0 - kr) * kr / kg) * chroma_gain) as f32;
    (coef, gv)
}

/// What a composite hands back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    /// RGB, converted once at the end — what a preview or export shows.
    Rgb,
    /// The canvas planes before that conversion.
    Yuv,
    /// RGB, left on the GPU for a presenter.
    Texture,
}

/// What a composite hands back, by [`Output`].
enum Rendered {
    Pixels(RgbaFrame),
    Texture(RenderedFrame),
}

impl Rendered {
    fn pixels(self) -> Result<RgbaFrame, GpuError> {
        match self {
            Rendered::Pixels(frame) => Ok(frame),
            Rendered::Texture(_) => Err(GpuError::Gpu("a texture was rendered where pixels were asked for".into())),
        }
    }
}

/// A layer that has a picture and a place to draw it.
struct Drawn<'a> {
    layer: &'a kerf_core::PlanLayer,
    frame: &'a YuvFrame,
    geom: LayerGeometry,
    /// Per stage, per plane (Y, U, V): the scaler's tables.
    filters: Vec<[PlaneFilters; 3]>,
    fades: LayerFades,
    look: LayerLook,
    /// The frames the layer's chain has passed on before this one (`vignette`'s dither runs on from them).
    count: u64,
}

/// What the `fade` filters of a layer's chain do at this frame, as the passes take them:
/// the integer factors of the clip's own fade-in and fade-out (a dip to black's halves are those),
/// a dissolve's alpha ramp ([`FADE_FULL`]: not running), the black level the filter takes for the
/// chain's pixel format and the layer's alpha before the ramp, and a dip to white: `fade c=white`
/// only takes RGB, so the picture goes `yuv420p -> rgb24 -> yuv420p` around it (`white_round_trip`),
/// in every frame of the clip in an export and while the fade runs in a still.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LayerFades {
    fade_in: u32,
    fade_out: u32,
    black: u32,
    alpha: u32,
    ramp: u32,
    white_in: u32,
    white_out: u32,
    white_round_trip: bool,
}

impl LayerFades {
    fn of(plan: &RenderPlan, layer: &kerf_core::PlanLayer, geom: &LayerGeometry) -> Result<Self, GpuError> {
        let mut fades = Self {
            fade_in: FADE_FULL,
            fade_out: FADE_FULL,
            black: if plan.on_alpha_plane(layer) { 0 } else { 16 },
            alpha: (geom.opacity * 255.0).round().clamp(0.0, 255.0) as u32,
            ramp: FADE_FULL,
            white_in: FADE_FULL,
            white_out: FADE_FULL,
            white_round_trip: false,
        };
        let motion = plan.mode == kerf_core::PlanMode::Motion;
        let mut seen = [false; 5];
        for op in plan.fade_ops(layer) {
            let (slot, which) = match (op.tint, op.edge) {
                (FadeTint::Black, FadeEdge::In) => (&mut fades.fade_in, 0),
                (FadeTint::Black, FadeEdge::Out) => (&mut fades.fade_out, 1),
                (FadeTint::Alpha, FadeEdge::In) => (&mut fades.ramp, 2),
                (FadeTint::White, FadeEdge::In) => (&mut fades.white_in, 3),
                (FadeTint::White, FadeEdge::Out) => (&mut fades.white_out, 4),
                (tint, edge) => {
                    return Err(GpuError::Unsupported(format!(
                        "layer {}: a {tint:?} fade-{edge:?}, which the compositor does not draw",
                        layer.name
                    )))
                }
            };
            if std::mem::replace(&mut seen[which], true) {
                return Err(GpuError::Unsupported(format!(
                    "layer {}: two {:?} fades on one side",
                    layer.name, op.tint
                )));
            }
            *slot = op.factor;
            // The filter is in the chain for every frame in an export, and for the frames it runs in a still.
            if op.tint == FadeTint::White && (motion || op.factor < FADE_FULL) {
                fades.white_round_trip = true;
            }
        }
        if plan.through_rgba(layer) {
            // `colorchannelmixer` writes the alpha in an RGBA picture, a dissolve's ramp runs on that
            // alpha there, and the conversion to `yuva420p` that follows scales it by 256/255: the ramp
            // comes ahead of that scale, not after it. (A black or white fade there would fade the
            // colours too, which the plan refuses.)
            if fades.fade_in < FADE_FULL || fades.fade_out < FADE_FULL || fades.white_round_trip {
                return Err(GpuError::Unsupported(format!(
                    "layer {}: a fade to black or white on a layer below full opacity",
                    layer.name
                )));
            }
            let mixed = (layer.transform.opacity.clamp(0.0, 1.0) * 255.0).round_ties_even() as u32;
            let ramped = if fades.ramp < FADE_FULL {
                (mixed * fades.ramp + 32768) >> 16
            } else {
                mixed
            };
            fades.alpha = ((ramped * 256 + 127) / 255).min(255);
            fades.ramp = FADE_FULL;
        }
        if fades.white_round_trip {
            let why = if plan.on_alpha_plane(layer) {
                Some("on an alpha plane (a rotation, an opacity, a dissolve or a mask in the same chain), where FFmpeg converts it through another RGB format")
            } else if fades.fade_in < FADE_FULL || fades.fade_out < FADE_FULL {
                Some("while a black fade of the same clip runs ahead of it")
            } else {
                None
            };
            if let Some(why) = why {
                return Err(GpuError::Unsupported(format!("layer {}: a dip to white {why}", layer.name)));
            }
        }
        Ok(fades)
    }

    /// The layer's alpha is below 255 somewhere: the hardware's blend is not FFmpeg's, the integer one is.
    fn blends(&self) -> bool {
        self.alpha < 255 || self.ramp < FADE_FULL
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        // Non-filterable accepts every format used here; the shader only ever
        // `textureLoad`s, so no sampler (and no filtering) is involved.
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn extent(w: u32, h: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    }
}

/// Do ahead of a frame what a plan needs measured and what takes a while: today FFmpeg's `geq`, for a
/// masked plan (one short child process, once per process). Call it from a blocking thread **off the
/// render lock**, with the plan in hand, so the first masked frame does not pay for it under the
/// lock; a compositor pinned with [`Compositor::with_geq_edge`] needs none of it.
pub fn prepare(plan: &RenderPlan) {
    if plan.layers.iter().any(|l| l.mask.is_some()) {
        let _ = kerf_core::geq_edge();
    }
}

impl Compositor {
    /// What this compositor draws exactly, as data: the plan refuses (through
    /// [`RenderPlan::reasons`]) whatever is not in it. [`GpuCaps::A0`] (layers, transform, crop, fit,
    /// `eq`, opacity, stills, sampled keyframes) and
    ///
    /// * `text`: titles and captions, FFmpeg's own `drawtext` raster blended here ([`crate::text`]);
    /// * `motion`, `fades`, `transitions`, `dip_white`: fades, dips, dissolves, slides and pushes, in
    ///   still plans and in export frames (`tests/motion_parity.rs`);
    /// * `keyed_zoom`: a moving zoom drawn last, as FFmpeg's chain has it;
    /// * `mask`, and the blur, sharpen, grayscale, invert, vignette and chroma key `effects`.
    ///
    /// The combinations the plan names (`Unsupported`) stay FFmpeg's.
    /// Each later pass flips its bit beside the pass and its parity cases.
    pub fn caps(&self) -> GpuCaps {
        Self::CAPS
    }

    /// [`Compositor::caps`] without a device: what any compositor of this build draws.
    pub const CAPS: GpuCaps = {
        let mut caps = GpuCaps::A0;
        caps.text = true;
        caps.motion = true;
        caps.fades = true;
        caps.transitions = true;
        caps.dip_white = true;
        caps.keyed_zoom = true;
        caps.mask = true;
        caps.effects = EffectKinds::BLUR
            .union(EffectKinds::SHARPEN)
            .union(EffectKinds::GRAYSCALE)
            .union(EffectKinds::INVERT)
            .union(EffectKinds::VIGNETTE)
            .union(EffectKinds::CHROMA_KEY);
        caps
    };

    /// Pin what FFmpeg's `geq` does at a plane's last column and row, instead of measuring the
    /// ffmpeg this process runs ([`kerf_core::geq_edge`], one child process, on the first masked
    /// frame unless [`prepare`] ran ahead of it): for a host that knows its ffmpeg, and for tests.
    #[must_use]
    pub fn with_geq_edge(mut self, edge: GeqEdge) -> Self {
        self.geq_edge = Some(edge);
        self
    }

    /// Draw `chromakey` as an FFmpeg filter graph of `threads` slice threads draws it (the default is
    /// what an export on this machine runs: [`kerf_core::cpu_graph_threads`]): the first sample of each
    /// slice but the first has no window carried over from the row before it.
    #[must_use]
    pub fn with_graph_threads(mut self, threads: u32) -> Self {
        self.graph_threads = threads.max(1);
        self
    }

    fn geq_edge(&self) -> GeqEdge {
        self.geq_edge.unwrap_or_else(kerf_core::geq_edge)
    }

    /// Build the pipelines on `gpu`. Fails with a [`GpuError`] — it never panics —
    /// when the device rejects the shader or a pipeline, or is already lost.
    pub fn new(gpu: Arc<Gpu>) -> Result<Self, GpuError> {
        let g = Arc::clone(&gpu);
        gpu.guarded("creating the compositor", move || {
            let device = &g.device;
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("kerf composite"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/composite.wgsl").into()),
            });
            let resample_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("resample"),
                entries: &[texture_entry(0), uniform_entry(1), texture_entry(2), texture_entry(3)],
            });
            let compose_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("compose"),
                entries: &[uniform_entry(0), texture_entry(1), texture_entry(2), texture_entry(3)],
            });
            let compose_exact_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("compose exact"),
                entries: &[
                    uniform_entry(0),
                    texture_entry(1),
                    texture_entry(2),
                    texture_entry(3),
                    texture_entry(4),
                    texture_entry(5),
                ],
            });
            let convert_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("convert"),
                entries: &[uniform_entry(0), texture_entry(1)],
            });
            let pipeline = |label: &str, layout: &wgpu::BindGroupLayout, fs: &str, format, blend, mask| {
                let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(label),
                    bind_group_layouts: &[Some(layout)],
                    immediate_size: 0,
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pl),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(fs),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend,
                            write_mask: mask,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
            let over = wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                    operation: wgpu::BlendOperation::Add,
                },
            };
            let all = wgpu::ColorWrites::ALL;
            let resample_h = pipeline("resample_h", &resample_layout, "fs_h", MID_FORMAT, None, all);
            let resample_v = pipeline("resample_v", &resample_layout, "fs_v", PLANE_FORMAT, None, all);
            let compose = pipeline("compose", &compose_layout, "fs_compose", CANVAS_FORMAT, Some(over), all);
            let chroma_only = wgpu::ColorWrites::GREEN | wgpu::ColorWrites::BLUE;
            let compose_chroma = pipeline(
                "compose chroma",
                &compose_layout,
                "fs_compose",
                CANVAS_FORMAT,
                Some(over),
                chroma_only,
            );
            let compose_exact = pipeline(
                "compose exact",
                &compose_exact_layout,
                "fs_compose_exact",
                CANVAS_FORMAT,
                None,
                all,
            );
            let compose_exact_chroma = pipeline(
                "compose exact chroma",
                &compose_exact_layout,
                "fs_compose_exact",
                CANVAS_FORMAT,
                None,
                chroma_only,
            );
            let convert = pipeline("convert", &convert_layout, "fs_convert", CANVAS_FORMAT, None, all);

            // The round trip: its own module, its own bindings.
            let rt_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("kerf roundtrip"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/roundtrip.wgsl").into()),
            });
            let rt_v_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("roundtrip vertical"),
                entries: &[uniform_entry(0), texture_entry(1), texture_entry(2)],
            });
            let rt_pipeline = |label: &str, layout: &wgpu::BindGroupLayout, fs: &str, format| {
                let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(label),
                    bind_group_layouts: &[Some(layout)],
                    immediate_size: 0,
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pl),
                    vertex: wgpu::VertexState {
                        module: &rt_shader,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &rt_shader,
                        entry_point: Some(fs),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
            };
            let rt_rgb = rt_pipeline("rt rgb", &compose_layout, "fs_rgb", CANVAS_FORMAT);
            let rt_luma = rt_pipeline("rt luma", &convert_layout, "fs_luma", PLANE_FORMAT);
            let rt_chroma_h = rt_pipeline("rt chroma h", &convert_layout, "fs_chroma_h", CHROMA_FORMAT);
            let rt_chroma_v = rt_pipeline("rt chroma v", &rt_v_layout, "fs_chroma_v", PLANE_FORMAT);

            let identity: Vec<u8> = (0..3).flat_map(|_| 0..=255u8).collect();
            let identity_lut = Self::lut_texture(&g, &identity);

            // Overlays: one pipeline, whose input is the canvas and a mask.
            let text_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("kerf text"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/text.wgsl").into()),
            });
            let text_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("text"),
                entries: &[uniform_entry(0), texture_entry(1), texture_entry(2)],
            });
            let text_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("text"),
                bind_group_layouts: &[Some(&text_layout)],
                immediate_size: 0,
            });
            let text_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("text"),
                layout: Some(&text_pl),
                vertex: wgpu::VertexState {
                    module: &text_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &text_shader,
                    entry_point: Some("fs_text"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: CANVAS_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
            let blank = g
                .device
                .create_texture_with_data(
                    &g.queue,
                    &wgpu::TextureDescriptor {
                        label: Some("text blank mask"),
                        size: extent(1, 1),
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: PLANE_FORMAT,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &[0],
                )
                .create_view(&wgpu::TextureViewDescriptor::default());
            let ops = Self::plane_ops(device);
            Ok(Self {
                gpu: Arc::clone(&g),
                resample_layout,
                compose_layout,
                convert_layout,
                resample_h,
                resample_v,
                compose,
                compose_chroma,
                compose_exact_layout,
                compose_exact,
                compose_exact_chroma,
                convert,
                rt_rgb,
                rt_luma,
                rt_chroma_h,
                rt_chroma_v,
                rt_v_layout,
                identity_lut,
                text: TextPass {
                    layout: text_layout,
                    pipeline: text_pipeline,
                    blank,
                    atlas: TextAtlas::default(),
                    drafts: TextAtlas::with_limits(text::DRAFT_ATLAS_MAX_ENTRIES, text::DRAFT_ATLAS_MAX_BYTES),
                },
                ops,
                masks: MaskCache::default(),
                vignettes: VignetteMaps::default(),
                geq_edge: None,
                key_terms: OnceLock::new(),
                graph_threads: u32::try_from(kerf_core::cpu_graph_threads()).unwrap_or(1),
            })
        })
    }

    /// The pipelines of [`PlaneOps`].
    fn plane_ops(device: &wgpu::Device) -> PlaneOps {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("plane ops"),
            entries: &[uniform_entry(0), texture_entry(1), texture_entry(2)],
        });
        let render = |label: &str, module: &wgpu::ShaderModule, layout: &wgpu::BindGroupLayout, fs: &str| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(layout)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: PLANE_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let ops_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf plane ops"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/planeops.wgsl").into()),
        });
        let lut = render("ops lut", &ops_module, &layout, "fs_lut");
        let fade = render("ops fade", &ops_module, &layout, "fs_fade");
        let unsharp = render("ops unsharp", &ops_module, &layout, "fs_unsharp");
        let edge = render("ops edge", &ops_module, &layout, "fs_edge");

        let entry = |binding: u32, ty: wgpu::BindingType, visibility: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty,
            count: None,
        };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let storage = |read_only| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let texture = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
            entries: &[
                entry(0, uniform, wgpu::ShaderStages::COMPUTE),
                entry(1, texture, wgpu::ShaderStages::COMPUTE),
                entry(2, storage(false), wgpu::ShaderStages::COMPUTE),
            ],
        });
        let blur_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf blur"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blur.wgsl").into()),
        });
        let compute = |label: &str, cs: &str| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(&blur_layout)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                module: &blur_module,
                entry_point: Some(cs),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let blur_h = compute("blur rows", "cs_h");
        let blur_v = compute("blur columns", "cs_v");
        let blur_out_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur out"),
            entries: &[
                entry(0, uniform, wgpu::ShaderStages::FRAGMENT),
                entry(1, storage(true), wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let out_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf blur out"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blur_out.wgsl").into()),
        });
        let blur_out = render("blur out", &out_module, &blur_out_layout, "fs_out");
        let vignette_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vignette"),
            entries: &[
                entry(0, uniform, wgpu::ShaderStages::FRAGMENT),
                entry(1, texture, wgpu::ShaderStages::FRAGMENT),
                entry(2, storage(true), wgpu::ShaderStages::FRAGMENT),
                entry(3, storage(true), wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let vignette_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf vignette"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/vignette.wgsl").into()),
        });
        let vignette = render("vignette", &vignette_module, &vignette_layout, "fs_vignette");
        let key_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("chroma key"),
            entries: &[
                entry(0, uniform, wgpu::ShaderStages::FRAGMENT),
                entry(1, texture, wgpu::ShaderStages::FRAGMENT),
                entry(2, texture, wgpu::ShaderStages::FRAGMENT),
                entry(3, storage(true), wgpu::ShaderStages::FRAGMENT),
                entry(4, storage(true), wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let key_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kerf chroma key"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/chromakey.wgsl").into()),
        });
        let key = render("chroma key", &key_module, &key_layout, "fs_key");
        PlaneOps {
            layout,
            lut,
            fade,
            unsharp,
            edge,
            blur_layout,
            blur_h,
            blur_v,
            blur_out_layout,
            blur_out,
            vignette_layout,
            vignette,
            key_layout,
            key,
        }
    }

    fn lut_texture(gpu: &Gpu, rows: &[u8]) -> wgpu::TextureView {
        let tex = gpu.device.create_texture_with_data(
            &gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("eq tables"),
                size: extent(256, 3),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PLANE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rows,
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn plane(&self, label: &str, w: u32, h: u32, format: wgpu::TextureFormat, extra: wgpu::TextureUsages) -> Plane {
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent(w, h),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | extra,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Plane { texture, view, w, h }
    }

    fn upload(&self, label: &str, w: u32, h: u32, data: &[u8]) -> Plane {
        let p = self.plane(label, w, h, PLANE_FORMAT, wgpu::TextureUsages::COPY_DST);
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &p.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            extent(w, h),
        );
        p
    }

    fn pass(
        &self,
        enc: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        target: &wgpu::TextureView,
        bind_group: &wgpu::BindGroup,
        clear: Option<wgpu::Color>,
        scissor: Option<(u32, u32, u32, u32)>,
    ) {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some((x, y, w, h)) = scissor {
            pass.set_scissor_rect(x, y, w, h);
        }
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// One uniform buffer and a bind group over `views` for a round-trip pass.
    fn rt_bind(&self, layout: &wgpu::BindGroupLayout, params: &RoundTripParams, views: &[&wgpu::TextureView]) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("roundtrip params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: buf.as_entire_binding(),
        }];
        for (i, v) in views.iter().enumerate() {
            entries.push(wgpu::BindGroupEntry {
                binding: i as u32 + 1,
                resource: wgpu::BindingResource::TextureView(v),
            });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("roundtrip"),
            layout,
            entries: &entries,
        })
    }

    /// The layer's planes after FFmpeg's `yuva420p -> argb -> yuva420p` (see
    /// [`crate::roundtrip`]): out of YUV with the layer's matrix, then back with
    /// the composite's (`back`) and the chroma rebuilt from the RGB.
    fn translucent_planes(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &[Plane; 3],
        forward: YuvMatrix,
        back: YuvMatrix,
        white: [u32; 2],
    ) -> Result<[Plane; 3], GpuError> {
        let [py, pu, pv] = input;
        let (w, h) = (py.w, py.h);
        let (cw, ch) = (pu.w, pu.h);
        let t = Yuv2Rgb::new(forward);
        let b = Rgb2Yuv::new(back);
        let params = RoundTripParams {
            k: [t.crv, t.cbu, t.cgu, t.cgv],
            m: [t.cy, t.oy, 326 + 512, 0],
            ky: [b.ry, b.gy, b.by, 0],
            ku: [b.ru, b.gu, b.bu, 0],
            kv: [b.rv, b.gv, b.bv, 0],
            size: [w as i32, h as i32],
            csize: [cw as i32, ch as i32],
            channel: 0,
            taps: 0,
            white: white.map(|f| f as i32),
        };
        let rgb = self.plane("rt rgb", w, h, CANVAS_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let bind = self.rt_bind(&self.compose_layout, &params, &[&py.view, &pu.view, &pv.view]);
        self.pass(enc, &self.rt_rgb, &rgb.view, &bind, None, None);

        let luma = self.plane(
            "rt y",
            w,
            h,
            PLANE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let bind = self.rt_bind(&self.convert_layout, &params, &[&rgb.view]);
        self.pass(enc, &self.rt_luma, &luma.view, &bind, None, None);

        let inter = self.plane("rt chroma", cw, h, CHROMA_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        self.pass(enc, &self.rt_chroma_h, &inter.view, &bind, None, None);

        let filter = sws::bicubic(h, ch, sws::ONE_VERTICAL)
            .ok_or_else(|| GpuError::Unsupported("a picture too tall for the chroma filter".into()))?;
        let taps = self.filter_texture(&filter);
        let mut out = Vec::with_capacity(2);
        for channel in 0..2 {
            let p = RoundTripParams {
                channel,
                taps: filter.size as i32,
                ..params
            };
            let plane = self.plane(
                "rt chroma plane",
                cw,
                ch,
                PLANE_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            );
            let bind = self.rt_bind(&self.rt_v_layout, &p, &[&inter.view, &taps]);
            self.pass(enc, &self.rt_chroma_v, &plane.view, &bind, None, None);
            out.push(plane);
        }
        let (Some(v), Some(u)) = (out.pop(), out.pop()) else {
            unreachable!("two chroma planes")
        };
        Ok([luma, u, v])
    }

    /// A swscale filter table as a texture: one row per output sample, the window
    /// start then the weights.
    fn filter_texture(&self, f: &Filter) -> wgpu::TextureView {
        let texels = f.texels();
        let tex = self.gpu.device.create_texture_with_data(
            &self.gpu.queue,
            &wgpu::TextureDescriptor {
                label: Some("scaler taps"),
                size: extent((f.size + 1) as u32, f.pos.len() as u32),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FILTER_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            bytemuck::cast_slice(&texels),
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// One swscale stage over one plane: horizontal pass into a 15-bit
    /// intermediate, vertical pass into the 8-bit result.
    fn scale_plane(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &Plane,
        stage: PlaneStage,
        filters: &PlaneFilters,
        lut: Option<(&wgpu::TextureView, u32)>,
    ) -> Plane {
        let device = &self.gpu.device;
        let PlaneStage { src, keep, .. } = stage;
        // Horizontal: (kept width) x (source window height).
        let mid = self.plane("mid", keep.w, src.h, MID_FORMAT, wgpu::TextureUsages::RENDER_ATTACHMENT);
        let out = self.plane(
            "plane",
            keep.w,
            keep.h,
            PLANE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let base = ResampleParams {
            src_off: [src.x as i32, src.y as i32],
            src_size: [src.w as i32, src.h as i32],
            dst_off: [keep.x as i32, keep.y as i32],
            out_size: [keep.w as i32, keep.h as i32],
            apply_lut: 0,
            plane: 0,
            taps: filters.h.size as i32,
            pad: 0,
        };
        let (lut_view, lut_row) = lut.map_or((&self.identity_lut, 0), |(v, r)| (v, r));
        let bind = |params: ResampleParams, view: &wgpu::TextureView, taps: &wgpu::TextureView| {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("resample params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.resample_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(lut_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(taps),
                    },
                ],
            })
        };
        let h_taps = self.filter_texture(&filters.h);
        let v_taps = self.filter_texture(&filters.v);
        let h = bind(base, &input.view, &h_taps);
        self.pass(enc, &self.resample_h, &mid.view, &h, Some(wgpu::Color::BLACK), None);
        let v = bind(
            ResampleParams {
                apply_lut: u32::from(lut.is_some()),
                plane: lut_row,
                taps: filters.v.size as i32,
                ..base
            },
            &mid.view,
            &v_taps,
        );
        self.pass(enc, &self.resample_v, &out.view, &v, Some(wgpu::Color::BLACK), None);
        out
    }

    /// A bind group over one uniform and two textures for a pass of [`PlaneOps`].
    fn ops_bind(&self, params: &PlaneOpParams, src: &wgpu::TextureView, aux: &wgpu::TextureView) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("plane op params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("plane op"),
            layout: &self.ops.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(aux),
                },
            ],
        })
    }

    /// A new 8-bit plane of the size of `like`, to render into.
    fn out_plane(&self, label: &str, like: &Plane) -> Plane {
        self.plane(
            label,
            like.w,
            like.h,
            PLANE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        )
    }

    /// The three planes through per-plane tables (Y, U, V).
    fn table_planes(&self, enc: &mut wgpu::CommandEncoder, input: &[Plane; 3], tables: &[[u8; 256]; 3]) -> [Plane; 3] {
        let flat: Vec<u8> = tables.iter().flatten().copied().collect();
        let view = Self::lut_texture(&self.gpu, &flat);
        std::array::from_fn(|row| {
            let plane = &input[row];
            let out = self.out_plane("table", plane);
            let params = PlaneOpParams {
                size: [plane.w as i32, plane.h as i32],
                row: row as i32,
                amount: 0,
                kind: 0,
                black: 0,
                f_in: 0,
                f_out: 0,
            };
            let bind = self.ops_bind(&params, &plane.view, &view);
            self.pass(enc, &self.ops.lut, &out.view, &bind, None, None);
            out
        })
    }

    /// `fade` on one plane of a layer that is faded before it is zoomed: `kind` 0 luma (the
    /// layer's black level), 1 chroma, 2 the alpha plane (`f_in` is the dissolve's ramp).
    fn fade_plane(&self, enc: &mut wgpu::CommandEncoder, input: &Plane, kind: u32, black: u32, f_in: u32, f_out: u32) -> Plane {
        let out = self.out_plane("fade", input);
        let params = PlaneOpParams {
            size: [input.w as i32, input.h as i32],
            row: 0,
            amount: 0,
            kind,
            black,
            f_in,
            f_out,
        };
        let bind = self.ops_bind(&params, &input.view, &self.text.blank);
        self.pass(enc, &self.ops.fade, &out.view, &bind, None, None);
        out
    }

    /// The three planes as `geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)'` hands them on where it reads
    /// one in from a plane's last column and row (`kerf_core::GeqEdge::Penultimate`).
    fn edge_planes(&self, enc: &mut wgpu::CommandEncoder, input: &[Plane; 3]) -> [Plane; 3] {
        std::array::from_fn(|i| {
            let plane = &input[i];
            let out = self.out_plane("geq edge", plane);
            let params = PlaneOpParams {
                size: [plane.w as i32, plane.h as i32],
                row: 0,
                amount: 0,
                kind: 0,
                black: 0,
                f_in: 0,
                f_out: 0,
            };
            let bind = self.ops_bind(&params, &plane.view, &self.text.blank);
            self.pass(enc, &self.ops.edge, &out.view, &bind, None, None);
            out
        })
    }

    /// `unsharp` (5x5, luma only: the chroma amount is 0, which copies) on the luma plane.
    fn sharpen_planes(&self, enc: &mut wgpu::CommandEncoder, input: &[Plane; 3], amount: i32) -> [Plane; 3] {
        let out = self.out_plane("sharpen", &input[0]);
        let params = PlaneOpParams {
            size: [out.w as i32, out.h as i32],
            row: 0,
            amount,
            kind: 0,
            black: 0,
            f_in: 0,
            f_out: 0,
        };
        let bind = self.ops_bind(&params, &input[0].view, &self.text.blank);
        self.pass(enc, &self.ops.unsharp, &out.view, &bind, None, None);
        [out, input[1].clone(), input[2].clone()]
    }

    /// `vignette` on the three planes of the `count`th frame the layer's chain has passed on: each sample's
    /// dither is the LCG's state at its step in the filter's run (`effects.rs`).
    fn vignette_planes(&self, enc: &mut wgpu::CommandEncoder, input: &[Plane; 3], count: u64) -> [Plane; 3] {
        let device = &self.gpu.device;
        let (w, h) = (input[0].w, input[0].h);
        let map = self.vignettes.get(device, w, h);
        let buffer = &map.gpu;
        // The run's step at this frame's first sample, then each plane's: Y, U, V in turn.
        let start = count.wrapping_mul(effects::vignette_samples(w, h));
        let luma = u64::from(w) * u64::from(h);
        let chroma = u64::from(input[1].w) * u64::from(input[1].h);
        let offsets = [0, luma, luma + chroma];
        std::array::from_fn(|pi| {
            let plane = &input[pi];
            let params = VignetteParams {
                size: [plane.w, plane.h],
                map_w: w,
                chroma: u32::from(pi > 0),
            };
            let steps = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vignette steps"),
                contents: bytemuck::cast_slice(&effects::vignette_steps(
                    start.wrapping_add(offsets[pi]) as u32,
                    plane.w,
                    plane.h,
                )),
                usage: wgpu::BufferUsages::STORAGE,
            });
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("vignette params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vignette"),
                layout: &self.ops.vignette_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&plane.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: steps.as_entire_binding(),
                    },
                ],
            });
            let out = self.out_plane("vignetted", plane);
            self.pass(enc, &self.ops.vignette, &out.view, &bind, None, None);
            out
        })
    }

    /// The alpha plane the compositor keys `frame` to (`chromakey.wgsl`, at the picture's own size, with this
    /// compositor's graph threads): what a keyed layer is blended with, read back so the pass can be compared
    /// with the filter directly.
    pub fn chroma_key_alpha(&self, frame: &YuvFrame, key: &effects::ChromaKeyOptions) -> Result<Vec<u8>, GpuError> {
        let (w, h) = (frame.width, frame.height);
        if !frame.is_consistent() || w < 2 || h < 2 {
            return Err(GpuError::Unsupported(format!("a chroma key of a {w}x{h} frame")));
        }
        let (cw, ch) = chroma_size(w, h);
        let planes = [
            self.upload("key y", w, h, &frame.y),
            self.upload("key u", cw, ch, &frame.u),
            self.upload("key v", cw, ch, &frame.v),
        ];
        let mut enc = self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("chroma key"),
        });
        let alpha = self.chroma_key_plane(&mut enc, &planes, key);
        Ok(read_texels(&self.gpu, enc, &alpha.texture, w, h, 1)?.data)
    }

    /// `chromakey`'s alpha plane, at the picture's size, from its chroma (`chromakey.wgsl`).
    fn chroma_key_plane(&self, enc: &mut wgpu::CommandEncoder, planes: &[Plane; 3], key: &effects::ChromaKeyOptions) -> Plane {
        let device = &self.gpu.device;
        let (w, h) = (planes[0].w, planes[0].h);
        let words = |bits: &[u64]| -> Vec<u32> { bits.iter().flat_map(|b| [(b >> 32) as u32, *b as u32]).collect() };
        let terms = self.key_terms.get_or_init(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("chroma key terms"),
                contents: bytemuck::cast_slice(&words(&effects::chroma_key_terms())),
                usage: wgpu::BufferUsages::STORAGE,
            })
        });
        let thresholds = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chroma key thresholds"),
            contents: bytemuck::cast_slice(&words(&effects::chroma_key_thresholds(key.similarity, key.blend))),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let params = KeyParams {
            size: [w, h],
            key: [u32::from(key.uv[0]), u32::from(key.uv[1])],
            slices: self.graph_threads.min(h),
            pad: [0; 3],
        };
        let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("chroma key params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chroma key"),
            layout: &self.ops.key_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubuf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&planes[1].view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&planes[2].view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: terms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: thresholds.as_entire_binding(),
                },
            ],
        });
        let out = self.out_plane("chroma key alpha", &planes[0]);
        self.pass(enc, &self.ops.key, &out.view, &bind, None, None);
        out
    }

    /// `gblur` on all three planes, each at its own size with the same sigma.
    fn blur_planes(&self, enc: &mut wgpu::CommandEncoder, input: &[Plane; 3], g: &Gaussian) -> [Plane; 3] {
        let device = &self.gpu.device;
        std::array::from_fn(|i| {
            let plane = &input[i];
            let params = BlurParams {
                size: [plane.w as i32, plane.h as i32],
                nu: g.nu,
                boundary: g.boundary,
                post: g.post,
                pad: [0.0; 3],
            };
            let ubuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blur params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let work = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("blur work"),
                size: u64::from(plane.w) * u64::from(plane.h) * 4,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blur"),
                layout: &self.ops.blur_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&plane.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: work.as_entire_binding(),
                    },
                ],
            });
            for (pipeline, lines) in [(&self.ops.blur_h, plane.h), (&self.ops.blur_v, plane.w)] {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("gblur"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bind, &[]);
                pass.dispatch_workgroups(lines.div_ceil(64), 1, 1);
            }
            let out = self.out_plane("blurred", plane);
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blur out"),
                layout: &self.ops.blur_out_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: work.as_entire_binding(),
                    },
                ],
            });
            self.pass(enc, &self.ops.blur_out, &out.view, &bind, None, None);
            out
        })
    }

    /// One scale stage over the three planes (a stage that changes nothing is the plane itself).
    /// `lut` is a table texture to apply in the vertical pass of each plane (rows Y, U, V).
    fn scale_stage(
        &self,
        enc: &mut wgpu::CommandEncoder,
        input: &[Plane; 3],
        stage: &ScaleStage,
        filters: &[PlaneFilters; 3],
        lut: Option<&wgpu::TextureView>,
    ) -> [Plane; 3] {
        std::array::from_fn(|pi| {
            let plane = &input[pi];
            let ps = plane_stage(stage, pi > 0);
            let lut = lut.map(|t| (t, pi as u32));
            // A stage that changes nothing — the whole plane, at its own size, no table to apply —
            // is the plane itself. FFmpeg's `scale` is a passthrough in the same case, and it is by
            // far the common one (footage already at the delivery size).
            let identity = lut.is_none()
                && ps.src == Rect::whole(plane.w, plane.h)
                && ps.keep == Rect::whole(ps.src.w, ps.src.h)
                && ps.scaled == (ps.src.w, ps.src.h);
            if identity {
                plane.clone()
            } else {
                self.scale_plane(enc, plane, ps, &filters[pi], lut)
            }
        })
    }

    /// The alpha plane through a scale stage: scaled like luma (swscale's `yuva420p` alpha uses the
    /// luma filters).
    fn scale_alpha(&self, enc: &mut wgpu::CommandEncoder, alpha: &Plane, stage: &ScaleStage, filters: &PlaneFilters) -> Plane {
        let ps = plane_stage(stage, false);
        if ps.src == Rect::whole(alpha.w, alpha.h)
            && ps.keep == Rect::whole(ps.src.w, ps.src.h)
            && ps.scaled == (ps.src.w, ps.src.h)
        {
            return alpha.clone();
        }
        self.scale_plane(enc, alpha, ps, filters, None)
    }

    /// What happens to `layer`'s planes between the scaler and the blend, beyond its fades.
    fn look_of(&self, layer: &kerf_core::PlanLayer, geom: &LayerGeometry) -> Result<LayerLook, GpuError> {
        let unsupported = |what: String| GpuError::Unsupported(format!("layer {}: {what}", layer.name));
        let zoom_last = layer.animated.is_some_and(|a| a.zooms) && geom.stages.len() == 2;
        // The picture the effects and the mask act on: the fit-size one when the zoom comes last.
        let (pw, ph) = if zoom_last {
            (geom.stages[0].keep.w, geom.stages[0].keep.h)
        } else {
            geom.picture
        };
        let mut leading = eq::luts(&layer.color);
        let mut steps: Vec<Step> = Vec::new();
        let mut pending: Option<[[u8; 256]; 3]> = None;
        let mut split = false;
        let mut key = None;
        for effect in &layer.effects {
            match effect {
                VideoEffect::Grayscale | VideoEffect::Invert => {
                    let table = effects::table(if matches!(effect, VideoEffect::Grayscale) {
                        effects::GRAYSCALE
                    } else {
                        effects::INVERT
                    });
                    let slot = if split { &mut pending } else { &mut leading };
                    *slot = Some(match slot.take() {
                        Some(first) => effects::then(&first, &table),
                        None => table,
                    });
                }
                VideoEffect::Blur { sigma } => {
                    let g = effects::gaussian(*sigma).ok_or_else(|| unsupported(format!("a blur sigma of {sigma}")))?;
                    // Sigma 0 is a recursion that changes nothing (`nu` 0, every scale 1).
                    if g.nu == 0.0 && g.boundary == 1.0 && g.post == 1.0 {
                        continue;
                    }
                    let limit = self.gpu.device.limits().max_storage_buffer_binding_size;
                    if u64::from(pw) * u64::from(ph) * 4 > limit {
                        return Err(unsupported(format!(
                            "a blur of a {pw}x{ph} picture, over this device's {limit}-byte storage buffer"
                        )));
                    }
                    if let Some(table) = pending.take() {
                        steps.push(Step::Table(Box::new(table)));
                    }
                    split = true;
                    steps.push(Step::Blur(g));
                }
                VideoEffect::Sharpen { amount } => {
                    let a =
                        effects::unsharp_amount(*amount).ok_or_else(|| unsupported(format!("a sharpen amount of {amount}")))?;
                    if a == 0 {
                        continue;
                    }
                    if let Some(table) = pending.take() {
                        steps.push(Step::Table(Box::new(table)));
                    }
                    split = true;
                    steps.push(Step::Sharpen(a));
                }
                VideoEffect::Vignette => {
                    let limit = self.gpu.device.limits().max_storage_buffer_binding_size;
                    if u64::from(pw) * u64::from(ph) * 4 > limit {
                        return Err(unsupported(format!(
                            "a vignette of a {pw}x{ph} picture, over this device's {limit}-byte storage buffer"
                        )));
                    }
                    if let Some(table) = pending.take() {
                        steps.push(Step::Table(Box::new(table)));
                    }
                    split = true;
                    steps.push(Step::Vignette);
                }
                VideoEffect::ChromaKey {
                    color,
                    similarity,
                    blend,
                } => {
                    if pw < 2 || ph < 2 {
                        return Err(unsupported(format!("a chroma key on a {pw}x{ph} picture")));
                    }
                    let rgb =
                        kerf_core::color_rgb(color).ok_or_else(|| unsupported(format!("a chroma key colour of {color:?}")))?;
                    key = Some(effects::ChromaKeyOptions {
                        uv: effects::chroma_key_uv(rgb),
                        similarity: *similarity as f32,
                        blend: *blend as f32,
                    });
                }
            }
        }
        if let Some(table) = pending {
            steps.push(Step::Table(Box::new(table)));
        }
        // `geq` runs on the frame `pad` made of a letterboxed picture, bars included: the mask is the
        // size of the whole layer there, the canvas.
        let mask = match &layer.mask {
            Some(_) if key.is_some() => return Err(unsupported("a mask over a chroma key".into())),
            Some(mask) => {
                if geom.rotation.is_some() {
                    return Err(unsupported("a mask on a rotated layer".into()));
                }
                let (mw, mh) = if geom.matte { geom.layer } else { (pw, ph) };
                Some(self.masks.get(mask, mw, mh))
            }
            None => None,
        };
        // (Only a masked layer asks: the first answer is a child process.)
        let mask_edge = if mask.is_none() {
            false
        } else {
            match self.geq_edge() {
                GeqEdge::Exact => false,
                GeqEdge::Penultimate => {
                    // A frame one chroma sample wide or high has nothing to read one in from (`geq` reads
                    // outside the plane there).
                    let (mw, mh) = if geom.matte { geom.layer } else { (pw, ph) };
                    if mw.div_ceil(2) < 2 || mh.div_ceil(2) < 2 {
                        return Err(unsupported(format!(
                            "a mask on a {mw}x{mh} frame, on an FFmpeg whose geq reads a plane's last column and row one in"
                        )));
                    }
                    true
                }
                GeqEdge::Unknown => {
                    return Err(unsupported(
                        "a mask, until FFmpeg's geq can be measured (it reads a plane's last column and row differently on some builds)".into(),
                    ));
                }
            }
        };
        Ok(LayerLook {
            leading,
            steps,
            mask,
            mask_edge,
            zoom_last,
            key,
        })
    }

    /// Composite `frames` (one decoded picture per plan layer; `None` is a layer
    /// whose source had no frame there, which FFmpeg draws nothing for) at `size`.
    ///
    /// Refuses a plan that is not [`RenderPlan::gpu_supported_at`] this size, a frame that does
    /// not match its layer's stream, or planes whose lengths do not match their
    /// size, so a caller that forgot to ask gets an error naming why instead of a
    /// wrong picture — or a wgpu panic.
    pub fn composite(&self, plan: &RenderPlan, frames: &[Option<YuvFrame>], size: (u32, u32)) -> Result<RgbaFrame, GpuError> {
        self.composite_as(plan, frames, size, Output::Rgb)?.pixels()
    }

    /// [`Compositor::composite`] over shared frames (a [`FrameSource`]'s are `Arc`s, which
    /// may be in its cache and in another composite at once).
    pub fn composite_shared(
        &self,
        plan: &RenderPlan,
        frames: &[Option<Arc<YuvFrame>>],
        size: (u32, u32),
    ) -> Result<RgbaFrame, GpuError> {
        self.composite_as(plan, frames, size, Output::Rgb)?.pixels()
    }

    /// [`Compositor::composite_shared`] with each layer's frame count ([`crate::MotionFrames::counts`]),
    /// which an export frame's `vignette` needs.
    pub fn composite_counted(
        &self,
        plan: &RenderPlan,
        frames: &[Option<Arc<YuvFrame>>],
        counts: &[Option<u64>],
        size: (u32, u32),
    ) -> Result<RgbaFrame, GpuError> {
        self.composite_texts(plan, frames, size, Output::Rgb, Texts::Cut, Counts::Of(counts))?
            .pixels()
    }

    /// [`Compositor::composite`] stopping before the final YUV -> RGB conversion:
    /// the canvas as the 8-bit Y, U and V planes the layers were blended in (the
    /// chroma planes at half size). It exists so the scaler and the blend can be
    /// compared with FFmpeg's planes directly, without a conversion in between.
    /// A plan with overlays and no layer is `Unsupported`: its text is blended in RGB, and
    /// returning the black planes without it would be a frame that silently lacks its text.
    pub fn composite_yuv(&self, plan: &RenderPlan, frames: &[Option<YuvFrame>], size: (u32, u32)) -> Result<YuvFrame, GpuError> {
        self.yuv_planes(plan, frames, Counts::None, size)
    }

    /// [`Compositor::composite_yuv`] over shared frames (a [`crate::MotionFrames`]'s or a [`FrameSource`]'s).
    pub fn composite_yuv_shared(
        &self,
        plan: &RenderPlan,
        frames: &[Option<Arc<YuvFrame>>],
        size: (u32, u32),
    ) -> Result<YuvFrame, GpuError> {
        self.yuv_planes(plan, frames, Counts::None, size)
    }

    /// [`Compositor::composite_yuv_shared`] with each layer's frame count ([`crate::MotionFrames::counts`]),
    /// which an export frame's `vignette` needs.
    pub fn composite_yuv_counted(
        &self,
        plan: &RenderPlan,
        frames: &[Option<Arc<YuvFrame>>],
        counts: &[Option<u64>],
        size: (u32, u32),
    ) -> Result<YuvFrame, GpuError> {
        self.yuv_planes(plan, frames, Counts::Of(counts), size)
    }

    fn yuv_planes<F: Borrow<YuvFrame>>(
        &self,
        plan: &RenderPlan,
        frames: &[Option<F>],
        counts: Counts,
        size: (u32, u32),
    ) -> Result<YuvFrame, GpuError> {
        let canvas = self
            .composite_texts(plan, frames, size, Output::Yuv, Texts::Cut, counts)?
            .pixels()?;
        let (w, h) = (canvas.width as usize, canvas.height as usize);
        let at = |x: usize, y: usize, c: usize| canvas.data[(y * w + x) * 4 + c];
        let chroma = |c: usize| -> Vec<u8> {
            (0..h / 2)
                .flat_map(|y| (0..w / 2).map(move |x| (x, y)))
                .map(|(x, y)| at(x * 2, y * 2, c))
                .collect()
        };
        Ok(YuvFrame {
            width: canvas.width,
            height: canvas.height,
            y: (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| at(x, y, 0))
                .collect(),
            u: chroma(1),
            v: chroma(2),
        })
    }

    fn composite_as<F: Borrow<YuvFrame>>(
        &self,
        plan: &RenderPlan,
        frames: &[Option<F>],
        size: (u32, u32),
        output: Output,
    ) -> Result<Rendered, GpuError> {
        self.composite_texts(plan, frames, size, output, Texts::Cut, Counts::None)
    }

    fn composite_texts<F: Borrow<YuvFrame>>(
        &self,
        plan: &RenderPlan,
        frames: &[Option<F>],
        size: (u32, u32),
        output: Output,
        texts: Texts,
        counts: Counts,
    ) -> Result<Rendered, GpuError> {
        let reasons = plan.reasons(&self.caps(), size);
        if !reasons.is_empty() {
            return Err(GpuError::Unsupported(join_reasons(&reasons)));
        }
        if output == Output::Yuv && plan.layers.is_empty() && !plan.overlays.is_empty() {
            // Such a frame is RGB all the way in FFmpeg (the text goes on the black base after no
            // conversion), so there are no YUV planes the text could be compared in.
            return Err(GpuError::Unsupported(
                "text over a frame with no layer has no YUV planes (it is drawn in RGB)".into(),
            ));
        }
        if frames.len() != plan.layers.len() {
            return Err(GpuError::Decode(format!(
                "{} decoded frames for {} layers",
                frames.len(),
                plan.layers.len()
            )));
        }
        let (ow, oh) = size;
        if ow == 0 || oh == 0 || ow % 2 != 0 || oh % 2 != 0 {
            return Err(GpuError::Unsupported(format!("a {ow}x{oh} canvas (4:2:0 needs even sides)")));
        }
        let limit = self.gpu.device.limits().max_texture_dimension_2d;
        if ow > limit || oh > limit {
            return Err(GpuError::Unsupported(format!(
                "a {ow}x{oh} canvas is over this device's {limit}px texture limit"
            )));
        }

        // Everything that can be refused is refused before the GPU is touched.
        let mut drawn = Vec::with_capacity(frames.len());
        for (n, (layer, frame)) in plan.layers.iter().zip(frames).enumerate() {
            let Some(frame) = frame.as_ref().map(Borrow::borrow) else {
                continue;
            };
            if !frame.is_consistent() {
                return Err(GpuError::Decode(format!(
                    "layer {n}: the planes do not add up to a {}x{} 4:2:0 picture",
                    frame.width, frame.height
                )));
            }
            if (frame.width, frame.height) != (layer.stream.width, layer.stream.height) {
                return Err(GpuError::Unsupported(format!(
                    "layer {n}: a {}x{} frame for a {}x{} stream",
                    frame.width, frame.height, layer.stream.width, layer.stream.height
                )));
            }
            // The stream's size is the frame's (checked above), so the plan's own
            // geometry — rounded to the picture's native chroma grid — is the one.
            let geom = plan
                .layer_geometry(layer, size)
                .map_err(|e| GpuError::Unsupported(format!("layer {n}: {e}")))?;
            // wgpu raises a validation error for a texture over the device's
            // limit — a 10x zoom of a 4K layer would be one. Refuse it instead,
            // so the frame goes to FFmpeg.
            let biggest = geom
                .stages
                .iter()
                .flat_map(|s| [s.src.w, s.src.h, s.keep.w, s.keep.h, s.scaled.0, s.scaled.1])
                .chain([frame.width, frame.height, geom.layer.0, geom.layer.1])
                .max()
                .unwrap_or(0);
            if biggest > limit {
                return Err(GpuError::Unsupported(format!(
                    "layer {n}: a {biggest}px texture is over this device's {limit}px limit"
                )));
            }
            // swscale's tables for every plane of every stage; a ratio it would
            // need two scalers for is FFmpeg's to render.
            let mut filters = Vec::with_capacity(geom.stages.len());
            for stage in &geom.stages {
                let table = |chroma: bool| -> Result<PlaneFilters, GpuError> {
                    let ps = plane_stage(stage, chroma);
                    let too_far = || GpuError::Unsupported(format!("layer {n}: a scale ratio swscale needs two passes for"));
                    Ok(PlaneFilters {
                        h: sws::bicubic(ps.src.w, ps.scaled.0, sws::ONE_HORIZONTAL).ok_or_else(too_far)?,
                        v: sws::bicubic(ps.src.h, ps.scaled.1, sws::ONE_VERTICAL).ok_or_else(too_far)?,
                    })
                };
                filters.push([table(false)?, table(true)?, table(true)?]);
            }
            let fades = LayerFades::of(plan, layer, &geom)?;
            let look = self.look_of(layer, &geom)?;
            let count = if look.steps.iter().any(|s| matches!(s, Step::Vignette)) {
                layer_count(plan, n, counts)?
            } else {
                0
            };
            drawn.push(Drawn {
                layer,
                frame,
                geom,
                filters,
                fades,
                look,
                count,
            });
        }
        // FFmpeg rasterises what text there is, here, before the GPU is touched. No project lock is
        // held, but the caller's render lock is (the app's preview holds `RenderState` across a render,
        // decode included): the wait for a first-seen caption is one process, bounded by RASTER_TIMEOUT.
        let texts = self.prepare_text(plan, size, texts)?;
        self.gpu
            .guarded("compositing a frame", || self.draw(plan, &drawn, &texts, size, output))
    }

    /// Every overlay of the frame rasterised (or served from the atlas) and turned into the blends
    /// that draw it. Blocks for one `ffmpeg` process (at most `RASTER_TIMEOUT`) the first time a state
    /// is seen, so a caller holding a lock across a render holds it that long.
    fn prepare_text(&self, plan: &RenderPlan, size: (u32, u32), texts: Texts) -> Result<Vec<PreparedText>, GpuError> {
        let limit = self.gpu.device.limits().max_texture_dimension_2d;
        plan.overlays
            .iter()
            .map(|t| {
                let graph = kerf_core::text_raster_graph(t, size);
                let key = text::Key::new(&graph, t.font_file.as_deref());
                let make = || text::rasterize(&graph);
                let raster = match texts {
                    Texts::Cut => self.text.atlas.get(key, make)?,
                    Texts::Draft => self.text.atlas.get_drafted(&self.text.drafts, key, make)?,
                };
                if let Some(mask) = &raster.glyphs {
                    if mask.w > limit || mask.h > limit {
                        return Err(GpuError::Unsupported(format!(
                            "a {}x{} text mask is over this device's {limit}px texture limit",
                            mask.w, mask.h
                        )));
                    }
                }
                let blends = text::blends(t, &raster, plan.canvas.matrix, plan.layers.is_empty())?;
                Ok(PreparedText { raster, blends })
            })
            .collect()
    }

    /// Record, submit and read back the passes for the layers that are drawn.
    fn draw(
        &self,
        plan: &RenderPlan,
        drawn: &[Drawn],
        texts: &[PreparedText],
        size: (u32, u32),
        output: Output,
    ) -> Result<Rendered, GpuError> {
        let (ow, oh) = size;
        let device = &self.gpu.device;
        // The canvas holds Y, U, V (and alpha) in an RGBA8 texture: FFmpeg's
        // `overlay` blends the encoded YUV planes, so this does too, and the one
        // conversion to RGB happens at the very end.
        let canvas = self.plane(
            "canvas",
            ow,
            oh,
            CANVAS_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kerf composite"),
        });

        // The empty canvas is black — limited-range Y 16, U = V = 128 — and every
        // layer's pass loads on top of it.
        {
            let _clear = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &canvas.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 16.0 / 255.0,
                            g: 128.0 / 255.0,
                            b: 128.0 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let (coef, gv) = matrix_coefs(plan.canvas.matrix);

        // The canvas as it was before a layer that blends, made once the first one needs it.
        let mut before: Option<Plane> = None;
        for Drawn {
            layer,
            frame,
            geom,
            filters,
            fades,
            look,
            count,
        } in drawn
        {
            let (cw, ch) = frame.chroma_size();
            let planes = [
                self.upload("y", frame.width, frame.height, &frame.y),
                self.upload("u", cw, ch, &frame.u),
                self.upload("v", cw, ch, &frame.v),
            ];

            // `eq` (and the pointwise effects that follow it) as tables, applied in the vertical pass of the
            // last scale stage that comes before the zoom.
            let tables = look.leading.as_ref().map(|rows| {
                let flat: Vec<u8> = rows.iter().flatten().copied().collect();
                Self::lut_texture(&self.gpu, &flat)
            });
            // `pad`'s black bars go through the same tables (they are part of the
            // frame `eq` sees): Y 16, U = V = 128 mapped through them.
            let matte = look.leading.as_ref().map_or([16.0, 128.0, 128.0], |t| {
                [f32::from(t[0][16]), f32::from(t[1][128]), f32::from(t[2][128])]
            });

            // A moving zoom is the last stage of FFmpeg's chain: the fit scale, the grade, the effects, the
            // mask and the fades all act on the picture at its fit size, and the zoom scales the result.
            let before_zoom = if look.zoom_last { 1 } else { geom.stages.len() };
            let mut current = planes;
            for (si, stage) in geom.stages[..before_zoom].iter().enumerate() {
                let lut = (si + 1 == before_zoom).then_some(tables.as_ref()).flatten();
                current = self.scale_stage(&mut enc, &current, stage, &filters[si], lut);
            }
            for step in &look.steps {
                current = match step {
                    Step::Table(rows) => self.table_planes(&mut enc, &current, rows),
                    Step::Blur(g) => self.blur_planes(&mut enc, &current, g),
                    Step::Sharpen(amount) => self.sharpen_planes(&mut enc, &current, *amount),
                    Step::Vignette => self.vignette_planes(&mut enc, &current, *count),
                };
            }
            // (Where the zoom comes last the planes are shifted here, ahead of the zoom's scale; else
            // the blend reads them one in from the layer's last column and row.)
            if look.mask_edge && look.zoom_last {
                current = self.edge_planes(&mut enc, &current);
            }
            // The mask is the alpha plane (`geq` rewrites it); nothing else gives the layer one but the
            // dissolve's ramp, which acts on whatever plane there is.
            let mut alpha: Option<Plane> = match &look.key {
                Some(key) => Some(self.chroma_key_plane(&mut enc, &current, key)),
                None => look
                    .mask
                    .as_ref()
                    .map(|m| m.gpu.get_or_init(|| self.upload("mask", m.w, m.h, &m.data)).clone()),
            };
            // Below full opacity FFmpeg takes the layer through RGB and back before
            // `overlay` sees it (see `roundtrip.rs`); that happens to the scaled
            // picture, ahead of the rotation. An opaque layer never leaves YUV.
            if geom.translucent || fades.white_round_trip {
                // Out of YCbCr with the layer's own matrix; back with the
                // composite's (BT.601 under a fixed policy, the negotiated one else).
                let forward = layer.stream.matrix().unwrap_or(YuvMatrix::Bt601);
                let white = [fades.white_in, fades.white_out];
                current = self.translucent_planes(&mut enc, &current, forward, plan.canvas.matrix, white)?;
            }
            // What the blend pass still has to do about fades: all of them, unless the zoom is last.
            let mut fades = *fades;
            if look.zoom_last {
                if fades.fade_in < FADE_FULL || fades.fade_out < FADE_FULL {
                    let [y, u, v] = &current;
                    current = [
                        self.fade_plane(&mut enc, y, 0, fades.black, fades.fade_in, fades.fade_out),
                        self.fade_plane(&mut enc, u, 1, fades.black, fades.fade_in, fades.fade_out),
                        self.fade_plane(&mut enc, v, 1, fades.black, fades.fade_in, fades.fade_out),
                    ];
                    fades.fade_in = FADE_FULL;
                    fades.fade_out = FADE_FULL;
                }
                // (Without a mask the ramp scales a uniform alpha plane into the same uniform one: the
                // blend pass applies it.)
                if let (Some(a), true) = (&alpha, fades.ramp < FADE_FULL) {
                    alpha = Some(self.fade_plane(&mut enc, a, 2, 0, fades.ramp, FADE_FULL));
                    fades.ramp = FADE_FULL;
                }
                current = self.scale_stage(&mut enc, &current, &geom.stages[1], &filters[1], None);
                alpha = alpha.map(|a| self.scale_alpha(&mut enc, &a, &geom.stages[1], &filters[1][0]));
            }
            let fades = &fades;
            let [py, pu, pv] = &current;

            let (rotate, rot) = match geom.rotation {
                Some(r) => (1, [r.angle.cos() as f32, r.angle.sin() as f32]),
                None => (0, [1.0, 0.0]),
            };
            let (lw, lh) = geom.layer;
            let (clw, clh) = chroma_size(lw, lh);
            let (pcw, pch) = chroma_size(geom.picture.0, geom.picture.1);
            let mut params = ComposeParams {
                opacity: [geom.opacity, 0.0, 0.0, 0.0],
                matte: [matte[0], matte[1], matte[2], 0.0],
                origin: [geom.origin.0, geom.origin.1],
                layer: [lw as i32, lh as i32],
                pic: [geom.picture.0 as i32, geom.picture.1 as i32],
                cpic: [pcw as i32, pch as i32],
                clayer: [clw as i32, clh as i32],
                rot,
                pic_at: [geom.picture_at.0 as i32, geom.picture_at.1 as i32],
                pic_shows: [geom.picture_shows.0 as i32, geom.picture_shows.1 as i32],
                rotate,
                tail: 0,
                has_alpha: u32::from(alpha.is_some()),
                edge: u32::from(look.mask_edge && !look.zoom_last),
                fade_in: fades.fade_in,
                fade_out: fades.fade_out,
                black: fades.black,
                alpha: fades.alpha,
                ramp: fades.ramp,
                pad2: [0; 3],
            };
            // Only the part of the canvas the layer can touch.
            let x0 = i64::from(geom.origin.0).max(0);
            let y0 = i64::from(geom.origin.1).max(0);
            let x1 = (i64::from(geom.origin.0) + i64::from(lw)).min(i64::from(ow));
            let y1 = (i64::from(geom.origin.1) + i64::from(lh)).min(i64::from(oh));
            if x1 <= x0 || y1 <= y0 {
                continue; // entirely off the canvas
            }
            let views = [&py.view, &pu.view, &pv.view];
            // `overlay` blends whole chroma samples: the half block an odd layer
            // ends in reaches one pixel past it, carrying the layer's chroma onto
            // the base's luma. Reproduced for a layer that is not rotated (a
            // rotated one has soft, partly transparent edges there).
            let odd = (lw % 2 == 1, lh % 2 == 1);
            let past_x = i64::from(geom.origin.0) + i64::from(lw);
            let past_y = i64::from(geom.origin.1) + i64::from(lh);
            let tail = rotate == 0 && (odd.0 || odd.1);
            let tx1 = (past_x + i64::from(odd.0)).min(i64::from(ow));
            let ty1 = (past_y + i64::from(odd.1)).min(i64::from(oh));
            let (rx1, ry1) = if tail { (tx1.max(x1), ty1.max(y1)) } else { (x1, y1) };

            // A layer that blends reads the canvas as it stands, and a pass cannot read what it writes:
            // the region it touches is copied aside first.
            let canvas_before = if fades.blends() || alpha.is_some() {
                let copy = before
                    .get_or_insert_with(|| self.plane("canvas before", ow, oh, CANVAS_FORMAT, wgpu::TextureUsages::COPY_DST));
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &canvas.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: x0 as u32,
                            y: y0 as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &copy.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: x0 as u32,
                            y: y0 as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    extent((rx1 - x0) as u32, (ry1 - y0) as u32),
                );
                Some(copy.view.clone())
            } else {
                None
            };
            let (bind, pipeline) = match &canvas_before {
                Some(prev) => (
                    self.compose_bind_exact(&params, views, prev, alpha.as_ref()),
                    &self.compose_exact,
                ),
                None => (self.compose_bind(&params, views), &self.compose),
            };
            self.pass(
                &mut enc,
                pipeline,
                &canvas.view,
                &bind,
                None,
                Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)),
            );

            if tail {
                params.tail = 1;
                let (bind, pipeline) = match &canvas_before {
                    Some(prev) => (
                        self.compose_bind_exact(&params, views, prev, alpha.as_ref()),
                        &self.compose_exact_chroma,
                    ),
                    None => (self.compose_bind(&params, views), &self.compose_chroma),
                };
                self.pass(
                    &mut enc,
                    pipeline,
                    &canvas.view,
                    &bind,
                    None,
                    Some((x0 as u32, y0 as u32, (tx1 - x0) as u32, (ty1 - y0) as u32)),
                );
            }
        }

        // Overlays go on last of all: `drawtext` runs on the finished composite, in its YUV. A frame
        // with no layer has no YUV composite — FFmpeg hands the black base to `drawtext` and on to
        // the PNG as RGB, never converting it — so there the text goes on after the conversion.
        let text_in_rgb = plan.layers.is_empty();
        let canvas = if text_in_rgb {
            canvas
        } else {
            self.draw_text(&mut enc, canvas, texts, false)?
        };
        if output == Output::Yuv {
            return self.read_back(enc, &canvas).map(Rendered::Pixels);
        }
        // The single YUV -> RGB conversion of the finished composite.
        let rgb = self.plane(
            "rgb",
            ow,
            oh,
            CANVAS_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let convert = ConvertParams {
            coef,
            coef2: [gv, 0.0, 0.0, 0.0],
        };
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("convert params"),
            contents: bytemuck::bytes_of(&convert),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("convert"),
            layout: &self.convert_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&canvas.view),
                },
            ],
        });
        self.pass(&mut enc, &self.convert, &rgb.view, &bind, Some(wgpu::Color::BLACK), None);
        let rgb = if text_in_rgb {
            self.draw_text(&mut enc, rgb, texts, true)?
        } else {
            rgb
        };

        if output == Output::Texture {
            // No wait: whatever draws it next is on the same queue, behind this.
            self.gpu.queue.submit(Some(enc.finish()));
            return Ok(Rendered::Texture(RenderedFrame {
                texture: rgb.texture,
                width: ow,
                height: oh,
            }));
        }
        self.read_back(enc, &rgb).map(Rendered::Pixels)
    }

    /// Blend `texts` onto `cur` (the composite's Y, U, V in its channels, or RGB when `rgb`): one
    /// pass per blend, each reading the canvas as the one before left it and writing the other of two
    /// canvases that agree outside the region the overlays can touch. Returns the canvas that holds
    /// the result.
    fn draw_text(
        &self,
        enc: &mut wgpu::CommandEncoder,
        cur: Plane,
        texts: &[PreparedText],
        rgb: bool,
    ) -> Result<Plane, GpuError> {
        let (w, h) = (cur.w, cur.h);
        let mut region: Option<(u32, u32, u32, u32)> = None;
        for t in texts {
            if let Some((x0, y0, x1, y1)) = text::touched(&t.raster, &t.blends, w, h) {
                region = Some(match region {
                    None => (x0, y0, x1, y1),
                    Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
                });
            }
        }
        let Some((x0, y0, x1, y1)) = region else {
            return Ok(cur);
        };
        let other = self.plane(
            "canvas with text",
            w,
            h,
            CANVAS_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        );
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &cur.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &other.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            extent(w, h),
        );
        let device = &self.gpu.device;
        let (mut from, mut to) = (cur, other);
        for t in texts {
            let masks = t.raster.uploaded.get_or_init(|| text::UploadedMasks {
                glyphs: t
                    .raster
                    .glyphs
                    .as_ref()
                    .map(|m| text::UploadedMask::new(self.upload("text glyphs", m.w, m.h, &m.data).texture)),
            });
            for b in &t.blends {
                let (kind, area, mask_view) = match b.source {
                    Source::Box(r) => (1, [r.x, r.y, r.w, r.h], &self.text.blank),
                    Source::Glyphs => {
                        let (Some(mask), Some(uploaded)) = (&t.raster.glyphs, &masks.glyphs) else {
                            continue;
                        };
                        (
                            0,
                            [mask.x as i32, mask.y as i32, mask.w as i32, mask.h as i32],
                            &uploaded.view,
                        )
                    }
                };
                let params = TextParams {
                    colour: [u32::from(b.colour[0]), u32::from(b.colour[1]), u32::from(b.colour[2]), 0],
                    area,
                    alpha: b.alpha,
                    kind,
                    rgb: u32::from(rgb),
                    pad: 0,
                };
                let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("text params"),
                    contents: bytemuck::bytes_of(&params),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("text"),
                    layout: &self.text.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&from.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(mask_view),
                        },
                    ],
                });
                self.pass(
                    enc,
                    &self.text.pipeline,
                    &to.view,
                    &bind,
                    None,
                    Some((x0, y0, x1 - x0, y1 - y0)),
                );
                std::mem::swap(&mut from, &mut to);
            }
        }
        Ok(from)
    }

    /// [`Compositor::compose_bind`] for a layer that blends: the canvas as it stood before it is the fifth binding.
    fn compose_bind_exact(
        &self,
        params: &ComposeParams,
        planes: [&wgpu::TextureView; 3],
        before: &wgpu::TextureView,
        alpha: Option<&Plane>,
    ) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("compose params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let texture = |binding: u32, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose exact"),
            layout: &self.compose_exact_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                texture(1, planes[0]),
                texture(2, planes[1]),
                texture(3, planes[2]),
                texture(4, before),
                texture(5, alpha.map_or(&self.text.blank, |a| &a.view)),
            ],
        })
    }

    fn compose_bind(&self, params: &ComposeParams, planes: [&wgpu::TextureView; 3]) -> wgpu::BindGroup {
        let device = &self.gpu.device;
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("compose params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compose"),
            layout: &self.compose_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(planes[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(planes[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(planes[2]),
                },
            ],
        })
    }

    fn read_back(&self, enc: wgpu::CommandEncoder, canvas: &Plane) -> Result<RgbaFrame, GpuError> {
        read_texture(&self.gpu, enc, &canvas.texture, canvas.w, canvas.h)
    }

    /// Decode every layer of `plan` through FFmpeg and composite them at `size`,
    /// timing the two halves apart.
    pub fn render_plan(&self, plan: &RenderPlan, size: (u32, u32)) -> Result<(RgbaFrame, RenderTimings), GpuError> {
        let reasons = plan.reasons(&self.caps(), size);
        if !reasons.is_empty() {
            return Err(GpuError::Unsupported(join_reasons(&reasons)));
        }
        let t0 = Instant::now();
        let frames = decode_layers(&plan.layers)?;
        let decode = t0.elapsed();
        let t1 = Instant::now();
        let frame = self.composite(plan, &frames, size)?;
        Ok((
            frame,
            RenderTimings {
                decode,
                composite: t1.elapsed(),
            },
        ))
    }
}

impl Compositor {
    /// [`Compositor::render_plan`] with the layers' frames from a [`FrameSource`] (long-lived
    /// runs and a cache) instead of one `ffmpeg` per layer. The frames are the same, byte for
    /// byte: a `FrameSource` answers a still's pick with the frame its `-ss` decodes. An error is
    /// for that frame only — `Busy`, a timeout, `Unsupported` — and the caller renders it
    /// through FFmpeg.
    pub fn render_plan_with(
        &self,
        plan: &RenderPlan,
        size: (u32, u32),
        source: &FrameSource,
        hint: Hint,
    ) -> Result<(RgbaFrame, RenderTimings), GpuError> {
        let reasons = plan.reasons(&self.caps(), size);
        if !reasons.is_empty() {
            return Err(GpuError::Unsupported(join_reasons(&reasons)));
        }
        refuse_prefetch(hint)?;
        let t0 = Instant::now();
        let frames = source.frames(&plan.layers, hint)?;
        let decode = t0.elapsed();
        let t1 = Instant::now();
        let frame = self.composite_shared(plan, &frames, size)?;
        Ok((
            frame,
            RenderTimings {
                decode,
                composite: t1.elapsed(),
            },
        ))
    }
}

impl Compositor {
    /// [`Compositor::render_plan_with`] without the readback: the frame stays on the GPU as a
    /// [`RenderedFrame`], for [`crate::Presenter::present`]. The composite time in the
    /// returned timings is the recording and submission of the passes, not their completion
    /// (nothing waits for the GPU here; the presenter's work queues behind it).
    pub fn render_plan_texture_with(
        &self,
        plan: &RenderPlan,
        size: (u32, u32),
        source: &FrameSource,
        hint: Hint,
    ) -> Result<(RenderedFrame, RenderTimings), GpuError> {
        self.render_texture(plan, size, source, hint, Texts::Cut)
    }

    /// [`Compositor::render_plan_texture_with`] for a drag's frame: `plan` is made from the cut as a
    /// gesture under way would leave it, drawn exactly as any other, but the poses of its titles are
    /// kept apart from the cut's ([`TextAtlas::get_drafted`]), so a drag does not push them out.
    pub fn render_draft_texture_with(
        &self,
        plan: &RenderPlan,
        size: (u32, u32),
        source: &FrameSource,
        hint: Hint,
    ) -> Result<(RenderedFrame, RenderTimings), GpuError> {
        self.render_texture(plan, size, source, hint, Texts::Draft)
    }

    fn render_texture(
        &self,
        plan: &RenderPlan,
        size: (u32, u32),
        source: &FrameSource,
        hint: Hint,
        texts: Texts,
    ) -> Result<(RenderedFrame, RenderTimings), GpuError> {
        let reasons = plan.reasons(&self.caps(), size);
        if !reasons.is_empty() {
            return Err(GpuError::Unsupported(join_reasons(&reasons)));
        }
        refuse_prefetch(hint)?;
        let t0 = Instant::now();
        let frames = source.frames(&plan.layers, hint)?;
        let decode = t0.elapsed();
        let t1 = Instant::now();
        let rendered = match self.composite_texts(plan, &frames, size, Output::Texture, texts, Counts::None)? {
            Rendered::Texture(texture) => texture,
            Rendered::Pixels(_) => return Err(GpuError::Gpu("pixels were rendered where a texture was asked for".into())),
        };
        Ok((
            rendered,
            RenderTimings {
                decode,
                composite: t1.elapsed(),
            },
        ))
    }
}

impl Compositor {
    /// A frame of a [`PlanMode::Motion`](kerf_core::PlanMode) plan, played: every layer's picture
    /// from `motion` (one forward-only cursor per clip, read side by side), composited at `size`
    /// and left on the GPU as a [`RenderedFrame`] for [`crate::Presenter::present`]. The plan is
    /// asked of this compositor's caps first, as [`Compositor::render_plan_texture_with`] asks it.
    /// An error is for that frame only (a cursor refused or failed): the caller plays what it can
    /// from FFmpeg's stream instead. The decode time in the timings is the wait for the cursors,
    /// the composite time the recording and submission of the passes (nothing waits for the GPU).
    pub fn render_motion_texture(
        &self,
        plan: &RenderPlan,
        size: (u32, u32),
        motion: &mut MotionFrames,
    ) -> Result<(RenderedFrame, RenderTimings), GpuError> {
        let reasons = plan.reasons(&self.caps(), size);
        if !reasons.is_empty() {
            return Err(GpuError::Unsupported(join_reasons(&reasons)));
        }
        let t0 = Instant::now();
        // The titles of a frame are rasterised (a first-seen one costs an `ffmpeg`) while its cursors
        // read, not after them: the two waits are independent, and the composite below finds the text
        // in the atlas. A failure here is the composite's to report.
        let frames = std::thread::scope(|scope| {
            let text = (!plan.overlays.is_empty()).then(|| scope.spawn(|| self.warm_text(plan, size)));
            let frames = motion.frames(plan);
            if let Some(text) = text {
                let _ = text.join();
            }
            frames
        })?;
        let decode = t0.elapsed();
        let t1 = Instant::now();
        let rendered =
            match self.composite_texts(plan, &frames, size, Output::Texture, Texts::Cut, Counts::Of(motion.counts()))? {
                Rendered::Texture(texture) => texture,
                Rendered::Pixels(_) => return Err(GpuError::Gpu("pixels were rendered where a texture was asked for".into())),
            };
        Ok((
            rendered,
            RenderTimings {
                decode,
                composite: t1.elapsed(),
            },
        ))
    }

    /// Have the text of `plan` rasterised and held, drawing nothing: a title that comes on screen
    /// costs one `ffmpeg` (tens of milliseconds) the first time its state is seen, which a playing
    /// frame must not wait for. Called for the frames a second ahead from a thread that is not
    /// drawing; the frame that draws the title then finds it in the atlas. A plan with no text
    /// costs nothing.
    pub fn warm_text(&self, plan: &RenderPlan, size: (u32, u32)) -> Result<(), GpuError> {
        self.prepare_text(plan, size, Texts::Cut).map(|_| ())
    }
}

/// How many frames layer `n`'s chain has passed on before this one: none in a still graph (it runs
/// the one frame it shows through the chain); an export frame's is its clip's, counted by the cursor.
fn layer_count(plan: &RenderPlan, n: usize, counts: Counts) -> Result<u64, GpuError> {
    if plan.mode == kerf_core::PlanMode::Still {
        return Ok(0);
    }
    match counts {
        Counts::Of(counts) => counts.get(n).copied().flatten().ok_or_else(|| {
            GpuError::Unsupported(format!(
                "layer {n}: a vignette on a frame whose place in its clip is not known"
            ))
        }),
        Counts::None => Err(GpuError::Unsupported(
            "a vignette in an export frame needs its clip's frames counted".into(),
        )),
    }
}

/// A frame is drawn from frames, and [`Hint::Prefetch`] returns none: a render with it would
/// composite layers of nothing.
fn refuse_prefetch(hint: Hint) -> Result<(), GpuError> {
    match hint {
        Hint::Prefetch => Err(GpuError::Unsupported(
            "Hint::Prefetch decodes nothing to draw (it warms the next clip's run)".into(),
        )),
        _ => Ok(()),
    }
}

/// Copy an `Rgba8Unorm` texture of `w` x `h` back to the CPU, after the work in `enc`.
fn read_texture(gpu: &Gpu, enc: wgpu::CommandEncoder, texture: &wgpu::Texture, w: u32, h: u32) -> Result<RgbaFrame, GpuError> {
    read_texels(gpu, enc, texture, w, h, 4)
}

/// [`read_texture`] for a texture of `bpp` bytes a texel.
fn read_texels(
    gpu: &Gpu,
    mut enc: wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    w: u32,
    h: u32,
    bpp: u32,
) -> Result<RgbaFrame, GpuError> {
    let device = &gpu.device;
    // Rows in a texture-to-buffer copy are padded to 256 bytes.
    let pitch = (w * bpp).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(pitch) * u64::from(h),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: Some(h),
            },
        },
        extent(w, h),
    );
    gpu.queue.submit(Some(enc.finish()));

    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        // The receiver outlives the wait below unless that gave up, in which
        // case nobody wants the result.
        let _ = tx.send(r);
    });
    // A wait that cannot hang the caller: a GPU that stops answering is an
    // error (the owner falls back to FFmpeg and may rebuild the device), not a
    // thread stuck in `poll` forever.
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(READBACK_TIMEOUT),
        })
        .map_err(|e| GpuError::Readback(format!("the GPU did not finish within {} s: {e}", READBACK_TIMEOUT.as_secs())))?;
    rx.recv_timeout(Duration::from_secs(1))
        .map_err(|e| GpuError::Readback(e.to_string()))?
        .map_err(|e| GpuError::Readback(e.to_string()))?;
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .map_err(|e| GpuError::Readback(e.to_string()))?;
    let mut data = Vec::with_capacity((w * h * bpp) as usize);
    for row in mapped.chunks(pitch as usize).take(h as usize) {
        data.extend_from_slice(&row[..(w * bpp) as usize]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(RgbaFrame {
        width: w,
        height: h,
        data,
    })
}

fn join_reasons(reasons: &[kerf_core::Unsupported]) -> String {
    reasons.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

/// A scale stage as one plane sees it.
#[derive(Clone, Copy)]
struct PlaneStage {
    src: Rect,
    scaled: (u32, u32),
    keep: Rect,
}

/// The two tables of one plane's stage.
struct PlaneFilters {
    h: Filter,
    v: Filter,
}

/// A scale stage as one plane sees it. The chroma planes are half the size
/// (rounded up) and are scaled on their own grid. A crop or Cover offset is a whole
/// chroma sample only for a 4:2:0 picture; for 4:2:2, 4:4:4, gray or RGB (rounded to
/// their own, finer grid) it can be odd, and the chroma window then starts a luma
/// pixel early (half a chroma sample): luma is placed exactly, chroma within that
/// half sample — the decode has already averaged those pictures' chroma to 4:2:0.
fn plane_stage(stage: &ScaleStage, chroma: bool) -> PlaneStage {
    if !chroma {
        return PlaneStage {
            src: stage.src,
            scaled: stage.scaled,
            keep: stage.keep,
        };
    }
    let half = |v: u32| v.div_ceil(2);
    PlaneStage {
        src: Rect {
            x: stage.src.x / 2,
            y: stage.src.y / 2,
            w: half(stage.src.w),
            h: half(stage.src.h),
        },
        scaled: (half(stage.scaled.0), half(stage.scaled.1)),
        keep: Rect {
            x: stage.keep.x / 2,
            y: stage.keep.y / 2,
            w: half(stage.keep.w),
            h: half(stage.keep.h),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(x: f64) -> Mask {
        Mask {
            shape: MaskShape::Rect,
            x,
            y: 0.5,
            width: 0.5,
            height: 0.5,
            feather: 0.1,
            inverted: false,
        }
    }

    fn held(cache: &MaskCache) -> Vec<(u32, u32)> {
        let entries = cache.entries.lock().unwrap();
        entries.iter().map(|(k, _)| (k.1, k.2)).collect()
    }

    #[test]
    fn a_mask_plane_is_made_once_for_a_mask_and_a_size() {
        let cache = MaskCache::default();
        let a = cache.get(&mask(0.5), 32, 18);
        assert_eq!(a.data, kerf_core::mask_alpha_plane(&mask(0.5), 32, 18));
        assert!(
            Arc::ptr_eq(&a, &cache.get(&mask(0.5), 32, 18)),
            "the same mask and size is one plane"
        );
        assert!(
            !Arc::ptr_eq(&a, &cache.get(&mask(0.5), 64, 36)),
            "another size is another plane"
        );
        assert!(
            !Arc::ptr_eq(&a, &cache.get(&mask(0.4), 32, 18)),
            "another mask is another plane"
        );
        // A size no mask can have is clamped to a visible one before it is a key.
        let wide = |w| Mask { width: w, ..mask(0.5) };
        assert!(Arc::ptr_eq(&cache.get(&wide(9.0), 8, 8), &cache.get(&wide(99.0), 8, 8)));
    }

    #[test]
    fn the_newest_planes_are_kept_under_the_entry_cap_and_the_byte_cap() {
        let cache = MaskCache {
            max_entries: 3,
            ..MaskCache::default()
        };
        for w in 10..15 {
            cache.get(&mask(0.5), w, 10);
        }
        assert_eq!(held(&cache), [(14, 10), (13, 10), (12, 10)], "three, newest first");
        // A hit moves a plane to the front, so it outlives older ones.
        cache.get(&mask(0.5), 12, 10);
        cache.get(&mask(0.5), 15, 10);
        assert_eq!(held(&cache), [(15, 10), (12, 10), (14, 10)]);

        // The cap counts the CPU copy and the uploaded one: a plane of 120 samples is 240 bytes.
        let cache = MaskCache {
            max_bytes: 500,
            ..MaskCache::default()
        };
        for w in [10, 11, 12] {
            cache.get(&mask(0.5), w, 10);
        }
        assert_eq!(
            held(&cache),
            [(12, 10), (11, 10)],
            "two planes are 460 bytes counted twice, a third 660"
        );
        // The plane just asked for is kept whatever its size.
        cache.get(&mask(0.5), 100, 100);
        assert_eq!(held(&cache), [(100, 100)]);
    }
}

#[cfg(test)]
mod prefetch_tests {
    use super::*;

    #[test]
    fn a_render_is_not_made_with_the_hint_that_draws_nothing() {
        let refused = refuse_prefetch(Hint::Prefetch);
        assert!(matches!(refused, Err(GpuError::Unsupported(_))), "{refused:?}");
        for hint in [Hint::Exact, Hint::Scrub, Hint::Forward { fps: 30.0 }] {
            assert!(refuse_prefetch(hint).is_ok(), "{hint:?}");
        }
    }
}
