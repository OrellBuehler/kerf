# Build identity, local checks, CI and releases

```

**Debug builds have their own identity.** `tauri.dev.conf.json` (next to
`tauri.conf.json`) sets `identifier` to `ch.orellbuehler.kerf.dev`, which names the
app's config and log directories and the single-instance lock — so a dev run
neither rewrites an installed Kerf's `settings.json` nor refuses to start while
that one is open. Tauri resolves its config at compile time (`tauri_build` and
`generate_context!` both merge the `TAURI_CONFIG` JSON env var over the file), and
`cargo run` has no CLI to set it, so `crates/kerf-app/build.rs` does: when cargo's
`PROFILE` is `debug` it merges `tauri.dev.conf.json` into `TAURI_CONFIG` (keys the
CLI already set win) and passes the result to rustc via `cargo:rustc-env`. That
covers `cargo run`, `cargo test` and `tauri dev` alike with no extra flag;
release builds never see it. A debug-profile build is therefore *not* the
shipping identifier — to test that, build `--release`.

### Local checks (prek) and the agent harness

`.pre-commit-config.yaml` is the single definition of "the checks", run by
[prek](https://prek.j178.dev) (`prek install --install-hooks` once per clone):
the **commit** stage is hygiene, `typos` (allowlist in `_typos.toml`),
`actionlint`, `zizmor`, `cargo fmt`, `svelte-check` (fails on warnings) and
`bun test`; the **push** stage adds clippy `-D warnings` and the kerf-core tests;
`commit-msg` enforces the lowercase-imperative subject and rejects AI
attribution trailers. `prek run --all-files [--hook-stage pre-push]` runs them by
hand. CI's `lint (prek)` job runs the commit stage (skipping the hooks that have
their own job), and `ci ok` is one status that is green only when every CI job is. Every
workflow installs Ubuntu packages through `.github/actions/apt-install` (per-request
timeouts, apt retries, each command bounded by `sudo timeout -k` and the whole tried
three times) — a hung or trickling mirror connection otherwise sat until the job's
timeout and read as a cancelled job. The `timeout` goes *inside* `sudo` with a KILL
follow-up: apt-get outlives a SIGTERM mid-download, and sudo does not relay a KILL.
A retry drops `azure.archive.ubuntu.com` from the runner's `apt-mirrors.txt` (the
mirror that was stalling) and goes through the rest of the list.
Rust lints are `[workspace.lints]` in the root `Cargo.toml` (no `dbg!`/`todo!`/
`println!`, justified `unsafe`, a few style lints) — every crate opts in with
`[lints] workspace = true`.

`.claude/` carries the shared agent setup: `settings.json` (an allowlist for the
check commands, and a PostToolUse hook that rustfmt's every `.rs` file an agent
writes, reporting parse errors back) and project subagents in `.claude/agents/`
— `engine` (kerf-core), `frontend`, `surface` (wire a core op into the Tauri
command + MCP tool + api.ts), `gpu` (kerf-gpu and its parity harness), and the
read-only `reviewer` and `verifier`.


**Auto-update.** The app updates itself from its own GitHub releases via
`tauri-plugin-updater` (+ `tauri-plugin-process` for the relaunch), both
registered in `run()`. `plugins.updater` in `tauri.conf.json` points at
`https://github.com/OrellBuehler/kerf/releases/latest/download/latest.json`
and embeds the **minisign public key**: a bundle only installs if its signature
verifies against that key, so the update path is not just "trust whatever the
URL serves". `bundle.createUpdaterArtifacts` makes `tauri build` emit the
updatable bundles (`.app.tar.gz` / `.AppImage` / NSIS `-setup.exe`) plus a
`.sig` per bundle — which means **a bundle build now needs the private key**
(`TAURI_SIGNING_PRIVATE_KEY`, or `TAURI_SIGNING_PRIVATE_KEY_PATH`, plus
`…_PASSWORD`) in the environment; plain `cargo build` / CI is unaffected.
`release.yml` passes those from repo secrets, and a **separate
`updater-manifest` job** assembles `latest.json` from the uploaded `.sig` files
*after* all bundles land (`includeUpdaterJson: false` on the build step): the
per-platform jobs run concurrently and each writing the manifest would leave
only whichever finished last. It runs under `!cancelled()`, not on plain
success — the matrix is `fail-fast: false`, and one platform failing must not
leave the release with no manifest at all, which would 404 the feed for
*everyone*. Prereleases are skipped, so they never become the update everyone is
offered.
A published manifest is then **read back** from `releases/latest/download/`
and the run fails if any of the five platform keys is missing — a partial
manifest still ships (better than none), but no longer silently. Nothing is
built until `ci-green` has seen a successful CI run on the tagged commit (it
polls, because a release is usually published while CI on the merge commit is
still running), and `attest` adds a build-provenance attestation to every
installer. Only `v*` tags run it: a release that just hosts files (`models-v1`, the
Demucs model) skips every job. **`prepare-release.yml`** (`workflow_dispatch`, input `version`) does
the release PR's edits: the three version fields, `cargo update --workspace`,
and `fetch-ffmpeg.mjs --repin`, which moves the FFmpeg pins to the newest
upstream builds and rewrites the script's digests. CI's `engine` job runs the
`#[ignore]`d binary tests against both the distro FFmpeg and the **pinned**
one on Linux, Windows and macOS, and runs weekly, so a pruned BtbN pin shows
up before a release needs it; the `parity` job is the same pair on Linux (the
GPU compositor against each FFmpeg, on lavapipe); a `libav` job compiles the `ffmpeg` /
`libav-render` features against the Ubuntu dev libraries.

**Publishing a release would open a gap in the feed**, so the workflow closes it:
the new tag becomes `releases/latest` the moment it is published, but its
`latest.json` is only attached ~25 min later when the slowest bundle (Windows)
finishes — `releases/latest/download/latest.json` would 404 until then and every
running install's check fail with the plugin's `Could not fetch a valid release
JSON`. A **`hold-release` job** (first in `release.yml`, no `needs:`, so it lands
seconds after the publish event) marks the release a *prerelease*, which parks
`releases/latest` on the previous version whose manifest is intact — a check
during the build says "up to date" instead of erroring — and `updater-manifest`
**promotes it back** (`gh release edit --prerelease=false --latest`) in the same
step that uploads `latest.json`, so `releases/latest` only ever points at a
release that already has its manifest. Both jobs gate on
`!github.event.release.prerelease`, the *event payload*, which the hold's own
edit cannot change — a release cut as a genuine prerelease is skipped by both and
never becomes `releases/latest`. A release that fails outright just stays the
prerelease it was parked as, which is the safe state; `api.ts` still rewrites the
plugin's error into an explanation (`describeFeedFailure`) as a backstop.
**PR builds** (`pr-build.yml`) bundle every non-draft, non-Dependabot PR for
Windows x64 / macOS arm64 / Linux x64 like the release does (bundled FFmpeg,
`--features whisper`) but unsigned, with no updater artifacts and the cargo
cache on, and upload them as 14-day artifacts. `pr-build-comment.yml` keeps one
comment on the PR linking them: it runs on `workflow_run` because a fork's run
only holds a read-only token, so it never checks out PR code, and it accepts the
`pr-number` artifact only when that PR's head is the commit the run built.
`workflow_run` fires only for workflows on the default branch, so a PR that adds
or changes the commenter is not commented on by its own version.
In-place update is per-platform: macOS and Windows
(NSIS, `installMode: passive`) always; on Linux **only the AppImage** — a
`.deb`/`.rpm` install fails the install step, which the dialog reports with a
link to the release page.
