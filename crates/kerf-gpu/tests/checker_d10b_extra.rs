//! More of the independent checker's cases for 10-bit deliveries (`checker_d10b_*`): the user's CPU share (`set_cpu_percent`) against the compositor that is
//! told nothing about the graph's threads, big frames (720p, 1080p) at the thread counts where their slices flip, a picture rotated by its metadata, and
//! three tracks. Needs `ffmpeg` and an adapter (lavapipe is enough); `#[ignore]`d.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_d10b_extra -- --ignored --nocapture
//! taskset -c 0-1 cargo test ...   # a two-core machine: FFmpeg counts two CPUs, the budget is two threads of two
//! ```

#![allow(clippy::print_stderr, clippy::single_range_in_vec_init)]

mod common;
#[path = "common/d10b.rs"]
mod d10b;

use d10b::{clip, media, options, run, timeline, Run, Stats, LAYOUTS};
use kerf_core::{Asset, Fit, TenBitLayout};

fn finish(total: &Stats) {
    eprintln!(
        "TOTAL drawn {} refused {} compared {} differing {}",
        total.drawn, total.refused, total.compared, total.differing
    );
    assert!(
        total.failures.is_empty(),
        "{} differing samples in drawn frames:\n{}",
        total.differing,
        total.failures.join("\n")
    );
}

/// The user's share of the machine (10 to 100 percent) decides the threads `limit_args` writes into the export; the product's own export (`render_with`
/// to lossless 10-bit VP9) against a compositor that is told nothing. Heights at which the slices of two, three and four threads flip.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_the_users_cpu_share_decides_the_threads_the_plan_judges() {
    d10b::measured();
    let src = media("share-noise", "noise", (160, 90), "30", 3.0, "yuv420p", None);
    let assets = vec![src.clone()];
    let mut total = Stats::default();
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    for percent in [10u8, 25, 40, 50, 60, 75, 90, 100] {
        kerf_core::set_cpu_percent(percent);
        for h in [34u32, 50, 54, 66, 90, 100, 130] {
            let w = ((h * 16 / 9) & !1).max(16);
            let mut c = clip(&src, 0.1, 0.25, 0.0);
            c.fade_in = 0.1;
            let tl = timeline(vec![vec![c]], vec![], (w, h), Fit::Cover, 30.0);
            for layout in LAYOUTS {
                let exported = d10b::real_export(&tl, &assets, layout, 30.0, &[]);
                let s = run(&Run {
                    case: &format!("{percent}% of {cores} cores, height {h}"),
                    tl: &tl,
                    assets: &assets,
                    layout,
                    opts: options(layout, 30.0),
                    threads: None,
                    argv: None,
                    extra: Vec::new(),
                    frames: vec![0..8],
                    allow_refused: true,
                    exported: Some(exported),
                });
                if s.differing != 0 {
                    d10b::report(&format!("{percent}% height {h}"), layout, None, &s);
                }
                total.add(s);
            }
        }
        eprintln!(
            "{percent:>3}% of {cores} cores: threads {:?}, so far drawn {} compared {} differing {}",
            kerf_core::cpu_graph_thread_range(),
            total.drawn,
            total.compared,
            total.differing
        );
    }
    kerf_core::set_cpu_percent(kerf_core::DEFAULT_CPU_PERCENT);
    assert!(total.drawn > 100);
    finish(&total);
}

/// 720p and 1080p frames (a 720p picture enlarged into 1080p; a 1080p picture as it is; a 1080p picture shrunk to 720p), at the thread counts the big
/// frames' slices flip at (4 and 8 and 16 for 1080 / 720 rows), both layouts: the compositor told the count against the export told the same.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); minutes"]
fn checker_d10b_big_frames_at_the_thread_counts_where_their_slices_flip() {
    d10b::measured();
    let hd = media("big-noise-720", "noise", (1280, 720), "30", 1.0, "yuv420p", None);
    let fhd = media("big-noise-1080", "noise", (1920, 1080), "30", 1.0, "yuv420p", None);
    let assets: Vec<Asset> = vec![hd.clone(), fhd.clone()];
    let mut total = Stats::default();
    let cuts: Vec<(&str, (u32, u32), kerf_core::Clip)> = vec![
        ("720p into 1080p", (1920, 1080), {
            let mut c = clip(&hd, 0.1, 0.2, 0.0);
            c.fade_in = 0.1;
            c
        }),
        ("1080p into 1080p", (1920, 1080), {
            let mut c = clip(&fhd, 0.1, 0.2, 0.0);
            c.fade_out = 0.1;
            c
        }),
        ("1080p into 720p", (1280, 720), {
            let mut c = clip(&fhd, 0.1, 0.2, 0.0);
            c.fade_in = 0.1;
            c
        }),
    ];
    for (name, canvas, c) in cuts {
        let tl = timeline(vec![vec![c]], vec![], canvas, Fit::Cover, 30.0);
        for layout in LAYOUTS {
            let counts: &[u32] = if layout == TenBitLayout::P420 {
                &[2, 4, 5, 6, 8, 16]
            } else {
                &[3]
            };
            for &n in counts {
                let s = run(&Run {
                    case: &format!("{name} on {n} threads"),
                    tl: &tl,
                    assets: &assets,
                    layout,
                    opts: options(layout, 30.0),
                    threads: Some(n),
                    argv: None,
                    extra: Vec::new(),
                    frames: vec![0..4],
                    allow_refused: true,
                    exported: None,
                });
                d10b::report(name, layout, Some(n), &s);
                total.add(s);
            }
        }
    }
    finish(&total);
}

