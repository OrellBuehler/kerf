# GPU-1, option B: the BT.709 label in the graph builders (rule R)

Branch `gpu-1b` in `/home/user/kerf-gpu-1`, from origin/main 255ac36, eight commits, not pushed:

```
d281c40 read a proxy's source once per encode and treat a reserved matrix as none      (ported from gpu-1: 9dc9625, the part that is not the label)
635d8c0 label untagged hd clips bt709 beside footage that names a matrix                (the rule, the plan mirror, the golden family, the unit tests)
10f7b45 add parity cases for untagged hd stacks under the bt709 label                   (kerf-gpu parity.rs + motion_parity.rs, gpu.md)
3424edb document the bt709 label, its plan mirror and the golden family                 (engine-cli.md, core-render-plan.md, core-golden.md)
        review round 1
14bc907 name no matrix from a decoder's default tag                                      (B1: model.rs, test_support.rs, bt709_label.rs, golden.rs)
3fdb3e5 say when the proxy path test has nothing to check                                (nit)
3beda52 add a parity case for decoder default tags beside untagged hd                   (kerf-gpu parity.rs)
a9da70b document decoder default tags and the handover slice difference                  (docs; S1)
```

## The rule

An **untagged HD YCbCr input is labelled `setparams=colorspace=bt709` when another input of the same graph names a matrix**:
HDR (the graph tone-maps it to a tagged BT.709 picture), or a tag that is **the file's word**. Nothing else is touched: a single clip,
a graph with no matrix-naming input, SD, MJPEG (`yuvj…`), RGB, grey, alpha and still images are built exactly as before, and
**proxies are not labelled** (no proxy key moves, nothing is re-encoded; the preview's graph keeps the original's metadata and
so labels exactly as the export's).

**What does not name a matrix (review B1).** Decoders write a tag of their own on files that say nothing; probed with
`Project::probe_asset` on FFmpeg 6.1.1 and 9.0.2 (`probe-shapes.txt`, 22 real files): every PNG (`rgb24`, `rgba`, `gray`) and RGB
video is tagged `gbr`; a JPEG, a WebP and MJPEG video (`yuvj420p` / `yuvj422p`) are `bt470bg`. Everything else probed (H.264, H.264 `yuvj`, ProRes, VP9, AV1, FFV1, MPEG-2, DV, raw, GIF, BMP, TIFF) is untagged unless the file says. So `StreamInfo::names_a_matrix`
is HDR, or a stated tag that is not `gbr`, on a stream that is not a still image, not MJPEG / `yuvj…` and not RGB. The rule is
symmetric: what is never labelled (still image, `yuvj…`/MJPEG, RGB, grey, alpha) never labels. The cost, deliberately: an HD clip
beside a camera's *explicitly* tagged full-range H.264 is left as FFmpeg reads it, which is what it always was.

* **HD** is mpv's rule, `width >= 1280 || height > 576`, on the **original's** displayed size, never on a proxy's. The plan reads the original too.
* **Where in the chain**: right after `setsar=1`, after the fit `scale` and the clip's own zoom, ahead of `fps`, the grade, the opacity round trip
  and the terminal format. Never the `-colorspace` output option. After the geometry is closer to the footage's colours beside a BT.601 neighbour than
  a tag at the head of the chain (42.7 against 38.7 dB) and identical beside BT.709 neighbours (the stacks the GPU draws).
* **One table** decides it for the graph builders and the planner: `ClipFx.label_bt709` (`transition_fx` → `graph_bt709_labels` → `model::bt709_labels`).
  The export, the levels meter, the playback stream and the scrub window decide over the clips of their own cut; the still over the layers on screen.
* **The plan mirrors it** (`PlanStream::label_bt709`; `matrix()` is BT.709 for a labelled layer under every policy): graph-wide for a Motion plan, the
  layers on screen for a Still plan. Pinned by a test over every pairing of 18 kinds of footage × 3 policies.

## Review round 1

**B1 (blocking) fixed, 14bc907.** `names_a_matrix` fired on decoder-default tags, so an ordinary "HD clip + logo / photo / webcam clip" project was labelled
and moved (HD under an rgba PNG 30.7 dB, under a WebP 31.1, under a JPEG 28 to 34, over MJPEG 32.9; the export's VUI went unset → `bt709`; HD under WebP or MJPEG went
from drawn to refused on the GPU). It slipped through because the golden pool's stills carry no tag and the unit kit modelled PNG as untagged `rgb24` and MJPEG as untagged.
* The free function is now `matrix_tag_is_stated` (a stated tag, `gbr` included: the *subject* check "untagged" still reads it so); the method is `StreamInfo::names_a_matrix`.
* The unit kit (`test_support::decoder_default_pictures`) and the model tests carry the real probe shapes: rgba and grey PNG `gbr`, rgb24 PNG, a JPEG and a WebP `bt470bg` (`image`), MJPEG video `yuvj420p`/`yuvj422p` `bt470bg`, FFV1 `bgr0` and H.264 `gbrp` `gbr`; each beside untagged HD, each way round, in all four builders, asserting no label (and the same graph as the tag-stripped twin).
* A rendered `#[ignore]` test makes eight real files, probes them, and holds every builder's graph equal to the tag-stripped twin's.
* **The golden universe catches the class** without moving a digest: `a_decoders_default_tag_names_no_matrix_in_any_graph_of_the_corpus` builds every case that holds an HDR clip again with that clip replaced (same size, same place) by each shape, and requires the label in no argv; the controls are explicitly tagged H.264 (`bt709`, `smpte170m`), which must label exactly where the HDR clip did. With the old rule it fails with 3698 disagreements. The golden counts and digests are unchanged (3054 / 644 / 2139; no re-bless).
* Re-measured with real files (below): argv **byte-identical to main** for JPEG, WebP, rgba PNG, grey PNG, rgb24 PNG and MJPEG beside untagged HD, all four ffmpeg runs of each case, orig and prev, on both FFmpegs (a logging `KERF_FFMPEG` wrapper, `tools/ffwrap.sh`, `argv-diff.py`); the pixels equal; the GPU plan's verdict unchanged under every policy.

**S1 (documentation only).** `core-render-plan.md` and `gpu.md` now state that a Motion plan's graph-wide decision can differ from the handover stream's slice (`SpanPlan::handover`):
accepted, decision 3. Opaque pictures are identical; a translucent HD layer or a dip to white differs by up to 2 levels on 9.0.2, 10 to 16 on 6.1.

**S2: the 6.1 uniform label, re-checked.** After B1 a project moves on 6.1 only if it holds **untagged HD footage and a matrix-naming input** (HDR, or a clip whose tag is the file's word: a phone's BT.709, a camera's BT.709 or `smpte170m`; not a logo, a photo or a webcam) **and** an HD layer that takes the RGB round trip: a translucent one (opacity below 1) or a dip to white. Measured on 6.1.1 (`export-label-vs-main-ffmpeg-6.1.1.txt`), HD beside a BT.709 clip: opaque, picture-in-picture, coloured text, fade to black, grade, rotation, mask, vignette and dissolve are **99 dB / 0** (byte-identical); translucent HD over the BT.709 clip 39.2 dB / 10 levels (the still 37.3 / 16); an HD clip dipping through white 31.5 dB / 11 (export). A translucent *BT.709* layer over HD, and everything with a logo, photo, SD or RGB, does not move. In the synthetic golden corpus, which draws opacity freely, 2533 of the 3054 labelled graphs hold a translucent or dip-to-white labelled clip; real projects that mix untagged HD with tagged video *and* fade an HD layer through RGB are a small subset of "untagged HD plus a tagged clip". The direction of the shift is towards the BT.709 reading a tagged layer already gets. Decision 2 below stands for you to confirm.

**Nits.** The proxy-path test now says `skipped: …` (the repo's `skip` helper); the two `names_a_matrix` functions are `matrix_tag_is_stated` and `StreamInfo::names_a_matrix`; `Asset::hdr()` is the first video stream's, like every graph input (`[i:v]`) and the label (it was any stream's: a cover-art second stream could have tone-mapped the first; unit-tested).

## Decisions

| Question | Decision | Why |
| --- | --- | --- |
| Where HD is measured | the original's displayed size (mpv's rule) | the preview decodes proxies whose size is a Settings choice; the builders and the plan must agree with the export |
| FFmpeg 6.1 (`FixedBt601`) | **applied uniformly**; documented | the builders cannot ask for the policy: `composite_color_policy()` is measured *by running `build_still_args`* while holding its own mutex (a builder that asked would deadlock), and a read that does not probe answers differently before and after the first probe (the still cache keys on the argv). Cost: see S2. |
| Still images | never labelled, never naming | a lossy WebP is BT.601 by its format; their tags are the decoder's |
| `gbr` | names nothing, on any layout | a PNG's default; an RGB picture has no YCbCr matrix to name |
| `reserved` matrix | no tag | FFmpeg reads it as none (measured; ffprobe 6.1.1 prints `reserved`, 9.0.2 `unknown`) |
| A stream / window / ranged export that starts past the tagged clip | decides over its own graph | negotiation is per graph; only a translucent HD layer or a dip differs (S1) |

### Ported from gpu-1
The proxy-traits race fix (`generate_proxy_with` reads `source_traits` once; test `an_encode_names_its_proxy_from_the_traits_it_was_given`), the `reserved`-matrix nit, the parity-case ideas (`HdMedia`, `Proxies`, adapted).
### Not ported
The proxy label and its cache key; the superseded-proxy removal and its Windows delete retry (not independent of the label: a "superseded" proxy exists only because the label moved the key, so with no label there is nothing to retry; the tracker items on it and on hardware proxy encoders carrying the tag are moot); `labelling_a_proxy_moves_no_pixel…`.

## Numbers

Method: kerf-core's own `export_still`, `render_with` (lossless x264), `stream_preview` and `Project::composite_timeline_frame` on 1280x720 clips and real files
(`tools/mkmedia.sh`, `tools/mkshapes.sh`), 640x360 canvas, via `tools/gpu1b_explore.rs`; PSNR / worst level error by `tools/cmp.py`. "main" is origin/main 255ac36
**built and run** (a worktree and a separate target dir; sharing a target dir between the two silently links one lib into both binaries). FFmpeg 9.0.2 is the pinned
build (policy `BottomLayerTag`), 6.1.1 the system one (`FixedBt601`).

### Golden (`golden-counts.txt`; unchanged by round 1)
Of 6000 cases the label moves **3054 export, 644 still, 2139 preview** digests (2852 / 610 / 1993 of the first 5600; the prototype's R was 2879 / 613 / 2026 on an older corpus,
reproduced exactly for export and preview by labelling still images too). The moved set equals the label's argv set equals an independent by-name oracle's set; stripping
`,setparams=colorspace=bt709` from the new argv gives the old digest of all 18000 (case, builder) pairs.

### What the label moves (`export-label-vs-main-ffmpeg-{9.0.2,6.1.1}.txt`), export yuv, PSNR dB / worst level
| stack (subject = untagged HD) | 9.0.2 | 6.1.1 |
| --- | --- | --- |
| single clip: opaque, pip, translucent, coloured text | 99 / 0 | 99 / 0 |
| **HD with a real JPEG, WebP, rgba / grey / rgb PNG, MJPEG clip, RGB capture, alpha clip, untagged SD** (either order) | **99 / 0** | **99 / 0** |
| HD over / under a BT.709 clip | 35.3 / 14, 29.7 / 36 | 99 / 0 |
| ... with a coloured caption / a vignette / a rotated pip | 35.3 / 14, 37.3 / 14, 24.8 / 37 | 99 / 0 |
| ... a masked pip / a grade / a dissolve into the HD clip | 24.0 / 37, 31.6 / 21, 23.6 / 37 | 99 / 0 |
| ... HD translucent over BT.709 | 38.0 / 11 | **39.2 / 10** |
| ... a dip to white into the HD clip | 44.1 / 3 | **31.5 / 11** |
| HD with an HLG clip (over, under) | 35.3 / 14, 32.0 / 26 | 99 / 0 |
| HD over SD / HD tagged smpte170m | 34.0 / 17 | 99 / 0 |
| BT.709 clip then HD, never on screen together (graph-wide) | 29.3 / 14 (the still: 99 / 0) | 99 / 0 |

On 9.0.2 these are the conversions FFmpeg no longer makes (main read the HD clip as BT.601 and converted it into its neighbour's matrix). The pip region against the colours
the footage was made from (`colour-vs-the-footage-ffmpeg-9.0.2.txt`): **28.1 dB on main, 42.7 labelled**, 42.7 for the same footage tagged BT.709; over a real JPEG 27.4 dB on both builds (unchanged).
The label equals the tag to the bit in all four builders for every stack the GPU draws (rendered test, both FFmpegs).

### Argv identity (`argv-identity-ffmpeg-{9.0.2,6.1.1}.txt`)
Every ffmpeg run of a case (export still, composite still, export, stream), main against the branch, the output directory normalised, over the 39 cases × (originals, proxies): **46 pairs identical, 32 differ, and every difference is exactly the inserted `,setparams=colorspace=bt709`** on both FFmpegs. The identical ones include all of single clips and the real-file shapes, orig and prev.

### Preview against export (`preview-assets-vs-originals.txt`)
Preview assets (proxies, the original's metadata) against the originals: with the label the figures are what they were on main (export 52 to 64 dB, still 45 to 57, stream 44 to 57; HLG rows 36 to 41 are the proxy's tone-map against the graph's, unchanged). Option B moves no proxy.

### GPU (`gpu-plan-verdicts-before-after.txt`, `parity-new-cases.txt`)
The 35-case battery of stacks (untagged HD over / under BT.709, HLG, SD, SD tagged, HD tagged, RGB, alpha, **JPEG, WebP, three PNGs, MJPEG**, effects, sequential), a Still plan:

| | main → label (drawn of 35) | refused → drawn | drawn → refused |
| --- | --- | --- | --- |
| 9.0.2, the previews' proxies | 11 → 17 | 8 (HD with BT.709 or HLG, both orders, plain / translucent / graded / rotated) | 2 (HD with `smpte170m` SD / HD) |
| 9.0.2, originals | 11 → 15 | 6 (HLG originals stay refused: the HDR cap) | 2 |
| 6.1.1 | 22 → 22 | 0 | 0 |
| unmeasured (`Unknown`) | 11 → 9 | 0 | 2 |

**The 14 real decoder-default cases (JPEG, WebP, rgba / grey / rgb PNG, MJPEG, both orders) have the same verdict as main under every policy, orig and prev.**
New parity rows: 21 on 9.0.2 (641 → 662), 28 on 6.1.1 (681 → 709); drawn ones 43.6 to 49 dB flat, worst level 5. Motion: 3 cases, Y ≤ 2 / UV ≤ 2 on both FFmpegs.

## Test results (Mesa lavapipe, `KERF_HWACCEL=none`, `KERF_HW_ENCODE=none`)
* `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --no-default-features --locked --color never -- -D warnings`: exit 0 (my round-0 grep of the coloured output hid errors; the exit code is what counts).
* `cargo test -p kerf-core --no-default-features`: 1310 passed (main 1289; +21), 115 ignored; golden passing, no re-bless.
* kerf-core `-- --ignored --skip downloads_a_real_model --skip synthesizes_a_script_end_to_end` (the latter is the sandbox's certificate error downloading the ONNX runtime, unrelated): **113 passed, 0 failed on 9.0.2 and on 6.1.1**, the three `bt709_label` rendered tests included.
* kerf-gpu `-- --ignored`, all targets, on 9.0.2 and on 6.1.1: everything passes (parity 48, motion_parity 24, frame_source 17, compositor 16, playback 12, …).
* kerf-app `--no-default-features`: 175 passed. No frontend file changed.

## Needs a real machine
* A real GPU adapter and arm64 / macOS (all numbers are lavapipe, x86-64). FFmpeg builds other than 6.1.1 and 9.0.2.
* The app itself with a mixed stack, and with a project of HD clips and a logo / photo (not launched here).
* Real cameras' files: the decoder-default list is from ffprobe on synthetic files made with this FFmpeg's encoders; a phone's HEVC / H.264 with an odd tag would name a matrix (correctly, it is the file's word).

## Needs a maintainer decision
1. **Untagged HD beside explicitly tagged `smpte170m` / `bt470bg` / `bt2020nc` footage** (a DVD rip, an SD camera that tags itself; not a JPEG, WebP, MJPEG or PNG any more) is a mixed stack: the export moves (34 dB; the HD clip's colours become right, 28.0 → 42.7 dB) and the GPU refuses it on 9.0.2, where main drew it as BT.601 throughout. Rule R as given; restricting the naming input to `bt709` / HDR would keep those exports byte-identical and drawn.
2. The 6.1 uniform label (S2): confirm, or thread the policy into the builders (and give the probe a policy-free still builder).
3. Slices (stream, scrub window, ranged export, the handover stream) decide over their own graph: accepted (S1).
4. Tracker: the old `origin/gpu-1` branch can go; GPU-1's items on hardware proxy encoders and a Windows delete retry are moot; "untagged HD originals still read as BT.601 on 9.0.2 until their proxy lands" now applies only to a lone untagged HD clip.

## Files
`golden-counts.txt`, `export-label-vs-main-ffmpeg-{9.0.2,6.1.1}.txt`, `argv-identity-ffmpeg-{9.0.2,6.1.1}.txt`, `preview-assets-vs-originals.txt`, `colour-vs-the-footage-ffmpeg-9.0.2.txt`,
`gpu-plan-verdicts-before-after.txt`, `parity-new-cases.txt`, `probe-shapes.txt`; `tools/`: `mkmedia.sh`, `mkshapes.sh` (the files), `gpu1b_explore.rs` (the harness; copy into
`crates/kerf-core/examples/`, once per checkout, each with its own target dir), `ffwrap.sh` + `argv-diff.py` (argv logging and comparison), `cmp.py` (the tables), `probe_shapes.rs` (the probe survey).
