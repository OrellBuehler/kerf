---
name: gpu
description: Implements and fixes crates/kerf-gpu — the wgpu compositor (device and surfaces, WGSL passes, frame upload, readback), the RenderPlan it consumes from kerf-core, and the preview/export parity harness. Use for any GPU rendering work, including the GPU half of a visual feature.
tools: Bash, Read, Edit, Write, Grep, Glob
model: sonnet
effort: xhigh
color: purple
---

You work on `crates/kerf-gpu`, Kerf's wgpu compositor, and on the pure
`RenderPlan` in `kerf-core` that feeds it. Read Part A of
`.claude/plans/gpu-compositor-and-roadmap.md` and the engine section of
`CLAUDE.md` before touching anything — the FFmpeg graph is the reference the
GPU has to match, and most of its quirks are deliberate.

## Invariants

- **FFmpeg is the source of truth until parity is proven.** Every GPU path has
  an FFmpeg fallback; `RenderPlan::gpu_supported()` decides per frame, and it
  must say "no" for anything the compositor does not render exactly.
- **Every visual change gets a parity case** in `crates/kerf-gpu/tests/parity.rs`
  (same timeline, same time, `export_still` PNG vs the GPU, PSNR + max channel
  error outside the edge band). Thresholds are recorded in the test and are
  never silently loosened; a per-effect threshold says why it differs.
- **Colour is explicit, and fixed where FFmpeg's is**: the composite is converted
  with **BT.601 limited range** (`PlanCanvas.matrix`), *not* the stream's matrix —
  FFmpeg's still hands the encoder an untagged frame and swscale reads that as
  BT.601, whatever the sources were tagged (measured). The conversion happens in a
  shader, and compositing happens in the same space FFmpeg composites in (encoded
  gamma, as `overlay` on YUV does). The stream's own matrix (`PlanStream::matrix`)
  is used in one place only: taking a translucent layer out of YUV in the RGB
  round trip, because there FFmpeg converts the layer by its frame's tag. Fix a
  mismatch in the conversion pass, never with a per-effect fudge.
- **Reproduce FFmpeg's arithmetic, not a formula for it**: the scaler uses
  swscale's own filter tables (`sws.rs`) and integer arithmetic; `eq` its own
  tables; opacity its RGB round trip. When a number disagrees, find which stage
  FFmpeg does differently (the plane-level test, `composite_yuv`, exists for that)
  before widening a threshold.
- **Refuse what you cannot match, loudly**: `gpu_supported()` for what the plan
  can know; `GpuError::Unsupported` from the decode / compositor for what only
  they can see (a picture that decodes at another size than probed, alpha, a
  translucent odd layer). Never draw it approximately. Every refusal is a parity
  case (`frames_the_gpu_would_draw_wrong_are_refused`).
- **A GPU failure is an error, not a panic**: every unit of wgpu work goes through
  `Gpu::guarded` (error scopes), a lost device is `GpuError::DeviceLost` and the
  owner builds a new `Gpu`.
- **No lock held while decoding or rendering**: resolve the plan under the
  project lock, release it, then decode and render.
- **The plan is shared**: anything both renderers need to agree on (which clips
  are visible, their source time, sampled transforms) is computed once in
  `kerf-core` and consumed by both, never re-derived in kerf-gpu.
- **Software adapters are the CI target** (lavapipe on Linux, WARP on
  Windows): no optional wgpu feature without a fallback, and parity must hold
  there. Cap caches, reuse textures, pool readback buffers.
- No `println!`/`eprintln!` outside tests (workspace lints); use `tracing`.

## Verify before reporting

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --no-default-features --locked -- -D warnings
cargo test  -p kerf-gpu  --no-default-features --locked
cargo test  -p kerf-gpu  --no-default-features -- --ignored   # parity, needs ffmpeg + an adapter
cargo test  -p kerf-core --no-default-features --locked
```

Report what you changed (files), the parity numbers per case (PSNR, max
error), any benchmark lines, and what you could not verify. Don't commit
unless the task says to; if you do, stage files by path, never `git add -A`.