/// A picture rotated by its metadata (a phone's portrait clip) and three tracks of plain clips: drawn only where the plan can prove it.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_a_rotated_source_and_three_tracks() {
    d10b::measured();
    let a = media("extra-noise", "noise", (160, 90), "30", 3.0, "yuv420p", None);
    let b = media("extra-bars", "bars", (160, 90), "30", 3.0, "yuv420p", None);
    let mut total = Stats::default();
    // A portrait clip by metadata: encoded 160x90 and displayed turned a quarter.
    let dir = common::media_dir("d10b-media");
    let rotated = dir.join("rotated.mp4");
    if !rotated.exists() {
        let plain = dir.join("rotated-base.mp4");
        common::ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x90:rate=30:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            plain.to_str().unwrap(),
        ]);
        common::ffmpeg(&[
            "-display_rotation:v",
            "90",
            "-i",
            plain.to_str().unwrap(),
            "-c",
            "copy",
            rotated.to_str().unwrap(),
        ]);
    }
    let r = kerf_core::Project::probe_asset(&rotated).expect("probe the rotated clip");
    eprintln!(
        "the rotated clip: rotation {:?}",
        r.streams.iter().map(|s| s.rotation).collect::<Vec<_>>()
    );
    for canvas in [(54u32, 96u32), (96, 54)] {
        let tl = timeline(vec![vec![clip(&r, 0.0, 0.4, 0.0)]], vec![], canvas, Fit::Cover, 30.0);
        for layout in LAYOUTS {
            let exported = d10b::real_export(&tl, std::slice::from_ref(&r), layout, 30.0, &[]);
            let s = run(&Run {
                case: &format!("a rotated clip into {canvas:?}"),
                tl: &tl,
                assets: std::slice::from_ref(&r),
                layout,
                opts: options(layout, 30.0),
                threads: None,
                argv: None,
                extra: Vec::new(),
                frames: vec![0..12],
                allow_refused: true,
                exported: Some(exported),
            });
            d10b::report(&format!("a rotated clip into {canvas:?}"), layout, None, &s);
            total.add(s);
        }
    }
    // Three tracks: each covers the one below; the top fades; the middle has a gap.
    let low = clip(&a, 0.0, 0.8, 0.0);
    let mid = clip(&b, 0.2, 0.3, 0.2);
    let mut top = clip(&a, 1.0, 0.3, 0.3);
    top.fade_in = 0.1;
    top.fade_out = 0.1;
    let tl = timeline(vec![vec![low], vec![mid], vec![top]], vec![], (96, 54), Fit::Cover, 30.0);
    let assets = vec![a, b];
    for layout in LAYOUTS {
        let exported = d10b::real_export(&tl, &assets, layout, 30.0, &[]);
        let s = run(&Run {
            case: "three tracks",
            tl: &tl,
            assets: &assets,
            layout,
            opts: options(layout, 30.0),
            threads: None,
            argv: None,
            extra: Vec::new(),
            frames: vec![0..24],
            allow_refused: true,
            exported: Some(exported),
        });
        d10b::report("three tracks", layout, None, &s);
        assert!(s.drawn > 0, "three tracks: nothing drawn ({:?})", s.kinds);
        total.add(s);
    }
    finish(&total);
}

