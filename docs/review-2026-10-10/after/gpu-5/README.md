# GPU-5: scrub at GPU speed, plus hardening

What changed, in one paragraph: a preview frame on the GPU path used to take the project lock, parse the
working timeline from its JSON, read every asset and ask the disk for each one's proxy, **for every frame of
a drag**, hold the render lock across the decode wait, and be asked for by a page that waited for each answer
before asking again. It now plans from an `Arc` snapshot of the project (stale only when a process-wide plan
epoch moved, or after 2 s), takes the pointer's positions through a latest-wins mailbox, keeps the render lock
out of the decode, presents a drag's frames without waiting for the refresh, prefetches the cuts behind the
playhead as well as the ones ahead, refuses CPU rasterisers for the preview unless told otherwise, checks the
compositor against the machine's own FFmpeg before it draws a frame the user sees, and counts why frames went
through FFmpeg.

## Method

- Machine: 4 shared cores, load average 5 to 13 from other agents the whole time. Absolute numbers are
  noisy and pessimistic; three runs per FFmpeg are stored so the spread shows. Adapter: Mesa lavapipe
  (llvmpipe, LLVM 20.1.2, 256-bit), no real GPU, no display.
- FFmpeg builds: system `6.1.1-3ubuntu5` and the pinned `n9.0.2-25-g67b60c310b-20261010`
  (`crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu`); see `ffmpeg-versions.txt`.
- Benchmarks are `#[ignore]`d tests in `crates/kerf-app/src/gpu_preview/bench_tests.rs`, release build of
  `kerf-app`, taken on this branch before origin/main (GPU-4's compositor work) was merged into it, which none
  of these timings depend on; both sides of every comparison come from the same binary in the same run:
  `KERF_BENCH=1 KERF_BENCH_OUT=<dir> cargo test -p kerf-app --no-default-features --release --lib -- --ignored --nocapture a_drag_through_the_frame_path the_inputs_of_a_frame`
- `the_inputs_of_a_frame` times what a frame needed from the project, old way and new way, on three
  generated projects (2, 12 and 40 assets). `a_drag_through_the_frame_path` pushes a 60-position forward drag
  through the real frame path (`GpuPreview::post` -> mailbox -> real device, compositor and frame source ->
  a recording screen) over 1280x720 all-intra clips, in four shapes: an 800 px picture frame where the
  screen waits for the GPU (lavapipe's raster is the frame), the same where it does not (CPU side only, as a
  real present does not wait), a 400 px frame over a gap in the cut (nothing to decode: the cheapest frame
  the path draws), and the same in a project of 120 clips on four tracks.
- "Before" rows model the old behaviour inside the new binary: the page's single flight (the newest position
  once the last answer is in, 3 ms for the IPC round trip) and the inputs taken from the project for every
  frame. "After" rows are the pointer into the mailbox with the inputs held.

## 1. What a frame needs from the project (median / p95 of 3000, microseconds)

Three runs in `inputs-run{1,2,3}.txt`; `inputs-with-rebuild-run{1,2}.txt` is the later version of the bench, which
also times a stale snapshot being rebuilt.

| project | before: lock + proxy waits + timeline JSON parse + assets | after: held snapshot + proxy wait | a stale snapshot rebuilt |
| --- | --- | --- | --- |
| 2 assets, 8 clips (4 KB JSON) | 42.9 to 43.1 / 65.8 to 71.7 | 0.13 / 0.16 to 0.17 | 39.8 / 70 to 75 |
| 12 assets, 120 clips (55 KB) | 201 to 216 / 280 to 410 | 0.14 to 0.15 / 0.28 to 0.43 | 196 to 199 / 332 to 349 |
| 40 assets, 480 clips (221 KB) | 686 to 710 / 895 to 1224 | 0.19 to 0.24 / 0.57 to 0.89 | 688 to 722 / 1070 to 1309 |

The old per-frame cost grew with the project (the timeline is parsed from JSON every time); the new one does
not. The rebuild costs what the old way cost for every frame, and now happens once per edit (the epoch moves)
or per 2 s. `snapshot.rs` has the epoch/TTL rules; `drive_tests::an_edit_makes_the_next_frame_take_the_inputs_again`
holds that an edit makes the next frame take them again.

