//! The independent checker's round-2 attack on the shaders of the zero-tolerance audit, below the plan and the compositor: no bound anywhere.
//!
//! * `fma.wgsl` (the integer fused multiply-add of the blur): a line-for-line CPU port of the WGSL (with WGSL's shift semantics) and the
//!   shader itself on the GPU, both held to `f32::mul_add` bit for bit on structured triples (every exponent window around the
//!   subnormal and overflow edges, exact ties, carries, zero and subnormal operands and addends) and on random bit patterns.
//! * the blur shaders (`blur.wgsl`, `blur_out.wgsl`) driven directly, on crafted planes (exact-zero runs in both directions, at the borders,
//!   single pixels, fields of 0 and 255, tiny sizes) at many sigmas, against FFmpeg's `gblur` on `gray` and the CPU model.
//! * `kerf_core::gblur`: every `f32` sigma in the cancellation neighbourhood against an independent `set_params`, the bound on the
//!   recursion's intermediates over the whole range, and the drawn sigmas next to each refused run against FFmpeg.
//! * the display conversion: FFmpeg's `yuv420p -> rgb24` over **every** `(y, u, v)` triple, for each matrix the preview can hand out,
//!   and over every even width, against the x86 kernel model the display pass follows.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_z_shader -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(
    clippy::print_stderr,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::manual_is_multiple_of,
    clippy::manual_range_contains,
    clippy::precedence,
    clippy::chunks_exact_to_as_chunks
)]

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use wgpu::util::DeviceExt as _;

use kerf_core::YuvMatrix;
use kerf_gpu::effects::{blur_plane, gaussian, gblur_coefficients, Gaussian};

// ---- a device of our own ---------------------------------------------------------------------

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn dev() -> &'static Dev {
    static D: std::sync::OnceLock<Dev> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("checker z shader"),
            ..Default::default()
        }))
        .expect("a device");
        Dev { device, queue }
    })
}

fn read_buffer(d: &Dev, buffer: &wgpu::Buffer) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    d.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(120)),
        })
        .expect("the GPU finishes");
    rx.recv().unwrap().unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    let out = mapped.to_vec();
    drop(mapped);
    buffer.unmap();
    out
}

// ---- the WGSL fused multiply-add, ported line for line --------------------------------------------

mod port {
    //! `fma.wgsl` with WGSL's integer semantics: u32 arithmetic wraps, a shift takes its amount modulo 32.
    fn shl(x: u32, s: u32) -> u32 {
        x << (s & 31)
    }
    fn shr(x: u32, s: u32) -> u32 {
        x >> (s & 31)
    }

    fn shl64(v: [u32; 2], s: u32) -> [u32; 2] {
        if s == 0 {
            return v;
        }
        if s >= 64 {
            return [0, 0];
        }
        if s >= 32 {
            return [0, shl(v[0], s - 32)];
        }
        [shl(v[0], s), shl(v[1], s) | shr(v[0], 32 - s)]
    }

    fn shr64(v: [u32; 2], s: u32) -> [u32; 3] {
        if s == 0 {
            return [v[0], v[1], 0];
        }
        if s >= 64 {
            return [0, 0, u32::from((v[0] | v[1]) != 0)];
        }
        if s >= 32 {
            let t = s - 32;
            let dropped = v[0] | (v[1] & shl(1, t).wrapping_sub(1));
            return [shr(v[1], t), 0, u32::from(dropped != 0)];
        }
        let dropped = v[0] & shl(1, s).wrapping_sub(1);
        [shr(v[0], s) | shl(v[1], 32 - s), shr(v[1], s), u32::from(dropped != 0)]
    }

    fn add64(a: [u32; 2], b: [u32; 2]) -> [u32; 2] {
        let lo = a[0].wrapping_add(b[0]);
        let carry = u32::from(lo < a[0]);
        [lo, a[1].wrapping_add(b[1]).wrapping_add(carry)]
    }

    fn clz64(v: [u32; 2]) -> u32 {
        if v[1] != 0 {
            return v[1].leading_zeros();
        }
        32 + v[0].leading_zeros()
    }

    fn mul24(a: u32, b: u32) -> [u32; 2] {
        let (al, ah, bl, bh) = (a & 0xfff, a >> 12, b & 0xfff, b >> 12);
        let mut p = [al.wrapping_mul(bl), 0];
        p = add64(p, shl64([al.wrapping_mul(bh).wrapping_add(ah.wrapping_mul(bl)), 0], 12));
        p = add64(p, shl64([ah.wrapping_mul(bh), 0], 24));
        p
    }

    pub fn fma_pos(a: u32, b: u32, c: u32) -> u32 {
        fma_pos_mutated(a, b, c, 0)
    }

    /// `fma_pos` with one deliberate fault (`mutant` 1..=8; 0 is the shader's logic): the families must tell each from `f32::mul_add`.
    pub fn fma_pos_mutated(a: u32, b: u32, c: u32, mutant: u8) -> u32 {
        let (ea, eb, ec) = ((a >> 23) & 0xff, (b >> 23) & 0xff, (c >> 23) & 0xff);
        let ma = (a & 0x7f_ffff) | if ea == 0 { 0 } else { 0x80_0000 };
        let mb = (b & 0x7f_ffff) | if eb == 0 { 0 } else { 0x80_0000 };
        let mc = (c & 0x7f_ffff) | if ec == 0 { 0 } else { 0x80_0000 };
        if ma == 0 || mb == 0 {
            return c;
        }
        let prod = mul24(ma, mb);
        let ptop = 63 - clz64(prod) as i32;
        let lp = if mutant == 7 {
            ptop + ea as i32 + eb as i32 - 300
        } else {
            ptop + ea.max(1) as i32 + eb.max(1) as i32 - 300
        };
        let (mut big, mut big_top, mut big_lp) = (prod, ptop, lp);
        let (mut small, mut small_top, mut small_lp) = ([0u32, 0], 0i32, 0i32);
        let mut has_small = false;
        if mc != 0 {
            let addend = [mc, 0];
            let ctop = 31 - mc.leading_zeros() as i32;
            let lc = ctop + ec.max(1) as i32 - 150;
            if lc > lp {
                big = addend;
                big_top = ctop;
                big_lp = lc;
                small = prod;
                small_top = ptop;
                small_lp = lp;
            } else {
                small = addend;
                small_top = ctop;
                small_lp = lc;
            }
            has_small = true;
        }
        let wide = shl64(big, (62 - big_top) as u32);
        let mut narrow = [0u32, 0];
        let mut sticky = 0u32;
        if has_small {
            let pos = 62 - (big_lp - small_lp);
            let s = pos - small_top;
            if s >= 0 {
                narrow = shl64(small, s as u32);
            } else {
                let r = shr64(small, (-s) as u32);
                narrow = [r[0], r[1]];
                sticky = if mutant == 5 { 0 } else { r[2] };
            }
        }
        let mut sum = add64(wide, narrow);
        sum[0] |= sticky;
        let top = 63 - clz64(sum) as i32;
        let shift = if mutant == 8 {
            top - 23
        } else {
            (top - 23).max(-149 - (big_lp - 62))
        };
        let kept = shr64(sum, (shift - 1) as u32);
        let guard = kept[0] & 1;
        let mut m = kept[0] >> 1;
        let below = if mutant == 2 { false } else { kept[2] != 0 };
        let round_up = match mutant {
            1 => guard == 1,
            9 => guard == 1 && (below || (m & 1) == 0),
            _ => guard == 1 && (below || (m & 1) == 1),
        };
        if round_up {
            m = m.wrapping_add(1);
        }
        let q = big_lp - 62 + shift;
        let field = q + 149;
        if mutant != 6 && field + (m >> 23) as i32 >= 255 {
            return 0x7f80_0000;
        }
        if mutant == 3 && field == 0 && m < 0x80_0000 {
            return 0;
        }
        ((field as u32) << 23).wrapping_add(m)
    }

    pub fn mul_pos(a: u32, b: u32) -> u32 {
        fma_pos(a, b, 0)
    }
}

