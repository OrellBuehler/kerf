//! `kerf-app --headless`: the MCP server with no window and no webview, for an agent on a machine
//! with no display (CI, a server, a container).
//!
//! No Tauri app is built — that opens a display — so this runs on Tauri's async runtime alone, with
//! a [`Shell::Headless`] in place of the `AppHandle`. stdout carries one JSON line once the server
//! listens (the ready line); logs go to stderr and the log directory. It runs until SIGINT / SIGTERM:
//! the listener closes, every job is stopped, an untitled project with work is kept for recovery.
//!
//! With `--stdio` there is no listener: one MCP session is served over stdin / stdout, which carries
//! JSON-RPC and nothing else, until the client closes stdin (or a signal) and the run stops the same way.

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
/// ... its cache ceiling in GiB (`--cache-max-gb`), and the directory its caches live under (`--cache-dir`).
const ENV_CACHE_MAX_GB: &str = "KERF_CACHE_MAX_GB";
const ENV_CACHE_DIR: &str = "KERF_CACHE_DIR";

const EXIT_FAILURE: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_NOT_LISTENING: i32 = 3;
const EXIT_INTERRUPTED: i32 = 130;

const USAGE: &str = "\
kerf-app --headless [options]

Run Kerf's MCP server with no window. Prints one JSON line on stdout once it listens;
logs go to stderr. Stops on SIGINT / SIGTERM.

  --stdio                  serve MCP on stdin / stdout instead of HTTP (no listener, no token);
                           stops when the client closes stdin
  --tools SETS             with --stdio, list only these tool sets (e.g. core,titles)
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
  --cache-dir DIR          keep the caches (proxies, waveforms, models, ...) in DIR/kerf instead of the
                           system's cache directory (KERF_CACHE_DIR)
  --cache-max-gb N         keep the regenerable caches (proxies, waveforms, filmstrips, stems) under N GiB:
                           pruned at start and every 10 minutes, least recently used first; what the open
                           project uses, what is being written and the downloaded models are never removed
                           (KERF_CACHE_MAX_GB; default: no limit)

The token is KERF_MCP_TOKEN when set (this run only, never stored), else the one in the settings.

