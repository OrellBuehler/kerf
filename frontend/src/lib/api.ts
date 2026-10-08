// Bridge to the Tauri backend (kerf-core via kerf-app commands).
//
// When running outside Tauri (e.g. `bun run dev` in a browser for design work)
// the calls fall back to seeded sample data and a local in-memory timeline, so
// the whole editor — including edits, analysis, and waveforms — stays
// explorable without the desktop shell.

import type {
	AudioDetached,
	Asset,
	AssetAnalysis,
	AssetMetadata,
	AudioEffect,
	CaptionFormat,
	CaptionImportRequest,
	CaptionImportResult,
	CaptionOptions,
	Clip,
	ClipCut,
	ClipMove,
	Mask,
	Color,
	Delivery,
	DeliveryCheck,
	EditSource,
	ExportOptions,
	ExportProgress,
	Filmstrip,
	ImportProgress,
	Easing,
	Keyframe,
	Property,
	PropertyKey,
	LaunchRequest,
	Levels,
	Projection,
	Reframe,
	ReframeKeyframe,
	Revision,
	AppSettings,
	SettingsView,
	SplitSide,
	StagedEdit,
	StreamKind,
	Task,
	TextKeyframe,
	TextOverlay,
	Timeline,
	TimelineDiff,
	Track,
	Transform,
	Transition,
	TranscriptionStatus,
	TranscriptSegment,
	UpdateInfo,
	VideoEffect,
	VoiceoverProgress,
	VoiceoverRequest,
	VoiceoverResult,
	VoiceoverStatus,
	WaveformRange
} from './types';
import { clipDuration, DEFAULT_COLOR, DEFAULT_REFRAME, DEFAULT_TRANSFORM } from './types';
import { alignCutsToBeats, beatGrid, defaultBeatTolerance } from './beats';
import { fileTooLarge, importCaptionsInto, MAX_CAPTION_FILE_BYTES, parseFormat, resolveBase } from './caption-import';
import { baseName, CAPTION_EXTENSIONS } from './caption-import-ui';
import { easingProblem, insertKeyframe } from './easing';
import {
	channelOf,
	clearTransformAnimation,
	insertPropertyKey,
	pruneChannels,
	propertyKeysProblem,
	propertyKeysShifted,
	setPropertyEasing,
	setPropertyKeys,
	transformAt
} from './channels';
import { formatTime as fmtTime } from './diff';
import {
	rollEdit as rollEditLocal,
	rollEditLinked as rollEditLinkedLocal,
	slideClip as slideClipLocal,
	slideClipLinked as slideClipLinkedLocal,
	slipClip as slipClipLocal,
	slipClipLinked as slipClipLinkedLocal,
	type SourceLimits
} from './edit-modes';
import { linkPartners, withLinkPartners } from './link-groups';
import * as ops from './link-ops';
import { runEdit } from './link-ops';
import { sourceLimits } from './trim-tools';
import { clampCeiling, clampMasterVolume, DEFAULT_MASTER, estimateLevels, masterOf } from './levels';
import { checkAll } from './platforms';
import { centeredCrop } from './smart-crop';
import { synthWaveformRange } from './sample-waveform';
import { sampleFilmstrip } from './sample-filmstrip';
import { synthPcm } from './sample-audio';
import { sampleFrameUrl } from './sample-frame';
import { captionsForTimeline, resolveCaptions } from './captions';
import { describeError, logFrontend } from './log';
import { parseLaunchRequest } from './launch';
import { VOICE_IDS, DEFAULT_SPEED, DEFAULT_VOICE, clampSpeed, estimateSeconds, scriptSegments, voiceInfo } from './voiceover';

export type { AudioDetached };

export function inTauri(): boolean {
	return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

/** Whether `v` holds a NaN / Infinity anywhere. JSON turns those into `null`,
 *  which the backend then rejects with a deserialize error that says nothing
 *  about which field — and an emptied `<input type=number>` is a NaN. */
function hasNonFinite(v: unknown, depth = 0): boolean {
	if (typeof v === 'number') return !Number.isFinite(v);
	if (depth > 6 || v === null || typeof v !== 'object' || ArrayBuffer.isView(v)) return false;
	return Object.values(v).some((x) => hasNonFinite(x, depth + 1));
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	if (hasNonFinite(args)) throw new Error('That value is not a number.');
	const { invoke } = await import('@tauri-apps/api/core');
	try {
		return await invoke<T>(cmd, args);
	} catch (e) {
		const message = describeError(e);
		logFrontend(/cancel/i.test(message) ? 'info' : 'error', `${cmd} failed: ${message}`, `command ${cmd}`);
		throw e;
	}
}

// ---- sample fallback (browser dev) ----------------------------------------

// No real system font enumeration is available outside Tauri; a small static
// list keeps the font picker non-empty in the browser dev harness.
const DEV_FONTS = ['Arial', 'Georgia', 'Helvetica', 'Times New Roman', 'Verdana'];

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
		],
		loudness: { integrated_lufs: -16.2, loudness_range: 6.4, true_peak_dbtp: -1.5, threshold_lufs: -26.5 },
		onsets: [0.5, 1.2, 2.0, 2.8, 3.6, 5.6],
		tempo: { bpm: 120, beats: [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0], confidence: 0.62 },
		audio_class: { class: 'speech', confidence: 0.71 }
	},
	[sampleAssets[1].id]: {
		asset_id: sampleAssets[1].id,
		silence_segments: [],
		scene_changes: [0, 8, 20, 33],
		transcript: [],
		loudness: { integrated_lufs: -11.8, loudness_range: 9.1, true_peak_dbtp: -0.8, threshold_lufs: -22.0 },
		onsets: [0.4, 0.9, 1.5, 2.1, 2.7, 3.3, 3.9],
		tempo: { bpm: 128, beats: [0.23, 0.7, 1.17, 1.64, 2.11, 2.58, 3.05, 3.52], confidence: 0.78 },
		audio_class: { class: 'music', confidence: 0.83 }
	}
};

