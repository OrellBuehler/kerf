// A blended clip (`engine/cli/blend.rs`): the chain `alphamerge, scale=neighbor, format=gbrap, blend, scale=neighbor,
// format=yuva420p, overlay`, in integers (`src/blend.rs` holds the model and what it was held to).
//
//   fs_blend_rgb   both frames to RGB (`yuv2gbrp_full_X_c`) and the mode on each channel   -> Rgba8 (R, G, B)
//   fs_blend_over  the mix back to 4:2:0 (`planar_rgb_to_y` / `_uv`, chroma from the bottom row of each 2x2 block, the
//                  sum of its pair) and `overlay`'s blend of it onto the composite by the clip's alpha -> the canvas
//
// The canvas holds Y, U, V per pixel with chroma repeated over each 2x2 block, so a pixel's own texel is its block's.

struct P {
    // `Tables::words`: y_off, y_coeff, v2r, v2g, u2g, u2b, then ry gy by, ru gu bu, rv gv bv, and a zero.
    k: array<vec4<i32>, 4>,
    origin: vec2<i32>,  // where the clip's top-left lands on the canvas
    layer: vec2<i32>,   // the clip's size
    canvas: vec2<i32>,  // the canvas (even)
    mode: u32,
    has_alpha: u32,     // 1: the clip's alpha is a plane (a mask, a key), bound as `atex`
    alpha: u32,         // the clip's alpha (0..255) before the ramps
    legal: u32,         // 1: the clip's colours are held to the legal range (`lutyuv`'s `clipval`), as the chain does before it is laid
    pad0: u32,
    pad1: u32,
    ramps: array<vec4<u32>, 2>, // the clip's `fade alpha=1` factors, in the order the chain runs them (65535: not running)
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var top: texture_2d<f32>;      // the composite with the clip laid on it, opaque
@group(0) @binding(2) var bottom: texture_2d<f32>;   // the composite as it was
@group(0) @binding(3) var mixed: texture_2d<f32>;    // fs_blend_over: the mode's RGB
@group(0) @binding(4) var atex: texture_2d<f32>;     // the clip's alpha plane, when it has one

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

fn k(i: u32) -> i32 {
    return p.k[i / 4u][i % 4u];
}

fn byte3(v: vec4<f32>) -> vec3<i32> {
    return vec3<i32>(round(v.rgb * 255.0));
}

// `yuv2gbrp_full_X_c`: the sums are 32-bit integers that wrap, as the C's do (a saturated blue overflows), so they are
// done unsigned and read back as signed; each is clipped to 30 bits and shifted down by 22.
fn wrap(a: i32, b: i32) -> u32 {
    return u32(a) * u32(b);
}

fn yuv_to_rgb(yuv: vec3<i32>) -> vec3<i32> {
    let y = yuv.x * 512;
    let u = (yuv.y - 128) * 512;
    let v = (yuv.z - 128) * 512;
    let luma = (u32(y - k(0u)) * u32(k(1u))) + 2097152u;
    let r = luma + wrap(v, k(2u));
    let g = luma + wrap(v, k(3u)) + wrap(u, k(4u));
    let b = luma + wrap(u, k(5u));
    return vec3<i32>(
        clamp(bitcast<i32>(r), 0, 1073741823) >> 22u,
        clamp(bitcast<i32>(g), 0, 1073741823) >> 22u,
        clamp(bitcast<i32>(b), 0, 1073741823) >> 22u,
    );
}

// `blend_<mode>_8bit` of vf_blend.c, `a` the clip (the top input) and `b` the composite. The mode numbers are
// `blend::mode_index`'s.
fn mode_channel(a: i32, b: i32) -> i32 {
    switch p.mode {
        case 0u: { return (a * b) / 255; }
        case 1u: { return 255 - (255 - a) * (255 - b) / 255; }
        // FFmpeg's `hardlight` (kerf's Overlay): `b` decides.
        case 2u: {
            if (b < 128) { return 2 * ((b * a) / 255); }
            return 255 - 2 * ((255 - b) * (255 - a) / 255);
        }
        // FFmpeg's `overlay` (kerf's HardLight): `a` decides.
        case 3u: {
            if (a < 128) { return 2 * ((a * b) / 255); }
            return 255 - 2 * ((255 - a) * (255 - b) / 255);
        }
        case 4u: { return min(255, a + b); }
        case 5u: { return min(a, b); }
        case 6u: { return max(a, b); }
        case 7u: { return abs(a - b); }
        default: { return a + b - 2 * a * b / 255; }
    }
}

// The clip is laid opaque by `lutyuv=a=255`, whose Y, U and V are `clipval`: luma held to 16..235 and chroma to 16..240 (the
// legal range of a limited-range 8-bit picture), on every sample of the clip. Luma is the clip's inside its rectangle; a
// chroma block is the clip's wherever the clip touches it (the half block an odd clip ends in, too).
fn clip_colours(o: vec2<i32>, yuv: vec3<i32>) -> vec3<i32> {
    var c = yuv;
    if (p.legal == 0u) {
        return c;
    }
    let l = o - p.origin;
    if (l.x >= 0 && l.y >= 0 && l.x < p.layer.x && l.y < p.layer.y) {
        c.x = clamp(c.x, 16, 235);
    }
    let bx = (o.x / 2) * 2;
    let by = (o.y / 2) * 2;
    if (bx < p.origin.x + p.layer.x && bx + 1 >= p.origin.x && by < p.origin.y + p.layer.y && by + 1 >= p.origin.y) {
        c.y = clamp(c.y, 16, 240);
        c.z = clamp(c.z, 16, 240);
    }
    return c;
}

@fragment
fn fs_blend_rgb(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let t = yuv_to_rgb(clip_colours(o, byte3(textureLoad(top, o, 0))));
    let b = yuv_to_rgb(byte3(textureLoad(bottom, o, 0)));
    let m = vec3<i32>(mode_channel(t.x, b.x), mode_channel(t.y, b.y), mode_channel(t.z, b.z));
    return vec4<f32>(vec3<f32>(m) / 255.0, 1.0);
}

// `planar_rgb_to_y` and the 14-bit to 8-bit step: `(((ry r + gy g + by b + 0x80100) >> 9) + 32) >> 6`.
fn luma_of(rgb: vec3<i32>) -> i32 {
    let t = k(6u) * rgb.x + k(7u) * rgb.y + k(8u) * rgb.z + 0x80100;
    return clamp((((t >> 9u) + 32) >> 6u), 0, 255);
}

// The chroma of a block from the sum of the two pixels of its bottom row: `((ru R + gu G + bu B + 0x800200) >> 10) + 32) >> 6`.
fn chroma_of(sum: vec3<i32>) -> vec2<i32> {
    let u = k(9u) * sum.x + k(10u) * sum.y + k(11u) * sum.z + 0x800200;
    let v = k(12u) * sum.x + k(13u) * sum.y + k(14u) * sum.z + 0x800200;
    return vec2<i32>(clamp((((u >> 10u) + 32) >> 6u), 0, 255), clamp((((v >> 10u) + 32) >> 6u), 0, 255));
}

fn blend255(d: i32, s: i32, a: i32) -> i32 {
    return ((d * (255 - a) + s * a + 128) * 257) >> 16u;
}

// The clip's alpha at canvas pixel `o`: the layer's own (a uniform, or the mask's plane), then the clip's `fade alpha=1`
// steps, each its own rounding, in the order the chain runs them; 0 where the clip is not (`overlay` of a transparent canvas).
fn alpha_at(o: vec2<i32>) -> i32 {
    let l = o - p.origin;
    if (l.x < 0 || l.y < 0 || l.x >= p.layer.x || l.y >= p.layer.y) {
        return 0;
    }
    var a = i32(p.alpha);
    if (p.has_alpha != 0u) {
        a = i32(round(textureLoad(atex, l, 0).r * 255.0));
    }
    for (var i = 0u; i < 8u; i = i + 1u) {
        let r = p.ramps[i / 4u][i % 4u];
        if (r < 65535u) {
            a = ((a * i32(r)) + 32768) >> 16u;
        }
    }
    return a;
}

// `overlay`'s alpha for chroma block `b` of a whole-frame `yuva420p` (`blend_plane`): the four alphas of the block
// `>> 2` where it has a neighbour to its right and below; the last chroma column and row average what they have, and a
// last column ignores its own second luma column (a quirk of the filter, kept; `blend::chroma_alpha`).
fn chroma_alpha_at(b: vec2<i32>) -> i32 {
    let q = b * 2;
    let cw = p.canvas.x / 2;
    let ch = p.canvas.y / 2;
    let right = b.x + 1 < cw;
    let below = b.y + 1 < ch;
    let a00 = alpha_at(q);
    if (right && below) {
        return (a00 + alpha_at(q + vec2<i32>(1, 0)) + alpha_at(q + vec2<i32>(0, 1)) + alpha_at(q + vec2<i32>(1, 1))) >> 2u;
    }
    var ah = a00;
    if (right) {
        ah = (a00 + alpha_at(q + vec2<i32>(1, 0))) >> 1u;
    }
    var av = a00;
    if (below) {
        av = (a00 + alpha_at(q + vec2<i32>(0, 1))) >> 1u;
    }
    return (av + ah) >> 1u;
}

@fragment
fn fs_blend_over(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let d = byte3(textureLoad(bottom, o, 0));
    let a_y = alpha_at(o);
    let y_new = luma_of(byte3(textureLoad(mixed, o, 0)));
    let b = o / 2;
    let q = vec2<i32>(b.x * 2, b.y * 2 + 1);
    let sum = byte3(textureLoad(mixed, q, 0)) + byte3(textureLoad(mixed, q + vec2<i32>(1, 0), 0));
    let c = chroma_of(sum);
    let a_c = chroma_alpha_at(b);
    let out = vec3<i32>(blend255(d.x, y_new, a_y), blend255(d.y, c.x, a_c), blend255(d.z, c.y, a_c));
    return vec4<f32>(vec3<f32>(out) / 255.0, 1.0);
}
