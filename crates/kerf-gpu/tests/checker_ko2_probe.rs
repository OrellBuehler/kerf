//! `checker/ko2/probe`: does `kerf_gpu::roundtrip_probe::translucency()` notice a build that maps a chroma key's alpha another way? The probe's keyed
//! cases run the engine's own `chromakey` text and `colorchannelmixer=aa=<opacity>` through whatever `ffmpeg` is configured, so a shim in front of the
//! real one that edits the opacity in the text of the graphs that hold a `chromakey` (and only those: the masked and unmasked cases the probe also runs
//! are left alone) stands for a build whose `lrint` meets a tie another way, or whose scale differs. The verdict must be `Differs` and must name a keyed
//! case. A second shim edits the `geq` of `geq_edge()`'s probe (`a='alpha(X,Y)'` alone: the export's `geq`s are not touched) so that a build that clamps
//! its read is measured as one that copies it. Each variant runs in a child process of this test binary (the verdict is cached for a process's life).
//!
//! ```text
//! cargo test -p kerf-gpu --no-default-features --test checker_ko2_probe -- --ignored --nocapture
//! ```

#![allow(clippy::print_stderr)]

/// The child: asks the probe, prints its verdict. Does nothing unless a parent set `KO2_PROBE_CHILD`.
#[test]
#[ignore = "run by the checker_ko2_probe_* tests"]
#[allow(clippy::print_stdout)]
fn checker_ko2_probe_child() {
    if std::env::var_os("KO2_PROBE_CHILD").is_none() {
        return;
    }
    let verdict = kerf_gpu::roundtrip_probe::translucency();
    println!("VERDICT {verdict:?}");
    println!("GEQ {:?}", kerf_core::geq_edge());
}

#[cfg(unix)]
fn child(shim: Option<&std::path::Path>, extra: &str) -> (String, String) {
    use std::process::Command;
    let real = kerf_core::ffmpeg_path();
    let exe = std::env::current_exe().unwrap();
    let mut cmd = Command::new(&exe);
    cmd.args([
        "--ignored",
        "--exact",
        "checker_ko2_probe_child",
        "--nocapture",
        "--test-threads=1",
    ])
    .env("KO2_PROBE_CHILD", "1")
    .env("KO2_REAL_FFMPEG", &real)
    .env("KO2_EXTRA", extra);
    if let Some(shim) = shim {
        cmd.env("KERF_FFMPEG", shim);
    }
    let out = cmd.output().expect("run the child");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = |tag: &str| {
        text.find(tag)
            .map_or("no verdict", |at| text[at..].lines().next().unwrap_or(""))
            .to_string()
    };
    (line("VERDICT "), line("GEQ "))
}

