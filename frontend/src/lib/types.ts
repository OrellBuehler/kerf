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
}

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

/** One keyframe of a clip's animated transform. */
export interface Keyframe {
	time: number;
	scale: number;
	pos_x: number;
	pos_y: number;
	rotation: number;
	opacity: number;
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
	/** Written by `generate_captions` rather than by hand. Regenerating replaces
	 *  these and leaves typed titles alone. */
	generated?: boolean;
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
	/** 360 reprojection; absent for ordinary flat footage. */
	reframe?: Reframe | null;
	/** Whether the clip renders. Absent means enabled (the backend omits it when true). */
	enabled?: boolean;
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

export interface Timeline {
	tracks: Track[];
	overlays?: TextOverlay[];
	markers?: Marker[];
	/** Unset = the shape follows the footage (the historical behaviour). */
	format?: Delivery | null;
}

export interface AssetMetadata {
	asset: Asset;
	analysis: AssetAnalysis | null;
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
	| 'format_changed';

export interface DiffEntry {
	kind: DiffKind;
	summary: string;
	detail?: string | null;
	track_id?: string | null;
	clip_id?: string | null;
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
	edits: string[];
	created_at: string;
	updated_at: string;
	/** The live timeline moved on since this branched — applying replaces it. */
	stale: boolean;
	diff: TimelineDiff;
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
	/** Normalize the final mix to -14 LUFS before encoding. */
	loudnorm?: boolean;
}

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

/** Payload of the `export-progress` event streamed during a render. */
export interface ExportProgress {
	fraction: number;
	elapsed_secs: number;
	eta_secs?: number | null;
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
	/** `silence` · `scenes` · `loudness` · `rhythm` · `download_model` · `transcribe` · `done` */
	stage: string;
	fraction?: number | null;
	/** A short note, e.g. `84 MB / 142 MB`. */
	detail?: string | null;
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

/** App preferences — machine settings, not project state, so they live in the
 *  platform config directory rather than in the `.kerf` file. */
export interface AppSettings {
	/** Share of the machine one heavy job (analysis, transcription, proxy,
	 *  stitch, export) may take. Kerf runs one such job at a time; this is how
	 *  much of the computer that job gets. */
	cpu_percent: number;
	/** Whether the analysis pass transcribes speech. Off, importing media still
	 *  detects silence / scenes / loudness / rhythm but never fetches a speech
	 *  model or runs inference. */
	transcribe: boolean;
	/** Whether the preview shades the delivery safe areas — where a phone's
	 *  own UI covers a vertical or square cut. */
	safe_areas: boolean;
	/** The workspace arrangement (dockview's serialized layout), validated by
	 *  `layout.ts` on the way back in. */
	layout: unknown | null;
	/** The color theme, validated by `theme.ts` on the way back in. */
	theme: unknown | null;
}

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