// The starter cut is the *detached-and-linked* shape linked A/V produces: the interview's
// sound is its own clip on A1, linked to the picture on V1, whose own sound is muted
// (`source_audio: false`) — so it is heard once, and a move, trim, split or delete of
// either carries to both. (Before detaching existed this was the interview's whole audio
// on A1 *under* a picture still playing the same sound: heard twice.) The Rust seed
// (`Project::sample`) detaches the same way and then unlinks, because the kerf-core tests
// built on it edit one clip at a time; the harness is for exploring, so it keeps the link.
const SAMPLE_LINK = 'sample-link-interview';
const sampleTimeline: Timeline = {
	tracks: [
		{
			id: 'v1',
			kind: 'video',
			name: 'V1',
			clips: [
				{
					id: 'c1',
					asset_id: sampleAssets[0].id,
					source_in: 0,
					source_out: 12.5,
					timeline_start: 0,
					volume: 1,
					fade_in: 0,
					fade_out: 0,
					source_audio: false,
					link_id: SAMPLE_LINK
				},
				{ id: 'c2', asset_id: sampleAssets[1].id, source_in: 0, source_out: 8, timeline_start: 12.5, volume: 1, fade_in: 0.5, fade_out: 0.5 }
			]
		},
		{
			id: 'a1',
			kind: 'audio',
			name: 'A1',
			clips: [
				{
					id: 'c3',
					asset_id: sampleAssets[0].id,
					source_in: 0,
					source_out: 12.5,
					timeline_start: 0,
					volume: 1,
					fade_in: 0,
					fade_out: 0,
					link_id: SAMPLE_LINK
				}
			]
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

// ---- ripple mode (browser dev fallback) -----------------------------------
// The project's ripple flag lives in the harness's project state, as it lives in
// the `.kerf` file's meta. Every harness edit that can change how much footage
// sits ahead of a clip runs through `devEdit`, which is `Project::edit_timeline`
// in miniature: with ripple on it snapshots the timeline, runs the edit and
// stores `rippleFrom(after, before)` — the faithful mirror in `ripple.ts`.
// Edits that decide their own layout (move, reorder, ripple delete, cut range,
// the beat snap, paste) never go through it, exactly as in the core.

let devRippleMode = false;

/** Run one harness edit with ripple applied after it when it is on: the project's
 *  flag, or `ripple` for this call (`Project::with_ripple`). `run` mutates `devTimeline`;
 *  it is `Project::run_edit` in miniature (`runEdit` in `link-ops.ts`): the sync lock and
 *  guard when links are in force, and a throwing edit has changed nothing. */
function devEdit<R>(ripple: boolean | undefined, run: () => R, link?: boolean): R {
	const home = devTimeline;
	const done = runEdit(home, { ripple: ripple ?? devRippleMode, links: devLinks(link) }, (scratch) => {
		devTimeline = scratch;
		try {
			return run();
		} finally {
			devTimeline = home;
		}
	});
	devTimeline = done.timeline;
	return done.result;
}

// ---- linked clips (browser dev fallback) ------------------------------------
// Links are a property of the clips (`link_id`), so there is no project switch — only
// the per-call escape hatch, `link: false` (`Project::with_links`). The link-aware ops are
// the faithful mirror in `link-ops.ts`, which does what the backend's edit closure does:
// work on a scratch copy, so a refusal part-way (a locked partner, a lane the sync lock
// cannot lay out) leaves the timeline exactly as it was, then settle ripple, the sync lock
// and the guard.

/** `Project::links_active`: links are in force unless a call says `link: false`. */
const devLinks = (link: boolean | undefined): boolean => link !== false;

/** What each harness asset's footage reaches — `Project::source_limits`: its duration,
 *  or `Infinity` for a still (it loops). */
function devLimits(): SourceLimits {
	return sourceLimits(sampleAssets);
}

/** What a harness call is made under (`link-ops.ts`'s `EditEnv`). `ripple` forces ripple on or
 *  off for the call (`undefined` follows the project's flag); `link` is the per-call escape hatch. */
function devEnv(ripple: boolean | undefined, link: boolean | undefined): ops.EditEnv {
	return {
		ripple: ripple ?? devRippleMode,
		links: devLinks(link),
		footage: devLimits(),
		hasAudio: (assetId) => !!assetById(assetId)?.streams.some((s) => s.kind === 'audio')
	};
}

/** Run one link-op against the harness timeline: store what it leaves, record its label. */
function devRun<R>(op: (timeline: Timeline) => ops.Edited<R>): R {
	const done = op(devTimeline);
	devTimeline = done.timeline;
	recordDev(done.label);
	return done.result;
}

// ---- read ------------------------------------------------------------------

export async function listAssets(): Promise<Asset[]> {
	if (!inTauri()) return structuredClone(sampleAssets);
	return invoke<Asset[]>('list_assets');
}

/** Distinct system font family names for the text overlay font picker. */
export async function listFonts(): Promise<string[]> {
	if (!inTauri()) return DEV_FONTS;
	return invoke<string[]>('list_fonts');
}

/** GPU video encoders the backend verified usable (empty in the browser). */
export async function hwEncoders(): Promise<string[]> {
	if (!inTauri()) return [];
	return invoke<string[]>('hw_encoders');
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
/** The file types Kerf imports. Shared by the picker's filter and the
 *  drag-and-drop handler, so dropping a file onto the window accepts exactly
 *  what browsing for one does. */
export const MEDIA_EXTENSIONS = [
	'mp4', 'mov', 'mkv', 'webm', 'wav', 'mp3', 'm4a', 'aac',
	'png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'tiff', 'tif',
	// Insta360 captures (360 video / photo) — MP4 under a custom extension.
	'insv', 'insp'
];

/** Whether a path looks like media Kerf can import. */
export function isMediaPath(path: string): boolean {
	const ext = path.split('.').pop()?.toLowerCase() ?? '';
	return MEDIA_EXTENSIONS.includes(ext);
}

export async function pickMediaPaths(): Promise<string[]> {
	if (!inTauri()) return [];
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: true,
		filters: [{ name: 'Media', extensions: MEDIA_EXTENSIONS }]
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

/** Ask a yes/no question before something irreversible. `window.confirm` is
 *  replaced by the dialog plugin in the desktop app and returns a promise
 *  there, so it can't be used synchronously — hence this. */
export async function confirmAction(message: string, title = 'Kerf'): Promise<boolean> {
	if (!inTauri()) return window.confirm(message);
	const { ask } = await import('@tauri-apps/plugin-dialog');
	return ask(message, { title, kind: 'warning' });
}

/** When the window is asked to close and `needsConfirm()` says there is
 *  something to lose, hold it open and close only if `confirm` resolves true.
 *  Otherwise the close goes through untouched. Returns an unlisten fn. */
export async function onWindowCloseRequested(
	needsConfirm: () => boolean,
	confirm: () => Promise<boolean>
): Promise<() => void> {
	if (!inTauri()) return () => {};
	const { getCurrentWindow } = await import('@tauri-apps/api/window');
	const win = getCurrentWindow();
	return win.onCloseRequested(async (e) => {
		if (!needsConfirm()) return;
		e.preventDefault();
		let close = false;
		try {
			close = await confirm();
		} catch {
			// A dialog that fails to show must not make the window unclosable.
			close = true;
		}
		if (close) await win.destroy();
	});
}

/** Show the main window. The desktop app creates it hidden (`visible: false`) so
 *  the webview's unthemed first frame is never seen; the page calls this once the
 *  theme is applied and the first frame has painted (`reveal.ts`). Rust shows the
 *  window itself after a few seconds if this never comes, and ignores a repeat.
 *  A no-op in the browser harness, which has no native window. */
export async function showMainWindow(): Promise<void> {
	if (!inTauri()) return;
	await invoke('show_main_window');
}

/** What this launch was started asking to open (`kerf path/to/cut.kerf`) — a
 *  `.kerf` that exists, or one that was named and is not there — handed over once;
 *  `null` when there was nothing or it was already taken. A second launch's request
 *  arrives as an event instead (`open-project-file` / `launch-project-missing`),
 *  unless it came while this page was still starting, in which case it is waiting
 *  here too, newest first. */
export async function takeLaunchProject(): Promise<LaunchRequest | null> {
	if (!inTauri()) return null;
	return parseLaunchRequest(await invoke<unknown>('take_launch_project'));
}

/** Discard the open project for a fresh, empty one; `false` outside Tauri. */
export async function newProject(): Promise<boolean> {
	if (!inTauri()) return false;
	await invoke('new_project');
	return true;
}

/** Open a `.kerf` file — the one at `path` when given (a launch hands one over),
 *  else one picked natively; resolves to its path, or `null` if
 *  cancelled. */
export async function openProject(path?: string): Promise<string | null> {
	if (!inTauri()) return null;
	let selected: string | string[] | null = path ?? null;
	if (selected === null) {
		const { open } = await import('@tauri-apps/plugin-dialog');
		selected = await open({
			multiple: false,
			filters: [{ name: 'Kerf project', extensions: ['kerf'] }]
		});
	}
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

/** Pick a theme file and read it; `null` if cancelled. In the browser the
 *  file comes through an `<input type=file>` instead of the native picker. */
export async function importThemeFile(): Promise<string | null> {
	if (!inTauri()) {
		return new Promise((resolve) => {
			const input = document.createElement('input');
			input.type = 'file';
			input.accept = '.json,application/json';
			input.onchange = () => {
				const f = input.files?.[0];
				if (!f) return resolve(null);
				f.text().then(resolve, () => resolve(null));
			};
			input.oncancel = () => resolve(null);
			input.click();
		});
	}
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({ multiple: false, filters: [{ name: 'Kerf theme', extensions: ['json'] }] });
	if (typeof selected !== 'string') return null;
	return invoke<string>('read_text_file', { path: selected });
}

/** Save a theme as a JSON file; resolves to where it went, `null` if
 *  cancelled. The browser harness hands the file to the download manager. */
export async function exportThemeFile(contents: string, defaultName: string): Promise<string | null> {
	if (!inTauri()) {
		const url = URL.createObjectURL(new Blob([contents], { type: 'application/json' }));
		const a = document.createElement('a');
		a.href = url;
		a.download = defaultName;
		a.click();
		setTimeout(() => URL.revokeObjectURL(url), 1000);
		return defaultName;
	}
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({ filters: [{ name: 'Kerf theme', extensions: ['json'] }], defaultPath: defaultName });
	if (typeof path !== 'string') return null;
	return invoke<string>('write_text_file', { path, contents });
}

export async function analyzeAsset(assetId: string): Promise<AssetAnalysis> {
	if (!inTauri()) {
		await new Promise((r) => setTimeout(r, 900));
		return (
			sampleAnalysis[assetId] ?? {
				asset_id: assetId,
				silence_segments: [],
				scene_changes: [],
				transcript: [],
				loudness: null,
				onsets: [],
				tempo: null,
				audio_class: null
			}
		);
	}
	return invoke<AssetAnalysis>('analyze_asset', { assetId });
}

// ---- playback --------------------------------------------------------------

/** A composited frame pushed up during playback. */
export interface PlaybackFrame {
	/** Timeline time this frame shows, for dropping ones the clock has passed. */
	time: number;
	/** `data:image/jpeg;base64,…` — the same shape `getTimelineFrame` returns. */
	jpeg: string;
}

/**
 * Play the timeline from `start`, invoking `onFrame` with each composited frame
 * until playback stops. Returns a function that stops it.
 *
 * One long-lived ffmpeg renders the whole span, rather than the one process per
 * frame that scrubbing uses — the difference between a slideshow and video. In
 * the browser harness there is no backend, so frames are synthesized instead
 * (see `samplePlayback`).
 */
let playbackSeq = 0;

export function startPlayback(
	start: number,
	fps: number,
	onFrame: (f: PlaybackFrame) => void,
	onError?: (message: string) => void
): () => void {
	if (!inTauri()) return samplePlayback(start, fps, onFrame);
	// The backend cancels *by id* rather than by a generation counter: start and
	// stop are separate async calls that can arrive out of order, and a stop
	// meant for the previous stream must not kill the one that replaced it.
	const playbackId = ++playbackSeq;
	let stopped = false;
	void (async () => {
		const { Channel } = await import('@tauri-apps/api/core');
		if (stopped) return;
		const channel = new Channel<PlaybackFrame>();
		channel.onmessage = (f) => {
			if (!stopped) onFrame(f);
		};
		// Resolves only when playback ends; nothing waits on it.
		// The backend only rejects for a real failure (ffmpeg died, inputs
		// would not resolve), never for a stop or a supersede — but a stream
		// already stopped on this side has no one left to tell.
		void invoke('start_playback', { playbackId, start, fps, onFrame: channel }).catch((e) => {
			if (!stopped) onError?.(e instanceof Error ? e.message : String(e));
		});
	})();
	return () => {
		if (stopped) return;
		stopped = true;
		void invoke('stop_playback', { playbackId }).catch((e) => console.warn('stop_playback failed', e));
	};
}

/** What the desktop app really pays to put one frame on screen: ffmpeg's startup
 *  plus base64, JSON and IPC. Deliberately larger than the preview's two-frame
 *  staleness budget — a harness whose frames arrive instantly cannot reproduce
 *  the class of bug where that constant is mistaken for the picture drifting. */
const SAMPLE_FRAME_LAG_MS = 90;

/**
 * The browser harness's stand-in for the streamed composite: a generated frame
 * per tick, paced at `fps` and delayed by a realistic transport cost, so
 * playback moves in `bun run dev` and the preview's frame pacing is exercised
 * end to end without a backend.
 */
function samplePlayback(start: number, fps: number, onFrame: (f: PlaybackFrame) => void): () => void {
	let index = 0;
	let timer: ReturnType<typeof setInterval> | null = null;
	const spawn = setTimeout(() => {
		timer = setInterval(() => {
			const time = start + index++ / fps;
			onFrame({ time, jpeg: sampleFrameUrl(time) });
		}, 1000 / fps);
	}, SAMPLE_FRAME_LAG_MS);
	return () => {
		clearTimeout(spawn);
		if (timer) clearInterval(timer);
	};
}

// ---- speech-to-text --------------------------------------------------------

/** Which transcription backend is available, and whether its model is cached. */
export async function transcriptionStatus(): Promise<TranscriptionStatus> {
	if (!inTauri()) {
		// The browser harness has no backend at all; say so plainly rather than
		// implying a download would help.
		return {
			backend: 'none',
			available: false,
			enabled: readBrowserTranscribe(),
			model: null,
			model_path: null,
			model_ready: false,
			approx_download_bytes: null,
			models: [],
			reason: 'transcription runs in the desktop app'
		};
	}
	return invoke<TranscriptionStatus>('transcription_status');
}

/** Choose which speech model transcription uses; remembered in the project. */
export async function setSpeechModel(name: string | null): Promise<TranscriptionStatus> {
	if (!inTauri()) return transcriptionStatus();
	return invoke<TranscriptionStatus>('set_speech_model', { name });
}

/** Fetch a speech model into Kerf's cache, streaming `model-progress`. */
export async function downloadSpeechModel(name: string): Promise<string> {
	if (!inTauri()) throw new Error('speech models are only available in the desktop app');
	return invoke<string>('download_speech_model', { name });
}

// ---- voiceover (text-to-speech) ---------------------------------------------

// The browser harness has no synthesizer, so it fakes one: a few progress ticks,
// then an audio-only asset timed from the script. What stays real is everything
// the editor does with the result — the VO track, the transcript, the captions.
const devVoiceoverListeners = new Set<(p: VoiceoverProgress) => void>();
const devVoices = new Set<string>();
let devVoiceoverReady = false;
let devVoiceoverCancelled = false;
const DEV_VOICEOVER_BYTES = 104857600;

function devVoiceoverStatus(): VoiceoverStatus {
	return {
		available: true,
		ready: devVoiceoverReady,
		approx_download_bytes: DEV_VOICEOVER_BYTES,
		default_voice: DEFAULT_VOICE,
		voices: VOICE_IDS.map((id) => voiceInfo(id, devVoices.has(id))),
		reason: null
	};
}

function devVoiceoverEmit(p: VoiceoverProgress) {
	for (const cb of devVoiceoverListeners) cb(p);
}

/** One stage's worth of synthetic ticks; stops with the real error on a cancel. */
async function devVoiceoverStage(stage: VoiceoverProgress['stage'], steps: number, detail: (i: number) => string) {
	for (let i = 1; i <= steps; i++) {
		await new Promise((r) => setTimeout(r, 220));
		if (devVoiceoverCancelled) throw new Error('voiceover cancelled');
		devVoiceoverEmit({ stage, fraction: i / steps, detail: detail(i) });
	}
}

async function devVoiceoverDownload(voice: string) {
	if (!devVoiceoverReady) {
		await devVoiceoverStage('download_runtime', 3, (i) => `${Math.round((i / 3) * 12)} MB / 12 MB`);
		await devVoiceoverStage('download_model', 4, (i) => `${Math.round((i / 4) * 88)} MB / 88 MB`);
	}
	if (!devVoices.has(voice)) await devVoiceoverStage('download_voice', 2, (i) => `${i / 2} MB / 1 MB`);
	devVoiceoverReady = true;
	devVoices.add(voice);
}

/** Whether voiceover can run here, which voices there are and whether the first
 *  use still has to download the model. */
export async function voiceoverStatus(): Promise<VoiceoverStatus> {
	if (!inTauri()) return devVoiceoverStatus();
	return invoke<VoiceoverStatus>('voiceover_status');
}

/** Download the runtime, the model and one voice ahead of the first voiceover,
 *  streaming `voiceover-progress`. */
export async function prepareVoiceover(voice: string): Promise<VoiceoverStatus> {
	if (!inTauri()) {
		devVoiceoverCancelled = false;
		await devVoiceoverDownload(voice);
		return devVoiceoverStatus();
	}
	return invoke<VoiceoverStatus>('prepare_voiceover', { voice });
}

/** Synthesize a script, import it as an asset on the "VO" track and, when asked,
 *  caption the cut from it. Rejects with `voiceover cancelled` if stopped. */
export async function generateVoiceover(opts: VoiceoverRequest): Promise<VoiceoverResult> {
	if (!inTauri()) {
		devVoiceoverCancelled = false;
		const voice = opts.voice ?? DEFAULT_VOICE;
		const speed = clampSpeed(opts.speed ?? DEFAULT_SPEED);
		await devVoiceoverDownload(voice);
		const segments = scriptSegments(opts.text, speed);
		await devVoiceoverStage('synthesize', Math.min(segments.length, 6) || 1, (i) => {
			const done = Math.ceil((i / (Math.min(segments.length, 6) || 1)) * segments.length);
			return `${done} of ${segments.length} sentences`;
		});
		const id = uid();
		const words = opts.text.trim().split(/\s+/).slice(0, 4).join(' ');
		const asset: Asset = {
			id,
			path: `/voiceover/${id}.wav`,
			name: `Voiceover - ${words}.wav`,
			duration: estimateSeconds(opts.text, speed),
			streams: [{ index: 0, kind: 'audio', codec: 'pcm_s16le', sample_rate: 24000, channels: 1 }],
			imported_at: new Date().toISOString(),
			voiceover: { text: opts.text, voice, speed, segments }
		};
		sampleAssets.push(asset);
		sampleAnalysis[id] = {
			asset_id: id,
			silence_segments: [],
			scene_changes: [],
			transcript: segments,
			loudness: null,
			onsets: [],
			tempo: null,
			audio_class: { class: 'speech', confidence: 1 }
		};
		devEdit(undefined, () => {
			let track = opts.trackId ? devTimeline.tracks.find((t) => t.id === opts.trackId) : undefined;
			track ??= devTimeline.tracks.find((t) => t.kind === 'audio' && t.name === 'VO');
			if (!track) {
				track = { id: uid(), kind: 'audio', name: 'VO', clips: [] };
				devTimeline.tracks.push(track);
			}
			track.clips.push({
				id: uid(),
				asset_id: id,
				source_in: 0,
				source_out: asset.duration,
				timeline_start: opts.timelineStart ?? trackEnd(track),
				volume: 1,
				fade_in: 0,
				fade_out: 0
			});
		});
		recordDev('Add voiceover');
		const timeline = opts.captions ? await generateCaptions(opts.captions) : snapshot();
		return { asset: structuredClone(asset), timeline };
	}
	return invoke<VoiceoverResult>('generate_voiceover', {
		text: opts.text,
		voice: opts.voice,
		speed: opts.speed,
		trackId: opts.trackId,
		timelineStart: opts.timelineStart,
		captions: opts.captions
	});
}

/** Stop the download or synthesis in flight; it then rejects with `voiceover cancelled`. */
export async function cancelVoiceover(): Promise<void> {
	if (!inTauri()) {
		devVoiceoverCancelled = true;
		return;
	}
	return invoke<void>('cancel_voiceover');
}

/** Subscribe to `voiceover-progress` events — the GUI's own and an agent's. Returns an unlisten fn. */
export async function onVoiceoverProgress(cb: (p: VoiceoverProgress) => void): Promise<() => void> {
	if (!inTauri()) {
		devVoiceoverListeners.add(cb);
		return () => void devVoiceoverListeners.delete(cb);
	}
	const { listen } = await import('@tauri-apps/api/event');
	return listen<VoiceoverProgress>('voiceover-progress', (e) => cb(e.payload));
}

// ---- ripple mode -----------------------------------------------------------

/** Whether the project edits in ripple mode (off until someone turns it on). */
export async function getRippleMode(): Promise<boolean> {
	if (!inTauri()) return devRippleMode;
	return invoke<boolean>('get_ripple_mode');
}

/** Turn ripple mode on or off for the project; resolves to what is stored. A
 *  setting, not an edit — no revision, and the timeline does not move. */
export async function setRippleMode(on: boolean): Promise<boolean> {
	if (!inTauri()) {
		devRippleMode = on;
		return devRippleMode;
	}
	return invoke<boolean>('set_ripple_mode', { on });
}

// ---- timeline editing (each resolves to the refreshed timeline) ------------

export async function cutClip(assetId: string, start: number, end: number): Promise<Timeline> {
	if (!inTauri()) {
		devEdit(undefined, () => {
			const track = trackForAsset(devTimeline, assetId);
			track.clips.push({ id: uid(), asset_id: assetId, source_in: start, source_out: end, timeline_start: trackEnd(track), volume: 1, fade_in: 0, fade_out: 0 });
		});
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
		devEdit(undefined, () => {
			const track = (trackId && devTimeline.tracks.find((t) => t.id === trackId)) || trackForAsset(devTimeline, assetId);
			const start = timelineStart ?? trackEnd(track);
			track.clips.push({ id: uid(), asset_id: assetId, source_in: sourceIn, source_out: sourceOut, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0 });
		});
		recordDev('Add clip');
		return snapshot();
	}
	return invoke<Timeline>('add_clip', { assetId, trackId, sourceIn, sourceOut, timelineStart });
}

/** Split a clip at timeline time `at`. Linked clips (a picture and its detached sound)
 *  are split at the same moment unless `link` is `false`; the new halves link up. */
export async function splitClip(clipId: string, at: number, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.splitAt(tl, devEnv(undefined, link), clipId, at));
		return snapshot();
	}
	return invoke<Timeline>('split_clip', { clipId, at, link });
}

export async function trimClip(
	clipId: string,
	sourceIn?: number,
	sourceOut?: number,
	timelineStart?: number,
	link?: boolean
): Promise<Timeline> {
	if (!inTauri()) {
		// With ripple on the later clips follow the clip's change in length and a
		// left-edge trim keeps the clip's start, so the timeline handed back
		// carries the clip as it ended up — as the backend's does. Linked partners
		// follow the edge that moved (`carryExtentEdit`).
		devRun((tl) => ops.trim(tl, devEnv(undefined, link), clipId, sourceIn, sourceOut, timelineStart));
		return snapshot();
	}
	return invoke<Timeline>('trim_clip', { clipId, sourceIn, sourceOut, timelineStart, link });
}

export async function reorderClip(trackId: string, clipId: string, newIndex: number): Promise<Timeline> {
	if (!inTauri()) {
		// Never rippled (it re-lays the lane itself); the clips linked to what it moved follow.
		devRun((tl) => ops.reorder(tl, devEnv(false, undefined), trackId, clipId, newIndex));
		return snapshot();
	}
	return invoke<Timeline>('reorder_clip', { trackId, clipId, newIndex });
}

/** One clipboard entry: the clip's data plus the track it should land on. */
export interface Placement {
	track_id: string;
	clip: Clip;
}

export async function insertClips(placements: Placement[], at: number): Promise<Timeline> {
	if (!inTauri()) {
		// All or nothing; copies of a linked group link up, a picture pasted without its sound
		// gets its own back (`Project::insert_clips`, in `link-ops.ts`).
		devRun((tl) => ops.insertClips(tl, devEnv(false, undefined), placements, at));
		return snapshot();
	}
	return invoke<Timeline>('insert_clips', { placements, at });
}

export async function duplicateClips(clipIds: string[], at: number): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.duplicateClips(tl, devEnv(false, undefined), clipIds, at));
		return snapshot();
	}
	return invoke<Timeline>('duplicate_clips', { clipIds, at });
}

export async function removeClip(clipId: string, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		// A clip with partners is a group removal (`Project::remove`): one revision, all or nothing.
		devRun((tl) => ops.remove(tl, devEnv(undefined, link), clipId));
		return snapshot();
	}
	return invoke<Timeline>('remove_clip', { clipId, link });
}