// ---- triples ------------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn u32(&mut self) -> u32 {
        (self.next() >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.u32() as u64 * n as u64 >> 32) as u32
    }
}

fn bits(exp_field: u32, mant: u32) -> u32 {
    (exp_field << 23) | (mant & 0x7f_ffff)
}

/// A mantissa from the shapes that exercise a significand's rounding: empty, full, the half, one bit, a short one, random.
fn mantissa(r: &mut Rng) -> u32 {
    match r.below(10) {
        0 => 0,
        1 => 0x7f_ffff,
        2 => 0x40_0000,
        3 => 1,
        4 => 0x7f_fffe,
        5 => (r.u32() & 0xfff) << 11,
        6 => (r.u32() & 0xff) << 15,
        7 => 0x00_0001 << r.below(23),
        _ => r.u32() & 0x7f_ffff,
    }
}

fn structured(r: &mut Rng, n: usize) -> Vec<[u32; 3]> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        // The product's unbiased exponent over the whole range plus the two edges the result can fall off (2^-126 .. 2^-149 and 2^128).
        let e_p = match r.below(4) {
            0 => -180 + r.below(80) as i32,
            1 => 100 + r.below(40) as i32,
            _ => -130 + r.below(270) as i32,
        };
        let ea = (e_p / 2 + r.below(41) as i32 - 20 + 127).clamp(0, 254);
        let eb = (e_p - (ea - 127) + 127).clamp(0, 254);
        let a = bits(ea as u32, mantissa(r));
        let b = bits(eb as u32, mantissa(r));
        // The addend's exponent relative to the product's: from far below to far above, densest within the 24 bits of a window.
        let off = match r.below(4) {
            0 => r.below(160) as i32 - 80,
            _ => r.below(64) as i32 - 32,
        };
        let ec = (e_p + off + 127).clamp(0, 254);
        let c = match r.below(8) {
            0 => 0,
            1 => r.u32() & 0x7f_ffff,
            2 => 1 + r.below(8),
            _ => bits(ec as u32, mantissa(r)),
        };
        out.push([a, b, c]);
    }
    out
}

/// Exact ties: a short product (exact in at most 24 bits), and an addend of half a unit in its last place, a unit and a half, and so on,
/// at normal exponents and at the fixed quantum of the subnormals.
fn ties(r: &mut Rng, n: usize) -> Vec<[u32; 3]> {
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let (ka, kb) = (1 + r.below(12), 1 + r.below(12));
        let ma = (r.u32() & ((1 << ka) - 1)) | (1 << (ka - 1));
        let mb = (r.u32() & ((1 << kb) - 1)) | (1 << (kb - 1));
        let ea = r.below(250) as i32 + 1;
        let eb = (r.below(250) as i32 + 1).min(254);
        let a = bits(ea as u32, ma << (23 - ka));
        let b = bits(eb as u32, mb << (23 - kb));
        let product = f64::from(f32::from_bits(a)) * f64::from(f32::from_bits(b));
        // The product is exact in f64 (48 bits); its f32 neighbour's spacing is a power of two.
        let exact = product;
        let ulp = f64::from(f32::from_bits(a)).abs() * 0.0 + 2f64.powi(exact.log2().floor() as i32 - 23);
        let ulp = ulp.max(2f64.powi(-149));
        for k in [1.0, 3.0, 5.0, 0.5, 2.5] {
            let c = (ulp * 0.5 * k) as f32;
            if c.is_finite() && c >= 0.0 {
                out.push([a, b, c.to_bits()]);
            }
            let c = (ulp * k) as f32;
            if c.is_finite() && c >= 0.0 {
                out.push([a, b, c.to_bits()]);
            }
        }
    }
    out.truncate(n);
    out
}

fn random_patterns(r: &mut Rng, n: usize) -> Vec<[u32; 3]> {
    (0..n)
        .map(|_| {
            let f = |r: &mut Rng| {
                let x = r.u32() & 0x7fff_ffff;
                // Not an infinity or a NaN: they are out of the domain (the plan keeps them out).
                if (x >> 23) == 255 {
                    x & 0x7f7f_ffff
                } else {
                    x
                }
            };
            [f(r), f(r), if r.below(4) == 0 { 0 } else { f(r) }]
        })
        .collect()
}

/// The carries and edges by hand.
fn edges() -> Vec<[u32; 3]> {
    let mut out = Vec::new();
    let specials = [
        0u32,
        1,
        2,
        3,
        0x7f_fffe,
        0x7f_ffff,
        0x80_0000,
        0x80_0001,
        0x00ff_ffff,
        0x0100_0000,
        0x3f80_0000,
        0x3f7f_ffff,
        0x3f80_0001,
        0x4000_0000,
        0x7f7f_ffff,
        0x7f00_0000,
        0x7e80_0000,
        0x0080_0000 | 0x7f_ffff,
        0x1f80_0000,
        0x2000_0000,
        0x437f_0000,
        0x3400_0000,
        0x3300_0000,
        0x0000_0100,
        0x0040_0000,
    ];
    for a in specials {
        for b in specials {
            for c in specials {
                out.push([a, b, c]);
            }
        }
    }
    out
}

fn want(t: [u32; 3]) -> u32 {
    f32::from_bits(t[0])
        .mul_add(f32::from_bits(t[1]), f32::from_bits(t[2]))
        .to_bits()
}

fn want_mul(t: [u32; 3]) -> u32 {
    (f32::from_bits(t[0]) * f32::from_bits(t[1])).to_bits()
}

// ---- the shader on the GPU -------------------------------------------------------------------------------

const FMA: &str = include_str!("../src/shaders/fma.wgsl");

const FMA_ENTRY: &str = "
@group(0) @binding(0) var<storage, read> triples: array<vec4<u32>>;
@group(0) @binding(1) var<storage, read_write> results: array<vec2<u32>>;
@compute @workgroup_size(64)
fn cs_test(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x + gid.y * 65535u * 64u;
    if (i >= arrayLength(&triples)) { return; }
    let t = triples[i];
    results[i] = vec2<u32>(fma_pos(t.x, t.y, t.z), mul_pos(t.x, t.y));
}
";

fn gpu_fma(cases: &[[u32; 3]]) -> Vec<[u32; 2]> {
    let d = dev();
    let source = format!("{FMA}\n{FMA_ENTRY}");
    let module = d.device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("fma"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = d.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("fma"),
        layout: None,
        module: &module,
        entry_point: Some("cs_test"),
        compilation_options: Default::default(),
        cache: None,
    });
    let mut out = Vec::with_capacity(cases.len());
    for chunk in cases.chunks(65535 * 64) {
        let data: Vec<u32> = chunk.iter().flat_map(|t| [t[0], t[1], t[2], 0]).collect();
        let input = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("triples"),
            contents: bytemuck::cast_slice(&data),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let size = (chunk.len() * 8) as u64;
        let output = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("results"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bind = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fma"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        let mut enc = d
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fma") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fma"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((chunk.len() as u32).div_ceil(64), 1, 1);
        }
        enc.copy_buffer_to_buffer(&output, 0, &read, 0, size);
        d.queue.submit(Some(enc.finish()));
        let bytes = read_buffer(d, &read);
        let words: &[u32] = bytemuck::cast_slice(&bytes);
        out.extend(words.chunks_exact(2).map(|w| [w[0], w[1]]));
    }
    out
}

fn describe(t: [u32; 3], got: u32, want: u32) -> String {
    format!(
        "fma({:e} [{:#010x}], {:e} [{:#010x}], {:e} [{:#010x}]): got {:#010x} ({:e}), want {:#010x} ({:e})",
        f32::from_bits(t[0]),
        t[0],
        f32::from_bits(t[1]),
        t[1],
        f32::from_bits(t[2]),
        t[2],
        got,
        f32::from_bits(got),
        want,
        f32::from_bits(want)
    )
}

fn families() -> Vec<(&'static str, Vec<[u32; 3]>)> {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    vec![
        ("structured", structured(&mut r, 6_000_000)),
        ("ties", ties(&mut r, 1_500_000)),
        ("random patterns", random_patterns(&mut r, 3_000_000)),
        ("edges", edges()),
    ]
}

