// What FFmpeg does to a translucent layer before it is overlaid: the layer goes
// yuva420p -> argb -> yuva420p, because `colorchannelmixer` only takes RGB. Four
// passes over the layer's planes, in swscale's own integer arithmetic (the scalar
// reference, with the derivation and the measurements, is src/roundtrip.rs):
//
//   fs_rgb       Y, U, V  -> R, G, B   swscale's yuv2rgb tables, the layer's matrix
//   fs_luma      R, G, B  -> Y         the composite's matrix (BT.601 unless negotiated)
//   fs_chroma_h  R, G, B  -> U, V      sums of horizontal pixel pairs, 15 bits
//   fs_chroma_v  U, V     -> U, V      swscale's vertical bicubic, 2:1, 12-bit weights

struct P {
    k: vec4<i32>,     // crv, cbu, cgu, cgv: the chroma coefficients, scaled by cy
    m: vec4<i32>,     // cy, oy, the luma offset in the table, unused
    ky: vec4<i32>,    // the way back (RGB -> YCbCr): luma weights ry, gy, by (15 bits)
    ku: vec4<i32>,    // ... and ru, gu, bu
    kv: vec4<i32>,    // ... and rv, gv, bv
    size: vec2<i32>,  // the picture (luma size)
    csize: vec2<i32>, // ... and its chroma planes
    channel: i32,     // fs_chroma_v: 0 = U, 1 = V
    taps: i32,        // fs_chroma_v: taps per output row
    pad0: i32,
    pad1: i32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var t1: texture_2d<f32>;
@group(0) @binding(2) var t2: texture_2d<f32>;
@group(0) @binding(3) var t3: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

fn byte(v: f32) -> i32 {
    return i32(round(v * 255.0));
}

// `y_table[i]`: the luma term at table index `i`, clipped to a byte.
fn ytab(i: i32) -> i32 {
    let yb = -(384 << 16u) - 512 * p.m.x - p.m.y + i * p.m.x;
    return clamp((yb + 0x8000) >> 16u, 0, 255);
}

@fragment
fn fs_rgb(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec2<i32>(o.x / 2, o.y / 2);
    let y = byte(textureLoad(t1, o, 0).r);
    let u = byte(textureLoad(t2, c, 0).r);
    let v = byte(textureLoad(t3, c, 0).r);
    let crv = p.k.x;
    let cbu = p.k.y;
    let cgu = p.k.z;
    let cgv = p.k.w;
    let base = p.m.z + y;
    let r = ytab(base - (crv >> 9u) + ((v * crv) >> 16u));
    let g = ytab(base - (cgu >> 9u) + ((u * cgu) >> 16u) - (cgv >> 9u) + ((v * cgv) >> 16u));
    let b = ytab(base - (cbu >> 9u) + ((u * cbu) >> 16u));
    return vec4<f32>(f32(r) / 255.0, f32(g) / 255.0, f32(b) / 255.0, 1.0);
}

@fragment
fn fs_luma(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec3<i32>(round(textureLoad(t1, o, 0).rgb * 255.0));
    let y14 = (p.ky.x * c.x + p.ky.y * c.y + p.ky.z * c.z + (32 << 14u) + (1 << 8u)) >> 9u;
    let y8 = clamp((y14 + 32) >> 6u, 0, 255);
    return vec4<f32>(f32(y8) / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_chroma_h(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    // The pair of pixels this chroma sample covers. (An odd picture never gets
    // here — the geometry refuses a translucent layer of odd size — so the clamp
    // only keeps the read in range.)
    let x0 = min(2 * o.x, p.size.x - 1);
    let x1 = min(2 * o.x + 1, p.size.x - 1);
    let a = vec3<i32>(round(textureLoad(t1, vec2<i32>(x0, o.y), 0).rgb * 255.0));
    let b = vec3<i32>(round(textureLoad(t1, vec2<i32>(x1, o.y), 0).rgb * 255.0));
    let s = a + b;
    let u = (p.ku.x * s.x + p.ku.y * s.y + p.ku.z * s.z + (256 << 15u) + (1 << 9u)) >> 10u;
    let v = (p.kv.x * s.x + p.kv.y * s.y + p.kv.z * s.z + (256 << 15u) + (1 << 9u)) >> 10u;
    // 15 bits, as the scaler's intermediate (value * 128).
    return vec4<f32>(f32(u * 2), f32(v * 2), 0.0, 1.0);
}

// t1: the 15-bit chroma (width csize.x, one row per picture row); t2: the filter,
// one row per output row — the window start, then `taps` 12-bit weights.
@fragment
fn fs_chroma_v(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let pos = i32(textureLoad(t2, vec2<i32>(0, o.y), 0).r);
    var acc = 64 << 12u;
    for (var j = 0; j < p.taps; j = j + 1) {
        let w = i32(textureLoad(t2, vec2<i32>(1 + j, o.y), 0).r);
        let r = clamp(pos + j, 0, p.size.y - 1);
        let s = textureLoad(t1, vec2<i32>(o.x, r), 0);
        var v = s.r;
        if (p.channel == 1) {
            v = s.g;
        }
        acc = acc + w * i32(v);
    }
    let q = clamp(acc >> 19u, 0, 255);
    return vec4<f32>(f32(q) / 255.0, 0.0, 0.0, 1.0);
}