In the drag bench, the 120-clip gap frame (the cheapest frame, so the inputs are the largest share) goes from
4.2 / 3.8 ms to 2.6 / 2.9 ms per drawn frame (FFmpeg 6.1.1 / 9.0.2, steady rows); the inputs alone account for
0.3 ms of that and the rest is noise and cache effects, so the micro-bench above is the figure to trust.

## 2. A drag through the frame path

`drag-table.md` has every row with the min to max of three runs per FFmpeg; the raw output is
`drag-ffmpeg-<build>-run<n>.txt`. `drag-input-timing-run.txt` is a later single run that also prints the
time spent getting the inputs.

What the lavapipe numbers show:

- **A frame with a picture costs 65 to 100 ms** at 800 px (the CPU rasteriser is the frame; decode is 2 ms,
  recording 1 ms), against **2.6 to 5.0 ms for a frame with nothing to decode**. The GPU path is therefore only a
  win on a real GPU, which is why it refuses CPU adapters by default (below).
- **At a pointer rate of 125 Hz or 60 Hz, mailbox and single-flight draw the same number of frames** on picture
  frames (5 to 8 of 60 at 125 Hz, 9 to 18 at 60 Hz) and all 60 on gap frames. A frame takes longer than
  the 3 ms the single-flight page waits, so waiting for the answer cost it nothing here. The p50 request-to-screen
  figures differ between the two by less than the spread between runs (the 125 Hz row of the mailbox is 204 ms on
  6.1.1 and 99 ms on 9.0.2 in the same table). **No improvement is claimed from these rows.**
- **Inputs taken per drag: 5 to 15 before, 1 after** (60 per drag in the gap shapes, where every position is
  drawn); the steady rows show 60 against 1 to 4.

What they cannot show, and needs a real machine: the benefit the mailbox exists for is the dead time between
two frames (a WebKit IPC round trip per frame in the page, on a frame that takes 2 to 5 ms on a real GPU, not 90),
and that the position the user stops on is always the last one drawn. `AutoNoVsync` has no effect on lavapipe
(nothing waits for a refresh); `x11.rs`' present test now switches a surface to no-vsync and back on an Xvfb with
lavapipe (the surface has a mode that skips the wait, and the frame still reaches the X server), which proves the
switch works on a real surface, not that it feels better.

## 3. The runtime self-test on lavapipe

`selftest-ffmpeg-<build>-run<n>.txt`, three runs each (the `selftest::tests` ignored test, debug build). Total time,
including making the clips with that FFmpeg and the colour probe:

| FFmpeg | total | colour policy | fit (flat PSNR / max) | pip | bt709 |
| --- | --- | --- | --- | --- | --- |
| 6.1.1 | 1.85 to 2.72 s | `FixedBt601` | 48.8 dB / 4 | 46.4 dB / 6 | 49.4 dB / 5 |
| 9.0.2 | 0.98 to 1.18 s | `BottomLayerTag` | 48.8 dB / 4 | 46.4 dB / 6 | 48.6 dB / 5 |

Taken again on the merged tree (`selftest-merged-tree-ffmpeg-<build>-run<n>.txt`, debug build, load average 8 to 11):
3.03 to 3.11 s on 6.1.1 and 1.68 to 2.02 s on 9.0.2, with the same PSNR and max error per case (`pip` 46.2 dB / 6): the
time is the FFmpeg children and the CPU rasteriser, so it follows the load of the box, and a real GPU only shortens
the GPU part (0.4 to 0.65 s per case here, drawn and read back on the CPU rasteriser).

Limits are the harness's own (PSNR >= 40 dB, max <= 8, edge band 2 px, step 24, >= 50 % judged), copied into
`selftest.rs` and held equal by `the_runtime_self_test_judges_by_the_limits_and_the_comparison_of_this_harness`.
**`pip` is the closest to its limit (max 6 of 8)**; on a real GPU it is the case to watch for a false positive
(a mismatch closes the GPU path for the session). The same GPU drawn under the *other* colour policy fails the
`bt709` case at 24 dB / 40 to 42 levels: that is the difference between the two FFmpegs the test is for
(`a_gpu_drawn_under_the_wrong_colour_policy_fails_it`). The check runs once, on its own thread, never under the
project lock; its children are thread-capped like interactive reads and take no machine-budget slot, so the
first GPU frame goes through FFmpeg and the page is asked again when the verdict is in.