/** Remove several clips as **one** edit — all or nothing, one history step. With
 *  `ripple` true every track closes up behind what it lost (a multi-select ripple
 *  delete, Shift+Delete); omitted, the project's ripple mode decides. Linked partners of
 *  the named clips go too, unless `link` is `false`. */
export async function removeClips(clipIds: string[], ripple?: boolean, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.removeClips(tl, devEnv(ripple, link), clipIds));
		return snapshot();
	}
	return invoke<Timeline>('remove_clips', { clipIds, ripple, link });
}

/** Move a clip to a new timeline position, optionally onto another same-kind track.
 *  Linked clips move with it by the same Δt, each on its own track, unless `link` is `false`. */
export async function moveClip(clipId: string, timelineStart: number, trackId?: string, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const start = Math.max(0, timelineStart);
		if (devLinks(link) && linkPartners(devTimeline, clipId).length > 0) {
			// A clip with partners is moved as the group it is, through the group move.
			return moveClips([{ clip_id: clipId, timeline_start: start, track_id: trackId }], link);
		}
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
	return invoke<Timeline>('move_clip', { clipId, timelineStart, trackId, link });
}

/** Move several clips as **one** edit — a marquee selection dragged together.
 *  Each move is a clip, an absolute start and optionally another same-kind track;
 *  the group is checked as a group and an illegal one changes nothing (the promise
 *  rejects). Never ripples. Linked partners of the named clips move with them (by the
 *  same Δt, on their own tracks) unless `link` is `false`. */
export async function moveClips(moves: ClipMove[], link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.moveClips(tl, devEnv(false, link), moves));
		return snapshot();
	}
	return invoke<Timeline>('move_clips', { moves, link });
}

/** Remove a clip and close the gap (later clips on its track shift left). Linked clips
 *  are removed too, each closing the gap on its own track, unless `link` is `false`. */
export async function rippleDelete(clipId: string, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.rippleDelete(tl, devEnv(false, link), clipId));
		return snapshot();
	}
	return invoke<Timeline>('ripple_delete', { clipId, link });
}

/** Cut a source-time range out of a clip (split + ripple in one edit) — the
 * transcript-editing primitive. Linked clips lose the same stretch of timeline unless
 * `link` is `false`. */
export async function cutClipRange(clipId: string, from: number, to: number, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.cutRange(tl, devEnv(false, link), clipId, from, to));
		return snapshot();
	}
	return invoke<Timeline>('cut_clip_range', { clipId, from, to, link });
}

// ---- edit modes: roll, slip, slide, split-and-remove ----------------------
// The harness runs the faithful mirror in `edit-modes.ts`; the desktop app asks the
// backend, which clamps to each clip's footage and its neighbours (and says so by
// moving less than asked). Roll, slip and slide never ripple; split-and-remove
// follows the project's ripple mode like any trim.

/** Roll the cut between two adjacent clips of one track by `delta` seconds
 *  (positive is later): `clipA` (the earlier clip) loses or gains at its end what
 *  `clipB` gains or loses at its start, so the pair keeps its span. Clamped to each
 *  clip's footage and a 0.05 s floor; rejects when the clips are not adjacent or
 *  the track is locked. */
export async function rollEdit(clipA: string, clipB: string, delta: number, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const roll = devLinks(link) ? rollEditLinkedLocal : rollEditLocal;
		roll(devTimeline, clipA, clipB, delta, devLimits());
		recordDev('Roll edit');
		return snapshot();
	}
	return invoke<Timeline>('roll_edit', { clipA, clipB, delta, link });
}

/** Slip a clip: show a different part of its footage in the same place and for the
 *  same length. `delta` is in **source** seconds; positive starts the clip later in
 *  its own footage (mirrored for a reversed clip). Clamped to the footage; a still
 *  has none to slip. */
export async function slipClip(clipId: string, delta: number, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const slip = devLinks(link) ? slipClipLinkedLocal : slipClipLocal;
		slip(devTimeline, clipId, delta, devLimits());
		recordDev('Slip clip');
		return snapshot();
	}
	return invoke<Timeline>('slip_clip', { clipId, delta, link });
}

/** Slide a clip along its track by `delta` timeline seconds (positive is later);
 *  the neighbours that touch it give way. Clamped to their footage and a 0.05 s
 *  floor. */
export async function slideClip(clipId: string, delta: number, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const slide = devLinks(link) ? slideClipLinkedLocal : slideClipLocal;
		slide(devTimeline, clipId, delta, devLimits());
		recordDev('Slide clip');
		return snapshot();
	}
	return invoke<Timeline>('slide_clip', { clipId, delta, link });
}

