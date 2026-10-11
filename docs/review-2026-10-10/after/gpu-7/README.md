# GPU-7: GPU playback (A4)

What changed, in one paragraph: with the GPU preview on (A2, opt-in), pressing Play used to start `stream_preview`
(an FFmpeg process that composites and encodes JPEGs) whatever the surface was. Now the page asks the backend for
the picture of the time that is being *heard* once per animation frame (`getPreviewFrame(..., hint: forward)`), the
backend draws it from one forward-only `FrameCursor` per clip over the export's own frames, and the stream is kept
for everything the compositor does not draw exactly. The webview stays the clock and the audio stays the clock.
The first request of a session decides it for the whole play (everything in the next 5 s is drawable, or the
stream plays), a refusal that comes into view later hands the session to `stream_preview` at the frame where the
GPU stops (`SpanPlan::handover`), and the playhead resting on a paused frame starts the cursors of the next Play
before it is pressed (priming).

## Method

- Machine: 4 shared cores, load average 2 to 4 during the runs (other agents). Adapter: Mesa lavapipe (llvmpipe,
  LLVM 20.1.2), no real GPU, no display, no audio device. **A CPU rasteriser is the GPU here**: its raster is the
  frame time and most of the process CPU, so the GPU-side figures below are not a real GPU's and are kept apart.
- FFmpeg builds: system `6.1.1-3ubuntu5` and the pinned `n9.0.2-25-g67b60c310b-20261010` (`ffmpeg-versions.txt`).
- Bench: `a_play_session_through_the_frame_path` in `crates/kerf-app/src/gpu_preview/bench_tests.rs`, release build,
  three runs per FFmpeg, raw output in `raw-<ffmpeg>-run<n>/` (`bench-play.txt` is the report, `stdout.txt` the
  console): `KERF_BENCH=1 KERF_BENCH_OUT=<dir> cargo test -p kerf-app --no-default-features --release --lib -- --ignored --nocapture a_play_session_through_the_frame_path`.
  Taken on commit 33c70a2 (the merge of origin/main 70d0f48 with GPU-6), before the teardown change 155d7ad, which
  touches only how a session ends.
