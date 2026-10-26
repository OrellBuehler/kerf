//! The golden argv oracle: 8800 seeded timelines (4000, then 800 with a master bus, then 800
//! with per-property channels, then 400 with an AMF or VAAPI encoder, then 400 with animated SVG
//! titles, then 400 exported at several delivery frames in one run, then 400 with titles on tracks,
//! then 400 with a blurred fill, then 400 with a keyed mask or crop, then 400 with speed ramps, then
//! 400 with noise reduction) whose ffmpeg argv must not move.
//!
//! `build_export_args_phase`, `build_still_args` and `build_preview_args_with` are
//! built for every case and their argv reduced to a digest, checked against three
//! committed files (`golden/{export,still,preview}.txt`, one line per block of
//! [`BLOCK`] cases). A refactor of `video_clip_chain`, `audio_clip_chain`,
//! `transition_fx` or `build_filter_complex` has to leave all three untouched; an
//! intended change to one builder moves only its own file, so the other two keep
//! proving nothing else moved.
//!
//! **The master-bus family is appended, not interleaved.** Cases `0..`[`CASES`] are the
//! oracle as it was before the master bus existed and none of them has one; the
//! [`MASTER_CASES`] after them draw their timelines from the same generator and then
//! get a master bus ([`master_for`], no dice of its own). Adding the family therefore
//! moved no existing case — `KERF_GOLDEN_CASES` on the commit before and after agrees
//! on all of `0..CASES` — and only appended block lines to the three digest files.
//! **The channel family is appended the same way**: the [`CHANNEL_CASES`] after those draw
//! their timelines from the same generator and then get animation channels
//! ([`channels_for`], on dice of its own seeded by the case and the clip, so the generator's
//! are untouched): colour numbers, a volume, some transform numbers keyed beside the others,
//! and a legacy bundle with one number taken off it. The first 4800 per-case digests are the
//! ones the commit before channels existed gave.
//! **The hardware-encoder family is appended the same way**: the [`HW_CASES`] after those draw
//! from the same generator and then have their `video_codec` replaced by one of six AMF / VAAPI
//! encoders ([`hw_encoder_for`], no dice of its own); `av1_amf` and the three VAAPI encoders
//! are in none of the first 5600. What moved among those is only the export of a case already
//! holding `h264_amf` or `hevc_amf` (the recalibrated QP and `-quality quality`).
//!
//! **The animated-title family is appended the same way**: the [`SVG_CASES`] after those draw
//! their timelines from the same generator and then get SVG overlays ([`svg_for`], on dice of its
//! own seeded by the case, with the artwork fixed strings — a preset would put the machine's libm
//! in the key — and the cache directory pinned at `/kerf-test/svg`, `raster::with_pinned_dir`).
//! A few are forced to the gif container ([`svg_container_for`], no dice either). The first 6000
//! per-case digests are the ones the commit before titles existed gave; only 4 block lines were
//! appended to each file. Changing how a title is read (a document that cannot move is one looped
//! picture, a stream reads the title's own sequence from `-start_number`, a still is `-f image2
//! -pattern_type none`) moved those four lines (blocks 60..63) and nothing else.
//!
//! **Late-starting twins.** `retarget_late` (every nineteenth case, no dice) cuts the clips of
//! `interview` and `phone` from twins whose video starts after their sound (`StreamInfo::lead`),
//! half of them from the head of the file: the window read without a seek and the picture's
//! `+(start+delay)/TB` (`late-start-delay`). Only those 248 cases moved.
//!
//! **The multi-delivery family is appended the same way**: the [`TOGETHER_CASES`] after those draw
//! from the same generator and are then exported at two or three delivery frames by one run
//! ([`together_for`], on dice of its own): their export text is `together::build_together_args`
//! (its argv and where its outputs sit) instead of `build_export_args_phase`'s, half carry titles,
//! clips carry framings, a quarter mux captions and chapters, and the encode is made one the run
//! takes (no two-pass, a software encoder); a tenth keep an audio-only container, which no run
//! writes together. Only 4 block lines were appended to each file.
//!
//! **The title-clip family is appended the same way**: the [`TITLE_CASES`] after those draw their
//! timelines from the same generator and then get one or two titles on video tracks ([`title_clip_for`],
//! on dice of its own seeded by the case, the same fixed artworks): after the last clip of the lowest
//! video track, touching it under a transition or across a gap, or over the cut on a track of their own;
//! some head-trimmed, faded or placed. The first 6800 per-case digests are the ones the commit before
//! title clips gave; only 4 block lines were appended to each file.
//!
//! **The blurred-fill family is appended the same way**: the [`BLUR_CASES`] after those draw from
//! the same generator and then fit to the frame over a blurred copy of the picture ([`Fit::Blur`],
//! on the export's options and on the delivery frame when the cut has one; no dice, no titles). The
//! first 7200 per-case digests are the ones the commit before it gave; only 4 block lines were
//! appended to each file.
//!
//! **The keyed mask and crop family is appended the same way**: the [`MASK_CROP_CASES`] after the
//! blurred-fill ones draw from the same generator (and get the channel family's channels, but no
//! titles and one delivery frame) and then have numbers of their mask and the animated crop keyed
//! ([`mask_crop_for`], on dice of its own seeded by the case and the clip). The first 7600
//! per-case digests are the ones the commit before those numbers could be keyed gave; only 4 block
//! lines were appended to each file.
//!
//! **The speed-ramp family is appended the same way**: the [`RAMP_CASES`] after those draw from the
//! same generator (with the channel family's channels, one delivery frame, no titles) and then have
//! most of their clips ramped ([`ramp_for`], on dice of its own seeded by the case and the clip: one to
//! five keys, sixteen now and then, a hold here and there, a quarter played backwards, the track rippled
//! so clips that touched still touch and a transition keeps its tail). The first 8000 per-case digests
//! are the ones the commit before ramps gave; 4 block lines were appended to each file, `parts.txt`'s too
//! (`git diff` is additions only). Families `speed-ramp` (`setpts='st(0,`), `speed-ramp-linear`
//! (`sqrt((`), `speed-ramp-audio` (`acrossfade=d=`), their `preview-` forms, and the structural
//! `speed-ramp-hold`, `speed-ramp-reversed`, `speed-ramp-tail` (a ramped outgoing clip under a
//! transition) and `still-speed-ramp`.
//!
//! **The noise-reduction family is appended the same way**: the [`DENOISE_CASES`] after those draw
//! from the same generator (with the channel family's channels) and then have a `Denoise` put into the
//! audio chain of most of their clips ([`denoise_for`], on dice of its own seeded by the case and the
//! clip). The first 8400 per-case digests are the ones the commit before it gave; 4 block lines were
//! appended to each file, `parts.txt`'s too. Families `audio-denoise` (`afftdn=nr=`),
//! `audio-denoise-track` (`:tn=1`) and `audio-denoise-delay` (the pad that leads the delay's trim).
//!
//! **The blend-mode family is appended the same way**: the [`BLEND_CASES`] after the noise-reduction ones draw from the
//! same generator (with the channel family's channels) and then have some of their video clips composited with
//! a blend mode ([`blend_for`], on dice of its own seeded by the case and the clip, every mode but `normal`). The
//! first 8800 per-case digests are the ones the commit before blend modes gave; 4 block lines were appended to
//! each file, `parts.txt`'s too. Families `blend`, `still-blend`, `preview-blend` (`blend=c0_mode=`) and the two
//! names that differ from the mode's own, `blend-hardlight` (overlay) and `blend-addition` (add). Its blocks are
//! 88–91: they were 84–87 until it was merged after the noise-reduction family, its dice on a seed of their own
//! (`5 << 40`).
//!
//! **The stabilization family is appended the same way**: the [`STABILIZE_CASES`] after the blend-mode ones
//! draw from the same generator (with the channel family's channels, one delivery frame, no titles) and
//! then have most of their clips of footage stabilized ([`stabilize_for`], on dice of its own seeded by
//! the case and the clip: a strength, a fixed zoom now and then), every fourth case for an ffmpeg without
//! vid.stab (`deshake`). The analyses are pinned ([`pinned`]: a made-up one per asset of the pool, over
//! its whole length, measured at most 1920 wide as the detect pass measures, and slice files named rather
//! than written), so no disk and no libm reaches the argv. The first 9200 per-case digests are the ones
//! the commit before stabilization gave; 4 block lines were appended to each file, `parts.txt`'s too.
//! Families `stabilize-vidstab`, `stabilize-deshake`, `stabilize-proxy` (a padded proxy's slice, scaled to
//! its size: a clip of footage with a twin reads it now and then), `stabilize-4k` (a 4K original's slice,
//! scaled by 2 from the 1920 measured), `stabilize-hdr` (an HDR clip tone-mapped before it is steadied)
//! and their `still-` / `preview-` forms. Its blocks are 92–95, after the blend-mode family, its dice on a seed of their
//! own (`6 << 40`).
//!
//! **The export in parts has a file of its own** (`golden/parts.txt`, [`the_parts_argv_builders_produce_the_golden_digests`]):
//! every case is planned with `segments::plan_export` under [`PARTS_LIMITS`] and the argv of every part and of the last pass is
//! digested, so the three files above prove that a small export is unchanged and this one pins what a long one is made of.
//!
//! **Machine-independent by construction.** The builders read the machine in eight
//! places, each pinned: the preview's decode acceleration (the
//! `build_preview_args_with` seam), `zscale_available()` (`with_zscale`; a case
//! holding HDR footage is built *both* ways), `alimiter_latency_available()`
//! (`with_alimiter_latency`; every case is built with the option, and a case with the
//! limiter on has its export argv appended once more without it), `afftdn_delays()`
//! (`with_afftdn_delay`: every case is built as for an `afftdn` that delays, FFmpeg 5.1 on), `drawtext`'s
//! font path (`with_pinned_default_font`: the shipped default that an overlay with no
//! family gets, and the one family an overlay names, `PINNED_FAMILY`, are fixed paths, so
//! neither this machine's cache directory nor its installed fonts reach the argv),
//! whether the ffmpeg has vid.stab (`with_vidstab`) and the stabilization analyses on disk
//! (`stabilize::pinned`: made-up ones, slice files named and not written),
//! and libm: the
//! compressor and gate thresholds go through `powf`, whose last digits differ between
//! glibc builds (FMA or not), macOS, Windows and arm, so [`round_libm`] compares those
//! numbers to 10 significant digits, which still pins the dB mapping (a ramp's `atempo` stages and read
//! points are made without libm, `ramp::stage_tempo`, so they need no rounding). The digest files are
//! LF whatever the checkout (`.gitattributes`, and the comparison ignores `\r`).
//!
//! **Bless** after an intended argv change, from the repo root:
//!
//! ```text
//! KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden -- --nocapture
//! ```
//!
//! It rewrites all three files (and says so); `git diff` is the guard, and only the
//! files of the builders you changed should move. To find the case behind a failing
//! block, run the test on the base and on the change with `KERF_GOLDEN_CASES=<file>`
//! (one digest line per case) and diff the two files; `KERF_GOLDEN_DUMP=<case>` then
//! prints that case's argv (`KERF_GOLDEN_COVERAGE=1` prints the thinnest families, and
//! `KERF_GOLDEN_FAMILIES=<file>` writes the families each case hit, one line per case,
//! which is how to say *which kind* of case a moved digest belongs to).
//! The files are identical under any `KERF_HWACCEL` and
//! with `GLIBC_TUNABLES=glibc.cpu.hwcaps=-FMA,-FMA4`.
//!
//! **Coverage.** [`FAMILIES`] lists the branches the oracle exists to protect, each
//! as text that must appear in a case's argv, and the test fails if any family shows
//! up in fewer than [`MIN_PER_FAMILY`] cases (the generator stopped covering it, or
//! the argv text changed), so it cannot quietly stop covering one.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use uuid::Uuid;

use super::*;
use crate::model::{
    Easing, Keyframe, MasterBus, Property, PropertyKey, RampEasing, SpeedKey, TextKeyframe, Track, Transition, TransitionKind,
};

const CASES: usize = 4000;
/// The master-bus cases appended after the first [`CASES`] (see the module docs).
const MASTER_CASES: usize = 800;
/// The channel cases appended after the master-bus ones (see the module docs).
const CHANNEL_CASES: usize = 800;
/// The AMF / VAAPI encoder cases appended after the channel ones (see the module docs).
const HW_CASES: usize = 400;
/// The animated-title cases appended after the hardware-encoder ones (see the module docs).
const SVG_CASES: usize = 400;
const SVG_FIRST: usize = CASES + MASTER_CASES + CHANNEL_CASES + HW_CASES;
/// The multi-delivery cases appended after the animated-title ones (see the module docs).
const TOGETHER_CASES: usize = 400;
const TOGETHER_FIRST: usize = SVG_FIRST + SVG_CASES;
/// The title-clip cases appended after the multi-delivery ones (see the module docs).
const TITLE_CASES: usize = 400;
const TITLE_FIRST: usize = TOGETHER_FIRST + TOGETHER_CASES;
/// The blurred-fill cases appended after the title-clip ones (see the module docs).
const BLUR_CASES: usize = 400;
const BLUR_FIRST: usize = TITLE_FIRST + TITLE_CASES;
/// The keyed mask and crop cases appended after the blurred-fill ones (see the module docs).
const MASK_CROP_CASES: usize = 400;
const MASK_CROP_FIRST: usize = BLUR_FIRST + BLUR_CASES;
/// The speed-ramp cases appended after the keyed mask and crop ones (see the module docs).
const RAMP_CASES: usize = 400;
const RAMP_FIRST: usize = MASK_CROP_FIRST + MASK_CROP_CASES;
/// The noise-reduction cases appended after the speed-ramp ones (see the module docs).
const DENOISE_CASES: usize = 400;
const DENOISE_FIRST: usize = RAMP_FIRST + RAMP_CASES;
/// The blend-mode cases appended after the noise-reduction ones (see the module docs).
const BLEND_CASES: usize = 400;
const BLEND_FIRST: usize = DENOISE_FIRST + DENOISE_CASES;
/// The stabilization cases appended after the blend-mode ones (see the module docs).
const STABILIZE_CASES: usize = 400;
const STABILIZE_FIRST: usize = BLEND_FIRST + BLEND_CASES;
const TOTAL: usize = STABILIZE_FIRST + STABILIZE_CASES;
const BLOCK: usize = 100;
const MIN_PER_FAMILY: usize = 20;
const KINDS: [&str; 3] = ["export", "still", "preview"];
/// The assets the generator draws from; the head-padded twins after them are only
/// ever reached by [`retarget`], so that adding them moved no other case's dice.
const LIBRARY: usize = 11;
const BLESSED: [&str; 3] = [
    include_str!("golden/export.txt"),
    include_str!("golden/still.txt"),
    include_str!("golden/preview.txt"),
];