/** Split a clip at timeline time `at` and remove one half — trim the start (`left`)
 *  or the end (`right`) to the playhead. The half that stays keeps the clip's id.
 *  Follows the project's ripple mode: on, the later clips close the gap. */
export async function splitRemove(clipId: string, at: number, side: SplitSide, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		return splitRemoveClips([{ clip_id: clipId, at }], side, link);
	}
	return invoke<Timeline>('split_remove', { clipId, at, side, link });
}

/** `splitRemove` on several clips as **one** edit — the playhead trim of a selection,
 *  a picture and its sound together, one revision and so one undo. Each cut names a
 *  clip and its time; `side` is the same for all. All or nothing (the promise rejects
 *  and nothing changed), one clip per track; ripple mode closes each track's own gap. */
export async function splitRemoveClips(cuts: ClipCut[], side: SplitSide, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.splitRemoveClips(tl, devEnv(undefined, link), cuts, side));
		return snapshot();
	}
	return invoke<Timeline>('split_remove_clips', { cuts, side, link });
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

export async function setTrackDuck(trackId: string, duck: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const track = devTimeline.tracks.find((t) => t.id === trackId);
		if (track) track.duck = duck;
		recordDev(duck ? 'Duck track' : 'Unduck track');
		return snapshot();
	}
	return invoke<Timeline>('set_track_duck', { trackId, duck });
}

/** Cut a clip to a shape, or pass `null` to clear the mask. */
export async function setMask(clipId: string, mask: Mask | null): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].mask = mask;
		recordDev(mask ? 'Mask clip' : 'Clear mask');
		return snapshot();
	}
	return invoke<Timeline>('set_mask', { clipId, mask });
}

/** Set a track's fader — the gain riding every clip on it. */
export async function setTrackVolume(trackId: string, volume: number): Promise<Timeline> {
	const v = Math.min(4, Math.max(0, volume));
	if (!inTauri()) {
		const track = devTimeline.tracks.find((t) => t.id === trackId);
		if (track) track.volume = v;
		recordDev('Set track level');
		return snapshot();
	}
	return invoke<Timeline>('set_track_volume', { trackId, volume: v });
}

/** Set a track's stereo placement, -1 (hard left) to 1 (hard right). */
export async function setTrackPan(trackId: string, pan: number): Promise<Timeline> {
	const p = Math.min(1, Math.max(-1, pan));
	if (!inTauri()) {
		const track = devTimeline.tracks.find((t) => t.id === trackId);
		if (track) track.pan = p;
		recordDev('Set track pan');
		return snapshot();
	}
	return invoke<Timeline>('set_track_pan', { trackId, pan: p });
}

/** Keep the harness timeline's master absent while it is the default, like the saved file. */
function storeDevMaster(next: { volume: number; limiter: boolean; ceiling_db: number }) {
	const untouched =
		next.volume === DEFAULT_MASTER.volume &&
		next.limiter === DEFAULT_MASTER.limiter &&
		next.ceiling_db === DEFAULT_MASTER.ceiling_db;
	if (untouched) delete devTimeline.master;
	else devTimeline.master = next;
}

/**
 * Set the master fader — the linear gain on the finished mix, after every track
 * and before `loudnorm`. Clamped to `0..4` (+12 dB), as the engine does.
 */
export async function setMasterVolume(volume: number): Promise<Timeline> {
	if (!Number.isFinite(volume)) throw new Error('master volume must be a number');
	const v = clampMasterVolume(volume);
	if (!inTauri()) {
		storeDevMaster({ ...masterOf(devTimeline), volume: v });
		recordDev('Set master level');
		return snapshot();
	}
	return invoke<Timeline>('set_master_volume', { volume: v });
}

/**
 * Switch the master limiter on or off, optionally moving its ceiling (dBFS,
 * clamped to -24..0). Omitting `ceilingDb` keeps the one it had.
 */
export async function setMasterLimiter(enabled: boolean, ceilingDb?: number | null): Promise<Timeline> {
	if (ceilingDb != null && !Number.isFinite(ceilingDb)) throw new Error('limiter ceiling must be a number');
	const ceiling = ceilingDb == null ? null : clampCeiling(ceilingDb);
	if (!inTauri()) {
		const current = masterOf(devTimeline);
		storeDevMaster({ ...current, limiter: enabled, ceiling_db: ceiling ?? current.ceiling_db });
		recordDev('Set master limiter');
		return snapshot();
	}
	return invoke<Timeline>('set_master_limiter', { enabled, ceilingDb: ceiling });
}

/**
 * Measure how loud the cut is — the finished mix and each track — in one pass
 * over the audio the export would render (`range` is a `{start, end}` span of the
 * cut, default all of it; `loudnorm` measures it as an export with normalisation
 * on would write it). Whole-file work: it takes seconds on a long cut. Outside the
 * desktop app there is no ffmpeg, so the numbers are an **estimate** from the
 * sample analysis, and the result says so (`estimated`).
 */
export async function getLevels(range?: { start: number; end: number } | null, loudnorm = false): Promise<Levels> {
	if (!inTauri()) {
		// The backend refuses a span that starts past the end rather than widening it
		// to the whole cut, which would answer a question nobody asked.
		const cut = timelineDuration(devTimeline);
		if (range && cut > 0 && range.start >= cut) {
			throw new Error(`range starts at ${range.start.toFixed(1)}s but the cut is only ${cut.toFixed(1)}s long`);
		}
		return estimateLevels(devTimeline, sampleAssets, (id) => sampleAnalysis[id]?.loudness ?? undefined, {
			range,
			loudnorm
		});
	}
	return invoke<Levels>('get_levels', { range: range ?? null, loudnorm });
}

/** Set the frame the project is cut for, or pass `null` to follow the footage. */
export async function setDeliveryFormat(format: Delivery | null): Promise<Timeline> {
	if (!inTauri()) {
		devTimeline.format = format;
		recordDev(format ? `Deliver ${format.width}x${format.height}` : 'Deliver at source shape');
		return snapshot();
	}
	return invoke<Timeline>('set_delivery_format', {
		width: format?.width ?? null,
		height: format?.height ?? null,
		fit: format?.fit ?? null
	});
}

export async function setTrackMuted(trackId: string, muted: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const track = devTimeline.tracks.find((t) => t.id === trackId);
		if (track) track.muted = muted;
		recordDev(muted ? 'Mute track' : 'Unmute track');
		return snapshot();
	}
	return invoke<Timeline>('set_track_muted', { trackId, muted });
}

export async function setTrackSolo(trackId: string, solo: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const track = devTimeline.tracks.find((t) => t.id === trackId);
		if (track) track.solo = solo;
		recordDev(solo ? 'Solo track' : 'Unsolo track');
		return snapshot();
	}
	return invoke<Timeline>('set_track_solo', { trackId, solo });
}

export async function setTrackLocked(trackId: string, locked: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const track = devTimeline.tracks.find((t) => t.id === trackId);
		if (track) track.locked = locked;
		recordDev(locked ? 'Lock track' : 'Unlock track');
		return snapshot();
	}
	return invoke<Timeline>('set_track_locked', { trackId, locked });
}

export async function setClipEnabled(clipId: string, enabled: boolean): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].enabled = enabled;
		recordDev(enabled ? 'Enable clip' : 'Disable clip');
		return snapshot();
	}
	return invoke<Timeline>('set_clip_enabled', { clipId, enabled });
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
export async function setSpeed(clipId: string, speed: number, link?: boolean): Promise<Timeline> {
	if (!inTauri()) {
		// Linked clips are retimed by the same ratio (`setSpeedLinked`) and re-placed about the
		// named clip, so a picture and its sound keep step; `link: false` retimes the named clip alone.
		devRun((tl) => ops.setSpeed(tl, devEnv(undefined, link), clipId, speed));
		return snapshot();
	}
	return invoke<Timeline>('set_speed', { clipId, speed, link });
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
		gamma: patch.gamma,
		temperature: patch.temperature
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

/** Replace a clip's video effect chain (empty list clears it). */
export async function setVideoEffects(clipId: string, effects: VideoEffect[]): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].effects = effects;
		recordDev('Set video effects');
		return snapshot();
	}
	return invoke<Timeline>('set_video_effects', { clipId, effects });
}

/** Replace a clip's audio effect chain (empty list clears it). */
export async function setAudioEffects(clipId: string, effects: AudioEffect[]): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].audio = effects;
		recordDev('Set audio effects');
		return snapshot();
	}
	return invoke<Timeline>('set_audio_effects', { clipId, effects });
}

/** Replace a clip's transform keyframes (empty list clears the animation). */
export async function setKeyframes(clipId: string, keyframes: Keyframe[]): Promise<Timeline> {
	if (!inTauri()) {
		for (const k of keyframes) {
			const problem = easingProblem(k.easing);
			if (problem) throw new Error(problem);
		}
		const found = locate(devTimeline, clipId);
		if (found) {
			const clip = found[0].clips[found[1]];
			clip.keyframes = [...keyframes].sort((a, b) => a.time - b.time);
			pruneChannels(clip);
		}
		recordDev('Set keyframes');
		return snapshot();
	}
	return invoke<Timeline>('set_keyframes', { clipId, keyframes });
}

/** Add (or replace) a keyframe at `time`; unspecified channels capture the
 *  clip's current static transform. */
export async function addKeyframe(
	clipId: string,
	time: number,
	patch: Partial<Omit<Keyframe, 'time'>> = {}
): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const clip = found[0].clips[found[1]];
			// The clip's present pose there (`transform_at`), whichever of the bundle and the
			// numbers' own tracks drives each number.
			const tf = transformAt(clip, time);
			const base: Keyframe = {
				time,
				scale: tf.scale,
				pos_x: tf.pos_x,
				pos_y: tf.pos_y,
				rotation: tf.rotation,
				opacity: tf.opacity,
				...patch
			};
			// As the backend puts a key in: re-keying a moment keeps how it leaves, and a key inside
			// a segment splits it (a hold stays held, a curve stays the same curve).
			const tracked = (['scale', 'pos_x', 'pos_y', 'rotation', 'opacity'] as const).filter(
				(p) => patch[p] !== undefined && channelOf(clip, p)
			);
			clip.keyframes = insertKeyframe(clip.keyframes ?? [], base);
			// A number with a track of its own ignores the bundle's key: what was asked for it
			// goes into that track.
			for (const p of tracked) insertPropertyKey(clip, p, { time, value: patch[p] as number });
		}
		recordDev('Add keyframe');
		return snapshot();
	}
	return invoke<Timeline>('add_keyframe', {
		clipId,
		time,
		scale: patch.scale,
		posX: patch.pos_x,
		posY: patch.pos_y,
		rotation: patch.rotation,
		opacity: patch.opacity
	});
}

/** Set the easing of the segment leaving the keyframe at `time` (within a millisecond). With
 *  `prop`, that one number's key; without, the transform key there — the bundle's and every
 *  transform number's own track's. */
export async function setKeyframeEasing(clipId: string, time: number, easing: Easing, prop?: Property): Promise<Timeline> {
	if (!inTauri()) {
		const problem = easingProblem(easing);
		if (problem) throw new Error(problem);
		const found = locate(devTimeline, clipId);
		const clip = found ? found[0].clips[found[1]] : undefined;
		if (!clip) throw new Error(`clip ${clipId} not found`);
		if (prop) {
			if (!setPropertyEasing(clip, prop, time, easing)) {
				throw new Error(`the clip has no ${prop} keyframe at ${time.toFixed(3)} s`);
			}
			recordDev(`Set ${prop} keyframe easing`);
			return snapshot();
		}
		const near = (clip.keyframes ?? [])
			.filter((k) => Math.abs(k.time - time) <= 1e-3)
			.sort((a, b) => Math.abs(a.time - time) - Math.abs(b.time - time))[0];
		const tracked = (['scale', 'pos_x', 'pos_y', 'rotation', 'opacity'] as const).filter((p) =>
			channelOf(clip, p)?.keys.some((k) => Math.abs(k.time - time) <= 1e-3)
		);
		if (!near && tracked.length === 0) throw new Error(`the clip has no keyframe at ${time.toFixed(3)} s`);
		if (near) {
			if (easing === 'linear') delete near.easing;
			else near.easing = easing;
		}
		for (const p of tracked) setPropertyEasing(clip, p, time, easing);
		recordDev('Set keyframe easing');
		return snapshot();
	}
	return invoke<Timeline>('set_keyframe_easing', { clipId, time, easing, prop });
}

