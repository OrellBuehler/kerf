//! The blend chain's arithmetic against FFmpeg, and the compositor's passes against that arithmetic.
//!
//! `kerf_gpu::blend` is a model, in integers, of what `kerf_core::blend_colour_graph` makes of a clip laid on the composite
//! (`alphamerge`, `scale=neighbor` to `gbrap`, `blend`, `scale=neighbor` to `yuva420p`, `overlay`). Held to the real filters
//! here, **every value, to the level**, on whichever FFmpeg `KERF_FFMPEG` names (the distro's and the pinned one: swscale's
//! x86 kernels are not the same code on every build):
//!
//! * the conversion into RGB, for **every** (Y, U, V) (16.7 million), BT.601 and BT.709;
//! * the conversion out of RGB: luma for **every** (R, G, B), chroma over random pairs;
//! * every mode over every pair of 8-bit values;
//! * the whole chain on noise, mode by mode and matrix by matrix (what [`kerf_gpu::blend::kernels`] measures);
//! * and the compositor's two passes (`Compositor::blend_planes`) against the model, over noise with partial alpha.
//!
//! Needs `ffmpeg`; the last case an adapter too. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test blend_kernels -- --ignored
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ... --test blend_kernels -- --ignored
//! ```

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};

use kerf_core::{BlendKinds, BlendMode, YuvMatrix};
use kerf_gpu::blend::{channel, kernels, reference, Planes, Tables};
use kerf_gpu::{Compositor, Gpu, GpuOptions};

const MODES: [BlendMode; 9] = [
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::HardLight,
    BlendMode::Add,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::Difference,
    BlendMode::Exclusion,
];