/// `family needle`: the family is hit when the needle is in the argv text (words
/// joined by spaces) of a case: of the still for `still-...`, of the preview for
/// `preview-...`, of the export for the rest.
const FAMILIES: &str = r"
fade-in fade=t=in:st=
fade-out fade=t=out:st=
dissolve :alpha=1
dip-white :c=white
reverse ,reverse,
speed setpts=(PTS-STARTPTS)/
scale-static scale=iw*
scale-keyed eval=frame
hdr-even-fit force_divisible_by=2
tiny-scale max(1,iw*
still-tiny-scale max(1,iw*
preview-tiny-scale max(1,iw*
hdr-even-zoom 2*trunc((iw*
preview-hdr-even-fit force_divisible_by=2
preview-hdr-even-zoom 2*trunc((iw*
crop crop=w=iw*
rotate-static :fillcolor=none:ow=rotw(
rotate-keyed rotate=a='(
rotate-keyed-transparent :fillcolor=black@0:ow='hypot(
preview-rotate-keyed-transparent :fillcolor=black@0:ow='hypot(
opacity-static colorchannelmixer=aa=
opacity-keyed a='(if(lt((T-
mask-rect max(abs(
mask-ellipse hypot((X-
mask-feather clip((1-
mask-inverted-hard (1-lte(
mask-inverted-feathered (1-clip(
mask+keyed-opacity )*(if(lt((T-
effect-blur gblur=sigma=
effect-sharpen unsharp=
effect-grayscale hue=s=0
effect-invert negate
effect-vignette vignette
chroma-key chromakey=
colour eq=brightness=
colour-temperature :gamma_r=
colour-keyed-brightness eq=brightness='if(lt((t-
colour-keyed-contrast :contrast='if(lt((t-
colour-keyed-saturation :saturation='if(lt((t-
colour-keyed-gamma :gamma='if(lt((t-
colour-keyed-temperature :gamma_r='1+0.3*(
volume-keyed volume='if(lt((t-
volume-keyed-frames asetnsamples=n=128:p=0,volume=
mask-keyed a='if(eq(X,0),st(
mask-keyed-feather if(lte(ld(4)
preview-mask-keyed a='if(eq(X,0),st(
crop-keyed perspective=x0=
preview-crop-keyed perspective=x0=
hdr-zscale zscale=tin=
hdr-colorspace colorspace=all=bt709:iall=bt2020
hdr-pq tin=smpte2084
hdr-hlg tin=arib-std-b67
bt709-label setparams=colorspace=bt709
still-bt709-label setparams=colorspace=bt709
preview-bt709-label setparams=colorspace=bt709
reframe v360@c
reframe-animated sendcmd=c='
reframe-equirect-out output=e:
reframe-fisheye ih_fov=
still-reframe v360=input=
shared-input [vsp
head-padded-proxy trim=start_frame=1
clip-end-cut ,trim=end_pts=
preview-clip-end-cut ,trim=end_pts=
late-start-delay )/TB
preview-late-start-delay )/TB
image-input -loop 1 -framerate
image-under-a-frame -t 0.0
overlay drawtext=
overlay-keyed alpha='
overlay-percent \\%
overlay-quote \'\''
overlay-colon a\: b
overlay-backslash back\\\\slash
overlay-linebreak one two three four five
still-overlay-quote \'\''
still-overlay-colon a\: b
still-overlay-backslash back\\\\slash
still-overlay-linebreak one two three four five
preview-overlay-quote \'\''
preview-overlay-colon a\: b
preview-overlay-backslash back\\\\slash
preview-overlay-linebreak one two three four five
overlay-bold borderw=2
overlay-box box=1
overlay-default-font fontfile='/kerf-test/fonts/Geist-Regular.ttf'
overlay-default-font-bold fontfile='/kerf-test/fonts/Geist-Bold.ttf'
overlay-named-font fontfile='/kerf-test/fonts/PinnedSans-Regular.ttf'
still-overlay-default-font fontfile='/kerf-test/fonts/Geist-
still-overlay-named-font fontfile='/kerf-test/fonts/PinnedSans-Regular.ttf'
preview-overlay-default-font fontfile='/kerf-test/fonts/Geist-
preview-overlay-named-font fontfile='/kerf-test/fonts/PinnedSans-Regular.ttf'
svg-input -f image2 -framerate
svg-held -f image2 -pattern_type none -loop 1 -framerate
svg-overlay ][svg0]overlay=x=
svg-two ][svg1]overlay=x=
svg-partly-off ][svg0]overlay=x=-
svg-restamp settb=expr=1/
still-svg /kerf-test/svg/still-
still-svg-one-file -f image2 -pattern_type none -i /kerf-test/svg/still-
preview-svg-input -f image2 -framerate
preview-svg-held -f image2 -pattern_type none -loop 1 -framerate
preview-svg-seek -start_number
preview-svg-overlay ][svg0]overlay=x=
title-clip-held ,trim=end_frame=
preview-title-clip-held ,trim=end_frame=
cover-fit force_original_aspect_ratio=increase
blur-fill boxblur=
still-blur-fill boxblur=
preview-blur-fill boxblur=
scaler-flag :flags=lanczos
pix-fmt format=yuv422p10le
gif paletteuse=dither=
gif-loop-forever -loop 0
gif-loop-once -loop -1
hwaccel -hwaccel
still-region crop=2*trunc(
still-fade settb=expr=
still-fade-alpha :alpha=1,setpts=PTS-STARTPTS
still-fade-white :c=white,setpts=PTS-STARTPTS
still-fade-pixel-format ,setpts=PTS-STARTPTS,format=yuv
still-file-png image2 -vcodec png
still-file-jpeg image2 -vcodec mjpeg -y
still-jpeg-pipe image2pipe
still-rgb-pipe -pix_fmt rgb24
ducking sidechaincompress
audio-delay-samples S:all=1
audio-crossfade-qsin :curve=qsin
audio-pan pan=stereo|c0=
audio-compressor acompressor=
libm-canary-39 threshold=1.065368864e-2
libm-canary-47 threshold=4.246195639e-3
libm-canary-52 threshold=2.401596268e-3
audio-gate agate=
audio-equalizer equalizer=
audio-highpass highpass=
audio-lowpass lowpass=
audio-tempo atempo=
audio-reverse areverse
audio-aac -c:a aac
audio-aac-pns-off -c:a aac -aac_pns 0
audio-mp3 -c:a libmp3lame
audio-opus -c:a libopus
audio-flac -c:a flac
audio-bitrate -b:a
audio-flac-level -compression_level
audio-muted -an
loudnorm loudnorm=
master-fader dropout_transition=0,volume=
master-limiter ,alimiter=limit=
master-limiter-loudnorm :latency=1,loudnorm=
master-limiter-no-latency :level=0[outa]
master-limiter-no-latency-loudnorm :level=0,loudnorm=
master-fader-ducked [aducked]amix=inputs=2:normalize=0:dropout_transition=0,volume=
master-limiter-ducked [aducked]amix=inputs=2:normalize=0:dropout_transition=0,alimiter=
master-fader-clamped ,volume=4,
master-fader-silent ,volume=0[outa]
master-ceiling-clamped-low limit=0.063096:
master-ceiling-clamped-high limit=1:attack=
mono-delivery channel_layouts=mono
codec-x264 -c:v libx264
codec-x265 -c:v libx265
codec-svtav1 -c:v libsvtav1
codec-vp9 -c:v libvpx-vp9
codec-prores -c:v prores_ks
codec-gif -c:v gif
codec-nvenc -c:v h264_nvenc
codec-qsv -c:v hevc_qsv
codec-videotoolbox -c:v h264_videotoolbox
codec-amf-h264 -c:v h264_amf
codec-amf -c:v hevc_amf
codec-amf-av1 -c:v av1_amf
codec-vaapi-h264 -c:v h264_vaapi
codec-vaapi-hevc -c:v hevc_vaapi
codec-vaapi-av1 -c:v av1_vaapi
codec-unknown -c:v libfoo
rate-crf -crf
rate-vp9-constant-quality -b:v 0
rate-nvenc-vbr -rc vbr
rate-nvenc-cq -cq
rate-qsv _qsv -global_quality
rate-videotoolbox -q:v
rate-amf-cqp -qp_i
rate-amf-quality -quality quality
rate-vaapi-cqp -rc_mode CQP
rate-vaapi-quality -rc_mode CQP -global_quality
vaapi-device -vaapi_device
vaapi-upload [vhw]format=nv12,hwupload[outv]
rate-bitrate -b:v 8M
rate-bitrate-k -b:v 2500k
rate-maxrate -maxrate
rate-bufsize -bufsize
rate-lossless-vp9 -lossless 1
rate-lossless-nvenc -rc constqp
two-pass-first -pass 1
two-pass-second -pass 2
two-pass-log -passlogfile
first-pass-null -f null
preset -preset
vp9-speed -cpu-used
tune -tune
profile-high -profile:v high
profile-main -profile:v main
prores-profile-3 -profile:v 3
prores-4444 -profile:v 4
prores-4444-xq -profile:v 5
prores-4444-pix -pix_fmt yuva444p10le
hevc-tag -tag:v hvc1
faststart -movflags +faststart
title -metadata title=
together-split-two :v]split=2[in
together-split-three :v]split=3[in
together-mix [d0_outa]asplit=
together-titles [d1_svg0]
together-second-frame [d1_vbase]
together-chapters -map_chapters
together-subtitles -disposition:s:0 0
together-refused NOT-TOGETHER
together-reframe-named v360@d1c
speed-ramp setpts='st(0,
speed-ramp-linear sqrt((
speed-ramp-audio acrossfade=d=
preview-speed-ramp setpts='st(0,
preview-speed-ramp-linear sqrt((
audio-denoise afftdn=nr=
audio-denoise-track :tn=1
audio-denoise-delay apad=pad_dur=0.024995,afftdn=
blend blend=c0_mode=
blend-hardlight blend=c0_mode=hardlight:
blend-addition blend=c0_mode=addition:
still-blend blend=c0_mode=
preview-blend blend=c0_mode=
stabilize-vidstab vidstabtransform=input='/kerf-test/stabilize/
stabilize-deshake ,deshake=rx=
stabilize-proxy -0.6667x0.6667.trf'
stabilize-4k -2.0000x2.0000.trf'
stabilize-hdr format=yuv420p,vidstabtransform=
still-stabilize-hdr format=yuv420p,vidstabtransform=
still-stabilize-vidstab vidstabtransform=input='/kerf-test/stabilize/
still-stabilize-deshake deshake=rx=
preview-stabilize-vidstab vidstabtransform=input='/kerf-test/stabilize/
preview-stabilize-deshake ,deshake=rx=
";

// ---- a seeded generator --------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut r = Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        r.next();
        r
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() >> 11) as usize % n
    }

    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }

    /// A real in `[lo, hi)`: 0.37-style two decimals or a full-precision double,
    /// never a dyadic value (0.5, 0.25) whose shortest form hides what `{}` does to
    /// a real one.
    fn real(&mut self, lo: f64, hi: f64) -> f64 {
        let v = lo + (hi - lo) * self.unit();
        let two = (v * 100.0).round() / 100.0;
        match (self.chance(0.6), (two * 8.0).fract() == 0.0) {
            (true, false) => two,
            (true, true) => two + 0.01,
            _ => v,
        }
    }

    fn id(&mut self) -> Uuid {
        Uuid::from_u128((u128::from(self.next()) << 64) | u128::from(self.next()))
    }
}

#[rustfmt::skip]
fn stream(kind: StreamKind, codec: &str, size: (u32, u32), fps: f64) -> StreamInfo {
    let video = kind == StreamKind::Video;
    StreamInfo {
        index: 0, kind, codec: codec.into(), width: video.then_some(size.0), height: video.then_some(size.1), fps: video.then_some(fps),
        sample_rate: None, channels: None, image: false, projection: None, rotation: 0, color_transfer: None, color_primaries: None,
        pix_fmt: video.then(|| "yuv420p".into()), color_space: None, color_range: None, lead: 0.0,
    }
}

#[rustfmt::skip]
fn audio(rate: u32, channels: u16) -> StreamInfo {
    StreamInfo { sample_rate: Some(rate), channels: Some(channels), ..stream(StreamKind::Audio, "aac", (0, 0), 0.0) }
}

/// The fixed library every case draws from: shapes and rates, a still, both HDR
/// transfers, two 360 projections, a phone clip turned by its metadata, audio only —
/// and, after those, the head-padded proxies of four of them (see [`retarget`]).
#[rustfmt::skip]
fn pool() -> Vec<Asset> {
    let video = |size, fps| stream(StreamKind::Video, "h264", size, fps);
    let hdr = |size, fps, trc: &str| StreamInfo {
        color_transfer: Some(trc.into()), color_primaries: Some("bt2020".into()), color_space: Some("bt2020nc".into()), ..video(size, fps)
    };
    let sphere = |p| StreamInfo { projection: Some(p), ..video((5760, 2880), 30.0) };
    let still = StreamInfo { image: true, codec: "png".into(), fps: None, ..video((1280, 720), 0.0) };
    let phone = StreamInfo { rotation: 90, ..video((1080, 1920), 30.0) };
    let specs = vec![
        ("interview", 120.0, vec![video((1920, 1080), 30_000.0 / 1001.0), audio(48_000, 2)]),
        ("broll", 90.0, vec![video((3840, 2160), 24.0)]),
        ("phone", 40.0, vec![phone, audio(44_100, 1)]),
        ("still", 5.0, vec![still]),
        ("hdr-pq", 60.0, vec![hdr((3840, 2160), 30.0, "smpte2084"), audio(48_000, 2)]),
        ("hdr-hlg", 30.0, vec![hdr((1920, 1080), 59.94, "arib-std-b67")]),
        ("sphere", 50.0, vec![sphere(Projection::Equirect), audio(48_000, 1)]),
        ("lenses", 20.0, vec![sphere(Projection::DualFisheye)]),
        ("wide", 75.0, vec![video((1280, 720), 25.0), audio(44_100, 2)]),
        ("music", 200.0, vec![audio(44_100, 2)]),
        ("voice", 60.0, vec![audio(16_000, 1)]),
    ];
    let imported_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    let asset = |(i, (name, duration, streams)): (usize, (&str, f64, Vec<StreamInfo>))| Asset {
        id: Uuid::from_u128(i as u128 + 1), path: format!("/golden/{name}.mp4"), name: name.into(), duration, streams, imported_at,
        source_paths: Vec::new(), voiceover: None, music_score: None, tags: Vec::new(),
    };
    let mut pool: Vec<Asset> = specs.into_iter().enumerate().map(asset).collect();
    debug_assert_eq!(pool.len(), LIBRARY);
    // The proxy a preview of a late-starting source is cut from: the same footage at a
    // path `is_head_padded_proxy` recognizes (`.../kerf/proxies/<16 hex>.lead.mp4`).
    for (n, name) in ["interview", "broll", "phone", "wide"].into_iter().enumerate() {
        let original = pool.iter().find(|a| a.name == name).unwrap().clone();
        pool.push(Asset {
            id: Uuid::from_u128(100 + n as u128),
            path: format!("/golden/kerf/proxies/{:016x}.lead.mp4", 0x5eed_0000 + n as u64),
            name: format!("{name}-padded"),
            ..original
        });
    }
    // Footage with an alpha channel (a transparent sticker PNG, an FFV1 and a ProRes-4444-like
    // clip) and a 4:3 HLG clip whose fit into a delivery is odd-sized more often than not:
    // the twins of `still`, `wide`, `interview` and `hdr-hlg`, reached only by
    // [`retarget_alpha`] and [`retarget_hdr43`].
    let alpha = |name: &str, pix_fmt: &str| {
        let original = pool.iter().find(|a| a.name == name).unwrap().clone();
        let streams = original
            .streams
            .iter()
            .cloned()
            .map(|s| if s.kind == StreamKind::Video { StreamInfo { pix_fmt: Some(pix_fmt.into()), ..s } } else { s })
            .collect();
        Asset { id: Uuid::from_u128(200 + original.id.as_u128()), path: format!("/golden/{name}-alpha.mov"), name: format!("{name}-alpha"), streams, ..original }
    };
    let twins = [alpha("still", "rgba"), alpha("wide", "yuva420p"), alpha("interview", "yuva444p10le")];
    pool.extend(twins);
    let hlg = pool.iter().find(|a| a.name == "hdr-hlg").unwrap().clone();
    let streams = hlg
        .streams
        .iter()
        .cloned()
        .map(|s| if s.kind == StreamKind::Video { StreamInfo { width: Some(1440), height: Some(1080), ..s } } else { s })
        .collect();
    pool.push(Asset { id: Uuid::from_u128(300), path: "/golden/hdr-hlg-43.mp4".into(), name: "hdr-hlg-43".into(), streams, ..hlg });
    // Footage whose video starts after its sound (`StreamInfo::lead`): the twins of `interview`
    // (80 ms, a phone's) and `phone` (half a second), and the head-padded proxy of the first,
    // which carries its original's lead — reached only by [`retarget_late`].
    let late = |name: &str, lead: f64, path: String, twin: &str| {
        let original = pool.iter().find(|a| a.name == name).unwrap().clone();
        let streams = original
            .streams
            .iter()
            .cloned()
            .map(|s| if s.kind == StreamKind::Video { StreamInfo { lead, ..s } } else { s })
            .collect();
        Asset { id: Uuid::from_u128(400 + original.id.as_u128() + 50 * u128::from(path.contains("proxies"))), path, name: format!("{name}{twin}"), streams, ..original }
    };
    let late_twins = [
        late("interview", 0.08, "/golden/interview-late.mov".into(), "-late"),
        late("phone", 0.5, "/golden/phone-late.mp4".into(), "-late"),
        late("interview", 0.08, format!("/golden/kerf/proxies/{:016x}.lead.mp4", 0x5eed_1000), "-late-padded"),
    ];
    pool.extend(late_twins);
    pool
}

/// Every seventh case, about half of the clips whose footage has a head-padded proxy
/// are cut from that proxy instead. It is done once the timeline is drawn and rolls
/// no dice of its own (the clip's id decides), so every other case is exactly what it
/// was; it is what puts `ClipFx.head_pad` — and its `trim=start_frame=1` — in the oracle.
fn retarget(tl: &mut Timeline, i: usize, assets: &[Asset]) {
    if i % 7 != 3 {
        return;
    }
    for clip in tl.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        let Some(original) = assets[..LIBRARY].iter().find(|a| a.id == clip.asset_id) else {
            continue;
        };
        if clip.id.as_u128() % 2 == 1 {
            if let Some(twin) = assets[LIBRARY..]
                .iter()
                .find(|a| a.name.strip_suffix("-padded") == Some(&original.name))
            {
                clip.asset_id = twin.id;
            }
        }
    }
}

/// Every eleventh case, about half of the clips whose footage has an alpha twin are cut
/// from it instead (no dice of its own: the clip's id decides, so every other case is
/// what it was) — what puts `ClipFx.alpha` and a transparent source in the oracle.
fn retarget_alpha(tl: &mut Timeline, i: usize, assets: &[Asset]) {
    if i % 11 != 5 {
        return;
    }
    for clip in tl.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        let Some(original) = assets[..LIBRARY].iter().find(|a| a.id == clip.asset_id) else {
            continue;
        };
        if (clip.id.as_u128() >> 1) % 2 == 0 {
            if let Some(twin) = assets[LIBRARY..]
                .iter()
                .find(|a| a.name.strip_suffix("-alpha") == Some(&original.name))
            {
                clip.asset_id = twin.id;
            }
        }
    }
}

/// Every thirteenth case, a third of the HDR clips are cut from the 4:3 HLG twin.
fn retarget_hdr43(tl: &mut Timeline, i: usize, assets: &[Asset]) {
    if i % 13 != 8 {
        return;
    }
    let twin = assets.iter().find(|a| a.name == "hdr-hlg-43").unwrap().id;
    for clip in tl.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        if assets[..LIBRARY].iter().any(|a| a.id == clip.asset_id && a.hdr().is_some()) && clip.id.as_u128() % 3 == 0 {
            clip.asset_id = twin;
        }
    }
}

/// Every nineteenth case, the clips of `interview` and `phone` are cut from their late-starting
/// twins (about a third of `interview`'s from its padded proxy), and half of those from the head
/// of the file, their length kept: what puts `ClipFx::lead` — the window read without a seek and
/// the picture's `picture_delay` — in the oracle. No dice of its own: the clip's id decides.
fn retarget_late(tl: &mut Timeline, i: usize, assets: &[Asset]) {
    if i % 19 != 6 {
        return;
    }
    for clip in tl.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        let Some(original) = assets[..LIBRARY].iter().find(|a| a.id == clip.asset_id) else {
            continue;
        };
        let twin = if original.name == "interview" && clip.id.as_u128() % 3 == 0 {
            "-late-padded"
        } else {
            "-late"
        };
        let Some(twin) = assets[LIBRARY..]
            .iter()
            .find(|a| a.name.strip_suffix(twin) == Some(&original.name))
        else {
            continue;
        };
        clip.asset_id = twin.id;
        if (clip.id.as_u128() >> 2) % 2 == 0 {
            clip.source_out -= clip.source_in;
            clip.source_in = 0.0;
        }
    }
}

/// Every seventeenth case, a third of the clips are scaled to a size that is under a
/// pixel (0.0004 of a frame): the first key of a keyed clip, else its own scale.
fn retarget_tiny(tl: &mut Timeline, i: usize) {
    if i % 17 != 4 {
        return;
    }
    for clip in tl.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        if clip.id.as_u128() % 3 == 0 {
            match clip.keyframes.first_mut() {
                Some(key) => key.scale = 0.0004,
                None => clip.transform.scale = 0.0004,
            }
        }
    }
}

/// The animation channels of the `j`-th channel case: on dice of their own (seeded by the case
/// and the clip's place in it), so the family never rolls the generator's and the cases before
/// it cannot move. About half the clips get colour numbers keyed (each of the five with its own
/// chance, so one, several and all are covered), a third a volume, a third some transform numbers
/// beside the rest (an opacity ramp on a still pose, a zoom without a move, ...), and a clip
/// that was animated through the legacy bundle sometimes has one number taken off it — keyed on
/// its own, or held static by an empty track.
fn channels_for(tl: &mut Timeline, j: usize) {
    const EASINGS: [Easing; 6] = [
        Easing::Linear,
        Easing::Linear,
        Easing::Hold,
        Easing::EaseIn,
        Easing::EaseInOut,
        Easing::Bezier {
            x1: 0.2,
            y1: 0.9,
            x2: 0.3,
            y2: 1.0,
        },
    ];
    let range = |p: Property| match p {
        Property::Scale => (0.3, 2.2),
        Property::PosX | Property::PosY => (-0.5, 0.5),
        Property::Rotation => (-90.0, 90.0),
        Property::Opacity => (0.0, 1.0),
        Property::Brightness => (-0.5, 0.5),
        Property::Contrast => (0.4, 2.5),
        Property::Saturation => (0.0, 2.5),
        Property::Gamma => (0.4, 2.5),
        Property::Temperature => (-1.0, 1.0),
        Property::Volume => (0.0, 2.0),
        _ => (0.0, 1.0),
    };
    for (n, clip) in tl.tracks.iter_mut().flat_map(|t| &mut t.clips).enumerate() {
        let mut r = Rng::new((1 << 40) + (j as u64) * 64 + n as u64);
        let span = clip.duration() * 1.1 + 0.2;
        let keys = |r: &mut Rng, p: Property| {
            let (lo, hi) = range(p);
            let mut keys: Vec<PropertyKey> = (0..1 + r.below(4))
                .map(|_| PropertyKey {
                    time: r.real(0.0, span),
                    value: r.real(lo, hi),
                    easing: r.pick(&EASINGS),
                })
                .collect();
            keys.sort_by(|a, b| a.time.total_cmp(&b.time));
            keys
        };
        if r.chance(0.5) {
            for p in Property::COLOR {
                if r.chance(0.45) {
                    clip.set_property_keys(p, keys(&mut r, p));
                }
            }
        }
        if r.chance(0.35) {
            clip.set_property_keys(Property::Volume, keys(&mut r, Property::Volume));
        }
        if r.chance(0.35) {
            for p in Property::TRANSFORM {
                if r.chance(0.4) {
                    clip.set_property_keys(p, keys(&mut r, p));
                }
            }
        }
        if !clip.keyframes.is_empty() && r.chance(0.4) {
            let p = r.pick(&Property::TRANSFORM);
            let held = if r.chance(0.5) { Vec::new() } else { keys(&mut r, p) };
            clip.set_property_keys(p, held);
        }
    }
}

/// The keyed mask and crop of the `j`-th mask-and-crop case: on dice of their own (seeded by the
/// case and the clip's place in it), so the family never rolls the generator's. About half the
/// clips get a mask (the one they had, or a new one) with some of its numbers keyed — a feather
/// keyed to nothing in places — and about half get the animated crop keyed: a zoom alone, a pan
/// alone, or both, each with its own chance, so one and all of the three are covered.
fn mask_crop_for(tl: &mut Timeline, j: usize) {
    const EASINGS: [Easing; 4] = [Easing::Linear, Easing::Hold, Easing::EaseInOut, Easing::EaseOut];
    let range = |p: Property| match p {
        Property::MaskX | Property::MaskY => (0.1, 0.9),
        Property::MaskWidth | Property::MaskHeight => (0.05, 1.4),
        Property::MaskFeather => (0.0, 0.6),
        Property::CropZoom => (1.0, 3.5),
        _ => (-1.0, 1.0),
    };
    for (n, clip) in tl.tracks.iter_mut().flat_map(|t| &mut t.clips).enumerate() {
        let mut r = Rng::new((2 << 40) + (j as u64) * 64 + n as u64);
        let span = clip.duration() * 1.1 + 0.2;
        let keys = |r: &mut Rng, p: Property| {
            let (lo, hi) = range(p);
            let mut keys: Vec<PropertyKey> = (0..1 + r.below(4))
                .map(|_| PropertyKey {
                    time: r.real(0.0, span),
                    value: if p == Property::MaskFeather && r.chance(0.2) {
                        0.0
                    } else {
                        r.real(lo, hi)
                    },
                    easing: r.pick(&EASINGS),
                })
                .collect();
            keys.sort_by(|a, b| a.time.total_cmp(&b.time));
            keys
        };
        if r.chance(0.5) {
            clip.mask.get_or_insert_with(Mask::default);
            for p in Property::MASK {
                if r.chance(0.5) {
                    clip.set_property_keys(p, keys(&mut r, p));
                }
            }
        }
        if r.chance(0.5) {
            for p in Property::CROP {
                if r.chance(0.6) {
                    clip.set_property_keys(p, keys(&mut r, p));
                }
            }
        }
    }
}

/// A speed ramp on most of the clips of the `j`-th ramp case, on dice of its own seeded by the case and
/// the clip (so the generator's are untouched): one to five keys, now and then sixteen (the most the
/// export nests), speeds from a crawl to eight times, a hold here and there, a quarter of the clips played
/// backwards. A still image and a title keep the constant speed they have. The window stays, so the clip's
/// length changes, and what follows it on its track moves with it (the gaps and overlaps of the track kept,
/// as a rippling edit would): clips that touched still touch, so a transition keeps its partner and its
/// tail plays on at the last key's speed.
fn ramp_for(tl: &mut Timeline, j: usize, assets: &[Asset]) {
    let mut n = 0;
    for track in &mut tl.tracks {
        let old: Vec<(f64, f64)> = track.clips.iter().map(|c| (c.timeline_start, c.timeline_end())).collect();
        for clip in &mut track.clips {
            n += 1;
            let mut r = Rng::new((3 << 40) + (j as u64) * 64 + n as u64);
            let still = assets.iter().any(|a| a.id == clip.asset_id && a.is_image());
            if still || clip.is_generated() || !r.chance(0.75) {
                continue;
            }
            let count = if r.chance(0.06) { 16 } else { 1 + r.below(5) };
            let mut time = if r.chance(0.5) { 0.0 } else { r.real(0.0, 1.0) };
            clip.speed_ramp = (0..count)
                .map(|_| {
                    let key = SpeedKey {
                        time,
                        speed: match r.below(3) {
                            0 => r.pick(&[0.25, 0.5, 0.75, 1.5, 2.0, 3.0, 8.0]),
                            1 => r.real(0.2, 3.0),
                            _ => r.real(0.5, 8.0),
                        },
                        easing: if r.chance(0.3) { RampEasing::Hold } else { RampEasing::Linear },
                    };
                    time += r.real(0.1, 1.6);
                    key
                })
                .collect();
            if r.chance(0.25) {
                clip.speed = -clip.speed.abs().max(1.0);
            }
        }
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| old[a].0.total_cmp(&old[b].0));
        for pair in order.windows(2) {
            let gap = old[pair[1]].0 - old[pair[0]].1;
            track.clips[pair[1]].timeline_start = (track.clips[pair[0]].timeline_end() + gap).max(0.0);
        }
    }
}

/// A noise reduction in the audio chain of most of the clips of the `j`-th denoise case, on dice of its
/// own seeded by the case and the clip (so the generator's are untouched): anywhere in the chain the clip
/// has, its reduction and floor across most of the filter's range, the floor tracked a third of the time.
fn denoise_for(tl: &mut Timeline, j: usize) {
    let mut n = 0;
    for clip in tl.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        n += 1;
        let mut r = Rng::new((4 << 40) + (j as u64) * 64 + n as u64);
        if !r.chance(0.7) {
            continue;
        }
        let at = r.below(clip.audio.len() + 1);
        let denoise = AudioEffect::Denoise {
            reduction_db: r.real(0.5, 96.0),
            floor_db: r.real(-79.0, -21.0),
            track_noise: r.chance(0.33),
        };
        clip.audio.insert(at, denoise);
    }
}

/// A blend mode on some of the video clips of the `j`-th blend case, on dice of its own seeded by the case and the
/// clip (so the generator's are untouched): every mode but `normal`, footage, stills and titles alike.
fn blend_for(tl: &mut Timeline, j: usize) {
    let mut n = 0;
    for track in tl.tracks.iter_mut().filter(|t| t.kind == StreamKind::Video) {
        for clip in &mut track.clips {
            n += 1;
            let mut r = Rng::new((5 << 40) + (j as u64) * 64 + n as u64);
            if r.chance(0.6) {
                clip.blend_mode = r.pick(&crate::model::BlendMode::ALL[1..]);
            }
        }
    }
}

/// Stabilization on most clips of footage of the `j`-th stabilization case, on dice of its own seeded by
/// the case and the clip (the generator's are untouched): a strength from a little to all of it, a fixed
/// zoom on a third. A still, a title, a clip of sound and 360 footage are left alone (the core refuses
/// them; the graph would leave them out anyway).
fn stabilize_for(tl: &mut Timeline, j: usize, assets: &[Asset]) {
    let mut n = 0;
    for track in tl.tracks.iter_mut().filter(|t| t.kind == StreamKind::Video) {
        for clip in &mut track.clips {
            n += 1;
            let mut r = Rng::new((6 << 40) + (j as u64) * 64 + n as u64);
            let asset = assets.iter().find(|a| a.id == clip.asset_id);
            let footage = asset.is_some_and(|a| !a.is_image() && a.projection().is_none());
            if !footage || clip.is_generated() || clip.reframe.is_some() || !r.chance(0.8) {
                continue;
            }
            clip.stabilize = Some(crate::model::Stabilize {
                strength: r.pick(&[0.1, 0.35, 0.5, 0.8, 1.0]),
                zoom: r.chance(0.33).then(|| r.pick(&[0.0, 4.0, 7.5, 12.0])),
            });
            // Now and then read through its padded proxy, which the slice is scaled to.
            let twin = asset.and_then(|a| assets.iter().find(|t| t.name == format!("{}-padded", a.name)));
            if let Some(twin) = twin.filter(|_| r.chance(0.3)) {
                clip.asset_id = twin.id;
            }
        }
    }
}

/// The analyses a stabilized clip of the pool reads in the oracle: one made-up one per asset of
/// footage (the padded proxies' twins too), over its whole length at its own rate and at the size the
/// detect pass measures (at most 1920 wide, so a 4K original's slice is scaled by 2).
fn pinned_analyses() -> HashMap<Uuid, Arc<crate::stabilize::MotionAnalysis>> {
    static ANALYSES: std::sync::OnceLock<HashMap<Uuid, Arc<crate::stabilize::MotionAnalysis>>> = std::sync::OnceLock::new();
    ANALYSES
        .get_or_init(|| {
            pool()
                .iter()
                .filter(|a| !a.is_image())
                .filter_map(|a| {
                    let v = a.streams.iter().find(|s| s.kind == StreamKind::Video)?;
                    // Measured as the detect pass measures: at most 1920 wide, the height even.
                    let (w, h) = (v.width?, v.height?);
                    let mw = w.min(1920);
                    let mh = ((f64::from(h) * f64::from(mw) / f64::from(w) / 2.0).round() as u32) * 2;
                    let made = crate::stabilize::pinned::shaky(a.duration, v.fps?, mw, mh);
                    Some((a.id, Arc::new(made)))
                })
                .collect()
        })
        .clone()
}

/// The master bus of the `j`-th master-bus case: a function of `j` alone, so the
/// family never rolls a die (and the cases before it cannot move). A cycle of
/// fader only, limiter only, both and "both, from a file that never went through the
/// op" — a fader past +12 dB, a ceiling below the filter's floor or above 0 dB, a
/// muted master, and values that are not numbers at all.
fn master_for(j: usize) -> MasterBus {
    // Non-dyadic on purpose (see `Rng::real`): 0.353..1.603 and -0.5..-9.9 dB.
    let volume = 0.353 + ((j * 37) % 125) as f64 / 100.0;
    let ceiling_db = -(0.5 + ((j * 13) % 95) as f64 / 10.0);
    match j % 16 {
        0 | 4 | 8 => MasterBus {
            volume,
            ..MasterBus::default()
        },
        1 | 5 | 9 => MasterBus {
            limiter: true,
            ..MasterBus::default()
        },
        2 | 6 | 10 => MasterBus {
            limiter: true,
            ceiling_db,
            ..MasterBus::default()
        },
        3 | 11 => MasterBus {
            duck_depth_db: None,
            true_peak: false,
            volume,
            limiter: true,
            ceiling_db,
        },
        7 => MasterBus {
            duck_depth_db: None,
            true_peak: false,
            volume: 9.0,
            limiter: true,
            ceiling_db: -90.0,
        },
        12 => MasterBus {
            volume: 0.0,
            ..MasterBus::default()
        },
        13 => MasterBus {
            limiter: true,
            ceiling_db: 12.0,
            ..MasterBus::default()
        },
        14 => MasterBus {
            duck_depth_db: None,
            true_peak: false,
            volume: 1.0 + (j % 5) as f64 * 0.1,
            limiter: true,
            ceiling_db: -1.0,
        },
        _ => MasterBus {
            duck_depth_db: None,
            true_peak: false,
            volume: f64::NAN,
            limiter: true,
            ceiling_db: f64::INFINITY,
        },
    }
}

fn has_video(a: &Asset) -> bool {
    a.streams.iter().any(|s| s.kind == StreamKind::Video)
}

/// `Some(f(r))` with probability `p`, else `None`.
fn maybe<T>(r: &mut Rng, p: f64, f: impl FnOnce(&mut Rng) -> T) -> Option<T> {
    r.chance(p).then(|| f(r))
}

// The generator is a table of dice rolls: kept as written (130 columns) rather than
// one field per line.
#[rustfmt::skip]
fn clip(r: &mut Rng, a: &Asset, start: f64, earlier: &[Clip]) -> Clip {
    let len = if a.is_image() { if r.chance(0.15) { r.real(0.001, 0.03) } else { r.real(1.0, 6.0) } } else { r.real(0.5, 8.0) };
    let (src_in, src_out) = match earlier.last() {
        // The same footage again (a picture-in-picture of one source): a shared input.
        Some(e) if r.chance(0.1) && !a.is_image() => (e.source_in, e.source_out),
        _ if a.is_image() => (0.0, len),
        _ => {
            let src_in = if r.chance(0.3) { 0.0 } else { r.real(0.0, (a.duration - len).max(0.0)) };
            let out = match r.below(10) { 0 => a.duration + r.real(0.1, 2.0), 1 | 2 => a.duration, _ => (src_in + len).min(a.duration) };
            (src_in, out)
        }
    };
    let mut c = Clip::new(a.id, src_in, src_out, start);
    c.id = r.id();
    if !a.is_image() || r.chance(0.2) {
        c.speed = r.pick(&[1.0, 1.0, 1.0, 1.0, 0.5, 2.0, 0.7, 1.3, 4.0, 0.25, -1.0, -2.0, -0.7, 0.0]);
    }
    c.volume = maybe(r, 0.3, |r| r.real(0.0, 2.0) as f32).unwrap_or(1.0);
    c.fade_in = maybe(r, 0.35, |r| r.real(0.05, 2.0)).unwrap_or(0.0);
    c.fade_out = maybe(r, 0.35, |r| r.real(0.05, 2.0)).unwrap_or(0.0);
    c.transition_in = maybe(r, 0.4, |r| Transition { kind: r.pick(&TransitionKind::ALL), duration: r.real(0.1, 2.5) });
    if r.chance(0.5) {
        let t = &mut c.transform;
        t.scale = maybe(r, 0.5, |r| r.real(0.2, 2.4)).unwrap_or(1.0);
        (t.pos_x, t.pos_y) = maybe(r, 0.35, |r| (r.real(-0.6, 0.6), r.real(-0.6, 0.6))).unwrap_or_default();
        t.rotation = maybe(r, 0.25, |r| r.real(-120.0, 120.0)).unwrap_or(0.0);
        t.opacity = maybe(r, 0.3, |r| r.real(0.05, 0.99)).unwrap_or(1.0);
        if r.chance(0.25) {
            (t.crop_left, t.crop_right, t.crop_top, t.crop_bottom) = (r.real(0.0, 0.3), r.real(0.0, 0.3), r.real(0.0, 0.3), r.real(0.0, 0.3));
        }
    }
    if r.chance(0.3) {
        let k = &mut c.color;
        (k.brightness, k.contrast) = (r.real(-0.5, 0.5), r.real(0.5, 2.0));
        k.saturation = maybe(r, 0.5, |r| r.real(0.0, 2.0)).unwrap_or(1.0);
        k.gamma = maybe(r, 0.4, |r| r.real(0.5, 2.0)).unwrap_or(1.0);
        k.temperature = maybe(r, 0.4, |r| r.real(-1.0, 1.0)).unwrap_or(0.0);
    }
    for _ in 0..maybe(r, 0.28, |r| 1 + r.below(3)).unwrap_or(0) {
        let color = r.pick(&["green", "0x00ff00", "#00FF00@0.9", "bad colour!", "blue"]).to_string();
        let (x, y) = (r.real(0.01, 20.0), r.real(0.01, 0.6));
        c.effects.push(match r.below(6) {
            0 => VideoEffect::Blur { sigma: x },
            1 => VideoEffect::Sharpen { amount: x / 7.0 },
            2 => VideoEffect::Grayscale,
            3 => VideoEffect::Invert,
            4 => VideoEffect::Vignette,
            _ => VideoEffect::ChromaKey { color, similarity: y, blend: y / 2.0 },
        });
    }
    for _ in 0..maybe(r, 0.2, |r| 1 + r.below(2)).unwrap_or(0) {
        // A tenth of the thresholds are dB values whose `powf` differs between glibc with and without FMA.
        let (x, y) = (r.real(1.0, 80.0), if r.chance(0.1) { r.pick(&[-39.45, -47.44, -52.39]) } else { r.real(-40.0, 12.0) });
        c.audio.push(match r.below(5) {
            0 => AudioEffect::Highpass { hz: x * 5.0 },
            1 => AudioEffect::Lowpass { hz: x * 150.0 },
            2 => AudioEffect::Equalizer { hz: x * 90.0, width: x * 20.0, gain_db: y / 3.0 },
            3 => AudioEffect::Compressor { threshold_db: y, ratio: x / 8.0 + 1.0, attack_ms: x, release_ms: x * 8.0, makeup_db: x / 9.0 },
            _ => AudioEffect::Gate { threshold_db: y },
        });
    }
    for _ in 0..maybe(r, 0.25, |r| 1 + r.below(4)).unwrap_or(0) {
        let mut k = Keyframe::from_transform(r.real(0.0, c.duration() * 1.1 + 0.2), &c.transform);
        k.scale = maybe(r, 0.5, |r| r.real(0.3, 2.2)).unwrap_or(k.scale);
        (k.pos_x, k.pos_y) = maybe(r, 0.4, |r| (r.real(-0.5, 0.5), r.real(-0.5, 0.5))).unwrap_or((k.pos_x, k.pos_y));
        k.rotation = maybe(r, 0.3, |r| r.real(-90.0, 90.0)).unwrap_or(k.rotation);
        k.opacity = maybe(r, 0.4, |r| r.real(0.0, 1.0)).unwrap_or(k.opacity);
        c.keyframes.push(k);
    }
    // A reframe's `sendcmd` list grows with duration x fps, so keep slow clips out.
    if let (Some(p), true, true) = (a.projection(), c.speed_mag() >= 0.2, r.chance(0.85)) {
        let mut rf = Reframe::new(p);
        if r.chance(0.5) {
            (rf.yaw, rf.pitch, rf.roll, rf.fov) = (r.real(-200.0, 200.0), r.real(-100.0, 100.0), r.real(-20.0, 20.0), r.real(20.0, 150.0));
        }
        if r.chance(0.12) {
            rf.output = Projection::Equirect;
        }
        // A moving camera: every channel, exactly one, a single key, or a held pose.
        let (keys, mode, one) = (maybe(r, 0.4, |r| 1 + r.below(4)).unwrap_or(0), r.below(4), r.below(4));
        for _ in 0..keys {
            let v = [r.real(-200.0, 200.0), r.real(-100.0, 100.0), r.real(-30.0, 30.0), r.real(10.0, 170.0)];
            let held = [rf.yaw, rf.pitch, rf.roll, rf.fov];
            let at = |n: usize| if mode == 0 || mode == 2 || (mode == 1 && n == one) { v[n] } else { held[n] };
            let time = r.real(0.0, c.duration() + 0.5);
            rf.keyframes.push(ReframeKeyframe { time, yaw: at(0), pitch: at(1), roll: at(2), fov: at(3) });
        }
        if mode == 2 {
            rf.keyframes.truncate(1);
        }
        c.reframe = Some(rf);
    }
    c.mask = maybe(r, 0.13, |r| Mask {
        shape: r.pick(&[MaskShape::Rect, MaskShape::Ellipse]), x: r.real(-0.2, 1.2), y: r.real(-0.2, 1.2),
        width: r.real(0.0, 1.5), height: r.real(0.0, 1.5), feather: maybe(r, 0.5, |r| r.real(0.0, 1.2)).unwrap_or(0.0), inverted: r.chance(0.3),
    });
    c.enabled = !r.chance(0.05);
    c
}

#[rustfmt::skip]
fn overlay(r: &mut Rng) -> TextOverlay {
    let text = r.pick(&["Hello", "50% off", "It's \"quoted\"", "back\\slash", "two\nlines", "a: b, c; [d] =e", "日本語 ünï", "", "%{pts}"]);
    let start = r.real(0.0, 20.0);
    // Half of the draws of "two\nlines" break the line with every other kind of break as well (a CR, a CRLF,
    // the Unicode separators), so the oracle pins that they are all one space; from draws already made, so no
    // other case moves.
    let text = if text == "two\nlines" && start.fract() >= 0.5 { "one\rtwo\r\nthree\u{2028}four\u{2029}five" } else { text };
    let mut o = TextOverlay::new(text, start, start + r.real(0.2, 6.0));
    o.id = r.id();
    (o.pos_x, o.pos_y, o.size) = (r.real(0.0, 1.0), r.real(0.0, 1.0), r.real(0.02, 0.2));
    o.color = r.pick(&["white", "yellow@0.9", "#ffcc00", "0xFF00FF80", "not a colour!", "a,b"]).into();
    o.bg = r.pick(&[None, None, Some("black@0.5"), Some("#00000080"), Some("bad:colour")]).map(String::from);
    (o.bold, o.generated) = (r.chance(0.2), r.chance(0.2));
    // The shipped default has a bold face, so a bold overlay no longer carries a border; one font with
    // none keeps the border branch covered. Chosen from draws already made, so no other case moves.
    if o.bold && !o.generated {
        o.font = Some(crate::fonts::PINNED_FAMILY.into());
    }
    for _ in 0..maybe(r, 0.3, |r| 1 + r.below(3)).unwrap_or(0) {
        let (time, pos_x, pos_y) = (r.real(0.0, 5.0), r.real(0.0, 1.0), r.real(0.0, 1.0));
        o.keyframes.push(TextKeyframe { time, pos_x, pos_y, opacity: r.real(0.0, 1.0) });
    }
    o
}

#[rustfmt::skip]
fn timeline(r: &mut Rng, assets: &[Asset]) -> Timeline {
    let video: Vec<&Asset> = assets.iter().filter(|a| has_video(a)).collect();
    let mut kinds = vec![StreamKind::Video; 1 + r.below(4)];
    kinds.extend(vec![StreamKind::Audio; r.below(4)]);
    for i in (1..kinds.len()).rev() {
        kinds.swap(i, r.below(i + 1));
    }
    let mut tl = Timeline::new();
    tl.tracks.clear();
    for (n, kind) in kinds.into_iter().enumerate() {
        let mut t = Track::new(kind, format!("T{n}"));
        t.id = r.id();
        (t.muted, t.solo) = (r.chance(0.04), r.chance(0.04));
        t.duck = kind == StreamKind::Audio && r.chance(0.3);
        t.volume = maybe(r, 0.3, |r| r.real(0.1, 1.6) as f32).unwrap_or(1.0);
        t.pan = maybe(r, 0.3, |r| r.real(-1.0, 1.0) as f32).unwrap_or(0.0);
        let mut cursor = maybe(r, 0.5, |r| r.real(0.0, 3.0)).unwrap_or(0.0);
        for _ in 0..r.below(if kind == StreamKind::Video { 7 } else { 5 }) {
            let a = if kind == StreamKind::Video { video[r.below(video.len())] } else { &assets[r.below(assets.len())] };
            // Adjacent exactly, adjacent within the 1 ms transitions tolerate, overlapping, or a gap.
            let gap = match r.below(10) { 0..=3 => 0.0, 4 => 0.0005, 5 => -r.real(0.05, 0.8), _ => r.real(0.1, 4.0) };
            let c = clip(r, a, (cursor + gap).max(0.0), &t.clips);
            cursor = c.timeline_end();
            t.clips.push(c);
        }
        tl.tracks.push(t);
    }
    for _ in 0..maybe(r, 0.35, |r| 1 + r.below(3)).unwrap_or(0) {
        tl.overlays.push(overlay(r));
    }
    tl.format = maybe(r, 0.3, |r| {
        let (w, h) = r.pick(&[(1080, 1920), (1080, 1080), (1920, 1080), (1080, 1350), (721, 405), (3840, 2160)]);
        Delivery::new(w, h, if r.chance(0.5) { Fit::Cover } else { Fit::Contain })
    });
    tl
}

#[rustfmt::skip]
fn options(r: &mut Rng, dur: f64) -> ExportOptions {
    let plain = [Container::Mp4, Container::Mp4, Container::Mp4, Container::Mkv, Container::Webm, Container::Mov];
    let other = [Container::Gif, Container::Mp3, Container::M4a, Container::Wav, Container::Flac];
    let mut o = ExportOptions { container: if r.chance(0.2) { r.pick(&other) } else { r.pick(&plain) }, ..ExportOptions::default() };
    // ProRes thrice: its profile has six values and the 4444 ones change the pixel format.
    let codecs = ["libx264", "libx265", "libsvtav1", "libvpx-vp9", "prores_ks", "prores_ks", "prores_ks", "gif", "h264_nvenc", "hevc_nvenc",
                  "av1_nvenc", "h264_qsv", "hevc_qsv", "h264_videotoolbox", "hevc_videotoolbox", "h264_amf", "hevc_amf", "libfoo"];
    o.video_codec = maybe(r, 0.6, |r| r.pick(&codecs).into());
    o.rate_control = r.pick(&[RateControl::Crf, RateControl::Crf, RateControl::Bitrate, RateControl::TwoPass, RateControl::Lossless, RateControl::Lossless]);
    o.crf = maybe(r, 0.5, |r| r.below(40) as u32);
    o.video_bitrate = maybe(r, 0.5, |r| r.pick(&["8M", "2500k"]).into());
    o.max_rate = maybe(r, 0.4, |r| r.pick(&["12M", "4000k"]).into());
    o.buf_size = maybe(r, 0.4, |r| r.pick(&["16M", "5000k"]).into());
    o.preset = maybe(r, 0.4, |r| r.pick(&["slow", "veryfast", "p4", "5"]).into());
    o.tune = maybe(r, 0.5, |r| r.pick(&["film", "zerolatency", "stillimage", "bogus"]).into());
    o.profile_v = maybe(r, 0.3, |r| r.pick(&["high", "main", "main10"]).into());
    o.prores_profile = maybe(r, 0.6, |r| r.pick(&[0, 1, 2, 3, 4, 4, 5, 5]));
    o.audio_codec = maybe(r, 0.5, |r| r.pick(&["aac", "libmp3lame", "libopus", "flac", "pcm_s16le", "bogus"]).into());
    o.audio_bitrate = maybe(r, 0.5, |r| r.pick(&["128k", "192k"]).into());
    o.flac_compression = maybe(r, 0.6, |r| r.below(13) as u8);
    o.faststart = r.chance(0.3);
    o.metadata_title = maybe(r, 0.25, |r| r.pick(&["My cut", "Say \"hi\"", ""]).into());
    o.gif_loop = !r.chance(0.4);
    o.resolution = maybe(r, 0.25, |r| (r.pick(&[640, 1280, 1920, 721, 1080]), r.pick(&[360, 720, 1080, 405, 1920])));
    o.fps = maybe(r, 0.3, |r| r.pick(&[24.0, 25.0, 29.97, 30_000.0 / 1001.0, 50.0, 60.0]));
    o.pix_fmt = maybe(r, 0.15, |r| r.pick(&["yuv420p", "yuv422p10le", "yuvj420p", "nope"]).into());
    o.scaler = maybe(r, 0.25, |r| r.pick(&["bicubic", "bilinear", "lanczos", "not-a-scaler"]).into());
    o.hwaccel = maybe(r, 0.25, |r| r.pick(&["auto", "none", "", "NONE", "cuda"]).into());
    (o.include_audio, o.loudnorm) = (!r.chance(0.1), r.chance(0.1));
    o.audio_channels = maybe(r, 0.12, |r| r.pick(&[1, 2]));
    o.audio_sample_rate = maybe(r, 0.1, |r| r.pick(&[22_050, 44_100]));
    o.gif_dither = maybe(r, 0.3, |r| r.pick(&["sierra2", "bayer", "bogus"]).into());
    o.fit = if r.chance(0.25) { Fit::Cover } else { Fit::Contain };
    o.range = maybe(r, 0.1, |r| { let start = r.real(0.0, dur.max(1.0)); TimeRange { start, end: start + r.real(0.5, 10.0) } });
    o
}

/// The artwork of the animated-title cases: fixed strings, so nothing a library computes is in the
/// key. A box that slides, a fade that then holds, a title with text, and a still picture.
const SVG_DOCS: [&str; 4] = [
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 192 108\"><rect y=\"40\" width=\"40\" height=\"28\" fill=\"#ff3355\"><animate attributeName=\"x\" from=\"0\" to=\"152\" dur=\"2s\" fill=\"freeze\"/></rect></svg>",
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1920 1080\"><rect width=\"1920\" height=\"1080\" fill=\"#102030\" opacity=\"0\"><animate attributeName=\"opacity\" from=\"0\" to=\"0.8\" dur=\"0.5s\" fill=\"freeze\"/></rect></svg>",
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 800 200\"><text x=\"20\" y=\"120\" font-size=\"90\" fill=\"#fff\"><animate attributeName=\"opacity\" values=\"0;1;1;0\" keyTimes=\"0;0.2;0.8;1\" dur=\"3s\" fill=\"freeze\"/>Kerf</text></svg>",
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 100 100\"><circle cx=\"50\" cy=\"50\" r=\"40\" fill=\"#00ccaa\"/></svg>",
];

/// The animated titles of the `j`-th title case: one to three, on dice of their own (seeded by the
/// case, so the generator's draws are untouched and the cases before it cannot move). Half cover the
/// frame and half sit in a box that may hang off its edge; a third are translucent, a fifth start
/// part-way into their animation (as a slice leaves them).
fn svg_for(tl: &mut Timeline, j: usize) {
    let mut r = Rng::new(0x5356_4700_0000 + j as u64);
    let dur = tl.duration().max(1.0);
    for _ in 0..r.pick(&[1, 1, 1, 2, 2, 3]) {
        let start = r.real(0.0, dur * 0.8);
        let end = start + r.real(0.4, 8.0);
        let mut o = crate::model::SvgOverlay {
            id: r.id(),
            name: String::new(),
            svg: r.pick(&SVG_DOCS).into(),
            start,
            end,
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            opacity: 1.0,
            offset: 0.0,
            preset: None,
            template: None,
        };
        if r.chance(0.5) {
            (o.x, o.y, o.width, o.height) = (r.real(-0.3, 0.7), r.real(-0.3, 0.7), r.real(0.05, 0.6), r.real(0.05, 0.6));
        }
        if r.chance(0.35) {
            o.opacity = r.real(0.1, 0.95);
        }
        if r.chance(0.2) {
            o.offset = r.real(0.1, 3.0);
        }
        tl.svg_overlays.push(o);
    }
}

/// The titles on tracks of the `j`-th title-clip case: one or two, on dice of their own (seeded by the
/// case). Half follow the last clip of the lowest video track, most of those touching it and most of
/// those under a transition; the rest go over the cut on a video track of their own, on top. Some are
/// head-trimmed (their document starts part-way, as a slice leaves it), faded, or scaled and moved.
fn title_clip_for(tl: &mut Timeline, j: usize) {
    let mut r = Rng::new(0x5449_544c_0000 + j as u64);
    let Some(lowest) = tl.tracks.iter().position(|t| t.kind == StreamKind::Video) else {
        return;
    };
    for _ in 0..r.pick(&[1, 1, 2]) {
        let title = crate::model::TitleClip {
            name: String::new(),
            svg: r.pick(&SVG_DOCS).into(),
            template: None,
        };
        let length = r.real(0.4, 6.0);
        let after = r.chance(0.5);
        let start = if after {
            let end = tl.tracks[lowest].end();
            if r.chance(0.75) {
                end
            } else {
                end + r.real(0.1, 2.0)
            }
        } else {
            r.real(0.0, tl.duration().max(1.0))
        };
        let mut clip = Clip::title(title, start, length);
        clip.id = r.id();
        if after && r.chance(0.7) {
            clip.transition_in = Some(Transition {
                kind: r.pick(&TransitionKind::ALL),
                duration: r.real(0.2, 1.5),
            });
        }
        if r.chance(0.2) {
            clip.source_in = r.real(0.1, 2.0);
            clip.source_out += clip.source_in;
        }
        if r.chance(0.25) {
            clip.fade_in = r.real(0.1, 1.0);
        }
        if r.chance(0.25) {
            clip.fade_out = r.real(0.1, 1.0);
        }
        if r.chance(0.3) {
            clip.transform.scale = r.real(0.3, 1.2);
            clip.transform.pos_x = r.real(-0.3, 0.3);
            clip.transform.pos_y = r.real(-0.3, 0.3);
        }
        if after {
            tl.tracks[lowest].clips.push(clip);
        } else {
            let mut track = Track::new(StreamKind::Video, "TT");
            track.id = r.id();
            track.clips.push(clip);
            let at = tl
                .tracks
                .iter()
                .rposition(|t| t.kind == StreamKind::Video)
                .map_or(0, |i| i + 1);
            tl.tracks.insert(at, track);
        }
    }
}

/// Whether a graph holds a title clip's chain: one that starts by stamping its pictures onto the
/// canvas' grid (`setpts=N+k,` and the clip's chain going on, where a floating title's ends on its pad).
fn stamped_chain(graph: &str) -> bool {
    graph.match_indices("setpts=N+").any(|(at, needle)| {
        let rest = &graph[at + needle.len()..];
        rest.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with(',')
    })
}

/// Every eighth title case is exported as a gif (no dice): the titles end on `vcomp`, ahead of the
/// palette, which the text overlay's and the plain cut's graphs never do.
fn svg_container_for(j: usize) -> Option<Container> {
    (j % 8 == 3).then_some(Container::Gif)
}

/// The encoder case `n` of the hardware family is built for: the AMF and VAAPI encoders in
/// turn, no dice (so the generator's draws are the ones it always made).
fn hw_encoder_for(n: usize) -> &'static str {
    ["h264_amf", "hevc_amf", "av1_amf", "h264_vaapi", "hevc_vaapi", "av1_vaapi"][n % 6]
}

/// One timeline of each family: what `project::persist_tests` freezes as the shape a file stores
/// (the blurred-fill one is the first of its family whose cut has a delivery frame, which is where
/// the timeline stores the fit; the keyed mask and crop one the first of its family keying both).
pub(crate) fn sample_timelines() -> Vec<Timeline> {
    let assets = pool();
    let blur = (BLUR_FIRST..MASK_CROP_FIRST)
        .find(|&i| case(i, &assets).timeline.format.is_some())
        .expect("a blurred-fill case with a delivery frame");
    let keys = |i: usize, props: &[Property]| {
        let tl = case(i, &assets).timeline;
        tl.tracks
            .iter()
            .flat_map(|t| &t.clips)
            .any(|c| props.iter().any(|p| c.is_keyed(*p)))
    };
    let mask_crop = (MASK_CROP_FIRST..RAMP_FIRST)
        .find(|&i| keys(i, &Property::MASK) && keys(i, &Property::CROP))
        .expect("a case keying a mask and a crop");
    [
        0,
        4,
        CASES + MASTER_CASES,
        CASES + MASTER_CASES + 1,
        SVG_FIRST,
        TITLE_FIRST,
        blur,
        mask_crop,
    ]
    .into_iter()
    .map(|i| case(i, &assets).timeline)
    .collect()
}

struct Case {
    timeline: Timeline,
    opts: ExportOptions,
    /// The two stills (time, sink) and the rest of what `build_still_args` takes.
    stills: [(f64, StillOutput); 2],
    region: Option<Region>,
    still_width: u32,
    /// The preview: playhead, fps, width, JPEG quality (clamped by the builder), decode acceleration.
    preview: (f64, f64, u32, u8, Option<String>),
    /// The export's pass, null sink and passlog file.
    pass: (PassPhase, &'static str, &'static str),
    /// The delivery frames one run exports the cut at; none for an ordinary export.
    variants: Vec<ExportVariant>,
    /// Whether the ffmpeg the case is built for has vid.stab (else a stabilized clip is `deshake`d).
    vidstab: bool,
}

#[rustfmt::skip]
fn case(i: usize, assets: &[Asset]) -> Case {
    let mut r = Rng::new(i as u64 + 1);
    let mut timeline = timeline(&mut r, &assets[..LIBRARY]);
    retarget(&mut timeline, i, assets);
    retarget_alpha(&mut timeline, i, assets);
    retarget_hdr43(&mut timeline, i, assets);
    retarget_tiny(&mut timeline, i);
    retarget_late(&mut timeline, i, assets);
    if (CASES..CASES + MASTER_CASES).contains(&i) {
        timeline.master = master_for(i - CASES);
    }
    if i >= CASES + MASTER_CASES {
        channels_for(&mut timeline, i - CASES - MASTER_CASES);
    }
    if (SVG_FIRST..TOGETHER_FIRST).contains(&i) {
        svg_for(&mut timeline, i - SVG_FIRST);
    }
    if (TITLE_FIRST..BLUR_FIRST).contains(&i) {
        title_clip_for(&mut timeline, i - TITLE_FIRST);
    }
    if (BLUR_FIRST..MASK_CROP_FIRST).contains(&i) {
        if let Some(format) = timeline.format.as_mut() {
            format.fit = Fit::Blur;
        }
    }
    if (MASK_CROP_FIRST..RAMP_FIRST).contains(&i) {
        mask_crop_for(&mut timeline, i - MASK_CROP_FIRST);
    }
    if (RAMP_FIRST..DENOISE_FIRST).contains(&i) {
        ramp_for(&mut timeline, i - RAMP_FIRST, assets);
    }
    if (DENOISE_FIRST..BLEND_FIRST).contains(&i) {
        denoise_for(&mut timeline, i - DENOISE_FIRST);
    }
    if (BLEND_FIRST..TOTAL).contains(&i) {
        blend_for(&mut timeline, i - BLEND_FIRST);
    }
    if (STABILIZE_FIRST..TOTAL).contains(&i) {
        stabilize_for(&mut timeline, i - STABILIZE_FIRST, assets);
    }
    let dur = timeline.duration();
    let edges: Vec<f64> = (timeline.tracks.iter().flat_map(|t| &t.clips))
        .flat_map(|c| [c.timeline_start, c.timeline_end(), c.timeline_end() - 1e-3, c.timeline_start + 1e-4])
        .collect();
    let overlay_edges: Vec<f64> = (timeline.overlays.iter().flat_map(|o| [o.start, o.end]))
        .chain(timeline.svg_overlays.iter().flat_map(|o| [o.start, o.end]))
        .collect();
    let time = |r: &mut Rng| match r.below(20) {
        0..=2 => 0.0,
        3 | 4 => dur,
        5 => -r.real(0.0, 2.0),
        6 | 7 if !overlay_edges.is_empty() => overlay_edges[r.below(overlay_edges.len())],
        8..=12 if !edges.is_empty() => edges[r.below(edges.len())],
        _ => r.real(0.0, dur.max(0.5)),
    };
    let sink = |r: &mut Rng| match r.below(4) {
        0 => StillOutput::JpegPipe { quality: r.pick(&[2, 4, 31]) },
        1 => StillOutput::File { path: "/golden/cover.png".into(), format: ImageFormat::Png, quality: 2 },
        2 => StillOutput::File { path: "/golden/cover.jpg".into(), format: ImageFormat::Jpeg, quality: r.pick(&[2, 15, 31]) },
        _ => StillOutput::RgbPipe,
    };
    let mut opts = options(&mut r, dur);
    if (CASES + MASTER_CASES + CHANNEL_CASES..SVG_FIRST).contains(&i) {
        opts.video_codec = Some(hw_encoder_for(i - CASES - MASTER_CASES - CHANNEL_CASES).into());
    }
    if (SVG_FIRST..TOGETHER_FIRST).contains(&i) {
        opts.container = svg_container_for(i - SVG_FIRST).unwrap_or(opts.container);
    }
    if (BLUR_FIRST..MASK_CROP_FIRST).contains(&i) {
        opts.fit = Fit::Blur;
    }
    let pass = match r.below(20) { 0..=11 => PassPhase::Single, 12..=15 => PassPhase::First, _ => PassPhase::Second };
    let mut c = Case {
        stills: [(time(&mut r), sink(&mut r)), (time(&mut r), sink(&mut r))],
        region: maybe(&mut r, 0.2, |r| Region { left: r.real(-0.1, 0.8), top: r.real(-0.1, 0.8), width: r.real(0.0, 1.2), height: r.real(0.0, 1.2) }),
        still_width: r.pick(&[320, 640, 960, 1280, 1920, u32::MAX]),
        preview: (time(&mut r), r.pick(&[24.0, 25.0, 29.97, 30.0, 60.0]), r.pick(&[2, 320, 640, 960, 1280, 1920, 100_000]),
                  r.pick(&[0, 1, 2, 6, 31, 32, 255]), maybe(&mut r, 0.3, |r| r.pick(&["auto", "none", ""]).into())),
        pass: (pass, r.pick(&["/dev/null", "NUL"]), r.pick(&["", "/golden/pass"])),
        timeline,
        opts,
        variants: Vec::new(),
        vidstab: !(STABILIZE_FIRST..TOTAL).contains(&i) || (i - STABILIZE_FIRST) % 4 != 3,
    };
    if (SVG_FIRST..TOGETHER_FIRST).contains(&i) {
        svg_playhead(&mut c, i - SVG_FIRST);
    }
    if (TOGETHER_FIRST..TITLE_FIRST).contains(&i) {
        together_for(&mut c, i - TOGETHER_FIRST);
    }
    c
}

/// A variant of `/golden/out.mp4`, named as `ExportVariant::beside` names it but spelled out:
/// `beside` joins with the platform's separator, and the digests must hold on every OS.
fn together_variant(delivery: Delivery) -> ExportVariant {
    let (w, h) = delivery.ratio();
    ExportVariant {
        delivery,
        output: format!("/golden/out-{w}x{h}.mp4").into(),
    }
}

/// The `j`-th multi-delivery case, on dice of its own: two or three delivery frames, titles on every
/// other case, framings on the clips, captions and chapters to mux on a quarter, and an encode one
/// run takes — every tenth case keeps an audio-only container, which none does.
fn together_for(c: &mut Case, j: usize) {
    const FRAMES: [(u32, u32, Fit); 5] = [
        (1080, 1920, Fit::Cover),
        (1080, 1080, Fit::Cover),
        (1080, 1350, Fit::Cover),
        (1920, 1080, Fit::Contain),
        (640, 360, Fit::Cover),
    ];
    let mut r = Rng::new(0x4445_4c56_0000 + j as u64);
    if j.is_multiple_of(2) {
        svg_for(&mut c.timeline, SVG_CASES + j);
    }
    for clip in c.timeline.tracks.iter_mut().flat_map(|t| &mut t.clips) {
        for ratio in [(9, 16), (1, 1), (4, 5)] {
            if r.chance(0.6) {
                let (l, t) = (r.real(0.0, 0.4), r.real(0.0, 0.2));
                clip.framings.push(crate::model::Framing {
                    aspect_w: ratio.0,
                    aspect_h: ratio.1,
                    crop_left: l,
                    crop_right: r.real(0.0, 0.4),
                    crop_top: t,
                    crop_bottom: r.real(0.0, 0.2),
                });
            }
        }
    }
    let mut frames = FRAMES.to_vec();
    for _ in 0..r.pick(&[2, 2, 3]) {
        let (w, h, fit) = frames.remove(r.below(frames.len()));
        c.variants.push(together_variant(Delivery::new(w, h, fit)));
    }
    let o = &mut c.opts;
    if matches!(o.rate_control, RateControl::TwoPass) {
        o.rate_control = RateControl::Bitrate;
    }
    if o.video_codec
        .as_deref()
        .is_some_and(|vc| enc_family(vc) != EncFamily::Software)
    {
        o.video_codec = Some("libx264".into());
    }
    if o.container.is_audio_only() && j % 10 != 7 {
        o.container = Container::Mp4;
    }
    if j % 4 == 1 {
        o.subtitle_input = Some("/golden/captions.srt".into());
        o.chapters_input = Some("/golden/chapters.txt".into());
    }
}

/// Every third animated-title case plays from a frame of the stream's own grid in the middle of its
/// first title, with no dice of its own (the playhead the generator drew is replaced, and nothing
/// after it moves): the stream then reads the title's sequence from the picture that frame needs,
/// `-start_number`, which a playhead between frames (nearly all the drawn ones) never does.
fn svg_playhead(c: &mut Case, j: usize) {
    if !j.is_multiple_of(3) {
        return;
    }
    let Some(o) = c.timeline.svg_overlays.first() else { return };
    let fps = c.preview.1;
    c.preview.0 = ((o.start + o.end) / 2.0 * fps).round() / fps;
}

// ---- digests and coverage ------------------------------------------------------

/// `db_to_linear` is `10f64.powf(db / 20.0)`, and a libm's `pow` is not correctly
/// rounded: the last digits differ between glibc builds (with and without FMA),
/// macOS, Windows and arm. The compressor and gate numbers (`threshold=`, `makeup=`)
/// are therefore kept to 10 significant digits, which still pins the dB mapping: a
/// changed formula moves the third digit, a different libm only the seventeenth.
fn round_libm(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, key)) = ["threshold=", "makeup="]
        .iter()
        .filter_map(|k| Some((rest.find(k)?, k.len())))
        .min()
    {
        let (head, tail) = rest.split_at(at + key);
        out += head;
        let digits = tail
            .find(|c: char| !(c.is_ascii_digit() || ".eE+-".contains(c)))
            .unwrap_or(tail.len());
        match tail[..digits].parse::<f64>() {
            Ok(v) => out += &format!("{v:.9e}"),
            Err(_) => out += &tail[..digits],
        }
        rest = &tail[digits..];
    }
    out + rest
}

fn repr(args: &Result<Vec<String>>) -> String {
    match args {
        Ok(a) => round_libm(&a.join("\0")),
        Err(e) => format!("ERR\0{e}"),
    }
}

/// [`repr`] of the one run that exports every variant: its argv and where its outputs sit.
fn repr_together(built: Result<Option<together::TogetherArgs>>) -> String {
    match built {
        Ok(Some(t)) => format!("{}\0sinks\0{:?}", repr(&Ok(t.args)), t.sinks_from_end),
        Ok(None) => "NOT-TOGETHER".to_string(),
        Err(e) => repr(&Err(e)),
    }
}

/// What one case builds: the argv text of each builder (a case holding HDR footage
/// built with `zscale` present *and* absent; a case with the master limiter on has its
/// export built once more for an ffmpeg whose `alimiter` has no `latency`, the one
/// place the other two builders — which carry no sound — would only repeat
/// themselves), and the transition branches it took.
fn build(c: &Case, assets: &[Asset]) -> ([String; 3], Vec<String>) {
    let hdr = |id| assets.iter().any(|a| a.id == id && a.hdr().is_some());
    let both = c.timeline.tracks.iter().flat_map(|t| &t.clips).any(|k| hdr(k.asset_id));
    let mut text = [String::new(), String::new(), String::new()];
    let (pass, null_sink, passlog) = c.pass;
    let export = || {
        if !c.variants.is_empty() {
            return repr_together(together::build_together_args(&c.timeline, assets, &c.variants, &c.opts));
        }
        repr(&build_export_args_phase(
            &c.timeline,
            assets,
            "/golden/out.mp4",
            &c.opts,
            pass,
            null_sink,
            passlog,
        ))
    };
    for &zscale in if both { &[true, false][..] } else { &[true][..] } {
        pinned(zscale, true, || {
            with_vidstab(c.vidstab, || {
                text[0] += &export();
                for (t, out) in &c.stills {
                    text[1] += &repr(&build_still_args(
                        &c.timeline,
                        assets,
                        &c.opts,
                        *t,
                        c.still_width,
                        c.region,
                        out,
                    ));
                    text[1].push('\u{1}');
                }
                let (start, fps, width, quality, hw) = c.preview.clone();
                text[2] += &repr(&build_preview_args_with(&c.timeline, assets, start, fps, width, quality, hw));
            });
        });
    }
    if c.timeline.master.limiter {
        pinned(true, false, || with_vidstab(c.vidstab, || text[0] += &export()));
    }
    (text, transitions(c, assets))
}

/// `f` as if this ffmpeg did (or did not) have `zscale` and an `alimiter` with `latency`, its `afftdn`
/// delayed what it puts out, and the shipped default font were at a fixed path; with vid.stab, and the pool's
/// stabilization analyses handed over without a disk ([`pinned_analyses`]).
fn pinned<R>(zscale: bool, latency: bool, f: impl FnOnce() -> R) -> R {
    with_zscale(zscale, || {
        with_alimiter_latency(latency, || {
            with_afftdn_delay(true, || {
                crate::fonts::with_pinned_default_font(|| {
                    crate::svg::raster::with_pinned_dir("/kerf-test/svg", || {
                        crate::stabilize::pinned::with(pinned_analyses(), || with_vidstab(true, f))
                    })
                })
            })
        })
    })
}

/// Which way each transition went, from what `transition_fx` did with it: a dip
/// next to a clip or across a gap, a dissolve or motion transition with a source
/// handle to borrow, without one (a hard cut), or across a gap.
fn transitions(c: &Case, assets: &[Asset]) -> Vec<String> {
    let tl = c.timeline.for_render();
    let fx = transition_fx(&tl, assets);
    let (mut out, mut base) = (Vec::new(), 0);
    for track in &tl.tracks {
        let mut order: Vec<usize> = (0..track.clips.len()).collect();
        order.sort_by(|&a, &b| track.clips[a].timeline_start.total_cmp(&track.clips[b].timeline_start));
        for (w, &j) in order.iter().enumerate() {
            let clip = &track.clips[j];
            let Some(tr) = clip
                .transition_in
                .filter(|t| t.duration > 0.0 && track.kind == StreamKind::Video)
            else {
                continue;
            };
            let adjacent = w > 0 && (track.clips[order[w - 1]].timeline_end() - clip.timeline_start).abs() < 1e-3;
            let moved = fx[base + j].xfade_in > 0.0 || fx[base + j].move_in.is_some();
            let state = match (adjacent, tr.kind.dip_color().is_some(), moved) {
                (false, ..) => "gap",
                (true, true, _) => "adjacent",
                (true, false, true) => "handle",
                (true, false, false) => "no-handle",
            };
            out.push(format!("transition/{}/{state}", tr.kind.as_str()));
        }
        base += track.clips.len();
    }
    out
}

/// The families that come from the case's structure rather than from argv text.
const STRUCTURAL: [&str; 48] = [
    "speed-ramp-hold",
    "speed-ramp-reversed",
    "speed-ramp-tail",
    "still-speed-ramp",
    "muted-track",
    "solo-track",
    "disabled-clip",
    "no-video",
    "hwaccel-skipped",
    "preview-quality-clamped-low",
    "preview-quality-clamped-high",
    "reframe-one-channel",
    "reframe-single-keyframe",
    "reframe-held",
    "time-negative",
    "zoom-keyed",
    "zoom-keyed-constant",
    "still-zoom-last",
    "alpha-source",
    "zoom-keyed-last",
    "preview-zoom-keyed-last",
    "alpha-kept",
    "preview-alpha-kept",
    "master-nonfinite",
    "channel-partial-transform",
    "channel-off-the-bundle",
    "channel-held-static",
    "channel-colour-only",
    "channel-opacity-alone",
    "still-tail",
    "still-travel",
    "svg-under-text",
    "svg-gif",
    "svg-offset",
    "title-clip",
    "title-clip-dip",
    "title-clip-under-transition",
    "still-title-clip",
    "preview-title-clip",
    "bt709-mix",
    "still-bt709-mix",
    "preview-bt709-mix",
    "zoom-folded",
    "preview-zoom-folded",
    "still-zoom-folded",
    "cover-uncropped",
    "preview-cover-uncropped",
    "still-cover-uncropped",
];

fn expected(table: &[(&str, &str)]) -> Vec<String> {
    let mut f: Vec<String> = table.iter().map(|(n, _)| n.to_string()).collect();
    f.extend(STRUCTURAL.iter().chain(&["time-at-overlay-edge"]).map(|s| s.to_string()));
    for k in TransitionKind::ALL {
        let states: &[&str] = if k.dip_color().is_some() {
            &["adjacent", "gap"]
        } else {
            &["handle", "no-handle", "gap"]
        };
        f.extend(states.iter().map(|s| format!("transition/{}/{s}", k.as_str())));
    }
    f
}

/// Whether a clip chain of the still graph **ends** in the zoom (`scale=iw*S:ih*S`): only a
/// moving zoom is run last, every other clip zooms first and ends on `setsar` or later.
fn still_zoom_last(still: &str) -> bool {
    still.split(';').any(|segment| {
        let Some((head, label)) = segment.trim_end().rsplit_once('[') else {
            return false;
        };
        label.starts_with('v')
            && label.ends_with(']')
            && head.rsplit(',').next().is_some_and(|last| last.starts_with("scale=iw*"))
    })
}

/// Splits `s` at the `sep`s outside single quotes (an expression's own commas, and the semicolons of a
/// keyed mask's statements, are quoted). A backslash escapes the character after it **outside** quotes,
/// as the filtergraph parser reads it (inside them it is a backslash: `'It\'\''s'` is one quoted
/// piece, an escaped apostrophe, another quoted piece).
fn split_unquoted(s: &str, sep: char) -> Vec<&str> {
    let (mut out, mut quoted, mut escaped, mut from) = (Vec::new(), false, false, 0);
    for (i, c) in s.char_indices() {
        if std::mem::take(&mut escaped) {
            continue;
        }
        match c {
            '\\' if !quoted => escaped = true,
            '\'' => quoted = !quoted,
            c if c == sep && !quoted => {
                out.push(&s[from..i]);
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[from..]);
    out
}

/// Splits `s` at the commas outside single quotes (an expression's own commas are quoted).
fn filters(s: &str) -> Vec<&str> {
    split_unquoted(s, ',')
}

/// How the clip chains of an export or preview graph end: `(zoom last, alpha kept)`. A chain
/// ending `format=yuva420p` is either a moving zoom (the `scale ... eval=frame` right before
/// it) or a source whose alpha channel the chain keeps; an ordinary one ends in the
/// delivery's own format, or in whatever the last effect is.
fn chain_tails(graph: &str) -> (bool, bool) {
    let (mut zoom, mut alpha) = (false, false);
    for segment in graph.split(';') {
        let Some((head, label)) = segment.trim_end().rsplit_once('[') else {
            continue;
        };
        if !(label.starts_with('v') && label.ends_with(']') && label[1..label.len() - 1].chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        let f = filters(head);
        if f.last() == Some(&"format=yuva420p") && f.len() >= 2 {
            let before = f[f.len() - 2];
            if before.starts_with("scale=w=") && before.contains(":eval=frame") {
                zoom = true;
            } else {
                alpha = true;
            }
        }
    }
    (zoom, alpha)
}

/// Whether a clip chain of a graph holds a zoom in folded into the fit: the fit's own `scale`
/// with a product for its box (`scale=<W>*<S>:<H>*<S>:force_original_aspect_ratio=decrease`), where
/// the plain fit's box is the frame's own numbers and a zoom after it reads `iw` and `ih`.
fn zoom_folded(graph: &str) -> bool {
    split_unquoted(graph, ';').into_iter().any(|segment| {
        let Some((head, _)) = segment.trim_end().rsplit_once('[') else {
            return false;
        };
        filters(head).iter().any(|filter| {
            let Some(args) = filter.split_once("scale=").map(|(_, a)| a) else {
                return false;
            };
            let boxed = args.starts_with("w='") || args.split(':').next().is_some_and(|b| b.contains('*'));
            boxed && !args.contains("iw") && args.contains("force_original_aspect_ratio=decrease")
        })
    })
}

/// Whether a clip chain of a graph holds a Cover clip that is not cropped to the frame: a `scale` to
/// cover (`force_original_aspect_ratio=increase`) that the plain Cover fit would follow with
/// `crop=<W>:<H>`, followed instead by the export's centred `crop=w='iw-4*floor(` (`bounded`) or, in a
/// still, by nothing of the kind.
fn cover_uncropped(graph: &str, bounded: bool) -> bool {
    split_unquoted(graph, ';').into_iter().any(|segment| {
        let Some((head, _)) = segment.trim_end().rsplit_once('[') else {
            return false;
        };
        let f = filters(head);
        f.iter().enumerate().any(|(i, filter)| {
            filter.contains("scale=")
                && filter.contains("force_original_aspect_ratio=increase")
                && f.get(i + 1).is_some_and(|next| {
                    if bounded {
                        next.starts_with("crop=w='iw-4*floor(")
                    } else {
                        !next.starts_with("crop=")
                    }
                })
        })
    })
}

/// The families a case hits: its transition branches, the table entries whose text is
/// in the argv they are about (see [`FAMILIES`]), and what only the structure shows.
fn families(c: &Case, assets: &[Asset], text: &[String; 3], table: &[(&str, &str)], transitions: Vec<String>) -> Vec<String> {
    let flat = text.each_ref().map(|t| t.replace(['\0', '\u{1}'], " "));
    let mut f = transitions;
    for (name, needle) in table {
        let scope = if name.starts_with("still-") {
            1
        } else if name.starts_with("preview-") {
            2
        } else {
            0
        };
        if flat[scope].contains(needle) {
            f.push(name.to_string());
        }
    }
    let tl = &c.timeline;
    let rendered = tl.for_render();
    let clips = || {
        rendered
            .tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| &t.clips)
    };
    let times = [c.stills[0].0, c.stills[1].0, c.preview.0];
    // A still inside a transition: a clip kept on screen past its end, or one a slide or a push moves.
    let fps = export_format(&rendered, assets, &c.opts).fps;
    let layers = |t: f64| still_layers(&rendered, assets, fps, t.max(0.0));
    let still_transitions = c.stills.iter().fold((false, false), |(tail, travel), (t, _)| {
        let layers = layers(*t);
        (
            tail || layers.iter().any(|l| t.max(0.0) >= l.active.clip.timeline_end()),
            travel || layers.iter().any(|l| l.motion != (0.0, 0.0)),
        )
    });
    // Which graphs hold untagged HD footage beside an input that names a matrix (the pool's HDR
    // clips; it holds no tagged SDR one): the BT.709 label's oracle, by asset *name* so it does not
    // read the facts the builders decide from. The export's graph is the range's slice, the
    // preview's the cut from the playhead, a still's the layers on screen.
    let untagged_hd = |name: &str| {
        let name = name.strip_suffix("-padded").unwrap_or(name);
        matches!(
            name.strip_suffix("-late").unwrap_or(name),
            "interview" | "broll" | "phone" | "wide" | "sphere" | "lenses"
        )
    };
    let mixes = |ids: Vec<Uuid>| {
        let names: Vec<&str> = ids
            .iter()
            .filter_map(|id| assets.iter().find(|a| a.id == *id))
            .map(|a| a.name.as_str())
            .collect();
        names.iter().any(|n| untagged_hd(n)) && names.iter().any(|n| n.starts_with("hdr-"))
    };
    let ids_of = |tl: &Timeline| -> Vec<Uuid> {
        tl.tracks
            .iter()
            .filter(|t| t.kind == StreamKind::Video)
            .flat_map(|t| t.clips.iter().map(|k| k.asset_id))
            .collect()
    };
    let export_graph = match effective_range(&rendered, &c.opts) {
        Some((from, to)) => rendered.slice(from, to),
        None => rendered.clone(),
    };
    let preview_graph = {
        let start = c.preview.0;
        (start.is_finite() && start < rendered.duration()).then(|| rendered.slice(start, rendered.duration()))
    };
    let (bt709_export, bt709_preview, bt709_still) = (
        !c.opts.container.is_audio_only() && mixes(ids_of(&export_graph)),
        preview_graph.as_ref().is_some_and(|g| mixes(ids_of(g))),
        c.stills
            .iter()
            .any(|(t, _)| mixes(layers(*t).iter().map(|l| l.active.clip.asset_id).collect())),
    );
    let channels = |rf: &Reframe| {
        let k = &rf.keyframes;
        [
            k.iter().any(|x| x.yaw != k[0].yaw),
            k.iter().any(|x| x.pitch != k[0].pitch),
            k.iter().any(|x| x.roll != k[0].roll),
            k.iter().any(|x| x.fov != k[0].fov),
        ]
        .iter()
        .filter(|moves| **moves)
        .count()
    };
    let moving: Vec<usize> = clips()
        .filter_map(|k| k.reframe.as_ref().filter(|r| r.keyframes.len() > 1))
        .map(channels)
        .collect();
    for (hit, name) in [
        (tl.tracks.iter().any(|t| t.muted), "muted-track"),
        (tl.tracks.iter().any(|t| t.solo), "solo-track"),
        (tl.tracks.iter().flat_map(|t| &t.clips).any(|k| !k.enabled), "disabled-clip"),
        (clips().next().is_none(), "no-video"),
        (
            matches!(c.opts.hwaccel.as_deref(), Some(h) if h.is_empty() || h.eq_ignore_ascii_case("none")),
            "hwaccel-skipped",
        ),
        (c.preview.3 < 2, "preview-quality-clamped-low"),
        (c.preview.3 > 31, "preview-quality-clamped-high"),
        (moving.contains(&1), "reframe-one-channel"),
        (moving.contains(&0), "reframe-held"),
        (
            clips().any(|k| matches!(&k.reframe, Some(r) if r.keyframes.len() == 1)),
            "reframe-single-keyframe",
        ),
        (times.iter().any(|t| *t < 0.0), "time-negative"),
        (
            !tl.master.volume.is_finite() || !tl.master.ceiling_db.is_finite(),
            "master-nonfinite",
        ),
        // A keyed clip whose scale moves is the one the export restructures; one
        // whose scale holds still (position / rotation / opacity keys only) keeps
        // the chain it always had, so both are covered.
        (clips().any(Clip::zoom_animated), "zoom-keyed"),
        (clips().any(|k| k.is_animated() && !k.zoom_animated()), "zoom-keyed-constant"),
        (still_zoom_last(&flat[1]), "still-zoom-last"),
        // Per-property channels: a transform with some numbers keyed and the rest static, a
        // number taken off the legacy bundle (keyed on its own, or held static), and a clip
        // that is animated in colour alone (its picture is placed as a static one).
        (
            clips().any(|k| k.is_animated() && Property::TRANSFORM.iter().any(|p| !k.is_keyed(*p))),
            "channel-partial-transform",
        ),
        (
            clips().any(|k| !k.keyframes.is_empty() && k.channels.iter().any(|c| c.prop.is_transform() && !c.keys.is_empty())),
            "channel-off-the-bundle",
        ),
        (
            clips().any(|k| !k.keyframes.is_empty() && k.channels.iter().any(|c| c.prop.is_transform() && c.keys.is_empty())),
            "channel-held-static",
        ),
        (clips().any(|k| k.color_animated() && !k.is_animated()), "channel-colour-only"),
        (
            clips().any(|k| k.is_keyed(Property::Opacity) && !k.is_keyed(Property::Scale)),
            "channel-opacity-alone",
        ),
        // Titles laid under a text overlay, ahead of a gif's palette, and started part-way into
        // their animation (a range export slices them; some are drawn so).
        (
            flat[0].contains("][svg0]overlay=") && flat[0].contains("drawtext="),
            "svg-under-text",
        ),
        (
            flat[0].contains("][svg0]overlay=") && flat[0].contains("paletteuse="),
            "svg-gif",
        ),
        (
            flat[0].contains("][svg0]overlay=") && export_timeline(tl, &c.opts).svg_overlays.iter().any(|o| o.offset > 0.0),
            "svg-offset",
        ),
        // Titles on tracks: drawn by the export, dipping, under a transition of either kind, in a still,
        // and in the stream.
        (stamped_chain(&flat[0]), "title-clip"),
        (
            stamped_chain(&flat[0])
                && export_timeline(tl, &c.opts)
                    .title_clips()
                    .any(|(k, _)| k.transition_in.is_some_and(|t| t.kind.dip_color().is_some())),
            "title-clip-dip",
        ),
        (
            stamped_chain(&flat[0])
                && export_timeline(tl, &c.opts)
                    .title_clips()
                    .any(|(k, _)| k.transition_in.is_some_and(|t| t.kind.dip_color().is_none())),
            "title-clip-under-transition",
        ),
        (
            c.stills
                .iter()
                .any(|(t, _)| layers(*t).iter().any(|l| l.active.clip.is_generated())),
            "still-title-clip",
        ),
        (stamped_chain(&flat[2]), "preview-title-clip"),
        (bt709_export, "bt709-mix"),
        (bt709_still, "still-bt709-mix"),
        (bt709_preview, "preview-bt709-mix"),
        (still_transitions.0, "still-tail"),
        (still_transitions.1, "still-travel"),
        (chain_tails(&flat[0]).0, "zoom-keyed-last"),
        (chain_tails(&flat[2]).0, "preview-zoom-keyed-last"),
        (zoom_folded(&flat[0]), "zoom-folded"),
        (zoom_folded(&flat[2]), "preview-zoom-folded"),
        (zoom_folded(&flat[1]), "still-zoom-folded"),
        (cover_uncropped(&flat[0], true), "cover-uncropped"),
        (cover_uncropped(&flat[2], true), "preview-cover-uncropped"),
        (cover_uncropped(&flat[1], false), "still-cover-uncropped"),
        (chain_tails(&flat[0]).1, "alpha-kept"),
        (chain_tails(&flat[2]).1, "preview-alpha-kept"),
        // Speed ramps: a hold among the keys, a ramp played backwards, one with a transition's tail
        // playing on at the last key's speed (picture or sound), and one on screen in a still.
        (
            rendered.tracks.iter().flat_map(|t| &t.clips).any(|k| {
                k.ramp()
                    .is_some_and(|keys| keys.iter().any(|key| key.easing == RampEasing::Hold))
            }),
            "speed-ramp-hold",
        ),
        (
            rendered
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .any(|k| k.is_ramped() && k.is_reversed()),
            "speed-ramp-reversed",
        ),
        (
            crate::clip_timing::clips_with_fx(&rendered, assets).any(|(_, _, k, fx)| k.is_ramped() && fx.tail > 0.0),
            "speed-ramp-tail",
        ),
        (
            c.stills
                .iter()
                .any(|(t, _)| layers(*t).iter().any(|l| l.active.clip.is_ramped())),
            "still-speed-ramp",
        ),
        (
            clips().any(|k| {
                assets.iter().any(|a| {
                    a.id == k.asset_id
                        && a.streams.iter().any(|s| {
                            s.kind == StreamKind::Video && s.pix_fmt.as_deref().is_some_and(crate::model::pix_fmt_has_alpha)
                        })
                })
            }),
            "alpha-source",
        ),
        (
            tl.overlays.iter().any(|o| times.contains(&o.start) || times.contains(&o.end)),
            "time-at-overlay-edge",
        ),
    ] {
        if hit {
            f.push(name.into());
        }
    }
    f
}

#[allow(clippy::print_stderr)]
fn dump(n: usize, assets: &[Asset]) {
    let c = case(n, assets);
    let (text, _) = build(&c, assets);
    eprintln!("case {n}: {:?}", c.opts);
    for (kind, t) in KINDS.iter().zip(&text) {
        eprintln!("-- {kind}\n{}", t.replace('\0', " ").replace('\u{1}', "\n"));
    }
}

/// A case's three digests and the families it hits.
type Done = ([u64; 3], Vec<String>);

/// One case's three digests and the families it hits. Cases are independent, so the
/// test spreads them over the machine's cores (`with_zscale` is per thread).
fn run(i: usize, assets: &[Asset], table: &[(&str, &str)]) -> Done {
    let c = case(i, assets);
    let (text, transitions) = build(&c, assets);
    let mut hit = families(&c, assets, &text, table, transitions);
    hit.sort();
    hit.dedup();
    (text.each_ref().map(|t| fnv1a(t)), hit)
}

#[test]
fn round_libm_keeps_the_mapping_and_drops_the_last_digits() {
    let a = "acompressor=threshold=0.031622776601683794:ratio=4:makeup=1.2589254117941673 agate=threshold=0.01";
    // One ulp of the compressor threshold: what another libm's `pow` may return.
    let b = "acompressor=threshold=0.03162277660168380:ratio=4:makeup=1.2589254117941675 agate=threshold=0.01";
    assert_ne!(a, b);
    assert_eq!(round_libm(a), round_libm(b));
    // A different dB mapping (`/ 10` rather than `/ 20`) is not hidden.
    assert_ne!(
        round_libm(a),
        round_libm("acompressor=threshold=0.001:ratio=4:makeup=1.2589254117941673 agate=threshold=0.01")
    );
    assert!(round_libm("a=threshold=0.5:b=1").contains("5.000000000e-1:b=1"));
}

/// Every export graph of the corpus reads back through `together::relabel` unchanged, and every
/// label it finds is one the builders write: no `[` inside a filter's arguments is taken for one.
#[test]
fn the_labels_one_run_renames_are_the_graphs_own() {
    let assets = pool();
    for i in (0..TOGETHER_FIRST).chain(BLUR_FIRST..TOTAL).step_by(3) {
        let c = case(i, &assets);
        let built = pinned(true, true, || {
            build_export_args(&c.timeline, &assets, "/golden/out.mp4", &c.opts)
        });
        let Ok(args) = built else { continue };
        let Some(at) = args.iter().position(|a| a == "-filter_complex") else {
            continue;
        };
        let graph = &args[at + 1];
        let mut odd = Vec::new();
        let same = together::relabel(graph, |label| {
            if !label.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == ':') {
                odd.push(label.to_string());
            }
            label.to_string()
        });
        assert_eq!(&same, graph, "case {i}");
        assert!(odd.is_empty(), "case {i}: {odd:?}");
    }
}

#[test]
#[allow(clippy::print_stderr)]
fn the_argv_builders_still_produce_the_golden_digests() {
    let assets = pool();
    if let Some(n) = std::env::var("KERF_GOLDEN_DUMP").ok().and_then(|v| v.parse().ok()) {
        dump(n, &assets);
    }
    let table: Vec<(&str, &str)> = FAMILIES.lines().filter_map(|l| l.split_once(' ')).collect();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    let mut results: Vec<(usize, Done)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|w| {
                let (assets, table) = (&assets, &table);
                s.spawn(move || {
                    (w..TOTAL)
                        .step_by(threads)
                        .map(|i| (i, run(i, assets, table)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    results.sort_by_key(|(i, _)| *i);

    let mut seen: BTreeMap<String, usize> = expected(&table).into_iter().map(|f| (f, 0)).collect();
    let mut digests: [Vec<u64>; 3] = Default::default();
    let (mut per_case, mut per_case_families) = (String::new(), String::new());
    for (i, (h, hit)) in &results {
        per_case += &format!("{i} {:016x} {:016x} {:016x}\n", h[0], h[1], h[2]);
        per_case_families += &format!("{i} {}\n", hit.join(","));
        h.iter().zip(&mut digests).for_each(|(h, d)| d.push(*h));
        for f in hit {
            *seen
                .get_mut(f)
                .unwrap_or_else(|| panic!("a family the oracle does not list: {f}")) += 1;
        }
    }
    if let Some(path) = std::env::var_os("KERF_GOLDEN_CASES") {
        std::fs::write(path, per_case).unwrap();
    }
    if let Some(path) = std::env::var_os("KERF_GOLDEN_FAMILIES") {
        std::fs::write(path, per_case_families).unwrap();
    }

    // The digests first, so a moved block is reported even when coverage fails too.
    let bless = std::env::var("KERF_GOLDEN_BLESS").as_deref() == Ok("1");
    let mut failures = Vec::new();
    for ((kind, cases), blessed) in KINDS.iter().zip(&digests).zip(BLESSED) {
        let blessed = blessed.replace("\r\n", "\n");
        let mut want = format!(
            "# {kind} argv digests (engine/cli/golden.rs): block, then FNV-1a of its {BLOCK} cases.\n\
             # Regenerate after an intended change: KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden\n"
        );
        for (i, block) in cases.chunks(BLOCK).enumerate() {
            want += &format!("{i} {:016x}\n", fnv1a(&format!("{block:x?}")));
        }
        if bless {
            let file = format!("src/engine/cli/golden/{kind}.txt");
            std::fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(&file), &want).unwrap();
            let moved = want != blessed;
            eprintln!(
                "golden: wrote {file} ({} blocks, {})",
                cases.chunks(BLOCK).count(),
                if moved { "changed" } else { "unchanged" }
            );
        } else if want != blessed {
            let bad: Vec<_> = want
                .lines()
                .zip(blessed.lines())
                .filter(|(a, b)| a != b)
                .map(|(a, _)| a.split(' ').next().unwrap().to_string())
                .collect();
            failures.push(format!(
                "the {kind} argv moved in blocks {bad:?} (case = block x {BLOCK} .. +{BLOCK}; an empty list is a stale file). If that is intended, \
                 re-bless (see the module docs) and review `git diff`; otherwise find the case with KERF_GOLDEN_CASES=<file>."
            ));
        }
    }
    if std::env::var_os("KERF_GOLDEN_COVERAGE").is_some() {
        let mut by_count: Vec<_> = seen.iter().collect();
        by_count.sort_by_key(|(_, n)| **n);
        eprintln!("golden: the {} thinnest families: {:?}", 12, &by_count[..12]);
    }
    // The BT.709 label appears in a graph exactly when it holds untagged HD beside an input that names
    // a matrix: every other case, single clips and graphs of untagged footage among them, is unmoved.
    let wrong: Vec<String> = results
        .iter()
        .flat_map(|(i, (_, hit))| {
            let has = |f: &str| hit.iter().any(|h| h == f);
            [("bt709", ""), ("bt709", "still-"), ("bt709", "preview-")]
                .into_iter()
                .filter(move |(name, scope)| has(&format!("{scope}{name}-label")) != has(&format!("{scope}{name}-mix")))
                .map(move |(name, scope)| format!("case {i}: {scope}{name}-label differs from {scope}{name}-mix"))
        })
        .collect();
    if !wrong.is_empty() {
        failures.push(format!(
            "the BT.709 label is not where untagged HD meets a matrix-naming input ({} cases): {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(8)]
        ));
    }
    let thin: Vec<_> = seen.iter().filter(|(_, n)| **n < MIN_PER_FAMILY).collect();
    if !thin.is_empty() {
        failures.push(format!(
            "families under {MIN_PER_FAMILY} cases (the generator stopped covering them, or the argv text changed): {thin:?}"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// What the BT.709 label may be asked of: pictures standing where the corpus' HDR clips stand.
/// The corpus names a matrix only with HDR, and its stills carry no tag, so the digests cannot
/// tell a decoder's default tag (a PNG's `gbr`, a JPEG's or WebP's or an MJPEG webcam's `bt470bg`,
/// all probed) from the file's word. Every case that holds an HDR clip is therefore built again with
/// that clip replaced by one of these, same size and place, and the label must appear in exactly the
/// graphs the stand-in names a matrix for: nowhere for the shapes that name nothing (a project of
/// untagged HD footage and a logo or a photo is as it always was), where HDR was for the shapes
/// that do.
const STAND_INS: &[(&str, bool, bool, &str, &str, &str)] = &[
    // (name, names a matrix, still image, codec, pix_fmt, tag)
    ("png rgba gbr", false, true, "png", "rgba", "gbr"),
    ("png gray gbr", false, true, "png", "gray", "gbr"),
    ("jpeg bt470bg", false, true, "mjpeg", "yuvj420p", "bt470bg"),
    ("webp bt470bg", false, true, "webp", "yuv420p", "bt470bg"),
    ("mjpeg video bt470bg", false, false, "mjpeg", "yuvj420p", "bt470bg"),
    ("rgb video gbr", false, false, "ffv1", "bgr0", "gbr"),
    // Each of these is kept off by one rule alone: `gbr` on a layout that is not RGB (a moving
    // picture with alpha), a decoder that tags everything it decodes with no `yuvj` to give it away
    // (MJPEG at 4:2:2, SpeedHQ, JPEG-LS grey, a decode-only one of the list), AMV, and an RGB picture
    // whose tag is not `gbr` from a codec that is not on the list.
    ("rgba video gbr", false, false, "ffv1", "bgra", "gbr"),
    ("mjpeg video yuv422p bt470bg", false, false, "mjpeg", "yuv422p", "bt470bg"),
    ("speedhq yuv422p bt470bg", false, false, "speedhq", "yuv422p", "bt470bg"),
    ("amv yuvj420p bt470bg", false, false, "amv", "yuvj420p", "bt470bg"),
    ("jpegls rgb24 bt470bg", false, false, "jpegls", "rgb24", "bt470bg"),
    ("jpegls gray bt470bg", false, false, "jpegls", "gray", "bt470bg"),
    ("thp yuv420p bt470bg", false, false, "thp", "yuv420p", "bt470bg"),
    ("ffv1 rgb24 bt470bg", false, false, "ffv1", "rgb24", "bt470bg"),
    // The controls: a tag that is the file's word, `yuvj420p` full-range H.264 (OBS, drones) included.
    ("h264 bt709", true, false, "h264", "yuv420p", "bt709"),
    ("h264 smpte170m", true, false, "h264", "yuv420p", "smpte170m"),
    ("h264 yuvj420p bt709", true, false, "h264", "yuvj420p", "bt709"),
];

/// How many stand-ins there are.
const SHAPES: usize = STAND_INS.len();

#[test]
fn a_decoders_default_tag_names_no_matrix_in_any_graph_of_the_corpus() {
    const LABEL: &str = "setparams=colorspace=bt709";
    let assets = pool();
    let hdr_ids: Vec<Uuid> = assets.iter().filter(|a| a.hdr().is_some()).map(|a| a.id).collect();
    assert!(!hdr_ids.is_empty());
    let stand_in = |shape: usize| -> Vec<Asset> {
        let (_, _, image, codec, pix_fmt, tag) = STAND_INS[shape];
        assets
            .iter()
            .map(|a| {
                let mut a = a.clone();
                if hdr_ids.contains(&a.id) {
                    for s in a.streams.iter_mut().filter(|s| s.kind == StreamKind::Video) {
                        *s = StreamInfo {
                            image,
                            fps: (!image).then_some(s.fps.unwrap_or(30.0)),
                            codec: codec.into(),
                            pix_fmt: Some(pix_fmt.into()),
                            color_space: Some(tag.into()),
                            color_transfer: None,
                            color_primaries: None,
                            ..s.clone()
                        };
                    }
                }
                a
            })
            .collect()
    };
    let swapped: Vec<Vec<Asset>> = (0..SHAPES).map(stand_in).collect();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    // (shape, case, kind) triples that disagree, and how many cases each shape covered / labelled.
    type Found = (Vec<String>, [usize; SHAPES], [usize; SHAPES]);
    let results: Vec<Found> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|w| {
                let (assets, swapped, hdr_ids) = (&assets, &swapped, &hdr_ids);
                s.spawn(move || {
                    let (mut bad, mut covered, mut labelled) = (Vec::new(), [0usize; SHAPES], [0usize; SHAPES]);
                    for i in (w..TOTAL).step_by(threads) {
                        let c = case(i, assets);
                        let holds_hdr = c
                            .timeline
                            .for_render()
                            .tracks
                            .iter()
                            .filter(|t| t.kind == StreamKind::Video)
                            .flat_map(|t| &t.clips)
                            .any(|k| hdr_ids.contains(&k.asset_id));
                        if !holds_hdr {
                            continue;
                        }
                        let shape = i % SHAPES;
                        let (text, _) = build(&c, &swapped[shape]);
                        covered[shape] += 1;
                        let named = STAND_INS[shape].1;
                        let before = named.then(|| build(&c, assets).0);
                        for (k, kind) in KINDS.iter().enumerate() {
                            let has = text[k].contains(LABEL);
                            // A shape that names a matrix labels exactly where HDR did.
                            let want = before.as_ref().is_some_and(|b| b[k].contains(LABEL));
                            labelled[shape] += usize::from(has);
                            if has != want {
                                bad.push(format!(
                                    "case {i}, {kind}, {}: label {has}, wanted {want}",
                                    STAND_INS[shape].0
                                ));
                            }
                        }
                    }
                    (bad, covered, labelled)
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    let (mut covered, mut labelled) = ([0usize; SHAPES], [0usize; SHAPES]);
    let mut bad = Vec::new();
    for (b, c, l) in results {
        bad.extend(b);
        for n in 0..SHAPES {
            covered[n] += c[n];
            labelled[n] += l[n];
        }
    }
    assert!(
        bad.is_empty(),
        "{} disagreements, first: {:?}",
        bad.len(),
        &bad[..bad.len().min(6)]
    );
    for (n, (name, named, ..)) in STAND_INS.iter().enumerate() {
        assert!(covered[n] >= MIN_PER_FAMILY, "{name}: only {} cases", covered[n]);
        // The controls are not empty: a shape that names a matrix does label.
        assert_eq!(
            labelled[n] >= MIN_PER_FAMILY,
            *named,
            "{name}: {} labelled graphs",
            labelled[n]
        );
    }
}

// ---- the export in parts -----------------------------------------------------------------------

/// The limits the parts family is planned under: small enough that the corpus' footage (1080p and up)
/// is cut into parts.
const PARTS_LIMITS: segments::SegmentLimits = segments::SegmentLimits {
    decode_mpx: 5.0,
    inputs: 4,
    preroll_s: 10.0,
};
const PARTS_BLESSED: &str = include_str!("golden/parts.txt");

/// The families of the parts oracle: what must show in at least [`MIN_PER_FAMILY`] cases.
const PARTS_FAMILIES: [&str; 14] = [
    "parts",
    "parts-refused",
    "parts-many",
    "parts-straddler",
    "parts-restart",
    "parts-bare-canvas",
    "parts-sound",
    "parts-svg-pictures",
    "parts-held",
    "parts-two-pass",
    "parts-10-bit",
    "parts-hdr",
    "parts-caption",
    "parts-faststart",
];

/// The argv of every part of a case's export in parts and of its last pass (the case's own two-pass
/// phase), as one text, and the parts families it hits. `NO-PARTS` for a case the plan leaves whole.
fn parts_text(c: &Case, assets: &[Asset]) -> (String, Vec<&'static str>) {
    pinned(true, true, || {
        let Some(plan) = segments::plan_export(&c.timeline, assets, &c.opts, PARTS_LIMITS) else {
            return ("NO-PARTS".to_string(), vec!["parts-refused"]);
        };
        let floating = svg_layer::svg_sequences(&plan.cut, &plan.fmt);
        let mut text = String::new();
        let contents: Vec<_> = plan.parts.iter().map(|p| segments::part_contents(&plan, p)).collect();
        for (k, part) in plan.parts.iter().enumerate() {
            text += &repr(&segments::build_part_args(
                &plan,
                assets,
                &c.opts,
                part,
                &floating,
                &format!("/golden/part-{k:04}.mkv"),
                "-fps_mode",
            ));
            text.push('\u{1}');
        }
        let (pass, null_sink, passlog) = c.pass;
        text += &repr(&segments::build_final_args(
            &plan,
            assets,
            "/golden/out.mp4",
            &c.opts,
            pass,
            null_sink,
            passlog,
            "/golden/parts.ffconcat",
        ));
        let words = text.replace(['\0', '\u{1}'], " ");
        let flats = contents.first().map_or(0, |c| c.include.len());
        let mut hit = vec!["parts"];
        let mut add = |on: bool, name: &'static str| {
            if on {
                hit.push(name);
            }
        };
        add(plan.parts.len() >= 4, "parts-many");
        add(
            (0..flats).any(|f| contents.iter().filter(|c| c.include[f]).count() >= 2),
            "parts-straddler",
        );
        add(contents.iter().any(|c| !c.restarts.is_empty()), "parts-restart");
        add(contents.iter().any(|c| c.include.iter().all(|&on| !on)), "parts-bare-canvas");
        add(words.contains(" -vn "), "parts-sound");
        add(words.contains("-start_number"), "parts-svg-pictures");
        add(words.contains("trim=end_frame="), "parts-held");
        add(words.contains("-pass 1"), "parts-two-pass");
        add(words.contains("p10le"), "parts-10-bit");
        add(words.contains("zscale="), "parts-hdr");
        add(words.contains("drawtext="), "parts-caption");
        add(words.contains("+faststart"), "parts-faststart");
        (round_libm(&text), hit)
    })
}

#[test]
#[allow(clippy::print_stderr)]
fn the_parts_argv_builders_produce_the_golden_digests() {
    let assets = pool();
    if let Some(n) = std::env::var("KERF_GOLDEN_PARTS_DUMP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        let (text, hit) = parts_text(&case(n, &assets), &assets);
        eprintln!("case {n}: {hit:?}\n{}", text.replace('\0', " ").replace('\u{1}', "\n"));
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    let mut results: Vec<(usize, u64, Vec<&'static str>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|w| {
                let assets = &assets;
                s.spawn(move || {
                    (w..TOTAL)
                        .step_by(threads)
                        .map(|i| {
                            let (text, hit) = parts_text(&case(i, assets), assets);
                            (i, fnv1a(&text), hit)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    results.sort_by_key(|(i, ..)| *i);
    if let Some(path) = std::env::var_os("KERF_GOLDEN_PARTS_CASES") {
        let lines: String = results.iter().map(|(i, h, _)| format!("{i} {h:016x}\n")).collect();
        std::fs::write(path, lines).unwrap();
    }

    let digests: Vec<u64> = results.iter().map(|(_, h, _)| *h).collect();
    let mut want = format!(
        "# parts argv digests (engine/cli/golden.rs): block, then FNV-1a of its {BLOCK} cases.\n\
         # Regenerate after an intended change: KERF_GOLDEN_BLESS=1 cargo test -p kerf-core --no-default-features golden\n"
    );
    for (i, block) in digests.chunks(BLOCK).enumerate() {
        want += &format!("{i} {:016x}\n", fnv1a(&format!("{block:x?}")));
    }
    let blessed = PARTS_BLESSED.replace("\r\n", "\n");
    let mut failures = Vec::new();
    if std::env::var("KERF_GOLDEN_BLESS").as_deref() == Ok("1") {
        let file = "src/engine/cli/golden/parts.txt";
        std::fs::write(Path::new(env!("CARGO_MANIFEST_DIR")).join(file), &want).unwrap();
        eprintln!(
            "golden: wrote {file} ({} blocks, {})",
            digests.chunks(BLOCK).count(),
            if want != blessed { "changed" } else { "unchanged" }
        );
    } else if want != blessed {
        let bad: Vec<_> = want
            .lines()
            .zip(blessed.lines())
            .filter(|(a, b)| a != b)
            .map(|(a, _)| a.split(' ').next().unwrap().to_string())
            .collect();
        failures.push(format!(
            "the parts argv moved in blocks {bad:?} (case = block x {BLOCK} .. +{BLOCK}; an empty list is a stale file). If that is intended, \
             re-bless (see the module docs) and review `git diff`; otherwise find the case with KERF_GOLDEN_PARTS_CASES=<file>."
        ));
    }
    let mut seen: BTreeMap<&str, usize> = PARTS_FAMILIES.iter().map(|f| (*f, 0)).collect();
    for (_, _, hit) in &results {
        for f in hit {
            *seen
                .get_mut(f)
                .unwrap_or_else(|| panic!("a family the oracle does not list: {f}")) += 1;
        }
    }
    let thin: Vec<_> = seen.iter().filter(|(_, n)| **n < MIN_PER_FAMILY).collect();
    if !thin.is_empty() {
        failures.push(format!(
            "parts families under {MIN_PER_FAMILY} cases (the generator stopped covering them, or the argv text changed): {thin:?}"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// In every case of the corpus a part draws each clip it holds with the chain, and the overlay, the clip
/// has in the whole export's graph: the fit order, the zoom, the crop, the effects. The digests pin the
/// parts' argv, which a change to the whole chain moves along with it; this is what says the move was
/// not one the parts alone made.
#[test]
fn a_part_draws_every_clip_of_the_corpus_as_the_whole_export_does() {
    let assets = pool();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    let results: Vec<(usize, Option<String>, usize)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|w| {
                let assets = &assets;
                s.spawn(move || {
                    (w..TOTAL)
                        .step_by(threads)
                        .filter_map(|i| {
                            let c = case(i, assets);
                            pinned(true, true, || {
                                let plan = segments::plan_export(&c.timeline, assets, &c.opts, PARTS_LIMITS)?;
                                let whole = build_export_args(&c.timeline, assets, "/golden/out.mp4", &c.opts).ok()?;
                                Some(match segments_tests::parts_draw_as_whole(&plan, assets, &c.opts, &whole) {
                                    Ok(n) => (i, None, n),
                                    Err(e) => (i, Some(e), 0),
                                })
                            })
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    let wrong: Vec<_> = results.iter().filter(|(_, e, _)| e.is_some()).collect();
    let compared: usize = results.iter().map(|r| r.2).sum();
    assert!(
        results.len() > TOTAL / 2 && compared > 8 * TOTAL,
        "the corpus stopped being cut into parts: {} cases, {compared} clips",
        results.len()
    );
    assert!(
        wrong.is_empty(),
        "{} of {} cases have a part that draws a clip other than the whole export does; the first:\n{}",
        wrong.len(),
        results.len(),
        wrong
            .iter()
            .take(5)
            .map(|(i, e, _)| format!("case {i}: {}", e.as_deref().unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
