# GPU-1, option B: the BT.709 label in the graph builders (rule R)

Branch `gpu-1b` in `/home/user/kerf-gpu-1`, from origin/main 255ac36, four commits, not pushed:

```
d281c40 read a proxy's source once per encode and treat a reserved matrix as none      (ported from gpu-1: 9dc9625, the part that is not the label)
635d8c0 label untagged hd clips bt709 beside footage that names a matrix                (the rule, the plan mirror, the golden family, the unit tests)
10f7b45 add parity cases for untagged hd stacks under the bt709 label                   (kerf-gpu parity.rs + motion_parity.rs)
3424edb document the bt709 label, its plan mirror and the golden family                 (engine-cli.md, core-render-plan.md, core-golden.md)
```
(`gpu.md` is in 10f7b45 with the cases it describes.)

## The rule

An **untagged HD YCbCr input is labelled `setparams=colorspace=bt709` when another input of the same graph names a matrix**
(a tag other than `unknown` / `unspecified` / `reserved`, or HDR, which the graph tone-maps to a tagged BT.709 picture).
Nothing else is touched: a single clip, a graph with no matrix-naming input, SD, MJPEG (`yuvj…`), RGB (even one a container
tags `gbr`), grey, alpha and still images are built exactly as before, and **proxies are not labelled** (no proxy key
moves, nothing is re-encoded; the preview's graph keeps the original's metadata and so labels exactly as the export's).

* **HD** is mpv's rule, `width >= 1280 || height > 576`, on the **original's** displayed size, never on a proxy's (a small
  proxy size in Settings makes a 1080p proxy 640x360). The plan reads the original too (`AssetFacts::original`).
* **Where in the chain**: right after `setsar=1`, after the fit `scale` and the clip's own zoom, ahead of `fps`, the
  grade, the opacity round trip and the terminal format. Never the `-colorspace` output option. Measured (below), after
  the geometry is closer to the footage's colours beside a BT.601 neighbour than a tag at the head of the chain
  (42.7 against 38.7 dB) and identical beside BT.709 neighbours (the stacks the GPU draws).
* **One table** decides it for the graph builders and the planner: `ClipFx.label_bt709` (`transition_fx` →
  `graph_bt709_labels` → `model::bt709_labels`). The export, the levels meter, the playback stream and the scrub window
  (`build_filter_complex`) decide over the clips of their own cut; the still (`build_still_args`) over the layers on
  screen (`StillLayer.label_bt709`).
* **The plan mirrors it** (`PlanStream::label_bt709`; `matrix()` is BT.709 for a labelled layer under every policy):
  graph-wide for a Motion plan (a BT.709 clip that is not on screen at this frame still labels the HD clip, as the export
  graph does), the layers on screen for a Still plan. Pinned by a test over every pairing of 11 kinds of footage × 3
  policies (`an_export_plan_labels_the_clips_the_export_graph_labels`, `a_still_plan_labels_the_layers_the_still_graph_labels`).

## Decisions

