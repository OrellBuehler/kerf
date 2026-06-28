// Bridge to the Tauri backend (kerf-core via kerf-app commands).
//
// When running outside Tauri (e.g. `bun run dev` in a browser for design work)
// the calls fall back to seeded sample data and a local in-memory timeline, so
// the whole editor — including edits, analysis, and waveforms — stays
// explorable without the desktop shell.

import type {
	Asset,
	AssetAnalysis,
	AssetMetadata,
	Clip,
	Color,
	EditSource,
	ExportOptions,
	Revision,
	StreamKind,
	Task,
	Timeline,
	Track,
	Transform,
	Transition
} from './types';
import { clipDuration, DEFAULT_COLOR, DEFAULT_TRANSFORM } from './types';

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
				{ id: 'c1', asset_id: sampleAssets[0].id, source_in: 0, source_out: 12.5, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 },
				{ id: 'c2', asset_id: sampleAssets[1].id, source_in: 0, source_out: 8, timeline_start: 12.5, volume: 1, fade_in: 0.5, fade_out: 0.5 }
			]
		},
		{
			id: 'a1',
			kind: 'audio',
			name: 'A1',
			clips: [{ id: 'c3', asset_id: sampleAssets[0].id, source_in: 0, source_out: 120, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 }]
		}
	]
};

// A representative queue spanning the task lifecycle, mirroring the Rust seed.
const now = () => new Date().toISOString();
let devTasks: Task[] = [
	{
		id: 't1',
		prompt: 'Assemble a rough cut from the interview',
		status: 'done',
		result: 'Kept 6 segments; cut 2 fillers and 14 silences (−1:48)',
		created_at: now(),
		updated_at: now()
	},
	{
		id: 't2',
		prompt: 'Tighten the intro and remove filler words',
		status: 'ready',
		result: 'Staged 3 cuts; review on the timeline',
		created_at: now(),
		updated_at: now()
	},
	{
		id: 't3',
		prompt: 'Balance the voiceover levels against the music bed',
		status: 'queued',
		result: null,
		created_at: now(),
		updated_at: now()
	}
];

// ---- local timeline ops (browser dev fallback) ----------------------------

let devTimeline: Timeline = structuredClone(sampleTimeline);
const uid = () => (crypto.randomUUID ? crypto.randomUUID() : `id-${Math.random().toString(36).slice(2)}`);
const snapshot = () => structuredClone(devTimeline);

// ---- local edit history (browser dev fallback) ----------------------------
// Mirrors kerf-core's snapshot history so undo/redo/revert work without Tauri.

type DevRevision = { seq: number; label: string; source: EditSource; snapshot: Timeline };
let devHistory: DevRevision[] = [
	{ seq: 0, label: 'Initial state', source: 'system', snapshot: structuredClone(sampleTimeline) }
];
let devHead = 0;

/** Append a snapshot of the current dev timeline, dropping any redo branch. */
function recordDev(label: string) {
	devHistory = devHistory.slice(0, devHead + 1);
	devHead += 1;
	devHistory.push({ seq: devHead, label, source: 'user', snapshot: snapshot() });
}

function devRestore(seq: number): Timeline {
	const rev = devHistory.find((r) => r.seq === seq);
	if (rev) {
		devTimeline = structuredClone(rev.snapshot);
		devHead = seq;
	}
	return snapshot();
}

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

/** Open a native (multi-select) file picker and return the chosen media paths. */
export async function pickMediaPaths(): Promise<string[]> {
	if (!inTauri()) return [];
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: true,
		filters: [{ name: 'Media', extensions: ['mp4', 'mov', 'mkv', 'webm', 'wav', 'mp3', 'm4a', 'aac'] }]
	});
	if (selected == null) return [];
	return Array.isArray(selected) ? selected : [selected];
}

// ---- project file (open / save) --------------------------------------------

/** Path of the `.kerf` file backing the open project, or `null` if unsaved. */
export async function projectPath(): Promise<string | null> {
	if (!inTauri()) return null;
	return (await invoke<string | null>('project_path')) ?? null;
}

/** Discard the open project for a fresh, empty one; `false` outside Tauri. */
export async function newProject(): Promise<boolean> {
	if (!inTauri()) return false;
	await invoke('new_project');
	return true;
}

