//! Whether this FFmpeg's `gblur` runs as the compositor's blur does — measured once per process on the filter text the engine
//! writes ([`kerf_core::blur_filter`]), never assumed and never keyed on a version string. A blur is in `Compositor::caps` only on a
//! [`Blur::Match`].
//!
//! `gblur` is a recursion in `float`, and its x86 kernels fix the last bit of every step: with AVX2 or AVX-512 each step is a fused
//! multiply-add (`x86/vf_gblur.asm`), and the model ([`crate::effects::blur_plane`], the shader `blur.wgsl`) is that. A build
//! whose CPU has only SSE4 associates the sums another way (four samples at a time through powers of `nu`), and one with no kernel
//! at all does not fuse; they are a level from the model on the few samples where the value sits on a half, which is not
//! drawn. The probe runs the filter over noise in all three planes of a few pictures (an odd one, so the kernels' tails are looked
//! at, and a wide range of sigmas, so `nu` and the scales vary) and holds every sample of every plane to the model, with no
//! tolerance. A probe that could not tell is [`Blur::Unknown`]: asked again after a backoff, and nothing blurred is drawn
//! until it answers.

use std::time::{Duration, Instant};

use crate::effects::{blur_plane, gaussian};
use crate::measure::{self, Budget, Probe, Timing, Verdict};

/// What the probe found: [`Verdict::Match`] (the filter is the model's, every sample of every case), [`Verdict::Differs`] (it
/// is not: the detail names the case) or [`Verdict::Unknown`].
pub type Blur = Verdict;

const TIMING: Timing = Timing {
    backoff: Duration::from_secs(30),
    overall: Duration::from_secs(30),
    per_run: Duration::from_secs(20),
};

static STATE: Probe = Probe::new();

/// The pictures: `(width, height, sigma)`.
const CASES: [(usize, usize, f64); 6] = [
    (37, 23, 2.5),
    (66, 38, 0.7),
    (66, 38, 9.0),
    (130, 46, 40.0),
    (97, 61, 300.0),
    (16, 16, 1.5),
];

/// What is known without running anything: the measured answer, else [`Blur::Unknown`]. What the compositor's caps are made from.
pub fn measured() -> Blur {
    measure::known(&STATE)
}

/// The answer, measured on the first call (a few short `ffmpeg` children, bounded by 20 s each and 30 s in all, no lock held: a
/// blocking thread's, ahead of the frame that needs it, [`crate::prepare`]). A caller that arrives while it runs waits for it and
/// takes its answer. `Unknown` is not remembered but for a backoff.
pub fn blur() -> Blur {
    measure::cached(&STATE, Instant::now(), TIMING, probe)
}

/// A deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n)
            .map(|_| {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (self.0 >> 33) as u8
            })
            .collect()
    }
}

fn probe(budget: &Budget) -> Blur {
    probe_with(budget, &[])
}