Exit codes: 0 stopped by a signal (or stdin closed, with --stdio), 1 failure, 2 bad arguments,
3 the server could not listen.";

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectArg {
    Open(PathBuf),
    Create(PathBuf),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    stdio: bool,
    tools: Option<String>,
    project: Option<ProjectArg>,
    mcp_addr: Option<String>,
    data_dir: Option<PathBuf>,
    token_file: Option<PathBuf>,
    show_token: bool,
    review: bool,
    proxies: bool,
    cpu_percent: Option<u8>,
    cache_dir: Option<PathBuf>,
    cache_max_bytes: Option<u64>,
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

/// A ceiling given in GiB (`--cache-max-gb`, `KERF_CACHE_MAX_GB`) as bytes: a number above 0.
fn parse_cache_max(text: &str) -> Result<u64, String> {
    let gib = text.trim().parse::<f64>().ok().filter(|g| g.is_finite() && *g > 0.0);
    match gib {
        Some(gib) => Ok(((gib * kerf_core::cache_cap::GIB as f64).round() as u64).max(1)),
        None => Err(format!("takes a number of GiB above 0, not {text:?}")),
    }
}

/// What the environment adds to a headless command line: the cache directory and ceiling, each
/// only when its flag was not given. `get` reads a variable (blank is unset).
fn cache_from_env(args: &mut Args, cwd: &Path, get: impl Fn(&str) -> Option<String>) -> Result<(), String> {
    let var = |name: &str| get(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if args.cache_dir.is_none() {
        args.cache_dir = var(ENV_CACHE_DIR).map(|dir| absolute(&dir, cwd));
    }
    if args.cache_max_bytes.is_none() {
        if let Some(text) = var(ENV_CACHE_MAX_GB) {
            args.cache_max_bytes = Some(parse_cache_max(&text).map_err(|e| format!("{ENV_CACHE_MAX_GB} {e}"))?);
        }
    }
    Ok(())
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
            "--stdio" => switch(&mut args.stdio)?,
            "--tools" => {
                let tools = value()?;
                mcp::check_tool_sets(&tools).map_err(|e| format!("--tools: {e}"))?;
                args.tools = Some(tools);
            }
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
            "--cache-dir" => args.cache_dir = Some(absolute(&value()?, cwd)),
            "--cache-max-gb" => {
                let bytes = parse_cache_max(&value()?).map_err(|e| format!("--cache-max-gb {e}"))?;
                args.cache_max_bytes = Some(bytes);
            }
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
    if args.stdio {
        let http_only = [
            ("--mcp-addr", args.mcp_addr.is_some()),
            ("--token-file", args.token_file.is_some()),
            ("--show-token", args.show_token),
        ];
        if let Some((flag, _)) = http_only.into_iter().find(|(_, given)| *given) {
            return Err(format!(
                "{flag} is for the HTTP server: a --stdio run has no listener and no token"
            ));
        }
    } else if args.tools.is_some() {
        return Err("--tools is for --stdio (over HTTP, add ?tools= to the URL)".to_string());
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
    let mut args = match parse(argv, &cwd) {
        Ok(Some(args)) => args,
        Ok(None) => return EXIT_USAGE,
        Err(e) => {
            say(&format!("kerf-app: {e}\n\n{USAGE}"));
            return EXIT_USAGE;
        }
    };
    if args.help {
        // stdout is the client's alone with --stdio.
        if args.stdio {
            say(USAGE);
        } else {
            let _ = writeln!(std::io::stdout().lock(), "{USAGE}");
        }
        return 0;
    }
    if let Err(e) = cache_from_env(&mut args, &cwd, |name| std::env::var(name).ok()) {
        say(&format!("kerf-app: {e}\n\n{USAGE}"));
        return EXIT_USAGE;
    }
    if let Err(Exit(code, message)) =
        use_cache_dir(args.cache_dir.as_deref()).and_then(|()| check_cache_cap(&args, kerf_core::paths::cache_dir()))
    {
        say(&format!("kerf-app: {message}"));
        return code;
    }
    let token = match check_args(&args) {
        Ok(token) => token,
        Err(Exit(code, message)) => {
            say(&format!("kerf-app: {message}"));
            return code;
        }
    };
    let rpc = match args.stdio.then(rpc_out).transpose() {
        Ok(rpc) => rpc,
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
    match run(&args, token, rpc, config, data) {
        Ok(code) => code,
        Err(Exit(code, message)) => {
            tracing::error!("{message}");
            code
        }
    }
}

/// Put the caches under `dir` for this run, before anything reads where they are.
fn use_cache_dir(dir: Option<&Path>) -> Result<(), Exit> {
    let Some(dir) = dir else { return Ok(()) };
    std::fs::create_dir_all(dir).map_err(|e| Exit(EXIT_USAGE, format!("cannot use --cache-dir {}: {e}", dir.display())))?;
    kerf_core::paths::set_cache_dir(dir.to_path_buf());
    Ok(())
}

/// A ceiling needs somewhere to keep the caches: refused up front, before a project is opened,
/// rather than run without the cap the operator asked for.
fn check_cache_cap(args: &Args, cache_dir: Option<PathBuf>) -> Result<(), Exit> {
    if args.cache_max_bytes.is_some() && cache_dir.is_none() {
        return Err(Exit(
            EXIT_USAGE,
            "--cache-max-gb needs a cache directory and this system has none (--cache-dir names one)".into(),
        ));
    }
    Ok(())
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
    if args.stdio {
        return Ok(None);
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

/// Where a `--stdio` run writes JSON-RPC: what fd 1 was, kept for the protocol alone, with fd 1 itself
/// pointed at stderr — so nothing else (a stray print, a native library's, a child process that
/// inherits it) can corrupt the stream.
#[cfg(unix)]
type RpcOut = tokio::fs::File;
#[cfg(not(unix))]
type RpcOut = tokio::io::Stdout;

/// A stdout that is stderr too (`2>&1`) and no terminal: the logs would land in the client's stream.
#[cfg(unix)]
fn rpc_out() -> Result<RpcOut, Exit> {
    use std::io::IsTerminal;
    use std::os::fd::FromRawFd;
    let kept = |e: std::io::Error| Exit(EXIT_FAILURE, format!("cannot keep stdout for the MCP client: {e}"));
    if same_file(libc::STDOUT_FILENO, libc::STDERR_FILENO).map_err(kept)? && !std::io::stdout().is_terminal() {
        return Err(Exit(
            EXIT_USAGE,
            "--stdio needs stdout to itself, and stderr is the same file (2>&1?): send stderr elsewhere".to_string(),
        ));
    }
    // SAFETY: `fcntl` / `dup2` / `close` on this process's own standard descriptors; the duplicate is
    // checked, and from here on owned by the `File` alone.
    unsafe {
        let rpc = libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3);
        if rpc < 0 {
            return Err(kept(std::io::Error::last_os_error()));
        }
        if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
            let e = std::io::Error::last_os_error();
            libc::close(rpc);
            return Err(kept(e));
        }
        Ok(tokio::fs::File::from_std(std::fs::File::from_raw_fd(rpc)))
    }
}

/// Whether the descriptors `a` and `b` are open on the same file (device and inode).
#[cfg(unix)]
fn same_file(a: libc::c_int, b: libc::c_int) -> std::io::Result<bool> {
    let stat = |fd| {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fstat` writes a whole `stat` into the buffer when it returns 0, and only then is
        // it read.
        unsafe {
            if libc::fstat(fd, stat.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(stat.assume_init())
        }
    };
    let (a, b) = (stat(a)?, stat(b)?);
    Ok(a.st_dev == b.st_dev && a.st_ino == b.st_ino)
}

#[cfg(not(unix))]
fn rpc_out() -> Result<RpcOut, Exit> {
    Ok(tokio::io::stdout())
}

fn run(
    args: &Args,
    token: Option<String>,
    rpc: Option<RpcOut>,
    config: Option<PathBuf>,
    data: Option<PathBuf>,
) -> Result<i32, Exit> {
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
    if let Some(cap) = args.cache_max_bytes {
        let open = project.clone();
        // The assets are read under the lock and the lock is let go on return: the pruner stats and
        // deletes with it released. A read that fails skips that pass; it is never an empty project.
        kerf_core::cache_cap::start(cap, move || lock_user(&open).list_assets())
            .map_err(|e| Exit(EXIT_USAGE, format!("--cache-max-gb cannot be kept: {e}")))?;
    }
    std::thread::spawn(kerf_core::fonts::warm);
    std::thread::spawn(crate::warm_transcription_backend);

    let stdio = rpc.map(|rpc| (mcp::start_stdio(shell.clone(), project.clone(), args.tools.as_deref()), rpc));
    let token_source = match (&token, &stored.mcp_token) {
        (Some(_), _) => "env",
        (None, Some(_)) => "settings",
        (None, None) => "generated",
    };
    if stdio.is_none() {
        mcp::set_overrides(mcp::Overrides {
            addr: args.mcp_addr.clone(),
            token,
        });
        mcp::start(shell.clone(), project.clone());
    }
    // Nobody is there to review: the agent's edits land live unless `--review` asks otherwise. For
    // this run only; the stored switch is the desktop app's.
    if !args.review {
        settings::set_agent_review(false);
        lock_user(&project).set_agent_review(false);
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data_dir = %data.as_deref().map(|d| d.display().to_string()).unwrap_or_default(),
        cache_dir = %kerf_core::paths::cache_dir().map(|d| d.display().to_string()).unwrap_or_default(),
        project = %lock_user(&project).path().map(|p| p.display().to_string()).unwrap_or_default(),
        review = settings::agent_review(),
        stdio = stdio.is_some(),
        "kerf starting headless"
    );
    match stdio {
        Some((server, rpc)) => tauri::async_runtime::block_on(serve_stdio(server, rpc, &shell, &project)),
        None => tauri::async_runtime::block_on(serve(args, &shell, &project, token_source)),
    }
}

async fn serve_stdio(server: mcp::Stdio, rpc: RpcOut, shell: &Shell, project: &Arc<Mutex<Project>>) -> Result<i32, Exit> {
    let mut signals = Signals::new().map_err(|e| Exit(EXIT_FAILURE, format!("cannot listen for signals: {e}")))?;
    let stop = tokio_util::sync::CancellationToken::new();
    let serving = server.serve((tokio::io::stdin(), rpc), stop.clone());
    let mut serving = std::pin::pin!(serving);
    say("kerf: MCP server on stdin / stdout");
    let (ended, signalled) = tokio::select! {
        ended = &mut serving => {
            tracing::info!("the MCP session on stdio ended; shutting down");
            (ended, false)
        }
        () = signals.next() => {
            tracing::info!("signal received; shutting down (a second one exits at once)");
            stop.cancel();
            (tokio::time::timeout(Duration::from_secs(3), &mut serving).await.unwrap_or(Ok(())), true)
        }
    };
    tauri::async_runtime::spawn(async move {
        // A first signal during a shutdown that stdin closing began asks for what is under way.
        if !signalled {
            signals.next().await;
            tracing::info!("signal received; already shutting down (a second one exits at once)");
        }
        signals.next().await;
        tracing::warn!("second signal; exiting without waiting");
        std::process::exit(EXIT_INTERRUPTED);
    });
    shutdown(shell, project).await;
    ended.map(|()| 0).map_err(|e| Exit(EXIT_FAILURE, e))
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
            "--cache-dir=caches",
            "--cache-max-gb",
            "0.5",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(
            args,
            Args {
                stdio: false,
                tools: None,
                project: Some(ProjectArg::Open(PathBuf::from("/work/cut.kerf"))),
                mcp_addr: Some("127.0.0.1:0".to_string()),
                data_dir: Some(PathBuf::from("/tmp/kerf")),
                token_file: Some(PathBuf::from("/work/token")),
                show_token: true,
                review: true,
                proxies: true,
                cpu_percent: Some(50),
                cache_dir: Some(PathBuf::from("/work/caches")),
                cache_max_bytes: Some(1 << 29),
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
        for bad in ["0", "-1", "lots", "NaN", "inf", ""] {
            assert!(
                err(&["--headless", "--cache-max-gb", bad]).contains("--cache-max-gb takes a number of GiB above 0"),
                "{bad:?}"
            );
        }
        assert!(err(&["--headless", "--cache-max-gb"]).contains("needs a value"));
        assert!(err(&["--headless", "--cache-dir"]).contains("needs a value"));
    }

    #[test]
    fn the_cache_ceiling_is_a_number_of_gib_in_bytes() {
        assert_eq!(parse_cache_max("50"), Ok(50 << 30));
        assert_eq!(parse_cache_max(" 1.5 "), Ok(3 << 29));
        assert_eq!(parse_cache_max("0.000000001"), Ok(1));
        assert!(parse_cache_max("0").is_err());
    }

    #[test]
    fn the_environment_fills_in_the_cache_flags_only_where_there_are_none() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_string())
        };
        let cwd = Path::new("/work");
        let mut args = Args::default();
        cache_from_env(&mut args, cwd, env(&[])).unwrap();
        assert_eq!(args, Args::default(), "nothing set, nothing changes");
        cache_from_env(&mut args, cwd, env(&[(ENV_CACHE_MAX_GB, "  "), (ENV_CACHE_DIR, "")])).unwrap();
        assert_eq!(args, Args::default(), "blank is unset");

        cache_from_env(
            &mut args,
            cwd,
            env(&[(ENV_CACHE_MAX_GB, "20"), (ENV_CACHE_DIR, "/mnt/volume/cache")]),
        )
        .unwrap();
        assert_eq!(args.cache_max_bytes, Some(20 << 30));
        assert_eq!(args.cache_dir, Some(PathBuf::from("/mnt/volume/cache")));

        let mut flagged = Args {
            cache_max_bytes: Some(1 << 30),
            cache_dir: Some(PathBuf::from("/flag")),
            ..Args::default()
        };
        cache_from_env(&mut flagged, cwd, env(&[(ENV_CACHE_MAX_GB, "20"), (ENV_CACHE_DIR, "/env")])).unwrap();
        assert_eq!(flagged.cache_max_bytes, Some(1 << 30), "the flag wins");
        assert_eq!(flagged.cache_dir, Some(PathBuf::from("/flag")));

        let mut relative = Args::default();
        cache_from_env(&mut relative, cwd, env(&[(ENV_CACHE_DIR, "cache")])).unwrap();
        assert_eq!(relative.cache_dir, Some(PathBuf::from("/work/cache")));

        let err = cache_from_env(&mut Args::default(), cwd, env(&[(ENV_CACHE_MAX_GB, "plenty")])).unwrap_err();
        assert!(err.contains("KERF_CACHE_MAX_GB takes a number of GiB above 0"), "{err}");
    }

    #[test]
    fn stdio_takes_its_own_flags_and_refuses_the_http_ones() {
        let args = parsed(&[
            "--headless",
            "--stdio",
            "--tools",
            "core,titles",
            "--project=cut.kerf",
            "--review",
        ])
        .unwrap()
        .unwrap();
        assert!(args.stdio && args.review);
        assert_eq!(args.tools.as_deref(), Some("core,titles"));
        assert_eq!(args.project, Some(ProjectArg::Open(PathBuf::from("/work/cut.kerf"))));
        assert!(parsed(&["--headless", "--stdio", "--tools=all"]).is_ok());
        let err = |args: &[&str]| parsed(args).unwrap_err();
        for http in [
            &["--mcp-addr", "127.0.0.1:0"][..],
            &["--token-file", "token"],
            &["--show-token"],
        ] {
            let mut argv = vec!["--headless", "--stdio"];
            argv.extend_from_slice(http);
            assert!(err(&argv).contains("no listener and no token"), "{http:?}");
        }
        assert!(err(&["--headless", "--tools", "core"]).contains("--tools is for --stdio"));
        assert!(err(&["--headless", "--stdio", "--tools", "core,colour"]).contains("`colour` is no tool set"));
        assert!(err(&["--headless", "--stdio=yes"]).contains("takes no value"));
    }

    #[cfg(unix)]
    #[test]
    fn a_descriptor_is_the_same_file_as_its_duplicate_and_not_another() {
        use std::os::fd::AsRawFd;
        let dir = std::env::temp_dir().join(format!("kerf-headless-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let one = std::fs::File::create(dir.join("one")).unwrap();
        let again = std::fs::File::open(dir.join("one")).unwrap();
        let other = std::fs::File::create(dir.join("other")).unwrap();
        assert!(same_file(one.as_raw_fd(), again.as_raw_fd()).unwrap());
        assert!(!same_file(one.as_raw_fd(), other.as_raw_fd()).unwrap());
        assert!(same_file(-1, one.as_raw_fd()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_stdio_run_takes_no_token_from_the_environment() {
        let args = Args {
            stdio: true,
            ..Args::default()
        };
        assert!(matches!(check_args(&args), Ok(None)));
    }

    #[test]
    fn a_cache_ceiling_without_a_cache_directory_is_refused() {
        let capped = Args {
            cache_max_bytes: Some(1 << 30),
            ..Args::default()
        };
        let Err(Exit(code, message)) = check_cache_cap(&capped, None) else {
            panic!("a cap with nowhere to keep the caches must not start");
        };
        assert_eq!(code, EXIT_USAGE);
        assert!(
            message.contains("--cache-max-gb") && message.contains("--cache-dir"),
            "{message}"
        );
        assert!(check_cache_cap(&capped, Some(PathBuf::from("/cache"))).is_ok());
        assert!(check_cache_cap(&Args::default(), None).is_ok(), "no cap asked, none needed");
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
