// The end of `gblur` on a plane: `postscale_c` (a multiply and `av_clipf` to 0..255) then
// `lrintf` (round half to even, which WGSL's `round` is) into the 8-bit plane.

struct B {
    size: vec2<i32>,
    nu: f32,
    boundary: f32,
    post: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
}

@group(0) @binding(0) var<uniform> b: B;
@group(0) @binding(1) var<storage, read> buf: array<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

@fragment
fn fs_out(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let v = clamp(buf[o.y * b.size.x + o.x] * b.post, 0.0, 255.0);
    return vec4<f32>(round(v) / 255.0, 0.0, 0.0, 1.0);
}