/// [`probe`] with `cpu` ahead of FFmpeg's other arguments (the tests run it with `-cpuflags 0`, a machine with none of the SIMD kernels).
fn probe_with(budget: &Budget, cpu: &[&str]) -> Blur {
    let mut rng = Lcg(0x2b99_17f3_0c5a_6d41);
    for (w, h, sigma) in CASES {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let planes = [rng.bytes(w * h), rng.bytes(cw * ch), rng.bytes(cw * ch)];
        let raw: Vec<u8> = planes.iter().flatten().copied().collect();
        let size = format!("{w}x{h}");
        let filter = kerf_core::blur_filter(sigma);
        let args: Vec<&str> = cpu
            .iter()
            .copied()
            .chain([
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                &size,
                "-i",
                "pipe:0",
                "-vf",
                &filter,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "pipe:1",
            ])
            .collect();
        let Some(out) = budget.ffmpeg(&args, raw) else {
            return Blur::Unknown;
        };
        if out.len() != w * h + 2 * cw * ch {
            return Blur::Unknown;
        }
        let Some(g) = gaussian(sigma) else {
            return Blur::Unknown;
        };
        let (luma, rest) = out.split_at(w * h);
        let (u, v) = rest.split_at(cw * ch);
        let mut apart = 0;
        for (got, plane, (pw, ph)) in [
            (luma, &planes[0], (w, h)),
            (u, &planes[1], (cw, ch)),
            (v, &planes[2], (cw, ch)),
        ] {
            let want = blur_plane(plane, pw, ph, &g);
            apart += got.iter().zip(&want).filter(|(a, b)| a != b).count();
        }
        if apart != 0 {
            return Blur::Differs(format!(
                "{filter} over {w}x{h}: {apart} samples differ from the fused-multiply-add model"
            ));
        }
    }
    Blur::Match
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The memory of the probe: a probe that could not tell is asked again after the backoff and not before, and a match or a
    /// difference is kept for good.
    #[test]
    fn an_unknown_is_asked_again_after_the_backoff_and_a_match_or_a_difference_is_kept() {
        let state = Probe::new();
        let t0 = Instant::now();
        assert_eq!(measure::known(&state), Blur::Unknown, "nothing measured yet");
        assert_eq!(measure::cached(&state, t0, TIMING, |_| Blur::Unknown), Blur::Unknown);
        assert_eq!(
            measure::cached(&state, t0 + Duration::from_secs(29), TIMING, |_| panic!(
                "probed inside the backoff"
            )),
            Blur::Unknown
        );
        let after = Instant::now() + TIMING.backoff + Duration::from_secs(1);
        let differs = Blur::Differs("gblur=sigma=2.5 over 37x23: 3 samples differ".into());
        assert_eq!(measure::cached(&state, after, TIMING, |_| differs.clone()), differs);
        assert_eq!(measure::known(&state), differs);
        assert_eq!(measure::cached(&state, after, TIMING, |_| panic!("measured twice")), differs);
    }

    /// The shader's fused multiply-add (`shaders/fma.wgsl`, integers) is `f32::mul_add` and its multiply is `*`, bit for bit, on
    /// random triples across a wide range of exponents, on the ties a rounding to nearest even has to break (a product of two
    /// 12-bit significands has exactly 24 bits, one of 13 and 12 has 25: half an ulp, which the addend then moves), on a zero
    /// addend, on an addend far above or far below the product, and on operands whose sum cancels to a power of two's edge.
    #[test]
    #[ignore = "needs a GPU adapter (lavapipe is enough)"]
    #[allow(clippy::print_stderr)]
    fn the_shaders_fused_multiply_add_is_f32_mul_add_on_every_case_tried() {
        use crate::gpu::{Gpu, GpuOptions};
        use wgpu::util::DeviceExt as _;
        let gpu = Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)");
        let mut rng = Lcg(0x5eed_f00d_1234_abcd);
        let mut next = move || {
            rng.0 = rng.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng.0 >> 32) as u32
        };
        // A positive normal float with a random significand (or a few bits of one) and an exponent in 2^-e..2^e.
        let float = |bits: u32, e: i32, next: &mut dyn FnMut() -> u32| -> f32 {
            let mant = if bits >= 23 {
                next() & 0x7f_ffff
            } else {
                (next() & ((1 << bits) - 1)) << (23 - bits)
            };
            let exp = 127 + (next() % (2 * e as u32 + 1)) as i32 - e;
            f32::from_bits(((exp as u32) << 23) | mant)
        };
        let mut cases: Vec<[f32; 3]> = Vec::new();
        for _ in 0..200_000 {
            let (a, b) = (float(23, 30, &mut next), float(23, 30, &mut next));
            let c = match next() % 8 {
                0 => 0.0,
                // Near the product: the cancellation-free sum of two comparable terms.
                1..=3 => a * b * (0.25 + (next() % 1000) as f32 / 500.0),
                _ => float(23, 60, &mut next),
            };
            cases.push([a, b, c]);
        }
        // Ties: short significands, so the product is exact in 24 or 25 bits and the addend decides the rounding.
        for _ in 0..200_000 {
            let (a, b) = (float(12, 20, &mut next), float(13, 20, &mut next));
            let c = match next() % 4 {
                0 => 0.0,
                1 => float(3, 20, &mut next),
                2 => a * b,
                _ => float(23, 20, &mut next),
            };
            cases.push([a, b, c]);
        }
        for _ in 0..50_000 {
            // The recursion's own range: nu in (0, 1), a running value in 0..2^20, a pixel 0..255.
            let nu = f32::from_bits(0x3f80_0000 - 1 - (next() % 0x40_0000));
            let prev = (next() % (1 << 20)) as f32 / 64.0 + 1.0;
            let c = (next() % 256) as f32;
            cases.push([nu, prev, c]);
        }
        let mut data = Vec::with_capacity(cases.len() * 4);
        for [a, b, c] in &cases {
            data.extend([a.to_bits(), b.to_bits(), c.to_bits(), 0]);
        }
        let device = &gpu.device;
        let source = concat!(include_str!("shaders/fma.wgsl"), include_str!("shaders/fma_test.wgsl"));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fma test"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("fma test"),
            layout: None,
            module: &module,
            entry_point: Some("cs_test"),
            compilation_options: Default::default(),
            cache: None,
        });
        let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("triples"),
            contents: bytemuck::cast_slice(&data),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let size = (cases.len() * 8) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("results"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fma test"),
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
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fma test") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("fma test"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups((cases.len() as u32).div_ceil(64), 1, 1);
        }
        enc.copy_buffer_to_buffer(&output, 0, &read, 0, size);
        gpu.queue.submit(Some(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(60)),
            })
            .expect("the GPU finishes");
        rx.recv().unwrap().unwrap();
        let mapped = read.slice(..).get_mapped_range().unwrap();
        let got: &[u32] = bytemuck::cast_slice(&mapped);
        let (mut bad_fma, mut bad_mul) = (Vec::new(), 0usize);
        for (i, [a, b, c]) in cases.iter().enumerate() {
            let (fused, plain) = (got[2 * i], got[2 * i + 1]);
            if fused != a.mul_add(*b, *c).to_bits() && bad_fma.len() < 5 {
                bad_fma.push((*a, *b, *c, f32::from_bits(fused), a.mul_add(*b, *c)));
            }
            if plain != (a * b).to_bits() {
                bad_mul += 1;
            }
        }
        let bad_total = cases
            .iter()
            .enumerate()
            .filter(|(i, [a, b, c])| got[2 * i] != a.mul_add(*b, *c).to_bits())
            .count();
        eprintln!(
            "{} triples: {bad_total} fused multiply-adds differ, {bad_mul} multiplies; first {bad_fma:?}",
            cases.len()
        );
        assert_eq!((bad_total, bad_mul), (0, 0), "first {bad_fma:?}");
    }

    /// The probe has teeth: FFmpeg run with none of its SIMD kernels (`-cpuflags 0`: `gblur`'s C loop, which does not fuse) is not the
    /// model, and the probe says `Differs`, not `Match`.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn a_build_with_no_simd_kernels_is_differs() {
        let found = probe_with(&Budget::new(&TIMING), &["-cpuflags", "0"]);
        eprintln!("gblur with -cpuflags 0: {found:?}");
        assert!(matches!(found, Blur::Differs(_)), "{found:?}");
    }

    /// The probe itself, on the FFmpeg in use (`#[ignore]`d: it needs one). It must be able to tell, and says the same twice.
    #[test]
    #[ignore = "needs ffmpeg"]
    #[allow(clippy::print_stderr)]
    fn this_ffmpeg_blurs_as_the_model_does_or_the_probe_says_where_not() {
        let first = probe(&Budget::new(&TIMING));
        assert_ne!(first, Blur::Unknown, "the probe could not run");
        assert_eq!(first, probe(&Budget::new(&TIMING)));
        eprintln!("gblur: {first:?}");
    }
}
