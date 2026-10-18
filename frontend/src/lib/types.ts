// Mirrors the kerf-core domain model serialized over Tauri / MCP.

export type StreamKind = 'video' | 'audio' | 'subtitle' | 'data';

export interface StreamInfo {
	index: number;
	kind: StreamKind;
	codec: string;
	width?: number;
	height?: number;
	fps?: number;
	sample_rate?: number;
	channels?: number;
	/** True for a single-frame still image (looped, not seeked, on export). */
	image?: boolean;
	/** Set when the stream is 360 footage, detected at probe time. */
	projection?: Projection | null;
	/**
	 * Degrees counter-clockwise the file's coded frame is turned for display (a
	 * portrait phone clip is 90 or -90). `width`/`height` are already the
	 * displayed size; absent when 0.
	 */
	rotation?: number;
	/** Transfer characteristic as ffprobe names it: `arib-std-b67` (HLG), `smpte2084` (PQ), `bt709`, … */
	color_transfer?: string | null;
	/** Colour primaries as ffprobe names them: `bt2020`, `bt709`, … */
	color_primaries?: string | null;
	/** ffprobe's pixel format (`yuv420p`, `yuva420p`, ...); absent on an asset probed before it was recorded. */
	pix_fmt?: string | null;
	/** The YCbCr matrix the stream declares, as ffprobe names it (`bt709`, `smpte170m`, `bt2020nc`, …); absent when it declares none. */
	color_space?: string | null;
}

/**
 * How a video stream maps the world onto its frame. `flat` is ordinary video;
 * the rest are 360 sources (a raw Insta360 `.insv` is `dual_fisheye`, a stitched
 * Studio export is `equirect`).
 */
export type Projection = 'equirect' | 'dual_fisheye' | 'fisheye' | 'flat';

export interface Asset {
	id: string;
	path: string;
	name: string;
	duration: number;
	streams: StreamInfo[];
	imported_at: string;
	/** Original capture files a derived asset was built from (an Insta360 lens pair). */
	source_paths?: string[];
	/** Set on a synthesized voiceover: the script and settings it was made from. */
	voiceover?: Voiceover;
}

/** What a generated voiceover was made from, kept on its asset so it can be
 *  regenerated. `segments` are the script's sentences in asset time. */
export interface Voiceover {
	text: string;
	voice: string;
	speed: number;
	segments: TranscriptSegment[];
}

export interface TimeRange {
	start: number;
	end: number;
}

export interface TranscriptSegment {
	start: number;
	end: number;
	text: string;
}

export interface Loudness {
	integrated_lufs: number;
	loudness_range: number;
	true_peak_dbtp: number;
	threshold_lufs: number;
}

export interface Tempo {
	bpm: number;
	beats: number[];
	confidence: number;
	/** Bar starts in source seconds; empty when no bar grid could be fitted (the beats are then
	 *  a plain estimate). With a grid, `beats` are its beats. */
	downbeats: number[];
}

export interface BeatGrid {
	period_s: number;
	phase_s: number;
	downbeat_offset: number;
	beats_per_bar: number;
}

export interface PhraseMatch {
	a: number;
	b: number;
	bars: number;
}

export interface MusicAnalysis {
	grid: BeatGrid;
	duration: number;
	bar_chroma: number[][];
	phrases: PhraseMatch[];
}

/** One piece of the source, placed at `output_start` seconds into the fitted music
 *  (`kerf_core::MusicSegment`). */
export interface MusicSegment {
	source_start: number;
	source_end: number;
	output_start: number;
}

/** A planned "fit music to length" (`kerf_core::MusicFit`): the segments in output order, how
 *  long they last and how far that is from the target. `remainder = target - duration`:
 *  positive is short of it, negative runs over and wants a fade-out. */
export interface MusicFit {
	segments: MusicSegment[];
	target: number;
	duration: number;
	remainder: number;
	/** Whole bars played between the intro and the ending. */
	bars: number;
	splices: number;
}

/** What fitting did (`kerf_core::MusicFitReport`): the clips it placed (in order), the plan they
 *  came from, whether an overrun was cut and faded, and how long the music now lasts. */
export interface MusicFitReport {
	clips: string[];
	fit: MusicFit;
	faded: boolean;
	duration: number;
}

export type AudioClass = 'speech' | 'music' | 'mixed' | 'unknown';

export interface AudioClassification {
	class: AudioClass;
	confidence: number;
}

export interface AssetAnalysis {
	asset_id: string;
	silence_segments: TimeRange[];
	scene_changes: number[];
	transcript: TranscriptSegment[];
	loudness: Loudness | null;
	onsets: number[];
	tempo: Tempo | null;
	audio_class: AudioClassification | null;
	/** The kinds whose step ran to completion — what tells "ran and found nothing" from
	 *  "never ran". Absent in an analysis cached before it was recorded (a kind then
	 *  counts as done when it has data). */
	ran?: AnalysisKind[];
	music: MusicAnalysis | null;
}

/** One kind of analysis: each is a step of its own, run alone and cached independently. */
export type AnalysisKind = 'silence' | 'scenes' | 'loudness' | 'rhythm' | 'transcript';

/** Where one kind of an asset's analysis stands. `off`: not run and will not by itself
 *  (switched off in Settings, or no speech backend). */
export type AnalysisState = 'done' | 'not_run' | 'running' | 'failed' | 'off';

export interface AnalysisKindStatus {
	kind: AnalysisKind;
	state: AnalysisState;
	/** Why it failed or is off. */
	reason?: string | null;
}

/** An asset's analysis kind by kind (`analysis_statuses`, the `analysis-status` event):
 *  always one entry per kind, in pass order. */
export interface AnalysisStatus {
	asset_id: string;
	kinds: AnalysisKindStatus[];
}

/** Which analyses run by themselves on import (Settings › Analysis). */
export interface AutoAnalysis {
	/** Analyze new media on import at all. */
	enabled: boolean;
	silence: boolean;
	scenes: boolean;
	loudness: boolean;
	rhythm: boolean;
	transcript: boolean;
}

/** Where an asset's preview proxy stands. */
export type ProxyPhase = 'not_needed' | 'off' | 'missing' | 'queued' | 'building' | 'ready' | 'failed';

/** An asset's preview proxy (`proxy_statuses`, the `proxy-progress` event). */
export interface ProxyStatus {
	asset_id: string;
	state: ProxyPhase;
	/** 0..1, while building. */
	fraction?: number | null;
	/** Why it is off, not needed or failed. */
	reason?: string | null;
	/** How wide the proxy is (or would be), pixels across. */
	width?: number | null;
	/** The proxy file's size on disk. */
	bytes?: number | null;
	elapsed_secs?: number | null;
	eta_secs?: number | null;
}

/** How wide preview proxies are, pixels across; 0 = none are made. 1280 is the default. */
export type ProxySize = 0 | 720 | 1080 | 1280;

