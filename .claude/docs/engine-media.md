# Engine: waveforms, filmstrips, libav, transcription, voiceover

- `peaks.rs` (always compiled, CLI only) is what the timeline draws a clip's **waveform**
  from, and it answers a different question than `waveform` (N peaks for a whole
  file, 8 kHz, kept as it was): a clip shows a *window* at a zoom that never stops
  changing, so a file is decoded **once** into a `WaveformPyramid` — min/max peak
  pairs at 500 / 100 / 25 / 10 buckets per second, per channel (stereo when the
  source has two or more channels, mono otherwise) — and any window is then a slice
  read, `waveform_range(pyramid, start, end, buckets)` (pure + unit-tested; `start` /
  `end` in **source** seconds). It picks the *coarsest* level that still has a source
  bucket per requested one, partitions source buckets among the requested ones by
  where each begins (a peak lands in one column, never smeared across two; when the
  request is finer than the source it reads the bucket under each column's midpoint),
  leaves the part of a window outside the media as 0/0 buckets so the caller's
  time-to-column mapping stays linear, and caps `buckets` at 4096. The decode is
  **48 kHz f32** on purpose: 96 samples is exactly one 2 ms bucket, every coarser
  level is a whole multiple, and 48 kHz is what video audio already is, so no
  resampler runs and the peaks are the real samples' — at 8 kHz the low-pass smears
  transients and rings around a clipped plateau, so a flat-topped 1.0 would not read
  as full scale. Peaks are stored as `i16` (±32767 = full scale, so a clipped sample
  is recognizable; ~18 MB per hour of stereo) and the PCM is folded into buckets as
  it streams off the pipe, so memory is the pyramid, never the file. Like every read
  the timeline draws from it is **ungated** (no `cpu::lease`) but thread-capped and
  niced, at most two decodes at once (a freshly opened project asks for every clip
  at the same instant), concurrent requests for one file share one decode
  (`shared_pyramid`), and the pipe is read on a side thread so a decode silent for
  60 s is killed. The pyramid is cached at `<cache>/kerf/waveforms/<hash>.bin`, keyed
  by path + size + mtime + a format version, written to a temp file and renamed; a
  file that is short, long, the wrong version, inconsistent with its own frame count
  or has a bucket whose min exceeds its max is recomputed, never trusted — and the
  size is checked before anything is allocated. Loaded pyramids sit in a
  byte-bounded in-process LRU (64 MB) so scrolling does not re-read the cache file.
  `Project::waveform_range` / the lock-free `Project::decode_waveform_range` are the
  op (an asset with no audio stream is `InvalidArgument`), exposed as the
  `get_waveform_range` Tauri command and MCP tool; `get_waveform` / `get_energy`
  are unchanged.
- `filmstrip.rs` (always compiled, CLI only) is the video twin of `peaks.rs`: what the
  timeline draws a clip's **thumbnails** from. An asset is sampled once into a
  `Filmstrip` — 96 px-high frames (width from the *displayed* aspect, so rotation is
  autorotate's; a 360 asset is its raw equirect frame), `tile`d into one-row JPEG
  sheets of at most 8192 px, balanced so none is mostly padding (`sheet_layout`) — and
  a window is a few thumbnails picked with `Filmstrip::frame_at(t)` / `locate(k)`.
  **Thumbnail `k` is the frame on screen at source time `k * interval`** (the last
  frame at or before it; 0 is the first frame): that is `fps=…:start_time=0:round=up`
  — `fps`'s default `round=near` picks the frame half an interval *later*, and
  without `start_time=0` a picture that starts after the sound numbers its samples
  from its first frame's slot. The interval is the finest rung of 0.5 / 1 / 2 / 5 /
  10 / 15 / 30 / 60 … s that keeps the strip within 300 thumbnails (`pick_interval`;
  ~3-6 KB a frame); a still is one thumbnail. It decodes the **proxy when
  `ready_proxy` has one, else the original** — resolved *inside* the lock-free
  `Project::decode_filmstrip(&Asset)` and only on a cache miss (the proxy's path can
  take an ffprobe for the HDR check, so nothing under the project lock may touch
  the media: the surface holds the lock for `require_asset` alone) — and a failing
  proxy falls back to the original, hardware decode to software. The cache key is
  the **original's** identity (path + size + mtime + geometry + interval + version),
  never the proxy's: the frames look the same and a proxy keeps frame times, so a
  strip made before the proxy landed is the same entry after. HDR is tone-mapped
  after the downscale from `source_hdr` of the file actually decoded (a proxy
  converts nothing twice). **Keyframes for coarse originals:** from a 5 s interval
  up (assets of ten minutes and more) an *original* is decoded with
  `-skip_frame nokey`, so a thumbnail is the last *keyframe* at or before `k *
  interval` — up to a GOP earlier, never later — instead of the frame itself; 90 s
  of 1080p H.264 took 12.2 s every-frame and 0.8 s by keyframes. Never on the
  all-intra proxy (moot there: every frame is a keyframe, so its strip is exact),
  and never hardware-decoded (one frame in 60 is cheap in software); a keyframe pass
  that comes up short is retried with every frame. **Two ffmpegs, one decode:** the
  first streams *raw* thumbnails over a pipe — so the no-hang rule is per thumbnail
  (silent for `max(60 s, 6 × interval)` and it is killed) and the count is exact
  rather than inferred from tile padding — the second tiles and JPEG-encodes them
  from memory. **Ungated** (no `cpu::lease`) although it reads the whole file: the
  timeline draws from it, and gated it would sit behind the proxy encode and the
  import's analysis while clips stay empty. But it is a video decode beside whatever
  holds the lease, so it is capped at a quarter of the cores (one or two threads,
  `decode_threads`) and niced at **every** budget, 100% included, two at a time;
  concurrent asks for one asset share one decode. **A short decode is not cached:**
  ffmpeg exits 0 on a file that goes bad halfway, so when fewer thumbnails arrive
  than planned the video stream's own duration (ffprobe, only on a shortfall) says
  how many to expect, and more than 2 short is returned and memoized for the session
  but never written to disk (a video simply shorter than its container is complete
  at its own length). Cached at
  `<cache>/kerf/filmstrips/<hash>/` (`sheet-000.jpg …` + `manifest.json`) via a
  `.part` directory and a rename; a manifest that is not the canonical layout for its
  frame count, or a sheet that is missing / resized / not a JPEG of the promised size,
  is rebuilt, never trusted. The thumbnail width is the *coded* aspect — `StreamInfo`
  carries no sample aspect ratio, so an anamorphic source is not corrected (nor is it
  anywhere else: the project frame, fit, preview and export all work in coded pixels).
  `Filmstrip` serializes its geometry *without* the JPEG bytes (`#[serde(skip)]`) — a
  surface adds its own transport. No video stream is `InvalidArgument`; the
  `get_filmstrip` Tauri command is `require_asset` under the lock, then
  `Project::decode_filmstrip` with it released.
- `ffmpeg.rs` is the in-process **libav** backend (the `ffmpeg` feature): it supplies
  `probe` (reading the display matrix and colour tags the same way the ffprobe path does) and, behind the extra `libav-render` feature, an **experimental** in-process
  export pipeline. It can only compile with the dev libraries present (written against
  the ffmpeg-next 8.1 API). The default export path is the CLI one even in full builds.

**Transcription works in every build** (`engine/whisper.rs`, always compiled). Two
backends, picked by `analysis::default_transcriber`: the `whisper` feature's
in-process `whisper-rs` when it is compiled in, otherwise **FFmpeg 8.0's native
`whisper` audio filter** driven through the binary — `filter_available()` probes
`ffmpeg -h filter=whisper` once per process, `transcribe` runs
`aresample=16000,aformat=…,whisper=model=…:destination=…:format=srt` and parses the
SRT back (its `format=json` writes unescaped text, so SRT is the safe wire format).
`queue=30` overrides the filter's 3 s default, which would otherwise transcribe
three-second windows with no context. The model path is **never** put in the filter
graph (`:` and `\` are graph syntax, and a Windows path is both): ffmpeg runs with its
working directory set to the model's folder and both `model=` and `destination=` are
bare file names. Either backend gets its ggml model from `ensure_model`, which
**downloads it on first use** into `<cache>/kerf/models/ggml-<name>.bin` (streamed with
progress, `.part` + atomic rename, resumed via a range request, magic-byte checked so an
error page is never mistaken for a model). Which model: `set_speech_model` (the GUI
picker, persisted in project meta under `speech_model`) → `KERF_WHISPER_MODEL` (still
accepts a *path*, for existing setups) → `base`. `KERF_WHISPER_LANGUAGE` sets the
language hint, `KERF_WHISPER_MODEL_URL` an offline model mirror. `transcription_status()`
reports which backend is live, the model, and whether it still has to be fetched — the
transcript tab and an agent both read it to explain an empty transcript.
`analyze_asset_steps` streams a per-step `AnalysisProgress`
(`waiting`/`silence`/`scenes`/`loudness`/`rhythm`/`download_model`/`transcribe`/`done`), and
transcription runs **last** so the cheap results are cached — each finished step is handed to the
caller's `on_step` and merged at once (`AssetAnalysis::merge`), so markers and chips land before
minutes of inference. **A pass is abandonable** (a `CancelFn` alongside the `ProgressFn`): the check
lands between steps, **while a step is still queued for the machine's slot** (`cpu::lease_waiting`
polls it four times a second, and the check is repeated the instant the slot is taken, so a Stop
that arrives while queued never starts a whole-file pass) *and* inside transcription — the ffmpeg
`whisper` run polls it about once a second off `-stats_period 1` and kills the child, and the model
download polls it per chunk, keeping the `.part` file so the next attempt resumes rather than
re-fetching 148 MB. A cancelled or failed *step* caches nothing (a half-analyzed kind would read as
analyzed, a missing transcript as "no speech"); the steps that finished before it stay cached and
show done. Analysis runs in the **background lane** of the heavy-job queue (`cpu::in_background`),
behind exports and preview proxies; transcription takes its slot after its model download, which is
not heavy work. Kinds that read sound are not run on a file with none (`resolve_steps`).

**Voiceover works in every build too** (`engine/tts.rs`, always compiled, no cargo
feature): Kokoro-82M text-to-speech, **in-process on ONNX Runtime**. Nothing of it
ships — `ort` is built `load-dynamic`, and the runtime library (Microsoft's release
archive for the platform, **pinned by SHA-256** in `runtime_spec`; Intel Macs stay
on 1.20, the last build Microsoft made for them; `ORT_DYLIB_PATH` supplies one
instead), the model (`onnx-community/Kokoro-82M-v1.0-ONNX`'s `model_quantized.onnx`,
~88 MB; `KERF_KOKORO_MODEL_URL` a mirror) and each voice pack (~0.5 MB) are fetched
on first use through **`engine/download.rs`** — the resumable, cancellable `.part` +
verify + rename fetch lifted out of whisper, which both now share. Static linking
was ruled out: pyke's prebuilt runtime needs glibc 2.38 and the Linux bundles build
on Ubuntu 22.04. Phonemes come from **`misaki-rs` without its espeak-ng fallback**
(espeak-ng is GPL-3), so unknown words are spelled out and only the English (`a*` /
`b*`) voices are offered; misaki-rs writes espeak-flavoured IPA (ZWJ-joined
diphthongs, length marks), which `to_kokoro_phonemes` (pure + unit-tested) maps onto
Kokoro's own single-symbol set. **Each sentence is synthesized on its own**
(`split_sentences`, abbreviation- and decimal-aware; blank line = paragraph pause),
which keeps every call inside the 510-token window (`chunk_tokens` splits a longer
one at a clause) and makes the timings exact — sample counts, not a speech model's
estimate. So a generated asset carries **`Asset.voiceover`** (`Voiceover`: script,
voice, speed, per-sentence `segments`; a `voiceover` JSON column migrated like
`source_paths`), `place_voiceover` writes those segments as its transcript, and
`generate_captions` subtitles it with no new caption code; `analyze_asset_steps`
reads a voiceover's transcript from its script (`VoiceoverScript`) instead of
running whisper over audio whose words are known. The WAV lands in the **data**
dir (`<data>/kerf/voiceovers/voiceover-<hash>.wav` — the only copy, unlike a proxy)
with a `.json` timings sidecar, named by a hash of model/voice/speed/text so the
same script is a cache hit and the same asset. `Project::synthesize_voiceover`
(static, lock released, `cpu::lease` held, ORT intra-op threads = the budget, no
spinning) + `place_voiceover` (under the lock: the asset, its transcript, and one
`Add voiceover` edit onto the `VO` audio track, created on first use) is the
lock-free split again.

Two more optional features: `libav-render` (above) and `whisper` (in-process
`whisper-rs`; needs cmake, a C++ compiler and **libclang** at build time). Both are
off by default, so `--no-default-features` CI exercises neither — but **release
bundles are built `--features whisper`** (`release.yml`), because the bundled Windows
FFmpeg is a `--disable-whisper` build and a release with no in-process backend would
have no transcription at all there. `.github/actions/whisper-toolchain` installs and
*verifies* that toolchain (a missing libclang is not an error to whisper-rs-sys — it
silently falls back to its bundled Linux-generated bindings), and CI's `whisper` job
compiles the feature on every release runner (arm64 Linux included), plus the x86_64 macOS cross-compile the
release does, so it can't break for the first time during a release. On macOS it
also sets `MACOSX_DEPLOYMENT_TARGET` to `bundle.macOS.minimumSystemVersion`
(**10.15**, raised from Tauri's 10.13 default): ggml reaches for
`std::filesystem`, which libc++ marks unavailable below 10.15, so the floor is
what the feature costs — and a job compiling against the runner's own SDK would
never see it.

- **With FFmpeg dev libs** (full build): `cargo build` / `cargo run -p kerf-app`.
- **Without them** (CI, UI work): pass `--no-default-features`; everything but the
  in-process libav probe still works via the binaries.
