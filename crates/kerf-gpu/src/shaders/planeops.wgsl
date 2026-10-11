// Per-plane passes that sit between the scaler and the blend, on 8-bit planes (R8Unorm in, R8Unorm out):
//
//   fs_lut      a 256-entry table (`eq`'s, `hue=s=0`, `negate`; composed on the CPU)
//   fs_fade     `fade` on a plane that is not blended yet: luma, chroma or the alpha ramp. A layer
//               whose moving zoom comes last is faded at the size it is zoomed from
//   fs_unsharp  `unsharp`, 5x5, luma: integer arithmetic (the binomial [1 4 6 4 1] blur, exactly summed)
//
// FFmpeg's arithmetic, not a formula for it: see `src/effects.rs` for the references and
// `kerf_core::fade_luma` for `fade`.

struct P {
    size: vec2<i32>,   // the plane
    row: i32,          // fs_lut: the table's row (0 Y, 1 U, 2 V)
    amount: i32,       // fs_unsharp: the integer amount (65536 = 1.0)
    kind: u32,         // fs_fade: 0 luma, 1 chroma, 2 alpha
    black: u32,        // fs_fade luma: the black level `fade` takes for the layer's pixel format
    f_in: u32,         // fs_fade: the fades' integer factors (65535: not running); alpha: the ramp
    f_out: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var aux: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

fn byte(v: f32) -> i32 {
    return i32(round(v * 255.0));
}

fn out_byte(v: i32) -> vec4<f32> {
    return vec4<f32>(f32(v) / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn fs_lut(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let v = byte(textureLoad(src, vec2<i32>(frag.xy), 0).r);
    return out_byte(byte(textureLoad(aux, vec2<i32>(v, p.row), 0).r));
}

// `vf_fade.c`: the factor is `UINT16_MAX * i / N`; 65535 means the filter does not run.
fn fade_luma(y: i32, f: u32) -> i32 {
    let bl = i32(p.black);
    return clamp((((y - bl) * i32(f)) + (bl << 16u) + 32768) >> 16u, 0, 255);
}

fn fade_chroma(c: i32, f: u32) -> i32 {
    return clamp((((c - 128) * i32(f)) + 8421367) >> 16u, 0, 255);
}

@fragment
fn fs_fade(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    var v = byte(textureLoad(src, vec2<i32>(frag.xy), 0).r);
    if (p.kind == 0u) {
        if (p.f_in < 65535u) {
            v = fade_luma(v, p.f_in);
        }
        if (p.f_out < 65535u) {
            v = fade_luma(v, p.f_out);
        }
    } else if (p.kind == 1u) {
        if (p.f_in < 65535u) {
            v = fade_chroma(v, p.f_in);
        }
        if (p.f_out < 65535u) {
            v = fade_chroma(v, p.f_out);
        }
    } else if (p.f_in < 65535u) {
        v = clamp((v * i32(p.f_in) + 32768) >> 16u, 0, 255);
    }
    return out_byte(v);
}

@fragment
fn fs_unsharp(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    var k = array<i32, 5>(1, 4, 6, 4, 1);
    var sum = 0;
    for (var j = 0; j < 5; j = j + 1) {
        let y = clamp(o.y + j - 2, 0, p.size.y - 1);
        for (var i = 0; i < 5; i = i + 1) {
            let x = clamp(o.x + i - 2, 0, p.size.x - 1);
            sum = sum + k[j] * k[i] * byte(textureLoad(src, vec2<i32>(x, y), 0).r);
        }
    }
    let s = byte(textureLoad(src, o, 0).r);
    // (`>> 16` of a negative number is an arithmetic shift, as the C filter's is.)
    let res = s + (((s - ((sum + 128) >> 8u)) * p.amount) >> 16u);
    return out_byte(clamp(res, 0, 255));
}