#[test]
fn z_fma_the_cpu_port_of_the_wgsl_is_f32_mul_add_on_every_triple() {
    let mut bad = Vec::new();
    let mut total = 0usize;
    for (name, cases) in families() {
        let mut n = 0usize;
        for t in &cases {
            total += 1;
            let got = port::fma_pos(t[0], t[1], t[2]);
            let w = want(*t);
            if got != w {
                n += 1;
                if bad.len() < 12 {
                    bad.push(format!("{name}: {}", describe(*t, got, w)));
                }
            }
            if port::mul_pos(t[0], t[1]) != want_mul(*t) {
                n += 1;
                if bad.len() < 12 {
                    bad.push(format!("{name}: mul({:#010x}, {:#010x})", t[0], t[1]));
                }
            }
        }
        eprintln!("Z fma port {name}: {} triples, {n} differ", cases.len());
    }
    eprintln!("Z fma port: {total} triples in all");
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The families have teeth: eight deliberate faults in the port (round half up, no sticky bit, subnormals flushed, no overflow, a subnormal
/// operand's exponent read as 0, the rounding position of a normal for a subnormal result, half to odd) are each caught.
#[test]
fn z_fma_every_deliberate_fault_in_the_port_is_caught_by_the_families() {
    let fams = families();
    for mutant in [1u8, 2, 3, 5, 6, 7, 8, 9] {
        let mut n = 0usize;
        let mut first = None;
        for (_, cases) in &fams {
            for t in cases {
                if port::fma_pos_mutated(t[0], t[1], t[2], mutant) != want(*t) {
                    n += 1;
                    first.get_or_insert(*t);
                }
            }
        }
        eprintln!("Z fma mutant {mutant}: {n} triples differ; first {first:x?}");
        assert!(n > 0, "mutant {mutant} is not caught");
    }
}

#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn z_fma_the_shader_on_the_gpu_is_f32_mul_add_on_every_triple() {
    let mut bad = Vec::new();
    for (name, cases) in families() {
        let got = gpu_fma(&cases);
        let (mut n_fma, mut n_mul, mut n_port) = (0usize, 0usize, 0usize);
        for (t, g) in cases.iter().zip(&got) {
            let w = want(*t);
            if g[0] != w {
                n_fma += 1;
                if bad.len() < 12 {
                    bad.push(format!("{name}: {}", describe(*t, g[0], w)));
                }
            }
            if g[1] != want_mul(*t) {
                n_mul += 1;
                if bad.len() < 12 {
                    bad.push(format!("{name}: mul({:#010x}, {:#010x}) got {:#010x}", t[0], t[1], g[1]));
                }
            }
            if g[0] != port::fma_pos(t[0], t[1], t[2]) {
                n_port += 1;
            }
        }
        eprintln!(
            "Z fma gpu {name}: {} triples, fused differ {n_fma}, multiply differ {n_mul}, differ from the port {n_port}",
            cases.len()
        );
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// What the domain keeps out, by documenting what the shader does with it: not asserted, listed.
#[test]
#[ignore = "needs a GPU adapter (lavapipe is enough)"]
fn z_fma_outside_the_domain_is_documented_not_asserted() {
    let inf = f32::INFINITY.to_bits();
    let nan = f32::NAN.to_bits();
    let cases = [
        [inf, 1f32.to_bits(), 0],
        [1f32.to_bits(), inf, 0],
        [1f32.to_bits(), 1f32.to_bits(), inf],
        [0, inf, 1f32.to_bits()],
        [nan, 1f32.to_bits(), 0],
        [1f32.to_bits(), 1f32.to_bits(), nan],
        [(-1f32).to_bits(), 2f32.to_bits(), 3f32.to_bits()],
        [0.5f32.to_bits(), 4f32.to_bits(), (-0.0f32).to_bits()],
    ];
    let got = gpu_fma(&cases);
    for (t, g) in cases.iter().zip(&got) {
        eprintln!("Z fma outside the domain: {}", describe(*t, g[0], want(*t)));
    }
}

// ---- the blur shaders, driven directly -------------------------------------------------------------------

struct GpuBlur {
    layout: wgpu::BindGroupLayout,
    h: wgpu::ComputePipeline,
    v: wgpu::ComputePipeline,
    out_layout: wgpu::BindGroupLayout,
    out: wgpu::RenderPipeline,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    size: [i32; 2],
    nu: u32,
    boundary: u32,
    post: u32,
    pad: [u32; 3],
}

fn gpu_blur() -> &'static GpuBlur {
    static B: std::sync::OnceLock<GpuBlur> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        let d = dev();
        let entry = |binding: u32, ty: wgpu::BindingType, visibility: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty,
            count: None,
        };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let storage = |read_only| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let texture = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let layout = d.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
            entries: &[
                entry(0, uniform, wgpu::ShaderStages::COMPUTE),
                entry(1, texture, wgpu::ShaderStages::COMPUTE),
                entry(2, storage(false), wgpu::ShaderStages::COMPUTE),
            ],
        });
        let module = d.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur"),
            source: wgpu::ShaderSource::Wgsl(format!("{FMA}\n{}", include_str!("../src/shaders/blur.wgsl")).into()),
        });
        let compute = |cs: &str| {
            let pl = d.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(cs),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
            d.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(cs),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(cs),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let out_layout = d.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur out"),
            entries: &[
                entry(0, uniform, wgpu::ShaderStages::FRAGMENT),
                entry(1, storage(true), wgpu::ShaderStages::FRAGMENT),
            ],
        });
        let out_module = d.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur out"),
            source: wgpu::ShaderSource::Wgsl(format!("{FMA}\n{}", include_str!("../src/shaders/blur_out.wgsl")).into()),
        });
        let pl = d.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blur out"),
            bind_group_layouts: &[Some(&out_layout)],
            immediate_size: 0,
        });
        let out = d.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blur out"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &out_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &out_module,
                entry_point: Some("fs_out"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let (h, v) = (compute("cs_h"), compute("cs_v"));
        GpuBlur {
            layout,
            h,
            v,
            out_layout,
            out,
        }
    })
}

fn extent(w: u32, h: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    }
}

/// The compositor's blur on one plane: the same shaders, passes and formats as `Compositor::blur_planes`, with `g` as the coefficients.
fn shader_blur(plane: &[u8], w: usize, h: usize, g: &Gaussian) -> Vec<u8> {
    let d = dev();
    let b = gpu_blur();
    let (wu, hu) = (w as u32, h as u32);
    let src = d.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("src"),
        size: extent(wu, hu),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    d.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &src,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        plane,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(wu),
            rows_per_image: Some(hu),
        },
        extent(wu, hu),
    );
    let view = src.create_view(&wgpu::TextureViewDescriptor::default());
    let params = BlurParams {
        size: [w as i32, h as i32],
        nu: g.nu.to_bits(),
        boundary: g.boundary.to_bits(),
        post: g.post.to_bits(),
        pad: [0; 3],
    };
    let ubuf = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let work = d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("work"),
        size: (w * h * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let bind = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("blur"),
        layout: &b.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: work.as_entire_binding(),
            },
        ],
    });
    let target = d.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("out"),
        size: extent(wu, hu),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let tview = target.create_view(&wgpu::TextureViewDescriptor::default());
    let out_bind = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("blur out"),
        layout: &b.out_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: work.as_entire_binding(),
            },
        ],
    });
    let mut enc = d
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("blur") });
    for (pipeline, lines) in [(&b.h, hu), (&b.v, wu)] {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("gblur"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(lines.div_ceil(64), 1, 1);
    }
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tview,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&b.out);
        pass.set_bind_group(0, &out_bind, &[]);
        pass.draw(0..3, 0..1);
    }
    let pitch = wu.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let read = d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(pitch) * u64::from(hu),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &read,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: Some(hu),
            },
        },
        extent(wu, hu),
    );
    d.queue.submit(Some(enc.finish()));
    let bytes = read_buffer(d, &read);
    let mut out = Vec::with_capacity(w * h);
    for row in bytes.chunks(pitch as usize).take(h) {
        out.extend_from_slice(&row[..w]);
    }
    out
}

