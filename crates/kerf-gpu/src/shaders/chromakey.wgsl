// `chromakey` (vf_chromakey.c, 8-bit) on a `yuva420p` picture: its alpha plane, sample for sample, as
// `effects.rs` computes it (`chroma_key_plane`). Each luma sample's 3x3 window of chroma samples is read
// where the filter reads it (`chroma_key_source`: outside the picture an entry keeps what the slice last
// wrote there, or the key colour), its nine distance terms are summed in `double` (emulated in integers:
// `chroma_key_add`), and the alpha is how many of the 255 thresholds the sum reaches
// (`chroma_key_thresholds`, which hold the rest of the filter's `double` arithmetic).

struct K {
    // The picture's (luma) size.
    size: vec2<u32>,
    // The key colour's U and V.
    key: vec2<u32>,
    // The filter's slice count, `min(h, threads)`.
    slices: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> k: K;
@group(0) @binding(1) var u_plane: texture_2d<f32>;
@group(0) @binding(2) var v_plane: texture_2d<f32>;
// `sqrt(d2 / (255 * 255 * 2))` for every `d2`, as `double` bits (hi, lo).
@group(0) @binding(3) var<storage, read> terms: array<vec2<u32>>;
// The least sum reaching each alpha level 1..=255 (hi, lo).
@group(0) @binding(4) var<storage, read> thresholds: array<vec2<u32>>;

// Two non-negative `double`s (hi, lo), normal or zero, added and rounded to nearest even.
fn dadd(a0: vec2<u32>, b0: vec2<u32>) -> vec2<u32> {
    if (a0.x == 0u && a0.y == 0u) {
        return b0;
    }
    if (b0.x == 0u && b0.y == 0u) {
        return a0;
    }
    var a = a0;
    var b = b0;
    if ((a.x >> 20u) < (b.x >> 20u)) {
        a = b0;
        b = a0;
    }
    var e = a.x >> 20u;
    let d = e - (b.x >> 20u);
    let ah = (a.x & 0xfffffu) | 0x100000u;
    let bh = (b.x & 0xfffffu) | 0x100000u;
    let ma = vec2<u32>((ah << 10u) | (a.y >> 22u), a.y << 10u);
    var mb = vec2<u32>((bh << 10u) | (b.y >> 22u), b.y << 10u);
    if (d >= 64u) {
        mb = vec2<u32>(0u, 1u);
    } else if (d >= 32u) {
        let s = d - 32u;
        var lost = mb.y != 0u;
        var low = mb.x;
        if (s > 0u) {
            lost = lost || (mb.x & ((1u << s) - 1u)) != 0u;
            low = mb.x >> s;
        }
        mb = vec2<u32>(0u, low | select(0u, 1u, lost));
    } else if (d > 0u) {
        let lost = (mb.y & ((1u << d) - 1u)) != 0u;
        mb = vec2<u32>(mb.x >> d, (mb.y >> d) | (mb.x << (32u - d)) | select(0u, 1u, lost));
    }
    let lo = ma.y + mb.y;
    let carry = select(0u, 1u, lo < ma.y);
    var sum = vec2<u32>(ma.x + mb.x + carry, lo);
    if ((sum.x & 0x80000000u) != 0u) {
        let lost = sum.y & 1u;
        sum = vec2<u32>(sum.x >> 1u, (sum.y >> 1u) | (sum.x << 31u) | lost);
        e = e + 1u;
    }
    let rest = sum.y & 0x3ffu;
    var keep = vec2<u32>(sum.x >> 10u, (sum.y >> 10u) | (sum.x << 22u));
    if (rest > 0x200u || (rest == 0x200u && (keep.y & 1u) == 1u)) {
        keep.y = keep.y + 1u;
        if (keep.y == 0u) {
            keep.x = keep.x + 1u;
        }
        if (keep.x == 0x200000u) {
            keep = vec2<u32>(0x100000u, 0u);
            e = e + 1u;
        }
    }
    return vec2<u32>((e << 20u) | (keep.x & 0xfffffu), keep.y);
}

// `a >= b` for non-negative `double`s: their bits are in their order.
fn reaches(a: vec2<u32>, b: vec2<u32>) -> bool {
    return a.x > b.x || (a.x == b.x && a.y >= b.y);
}

// The row the slice holding row `y` starts at (`ff_slice_pos`).
fn slice_start(y: u32) -> u32 {
    let h = k.size.y;
    let n = k.slices;
    var j = (y * n) / h;
    loop {
        if (j + 1u < n && (h * (j + 1u)) / n <= y) {
            j = j + 1u;
        } else {
            break;
        }
    }
    loop {
        if (j > 0u && (h * j) / n > y) {
            j = j - 1u;
        } else {
            break;
        }
    }
    return (h * j) / n;
}

// Where window entry (xo, yo) at (x, y) holds its value from (`chroma_key_source`); x -1 for the key.
fn source(x: i32, y: i32, xo: i32, yo: i32, s0: i32) -> vec2<i32> {
    let w = i32(k.size.x);
    let h = i32(k.size.y);
    let cx = x + xo - 1;
    let cy = y + yo - 1;
    if (cx >= 0 && cx < w && cy >= 0 && cy < h) {
        return vec2<i32>(cx, cy);
    }
    if (cy >= 0 && cy < h && cx >= w) {
        return vec2<i32>(w - 1, cy);
    }
    let col = select(w - 1, w - 2, xo == 0);
    let row = y + yo - 2;
    if (y > s0 && row >= 0) {
        return vec2<i32>(col, row);
    }
    return vec2<i32>(-1, -1);
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

@fragment
fn fs_key(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let x = i32(frag.x);
    let y = i32(frag.y);
    let s0 = i32(slice_start(u32(y)));
    var sum = vec2<u32>(0u, 0u);
    for (var yo = 0; yo < 3; yo = yo + 1) {
        for (var xo = 0; xo < 3; xo = xo + 1) {
            let at = source(x, y, xo, yo, s0);
            var cu = i32(k.key.x);
            var cv = i32(k.key.y);
            if (at.x >= 0) {
                let c = vec2<i32>(at.x >> 1u, at.y >> 1u);
                cu = i32(round(textureLoad(u_plane, c, 0).r * 255.0));
                cv = i32(round(textureLoad(v_plane, c, 0).r * 255.0));
            }
            let du = cu - i32(k.key.x);
            let dv = cv - i32(k.key.y);
            sum = dadd(sum, terms[u32(du * du + dv * dv)]);
        }
    }
    var lo = 0u;
    var hi = 255u;
    loop {
        if (lo >= hi) {
            break;
        }
        let mid = (lo + hi) / 2u;
        if (reaches(sum, thresholds[mid])) {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    return vec4<f32>(f32(lo) / 255.0, 0.0, 0.0, 1.0);
}
