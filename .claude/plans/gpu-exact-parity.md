# GPU ↔ FFmpeg bit-exact parity — orchestrator plan

TL;DR: The comparison suite already exists (`kerf-gpu/tests/parity.rs` 822 rows, `motion_parity.rs`, `coverage.rs`, `selftest.rs`, CI matrix on FFmpeg 6.1.1 + pinned 9.0.2 on lavapipe). It is not bit-exact: stills are judged in RGB at PSNR ≥ 40 dB / max 8 levels outside an edge band, and motion allows a 0/1/2-level ladder. This plan has one orchestrator session drive three roles — **implementer**, **parity-checker**, **reviewer** — first to make the suite bit-exact (0 differing samples, nothing excluded), then to win back and extend GPU coverage one `Unsupported` reason at a time. FFmpeg's FATE suite is not reusable as an oracle; only its sample corpus and ideas are.

## Context

Goal: move rendering from FFmpeg to the wgpu compositor for speed, with **output identical to FFmpeg, sample for sample** (user decision: bit-exact only — anything not exact is refused and FFmpeg draws it). Performance gain = share of frames the compositor draws (`coverage.rs`), never bought with tolerance.

Today:
- `parity.rs` (Still plans): RGB PNG vs GPU, `PSNR_FLAT_MIN` 40, `MAX_FLAT` 8, edge band excluded, 30 dB floor for rotated.
- `motion_parity.rs` / `playback.rs` (export frames): YUV planes, bounds `0` / `SCALED` 1 / `EFFECT` 1 / `CASCADED`, `EFFECT_SCALED`, `ZOOMED_MASK`, `GRADED_SCALED`, `WHITE` 2 / `WHITE_709` 3, rotated band of 3 px excluded.
- `selftest.rs` copies `parity.rs`' `compare` and constants (held equal by a test) and runs on the user's machine.
- Refusal catalogue: `Unsupported` in `crates/kerf-core/src/plan_caps.rs:226`; caps in `Compositor::caps()` / `GpuCaps`; runtime kernel probe pattern in `grade::kernels()` (`Match` / `Differs` / `Unknown`).
- Known non-exact sources: swscale x86 SIMD vertical scaler vs C (1 level, up to 5 at 40:1), `gblur` SIMD summation order (1), rotate bilinear edge (band), RGB round trip chroma "within a level", final YUV→RGB truncation bias (1–3 levels on 9.0.2), white kernel (2–3).
- CI tests x86 only; the macOS bundle's arm64 FFmpeg (NEON) is untested — exactness there can only be proven at runtime.

## FFmpeg's own tests (answer)

- **FATE** (`tests/fate/*.mak`, refs in `tests/ref/fate/` as `framecrc` / md5) checks FFmpeg against its own past output for one build. Kerf's oracle — running the user's actual FFmpeg — is strictly stronger, so the refs add nothing.
- **Reusable**: the **fate-suite sample corpus** (`rsync://fate-suite.ffmpeg.org/fate-suite/`, mixed licences — download at test time, never vendor) as a hostile decode corpus for `FrameSource` / probe / y4m; **`tests/fate/filter-video.mak`** as a checklist of parameter combinations per filter; **`tests/checkasm`** as the model for the SIMD-vs-C problem (it proves why the 1-level differences exist).
- **Not reusable as code**: FFmpeg is LGPL/GPL, Kerf is PolyForm Noncommercial — port arithmetic from reading the source (as `sws.rs` already does), never copy files.

## Roles (subagents)

| Role | Agent | Writes | Never |
|---|---|---|---|
| Orchestrator | main session | WP specs, ledger, progress log, merges | code |
| Implementer | existing `gpu` (+ `engine` for `plan_caps.rs` / `render_plan.rs` / `planner.rs`) | shaders, `src/`, plan caps, own parity cases, docs | loosen a bound, add a tolerance or exclusion band |
| Parity-checker | **new** `.claude/agents/parity.md` (Bash, Read, Edit, Write, Grep, Glob; sonnet, xhigh) | test files only (`crates/kerf-gpu/tests/**`, `kerf-core` plan tests) | edit `src/`, shaders, or the implementer's cases; see the implementer's reasoning |
| Reviewer | existing `reviewer` (read-only) + GPU checklist | findings | — |
| Verifier | existing `verifier` (read-only) | pass/fail + output | — |