fn ffmpeg(args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new(kerf_core::ffmpeg_path())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-nostdin"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().expect("ffmpeg output");
    let _ = writer.join();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

fn tag(matrix: YuvMatrix) -> &'static str {
    match matrix {
        YuvMatrix::Bt601 => "bt601",
        YuvMatrix::Bt709 => "bt709",
        YuvMatrix::Bt2020 => "bt2020",
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_conversion_into_rgb_is_swscales_for_every_value() {
    // 4096 x 4096 luma: 2048 x 2048 blocks, one chroma pair each and four luma values, so that all 2^24 triples are there.
    let (w, h) = (4096usize, 4096usize);
    let (cw, ch) = (w / 2, h / 2);
    let (mut y, mut u, mut v) = (vec![0u8; w * h], vec![0u8; cw * ch], vec![0u8; cw * ch]);
    for by in 0..ch {
        for bx in 0..cw {
            let n = by * cw + bx;
            u[n] = (n & 255) as u8;
            v[n] = ((n >> 8) & 255) as u8;
            for k in 0..4 {
                y[(by * 2 + (k >> 1)) * w + bx * 2 + (k & 1)] = (((n >> 16) & 63) * 4 + k) as u8;
            }
        }
    }
    let alpha: Vec<u8> = (0..w * h).map(|i| (i * 7 / 3) as u8).collect();
    // The clip's frame has an alpha plane (`alphamerge`), the composite's has none: `gbrap` out of both (`gbrp` is another
    // path in swscale, with other arithmetic, which the chain never takes).
    for (with_alpha, pix_fmt) in [(true, "yuva420p"), (false, "yuv420p")] {
        let input = if with_alpha {
            [&y[..], &u[..], &v[..], &alpha[..]].concat()
        } else {
            [&y[..], &u[..], &v[..]].concat()
        };
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
            let vf = format!(
                "scale=flags=neighbor:in_color_matrix={}:in_range=tv,format=gbrap",
                tag(matrix)
            );
            let out = ffmpeg(
                &[
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    pix_fmt,
                    "-s",
                    "4096x4096",
                    "-i",
                    "-",
                    "-vf",
                    &vf,
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gbrap",
                    "-",
                ],
                &input,
            );
            assert_eq!(out.len(), w * h * 4);
            let tables = Tables::new(matrix);
            let mut bad = 0usize;
            for py in 0..h {
                for px in 0..w {
                    let i = py * w + px;
                    let c = (py / 2) * cw + px / 2;
                    let [r, g, b] = tables.yuv_to_rgb(y[i], u[c], v[c]);
                    if (out[i], out[w * h + i], out[2 * w * h + i]) != (g, b, r) {
                        bad += 1;
                    }
                }
            }
            assert_eq!(
                bad, 0,
                "{pix_fmt} {matrix:?}: samples of 16.7 million that differ from swscale's yuv2gbrp"
            );
            let want_alpha = if with_alpha { &alpha[..] } else { &vec![255u8; w * h][..] };
            assert_eq!(
                &out[3 * w * h..],
                want_alpha,
                "{pix_fmt} {matrix:?}: the alpha plane passes through (or is opaque)"
            );
        }
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_conversion_out_of_rgb_is_swscales_for_every_value() {
    let (w, h) = (4096usize, 4096usize);
    let (cw, ch) = (w / 2, h / 2);
    let mut rng = Xorshift(0x9e37_79b9_7f4a_7c15);
    // Every RGB once for the luma; the chroma is held on the same pixels, which are noise in pairs.
    let (mut r, mut g, mut b) = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
    for i in 0..w * h {
        r[i] = (i & 255) as u8;
        g[i] = ((i >> 8) & 255) as u8;
        b[i] = ((i >> 16) & 255) as u8;
    }
    let mut noisy = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
    for i in 0..w * h {
        let n = rng.next();
        noisy.0[i] = n as u8;
        noisy.1[i] = (n >> 8) as u8;
        noisy.2[i] = (n >> 16) as u8;
    }
    for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
        let tables = Tables::new(matrix);
        let vf = format!(
            "scale=flags=neighbor:out_color_matrix={}:out_range=tv,format=yuv420p",
            tag(matrix)
        );
        for (case, (r, g, b)) in [("every colour", (&r, &g, &b)), ("noise", (&noisy.0, &noisy.1, &noisy.2))] {
            let input = [&g[..], &b[..], &r[..]].concat();
            let out = ffmpeg(
                &[
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "gbrp",
                    "-s",
                    "4096x4096",
                    "-i",
                    "-",
                    "-vf",
                    &vf,
                    "-frames:v",
                    "1",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-",
                ],
                &input,
            );
            assert_eq!(out.len(), w * h * 3 / 2);
            let (oy, rest) = out.split_at(w * h);
            let (ou, ov) = rest.split_at(cw * ch);
            let mut bad = (0usize, 0usize);
            for i in 0..w * h {
                if tables.luma([r[i], g[i], b[i]]) != oy[i] {
                    bad.0 += 1;
                }
            }
            for by in 0..ch {
                for bx in 0..cw {
                    let i = (by * 2 + 1) * w + bx * 2;
                    let pair = [[r[i], g[i], b[i]], [r[i + 1], g[i + 1], b[i + 1]]];
                    if tables.chroma(pair) != [ou[by * cw + bx], ov[by * cw + bx]] {
                        bad.1 += 1;
                    }
                }
            }
            assert_eq!(
                bad,
                (0, 0),
                "{matrix:?} {case}: luma and chroma samples that differ from swscale's rgb to yuv"
            );
        }
    }
}

#[test]
#[ignore = "needs ffmpeg"]
fn every_mode_is_vf_blends_over_every_pair_of_values() {
    let (w, h) = (256usize, 256usize);
    let top: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
    let bottom: Vec<u8> = (0..w * h).map(|i| (i / 256) as u8).collect();
    let dir = std::env::temp_dir().join(format!("kerf-blend-kernels-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (top_path, bottom_path) = (dir.join("top.raw"), dir.join("bottom.raw"));
    // The three colour planes carry the pair, and the alpha plane is the clip's.
    std::fs::write(&top_path, [&top[..], &bottom[..], &top[..], &bottom[..]].concat()).unwrap();
    std::fs::write(&bottom_path, [&bottom[..], &top[..], &bottom[..], &top[..]].concat()).unwrap();
    for mode in MODES {
        let name = mode.ffmpeg().expect("a blend mode");
        let vf = format!("[0:v][1:v]blend=c0_mode={name}:c1_mode={name}:c2_mode={name}");
        let out = ffmpeg(
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gbrap",
                "-s",
                "256x256",
                "-i",
                top_path.to_str().unwrap(),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gbrap",
                "-s",
                "256x256",
                "-i",
                bottom_path.to_str().unwrap(),
                "-filter_complex",
                &vf,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gbrap",
                "-",
            ],
            &[],
        );
        assert_eq!(out.len(), w * h * 4);
        for plane in 0..3 {
            let (a, b) = if plane % 2 == 0 { (&top, &bottom) } else { (&bottom, &top) };
            let bad = (0..w * h)
                .filter(|&i| out[plane * w * h + i] != channel(mode, a[i], b[i]))
                .count();
            assert_eq!(
                bad, 0,
                "{mode:?} plane {plane}: pairs of 65536 that differ from `blend={name}`"
            );
        }
        assert_eq!(&out[3 * w * h..], &bottom[..], "{mode:?}: the alpha plane is the top input's");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The major version of the ffmpeg this run drives (`ffmpeg version n6.1.1` / `4.4.2-0ubuntu...` / `N-12345-g...`).
fn ffmpeg_major() -> Option<u32> {
    let out = Command::new(kerf_core::ffmpeg_path()).arg("-version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = text.lines().next()?.split_whitespace().nth(2)?.trim_start_matches(['n', 'N']);
    version.split(['.', '-']).next()?.parse().ok()
}

#[test]
#[ignore = "needs ffmpeg"]
fn the_whole_chain_is_the_model_on_noise_in_every_mode_and_matrix() {
    // `blend::kernels` runs the export's own text on noise and compares it with `reference`: every mode, both matrices, and the
    // clip placed by the export's `overlay`. FFmpeg 4.4's `overlay` slices a clip above the frame's top edge wrongly on several
    // threads (its alpha plane loses rows), so there the answer is that nothing matches, which is the refusal that build needs.
    let want = if ffmpeg_major().is_some_and(|major| major < 5) {
        [BlendKinds::NONE; 2]
    } else {
        [BlendKinds::ALL; 2]
    };
    assert_eq!(
        kernels(),
        want,
        "the modes whose chain came back as the model computes it, [BT.601, BT.709] (ffmpeg {})",
        kerf_core::ffmpeg_path()
    );
}

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Gpu::new(GpuOptions::for_tests()).expect("a GPU adapter (install mesa-vulkan-drivers for lavapipe)"))
        .clone()
}

fn noise(w: usize, h: usize, rng: &mut Xorshift) -> Planes {
    let mut bytes = |n: usize| (0..n).map(|_| rng.next() as u8).collect::<Vec<u8>>();
    Planes {
        w,
        h,
        y: bytes(w * h),
        u: bytes(w * h / 4),
        v: bytes(w * h / 4),
    }
}

#[test]
#[ignore = "needs an adapter"]
fn the_compositors_passes_are_the_model_on_noise() {
    let compositor = Compositor::new(gpu()).expect("a compositor");
    let mut rng = Xorshift(0x2545_f491_4f6c_dd1d);
    for (w, h) in [(2usize, 2usize), (48, 32), (50, 34), (128, 64)] {
        let base = noise(w, h, &mut rng);
        let laid = noise(w, h, &mut rng);
        // Noise with runs of the two alphas `overlay` short-cuts.
        let alpha: Vec<u8> = (0..w * h)
            .map(|i| match i % 5 {
                0 => 0,
                1 => 255,
                _ => rng.next() as u8,
            })
            .collect();
        for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709] {
            for mode in MODES {
                let want = reference(mode, matrix, &base, &laid, &alpha);
                let got = compositor
                    .blend_planes(mode, matrix, &base, &laid, &alpha)
                    .expect("the passes");
                for (plane, (g, wnt)) in [(&got.y, &want.y), (&got.u, &want.u), (&got.v, &want.v)]
                    .into_iter()
                    .enumerate()
                {
                    let bad = g.iter().zip(wnt.iter()).filter(|(a, b)| a != b).count();
                    assert_eq!(
                        bad, 0,
                        "{mode:?} {matrix:?} {w}x{h} plane {plane}: samples that differ from the model"
                    );
                }
            }
        }
    }
}
