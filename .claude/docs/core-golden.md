# kerf-core: the golden argv oracle (`engine/cli/golden.rs`)

- `engine/cli/golden.rs` — **the golden argv oracle** (test-only, a child of `cli` so it
  reaches the private builders). 4000 seeded timelines (every transition kind with / without
  a source handle / across a gap, fades, speed, reverse, stills, keyframes, masks, effects,
  chroma, reframe, HDR, overlays, delivery format and fit, the audio mix, and **every
  `ExportOptions` field** with the one-, two- and no-pass encoder spellings), plus **800
  appended** with a master bus (`master_for`, no dice of its own, so the first 4000 never
  moved — a new family appends blocks, it does not re-bless old ones) and **800 more** with
  per-property channels (`channels_for`: colour numbers, a volume, some transform numbers keyed beside
  the rest, a number taken off a legacy bundle or held static by an empty track — on its own dice
  seeded by case and clip, so `0..4800` are the digests the commit before channels gave; coverage
  families `colour-keyed-*`, `volume-keyed*`, `channel-*`), have their
  `build_export_args_phase`, `build_still_args` and `build_preview_args_with` argv reduced to
  FNV-1a digests, committed as 56 block digests each in
  `engine/cli/golden/{export,still,preview}.txt` (LF: `.gitattributes`, and the comparison
  ignores `\r`). A refactor of the graph builders must leave all three untouched; an
  intended argv change moves the files of the builders it touched. (`build_proxy_args` and
  the probe `generate_proxy` now runs for its sidecar are outside it: the oracle covers the
  export, still and preview builders, and the sidecar changed none of them.) **Bless** with
  `KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden -- --nocapture`
  (exactly `1`): it rewrites **all three** files and says so, and `git diff` is the guard —
  only the files you meant to change should move. It is **machine-independent**: the
  builders read the machine in five places and each is pinned — the preview's
  `decode_hwaccel()` (through `build_preview_args_with`), `zscale_available()` (a `cfg(test)`
  thread-local override; every HDR case is built both ways), `alimiter_latency_available()`
  (the same kind of override, `with_alimiter_latency`; every case is built with the option,
  and a case with the master limiter on has its **export** argv appended once more without
  it — the still and the preview carry no sound, so those two files do not see it, and the
  limiter families `master-limiter-no-latency[-loudnorm]` fail the test if they stop being
  covered), `drawtext`'s resolved font path
  (no overlay names a font) and **libm** (`db_to_linear` is `powf`, whose last digits differ
  between glibc with and without FMA, macOS, Windows and arm, so `round_libm` keeps the
  compressor / gate numbers to 10 significant digits; the generator seeds dB values known to
  differ, so a run under `GLIBC_TUNABLES=glibc.cpu.hwcaps=-FMA,-FMA4` fails without it). The
  digests are identical blessed with `KERF_HWACCEL` unset, `none` and `auto`. A coverage
  table (`family needle` lines, plus the branch `transition_fx` took and a few structural
  tags) fails the test if a family hits fewer than 20 cases — the generator stopped covering
  it, or the argv text changed; digest and coverage failures are reported together.
  `KERF_GOLDEN_CASES=<file>` writes a digest per case (diff base vs change to find the case
  in a failing block), `KERF_GOLDEN_DUMP=<n>` prints one case's argv,
  `KERF_GOLDEN_COVERAGE=1` the thinnest families. Two intended changes since it landed:
  the still's `-ss` going from `{:.3}` to `{:.6}` re-blessed `still.txt` alone (only `-ss`
  values differ in any still argv), and the pool gained head-padded proxy twins of four
  assets (`.../kerf/proxies/<hex>.lead.mp4`, so `ClipFx.head_pad` and its
  `trim=start_frame=1` are covered), reached only by `retarget` — every seventh case, no
  dice of its own — so all three files re-blessed but only 428 of the 4000 per-case digests
  moved (`KERF_GOLDEN_CASES` before / after) and the rest are byte-identical. Raising
  `LIBRARY` (a new generated asset) moves the draws of every case; a new twin moves none.
  The **keyed-zoom fix** (zoom last in the chain, `rotate` filling `black@0` when keyed)
  re-blessed `export.txt` and `preview.txt` and left `still.txt` alone: 2027 export and 1507
  preview of 4000 cases moved, exactly the ones whose graph holds a moving zoom (1814 / 1298,
  the `zoom-keyed-last` families) or a keyed rotation (1748 / 1249, `rotate-keyed-transparent`,
  213 / 209 of them with no moving zoom: the fill fix is the one change not confined to a
  zoom), and with the fix compiled out the argv equals the committed digests. `KERF_GOLDEN_FAMILIES=<file>`
  writes the families each case hit, which is how a moved set is tied to a kind of case; a
  keyed clip whose scale holds still is byte-identical unless it also rotates (of the 482
  cases that carry only such clips, the 196 that moved are exactly the ones with a keyed
  rotation). The second round (the still following the export's order, the tiny-scale clamp,
  even sizes ahead of a tone-map, alpha sources kept) went in **one change at a time with
  `KERF_GOLDEN_CASES` between**, each moved set tied to its family: the generator's own
  inputs first (three no-dice retargets like the padded twins — `-alpha` twins of `still` /
  `wide` / `interview`, `i % 11 == 5`; a 4:3 HLG twin, `i % 13 == 8`; a 0.0004 scale,
  `i % 17 == 4` — moved 477 export / 171 still / 310 preview cases, all of them retargeted
  ones), then the still's zoom (still only: 616, exactly `still-zoom-last`, a chain that ends
  in the zoom), the clamp (138 / 66 / 116 = `tiny-scale`), the even sizes (export and
  preview 1733 / 1442 = `hdr-even-fit` or `hdr-even-zoom`; the still has no tone-map after
  the geometry) and the alpha chain (98 / 68 = `alpha-kept`: a clip chain whose last filter is
  `format=yuva420p` and not a zoom). Against the digests committed before the round 1920
  export, 750 still and 1550 preview cases differ. `zoom-keyed-last`, `alpha-kept` and
  `still-zoom-last` are structural families (read off how the chains *end*: the text of a moving
  zoom and of an alpha source's terminal format is the same `format=yuva420p`).
- **Sample-exact `adelay`** (a clip starting between milliseconds is delayed by samples)
  re-blessed `export.txt` alone: 2718 of 4800 cases moved, exactly the new
  `audio-delay-samples` family (`S:all=1`); still and preview carry no sound.