Parity-checker brief (in `parity.md`), per WP:
1. Read the FFmpeg graph the feature produces (`engine/cli.rs`) and the C source of every filter in it for **both** builds (6.1.1, 9.0.2); enumerate branches, rounding points, SIMD variants, parameter edges.
2. Write **adversarial cases** independently of the implementer (`checker/<wp>/…` family): parameter extremes, odd sizes / offsets, every matrix (601/709/2020/untagged/label), range (tv/pc flag/yuvj), pixel format, keyed vs static, inside fades / dissolves / dips / slides, over another track, both Still and Motion plans.
3. Run on both FFmpegs (`/usr/bin/ffmpeg` 6.1.1 and the pinned build via `bun scripts/fetch-ffmpeg.mjs x86_64-unknown-linux-gnu`, `KERF_FFMPEG=…`), lavapipe.
4. **Mutation check**: perturb one constant / rounding in the new shader (scratch copy, reverted), confirm ≥1 case fails.
5. **Refusal necessity**: for each refusal kept or added, draw the frame anyway (`drawn_anyway` pattern) and confirm it is not exact — otherwise the refusal is too wide.
6. Report `coverage.rs` delta per archetype.
Verdict: `PASS` or `FAIL` with case name, plane, max diff, first differing (x, y), montage path.

Reviewer checklist addition: no new tolerance constant, no exclusion band, no refusal silently widened, existing argv byte-identical (golden), lock / timeout invariants, caps gating per FFmpeg build and per runtime probe, docs updated.

## Hard rules

- **Bit-exact**: acceptance is 0 differing samples in every plane (and RGB for Still plans), whole frame, every frame of the range, both FFmpeg builds. Not exact → `Unsupported` + FFmpeg fallback.
- Exactness that depends on the machine (CPU SIMD path, FFmpeg build) is gated by a **runtime kernel probe** (generalise `grade::kernels()`: `Match` / `Differs` / `Unknown`, `Unknown` holds back) and the self-test; never keyed on a version string.
- Existing export graphs stay byte-identical, except decision D1 below if approved.
- Engine first, then surfaces; every `.claude/docs/gpu.md` / `core-render-plan.md` change in the same PR; commits lowercase imperative, no AI mention; one worktree per WP; ≤3 WPs in flight, never two on `parity.rs` / `motion_parity.rs` / `plan_caps.rs` at once (sequence them).

## Decisions taken with this plan

- **D1 — deterministic scaler.** Measure whether adding `accurate_rnd+bitexact` to the export/still `scale` flags makes swscale's SIMD output equal its C output on both builds and arm64, and its export-time cost. If equal and cost < 5 %: adopt (one golden family re-blessed, recorded), and the shader matches the C path exactly. Otherwise: runtime probe per machine, refuse where it differs. *(Changes existing exports by ≤1 level — this is the one exception to byte-identical graphs; approving this plan approves the measurement and adoption under those criteria.)*
- **D2** — `gblur`: same pattern; no flag exists, so runtime probe + refuse unless the shader's summation order equals this machine's.
- **D3** — FATE samples are fetched by an `#[ignore]`d test into `target/`, never committed.

## Work packages

### Phase 0 — make the suite bit-exact (no new GPU features)
- **WP0.1 shared exact comparator** — `tests/common/exact.rs`: plane + RGB comparison, 0 tolerance, report count / max / first diff / montage; replace `compare` in `parity.rs`, the ladder in `motion_parity.rs` / `playback.rs` / `grade.rs` / `text.rs`, and `selftest.rs` (keep the "self-test equals harness" test). Remove edge band, `BUSY`, rotated band.
- **WP0.2 demote** — run the exact suite on both builds; every case that now fails gets its cap turned off / a new `Unsupported` reason (e.g. `ScalerNotExact`, `BlurNotExact`, `RotateEdge`, `WhiteKernel`, `RoundTripChroma`) so main is green and every drawn frame is exact. Record the coverage drop.
- **WP0.3 ledger** — `crates/kerf-gpu/tests/ledger.rs` (or a `coverage.rs` mode) emits a table: `Unsupported` variant × FFmpeg build → refused / exact, plus frames-drawn per archetype; orchestrator keeps it in `.claude/plans/gpu-parity-progress.md`. This is the backlog and the performance metric.
- **WP0.4 corpus** — FATE-sample decode test (`#[ignore]`d) for `FrameSource` / cursor / probe hostile input; add more archetypes to `coverage.rs` (real-world project shapes).
- **WP0.5 parity agent** — add `.claude/agents/parity.md` and the reviewer checklist; update `.claude/docs/gpu.md` and CLAUDE.md agent list.

