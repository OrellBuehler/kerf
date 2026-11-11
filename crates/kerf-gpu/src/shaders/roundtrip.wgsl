// What FFmpeg does to a translucent layer before it is overlaid: the layer goes
// yuva420p -> argb -> yuva420p, because `colorchannelmixer` only takes RGB. Four
// passes over the layer's planes, in swscale's own integer arithmetic (the scalar
// reference, with the derivation and the measurements, is src/roundtrip.rs):
//
//   fs_rgb       Y, U, V  -> R, G, B   swscale's yuv2rgb tables, the layer's matrix
//   fs_luma      R, G, B  -> Y         the composite's matrix (BT.601 unless negotiated)
//   fs_chroma_h  R, G, B  -> U, V      sums of horizontal pixel pairs, 15 bits
//   fs_chroma_v_x86 U, V  -> U, V      swscale's vertical bicubic, 2:1, 12-bit weights, in the x86 `yuv2yuvX` kernel
//
// and for a picture with an odd side (the arithmetic and the measurements are in `src/roundtrip.rs`, `yuv_to_rgb` / `rgb_to_yuv`):
//
//   fs_rgb          an odd width: the last column is the zeroed buffer's, RGB 0 (`zero_col`)
//   fs_chroma_up    an odd height: the chroma plane enlarged to the picture's height (12-bit weights, `>> 19`), then fs_rgb
//   fs_wide, fs_tall, fs_rgb_full
//                   odd height and width: the chroma enlarged to the width (14-bit weights, 15 bits), to the height (centred on 0),
//                   and the colour of `yuv2rgb_write_full` in 32-bit wrapping integers
//   fs_chroma_h     an odd width: the pair's second pixel past the line is 0, 0, 0 (6.1) ...
//   fs_chroma_h_scaled  ... or the chroma of every pixel scaled to the chroma plane's width (9.0)

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
    white_in: i32,    // fs_rgb: a dip to white's `fade` factors in the RGB (65535: not running)
    white_out: i32,
    start: i32,       // fs_chroma_v_x86: the sum's start, `(64 + 8 * (taps - 1)) >> 4` for the filter size aligned to 2
    zero_col: i32,    // fs_rgb: 1: the last column is not written by the conversion (an odd width, an even height): RGB 0
    full_rows: i32,   // fs_rgb: 1: the chroma planes are already as tall as the picture (an odd height), else a row of luma uses chroma row y / 2
    pad2: i32,
    fc: vec4<i32>,    // fs_rgb_full: yCoeff, yOffset, v2r, v2g (`yuv2rgb_write_full`)
    fc2: vec4<i32>,   // ... u2g, u2b
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
    var row = o.y / 2;
    if (p.full_rows != 0) {
        row = o.y;
    }
    let c = vec2<i32>(o.x / 2, row);
    let y = byte(textureLoad(t1, o, 0).r);
    let u = byte(textureLoad(t2, c, 0).r);
    let v = byte(textureLoad(t3, c, 0).r);
    let crv = p.k.x;
    let cbu = p.k.y;
    let cgu = p.k.z;
    let cgv = p.k.w;
    let base = p.m.z + y;
    var rgb = vec3<i32>(
        ytab(base - (crv >> 9u) + ((v * crv) >> 16u)),
        ytab(base - (cgu >> 9u) + ((u * cgu) >> 16u) - (cgv >> 9u) + ((v * cgv) >> 16u)),
        ytab(base - (cbu >> 9u) + ((u * cbu) >> 16u)),
    );
    // The unscaled converter's tail loop stops at an even width: the last pixel is as the zeroed buffer was.
    if (p.zero_col != 0 && o.x == p.size.x - 1) {
        rgb = vec3<i32>(0);
    }
    // A dip to white is a `fade c=white`, which only takes RGB: it runs here, between the two
    // conversions, one filter for the fade-in and one for the fade-out.
    if (p.white_in < 65535) {
        rgb = fade_white(rgb, p.white_in);
    }
    if (p.white_out < 65535) {
        rgb = fade_white(rgb, p.white_out);
    }
    return vec4<f32>(vec3<f32>(rgb) / 255.0, 1.0);
}