/** Which file the preview decodes: the proxy once it is ready, always the original, or
 *  only the proxy (waiting for it). Export always reads the originals. */
export type PreviewSource = 'auto' | 'original' | 'proxy_only';

export interface Transform {
	scale: number;
	pos_x: number;
	pos_y: number;
	rotation: number;
	opacity: number;
	crop_left: number;
	crop_right: number;
	crop_top: number;
	crop_bottom: number;
}

export interface Color {
	brightness: number;
	contrast: number;
	saturation: number;
	gamma: number;
	/** Warm/cool shift −1..1 (0 = neutral; positive warms, negative cools). */
	temperature: number;
}

export type MaskShape = 'rect' | 'ellipse';

/** A shape cut out of a clip: inside it the clip is kept, outside it goes
 *  transparent so a lower track shows through. Everything is a fraction of the
 *  rendered frame. Mirrors `Mask` in crates/kerf-core/src/model.rs. */
export interface Mask {
	shape: MaskShape;
	/** Centre of the shape, 0..1 across and down the frame. */
	x: number;
	y: number;
	/** Full width / height of the shape as a fraction of the frame. */
	width: number;
	height: number;
	/** Edge softness as a fraction of the shape's half-size; 0 is a hard cut. */
	feather: number;
	/** Keep what is outside the shape instead of inside it. */
	inverted?: boolean;
}

/** Mirrors `Mask::default()` in crates/kerf-core/src/model.rs. */
export const DEFAULT_MASK: Mask = {
	shape: 'rect',
	x: 0.5,
	y: 0.5,
	width: 0.5,
	height: 0.5,
	feather: 0.15,
	inverted: false
};

export type TransitionKind =
	| 'crossfade'
	| 'dip_to_black'
	| 'dip_to_white'
	| 'slide_left'
	| 'slide_right'
	| 'slide_up'
	| 'slide_down'
	| 'push_left'
	| 'push_right'
	| 'push_up'
	| 'push_down';

export interface Transition {
	kind: TransitionKind;
	duration: number;
}

// Discriminated unions mirroring kerf_core::{VideoEffect, AudioEffect}
// (serde `#[serde(tag = "type")]`).
export type VideoEffect =
	| { type: 'blur'; sigma: number }
	| { type: 'sharpen'; amount: number }
	| { type: 'grayscale' }
	| { type: 'invert' }
	| { type: 'vignette' }
	| { type: 'chroma_key'; color: string; similarity: number; blend: number };

export type AudioEffect =
	| { type: 'highpass'; hz: number }
	| { type: 'lowpass'; hz: number }
	| { type: 'equalizer'; hz: number; width: number; gain_db: number }
	| {
			type: 'compressor';
			threshold_db: number;
			ratio: number;
			attack_ms: number;
			release_ms: number;
			makeup_db: number;
	  }
	| { type: 'gate'; threshold_db: number };

/** How a keyframe travels to the next one (kerf-core's `Easing`); omitted is linear. */
export type Easing =
	| 'linear'
	| 'hold'
	| 'ease_in'
	| 'ease_out'
	| 'ease_in_out'
	| { bezier: { x1: number; y1: number; x2: number; y2: number } };

/** One keyframe of a clip's animated transform. */
export interface Keyframe {
	time: number;
	scale: number;
	pos_x: number;
	pos_y: number;
	rotation: number;
	opacity: number;
	/** The shape of the segment leaving this key; absent is linear. */
	easing?: Easing;
}

/** One animatable number of a clip (kerf-core's `Property`): a transform number, a colour
 *  number, or the clip's own gain (linear). */
export type Property =
	| 'scale'
	| 'pos_x'
	| 'pos_y'
	| 'rotation'
	| 'opacity'
	| 'brightness'
	| 'contrast'
	| 'saturation'
	| 'gamma'
	| 'temperature'
	| 'volume';

/** One key of a property's own animation. `time` is seconds from the clip's start. */
export interface PropertyKey {
	time: number;
	value: number;
	/** The shape of the segment leaving this key; absent is linear. */
	easing?: Easing;
}

/** The keys of one property. A property with a track is driven by it (even an empty one:
 *  static, whatever `keyframes` says); a transform number without one reads `keyframes`. */
export interface PropertyTrack {
	prop: Property;
	keys: PropertyKey[];
}

/** One keyframe of a 360 clip's animated virtual camera. */
export interface ReframeKeyframe {
	time: number;
	yaw: number;
	pitch: number;
	roll: number;
	fov: number;
}

/**
 * Per-clip reprojection of 360 footage: aim a virtual camera into the sphere and
 * render what it sees. `output` is `flat` for a normal deliverable, or `equirect`
 * to stitch a dual-fisheye source without choosing a direction.
 */
export interface Reframe {
	input: Projection;
	output: Projection;
	/** Field of view of each physical lens (dual-fisheye sources only). */
	lens_fov: number;
	yaw: number;
	pitch: number;
	roll: number;
	/** Diagonal field of view of the virtual camera, in degrees. */
	fov: number;
	keyframes?: ReframeKeyframe[];
}

/** One keyframe of an animated text overlay (position + opacity). */
export interface TextKeyframe {
	time: number;
	pos_x: number;
	pos_y: number;
	opacity: number;
}

/** A timed text element (title / lower-third / caption) drawn over the cut. */
export interface TextOverlay {
	id: string;
	text: string;
	start: number;
	end: number;
	pos_x: number;
	pos_y: number;
	size: number;
	color: string;
	bg?: string | null;
	font?: string | null;
	bold: boolean;
	keyframes?: TextKeyframe[];
	/** Written by `generate_captions` or `import_captions` rather than by hand.
	 *  Regenerating or re-importing replaces these and leaves typed titles alone. */
	generated?: boolean;
}

/** The ready-made animated titles (`kerf_core::SvgPresetKind`). */
export type SvgPresetKind = 'fade_up' | 'typewriter' | 'lower_third' | 'word_pop';

/** What a preset is filled with. */
export interface SvgPresetParams {
	text: string;
	subtitle?: string | null;
	/** Any SVG colour. */
	color: string;
	/** The second colour a preset draws in (bar, last word, subtitle). */
	accent: string;
	/** A font family; unset / `null` is the font Kerf ships. */
	font?: string | null;
}

/** Params as a caller may write them: the backend fills what is left out. */
export type SvgPresetParamsInput = Pick<SvgPresetParams, 'text'> & Partial<Omit<SvgPresetParams, 'text'>>;

/** How an animated title was made when it came from a preset. */
export interface SvgPreset {
	kind: SvgPresetKind;
	params: SvgPresetParams;
}

/** An animated SVG title: SMIL-animated artwork drawn over the cut, above the clips and below the
 *  text overlays. `svg` is the **sanitised** source (stored = rendered); the page never inlines it,
 *  only shows a placement box and `<img>` data-URL thumbnails. The artwork is fit "contain",
 *  centred, into the rectangle `x, y, width, height` (fractions of the frame, top-left origin). */
