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

export interface Clip {
	id: string;
	asset_id: string;
	source_in: number;
	source_out: number;
	timeline_start: number;
	volume: number;
}

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

export type TaskStatus = 'queued' | 'working' | 'ready' | 'done' | 'failed';

export interface Task {
	id: string;
	prompt: string;
	status: TaskStatus;
	result?: string | null;
	created_at: string;
	updated_at: string;
}

export const clipDuration = (clip: Clip): number => Math.max(0, clip.source_out - clip.source_in);
