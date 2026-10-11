// Kerf GPU compositor v0 — passes over planar 8-bit YUV 4:2:0.
//
//   fs_h        horizontal resample of one plane (swscale's tables) -> R32Float
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
//
// swscale's scaler on swscale's own tables (src/sws.rs), in swscale's integer
// arithmetic: an 8-bit plane is filtered horizontally with 14-bit weights into a
// 15-bit intermediate (value * 128, clipped at the top), then vertically with
// 12-bit weights, rounded at bit 19 into 8 bits.

struct ResampleParams {
    src_off: vec2<i32>,   // window of the source plane that is read
    src_size: vec2<i32>,
    dst_off: vec2<i32>,   // the kept part of the scaled picture (a Cover crop)
    out_size: vec2<i32>,
    apply_lut: u32,
    plane: u32,           // row of the LUT texture
    taps: i32,            // taps per output sample in `filt`
    pad0: u32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> rp: ResampleParams;
@group(0) @binding(2) var lut: texture_2d<f32>;
// One row per output sample: the first source sample of its window, then `taps`
// integer weights.
@group(0) @binding(3) var filt: texture_2d<f32>;

@fragment
fn fs_h(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let idx = o.x + rp.dst_off.x;
    let pos = i32(textureLoad(filt, vec2<i32>(0, idx), 0).r);
    var acc = 0;
    for (var j = 0; j < rp.taps; j = j + 1) {
        let c = i32(textureLoad(filt, vec2<i32>(1 + j, idx), 0).r);
        let sx = clamp(pos + j, 0, rp.src_size.x - 1);
        let v = i32(round(textureLoad(src, vec2<i32>(rp.src_off.x + sx, rp.src_off.y + o.y), 0).r * 255.0));
        acc = acc + c * v;
    }
    // `hScale8To15`: shifted down to 15 bits and clipped at the top only.
    return vec4<f32>(f32(min(acc >> 7u, 32767)), 0.0, 0.0, 1.0);
}

@fragment
fn fs_v(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let idx = o.y + rp.dst_off.y;
    let pos = i32(textureLoad(filt, vec2<i32>(0, idx), 0).r);
    // `yuv2planeX_8`: rounding offset (64 << 12), 12-bit weights, >> 19, clipped.
    var acc = 64 << 12u;
    for (var j = 0; j < rp.taps; j = j + 1) {
        let c = i32(textureLoad(filt, vec2<i32>(1 + j, idx), 0).r);
        let sy = clamp(pos + j, 0, rp.src_size.y - 1);
        acc = acc + c * i32(textureLoad(src, vec2<i32>(o.x, sy), 0).r);
    }
    var q = clamp(acc >> 19u, 0, 255);
    // The next filter gets an 8-bit plane: for the last stage of a
    // colour-corrected layer that is vf_eq's table run on that byte.
    if (rp.apply_lut != 0u) {
        q = i32(round(textureLoad(lut, vec2<i32>(q, i32(rp.plane)), 0).r * 255.0));
    }
    return vec4<f32>(f32(q) / 255.0, 0.0, 0.0, 1.0);
}

// ---- compose ----------------------------------------------------------------

struct ComposeParams {
    opacity: vec4<f32>, // x: the layer's opacity (the rest is padding)
    matte: vec4<f32>,   // the black bars' Y, U, V (0..255), after the `eq` tables
    origin: vec2<i32>,  // where the layer's top-left lands on the canvas
    layer: vec2<i32>,   // the layer as overlaid (the rotated box, or the picture)
    pic: vec2<i32>,     // the picture before rotation
    cpic: vec2<i32>,    // ... and its chroma planes
    clayer: vec2<i32>,  // the layer's chroma planes
    rot: vec2<f32>,     // cos, sin of the clockwise angle
    pic_at: vec2<i32>,  // where the picture sits in the layer (a letterbox matte)
    pic_shows: vec2<i32>,
    rotate: u32,
    tail: u32,          // 1: draw only the chroma of an odd layer's last half block
    has_alpha: u32,     // 1: the layer's alpha is a plane (a mask), bound as `atex`
    edge: u32,          // 1: `geq` read the layer's last column and row one in (the mask's filter, on some builds)
    // `fade`'s integer factors (65535: not running), then the black level it takes for the layer's
    // pixel format and the layer's alpha before the ramp (0..255).
    fade_in: u32,
    fade_out: u32,
    black: u32,
    alpha: u32,
    // The dissolve's `fade alpha=1` factor (65535: not running).
    ramp: u32,
    pad2: u32,
    pad3: u32,
    pad4: u32,
}

@group(0) @binding(0) var<uniform> cp: ComposeParams;
@group(0) @binding(1) var ytex: texture_2d<f32>;
@group(0) @binding(2) var utex: texture_2d<f32>;
@group(0) @binding(3) var vtex: texture_2d<f32>;

// FFmpeg's `rotate` (bilinear, centre-to-centre): the sample for output pixel
// `o` of a `outsz` box from a `insz` picture. Returns (value 0..255, coverage).
// A source coordinate whose floor lies in [-1, size] is inside — the one-pixel
// apron `rotate` extends the edge by — and anything beyond is left empty.
// `edge`: a chroma plane. FFmpeg leaves chroma beyond the apron zeroed (a deep
// green) under an alpha that is only partly zero there; the compositor's alpha
// comes from luma alone, so chroma there is clamped to the picture's edge
// instead of read as green.
fn rot_sample(tex: texture_2d<f32>, o: vec2<i32>, outsz: vec2<i32>, insz: vec2<i32>, edge: bool) -> vec2<f32> {
    let c = cp.rot.x;
    let s = cp.rot.y;
    let dx = f32(o.x) - f32(outsz.x - 1) * 0.5;
    let dy = f32(o.y) - f32(outsz.y - 1) * 0.5;
    var sx = c * dx + s * dy + f32(insz.x - 1) * 0.5;
    var sy = -s * dx + c * dy + f32(insz.y - 1) * 0.5;
    var fx = floor(sx);
    var fy = floor(sy);
    var cov = 1.0;
    if (fx < -1.0 || fx > f32(insz.x) || fy < -1.0 || fy > f32(insz.y)) {
        if (!edge) {
            return vec2<f32>(0.0, 0.0);
        }
        cov = 0.0;
        sx = clamp(sx, 0.0, f32(insz.x - 1));
        sy = clamp(sy, 0.0, f32(insz.y - 1));
        fx = floor(sx);
        fy = floor(sy);
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
    return vec2<f32>((top + (bot - top) * ty) * 255.0, cov);
}

// The luma of layer pixel `l` (0..255) and its coverage, matte and rotation
// applied. Only an identity layer has a matte, and it never rotates.
fn layer_y(l: vec2<i32>) -> vec2<f32> {
    if (cp.rotate != 0u) {
        return rot_sample(ytex, l, cp.layer, cp.pic, false);
    }
    let p = l - cp.pic_at;
    if (p.x < 0 || p.y < 0 || p.x >= cp.pic_shows.x || p.y >= cp.pic_shows.y) {
        return vec2<f32>(cp.matte.x, 1.0);
    }
    return vec2<f32>(textureLoad(ytex, p, 0).r * 255.0, 1.0);
}

// The chroma (U, V) of layer chroma block `lc`.
fn layer_uv(lc: vec2<i32>) -> vec2<f32> {
    if (cp.rotate != 0u) {
        return vec2<f32>(
            rot_sample(utex, lc, cp.clayer, cp.cpic, true).x,
            rot_sample(vtex, lc, cp.clayer, cp.cpic, true).x,
        );
    }
    // The picture's offset in the layer is even (`pad` rounds it down to even),
    // so layer and picture chroma grids line up.
    let p = lc * 2 - cp.pic_at;
    if (p.x < 0 || p.y < 0 || p.x >= cp.pic_shows.x || p.y >= cp.pic_shows.y) {
        return vec2<f32>(cp.matte.y, cp.matte.z);
    }
    let pc = vec2<i32>(p.x / 2, p.y / 2);
    return vec2<f32>(textureLoad(utex, pc, 0).r * 255.0, textureLoad(vtex, pc, 0).r * 255.0);
}

// `fade`'s arithmetic (libavfilter `vf_fade.c`, identical in 6.1 and 9.0): the factor is
// `UINT16_MAX * i / N`, a luma sample goes toward the black level, chroma toward 128 with its own
// rounding constant, alpha toward 0. A factor of 65535 means the filter does not run at all.
fn fade_luma(y: i32, f: u32) -> i32 {
    let bl = i32(cp.black);
    return clamp((((y - bl) * i32(f)) + (bl << 16u) + 32768) >> 16u, 0, 255);
}

fn fade_chroma(c: i32, f: u32) -> i32 {
    return clamp((((c - 128) * i32(f)) + 8421367) >> 16u, 0, 255);
}

// The picture of the layer at canvas pixel `frag`: Y, U, V as the bytes `overlay` would see (the
// rotation, the letterbox matte and `fade` applied), and the luma pixel's coverage in w (a negative
// w: this pass does not draw the pixel).
fn layer_pixel(frag: vec4<f32>) -> vec4<f32> {
    let l = vec2<i32>(frag.xy) - cp.origin;
    let inside = l.x >= 0 && l.y >= 0 && l.x < cp.layer.x && l.y < cp.layer.y;
    if (cp.tail == 0u) {
        if (!inside) {
            return vec4<f32>(0.0, 0.0, 0.0, -1.0);
        }
    } else {
        // Only the pixels just beyond an odd layer's last column / row (and the
        // corner between them), where the layer's final chroma block reaches.
        let tx = l.x == cp.layer.x && (cp.layer.x & 1) == 1;
        let ty = l.y == cp.layer.y && (cp.layer.y & 1) == 1;
        let in_x = l.x >= 0 && l.x < cp.layer.x;
        let in_y = l.y >= 0 && l.y < cp.layer.y;
        if (!((tx && in_y) || (ty && in_x) || (tx && ty))) {
            return vec4<f32>(0.0, 0.0, 0.0, -1.0);
        }
    }
    // Chroma is shared by 2x2 luma pixels (the layer's origin is even, so the
    // grids line up); the final conversion replicates it, as swscale's does.
    var lc = vec2<i32>(l.x / 2, l.y / 2);
    var ly = l;
    if (cp.edge != 0u) {
        // `geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)'` on a build that clamps its read to `w - 2` /
        // `h - 2`: each plane's last column and row are their neighbours' (the planes are the layer's, the
        // padded frame of a letterboxed picture included).
        ly = min(l, cp.layer - vec2<i32>(2, 2));
        lc = min(lc, cp.clayer - vec2<i32>(2, 2));
    }
    let uv = layer_uv(lc);
    var u = i32(round(uv.x));
    var v = i32(round(uv.y));
    var y = 0;
    var cov = 1.0;
    // `overlay` blends whole chroma samples, so the chroma block an odd layer ends in reaches one pixel
    // past it — with the luma left to the layers below (the tail pass writes only U and V).
    if (cp.tail == 0u) {
        let sy = layer_y(ly);
        y = i32(round(sy.x));
        cov = sy.y;
    }
    // `fade` runs after `rotate`, on the 8-bit picture: the clip's own fade-in and out (a dip's
    // halves are merged into them), each one a filter of its own.
    if (cp.fade_in < 65535u) {
        y = fade_luma(y, cp.fade_in);
        u = fade_chroma(u, cp.fade_in);
        v = fade_chroma(v, cp.fade_in);
    }
    if (cp.fade_out < 65535u) {
        y = fade_luma(y, cp.fade_out);
        u = fade_chroma(u, cp.fade_out);
        v = fade_chroma(v, cp.fade_out);
    }
    return vec4<f32>(f32(y), f32(u), f32(v), cov);
}

@fragment
fn fs_compose(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = layer_pixel(frag);
    if (px.w < 0.0) {
        discard;
    }
    // Blend in YUV, like FFmpeg's overlay: the canvas holds the encoded Y, U, V
    // values (chroma replicated per pixel) and the blend factors act on each
    // channel alike. Converting each layer to RGB first would clamp it before
    // the blend, and a legal-but-out-of-gamut picture (saturated test patterns,
    // super-whites) would then composite differently.
    return vec4<f32>(px.x / 255.0, px.y / 255.0, px.z / 255.0, px.w * cp.opacity.x);
}

// The canvas before this layer, for a layer whose alpha is below 255 and so cannot be the hardware's
// blend: FFmpeg's own, in integers.
@group(0) @binding(4) var prev: texture_2d<f32>;
// The mask's alpha plane (a 1x1 stand-in when the layer has none: `cp.has_alpha` is 0).
@group(0) @binding(5) var atex: texture_2d<f32>;

// `overlay`'s blend of a `yuva420p` picture on a `yuv420p` one (`blend_plane`): `d * (255 - a) + s * a`
// divided by 255 with `FAST_DIV255`, rounding to nearest. Exact for a = 255 (the source) and 0 (the
// destination). 6.1 and 9.0 alike, on all three planes for a uniform alpha.
fn blend255(d: u32, s: u32, a: u32) -> u32 {
    return ((d * (255u - a) + s * a + 128u) * 257u) >> 16u;
}

// The alpha of layer pixel `l` (0..255) as `overlay` finds it: the mask's plane when the layer has one,
// else the layer's one alpha, then the dissolve's `fade alpha=1`, which runs on the plane.
fn alpha_at(l: vec2<i32>) -> u32 {
    var a = cp.alpha;
    if (cp.has_alpha != 0u) {
        a = u32(round(textureLoad(atex, l, 0).r * 255.0));
    }
    if (cp.ramp < 65535u) {
        a = u32(((i32(a) * i32(cp.ramp)) + 32768) >> 16u);
    }
    return a;
}

// `overlay`'s alpha for the chroma sample of block `lc` (`blend_plane`, 6.1 and 9.0 alike): the four
// alphas of its 2x2 luma block, `>> 2`, wherever the block has a neighbour sample to its right and
// below; the last chroma column and row (the sample counts are the chroma plane's) average what they
// have — and a last column ignores its own second luma column, a quirk of the filter, kept.
fn chroma_alpha_at(lc: vec2<i32>) -> u32 {
    let p = lc * 2;
    let right = lc.x + 1 < cp.clayer.x;
    let below = lc.y + 1 < cp.clayer.y;
    let a00 = alpha_at(p);
    if (right && below) {
        return (a00 + alpha_at(p + vec2<i32>(1, 0)) + alpha_at(p + vec2<i32>(0, 1)) + alpha_at(p + vec2<i32>(1, 1))) >> 2u;
    }
    var ah = a00;
    if (right) {
        ah = (a00 + alpha_at(p + vec2<i32>(1, 0))) >> 1u;
    }
    var av = a00;
    if (below) {
        av = (a00 + alpha_at(p + vec2<i32>(0, 1))) >> 1u;
    }
    return (av + ah) >> 1u;
}

@fragment
fn fs_compose_exact(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = layer_pixel(frag);
    if (px.w < 0.0) {
        discard;
    }
    // The alpha plane: opacity (colorchannelmixer), then the dissolve's ramp (`fade alpha=1`), and
    // wherever a rotation left the picture there is none. A mask makes it a plane: luma blends with
    // its own sample, and each chroma sample with the average `overlay` takes over its block.
    var a_y = 0u;
    var a_c = 0u;
    if (cp.has_alpha != 0u) {
        let l = vec2<i32>(frag.xy) - cp.origin;
        a_y = alpha_at(l);
        a_c = chroma_alpha_at(l / 2);
    } else {
        var a = cp.alpha;
        if (cp.ramp < 65535u) {
            a = u32(((i32(a) * i32(cp.ramp)) + 32768) >> 16u);
        }
        if (px.w < 0.5) {
            a = 0u;
        }
        a_y = a;
        a_c = a;
    }
    let d = vec3<u32>(round(textureLoad(prev, vec2<i32>(frag.xy), 0).rgb * 255.0));
    let s = vec3<u32>(px.xyz);
    let out = vec3<u32>(blend255(d.x, s.x, a_y), blend255(d.y, s.y, a_c), blend255(d.z, s.z, a_c));
    return vec4<f32>(vec3<f32>(out) / 255.0, 1.0);
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