- 1280x720 all-intra clips, a 960 px picture. *light*: one layer, a hard cut at 5 s. *heavy*: base + dissolve + a
  translucent PiP + a rotated PiP + a title. The page is simulated the way `frame-pump` does it (a request per
  16.7 ms tick, the latest wins, the first request alone until it is answered, 4 s of play across the cut), the
  screen is a recording one that waits for the GPU to finish each frame (so lavapipe's raster is in `present`).
- Start latency is Play to the first frame on the screen (no IPC: add 1 to 3 ms), 8 starts per cell. Three kinds
  of GPU start: *cold* (nothing made ready, the playhead was just put there), *rested* (the playhead rested on a
  paused frame, the play was primed, and Play came 600 ms later: the pointer going to the button) and *instant*
  (Play at the moment priming finished, so the `ffmpeg`s had no head start; the worst case of a primed start).
  *Without the screen's wait for the GPU* subtracts the recording screen's wait for lavapipe's raster (`present_ms`):
  what the start costs on a device whose frames are not the CPU's.

## 1. Start latency (p50, ms; the range is the three runs)

| | FFmpeg | light | heavy |
| --- | --- | --- | --- |
| `stream_preview` (start to first JPEG) | 6.1.1 | 117 to 122 | 183 to 196 |
| | 9.0.2 | 75 to 83 | 135 to 153 |
| GPU, cold, as measured (lavapipe's raster in it) | 6.1.1 | 136 to 142 | 266 to 280 |
| | 9.0.2 | 110 to 113 | 240 to 249 |
| GPU, cold, without the screen's wait for the GPU | 6.1.1 | 75 to 77 | 100 to 112 |
| | 9.0.2 | 40 to 45 | 67 to 72 |
| GPU, instant (priming just done), without the GPU wait | 6.1.1 | 71 to 75 | 100 to 112 |
| | 9.0.2 | 38 to 40 | 62 to 69 |
| GPU, rested (primed, Play 600 ms later), as measured | 6.1.1 | 50 to 52 | 157 to 174 |
| | 9.0.2 | 49 to 52 | 160 to 165 |
| GPU, rested, without the screen's wait for the GPU | 6.1.1 | 3.4 to 3.6 | 10.5 to 14.5 |
| | 9.0.2 | 3.5 to 3.9 | 12.4 to 12.8 |
| ...of which the wait for the cursors' first frames (cold / rested) | 6.1.1 | 73 to 75 / 1.9 to 2.0 | 90 to 105 / 5.7 to 6.5 |
| | 9.0.2 | 38 to 42 / 1.9 to 2.2 | 60 to 62 / 4.0 to 7.2 |

Reading it:

- Almost all of a cold start is `ffmpeg` starting (the file's open and its first frames), 40 to 75 ms for one clip
  and 60 to 105 ms for three, which is also what the stream's start is; the compositor's own cost is a few
  milliseconds. Without lavapipe's wait the cold GPU start is **faster than the stream's** by 1.6x to 2.0x on both
  FFmpegs and both cuts (75 vs 117 to 122 and 100 to 112 vs 183 to 196 on 6.1.1; 40 to 45 vs 75 to 83 and 67 to 72 vs
  135 to 153 on 9.0.2); as measured *on this software adapter* a cold start of the light cut is slower than the
  stream's (the raster of the first frame is 60 to 70 ms of CPU), which a real GPU does not pay.
- Priming is what makes the start short: a Play that comes after the playhead has rested finds the cursors with
  their first frames already decoded, and the wait for them goes from 40 to 100 ms to 2 to 7 ms. Without the
  raster that is **3.5 ms (light) and 11 to 15 ms (heavy)** from Play to a drawn frame. The instant column shows the
  limit: a Play at the very moment priming completes has no head start and is a cold start (it is never worse).
  In use the head start is the time to move to the Play button after the 300 ms rest that starts the priming.
- The 6.1.1 cold figures are larger than 9.0.2's because the older `ffmpeg` is slower to start and to open the
  clips (the stream's start shows the same difference).

## 2. A play across the cut (4 s, p50 unless stated; ranges over the three runs and both FFmpegs)

| | light | heavy |
| --- | --- | --- |
| frames of the cut drawn / replaced by a newer request (of 120) | 80 to 90 / 25 to 34 | 53 to 59 / 47 to 53 |
| frame time to the screen after the frame's own slot began: p50 / p95 | 5.6 to 12 / 25 to 36 ms | 3.8 to 7.8 / 25 to 33 ms |
| frames that reached the screen inside their own 33 ms slot | 61 to 85 % | 50 to 62 % |
| round trip of a request (p50) | 60 to 65 ms | 64 to 71 ms |
| first frame of the session (first request to its answer) | 133 to 183 ms | 246 to 296 ms |
| `ffmpeg` (cursor) CPU per drawn frame | 7.8 to 10.7 ms | 19.4 to 29.6 ms |
| ...per frame of the cut (x drawn/120: the cursors decode them all) | 5.9 to 7.6 ms | 8.7 to 13.6 ms |
| `stream_preview` `ffmpeg` CPU per frame of the stream (6.1.1 / 9.0.2) | 14.2 to 14.7 / 10.0 to 11.8 ms | 18.8 to 21.7 / 17.5 to 19.1 ms |
| whole process CPU per drawn frame (lavapipe's raster, all four cores) | 144 to 156 ms | 215 to 229 ms |

Reading it:

- **Skip, never slow down works as designed and is visible here**: the software adapter cannot draw 30 fps at
  960 px (the round trip is 60 ms), so a quarter of the light cut's frames and half of the heavy cut's are
  replaced by newer requests; the frames that are drawn reach the screen within a slot of their time (median a few
  ms after it began, p95 about one slot) and the picture never falls behind the clock. A real GPU draws the frame
  in 1 to 3 ms and replaces none; this run cannot show that and says only that overload degrades the frame rate,
  not the sync. The page also gives the session to the stream when the median round trip stays over three frame
  periods for a second, which this run (60 to 70 ms against a 100 ms limit) just stays under.
- **CPU**: the part that is not the rasteriser is the decoders. Per frame of the cut the cursors' `ffmpeg`s use about
  **half the CPU the stream's `ffmpeg` does** (light 6 to 7.5 vs 10 to 14.7 ms; heavy 9 to 13.6 vs 17.5 to 21.7 ms:
  no scale, overlay or JPEG encode per frame, and a frame is decoded once whatever the display rate). The whole
  process figure, 144 to 229 ms per drawn frame, is lavapipe drawing on every core and **cannot** be compared with
  the stream's: it needs a real GPU (see the end).
- The first frame of a session in this simulation is its first *request* through the whole path (the cold start
  above plus the raster) and is not what a user waits for: Play starts the audio and the first picture follows.

## 3. A/V offset

The webview asks for `clock - outputLatency + round trip`. What can be measured headlessly is the pipeline half:
how far from its own time the frame reached the screen on a clock that starts with the play (the frame-time rows
in section 2: median 4 to 12 ms after the frame's slot began, p95 25 to 36 ms, i.e. within a slot) with the round
trip folded into the request. The other half is the sound's output latency (`audio.ts`: `getOutputTimestamp()`,
else `outputLatency + baseLatency`, clamped to 0.5 s), which only an audio device has; with no device here that
is **not measured and needs a real machine**.

`make-av-fixture.sh` builds the clip for that: 30 s at 30 fps, every frame shows its own number, the whole frame
flashes white for two frames at every whole second (frames 0, 30, 60, ...) and the audio is silence with a 10 ms
1 kHz beep at the same instants. Checked here: the flashes are frames 0, 1, 30, 31, 60, 61, ... and
`silencedetect` finds the beeps' onsets at 1.00002 s, 2.00002 s, ... so the file's own offset is zero. Protocol:
import it, put it on the timeline, turn the GPU preview on, press Play from 0, and record the screen and the sound
together (a phone filming the screen with the speaker in view, or a screen capture plus a loopback of the audio);
the offset is the distance from each flash to its beep, per OS and per output (built-in speakers, a Bluetooth
headset, which has an output latency of 100+ ms and is the case `outputLatency` is for). Do it once with the GPU
preview on (this work) and once off (`stream_preview`, where the page's own sync ladder applies) for the same
output.

## 4. Parity of the played frames

`playback-report-ffmpeg-6.1.1.txt` and `-9.0.2.txt` (21 rows each) are the output of `tests/playback.rs`: a
session's frames (cursors over a `PlayCut`'s Motion plans, composited by the compositor) against the export of the
same timeline (`whole`) or of the slice `stream_preview` plays (`slice`), every frame of the range, worst error in
levels over all planes: **0 levels, 0 samples differ in all 21 rows on both FFmpegs**. They cover a start at 0, a
start mid-clip, starts inside a dissolve / dip / slide / push, 25 fps material in a 30 fps cut, a 2x speed clip,
fades, and a session that asks for every 2nd, 3rd, 7th and 30th frame. (A slide and a push are compared with the slice export and not
the whole one: their travel is truncated to a whole pixel, and at an exact tie the 1 ns bias that puts the slice
on its frames moves the pixel, up to 224 levels in a few frames of the transition against the whole export;
against the slice, which is what the stream plays, they are exact.)

## 5. Tests

`ignored-suites-merged-tree.txt` (both FFmpegs; the tree after merging GPU-6) and `unit-tests-merged-tree.txt`:
kerf-core 1289 passed, kerf-gpu `--lib` 123 passed, kerf-app 172 passed; `--ignored` on 6.1.1 and 9.0.2: kerf-gpu
`--lib` (self-test) 11, `frame_source` 17, `prefetch` 7, `motion_parity` 23, `parity` 44, `playback` 8, `cursor` 4,
`compositor` 16, `coverage` 1, `text` 6 passed; kerf-app `gpu_preview::drive_tests` + `play_tests` 11 passed (rerun
after the teardown commit, and 30 consecutive runs on 9.0.2 and 20 on 6.1.1 without an exit crash). Frontend: `bun run check` 0 errors
and warnings, `bun run test` 2878 passed, `bun run build` ok. The golden argv oracle is untouched
(`git diff --name-only origin/main HEAD | grep golden` is empty).

## 6. Not verified here (needs a real machine)

- The sound's output latency and the A/V offset (section 3), per OS and output; WebKit's `getOutputTimestamp()`
  support (WebKitGTK, WKWebView) is unknown, the fallback is `outputLatency + baseLatency`, then 0.
- Real GPU timings: the per-frame composite and present, the CPU of the process per frame with a hardware
  adapter, and whether a real GPU holds 30 fps at 1080p with three layers (here 960 px, lavapipe).
- The surfaces: a vsync-presented 30 fps stream on Windows (DX12 child window / HWND) and macOS (Metal layer), and the
  X11 child window switching between the GPU surface and the stream's `<canvas>` at a hand-over. The headless
  drive tests use a recording screen. (`gpu_preview::x11`'s `an_abstract_only_x_server_is_reached_at_once` fails under
  `xvfb-run` here, in code this work does not touch; the other two x11 tests pass.)
- Priming's cost with real clips on a spinning disk or a network drive: one idle `ffmpeg` per layer of the frame on
  screen for up to 10 s while the playhead rests.