/** Replace the keys of one animatable number — a transform number, a colour number or the clip's
 *  volume. No keys leaves it static (`set_property_keyframes`). */
export async function setPropertyKeyframes(clipId: string, prop: Property, keys: PropertyKey[]): Promise<Timeline> {
	if (!inTauri()) {
		const problem = propertyKeysProblem(prop, keys);
		if (problem) throw new Error(problem);
		const found = locate(devTimeline, clipId);
		if (!found) throw new Error(`clip ${clipId} not found`);
		setPropertyKeys(found[0].clips[found[1]], prop, keys);
		recordDev(`Set ${prop === 'pos_x' ? 'position x' : prop === 'pos_y' ? 'position y' : prop} keyframes`);
		return snapshot();
	}
	return invoke<Timeline>('set_property_keyframes', { clipId, prop, keys });
}

/** Copy the animation of `props` (every keyed number when empty) from one clip to another,
 *  `offset` seconds later (`copy_keyframes`). */
export async function copyKeyframes(
	fromClipId: string,
	toClipId: string,
	props: Property[] = [],
	offset = 0
): Promise<Timeline> {
	if (!inTauri()) {
		if (!Number.isFinite(offset) || Math.abs(offset) > 360_000) {
			throw new Error('offset must be a number within ±360000 seconds');
		}
		if (fromClipId === toClipId) throw new Error('copy keyframes from one clip to another: the two ids are the same clip');
		const from = locate(devTimeline, fromClipId);
		const to = locate(devTimeline, toClipId);
		if (!from) throw new Error(`clip ${fromClipId} not found`);
		if (!to) throw new Error(`clip ${toClipId} not found`);
		const source = from[0].clips[from[1]];
		const tracks = propertyKeysShifted(source, props, offset);
		if (tracks.length === 0) throw new Error('the source clip has no keyframes to copy');
		for (const t of tracks) {
			if (t.keys.length === 0) throw new Error(`the source clip has no ${t.prop} keyframes`);
		}
		const dest = to[0].clips[to[1]];
		for (const t of tracks) setPropertyKeys(dest, t.prop, t.keys);
		recordDev('Copy keyframes');
		return snapshot();
	}
	return invoke<Timeline>('copy_keyframes', { fromClipId, toClipId, props, offset });
}

export async function clearKeyframes(clipId: string): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) clearTransformAnimation(found[0].clips[found[1]]);
		recordDev('Clear keyframes');
		return snapshot();
	}
	return invoke<Timeline>('clear_keyframes', { clipId });
}

/**
 * Mark an asset as 360 footage the probe couldn't identify (or pass `null` to
 * unmark it). Sticks to the asset, so every clip cut from it afterwards is
 * reframed — unlike `setReframe`, which only touches one clip.
 */
export async function setAssetProjection(
	assetId: string,
	projection: Projection | null
): Promise<Asset> {
	if (!inTauri()) {
		const asset = assetById(assetId) ?? sampleAssets[0];
		for (const s of asset.streams) {
			if (s.kind === 'video') s.projection = projection ?? undefined;
		}
		return { ...asset, streams: [...asset.streams] };
	}
	return invoke<Asset>('set_asset_projection', { assetId, projection });
}

/** Aim a 360 clip's virtual camera. Unspecified fields are left unchanged. */
export async function setReframe(clipId: string, patch: Partial<Reframe>): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const clip = found[0].clips[found[1]];
			clip.reframe = { ...DEFAULT_REFRAME, ...(clip.reframe ?? {}), ...patch };
		}
		recordDev('Set reframe');
		return snapshot();
	}
	return invoke<Timeline>('set_reframe', {
		clipId,
		yaw: patch.yaw,
		pitch: patch.pitch,
		roll: patch.roll,
		fov: patch.fov,
		lensFov: patch.lens_fov,
		input: patch.input,
		output: patch.output
	});
}

/** Stop reprojecting a 360 clip, leaving its raw spherical picture. */
export async function clearReframe(clipId: string): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) found[0].clips[found[1]].reframe = null;
		recordDev('Clear reframe');
		return snapshot();
	}
	return invoke<Timeline>('clear_reframe', { clipId });
}

/** Replace a 360 clip's camera animation. An empty list holds the static pose. */
export async function setReframeKeyframes(
	clipId: string,
	keyframes: ReframeKeyframe[]
): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		const rf = found?.[0].clips[found[1]].reframe;
		if (rf) rf.keyframes = [...keyframes].sort((a, b) => a.time - b.time);
		recordDev('Set reframe keyframes');
		return snapshot();
	}
	return invoke<Timeline>('set_reframe_keyframes', { clipId, keyframes });
}

/** Add (or replace) a camera keyframe at `time`; unspecified channels capture
 *  the camera's current pose there. */
export async function addReframeKeyframe(
	clipId: string,
	time: number,
	patch: Partial<Omit<ReframeKeyframe, 'time'>> = {}
): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		const rf = found?.[0].clips[found[1]].reframe;
		if (rf) {
			const kfs = (rf.keyframes ?? []).filter((k) => Math.abs(k.time - time) > 1e-6);
			kfs.push({ time, yaw: rf.yaw, pitch: rf.pitch, roll: rf.roll, fov: rf.fov, ...patch });
			kfs.sort((a, b) => a.time - b.time);
			rf.keyframes = kfs;
		}
		recordDev('Add reframe keyframe');
		return snapshot();
	}
	return invoke<Timeline>('add_reframe_keyframe', { clipId, time, ...patch });
}

/** Add a text overlay (title / lower-third / caption). */
export async function addMarker(time: number, name: string, color?: string): Promise<Timeline> {
	if (!inTauri()) {
		devTimeline.markers = [
			...(devTimeline.markers ?? []),
			{ id: crypto.randomUUID(), time, name, color: color ?? null }
		].sort((a, b) => a.time - b.time);
		recordDev('Add marker');
		return snapshot();
	}
	return invoke<Timeline>('add_marker', { time, name, color });
}

export async function updateMarker(
	markerId: string,
	patch: { time?: number; name?: string; color?: string }
): Promise<Timeline> {
	if (!inTauri()) {
		const m = devTimeline.markers?.find((x) => x.id === markerId);
		if (m) {
			if (patch.time !== undefined) m.time = patch.time;
			if (patch.name !== undefined) m.name = patch.name;
			if (patch.color !== undefined) m.color = patch.color || null;
			devTimeline.markers = [...(devTimeline.markers ?? [])].sort((a, b) => a.time - b.time);
		}
		recordDev('Update marker');
		return snapshot();
	}
	return invoke<Timeline>('update_marker', { markerId, ...patch });
}

export async function removeMarker(markerId: string): Promise<Timeline> {
	if (!inTauri()) {
		devTimeline.markers = (devTimeline.markers ?? []).filter((m) => m.id !== markerId);
		recordDev('Remove marker');
		return snapshot();
	}
	return invoke<Timeline>('remove_marker', { markerId });
}

export async function addOverlay(text: string, start: number, end: number): Promise<Timeline> {
	if (!inTauri()) {
		(devTimeline.overlays ??= []).push({
			id: uid(),
			text,
			start,
			end,
			pos_x: 0.5,
			pos_y: 0.82,
			size: 0.06,
			color: 'white',
			bold: false
		});
		recordDev('Add text overlay');
		return snapshot();
	}
	return invoke<Timeline>('add_overlay', { text, start, end });
}

/** Update an overlay; only provided fields change. Pass `bg: ''` / `font: ''` to clear them. */
export async function updateOverlay(
	overlayId: string,
	patch: Partial<Omit<TextOverlay, 'id' | 'keyframes'>>
): Promise<Timeline> {
	if (!inTauri()) {
		const o = devTimeline.overlays?.find((ov) => ov.id === overlayId);
		if (o) {
			Object.assign(o, patch);
			if (patch.bg === '') o.bg = null;
			if (patch.font === '') o.font = null;
		}
		recordDev('Update text overlay');
		return snapshot();
	}
	return invoke<Timeline>('update_overlay', {
		overlayId,
		text: patch.text,
		start: patch.start,
		end: patch.end,
		posX: patch.pos_x,
		posY: patch.pos_y,
		size: patch.size,
		color: patch.color,
		bg: patch.bg ?? undefined,
		font: patch.font ?? undefined,
		bold: patch.bold
	});
}

export async function removeOverlay(overlayId: string): Promise<Timeline> {
	if (!inTauri()) {
		if (devTimeline.overlays) devTimeline.overlays = devTimeline.overlays.filter((o) => o.id !== overlayId);
		recordDev('Remove text overlay');
		return snapshot();
	}
	return invoke<Timeline>('remove_overlay', { overlayId });
}

export async function setOverlayKeyframes(overlayId: string, keyframes: TextKeyframe[]): Promise<Timeline> {
	if (!inTauri()) {
		const o = devTimeline.overlays?.find((ov) => ov.id === overlayId);
		if (o) o.keyframes = [...keyframes].sort((a, b) => a.time - b.time);
		recordDev('Set overlay keyframes');
		return snapshot();
	}
	return invoke<Timeline>('set_overlay_keyframes', { overlayId, keyframes });
}

/** Caption the cut: project every clip's transcript through the current edit and
 *  write the result as overlays, replacing any previously generated set. */
export async function generateCaptions(options?: CaptionOptions): Promise<Timeline> {
	if (!inTauri()) {
		const transcripts: Record<string, TranscriptSegment[]> = {};
		for (const track of devTimeline.tracks) {
			for (const clip of track.clips) {
				const segs = sampleAnalysis[clip.asset_id]?.transcript;
				if (segs) transcripts[clip.asset_id] = segs;
			}
		}
		const created = captionsForTimeline(devTimeline, transcripts, resolveCaptions(options));
		const kept = (devTimeline.overlays ??= []).filter((o) => !o.generated);
		devTimeline.overlays = [...kept, ...created.map((o) => ({ ...o, id: uid() }))];
		recordDev(options?.style === 'word_punch' ? 'Generate word captions' : 'Generate captions');
		return snapshot();
	}
	return invoke<Timeline>('generate_captions', { options: options ?? null });
}

/** Remove the generated captions, leaving typed titles and lower-thirds alone. */
export async function clearCaptions(): Promise<Timeline> {
	if (!inTauri()) {
		devTimeline.overlays = (devTimeline.overlays ?? []).filter((o) => !o.generated);
		recordDev('Clear captions');
		return snapshot();
	}
	return invoke<Timeline>('clear_captions');
}

/** Caption the cut from a `.srt` / `.ass` / `.ssa` file on disk. `base` is
 *  `timeline` (the file is a subtitle track for the finished cut — the default) or
 *  `source` with an `assetId` (the file times that asset's own footage and is
 *  projected through its clips like a transcript); `options` is the same look
 *  `generateCaptions` takes. Replaces the previous generated / imported captions
 *  as one `Import captions` revision and resolves to the refreshed cut plus what
 *  the import did. A browser has no disk to read — use `importCaptionsText`. */
export async function importCaptions(path: string, req: CaptionImportRequest = {}): Promise<CaptionImportResult> {
	if (!inTauri()) {
		throw new Error('Importing a subtitle file by path needs the desktop app; in a browser pass its text to importCaptionsText.');
	}
	return invoke<CaptionImportResult>('import_captions', {
		path,
		base: req.base ?? null,
		assetId: req.assetId ?? null,
		options: req.options ?? null,
		offset: req.offset ?? null
	});
}

