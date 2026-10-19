// `vignette` (vf_vignette.c, its defaults) on one plane of a `yuv420p` picture: the integer arithmetic of
// `effects.rs` (`vignette_luma`, `vignette_chroma`, `lcg_at`), sample for sample. Luma is
// `(int)(f * s + dither)`, chroma `(int)(f * (s - 127) + 127 + dither)`, the product and the `+ 127` rounded to
// `float` here exactly (as integers: no backend may fuse them into one rounding), the dither the LCG's state at
// this sample's step over 2^32, added as the filter adds a `double`.

struct V {
    // The plane's size.
    size: vec2<u32>,
    // The factor map's row: the luma width.
    map_w: u32,
    // 1 for U and V: the factor of the block's top-left luma sample.
    chroma: u32,
    // The LCG step of this plane's first sample: `(n * samples_per_frame + plane offset) mod 2^32`.
    base: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> v: V;
@group(0) @binding(1) var src: texture_2d<f32>;
// The factors (`float` bits), one per luma sample, row by row.
@group(0) @binding(2) var<storage, read> fmap: array<u32>;

const LCG_A: u32 = 1664525u;
const LCG_C: u32 = 1013904223u;

// The LCG's state after `i` steps from 0, by halving.
fn lcg_at(i0: u32) -> u32 {
    var acc_a = 1u;
    var acc_c = 0u;
    var a = LCG_A;
    var c = LCG_C;
    var i = i0;
    loop {
        if (i == 0u) {
            break;
        }
        if ((i & 1u) == 1u) {
            acc_a = acc_a * a;
            acc_c = acc_c * a + c;
        }
        c = c * (a + 1u);
        a = a * a;
        i = i >> 1u;
    }
    return acc_c;
}

// `m` (non-zero) rounded to 24 significant bits, half to even: the mantissa and its shift.
fn round24(m: u32) -> vec2<u32> {
    let len = firstLeadingBit(m) + 1u;
    if (len <= 24u) {
        return vec2<u32>(m, 0u);
    }
    let sh = len - 24u;
    var q = m >> sh;
    let rem = m & ((1u << sh) - 1u);
    let half = 1u << (sh - 1u);
    if (rem > half || (rem == half && (q & 1u) == 1u)) {
        q = q + 1u;
    }
    if (q == (1u << 24u)) {
        return vec2<u32>(q >> 1u, sh + 1u);
    }
    return vec2<u32>(q, sh);
}

// `(int)(m * 2^e + state / 2^32)`, held to 255 (`e` is negative and at least -25 here).
fn floor_dithered(m: u32, e: i32, state: u32) -> u32 {
    if (m == 0u) {
        return 0u;
    }
    var int_part: u32;
    var frac_bits: u32;
    if (e >= 0) {
        int_part = m << u32(e);
        frac_bits = 0u;
    } else {
        let k = u32(-e);
        int_part = m >> k;
        frac_bits = (m & ((1u << k) - 1u)) << (32u - k);
    }
    var carry = 0u;
    if (frac_bits != 0u && state >= (0u - frac_bits)) {
        carry = 1u;
    }
    return min(int_part + carry, 255u);
}

fn luma(fbits: u32, s: u32, state: u32) -> u32 {
    let exp = i32((fbits >> 23u) & 0xffu);
    if (exp == 0) {
        return 0u;
    }
    let p = ((fbits & 0x7fffffu) | 0x800000u) * s;
    if (p == 0u) {
        return 0u;
    }
    let r = round24(p);
    return floor_dithered(r.x, exp - 150 + i32(r.y), state);
}

fn chroma(fbits: u32, s: u32, state: u32) -> u32 {
    let exp = i32((fbits >> 23u) & 0xffu);
    let d = i32(s) - 127;
    if (exp == 0 || d == 0) {
        return 127u;
    }
    let q = round24(((fbits & 0x7fffffu) | 0x800000u) * u32(abs(d)));
    let eq = exp - 150 + i32(q.y);
    let unit = 127u << u32(-eq);
    var sum: u32;
    if (d > 0) {
        sum = unit + q.x;
    } else {
        sum = unit - q.x;
    }
    if (sum == 0u) {
        return 0u;
    }
    let r = round24(sum);
    return floor_dithered(r.x, eq + i32(r.y), state);
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

@fragment
fn fs_vignette(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<u32>(frag.xy);
    let s = u32(round(textureLoad(src, vec2<i32>(o), 0).r * 255.0));
    let state = lcg_at(v.base + o.y * v.size.x + o.x);
    var out: u32;
    if (v.chroma == 1u) {
        out = chroma(fmap[(o.y * 2u) * v.map_w + o.x * 2u], s, state);
    } else {
        out = luma(fmap[o.y * v.map_w + o.x], s, state);
    }
    return vec4<f32>(f32(out) / 255.0, 0.0, 0.0, 1.0);
}
