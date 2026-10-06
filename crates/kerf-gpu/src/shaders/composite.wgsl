// Kerf GPU compositor v0 — three passes over planar 8-bit YUV 4:2:0.
//
//   fs_h        horizontal bicubic resample of one plane   -> R32Float
//   fs_v        vertical resample (+ the `eq` tables)      -> R8Unorm  (8-bit, like swscale's output)
//   fs_compose  rotate, place and blend, still in YUV      -> Rgba8Unorm canvas holding Y, U, V, A
//   fs_convert  the finished composite, YUV -> RGB, once   -> Rgba8Unorm
//
// Everything here mirrors what FFmpeg's still graph does to the same planes;
// see the module docs in src/lib.rs for why (colour space, scaler, chroma).

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covers the target.
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}

// ---- resample ---------------------------------------------------------------

struct ResampleParams {
    src_off: vec2<i32>,   // window of the source plane that is read
    src_size: vec2<i32>,
    dst_full: vec2<i32>,  // size the window is scaled to (the kernel's ratio)
    dst_off: vec2<i32>,   // the kept part of the scaled picture (a Cover crop)
    out_size: vec2<i32>,
    apply_lut: u32,
    plane: u32,           // row of the LUT texture
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> rp: ResampleParams;
@group(0) @binding(2) var lut: texture_2d<f32>;

// swscale's SWS_BICUBIC: the Mitchell-Netravali family with B = 0, C = 0.6.
fn cubic(x: f32) -> f32 {
    let a = abs(x);
    if (a < 1.0) {
        return (8.4 * a * a * a - 14.4 * a * a + 6.0) / 6.0;
    }
    if (a < 2.0) {
        return (-3.6 * a * a * a + 18.0 * a * a - 28.8 * a + 14.4) / 6.0;
    }
    return 0.0;
}

// One output sample along `horizontal` or the other axis. The kernel is
// stretched by the ratio when shrinking (swscale does the same), pixel centres
// are aligned at +0.5, and taps outside the window clamp to its edge.
fn resample(horizontal: bool, o: vec2<i32>) -> f32 {
    var n: i32;
    var idx: i32;
    var ratio: f32;
    if (horizontal) {
        n = rp.src_size.x;
        idx = o.x + rp.dst_off.x;
        ratio = f32(rp.src_size.x) / f32(rp.dst_full.x);
    } else {
        n = rp.src_size.y;
        idx = o.y + rp.dst_off.y;
        ratio = f32(rp.src_size.y) / f32(rp.dst_full.y);
    }
    let s = max(1.0, ratio);
    let pos = (f32(idx) + 0.5) * ratio - 0.5;
    let lo = i32(floor(pos - 2.0 * s));
    let hi = i32(ceil(pos + 2.0 * s));
    var acc = 0.0;
    var wsum = 0.0;
    for (var j = lo; j <= hi; j = j + 1) {
        let w = cubic((f32(j) - pos) / s);
        let jc = clamp(j, 0, n - 1);
        var v: f32;
        if (horizontal) {
            v = textureLoad(src, vec2<i32>(rp.src_off.x + jc, rp.src_off.y + o.y), 0).r;
        } else {
            v = textureLoad(src, vec2<i32>(o.x, jc), 0).r;
        }
        acc = acc + w * v;
        wsum = wsum + w;
    }
    return acc / wsum;
}

@fragment
fn fs_h(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    return vec4<f32>(resample(true, o), 0.0, 0.0, 1.0);
}

@fragment
fn fs_v(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    // swscale hands the next filter an 8-bit plane: round, then (for the last
    // stage of a colour-corrected layer) run vf_eq's table on that byte.
    var q = round(clamp(resample(false, o), 0.0, 1.0) * 255.0);
    if (rp.apply_lut != 0u) {
        q = round(textureLoad(lut, vec2<i32>(i32(q), i32(rp.plane)), 0).r * 255.0);
    }
    return vec4<f32>(q / 255.0, 0.0, 0.0, 1.0);
}

// ---- compose ----------------------------------------------------------------

struct ComposeParams {
    opacity: vec4<f32>, // x: the layer's opacity (the rest is padding)
    origin: vec2<i32>,  // where the layer's top-left lands on the canvas
    layer: vec2<i32>,   // the layer as overlaid (the rotated box, or the picture)
    pic: vec2<i32>,     // the picture before rotation
    cpic: vec2<i32>,    // ... and its chroma planes
    clayer: vec2<i32>,  // the layer's chroma planes
    rot: vec2<f32>,     // cos, sin of the clockwise angle
    rotate: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> cp: ComposeParams;
@group(0) @binding(1) var ytex: texture_2d<f32>;
@group(0) @binding(2) var utex: texture_2d<f32>;
@group(0) @binding(3) var vtex: texture_2d<f32>;

// FFmpeg's `rotate` (bilinear, centre-to-centre): the sample for output pixel
// `o` of a `outsz` box from a `insz` picture. Returns (value 0..255, coverage).
// A source coordinate whose floor lies in [-1, size] is inside — the one-pixel
// apron `rotate` extends the edge by — and anything beyond is left empty.
fn rot_sample(tex: texture_2d<f32>, o: vec2<i32>, outsz: vec2<i32>, insz: vec2<i32>) -> vec2<f32> {
    let c = cp.rot.x;
    let s = cp.rot.y;
    let dx = f32(o.x) - f32(outsz.x - 1) * 0.5;
    let dy = f32(o.y) - f32(outsz.y - 1) * 0.5;
    let sx = c * dx + s * dy + f32(insz.x - 1) * 0.5;
    let sy = -s * dx + c * dy + f32(insz.y - 1) * 0.5;
    let fx = floor(sx);
    let fy = floor(sy);
    if (fx < -1.0 || fx > f32(insz.x) || fy < -1.0 || fy > f32(insz.y)) {
        return vec2<f32>(0.0, 0.0);
    }
    let x0 = clamp(i32(fx), 0, insz.x - 1);
    let y0 = clamp(i32(fy), 0, insz.y - 1);
    let x1 = min(x0 + 1, insz.x - 1);
    let y1 = min(y0 + 1, insz.y - 1);
    let tx = sx - fx;
    let ty = sy - fy;
    let a = textureLoad(tex, vec2<i32>(x0, y0), 0).r;
    let b = textureLoad(tex, vec2<i32>(x1, y0), 0).r;
    let d = textureLoad(tex, vec2<i32>(x0, y1), 0).r;
    let e = textureLoad(tex, vec2<i32>(x1, y1), 0).r;
    let top = a + (b - a) * tx;
    let bot = d + (e - d) * tx;
    return vec2<f32>((top + (bot - top) * ty) * 255.0, 1.0);
}

@fragment
fn fs_compose(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let l = vec2<i32>(frag.xy) - cp.origin;
    if (l.x < 0 || l.y < 0 || l.x >= cp.layer.x || l.y >= cp.layer.y) {
        discard;
    }
    // Chroma is shared by 2x2 luma pixels (the layer's origin is even, so the
    // grids line up); the final conversion replicates it, as swscale's does.
    let lc = vec2<i32>(l.x / 2, l.y / 2);
    var y: f32;
    var u: f32;
    var v: f32;
    var a = 1.0;
    if (cp.rotate == 0u) {
        y = textureLoad(ytex, l, 0).r * 255.0;
        u = textureLoad(utex, lc, 0).r * 255.0;
        v = textureLoad(vtex, lc, 0).r * 255.0;
    } else {
        let sy = rot_sample(ytex, l, cp.layer, cp.pic);
        y = sy.x;
        a = sy.y;
        u = rot_sample(utex, lc, cp.clayer, cp.cpic).x;
        v = rot_sample(vtex, lc, cp.clayer, cp.cpic).x;
    }
    // Blend in YUV, like FFmpeg's overlay: the canvas holds the encoded Y, U, V
    // values (chroma replicated per pixel) and the blend factors act on each
    // channel alike. Converting each layer to RGB first would clamp it before
    // the blend, and a legal-but-out-of-gamut picture (saturated test patterns,
    // super-whites) would then composite differently.
    return vec4<f32>(round(y) / 255.0, round(u) / 255.0, round(v) / 255.0, a * cp.opacity.x);
}

// ---- convert ----------------------------------------------------------------

struct ConvertParams {
    coef: vec4<f32>,    // ky, rv, bu, gu   (YUV -> RGB, 0..255 domain)
    coef2: vec4<f32>,   // gv, bias, 0, 0
}

@group(0) @binding(0) var<uniform> vp: ConvertParams;
@group(0) @binding(1) var canvas: texture_2d<f32>;

// The one YUV -> RGB conversion of a frame: limited range, the plan's matrix.
// (Chroma is already shared by each 2x2 block, which is what swscale's unscaled
// conversion does: it replicates, it does not interpolate.)
@fragment
fn fs_convert(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let yuv = round(textureLoad(canvas, vec2<i32>(frag.xy), 0).rgb * 255.0);
    let yy = vp.coef.x * (yuv.x - 16.0);
    let r = yy + vp.coef.y * (yuv.z - 128.0);
    let g = yy + vp.coef.w * (yuv.y - 128.0) + vp.coef2.x * (yuv.z - 128.0);
    let b = yy + vp.coef.z * (yuv.y - 128.0);
    let rgb = clamp((vec3<f32>(r, g, b) - vp.coef2.y) / 255.0, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(rgb, 1.0);
}
