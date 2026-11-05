// The placement of a moving zoom or position: `pad`, `perspective` (`interpolation=linear`) and `crop`, one
// plane at a time (R8Unorm in, R8Unorm out at the canvas' size, halved for chroma).
//
// `src/placement.rs` is the scalar reference and has the story; `kerf_core::perspective` builds the tables
// (the filter's `f64` matrix and `lrint`, in 1/256 of a luma sample), which are all this reads. A sample takes
// the entry of its top-left luma pixel (`pv[(x << hsub) + (y << vsub) * w]`, then `>> hsub`: no half-sample
// centring), splits it into a whole part and 8 bits of fraction, and mixes four neighbours with integer
// weights: `(sub_v_inv * (sub_u_inv * a + sub_u * b) + sub_v * (sub_u_inv * c + sub_u * d) + 2^15) >> 16`.
// Reads are clamped to the padded frame (what the filter's two edge branches come to), and past the picture
// the frame is `pad`'s: `black@0`, Y 16, U = V = 128, alpha 0.

struct P {
    out_size: vec2<i32>,    // this plane of the canvas
    frame: vec2<i32>,       // this plane of the padded frame (`perspective`'s input)
    margin: vec2<i32>,      // `pad`'s margin, in this plane's samples
    shows: vec2<i32>,       // the picture, as `pad` leaves it, in this plane's samples
    canvas_w: i32,          // the luma plane's width: a per-pixel table's stride
    sub: i32,               // 1 on U and V
    fill: i32,              // what `pad` draws past the picture
    pixels: i32,            // 1: the tables hold a pair per pixel, else one per column and one per row
    flat: i32,              // 1: the plane is one value (`inside`), the alpha `format=yuva420p` adds
    inside: i32,
    pad0: i32,
    pad1: i32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var<storage, read> tab_u: array<i32>;
@group(0) @binding(3) var<storage, read> tab_v: array<i32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    var q = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(q[i], 0.0, 1.0);
}

fn tap(ix: i32, iy: i32) -> i32 {
    let tx = clamp(ix, 0, p.frame.x - 1);
    let ty = clamp(iy, 0, p.frame.y - 1);
    let sx = tx - p.margin.x;
    let sy = ty - p.margin.y;
    if (sx < 0 || sy < 0 || sx >= p.shows.x || sy >= p.shows.y) {
        return p.fill;
    }
    if (p.flat != 0) {
        return p.inside;
    }
    return i32(round(textureLoad(src, vec2<i32>(sx, sy), 0).r * 255.0));
}

@fragment
fn fs_place(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let o = vec2<i32>(frag.xy);
    // The luma pixel whose table entry this sample reads.
    let lx = o.x << u32(p.sub);
    let ly = o.y << u32(p.sub);
    var u: i32;
    var v: i32;
    if (p.pixels != 0) {
        u = tab_u[ly * p.canvas_w + lx];
        v = tab_v[ly * p.canvas_w + lx];
    } else {
        u = tab_u[lx];
        v = tab_v[ly];
    }
    // (`>>` of a negative number is an arithmetic shift, as the C filter's is.)
    u = u >> u32(p.sub);
    v = v >> u32(p.sub);
    let su = u & 255;
    let sv = v & 255;
    let iu = u >> 8u;
    let iv = v >> 8u;
    let a = tap(iu, iv);
    let b = tap(iu + 1, iv);
    let c = tap(iu, iv + 1);
    let d = tap(iu + 1, iv + 1);
    let sum = (256 - sv) * ((256 - su) * a + su * b) + sv * ((256 - su) * c + su * d);
    let res = clamp((sum + 32768) >> 16u, 0, 255);
    return vec4<f32>(f32(res) / 255.0, 0.0, 0.0, 1.0);
}
