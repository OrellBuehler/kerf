# Bit-exact GPU parity — progress log

Plan: `.claude/plans/gpu-exact-parity.md`. Newest entries last.

## Decisions

- **D1 measured (2026-10-31, x86_64, 4 cores).** `scale` with `flags=bicubic+accurate_rnd+bitexact`
  is byte-identical to the C path (`-cpuflags 0`) on FFmpeg 6.1.1 and 9.0.2 for testsrc2 and
  noise, 1920x1080 to 1280x720, 960x540, 320x180, 48x27, 2560x1440, 1000x562; the default
  flags differ from the C path on every one of them (1 level, 5 at 40:1). A graph-level
  `sws_flags=bicubic+accurate_rnd+bitexact;` prefix reaches both a `scale` without its own flags
  and the auto-inserted format conversions (exact against `-cpuflags 0`). Cost: a 4K to 1080p
  bicubic scale is 5.1 to 6.2 ms a frame on one thread (6.1.1), 5.1 to 6.6 ms (9.0.2), about
  1 ms a frame, under 5 % of an export frame (decode of 4K FFV1 alone is ~33 ms a frame).
  arm64 (the macOS bundle) not measured here: listed under *Needs a real machine*; the runtime
  self-test is what holds it there.
- **D1 narrowed: per resizing `scale`, never graph-level.** A graph-level `sws_flags` also reaches
  same-size conversions and moves swscale off its unscaled fast path: `yuv420p -> rgb24` moved by up
  to 88 levels (chroma interpolated instead of replicated), `-> argb` 88, an `rgb24` round trip 23,
  `gbrp` on 9.0.2 255. So the flags go only on `scale` filters that resize (where the change is the
  vertical scaler's rounding, at most 1 level, 5 at 40:1), and conversions stay as they are.
  With the flags, conversions are portable too (SIMD equals `-cpuflags 0` on both builds), which
  matters for the next decision.
- **The GPU scaler is swscale's C path, exactly.** `the_scaler_matches_ffmpegs_scale_plane_by_plane`
  against `ffmpeg -cpuflags 0`: 75 of 75 plane rows at max 0 (6.1.1). With D1 the scaled cases
  become exact on every CPU.
- **Baseline (6.1.1, lavapipe, main a8658fa): green, 17m41s.** Judged exactly, 367 of 462
  `motion-report` rows are 0 / 0; 23 of 783 Still rows are max 0 in RGB; the plane-level runs 103 of
  120. The Still gap is the final YUV -> RGB conversion: the GPU's (a float formula) is 3 levels from
  both today's x86 path and the C table path (`single/*` max 3 against `-cpuflags 0` too).
- **D4 — the still's final conversion goes through the exact scaler path.** Today's `rgb24`
  conversion is the platform's fast path (x86 SSSE3 differs from C by 3; aarch64 has its own NEON
  `argb` path), so matching it would hold only per machine. Instead the still graph's last
  conversion to RGB is made with `accurate_rnd+bitexact` (portable, measured equal to the C path on
  both builds) and the compositor ports that arithmetic (bicubic chroma upsampling with swscale's
  filter tables, then the yuv2rgb tables). This changes still / preview frames (chroma is
  interpolated, not replicated), never an export file. Lands as one PR with the GPU port.

## Needs a real machine

- arm64 macOS: confirm `accurate_rnd+bitexact` NEON scaling equals the C path, and the self-test passes.
- A real GPU (not lavapipe): the self-test passes bit-exact.

## Log