export interface SvgOverlay {
	id: string;
	name: string;
	svg: string;
	start: number;
	end: number;
	x: number;
	y: number;
	width: number;
	height: number;
	opacity: number;
	/** Seconds of the document's own clock already elapsed at `start` (a slice cut into it).
	 *  The backend omits it at 0. */
	offset?: number;
	/** Absent on custom artwork. */
	preset?: SvgPreset;
}

/** Where an animated title's artwork comes from; externally tagged, as the backend reads it. */
export type SvgSource = { custom: string } | { preset: { kind: SvgPresetKind; params: SvgPresetParamsInput } };

/** Name and placement of a new animated title; a field left out takes the default (full frame, opaque). */
export interface SvgLayout {
	name?: string;
	x?: number;
	y?: number;
	width?: number;
	height?: number;
	opacity?: number;
}

/** Fields of an animated title to change; one left out stays. `svg` replaces the artwork with
 *  custom source (and so drops the preset); `preset_params` re-fills a preset title and `preset_kind`
 *  switches it to another kind, keeping its id and place in the draw order (a custom title refuses
 *  both); `svg` is exclusive with either. */
export interface SvgOverlayPatch {
	name?: string;
	svg?: string;
	preset_params?: SvgPresetParamsInput;
	preset_kind?: SvgPresetKind;
	start?: number;
	end?: number;
	x?: number;
	y?: number;
	width?: number;
	height?: number;
	opacity?: number;
}

/** One parameter a preset takes. */
export interface SvgPresetParamInfo {
	name: string;
	required: boolean;
	default?: string;
}

/** A preset and what it takes (`list_svg_presets`). */
export interface SvgPresetInfo {
	kind: SvgPresetKind;
	label: string;
	params: SvgPresetParamInfo[];
}

/** What `validate_svg` made of a source: the sanitised document, what could not be honoured, and how
 *  long its animations run (`null` when they never end or there are none). */
export interface SvgCheck {
	svg: string;
	warnings: string[];
	natural_duration: number | null;
}

/** The look a generated caption set takes: a held subtitle line, or one large
 *  word at a time — the style social captions have converged on. */
export type CaptionStyle = 'lines' | 'word_punch';

/** How a transcript is turned into on-screen captions. Everything but the style
 *  is an override: omit a field and it follows the style, so asking for
 *  `word_punch` alone gets the whole look. */
export interface CaptionOptions {
	style?: CaptionStyle;
	max_words?: number;
	max_chars?: number;
	pos_y?: number;
	size?: number;
}

/** A subtitle format Kerf reads: SubRip (`.srt`) or ASS / SSA (`.ass`, `.ssa`). */
export type CaptionFormat = 'srt' | 'ass';

/** Which clock an imported subtitle file's times are on. `timeline`: a subtitle
 *  track made for the finished cut (the default). `source`: the file times one
 *  asset's own footage — a transcript, or subtitles for the uncut recording — and
 *  each cue is projected through that asset's clips like a transcript. */
export type CaptionTimeBase = 'timeline' | 'source';

/** What an import did. Every usable cue lands in exactly one of `placed`,
 *  `dropped_outside`, `dropped_short` or `dropped_overlap`, so
 *  `cues === placed + dropped_outside + dropped_short + dropped_overlap`;
 *  `captions` can exceed `placed` because a long cue is split into several lines. */
export interface CaptionImportSummary {
	format: CaptionFormat;
	/** Usable cues read from the file. */
	cues: number;
	/** Cues that put at least one caption on screen. */
	placed: number;
	/** Caption overlays written. */
	captions: number;
	/** Entries of the file that could not be used. */
	skipped_lines: number;
	/** Cues past the end of the cut (or before its start), or timing footage no
	 *  clip shows. */
	dropped_outside: number;
	/** Cues that do reach the cut but only for a moment too short to read. */
	dropped_short: number;
	/** Cues that lost their slot: captions are one lane, never two at once. */
	dropped_overlap: number;
	/** Earlier generated / imported captions this import replaced. */
	replaced: number;
}

/** The refreshed cut and what the import did. */
export interface CaptionImportResult {
	timeline: Timeline;
	summary: CaptionImportSummary;
}

/** How a subtitle file is laid onto the cut. */
export interface CaptionImportRequest {
	/** Defaults to `timeline`; `source` needs `assetId`, and `assetId` alone implies it. */
	base?: CaptionTimeBase;
	assetId?: string;
	/** The caption look, as `generate_captions` takes it. */
	options?: CaptionOptions;
	/** Seconds added to every cue before it is placed; negative moves them
	 *  earlier. For a file whose clock does not start where the picture does — a
	 *  broadcast SRT that begins at 01:00:00 wants `-3600`. */
	offset?: number;
}

export interface Clip {
	id: string;
	asset_id: string;
	source_in: number;
	source_out: number;
	timeline_start: number;
	volume: number;
	fade_in: number;
	fade_out: number;
	// New per-clip primitives. Optional so browser-sample / older clip literals
	// still type-check; the backend always serializes them.
	speed?: number;
	transform?: Transform;
	color?: Color;
	transition_in?: Transition | null;
	/** A shape cut out of this clip; absent is the whole frame. */
	mask?: Mask | null;
	effects?: VideoEffect[];
	audio?: AudioEffect[];
	keyframes?: Keyframe[];
	/** Per-property animation (the backend omits it when empty): any one number of the
	 *  transform, the colour or the clip's volume with keys of its own. */
	channels?: PropertyTrack[];
	/** 360 reprojection; absent for ordinary flat footage. */
	reframe?: Reframe | null;
	/** Crops kept for delivery shapes other than the project's own, written by
	 *  the multi-format export's framing pass. The project frame's crop is the
	 *  transform's. */
	framings?: Framing[];
	/** Whether the clip renders. Absent means enabled (the backend omits it when true). */
	enabled?: boolean;
	/** The link group: clips sharing a `link_id` (a picture and its detached sound) are
	 *  edited together — moved, trimmed, split, removed. Absent is unlinked (the backend
	 *  omits it). A group of one is not a link; `linkPartners` is the question to ask. */
	link_id?: string | null;
	/** Whether the clip plays the audio of its own asset. `false` once its sound was
	 *  detached onto an audio track (the picture is then silent). Absent means it does
	 *  (the backend omits it when true). */
	source_audio?: boolean;
}

/** A crop for one delivery shape (`aspect_w:aspect_h` in lowest terms). */
export interface Framing {
	aspect_w: number;
	aspect_h: number;
	crop_left: number;
	crop_right: number;
	crop_top: number;
	crop_bottom: number;
}

export const DEFAULT_TRANSFORM: Transform = {
	scale: 1,
	pos_x: 0,
	pos_y: 0,
	rotation: 0,
	opacity: 1,
	crop_left: 0,
	crop_right: 0,
	crop_top: 0,
	crop_bottom: 0
};

export const DEFAULT_COLOR: Color = { brightness: 0, contrast: 1, saturation: 1, gamma: 1, temperature: 0 };