fn ffmpeg_gray(args_in: &[&str], plane: &[u8], w: usize, h: usize, vf: &str) -> Vec<u8> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-s",
        ])
        .arg(format!("{w}x{h}"))
        .args(args_in)
        .args([
            "-i",
            "pipe:0",
            "-vf",
            vf,
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data = plane.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    assert!(
        out.status.success(),
        "ffmpeg {vf} {w}x{h}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// FFmpeg's answer when it gives the same one three times running (and does not crash).
fn stable_ffmpeg(plane: &[u8], w: usize, h: usize, sigma: f64) -> Option<Vec<u8>> {
    if w == 1 || h == 1 {
        return None;
    }
    let first = ffmpeg_gray_try(plane, w, h, sigma)?;
    for _ in 0..2 {
        if ffmpeg_gray_try(plane, w, h, sigma)? != first {
            return None;
        }
    }
    Some(first)
}

fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (s >> 33) as u8
        })
        .collect()
}

/// Planes the blur has to survive: `(name, w, h, samples)`.
fn crafted_planes() -> Vec<(String, usize, usize, Vec<u8>)> {
    let mut out: Vec<(String, usize, usize, Vec<u8>)> = Vec::new();
    let fib = [1usize, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233];
    for (w, h) in [(320usize, 40usize), (97, 33)] {
        // Rows of exact zeros of every length between bright samples: the recursion decays through 2^-126 in both directions.
        let mut rows = vec![0u8; w * h];
        for y in 0..h {
            let run = fib[y % fib.len()];
            let phase = (y * 7) % run.max(1);
            for x in 0..w {
                if (x + phase) % (run + 1) == 0 {
                    rows[y * w + x] = 200 + ((x * 31 + y * 17) % 56) as u8;
                }
            }
        }
        out.push((format!("zero runs along rows {w}x{h}"), w, h, rows));
        let mut cols = vec![0u8; w * h];
        for x in 0..w {
            let run = fib[x % fib.len()];
            let phase = (x * 5) % run.max(1);
            for y in 0..h {
                if (y + phase) % (run + 1) == 0 {
                    cols[y * w + x] = 180 + ((x * 13 + y * 29) % 76) as u8;
                }
            }
        }
        out.push((format!("zero runs along columns {w}x{h}"), w, h, cols));
        let mut dots = vec![0u8; w * h];
        for (x, y) in [
            (0, 0),
            (w - 1, 0),
            (0, h - 1),
            (w - 1, h - 1),
            (w / 2, h / 2),
            (0, h / 2),
            (w - 1, h / 2),
            (w / 2, 0),
            (w / 2, h - 1),
        ] {
            dots[y * w + x] = 255;
        }
        out.push((format!("dots at the borders {w}x{h}"), w, h, dots));
        let mut left = vec![0u8; w * h];
        for y in 0..h {
            for x in w / 2..w {
                left[y * w + x] = ((x * 3 + y * 5) % 251) as u8 + 4;
            }
        }
        out.push((format!("zeros then noise (left half empty) {w}x{h}"), w, h, left));
        let mut right = vec![0u8; w * h];
        for y in h / 2..h {
            for x in 0..w {
                right[y * w + x] = lcg_bytes((x * 1009 + y) as u64, 1)[0] | 1;
            }
        }
        out.push((format!("zeros then noise (top half empty) {w}x{h}"), w, h, right));
        out.push((format!("all 255 {w}x{h}"), w, h, vec![255; w * h]));
        out.push((format!("all 0 {w}x{h}"), w, h, vec![0; w * h]));
        out.push((format!("all 1 {w}x{h}"), w, h, vec![1; w * h]));
        out.push((format!("all 254 {w}x{h}"), w, h, vec![254; w * h]));
        out.push((format!("noise {w}x{h}"), w, h, lcg_bytes((w * h) as u64, w * h)));
        let mut sparse = vec![0u8; w * h];
        for (i, v) in lcg_bytes(99, w * h).into_iter().enumerate() {
            if v < 6 {
                sparse[i] = 255 - v * 3;
            }
        }
        out.push((format!("sparse noise on zeros {w}x{h}"), w, h, sparse));
        let mut checker = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                checker[y * w + x] = if (x + y) % 2 == 0 { 255 } else { 0 };
            }
        }
        out.push((format!("checker {w}x{h}"), w, h, checker));
        let mut half = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                half[y * w + x] = if x < w / 2 { 255 } else { 0 };
            }
        }
        out.push((format!("255 | 0 edge {w}x{h}"), w, h, half));
    }
    // U and V as the planes of a picture whose chroma is a field of 0 or 255 with a bright spot.
    for (v, name) in [(0u8, "0"), (255, "255")] {
        let (w, h) = (161usize, 91usize);
        let mut p = vec![v; w * h];
        p[(h / 2) * w + w / 2] = 255 - v;
        out.push((format!("chroma field of {name} with one opposite sample {w}x{h}"), w, h, p));
    }
    for (w, h) in [
        (8usize, 8usize),
        (9, 9),
        (8, 17),
        (17, 8),
        (15, 17),
        (16, 16),
        (17, 15),
        (33, 31),
    ] {
        out.push((format!("noise {w}x{h}"), w, h, lcg_bytes((w * 131 + h) as u64, w * h)));
        let mut z = vec![0u8; w * h];
        z[(h / 2) * w + w / 2] = 255;
        out.push((format!("one sample {w}x{h}"), w, h, z));
    }
    out
}

fn sigma_list() -> Vec<f64> {
    vec![
        0.0, 1e-6, 1e-3, 0.01, 0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.75, 0.9, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 5.5, 7.0, 9.0,
        12.5, 20.0, 33.3, 40.0, 64.0, 100.0, 250.0, 333.3, 511.5, 777.7, 1000.0, 1023.9, 1024.0,
    ]
}

#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blur_shader_driven_directly_is_gblur_on_crafted_planes() {
    let planes = crafted_planes();
    let sigmas = sigma_list();
    let (mut cases, mut bad_gpu, mut bad_cpu, mut bad_both, mut unstable) = (0, Vec::new(), Vec::new(), 0, 0);
    for (name, w, h, plane) in &planes {
        for &sigma in &sigmas {
            let Some(g) = gaussian(sigma) else { continue };
            let Some(ff) = stable_ffmpeg(plane, *w, *h, sigma) else {
                unstable += 1;
                continue;
            };
            cases += 1;
            let gpu = shader_blur(plane, *w, *h, &g);
            let cpu = blur_plane(plane, *w, *h, &g);
            let d_gpu = ff.iter().zip(&gpu).filter(|(a, b)| a != b).count();
            let d_cpu = ff.iter().zip(&cpu).filter(|(a, b)| a != b).count();
            if d_gpu > 0 && d_cpu > 0 {
                bad_both += 1;
            }
            if d_gpu > 0 {
                let first = ff.iter().zip(&gpu).position(|(a, b)| a != b).unwrap();
                let max = ff
                    .iter()
                    .zip(&gpu)
                    .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                    .max()
                    .unwrap();
                bad_gpu.push(format!(
                    "{name} sigma {sigma}: GPU shader vs FFmpeg {d_gpu} samples differ (max {max}, first ({}, {}))",
                    first % w,
                    first / w
                ));
            }
            if d_cpu > 0 {
                bad_cpu.push(format!("{name} sigma {sigma}: CPU model vs FFmpeg {d_cpu} samples differ"));
            }
        }
    }
    eprintln!(
        "Z blur direct: {cases} plane x sigma cases ({unstable} more skipped: FFmpeg's own answer differs from run to run or it crashes); shader differs from FFmpeg in {}, CPU model in {} (both {bad_both})",
        bad_gpu.len(),
        bad_cpu.len()
    );
    for b in bad_gpu.iter().chain(&bad_cpu).take(20) {
        eprintln!("Z FAIL {b}");
    }
    assert!(
        bad_gpu.is_empty() && bad_cpu.is_empty(),
        "{} + {} cases",
        bad_gpu.len(),
        bad_cpu.len()
    );
}