/** Pick a `.kerf` file and open it; resolves to its path, or `null` if cancelled. */
export async function openProject(): Promise<string | null> {
	if (!inTauri()) return null;
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: false,
		filters: [{ name: 'Kerf project', extensions: ['kerf'] }]
	});
	if (typeof selected !== 'string') return null;
	return (await invoke<string | null>('open_project', { path: selected })) ?? null;
}

/** Save the project to a chosen `.kerf` file and switch to it; `null` if cancelled. */
export async function saveProjectAs(defaultPath?: string): Promise<string | null> {
	if (!inTauri()) return null;
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({
		filters: [{ name: 'Kerf project', extensions: ['kerf'] }],
		defaultPath: defaultPath ?? 'untitled.kerf'
	});
	if (typeof path !== 'string') return null;
	return (await invoke<string | null>('save_project_as', { path })) ?? null;
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
		track.clips.push({ id: uid(), asset_id: assetId, source_in: start, source_out: end, timeline_start: trackEnd(track), volume: 1, fade_in: 0, fade_out: 0 });
		recordDev('Add clip');
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
		track.clips.push({ id: uid(), asset_id: assetId, source_in: sourceIn, source_out: sourceOut, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0 });
		recordDev('Add clip');
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
				const mag = Math.max(Math.abs(clip.speed ?? 1), 0.01);
				const offset = (at - clip.timeline_start) * mag;
				const right: Clip = { ...clip, id: uid(), timeline_start: at, transition_in: null };
				if ((clip.speed ?? 1) < 0) {
					const splitSrc = clip.source_out - offset;
					right.source_out = splitSrc;
					clip.source_in = splitSrc;
				} else {
					const splitSrc = clip.source_in + offset;
					right.source_in = splitSrc;
					clip.source_out = splitSrc;
				}
				track.clips.splice(ci + 1, 0, right);
			}
		}
		recordDev('Split clip');
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
		recordDev('Trim clip');
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
		recordDev('Reorder clip');
		return snapshot();
	}
	return invoke<Timeline>('reorder_clip', { trackId, clipId, newIndex });
}

export async function removeClip(clipId: string): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips.splice(found[1], 1);
		recordDev('Remove clip');
		return snapshot();
	}
	return invoke<Timeline>('remove_clip', { clipId });
}

/** Move a clip to a new timeline position, optionally onto another same-kind track. */
export async function moveClip(clipId: string, timelineStart: number, trackId?: string): Promise<Timeline> {
	if (!inTauri()) {
		const start = Math.max(0, timelineStart);
		const found = locate(devTimeline, clipId);
		if (found) {
			const [srcTrack, ci] = found;
			const destTrack = (trackId && devTimeline.tracks.find((t) => t.id === trackId)) || srcTrack;
			if (destTrack.kind !== srcTrack.kind)
				throw new Error('cannot move a clip to a track of a different kind');
			const clip = srcTrack.clips[ci];
			const end = start + clipDuration(clip);
			const overlaps = destTrack.clips.some(
				(c) => c.id !== clipId && start < c.timeline_start + clipDuration(c) && c.timeline_start < end
			);
			if (overlaps) throw new Error('clip would overlap another clip on the destination track');
			srcTrack.clips.splice(ci, 1);
			clip.timeline_start = start;
			destTrack.clips.push(clip);
			destTrack.clips.sort((a, b) => a.timeline_start - b.timeline_start);
			recordDev('Move clip');
		}
		return snapshot();
	}
	return invoke<Timeline>('move_clip', { clipId, timelineStart, trackId });
}

/** Remove a clip and close the gap (later clips on its track shift left). */
export async function rippleDelete(clipId: string): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const [track, ci] = found;
			const removed = track.clips[ci];
			const dur = clipDuration(removed);
			const from = removed.timeline_start;
			track.clips.splice(ci, 1);
			for (const c of track.clips) if (c.timeline_start >= from) c.timeline_start = Math.max(0, c.timeline_start - dur);
			recordDev('Ripple delete');
		}
		return snapshot();
	}
	return invoke<Timeline>('ripple_delete', { clipId });
}