## Decisions

1. **CPU adapters are refused by default for the preview.** `GpuOptions::for_preview` maps `KERF_GPU_ADAPTER`:
   unset or anything else = the machine's real GPU only; `software` = force the CPU adapter; `auto` / `any` = take
   what wgpu prefers. The tests and the parity harness keep `GpuOptions::for_tests` (lavapipe / WARP). Reason: a
   frame with a picture is 65 to 100 ms on lavapipe (section 2), slower than FFmpeg's scrub path (PERF-4:
   the next frame of a drag is 0.04 to 15 ms), and the rasteriser takes the cores the machine needs. The refusal
   names the adapter and the variable in the error and the log. "Unless proven faster" is **not** implemented (it needs a timing
   race per machine and a rule for who wins); there is no persisted setting, only the variable.
2. **The self-test's verdict stands for the session.** A mismatch closes the GPU path until restart; a check
   that could not be made (no FFmpeg for the clips, an unmeasured colour policy, the 90 s deadline, the GPU failing
   on the check frame) closes it until the setting is turned off and on again. The verdict belongs to an adapter and an
   FFmpeg (a device rebuilt on another adapter is checked again; one rebuilt on the same device after a loss keeps it).
   `KERF_GPU_SELFTEST=off` skips it (logged).
3. **Plan epoch**: process-wide, moved by every row written through a project connection except the analysis,
   history and task tables (a SQLite `update_hook`, rusqlite feature `hooks`), by an opened project, a proxy that
   lands or goes, and a change of proxy size or preview source. 2 s TTL on top.
4. **Drag detection**: requests under 120 ms apart are a drag; two gaps of 600 ms or more in a row end it.
5. **Counters** are per session, in memory, keyed by `Unsupported::kind()`; `gpu_preview_diagnostics` (Tauri
   command, no MCP tool) and a log line per new kind and a summary at teardown. `kind()` is an exhaustive match on
   purpose: a new `Unsupported` variant does not compile until it names its key.

## Verification (final head, lavapipe, both FFmpegs)

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --no-default-features --locked -- -D warnings`: clean.
- `kerf-core` 1263 passed (ignored ones not run); `kerf-gpu` 115 + 5 passed; `kerf-app` 161 passed.
- `kerf-gpu -- --ignored` passes on 6.1.1 and on 9.0.2 (parity 40, motion parity 13, frame source 17, self-test 3, the rest);
  `kerf-app -- --ignored gpu_preview::drive_tests gpu_preview::bench_tests` 8 passed on both; `x11.rs`' 3 ignored tests pass under
  `xvfb-run`.
- Frontend: `bun run check` 0 errors / 0 warnings, `bun run test` 2840 passed, `bun run build` ok.
- The golden argv oracle did not move (`git diff origin/main HEAD --name-only | grep golden` is empty).

Two flakes of the ignored suite showed up on the way and were fixed in the test, not hidden: the self-test's own
leftovers check counted the scratch directories of tests running beside it (same process id), and
`prefetch.rs`' `dropping_the_source_kills_a_prefetched_run` (PERF-4's, untouched by this branch) counted every
ffmpeg child of the test binary and failed 2 of 3 full pinned runs under load when a neighbour's child exited; it now finds its
process by the file it reads.

## Needs a real machine

- Metal and WARP parity matrix; real-GPU frame times and the effect of the mailbox and of `AutoNoVsync` on them.
- The self-test on real GPUs (false-positive risk: `pip` max 6 of 8 here).
- WebKit's IPC cost per `get_preview_frame` (the premise of the mailbox).

## Files

`drag-table.md`, `drag-ffmpeg-6.1.1-run{1,2,3}.txt`, `drag-ffmpeg-9.0.2-run{1,2,3}.txt`, `drag-input-timing-run.txt`,
`inputs-run{1,2,3}.txt`, `inputs-with-rebuild-run{1,2}.txt`, `selftest-ffmpeg-6.1.1-run{1,2,3}.txt`,
`selftest-ffmpeg-9.0.2-run{1,2,3}.txt`, `selftest-merged-tree-ffmpeg-{6.1.1,9.0.2}-run{1,2,3}.txt`, `ffmpeg-versions.txt`.
