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

## Round 2: originals, backward drags, jumps

Review asked whether a backward miss (a window with 22 frames of lead) or a jump (a full window for one frame) is
slow when the inputs are originals and not proxies. `tests/scrub_bench.rs` now has a 1080p long-GOP original
(`-g 250`, B-frames, outside `kerf/proxies`) beside the proxy cases, and four new scenarios: **twenty steps back at
60 Hz**, **one step back** out of a drawn window (24 frames before it), **twelve jumps** on one cache, and **click,
then step back one**. `KERF_BENCH_ONLY=<case substring>` and `KERF_BENCH_CLICK_ONLY=1` run less. Runs are interleaved
at a load average of 13 to 15, so the one-shot (the control, unchanged) moves by 10 to 25 % between runs.

**Result: nothing was changed over originals.** The engine still starts a window one frame before the one asked for, a
step back still gets the window behind it, and a jump still gets the full window. Two changes were tried and rejected on
the numbers.

**Lead 0 over originals (rejected).** Reaching frame k the process decodes every frame from the keyframe, so a window
with no lead frame would have the asked frame first out. Round 2 first committed that (`e7c7042`). Measured against
the previous engine, 3 runs per build, medians of the runs' medians in ms (`round2/`), it looked like a gain on 9.0.2
and was not on 6.1.1, inside the noise either way:

| 1080p original | 9.0.2 lead 1 | 9.0.2 lead 0 | 6.1.1 lead 1 | 6.1.1 lead 0 |
| --- | --- | --- | --- | --- |
| one-shot per frame (control) | 770 | 782 | 862 | 921 |
| first frame of a window | 805 | 728 | 886 | 876 |
| 20 steps forward at 60 Hz | 1126 | 1020 | 1475 | 1232 |
| 20 steps back at 60 Hz | 2071 | 1690 | 2228 | 2450 |
| jumps, median | 874 | 672 | 855 | 822 |

What the scenarios did not cover is the commonest one after a click: **click on frame k, then step back one**. With a
lead of 1 that frame is already drawn, a hit. With none it is a miss that starts a reverse window. Added as a scenario
and measured, lead 1 against lead 0 interleaved (`click-back-one/`, 8 rounds on 9.0.2, 4 on 6.1.1, medians of each
round's median):

| click, then step back one, 1080p original | lead 1 | lead 0 | one-shot |
| --- | --- | --- | --- |
| 9.0.2 | 0.13 ms | 866 ms (692 to 1211) | 368 ms |
| 6.1.1 | 0.12 ms | 925 ms (871 to 1127) | 355 ms |

A step back that costs 2.4 times the one-shot, against a first-frame gain inside the noise: lead 1 is restored (the
reverted commit is `e7c7042`'s `window_range`; the epoch, retry, off-lock and reaper changes in it stay).

**A short window at the frame for a step back or a jump (rejected).** The review suggested it for a backward miss or no
direction. Tried as an interleaved A/B on the 1080p original (4 to 5 rounds, load ~14, toggles temporary and not in the
tree, `window-variants/`): A the old engine, B lead 0 with the reverse window kept for steps back, C B plus a short
window at the frame for the first step back and the reverse window from the second, D C plus a short first window for a
first request or a jump.

| 1080p original, 9.0.2, medians | A old | B | C | D |
| --- | --- | --- | --- | --- |
| first frame | 824 | 717 | 744 | 725 |
| jumps | 697 | 618 | 794 | 726 |
| 20 steps forward | 965 | 979 | 921 | 1846 |
| 20 steps back | 1916 | 1660 | 3044 | 2988 |
| one step back (4 rounds, A B C) | 1283 | 1338 | 1121 | |

C saves about 160 ms on an *isolated* step back and costs 1.2 to 1.4 s on a drag back (two processes, each with its own
start and seek, where one window behind the frame serves the whole drag). D doubles a forward drag from a cold start,
since the short first window is outrun and the next process pays a start. A jump still leaves the process decoding up
to 23 frames after its frame is out, until the next request kills it: that is CPU and not latency (the request
returned), and it is what makes the first steps of a forward drag hits.

## What the rendered tests hold

Round 2 added cut shapes: a cut that is itself 29.97 and 23.976 (frame `k * 1001 / 30000`, `k * 1001 / 24000`), a
23.976 cut with an in-point half a frame off, an animated keyframed title over a box, and two generated captions that
meet at a frame (with a `%` in the text). All 20 cases pass on 9.0.2 and on 6.1.1. On 9.0.2 the 29.97 and 23.976
cuts, the in-point case and the captions are the still's JPEG byte for byte (281 of 281 frames), the animated title
57 of 62 (the other 5 the still's picture through the encoder's rounding, 0.15 on the mean); on 6.1.1 every window frame
is the export's frame (the still's chroma is the odd one out there, below). The caption and the title text carry a `:` and an `'` too, since GPU-2's `escape_drawtext` is on the branch.


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
  classifiers (H.264 qualifies at 8 bit only: High10 has no D3D11VA, DXVA2 or VAAPI decoder; HEVC at 8 or 10 bit).
  On Linux and macOS no job qualifies until a decoder probe exists, so there it changes nothing. The proxy queue runs
  two encodes at once only where the lane can qualify (on, Windows), so a hardware proxy no longer waits for the
  software one in front of it; elsewhere it stays one.