/** A level, forward-facing 100° view — what a 360 clip gets on arrival. */
export const DEFAULT_REFRAME: Reframe = {
	input: 'equirect',
	output: 'flat',
	lens_fov: 190,
	yaw: 0,
	pitch: 0,
	roll: 0,
	fov: 100
};

/** One clip's destination in a group move (`move_clips`): where it starts
 *  afterwards (absolute seconds) and, optionally, another track of the same
 *  kind. Omitted, the clip stays on its track. */
export interface ClipMove {
	clip_id: string;
	timeline_start: number;
	track_id?: string;
}

/** One clip's cut in a group `split_remove_clips`: the clip and the timeline time it
 *  is cut at (inside it). */
export interface ClipCut {
	clip_id: string;
	at: number;
}

/** Which half of a split `split_remove` throws away: everything before the cut
 *  (`left` — trim the start to it) or everything after it (`right`). */
export type SplitSide = 'left' | 'right';

/** What a roll, slip or slide did (`roll_edit` / `slip_clip` / `slide_clip` over MCP;
 *  the Tauri commands answer with the refreshed timeline): how far it was asked to
 *  go, how far it went (it clamps to the footage and the neighbours), and the clips
 *  it changed as they stand afterwards, in timeline order. */
export interface EditOutcome {
	requested: number;
	applied: number;
	/** `applied` is not what was asked for. */
	clamped: boolean;
	clips: Clip[];
}

export interface Track {
	id: string;
	kind: StreamKind;
	name: string;
	clips: Clip[];
	/** Ducked under the rest of the mix on export (sidechain compression). */
	duck?: boolean;
	/** Silenced (audio) or hidden (video) — the track's clips do not render. */
	muted?: boolean;
	/** Soloed. While any track of a kind is soloed, the rest of that kind are shadowed. */
	solo?: boolean;
	/** Guarded against editing. A locked track still renders. */
	locked?: boolean;
	/** Track fader: a linear gain over every clip on the track. 1 is unity. */
	volume?: number;
	/** Stereo placement, -1 hard left to 1 hard right. 0 is centre. */
	pan?: number;
}

/** A named point on the timeline. Renders nothing — it is shared vocabulary
 *  for places in the cut, for the user and the agent alike. */
export interface Marker {
	id: string;
	time: number;
	name: string;
	color?: string | null;
}

/** The frame the project is cut *for* — the shape of the delivered video.
 *  Every rendered picture (preview, scrubbed still, export) uses it, so a
 *  vertical crop is visible while cutting rather than only in the output. */
export interface Delivery {
	width: number;
	height: number;
	fit: Fit;
}

/**
 * The last stage of the mix — mirrors `kerf_core::MasterBus`. A fader and a
 * safety limiter on the finished mix, after every track and the duck bus and
 * before `loudnorm`. Absent (the default, and every project that never touched
 * it) means unity gain, no limiter.
 */
export interface MasterBus {
	/** Linear gain on the finished mix; 1 is unity, clamped to 0..4 (+12 dB). */
	volume: number;
	/** A lookahead limiter holding the mix under `ceiling_db`. */
	limiter: boolean;
	/** Limiter ceiling in dBFS, -24..0 (-1.5 by default). A sample-peak ceiling. */
	ceiling_db: number;
	/** How tracks flagged `duck` dip. Absent / `null` is the sidechain compressor, which
	 *  dips by how loud the rest of the mix is; a number is the **speech gate**: the ducked
	 *  bus drops by exactly that many dB (negative, clamped to -40..-1) while the rest speaks.
	 *  The backend omits it when unset. */
	duck_depth_db?: number | null;
}

export interface Timeline {
	tracks: Track[];
	overlays?: TextOverlay[];
	/** Animated SVG titles, in draw order. The backend omits the list when empty. */
	svg_overlays?: SvgOverlay[];
	markers?: Marker[];
	/** Unset = the shape follows the footage (the historical behaviour). */
	format?: Delivery | null;
	/** The master bus. Unset = unity gain, no limiter. */
	master?: MasterBus;
}

/**
 * What one `ebur128` meter read — mirrors `kerf_core::LevelReading`. `null`
 * wherever there was nothing to report (silence, or a span under 3 s for the
 * short-term maximum).
 */
export interface LevelReading {
	/** Integrated (programme) loudness, LUFS. */
	integrated_lufs: number | null;
	/** Loudness range, LU. */
	loudness_range_lu: number | null;
	/** The loudest 3 s window, LUFS. */
	short_term_max_lufs: number | null;
	/** Highest sample, dBFS. */
	peak_dbfs: number | null;
	/** Highest true (inter-sample) peak, dBTP — what a re-encode can clip on. */
	true_peak_dbtp: number | null;
}

/** One track's strip: after its fader and pan, before the duck bus and the master. */
export interface TrackLevels {
	track_id: string;
	name: string;
	kind: StreamKind;
	ducked: boolean;
	/** Reaches the render at all — false for a muted or solo-shadowed track. */
	heard: boolean;
	level: LevelReading | null;
}

/** How loud the cut is (`get_levels`) — mirrors `kerf_core::Levels`. */
export interface Levels {
	/** Seconds of the cut measured. */
	duration: number;
	/** The finished mix; null when the cut has no audio. */
	master: LevelReading | null;
	tracks: TrackLevels[];
	/** The measurement ran through `loudnorm`. */
	loudnorm: boolean;
	/** The streaming target the notes judge against (-14 LUFS). */
	target_lufs: number;
	/** What the numbers mean for delivery, as advice. */
	notes: string[];
	/** Set **only** by the browser harness, whose numbers are a stand-in
	 *  estimated from the sample analysis — never by the backend. */
	estimated?: boolean;
}

export interface AssetMetadata {
	asset: Asset;
	analysis: AssetAnalysis | null;
}

/**
 * A window of an asset's audio as min/max peaks (`get_waveform_range`).
 * Mirrors `kerf_core::WaveformRange`. `min` / `max` are `[channel][bucket]`,
 * each `buckets` long and in -1..1; a bucket outside the media is 0 / 0.
 */
export interface WaveformRange {
	/** Lanes: 1 (mono) or 2 (stereo). */
	channels: number;
	/** Buckets per channel actually returned — the request, capped at 4096. */
	buckets: number;
	/** Length of the decoded audio in seconds. */
	duration: number;
	/** Resolution of the pyramid level served (10, 25, 100 or 500). */
	peaks_per_second: number;
	min: number[][];
	max: number[][];
}

/**
 * One JPEG of a filmstrip (`get_filmstrip`): `count` thumbnails side by side,
 * from the left. Mirrors `kerf_core::FilmstripSheet` plus the transport the app
 * adds. `width` is always `columns * frame_width`, so the last sheet of a strip
 * may be wider than `count * frame_width` — the tail is padding; trust `count`.
 */