/// `s` as a bash `$'...'` word.
#[cfg(unix)]
fn bash_word(s: &str) -> String {
    format!("$'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A shim that applies the bash substitution `${arg//from/to}` to every argument that contains `only` (all of them when `only` is empty).
#[cfg(unix)]
fn shim(dir: &std::path::Path, name: &str, only: &str, from: &str, to: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let body = format!(
        "#!/bin/bash\nonly={}\nfrom={}\nto={}\nargs=()\nfor a in \"$@\"; do\n  if [ -n \"$from\" ] && [[ \"$a\" == *\"$only\"* ]]; then a=\"${{a//\"$from\"/\"$to\"}}\"; fi\n  args+=(\"$a\")\ndone\nexec \"$KO2_REAL_FFMPEG\" $KO2_EXTRA \"${{args[@]}}\"\n",
        bash_word(only),
        bash_word(from),
        bash_word(to)
    );
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg"]
fn checker_ko2_probe_notices_a_build_that_maps_the_keyed_alpha_another_way() {
    let dir = std::env::temp_dir().join(format!("checker-ko2-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // (variant, text to find in a graph that holds a chromakey, its replacement, whether the probe must say Differs)
    let variants: [(&str, &str, &str, bool); 9] = [
        ("control, nothing rewritten", "", "", false),
        ("aa=0.5 one ulp under", "aa=0.5,", "aa=0.49999999999999994,", true),
        ("aa=0.5 a hair over", "aa=0.5,", "aa=0.5000001,", true),
        ("aa=0.85 one ulp over", "aa=0.85,", "aa=0.8500000000000001,", true),
        ("aa=0.85 a hair under", "aa=0.85,", "aa=0.8499999,", true),
        ("aa=0.04 to 0.045", "aa=0.04,", "aa=0.045,", true),
        ("aa=0.6 to 0.59", "aa=0.6,", "aa=0.59,", true),
        ("aa=0.5 to 0.51", "aa=0.5,", "aa=0.51,", true),
        (
            "the key's own similarity moved in both branches (a relation, not a value)",
            "chromakey=0x808080:0.1:0.5",
            "chromakey=0x808080:0.1001:0.5",
            false,
        ),
    ];
    let mut report = Vec::new();
    let mut unnoticed = Vec::new();
    for (i, (name, from, to, must_differ)) in variants.iter().enumerate() {
        let sh = shim(&dir, &format!("ffmpeg-shim-{i}.sh"), "chromakey", from, to);
        let (verdict, _) = child(Some(&sh), "");
        eprintln!("checker/ko2/probe {name}: {verdict}");
        if !must_differ {
            assert!(verdict.contains("Match"), "{name}: must match: {verdict}");
        } else if verdict.contains("Differs") {
            assert!(verdict.contains("a keyed"), "{name}: a keyed case must be what notices: {verdict}");
        } else {
            unnoticed.push(format!("{name}: {verdict}"));
        }
        report.push(format!("{name}: {verdict}"));
    }
    // Not asserted: a hair at 4 % flips only the levels 213 and 238, which the keyed picture of that case may not hold (the masked case holds every
    // level, and the verdict is one for all of them).
    for (name, from, to) in [("aa=0.04 to 0.0399 (a hair)", "aa=0.04,", "aa=0.0399,")] {
        let sh = shim(&dir, "ffmpeg-shim-hair.sh", "chromakey", from, to);
        eprintln!("checker/ko2/probe (informational) {name}: {}", child(Some(&sh), "").0);
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("checker/ko2/probe: {} variants\n{}", report.len(), report.join("\n"));
    assert!(unnoticed.is_empty(), "the probe did not notice:\n{}", unnoticed.join("\n"));
}

/// The graphs without a chromakey are left alone by the shim above, so a `Differs` that names a keyed case is the keyed cases' own: the same
/// perturbation applied to the masked and unmasked graphs only (`only` = `geq=`... no, to every graph that has no chromakey) leaves the keyed cases
/// matching, and the verdict names a masked or an unmasked case, never a keyed one.
#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg"]
fn checker_ko2_probe_names_a_keyed_case_only_for_the_keyed_graphs() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("checker-ko2-probe-other-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ffmpeg-shim.sh");
    let body = "#!/bin/bash\nargs=()\nfor a in \"$@\"; do\n  if [[ \"$a\" != *chromakey* ]]; then a=\"${a//aa=0.5,/aa=0.5000001,}\"; fi\n  args+=(\"$a\")\ndone\nexec \"$KO2_REAL_FFMPEG\" $KO2_EXTRA \"${args[@]}\"\n";
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (verdict, _) = child(Some(&path), "");
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("checker/ko2/probe other graphs rewritten: {verdict}");
    assert!(verdict.contains("Differs"), "{verdict}");
    assert!(
        !verdict.contains("a keyed"),
        "a keyed case must not be what notices a change to the other graphs: {verdict}"
    );
}

/// `geq_edge()`'s probe asks `geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='alpha(X,Y)'` of a picture whose every sample differs. A shim that gives that
/// text (and only that: the export's `geq`s end `*alpha(X,Y)'`) the nearest-neighbour read makes a build that clamps its bilinear read look like one that
/// copies: the verdict becomes `Exact` where 6.1 says `Penultimate`. (Printed, and asserted on a build whose answer is `Penultimate`; 9.0's is `Exact` both ways.)
#[cfg(unix)]
#[test]
#[ignore = "needs ffmpeg"]
fn checker_ko2_probe_geq_edge_follows_the_text_it_is_given() {
    let dir = std::env::temp_dir().join(format!("checker-ko2-probe-geq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let plain = child(None, "").1;
    let sh = shim(
        &dir,
        "ffmpeg-shim-geq.sh",
        "a='alpha(X,Y)'",
        "a='alpha(X,Y)'",
        "a='alpha(X,Y)':interpolation=nearest",
    );
    let (_, bent) = child(Some(&sh), "");
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("checker/ko2/probe geq edge: plain {plain}, with the probe's geq read as nearest {bent}");
    if plain.contains("Penultimate") {
        assert!(bent.contains("Exact"), "{bent}");
    } else {
        assert!(plain.contains("Exact") && bent.contains("Exact"), "{plain} / {bent}");
    }
}
