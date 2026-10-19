# Engine: waveforms, filmstrips, libav, transcription, voiceover, composing, band balance

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
  (`shared_pyramid_cancellable`: a request waiting behind another's decode, or for one of the two slots, polls its
  own cancel and gives up with `Cancelled`; a stopped decode caches nothing), and the pipe is read on a side thread (`pump_pcm_cancellable`, shared with
  `cli::waveform`, the in-memory PCM decodes and `pcm::decode_audio_cancellable`) so a decode silent
  for 60 s is killed and a caller's cancel is polled every 100 ms. The pyramid is cached at `<cache>/kerf/waveforms/<hash>.bin`, keyed
  by path + size + mtime + a format version, written to a temp file and renamed; a
  file that is short, long, the wrong version, inconsistent with its own frame count
  or has a bucket whose min exceeds its max is recomputed, never trusted — and the
  size is checked before anything is allocated. Loaded pyramids sit in a
  byte-bounded in-process LRU (64 MB) so scrolling does not re-read the cache file.
  `Project::waveform_range` / the lock-free `Project::decode_waveform_range` are the
  op (an asset with no audio stream is `InvalidArgument`), exposed as the
  `get_waveform_range` Tauri command and MCP tool; the MCP `get_waveform` / `get_energy` stay (the GUI draws from the
  pyramid). The agent's reads have cancellable twins — `decode_waveform_cancellable`, `decode_waveform_range_cancellable`,
  `decode_energy_cancellable`, `decode_contact_sheet_cancellable`, `composite_timeline_frame_cancellable` /
  `_region_cancellable` — which the MCP tools hand the request's cancellation token, so a dropped request stops its
  ffmpeg (`agent_reads_stop_when_their_request_is_dropped`, `#[ignore]`d, drives them).
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
- `pcm.rs` (always compiled, CLI only): `AudioBuffer {sample_rate, channels, samples}`
  (interleaved f32) and `decode_audio(path, rate, channels)` — the ffmpeg binary
  resamples and remixes (`-ar` / `-ac`), so there is no Rust decoder or resampler. A
  whole-file decode, so it takes `cpu::lease` (`decode_audio_cancellable(.., cancel)` waits for it
  with `lease_waiting`); the pipe goes through `peaks.rs`'s `pump_pcm_cancellable`, so a decode silent
  for 60 s is killed and a cancel ends it (`Error::Cancelled`). `write_wav` / `wav_bytes` write
  32-bit float WAV for test fixtures and debug renders (no `hound`). `decode_audio_window(path,
  start, secs, .., &lease, cancel)` is the same with an input-side `-ss` / `-t`, on a slot the caller holds.
- `audio_sync.rs` (pure DSP, ported from FilmCraft's `sync.rs` onto `realfft`): `find_offset(a, b,
  rate)` → `SyncResult {lag, lag_fine, confidence, distinct, inverted}` with `b[n] ≈ g·a[n + lag]`.
  GCC-PHAT-β (β 0.75) on signals decimated to ≤ 8 kHz (further past 4 M samples), then DC-removed
  and normalised (one copy a side, dropped with the correlation before the refine), then refined at
  the full rate on the loudest common window (picked from `w/4`-long block energies, not prefix sums
  of the whole input) with parabolic interpolation; `reliable()` is confidence ≥ 8 and distinct (peak over the best one > 50 ms away)
  ≥ 1.5. Unit-tested on synthetic speech: exact pure delays both ways, noisy coloured mics within a
  sample, a 48 kHz search decimated by 6 and by 11, inverted polarity, unrelated signals unreliable.
  The project half is `project/audio_sync.rs`: `audio_sync_inputs` (both clips forward at speed 1,
  with sound, not linked to each other; the move checked up front so it fails before any decode —
  not on the reference's track, where in sync they would overlap, and the clip and its partners on
  unlocked tracks; window `source_in..source_out`, ≤ 20 min) → static
  `measure_audio_sync(plan, on_wait, cancel)` (one `cpu::lease_waiting` held for both mono 16 kHz
  window decodes, `decode_audio_window(.., &lease, cancel)`; cancel polled while queued, between steps and
  inside each decode)
  → `apply_audio_sync(m, force)`, which re-reads both clips (either may have moved
  or been trimmed meanwhile: `synced_start`), reports an unreliable match without moving unless
  `force`, refuses a start before 0 and moves through `move_clip` (linked partners follow; no ripple).
  `AudioSync.distinct` is `None` (JSON `null`) when nothing else correlated, never `inf`.
  An `#[ignore]`d test syncs two ffmpeg-cut copies of one pink-noise take to within a sample.