export interface FilmstripSheet {
	/** Index of the first thumbnail on this sheet. */
	first_frame: number;
	/** Thumbnails on this sheet. */
	count: number;
	/** Pixel width of the image. */
	width: number;
	/** Pixel height of the image: the strip's `frame_height`. */
	height: number;
	/** `data:image/jpeg;base64,…` — a `data:` URL because the CSP admits no `blob:`.
	 *  (An SVG data URL in the browser harness, which has no decoder.) */
	data_url: string;
}

/**
 * An asset's thumbnails over time (`get_filmstrip`). Mirrors
 * `kerf_core::Filmstrip`; `filmstrip-geometry.ts` is the mirror of its lookups.
 * Thumbnail `k` is the frame on screen at source time `k * interval`.
 */
export interface Filmstrip {
	/** Seconds of source between two thumbnails. */
	interval: number;
	/** Width of one thumbnail in pixels (even). */
	frame_width: number;
	/** Height of one thumbnail in pixels (96). */
	frame_height: number;
	/** Thumbnails in the strip: at least 1, at most 300. */
	frames: number;
	/** Thumbnails per sheet (the last may hold fewer): thumbnail `k` is on sheet
	 *  `floor(k / columns)`, at `x = (k % columns) * frame_width`, `y = 0`. */
	columns: number;
	/** The sheets, in order; together they hold thumbnails `0..frames`. */
	sheets: FilmstripSheet[];
}

export type EditSource = 'user' | 'agent' | 'system';

export interface Revision {
	seq: number;
	label: string;
	source: EditSource;
	created_at: string;
	current: boolean;
}

export type DiffKind =
	| 'track_added'
	| 'track_removed'
	| 'track_changed'
	| 'clip_added'
	| 'clip_removed'
	| 'clip_moved'
	| 'clip_retrimmed'
	| 'clip_changed'
	| 'overlay_added'
	| 'overlay_removed'
	| 'overlay_changed'
	| 'marker_added'
	| 'marker_removed'
	| 'marker_changed'
	| 'format_changed'
	| 'master_changed';

export interface DiffEntry {
	/** The change's stable name, `<kind>:<the clip, text, marker or track it is about>`, which it keeps for
	 *  as long as the proposal holds it: what `applyStagedSubset` is sent. The backend always sets it; it is
	 *  optional here so a fixture need not, and `entryId` (`staged-subset.ts`) derives the same name. */
	id?: string;
	kind: DiffKind;
	summary: string;
	detail?: string | null;
	track_id?: string | null;
	clip_id?: string | null;
	/** The title or caption an `overlay_*` entry is about. */
	overlay_id?: string | null;
	/** The marker a `marker_*` entry is about. */
	marker_id?: string | null;
	at?: number | null;
}

export interface TimelineDiff {
	entries: DiffEntry[];
	duration_before: number;
	duration_after: number;
	clips_before: number;
	clips_after: number;
}

/** A batch of agent edits held back from the live timeline for review. */
export interface StagedEdit {
	base_seq: number;
	task_id?: string | null;
	note?: string | null;
	/** Some agent session owns the proposal. (The session id itself never leaves the backend.) */
	owned_by_agent?: boolean;
	edits: string[];
	/** How many of the latest staged steps `undoStagedEdit` can still take back (absent: none). */
	undo_depth?: number;
	/** Changes to the library held back with the proposal (an asset marked as 360); they land with it. */
	asset_changes?: StagedAssetChange[];
	created_at: string;
	updated_at: string;
	/** The live timeline moved on since this branched — applying replaces it. */
	stale: boolean;
	diff: TimelineDiff;
}

/** A change to an *asset* an agent made while a proposal was open (`kerf_core::StagedAssetChange`). */
export interface StagedAssetChange {
	/** `asset_projection:<asset id>`, usable wherever a `DiffEntry.id` is. */
	id: string;
	asset_id: string;
	/** The projection the asset is to be marked with; `null` clears it. */
	projection: Projection | null;
	/** One line for the review card. */
	summary: string;
}

/** A change applied although the reviewer did not pick it, and why it had to be. */
export interface AutoIncluded {
	id: string;
	summary: string;
	because: string;
}

/** A picked change that no longer applies, and why. */
export interface SkippedChange {
	id: string;
	summary: string;
	why: string;
}

/** What `applyStagedSubset` did (`kerf_core::SubsetApplied`). */
export interface SubsetApplied {
	/** Changes applied, the ones that came along included. */
	applied: number;
	auto_included: AutoIncluded[];
	skipped: SkippedChange[];
	/** Changes still waiting for review in the proposal. */
	remaining: number;
	/** Nothing is left, so the proposal closed. */
	closed: boolean;
	/** One sentence for the toast. */
	message: string;
	/** The live timeline afterwards. */
	timeline: Timeline;
}

/** What `undoStagedEdit` took back: the step's label and the proposal as it now stands. */
export interface StagedUndone {
	label: string;
	staged: StagedEdit;
}

/** What `relinkAsset` did (`kerf_core::Relinked`). */
export interface Relinked {
	asset: Asset;
	/** The new file is the old media moved, so its analysis still holds; otherwise it was dropped. */
	analysis_kept: boolean;
	previous_path: string;
}

/** What removing an asset did (`kerf_core::RemovedAsset`). */
export interface RemovedAsset {
	asset: Asset;
	/** Clips of the cut removed with it (only with `force`). */
	clips_removed: number;
}

/** The `remove_asset` answer: what was removed and the refreshed cut. */
export interface AssetRemoval extends RemovedAsset {
	timeline: Timeline;
}

/** An asset whose source file is gone (`kerf_core::MissingAsset`). */
export interface MissingAsset {
	asset_id: string;
	name: string;
	path: string;
	/** Clips of the cut that use it. */
	clips: number;
}

export type TaskStatus = 'queued' | 'working' | 'ready' | 'done' | 'failed';

export interface Task {
	id: string;
	prompt: string;
	status: TaskStatus;
	result?: string | null;
	created_at: string;
	updated_at: string;
}

// ---- export options (mirrors kerf_core::engine::ExportOptions) -------------

export type Container = 'mp4' | 'mov' | 'mkv' | 'webm' | 'gif' | 'mp3' | 'm4a' | 'wav' | 'flac';

export type RateControl = 'crf' | 'bitrate' | 'two_pass' | 'lossless';

