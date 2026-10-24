//! `kerf-app --headless`: the MCP server with no window and no webview, for an agent on a machine
//! with no display (CI, a server, a container).
//!
//! No Tauri app is built — that opens a display — so this runs on Tauri's async runtime alone, with
//! a [`Shell::Headless`] in place of the `AppHandle`. stdout carries one JSON line once the server
//! listens (the ready line); logs go to stderr and the log directory. It runs until SIGINT / SIGTERM:
//! the listener closes, every job is stopped, an untitled project with work is kept for recovery.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kerf_core::Project;

use crate::shell::{Headless, Shell};
use crate::{lock_user, mcp, recovery, settings, AppState, LaunchSlot};

/// The environment variable a headless run takes its token from (this run only, never stored).
const ENV_TOKEN: &str = "KERF_MCP_TOKEN";

const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_NOT_LISTENING: i32 = 3;
const EXIT_INTERRUPTED: i32 = 130;

const USAGE: &str = "\
kerf-app --headless [options]

Run Kerf's MCP server with no window. Prints one JSON line on stdout once it listens;
logs go to stderr. Stops on SIGINT / SIGTERM.

  --project PATH.kerf      open an existing project (default: a new untitled one)
  --new-project PATH.kerf  create a project file and open it (refused if it exists)
  --mcp-addr HOST:PORT     where to listen (default: KERF_MCP_ADDR, the setting, 127.0.0.1:7777;
                           port 0 picks a free one)
  --data-dir DIR           keep settings, logs and recovery files in DIR instead of the app's own
  --token-file PATH        write the bearer token to PATH (mode 0600)
  --show-token             print the bearer token on stderr
  --review                 stage the agent's edits for review (default: they land live)
  --proxies                build preview proxies (default: off)
  --cpu-percent N          share of the machine one heavy job may take, this run only

The token is KERF_MCP_TOKEN when set (this run only, never stored), else the one in the settings.

Exit codes: 0 stopped by a signal, 1 failure, 2 bad arguments, 3 the server could not listen.";

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectArg {
    Open(PathBuf),
    Create(PathBuf),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    project: Option<ProjectArg>,
    mcp_addr: Option<String>,
    data_dir: Option<PathBuf>,
    token_file: Option<PathBuf>,
    show_token: bool,
    review: bool,
    proxies: bool,
    cpu_percent: Option<u8>,
    help: bool,
}

/// Whether the command line asks for a headless run.
pub(crate) fn requested(argv: &[String]) -> bool {
    argv.iter().skip(1).any(|a| a == "--headless")
}

fn absolute(path: &str, cwd: &Path) -> PathBuf {
    cwd.join(path)
}

fn kerf_path(path: &str, cwd: &Path) -> Result<PathBuf, String> {
    let path = absolute(path, cwd);
    let is_kerf = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("kerf"));
    if !is_kerf {
        return Err(format!(
            "{} is not a project file: Kerf projects end in .kerf",
            path.display()
        ));
    }
    Ok(path)
}

/// The command line of a headless run (`None` when it is not one), relative paths against `cwd`.
fn parse(argv: &[String], cwd: &Path) -> Result<Option<Args>, String> {
    if !requested(argv) {
        return Ok(None);
    }
    let mut args = Args::default();
    let mut rest = argv.iter().skip(1).map(String::as_str);
    while let Some(arg) = rest.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (arg, None),
        };
        let mut value = || {
            inline
                .or_else(|| rest.next())
                .map(str::to_string)
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        let switch = |on: &mut bool| match inline {
            Some(_) => Err(format!("{flag} takes no value")),
            None => {
                *on = true;
                Ok(())
            }
        };
        match flag {
            "--headless" => switch(&mut true)?,
            "-h" | "--help" => switch(&mut args.help)?,
            "--show-token" => switch(&mut args.show_token)?,
            "--review" => switch(&mut args.review)?,
            "--proxies" => switch(&mut args.proxies)?,
            "--project" | "--new-project" => {
                if args.project.is_some() {
                    return Err("give one --project or --new-project".to_string());
                }
                let path = kerf_path(&value()?, cwd)?;
                args.project = Some(match flag {
                    "--project" => ProjectArg::Open(path),
                    _ => ProjectArg::Create(path),
                });
            }
            "--mcp-addr" => {
                let addr = value()?.trim().to_string();
                mcp::check_addr(&addr)?;
                args.mcp_addr = Some(addr);
            }
            "--data-dir" => args.data_dir = Some(absolute(&value()?, cwd)),
            "--token-file" => args.token_file = Some(absolute(&value()?, cwd)),
            "--cpu-percent" => {
                let range = kerf_core::MIN_CPU_PERCENT..=100;
                let percent = value()?
                    .parse()
                    .ok()
                    .filter(|p| range.contains(p))
                    .ok_or_else(|| format!("--cpu-percent takes a number from {} to 100", range.start()))?;
                args.cpu_percent = Some(percent);
            }
            other => return Err(format!("unknown argument {other:?} (see --help)")),
        }
    }
    Ok(Some(args))
}