/** `importCaptions` for text the page already holds — a file read through an
 *  `<input type=file>`, or pasted. `format` is `srt` / `ass`; omitted, it is
 *  guessed from the text. This is the variant the browser harness runs. */
export async function importCaptionsText(
	text: string,
	req: CaptionImportRequest & { format?: CaptionFormat } = {}
): Promise<CaptionImportResult> {
	if (!inTauri()) {
		const base = resolveBase(req.base, req.assetId);
		const { overlays, kept, summary } = importCaptionsInto(
			devTimeline,
			text,
			{ format: req.format, base, options: req.options, offset: req.offset },
			{ assetKnown: (id) => assetById(id) !== undefined }
		);
		devTimeline.overlays = [...kept, ...overlays.map((o) => ({ ...o, id: uid() }))];
		recordDev('Import captions');
		return { timeline: snapshot(), summary };
	}
	return invoke<CaptionImportResult>('import_captions_text', {
		text,
		format: req.format ?? null,
		base: req.base ?? null,
		assetId: req.assetId ?? null,
		options: req.options ?? null,
		offset: req.offset ?? null
	});
}

/** A subtitle file the user picked: a path the backend reads (the desktop app),
 *  or text the page already read (the browser harness, which has no disk). */
export type CaptionFilePick =
	| { kind: 'path'; path: string; name: string }
	| { kind: 'text'; text: string; name: string; format: CaptionFormat | null };

/** Pick a `.srt` / `.ass` / `.ssa` file; `null` if cancelled. Desktop: the native
 *  dialog, answering with the path (`importCaptions` reads it, with its guards).
 *  Browser: an `<input type=file>` read as text, for `importCaptionsText`. In the
 *  browser the picker must be opened straight from the click, before any `await`,
 *  or the page has no user gesture to open it with. Rejects a file over the size
 *  cap without reading it. */
export async function pickCaptionFile(): Promise<CaptionFilePick | null> {
	if (!inTauri()) {
		return new Promise((resolve, reject) => {
			const input = document.createElement('input');
			input.type = 'file';
			input.accept = CAPTION_EXTENSIONS.map((e) => `.${e}`).join(',');
			input.onchange = () => {
				const f = input.files?.[0];
				if (!f) return resolve(null);
				if (f.size > MAX_CAPTION_FILE_BYTES) return reject(fileTooLarge());
				f.text().then(
					(text) => resolve({ kind: 'text', text, name: f.name, format: parseFormat(f.name.split('.').pop() ?? '') }),
					reject
				);
			};
			input.oncancel = () => resolve(null);
			input.click();
		});
	}
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: false,
		filters: [{ name: 'Subtitles', extensions: [...CAPTION_EXTENSIONS] }]
	});
	if (typeof selected !== 'string') return null;
	return { kind: 'path', path: selected, name: baseName(selected) };
}

/** Write an asset's transcript to a `.srt` file; returns the path. */
export async function exportSrt(assetId: string, outputPath: string): Promise<string> {
	if (!inTauri()) return outputPath;
	return invoke<string>('export_srt', { assetId, outputPath });
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

/**
 * Cut to the beat: ripple a track's cuts onto the beat grid of the analyzed
 * music. `trackId` omitted aligns every video track.
 */
export async function snapToBeats(trackId?: string, tolerance?: number): Promise<Timeline> {
	if (!inTauri()) {
		const beats = beatGrid(devTimeline, (id) => sampleAnalysis[id]?.tempo);
		if (beats.length < 2) throw new Error('no beat grid — put rhythmic audio on an audio track and analyze it first');
		const tol = tolerance ?? defaultBeatTolerance(beats);
		const limitFor = (id: string) => assetById(id)?.duration ?? Infinity;
		// The snap reflows a lane without knowing about links; each retimed clip then carries
		// its change to its partners (`carryLinksSince`, inside `ops.carrySince`).
		devRun((tl) =>
			ops.carrySince(tl, devEnv(false, undefined), (t) => {
				const targets = t.tracks.filter((x) => (trackId ? x.id === trackId : x.kind === 'video'));
				for (const track of targets) alignCutsToBeats(track.clips, beats, tol, limitFor);
			})
		);
		return snapshot();
	}
	return invoke<Timeline>('snap_to_beats', { trackId, tolerance });
}

/** Frame each shot for the delivery frame instead of centring it blindly.
 *
 *  The app samples where each clip's content actually sits (one short ffmpeg
 *  pass per clip) and writes the crop that keeps it. The browser harness has no
 *  decoder, so it applies the centre window from `smart-crop.ts` — the shape is
 *  right, the choice of *which* part of the shot survives is the half that only
 *  exists with media behind it. Either way the result is an ordinary transform
 *  crop the inspector can adjust. */
export async function smartCrop(clipId?: string): Promise<Timeline> {
	if (!inTauri()) {
		const fmt = devTimeline.format;
		const first = sampleAssets[0]?.streams.find((s) => s.kind === 'video');
		const aspect = (fmt?.width ?? first?.width ?? 1920) / (fmt?.height ?? first?.height ?? 1080);
		let moved = 0;
		for (const track of devTimeline.tracks) {
			if (track.kind !== 'video' || (track.locked && !clipId)) continue;
			for (const clip of track.clips) {
				if (clipId && clip.id !== clipId) continue;
				const stream = assetById(clip.asset_id)?.streams.find((s) => s.kind === 'video');
				const crop = stream?.width && stream?.height ? centeredCrop(stream.width, stream.height, aspect) : null;
				if (!crop) continue;
				clip.transform = {
					...(clip.transform ?? DEFAULT_TRANSFORM),
					crop_left: crop.left,
					crop_right: crop.right,
					crop_top: crop.top,
					crop_bottom: crop.bottom
				};
				moved += 1;
			}
		}
		if (!moved) throw new Error('every shot is already that shape — nothing to reframe');
		recordDev('Smart crop');
		return snapshot();
	}
	return invoke<Timeline>('smart_crop', { clipId });
}

/** Give an asset's sound its own clip on an audio track, for every use of the asset on a
 *  video track that still plays it: each is **detached** (the picture muted, an audio clip with
 *  the same span linked to it) so nothing is heard twice — one revision. A clip on a locked
 *  track is skipped and reported; with nothing to detach it rejects (`addAssetAudio` is the
 *  explicit way to append the asset's whole audio). */
export async function extractAudio(assetId: string): Promise<AudioDetached> {
	if (!inTauri()) {
		const done = devRun((tl) => ops.extractAudio(tl, devEnv(false, undefined), assetId));
		return { timeline: snapshot(), detached: done.detached.length, skipped: done.skipped };
	}
	return invoke<AudioDetached>('extract_audio', { assetId });
}

/** Append an asset's whole audio to the first audio track as a clip of its own. It never
 *  touches a picture clip, so an asset that also plays its own sound from a video track is
 *  heard twice where they overlap — `extractAudio` is for that. */
export async function addAssetAudio(assetId: string): Promise<Timeline> {
	if (!inTauri()) {
		const asset = assetById(assetId);
		if (!asset) throw new Error(`asset not found: ${assetId}`);
		if (!asset.streams.some((s) => s.kind === 'audio')) throw new Error('invalid argument: asset has no audio stream');
		devRun((tl) => ops.addAssetAudio(tl, devEnv(false, undefined), asset));
		return snapshot();
	}
	return invoke<Timeline>('add_asset_audio', { assetId });
}

// ---- linked A/V ---------------------------------------------------------------

/** **Detach audio**: split a picture clip's own sound onto an audio track — a new audio
 *  clip with the same span and position, linked to the picture, whose own sound is muted;
 *  the picture track's fader is folded into the new clip so the level is unchanged.
 *  One revision. Rejects a clip that is not on a video track, whose asset has no audio,
 *  whose sound is already detached, or whose track is locked. */
export async function detachAudio(clipId: string): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.detach(tl, devEnv(false, undefined), clipId));
		return snapshot();
	}
	return invoke<Timeline>('detach_audio', { clipId });
}

/** **Detach audio** from several picture clips as **one** revision. A clip that cannot be
 *  detached is skipped and reported; rejects only when none could be. */
export async function detachAudioClips(clipIds: string[]): Promise<AudioDetached> {
	if (!inTauri()) {
		const done = devRun((tl) => ops.detachClips(tl, devEnv(false, undefined), clipIds));
		return { timeline: snapshot(), detached: done.detached.length, skipped: done.skipped };
	}
	return invoke<AudioDetached>('detach_audio_clips', { clipIds });
}

/** **Reattach audio**: delete the linked audio clip(s) carrying a picture's sound and let
 *  the picture play its own again. Name either clip of the pair. Rejects when the picture's
 *  sound is already playing from another audio clip (it would double). One revision. */
export async function reattachAudio(clipId: string): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.reattach(tl, devEnv(false, undefined), clipId));
		return snapshot();
	}
	return invoke<Timeline>('reattach_audio', { clipId });
}

/** **Reattach audio** on several pictures as **one** revision, all or nothing: name either
 *  clip of each pair. Rejects — changing nothing — when any pair cannot be reattached. */
export async function reattachAudioClips(clipIds: string[]): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.reattachClips(tl, devEnv(false, undefined), clipIds));
		return snapshot();
	}
	return invoke<Timeline>('reattach_audio_clips', { clipIds });
}

/** Link clips (at least two, on different tracks) so an edit to one is carried to the
 *  others. One revision. */
export async function linkClips(clipIds: string[]): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.link(tl, devEnv(false, undefined), clipIds));
		return snapshot();
	}
	return invoke<Timeline>('link_clips', { clipIds });
}

