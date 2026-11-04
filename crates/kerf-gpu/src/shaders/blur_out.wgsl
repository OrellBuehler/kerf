// The end of `gblur` on a plane: `postscale_c` / `ff_postscale_slice_*` (a multiply and a clip to 0..255, a plain multiply with no
// addend) then `lrintf` (round half to even, which WGSL's `round` is) into the 8-bit plane. `fma.wgsl` is included ahead of this:
// the multiply is `mul_pos`, rounded to nearest even in integers like the recursion's steps.

struct B {
    size: vec2<i32>,
    nu: u32,
    boundary: u32,
    post: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> b: B;
@group(0) @binding(1) var<storage, read> buf: array<u32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

@fragment
fn fs_out(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    // (The clip to 255 is on the bit patterns, which order non-negative floats: 0x437f0000 is 255.0.)
    let v = bitcast<f32>(min(mul_pos(buf[o.y * b.size.x + o.x], b.post), 0x437f0000u));
    return vec4<f32>(round(v) / 255.0, 0.0, 0.0, 1.0);
}