/// Why a run ends early: the exit code and what to say.
struct Exit(i32, String);

/// Write a line to stderr, past the logs: what a person running this has to read (the token
/// included, which must never reach the log file).
fn say(line: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// Run headless with `argv` until a signal; the exit code.
pub(crate) fn main(argv: &[String], bundled_ffmpeg: &[(&'static str, PathBuf)]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_default();
    let args = match parse(argv, &cwd) {
        Ok(Some(args)) => args,
        Ok(None) => return EXIT_USAGE,
        Err(e) => {
            say(&format!("kerf-app: {e}\n\n{USAGE}"));
            return EXIT_USAGE;
        }
    };
    if args.help {
        let _ = writeln!(std::io::stdout().lock(), "{USAGE}");
        return 0;
    }
    let token = match check_args(&args) {
        Ok(token) => token,
        Err(Exit(code, message)) => {
            say(&format!("kerf-app: {message}"));
            return code;
        }
    };
    let (config, data) = match dirs_for(args.data_dir.as_deref()) {
        Ok(dirs) => dirs,
        Err(Exit(code, message)) => {
            say(&format!("kerf-app: {message}"));
            return code;
        }
    };
    crate::init_logging(data.as_ref().map(|d| d.join("logs")), true);
    crate::install_panic_hook();
    for (var, path) in bundled_ffmpeg {
        tracing::info!(%var, path = %path.display(), "using bundled FFmpeg binary");
    }
    match run(&args, token, config, data) {
        Ok(code) => code,
        Err(Exit(code, message)) => {
            tracing::error!("{message}");
            code
        }
    }
}

/// What can be refused before anything starts: the project file, the token file, the token. The
/// token from the environment, if any.
fn check_args(args: &Args) -> Result<Option<String>, Exit> {
    let usage = |message: String| Exit(EXIT_USAGE, message);
    match &args.project {
        // `Project::open` would create it: a mistyped path must not leave an empty project behind.
        Some(ProjectArg::Open(path)) if !path.is_file() => {
            return Err(usage(format!(
                "{} does not exist (--new-project creates one)",
                path.display()
            )));
        }
        Some(ProjectArg::Create(path)) if path.exists() => {
            return Err(usage(format!("{} already exists (--project opens it)", path.display())));
        }
        _ => {}
    }
    if let Some(path) = args.token_file.as_deref().filter(|p| p.is_dir()) {
        return Err(usage(format!("--token-file {} is a directory", path.display())));
    }
    let token = std::env::var(ENV_TOKEN)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if let Some(token) = &token {
        mcp::check_token(token).map_err(|e| usage(format!("{ENV_TOKEN}: {e}")))?;
    }
    Ok(token)
}

/// The config and data directories: `--data-dir` for both, else the desktop app's own (the
/// identifier is the build's, so a debug build keeps to the `.dev` ones).
fn dirs_for(data_dir: Option<&Path>) -> Result<(Option<PathBuf>, Option<PathBuf>), Exit> {
    match data_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| Exit(EXIT_USAGE, format!("cannot use --data-dir {}: {e}", dir.display())))?;
            Ok((Some(dir.to_path_buf()), Some(dir.to_path_buf())))
        }
        None => {
            let identifier = crate::context().config().identifier.clone();
            Ok((
                dirs::config_dir().map(|d| d.join(&identifier)),
                dirs::data_dir().map(|d| d.join(&identifier)),
            ))
        }
    }
}