/** Unlink clips; a group left with a single clip dissolves. One revision. */
export async function unlinkClips(clipIds: string[]): Promise<Timeline> {
	if (!inTauri()) {
		devRun((tl) => ops.unlink(tl, devEnv(false, undefined), clipIds));
		return snapshot();
	}
	return invoke<Timeline>('unlink_clips', { clipIds });
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

/**
 * What one revision changed. `null` in the browser harness — the diff engine
 * lives in kerf-core, and mirroring it here would be a second source of truth
 * for the one thing the review card must not get wrong.
 */
export async function revisionDiff(seq: number): Promise<TimelineDiff | null> {
	if (!inTauri()) return null;
	return invoke<TimelineDiff>('revision_diff', { seq });
}

// ---- staged edits (the agent's pending proposal) ---------------------------
//
// A connected agent's task edits never touch the open cut: they accumulate in a
// proposal that the user applies or discards. In the browser there is no agent,
// so `?staged=1` seeds a synthetic one — the review flow is the point, and it
// has to be explorable (and testable) without a desktop build.

function timelineDuration(tl: Timeline): number {
	return tl.tracks.reduce((m, t) => Math.max(m, trackEnd(t)), 0);
}

/** The `true` in the browser harness: `?staged=1` seeds a fake agent proposal. */
function fakeStagedRequested(): boolean {
	return typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('staged');
}

let devStaged: { edit: StagedEdit; timeline: Timeline } | null = null;
let devStagedSeeded = false;

/** A plausible proposal over the dev timeline: a tightened intro, ripple closed. */
function seedDevStaged() {
	devStagedSeeded = true;
	const before = snapshot();
	const proposed = snapshot();
	const video = proposed.tracks.find((t) => t.kind === 'video');
	const audio = proposed.tracks.find((t) => t.kind === 'audio');
	if (!video || video.clips.length < 2 || !audio || audio.clips.length === 0) return;

	const [first, second] = video.clips;
	const entries: TimelineDiff['entries'] = [];
	const wasSource = first.source_out - first.source_in;
	first.source_out = first.source_in + wasSource - 3.5;
	entries.push({
		kind: 'clip_retrimmed',
		summary: `Trimmed clip on ${video.name} at ${fmtTime(first.timeline_start)} — ${wasSource.toFixed(1)}s → ${(wasSource - 3.5).toFixed(1)}s (-3.5s)`,
		track_id: video.id,
		clip_id: first.id,
		at: first.timeline_start
	});
	const movedFrom = second.timeline_start;
	second.timeline_start = trackEnd({ ...video, clips: [first] });
	entries.push({
		kind: 'clip_moved',
		summary: `Moved clip on ${video.name} — ${fmtTime(movedFrom)} → ${fmtTime(second.timeline_start)}`,
		track_id: video.id,
		clip_id: second.id,
		at: second.timeline_start
	});
	// The sound is linked to the picture, so the trim reaches it too (an agent's edits carry to
	// partners like anyone's); with nothing linked, the bed is shortened by a share of its own length.
	const bed = audio.clips.find((c) => !!c.link_id && c.link_id === first.link_id) ?? audio.clips[0];
	const bedWas = bed.source_out - bed.source_in;
	const bedCut = bed.link_id && bed.link_id === first.link_id ? 3.5 : Math.min(20, bedWas / 2);
	bed.source_out = bed.source_in + bedWas - bedCut;
	entries.push({
		kind: 'clip_retrimmed',
		summary: `Trimmed clip on ${audio.name} at ${fmtTime(bed.timeline_start)} — ${bedWas.toFixed(1)}s → ${(bedWas - bedCut).toFixed(1)}s (-${bedCut.toFixed(1)}s)`,
		track_id: audio.id,
		clip_id: bed.id,
		at: bed.timeline_start
	});
	const overlay: TextOverlay = {
		id: uid(),
		text: 'How we cut this',
		start: 1,
		end: 4,
		pos_x: 0.5,
		pos_y: 0.5,
		size: 0.08,
		color: 'white',
		bold: true
	};
	proposed.overlays = [...(proposed.overlays ?? []), overlay];
	entries.push({
		kind: 'overlay_added',
		summary: `Added text “${overlay.text}” at ${fmtTime(overlay.start)}–${fmtTime(overlay.end)}`,
		at: overlay.start
	});

	const countClips = (tl: Timeline) => tl.tracks.reduce((n, t) => n + t.clips.length, 0);
	devStaged = {
		timeline: proposed,
		edit: {
			base_seq: devHead,
			task_id: devTasks.find((t) => t.status === 'ready')?.id ?? null,
			note: 'Tighten the intro',
			edits: ['Trim clip', 'Move clip', 'Trim clip', 'Add overlay'],
			created_at: now(),
			updated_at: now(),
			stale: false,
			diff: {
				entries,
				duration_before: timelineDuration(before),
				duration_after: timelineDuration(proposed),
				clips_before: countClips(before),
				clips_after: countClips(proposed)
			}
		}
	};
}

/** The proposal a connected agent has staged, or `null` when there is none. */
export async function getStagedEdit(): Promise<StagedEdit | null> {
	if (!inTauri()) {
		if (!devStagedSeeded && fakeStagedRequested()) seedDevStaged();
		return devStaged ? structuredClone(devStaged.edit) : null;
	}
	return invoke<StagedEdit | null>('get_staged_edit');
}

/** The staged timeline itself, for previewing the proposal in the editor. */
export async function getStagedTimeline(): Promise<Timeline | null> {
	if (!inTauri()) {
		if (!devStagedSeeded && fakeStagedRequested()) seedDevStaged();
		return devStaged ? structuredClone(devStaged.timeline) : null;
	}
	return invoke<Timeline | null>('get_staged_timeline');
}

/** Accept the proposal: it becomes the live timeline as a single revision. */
export async function applyStagedEdit(force = false): Promise<Timeline> {
	if (!inTauri()) {
		if (!devStaged) throw new Error('no staged edit is pending');
		if (devStaged.edit.stale && !force) throw new Error('the timeline changed since these edits were staged');
		devTimeline = structuredClone(devStaged.timeline);
		recordDev(devStaged.edit.note ?? 'Agent edit');
		devHistory[devHead].source = 'agent';
		devStaged = null;
		return snapshot();
	}
	return invoke<Timeline>('apply_staged_edit', { force });
}

/** Throw the proposal away, leaving the live timeline untouched. */
export async function discardStagedEdit(): Promise<Timeline> {
	if (!inTauri()) {
		if (!devStaged) throw new Error('no staged edit is pending');
		devStaged = null;
		return snapshot();
	}
	return invoke<Timeline>('discard_staged_edit');
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
		// Accepting a task is accepting its edits — same rule as kerf-core.
		if (devStaged?.edit.task_id === taskId) await applyStagedEdit(true);
		devTasks = devTasks.map((t) => (t.id === taskId ? { ...t, status: 'done', updated_at: now() } : t));
		return structuredClone(devTasks);
	}
	return invoke<Task[]>('resolve_task', { taskId });
}

/** Remove a task from the queue; resolves to the refreshed queue. */
export async function removeTask(taskId: string): Promise<Task[]> {
	if (!inTauri()) {
		if (devStaged?.edit.task_id === taskId) await discardStagedEdit();
		devTasks = devTasks.filter((t) => t.id !== taskId);
		return structuredClone(devTasks);
	}
	return invoke<Task[]>('remove_task', { taskId });
}

// ---- media (preview frames, waveforms) -------------------------------------

/**
 * A JPEG `data:` URL for one decoded frame, or `null` outside the desktop app.
 * `accurate = false` returns a fast keyframe-snapped frame (for scrubbing); the
 * exact frame is fetched once the playhead settles.
 */
export async function getFrame(
	assetId: string,
	timeSecs: number,
	maxWidth = 960,
	accurate = true
): Promise<string | null> {
	if (!inTauri()) return null;
	return invoke<string>('get_frame', { assetId, timeSecs, maxWidth, accurate });
}

/**
 * A JPEG `data:` URL for the **composited timeline** at `timeSecs` — every visible
 * clip with its color / effects / transform / overlays applied, so the preview
 * reflects Inspector edits. `null` outside the desktop app. Heavier than
 * {@link getFrame} (a raw source decode), so callers should single-flight it.
 */