### Phase 1 — win back exactness (highest coverage first)
- Final YUV→RGB conversion truncation (Still plans, 9.0.2).
- Scaler: D1, then shader matches the chosen path; runtime probe.
- RGB round trip chroma (`roundtrip.wgsl` vs swscale pair sums + vertical bicubic).
- Rotate bilinear edges (port `vf_rotate` integer interpolation incl. edge chroma).
- `gblur` (D2), white kernel, cascaded zoom, zoomed mask edges, graded+scaled.
Each: implementer → checker → reviewer, caps re-enabled only where checker passes on both builds.

### Phase 2 — lift refusals, ordered by ledger frames gained
Candidates from `plan_caps.rs`: `FoldedFit`, `KeyedPlacement` / `StillMotion` / `KeyedCrop` (`perspective`), `ZoomBehind`, `FadeOnTranslucent`, `DipToWhite` variants, `FormatResize` / `Shrink` (10-bit, 4:2:2, 4:4:4), full-range family, `BlendMode`, `BlurFill`, `SpeedRamp`, `Tail`, `TextTooSmall` / `TextBorder`, `Hdr` tone map, `Svg`, `Gif` / `Delivery`, `Stabilized`, v360. One WP = one reason (split if > ~1,500 lines). Orchestrator re-ranks after every merge.

### Phase 3 (follow-on, separate approval) — A7 export through the compositor, gated on the ledger showing 100 % exact on the reference projects.

## Loop per WP (orchestrator)

1. Pick top ledger item; write a ≤1-page spec: FFmpeg filters + source files for both builds, current refusal, target frames, expected coverage gain.
2. Worktree `../kerf-<wp>` off `origin/main`, branch `feat/gpu-exact-<wp>`.
3. **Implementer** (`gpu`, plus `engine` for plan changes): implement, own parity cases at 0, docs.
4. **Verifier**: `prek run --all-files`, pre-push stage, `cargo test -p kerf-core --no-default-features`, `cargo test -p kerf-gpu --no-default-features -- --ignored` on both FFmpegs.
5. **Parity-checker** (fresh context; gets spec + diff only). `FAIL` → findings to implementer; ≤3 rounds, then narrow: refuse the failing sub-case with a new reason + `drawn_anyway` case, record why.
6. **Reviewer** on `git diff origin/main...HEAD`; fix confirmed findings, re-verify.
7. Push (only when ready), PR, CI `parity` matrix green, merge, update ledger + progress log, remove worktree.
Never ask the user mid-loop; record decisions in the progress log. Same CI failure 3× → mark blocked, continue.

## Critical files

- `crates/kerf-gpu/tests/{parity,motion_parity,playback,grade,text,coverage}.rs`, `tests/common/mod.rs`
- `crates/kerf-gpu/src/{selftest,sws,roundtrip,grade,compositor}.rs`, `src/shaders/*.wgsl`
- `crates/kerf-core/src/{plan_caps,render_plan,planner}.rs`, `engine/cli.rs` (D1 only), `engine/cli/golden.rs`
- `.github/workflows/ci.yml` (`parity` job), `scripts/fetch-ffmpeg.mjs`
- `.claude/agents/{gpu,reviewer,verifier}.md`, new `parity.md`; `.claude/docs/gpu.md`, `core-render-plan.md`

## Verification

- Per WP: both FFmpeg builds, lavapipe, 0 differing samples in every drawn frame; mutation check fails a case; refusal-necessity cases pass; golden oracle unchanged (or D1's single blessed family).
- Phase 0 done when: no tolerance / band constant remains in `kerf-gpu/tests` or `selftest.rs`, CI green, ledger published.
- Each later WP: ledger shows frames-drawn up, zero exactness regressions; `KERF_BENCH=1 … bench` line recorded.
- Self-test on a real GPU / arm64 Mac listed under "Needs a real machine" in the progress log.
