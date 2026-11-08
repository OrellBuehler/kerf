//! `checker/bl`: the blend probe's `TRANSLUCENT` extra. The probe runs the export's own `colorchannelmixer` / `split` / `lutyuv` / `overlay` /
//! `alphaextract` chain on a clip below full opacity and keeps the extra only if FFmpeg's planes are the model's. Each scenario runs the probe in a
//! process of its own (the probe's state is the process's) against this FFmpeg, or against a wrapper script that breaks one premise of the
//! model: no SIMD kernels (`-cpuflags 0`), another alpha rounding (`aa=0.604` for the `0.6` the probe writes), and a child that never answers;
//! with the vector extensions switched off one by one (`-cpuflags -avx512`, `-avx2`, `-avx`) it prints the verdict (the wrappers it writes are
//! the `KERF_FFMPEG`s to hold the parity cases to under the same extensions). The extra must be out for every broken premise, in for none of them, and
//! the probe bounded. No adapter. `#[ignore]`d:
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_bl_probe -- --ignored --nocapture
//! KERF_FFMPEG=.../ffmpeg-<triple> KERF_FFPROBE=.../ffprobe-<triple> cargo test ...   # the pinned build
//! ```

#![allow(clippy::print_stderr)]
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use kerf_core::{BlendExtras, BlendKinds};
use kerf_gpu::blend;

const SCENARIO: &str = "KERF_BL_PROBE_SCENARIO";

fn wrapper(scenario: &str) -> Option<PathBuf> {
    let real = kerf_core::ffmpeg_path();
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("checker-bl-probe");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("ffmpeg-{scenario}"));
    let body = match scenario {
        "plain" => return None,
        "cpuflags0" => format!("#!/bin/sh\nexec '{real}' -cpuflags 0 \"$@\"\n"),
        "noavx512" => format!("#!/bin/sh\nexec '{real}' -cpuflags -avx512 \"$@\"\n"),
        "noavx2" => format!("#!/bin/sh\nexec '{real}' -cpuflags -avx2-avx512 \"$@\"\n"),
        "noavx" => format!("#!/bin/sh\nexec '{real}' -cpuflags -avx-avx2-avx512 \"$@\"\n"),
        "rounding" => format!(
            "#!/bin/bash\nargs=()\nfor a in \"$@\"; do args+=(\"${{a//colorchannelmixer=aa=0.6/colorchannelmixer=aa=0.604}}\"); done\nexec '{real}' \"${{args[@]}}\"\n"
        ),
        "stall" => format!(
            "#!/bin/sh\ncase \"$*\" in *'colorchannelmixer=aa=0.6'*) sleep 31 & wait; exit 1;; esac\nexec '{real}' \"$@\"\n"
        ),
        other => panic!("no scenario {other}"),
    };
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Some(path)
}

/// The scenario's probe, in this process: prints what it found and how long it took.
#[test]
#[ignore = "needs ffmpeg and a shell; run by checker_bl_the_probe_keeps_translucent_only_where_its_premise_holds"]
fn checker_bl_probe_scenario() {
    let scenario = std::env::var(SCENARIO).unwrap_or_else(|_| "plain".into());
    if let Some(w) = wrapper(&scenario) {
        std::env::set_var("KERF_FFMPEG", &w);
    }
    let started = Instant::now();
    let kinds = blend::kernels();
    let took = started.elapsed();
    let extras = blend::extras();
    let again = Instant::now();
    let _ = blend::kernels();
    eprintln!(
        "PROBE scenario={scenario} kinds={kinds:?} translucent={} bt2020={} rgb={} took={took:?} second call={:?}",
        extras.contains(BlendExtras::TRANSLUCENT),
        extras.contains(BlendExtras::BT2020),
        extras.contains(BlendExtras::RGB_PICTURE),
        again.elapsed(),
    );
}

fn run_scenario(scenario: &str) -> String {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "checker_bl_probe_scenario",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCENARIO, scenario)
        .stdin(Stdio::null())
        .output()
        .expect("run the scenario");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let line = text
        .lines()
        .find(|l| l.starts_with("PROBE "))
        .unwrap_or_else(|| panic!("scenario {scenario}: no PROBE line in:\n{text}"))
        .to_string();
    eprintln!("{line}");
    line
}

fn field(line: &str, key: &str) -> String {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {line}"))
        .to_string()
}

fn secs(line: &str) -> f64 {
    let t = field(line, "took");
    t.strip_suffix("ms")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|v| v / 1e3)
        .or_else(|| t.strip_suffix('s').and_then(|v| v.parse::<f64>().ok()))
        .unwrap_or_else(|| panic!("took {t}"))
}

#[test]
#[ignore = "needs ffmpeg and a shell"]
fn checker_bl_the_probe_keeps_translucent_only_where_its_premise_holds() {
    let all = format!("{:?}", [BlendKinds::ALL; 2]);
    // This FFmpeg, untouched: every mode on both matrices and the extra, the whole probe in seconds.
    let plain = run_scenario("plain");
    assert_eq!(field(&plain, "translucent"), "true", "{plain}");
    assert!(plain.contains(&format!("kinds={all}")), "{plain}");
    assert!(secs(&plain) < 20.0, "the probe is bounded: {plain}");
    // No SIMD in swscale and the filters: the modes are the model's still and the translucent chain is not.
    let cpu = run_scenario("cpuflags0");
    assert_eq!(field(&cpu, "translucent"), "false", "{cpu}");
    // Another alpha rounding behind the same text.
    let rounding = run_scenario("rounding");
    assert_eq!(field(&rounding, "translucent"), "false", "{rounding}");
    assert!(
        rounding.contains(&format!("kinds={all}")),
        "the modes do not use the opacity: {rounding}"
    );
    // The vector extensions switched off one by one: the verdict is printed, and the parity cases are run under the same wrappers.
    for scenario in ["noavx512", "noavx2", "noavx"] {
        run_scenario(scenario);
    }
    // A child that never answers: nothing is measured, nothing kept, the extra is out, and the probe returned at its deadline.
    let stall = run_scenario("stall");
    assert_eq!(field(&stall, "translucent"), "false", "{stall}");
    assert!(stall.contains(&format!("kinds={:?}", [BlendKinds::NONE; 2])), "{stall}");
    let took = secs(&stall);
    assert!(
        (15.0..27.0).contains(&took),
        "the probe waits for its deadline and no longer: {stall}"
    );
}