export interface ExportOptions {
	container: Container;
	video_codec?: string | null;
	audio_codec?: string | null;
	rate_control: RateControl;
	crf?: number | null;
	video_bitrate?: string | null;
	max_rate?: string | null;
	buf_size?: string | null;
	preset?: string | null;
	prores_profile?: number | null;
	tune?: string | null;
	profile_v?: string | null;
	pix_fmt?: string | null;
	hwaccel?: string | null;
	resolution?: [number, number] | null;
	/** How footage of a different shape is fitted to `resolution`: letterboxed
	 *  (`contain`, the default) or filled and cropped (`cover`). Only matters
	 *  when the delivery aspect differs from the footage. */
	fit?: Fit;
	fps?: number | null;
	scaler?: string | null;
	audio_sample_rate?: number | null;
	audio_channels?: number | null;
	audio_bitrate?: string | null;
	flac_compression?: number | null;
	include_audio: boolean;
	faststart: boolean;
	gif_dither?: string | null;
	gif_loop: boolean;
	metadata_title?: string | null;
	/** Render only this timeline span (e.g. the in/out marks); omit for all. */
	range?: TimeRange | null;
	/** Normalize the final mix to -14 LUFS before encoding. The same as `loudness: 'youtube'`. */
	loudnorm?: boolean;
	/** Normalize the final mix to a platform's loudness target. Wins over `loudnorm`. */
	loudness?: LoudnessPreset | null;
}

/** A delivery's loudness target (`kerf_core::LoudnessPreset`): YouTube and Spotify -14 LUFS,
 *  Apple -16, EBU R128 broadcast -23, all with a -1 dBTP true-peak ceiling. */
export type LoudnessPreset = 'youtube' | 'spotify' | 'apple' | 'broadcast';

/** How a clip's picture is fitted to an output frame of a different shape. */
export type Fit = 'contain' | 'cover';

/** A place a finished cut gets published, mirroring `kerf_core::platform`. */
export interface PlatformTarget {
	id: string;
	label: string;
	width: number;
	height: number;
	accepts: [number, number][];
	max_secs: number | null;
	reach_max_secs: number | null;
	min_secs: number | null;
	notes: string;
}

/** `error` = the platform rejects it, `warning` = accepted then under-distributed
 *  or letterboxed, `tip` = advice. */
export type Severity = 'error' | 'warning' | 'tip';

/** What an issue is about, so a UI can group four identical shape complaints
 *  into one line naming four platforms. */
export type IssueKind = 'empty' | 'length' | 'shape' | 'resolution' | 'captions';

export interface DeliveryIssue {
	severity: Severity;
	kind: IssueKind;
	message: string;
}

/** A cut's readiness for one publishing target. */
export interface DeliveryCheck {
	target: string;
	label: string;
	ok: boolean;
	issues: DeliveryIssue[];
}

/** What a readiness check read off the cut (`kerf_core::CutSummary`): the frame and length the
 *  file would have, which is not always the project's. */
export interface CutSummary {
	duration: number;
	width: number;
	height: number;
	has_audio: boolean;
	has_text: boolean;
}

/** `kerf_core::PlatformReport`: every target's check, with the cut they judged. */
export interface PlatformReport {
	cut: CutSummary;
	targets: DeliveryCheck[];
}

/** Payload of the `export-progress` event streamed during a render. */
export interface ExportProgress {
	fraction: number;
	elapsed_secs: number;
	eta_secs?: number | null;
	/** Set while the render has not started because the machine is busy with another heavy job
	 *  (`waiting for the preview proxy`): a queued export is not hung at 0%. */
	waiting?: string | null;
	/** Set by a multi-format export: which file is rendering, of how many.
	 *  `fraction` then spans all of them. */
	variant?: number;
	total?: number;
}

/** Payload of the `import-progress` event, emitted while a 360 pair is stitched. */
export interface ImportProgress extends ExportProgress {
	/** The file the user picked, so a batch import can label which one is working. */
	path: string;
}

/**
 * Payload of the `analysis-progress` event: one step of an analysis pass.
 * Transcription can download a model and then run for minutes, so analysis
 * reports where it is rather than showing an indeterminate spinner.
 */
export interface AnalysisProgress {
	asset_id: string;
	/** `waiting` (queued behind a preview proxy) · `silence` · `scenes` · `loudness` · `rhythm` ·
	 *  `download_model` · `transcribe` · `done` */
	stage: string;
	fraction?: number | null;
	/** A short note, e.g. `84 MB / 142 MB`. */
	detail?: string | null;
}

/** Payload of the `smart-crop-progress` event: how much of the shots have been sampled. */
export interface SmartCropProgress {
	fraction: number;
}

/** Payload of the `model-progress` event, emitted while a speech model downloads. */
export interface ModelProgress {
	model: string;
	downloaded_bytes: number;
	total_bytes?: number | null;
	fraction?: number | null;
}

/** A downloadable whisper.cpp speech model. */
export interface SpeechModelInfo {
	name: string;
	approx_bytes: number;
	multilingual: boolean;
}

/** Which speech-to-text backend this build uses, and whether it is ready. */
export interface TranscriptionStatus {
	/** `libwhisper` (in-process) · `ffmpeg_filter` · `none` */
	backend: string;
	available: boolean;
	/** The Settings toggle: off, an analyzed asset has no transcript. */
	enabled: boolean;
	model?: string | null;
	model_path?: string | null;
	model_ready: boolean;
	approx_download_bytes?: number | null;
	models: SpeechModelInfo[];
	reason?: string | null;
}

/** One Kokoro voice. `downloaded` is whether its style file is already cached. */
export interface VoiceInfo {
	id: string;
	name: string;
	accent: 'us' | 'gb';
	gender: 'female' | 'male';
	downloaded: boolean;
}

/** Whether voiceover can run, and what the first use still has to fetch. */
export interface VoiceoverStatus {
	available: boolean;
	/** The runtime and model are cached; only a new voice may still download. */
	ready: boolean;
	approx_download_bytes: number;
	default_voice: string;
	voices: VoiceInfo[];
	reason: string | null;
}

/** Payload of the `voiceover-progress` event, from the GUI or an agent. */
export interface VoiceoverProgress {
	stage: 'download_runtime' | 'download_model' | 'download_voice' | 'synthesize' | 'waiting';
	fraction: number | null;
	/** A short note, e.g. `42 MB / 88 MB` or `3 of 7 sentences`. */
	detail: string | null;
}

/** The arguments `generate_voiceover` takes; all but the script are optional. */
export interface VoiceoverRequest {
	text: string;
	voice?: string;
	speed?: number;
	/** The audio track to land on; omitted, the "VO" track (made on first use). */
	trackId?: string;
	/** Where it starts; omitted, it is appended to the track's end. */
	timelineStart?: number;
	/** Caption the cut from the script once it is placed. */
	captions?: CaptionOptions;
}

export interface VoiceoverResult {
	asset: Asset;
	timeline: Timeline;
}

/** What stem separation has on disk (`kerf_core::StemsStatus`): whether its first use still
 *  downloads the ONNX runtime or the Demucs model, how big that model is and the stems it makes. */
export interface StemsStatus {
	runtime_ready: boolean;
	model_ready: boolean;
	model_bytes: number;
	/** In order: `drums`, `bass`, `other`, `vocals`. */
	stems: string[];
}

/** Payload of the `stems-progress` event, the same shape as `voiceover-progress`. */
export interface StemsProgress {
	stage: 'download_runtime' | 'download_model' | 'separate' | 'encode';
	fraction: number | null;
	/** A short note, e.g. `42 MB / 170 MB` or the stem being encoded. */
	detail: string | null;
}