// `vf_fade.c`'s `filter_slice_rgb` toward white: `((c - 255) * factor + (255 << 16) + 32768) >> 16`.
fn fade_white(c: vec3<i32>, f: i32) -> vec3<i32> {
    return clamp(((c - vec3<i32>(255)) * f + vec3<i32>((255 << 16u) + 32768)) >> vec3<u32>(16u), vec3<i32>(0), vec3<i32>(255));
}

// A dip to white's `fade` over the `rgb24` picture the x86 kernel made (`grade.wgsl`'s `fs_rgb`): one filter for the fade-in and one
// for the fade-out, as `fs_rgb` runs them for the translucent round trip.
@fragment
fn fs_white(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    var rgb = vec3<i32>(round(textureLoad(t1, o, 0).rgb * 255.0));
    if (p.white_in < 65535) {
        rgb = fade_white(rgb, p.white_in);
    }
    if (p.white_out < 65535) {
        rgb = fade_white(rgb, p.white_out);
    }
    return vec4<f32>(vec3<f32>(rgb) / 255.0, 1.0);
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
    // The pair of pixels this chroma sample covers. The second of an odd width's last pair is past the line, in the row's
    // padding, which the graph's frame pool zeroed: a pixel of 0, 0, 0.
    var a = vec3<i32>(0);
    var b = vec3<i32>(0);
    if (2 * o.x < p.size.x) {
        a = vec3<i32>(round(textureLoad(t1, vec2<i32>(2 * o.x, o.y), 0).rgb * 255.0));
    }
    if (2 * o.x + 1 < p.size.x) {
        b = vec3<i32>(round(textureLoad(t1, vec2<i32>(2 * o.x + 1, o.y), 0).rgb * 255.0));
    }
    let s = a + b;
    let u = (p.ku.x * s.x + p.ku.y * s.y + p.ku.z * s.z + (256 << 15u) + (1 << 9u)) >> 10u;
    let v = (p.kv.x * s.x + p.kv.y * s.y + p.kv.z * s.z + (256 << 15u) + (1 << 9u)) >> 10u;
    // 15 bits, as the scaler's intermediate (value * 128).
    return vec4<f32>(f32(u * 2), f32(v * 2), 0.0, 1.0);
}

// An odd width on 9.0: the chroma of every pixel (`rgb24ToUV_c`, 14 bits) brought to the chroma plane's width by the horizontal bicubic
// (14-bit weights, `>> 13`, clipped at 15 bits), the edge taps clamped to the last pixel. `t2` holds the table, a row per chroma column.
@fragment
fn fs_chroma_h_scaled(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let pos = i32(textureLoad(t2, vec2<i32>(0, o.x), 0).r);
    var su = 0;
    var sv = 0;
    for (var j = 0; j < p.taps; j = j + 1) {
        let w = i32(textureLoad(t2, vec2<i32>(1 + j, o.x), 0).r);
        let col = clamp(pos + j, 0, p.size.x - 1);
        let c = vec3<i32>(round(textureLoad(t1, vec2<i32>(col, o.y), 0).rgb * 255.0));
        let u14 = (p.ku.x * c.x + p.ku.y * c.y + p.ku.z * c.z + (256 << 14u) + (1 << 8u)) >> 9u;
        let v14 = (p.kv.x * c.x + p.kv.y * c.y + p.kv.z * c.z + (256 << 14u) + (1 << 8u)) >> 9u;
        su = su + w * u14;
        sv = sv + w * v14;
    }
    return vec4<f32>(f32(min(su >> 13u, 32767)), f32(min(sv >> 13u, 32767)), 0.0, 1.0);
}

