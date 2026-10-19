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
  families `colour-keyed-*`, `volume-keyed*`, `channel-*`) and **400 more** with an AMF or VAAPI
  encoder (`hw_encoder_for`, by index: `h264/hevc/av1_amf` and `h264/hevc/av1_vaapi`, which exist
  nowhere in the first 5600; families `codec-amf-av1`, `codec-vaapi-*`, `rate-amf-quality`,
  `rate-vaapi-*`, `vaapi-device`, `vaapi-upload`) and **400 more** with animated SVG titles (`svg_for`, on dice seeded by the case:
  one to three overlays of four fixed artworks — never a preset, whose text would put the machine's libm in the cache key — half in a box
  that may hang off the frame, a third translucent, a fifth part-way into their animation; every eighth case forced to the gif container,
  no dice; the cache directory pinned at `/kerf-test/svg` by `pinned`, and a still or the playhead may land on a title's edge (the edge
  list gains the titles' edges, a different pick from the same single draw; every third case plays from a frame of the stream's own grid
  in the middle of its first title, `svg_playhead`, no dice, because the drawn playhead is between frames and a stream from one never reads
  `-start_number`); families `svg-input`, `svg-held`, `svg-overlay`, `svg-two`, `svg-partly-off`, `svg-restamp`, `still-svg`,
  `still-svg-one-file`, `preview-svg-*` (`input`, `held`, `seek`, `overlay`) and
  the structural `svg-under-text`, `svg-gif`, `svg-offset`, which gave 28 to hundreds of cases each), **400 more**
  exported at two or three delivery frames by one run (`together_for`, on dice of its own: framings on the clips,
  titles on every other case, captions and chapters to mux on a quarter, the encode made one the run takes, every
  tenth an audio-only container that none does; their export text is `together::build_together_args` plus its
  `sinks_from_end`; families `together-*`) and **400 more** with titles on tracks (`title_clip_for`, on dice seeded by
  the case, the same four artworks: after the last clip of the lowest video track — touching it under a transition of any
  kind or across a gap — or over the cut on a video track of their own, some head-trimmed, faded or placed; the title
  cases get none of the SVG, gif, playhead or multi-delivery tweaks of the families before; families `title-clip-held`,
  `preview-title-clip-held` and the structural `title-clip`, `title-clip-dip`, `title-clip-under-transition`,
  `still-title-clip`, `preview-title-clip`; appending them moved none of the first 6800 cases), have their
  `build_export_args_phase`, `build_still_args` and `build_preview_args_with` argv reduced to
  FNV-1a digests, committed as 72 block digests each in
  `engine/cli/golden/{export,still,preview}.txt` (LF: `.gitattributes`, and the comparison
  ignores `\r`). A refactor of the graph builders must leave all three untouched; an
  intended argv change moves the files of the builders it touched. (`build_proxy_args` and
  the probe `generate_proxy` now runs for its sidecar are outside it: the oracle covers the
  export, still and preview builders, and the sidecar changed none of them.) **Bless** with
  `KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden -- --nocapture`
  (exactly `1`): it rewrites **all three** files and says so, and `git diff` is the guard —
  only the files you meant to change should move. It is **machine-independent**: the
  builders read the machine in six places and each is pinned (the sixth is the animated titles' cache
  directory, `svg::raster::with_pinned_dir`) — the preview's
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
  Reading an animated title differently (a document that cannot move as one looped file, a stream
  reading the title's sequence from `-start_number`, a still as `-f image2 -pattern_type none`) re-blessed
  all three files and moved **only blocks 60..63** (the title cases): the diff is four lines a file.
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
- **The still inside a transition** (fades, tails and travel drawn as the export draws them,
  `engine/cli/still_fx.rs`) re-blessed `still.txt` alone: 1577 of 5600 still cases moved, exactly the
  cases hitting the new `still-fade` (`settb=expr=`; plus `still-fade-alpha`, `still-fade-white`,
  `still-fade-pixel-format`), `still-tail` and `still-travel` (structural: a clip kept past its end,
  a clip a slide or push moves) families; export and preview digests are identical per case. The
  review round moved 11 of them again: 3 clips that fade after a dissolve (the chain stays on an
  alpha plane) and 8 whose tail window closes inside the still's frame (1581 against the base);
  all in the same families.
- **The default font and the `drawtext` escapes** (two commits, `export.txt` / `still.txt` / `preview.txt` each time). Every overlay now names a
  font file (`fontfile='...'`, the shipped Geist, pinned in the oracle at `/kerf-test/fonts/Geist-*.ttf` by `fonts::with_pinned_default_font`, with
  `fonts::PINNED_FAMILY` — a family with no bold face — standing in for the named-font and synthetic-bold branches: a bold overlay that is not `generated`
  names it, chosen from draws already made so no other case moves). 1481 export / 388 still / 795 preview of 5600 cases moved, exactly the cases hitting
  the new families `overlay-default-font[-bold]`, `overlay-named-font` (and their `still-` / `preview-` forms). Then the three-level escape of apostrophe, colon and
  backslash (`overlay-quote`, `overlay-colon`, `overlay-backslash` and their `still-` / `preview-` forms): 801 / 148 / 402 cases moved, exactly those. Both sets
  tied with `KERF_GOLDEN_CASES` before / after and `KERF_GOLDEN_FAMILIES`.
- **Line breaks in overlay text** (`export.txt` / `still.txt` / `preview.txt`): half of the draws of the pool text `"two\nlines"` (those whose `start` has
  a fraction of at least one half, from draws already made so no other case moves) are `"one\rtwo\r\nthree\u{2028}four\u{2029}five"`, which every kind of break turns
  into one space. New families `overlay-linebreak` (`one two three four five`) and its `still-` / `preview-` forms: 152 / 23 / 85 cases moved
  (180 of 5600), exactly the cases hitting them.
- **Sample-exact `adelay`** (a clip starting between milliseconds is delayed by samples)
  re-blessed `export.txt` alone: 2718 of 4800 cases moved, exactly the new
  `audio-delay-samples` family (`S:all=1`); still and preview carry no sound.
- **The animated-title round** (appended, nothing re-blessed): the three files gained exactly four block lines each (`60`..`63`, 12 insertions,
  0 deletions in `git diff`), so every one of the 6000 earlier per-case digests is the one the commit before gave. The HW swap is now restricted
  to its own range (`5600..6000`) so it does not reach the title cases, and the time draws take the titles' edges only where titles exist.
- **The hardware-encoder round** (AMF recalibrated, `-quality quality`, VAAPI): `export.txt`
  re-blessed, `still.txt` / `preview.txt` only gained the four appended block lines. Of the first
  5600 cases exactly 282 export digests moved (`KERF_GOLDEN_CASES` before / after) — every case whose
  argv holds `-c:v h264_amf` (140) or `hevc_amf` (142), the families `codec-amf-h264` / `codec-amf`
  (`KERF_GOLDEN_FAMILIES`) — and no other: no still, no preview, no case of any other encoder. All 56
  old export *blocks* differ only because each holds AMF cases. The appended cases' codec swap has no
  dice, so the generator's draws are untouched.
- **The BT.709 label** (`setparams=colorspace=bt709` on an untagged HD clip that shares its graph with an input naming a
  matrix; `engine-cli.md`) re-blessed all three files as one new family: `bt709-label`, `still-bt709-label`,
  `preview-bt709-label` (the needle in the argv). Of 6000 cases 3054 export, 644 still and 2139 preview digests moved
  (2852 / 610 / 1993 of the first 5600), exactly the cases hitting those families, and `KERF_GOLDEN_CASES` before / after
  with the label text (`setparams=colorspace=bt709,`, the first filter of the chain) stripped from the new argv gives the old
  digest of every case in all three kinds, so the label is the only change. (Moving it from behind the geometry to the head of
  the chain, once a dip to white showed a label behind the fit `scale` is not a tag, moved the same 3054 / 644 / 2139 cases
  and no other.) The corpus holds HDR as its only matrix-naming footage, so the cases are the ones that put an untagged
  HD clip (`interview`, `broll`, `phone`, `wide`, `sphere`, `lenses` and their padded twins) in a graph with an HLG or PQ
  one. The structural families `bt709-mix`, `still-bt709-mix` and `preview-bt709-mix` are an oracle by asset *name* (the
  export's graph is the range's slice, the preview's the cut from the playhead, a still's the layers on screen), and the
  test fails unless the label is in an argv exactly when it says so: no single-clip case, no case of untagged footage
  only and no audio-only export moved. Tagged SDR footage, SD, MJPEG, RGB and alpha are covered by the builders' unit
  tests (`engine/cli/bt709_label.rs`), and the corpus' blind spot, its stills carrying no tag, by a **metamorphic test**
  (`a_decoders_default_tag_names_no_matrix_in_any_graph_of_the_corpus`, no digests): each case that holds an HDR clip is built
  again with that clip replaced, same size and place, by a shape the probe really gives (a PNG with alpha or grey, `gbr`; a JPEG,
  a WebP and MJPEG video, `bt470bg`; RGB video; and one shape per rule, each kept off by that rule alone: a moving `rgba` with `gbr`,
  MJPEG and SpeedHQ at `yuv422p`, JPEG-LS grey, a decode-only `thp`, AMV, an `rgb24` with `bt470bg` from FFV1) and the label must be in no argv, and by explicitly tagged H.264
  clips (`bt709`, `smpte170m`, full-range `yuvj420p` `bt709`) and must then be exactly where the HDR clip's was. It moves no digest.
  The 400 animated-title cases (`6000..6400`) were blessed with the label in (blocks `60`..`63`; the first 6000 are the digests the label gave);
  with the label text stripped from every argv all 64 block digests of the time equal the ones committed before it.
- **Constant-power crossfades** (`afade …:curve=qsin` only on a fade that is wholly one half of a real
  pair: a dissolve or motion transition from an adjacent clip whose overlap plays different sound — not a
  fade up from black, not a dip, not a cut inside one take) re-blessed `export.txt` alone: 1304 of 6400
  export cases moved (`KERF_GOLDEN_CASES` before / after, with the curve compiled out for the before —
  which also equals the committed digests), exactly the new `audio-crossfade-qsin` family
  (`KERF_GOLDEN_FAMILIES`); no still or preview digest moved. `MasterBus.true_peak` and
  `ExportOptions.loudness_static` landed in the same change and move nothing (both off in every case).