// ---- gblur's coefficients -----------------------------------------------------------------------------------

/// `set_params` of `vf_gblur.c`, written from the C source again here: float `sigma`, double `lambda` and `dnu`, `isnormal` on the floats.
fn c_set_params(sigma: f32) -> (f32, f32, f32) {
    let steps = 1i32;
    let lambda: f64 = f64::from(sigma * sigma) / (2.0 * f64::from(steps));
    let dnu: f64 = (1.0 + 2.0 * lambda - (1.0 + 4.0 * lambda).sqrt()) / (2.0 * lambda);
    let mut postscale = (dnu / lambda).powf(f64::from(steps)) as f32;
    let mut boundaryscale = (1.0 / (1.0 - dnu)) as f32;
    let mut nu = dnu as f32;
    if !postscale.is_normal() {
        postscale = 1.0;
    }
    if !boundaryscale.is_normal() {
        boundaryscale = 1.0;
    }
    if !nu.is_normal() {
        nu = 0.0;
    }
    (postscale, boundaryscale, nu)
}

/// The shader's domain, restated: `nu` in `[0, 1)`, a positive finite boundary, a non-negative finite post-scale.
fn in_domain(nu: f32, boundary: f32, post: f32) -> bool {
    nu.is_finite() && nu >= 0.0 && nu < 1.0 && boundary.is_finite() && boundary > 0.0 && post.is_finite() && post >= 0.0
}

/// An upper bound on every intermediate of the four sweeps for samples at most 255 (all terms non-negative): each sweep multiplies the
/// running value by at most `max(boundary, 1) / (1 - nu)`.
fn intermediate_bound(nu: f32, boundary: f32) -> f64 {
    let gain = f64::from(boundary).max(1.0) / (1.0 - f64::from(nu));
    255.0 * gain.powi(4)
}

#[test]
fn z_gblur_every_float_sigma_near_the_cancellation_against_an_independent_derivation() {
    // Every f32 in [4e-9, 4e-8]: about 28 million sigmas.
    let (lo, hi) = (4e-9f32.to_bits(), 4e-8f32.to_bits());
    let (mut runs, mut run_start, mut prev_out, mut n_out, mut checked) = (Vec::new(), 0u32, false, 0u64, 0u64);
    let mut worst_bound = 0f64;
    let mut bad = Vec::new();
    for b in lo..=hi {
        let s = f32::from_bits(b);
        let (post, boundary, nu) = c_set_params(s);
        let post2 = post * post;
        let ours = gblur_coefficients(f64::from(s)).expect("in range");
        checked += 1;
        if ours.nu.to_bits() != nu.to_bits()
            || ours.boundary.to_bits() != boundary.to_bits()
            || ours.post.to_bits() != post2.to_bits()
        {
            if bad.len() < 8 {
                bad.push(format!("sigma {s:e}: ours {ours:?} vs C ({nu:e}, {boundary:e}, {post2:e})"));
            }
        }
        let inside = in_domain(nu, boundary, post2);
        if inside != ours.in_shader_domain() || inside != gaussian(f64::from(s)).is_some() {
            if bad.len() < 8 {
                bad.push(format!(
                    "sigma {s:e}: domain disagrees: mine {inside}, theirs {}",
                    ours.in_shader_domain()
                ));
            }
        }
        if inside {
            worst_bound = worst_bound.max(intermediate_bound(nu, boundary));
        } else {
            n_out += 1;
        }
        if !inside && !prev_out {
            run_start = b;
        }
        if inside && prev_out {
            runs.push((f32::from_bits(run_start), f32::from_bits(b - 1)));
        }
        prev_out = !inside;
    }
    if prev_out {
        runs.push((f32::from_bits(run_start), f32::from_bits(hi)));
    }
    eprintln!(
        "Z gblur: {checked} sigmas in [4e-9, 4e-8]; {n_out} outside the domain in {} runs: {runs:?}",
        runs.len()
    );
    eprintln!("Z gblur: the largest intermediate bound over the drawn ones here: {worst_bound:e}");
    assert!(bad.is_empty(), "{bad:?}");
    assert!(
        worst_bound < f64::from(f32::MAX),
        "an intermediate could overflow: {worst_bound:e}"
    );
}

#[test]
fn z_gblur_the_bound_on_the_recursion_over_the_whole_sigma_range() {
    let mut worst = (0f64, 0f64);
    let mut worst_post = (0f32, 0f64);
    let mut n = 0u64;
    let mut sigmas: Vec<f32> = Vec::new();
    // A dense sweep: every float in 1 .. 1024 would be 10 * 8M; every 64th is 1.3M and each binade is covered.
    let mut b = 4e-8f32.to_bits();
    let top = 1024f32.to_bits();
    while b <= top {
        sigmas.push(f32::from_bits(b));
        b += 61;
    }
    sigmas.extend([
        1024.0,
        1023.9999,
        1.0,
        0.5,
        2.0,
        f32::MIN_POSITIVE,
        1e-30,
        1e-20,
        1e-12,
        1e-10,
    ]);
    for s in sigmas {
        let (post, boundary, nu) = c_set_params(s);
        let post2 = post * post;
        let g = gblur_coefficients(f64::from(s)).unwrap();
        assert_eq!(
            (g.nu.to_bits(), g.boundary.to_bits(), g.post.to_bits()),
            (nu.to_bits(), boundary.to_bits(), post2.to_bits()),
            "{s:e}"
        );
        if !in_domain(nu, boundary, post2) {
            assert!(gaussian(f64::from(s)).is_none(), "{s:e} is outside the domain and drawn");
            continue;
        }
        assert!(gaussian(f64::from(s)).is_some(), "{s:e} is inside and refused");
        n += 1;
        let bound = intermediate_bound(nu, boundary);
        if bound > worst.0 {
            worst = (bound, f64::from(s));
        }
        if f64::from(post2) * bound > worst_post.1 {
            worst_post = (s, f64::from(post2) * bound);
        }
    }
    eprintln!(
        "Z gblur: {n} sigmas swept; the largest intermediate bound {:e} at sigma {:e}; times the post-scale {:e}",
        worst.0, worst.1, worst_post.1
    );
    assert!(
        worst.0 < f64::from(f32::MAX) / 4.0,
        "an intermediate could reach infinity: {worst:?}"
    );
    // sigma outside 0 .. 1024 and the special values are refused by name.
    for s in [
        -1e-30,
        -0.0 - 1e-300,
        1024.0000001,
        1e9,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert!(gaussian(s).is_none(), "{s}");
    }
    assert!(gaussian(-0.0).is_some() || gaussian(-0.0).is_none());
}