/** Append a new empty track (video tracks above audio); auto-named when omitted. */
export async function addTrack(kind: StreamKind, name?: string): Promise<Timeline> {
	if (!inTauri()) {
		const count = devTimeline.tracks.filter((t) => t.kind === kind).length;
		const trackName = name ?? `${kind === 'audio' ? 'A' : 'V'}${count + 1}`;
		const track: Track = { id: uid(), kind, name: trackName, clips: [] };
		let at = devTimeline.tracks.length;
		if (kind !== 'audio') {
			let lastV = -1;
			devTimeline.tracks.forEach((t, i) => {
				if (t.kind === 'video') lastV = i;
			});
			at = lastV + 1;
		}
		devTimeline.tracks.splice(at, 0, track);
		recordDev('Add track');
		return snapshot();
	}
	return invoke<Timeline>('add_track', { kind, name });
}

/** Remove a track and all its clips; refuses to remove the last track. */
export async function removeTrack(trackId: string): Promise<Timeline> {
	if (!inTauri()) {
		if (devTimeline.tracks.length > 1) devTimeline.tracks = devTimeline.tracks.filter((t) => t.id !== trackId);
		recordDev('Remove track');
		return snapshot();
	}
	return invoke<Timeline>('remove_track', { trackId });
}

export async function setVolume(clipId: string, volume: number): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].volume = volume;
		recordDev('Set volume');
		return snapshot();
	}
	return invoke<Timeline>('set_volume', { clipId, volume });
}

export async function setFade(clipId: string, fadeIn?: number, fadeOut?: number): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const clip = found[0].clips[found[1]];
			if (fadeIn != null) clip.fade_in = fadeIn;
			if (fadeOut != null) clip.fade_out = fadeOut;
		}
		recordDev('Set fade');
		return snapshot();
	}
	return invoke<Timeline>('set_fade', { clipId, fadeIn, fadeOut });
}

/** Set a clip's playback speed (1.0 = normal, negative = reverse). */
export async function setSpeed(clipId: string, speed: number): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].speed = speed;
		recordDev('Set speed');
		return snapshot();
	}
	return invoke<Timeline>('set_speed', { clipId, speed });
}

/** Update a clip's geometric transform; only the provided fields change. */
export async function setTransform(clipId: string, patch: Partial<Transform>): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const clip = found[0].clips[found[1]];
			const next: Transform = { ...DEFAULT_TRANSFORM, ...(clip.transform ?? {}) };
			for (const k of Object.keys(patch) as (keyof Transform)[]) {
				const v = patch[k];
				if (v !== undefined) next[k] = v;
			}
			clip.transform = next;
		}
		recordDev('Set transform');
		return snapshot();
	}
	return invoke<Timeline>('set_transform', {
		clipId,
		scale: patch.scale,
		posX: patch.pos_x,
		posY: patch.pos_y,
		rotation: patch.rotation,
		opacity: patch.opacity,
		cropLeft: patch.crop_left,
		cropRight: patch.crop_right,
		cropTop: patch.crop_top,
		cropBottom: patch.crop_bottom
	});
}

/** Update a clip's color correction; only the provided fields change. */
export async function setColor(clipId: string, patch: Partial<Color>): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const clip = found[0].clips[found[1]];
			const next: Color = { ...DEFAULT_COLOR, ...(clip.color ?? {}) };
			for (const k of Object.keys(patch) as (keyof Color)[]) {
				const v = patch[k];
				if (v !== undefined) next[k] = v;
			}
			clip.color = next;
		}
		recordDev('Set color');
		return snapshot();
	}
	return invoke<Timeline>('set_color', {
		clipId,
		brightness: patch.brightness,
		contrast: patch.contrast,
		saturation: patch.saturation,
		gamma: patch.gamma
	});
}

/** Set or clear (`null`) the transition blending a clip's start with the prior clip. */
export async function setTransition(clipId: string, transition: Transition | null): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].transition_in = transition;
		recordDev('Set transition');
		return snapshot();
	}
	return invoke<Timeline>('set_transition', {
		clipId,
		kind: transition?.kind,
		duration: transition?.duration
	});
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
			track.clips.push({ id: uid(), asset_id: assetId, source_in: si, source_out: so, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0 });
			start += so - si;
		}
		recordDev('Remove silence');
		return snapshot();
	}
	return invoke<Timeline>('remove_silence', { assetId });
}

