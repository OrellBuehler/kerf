// `gblur` (vf_gblur.c) as an x86 build with AVX2 or AVX-512 runs it (`x86/vf_gblur.asm`): a first-order recursive Gaussian in `float`,
// along every row (rightwards then leftwards) and then along every column (downwards then upwards), each step of the recursion a
// **fused** multiply-add (`vfmadd231ps`: `x[i] = nu * x[i - 1] + x[i]` with one rounding) and the boundary scaling a plain multiply.
// One invocation per row and per column, each running its recursion sequentially in a storage buffer, as the filter does in place.
// `blur_out.wgsl` scales, clips and rounds the result back to an 8-bit plane.
//
// No backend's own `fma` is trusted for this: WGSL does not promise it is fused, and a compiler is free to fuse a multiply and an
// add that are not (the last bit of a `float` is the difference between two levels on a sample that sits on a half). So the
// fused multiply-add is done in integers (`fma_pos`: the exact 48-bit product, aligned to the addend in a 64-bit window with a
// sticky bit, rounded to nearest even; a subnormal or underflowing result as IEEE has it), for the non-negative finite floats the
// filter works on, and the multiply by `boundary` is the same function with a zero addend. The working buffer holds the bit patterns
// of the `float`s, not floats: no backend's handling of denormals can touch a value. The CPU twin is `effects::blur_plane`
// (`f32::mul_add`).

// (`nu`, `boundary` and `post` are the bit patterns of the `float`s: nothing but `fma_pos` touches them.)
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
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> buf: array<u32>;

// ---- the filter ------------------------------------------------------------------------------------------------------------

@compute @workgroup_size(64)
fn cs_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    let y = i32(gid.x);
    if (y >= b.size.y) {
        return;
    }
    let w = b.size.x;
    let base = y * w;
    for (var x = 0; x < w; x = x + 1) {
        buf[base + x] = bitcast<u32>(round(textureLoad(src, vec2<i32>(x, y), 0).r * 255.0));
    }
    var prev = mul_pos(buf[base], b.boundary);
    buf[base] = prev;
    for (var x = 1; x < w; x = x + 1) {
        prev = fma_pos(b.nu, prev, buf[base + x]);
        buf[base + x] = prev;
    }
    prev = mul_pos(prev, b.boundary);
    buf[base + w - 1] = prev;
    for (var x = w - 1; x > 0; x = x - 1) {
        prev = fma_pos(b.nu, prev, buf[base + x - 1]);
        buf[base + x - 1] = prev;
    }
}

@compute @workgroup_size(64)
fn cs_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    let x = i32(gid.x);
    if (x >= b.size.x) {
        return;
    }
    let w = b.size.x;
    let h = b.size.y;
    var prev = mul_pos(buf[x], b.boundary);
    buf[x] = prev;
    for (var y = 1; y < h; y = y + 1) {
        prev = fma_pos(b.nu, prev, buf[y * w + x]);
        buf[y * w + x] = prev;
    }
    prev = mul_pos(prev, b.boundary);
    buf[(h - 1) * w + x] = prev;
    for (var y = h - 1; y > 0; y = y - 1) {
        prev = fma_pos(b.nu, prev, buf[(y - 1) * w + x]);
        buf[(y - 1) * w + x] = prev;
    }
}