export async function getTimelineFrame(timeSecs: number, maxWidth = 960): Promise<string | null> {
	if (!inTauri()) return null;
	return invoke<string>('get_timeline_frame', { timeSecs, maxWidth });
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

/**
 * `[start, end)` **source seconds** of an asset's audio as `buckets` min/max
 * peak pairs per channel, `[channel][bucket]` in -1..1 — what a clip's waveform
 * is drawn from. Served from a peak pyramid the backend caches per file, so the
 * first call for a file decodes it and every later window at any zoom is cheap;
 * the backend caps `buckets` at 4096 and `buckets` in the answer is what came
 * back. Part of the window outside the media reads 0 / 0. Rejects for an asset
 * with no audio stream. Outside the desktop app a deterministic synthetic
 * waveform stands in, with a clipped stretch so the clipping colour shows.
 */
export async function getWaveformRange(
	assetId: string,
	start: number,
	end: number,
	buckets: number
): Promise<WaveformRange> {
	// The backend takes a `usize`: a fractional pixel width would be an opaque
	// deserialize error rather than a slightly different column count.
	const count = Math.max(0, Math.round(buckets));
	if (!inTauri()) {
		const asset = assetById(assetId);
		if (!asset) throw new Error(`asset not found: ${assetId}`);
		const audio = asset.streams.find((st) => st.kind === 'audio');
		if (!audio) throw new Error(`invalid argument: asset ${assetId} has no audio stream`);
		const analysis = sampleAnalysis[assetId];
		return synthWaveformRange(
			{
				id: asset.id,
				duration: asset.duration,
				channels: audio.channels ?? 2,
				silence: analysis?.silence_segments,
				kind: analysis?.audio_class?.class === 'music' ? 'music' : 'speech',
				bpm: analysis?.tempo?.bpm
			},
			start,
			end,
			count
		);
	}
	return invoke<WaveformRange>('get_waveform_range', { assetId, start, end, buckets: count });
}

/**
 * An asset's **filmstrip** — the thumbnails a timeline clip's picture is drawn
 * from: one strip per asset (96 px high, a thumbnail every 0.5 s for a short
 * clip up to a minute or more for a long one, at most 300), tiled into a few
 * JPEG sheets. Any source window is then a handful of thumbnails picked with
 * `filmstrip-geometry.ts`'s `frameAt` / `locate`, at any zoom. The first call for
 * an asset decodes it (the backend caches the result on disk and shares one
 * decode between concurrent callers); later calls are a cache read. Rejects for
 * an asset with no video stream. Outside the desktop app a deterministic strip of
 * the same shape stands in, its sheets generated images.
 */
export async function getFilmstrip(assetId: string): Promise<Filmstrip> {
	if (!inTauri()) {
		const asset = assetById(assetId);
		if (!asset) throw new Error(`asset not found: ${assetId}`);
		return sampleFilmstrip(asset);
	}
	return invoke<Filmstrip>('get_filmstrip', { assetId });
}

/**
 * A window of an asset's audio as raw mono s16le PCM at `sampleRate`, for the
 * preview's Web Audio playback. `null` outside the desktop app (the browser
 * demo has no real media to decode).
 */
/** Decode a clip's source window to raw mono PCM. Naming `clipId` decodes it
 *  through that clip's audio effect chain, so the monitor hears the EQ /
 *  compressor / gate the export will render. */
export async function getAudio(
	assetId: string,
	start: number,
	duration: number,
	sampleRate = 32000,
	clipId?: string
): Promise<ArrayBuffer | null> {
	if (!inTauri()) {
		// No decoder here: a synthetic voice at the loudness the sample analysis gives the
		// asset, so playback, the faders and the Mixer's meters have a sound to act on.
		const lufs = sampleAnalysis[assetId]?.loudness?.integrated_lufs;
		return synthPcm(assetId, start, duration, sampleRate, lufs).buffer as ArrayBuffer;
	}
	return invoke<ArrayBuffer>('get_audio', { assetId, start, duration, sampleRate, clipId });
}

export async function getEnergy(assetId: string, buckets: number): Promise<number[]> {
	if (!inTauri()) {
		// Synthetic but deterministic RMS-like curve for the browser demo.
		return Array.from({ length: buckets }, (_, i) => {
			const env = 0.4 + 0.4 * Math.sin(i * 0.11);
			return Math.min(1, Math.max(0.05, env));
		});
	}
	return invoke<number[]>('get_energy', { assetId, buckets });
}

// ---- export ----------------------------------------------------------------

export async function exportTimeline(outputPath: string, options: ExportOptions): Promise<string> {
	if (!inTauri()) throw new Error('export is only available in the desktop app');
	return invoke<string>('export_timeline', { outputPath, options });
}

/** Render the cut once per delivery frame — one file per shape beside
 *  `outputPath`, named by shape (`cut-9x16.mp4`). With `smartCrop` every shot
 *  is framed for every shape first, recorded on the clips as one revision. */
export async function exportVariants(
	outputPath: string,
	formats: Delivery[],
	smartCrop: boolean,
	options: ExportOptions
): Promise<string[]> {
	if (!inTauri()) throw new Error('export is only available in the desktop app');
	return invoke<string[]>('export_variants', { outputPath, formats, smartCrop, options });
}

/** Ask the backend to stop the in-flight export; it then rejects with `export cancelled`. */
export async function cancelExport(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_export');
}

/** Ask the running loudness measurement (`getLevels`) to give up; it then rejects
 *  with `levels cancelled`. */
export async function cancelLevels(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_levels');
}

/** Ask the running analysis pass to give up. It stops between steps, and about
 *  once a second during transcription — the step that runs for minutes. */
export async function cancelAnalysis(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_analysis');
}

/** Subscribe to `export-progress` events for the running render. Returns an unlisten fn. */
export async function onExportProgress(cb: (p: ExportProgress) => void): Promise<() => void> {
	if (!inTauri()) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<ExportProgress>('export-progress', (e) => cb(e.payload));
}

/**
 * Progress of a slow import — an Insta360 lens pair being stitched into one 360
 * asset. Ordinary imports are instant and never report.
 */
export async function onImportProgress(cb: (p: ImportProgress) => void): Promise<() => void> {
	if (!inTauri()) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<ImportProgress>('import-progress', (e) => cb(e.payload));
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

/** Write the composited frame at `timeSecs` to `outputPath` as a cover image —
 *  full delivery resolution, through the export graph. */
export async function exportCover(timeSecs: number, outputPath: string): Promise<string> {
	if (!inTauri()) throw new Error('saving a cover frame is only available in the desktop app');
	return invoke<string>('export_cover', { timeSecs, outputPath, format: null });
}

/** Open a save dialog for a cover image. */
export async function pickCoverPath(): Promise<string | null> {
	if (!inTauri()) return null;
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({
		filters: [{ name: 'Image', extensions: ['jpg', 'png'] }],
		defaultPath: 'kerf-cover.jpg'
	});
	return typeof path === 'string' ? path : null;
}

/** Show a rendered file in the OS file manager (opens its containing folder). */
export async function revealPath(path: string): Promise<void> {
	if (!inTauri()) return;
	await invoke('reveal_path', { path });
}

/** How ready the current cut is for each publishing target. `frame` overrides
 *  the shape it is judged at — the export dialog passes the resolution it is
 *  about to render when that differs from the project frame.
 *
 *  `kerf_core::platform` decides this in the app. The browser harness runs the
 *  mirror in `platforms.ts` over the dev timeline so the panel is explorable
 *  under `bun run dev`. */
export async function platformCheck(frame?: [number, number] | null): Promise<DeliveryCheck[]> {
	if (!inTauri()) {
		const fmt = devTimeline.format;
		const first = sampleAssets[0];
		return checkAll({
			duration: timelineDuration(devTimeline),
			width: frame?.[0] ?? fmt?.width ?? first.streams[0]?.width ?? 1920,
			height: frame?.[1] ?? fmt?.height ?? first.streams[0]?.height ?? 1080,
			has_audio: true,
			has_text: (devTimeline.overlays ?? []).length > 0
		});
	}
	return invoke<DeliveryCheck[]>('platform_check', { width: frame?.[0] ?? null, height: frame?.[1] ?? null });
}

// ---- agent connection (MCP endpoint) ---------------------------------------

/** The local MCP endpoint a connected agent points at (e.g. http://127.0.0.1:7777/mcp). */
export async function mcpEndpoint(): Promise<string> {
	if (!inTauri()) return 'http://127.0.0.1:7777/mcp';
	return invoke<string>('mcp_endpoint');
}

/** Where the endpoint is, and how long ago an agent last used it — `null` when
 *  none ever has. A streamable-HTTP client holds no connection between calls,
 *  so there is no socket to report as open; the panel judges from the age. In
 *  the browser harness there is no server at all, hence `null`. */
export async function agentStatus(): Promise<{ endpoint: string; last_seen_secs: number | null; error: string | null }> {
	if (!inTauri()) return { endpoint: 'http://127.0.0.1:7777/mcp', last_seen_secs: null, error: null };
	return invoke<{ endpoint: string; last_seen_secs: number | null; error: string | null }>('agent_status');
}

// ---- app settings ----------------------------------------------------------

/**
 * Read the preferences in force, resolved against the engine. In the browser
 * harness there is no engine, so it answers from `localStorage` over the
 * webview's own core count — enough to drive the dialog under `bun run dev`.
 */
export async function getSettings(): Promise<SettingsView> {
	if (!inTauri()) {
		return browserSettings({
			cpu_percent: readBrowserCpuPercent(),
			transcribe: readBrowserTranscribe(),
			safe_areas: readBrowserSafeAreas(),
			layout: readBrowserJson(LAYOUT_KEY),
			theme: readBrowserJson(THEME_KEY),
			workspaces: readBrowserJson(WORKSPACES_KEY),
			keybindings: readBrowserJson(KEYBINDINGS_KEY)
		});
	}
	return invoke<SettingsView>('get_settings');
}

/**
 * Persist the fields that changed and put them into force; returns the resolved
 * view. Only the fields in `patch` are written — the backend merges them into
 * the stored file, so a workspaces write cannot carry a stale copy of the theme.
 */
export async function setSettings(patch: Partial<AppSettings>): Promise<SettingsView> {
	if (!inTauri()) {
		try {
			if (patch.cpu_percent !== undefined) {
				const percent = Math.round(Math.min(100, Math.max(MIN_CPU_PERCENT, patch.cpu_percent)));
				localStorage.setItem(CPU_KEY, String(percent));
			}
			if (patch.transcribe !== undefined) localStorage.setItem(TRANSCRIBE_KEY, patch.transcribe ? '1' : '0');
			if (patch.safe_areas !== undefined) localStorage.setItem(SAFE_AREAS_KEY, patch.safe_areas ? '1' : '0');
			if ('layout' in patch) writeBrowserJson(LAYOUT_KEY, patch.layout);
			if ('theme' in patch) writeBrowserJson(THEME_KEY, patch.theme);
			if ('workspaces' in patch) writeBrowserJson(WORKSPACES_KEY, patch.workspaces);
			if ('keybindings' in patch) writeBrowserJson(KEYBINDINGS_KEY, patch.keybindings);
		} catch {
			// A private window with storage blocked still gets a working dialog.
		}
		return getSettings();
	}
	return invoke<SettingsView>('set_settings', { patch });
}

const CPU_KEY = 'kerf.settings.cpuPercent';
const TRANSCRIBE_KEY = 'kerf.settings.transcribe';
const SAFE_AREAS_KEY = 'kerf.settings.safeAreas';
const LAYOUT_KEY = 'kerf.settings.layout';
const THEME_KEY = 'kerf.settings.theme';
const WORKSPACES_KEY = 'kerf.settings.workspaces';
const KEYBINDINGS_KEY = 'kerf.settings.keybindings';
const MIN_CPU_PERCENT = 10;
const DEFAULT_CPU_PERCENT = 75;

function readBrowserCpuPercent(): number {
	try {
		const stored = Number(localStorage.getItem(CPU_KEY));
		if (Number.isFinite(stored) && stored > 0) return stored;
	} catch {
		// Ignore — fall through to the default.
	}
	return DEFAULT_CPU_PERCENT;
}

function readBrowserTranscribe(): boolean {
	try {
		return localStorage.getItem(TRANSCRIBE_KEY) !== '0';
	} catch {
		return true;
	}
}

function readBrowserSafeAreas(): boolean {
	try {
		return localStorage.getItem(SAFE_AREAS_KEY) === '1';
	} catch {
		return false;
	}
}

function readBrowserJson(key: string): unknown | null {
	try {
		const raw = localStorage.getItem(key);
		return raw ? JSON.parse(raw) : null;
	} catch {
		return null;
	}
}

function writeBrowserJson(key: string, value: unknown) {
	if (value == null) localStorage.removeItem(key);
	else localStorage.setItem(key, JSON.stringify(value));
}

function browserSettings(settings: AppSettings): SettingsView {
	const cores = Math.max(1, navigator.hardwareConcurrency || 4);
	return {
		...settings,
		cpu_cores: cores,
		cpu_threads: Math.min(cores, Math.max(1, Math.round((cores * settings.cpu_percent) / 100))),
		cpu_min_percent: MIN_CPU_PERCENT
	};
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

// ---- auto-update (GitHub releases) -----------------------------------------
//
// The desktop app checks the `latest.json` published on the repo's GitHub
// releases (see `plugins.updater.endpoints` in tauri.conf.json). Bundles are
// signed with the project's minisign key and verified against the embedded
// public key before anything is installed, so an update can only come from a
// release signed with that key.
//
// `checkUpdate` keeps the plugin's `Update` handle module-local and hands the
// UI plain data; `installUpdate` then downloads + installs that same handle.

export const RELEASES_URL = 'https://github.com/OrellBuehler/kerf/releases/latest';

let pendingUpdate: { downloadAndInstall: (cb: (e: DownloadEvent) => void) => Promise<void> } | null = null;

type DownloadEvent =
	| { event: 'Started'; data: { contentLength?: number } }
	| { event: 'Progress'; data: { chunkLength: number } }
	| { event: 'Finished' };

/** Bytes fetched so far of an update download; `total` is null if the server sent no length. */
export type UpdateProgress = { downloaded: number; total: number | null };

/** The `true` in the browser harness: `?update=1` makes a fake update available. */
function fakeUpdateRequested(): boolean {
	return typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('update');
}

/** This build's version — `dev` in the browser harness, which has no bundle. */
export async function appVersion(): Promise<string> {
	if (!inTauri()) return 'dev';
	const { getVersion } = await import('@tauri-apps/api/app');
	return getVersion();
}

/**
 * Turn the updater plugin's plumbing errors into something a user can act on.
 * The common one is not a broken feed but a *race*: publishing a release makes
 * it `releases/latest` immediately, while `latest.json` is only attached once
 * every platform's bundle has been built (~25 min), so a check in that window
 * 404s and the plugin reports "Could not fetch a valid release JSON".
 */
function describeFeedFailure(e: unknown): string {
	const raw = e instanceof Error ? e.message : String(e);
	if (/valid release JSON/i.test(raw)) {
		return "The newest release hasn't published its update manifest yet — its installers are probably still building. Try again in a few minutes, or grab it from the release page.";
	}
	return raw;
}

/**
 * Ask GitHub whether a newer signed release exists. Returns `null` when this
 * build is current (or when there is nothing to update, as in the browser).
 * Throws if the endpoint is unreachable — callers doing a silent startup check
 * should swallow that; a user-initiated check should show it.
 */
export async function checkUpdate(): Promise<UpdateInfo | null> {
	if (!inTauri()) {
		if (!fakeUpdateRequested()) return null;
		pendingUpdate = {
			async downloadAndInstall(cb) {
				cb({ event: 'Started', data: { contentLength: 40_000_000 } });
				for (let i = 0; i < 20; i++) {
					await new Promise((r) => setTimeout(r, 100));
					cb({ event: 'Progress', data: { chunkLength: 2_000_000 } });
				}
				cb({ event: 'Finished' });
			}
		};
		return {
			version: '0.99.0',
			current_version: 'dev',
			date: null,
			notes: 'Synthetic update from the browser dev harness (`?update=1`).'
		};
	}
	const { check } = await import('@tauri-apps/plugin-updater');
	const update = await check().catch((e) => {
		throw new Error(describeFeedFailure(e));
	});
	if (!update) {
		pendingUpdate = null;
		return null;
	}
	pendingUpdate = update;
	return {
		version: update.version,
		current_version: update.currentVersion,
		date: update.date ?? null,
		notes: update.body ?? null
	};
}

/**
 * Download and install the update the last `checkUpdate` found, reporting
 * progress. Resolves once the new version is staged; the app must then
 * [`relaunchApp`] to run it. On Linux this only works for the AppImage build —
 * a `.deb` / `.rpm` install rejects, and the caller should fall back to the
 * release page.
 */
export async function installUpdate(onProgress?: (p: UpdateProgress) => void): Promise<void> {
	if (!pendingUpdate) throw new Error('no update pending — check for updates first');
	// The installer can end this process outright (on Windows the plugin exits
	// without a normal shutdown), so the logfile's last line would otherwise look
	// like a crash. This one says it was the update.
	logFrontend('info', 'installing an update; the app may exit without a normal shutdown', 'updater');
	let downloaded = 0;
	let total: number | null = null;
	await pendingUpdate.downloadAndInstall((e) => {
		if (e.event === 'Started') total = e.data.contentLength ?? null;
		else if (e.event === 'Progress') downloaded += e.data.chunkLength;
		else downloaded = total ?? downloaded;
		onProgress?.({ downloaded, total });
	});
}

/** Restart into the freshly installed version. No-op outside Tauri. */
export async function relaunchApp(): Promise<void> {
	if (!inTauri()) return;
	const { relaunch } = await import('@tauri-apps/plugin-process');
	await relaunch();
}

/** Open the GitHub releases page — the manual fallback when an install can't run. */
export async function openReleases(): Promise<void> {
	if (!inTauri()) {
		window.open(RELEASES_URL, '_blank');
		return;
	}
	const { openUrl } = await import('@tauri-apps/plugin-opener');
	await openUrl(RELEASES_URL);
}
