# PERF-4: still cache and prefetch

What changed, in one paragraph: a scrubbed preview frame used to be one `ffmpeg` process per frame
(`Project::composite_timeline_frame`, a one-shot still), with nothing kept between frames. It now goes
through `ScrubFrames` (`kerf-core/src/engine/cli/scrub.rs`): a bounded cache of composited frames keyed by
the still's own argv plus the input files' stamps (an edit changes the key, nothing is invalidated, an undo
is a hit), and on a miss one long-lived `ffmpeg` that draws a window of 24 frames of the export graph over a
slice of the cut into a pipe. `Hint::Prefetch` is exposed on the GPU path (`FrameSource::prefetch`), and the
GPU preview's driver starts it for the clips the playhead is about to reach. `cpu.rs` has a GPU resource
class with the same three lanes as the CPU one.

## Method

- Machine: 4 shared cores, load average 8 to 13 from other agents the whole time. Absolute numbers are
  therefore noisy and pessimistic; the 3 runs per build are stored so the spread is visible.
- Builds: the pinned FFmpeg `n9.0.2-25-g67b60c310b-20261010`
  (`crates/kerf-app/binaries/ffmpeg-x86_64-unknown-linux-gnu`) and the system `6.1.1-3ubuntu5`.
- Scrub bench: `crates/kerf-core/tests/scrub_bench.rs`, release build, 1280x720 30 fps all-intra x264 sources
  (the shape of a preview proxy) previewed at 960 px, quality 4. One layer, and three layers (a full-frame
  clip and two picture-in-picture insets).
  `KERF_BENCH_OUT=<dir> cargo test -p kerf-core --no-default-features --release --test scrub_bench -- --ignored --nocapture`
- Cut stall: `crates/kerf-gpu/tests/prefetch.rs` (`#[ignore]`d, headless lavapipe), debug build of kerf-gpu, 12
  trials per run on fresh copies of the file so no page cache or probe cache survives between trials.
- Files here: `scrub-ffmpeg-<build>-run<n>.json`, `cut-stall-ffmpeg-<build>-run<n>.json`.

## Scrub: before and after

Medians over the 3 runs, range of the 3 in brackets. "Before" is the one-shot still per frame; "first frame" is
a cold miss on `ScrubFrames` (it starts a window); "next frames" are the following frames of a drag at 60 Hz.

| | FFmpeg 9.0.2, 1 layer | FFmpeg 9.0.2, 3 layers | FFmpeg 6.1.1, 1 layer | FFmpeg 6.1.1, 3 layers |
| --- | --- | --- | --- | --- |
| before: one-shot still per frame (ms) | 50 to 71 | 94 to 119 | 87 to 94 | 118 to 129 |
| first frame of a window (ms) | 71 to 89 | 104 to 132 | 97 to 120 | 142 to 154 |
| next frames of a 60 Hz drag (ms) | 0.04 to 0.05 | 4.8 to 9.0 | 4.6 to 7.4 | 12.7 to 14.7 |
| a frame already held (ms) | 0.01 | 0.01 | 0.01 | 0.01 |
| 20-step drag, before (ms) | 990 to 1419 | 1872 to 2386 | 1730 to 1876 | 2362 to 2584 |
| 20-step drag, after (ms) | 317 | 317 to 396 | 317 | 423 to 470 |

The "after" drag is the 19 intervals of the 60 Hz pacing (317 ms) plus whatever the last frames wait for: the
drag costs nothing beyond its own length on one layer, and 100 to 150 ms more on three layers on 6.1.1.

The first frame of an isolated seek is 10 to 20 ms slower than a one-shot still (the window's graph plus its
lead frame): that is the price of the frames behind it. A user who clicks once and does not move pays it; one
who drags earns it back on the second frame.

## Cut stall (GPU preview, the first frame of the next clip)

Median of 12 trials, range of the 3 runs.

| | clip never opened | probes done, no prefetch | with `Hint::Prefetch` |
| --- | --- | --- | --- |
| FFmpeg 9.0.2 (ms) | 145 to 183 | 33 to 38 | 0.22 to 0.26 |
| FFmpeg 6.1.1 (ms) | 321 to 361 | 73 to 93 | 0.22 to 0.23 |

The prefetch runs the probes and starts the decode on a spare slot while the previous clip is still on screen,
so the frame at the cut is a cache read. Measured on lavapipe in a debug build: the GPU side is not in the
number, the source side is.

## What the rendered tests hold

`scrub_rendered.rs` (`#[ignore]`d, drives the real ffmpeg): a window frame is the still's JPEG byte for byte or
the export's frame, for every kind of cut a window may draw, from a window started at any frame.
`cargo test -p kerf-core --no-default-features -- --ignored scrub_rendered`. All 3 pass on 9.0.2 and on 6.1.1.

Findings from writing them, all in `.claude/docs/engine-cli.md`:

- On 9.0.2 the window and the still are one JPEG on a plain cut. On 6.1.1 the still is the odd one out on every
  cut: its bare `color` base lets the overlay settle on another pixel format and `testsrc2`'s one-pixel chroma
  lines come out 14 levels off on the mean. Adding `format=yuv420p` after the base (as the window's graph has)
  gives the window's picture exactly. The window equals the export there. The still's argv was left alone.
- A source whose rate is not a whole multiple of the cut's (24 or 25 in 30, speed 0.5 or 1.5) samples frames
  depending on where the slice starts, so a window is refused there and the still serves it.
- A start exactly half a frame off the grid is a tie between two source frames: window and still take one on
  6.1, the export its neighbour.

## Caveats

- Shared 4-core box under load: both sides of every comparison were measured under the same load, so the ratios
  hold better than the absolute numbers; the spread across the 3 runs is the noise.
- The cut stall is a debug build of kerf-gpu on a software adapter; a release build on a real GPU shortens the
  uncached side, not the cached one.
- The GPU resource lane (`Resource::Gpu`, `MAX_GPU_JOBS = 2`) could not be measured: there is no hardware
  encoder here. It is covered by unit tests of the gate (lanes, priority, fall-back to the CPU class) and of the
  classifiers. On Linux and macOS no job qualifies until a decoder probe exists, so there it changes nothing.