/// The delivery's default fit is Contain: the same pictures as the Cover cases, with the aspect of the frame equal to the picture's (no bars: one `scale`,
/// a `pad` that adds nothing) and off by a hair (bars of two rows), at thread counts that flip and do not.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_contain_with_no_bars_and_with_two_rows_of_them() {
    d10b::measured();
    let a = media("contain-noise", "noise", (160, 90), "30", 3.0, "yuv420p", None);
    let b = media("contain-bars", "bars", (160, 90), "30", 3.0, "yuv420p", None);
    let assets = vec![a.clone(), b.clone()];
    let mut total = Stats::default();
    for (canvas, fit) in [
        ((96u32, 54u32), Fit::Contain),
        ((160, 90), Fit::Contain),
        ((192, 108), Fit::Contain),
        ((96, 56), Fit::Contain),
        ((96, 54), Fit::Blur),
        ((100, 54), Fit::Contain),
    ] {
        let mut c1 = clip(&a, 0.1, 0.3, 0.0);
        c1.fade_in = 0.1;
        let mut c2 = clip(&b, 0.0, 0.2, 0.4);
        c2.fade_out = 0.1;
        let tl = timeline(vec![vec![c1, c2]], vec![], canvas, fit, 30.0);
        for layout in LAYOUTS {
            for threads in [3u32, 4] {
                if layout == TenBitLayout::P422 && threads == 4 {
                    continue;
                }
                let s = run(&Run {
                    case: &format!("{fit:?} {canvas:?}"),
                    tl: &tl,
                    assets: &assets,
                    layout,
                    opts: options(layout, 30.0),
                    threads: Some(threads),
                    argv: None,
                    extra: Vec::new(),
                    frames: vec![0..18],
                    allow_refused: true,
                    exported: None,
                });
                d10b::report(&format!("{fit:?} {canvas:?}"), layout, Some(threads), &s);
                total.add(s);
            }
        }
    }
    finish(&total);
}

/// A machine with many cores: the budget's threads (75 percent of 24, 32, 64 cores...) are written into the argv, past the 16 FFmpeg's own default stops
/// at. The compositor told the count against the export told the same, at heights where several of these cut on an odd chroma row.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_machines_with_many_cores() {
    d10b::measured();
    let src = media("many-noise", "noise", (320, 180), "30", 2.0, "yuv420p", None);
    let assets = vec![src.clone()];
    let mut total = Stats::default();
    for h in [30u32, 54, 90, 126, 150, 162, 198, 270, 360] {
        let w = ((h * 16 / 9) & !1).max(16);
        let mut c = clip(&src, 0.1, 0.2, 0.0);
        c.fade_in = 0.1;
        let tl = timeline(vec![vec![c]], vec![], (w, h), Fit::Cover, 30.0);
        for n in [17u32, 18, 24, 31, 32, 36, 48, 64, 100] {
            let s = run(&Run {
                case: &format!("height {h} on {n} threads"),
                tl: &tl,
                assets: &assets,
                layout: TenBitLayout::P420,
                opts: options(TenBitLayout::P420, 30.0),
                threads: Some(n),
                argv: None,
                extra: Vec::new(),
                frames: vec![0..6],
                allow_refused: true,
                exported: None,
            });
            if s.differing != 0 {
                d10b::report(&format!("height {h}"), TenBitLayout::P420, Some(n), &s);
            }
            total.add(s);
        }
    }
    eprintln!(
        "many cores: drawn {} refused {} ({:?}) compared {} differing {}",
        total.drawn, total.refused, total.kinds, total.compared, total.differing
    );
    assert!(total.drawn > 100);
    finish(&total);
}