/** What `place_stems` did (`kerf_core::StemsPlaced`): the stem assets, in `stems` order, and the
 *  clips laid under the separated clip (empty when only the library was asked for). */
export interface StemsPlaced {
	assets: Asset[];
	clips: Clip[];
}

/** The `separate_stems` answer: what was placed and the refreshed timeline. */
export interface StemsResult {
	placed: StemsPlaced;
	timeline: Timeline;
}

/** What the app was launched asking to open (kerf-app's `LaunchProject`, externally
 *  tagged): a `.kerf` that exists, or one named on the command line that is not
 *  there — never created, so the page says so instead. */
export type LaunchRequest = { open: string } | { missing: string };

/** A newer signed release found on GitHub by the updater. */
export interface UpdateInfo {
	/** The available version, e.g. `0.18.0`. */
	version: string;
	/** The version this build is running. */
	current_version: string;
	/** Release date as published in `latest.json`, when present. */
	date?: string | null;
	/** Release notes (the GitHub release body), when present. */
	notes?: string | null;
}

export type TimeDisplay = 'timecode' | 'seconds';

/** App preferences — machine settings, not project state, so they live in the
 *  platform config directory rather than in the `.kerf` file. */
export interface AppSettings {
	/** Share of the machine one heavy job (analysis, transcription, proxy,
	 *  stitch, export) may take. Kerf runs one such job at a time; this is how
	 *  much of the computer that job gets. */
	cpu_percent: number;
	/** Which analyses run by themselves when media is imported: a master switch and
	 *  one per kind. (Replaces the old `transcribe` flag; the backend carries an
	 *  older settings file forward.) */
	auto_analysis: AutoAnalysis;
	/** How wide preview proxies are — 720, 1080 or 1280 pixels across, or 0 for none. */
	proxy_size: ProxySize;
	/** Which file the preview decodes. Export always reads the originals. */
	preview_source: PreviewSource;
	/** Whether the preview shades the delivery safe areas — where a phone's
	 *  own UI covers a vertical or square cut. */
	safe_areas: boolean;
	/** Whether the Preview panel is drawn by the GPU compositor in a native
	 *  surface for the frames it draws exactly, instead of FFmpeg's JPEG for
	 *  every frame (experimental; off by default). Frames it cannot draw, and
	 *  every failure, are the JPEG as ever. */
	gpu_preview: boolean;
	/** How times are written: frame-accurate `timecode` (the default) or plain `seconds`. */
	time_display: TimeDisplay;
	/** Open the project that was open last when Kerf starts without one named on its command line. */
	reopen_last_project: boolean;
	/** The `.kerf` files opened or saved most recently, newest first, at most 10. Kept by the app
	 *  (`recentProjects` prunes the ones that are gone, `clearRecentProjects` empties it); a settings
	 *  patch cannot write it. */
	recent_projects: string[];
	/** The single dock arrangement saved before there were workspaces (dockview's
	 *  serialized layout). Read only to migrate it into the Edit workspace when
	 *  `workspaces` is absent; nothing writes it any more. */
	layout: unknown | null;
	/** The color theme, validated by `theme.ts` on the way back in. */
	theme: unknown | null;
	/** The workspaces: `{ active, layouts: { <workspace>: <dockview layout> },
	 *  library: { tab, collapsed } }`, validated by `workspaces.ts` on the way
	 *  back in. Opaque to the backend. */
	workspaces: unknown | null;
	/** What the user changed about the keyboard shortcuts: `{ version, bindings:
	 *  { <action id>: [<chord>, …] } }`, only the actions they touched, validated
	 *  and migrated by `keymap.ts` on the way back in. Opaque to the backend. */
	keybindings: unknown | null;
	/** Whether an agent's edits are held back for the user to review (the default). Changed on the
	 *  Agent settings page (`setMcpSettings`); the agent cannot turn it off. */
	agent_review: boolean;
	/** Whether the embedded MCP server listens at all. */
	mcp_enabled: boolean;
	/** Where it listens (`host:port`); `null` is `127.0.0.1:7777`. `KERF_MCP_ADDR` in the environment
	 *  still overrides it (`McpConnectionInfo.env_override`). */
	mcp_addr: string | null;
	/** Whether a request must carry `Authorization: Bearer <token>`. The token itself is never in the
	 *  settings view: ask `mcpConnectionInfo`. */
	mcp_require_token: boolean;
}

/** What `setSettings` may write: any preference but the recent-projects list, which the app keeps itself
 *  as projects open and save (the backend refuses a patch that names it). */
export type SettingsPatch = Partial<Omit<AppSettings, 'recent_projects'>>;

/** `AppSettings` resolved against the engine, which is what the settings
 *  surface renders: the effective percentage (an environment override may win
 *  over the stored one), the cores it works out to, and the machine it is a
 *  share of. */
export interface SettingsView extends AppSettings {
	cpu_cores: number;
	cpu_threads: number;
	cpu_min_percent: number;
}

/** The dialog's baseline options (a preset is applied over this). One deliberate
 *  departure from the bare Rust `Default`: source decode is `hwaccel: 'auto'`,
 *  so exports GPU-decode where the machine can (ffmpeg falls back to software
 *  at init, and the engine retries a failed hardware render in software). */
export const DEFAULT_EXPORT_OPTIONS: ExportOptions = {
	container: 'mp4',
	rate_control: 'crf',
	fit: 'contain',
	include_audio: true,
	faststart: false,
	gif_loop: true,
	hwaccel: 'auto'
};

export const clipDuration = (clip: Clip): number => {
	const span = Math.max(0, clip.source_out - clip.source_in);
	const speed = Math.max(Math.abs(clip.speed ?? 1), 0.01);
	return span / speed;
};

/** What detaching sound from several clips did (`AudioDetached` in `crates/kerf-app/src/lib.rs`, the
 *  answer of `extract_audio` and `detach_audio_clips`): the refreshed timeline, how many clips were
 *  detached, and the ones left alone with the reason (a locked track, an asset without audio, a sound
 *  already detached). */
export interface AudioDetached {
	timeline: Timeline;
	detached: number;
	skipped: { clip_id: string; reason: string }[];
}

// ---- GPU preview (A2) -------------------------------------------------------

/** What the GPU preview is doing on this machine (`GpuPreviewStatus` in
 *  `crates/kerf-app/src/gpu_preview.rs`). */
export interface GpuPreviewStatus {
	/** The setting. */
	enabled: boolean;
	/** The platform has a surface technique (it may still fail to build one). */
	supported: boolean;
	/** `window`: the main window's own surface under a transparent webview.
	 *  `child`: a borderless child window over the panel (X11). */
	technique: 'window' | 'child' | null;
	/** A device and a surface are up. */
	ready: boolean;
	/** The page can draw over the picture (titles, guides, the trim monitor). */
	overlays: boolean;
	adapter: string | null;
	/** The adapter is a CPU rasterizer (lavapipe, WARP). */
	software: boolean;
	/** Why the GPU preview is not in use: no technique here, or the last failure. */
	reason: string | null;
}