export async function extractAudio(assetId: string): Promise<Timeline> {
	if (!inTauri()) {
		const asset = assetById(assetId);
		const track = devTimeline.tracks.find((t) => t.kind === 'audio') ?? devTimeline.tracks[0];
		if (asset) track.clips.push({ id: uid(), asset_id: assetId, source_in: 0, source_out: asset.duration, timeline_start: trackEnd(track), volume: 1, fade_in: 0, fade_out: 0 });
		recordDev('Extract audio');
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

// ---- history (undo / redo / revert) ----------------------------------------

export async function getHistory(): Promise<Revision[]> {
	if (!inTauri()) {
		return devHistory.map((r) => ({
			seq: r.seq,
			label: r.label,
			source: r.source,
			created_at: new Date().toISOString(),
			current: r.seq === devHead
		}));
	}
	return invoke<Revision[]>('get_history');
}

export async function undo(): Promise<Timeline> {
	if (!inTauri()) {
		const prev = [...devHistory].reverse().find((r) => r.seq < devHead);
		return devRestore(prev ? prev.seq : devHead);
	}
	return invoke<Timeline>('undo');
}

export async function redo(): Promise<Timeline> {
	if (!inTauri()) {
		const next = devHistory.find((r) => r.seq > devHead);
		return devRestore(next ? next.seq : devHead);
	}
	return invoke<Timeline>('redo');
}

export async function revertTo(seq: number): Promise<Timeline> {
	if (!inTauri()) return devRestore(seq);
	return invoke<Timeline>('revert_to', { seq });
}

// ---- agent task queue ------------------------------------------------------
//
// The desktop app persists tasks in kerf-core; a connected LLM claims and works
// them over MCP. In the browser there is no agent, so queued tasks simply wait —
// which is the honest behavior: Kerf never edits on its own.

export async function listTasks(): Promise<Task[]> {
	if (!inTauri()) return structuredClone(devTasks);
	return invoke<Task[]>('list_tasks');
}

/** Enqueue a task; resolves to the newly created (queued) task. */
export async function addTask(prompt: string): Promise<Task> {
	if (!inTauri()) {
		const ts = now();
		const task: Task = { id: uid(), prompt, status: 'queued', result: null, created_at: ts, updated_at: ts };
		devTasks = [...devTasks, task];
		return structuredClone(task);
	}
	return invoke<Task>('add_task', { prompt });
}

/** Accept a staged edit (status → done); resolves to the refreshed queue. */
export async function resolveTask(taskId: string): Promise<Task[]> {
	if (!inTauri()) {
		devTasks = devTasks.map((t) => (t.id === taskId ? { ...t, status: 'done', updated_at: now() } : t));
		return structuredClone(devTasks);
	}
	return invoke<Task[]>('resolve_task', { taskId });
}

/** Remove a task from the queue; resolves to the refreshed queue. */
export async function removeTask(taskId: string): Promise<Task[]> {
	if (!inTauri()) {
		devTasks = devTasks.filter((t) => t.id !== taskId);
		return structuredClone(devTasks);
	}
	return invoke<Task[]>('remove_task', { taskId });
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

export async function exportTimeline(outputPath: string, options: ExportOptions): Promise<string> {
	if (!inTauri()) throw new Error('export is only available in the desktop app');
	return invoke<string>('export_timeline', { outputPath, options });
}

/** Open a save dialog defaulted to the given container extension. */
export async function pickExportPath(ext: string): Promise<string | null> {
	if (!inTauri()) return null;
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({
		filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
		defaultPath: `kerf-export.${ext}`
	});
	return typeof path === 'string' ? path : null;
}

// ---- diagnostics (logs) ----------------------------------------------------

/** The platform log directory Kerf writes its logfile to, or `null` in the browser. */
export async function logDir(): Promise<string | null> {
	if (!inTauri()) return null;
	return (await invoke<string>('log_dir')) ?? null;
}

/** Open the log directory in the OS file manager so the user can attach the logfile. */
export async function revealLogs(): Promise<void> {
	if (!inTauri()) return;
	await invoke('reveal_logs');
}