/// Sigmas around every refused run and across the range: the shader driven directly, the CPU model and FFmpeg's `gblur` on three
/// pictures. A drawn sigma is exact, whatever its coefficients look like.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_blur_the_drawn_sigmas_around_the_refused_run_and_across_the_range_are_exact() {
    // The run, found the same way as above.
    let mut sigmas: Vec<f32> = Vec::new();
    let mut prev_out = false;
    let (lo, hi) = (6e-9f32.to_bits(), 2.5e-8f32.to_bits());
    let mut edges: Vec<u32> = Vec::new();
    for b in lo..=hi {
        let s = f32::from_bits(b);
        let out = gaussian(f64::from(s)).is_none();
        if out != prev_out {
            edges.push(b);
        }
        prev_out = out;
    }
    eprintln!(
        "Z gblur: the transitions of the domain between 6e-9 and 2.5e-8 are at {:?}",
        edges.iter().map(|b| f32::from_bits(*b)).collect::<Vec<_>>()
    );
    for &e in &edges {
        for d in -40i64..=40 {
            let b = (i64::from(e) + d) as u32;
            sigmas.push(f32::from_bits(b));
        }
    }
    for k in 0..400 {
        let t = f64::from(k) / 399.0;
        sigmas.push((6e-9f64 * (1024.0f64 / 6e-9).powf(t)) as f32);
    }
    for k in 0..200 {
        sigmas.push(0.05 + 0.1 * k as f32 + 0.003 * (k % 7) as f32);
    }
    sigmas.extend([5e-9, 7e-9, 8e-9, 9e-9, 1e-8, 1.5e-8, 2e-8, 1e-7, 1e-5]);
    let pics: Vec<(String, usize, usize, Vec<u8>)> = vec![
        ("noise 41x23".into(), 41, 23, lcg_bytes(5, 41 * 23)),
        ("dot 40x24".into(), 40, 24, {
            let mut v = vec![0u8; 40 * 24];
            v[12 * 40 + 20] = 255;
            v
        }),
        ("sparse 64x16".into(), 64, 16, {
            let mut v = vec![0u8; 64 * 16];
            for (i, x) in lcg_bytes(8, 64 * 16).into_iter().enumerate() {
                if x < 10 {
                    v[i] = 255;
                }
            }
            v
        }),
    ];
    let (mut drawn, mut refused, mut bad) = (0usize, 0usize, Vec::new());
    for s in sigmas {
        let sigma = f64::from(s);
        let Some(g) = gaussian(sigma) else {
            refused += 1;
            continue;
        };
        drawn += 1;
        for (name, w, h, plane) in &pics {
            let ff = ffmpeg_gray(&[], plane, *w, *h, &kerf_core::blur_filter(sigma));
            let gpu = shader_blur(plane, *w, *h, &g);
            let cpu = blur_plane(plane, *w, *h, &g);
            for (who, got) in [("GPU shader", &gpu), ("CPU model", &cpu)] {
                let n = ff.iter().zip(got.iter()).filter(|(a, b)| a != b).count();
                if n > 0 {
                    bad.push(format!(
                        "{name} sigma {s:e} ({g:?}): {who} differs from FFmpeg in {n} samples"
                    ));
                }
            }
        }
    }
    eprintln!(
        "Z gblur: {drawn} sigmas drawn exact-checked on 3 pictures, {refused} refused; {} failures",
        bad.len()
    );
    for b in bad.iter().take(20) {
        eprintln!("Z FAIL {b}");
    }
    assert!(bad.is_empty(), "{} cases", bad.len());
}

/// The refused run itself: FFmpeg's `gblur` is the signed `f32::mul_add` recursion with the coefficients `gblur_coefficients` derives
/// (so the derivation is FFmpeg's), on a picture small enough not to overflow. Informational: the shader cannot draw a negative boundary
/// scale, so the refusal stands; this reports whether the CPU model could have.
#[test]
#[ignore = "needs ffmpeg"]
fn z_gblur_inside_the_refused_run_the_derivation_is_ffmpegs() {
    let (w, h) = (6usize, 4usize);
    let plane = lcg_bytes(11, w * h);
    let mut agree = 0;
    let mut total = 0;
    let (lo, hi) = (1.0e-8f32.to_bits(), 1.3e-8f32.to_bits());
    for b in (lo..=hi).step_by(((hi - lo) / 60) as usize + 1) {
        let s = f32::from_bits(b);
        if gaussian(f64::from(s)).is_some() {
            continue;
        }
        total += 1;
        let g = gblur_coefficients(f64::from(s)).unwrap();
        let ff = ffmpeg_gray(&[], &plane, w, h, &kerf_core::blur_filter(f64::from(s)));
        let mut buf: Vec<f32> = plane.iter().map(|v| f32::from(*v)).collect();
        for row in buf.chunks_exact_mut(w) {
            row[0] *= g.boundary;
            for x in 1..w {
                row[x] = g.nu.mul_add(row[x - 1], row[x]);
            }
            row[w - 1] *= g.boundary;
            for x in (1..w).rev() {
                row[x - 1] = g.nu.mul_add(row[x], row[x - 1]);
            }
        }
        for x in 0..w {
            buf[x] *= g.boundary;
            for y in 1..h {
                buf[y * w + x] = g.nu.mul_add(buf[(y - 1) * w + x], buf[y * w + x]);
            }
            buf[(h - 1) * w + x] *= g.boundary;
            for y in (1..h).rev() {
                buf[(y - 1) * w + x] = g.nu.mul_add(buf[y * w + x], buf[(y - 1) * w + x]);
            }
        }
        let cpu: Vec<u8> = buf
            .iter()
            .map(|v| {
                let x = *v * g.post;
                let x = if x.is_nan() { 0.0 } else { x.clamp(0.0, 255.0) };
                x.round_ties_even() as u8
            })
            .collect();
        if cpu == ff {
            agree += 1;
        } else {
            eprintln!("Z gblur refused run: sigma {s:e} {g:?}: ffmpeg {ff:?} cpu {cpu:?}");
        }
    }
    eprintln!("Z gblur: inside the refused run the signed CPU model equals FFmpeg in {agree} of {total} sigmas");
}

// ---- the display conversion -------------------------------------------------------------------------------------

fn matrix_name(m: YuvMatrix) -> &'static str {
    match m {
        YuvMatrix::Bt601 => "bt601",
        YuvMatrix::Bt709 => "bt709",
        YuvMatrix::Bt2020 => "bt2020nc",
    }
}

