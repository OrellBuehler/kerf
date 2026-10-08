// Kerf GPU presenter: draw a finished composite (encoded RGB, Rgba8Unorm) into a rectangle of a
// window surface, the rest of the surface in the matte colour.
//
// The composite holds the values a screen is to show, as FFmpeg's still does. A surface in a
// non-sRGB format shows them as written; one in an sRGB format would encode them a second time,
// so for that case they are decoded here first and the hardware's encode gives them back.

struct Params {
    dest: vec4<f32>,   // x, y, width, height in target pixels
    matte: vec4<f32>,  // encoded rgb
    flags: vec4<u32>,  // x: the target is an sRGB format
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var frame: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var v = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(v[i], 0.0, 1.0);
}

fn decode(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let q = frag.xy - p.dest.xy;
    var rgb = p.matte.rgb;
    if (q.x >= 0.0 && q.y >= 0.0 && q.x < p.dest.z && q.y < p.dest.w) {
        // Bilinear, and exact (a texel centre) whenever the rectangle is the frame's size.
        rgb = textureSampleLevel(frame, smp, q / p.dest.zw, 0.0).rgb;
    }
    if (p.flags.x != 0u) {
        rgb = decode(rgb);
    }
    return vec4<f32>(rgb, 1.0);
}
