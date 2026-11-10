// A floating animated SVG title laid on the composite: FFmpeg's `overlay` of a `yuva420p` picture on a `yuv420p` one.
//
// The picture's planes are FFmpeg's own (it converted the PNG resvg drew: `src/svg.rs`); this is `vf_overlay.c`'s `blend_plane`
// on them, in integers:
//
//   * `x` and `y` are rounded down to even (`s->x &= ~((1 << hsub) - 1)`): the origin given is already that, so the picture's
//     chroma grid is the canvas'. It may be negative and the picture may reach past the canvas (both are clipped).
//   * luma: `FAST_DIV255(d * (255 - a) + s * a)` with the picture's own alpha at that sample;
//   * chroma: the same with the alpha the filter averages over the sample's 2x2 block (`>> 2`), and at the picture's last chroma
//     column and row `(alpha_v + alpha_h) >> 1` of what is there (its own second luma column / row is ignored, a quirk of the filter).
//   * an odd picture's last chroma sample reaches one pixel past its luma: that pixel's chroma is blended, its luma is not.

struct SvgParams {
    origin: vec2<i32>,  // the picture's top-left on the canvas (even)
    size: vec2<i32>,    // the picture
    csize: vec2<i32>,   // its chroma planes
    pad: vec2<i32>,
}

@group(0) @binding(0) var<uniform> sp: SvgParams;
@group(0) @binding(1) var prev: texture_2d<f32>;
@group(0) @binding(2) var ytex: texture_2d<f32>;
@group(0) @binding(3) var utex: texture_2d<f32>;
@group(0) @binding(4) var vtex: texture_2d<f32>;
@group(0) @binding(5) var atex: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}

fn byte(v: f32) -> u32 {
    return u32(round(v * 255.0));
}

// `FAST_DIV255(d * (255 - a) + s * a)`.
fn blend255(d: u32, s: u32, a: u32) -> u32 {
    return ((d * (255u - a) + s * a + 128u) * 257u) >> 16u;
}

fn alpha_at(p: vec2<i32>) -> u32 {
    return byte(textureLoad(atex, p, 0).r);
}

// The alpha `blend_plane` takes for the chroma sample `c` of the picture.
fn chroma_alpha(c: vec2<i32>) -> u32 {
    let p = c * 2;
    let right = c.x + 1 < sp.csize.x;
    let below = c.y + 1 < sp.csize.y;
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
fn fs_svg(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(frag.xy);
    let d = vec3<u32>(round(textureLoad(prev, p, 0).rgb * 255.0));
    var out = d;
    let l = p - sp.origin;
    if (l.x >= 0 && l.y >= 0 && l.x < sp.size.x && l.y < sp.size.y) {
        out.x = blend255(d.x, byte(textureLoad(ytex, l, 0).r), alpha_at(l));
    }
    // The origin is even, so the canvas' chroma block of this pixel is the picture's chroma sample plus half the origin.
    let c = p / 2 - sp.origin / 2;
    if (c.x >= 0 && c.y >= 0 && c.x < sp.csize.x && c.y < sp.csize.y) {
        let a = chroma_alpha(c);
        out.y = blend255(d.y, byte(textureLoad(utex, c, 0).r), a);
        out.z = blend255(d.z, byte(textureLoad(vtex, c, 0).r), a);
    }
    return vec4<f32>(vec3<f32>(out) / 255.0, 1.0);
}