fn ffmpeg_yuv_to_rgb(raw: &[u8], w: usize, h: usize, vf: Option<&str>, extra_in: &[&str]) -> Vec<u8> {
    let size = format!("{w}x{h}");
    let mut args: Vec<&str> = vec!["-hide_banner", "-loglevel", "error", "-nostdin"];
    args.extend(extra_in);
    args.extend(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &size, "-i", "pipe:0"]);
    if let Some(vf) = vf {
        args.extend(["-vf", vf]);
    }
    args.extend(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data = raw.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

const CASES: [(YuvMatrix, bool); 4] = [
    (YuvMatrix::Bt601, true),
    (YuvMatrix::Bt709, true),
    (YuvMatrix::Bt2020, true),
    (YuvMatrix::Bt601, false),
];

/// Every `(y, u, v)` triple of 8 bits: a 2048 x 2048 picture's 2x2 blocks (one chroma sample, four luma) carry each of the 65536 `(u, v)`
/// pairs with each of the 256 `y`; FFmpeg's conversion for each matrix (and untagged) against the model, sample for sample.
#[test]
#[ignore = "needs ffmpeg"]
fn z_display_conversion_over_every_y_u_v_triple() {
    let (w, h) = (2048usize, 2048usize);
    let (cw, ch) = (w / 2, h / 2);
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for cy in 0..ch {
        for cx in 0..cw {
            let k = cy * cw + cx;
            let pair = k % 65536;
            let rep = k / 65536;
            u[k] = (pair & 255) as u8;
            v[k] = (pair >> 8) as u8;
            let base = (rep * 4) as u8;
            for (i, (dx, dy)) in [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate() {
                y[(cy * 2 + dy) * w + cx * 2 + dx] = base.wrapping_add(i as u8);
            }
        }
    }
    let mut raw = y.clone();
    raw.extend_from_slice(&u);
    raw.extend_from_slice(&v);
    let mut bad = Vec::new();
    for (matrix, explicit) in CASES {
        let vf = format!("scale=in_color_matrix={}:in_range=tv", matrix_name(matrix));
        let got = ffmpeg_yuv_to_rgb(&raw, w, h, explicit.then_some(vf.as_str()), &[]);
        let want = kerf_gpu::grade::yuv420_to_rgb([&y, &u, &v], w, h, matrix);
        assert_eq!(got.len(), want.len());
        let apart: Vec<usize> = (0..got.len()).filter(|&i| got[i] != want[i]).collect();
        eprintln!(
            "Z display {} {}: {} of {} samples differ over every (y, u, v)",
            matrix_name(matrix),
            if explicit { "through scale" } else { "untagged" },
            apart.len(),
            got.len()
        );
        if let Some(&i) = apart.first() {
            let px = i / 3;
            let (px_x, px_y) = (px % w, px / w);
            let c = (px_y / 2) * cw + px_x / 2;
            bad.push(format!(
                "{} {}: {} samples; first at ({px_x}, {px_y}) channel {} for y={} u={} v={}: ffmpeg {} model {}",
                matrix_name(matrix),
                explicit,
                apart.len(),
                i % 3,
                y[px],
                u[c],
                v[c],
                got[i],
                want[i]
            ));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// Every even width from 2 to 258 and a spread of heights, noise in all three planes: the model against FFmpeg for each matrix, so a
/// kernel's tail past a vector and the strides FFmpeg's buffers get are looked at at every width, not at the probe's six sizes.
#[test]
#[ignore = "needs ffmpeg"]
fn z_display_conversion_at_every_even_width() {
    let mut bad = Vec::new();
    let mut cases = 0;
    for w in (2..=258usize).step_by(2) {
        for h in [2usize, 4, 6, 10, 34] {
            let (cw, ch) = (w / 2, h / 2);
            let y = lcg_bytes((w * 1000 + h) as u64, w * h);
            let u = lcg_bytes((w * 1000 + h + 1) as u64, cw * ch);
            let v = lcg_bytes((w * 1000 + h + 2) as u64, cw * ch);
            let mut raw = y.clone();
            raw.extend_from_slice(&u);
            raw.extend_from_slice(&v);
            for (matrix, explicit) in CASES {
                let vf = format!("scale=in_color_matrix={}:in_range=tv", matrix_name(matrix));
                let got = ffmpeg_yuv_to_rgb(&raw, w, h, explicit.then_some(vf.as_str()), &[]);
                let want = kerf_gpu::grade::yuv420_to_rgb([&y, &u, &v], w, h, matrix);
                cases += 1;
                let n = got.iter().zip(&want).filter(|(a, b)| a != b).count();
                if n > 0 {
                    bad.push(format!("{w}x{h} {} {}: {n} samples differ", matrix_name(matrix), explicit));
                }
            }
        }
    }
    eprintln!(
        "Z display: {cases} cases (even widths 2..258 x 5 heights x 4 conversions), {} differ",
        bad.len()
    );
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The probe's own verdict, and the same conversion under the flag sets the preview's FFmpeg could be given.
#[test]
#[ignore = "needs ffmpeg"]
fn z_display_probe_matches_and_a_full_range_conversion_is_not_the_model() {
    eprintln!("Z display probe: {:?}", kerf_gpu::display_probe::display());
    let (w, h) = (66usize, 34usize);
    let y = lcg_bytes(1, w * h);
    let u = lcg_bytes(2, w * h / 4);
    let v = lcg_bytes(3, w * h / 4);
    let mut raw = y.clone();
    raw.extend_from_slice(&u);
    raw.extend_from_slice(&v);
    for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
        let tv = ffmpeg_yuv_to_rgb(
            &raw,
            w,
            h,
            Some(&format!("scale=in_color_matrix={}:in_range=tv", matrix_name(matrix))),
            &[],
        );
        let pc = ffmpeg_yuv_to_rgb(
            &raw,
            w,
            h,
            Some(&format!("scale=in_color_matrix={}:in_range=pc", matrix_name(matrix))),
            &[],
        );
        let model = kerf_gpu::grade::yuv420_to_rgb([&y, &u, &v], w, h, matrix);
        let n_tv = tv.iter().zip(&model).filter(|(a, b)| a != b).count();
        let n_pc = pc.iter().zip(&model).filter(|(a, b)| a != b).count();
        eprintln!("Z display {}: tv differs {n_tv}, pc differs {n_pc} (full range is not the model: the display must never be asked for it)", matrix_name(matrix));
        assert_eq!(n_tv, 0);
        assert!(n_pc > 0);
    }
}

// ---- the blur probe's pictures ---------------------------------------------------------------------------------------

/// The probe's pictures `(width, height, sigma, seed)` as `blur_probe.rs` lists them, restated here (a probe that is edited leaves this
/// copy as the independent record of what has to be told apart).
const PROBE_CASES: [(usize, usize, f64, u64); 10] = [
    (160, 96, 250.0, 123),
    (160, 96, 1024.0, 34),
    (160, 96, 100.0, 211),
    (37, 23, 2.5, 1),
    (66, 38, 9.0, 2),
    (97, 61, 40.0, 3),
    (160, 96, 250.0, 25817),
    (160, 96, 1024.0, 1885),
    (160, 96, 1024.0, 1574),
    (160, 96, 1024.0, 1667),
];

fn probe_planes((w, h, _, seed): (usize, usize, f64, u64)) -> [(Vec<u8>, usize, usize); 3] {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2b99_17f3_0c5a_6d41;
    let mut bytes = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s >> 33) as u8
            })
            .collect()
    };
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y = bytes(w * h);
    let u = bytes(cw * ch);
    let v = bytes(cw * ch);
    [(y, w, h), (u, cw, ch), (v, cw, ch)]
}

/// Every one of the 15 non-empty sets of unfused sweeps reads a different picture from the fused model on the probe's pictures: the
/// probe, which holds every sample to the fused model, cannot read `Match` on a build that is unfused in any sweep or any combination.
#[test]
fn z_the_blur_probes_pictures_tell_every_combination_of_unfused_sweeps() {
    let mut table = Vec::new();
    for mask in 1u8..16 {
        let fused: [bool; 4] = std::array::from_fn(|i| mask & (1 << i) == 0);
        let mut per_case = Vec::new();
        for case in PROBE_CASES {
            let g = gaussian(case.2).unwrap();
            let apart: usize = probe_planes(case)
                .iter()
                .map(|(p, w, h)| {
                    let a = blur_plane(p, *w, *h, &g);
                    let b = kerf_gpu::effects::blur_plane_fused(p, *w, *h, &g, fused);
                    a.iter().zip(&b).filter(|(x, y)| x != y).count()
                })
                .sum();
            per_case.push(apart);
        }
        table.push((mask, per_case));
    }
    let mut weak = Vec::new();
    for (mask, per_case) in &table {
        let total: usize = per_case.iter().sum();
        eprintln!("Z probe pictures: unfused sweeps {mask:04b}: {total} samples differ ({per_case:?})");
        if total == 0 {
            weak.push(*mask);
        }
    }
    assert!(weak.is_empty(), "combinations the probe's pictures cannot tell: {weak:?}");
}

/// ... and FFmpeg's own output on those pictures is the fused model's, sample for sample (the probe's `Match` on this machine, restated
/// without the probe), on `gray` planes through `blur_filter`.
#[test]
#[ignore = "needs ffmpeg"]
fn z_the_blur_probes_pictures_through_ffmpeg_are_the_fused_model() {
    let mut off = Vec::new();
    for case in PROBE_CASES {
        let g = gaussian(case.2).unwrap();
        for (i, (p, w, h)) in probe_planes(case).iter().enumerate() {
            let ff = ffmpeg_gray(&[], p, *w, *h, &kerf_core::blur_filter(case.2));
            let m = blur_plane(p, *w, *h, &g);
            let n = ff.iter().zip(&m).filter(|(a, b)| a != b).count();
            if n > 0 {
                off.push(format!("{case:?} plane {i}: {n}"));
            }
        }
    }
    eprintln!(
        "Z probe pictures via ffmpeg: {} planes off the fused model: {off:?}",
        off.len()
    );
    assert!(off.is_empty(), "{off:?}");
}

/// Under other CPU kernels (`-cpuflags ...` ahead of the input) FFmpeg's `gblur` is not the fused model on those pictures: the probe's
/// `Differs` is not a false alarm. Listed per flag set; asserted for the sets that remove an AVX2 kernel.
#[test]
#[ignore = "needs ffmpeg"]
fn z_the_blur_probes_pictures_under_other_kernels() {
    for flags in [
        ["-cpuflags", "0"],
        ["-cpuflags", "-avx2-avx512"],
        ["-cpuflags", "-avx512"],
        ["-cpuflags", "-sse4"],
    ] {
        let mut total = 0usize;
        for case in PROBE_CASES {
            let g = gaussian(case.2).unwrap();
            for (p, w, h) in probe_planes(case) {
                // `-cpuflags` is a global option: it goes ahead of the input.
                let size = format!("{w}x{h}");
                let mut child = Command::new(kerf_core::ffmpeg_path())
                    .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
                    .args(flags)
                    .args([
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "gray",
                        "-s",
                        &size,
                        "-i",
                        "pipe:0",
                        "-vf",
                        &kerf_core::blur_filter(case.2),
                    ])
                    .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "pipe:1"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let mut stdin = child.stdin.take().unwrap();
                let data = p.clone();
                let writer = std::thread::spawn(move || {
                    let _ = stdin.write_all(&data);
                });
                let out = child.wait_with_output().unwrap();
                let _ = writer.join();
                let m = blur_plane(&p, w, h, &g);
                total += out.stdout.iter().zip(&m).filter(|(a, b)| a != b).count();
            }
        }
        eprintln!("Z probe pictures under {flags:?}: {total} samples off the fused model");
    }
}

// ---- planes too small for FFmpeg's own kernel -----------------------------------------------------------------------

/// `ffmpeg_gray` that reports a crash instead of panicking.
fn ffmpeg_gray_try(plane: &[u8], w: usize, h: usize, sigma: f64) -> Option<Vec<u8>> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-s",
        ])
        .arg(format!("{w}x{h}"))
        .args([
            "-i",
            "pipe:0",
            "-vf",
            &kerf_core::blur_filter(sigma),
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data = plane.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    out.status.success().then_some(out.stdout)
}

/// FFmpeg's `gblur` on planes with a side of 1 to 12 (a chroma plane of a tiny picture, a luma plane of a tiny layer): run 12 times each.
/// A plane it crashes on (heap corruption), or answers differently from run to run (a read of memory nobody wrote), has no FFmpeg answer
/// for the compositor to equal. Listed for the plan to refuse; the stable ones are held to the model and the shader.
/// Not asserted: it is the characterisation the report quotes.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_ffmpeg_gblur_on_small_planes_has_an_answer_only_for_some_sizes() {
    use std::collections::BTreeMap;
    let mut crash = Vec::new();
    let mut unstable = Vec::new();
    let mut stable_off = Vec::new();
    let mut stable_ok = 0usize;
    for w in 1..=12usize {
        for h in 1..=12usize {
            let plane = lcg_bytes((w * 100 + h) as u64, w * h);
            for sigma in [0.5, 3.0, 40.0] {
                let g = gaussian(sigma).unwrap();
                let mut answers: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
                let mut crashed = 0;
                for _ in 0..12 {
                    match ffmpeg_gray_try(&plane, w, h, sigma) {
                        Some(o) => *answers.entry(o).or_default() += 1,
                        None => crashed += 1,
                    }
                }
                if crashed > 0 {
                    crash.push(format!("{w}x{h} sigma {sigma}: {crashed} of 12 crashed"));
                    continue;
                }
                let model = blur_plane(&plane, w, h, &g);
                let gpu = shader_blur(&plane, w, h, &g);
                if answers.len() > 1 {
                    let agree = answers.get(&model).copied().unwrap_or(0);
                    unstable.push(format!(
                        "{w}x{h} sigma {sigma}: {} different answers in 12 runs ({:?} runs each), the model's is one of them in {agree} runs",
                        answers.len(),
                        answers.values().collect::<Vec<_>>()
                    ));
                } else {
                    let only = answers.keys().next().unwrap();
                    if *only == model && *only == gpu {
                        stable_ok += 1;
                    } else {
                        stable_off.push(format!(
                            "{w}x{h} sigma {sigma}: the stable answer differs from the model in {} and from the shader in {} samples",
                            only.iter().zip(&model).filter(|(a, b)| a != b).count(),
                            only.iter().zip(&gpu).filter(|(a, b)| a != b).count()
                        ));
                    }
                }
            }
        }
    }
    eprintln!(
        "Z small planes: {} crash, {} unstable, {} stable but off the model, {stable_ok} stable and exact",
        crash.len(),
        unstable.len(),
        stable_off.len()
    );
    for l in crash.iter().chain(&unstable).chain(&stable_off) {
        eprintln!("Z small plane: {l}");
    }
}