| Question | Decision | Why |
| --- | --- | --- |
| Where HD is measured | the original's displayed size (mpv's rule) | the preview decodes proxies whose size is a Settings choice; the graph builders and the plan must agree with the export |
| FFmpeg 6.1 (`FixedBt601`) | **applied uniformly**; documented | the builders cannot ask for the policy: `composite_color_policy()` is measured *by running `build_still_args`* while holding its own mutex (a builder that asked would deadlock), and a read that does not probe answers differently before and after the first probe (the still cache keys on the argv). The cost on 6.1: a translucent untagged-HD layer of a mixed stack moves 37 to 39 dB / up to 16 levels (towards the BT.709 reading a tagged layer already gets); opaque pictures do not move. The plan mirrors it (a translucent labelled layer leaves YUV with BT.709). |
| Still images | never labelled | a lossy WebP is BT.601 by its format; this is the 27 / 33 difference from the prototype's golden counts |
| RGB tagged `gbr` | names no matrix | an RGB picture has no YCbCr matrix to name; keeps RGB-with-HD stacks as they were |
| `reserved` matrix | no tag (`known_color`, `names_a_matrix`, `PlanStream::matrix`) | FFmpeg reads it as none (measured on 9.0.2 and 6.1.1: the overlay result equals the untagged clip's; ffprobe 6.1.1 prints `reserved`, 9.0.2 prints `unknown`) |
| A stream / window / ranged export that starts past the tagged clip | decides over its own graph | negotiation is per graph; the difference is only a translucent HD layer or coloured text (up to 2 levels on 9.0.2, 10 to 16 on 6.1) |

### Ported from gpu-1
* The proxy-traits race fix (`generate_proxy_with` reads `source_traits` once and gives them to `proxy_path_for`, the
  tone-map and the pad). It applies to main: a probe that failed once is remembered for a minute, an encode that waited in
  the proxy lane could read again and answer differently, and write a plain picture under the tone-mapped or padded name
  (or the reverse) and keep it. New test: `an_encode_names_its_proxy_from_the_traits_it_was_given`.
* The `reserved`-matrix nit (`known_color`, `PlanStream::matrix`, rows in the existing tests).
* The parity-case ideas: `HdMedia`, `Proxies`, the HLG / SD / RGB / PNG stacks, adapted (a proxy is no longer labelled).

### Not ported
* The proxy label, its cache-key change (`|bt709`), `label_bt709` in `SourceTraits`, `BT709_LABEL` in `build_proxy_args`.
* The **superseded-proxy removal and its Windows delete retry** (`superseded_proxy_path_for`, the retry in
  `generate_proxy_with`). The brief called it independent of the label; reading the code it is not: a "superseded" proxy
  exists only because the label moved the cache key. With no label nothing is superseded and there is nothing to retry.
  (Tracker items "Windows deleting a superseded proxy held open by a decoder" and "hardware proxy encoders carry the
  setparams tag into the VUI" are moot under option B.)
* `labelling_a_proxy_moves_no_pixel…` and the parity rows about the proxy colour target.

## Numbers

Method: kerf-core's own `export_still`, `render_with` (lossless x264), `stream_preview` and `Project::composite_timeline_frame`
on 1280x720 gradient and test-card clips (`tools/mkmedia.sh`), 640x360 canvas, via `tools/gpu1b_explore.rs`; PSNR / worst level
error by `tools/cmp.py`. "main" is the same tree with the label switched off (equivalently, run the harness from origin/main).
FFmpeg 9.0.2 is the pinned build (policy `BottomLayerTag`), 6.1.1 the system one (`FixedBt601`).

### Golden (`golden-counts.txt`)
Of 6000 cases the label moves **3054 export, 644 still, 2139 preview** digests (2852 / 610 / 1993 of the first 5600; the
prototype's R was 2879 / 613 / 2026 on an older 5600-case corpus, and its export and preview figures are reproduced exactly by
labelling still images too). Per case, for every builder: the moved set equals the set whose argv holds the label, which equals
the set an independent by-name oracle says holds untagged HD beside an HDR clip (so no single-clip, untagged-only, HDR-only
or audio-only case moved: that check is part of the oracle now), and stripping `,setparams=colorspace=bt709` from the new argv gives the old
digest of all 18000 (case, builder) pairs. One new family: `bt709-label`, `still-bt709-label`, `preview-bt709-label`.

### What the label moves (`export-label-vs-main-ffmpeg-9.0.2.txt`, `…-6.1.1.txt`), export yuv, PSNR dB / worst level
| stack (subject = untagged HD) | 9.0.2 | 6.1.1 |
| --- | --- | --- |
| single clip: opaque, pip, translucent, coloured text | 99 / 0 | 99 / 0 |
| HD with untagged SD, PNG logo, RGB, MJPEG, alpha (either order) | 99 / 0 | 99 / 0 |
| HD over / under a BT.709 clip | 35.3 / 14, 29.7 / 36 | 99 / 0 |
| ... HD translucent over BT.709 / BT.709 translucent over HD | 38.0 / 11, 34.4 / 17 | **39.2 / 10**, 99 / 0 |
| HD with an HLG clip (over, under) | 35.3 / 14, 32.0 / 26 | 99 / 0 |
| HD over SD / HD tagged smpte170m | 34.0 / 17 | 99 / 0 |
| BT.709 clip then HD, never on screen together (graph-wide) | 29.3 / 14 (the still: 99 / 0) | 99 / 0 |

On 9.0.2 these are the conversions FFmpeg no longer makes (main read the HD clip as BT.601 and converted it into its neighbour's
matrix). The picture-in-picture region against the colours the footage was made from (`colour-vs-the-footage-ffmpeg-9.0.2.txt`):
**28.1 dB on main, 42.7 dB labelled**, 42.7 dB for the same footage tagged BT.709 in the file. The label equals the tag to the
bit in all four builders for every stack the GPU draws (`a_labelled_input_renders_as_a_file_tagged_bt709_is_in_all_four_builders`:
export, still, stream, scrub window; opaque and translucent; both FFmpegs).

### Preview against export (`preview-assets-vs-originals.txt`)
Preview assets (proxy files, the original's metadata) against the originals, same timeline: with the label the figures are
what they were on main (export 52.5 to 64 dB, still 45 to 57, stream 44 to 57; the HLG rows 36 to 41 dB are the proxy's
own tone-map against the graph's, unchanged). Option B moves no proxy, so a mixed stack's preview and export are decided
from the same facts. (`a_mixed_stack_of_proxies_is_the_stack_of_the_originals` asserts export > 48, still > 44, stream > 42 dB.)

### GPU (`gpu-plan-verdicts-before-after.txt`, `parity-new-cases.txt`)
The 16-stack battery (untagged HD over / under BT.709, HLG, SD, SD tagged, HD tagged, RGB, PNG, MJPEG, alpha, sequential), a Still plan:

| | main → label (drawn of 16) | refused → drawn | drawn → refused |
| --- | --- | --- | --- |
| 9.0.2, the previews' proxies | 5 → 9 | 6 (HD with BT.709 or HLG, both orders, plain and translucent) | 2 (HD with `smpte170m` SD / HD) |
| 9.0.2, originals | 5 → 7 | 4 (HLG originals stay refused: the HDR cap) | 2 |
| 6.1.1 | 12 → 12 | 0 | 0 |
| unmeasured (`Unknown`) | 5 → 3 | 0 (a BT.709 stack is still refused, for a new reason) | 2 |

New parity rows: 18 on 9.0.2 (641 → 659), 25 on 6.1.1 (681 → 706); the drawn ones 43.6 to 47.9 dB flat, worst level 5 (7 whole-image), the refused
ones asserted refused by the plan on 9.0.2 and compared strictly on 6.1.1. Motion: 3 cases, Y ≤ 2 / UV ≤ 2 on both FFmpegs.
Negative controls (by hand): reading the labelled layer as BT.601 again fails the translucent case on 6.1.1 (37.4 dB, 15 levels as a Still; 8 levels of Y as
a Motion plan) and the stacks become refused on 9.0.2.

## Test results (Mesa lavapipe, KERF_HWACCEL=none, KERF_HW_ENCODE=none)
* `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --no-default-features --locked -- -D warnings`: clean.
* `cargo test -p kerf-core --no-default-features`: 1309 passed (main: 1289; +20, 2 more ignored), golden passing after the bless.
* kerf-core `-- --ignored --skip downloads_a_real_model` on 9.0.2: 112 passed, 1 failed: `engine::tts::tests::synthesizes_a_script_end_to_end`
  (`could not download voice runtime … invalid peer certificate`: the sandbox, unrelated); the integration tests' ignored ones pass.
  The two new rendered tests also pass on 6.1.1.
* kerf-gpu `-- --ignored` on 9.0.2 and on 6.1.1: everything passes (parity 47, motion_parity 24, frame_source 17, playback 12, compositor 16, …).
* kerf-app `--no-default-features`: 175 passed. No frontend file changed, so no `bun run check` / `bun run test`.

## Needs a real machine
* A real GPU adapter (all numbers are lavapipe) and arm64 / macOS: the new cases use the same kernels as the existing rows, but nothing here ran off x86-64.
* FFmpeg builds other than 6.1.1 and 9.0.2 (7.x, 8.x, distro builds): the label's effect on negotiation is measured only on those two; `composite_color_policy` is the proxy for the others.
* Opening the app (not done here: headless only) with a mixed stack: that the preview now shows the HD clip's BT.709 colours at once, before and after its proxy lands.

## Needs a maintainer decision
1. **Untagged HD beside `smpte170m` / `bt470bg` / `bt2020nc` footage** (a DVD rip, an SD camera that tags itself) is now a mixed stack: the export moves (34 dB, but the HD clip's colours become right: 28.0 → 42.7 dB against the footage) and the GPU refuses it on 9.0.2, where main drew it as BT.601 throughout. This is rule R as given ("an explicit tag"); restricting the naming input to `bt709` / HDR would keep those exports byte-identical and keep them drawn.
2. **The 6.1 translucent shift** (above) is accepted by applying the label uniformly; the alternative needs the builders to take the policy as an argument everywhere (and the probe to call a policy-free still builder).
3. Slices (stream, scrub window, ranged export) decide over their own graph; pinning the cut's decision into a slice would need a new input on those builders (the difference is up to 2 levels on 9.0.2).
4. Tracker: GPU-1's old items on hardware proxy encoders and a Windows delete retry are moot; "untagged HD originals still read as BT.601 on 9.0.2 until their proxy lands" is now only true of a lone untagged HD clip (27.3 dB against a player, as before), mixed stacks are labelled; branch `origin/gpu-1` can be deleted.

## Files
`golden-counts.txt`, `export-label-vs-main-ffmpeg-{9.0.2,6.1.1}.txt`, `preview-assets-vs-originals.txt`, `colour-vs-the-footage-ffmpeg-9.0.2.txt`,
`gpu-plan-verdicts-before-after.txt`, `parity-new-cases.txt`; `tools/`: `mkmedia.sh` (the clips), `gpu1b_explore.rs` (the harness, copy into
`crates/kerf-core/examples/`), `cmp.py` (the tables).
