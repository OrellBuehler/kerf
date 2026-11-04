// The unit test's entry point over `fma.wgsl` (`blur_probe.rs`, `the_shaders_fused_multiply_add_...`): one triple of bit patterns in,
// the fused multiply-add and the plain multiply out.

@group(0) @binding(0) var<storage, read> triples: array<vec4<u32>>;
@group(0) @binding(1) var<storage, read_write> results: array<vec2<u32>>;

@compute @workgroup_size(64)
fn cs_test(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= arrayLength(&triples)) {
        return;
    }
    let t = triples[i];
    results[i] = vec2<u32>(fma_pos(t.x, t.y, t.z), mul_pos(t.x, t.y));
}
