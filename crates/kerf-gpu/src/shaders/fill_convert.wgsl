// The second context of swscale's cascade when the destination is the larger picture (`convert::rgb_scale_up`, the scalar reference
// with the derivation and what it was held to): the horizontal pass of the generic scaler over a `bgr24` picture. One plane at a time:
// the colour of every pixel as 14 bits (`bgr24ToY_c` / `bgr24ToUV_c`, one chroma sample for each pixel), brought to the plane's width by
// the table (14-bit weights), `hScale16To15`'s `>> 13`, clipped at 15 bits. The vertical pass is the resampler's (`composite.wgsl`'s `fs_v`).
//
//   rgb    the picture, one pixel per texel
//   taps   a row per output column: the window's first source column, then the weights

struct P {
    ky: vec4<i32>, // the luma weights ry, gy, by, and the taps per output column
    ku: vec4<i32>, // ru, gu, bu, and which plane: 0 luma, 1 U, 2 V
    kv: vec4<i32>, // rv, gv, bv, and the picture's width
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var rgb: texture_2d<f32>;
@group(0) @binding(2) var taps: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

@fragment
fn fs_h(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    let pos = i32(textureLoad(taps, vec2<i32>(0, o.x), 0).r);
    var sum = 0;
    for (var j = 0; j < p.ky.w; j = j + 1) {
        let w = i32(textureLoad(taps, vec2<i32>(1 + j, o.x), 0).r);
        let col = clamp(pos + j, 0, p.kv.w - 1);
        let c = vec3<i32>(round(textureLoad(rgb, vec2<i32>(col, o.y), 0).rgb * 255.0));
        var v: i32;
        if (p.ku.w == 0) {
            v = (p.ky.x * c.x + p.ky.y * c.y + p.ky.z * c.z + (32 << 14u) + (1 << 8u)) >> 9u;
        } else if (p.ku.w == 1) {
            v = (p.ku.x * c.x + p.ku.y * c.y + p.ku.z * c.z + (256 << 14u) + (1 << 8u)) >> 9u;
        } else {
            v = (p.kv.x * c.x + p.kv.y * c.y + p.kv.z * c.z + (256 << 14u) + (1 << 8u)) >> 9u;
        }
        sum = sum + w * v;
    }
    return vec4<f32>(f32(min(sum >> 13u, 32767)), 0.0, 0.0, 1.0);
}
