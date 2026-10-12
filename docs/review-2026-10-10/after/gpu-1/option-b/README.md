# GPU-1, option B: the BT.709 label in the graph builders (rule R)

Branch `gpu-1b` in `/home/user/kerf-gpu-1`, from origin/main 255ac36, thirteen commits, not pushed:

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
        review round 2
4c783d0 key the naming exclusion on the decoder and cover each rule alone                (S1, N2 to N5: model.rs, test_support.rs, golden.rs, bt709_label.rs, parity.rs)
15f495c put the bt709 label first in the chain so a dip to white reads it                (found while testing S2: cli.rs, golden/*.txt re-blessed, tests)
06d2880 document the decoder-keyed exclusion, the label's place and the dip result        (S2, N1, N4, cosmetic, and the label's place: the four docs)
        review round 3
864608f exclude every decoder that tags what it decodes from naming a matrix              (jpegls grey and the decode-only mjpeg relatives: model.rs, test_support.rs, golden.rs, bt709_label.rs)
f1a910f state the dip and translucent figures by builder and by method                   (the dip / translucent numbers, re-measured over whole exports; decoder lists)
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
* **Where in the chain**: the **first filter of the clip's chain** (round 2 moved it there from behind the geometry, see below). Never the `-colorspace` output option.
  A label first equals the file tagged BT.709, to the bit, in every chain and all four builders; behind the fit `scale` it was not a tag when a dip to white's RGB
  conversion happened in that scaler. The price: beside a BT.601 neighbour the HD clip is 38.7 dB from the footage's colours (the tag's figure), where the first design had 42.7.
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

## Review round 2 and what it found

**S1, N1 to N5, cosmetic (4c783d0, 06d2880).** The naming exclusion is keyed on the **decoder**, not on `yuvj`: `DEFAULT_TAG_CODECS` = `mjpeg`, `amv`, `speedhq` (the decoders that tag everything they decode `bt470bg`; SpeedHQ's `yuv422p` is N4), and a still image, `gbr` and an RGB layout as before. So FFmpeg's h264 decoder reporting every full-range stream as `yuvj420p`
no longer silences an OBS / drone file's real `bt709` tag: `h264 yuvj420p pc bt709` names BT.709 and labels the untagged HD beside it (unit rows, the golden metamorphic control, a rendered real file, and a GPU parity case on both FFmpegs); `mjpeg yuv422p bt470bg` names nothing.
The `layout != Rgb` clause of `has_a_yuv_tag` has its own assert (`rgb24` + `bt470bg`, the `jpegls` shape). The metamorphic test replaces the HDR clip with 14 stand-ins, one rule each (a moving `rgba` `gbr`, MJPEG and SpeedHQ at `yuv422p`, AMV, `jpegls` `rgb24`, ...), their arrays sized by `STAND_INS.len()`.
The rule is documented as the principle "a tag a decoder writes by default is not the file's word", not as a mirror (grey, alpha and SD are never labelled but do name a matrix when tagged; intended asymmetry), and the "RGB video is tagged `gbr`" sentence says it depends on the encoder (FFV1 `bgr0` on 9.0.2 only).
Real files (`probe-shapes.txt`, now 13 in the rendered test): `ndi.mov` (speedhq 422), `jpegls.avi` (`rgb24` `bt470bg`), `capture-alpha.mkv` (ffv1 `bgra` `gbr`), `obs-full-range.mp4` (`h264 yuvj420p` `bt709`: labels 1 chain per builder).

**S2.** The 6.1 dip-to-white result is in the 6.1 paragraph of `engine-cli.md`, and the accepted-difference notes of `core-render-plan.md` and `gpu.md`. **The numbers were re-measured in the closing fixes** (below): the 31.5 dB / 11 levels of round 1's table, and the 32.3 dB / 11 I gave in round 2, were *one frame (frame 5)* of a 0.3 s + 0.7 s harness case, because `cmp.py`'s export column compares a single frame.

## The dip to white found a flaw in the label's place (15f495c)

My GPU motion test for a labelled HD clip dipping through white failed on both FFmpegs (the compositor Y 17 / UV 9 off the export on 9.0.2, Y 30 / UV 16 on 6.1.1, every frame of the clip, persisting after the fade) while the dip controls passed. It was not a test bug.
* `fade ... c=white` only takes RGB, so FFmpeg has the nearest scaler convert the picture: **the fit `scale` of the chain** (`scale=W:H:force_original_aspect_ratio=decrease` runs even when the size does not change). A label after the geometry therefore stated BT.709 for a picture already read as BT.601 on the way out of YUV, and the conversion back used BT.709. Measured on single chains (`tools/dip-chain-experiment.py <ffmpeg> W H`, frame 4 of the fade; `tools/dip-chain-all-frames.py` over the whole clip): label behind the geometry vs the file tagged BT.709 **34.1 dB / 8 (9.0.2), 28.6 / 15 (6.1.1)**; label first **99 dB / 0 on both**; untagged vs tagged 52 dB / 1 (9.0.2), 34 / 8 (6.1.1).
* In the real harness the label vs the tagged file differed only in the dip (every other mixed-stack case, fades, grades, rotations, masks, vignettes, dissolves, text and translucent layers, was 99 / 0 either way): export 30.9 dB / 12 (9.0.2), 25.9 / 21 (6.1.1).
  In isolation the label behind the geometry also left the dip's own round trip further from the tagged file than the untagged clip's is (34 dB against 52 on 9.0.2).
* **Fix**: the label is the first filter of the chain (ahead of `trim`) in `video_clip_chain` and `still_clip_chain`; nothing can have converted the picture before it, so it equals a tag in every chain, by construction.
* **Golden**: re-blessed once more, the same families. Per case: the moved set (new against the previous blessing) equals the previous labelled set exactly (3054 / 644 / 2139) and no other digest moved; with `setparams=colorspace=bt709,` stripped, the new argv digests equal **origin/main's for all 6000 cases in all three kinds** (checked by a temporary strip in `repr`, reverted). The metamorphic test passes.
* **Tests that fail on the old place** (mutation checked, both FFmpegs): the rendered `a_labelled_clip_dips_through_white_as_a_file_tagged_bt709_does` (into and out of untagged HD, export vs the tagged file: 99 dB / 0; with the label behind the geometry 32.0 dB / 17 on 9.0.2, 26.7 / 31 on 6.1.1); the unit test that the label starts the chain; the rendered label-equals-tag test now also runs a graded, rotated, vignetted and fading HD clip; GPU `a_labelled_hd_clip_dips_through_white_with_the_bt709_round_trip` (motion, `WHITE_709`: Y 2 / UV 2 on both) and `a_labelled_hd_layer_in_a_dip_to_white_is_drawn_as_the_layer_tagged_in_the_file` (a still mid-dip, strict; the labelled layer and the tagged one come out identical).
* **No plan change was needed**: the plan treated a labelled layer as BT.709 for the round trip; the export now agrees with it. (I considered refusing a labelled layer's dip in the plan instead; that would have left the export dip reading BT.601 in and BT.709 out.)
* **Cost, now measured** (`colour-vs-the-footage-ffmpeg-9.0.2.txt`): over SD tagged `smpte170m`, the one stack where the position mattered for the pixels, the label is 38.7 dB from the footage's colours (the tag's figure) where the old place gave 42.7; main 28.0.
  Beside a BT.709 clip nothing changed (42.7). The GPU refuses the `smpte170m` stack on 9.0.2 either way.

## Review round 3 (the closing fixes)

**1. `DEFAULT_TAG_CODECS` was incomplete (864608f).** `jpegls` writes `bt470bg` on `gray` and `gray16le` as well (probed on both FFmpegs: a real JPEG-LS grey file, `jpegls,gray,bt470bg`; grey is not the RGB layout, so only its `rgb24` was caught). The list is now `mjpeg`, `amv`, `speedhq`, `jpegls`, `mjpegb`, `sp5x`, `thp`, `smvjpeg` and `mxpeg`.
`mjpegb`, `sp5x`, `thp` and `smvjpeg` are decode-only (no encoder: no file to probe) and share `ff_mjpeg_decode_init`; **`mxpeg` is mine, not on the review's list**, listed on the same assumption (harmless if wrong: a codec that writes no tag has nothing to exclude). Say if you would rather drop it.
Tests: unit rows for all nine beside untagged HD (plus `jpegls` at `gray16le` / `yuv420p`, upper-case codec names), the probe-shape kit gains `jpegls gray` and `jpegls gray16le`, the metamorphic test gains `jpegls gray`, `thp` and an `rgb24` + `bt470bg` from FFV1 (the layout rule alone now that `jpegls` is on the list), the rendered real-files test makes two JPEG-LS grey files (15 files; both name nothing on both FFmpegs). With the old three-codec list the unit test, the kit-driven `nothing_is_labelled...` test and the metamorphic test (662 disagreements) all fail. No golden digest moves; the metamorphic test passes.

**2 and 3. The dip and translucent numbers (f1a910f).** They reproduce, and my 32.3 dB / 11 was a *single frame*: the export column of `tools/cmp.py` (and so every "export" figure in the older tables) compares frame 5 of the export. Re-measured over **every frame of the lossless export, all planes, worst level on any plane**, in the committed render scenario (1 s of BT.709 footage, 1 s of untagged HD, 640x360 at 10 fps, a 0.6 s dip), label on against the same argv with `setparams=colorspace=bt709,` removed (`tools/strip-label-ffmpeg.sh` as `KERF_FFMPEG`; `label-on-off-whole-export-ffmpeg-both.txt`):

| 6.1.1, label on against off | dB / worst level |
| --- | --- |
| dip into the HD clip (20 frames) | 33.0 / 15 |
| dip out of the HD clip | 32.5 / 15 |
| translucent HD, export (10 frames) / still / stream / scrub window | 40.7 / 8, 39.2 / 14, 39.7 / 21, 38.6 / 23 |

These are the review's numbers to the digit. The round trip is in the chain for every frame of the HD clip, so the 15 is on every frame where the picture is fully visible; the ramp's own frames read 0, 5, 11 (into) and 15, 11, 5 (out of). 9.0.2, same scenarios: dip into 33.1 / 16, out of 27.9 / 32, translucent 39.5 / 9, 38.2 / 14, 37.9 / 23, 38.1 / 24 (mostly the matrix negotiation the label settles, which moves the stack's other frames too).
The same numbers are in `engine-cli.md` (6.1 paragraph, with the method), `core-render-plan.md` and `gpu.md`. The stream-slice "accepted difference" is restated from the clip alone labelled against not (an untagged against the same footage tagged): translucent 1 level in the export on 9.0.2 and 8 to 14 on 6.1.1 (export 8, still 14, stream 11, scrub window 14), a dip 2 on 9.0.2 and 15 on 6.1.1, and **coloured text 16 levels on its own pixels on 9.0.2 and none on 6.1.1** (the old text of "2 / 10 to 16" was not reproducible and, for text, looks reversed; `label-on-off-whole-export-ffmpeg-both.txt`, second section).

**4.** The `names_a_matrix` doc comment is rewrapped.

## Decisions

| Question | Decision | Why |
| --- | --- | --- |
| Where HD is measured | the original's displayed size (mpv's rule) | the preview decodes proxies whose size is a Settings choice; the builders and the plan must agree with the export |
| FFmpeg 6.1 (`FixedBt601`) | **applied uniformly**; documented | the builders cannot ask for the policy: `composite_color_policy()` is measured *by running `build_still_args`* while holding its own mutex (a builder that asked would deadlock), and a read that does not probe answers differently before and after the first probe (the still cache keys on the argv). Cost: see S2. |
| Where the label sits | the first filter of the clip's chain (round 2; was behind the geometry) | equals a file tagged BT.709 in every chain; behind the fit `scale` a dip to white's RGB conversion happens before it |
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

### Golden (`golden-counts.txt`; re-blessed once more in round 2 for the label's place, the same cases)
Of 6000 cases the label moves **3054 export, 644 still, 2139 preview** digests (2852 / 610 / 1993 of the first 5600; the prototype's R was 2879 / 613 / 2026 on an older corpus,
reproduced exactly for export and preview by labelling still images too). The moved set equals the label's argv set equals an independent by-name oracle's set; stripping
`setparams=colorspace=bt709,` from the new argv gives the old digest of all 18000 (case, builder) pairs.

### What the label moves (`export-label-vs-main-ffmpeg-{9.0.2,6.1.1}.txt`), export yuv **at frame 5 (0.5 s)**, PSNR dB / worst level
| stack (subject = untagged HD) | 9.0.2 | 6.1.1 |
| --- | --- | --- |
| single clip: opaque, pip, translucent, coloured text | 99 / 0 | 99 / 0 |
| **HD with a real JPEG, WebP, rgba / grey / rgb PNG, MJPEG clip, RGB capture, alpha clip, untagged SD** (either order) | **99 / 0** | **99 / 0** |
| HD over / under a BT.709 clip | 35.3 / 14, 29.7 / 36 | 99 / 0 |
| ... with a coloured caption / a vignette / a rotated pip | 35.3 / 14, 37.3 / 14, 24.8 / 37 | 99 / 0 |
| ... a masked pip / a grade / a dissolve into the HD clip | 24.0 / 37, 31.6 / 21, 23.6 / 37 | 99 / 0 |
| ... HD translucent over BT.709 | 38.0 / 11 | **39.2 / 10** (frame 5; whole export 40.7 / 8, see below) |
| ... a dip to white into the HD clip | **31.9 / 11** (frame 5 of the harness case; was 44.1 / 3 with the label behind the geometry) | **32.3 / 11** (frame 5; the whole-export figures are below) |
| HD with an HLG clip (over, under) | 35.3 / 14, 32.0 / 26 | 99 / 0 |
| HD over SD / HD tagged smpte170m | 34.0 / 17 | 99 / 0 |
| BT.709 clip then HD, never on screen together (graph-wide) | 29.3 / 14 (the still: 99 / 0) | 99 / 0 |

On 9.0.2 these are the conversions FFmpeg no longer makes (main read the HD clip as BT.601 and converted it into its neighbour's matrix). The pip region against the colours
the footage was made from (`colour-vs-the-footage-ffmpeg-9.0.2.txt`): **28.1 dB on main, 42.7 labelled**, 42.7 for the same footage tagged BT.709; over SD tagged `smpte170m` 28.0 on main, **38.7 labelled** (= tagged; 42.7 with the label behind the geometry); over a real JPEG 27.4 dB on both builds (unchanged).
The label equals the tag to the bit in all four builders for every labelled stack, the dip included (rendered tests, both FFmpegs; `export-label-vs-main-*.txt` second section).

### Argv identity (`argv-identity-ffmpeg-{9.0.2,6.1.1}.txt`)
Every ffmpeg run of a case (export still, composite still, export, stream), main against the branch, the output directory normalised, over the 39 cases × (originals, proxies): **46 pairs identical, 32 differ, and every difference is exactly the inserted `setparams=colorspace=bt709,`** (now the first filter of the chain) on both FFmpegs. The identical ones include all of single clips and the real-file shapes, orig and prev.

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
New parity rows: 29 on 9.0.2 (641 → 670), 36 on 6.1.1 (681 → 717); drawn ones 43.6 to 49 dB flat, worst level 5 (the labelled-dip still: 99 / 0, 45.5 and 47.8 dB, worst 4, identical to the same footage tagged). Motion: 4 cases (three translucent, the dip), Y ≤ 2 / UV ≤ 2 on both FFmpegs.

## Test results (Mesa lavapipe, `KERF_HWACCEL=none`, `KERF_HW_ENCODE=none`), on the committed tree (f1a910f)
* `cargo fmt --all --check` clean; `cargo clippy --workspace --all-targets --no-default-features --locked --color never -- -D warnings`: exit 0.
* `cargo test -p kerf-core --no-default-features`: **1310 passed**, 116 ignored (main 1289 passed); the golden oracle and `a_decoders_default_tag_names_no_matrix_in_any_graph_of_the_corpus` pass.
* `bt709_label` `-- --ignored` (closing fixes): 4 passed, 0 failed on 9.0.2 and on 6.1.1 (15 real files, the dips, the proxies, label = tag).
* The full suites of the previous round (06d2880), before the closing fixes: kerf-core `-- --ignored --skip downloads_a_real_model --skip synthesizes_a_script_end_to_end` 114 passed on both FFmpegs; kerf-gpu `-- --ignored`, all targets, both FFmpegs: all pass (parity 50, motion_parity 25, frame_source 17, compositor 16, playback 12, ...); kerf-app 175 passed. The closing fixes touch a codec list and docs, not a graph, so I did not re-run the GPU suites.
* A trap worth knowing: a tree restored from a `tar` keeps old mtimes, so cargo reuses the previous build; touch the sources before trusting a run.

## Needs a real machine
* A real GPU adapter and arm64 / macOS (all numbers are lavapipe, x86-64). FFmpeg builds other than 6.1.1 and 9.0.2.
* The app itself with a mixed stack, and with a project of HD clips and a logo / photo (not launched here).
* Real cameras' files: the decoder-default list is from ffprobe on synthetic files made with this FFmpeg's encoders; a phone's HEVC / H.264 with an odd tag would name a matrix (correctly, it is the file's word).

## Needs a maintainer decision
1. **Untagged HD beside explicitly tagged `smpte170m` / `bt470bg` / `bt2020nc` footage** (a DVD rip, an SD camera that tags itself; not a JPEG, WebP, MJPEG or PNG any more) is a mixed stack: the export moves (34 dB; the HD clip's colours become right, 28.0 → 42.7 dB) and the GPU refuses it on 9.0.2, where main drew it as BT.601 throughout. Rule R as given; restricting the naming input to `bt709` / HDR would keep those exports byte-identical and drawn.
2. The 6.1 uniform label (S2): confirm, or thread the policy into the builders (and give the probe a policy-free still builder). On 6.1 it moves only an HD layer that takes the RGB round trip in a mixed stack: translucent (export 40.7 dB / 8, still 39.2 / 14, stream 39.7 / 21, scrub window 38.6 / 23) or a dip to white (33.0 dB / 15 into the HD clip, 32.5 / 15 out of it, whole export).
3. Slices (stream, scrub window, ranged export, the handover stream) decide over their own graph: accepted (S1).
4. The label's place changed in round 2 (first in the chain, equal to the tag; 38.7 instead of 42.7 dB beside a `smpte170m` neighbour, a stack the GPU refuses on 9.0.2 either way). Confirm, or choose the old place and refuse a labelled layer's dip to white in the plan (the export dip would then stay BT.601 in / BT.709 out).
5. Tracker: the old `origin/gpu-1` branch can go; GPU-1's items on hardware proxy encoders and a Windows delete retry are moot; "untagged HD originals still read as BT.601 on 9.0.2 until their proxy lands" now applies only to a lone untagged HD clip.

## Files
`golden-counts.txt`, `label-on-off-whole-export-ffmpeg-both.txt`, `export-label-vs-main-ffmpeg-{9.0.2,6.1.1}.txt`, `argv-identity-ffmpeg-{9.0.2,6.1.1}.txt`, `preview-assets-vs-originals.txt`, `colour-vs-the-footage-ffmpeg-9.0.2.txt`,
`gpu-plan-verdicts-before-after.txt`, `parity-new-cases.txt`, `probe-shapes.txt`; `tools/`: `mkmedia.sh`, `mkshapes.sh` (the files), `gpu1b_explore.rs` (the harness; copy into
`crates/kerf-core/examples/`, once per checkout, each with its own target dir), `ffwrap.sh` + `argv-diff.py` (argv logging and comparison), `cmp.py` (the tables), `probe_shapes.rs` (the probe survey), `dip-chain-experiment.py` and `dip-chain-all-frames.py` (the label-position experiment on single chains), `label-on-off-measure.rs.txt` + `strip-label-ffmpeg.sh` + `label-on-off-compare.py` + `label-alone-untagged-vs-tagged-compare.py` (the whole-export label on / off measurement of the closing fixes).
