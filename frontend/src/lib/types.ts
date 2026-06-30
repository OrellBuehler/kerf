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
}

export interface Asset {
	id: string;
	path: string;
	name: string;
	duration: number;
	streams: StreamInfo[];
	imported_at: string;
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
}

export type TransitionKind = 'crossfade' | 'dip_to_black';

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
	bold: boolean;
	keyframes?: TextKeyframe[];
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
	effects?: VideoEffect[];
	audio?: AudioEffect[];
	keyframes?: Keyframe[];
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

export const DEFAULT_COLOR: Color = { brightness: 0, contrast: 1, saturation: 1, gamma: 1 };

export interface Track {
	id: string;
	kind: StreamKind;
	name: string;
	clips: Clip[];
}

export interface Timeline {
	tracks: Track[];
	overlays?: TextOverlay[];
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
}

/** Payload of the `export-progress` event streamed during a render. */
export interface ExportProgress {
	fraction: number;
	elapsed_secs: number;
	eta_secs?: number | null;
}

/** The bare Rust `Default` — the dialog opens by applying a preset over this. */
export const DEFAULT_EXPORT_OPTIONS: ExportOptions = {
	container: 'mp4',
	rate_control: 'crf',
	include_audio: true,
	faststart: false,
	gif_loop: true
};

export const clipDuration = (clip: Clip): number => {
	const span = Math.max(0, clip.source_out - clip.source_in);
	const speed = Math.max(Math.abs(clip.speed ?? 1), 0.01);
	return span / speed;
};
