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

export interface AssetAnalysis {
	asset_id: string;
	silence_segments: TimeRange[];
	scene_changes: number[];
	transcript: TranscriptSegment[];
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

export const clipDuration = (clip: Clip): number => {
	const span = Math.max(0, clip.source_out - clip.source_in);
	const speed = Math.max(Math.abs(clip.speed ?? 1), 0.01);
	return span / speed;
};