// An odd height, `yuva420p -> argb` by the scaler: a chroma plane (`t1`, 8 bits) enlarged to the picture's height by the vertical bicubic
// (12-bit weights, a row of `t2` per output row: the window's start, then the weights), the sum started at 2^18 and shifted by 19; the
// colour tables clip the result to a byte.
@fragment
fn fs_chroma_up(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let pos = i32(textureLoad(t2, vec2<i32>(0, o.y), 0).r);
    var acc = 1 << 18;
    for (var j = 0; j < p.taps; j = j + 1) {
        let w = i32(textureLoad(t2, vec2<i32>(1 + j, o.y), 0).r);
        let r = clamp(pos + j, 0, p.csize.y - 1);
        acc = acc + w * (byte(textureLoad(t1, vec2<i32>(o.x, r), 0).r) << 7u);
    }
    return vec4<f32>(f32(clamp(acc >> 19u, 0, 255)) / 255.0, 0.0, 0.0, 1.0);
}

// An odd height and width (`SWS_FULL_CHR_H_INT` is forced on an RGB output of odd width): the chroma plane (`t1`) is brought to the
// picture's width first, by the horizontal bicubic (14-bit weights, a row of `t2` per output column), 15 bits clipped at the top.
@fragment
fn fs_wide(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let pos = i32(textureLoad(t2, vec2<i32>(0, o.x), 0).r);
    var acc = 0;
    for (var j = 0; j < p.taps; j = j + 1) {
        let w = i32(textureLoad(t2, vec2<i32>(1 + j, o.x), 0).r);
        let col = clamp(pos + j, 0, p.csize.x - 1);
        acc = acc + w * byte(textureLoad(t1, vec2<i32>(col, o.y), 0).r);
    }
    return vec4<f32>(f32(min(acc >> 7u, 32767)), 0.0, 0.0, 1.0);
}

// ... and then to the picture's height by the vertical bicubic (12-bit weights), centred on 0 and with 9 more fractional bits than a byte
// (`yuv2rgb_full_X_c_template`: `(2^9 - (128 << 19) + sum) >> 10`). `t1` is the wide plane, `t2` the table.
@fragment
fn fs_tall(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let pos = i32(textureLoad(t2, vec2<i32>(0, o.y), 0).r);
    var acc = (1 << 9) - (128 << 19);
    for (var j = 0; j < p.taps; j = j + 1) {
        let w = i32(textureLoad(t2, vec2<i32>(1 + j, o.y), 0).r);
        let r = clamp(pos + j, 0, p.csize.y - 1);
        acc = acc + w * i32(textureLoad(t1, vec2<i32>(o.x, r), 0).r);
    }
    return vec4<f32>(f32(acc >> 10u), 0.0, 0.0, 1.0);
}

// `yuv2rgb_write_full`: the colour of a pixel from its luma (`t1`, a byte) and the chroma of `fs_tall` (`t2`, `t3`), in 32-bit integers
// that wrap, the three clipped to 30 bits.
@fragment
fn fs_rgb_full(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let y = byte(textureLoad(t1, o, 0).r) << 9u;
    let u = i32(textureLoad(t2, o, 0).r);
    let v = i32(textureLoad(t3, o, 0).r);
    let yy = (y - p.fc.y) * p.fc.x + (1 << 21);
    let r = yy + v * p.fc.z;
    let g = yy + v * p.fc.w + u * p.fc2.x;
    let b = yy + u * p.fc2.y;
    let rgb = clamp(vec3<i32>(r, g, b), vec3<i32>(0), vec3<i32>((1 << 30) - 1)) >> vec3<u32>(22u);
    return vec4<f32>(vec3<f32>(rgb) / 255.0, 1.0);
}

