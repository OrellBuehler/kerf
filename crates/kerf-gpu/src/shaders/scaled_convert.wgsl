// A `yuv420p` picture swscale scales to a smaller size and converts to `bgr24` in the one context: the vertical pass of
// `convert::scaled_to_rgb24` (the scalar reference, with the derivation and what it was held to). The horizontal passes are
// composite.wgsl's `fs_h`, three of them (the luma, and the two chroma planes at half the width), into 15-bit intermediates.
//
//   t1, t2, t3   the luma / U / V intermediates (R32Float, 15 bits, one row per source row)
//   t4, t5       the vertical tables of the luma and of the chroma: per output row, the first source row, then the weights
//
// Rows before `p.sizes.w` are the inline MMX `yuv2bgr24_X` (`pmulhw` on 16-bit lanes), the last two the C `yuv2rgb_X_c_template`.

struct P {
    k: vec4<i32>,     // yCoeff, vrCoeff, ubCoeff, vgCoeff (13-bit lanes)
    k2: vec4<i32>,    // ugCoeff, yOffset, zero, zero
    c: vec4<i32>,     // the C tables' chroma gains, divided by the luma gain: crv, cbu, cgu, cgv
    c2: vec4<i32>,    // cy, oy, zero, zero
    sizes: vec4<i32>, // luma taps, chroma taps, the luma rows of the source, the rows done by MMX
    chroma_rows: vec4<i32>, // the chroma rows of the source, zero, zero, zero
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var t1: texture_2d<f32>;
@group(0) @binding(2) var t2: texture_2d<f32>;
@group(0) @binding(3) var t3: texture_2d<f32>;
@group(0) @binding(4) var lf: texture_2d<f32>;
@group(0) @binding(5) var cf: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

// `y_table[i]`: the luma term of table index `i`, clipped to a byte (`ff_yuv2rgb_c_init_tables`, 24 bpp).
fn ytab(i: i32) -> i32 {
    let yb = -(384 << 16u) - 512 * p.c2.x - p.c2.y + i * p.c2.x;
    return clamp((yb + 0x8000) >> 16u, 0, 255);
}

@fragment
fn fs_scaled_rgb(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let cx = o.x >> 1u;
    let lpos = i32(textureLoad(lf, vec2<i32>(0, o.y), 0).r);
    let cpos = i32(textureLoad(cf, vec2<i32>(0, o.y), 0).r);
    let mmx = o.y < p.sizes.w;
    // The MMX kernel starts every lane from `vRounder` (4) and adds the high half of each 16-bit product; the C one starts from
    // 1 << 18 and sums the full products.
    var ya = select(1 << 18u, 4, mmx);
    var ua = ya;
    var va = ya;
    for (var j = 0; j < p.sizes.x; j = j + 1) {
        let c = i32(textureLoad(lf, vec2<i32>(1 + j, o.y), 0).r);
        let sy = clamp(lpos + j, 0, p.sizes.z - 1);
        let t = c * i32(textureLoad(t1, vec2<i32>(o.x, sy), 0).r);
        ya = ya + select(t, t >> 16u, mmx);
    }
    for (var j = 0; j < p.sizes.y; j = j + 1) {
        let c = i32(textureLoad(cf, vec2<i32>(1 + j, o.y), 0).r);
        let sy = clamp(cpos + j, 0, p.chroma_rows.x - 1);
        let tu = c * i32(textureLoad(t2, vec2<i32>(cx, sy), 0).r);
        let tv = c * i32(textureLoad(t3, vec2<i32>(cx, sy), 0).r);
        ua = ua + select(tu, tu >> 16u, mmx);
        va = va + select(tv, tv >> 16u, mmx);
    }
    var rgb: vec3<i32>;
    if (mmx) {
        let yy = ((ya - p.k2.y) * p.k.x) >> 16u;
        let u = ua - 1024;
        let v = va - 1024;
        rgb = vec3<i32>(
            yy + ((v * p.k.y) >> 16u),
            yy + (((u * p.k2.x) >> 16u) + ((v * p.k.w) >> 16u)),
            yy + ((u * p.k.z) >> 16u),
        );
    } else {
        let y = ya >> 19u;
        let u = clamp(ua >> 19u, 0, 255);
        let v = clamp(va >> 19u, 0, 255);
        // (`Yuv2Rgb::rgb`: the luma sits 326 + 512 into the table; a chroma term is its gain times the sample, shifted, less the
        // table's own offset.)
        let base = 838 + y;
        rgb = vec3<i32>(
            ytab(base - (p.c.x >> 9u) + ((v * p.c.x) >> 16u)),
            ytab(base - (p.c.z >> 9u) + ((u * p.c.z) >> 16u) - (p.c.w >> 9u) + ((v * p.c.w) >> 16u)),
            ytab(base - (p.c.y >> 9u) + ((u * p.c.y) >> 16u)),
        );
    }
    return vec4<f32>(vec3<f32>(clamp(rgb, vec3<i32>(0), vec3<i32>(255))) / 255.0, 1.0);
}