/// The CPU model's `fade` on every 10-bit value (luma and chroma) at every factor of fades of many lengths, held to FFmpeg's own `fade` filter on a
/// ramp picture. The rounding constant of the model only matters where `(v - mid) * factor` is an odd multiple of 32768, which a 0.1 s fade never
/// reaches: a long fade (hundreds of frames) does, at a few frames.
#[test]
#[ignore = "needs ffmpeg"]
fn checker_d10b_the_fade_model_at_every_factor_of_long_fades() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let (w, h) = (1024usize, 4usize);
    // Luma: 0..1023 along each row; chroma (512 x 2): 0..1023 in reading order.
    let mut frame = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            frame.extend_from_slice(&(x as u16).to_le_bytes());
        }
    }
    for i in 0..(w / 2) * (h / 2) * 2 {
        frame.extend_from_slice(&((i % 1024) as u16).to_le_bytes());
    }
    let mut compared = 0usize;
    let mut worst_frames = Vec::new();
    for n in [41u32, 64, 128, 283, 291, 295, 297, 299] {
        for kind in ["in", "out"] {
            let d = f64::from(n) / 30.0;
            let (st, total) = if kind == "in" {
                (0.0, n + 3)
            } else {
                (1.0 / 30.0 * 2.0, n + 5)
            };
            let filter = format!("fade=t={kind}:st={st:.9}:d={d:.9}");
            let mut child = Command::new(kerf_core::ffmpeg_path())
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p10le",
                    "-s",
                    &format!("{w}x{h}"),
                    "-framerate",
                    "30",
                    "-i",
                    "pipe:0",
                    "-vf",
                    &filter,
                    "-f",
                    "rawvideo",
                    "pipe:1",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("ffmpeg");
            let mut stdin = child.stdin.take().expect("stdin");
            let input = frame.clone();
            let feeder = std::thread::spawn(move || {
                for _ in 0..total {
                    if stdin.write_all(&input).is_err() {
                        break;
                    }
                }
            });
            let out = child.wait_with_output().expect("output");
            let _ = feeder.join();
            assert!(out.status.success(), "{filter}: {}", String::from_utf8_lossy(&out.stderr));
            let frame_bytes = frame.len();
            assert_eq!(out.stdout.len(), frame_bytes * total as usize, "{filter}: frames");
            let start = if kind == "in" { 0 } else { 2 };
            for i in 0..total as usize {
                let got = d10b::samples(&out.stdout[i * frame_bytes..(i + 1) * frame_bytes]);
                // FFmpeg's factor: (pts - start) * 65535 / duration, inverted for a fade out; no change once the fade is over.
                let into = i as i64 - start as i64;
                let up = (into.max(0) * 65535 / i64::from(n)).min(65535) as u32;
                let factor = if kind == "in" { up } else { 65535 - up };
                let started = i >= start;
                for (p, (sample, original)) in got.iter().zip(samples_of(&frame)).enumerate() {
                    let chroma = p >= w * h;
                    let want = if started && factor < 65535 && (kind == "out" || i < start + n as usize) {
                        kerf_gpu::delivery10::fade_sample(original, factor, chroma)
                    } else {
                        original
                    };
                    if *sample != want {
                        worst_frames.push(format!(
                            "{filter} frame {i} sample {p} ({}): FFmpeg {sample}, the model {want} at factor {factor}",
                            if chroma { "chroma" } else { "luma" }
                        ));
                        break;
                    }
                }
                compared += got.len();
            }
        }
    }
    eprintln!(
        "the fade model: {compared} samples compared, {} frames differ",
        worst_frames.len()
    );
    assert!(worst_frames.is_empty(), "{}", worst_frames.join("\n"));
}

fn samples_of(frame: &[u8]) -> Vec<u16> {
    d10b::samples(frame)
}

/// End to end: the compositor's fade passes at every frame of fades of 299 and 297 frames (and one fading out), where `(v - mid) * factor` reaches an odd
/// multiple of 32768 for some values at a few frames, so that the rounding constant of the pass (and of the model) is looked at.
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough); minutes"]
fn checker_d10b_long_fades_where_the_rounding_constant_matters() {
    d10b::measured();
    let src = media("long-fade-noise", "noise", (160, 90), "30", 13.0, "yuv420p", None);
    let assets = vec![src.clone()];
    let mut total = Stats::default();
    for (frames, inward) in [(299u32, true), (297, true), (299, false)] {
        let mut c = clip(&src, 0.0, 11.0, 0.0);
        let secs = f64::from(frames) / 30.0;
        if inward {
            c.fade_in = secs;
        } else {
            c.fade_out = secs;
        }
        let tl = timeline(vec![vec![c]], vec![], (64, 36), Fit::Cover, 30.0);
        let range = if inward {
            0..frames as u64 + 2
        } else {
            330 - frames as u64 - 2..330
        };
        for layout in LAYOUTS {
            let s = run(&Run {
                case: &format!("a fade of {frames} frames {}", if inward { "in" } else { "out" }),
                tl: &tl,
                assets: &assets,
                layout,
                opts: options(layout, 30.0),
                threads: Some(3),
                argv: None,
                extra: Vec::new(),
                frames: vec![range.clone()],
                allow_refused: true,
                exported: None,
            });
            d10b::report(
                &format!("a fade of {frames} frames {}", if inward { "in" } else { "out" }),
                layout,
                Some(3),
                &s,
            );
            total.add(s);
        }
    }
    finish(&total);
}