fn run(args: &Args, token: Option<String>, config: Option<PathBuf>, data: Option<PathBuf>) -> Result<i32, Exit> {
    let failed = |e: kerf_core::Error| Exit(EXIT_FAILURE, e.to_string());
    let project = match &args.project {
        Some(ProjectArg::Open(path) | ProjectArg::Create(path)) => Project::open(path).map_err(failed)?,
        None => Project::open_in_memory().map_err(failed)?,
    };
    let project = Arc::new(Mutex::new(project));
    let state = AppState::new(
        project.clone(),
        Arc::new(AtomicBool::new(true)),
        LaunchSlot::default(),
        Arc::new(kerf_core::ScrubFrames::new(kerf_core::ScrubConfig::default())),
    );
    let shell = Shell::Headless(Arc::new(Headless::new(state, config, data.clone())));
    crate::set_app_dirs(&shell);
    if args.project.is_none() {
        recovery::start(&shell);
    }

    let stored = settings::load(&shell);
    settings::apply(&stored);
    if !args.proxies {
        kerf_core::proxy::set_preview_source_setting(kerf_core::PreviewSource::Original);
    }
    if let Some(percent) = args.cpu_percent {
        kerf_core::set_cpu_percent(percent);
    }
    crate::restore_speech_model(&project);
    crate::respawn_proxies(&shell);
    std::thread::spawn(kerf_core::fonts::warm);
    std::thread::spawn(crate::warm_transcription_backend);

    let token_source = match (&token, &stored.mcp_token) {
        (Some(_), _) => "env",
        (None, Some(_)) => "settings",
        (None, None) => "generated",
    };
    mcp::set_overrides(mcp::Overrides {
        addr: args.mcp_addr.clone(),
        token,
    });
    mcp::start(shell.clone(), project.clone());
    // Nobody is there to review: the agent's edits land live unless `--review` asks otherwise. For
    // this run only; the stored switch is the desktop app's.
    if !args.review {
        settings::set_agent_review(false);
        lock_user(&project).set_agent_review(false);
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %data.as_deref().map(|d| d.display().to_string()).unwrap_or_default(),
        project = %lock_user(&project).path().map(|p| p.display().to_string()).unwrap_or_default(),
        review = settings::agent_review(),
        "kerf starting headless"
    );
    tauri::async_runtime::block_on(serve(args, &shell, &project, token_source))
}

async fn serve(args: &Args, shell: &Shell, project: &Arc<Mutex<Project>>, token_source: &str) -> Result<i32, Exit> {
    // Listening before the ready line: a signal right after it must not kill the process outright.
    let mut signals = Signals::new().map_err(|e| Exit(EXIT_FAILURE, format!("cannot listen for signals: {e}")))?;
    let bound = mcp::wait_listening(Duration::from_secs(10))
        .await
        .map_err(|e| Exit(EXIT_NOT_LISTENING, format!("{e} (headless: pick another with --mcp-addr)")))?;
    let url = mcp::client_url(&bound.to_string());
    let token = mcp::current_token().unwrap_or_default();
    if let Some(path) = &args.token_file {
        settings::write_atomic(path, token.as_bytes())
            .map_err(|e| Exit(EXIT_FAILURE, format!("could not write the token to {}: {e}", path.display())))?;
    }
    say(&format!("kerf: MCP server listening at {url}"));
    let add = |token: &str| format!("claude mcp add --transport http kerf {url} --header \"Authorization: Bearer {token}\"");
    if args.show_token {
        say(&format!("kerf: token {token}\nkerf: {}", add(&token)));
    } else {
        let held = match (token_source, &args.token_file) {
            (_, Some(path)) => format!("written to {}", path.display()),
            ("env", None) => format!("the one in {ENV_TOKEN}"),
            _ => format!(
                "mcp_token in {} (or pass --show-token / --token-file)",
                settings::file_path(shell)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
        };
        say(&format!("kerf: the token is {held}\nkerf: {}", add("<token>")));
    }
    let ready = serde_json::json!({
        "kerf": "listening",
        "url": url,
        "addr": bound.to_string(),
        "pid": std::process::id(),
        "project": lock_user(project).path().map(|p| p.display().to_string()),
        "agent_review": settings::agent_review(),
        "token_source": token_source,
    });
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{ready}").and_then(|()| out.flush());
    drop(out);

    signals.next().await;
    tracing::info!("signal received; shutting down (a second one exits at once)");
    tauri::async_runtime::spawn(async move {
        signals.next().await;
        tracing::warn!("second signal; exiting without waiting");
        std::process::exit(EXIT_INTERRUPTED);
    });
    shutdown(shell, project).await;
    Ok(0)
}

/// Close the listener, stop every job (waiting a while for them), keep an untitled project that
/// holds work for recovery.
async fn shutdown(shell: &Shell, project: &Arc<Mutex<Project>>) {
    mcp::stop(Duration::from_secs(3)).await;
    shell.state::<AppState>().stop_all();
    kerf_core::proxy::cancel_all();
    let left = mcp::finish_jobs(Duration::from_secs(10)).await;
    if left > 0 {
        tracing::warn!(jobs = left, "jobs still running at exit");
    }
    let untitled = lock_user(project).recovery_dir().map(Path::to_path_buf);
    recovery::on_exit(shell);
    if let Some(dir) = untitled.filter(|d| d.exists()) {
        tracing::warn!(dir = %dir.display(), "the untitled project's work is kept for recovery");
    }
    tracing::info!("kerf exiting");
}

/// SIGINT and SIGTERM (Ctrl+C on Windows), listened for from the moment this is made.
struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
}

