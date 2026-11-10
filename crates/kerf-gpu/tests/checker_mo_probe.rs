//! `checker/mo/probe`: does `kerf_gpu::roundtrip_probe::translucency()` notice a build that rounds a mask's alpha another way? The probe runs
//! the export's own `colorchannelmixer` text through whatever `ffmpeg` is configured, so a shim in front of the real one that edits the
//! opacity in that text stands for a build whose `lrint` meets a tie another way (an `aa` one ulp under 0.5 or 0.25 rounds every odd level
//! down where the real filter goes to even), without touching the opacities of the unmasked cases (0.6, 0.999, 0.04) the probe also runs: only
//! the masked cases, whose alpha plane holds every level, can tell. Each variant runs the probe in a child process of this test binary (the
//! verdict is cached for a process's life); the verdict must be `Differs`, and name a masked case.
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_mo_probe -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

/// The child: asks the probe, prints its verdict. Does nothing unless a parent set `MO_PROBE_CHILD`.
#[test]
#[ignore = "run by checker_mo_probe_notices_a_build_that_rounds_the_masked_alpha_another_way"]
fn checker_mo_probe_child() {
    if std::env::var_os("MO_PROBE_CHILD").is_none() {
        return;
    }
    let verdict = kerf_gpu::roundtrip_probe::translucency();
    println!("VERDICT {verdict:?}");
}

#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg"]
fn checker_mo_probe_notices_a_build_that_rounds_the_masked_alpha_another_way() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let real = kerf_core::ffmpeg_path();
    let dir = std::env::temp_dir().join(format!("checker-mo-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = std::env::current_exe().unwrap();
    // (variant, rewrite of the opacity in the text, a masked case it must be named by)
    let variants: [(&str, &str, &str); 5] = [
        ("control, nothing rewritten", "", ""),
        ("aa=0.5 one ulp under", "aa=0.5,|aa=0.49999999999999994,", "50 %"),
        ("aa=0.25 one ulp under", "aa=0.25,|aa=0.24999999999999997,", "25 %"),
        ("aa=0.85 one ulp over", "aa=0.85,|aa=0.8500000000000001,", "85 %"),
        ("aa=0.5 a hair over", "aa=0.5,|aa=0.5000001,", "50 %"),
    ];
    let mut report = Vec::new();
    for (i, (name, rewrite, case)) in variants.iter().enumerate() {
        let shim = dir.join(format!("ffmpeg-shim-{i}.sh"));
        let (from, to) = rewrite.split_once('|').unwrap_or(("", ""));
        let body = format!(
            "#!/bin/bash\nargs=()\nfor a in \"$@\"; do\n  if [ -n '{from}' ]; then a=\"${{a//{from}/{to}}}\"; fi\n  args+=(\"$a\")\ndone\nexec \"$MO_REAL_FFMPEG\" $MO_EXTRA \"${{args[@]}}\"\n"
        );
        std::fs::write(&shim, body).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "checker_mo_probe_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MO_PROBE_CHILD", "1")
            .env("MO_REAL_FFMPEG", &real)
            .env("KERF_FFMPEG", &shim)
            .output()
            .expect("run the child");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let verdict = text
            .find("VERDICT ")
            .map_or("no verdict", |at| text[at..].lines().next().unwrap_or(""))
            .to_string();
        eprintln!("checker/mo/probe {name}: {verdict}");
        if rewrite.is_empty() {
            assert!(verdict.contains("Match"), "the control: {verdict}\n{text}");
        } else {
            assert!(verdict.contains("Differs"), "{name}: the probe did not notice: {verdict}");
            assert!(
                verdict.contains("masked"),
                "{name}: a masked case must be what notices: {verdict}"
            );
            assert!(
                verdict.contains(case),
                "{name}: expected the {case} case to be named: {verdict}"
            );
        }
        report.push(format!("{name}: {verdict}"));
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("checker/mo/probe: {} variants\n{}", report.len(), report.join("\n"));
}

/// What the probe says of this FFmpeg run under other CPU feature sets and scaler flags (prints, judges nothing): the machines the compositor's
/// round trip is not measured on are the ones whose swscale picks other kernels. A `Match` here for a set whose luma and chroma differ would be a
/// probe that cannot tell; every set that differs from the model must be `Differs`.
#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg"]
fn checker_mo_probe_under_other_cpu_flags_and_scaler_flags() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let real = kerf_core::ffmpeg_path();
    let dir = std::env::temp_dir().join(format!("checker-mo-probe-flags-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = std::env::current_exe().unwrap();
    let shim = dir.join("ffmpeg-shim.sh");
    std::fs::write(&shim, "#!/bin/bash\nexec \"$MO_REAL_FFMPEG\" $MO_EXTRA \"$@\"\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    for extra in [
        "",
        "-cpuflags 0",
        "-cpuflags -avx2",
        "-cpuflags -avx2-avx",
        "-cpuflags -ssse3",
        "-cpuflags -sse4",
        "-cpuflags -sse2",
        "-sws_flags +accurate_rnd",
        "-sws_flags +full_chroma_int",
        "-sws_flags +bitexact",
        "-sws_flags bilinear",
    ] {
        let out = Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "checker_mo_probe_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("MO_PROBE_CHILD", "1")
            .env("MO_REAL_FFMPEG", &real)
            .env("MO_EXTRA", extra)
            .env("KERF_FFMPEG", &shim)
            .output()
            .expect("run the child");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let verdict = text
            .find("VERDICT ")
            .map_or("no verdict", |at| text[at..].lines().next().unwrap_or(""));
        eprintln!(
            "checker/mo/probe flags [{extra}]: {}",
            verdict.chars().take(260).collect::<String>()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