- `music.rs` (always compiled, pure DSP + a cache): `analyze_music(mono, rate)` →
  `MusicAnalysis {grid: BeatGrid, duration, bar_chroma, phrases}`, computed inside
  `analyze_rhythm` from the same 22.05 kHz decode and cached at
  `<cache>/kerf/music/<hash>.json` (file identity like the waveform cache; a "no pulse"
  result is cached too). Onsets are log-magnitude spectral flux (`realfft`, Hann 1024 /
  hop 128 at 22.05 kHz — the 46 ms of a 2048 window at 44.1 kHz). The **grid is fitted,
  not tracked**: autocorrelation (weighted towards 120 BPM against octave errors,
  parabola-refined) gives a first period, then a brute-force scan (period ±1 % at
  0.2 ms, phase at 3 ms, then 10× finer around the winner) maximizes the mean flux under
  the grid; a grid under 1.5× the mean flux is no pulse (`None`). **The flux of a sharp
  attack peaks ~7–10 ms early** (the window gains it fastest while it enters at the edge),
  so `ONSET_LEAD_S` (8.5 ms) is added back — without it a click track misses the ±5 ms
  phase test. `phase_s` may sit up to 20 ms *before* 0 so a beat on the first sample is
  not wrapped a period later. The downbeat is the beat (mod 4) with the most kick-band
  (< 150 Hz) flux. Chroma (4096 / hop 1024, 55 Hz–4.2 kHz, A4 = 440) is summed per
  whole bar and L2-normalized (zeros when silent); `phrase_matches` lists 8- and 4-bar
  phrases on 4-bar boundaries whose bars all match above `SPLICE_SIMILARITY` (0.95) —
  the legal splice points. Loudness stays ffmpeg's (`measure_loudness`), not `ebur128`.
