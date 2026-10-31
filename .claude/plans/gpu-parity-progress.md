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
  Criterion met: adopted. arm64 (the macOS bundle) not measured here: listed under
  *Needs a real machine*; the runtime self-test is what holds it there.

## Needs a real machine

- arm64 macOS: confirm `accurate_rnd+bitexact` NEON scaling equals the C path, and the self-test passes.
- A real GPU (not lavapipe): the self-test passes bit-exact.

## Log
