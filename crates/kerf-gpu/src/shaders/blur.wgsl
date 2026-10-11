// `gblur` (vf_gblur.c): a first-order recursive Gaussian in `float`, along every row (rightwards
// then leftwards) and then along every column (downwards then upwards), in `horiz_slice_c`'s and
// `do_vertical_columns`' order. One invocation per row and per column, each running its recursion
// sequentially in a storage buffer, as the filter does in place. `blur_out.wgsl` scales, clips and
// rounds the result back to an 8-bit plane.

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
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> buf: array<f32>;

@compute @workgroup_size(64)
fn cs_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    let y = i32(gid.x);
    if (y >= b.size.y) {
        return;
    }
    let w = b.size.x;
    let base = y * w;
    for (var x = 0; x < w; x = x + 1) {
        buf[base + x] = round(textureLoad(src, vec2<i32>(x, y), 0).r * 255.0);
    }
    buf[base] = buf[base] * b.boundary;
    for (var x = 1; x < w; x = x + 1) {
        buf[base + x] = buf[base + x] + b.nu * buf[base + x - 1];
    }
    buf[base + w - 1] = buf[base + w - 1] * b.boundary;
    for (var x = w - 1; x > 0; x = x - 1) {
        buf[base + x - 1] = buf[base + x - 1] + b.nu * buf[base + x];
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
    buf[x] = buf[x] * b.boundary;
    for (var y = 1; y < h; y = y + 1) {
        buf[y * w + x] = buf[y * w + x] + b.nu * buf[(y - 1) * w + x];
    }
    buf[(h - 1) * w + x] = buf[(h - 1) * w + x] * b.boundary;
    for (var y = h - 1; y > 0; y = y - 1) {
        buf[(y - 1) * w + x] = buf[(y - 1) * w + x] + b.nu * buf[y * w + x];
    }
}
