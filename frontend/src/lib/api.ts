// Bridge to the Tauri backend (kerf-core via kerf-app commands).
//
// When running outside Tauri (e.g. `bun run dev` in a browser for design work)
// the calls fall back to seeded sample data and a local in-memory timeline, so
// the whole editor — including edits, analysis, and waveforms — stays
// explorable without the desktop shell.

import type { Asset, AssetAnalysis, AssetMetadata, Clip, Timeline, Track } from './types';
import { clipDuration } from './types';

export function inTauri(): boolean {
	return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	const { invoke } = await import('@tauri-apps/api/core');
	return invoke<T>(cmd, args);
}

// ---- sample fallback (browser dev) ----------------------------------------

const sampleAssets: Asset[] = [
	{
		id: '11111111-1111-1111-1111-111111111111',
		path: '/samples/interview.mp4',
		name: 'interview.mp4',
		duration: 120,
		streams: [
			{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps: 30 },
			{ index: 1, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 }
		],
		imported_at: new Date().toISOString()
	},
	{
		id: '22222222-2222-2222-2222-222222222222',
		path: '/samples/broll.mp4',
		name: 'broll.mp4',
		duration: 45,
		streams: [{ index: 0, kind: 'video', codec: 'h264', width: 3840, height: 2160, fps: 24 }],
		imported_at: new Date().toISOString()
	}
];

const sampleAnalysis: Record<string, AssetAnalysis> = {
	[sampleAssets[0].id]: {
		asset_id: sampleAssets[0].id,
		silence_segments: [
			{ start: 12.5, end: 14 },
			{ start: 60, end: 63.2 }
		],
		scene_changes: [0, 30, 75, 110],
		transcript: [
			{ start: 0, end: 5.5, text: 'Welcome back to the channel.' },
			{ start: 5.5, end: 12.5, text: 'Today we are talking about non-destructive editing.' },
			{ start: 14, end: 22, text: 'The agent watches the footage with you and proposes a cut.' }
		]
	},
	[sampleAssets[1].id]: {
		asset_id: sampleAssets[1].id,
		silence_segments: [],
		scene_changes: [0, 8, 20, 33],
		transcript: []
	}
};

const sampleTimeline: Timeline = {
	tracks: [
		{
			id: 'v1',
			kind: 'video',
			name: 'V1',
			clips: [
				{ id: 'c1', asset_id: sampleAssets[0].id, source_in: 0, source_out: 12.5, timeline_start: 0, volume: 1 },
				{ id: 'c2', asset_id: sampleAssets[1].id, source_in: 0, source_out: 8, timeline_start: 12.5, volume: 1 }
			]
		},
		{
			id: 'a1',
			kind: 'audio',
			name: 'A1',
			clips: [{ id: 'c3', asset_id: sampleAssets[0].id, source_in: 0, source_out: 120, timeline_start: 0, volume: 1 }]
		}
	]
};

// ---- local timeline ops (browser dev fallback) ----------------------------

let devTimeline: Timeline = structuredClone(sampleTimeline);
const uid = () => (crypto.randomUUID ? crypto.randomUUID() : `id-${Math.random().toString(36).slice(2)}`);
const snapshot = () => structuredClone(devTimeline);

function trackEnd(t: Track): number {
	return t.clips.reduce((m, c) => Math.max(m, c.timeline_start + clipDuration(c)), 0);
}
function reflow(t: Track) {
	let cursor = 0;
	for (const c of t.clips) {
		c.timeline_start = cursor;
		cursor += clipDuration(c);
	}
}
function locate(tl: Timeline, clipId: string): [Track, number] | null {
	for (const t of tl.tracks) {
		const i = t.clips.findIndex((c) => c.id === clipId);
		if (i >= 0) return [t, i];
	}
	return null;
}
function assetById(id: string): Asset | undefined {
	return sampleAssets.find((a) => a.id === id);
}
function trackForAsset(tl: Timeline, assetId: string): Track {
	const hasVideo = assetById(assetId)?.streams.some((s) => s.kind === 'video');
	return tl.tracks.find((t) => t.kind === (hasVideo ? 'video' : 'audio')) ?? tl.tracks[0];
}

