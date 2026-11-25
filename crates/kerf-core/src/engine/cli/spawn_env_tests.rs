//! The environment a child of the engine is started in (`command`, `bg_command`, `ffmpeg_command`).
//!
//! The distro FFmpeg links `libgomp`, which pins the initial thread of FFmpeg's process as it loads when an OpenMP binding variable is
//! set, so FFmpeg counts one CPU where Kerf counts all of them (`checker_p2_threads` holds the plan's thread range to the graph the real
//! binary runs). The variables that bind are removed from the child's environment; the ones that tune (`OMP_NUM_THREADS`) are not.

use super::*;

const CHILD: &str = "KERF_SPAWN_ENV_CHILD";

/// What `sh` started through `build` sees of each variable: `NAME=value` or `NAME=unset`.
fn seen(build: fn(&str) -> Command, names: &[&str]) -> Vec<String> {
    let script: String = names.iter().map(|n| format!("echo {n}=${{{n}-unset}}; ")).collect();
    let out = build("sh").args(["-c", &script]).output().expect("sh runs");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
}

/// The child half: run (by the test below) with the variables set in its environment, and says what each builder's `sh` sees.
#[test]
#[allow(clippy::print_stderr)]
fn a_spawn_env_child() {
    if std::env::var(CHILD).is_err() {
        return;
    }
    let mut names: Vec<&str> = OPENMP_BINDING_VARS.to_vec();
    names.extend([
        "OMP_NUM_THREADS",
        "OMP_THREAD_LIMIT",
        "OMP_WAIT_POLICY",
        "KERF_SPAWN_ENV_KEPT",
    ]);
    for (label, build) in [("command", command as fn(&str) -> Command), ("bg_command", bg_command)] {
        for line in seen(build, &names) {
            eprintln!("SPAWN-ENV {label} {line}");
        }
    }
}

#[test]
fn a_child_does_not_inherit_the_variables_that_bind_an_openmp_runtime_and_keeps_the_ones_that_tune_it() {
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = Command::new(exe);
    cmd.args([
        "engine::cli::spawn_env_tests::a_spawn_env_child",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(CHILD, "1")
    .env("OMP_PROC_BIND", "true")
    .env("OMP_PLACES", "cores")
    .env("GOMP_CPU_AFFINITY", "0-3")
    .env("KMP_AFFINITY", "compact")
    .env("OMP_NUM_THREADS", "2")
    .env("OMP_THREAD_LIMIT", "3")
    .env("OMP_WAIT_POLICY", "passive")
    .env("KERF_SPAWN_ENV_KEPT", "yes");
    let out = cmd.output().expect("the child runs");
    let text = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    for label in ["command", "bg_command"] {
        for var in OPENMP_BINDING_VARS {
            assert!(
                text.contains(&format!("SPAWN-ENV {label} {var}=unset")),
                "{label}: {var} reached the child:\n{text}"
            );
        }
        for (var, value) in [
            ("OMP_NUM_THREADS", "2"),
            ("OMP_THREAD_LIMIT", "3"),
            ("OMP_WAIT_POLICY", "passive"),
            ("KERF_SPAWN_ENV_KEPT", "yes"),
        ] {
            assert!(
                text.contains(&format!("SPAWN-ENV {label} {var}={value}")),
                "{label}: {var} did not reach the child:\n{text}"
            );
        }
    }
}

#[test]
fn the_variables_removed_are_exactly_the_binding_ones_and_the_argv_is_untouched() {
    assert_eq!(
        OPENMP_BINDING_VARS,
        ["OMP_PROC_BIND", "OMP_PLACES", "GOMP_CPU_AFFINITY", "KMP_AFFINITY"]
    );
    let cmd = command("ffmpeg");
    assert_eq!(cmd.get_args().count(), 0, "the argv is the caller's");
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    let mut want: Vec<String> = OPENMP_BINDING_VARS.iter().map(|v| (*v).to_string()).collect();
    want.sort();
    let mut got = removed;
    got.sort();
    assert_eq!(got, want);
    assert_eq!(cmd.get_envs().count(), want.len(), "nothing else is set or removed");
}