/// Steep shrinks of a wide picture (320x180, 640x360) into small frames, 4:2:2 and 4:2:0, at several thread counts: the ratio at which the chroma
/// planes' vertical tables change shape. (Found by the heights sweep: 320x180 into 66x38, 4:2:2, a chroma sample one level off.)
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_steep_shrinks_of_a_wide_picture() {
    d10b::measured();
    let wide = media("steep-noise-320", "noise", (320, 180), "30", 2.0, "yuv420p", None);
    let wider = media("steep-noise-640", "noise", (640, 360), "30", 2.0, "yuv420p", None);
    let mut total = Stats::default();
    for (src, name) in [(&wide, "320x180"), (&wider, "640x360")] {
        let assets = vec![src.clone()];
        for (w, h) in [
            (66u32, 38u32),
            (66, 36),
            (64, 36),
            (68, 38),
            (70, 40),
            (80, 44),
            (60, 34),
            (90, 50),
            (132, 76),
            (44, 26),
            (50, 28),
        ] {
            let mut c = clip(src, 0.1, 0.3, 0.0);
            c.fade_in = 0.1;
            let tl = timeline(vec![vec![c]], vec![], (w, h), Fit::Cover, 30.0);
            for layout in LAYOUTS {
                let s = run(&Run {
                    case: &format!("{name} into {w}x{h}"),
                    tl: &tl,
                    assets: &assets,
                    layout,
                    opts: options(layout, 30.0),
                    threads: Some(3),
                    argv: None,
                    extra: Vec::new(),
                    frames: vec![0..9],
                    allow_refused: true,
                    exported: None,
                });
                d10b::report(&format!("{name} into {w}x{h}"), layout, Some(3), &s);
                total.add(s);
            }
        }
    }
    finish(&total);
}

/// The cut of `checker_d10b_heights_where_the_slices_flip` at height 38, where the product's lossless VP9 file and the compositor differed by one level in two
/// samples of a 4:2:2 chroma plane: is the file the graph's planes (the raw pipe of the same argv)?
#[test]
#[ignore = "needs ffmpeg and a GPU adapter (lavapipe is enough)"]
fn checker_d10b_height_38_the_file_against_the_graphs_planes_and_the_compositor() {
    d10b::measured();
    let src = media("noise-160x90-30-3", "noise", (160, 90), "30", 3.0, "yuv420p", None);
    let wide = media("noise-320x180-30-3", "noise", (320, 180), "30", 3.0, "yuv420p", None);
    let assets = vec![src.clone(), wide.clone()];
    let mut c = clip(&src, 0.1, 0.3, 0.0);
    c.fade_in = 0.1;
    let big = clip(&wide, 0.1, 0.2, 0.3);
    let tl = timeline(vec![vec![c, big]], vec![], (66, 38), Fit::Cover, 30.0);
    for layout in LAYOUTS {
        let opts = options(layout, 30.0);
        let real = d10b::real_export(&tl, &assets, layout, 30.0, &[]);
        let args = kerf_core::export_args(&tl, &assets, "OUT", &opts).expect("argv");
        let bytes = d10b::frame_samples(layout, (66, 38)) * 2;
        let raw = d10b::run_frames(&d10b::raw_run(&args, Some(3), &[], 14), bytes, &|_| true);
        let raw5 = d10b::run_frames(&d10b::raw_run(&args, None, &[], 14), bytes, &|_| true);
        let mut apart = Vec::new();
        for k in 0..15u64 {
            if real.get(&k) != raw.get(&k) {
                let (a, b) = (d10b::samples(&real[&k]), d10b::samples(&raw[&k]));
                let n = a.iter().zip(&b).filter(|(x, y)| x != y).count();
                apart.push(format!("frame {k}: file vs raw pipe at 3 threads: {n} samples"));
            }
            if raw.get(&k) != raw5.get(&k) {
                apart.push(format!("frame {k}: raw pipe at 3 threads vs at the default"));
            }
        }
        eprintln!("{layout:?}: the file against the graph's planes: {apart:?}");
        let exported = raw.clone();
        let s = run(&Run {
            case: "height 38 against the raw pipe",
            tl: &tl,
            assets: &assets,
            layout,
            opts,
            threads: Some(3),
            argv: None,
            extra: Vec::new(),
            frames: vec![0..15],
            allow_refused: true,
            exported: Some(exported),
        });
        d10b::report("height 38 against the raw pipe", layout, Some(3), &s);
    }
}