// ---- read ------------------------------------------------------------------

export async function listAssets(): Promise<Asset[]> {
	if (!inTauri()) return structuredClone(sampleAssets);
	return invoke<Asset[]>('list_assets');
}

export async function getTimeline(): Promise<Timeline> {
	if (!inTauri()) return snapshot();
	return invoke<Timeline>('get_timeline');
}

export async function getAssetMetadata(assetId: string): Promise<AssetMetadata> {
	if (!inTauri()) {
		const asset = assetById(assetId) ?? sampleAssets[0];
		return { asset, analysis: sampleAnalysis[asset.id] ?? null };
	}
	return invoke<AssetMetadata>('get_asset_metadata', { assetId });
}

// ---- import / analysis -----------------------------------------------------

export async function importAsset(path: string): Promise<Asset> {
	if (!inTauri()) throw new Error('import is only available in the desktop app');
	return invoke<Asset>('import_asset', { path });
}

/** Open a native file picker and import the chosen media file. */
export async function pickAndImport(): Promise<Asset | null> {
	if (!inTauri()) return null;
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: false,
		filters: [{ name: 'Media', extensions: ['mp4', 'mov', 'mkv', 'webm', 'wav', 'mp3', 'm4a', 'aac'] }]
	});
	if (typeof selected !== 'string') return null;
	return importAsset(selected);
}

export async function analyzeAsset(assetId: string): Promise<AssetAnalysis> {
	if (!inTauri()) {
		await new Promise((r) => setTimeout(r, 900));
		return sampleAnalysis[assetId] ?? { asset_id: assetId, silence_segments: [], scene_changes: [], transcript: [] };
	}
	return invoke<AssetAnalysis>('analyze_asset', { assetId });
}

// ---- timeline editing (each resolves to the refreshed timeline) ------------

export async function cutClip(assetId: string, start: number, end: number): Promise<Timeline> {
	if (!inTauri()) {
		const track = trackForAsset(devTimeline, assetId);
		track.clips.push({ id: uid(), asset_id: assetId, source_in: start, source_out: end, timeline_start: trackEnd(track), volume: 1 });
		return snapshot();
	}
	return invoke<Timeline>('cut_clip', { assetId, start, end });
}

export async function addClip(
	assetId: string,
	sourceIn: number,
	sourceOut: number,
	trackId?: string,
	timelineStart?: number
): Promise<Timeline> {
	if (!inTauri()) {
		const track = (trackId && devTimeline.tracks.find((t) => t.id === trackId)) || trackForAsset(devTimeline, assetId);
		const start = timelineStart ?? trackEnd(track);
		track.clips.push({ id: uid(), asset_id: assetId, source_in: sourceIn, source_out: sourceOut, timeline_start: start, volume: 1 });
		return snapshot();
	}
	return invoke<Timeline>('add_clip', { assetId, trackId, sourceIn, sourceOut, timelineStart });
}

export async function splitClip(clipId: string, at: number): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const [track, ci] = found;
			const clip = track.clips[ci];
			if (at > clip.timeline_start && at < clip.timeline_start + clipDuration(clip)) {
				const splitSrc = clip.source_in + (at - clip.timeline_start);
				const right: Clip = { ...clip, id: uid(), source_in: splitSrc, timeline_start: at };
				clip.source_out = splitSrc;
				track.clips.splice(ci + 1, 0, right);
			}
		}
		return snapshot();
	}
	return invoke<Timeline>('split_clip', { clipId, at });
}

export async function trimClip(clipId: string, sourceIn?: number, sourceOut?: number): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const clip = found[0].clips[found[1]];
			if (sourceIn != null) clip.source_in = sourceIn;
			if (sourceOut != null) clip.source_out = sourceOut;
		}
		return snapshot();
	}
	return invoke<Timeline>('trim_clip', { clipId, sourceIn, sourceOut });
}

