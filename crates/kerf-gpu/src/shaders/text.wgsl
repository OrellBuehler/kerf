// One blend of an overlay onto the canvas: `drawtext`'s own arithmetic (libavfilter
// `drawutils.c`, `ff_blend_mask` and `ff_blend_rectangle`), in 32-bit integers. The canvas holds the
// composite's Y, U, V in R, G, B (chroma replicated per pixel), or RGB for a frame with no layer.
//
//   plane sample  d  ->  ((K - a) * d + a * s) >> 24          K = 0x1010101
//
// `a * s` and `(K - a) * d` stay under 2^32 (a <= K), so u32 arithmetic is exact.
//
// kind 0, a coverage mask: luma gets a = coverage * alpha; a 4:2:0 chroma sample gets
// a = ((sum of the coverage over its 2x2 block) >> 2) * alpha, the block counting only what is in
// the mask. An RGB canvas takes every channel as luma.
// kind 1, a rectangle: luma gets a = alpha inside it; a chroma sample gets alpha, halved once for
// each of the block's two rows and two columns the rectangle only half covers.

struct Params {
    colour: vec4<u32>,
    area: vec4<i32>,   // coverage: x, y, w, h of the mask on the canvas; rectangle: x, y, w, h
    alpha: u32,
    kind: u32,
    rgb: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> tp: Params;
@group(0) @binding(1) var canvas: texture_2d<f32>;
@group(0) @binding(2) var mask: texture_2d<f32>;

const K: u32 = 16843009u;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covers the target; the scissor rectangle bounds the work.
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[i], 0.0, 1.0);
}

fn coverage(p: vec2<i32>) -> u32 {
    let q = p - tp.area.xy;
    if (q.x < 0 || q.y < 0 || q.x >= tp.area.z || q.y >= tp.area.w) {
        return 0u;
    }
    return u32(round(textureLoad(mask, q, 0).r * 255.0));
}

fn blend(dst: u32, a: u32, src: u32) -> u32 {
    return ((K - a) * dst + a * src) >> 24u;
}

@fragment
fn fs_text(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(frag.xy);
    let px = round(textureLoad(canvas, p, 0) * 255.0);
    var out = vec3<u32>(u32(px.r), u32(px.g), u32(px.b));
    let c = tp.colour.xyz;
    let b = p & vec2<i32>(-2, -2);
    if (tp.kind == 0u) {
        let a = coverage(p) * tp.alpha;
        if (tp.rgb == 1u) {
            out = vec3<u32>(blend(out.x, a, c.x), blend(out.y, a, c.y), blend(out.z, a, c.z));
        } else {
            out.x = blend(out.x, a, c.x);
            let t = coverage(b) + coverage(b + vec2<i32>(1, 0)) + coverage(b + vec2<i32>(0, 1)) + coverage(b + vec2<i32>(1, 1));
            let ac = (t >> 2u) * tp.alpha;
            out.y = blend(out.y, ac, c.y);
            out.z = blend(out.z, ac, c.z);
        }
    } else {
        let r = tp.area;
        let inside = p.x >= r.x && p.y >= r.y && p.x < r.x + r.z && p.y < r.y + r.w;
        if (tp.rgb == 1u) {
            if (inside) {
                out = vec3<u32>(blend(out.x, tp.alpha, c.x), blend(out.y, tp.alpha, c.y), blend(out.z, tp.alpha, c.z));
            }
        } else {
            if (inside) {
                out.x = blend(out.x, tp.alpha, c.x);
            }
            let nx = max(0, min(r.x + r.z, b.x + 2) - max(r.x, b.x));
            let ny = max(0, min(r.y + r.w, b.y + 2) - max(r.y, b.y));
            if (nx > 0 && ny > 0) {
                var a = tp.alpha;
                if (ny < 2) {
                    a = a >> 1u;
                }
                if (nx < 2) {
                    a = a >> 1u;
                }
                out.y = blend(out.y, a, c.y);
                out.z = blend(out.z, a, c.z);
            }
        }
    }
    return vec4<f32>(vec3<f32>(out) / 255.0, px.a / 255.0);
}