impl Signals {
    fn new() -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                ctrl_c: tokio::signal::windows::ctrl_c()?,
            })
        }
    }

    async fn next(&mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
        #[cfg(windows)]
        self.ctrl_c.recv().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("kerf-app")
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect()
    }

    fn parsed(args: &[&str]) -> Result<Option<Args>, String> {
        parse(&argv(args), Path::new("/work"))
    }

    #[test]
    fn without_headless_the_command_line_is_the_desktop_app_s() {
        assert_eq!(parsed(&[]), Ok(None));
        assert_eq!(parsed(&["cut.kerf"]), Ok(None));
        assert!(!requested(&argv(&["--headlessly"])));
    }

    #[test]
    fn every_flag_is_read_with_a_space_or_an_equals_sign() {
        let args = parsed(&[
            "--headless",
            "--project",
            "cut.kerf",
            "--mcp-addr=127.0.0.1:0",
            "--data-dir",
            "/tmp/kerf",
            "--token-file=token",
            "--show-token",
            "--review",
            "--proxies",
            "--cpu-percent",
            "50",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            args,
            Args {
                project: Some(ProjectArg::Open(PathBuf::from("/work/cut.kerf"))),
                mcp_addr: Some("127.0.0.1:0".to_string()),
                data_dir: Some(PathBuf::from("/tmp/kerf")),
                token_file: Some(PathBuf::from("/work/token")),
                show_token: true,
                review: true,
                proxies: true,
                cpu_percent: Some(50),
                help: false,
            }
        );
        let args = parsed(&["--new-project", "/abs/new.KERF", "--headless"]).unwrap().unwrap();
        assert_eq!(args.project, Some(ProjectArg::Create(PathBuf::from("/abs/new.KERF"))));
        assert_eq!(parsed(&["--headless"]).unwrap().unwrap(), Args::default());
        assert!(parsed(&["--headless", "-h"]).unwrap().unwrap().help);
    }

    #[test]
    fn a_bad_command_line_says_what_is_wrong() {
        let err = |args: &[&str]| parsed(args).unwrap_err();
        assert!(err(&["--headless", "--project"]).contains("--project needs a value"));
        assert!(err(&["--headless", "--wat"]).contains("unknown argument \"--wat\""));
        assert!(err(&["--headless", "cut.kerf"]).contains("unknown argument"));
        assert!(err(&["--headless", "--project", "a.kerf", "--new-project", "b.kerf"]).contains("one --project"));
        assert!(err(&["--headless", "--project", "a.mp4"]).contains("end in .kerf"));
        assert!(err(&["--headless", "--mcp-addr", "nowhere"]).contains("not an address"));
        assert!(err(&["--headless", "--cpu-percent", "101"]).contains("from"));
        assert!(err(&["--headless", "--cpu-percent", "1"]).contains("from"));
        assert!(err(&["--headless", "--review=yes"]).contains("takes no value"));
    }

    #[test]
    fn a_project_to_open_must_exist_and_one_to_create_must_not() {
        let dir = std::env::temp_dir().join(format!("kerf-headless-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let there = dir.join("there.kerf");
        std::fs::write(&there, b"").unwrap();
        let missing = dir.join("missing.kerf");
        let check = |project| {
            check_args(&Args {
                project: Some(project),
                ..Args::default()
            })
            .err()
            .map(|Exit(code, message)| (code, message))
        };
        assert!(check(ProjectArg::Open(there.clone())).is_none());
        assert!(check(ProjectArg::Create(missing.clone())).is_none());
        let (code, message) = check(ProjectArg::Open(missing)).unwrap();
        assert_eq!(code, EXIT_USAGE);
        assert!(message.contains("does not exist"), "{message}");
        assert!(check(ProjectArg::Create(there)).unwrap().1.contains("already exists"));
        let dir_as_file = check_args(&Args {
            token_file: Some(dir.clone()),
            ..Args::default()
        });
        assert!(dir_as_file.is_err_and(|Exit(code, _)| code == EXIT_USAGE));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_the_user_s_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kerf-headless-{}", uuid::Uuid::new_v4().simple()));
        let file = dir.join("token");
        settings::write_atomic(&file, b"secret").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"secret");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }
}