/** One kind of reason a preview frame went through FFmpeg, counted over the session
 *  (`RefusalCount` in `gpu_preview/diagnostics.rs`). */
export interface GpuRefusalCount {
	/** A stable key (`mask`, `mixed_matrices`, `text_too_small`, `self_test_pending`, …). */
	kind: string;
	count: number;
	/** The first message of this kind seen this session. */
	example: string;
}

/** What this session's preview frames did (`GpuDiagnostics` in `gpu_preview.rs`): local,
 *  in memory, for finding out which refusals matter on real projects. */
export interface GpuPreviewDiagnostics {
	tally: {
		gpu_frames: number;
		ffmpeg_frames: number;
		/** Frame requests a newer one replaced before they were drawn. */
		superseded: number;
		/** Most frequent first. */
		refusals: GpuRefusalCount[];
	};
	/** How often the frames' inputs came from the held snapshot, and how often they were taken
	 *  from the project (its lock, its timeline parsed). */
	snapshots: { served: number; taken: number };
	requests: { posted: number; superseded: number };
	/** What the play sessions did (`PlayTally`): sessions the GPU played, first requests it
	 *  declined, sessions that ended in the stream, frames drawn / skipped / repeated. */
	play: { sessions: number; declined: number; handed_over: number; frames: number; skipped: number; repeats: number };
	/** The parity self-test against this machine's FFmpeg: running, passed or closed (and why). */
	self_test: string | null;
	adapter: string | null;
	software: boolean;
}

/** Where the Preview frame is in the window, in device pixels relative to the
 *  webview's top-left corner (`BoundsReport` in `gpu_preview.rs`). */
export interface PreviewBoundsReport {
	x: number;
	y: number;
	width: number;
	height: number;
	/** The webview's own size in device pixels. */
	viewport_width: number;
	viewport_height: number;
	/** Whether the native surface should be showing. */
	visible: boolean;
	/** The colour around the picture, `#rrggbb`. */
	matte: string | null;
	/** The colour of the rest of the surface, `#rrggbb` — what a transparent webview shows
	 *  where no element of the page paints. */
	backdrop: string | null;
	/** Which report this is, counting up across every Preview the page has had; the
	 *  backend ignores one older than the newest it has seen. */
	seq: number;
}

/** What one GPU frame took (`GpuTimings`). */
export interface GpuTimings {
	width: number;
	height: number;
	decode_ms: number;
	composite_ms: number;
	present_ms: number;
}

/** Which request of which pump a `get_preview_frame` is. IPC calls can reach the backend in the
 *  other order than they were made; the backend draws only the newest number of a `client`
 *  and answers `superseded` to an older one that arrives after it. */
export interface PreviewOrder {
	client: string;
	seq: number;
}

/** The first frame ahead that the GPU will not draw during playback (`AheadInfo` in
 *  `crates/kerf-app/src/gpu_preview/play.rs`), and where a stream that carries on from the compositor
 *  has to start. */
export interface PlayAhead {
	/** The time of that frame, timeline seconds. */
	at: number;
	/** The time to start the stream at: at or before `at`, with no transition window open. */
	handover: number;
	/** A stable key for the reason (`mask`, `reversed`, `moving_text`, `cursor`, …). */
	kind: string;
	why: string;
}

/** What a forward `get_preview_frame` says about its play session (`PlayInfo`). */
export interface PlayInfo {
	session: number;
	/** The timeline time of the session's first frame (a frame of the project's grid). */
	start: number;
	/** The frame drawn, counted from `start`. */
	frame: number;
	/** The request fell in the frame already on the screen: nothing was drawn. */
	repeat: boolean;
	ahead: PlayAhead | null;
}

/** The answer of `get_preview_frame` (`PreviewFrameResult`): which renderer made the
 *  frame, and the JPEG when it was FFmpeg. */
export interface PreviewFrameResult {
	/** `gpu`: drawn in the native surface (the webview shows it by showing nothing
	 *  over it). `ffmpeg`: the `frame` data URL. `superseded`: a newer request took this
	 *  one's place before it was drawn (the backend keeps only the newest); nothing was
	 *  drawn and the answer is ignored. `stream` (to a forward request only): the GPU does
	 *  not play this session, or no longer, and the page plays the stream, from
	 *  `play.ahead.handover` when there is one. */
	renderer: 'gpu' | 'ffmpeg' | 'superseded' | 'stream';
	frame: string | null;
	/** Why the GPU did not draw it (empty when it did). */
	reasons: string[];
	timings: GpuTimings | null;
	/** To a forward request: the session's state. */
	play?: PlayInfo | null;
}

/** How the page asks for a frame of GPU playback: the session it belongs to. */
export interface PlayAsk {
	session: number;
}

/** The answer of `fit_music` (`MusicFitted` in `crates/kerf-app/src/lib.rs`): the refreshed
 *  timeline and the report. */
export interface MusicFitted {
	timeline: Timeline;
	report: MusicFitReport;
}

// ---- agent connection (MCP server): `crates/kerf-app/src/mcp_commands.rs` ---------

/** What the Agent settings page shows and the user copies into an MCP client
 *  (`McpConnectionInfo`). The token is a secret: show it on request, never log it. */
export interface McpConnectionInfo {
	/** Whether the server is switched on. */
	enabled: boolean;
	/** Where a client connects, e.g. `http://127.0.0.1:7777/mcp`. */
	url: string;
	/** Whether requests must carry `Authorization: Bearer <token>`. */
	require_token: boolean;
	/** The bearer token (made on the first start), even while it is not required. */
	token: string | null;
	/** Why the token is not what it should be: it could not be saved (it works until Kerf quits) or
	 *  could not be made (the server stays off while one is required). `null` when it is fine. */
	token_problem: string | null;
	/** Whether the listener is up now. */
	running: boolean;
	/** Why it is not, when it should be (the port is taken, the address is refused). */
	error: string | null;
	/** The stored address (`null` is the default `127.0.0.1:7777`); what the address field edits. */
	configured_addr: string | null;
	/** `KERF_MCP_ADDR` is set in the environment and wins over `configured_addr`. */
	env_override: boolean;
	/** Whether an agent's edits wait for the user's review (the default). */
	agent_review: boolean;
	/** The one-line command that adds Kerf to Claude Code. */
	claude_code_command: string;
	/** A `mcpServers` JSON snippet for any other MCP client. */
	generic_json_config: string;
}

/** The agent switches `set_mcp_settings` takes — any of them. The server restarts by itself when
 *  the address or the enabled switch moves; a token or requirement change applies at once. */
export interface McpSettingsPatch {
	mcp_enabled?: boolean;
	/** `host:port`; `null` goes back to the default. A network address needs the token. */
	mcp_addr?: string | null;
	mcp_require_token?: boolean;
	agent_review?: boolean;
}