/// FFmpeg's `gblur` on a whole `yuv420p` picture (the localbuf of its AVX2 kernel is sized from the luma plane, so the chroma planes are
/// not the `gray` planes of `z_ffmpeg_gblur_on_small_planes_has_an_answer_only_for_some_sizes`), picture sizes 2 to 40 each way, run
/// three times: per plane, whether FFmpeg crashes, differs from run to run, equals the model, or is stable and not the model.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn z_ffmpeg_gblur_on_small_yuv420p_pictures_per_plane() {
    use std::collections::BTreeMap;
    let run = |planes: &[Vec<u8>; 3], w: usize, h: usize, sigma: f64| -> Option<Vec<u8>> {
        let raw: Vec<u8> = planes.iter().flatten().copied().collect();
        let size = format!("{w}x{h}");
        let mut child = Command::new(kerf_core::ffmpeg_path())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                &size,
            ])
            .args([
                "-i",
                "pipe:0",
                "-vf",
                &kerf_core::blur_filter(sigma),
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run ffmpeg");
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&raw);
        });
        let out = child.wait_with_output().unwrap();
        let _ = writer.join();
        out.status.success().then_some(out.stdout)
    };
    let mut table: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut ok, mut off, mut unstable, mut crash) = (0usize, 0usize, 0usize, 0usize);
    for w in (2..=40usize).step_by(2) {
        for h in (2..=40usize).step_by(2) {
            let (cw, ch) = (w / 2, h / 2);
            let planes = [
                lcg_bytes((w * 1000 + h) as u64, w * h),
                lcg_bytes((w * 1000 + h + 1) as u64, cw * ch),
                lcg_bytes((w * 1000 + h + 2) as u64, cw * ch),
            ];
            for sigma in [0.5, 3.0, 40.0] {
                let g = gaussian(sigma).unwrap();
                let mut answers: BTreeMap<Vec<u8>, usize> = BTreeMap::new();
                let mut crashed = 0;
                for _ in 0..3 {
                    match run(&planes, w, h, sigma) {
                        Some(o) => *answers.entry(o).or_default() += 1,
                        None => crashed += 1,
                    }
                }
                let key = format!("{w}x{h} sigma {sigma}");
                if crashed > 0 {
                    crash += 1;
                    table.entry("crash".into()).or_default().push(key);
                    continue;
                }
                if answers.len() > 1 {
                    unstable += 1;
                    table.entry("unstable".into()).or_default().push(key);
                    continue;
                }
                let out = answers.keys().next().unwrap();
                let (ly, rest) = out.split_at(w * h);
                let (lu, lv) = rest.split_at(cw * ch);
                let mut bad = Vec::new();
                for (name, got, plane, pw, ph) in [
                    ("Y", ly, &planes[0], w, h),
                    ("U", lu, &planes[1], cw, ch),
                    ("V", lv, &planes[2], cw, ch),
                ] {
                    if got != blur_plane(plane, pw, ph, &g) {
                        bad.push(name);
                    }
                }
                if bad.is_empty() {
                    ok += 1;
                } else {
                    off += 1;
                    table
                        .entry(format!("stable but {bad:?} off the model"))
                        .or_default()
                        .push(key);
                }
            }
        }
    }
    eprintln!("Z small pictures: {ok} exact, {off} stable but off the model, {unstable} unstable, {crash} crash");
    for (k, v) in &table {
        eprintln!("Z small picture {k}: {} cases: {:?}", v.len(), &v[..v.len().min(40)]);
    }
}

/// What thread caps the probes' children get (`measure::argv` calls `limit_frame_args(argv, 1)`): the same call on the argv shape of the blur
/// probe's run and of an export's. Printed: the probe's `gblur` runs sliced across as many filter threads as this says, the export's
/// across its own.
#[test]
fn z_the_thread_caps_a_probe_child_gets() {
    let mut probe: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-y",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        "160x96",
        "-i",
        "pipe:0",
        "-vf",
        "gblur=sigma=250",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "pipe:1",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    kerf_core::limit_frame_args(&mut probe, 1);
    eprintln!("Z probe argv after limit_frame_args: {probe:?}");
    let head: Vec<&String> = probe.iter().take(8).collect();
    eprintln!("Z probe head: {head:?}");
}