- `stems.rs` (always compiled): **stem separation** with Demucs `htdemucs` (MIT) on the
  ONNX Runtime the voiceover already loads (`tts::ensure_runtime` / `init_runtime` are
  shared). **Only the network is ONNX** — `scripts/demucs/export.py` exports it (inputs
  `mix` [1,2,343980] + `mag` [1,4,2048,336], outputs `spec_out` + `wave_out`; re-running
  it reproduces the pinned SHA-256) because the model's STFT does not export. `spec` /
  `ispec` port `HTDemucs._spec` / `_ispec` (two-stage reflect pad, periodic Hann,
  `normalized`, Nyquist bin and 2 edge frames dropped/restored, DC imag zeroed before the
  c2r) and are pinned against values printed by PyTorch; `separate_buffer` ports
  `apply_model` (7.8 s segments, stride ¾, each padded with its *neighbours'* audio and
  centre-trimmed, triangular blend) with `demucs.separate`'s mean/std normalization and
  `shifts=0` (deterministic). An ignored parity test (`KERF_DEMUCS_PARITY_DIR`, fixture
  from `scripts/demucs/parity.py`) matches PyTorch per stem under 1e-3 RMS across several
  segments, the stems summing back to the mix within 0.02 (Demucs is not constrained to;
  PyTorch lands the same on that synthetic mix). `separate(path)` decodes at 44.1 kHz
  stereo, runs under `cpu::lease` with ORT threads = the budget, and caches 24-bit FLAC
  stems at `<cache>/kerf/stems/<hash>/` (float WAV would be ~200 MB per stem for ten
  minutes). The model (~170 MB) downloads on first use from a release asset
  (`KERF_DEMUCS_MODEL_URL` overrides), verified as ONNX and by SHA-256.
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
error page is never mistaken for a model; the body is read on a thread, so **no byte for 60 s is a
stall** — an error that keeps the `.part` for the resume — and Cancel is polled every 200 ms
whatever the socket is doing; `download_speech_model_cancellable(name, progress, cancel)` is the
hooked entry point, `download_speech_model` the no-hooks wrapper). Which model: `set_speech_model` (the GUI
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
`whisper` run goes through `run_ffmpeg_streamed_in` (`-progress` every half second, cancel polled
at that rate, killed after 10 minutes with no line) and kills the child, and the model download
polls it every 200 ms, keeping the `.part` file so the next attempt resumes rather than
re-fetching 148 MB. **Every step's provider takes the pass's `CancelFn`** (`SilenceDetector`,
`SceneDetector`, `LoudnessAnalyzer`, `RhythmAnalyzer`, as `Transcriber` always did), so Stop kills
the whole-file ffmpeg read in flight (`engine-cli.md`, "Every ffmpeg / ffprobe child is
supervised") rather than waiting out the step, and a step whose decode fails is *failed*, not
"found nothing". A cancelled or failed *step* caches nothing (a half-analyzed kind would read as
analyzed, a missing transcript as "no speech"); the steps that finished before it stay cached and
show done. Analysis runs in the **background lane** of the heavy-job queue (`cpu::in_background`),
behind exports and preview proxies; transcription takes its slot after its model download, which is
not heavy work. Kinds that read sound are not run on a file with none (`resolve_steps`).

**Words and speakers** (`transcript.rs` pure; `engine/vad.rs`, `engine/mel.rs`, `engine/diarize.rs`,
adapted from FilmCraft). A `TranscriptSegment` carries `words: Vec<Word {text, start, end,
confidence?, estimated, speaker?}>` and a `speaker` (`S1`, `S2`, … by first appearance), both
`#[serde(default)]` and skipped when empty, so old projects load and re-save byte-identical. The
in-process backend sets `token_timestamps` and builds words from the text tokens (`id < eot`; a token
starting with a space starts a word; bytes joined before UTF-8 decoding, since a token can end
mid-character; clamped to the segment; confidence = mean token `p`) — `words_from_tokens`. **The ffmpeg
`whisper` filter reports segments only** (no token timings in its SRT/JSON), so its words are spread
over the line by character share and marked `estimated` (`estimate_words`). Then every transcription
(and a voiceover's script) is **refined** from one 16 kHz mono `decode_audio`: `vad::Levels` tightens
each word inward past 10 ms frames below a threshold 30 % of the way from the noise floor (p10 of
frames above −100 dBFS) to the speech level (p95), ≥ 6 dB over the floor, a word keeping ≥ 60 ms;
`diarize` chunks the words at ≥ 250 ms pauses / 4 s (chunks < 0.7 s join a neighbour), takes the
mean + std of MFCC 1–19 (40 Slaney mel bands, Whisper's 25 ms / 10 ms framing, `realfft`, computed
frame by frame so an hour never holds its spectrogram) over each chunk's voiced frames, scales them by
the file's spread, and clusters with average linkage (Lance–Williams updates + cached nearest
neighbours; a row whose neighbour merged rescans only when the merged distance grew, so ties cost
nothing: about O(n²) time on real input, O(n³) worst case, n² memory) until the closest pair is > 0.4
apart and ≤ 6 remain; clusters under 5 % of the voiced frames fold into their neighbour. n is capped at
`MAX_CHUNKS` (3000, a 36 MB matrix) by merging neighbouring chunks, and chunks whose vectors all coincide
are one speaker without clustering. A segment's speaker is the one with most of its word-time
(`speaker_ranges` goes by each word's own label). The refinement runs in the background lane, waits
for the heavy-job slot and holds it (`refine_transcript_media`, static); the in-process backend refines
from the samples it already decoded (`Transcriber::refines`). A refinement that does not finish — a
failed decode, a cancel, a stop while queued — keeps the transcript with estimated words and no
speakers: a finished transcription is never thrown away.

**Voiceover works in every build too** (`engine/tts.rs`, always compiled, no cargo
feature): Kokoro-82M text-to-speech, **in-process on ONNX Runtime**. Nothing of it
ships — `ort` is built `load-dynamic`, and the runtime library (Microsoft's release
archive for the platform, **pinned by SHA-256** in `runtime_spec`; Intel Macs stay
on 1.20, the last build Microsoft made for them; `ORT_DYLIB_PATH` supplies one
instead), the model (`onnx-community/Kokoro-82M-v1.0-ONNX`'s `model_quantized.onnx`,
~88 MB; `KERF_KOKORO_MODEL_URL` a mirror) and each voice pack (~0.5 MB) are fetched
on first use through **`engine/download.rs`** — the resumable, cancellable `.part` +
verify + rename fetch lifted out of whisper, which both now share. The `.part` is
named by pid, so threads of one process fetching the same destination take turns
(`download::take_turn`, a per-path claim; other processes are not excluded and rely on
the pid-named `.part` and the final rename). A waiter polls cancel, reports one
progress event (its `.part` size, no total) and, once in, finds the file in place. The
runtime install (`tts::install_runtime`, shared by voiceover and stems) holds a turn on
the library path for the fetch **and** the extract, which also goes through a pid-named
`.part`. A resume answered 416 drops the partial and restarts once — CI once had two
tests interleaving one SoundFont's `.part` and failing on the 416. Static linking
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

**Composing music works in every build too** (`engine/compose.rs`, always compiled, no cargo
feature): a `MusicScore` (`core-model.md`) becomes notes (`expand`) and the notes become sound through
**`rustysynth`** (pure Rust, MIT, pinned `=1.3.6`; 1.3.7 was too new to take) playing **GeneralUser GS
v2.0.3**, whose licence lets anyone use the music it makes. The SoundFont (~32 MB) downloads on the first
composition through `engine/download.rs` (resumable, cancellable, `KERF_SOUNDFONT_URL` a mirror) into
`<cache>/kerf/soundfonts/`, pinned at a commit of its repository and verified as RIFF `sfbk` **and by
SHA-256** before the rename. **One synthesizer per role** (drums, top of the kit, bass, pad, keys, lead,
effects — the top is its own role because the GM kits record hats and cymbals ~20 dB under the kick),
all sharing the one loaded `Arc<SoundFont>`, mixed **a tenth of a second at a time** so memory is the
finished stereo mix and nothing else: the role gains from the style's preset, a **sidechain** (a dip
`attack_s` *before* each kick, full at it, a squared recovery; the bass, pad and keys take shares of the
depth — `sidechain_curve`, pure, a block equals the whole), the picture's **dropouts** (`gate_curve`:
everything but the effects silent, the ramp up ending *on* the window's end so the downbeat that follows is
whole), a per-section **make-up gain** (`Arrangement.makeup`: a section with fewer layers — a pad-only intro,
a breakdown without kick and bass — is lifted half the way, ≤ 6 dB, to the loudness of the fullest, from
the style's role shares; without it the intro measured 15–20 LU under the drop), then `master`: raise until
the loudest 0.5 % of samples sit at 0.7 (never more than 6 dB over full scale), soft-clip past a 0.8 knee,
scale to **exactly −1 dBFS** — loudness stays the master bus's and the export preset's job. Output is
48 kHz stereo **16-bit WAV with a seeded triangular dither** in the data dir
(`<data>/kerf/music/music-<hash>.wav`, the only copy; hash of the renderer version, the SoundFont's
SHA-256 and the canonical score JSON, so the same score is a cache hit and the same asset;
`RENDERER_VERSION` is bumped when what a score sounds like changes). `render(score, progress, cancel)`
reports `download_soundfont`, `waiting` (`cpu::lease_waiting`, **foreground**: someone waits on it) and
`render` (a fraction), checks cancel every block, writes through `.part` + rename and never holds the
project lock (`Project::synthesize_music` is the static half, `place_music` lands it). `status()` →
`MusicStatus` (SoundFont ready / bytes to fetch, renders in flight, the styles). rustysynth applies
`note_off` to **every** voice of a key on a channel, so `expand` makes sure one key never overlaps itself;
drum one-shots ignore their note-off, but the orchestral kit's timpani do not (they get a held note). The
effects are GM sounds: the riser is the **reverse cymbal** (program 119), a sample that peaks a fixed time
after it starts at one pitch and `2^(-semitones/24)` as long a note higher (measured 1.9 s at key 48), so a
riser of length L is pitched to peak exactly at its end (`REVERSE_CYMBAL_PEAK_S`); an impact is a crash, a
kick, a timpani on the tonic and a low synth boom. `#[ignore]`d tests download the SoundFont and render
every style (duration, not silent, peak −1 dBFS, cache hit), print the **calibration report**
(`calibration_report`: each role solo as a K-weighted share of the song against the share its preset aims
at — the preset `gain`s were converged on it in two iterations, and the test **fails** when a role drifts
more than 1.5 points + 25 % from its `shares`, printing the gain to move — plus the band shares per section;
set `KERF_COMPOSE_DUMP=<dir>` to keep the WAVs) and check that the beat tracker, which knows nothing of the
score, finds the score's tempo and sits on its eighth grid within 20 ms (it may lock to the off-beat hats of a
four-on-the-floor beat, which is why the music's analysis is written from the score).

**Band balance** (`engine/bands.rs`, always compiled, pure DSP + a decode): `BandMeter` / `band_balance`
measure **windows** — RMS, the K-weighted loudness of BS.1770 (two biquads; ungated, so a window of a
song reads like the meter's LUFS; a 1 kHz full-scale sine is −3.0), and six bands (sub 20–60, bass 60–250,
low-mid 250–500, mid 500–2 k, high-mid 2–6 k, air 6–20 kHz) as an energy in dB and a share of the in-band
energy — from a Hann STFT (8192 / hop 4096 on the mono sum) scaled by Parseval so a full-scale sine reads
−3 dBFS in its band; a frame belongs to the window its centre is in; the meter takes interleaved samples as
they stream off the decoder, so memory is a frame. `stream_file` (the asset or a clip's source range,
stereo 44.1 kHz) and `cli::stream_mix_pcm` (the **export's own audio graph** — faders, ducking, master
bus, no `loudnorm` — to f32 PCM) are **ungated** like every read an agent looks at, but `cap_args` /
`limit_cmd` to a share of the cores, niced, a stall killed and cancel polled. A mono file is upmixed by
ffmpeg at −3 dB per channel (power kept), so its RMS reads 3 dB under a mono analysis and its loudness reads
the same.

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

`resvg` (pure Rust, `text` + `system-fonts` + `raster-images`: PNG, JPEG, GIF, WebP for an `<image>` with a `data:`
URL) draws animated SVG titles: kerf-core's `svg/` evaluates the SMIL to a static document per moment and
resvg rasterises it, so no build needs a system library for it (`core-model.md` has the model, `engine-cli.md`
the frame paths). `imagesize` and `data-url` are direct dependencies only because resvg already carries them: an embedded
image's size is read from its header and held to 16 Mpx before any decoder (which allocates what the header claims) sees it.

- **With FFmpeg dev libs** (full build): `cargo build` / `cargo run -p kerf-app`.
- **Without them** (CI, UI work): pass `--no-default-features`; everything but the
  in-process libav probe still works via the binaries.