export async function reorderClip(trackId: string, clipId: string, newIndex: number): Promise<Timeline> {
	if (!inTauri()) {
		const track = devTimeline.tracks.find((t) => t.id === trackId);
		if (track) {
			const cur = track.clips.findIndex((c) => c.id === clipId);
			if (cur >= 0) {
				const [clip] = track.clips.splice(cur, 1);
				track.clips.splice(Math.min(newIndex, track.clips.length), 0, clip);
				reflow(track);
			}
		}
		return snapshot();
	}
	return invoke<Timeline>('reorder_clip', { trackId, clipId, newIndex });
}

export async function removeClip(clipId: string): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips.splice(found[1], 1);
		return snapshot();
	}
	return invoke<Timeline>('remove_clip', { clipId });
}

export async function setVolume(clipId: string, volume: number): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].volume = volume;
		return snapshot();
	}
	return invoke<Timeline>('set_volume', { clipId, volume });
}

export async function removeSilence(assetId: string): Promise<Timeline> {
	if (!inTauri()) {
		const asset = assetById(assetId);
		const silence = [...(sampleAnalysis[assetId]?.silence_segments ?? [])].sort((a, b) => a.start - b.start);
		const track = trackForAsset(devTimeline, assetId);
		let cursor = 0;
		let start = trackEnd(track);
		const keep: [number, number][] = [];
		for (const s of silence) {
			if (s.start > cursor) keep.push([cursor, s.start]);
			cursor = Math.max(cursor, s.end);
		}
		if (asset && cursor < asset.duration) keep.push([cursor, asset.duration]);
		for (const [si, so] of keep) {
			track.clips.push({ id: uid(), asset_id: assetId, source_in: si, source_out: so, timeline_start: start, volume: 1 });
			start += so - si;
		}
		return snapshot();
	}
	return invoke<Timeline>('remove_silence', { assetId });
}

export async function extractAudio(assetId: string): Promise<Timeline> {
	if (!inTauri()) {
		const asset = assetById(assetId);
		const track = devTimeline.tracks.find((t) => t.kind === 'audio') ?? devTimeline.tracks[0];
		if (asset) track.clips.push({ id: uid(), asset_id: assetId, source_in: 0, source_out: asset.duration, timeline_start: trackEnd(track), volume: 1 });
		return snapshot();
	}
	return invoke<Timeline>('extract_audio', { assetId });
}

export async function concatenate(assetIds: string[]): Promise<Timeline> {
	if (!inTauri()) {
		for (const aId of assetIds) await cutClip(aId, 0, assetById(aId)?.duration ?? 0);
		return snapshot();
	}
	return invoke<Timeline>('concatenate', { assetIds });
}

// ---- media (preview frames, waveforms) -------------------------------------

/** A PNG `data:` URL for one decoded frame, or `null` outside the desktop app. */
export async function getFrame(assetId: string, timeSecs: number, maxWidth = 960): Promise<string | null> {
	if (!inTauri()) return null;
	return invoke<string>('get_frame', { assetId, timeSecs, maxWidth });
}

export async function getWaveform(assetId: string, buckets: number): Promise<number[]> {
	if (!inTauri()) {
		// Synthetic but deterministic peaks so the browser demo shows a waveform.
		return Array.from({ length: buckets }, (_, i) => {
			const seed = Math.sin(i * 0.7) * Math.cos(i * 0.19);
			return Math.min(1, 0.25 + Math.abs(seed) * 0.7);
		});
	}
	return invoke<number[]>('get_waveform', { assetId, buckets });
}

// ---- export ----------------------------------------------------------------

export async function exportTimeline(outputPath: string, format: string): Promise<string> {
	if (!inTauri()) throw new Error('export is only available in the desktop app');
	return invoke<string>('export_timeline', { outputPath, format });
}

/** Open a save dialog and render the timeline to the chosen path. */
export async function pickAndExport(): Promise<string | null> {
	if (!inTauri()) return null;
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({
		filters: [{ name: 'Video', extensions: ['mp4', 'mov', 'mkv'] }],
		defaultPath: 'kerf-export.mp4'
	});
	if (typeof path !== 'string') return null;
	const ext = path.split('.').pop() || 'mp4';
	return exportTimeline(path, ext);
}