// The vertical pass of the grade's way back (src/grade.rs): swscale's x86 `yuv2yuvX` for every row but the last
// chroma row, which `swscale.c` gives to the C kernel (`dstY >= dstH - 2`). Each tap is `pmulhw`, the high half of the
// 16-bit product, summed from `start`, then `>> 3`.
@fragment
fn fs_chroma_v_x86(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let pos = i32(textureLoad(t2, vec2<i32>(0, o.y), 0).r);
    let last = o.y == p.csize.y - 1;
    var acc = p.start;
    if (last) {
        acc = 64 << 12u;
    }
    for (var j = 0; j < p.taps; j = j + 1) {
        let w = i32(textureLoad(t2, vec2<i32>(1 + j, o.y), 0).r);
        let r = clamp(pos + j, 0, p.size.y - 1);
        let s = textureLoad(t1, vec2<i32>(o.x, r), 0);
        var v = s.r;
        if (p.channel == 1) {
            v = s.g;
        }
        if (last) {
            acc = acc + w * i32(v);
        } else {
            acc = acc + ((w * i32(v)) >> 16u);
        }
    }
    var q = clamp(acc >> 3u, 0, 255);
    if (last) {
        q = clamp(acc >> 19u, 0, 255);
    }
    return vec4<f32>(f32(q) / 255.0, 0.0, 0.0, 1.0);
}

// One plane of `rgb24 -> yuv444p` (the explicit `scale` a tagged picture's grade ends in), the unscaled vertical
// pass included: channel 0 is luma, 1 and 2 are U and V, each at the picture's size.
@fragment
fn fs_plane444(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec3<i32>(round(textureLoad(t1, o, 0).rgb * 255.0));
    var q: i32;
    if (p.channel == 0) {
        let y14 = (p.ky.x * c.x + p.ky.y * c.y + p.ky.z * c.z + (32 << 14u) + (1 << 8u)) >> 9u;
        q = clamp((y14 + 32) >> 6u, 0, 255);
    } else {
        var k = p.ku;
        if (p.channel == 2) {
            k = p.kv;
        }
        let c14 = (k.x * c.x + k.y * c.y + k.z * c.z + (256 << 14u) + (1 << 8u)) >> 9u;
        q = clamp((c14 * 2 + 64) >> 7u, 0, 255);
    }
    return vec4<f32>(f32(q) / 255.0, 0.0, 0.0, 1.0);
}

// `ff_rgb24toyv12_c` (`rgb2rgb_template.c`), what swscale gives an unscaled `bgr24 -> yuv420p` of even width: the luma of
// a pixel is `((ry*r + gy*g + by*b) >> 15) + 16`, and a chroma sample is made from the channel means of its 2x2 block
// (`>> 2`) the same way, plus 128. (An `unsigned int` stored in a byte in the C: the low byte of the sum.)
@fragment
fn fs_luma24(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec3<i32>(round(textureLoad(t1, o, 0).rgb * 255.0));
    let y = (((p.ky.x * c.x + p.ky.y * c.y + p.ky.z * c.z) >> 15u) + 16) & 255;
    return vec4<f32>(f32(y) / 255.0, 0.0, 0.0, 1.0);
}

// channel 0: U, 1: V; `o` is a chroma sample, the block is the four pixels from `2 * o`.
@fragment
fn fs_chroma24(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let at = o * 2;
    let a = vec3<i32>(round(textureLoad(t1, at, 0).rgb * 255.0));
    let b = vec3<i32>(round(textureLoad(t1, at + vec2<i32>(1, 0), 0).rgb * 255.0));
    let c = vec3<i32>(round(textureLoad(t1, at + vec2<i32>(0, 1), 0).rgb * 255.0));
    let d = vec3<i32>(round(textureLoad(t1, at + vec2<i32>(1, 1), 0).rgb * 255.0));
    let s = (a + b + c + d) >> vec3<u32>(2u);
    var k = p.ku;
    if (p.channel == 1) {
        k = p.kv;
    }
    let q = (((k.x * s.x + k.y * s.y + k.z * s.z) >> 15u) + 128) & 255;
    return vec4<f32>(f32(q) / 255.0, 0.0, 0.0, 1.0);
}
