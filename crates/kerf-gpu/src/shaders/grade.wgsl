// A clip's grade, as FFmpeg draws it. The grade's filters (`lut3d`, `lutrgb`, `colorbalance`) only take RGB, so
// FFmpeg converts the picture yuv420p -> rgb24, runs them, and converts it back; the scalar reference of every
// pass, with the derivation and what it was held to, is src/grade.rs. The way back is roundtrip.wgsl's.
//
//   fs_rgb       Y, U, V -> R, G, B   swscale's x86 kernel (pmulhw by 13-bit coefficients, chroma replicated 2x2)
//   fs_lut3d     R, G, B -> R, G, B   lut3d, tetrahedral, in float, every product rounded alone
//   fs_curves    R, G, B -> R, G, B   lutrgb: a table per channel
//   fs_balance   R, G, B -> R, G, B   colorbalance, in float

struct P {
    k: vec4<i32>,   // the kernel: yCoeff, vrCoeff, ubCoeff, vgCoeff
    k2: vec4<i32>,  // ugCoeff, yOffset, the LUT's size, zero
    // colorbalance's nine numbers, channel by channel: (shadows, midtones, highlights, unused).
    bal: array<vec4<f32>, 3>,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var t1: texture_2d<f32>;
@group(0) @binding(2) var t2: texture_2d<f32>;
@group(0) @binding(3) var t3: texture_2d<f32>;
@group(0) @binding(4) var lut: texture_3d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

fn byte(v: f32) -> i32 {
    return i32(round(v * 255.0));
}

fn rgb_out(c: vec3<i32>) -> vec4<f32> {
    return vec4<f32>(vec3<f32>(c) / 255.0, 1.0);
}

// A backend may fuse a product with the sum after it (a GPU's `a * b + c` is often one rounding, a CPU's two). Every
// product that a sum follows goes through here: an OR with a uniform that is zero, which no compiler can see through.
fn pin(x: f32) -> f32 {
    return bitcast<f32>(bitcast<u32>(x) | bitcast<u32>(p.k2.w));
}

fn pin3(x: vec3<f32>) -> vec3<f32> {
    return bitcast<vec3<f32>>(bitcast<vec3<u32>>(x) | vec3<u32>(bitcast<u32>(p.k2.w)));
}

@fragment
fn fs_rgb(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec2<i32>(o.x / 2, o.y / 2);
    let y = byte(textureLoad(t1, o, 0).r);
    let u = byte(textureLoad(t2, c, 0).r) * 8 - 1024;
    let v = byte(textureLoad(t3, c, 0).r) * 8 - 1024;
    let yy = ((y * 8 - p.k2.y) * p.k.x) >> 16u;
    let r = yy + ((v * p.k.y) >> 16u);
    let g = yy + (((u * p.k2.x) >> 16u) + ((v * p.k.w) >> 16u));
    let b = yy + ((u * p.k.z) >> 16u);
    return rgb_out(clamp(vec3<i32>(r, g, b), vec3<i32>(0), vec3<i32>(255)));
}

// ---- lutrgb: t2 is the tables, one row a channel ----

@fragment
fn fs_curves(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec3<i32>(round(textureLoad(t1, o, 0).rgb * 255.0));
    return rgb_out(vec3<i32>(
        byte(textureLoad(t2, vec2<i32>(c.x, 0), 0).r),
        byte(textureLoad(t2, vec2<i32>(c.y, 1), 0).r),
        byte(textureLoad(t2, vec2<i32>(c.z, 2), 0).r),
    ));
}

// ---- colorbalance: t2 holds `src / 255.0f` for every byte ----

fn clipf(x: f32) -> f32 {
    return min(max(x, 0.0), 1.0);
}

fn component(v: f32, l: f32, s: f32, m: f32, h: f32) -> f32 {
    let a: f32 = 4.0;
    let b: f32 = 0.333;
    let scale: f32 = 0.7;
    let ws = clipf((b - l) * a + 0.5) * scale;
    let wm = clipf((l - b) * a + 0.5) * clipf((1.0 - l - b) * a + 0.5) * scale;
    let wh = clipf((l + b - 1.0) * a + 0.5) * scale;
    var x = v + pin(s * ws);
    x = x + pin(m * wm);
    x = x + pin(h * wh);
    return clipf(x);
}

@fragment
fn fs_balance(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec3<i32>(round(textureLoad(t1, o, 0).rgb * 255.0));
    let r = textureLoad(t2, vec2<i32>(c.x, 0), 0).r;
    let g = textureLoad(t2, vec2<i32>(c.y, 0), 0).r;
    let b = textureLoad(t2, vec2<i32>(c.z, 0), 0).r;
    let l = max(max(r, g), b) + min(min(r, g), b);
    let out = vec3<f32>(
        component(r, l, p.bal[0].x, p.bal[0].y, p.bal[0].z),
        component(g, l, p.bal[1].x, p.bal[1].y, p.bal[1].z),
        component(b, l, p.bal[2].x, p.bal[2].y, p.bal[2].z),
    );
    // `lrintf(x * 255)`: a product, then the nearest integer, ties to even.
    return rgb_out(clamp(vec3<i32>(round(out * 255.0)), vec3<i32>(0), vec3<i32>(255)));
}

// ---- lut3d (tetrahedral): t2 is the axes' table, rows: lattice index below, above, fraction ----

fn axis(v: i32, row: i32) -> f32 {
    return textureLoad(t2, vec2<i32>(v, row), 0).r;
}

fn at(r: i32, g: i32, b: i32) -> vec3<f32> {
    return textureLoad(lut, vec3<i32>(r, g, b), 0).rgb;
}

@fragment
fn fs_lut3d(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let c = vec3<i32>(round(textureLoad(t1, o, 0).rgb * 255.0));
    let pr = i32(axis(c.x, 0));
    let pg = i32(axis(c.y, 0));
    let pb = i32(axis(c.z, 0));
    let nr = i32(axis(c.x, 1));
    let ng = i32(axis(c.y, 1));
    let nb = i32(axis(c.z, 1));
    let dr = axis(c.x, 2);
    let dg = axis(c.y, 2);
    let db = axis(c.z, 2);
    let c000 = at(pr, pg, pb);
    let c111 = at(nr, ng, nb);
    var ca: vec3<f32>;
    var cb: vec3<f32>;
    var w: vec4<f32>;
    if (dr > dg) {
        if (dg > db) {
            ca = at(nr, pg, pb);
            cb = at(nr, ng, pb);
            w = vec4<f32>(1.0 - dr, dr - dg, dg - db, db);
        } else if (dr > db) {
            ca = at(nr, pg, pb);
            cb = at(nr, pg, nb);
            w = vec4<f32>(1.0 - dr, dr - db, db - dg, dg);
        } else {
            ca = at(pr, pg, nb);
            cb = at(nr, pg, nb);
            w = vec4<f32>(1.0 - db, db - dr, dr - dg, dg);
        }
    } else {
        if (db > dg) {
            ca = at(pr, pg, nb);
            cb = at(pr, ng, nb);
            w = vec4<f32>(1.0 - db, db - dg, dg - dr, dr);
        } else if (db > dr) {
            ca = at(pr, ng, pb);
            cb = at(pr, ng, nb);
            w = vec4<f32>(1.0 - dg, dg - db, db - dr, dr);
        } else {
            ca = at(pr, ng, pb);
            cb = at(nr, ng, pb);
            w = vec4<f32>(1.0 - dg, dg - dr, dr - db, db);
        }
    }
    // Four products, each rounded alone, summed left to right.
    let sum = ((pin3(c000 * w.x) + pin3(ca * w.y)) + pin3(cb * w.z)) + pin3(c111 * w.w);
    // `av_clip_uint8(sum * 255.0f)`: the float is truncated toward zero.
    return rgb_out(clamp(vec3<i32>(pin3(sum) * 255.0), vec3<i32>(0), vec3<i32>(255)));
}
