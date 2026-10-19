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
	AnalysisKind,
	AnalysisStatus,
	AssetMetadata,
	AssetRemoval,
	AutoAnalysis,
	PreviewSource,
	ProxySize,
	ProxyStatus,
	AudioEffect,
	CaptionFormat,
	CaptionImportRequest,
	CaptionImportResult,
	CaptionOptions,
	Clip,
	ClipCut,
	ClipMove,
	Mask,
	MissingAsset,
	Color,
	Grade,
	LutInfo,
	Delivery,
	PlatformReport,
	EditSource,
	ExportOptions,
	ExportedFile,
	InterchangeFormat,
	MarkerListFormat,
	MarkerPatch,
	SubtitleFormat,
	ExportProgress,
	Filmstrip,
	GpuPreviewStatus,
	ImportOutcome,
	ImportProgress,
	ModelProgress,
	Easing,
	Keyframe,
	Property,
	PropertyKey,
	PropertyTrack,
	LaunchRequest,
	RecoveryInfo,
	Levels,
	AnalysisProgress,
	DiffEntry,
	MasterBus,
	MusicFit,
	MusicFitted,
	MusicProgress,
	MusicStatus,
	ComposeRequest,
	ComposeResult,
	AlignReport,
	BandBalance,
	BandBalanceRequest,
	BandWindow,
	MusicScore,
	ScoreAlign,
	SectionKind,
	PreviewBoundsReport,
	PlatformTarget,
	PlayAsk,
	PreviewDraft,
	PreviewFrameResult,
	PreviewOrder,
	Projection,
	Reframe,
	ReframeKeyframe,
	RelinkCheck,
	Relinked,
	RenderBar,
	Revision,
	ScopeGridSize,
	ScopeKind,
	ScopeMatrix,
	Scopes,
	AppSettings,
	McpConnectionInfo,
	McpSettingsPatch,
	TimeDisplay,
	SettingsPatch,
	SettingsView,
	SmartCropProgress,
	SplitSide,
	StagedEdit,
	StagedUndone,
	SubsetApplied,
	StemsProgress,
	StemsResult,
	SyncedByAudio,
	AudioSyncProgress,
	StemsStatus,
	StreamKind,
	SvgCheck,
	SvgElementId,
	SvgLayout,
	SvgOverlayPatch,
	SvgPresetInfo,
	SvgPresetKind,
	SvgPresetParamsInput,
	SvgSource,
	Task,
	TemplateControl,
	TemplateEdit,
	TextBox,
	TitleTextForm,
	TemplateList,
	TemplateSaved,
	TextKeyframe,
	TitleClipPatch,
	TextOverlay,
	Timeline,
	TimelineDiff,
	Track,
	Transform,
	Transition,
	TranscriptionStatus,
	TranscriptSegment,
	UiContext,
	UpdateInfo,
	VideoEffect,
	VoiceoverProgress,
	VoiceoverRequest,
	VoiceoverResult,
	VoiceoverStatus,
	WaveformRange
} from './types';
import { normalizeGrade } from './grade';
import { unsavedChoice, type UnsavedChoice, type UnsavedCopy } from './confirm-copy';
import { clipDuration, DEFAULT_COLOR, DEFAULT_REFRAME, DEFAULT_TRANSFORM } from './types';
import { alignCutsToBeats, beatGrid, defaultBeatTolerance } from './beats';
import { claudeCodeCommand, clientUrl, genericJsonConfig } from './mcp-connect';
import { fileTooLarge, importCaptionsInto, MAX_CAPTION_FILE_BYTES, parseFormat, resolveBase } from './caption-import';
import { baseName, CAPTION_EXTENSIONS } from './caption-import-ui';
import { easingProblem, insertKeyframe } from './easing';
import {
	channelOf,
	clearTransformAnimation,
	insertPropertyKey,
	isMaskProperty,
	keyableProblem,
	poseTransform,
	pruneChannels,
	propertyKeysProblem,
	propertyKeysShifted,
	propertyLabel,
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
import { dissolveAllOrphans, linkPartners, withLinkPartners } from './link-groups';
import * as ops from './link-ops';
import { runEdit } from './link-ops';
import { sourceLimits } from './trim-tools';
import { clampCeiling, clampDuckDepth, clampMasterVolume, DEFAULT_MASTER, estimateLevels, masterOf } from './levels';
import { toFixedEven } from './format-fixed';
import { fitMusicOnto, musicFitInputs, phraseMatches, tempoFromGrid, type FitSources } from './music-fit';
import {
	MUSIC_STYLES,
	barSeconds,
	planScoreToPicture,
	resolvedChords,
	scoreDuration,
	scoreTitle,
	sectionLabels,
	sectionSpans,
	validateScore
} from './music-score';
import { musicBed, musicStart, putMusic, rehearseMusic } from './music-place';
import { checkBandRequest, checkComposeRequest } from './music-wire';
import { balanceNotes, checkSections, equalWindows, phraseWindows, clipWindows, synthBalance } from './band-balance';
import { sampleMusicAnalysis, SAMPLE_MUSIC_DURATION } from './sample-music';
import { exportSpan, report as platformReport, TARGETS } from './platforms';
import { centeredCrop } from './smart-crop';
import { synthWaveformRange } from './sample-waveform';
import { analysisKindDone, ALL_KINDS } from './analysis-steps';
import { sampleFilmstrip } from './sample-filmstrip';
import { synthPcm } from './sample-audio';
import { sampleFrameJpeg } from './sample-frame';
import { harnessScopes } from './scopes-view';
import { harnessRenderBar } from './render-bar';
import { renderedFps } from './timecode';
import { playbackFps } from './playback-sync';
import { parsePlaybackMessage } from './frame-transport';
import { playbackId } from './playback-id';
import { entryId, mergeSubset, subsetMessage } from './staged-subset';
import { captionsForTimeline, resolveCaptions } from './captions';
import { estimateWords } from './transcript';
import { describeError, logFrontend } from './log';
import { parseLaunchRequest } from './launch';
import { recoveryChoice, type RecoveryChoice, type RecoveryCopy } from './recovery';
import { DEV_MODEL_BYTES, STEM_NAMES, STEMS_CANCELLED, devStemAssets, placeStems } from './stems';
import {
	addSvgOverlayTo,
	removeSvgOverlayFrom,
	svgPresetArt,
	svgPresetInfos,
	updateSvgOverlayIn,
	validateSvgSource
} from './svg-dev';
import {
	applyTemplateClipTo,
	applyTemplateTo,
	deleteTemplateDev,
	exportTemplateDev,
	importTemplateDev,
	latestExportedTemplate,
	listTemplatesDev,
	saveTemplateFrom,
	setTemplateValuesIn,
	svgElementIdsOf,
	templateImageValueOf,
	templatePosterDev
} from './template-dev';
import { checkTitleSpan, isTitleClip, placeTitleClip, titleClip, titleOf } from './title-clips';
import { convertTitleTextIn, setTitleTextBoxIn } from './title-text';
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
	},
	// Audio only, with a fitted bar grid: the song "Fit to video" and the ruler's bars are explored with.
	{
		id: '33333333-3333-3333-3333-333333333333',
		path: '/samples/music.mp3',
		name: 'music.mp3',
		duration: SAMPLE_MUSIC_DURATION,
		streams: [{ index: 0, kind: 'audio', codec: 'mp3', sample_rate: 44100, channels: 2 }],
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
			{ start: 0, end: 5.5, text: 'Welcome back to the channel.', speaker: 'S1' },
			{ start: 5.5, end: 12.5, text: 'Today we are talking about non-destructive editing.', speaker: 'S1' },
			{ start: 14, end: 22, text: 'The agent watches the footage with you and proposes a cut.', speaker: 'S2' }
		],
		loudness: { integrated_lufs: -16.2, loudness_range: 6.4, true_peak_dbtp: -1.5, threshold_lufs: -26.5 },
		onsets: [0.5, 1.2, 2.0, 2.8, 3.6, 5.6],
		tempo: { bpm: 120, beats: [0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0], confidence: 0.62, downbeats: [] },
		audio_class: { class: 'speech', confidence: 0.71 },
		music: null
	},
	[sampleAssets[1].id]: {
		asset_id: sampleAssets[1].id,
		silence_segments: [],
		scene_changes: [0, 8, 20, 33],
		transcript: [],
		loudness: { integrated_lufs: -11.8, loudness_range: 9.1, true_peak_dbtp: -0.8, threshold_lufs: -22.0 },
		onsets: [0.4, 0.9, 1.5, 2.1, 2.7, 3.3, 3.9],
		tempo: { bpm: 128, beats: [0.23, 0.7, 1.17, 1.64, 2.11, 2.58, 3.05, 3.52], confidence: 0.78, downbeats: [] },
		audio_class: { class: 'music', confidence: 0.83 },
		music: null
	},
	[sampleAssets[2].id]: sampleMusicAnalysis(sampleAssets[2].id)
};

/** `?missing=1` in the browser harness: the b-roll and the song have no file behind them, as after a
 *  drive was unplugged or the media was moved, so the missing-media prompt and relink are explorable. */
function wantsMissing(): boolean {
	return typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('missing');
}

/** The harness assets whose "file" is gone. */
const devMissing = new Set<string>(wantsMissing() ? [sampleAssets[1].id, sampleAssets[2].id] : []);

// The starter cut is the *detached-and-linked* shape linked A/V produces: the interview's
// sound is its own clip on A1, linked to the picture on V1, whose own sound is muted
// (`source_audio: false`) — so it is heard once, and a move, trim, split or delete of
// either carries to both. (Before detaching existed this was the interview's whole audio
// on A1 *under* a picture still playing the same sound: heard twice.) The Rust seed
// (`Project::sample`) detaches the same way and then unlinks, because the kerf-core tests
// built on it edit one clip at a time; the harness is for exploring, so it keeps the link.
const SAMPLE_LINK = 'sample-link-interview';

/** `?music=1` in the browser harness lays the sample song on an audio track of its own, 0 to its end,
 *  so the bar ticks on the ruler and *Fit to video* are one right-click away. (Without it the song is
 *  in the library, ready to be added.) */
function wantsSampleMusic(): boolean {
	return typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('music');
}

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
		},
		...(wantsSampleMusic()
			? [
					{
						id: 'a2',
						kind: 'audio' as const,
						name: 'A2',
						clips: [
							{
								id: 'c4',
								asset_id: sampleAssets[2].id,
								source_in: 0,
								source_out: SAMPLE_MUSIC_DURATION,
								timeline_start: 0,
								volume: 0.8,
								fade_in: 0,
								fade_out: 0
							}
						]
					}
				]
			: [])
	]
};

/** `?empty=1` in the browser harness: a first run — an empty library and a cut with its two empty tracks. */
function wantsEmpty(): boolean {
	return typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('empty');
}

// A representative queue spanning the task lifecycle, mirroring the Rust seed. The *ready* task is the one
// whose proposal `?staged=1` stages (`seedDevStaged` ties them together): a ready task with nothing staged
// under it is a card that offers to apply a change that does not exist. An empty project has no queue.
const now = () => new Date().toISOString();
let devTasks: Task[] = wantsEmpty()
	? []
	: [
			{
				id: 't1',
				prompt: 'Assemble a rough cut from the interview',
				status: 'done',
				result: 'Kept 6 segments; cut 2 fillers and 14 silences (−1:48)',
				created_at: now(),
				updated_at: now()
			},
			...(fakeStagedRequested()
				? [
						{
							id: 't2',
							prompt: 'Tighten the intro and remove filler words',
							status: 'ready' as const,
							result: 'Staged 3 cuts; review on the timeline',
							created_at: now(),
							updated_at: now()
						}
					]
				: []),
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

type DevRevision = { seq: number; label: string; source: EditSource; snapshot: Timeline; created_at: string };
let devHistory: DevRevision[] = [
	{ seq: 0, label: 'Initial state', source: 'system', snapshot: structuredClone(sampleTimeline), created_at: now() }
];
let devHead = 0;

if (wantsEmpty()) {
	sampleAssets.length = 0;
	devTimeline = { tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: [] }, { id: 'a1', kind: 'audio', name: 'A1', clips: [] }] };
	devHistory = [{ seq: 0, label: 'Initial state', source: 'system', snapshot: structuredClone(devTimeline), created_at: now() }];
}

/** `?long=N` in the browser harness: a cut of N one-second pieces of the interview on V1, their sound
 *  on A1 — the length at which a lane draws only the clips in view (`timeline-cull.ts`). */
function wantsLongCut(): number {
	if (typeof window === 'undefined') return 0;
	const n = Number(new URLSearchParams(window.location.search).get('long'));
	return Number.isFinite(n) ? Math.min(Math.max(0, Math.floor(n)), 5000) : 0;
}

const longCut = wantsLongCut();
if (longCut > 0 && sampleAssets.length > 0) {
	const piece = (kind: 'v' | 'a', i: number) => ({
		id: `${kind}${i}`,
		asset_id: sampleAssets[0].id,
		source_in: i % 110,
		source_out: (i % 110) + 1,
		timeline_start: i,
		volume: 1,
		fade_in: 0,
		fade_out: 0
	});
	const pieces = (kind: 'v' | 'a') => Array.from({ length: longCut }, (_, i) => piece(kind, i));
	devTimeline = {
		tracks: [
			{ id: 'v1', kind: 'video', name: 'V1', clips: pieces('v') },
			{ id: 'a1', kind: 'audio', name: 'A1', clips: pieces('a') }
		]
	};
	devHistory = [{ seq: 0, label: 'Initial state', source: 'system', snapshot: structuredClone(devTimeline), created_at: now() }];
}

/** Append a snapshot of the current dev timeline, dropping any redo branch. */
function recordDev(label: string) {
	devHistory = devHistory.slice(0, devHead + 1);
	devHead += 1;
	devHistory.push({ seq: devHead, label, source: 'user', snapshot: snapshot(), created_at: now() });
}

function devRestore(seq: number): Timeline {
	const rev = devHistory.find((r) => r.seq === seq);
	if (rev) {
		// History does not bring removed media back (`Project::restore`): a step with a clip of an asset that
		// is no longer in the library is refused, naming it.
		const gone = [
			...new Set(
				rev.snapshot.tracks
					.flatMap((t) => t.clips)
					.filter((c) => !assetById(c.asset_id))
					.map((c) => devRemovedNames.get(c.asset_id) ?? c.asset_id)
			)
		];
		if (gone.length > 0) {
			throw new Error(
				`that step uses media that was removed from the library (${gone.join(', ')}) — undo and redo cannot bring it back; import the file again to cut from it, or go back to a step from before it was used`
			);
		}
		devTimeline = structuredClone(rev.snapshot);
		devHead = seq;
	}
	return snapshot();
}

/** The names of the media removed from the harness library, for the refusal above. */
const devRemovedNames = new Map<string, string>();

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
		hasAudio: (assetId) => !!assetById(assetId)?.streams.some((s) => s.kind === 'audio'),
		asset: (assetId) => assetById(assetId)
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

let hwEncodersKnown: string[] | null = null;

/** `?hw=h264_amf,hevc_amf` makes the browser harness answer as a machine with those encoders. */
function harnessHwEncoders(): string[] {
	try {
		return (new URLSearchParams(location.search).get('hw') ?? '').split(',').filter(Boolean);
	} catch {
		return [];
	}
}

/** GPU video encoders the backend verified usable (none in the browser harness, unless `?hw=`
 *  names some). The backend probes once per process, so the answer is kept: a reopened export
 *  dialog starts with it. */
export async function hwEncoders(): Promise<string[]> {
	hwEncodersKnown = inTauri() ? await invoke<string[]>('hw_encoders') : harnessHwEncoders();
	return hwEncodersKnown;
}

/** What [`hwEncoders`] last answered, `[]` before it has. */
export function knownHwEncoders(): string[] {
	return hwEncodersKnown ?? [];
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

// ---- library: remove / relink / missing media -----------------------------------

/**
 * Take an asset out of the library. Refused while a clip of the cut (or of an open proposal) uses it,
 * unless `force`, which first removes those clips as one `Remove media` revision. Its analysis goes with
 * it; the backend also cleans up its proxies and waveform cache. Resolves to the asset as it was, how many
 * clips went with it and the refreshed cut.
 */
export async function removeAsset(assetId: string, force = false): Promise<AssetRemoval> {
	if (!inTauri()) {
		const asset = assetById(assetId);
		if (!asset) throw new Error(`asset not found: ${assetId}`);
		const uses = (tl: Timeline) => tl.tracks.flatMap((t) => t.clips).filter((c) => c.asset_id === assetId);
		const live = uses(devTimeline);
		const proposed = devStaged ? uses(devStaged.timeline) : [];
		const inUse = new Set([...live, ...proposed].map((c) => c.id));
		if (inUse.size > 0 && !force) {
			throw new Error(
				`asset ${assetId} is used by ${inUse.size} clip(s) on the timeline or in the proposal — remove them first, or remove with force`
			);
		}
		if (live.length > 0) {
			for (const t of devTimeline.tracks) t.clips = t.clips.filter((c) => c.asset_id !== assetId);
			dissolveAllOrphans(devTimeline);
			recordDev(`Remove media ${asset.name}`);
		}
		if (devStaged && proposed.length > 0) {
			const gone = new Set(proposed.map((c) => c.id));
			for (const t of devStaged.timeline.tracks) t.clips = t.clips.filter((c) => c.asset_id !== assetId);
			dissolveAllOrphans(devStaged.timeline);
			// Its remembered steps still show the asset.
			devStaged.steps = [];
			devStaged.edit.undo_depth = 0;
			devStaged.edit.diff.entries = devStaged.edit.diff.entries.filter((e) => !e.clip_id || !gone.has(e.clip_id));
		}
		sampleAssets.splice(sampleAssets.indexOf(asset), 1);
		devRemovedNames.set(assetId, asset.name);
		delete sampleAnalysis[assetId];
		delete devProxy[assetId];
		devMissing.delete(assetId);
		return { asset: structuredClone(asset), clips_removed: live.length, timeline: snapshot() };
	}
	return invoke<AssetRemoval>('remove_asset', { assetId, force });
}

const IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'webp', 'gif', 'bmp', 'tiff', 'tif', 'insp'];
const SOUND_EXTENSIONS = ['wav', 'mp3', 'm4a', 'aac'];

/** What the harness judges a relink by: the file's name and extension. Throws what the backend refuses with. */
function harnessRelinkPlan(assetId: string, newPath: string) {
	const asset = assetById(assetId);
	if (!asset) throw new Error(`asset not found: ${assetId}`);
	if (!newPath.startsWith('/') && !/^[A-Za-z]:[\\/]/.test(newPath)) {
		throw new Error(`invalid argument: ${newPath} must be an absolute path to a file`);
	}
	const ext = newPath.split('.').pop()?.toLowerCase() ?? '';
	const image = IMAGE_EXTENSIONS.includes(ext);
	const sound = SOUND_EXTENSIONS.includes(ext);
	const had = { video: asset.streams.some((s) => s.kind === 'video'), audio: asset.streams.some((s) => s.kind === 'audio') };
	const is = { video: !image && !sound, audio: sound || (!image && had.audio) };
	const describe = (image: boolean, video: boolean, audio: boolean) =>
		image ? 'a still image' : video && audio ? 'picture and sound' : video ? 'picture only' : audio ? 'sound only' : 'no audio or video';
	if (image || is.video !== had.video || is.audio !== had.audio) {
		throw new Error(
			`invalid argument: that file is ${describe(image, is.video, is.audio)} but "${asset.name}" is ${describe(false, had.video, had.audio)} — pick the same media`
		);
	}
	const other = sampleAssets.find((a) => a.path === newPath && a.id !== assetId);
	if (other) throw new Error(`invalid argument: ${newPath} is already in the library as "${other.name}"`);
	return { asset, sameMedia: baseName(newPath) === baseName(asset.path) };
}

/**
 * Point an asset at another file: the media was moved, renamed or restored from a backup. The file must be
 * the same kind of media (same streams, about the same length); the asset keeps its id, its place in the
 * library and every clip cut from it. The analysis is kept only when the file is the old media moved
 * (`analysis_kept`); any other compatible file is a different recording and its analysis is dropped.
 * The browser harness judges by file name and extension.
 */
export async function relinkAsset(assetId: string, newPath: string): Promise<Relinked> {
	if (!inTauri()) {
		const { asset, sameMedia } = harnessRelinkPlan(assetId, newPath);
		const previous = asset.path;
		asset.path = newPath;
		devMissing.delete(assetId);
		if (!sameMedia) delete sampleAnalysis[assetId];
		return { asset: structuredClone(asset), analysis_kept: sameMedia, previous_path: previous };
	}
	return invoke<Relinked>('relink_asset', { assetId, newPath });
}

/**
 * What `relinkAsset` to this file would cost, asked first and changing nothing: the same refusals (the file is not
 * the same kind of media, is taken, is not there), and whether the asset's analysis would survive
 * (`analysis_kept`) and whether it has any to lose (`has_analysis`). Reads both files in the desktop app.
 */
export async function checkRelink(assetId: string, newPath: string): Promise<RelinkCheck> {
	if (!inTauri()) {
		const { sameMedia } = harnessRelinkPlan(assetId, newPath);
		return { analysis_kept: sameMedia, has_analysis: !!sampleAnalysis[assetId] };
	}
	return invoke<RelinkCheck>('check_relink', { assetId, newPath });
}

/** The assets whose source file is no longer on disk, each with how many clips of the cut use it — what a
 *  "relink media" prompt lists. The browser harness has none unless `?missing=1` says so. */
export async function missingAssets(): Promise<MissingAsset[]> {
	if (!inTauri()) {
		return sampleAssets
			.filter((a) => devMissing.has(a.id))
			.map((a) => ({
				asset_id: a.id,
				name: a.name,
				path: a.path,
				clips: devTimeline.tracks.flatMap((t) => t.clips).filter((c) => c.asset_id === a.id).length
			}));
	}
	return invoke<MissingAsset[]>('missing_assets');
}

/** Open a native file picker for the file to relink `name` to (media files only); `null` if cancelled. The
 *  browser harness has no file system and answers with a plausible restored path (`?relinkto=other.mp4` names a
 *  different file, to see what relinking to another recording asks). */
export async function pickRelinkPath(name?: string): Promise<string | null> {
	if (!inTauri()) {
		const to = typeof window === 'undefined' ? null : new URLSearchParams(window.location.search).get('relinkto');
		return `/restored/${to ?? name ?? 'media.mp4'}`;
	}
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: false,
		title: name ? `Locate ${name}` : 'Locate the media file',
		filters: [{ name: 'Media', extensions: MEDIA_EXTENSIONS }]
	});
	return typeof selected === 'string' ? selected : null;
}

/** Which of `paths` exist on disk, in order — what the export dialog asks before it overwrites a file. In the
 *  browser harness nothing exists, unless `?exists=1` says everything does (to see the overwrite prompt). */
export async function pathsExist(paths: string[]): Promise<boolean[]> {
	if (!inTauri()) {
		const all = typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('exists');
		return paths.map(() => all);
	}
	if (paths.length === 0) return [];
	return invoke<boolean[]>('paths_exist', { paths });
}

// ---- import / analysis -----------------------------------------------------

export async function importAsset(path: string): Promise<Asset> {
	if (!inTauri()) throw new Error('import is only available in the desktop app');
	return invoke<Asset>('import_asset', { path });
}

/** Stop every import in flight (a 360 pair being stitched is the one that takes long enough to stop). The
 *  imports reject with `import cancelled` and leave no half-written stitch; progress arrives as `onImportProgress`. */
export async function cancelImport(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_import');
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

/** Ask before something irreversible, with the two ends named as what they do (`labels`) — "Reset
 *  all" / "Cancel" rather than Yes / No; there is no way to ask without naming them. `window.confirm`
 *  is replaced by the dialog plugin in the desktop app and returns a promise there, so it can't be
 *  used synchronously — hence this. */
export async function confirmAction(message: string, title: string, labels: { ok: string; cancel: string }): Promise<boolean> {
	if (!inTauri()) return window.confirm(message);
	const { message: show, ask } = await import('@tauri-apps/plugin-dialog');
	try {
		return (await show(message, { title, kind: 'warning', buttons: { ok: labels.ok, cancel: labels.cancel } })) === labels.ok;
	} catch {
		// A platform without custom buttons still gets the question, with the plugin's own.
		return ask(message, { title, kind: 'warning', okLabel: labels.ok, cancelLabel: labels.cancel });
	}
}

/** The unsaved-project prompt: Save…, Don't save and Cancel (`confirm-copy.ts`). Where the native
 *  dialog cannot show three named buttons it asks the two-button question and Save is not on offer;
 *  closing the dialog is always Cancel. The browser harness has `window.confirm`, which can only
 *  discard or cancel. */
export async function askUnsaved(copy: UnsavedCopy): Promise<UnsavedChoice> {
	if (!inTauri()) return window.confirm(copy.message) ? 'discard' : 'cancel';
	const { message: show } = await import('@tauri-apps/plugin-dialog');
	try {
		const answer = await show(copy.message, {
			title: copy.title,
			kind: 'warning',
			buttons: { yes: copy.save, no: copy.discard, cancel: copy.cancel }
		});
		return unsavedChoice(answer, copy);
	} catch {
		return (await confirmAction(copy.message, copy.title, { ok: copy.discard, cancel: copy.cancel })) ? 'discard' : 'cancel';
	}
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

/** Set the OS window's title (the taskbar and the window switcher read it; Kerf's own title bar is drawn
 *  under the native caption). In the browser harness it is the tab's title. A failure is not worth a toast:
 *  the title is a courtesy, not state. */
export async function setWindowTitle(title: string): Promise<void> {
	if (!inTauri()) {
		if (typeof document !== 'undefined') document.title = title;
		return;
	}
	const { getCurrentWindow } = await import('@tauri-apps/api/window');
	await getCurrentWindow().setTitle(title);
}

/** Quit Kerf from a menu. The window's own close button is guarded by
 *  `onWindowCloseRequested` (a project that was never saved is asked about first);
 *  this is the same question put to the same window, then the same `destroy` that
 *  guard ends with — the capability the app already has, so no new one.
 *  A no-op in the browser harness, which has no window of its own to close. */
export async function quitApp(needsConfirm: () => boolean, confirm: () => Promise<boolean>): Promise<void> {
	if (!inTauri()) return;
	if (needsConfirm()) {
		let quit = false;
		try {
			quit = await confirm();
		} catch {
			// A dialog that fails to show must not make the app unquittable.
			quit = true;
		}
		if (!quit) return;
	}
	const { getCurrentWindow } = await import('@tauri-apps/api/window');
	await getCurrentWindow().destroy();
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

// ---- detached panels --------------------------------------------------------
//
// A panel moved into a window of its own is dockview's popout, opened with
// `window.open`. The desktop app only answers that for a window the page announced
// first (`popout.rs`): where it should open, how big, and the colour to paint it until
// the panel is in it. A plain browser (`bun run dev`) opens popups itself and needs no
// announcement, so every call here is a no-op there.

/** A window's place in `screenX` / `innerWidth` pixels. */
export interface PopoutRect {
	x: number;
	y: number;
	width: number;
	height: number;
}

export interface PopoutRequest {
	/** Where it should open — a layout being restored; absent for a panel detached by hand. */
	rect?: PopoutRect | null;
	/** How big, when there is no rectangle: the panel's own size. */
	size?: { width: number; height: number } | null;
	/** `#rrggbb`, the window's backdrop until the panel is in it. */
	background?: string | null;
}

/** A window that was announced: its label, and where it will open (`null`: where the
 *  platform puts it) — not always where it was asked to, when that was off every screen. */
export interface PopoutAnnounced {
	label: string;
	position: [number, number] | null;
}

/** Announce a window that `window.open` is about to ask for; `null` in a browser. Rejects
 *  when the backend will not have another. */
export async function popoutExpect(request: PopoutRequest): Promise<PopoutAnnounced | null> {
	if (!inTauri()) return null;
	return invoke<PopoutAnnounced>('popout_expect', { request: { rect: null, size: null, background: null, ...request } });
}

/** Move a detached window (the correction after a restore, see `popout.svelte.ts`). */
export async function popoutMove(label: string, x: number, y: number): Promise<void> {
	if (!inTauri()) return;
	await invoke('popout_move', { label, x, y });
}

/** The announced window will not be opened after all. */
export async function popoutCancel(label: string): Promise<void> {
	if (!inTauri()) return;
	await invoke('popout_cancel', { label });
}

/** Destroy a detached panel's window (WKWebView cannot be closed by the page). */
export async function closePopout(label: string): Promise<void> {
	if (!inTauri()) return;
	await invoke('close_popout', { label });
}

/** Raise a detached panel's window. */
export async function popoutFocus(label: string): Promise<void> {
	if (!inTauri()) return;
	await invoke('popout_focus', { label });
}

/** A detached panel's window was destroyed, however: `cb` gets its label. */
export async function onPopoutClosed(cb: (label: string) => void): Promise<() => void> {
	if (!inTauri()) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<string>('popout-closed', (e) => cb(e.payload));
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

/** Untitled projects a crashed Kerf left behind, newest first. The browser harness has none. */
export async function listRecoveries(): Promise<RecoveryInfo[]> {
	if (!inTauri()) return [];
	return invoke<RecoveryInfo[]>('list_recoveries');
}

/** Make a left-behind project the open one (still untitled); `false` outside Tauri. */
export async function recoverProject(id: string): Promise<boolean> {
	if (!inTauri()) return false;
	await invoke('recover_project', { id });
	return true;
}

/** The user chose "Don't save" as the app closes: delete this project's recovery session at
 *  exit instead of offering it at the next launch. */
export async function discardRecoveryAtExit(): Promise<void> {
	if (!inTauri()) return;
	await invoke('discard_recovery_at_exit');
}

/** Delete a left-behind project. */
export async function discardRecovery(id: string): Promise<void> {
	if (!inTauri()) return;
	await invoke('discard_recovery', { id });
}

/** The recovery prompt: Recover, Discard and Not now (`recovery.ts`). Where the native dialog
 *  cannot show three named buttons it asks Recover / Not now, so Discard is never the fallback;
 *  closing the dialog is Not now. The browser harness has `window.confirm`. */
export async function askRecovery(copy: RecoveryCopy): Promise<RecoveryChoice> {
	if (!inTauri()) return window.confirm(copy.message) ? 'recover' : 'later';
	const { message: show } = await import('@tauri-apps/plugin-dialog');
	try {
		const answer = await show(copy.message, {
			title: copy.title,
			kind: 'info',
			buttons: { yes: copy.recover, no: copy.discard, cancel: copy.later }
		});
		return recoveryChoice(answer, copy);
	} catch {
		return (await confirmAction(copy.message, copy.title, { ok: copy.recover, cancel: copy.later })) ? 'recover' : 'later';
	}
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

const RECENT_KEY = 'kerf.settings.recentProjects';

/** `?recent=1` in the browser harness seeds a recent-projects list to explore the menu with. */
function harnessRecent(): string[] {
	try {
		const stored = localStorage.getItem(RECENT_KEY);
		if (stored !== null) return (JSON.parse(stored) as unknown[]).filter((p): p is string => typeof p === 'string');
		if (new URLSearchParams(window.location.search).has('recent')) {
			return ['/projects/summer-trip.kerf', '/projects/interview-cut.kerf', '/projects/showreel.kerf'];
		}
	} catch {
		// Storage blocked or the list is not JSON: no recent projects.
	}
	return [];
}

/** The `.kerf` files opened or saved most recently, newest first (at most 10), the ones whose file is gone
 *  dropped and forgotten. */
export async function recentProjects(): Promise<string[]> {
	if (!inTauri()) return harnessRecent();
	return invoke<string[]>('recent_projects');
}

/** Open one of the recent projects; resolves to its path. A file that has since gone rejects (and is
 *  dropped from the list). The caller asks about unsaved work first, as for any open. */
export async function openRecent(path: string): Promise<string | null> {
	if (!inTauri()) return null;
	return (await invoke<string | null>('open_recent', { path })) ?? null;
}

/** Forget every recent project. */
export async function clearRecentProjects(): Promise<void> {
	if (!inTauri()) {
		try {
			localStorage.setItem(RECENT_KEY, '[]');
		} catch {
			// Storage blocked: nothing was remembered anyway.
		}
		return;
	}
	return invoke<void>('clear_recent_projects');
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

/** Hand a text file to the browser's download manager — what the harness does where the desktop app asks where to
 *  save. Resolves to the file's name. */
export function downloadTextFile(contents: string, name: string, mime = 'text/plain'): string {
	const url = URL.createObjectURL(new Blob([contents], { type: mime }));
	const a = document.createElement('a');
	a.href = url;
	a.download = name;
	a.click();
	setTimeout(() => URL.revokeObjectURL(url), 1000);
	return name;
}

/** Save a theme as a JSON file; resolves to where it went, `null` if
 *  cancelled. The browser harness hands the file to the download manager. */
export async function exportThemeFile(contents: string, defaultName: string): Promise<string | null> {
	if (!inTauri()) return downloadTextFile(contents, defaultName, 'application/json');
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({ filters: [{ name: 'Kerf theme', extensions: ['json'] }], defaultPath: defaultName });
	if (typeof path !== 'string') return null;
	return invoke<string>('write_text_file', { path, contents });
}

/**
 * Run analysis on an asset and cache what it finds. `steps` names the kinds to run (silence,
 * scenes, loudness, rhythm, transcript); only those run and their results are merged into
 * what is cached, the other kinds left alone. Omitted, it runs the kinds the user has
 * switched on in Settings › Analysis. In the browser harness the steps are simulated.
 */
export async function analyzeAsset(assetId: string, steps?: readonly AnalysisKind[]): Promise<AssetAnalysis> {
	if (!inTauri()) return devAnalyze(assetId, steps);
	return invoke<AssetAnalysis>('analyze_asset', { assetId, steps: steps ? [...steps] : null });
}

const devProgressListeners = new Set<(p: AnalysisProgress) => void>();

/** Subscribe to `analysis-progress` events: the step an analysis pass is on, with a fraction where the backend can
 *  say one. Returns an unlisten fn. The browser harness emits a few ticks per step of its simulated run. */
export async function onAnalysisProgress(cb: (p: AnalysisProgress) => void): Promise<() => void> {
	if (!inTauri()) {
		devProgressListeners.add(cb);
		return () => void devProgressListeners.delete(cb);
	}
	const { listen } = await import('@tauri-apps/api/event');
	return listen<AnalysisProgress>('analysis-progress', (e) => cb(e.payload));
}

/** The per-kind analysis status of every asset (done / not run / running / failed / off). */
export async function getAnalysisStatuses(): Promise<AnalysisStatus[]> {
	if (!inTauri()) return sampleAssets.map((a) => devAnalysisStatus(a.id));
	return invoke<AnalysisStatus[]>('analysis_statuses');
}

/** Subscribe to `analysis-status` events: an asset's per-kind status after each step starts and
 *  when a run ends — the GUI's own and an agent's. Returns an unlisten fn. */
export async function onAnalysisStatus(cb: (s: AnalysisStatus) => void): Promise<() => void> {
	if (!inTauri()) {
		devAnalysisListeners.add(cb);
		return () => void devAnalysisListeners.delete(cb);
	}
	const { listen } = await import('@tauri-apps/api/event');
	return listen<AnalysisStatus>('analysis-status', (e) => cb(e.payload));
}

// ---- preview proxies -------------------------------------------------------

/** Every asset's preview proxy: queued, building (with a fraction), ready, failed (with why),
 *  off or not needed. */
export async function getProxyStatuses(): Promise<ProxyStatus[]> {
	if (!inTauri()) return sampleAssets.map((a) => devProxy[a.id] ?? { asset_id: a.id, state: 'missing' });
	return invoke<ProxyStatus[]>('proxy_statuses');
}

/** Build an asset's proxy again from the original (the current one is dropped first). */
export async function rebuildProxy(assetId: string): Promise<ProxyStatus> {
	if (!inTauri()) return devRebuildProxy(assetId);
	return invoke<ProxyStatus>('rebuild_proxy', { assetId });
}

/** Delete an asset's proxy; previews decode the original until it is rebuilt. */
export async function deleteProxy(assetId: string): Promise<ProxyStatus> {
	if (!inTauri()) return devDeleteProxy(assetId);
	return invoke<ProxyStatus>('delete_proxy', { assetId });
}

/** Subscribe to `proxy-progress` events: a proxy queued, each percent of a build, ready,
 *  failed. Returns an unlisten fn. */
export async function onProxyProgress(cb: (s: ProxyStatus) => void): Promise<() => void> {
	if (!inTauri()) {
		devProxyListeners.add(cb);
		return () => void devProxyListeners.delete(cb);
	}
	const { listen } = await import('@tauri-apps/api/event');
	return listen<ProxyStatus>('proxy-progress', (e) => cb(e.payload));
}

// The browser harness's stand-ins. Real kerf-core does all of this; here it is just enough for the
// bin's badges and chips, the menus and the settings to be explored under `bun run dev`.
// `?proxy=failed` makes the second sample's proxy a failed one, `?proxy=queued` a queued one, so
// every badge can be looked at.

const devProxyListeners = new Set<(s: ProxyStatus) => void>();
const devAnalysisListeners = new Set<(s: AnalysisStatus) => void>();

function harnessProxyMode(): string | null {
	try {
		return new URLSearchParams(location.search).get('proxy');
	} catch {
		return null;
	}
}

const devProxy: Record<string, ProxyStatus> = (() => {
	const mode = harnessProxyMode();
	const [a, b] = sampleAssets;
	// `?empty=1`: an empty library has no proxies to be building.
	if (!a || !b) return {};
	return {
		[a.id]: { asset_id: a.id, state: 'building', fraction: 0.42, width: 1280, elapsed_secs: 31, eta_secs: 43 },
		[b.id]:
			mode === 'failed'
				? {
						asset_id: b.id,
						state: 'failed',
						width: 1280,
						reason: 'could not generate preview proxy: Invalid data found when processing input'
					}
				: mode === 'queued'
					? { asset_id: b.id, state: 'queued', width: 1280 }
					: { asset_id: b.id, state: 'ready', width: 1280, bytes: 38_400_000 }
	};
})();

function devNoteProxy(s: ProxyStatus) {
	devProxy[s.asset_id] = s;
	for (const cb of devProxyListeners) cb(structuredClone(s));
}

const devTimers = new Map<string, ReturnType<typeof setInterval>>();

function devRebuildProxy(assetId: string): ProxyStatus {
	const asset = assetById(assetId);
	if (!asset) throw new Error('asset not found');
	devAbandonProxy(assetId);
	let fraction = 0;
	const start: ProxyStatus = { asset_id: assetId, state: 'building', fraction, width: 1280, elapsed_secs: 0 };
	devNoteProxy(start);
	devTimers.set(
		assetId,
		setInterval(() => {
			fraction = Math.min(1, fraction + 0.1);
			if (fraction >= 1) {
				devAbandonProxy(assetId);
				devNoteProxy({ asset_id: assetId, state: 'ready', width: 1280, bytes: 24_000_000 });
			} else {
				devNoteProxy({ asset_id: assetId, state: 'building', fraction, width: 1280, elapsed_secs: fraction * 10, eta_secs: (1 - fraction) * 10 });
			}
		}, 400)
	);
	return start;
}

function devAbandonProxy(assetId: string) {
	const timer = devTimers.get(assetId);
	if (timer) clearInterval(timer);
	devTimers.delete(assetId);
}

function devDeleteProxy(assetId: string): ProxyStatus {
	devAbandonProxy(assetId);
	const gone: ProxyStatus = {
		asset_id: assetId,
		state: 'off',
		width: 1280,
		reason: 'you deleted this proxy; rebuild it to bring it back'
	};
	devNoteProxy(gone);
	return gone;
}

const devRan: Record<string, Set<AnalysisKind>> = {};
let devRunning: { id: string; kind: AnalysisKind } | null = null;

function devAutoAnalysis(): AutoAnalysis {
	return readBrowserAutoAnalysis();
}

function devAnalysisStatus(assetId: string): AnalysisStatus {
	const analysis = sampleAnalysis[assetId];
	const auto = devAutoAnalysis();
	return {
		asset_id: assetId,
		kinds: ALL_KINDS.map((kind) => {
			if (devRunning?.id === assetId && devRunning.kind === kind) return { kind, state: 'running' as const };
			if (devRan[assetId]?.has(kind) || analysisKindDone(analysis, kind)) return { kind, state: 'done' as const };
			if (kind === 'transcript')
				return { kind, state: 'off' as const, reason: 'no speech-to-text backend: transcription runs in the desktop app' };
			if (!auto.enabled) return { kind, state: 'off' as const, reason: 'analysis on import is off in Settings' };
			if (!auto[kind]) return { kind, state: 'off' as const, reason: 'switched off in Settings › Analysis' };
			return { kind, state: 'not_run' as const };
		})
	};
}

function devNoteAnalysis(assetId: string) {
	const s = devAnalysisStatus(assetId);
	for (const cb of devAnalysisListeners) cb(structuredClone(s));
}

/** How long one simulated analysis step takes; `?slow=1` makes it long enough to look at. */
function devAnalysisStepMs(): number {
	try {
		return new URLSearchParams(location.search).has('slow') ? 2400 : 450;
	} catch {
		return 450;
	}
}

async function devAnalyze(assetId: string, steps?: readonly AnalysisKind[]): Promise<AssetAnalysis> {
	const auto = devAutoAnalysis();
	const kinds = steps ?? ALL_KINDS.filter((k) => auto[k] && k !== 'transcript');
	const empty: AssetAnalysis = {
		asset_id: assetId,
		silence_segments: [],
		scene_changes: [],
		transcript: [],
		loudness: null,
		onsets: [],
		tempo: null,
		audio_class: null,
		music: null
	};
	sampleAnalysis[assetId] ??= empty;
	const stage = (k: AnalysisKind) => (k === 'transcript' ? 'transcribe' : k);
	for (const kind of ALL_KINDS.filter((k) => kinds.includes(k))) {
		devRunning = { id: assetId, kind };
		devNoteAnalysis(assetId);
		for (const fraction of [0, 0.35, 0.7]) {
			for (const cb of devProgressListeners) cb({ asset_id: assetId, stage: stage(kind), fraction });
			await new Promise((r) => setTimeout(r, devAnalysisStepMs() / 3));
		}
		(devRan[assetId] ??= new Set()).add(kind);
	}
	devRunning = null;
	devNoteAnalysis(assetId);
	return { ...sampleAnalysis[assetId], ran: ALL_KINDS.filter((k) => devRan[assetId]?.has(k) || analysisKindDone(sampleAnalysis[assetId], k)) };
}

// ---- playback --------------------------------------------------------------

/** A composited frame pushed up during playback. */
export interface PlaybackFrame {
	/** Timeline time this frame shows, for dropping ones the clock has passed. */
	time: number;
	/** The frame as JPEG bytes, undecoded: a view into the channel message, so nothing is copied
	 *  until the preview decides to show it. */
	jpeg: Uint8Array<ArrayBuffer>;
}

/** What a playback stream is asked for. `null` leaves it to the engine: the cut's own frame rate
 *  and the default width. The preview states both only after a stream could not keep up. */
export interface PlaybackRequest {
	fps: number | null;
	width: number | null;
}

/**
 * Play the timeline from `start`, invoking `onFrame` with each composited frame
 * until playback stops. Returns a function that stops it.
 *
 * One long-lived ffmpeg renders the whole span, rather than the one process per
 * frame that scrubbing uses — the difference between a slideshow and video. Its frames
 * come up as raw bytes (the timeline time, then the JPEG), not as base64 data URLs in
 * JSON; the page builds a data URL itself, for the frames it shows. In the browser harness
 * there is no backend, so frames are synthesized instead (see `samplePlayback`).
 */
let playbackSeq = 0;

/** This page's playback session, asked of the backend once: ids are built from it so a reloaded page's
 *  cannot collide with the ones the page before it left behind (`playback-id.ts`). */
let playbackSessionAsked: Promise<number> | null = null;

function playbackSession(): Promise<number> {
	playbackSessionAsked ??= invoke<number>('playback_session').catch((e) => {
		playbackSessionAsked = null;
		throw e;
	});
	return playbackSessionAsked;
}

export function startPlayback(
	start: number,
	request: PlaybackRequest,
	onFrame: (f: PlaybackFrame) => void,
	onError?: (message: string) => void
): () => void {
	if (!inTauri()) return samplePlayback(start, request, onFrame);
	// The backend cancels *by id* rather than by a generation counter: start and
	// stop are separate async calls that can arrive out of order, and a stop
	// meant for the previous stream must not kill the one that replaced it.
	const seq = ++playbackSeq;
	// Known once the session is; a stop before then has nothing to stop, because the start never goes out.
	let id = 0;
	let stopped = false;
	void (async () => {
		const session = await playbackSession();
		const { Channel } = await import('@tauri-apps/api/core');
		if (stopped) return;
		id = playbackId(session, seq);
		const channel = new Channel<ArrayBuffer>();
		channel.onmessage = (message) => {
			if (stopped) return;
			// A throw here would end up in the channel's own bookkeeping, which delivers the messages
			// after this one in order: one bad frame must not stop the stream behind it.
			try {
				const frame = parsePlaybackMessage(message);
				if (frame) onFrame(frame);
			} catch (e) {
				logFrontend('warn', `a playback frame was dropped: ${describeError(e)}`, 'playback');
			}
		};
		// Resolves only when playback ends; nothing waits on it.
		// The backend only rejects for a real failure (ffmpeg died, inputs
		// would not resolve), never for a stop or a supersede — but a stream
		// already stopped on this side has no one left to tell.
		void invoke('start_playback', {
			playbackId: id,
			start,
			fps: request.fps,
			maxWidth: request.width,
			onFrame: channel
		}).catch((e) => {
			if (!stopped) onError?.(e instanceof Error ? e.message : String(e));
		});
	})().catch((e) => {
		if (!stopped) onError?.(e instanceof Error ? e.message : String(e));
	});
	return () => {
		if (stopped) return;
		stopped = true;
		if (id) void invoke('stop_playback', { playbackId: id }).catch((e) => console.warn('stop_playback failed', e));
	};
}

/** What the harness renders a frame at when the page does not say: the sample frame's own size. */
const SAMPLE_WIDTH = 640;

/** What the desktop app really pays to put one frame on screen: ffmpeg's startup
 *  plus the channel. Deliberately larger than the preview's staleness budget — a
 *  harness whose frames arrive instantly cannot reproduce the class of bug where
 *  that constant is mistaken for the picture drifting. */
const SAMPLE_FRAME_LAG_MS = 90;

/**
 * The browser harness's stand-in for the streamed composite: a generated JPEG per tick, paced
 * at the rate asked for (the cut's own, as the engine would read it, when none is) and drawn at
 * the width asked for, delayed by a realistic transport cost, so playback moves in `bun run dev`
 * through the same route as the desktop app and the preview's frame pacing and ladder are
 * exercised end to end without a backend.
 */
function samplePlayback(start: number, request: PlaybackRequest, onFrame: (f: PlaybackFrame) => void): () => void {
	const fps = request.fps ?? playbackFps(renderedFps(devTimeline, sampleAssets));
	const width = request.width ?? SAMPLE_WIDTH;
	let index = 0;
	let timer: ReturnType<typeof setInterval> | null = null;
	let stopped = false;
	const spawn = setTimeout(() => {
		timer = setInterval(() => {
			const time = start + index++ / fps;
			void sampleFrameJpeg(time, width)
				.then((jpeg) => jpeg?.arrayBuffer())
				.then((bytes) => {
					if (bytes && !stopped) onFrame({ time, jpeg: new Uint8Array(bytes) });
				});
		}, 1000 / fps);
	}, SAMPLE_FRAME_LAG_MS);
	return () => {
		stopped = true;
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
			enabled: readBrowserAutoAnalysis().transcript,
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

/** Fetch a speech model into Kerf's cache, streaming `model-progress` (`onModelProgress`).
 *  `cancelModelDownload` stops it; it then rejects with `model download cancelled`. */
export async function downloadSpeechModel(name: string): Promise<string> {
	if (!inTauri()) throw new Error('speech models are only available in the desktop app');
	return invoke<string>('download_speech_model', { name });
}

/** Stop the speech model download in flight (if any). */
export async function cancelModelDownload(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_model_download');
}

/** Subscribe to `model-progress` events: the bytes of the speech model fetched so far. Returns an unlisten fn. */
export async function onModelProgress(cb: (p: ModelProgress) => void): Promise<() => void> {
	if (!inTauri()) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<ModelProgress>('model-progress', (e) => cb(e.payload));
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
			audio_class: { class: 'speech', confidence: 1 },
			music: null
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

// ---- composed music (SoundFont synthesizer) -----------------------------------

// The browser harness has no synthesizer, so it fakes one: a SoundFont download on first use and a
// few render ticks, then an audio-only asset as long as the score, with the tempo and bar analysis
// written from the score exactly as `Project::place_music` does. What stays real is everything the
// editor does with the result — the M track, replacing a clip, aligning to the cuts (the faithful
// mirror in `music-score.ts`), and the refusals, worded as the engine words them.
const devMusicListeners = new Set<(p: MusicProgress) => void>();
let devMusicReady = false;
let devMusicCancelled = false;
let devMusicRendering = 0;
let devBandCancelled = false;
const DEV_SOUNDFONT_BYTES = 32319396;

/** What a cancelled composition rejects with, for a dialog to stay quiet on. */
export const MUSIC_CANCELLED = 'music cancelled';
/** What a stopped band-balance measurement rejects with. */
export const BAND_BALANCE_CANCELLED = 'band balance cancelled';

function devMusicStatus(): MusicStatus {
	return {
		soundfont_ready: devMusicReady,
		soundfont: 'GeneralUser-GS-v2.0.3.sf2',
		approx_download_bytes: devMusicReady ? 0 : DEV_SOUNDFONT_BYTES,
		rendering: devMusicRendering,
		sample_rate: 48000,
		styles: MUSIC_STYLES.map((s) => ({ ...s }))
	};
}

function devMusicEmit(p: MusicProgress) {
	for (const cb of devMusicListeners) cb(p);
}

async function devMusicStage(stage: MusicProgress['stage'], steps: number, detail: (i: number) => string) {
	for (let i = 1; i <= steps; i++) {
		await new Promise((r) => setTimeout(r, 220));
		if (devMusicCancelled) throw new Error(MUSIC_CANCELLED);
		devMusicEmit({ stage, fraction: i / steps, detail: detail(i) });
	}
}

async function devSoundfont() {
	if (devMusicReady) return;
	await devMusicStage('download_soundfont', 4, (i) => `${Math.round((i / 4) * 32)} MB / 32 MB`);
	devMusicReady = true;
}

/** A stable short hex name for a score, so the same score is the same file and the same asset. */
function devScoreHash(score: MusicScore): string {
	let h = 0xcbf29ce484222325n;
	for (const c of new TextEncoder().encode(JSON.stringify(score))) h = ((h ^ BigInt(c)) * 0x100000001b3n) & 0xffffffffffffffffn;
	return h.toString(16).padStart(16, '0');
}

/** The timeline's cut times a score is fitted to (`cut_times` in `project/music.rs`). */
function devAlignCuts(align: ScoreAlign): number[] {
	if (align.to === 'markers') {
		const markers = devTimeline.markers ?? [];
		if (markers.length === 0) throw new Error('invalid argument: there are no markers to align the music to');
		return markers.map((m) => m.time);
	}
	const track = align.track_id
		? devTimeline.tracks.find((t) => t.id === align.track_id)
		: devTimeline.tracks.find((t) => t.kind === 'video');
	if (align.track_id && !track) throw new Error(`track not found: ${align.track_id}`);
	if (!track) throw new Error('invalid argument: there is no video track to align the music to');
	if (track.kind !== 'video') {
		throw new Error("invalid argument: cuts are the boundaries of a video track's clips: pick a video track");
	}
	const edges = track.clips.flatMap((c) => [c.timeline_start, c.timeline_start + clipDuration(c)]).sort((a, b) => a - b);
	const distinct = edges.filter((t, i) => i === 0 || Math.abs(t - edges[i - 1]) >= 1e-6);
	if (distinct.length < 3) throw new Error('invalid argument: the video track has no cuts: it holds a single clip');
	return distinct.slice(1, -1);
}

/** Whether the asset of a clip was composed (it carries a score); `undefined` for an asset the project does not have. */
const devComposed = (assetId: string): boolean | undefined => {
	const asset = assetById(assetId);
	return asset ? !!asset.music_score : undefined;
};

/** Check a compose request and resolve it (`Project::compose_music_inputs`): the request has no key the structs do
 *  not, the score is valid, a replaced clip is a music bed that can be, the music's start is known and, with
 *  `align`, the score is fitted to the cuts or markers. Last, the placement is rehearsed on a copy of the cut with
 *  the render's length (`check_music_fits`): a piece that would land on another clip of its track is refused now,
 *  not after the render. */
function devComposePlan(request: ComposeRequest) {
	checkComposeRequest(request);
	const invalid = (why: string) => new Error(`invalid argument: ${why}`);
	const bad = validateScore(request.score);
	if (bad) throw invalid(bad);
	if (request.at !== undefined && (!Number.isFinite(request.at) || request.at < 0)) {
		throw invalid('at must be a time in seconds, 0 or later');
	}
	let start: number;
	let replaces: string[] = [];
	if (request.replace_clip_id !== undefined) {
		const bed = musicBed(devTimeline, request.replace_clip_id, devComposed);
		start = Math.min(...bed.map((c) => c.timeline_start));
		replaces = bed.map((c) => c.id);
	} else {
		start = musicStart(devTimeline, request.track_id, request.at);
	}
	let score = request.score;
	let align: AlignReport | null = null;
	if (request.align) {
		const max = request.align.max_tempo_change ?? 0.06;
		if (!Number.isFinite(max) || max < 0 || max > 0.25) {
			throw invalid('max_tempo_change is a fraction from 0 to 0.25 (0.06 is 6 %)');
		}
		const planned = planScoreToPicture(score, devAlignCuts(request.align), start, {
			max_tempo_change: max,
			tolerance_s: 0.04,
			add_hits: true
		});
		score = planned.score;
		align = planned.report;
	}
	rehearseMusic(devTimeline, request, scoreDuration(score), devRippleMode, devComposed, uid);
	return { score, align, start, replaces };
}

/** The asset a rendered score is, and the analysis `place_music` writes from it — made, not stored: `commit` stores
 *  them, once the placement has gone through (a refused placement leaves neither the asset nor its analysis; an asset
 *  that was already there, the same score composed before, stays). The chords are the ones **as played**: the last bar
 *  of an outro that ends the piece is the tonic, whatever it was written as (`resolvedChords`). */
function devMusicAsset(score: MusicScore): { asset: Asset; commit: () => void } {
	const path = `/music/music-${devScoreHash(score)}.wav`;
	const known = sampleAssets.find((a) => a.path === path);
	if (known) return { asset: known, commit: () => {} };
	const asset: Asset = {
		id: uid(),
		path,
		name: `Music: ${scoreTitle(score)}`,
		duration: scoreDuration(score),
		streams: [{ index: 0, kind: 'audio', codec: 'pcm_s16le', sample_rate: 48000, channels: 2 }],
		imported_at: new Date().toISOString(),
		music_score: structuredClone(score)
	};
	const grid = { period_s: 60 / score.bpm, phase_s: 0, downbeat_offset: 0, beats_per_bar: 4 };
	const bars = Math.floor((asset.duration + 1e-6) / barSeconds(score.bpm));
	// One chroma per distinct chord, so a repeated chord is a repeated bar and the phrases are found.
	const chords = resolvedChords(score).flat();
	const chroma = (chord: string) => {
		let h = 7;
		for (const c of chord) h = Math.imul(h ^ c.charCodeAt(0), 0x01000193);
		const v = Array.from({ length: 12 }, (_, pc) => ((Math.imul(h ^ pc, 0x9e3779b1) >>> 0) % 1000) / 1000 + (pc === (h >>> 0) % 12 ? 2 : 0));
		const norm = Math.sqrt(v.reduce((a, x) => a + x * x, 0));
		return v.map((x) => Math.fround(x / norm));
	};
	const bar_chroma = Array.from({ length: bars }, (_, k) => (chords[k] ? chroma(chords[k]) : new Array<number>(12).fill(0)));
	const tempo = tempoFromGrid(grid, asset.duration, 1);
	const analysis: AssetAnalysis = {
		asset_id: asset.id,
		silence_segments: [],
		scene_changes: [],
		transcript: [],
		loudness: null,
		onsets: tempo.beats,
		tempo,
		audio_class: { class: 'music', confidence: 1 },
		ran: ['rhythm'],
		music: { grid, duration: asset.duration, bar_chroma, phrases: phraseMatches(bar_chroma) }
	};
	return {
		asset,
		commit: () => {
			sampleAssets.push(asset);
			sampleAnalysis[asset.id] = analysis;
		}
	};
}

/** Whether music can be composed here, whether the first composition still downloads the SoundFont,
 *  and the styles on offer. */
export async function musicStatus(): Promise<MusicStatus> {
	if (!inTauri()) return devMusicStatus();
	return invoke<MusicStatus>('music_status');
}

/** Download the SoundFont ahead of the first composition, streaming `music-progress`. */
export async function prepareMusic(): Promise<MusicStatus> {
	if (!inTauri()) {
		devMusicCancelled = false;
		await devSoundfont();
		return devMusicStatus();
	}
	return invoke<MusicStatus>('prepare_music');
}

/** Render a score and put it on the timeline: on the "M" track unless `track_id` names another audio
 *  track, at `at` or after what is there — or, with `replace_clip_id`, swapped into that composed
 *  clip. `align` first fits the score to the picture's cuts (or markers). Streams `music-progress`;
 *  rejects with `music cancelled` (`MUSIC_CANCELLED`) if stopped. */
export async function composeMusic(request: ComposeRequest): Promise<ComposeResult> {
	if (!inTauri()) {
		devMusicCancelled = false;
		const plan = devComposePlan(request);
		devMusicRendering += 1;
		try {
			await devSoundfont();
			const total = Math.round(scoreDuration(plan.score));
			await devMusicStage('render', 4, (i) => `${Math.round((i / 4) * total)} of ${total} s`);
		} finally {
			devMusicRendering -= 1;
		}
		// The plan was made before the wait; landing it checks the placement again against the cut as it is now,
		// as the engine does under the lock — one edit, rippled like any other, in which the clip the edit put
		// somewhere may not end up on another clip. Only then is the asset stored: a refused placement leaves
		// neither it nor its analysis.
		const made = devMusicAsset(plan.score);
		const clipId = request.replace_clip_id ?? uid();
		const done = runEdit(
			devTimeline,
			{ ripple: devRippleMode, links: devLinks(undefined), lanes: [clipId] },
			(scratch) => putMusic(scratch, made.asset, request, clipId, devComposed, uid)
		);
		made.commit();
		devTimeline = done.timeline;
		recordDev(request.replace_clip_id !== undefined ? 'Recompose music' : 'Add music');
		const timeline = snapshot();
		const clip = timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === clipId)!;
		return {
			asset: structuredClone(made.asset),
			clip,
			score: structuredClone(plan.score),
			align: plan.align,
			replaced: plan.replaces,
			timeline
		};
	}
	return invoke<ComposeResult>('compose_music', { request });
}

/** Stop the SoundFont download or the render in flight; it then rejects with `music cancelled`. */
export async function cancelMusic(): Promise<void> {
	if (!inTauri()) {
		devMusicCancelled = true;
		return;
	}
	return invoke<void>('cancel_music');
}

/** Subscribe to `music-progress` events — the GUI's own and an agent's. Returns an unlisten fn. */
export async function onMusicProgress(cb: (p: MusicProgress) => void): Promise<() => void> {
	if (!inTauri()) {
		devMusicListeners.add(cb);
		return () => void devMusicListeners.delete(cb);
	}
	const { listen } = await import('@tauri-apps/api/event');
	return listen<MusicProgress>('music-progress', (e) => cb(e.payload));
}

// ---- band balance ---------------------------------------------------------------

/** Where a stretch of sound keeps its energy: six bands per window, with RMS and loudness, for one
 *  asset, one clip's source range or a range of the mix; exactly one of `asset_id`, `clip_id`, `range`.
 *  Windows are the given `sections`, else a composed asset's score sections, else the music's
 *  phrases, else eight equal parts. The harness has no audio: the numbers are made up, the windows
 *  and notes are the engine's. Rejects with `band balance cancelled` if stopped. */
export async function getBandBalance(request: BandBalanceRequest): Promise<BandBalance> {
	if (!inTauri()) {
		checkBandRequest(request);
		const invalid = (why: string) => new Error(`invalid argument: ${why}`);
		const named = [request.asset_id, request.clip_id, request.range].filter((x) => x !== undefined).length;
		if (named !== 1) throw invalid('name exactly one of asset_id, clip_id or range to measure');
		let target: BandBalance['target'];
		let name: string;
		let origin = 0;
		let span: number;
		let asset: Asset | undefined;
		if (request.range) {
			const r = request.range;
			if (!Number.isFinite(r.start) || !Number.isFinite(r.end) || r.start < 0 || r.end <= r.start) {
				throw invalid('range needs 0 <= start < end, in seconds');
			}
			const why = rangePastTheSound(devTimeline, r.start);
			if (why) throw invalid(why);
			const cut = untitledDuration(devTimeline);
			target = 'mix';
			name = 'mix';
			origin = r.start;
			span = Math.min(r.end, cut) - r.start;
		} else {
			let clip: Clip | undefined;
			if (request.clip_id) {
				clip = locate(devTimeline, request.clip_id)?.[0].clips.find((c) => c.id === request.clip_id);
				if (!clip) throw new Error(`clip not found: ${request.clip_id}`);
			}
			asset = assetById(clip ? clip.asset_id : request.asset_id!);
			if (!asset) throw new Error(`asset not found: ${clip ? clip.asset_id : request.asset_id}`);
			if (!asset.streams.some((x) => x.kind === 'audio')) throw invalid(`${asset.name} has no audio stream to measure`);
			target = clip ? 'clip' : 'asset';
			name = asset.name;
			origin = clip ? clip.source_in : 0;
			span = clip ? Math.max(clip.source_out - clip.source_in, 0) : asset.duration;
		}
		if (span < 0.25) throw invalid(`the sound is ${span.toFixed(2)}s long: too short to measure`);
		const lo = origin;
		const hi = origin + span;
		let windows: BandWindow[];
		let kinds: (SectionKind | null)[] = [];
		let windows_from: BandBalance['windows_from'];
		if (request.sections) {
			const checked = checkSections(request.sections, lo, hi);
			if (typeof checked === 'string') throw invalid(checked);
			windows = checked;
			windows_from = 'sections';
		} else {
			const spans = asset?.music_score ? sectionSpans(asset.music_score) : [];
			const labels = sectionLabels(spans.map((x) => x.kind));
			const fromScore = clipWindows(
				spans.map((x, i) => ({ start: x.start_s, end: x.end_s, label: labels[i] })),
				lo,
				hi
			);
			const grid = asset ? sampleAnalysis[asset.id]?.music : null;
			const phrases = grid
				? phraseWindows(
						grid.grid.phase_s + grid.grid.downbeat_offset * grid.grid.period_s,
						grid.grid.period_s * grid.grid.beats_per_bar,
						lo,
						Math.min(hi, grid.duration)
					)
				: [];
			if (fromScore.length > 0) {
				windows = fromScore;
				windows_from = 'score';
				kinds = spans.map((x) => x.kind);
			} else if (phrases.length >= 2) {
				windows = phrases;
				windows_from = 'phrases';
			} else {
				windows = equalWindows(lo, hi);
				windows_from = 'equal';
			}
		}
		// In a browser the made-up measurement takes a moment, so Stop can be tried; under bun it is at once.
		devBandCancelled = false;
		if (typeof window !== 'undefined') {
			for (let i = 0; i < 4; i++) {
				await new Promise((r) => setTimeout(r, 150));
				if (devBandCancelled) throw new Error(BAND_BALANCE_CANCELLED);
			}
		}
		const measured = synthBalance(windows, kinds);
		return {
			target,
			name,
			start: origin,
			end: origin + span,
			...measured,
			windows_from,
			notes: balanceNotes(measured.overall, measured.windows)
		};
	}
	return invoke<BandBalance>('get_band_balance', { request });
}

/** Ask the running band-balance measurement to give up; it then rejects with `band balance cancelled`. */
export async function cancelBandBalance(): Promise<void> {
	if (!inTauri()) {
		devBandCancelled = true;
		return;
	}
	return invoke<void>('cancel_band_balance');
}

// ---- stem separation (Demucs) -----------------------------------------------

// The browser harness has no separator, so it fakes one: the same progress sequence the app
// streams (a download on first use, then separate, then encode), four silent-in-fact stem assets
// named like the real ones and, under a clip, `placeStems` — the faithful mirror of
// `Project::place_stems`. Repeating a run finds the stems again by path and skips the work, as
// the app's per-file cache does.
const devStemsListeners = new Set<(p: StemsProgress) => void>();
let devStemsReady = false;
let devStemsCancelled = false;
const DEV_STEMS_TICK_MS = 160;

function devStemsStatus(): StemsStatus {
	return { runtime_ready: devStemsReady, model_ready: devStemsReady, model_bytes: DEV_MODEL_BYTES, stems: [...STEM_NAMES] };
}

/** One stage's worth of synthetic ticks; stops with the real error on a cancel. */
async function devStemsStage(stage: StemsProgress['stage'], steps: number, detail: (i: number) => string | null) {
	for (let i = 1; i <= steps; i++) {
		await new Promise((r) => setTimeout(r, DEV_STEMS_TICK_MS));
		if (devStemsCancelled) throw new Error(STEMS_CANCELLED);
		const p: StemsProgress = { stage, fraction: i / steps, detail: detail(i) };
		for (const cb of devStemsListeners) cb(p);
	}
}

async function devStemsRun() {
	if (!devStemsReady) {
		await devStemsStage('download_runtime', 3, (i) => `${Math.round((i / 3) * 12)} MB / 12 MB`);
		await devStemsStage('download_model', 4, (i) => `${Math.round((i / 4) * 174)} MB / 174 MB`);
		devStemsReady = true;
	}
	await devStemsStage('separate', 6, () => null);
	await devStemsStage('encode', STEM_NAMES.length, (i) => (i < STEM_NAMES.length ? STEM_NAMES[i] : null));
}

/** Whether stem separation would still download its runtime or model first, how big the
 *  model is, and the stems it makes. */
export async function stemsStatus(): Promise<StemsStatus> {
	if (!inTauri()) return devStemsStatus();
	return invoke<StemsStatus>('stems_status');
}

/**
 * Split an asset's sound into drums, bass, other and vocals, streaming `stems-progress`, and
 * add the four stems to the library as `<name> · drums` … With `clipId` they are also laid under
 * that clip on four new audio tracks (Drums / Bass / Other / Vocals) at its position and span, and
 * the clip's own sound is switched off (an audio clip disabled, a picture's `source_audio` off) —
 * one `Separate stems` revision. Rejects with `stems cancelled` if stopped; the app also refuses an
 * asset without sound, a clip of another asset, a locked track and a picture whose sound is on a
 * linked audio clip.
 */
export async function separateStems(assetId: string, clipId?: string): Promise<StemsResult> {
	if (!inTauri()) {
		devStemsCancelled = false;
		const source = assetById(assetId);
		if (!source) throw new Error(`asset not found: ${assetId}`);
		if (!source.streams.some((s) => s.kind === 'audio')) throw new Error('invalid argument: this asset has no sound to separate');
		const made = devStemAssets(source, uid, new Date().toISOString());
		const known = made.map((m) => sampleAssets.find((a) => a.path === m.path));
		// The app caches the stems per file: a second run answers at once, without a word of progress.
		if (known.some((k) => !k)) await devStemsRun();
		// `insert_or_get_asset`: stems already in the library (same path) are reused, and they stay
		// there even if the placement is then refused — the assets land before the edit does.
		const assets = made.map((fresh, i) => {
			const had = known[i];
			if (had) return had;
			sampleAssets.push(fresh);
			return fresh;
		});
		let clips: Clip[] = [];
		if (clipId) {
			// `edit_timeline_exact`: no ripple; a refusal leaves the timeline as it was.
			const scratch = snapshot();
			clips = placeStems(scratch, assetId, assets, clipId, uid);
			devTimeline = scratch;
			recordDev('Separate stems');
		}
		return { placed: structuredClone({ assets, clips }), timeline: snapshot() };
	}
	return invoke<StemsResult>('separate_stems', { assetId, clipId });
}

/** Stop the download or separation in flight; it then rejects with `stems cancelled`. */
export async function cancelStems(): Promise<void> {
	if (!inTauri()) {
		devStemsCancelled = true;
		return;
	}
	return invoke<void>('cancel_stems');
}

/** Subscribe to `stems-progress` events — the GUI's own and an agent's. Returns an unlisten fn. */
export async function onStemsProgress(cb: (p: StemsProgress) => void): Promise<() => void> {
	if (!inTauri()) {
		devStemsListeners.add(cb);
		return () => void devStemsListeners.delete(cb);
	}
	const { listen } = await import('@tauri-apps/api/event');
	return listen<StemsProgress>('stems-progress', (e) => cb(e.payload));
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
		const asset = assetById(assetId);
		if (!asset) throw new Error(`asset not found: ${assetId}`);
		devRun((tl) => ops.addClip(tl, devEnv(undefined, undefined), asset, null, start, end));
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
		const asset = assetById(assetId);
		if (!asset) throw new Error(`asset not found: ${assetId}`);
		devRun((tl) => ops.addClip(tl, devEnv(undefined, undefined), asset, trackId, sourceIn, sourceOut, timelineStart));
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
		if (found) {
			const clip = found[0].clips[found[1]];
			clip.mask = mask;
			// Clearing the mask clears its keys (`Project::set_mask`).
			if (!mask && clip.channels) {
				clip.channels = clip.channels.filter((c) => !isMaskProperty(c.prop));
				if (clip.channels.length === 0) delete clip.channels;
			}
		}
		recordDev(mask ? 'Mask clip' : 'Clear mask');
		return snapshot();
	}
	return invoke<Timeline>('set_mask', { clipId, mask });
}

/** Replace a clip's curves, colour wheels and LUT. The backend checks a LUT file before it is set. */
export async function setGrade(clipId: string, grade: Grade): Promise<Timeline> {
	if (!inTauri()) {
		const found = locate(devTimeline, clipId);
		if (found) {
			const g = normalizeGrade(grade);
			const clip = found[0].clips[found[1]];
			if (Object.keys(g).length) clip.grade = g;
			else delete clip.grade;
		}
		recordDev('Set grade');
		return snapshot();
	}
	return invoke<Timeline>('set_grade', { clipId, grade });
}

/** What a `.cube` file holds, or why it is not a usable LUT (thrown). */
export async function inspectLut(path: string): Promise<LutInfo> {
	if (!inTauri()) return { path, size: 33, title: 'Harness look' };
	return invoke<LutInfo>('inspect_lut', { path });
}

/** Pick a `.cube` LUT file; `null` if cancelled. The harness hands back a stand-in path. */
export async function pickLutFile(): Promise<string | null> {
	if (!inTauri()) return '/harness/looks/teal-orange.cube';
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({ multiple: false, filters: [{ name: 'Cube LUT', extensions: ['cube'] }] });
	return typeof selected === 'string' ? selected : null;
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
function storeDevMaster(next: MasterBus) {
	// The compressor duck is `None` in the core: the key is omitted, not `null`.
	if (next.duck_depth_db == null) {
		next = { ...next };
		delete next.duck_depth_db;
	}
	// `true_peak` is not written while off.
	if (!next.true_peak) {
		next = { ...next };
		delete next.true_peak;
	}
	const untouched =
		next.volume === DEFAULT_MASTER.volume &&
		next.limiter === DEFAULT_MASTER.limiter &&
		next.ceiling_db === DEFAULT_MASTER.ceiling_db &&
		next.duck_depth_db === undefined &&
		next.true_peak === undefined;
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

/** Make the master limiter hold the true peak (4x oversampled on export) or only the sample
 *  peak. Kept while the limiter is off. One revision; the preview's limiter is the same either way. */
export async function setMasterTruePeak(enabled: boolean): Promise<Timeline> {
	if (!inTauri()) {
		storeDevMaster({ ...masterOf(devTimeline), true_peak: enabled });
		recordDev(enabled ? 'Limit true peaks' : 'Limit sample peaks');
		return snapshot();
	}
	return invoke<Timeline>('set_master_true_peak', { enabled });
}

/**
 * Choose how tracks flagged `duck` dip under the rest of the mix on export. A `depthDb` (negative
 * dB, clamped to -40..-1) is the **speech gate**: the ducked bus drops by exactly that much while
 * the other tracks speak. Omitted (or `null`) is the sidechain compressor, which dips by how loud
 * the rest is. One revision; the preview does not duck either way (export-only).
 */
export async function setMasterDuck(depthDb?: number | null): Promise<Timeline> {
	if (depthDb != null && !Number.isFinite(depthDb)) throw new Error('duck depth must be a number');
	const depth = depthDb == null ? null : clampDuckDepth(depthDb);
	if (!inTauri()) {
		storeDevMaster({ ...masterOf(devTimeline), duck_depth_db: depth });
		recordDev(depth === null ? 'Duck with the compressor' : `Duck by ${toFixedEven(depth, 0)} dB under speech`);
		return snapshot();
	}
	return invoke<Timeline>('set_master_duck', { depthDb: depth });
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
		const why = range && untitledDuration(devTimeline) > 0 ? rangePastTheSound(devTimeline, range.start) : null;
		if (why) throw new Error(why);
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
		const found = locate(devTimeline, clipId);
		if (found && titleOf(found[0].clips[found[1]]) && speed !== 1) {
			throw new Error('that edit would leave the timeline invalid: the clip is a title, which plays at its own speed');
		}
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

/** Set a clip's transform as it should look `time` seconds into the clip, as one revision: a keyed
 *  number gets a key there, every other number (the crop always) its static value. */
export async function setTransformAt(clipId: string, time: number, patch: Partial<Transform>): Promise<Timeline> {
	if (!inTauri()) {
		if (!Number.isFinite(time) || time < 0) throw new Error('time must be >= 0');
		const found = locate(devTimeline, clipId);
		if (!found) throw new Error(`clip ${clipId} not found`);
		const clip = found[0].clips[found[1]];
		const before = structuredClone(clip);
		poseTransform(clip, time, patch);
		const t = clip.transform!;
		if (t.crop_left + t.crop_right >= 1 || t.crop_top + t.crop_bottom >= 1) {
			found[0].clips[found[1]] = before;
			throw new Error('crop removes the entire frame');
		}
		recordDev('Set transform');
		return snapshot();
	}
	return invoke<Timeline>('set_transform_at', {
		clipId,
		time,
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

/** Replace a clip's whole-transform keyframes. An empty list clears these only: a number with
 *  keys of its own (`setPropertyKeyframes`) keeps them; `clearKeyframes` makes the whole
 *  transform static. */
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
		const keyable = keyableProblem(found[0].clips[found[1]], prop, keys);
		if (keyable) throw new Error(keyable);
		setPropertyKeys(found[0].clips[found[1]], prop, keys);
		recordDev(`Set ${prop === 'pos_x' ? 'position x' : prop === 'pos_y' ? 'position y' : prop} keyframes`);
		return snapshot();
	}
	return invoke<Timeline>('set_property_keyframes', { clipId, prop, keys });
}

/** Replace the keys of several animatable numbers of a clip in one edit (`set_property_tracks`):
 *  what one dope-sheet gesture writes. */
export async function setPropertyTracks(clipId: string, tracks: PropertyTrack[]): Promise<Timeline> {
	if (!inTauri()) {
		if (tracks.length === 0) throw new Error("give at least one property's keys");
		for (const [i, t] of tracks.entries()) {
			if (tracks.slice(0, i).some((o) => o.prop === t.prop)) throw new Error(`${propertyLabel(t.prop)} is given more than once`);
			const problem = propertyKeysProblem(t.prop, t.keys);
			if (problem) throw new Error(problem);
		}
		const found = locate(devTimeline, clipId);
		if (!found) throw new Error(`clip ${clipId} not found`);
		for (const t of tracks) {
			const keyable = keyableProblem(found[0].clips[found[1]], t.prop, t.keys);
			if (keyable) throw new Error(keyable);
		}
		for (const t of tracks) setPropertyKeys(found[0].clips[found[1]], t.prop, t.keys);
		recordDev('Edit keyframes');
		return snapshot();
	}
	return invoke<Timeline>('set_property_tracks', { clipId, tracks });
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
		const maskless = props.length === 0 && !to[0].clips[to[1]].mask;
		const tracks = propertyKeysShifted(source, props, offset).filter((t) => !(maskless && isMaskProperty(t.prop)));
		if (tracks.length === 0) throw new Error('the source clip has no keyframes to copy');
		for (const t of tracks) {
			if (t.keys.length === 0) throw new Error(`the source clip has no ${t.prop} keyframes`);
		}
		const dest = to[0].clips[to[1]];
		for (const t of tracks) {
			const keyable = keyableProblem(dest, t.prop, t.keys);
			if (keyable) throw new Error(keyable);
		}
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

export async function updateMarker(markerId: string, patch: MarkerPatch): Promise<Timeline> {
	if (!inTauri()) {
		const m = devTimeline.markers?.find((x) => x.id === markerId);
		if (m) {
			if (patch.time !== undefined) m.time = patch.time;
			if (patch.name !== undefined) m.name = patch.name;
			if (patch.color !== undefined) m.color = patch.color || null;
			if (patch.duration !== undefined) m.duration = patch.duration || undefined;
			if (patch.comment !== undefined) m.comment = patch.comment.trimEnd() || undefined;
			if (patch.kind !== undefined) m.kind = patch.kind === 'comment' ? undefined : patch.kind;
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

/**
 * Add an animated SVG title between `start` and `end` (timeline seconds). `source` is custom SVG
 * (`{ custom }`, sanitised by the backend) or a preset (`{ preset: { kind, params } }`). Without
 * `end` it runs as long as the SVG's own animation (2–30 s), else four seconds; `layout` places it
 * (frame fractions, default the whole frame, opaque). The new title is the one in the returned
 * timeline that was not there before; `validateSvg` first shows what the backend could not honour.
 */
export async function addSvgOverlay(
	source: SvgSource,
	start: number,
	end?: number | null,
	layout: SvgLayout = {}
): Promise<Timeline> {
	if (!inTauri()) {
		addSvgOverlayTo(devTimeline, source, start, end, layout);
		recordDev('Add animated title');
		return snapshot();
	}
	return invoke<Timeline>('add_svg_overlay', { source, start, end: end ?? null, layout });
}

/** Change an animated title; only provided fields change. `patch.svg` swaps in custom artwork (the title
 *  stops being a preset), `patch.preset_params` re-fills a preset title and `patch.preset_kind` makes it another
 *  preset (one edit; it keeps its id and place in the draw order); a preset re-times itself when `start` / `end`
 *  change its length. */
export async function updateSvgOverlay(overlayId: string, patch: SvgOverlayPatch): Promise<Timeline> {
	if (!inTauri()) {
		updateSvgOverlayIn(devTimeline, overlayId, patch);
		recordDev('Update animated title');
		return snapshot();
	}
	return invoke<Timeline>('update_svg_overlay', { overlayId, patch });
}

export async function removeSvgOverlay(overlayId: string): Promise<Timeline> {
	if (!inTauri()) {
		removeSvgOverlayFrom(devTimeline, overlayId);
		recordDev('Remove animated title');
		return snapshot();
	}
	return invoke<Timeline>('remove_svg_overlay', { overlayId });
}

/** What the backend would make of this SVG source: the sanitised document, what it could not honour
 *  and when its animations end. Touches nothing; throws on a document it would refuse. */
export async function validateSvg(svg: string): Promise<SvgCheck> {
	if (!inTauri()) return validateSvgSource(svg);
	return invoke<SvgCheck>('validate_svg', { svg });
}

/** The ready-made animated titles and the parameters each takes. */
export async function listSvgPresets(): Promise<SvgPresetInfo[]> {
	if (!inTauri()) return svgPresetInfos();
	return invoke<SvgPresetInfo[]>('list_svg_presets');
}

/** A preset's artwork as SVG text, drawn for the project's delivery frame and `duration` seconds
 *  (default 4). Show it through an `<img>` data URL, never inline. */
export async function svgPresetPreview(
	kind: SvgPresetKind,
	params: SvgPresetParamsInput,
	duration?: number
): Promise<string> {
	if (!inTauri()) return svgPresetArt(kind, params);
	return invoke<string>('svg_preset_preview', { kind, params, duration: duration ?? null });
}

// ---- motion templates ------------------------------------------------------
// An animated SVG title kept in the user's library with named controls (a tagline, a brand colour, a
// logo). The library is app-level (not a timeline edit: not undone, not staged); applying a template and
// setting a template title's values are edits and hand back the refreshed timeline.

/** The library by name, and the files in it that did not read (`problems`). Only manifests are read; a
 *  poster is fetched on its own with `templatePoster`. */
export async function listTemplates(): Promise<TemplateList> {
	if (!inTauri()) return listTemplatesDev();
	return invoke<TemplateList>('list_templates');
}

/** A template's poster as a `data:` URL for an `<img>` (a PNG of at most 512 px in the app), `null` when
 *  it has none. Rejects for an id the library lacks. */
export async function templatePoster(templateId: string): Promise<string | null> {
	if (!inTauri()) return templatePosterDev(templateId);
	return invoke<string | null>('template_poster', { templateId });
}

/** The `id`s of a title's elements and the tag each is on, for offering them as the targets of a control. */
export async function svgElementIds(svg: string): Promise<SvgElementId[]> {
	if (!inTauri()) return svgElementIdsOf(svg);
	return invoke<SvgElementId[]>('svg_element_ids', { svg });
}

/** A picture file (PNG, JPEG, GIF or WebP, a little under 96 KB) as the `data:` URL an `image` control takes.
 *  The browser harness cannot read files and answers a fixed picture for any image path. */
export async function templateImageValue(path: string): Promise<string> {
	if (!inTauri()) return templateImageValueOf(path);
	return invoke<string>('template_image_value', { path });
}

/** Keep the animated title `overlayId` in the library as a template with these `controls` (each aimed at
 *  element ids of its artwork). Slow (fonts, sanitiser, poster). Resolves to the new template and what could
 *  not be carried over (a font whose licence or size keeps it out). Not undoable: `deleteTemplate` removes it. */
export async function saveTemplate(
	overlayId: string,
	name: string,
	controls: TemplateControl[],
	description?: string
): Promise<TemplateSaved> {
	if (!inTauri()) {
		const saved = saveTemplateFrom(devTimeline, overlayId, name, description, controls);
		noteTemplatesChanged();
		return saved;
	}
	const saved = await invoke<TemplateSaved>('save_template', { overlayId, name, description: description ?? null, controls });
	noteTemplatesChanged();
	return saved;
}

/** Add a library template to the timeline as an animated title from `start` to `end` (default: its own
 *  length). `values` (control id to value, partial) are spliced into its artwork; `layout` places and names it
 *  like `addSvgOverlay`'s. The template's fonts are stored in the project, so the title keeps working if the
 *  template is deleted. Resolves to the refreshed timeline, the new title's id (to select it) and the
 *  warnings (a font that is not installed, ...). */
export async function applyTemplate(
	templateId: string,
	values: Record<string, string>,
	start: number,
	end?: number | null,
	layout: SvgLayout = {}
): Promise<TemplateEdit> {
	if (!inTauri()) {
		const { overlay, warnings } = applyTemplateTo(devTimeline, templateId, values, start, end, layout);
		recordDev(`Add template title “${overlay.name}”`);
		return { timeline: snapshot(), overlay_id: overlay.id, warnings };
	}
	return invoke<TemplateEdit>('apply_template', { templateId, values, start, end: end ?? null, layout });
}

/** Change control values of a template title (partial: only the controls named). One edit; the title keeps its
 *  id, timing and placement. Rejects for a title that did not come from a template, a value its control refuses,
 *  and a title that changed while the values were being applied ("try again"). */
export async function setTemplateValues(overlayId: string, values: Record<string, string>): Promise<TemplateEdit> {
	if (!inTauri()) {
		const { overlay, clip, warnings } = setTemplateValuesIn(devTimeline, overlayId, values);
		recordDev('Set template values');
		return { timeline: snapshot(), overlay_id: overlay?.id, clip_id: clip?.id, warnings };
	}
	return invoke<TemplateEdit>('set_template_values', { overlayId, values });
}

/** Give the `<text>` `element` of a title (floating or on a track, not a preset) a box: a width its lines wrap to, or
 *  that it shrinks to fit on one line (`width: null` takes it away). A box is drawn only by glyph outlines, so the
 *  title's text is set as outlines again; a box for a text that cannot be one rejects with why. One edit. */
export async function setTitleTextBox(titleId: string, element: string, textBox: TextBox): Promise<TemplateEdit> {
	if (!inTauri()) {
		const { overlay, clip, warnings } = setTitleTextBoxIn(devTimeline, titleId, element, textBox);
		recordDev('Set text box');
		return { timeline: snapshot(), overlay_id: overlay?.id, clip_id: clip?.id, warnings };
	}
	return invoke<TemplateEdit>('set_title_text_box', { titleId, element, textBox });
}

/** Draw a title's text as glyph outlines (the same on every machine, and what a box needs) or with its fonts again.
 *  Each text that cannot be an outline says why in the warnings; a title none of whose text can rejects. One edit. */
export async function convertTitleText(titleId: string, form: TitleTextForm): Promise<TemplateEdit> {
	if (!inTauri()) {
		const { overlay, clip, warnings } = convertTitleTextIn(devTimeline, titleId, form);
		recordDev(form === 'outlines' ? 'Draw text as outlines' : 'Draw text with its fonts');
		return { timeline: snapshot(), overlay_id: overlay?.id, clip_id: clip?.id, warnings };
	}
	return invoke<TemplateEdit>('convert_title_text', { titleId, form });
}

/** Add a library template as a title clip on a video track, so it takes transitions (a dip to white from the shot
 *  before it), fades and keys and extends like any clip. It runs from `start` for `duration` seconds (default: its own
 *  length) on `trackId` (default: the lowest video track free over its span, else a new one on top). Resolves to the
 *  refreshed timeline, the new clip's id (`clip_id`, to select it) and the warnings. */
export async function applyTemplateClip(
	templateId: string,
	values: Record<string, string>,
	start: number,
	duration?: number | null,
	trackId?: string | null,
	name?: string | null
): Promise<TemplateEdit> {
	if (!inTauri()) {
		const { clip, warnings } = applyTemplateClipTo(devTimeline, templateId, values, start, duration, trackId, name);
		recordDev(`Add template title “${titleOf(clip)?.name ?? ''}”`);
		return { timeline: snapshot(), clip_id: clip.id, warnings };
	}
	return invoke<TemplateEdit>('apply_template_clip', {
		templateId,
		values,
		start,
		duration: duration ?? null,
		trackId: trackId ?? null,
		name: name ?? null
	});
}

/** Add custom SVG artwork as a title clip on a video track (placed as `applyTemplateClip` places one). `duration`
 *  defaults to how long its animations run. */
export async function addTitleClip(
	svg: string,
	start: number,
	duration?: number | null,
	trackId?: string | null,
	name?: string | null
): Promise<TemplateEdit> {
	if (!inTauri()) {
		const checked = validateSvgSource(svg);
		const length = duration ?? 4;
		checkTitleSpan(start, length);
		const clip = titleClip({ name: name?.trim() ?? '', svg: checked.svg }, Math.max(0, start), length);
		placeTitleClip(devTimeline, clip, trackId);
		recordDev('Add title clip');
		return { timeline: snapshot(), clip_id: clip.id, warnings: checked.warnings };
	}
	return invoke<TemplateEdit>('add_title_clip', {
		svg,
		start,
		duration: duration ?? null,
		trackId: trackId ?? null,
		name: name ?? null
	});
}

/** Rename a title clip or replace its artwork (`svg`, which ends a template). One edit; the clip keeps its place. */
export async function updateTitleClip(clipId: string, patch: TitleClipPatch): Promise<TemplateEdit> {
	if (!inTauri()) {
		const clip = devTimeline.tracks.flatMap((t) => t.clips).find((c) => c.id === clipId);
		const title = clip && titleOf(clip);
		if (!clip || !title) throw new Error(`the clip ${clipId} is not a title`);
		if (patch.name === undefined && patch.svg === undefined) throw new Error('nothing to change: give a name or svg');
		let warnings: string[] = [];
		const next = { ...title };
		if (patch.name !== undefined) next.name = patch.name.trim();
		if (patch.svg !== undefined) {
			const checked = validateSvgSource(patch.svg);
			next.svg = checked.svg;
			next.template = undefined;
			warnings = checked.warnings;
		}
		clip.generator = { ...next, kind: 'title' };
		recordDev('Update title clip');
		return { timeline: snapshot(), clip_id: clipId, warnings };
	}
	return invoke<TemplateEdit>('update_title_clip', { clipId, patch });
}

/** Bring a `.kerftemplate` file into the library. Never overwrites: the same template is `already_present`,
 *  another one with its id is kept under a new id (`added_as_copy`). */
export async function importTemplate(path: string): Promise<ImportOutcome> {
	const outcome = inTauri() ? await invoke<ImportOutcome>('import_template', { path }) : importTemplateDev(path);
	if (outcome.status !== 'already_present') noteTemplatesChanged();
	return outcome;
}

/** Write a library template to `path` (a folder gets `<name>.kerftemplate` in it, a path without an
 *  extension gets `.kerftemplate`) and resolve to where it went. The file picker has asked about replacing. */
export async function exportTemplate(templateId: string, path: string): Promise<string> {
	if (!inTauri()) return exportTemplateDev(templateId, path);
	return invoke<string>('export_template', { templateId, path });
}

/** Remove a template from the library. Titles already made from it keep working. */
export async function deleteTemplate(templateId: string): Promise<void> {
	if (inTauri()) await invoke<void>('delete_template', { templateId });
	else deleteTemplateDev(templateId);
	noteTemplatesChanged();
}

const templateListeners = new Set<() => void>();
/** The page's own save, import or delete went through. The backend's `templates-changed` is an agent's doing (the MCP
 *  tools): the Tauri commands the GUI calls emit nothing, so every mounted grid would stay as it was. */
const noteTemplatesChanged = () => [...templateListeners].forEach((cb) => cb());

/** Subscribe to `templates-changed`: the library gained, lost or replaced a template. Fires for an agent's change (the
 *  backend's event) and for this page's own save, import and delete (announced here once the call has resolved, in the
 *  app and in the harness alike), so every mounted listener hears both. Returns an unlisten fn. */
export async function onTemplatesChanged(cb: () => void): Promise<() => void> {
	templateListeners.add(cb);
	if (!inTauri()) return () => void templateListeners.delete(cb);
	try {
		const { listen } = await import('@tauri-apps/api/event');
		const off = await listen('templates-changed', () => cb());
		return () => {
			templateListeners.delete(cb);
			off();
		};
	} catch (e) {
		templateListeners.delete(cb);
		throw e;
	}
}

/** Pick a `.kerftemplate` to import; `null` if cancelled. The harness has no file system: it offers the template most
 *  recently exported in this session (export, delete, import is the round trip), else nothing. */
export async function pickTemplateFile(): Promise<string | null> {
	if (!inTauri()) return latestExportedTemplate();
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({ multiple: false, filters: [{ name: 'Kerf template', extensions: ['kerftemplate'] }] });
	return typeof selected === 'string' ? selected : null;
}

/** Ask where to write a template (`defaultName` is the file's name); `null` if cancelled. The harness answers a path
 *  under `/harness/`, which `exportTemplate` keeps in memory. */
export async function pickTemplateExportPath(defaultName: string): Promise<string | null> {
	if (!inTauri()) return `/harness/${defaultName}`;
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({ filters: [{ name: 'Kerf template', extensions: ['kerftemplate'] }], defaultPath: defaultName });
	return typeof path === 'string' ? path : null;
}

/** Pick a picture for an `image` control (PNG, JPEG, GIF or WebP); `null` if cancelled. The harness answers a stand-in
 *  path, and `templateImageValue` gives any such path the same small picture. */
export async function pickImageFile(): Promise<string | null> {
	if (!inTauri()) return '/harness/logo.png';
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: false,
		filters: [{ name: 'Image', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp'] }]
	});
	return typeof selected === 'string' ? selected : null;
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

/** Caption the cut from a `.srt` / `.vtt` / `.ass` / `.ssa` file on disk. `base` is
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
 *  `<input type=file>`, or pasted. `format` is `srt` / `vtt` / `ass`; omitted, it is
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

/** Pick a `.srt` / `.vtt` / `.ass` / `.ssa` file; `null` if cancelled. Desktop: the native
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

/** Re-time an asset's cached transcript to the speech energy and label its speakers (`S1`,
 *  `S2`, …) without re-transcribing. The harness spreads each line's words by character share
 *  and keeps the speakers it was seeded with. */
export async function refineTranscript(assetId: string): Promise<AssetAnalysis> {
	if (!inTauri()) {
		const analysis = sampleAnalysis[assetId];
		if (!analysis?.transcript.length) throw new Error('asset has no transcript; transcribe it first');
		analysis.transcript = analysis.transcript.map((seg) => {
			const speaker = seg.speaker ?? 'S1';
			return { ...seg, speaker, words: seg.words?.length ? seg.words : estimateWords({ ...seg, speaker }) };
		});
		return { ...analysis };
	}
	return invoke<AssetAnalysis>('refine_transcript', { assetId });
}

/** Write an asset's transcript to a `.srt` file; returns the path. */
export async function exportSrt(assetId: string, outputPath: string): Promise<string> {
	if (!inTauri()) return outputPath;
	return invoke<string>('export_srt', { assetId, outputPath });
}

/** What the browser harness answers for a side-file export: nothing is written there. */
const harnessFile = (path: string): ExportedFile => ({
	path,
	report: { entries: [{ level: 'info', message: 'the browser harness writes no file; the desktop app does', count: 1 }] }
});

/** Write the cut's captions as SubRip or WebVTT (by `format`, else the path's extension). */
export async function exportCaptions(outputPath: string, format?: SubtitleFormat): Promise<string> {
	if (!inTauri()) return outputPath;
	return invoke<string>('export_captions', { outputPath, format: format ?? null });
}

/** Write the cut as OpenTimelineIO or a CMX 3600 EDL (`videoTrack`: which video track an EDL holds). */
export async function exportInterchange(
	outputPath: string,
	format: InterchangeFormat,
	videoTrack?: number
): Promise<ExportedFile> {
	if (!inTauri()) return harnessFile(outputPath);
	return invoke<ExportedFile>('export_interchange', { outputPath, format, videoTrack: videoTrack ?? null });
}

/** Write the markers as a formula-safe CSV or as YouTube chapter text. */
export async function exportMarkers(outputPath: string, format: MarkerListFormat): Promise<ExportedFile> {
	if (!inTauri()) return harnessFile(outputPath);
	return invoke<ExportedFile>('export_markers', { outputPath, format });
}

/** Ask where to save a side-file; `null` if cancelled. The browser harness answers
 *  `defaultPath`, so the export flow stays explorable there. */
export async function pickSavePath(name: string, extensions: string[], defaultPath: string): Promise<string | null> {
	if (!inTauri()) return defaultPath;
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({ filters: [{ name, extensions }], defaultPath });
	return typeof path === 'string' ? path : null;
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

/** What the harness's fit reads the project for: its assets and the analysis it has of them. */
function devFitSources(): FitSources {
	return { assets: sampleAssets, analysisOf: (id) => sampleAnalysis[id] };
}

/**
 * What **Fit to video** would make of a music clip, without making it: the bar-aligned arrangement
 * (source segments in output order), how long it lasts and how far that is from the target
 * (`remainder = target - duration`: positive is short, negative runs over and would be faded).
 * `target` seconds omitted fits the picture — from the clip's start to the end of the last video
 * clip. Rejects when the clip is not music on an audio track or has no bar grid (`analyze` first).
 * The browser harness plans with `music-fit.ts`, the faithful mirror of the Rust planner.
 */
export async function planMusicFit(clipId: string, target?: number | null): Promise<MusicFit> {
	if (!inTauri()) return musicFitInputs(devTimeline, devFitSources(), clipId, target).fit;
	return invoke<MusicFit>('plan_music_fit', { clipId, target: target ?? null });
}

/**
 * **Fit to video**: replace a music clip with the bar-aligned edit list `planMusicFit` shows — intro
 * and ending kept, whole repeating phrases repeated or dropped, each splice crossfaded over 10 ms —
 * as clips on the original file. With `fadeOut`, a fit that runs over its target is cut there and
 * faded out over the last 2 s. One revision; resolves to the refreshed timeline and the report.
 */
export async function fitMusic(clipId: string, target: number | null | undefined, fadeOut: boolean): Promise<MusicFitted> {
	if (!inTauri()) {
		// `edit_timeline_exact`: no ripple; a refusal part-way leaves the timeline as it was.
		const scratch = snapshot();
		const report = fitMusicOnto(scratch, devFitSources(), clipId, target, fadeOut, uid);
		devTimeline = scratch;
		recordDev('Fit music to length');
		return { timeline: snapshot(), report };
	}
	return invoke<MusicFitted>('fit_music', { clipId, target: target ?? null, fadeOut });
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

/** Stop the smart crop in flight (if any); it then rejects with `smart crop cancelled`. */
export async function cancelSmartCrop(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_smart_crop');
}

/** Subscribe to `smart-crop-progress` events: the fraction of the shots sampled so far. Returns an unlisten fn. */
export async function onSmartCropProgress(cb: (p: SmartCropProgress) => void): Promise<() => void> {
	if (!inTauri()) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<SmartCropProgress>('smart-crop-progress', (e) => cb(e.payload));
}

/** Move `clipId` so its sound lines up with `referenceId`'s (a camera's scratch audio against
 *  the recorder): the backend decodes both clips' used source windows and cross-correlates them.
 *  Linked partners move with it. A match that is not `reliable` is reported and nothing moves
 *  unless `force` — which, for the pair just measured, applies that measurement without decoding
 *  again. A wait for the heavy-job slot arrives as `audio-sync-progress` (`onAudioSyncProgress`);
 *  `cancelAudioSync` stops it (`audio sync cancelled`). The harness has no sound to compare: it
 *  answers as if both recordings started together (the same source second at the same timeline
 *  time) with a clear match. */
export async function syncClipsByAudio(referenceId: string, clipId: string, force = false): Promise<SyncedByAudio> {
	if (!inTauri()) {
		if (referenceId === clipId) throw new Error('a clip cannot be synced to itself');
		const ref = locate(devTimeline, referenceId);
		const own = locate(devTimeline, clipId);
		if (!ref || !own) throw new Error('clip not found');
		const partners = linkPartners(devTimeline, clipId);
		if (partners.includes(referenceId)) {
			throw new Error('the two clips are linked and move together; detach them first');
		}
		if (ref[0].id === own[0].id) {
			throw new Error(`both clips are on track ${own[0].name}; in sync they would overlap, so move one to another track first`);
		}
		for (const id of [clipId, ...partners]) {
			const track = locate(devTimeline, id)?.[0];
			if (track?.locked) throw new Error(`track ${track.name} is locked`);
		}
		const [r, c] = [ref[0].clips[ref[1]], own[0].clips[own[1]]];
		for (const x of [r, c]) {
			if (!assetById(x.asset_id)?.streams.some((st) => st.kind === 'audio')) throw new Error(`clip ${x.id} has no sound to sync by`);
			if ((x.speed ?? 1) !== 1) throw new Error(`clip ${x.id} plays at another speed or reversed; sync needs both at normal speed`);
		}
		const start = r.timeline_start - r.source_in + c.source_in;
		if (start < 0) throw new Error(`in sync the clip would start ${(-start).toFixed(3)}s before the timeline does; move the reference later first`);
		const offset = start - c.timeline_start;
		const timeline = await moveClip(clipId, start);
		return {
			sync: { reference_id: referenceId, clip_id: clipId, offset, timeline_start: start, confidence: 24, distinct: 3, reliable: true, inverted: false, applied: true },
			timeline
		};
	}
	return invoke<SyncedByAudio>('sync_clips_by_audio', { referenceId, clipId, force });
}

/** Stop the sync by audio in flight; it then rejects with `audio sync cancelled`. */
export async function cancelAudioSync(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_audio_sync');
}

/** Subscribe to `audio-sync-progress` (a sync waiting for another job). Returns an unlisten fn;
 *  the harness never waits. */
export async function onAudioSyncProgress(cb: (p: AudioSyncProgress) => void): Promise<() => void> {
	if (!inTauri()) return () => {};
	const { listen } = await import('@tauri-apps/api/event');
	return listen<AudioSyncProgress>('audio-sync-progress', (e) => cb(e.payload));
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

/** `offset` of the newest entries skipped, then up to `limit` of the ones before them, kept in the list's own
 *  (oldest-first) order — the backend's `Page`. Neither is everything. */
function windowOf<T>(all: T[], limit?: number, offset?: number): T[] {
	if (limit === undefined && !offset) return all;
	const end = Math.max(0, all.length - (offset ?? 0));
	const start = limit === undefined ? 0 : Math.max(0, end - limit);
	return all.slice(start, end);
}

/** The edit history, oldest first. `limit` / `offset` window it from the newest end: `limit: 50` is the last
 *  fifty edits and a following `offset: 50` the fifty before. */
export async function getHistory(limit?: number, offset?: number): Promise<Revision[]> {
	if (!inTauri()) {
		return windowOf(
			devHistory.map((r) => ({
				seq: r.seq,
				label: r.label,
				source: r.source,
				created_at: r.created_at,
				current: r.seq === devHead
			})),
			limit,
			offset
		);
	}
	return invoke<Revision[]>('get_history', { limit: limit ?? null, offset: offset ?? null });
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

/** How long the cut runs without its titles on tracks, which have no sound (`Timeline::untitled_duration`): what a
 *  pass over the mix measures. */
function untitledDuration(tl: Timeline): number {
	return timelineDuration({ ...tl, tracks: tl.tracks.map((t) => ({ ...t, clips: t.clips.filter((c) => !isTitleClip(c)) })) });
}

/** Why a range starting at `start` measures nothing of the mix (`Timeline::range_past_the_sound`). */
function rangePastTheSound(tl: Timeline, start: number): string | null {
	const cut = untitledDuration(tl);
	if (cut > 0 && start < cut) return null;
	return timelineDuration(tl) > cut + 1e-9
		? `range starts at ${start.toFixed(1)}s, but the cut ends at ${cut.toFixed(1)}s before its titles, which have no sound`
		: `range starts at ${start.toFixed(1)}s but the cut is only ${cut.toFixed(1)}s long`;
}

/** The `true` in the browser harness: `?staged=1` seeds a fake agent proposal. */
function fakeStagedRequested(): boolean {
	return typeof window !== 'undefined' && new URLSearchParams(window.location.search).has('staged');
}

/** One thing the agent did inside the proposal: its label, what it changed and the proposal after it. */
interface DevStep {
	label: string;
	entries: DiffEntry[];
	timeline: Timeline;
}

interface DevStaged {
	edit: StagedEdit;
	/** The proposal. */
	timeline: Timeline;
	/** The cut it branched from. */
	base: Timeline;
	/** The steps it can still take back, oldest first (`undoStagedEdit`). */
	steps: DevStep[];
}

let devStaged: DevStaged | null = null;
/** The proposal thrown away last, kept so a discard made by mistake can be taken back. */
let devDiscarded: DevStaged | null = null;
let devStagedSeeded = false;

const countClips = (tl: Timeline) => tl.tracks.reduce((n, t) => n + t.clips.length, 0);

/** A harness diff entry with the id the backend would give it. */
const named = (entry: DiffEntry): DiffEntry => ({ ...entry, id: entryId(entry) });

function devDiff(base: Timeline, proposed: Timeline, entries: DiffEntry[]): TimelineDiff {
	return {
		entries,
		duration_before: timelineDuration(base),
		duration_after: timelineDuration(proposed),
		clips_before: countClips(base),
		clips_after: countClips(proposed)
	};
}

/** A plausible proposal over the dev timeline: a tightened intro, ripple closed. */
function seedDevStaged() {
	devStagedSeeded = true;
	const before = snapshot();
	const proposed = snapshot();
	const video = proposed.tracks.find((t) => t.kind === 'video');
	const audio = proposed.tracks.find((t) => t.kind === 'audio');
	if (!video || video.clips.length < 2 || !audio || audio.clips.length === 0) return;

	const steps: DevStep[] = [];
	const step = (label: string, entry: DiffEntry) =>
		steps.push({ label, entries: [named(entry)], timeline: structuredClone(proposed) });

	const [first, second] = video.clips;
	const wasSource = first.source_out - first.source_in;
	first.source_out = first.source_in + wasSource - 3.5;
	step('Trim clip', {
		kind: 'clip_retrimmed',
		summary: `Trimmed clip on ${video.name} at ${fmtTime(first.timeline_start)} — ${wasSource.toFixed(1)}s → ${(wasSource - 3.5).toFixed(1)}s (-3.5s)`,
		track_id: video.id,
		clip_id: first.id,
		at: first.timeline_start
	});
	const movedFrom = second.timeline_start;
	second.timeline_start = trackEnd({ ...video, clips: [first] });
	step('Move clip', {
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
	step('Trim clip', {
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
	step('Add overlay', {
		kind: 'overlay_added',
		summary: `Added text “${overlay.text}” at ${fmtTime(overlay.start)}–${fmtTime(overlay.end)}`,
		overlay_id: overlay.id,
		at: overlay.start
	});
	// With the sample song on a track of its own (`?music=1`), the proposal also drops it, so the
	// review has a removal to draw.
	const bedTrack = proposed.tracks.find((t) => t.kind === 'audio' && t !== audio && t.clips.length > 0);
	if (bedTrack) {
		const [gone] = bedTrack.clips.splice(0, 1);
		step('Remove clip', {
			kind: 'clip_removed',
			summary: `Removed clip from ${bedTrack.name} at ${fmtTime(gone.timeline_start)} (${clipDuration(gone).toFixed(1)}s)`,
			track_id: bedTrack.id,
			clip_id: gone.id,
			at: gone.timeline_start
		});
	}

	devStaged = {
		timeline: proposed,
		base: before,
		steps,
		edit: {
			base_seq: devHead,
			task_id: devTasks.find((t) => t.status === 'ready')?.id ?? null,
			note: 'Tighten the intro',
			owned_by_agent: true,
			edits: steps.map((st) => st.label),
			undo_depth: steps.length,
			created_at: now(),
			updated_at: now(),
			stale: false,
			diff: devDiff(
				before,
				proposed,
				steps.flatMap((st) => st.entries)
			)
		}
	};
}

/** Whether the live cut moved on since the proposal branched from it — `Project::staged`'s `stale`: the
 *  history head is not where the agent started. Applying it then replaces the newer cut and asks for `force`. */
function devStagedStale(): boolean {
	return !!devStaged && devStaged.edit.base_seq !== devHead;
}

const STALE_MESSAGE =
	'the timeline changed since these edits were staged — applying would replace that newer cut; apply with force, or discard the staged edit';

/** The proposal a connected agent has staged, or `null` when there is none. */
export async function getStagedEdit(): Promise<StagedEdit | null> {
	if (!inTauri()) {
		if (!devStagedSeeded && fakeStagedRequested()) seedDevStaged();
		return devStaged ? { ...structuredClone(devStaged.edit), stale: devStagedStale() } : null;
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
		if (devStagedStale() && !force) throw new Error(STALE_MESSAGE);
		devTimeline = structuredClone(devStaged.timeline);
		recordDev(devStaged.edit.note ?? 'Agent edit');
		devHistory[devHead].source = 'agent';
		// Accepting the edits finishes the task they were staged under, as `apply_staged` does.
		const taskId = devStaged.edit.task_id;
		devTasks = devTasks.map((t) => (t.id === taskId ? { ...t, status: 'done', updated_at: now() } : t));
		devStaged = null;
		return snapshot();
	}
	return invoke<Timeline>('apply_staged_edit', { force });
}

/**
 * Accept only the picked changes of the proposal (`entryIds`: the `id`s of its diff entries) onto the live
 * cut, by the clip, text, marker or track each is about. A partner that has to come along (a picture and its
 * sound) is applied too and reported in `auto_included`; a pick that cannot stand alone rejects with where,
 * and one the cut has since moved past is `skipped`. The rest of the proposal stays open, branched from the
 * cut as it now is, until nothing is left of it (`closed`). The browser harness runs the port in
 * `staged-subset.ts`.
 */
export async function applyStagedSubset(entryIds: string[], force = false): Promise<SubsetApplied> {
	if (!inTauri()) {
		if (!devStaged) throw new Error('no staged edit is pending');
		if (devStagedStale() && !force) throw new Error(STALE_MESSAGE);
		const total = devStaged.edit.diff.entries.length;
		const merge = mergeSubset(devTimeline, devStaged.base, devStaged.timeline, devStaged.edit.diff, entryIds);
		if (merge.applied.length === 0) {
			const why = merge.skipped.map((c) => `${c.summary} (${c.why})`).join('; ');
			throw new Error(`invalid argument: none of those changes can be applied: ${why}`);
		}
		const applied = new Set(merge.applied);
		const left = devStaged.edit.diff.entries.filter((e) => !applied.has(entryId(e)));
		devTimeline = merge.timeline;
		recordDev(`${devStaged.edit.note ?? 'Agent edit'} (${merge.applied.length} of ${total})`);
		devHistory[devHead].source = 'agent';
		const closed = left.length === 0;
		if (closed) {
			const taskId = devStaged.edit.task_id;
			devTasks = devTasks.map((t) => (t.id === taskId ? { ...t, status: 'done', updated_at: now() } : t));
			devStaged = null;
		} else {
			// The rest of the proposal now branches from the cut as it stands, and its steps are forgotten.
			devStaged.base = snapshot();
			devStaged.steps = [];
			devStaged.edit = {
				...devStaged.edit,
				base_seq: devHead,
				undo_depth: 0,
				updated_at: now(),
				diff: devDiff(devStaged.base, devStaged.timeline, left)
			};
		}
		return {
			applied: merge.applied.length,
			auto_included: merge.autoIncluded,
			skipped: merge.skipped,
			remaining: left.length,
			closed,
			message: subsetMessage(merge.applied.length, total, merge.autoIncluded, merge.skipped, left.length),
			timeline: snapshot()
		};
	}
	return invoke<SubsetApplied>('apply_staged_subset', { entryIds, force });
}

/** Throw the proposal away, leaving the live timeline untouched. The last one thrown away can be brought
 *  back with `restoreDiscardedEdits`. */
export async function discardStagedEdit(): Promise<Timeline> {
	if (!inTauri()) {
		if (!devStaged) throw new Error('no staged edit is pending');
		if (devStaged.edit.edits.length > 0) devDiscarded = devStaged;
		devStaged = null;
		return snapshot();
	}
	return invoke<Timeline>('discard_staged_edit');
}

/** Take back the latest edit the agent staged, inside the proposal (the live cut is untouched). Resolves to
 *  the step's label and the proposal as it now stands; rejects when nothing is left to undo. */
export async function undoStagedEdit(): Promise<StagedUndone> {
	if (!inTauri()) {
		if (!devStaged) throw new Error('no staged edit is pending');
		const step = devStaged.steps.pop();
		if (!step) throw new Error('invalid argument: nothing left to undo in the proposal (it remembers the last 20 steps)');
		const proposal = devStaged.steps[devStaged.steps.length - 1]?.timeline ?? devStaged.base;
		devStaged.timeline = structuredClone(proposal);
		devStaged.edit = {
			...devStaged.edit,
			edits: devStaged.steps.map((st) => st.label),
			undo_depth: devStaged.steps.length,
			updated_at: now(),
			diff: devDiff(
				devStaged.base,
				devStaged.timeline,
				devStaged.steps.flatMap((st) => st.entries)
			)
		};
		return { label: step.label, staged: { ...structuredClone(devStaged.edit), stale: devStagedStale() } };
	}
	return invoke<StagedUndone>('undo_staged_edit');
}

/** The proposal last discarded, while it can still be brought back; `null` when there is none. */
export async function discardedStagedEdit(): Promise<StagedEdit | null> {
	if (!inTauri()) return devDiscarded ? { ...structuredClone(devDiscarded.edit), stale: devDiscarded.edit.base_seq !== devHead } : null;
	return invoke<StagedEdit | null>('discarded_staged_edit');
}

/** Bring the discarded proposal back (only while no other proposal is open). */
export async function restoreDiscardedEdits(): Promise<StagedEdit> {
	if (!inTauri()) {
		if (devStaged) throw new Error('a staged edit is already pending; apply or discard it first');
		if (!devDiscarded) throw new Error('there is no discarded proposal to bring back');
		devStaged = devDiscarded;
		devDiscarded = null;
		// It comes back as it was but with no undo history (`Project::restore_discarded`).
		devStaged.steps = [];
		devStaged.edit = { ...devStaged.edit, undo_depth: 0, updated_at: now() };
		return { ...structuredClone(devStaged.edit), stale: devStagedStale() };
	}
	return invoke<StagedEdit>('restore_discarded_edits');
}

// ---- agent task queue ------------------------------------------------------
//
// The desktop app persists tasks in kerf-core; a connected LLM claims and works
// them over MCP. In the browser there is no agent, so queued tasks simply wait —
// which is the honest behavior: Kerf never edits on its own.

/** The agent task queue, oldest first; `limit` / `offset` window it from the newest end like `getHistory`. */
export async function listTasks(limit?: number, offset?: number): Promise<Task[]> {
	if (!inTauri()) return structuredClone(windowOf(devTasks, limit, offset));
	return invoke<Task[]>('list_tasks', { limit: limit ?? null, offset: offset ?? null });
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

/**
 * The video scopes of the composited frame at `timeSecs` (`get_scopes`): the export's still of
 * the cut, from the originals, read with the delivery's matrix (or `matrix`). The numeric summary
 * always; the `grids` asked for at `size`. A real decode, so callers throttle it. In the browser
 * harness: the scopes of colour bars.
 */
export async function getScopes(
	timeSecs: number,
	grids: ScopeKind[] = [],
	size?: ScopeGridSize,
	matrix: ScopeMatrix = 'auto',
	maxWidth?: number
): Promise<Scopes> {
	if (!inTauri()) return harnessScopes(grids, size);
	return invoke<Scopes>('get_scopes', { timeSecs, grids, size: size ?? null, matrix, maxWidth: maxWidth ?? null });
}

/**
 * How the Preview will play each span of the cut (`get_render_bar`): the GPU preview where it
 * is on and draws it, the FFmpeg stream where it hands over, and the spans where playback stalls.
 * `width` is the Preview's render width. In the browser harness: the reversed clips.
 */
export async function getRenderBar(width = 960): Promise<RenderBar> {
	if (!inTauri()) return harnessRenderBar(devTimeline);
	return invoke<RenderBar>('get_render_bar', { width });
}

/**
 * The frame under the playhead for the Preview panel — drawn by the GPU compositor in the
 * native surface when the GPU preview is on and draws it exactly, else FFmpeg's JPEG (the
 * same one {@link getTimelineFrame} returns), in the same call, with the reasons. `overlays`
 * says the page has something to draw over the picture (a title box, the trim monitor,
 * safe-area guides), which a surface above the page cannot show. `order` says which request
 * of which pump this is (`PreviewOrder`): the calls are separate tasks over the IPC and can
 * arrive reversed, and the backend answers `superseded` to one that arrives after a newer one
 * of the same `client` instead of drawing it last. `play` makes it a frame of **GPU playback**
 * (`hint: 'forward'`): the time is the one the picture should show, asked once per animation
 * frame while the cut plays, and the first request of a `session` decides whether the GPU plays
 * it or the stream does (`renderer: 'stream'`). `draft` is what a gesture under way would write
 * (`preview-draft.ts`): the frame is drawn as the cut would be with it written, and nothing is; a frame
 * of it the GPU does not draw is the JPEG of the cut as it is. In the browser harness there is no
 * backend: the answer is "FFmpeg, no frame" (to a forward request, "the stream"), as `getTimelineFrame`
 * is `null`, and `order` is unused.
 */
export async function getPreviewFrame(
	timeSecs: number,
	maxWidth = 960,
	overlays = false,
	order?: PreviewOrder,
	play?: PlayAsk,
	draft?: PreviewDraft | null
): Promise<PreviewFrameResult> {
	if (!inTauri()) {
		// `?gpusurface=1` pretends a surface under a transparent webview drew the frame, so the
		// page's side of it (the hole in the pane, nothing painted over the surface) can be looked
		// at under `bun run dev`. Otherwise there is no GPU here.
		if (harnessSurface()) {
			// `?gpuplay=stream` makes the GPU decline every session (the stream plays, as when a frame is
			// refused); `?gpuplay=ahead` has it draw for two seconds and then announce a refusal ahead.
			const mode = harnessPlay();
			if (play && mode === 'stream') {
				return {
					renderer: 'stream',
					frame: null,
					reasons: ['the browser harness was told to decline'],
					timings: null,
					play: null
				};
			}
			// A play the stream took over is taken back at once under `?gpuplay=ahead` (and draws two more seconds).
			if (play?.ready) {
				return mode === 'ahead'
					? {
							renderer: 'ready',
							frame: null,
							reasons: [],
							timings: null,
							play: { session: play.session, start: timeSecs, frame: 0, repeat: false, ahead: null, resume: timeSecs }
						}
					: { renderer: 'stream', frame: null, reasons: ['the browser harness does not take a play back'], timings: null, play: null };
			}
			if (play) harnessPlayStart.set(play.session, harnessPlayStart.get(play.session) ?? timeSecs);
			const began = play ? (harnessPlayStart.get(play.session) ?? timeSecs) : 0;
			const stops = began + 2;
			if (play && mode === 'ahead' && timeSecs >= stops) {
				return {
					renderer: 'stream',
					frame: null,
					reasons: ['the browser harness announced a refusal'],
					timings: null,
					play: {
						session: play.session,
						start: began,
						frame: 0,
						repeat: false,
						ahead: { at: stops, handover: stops - 0.1, kind: 'mask', why: 'the browser harness announces a refusal' }
					}
				};
			}
			return {
				renderer: 'gpu',
				frame: null,
				reasons: [],
				timings: { width: 1920, height: 1080, decode_ms: 4, composite_ms: 9, present_ms: 1 },
				play: play
					? {
							session: play.session,
							start: began,
							frame: Math.max(0, Math.floor((timeSecs - began) * 30)),
							repeat: false,
							ahead:
								mode === 'ahead'
									? { at: stops, handover: stops - 0.1, kind: 'mask', why: 'the browser harness announces a refusal' }
									: null
						}
					: null
			};
		}
		return {
			renderer: play ? 'stream' : 'ffmpeg',
			frame: null,
			reasons: ['the browser harness has no GPU'],
			timings: null,
			play: null
		};
	}
	return invoke<PreviewFrameResult>('get_preview_frame', {
		timeSecs,
		maxWidth,
		overlays,
		client: order?.client,
		seq: order?.seq,
		hint: play ? (play.ready ? 'ready' : 'forward') : undefined,
		session: play?.session,
		draft: play ? undefined : (draft ?? undefined)
	});
}

/** When each GPU play session of the harness began (its first request's time). */
const harnessPlayStart = new Map<number, number>();

/** `?gpuplay=stream|ahead` in the browser harness (see {@link getPreviewFrame}). */
function harnessPlay(): 'gpu' | 'stream' | 'ahead' {
	try {
		const mode = new URLSearchParams(location.search).get('gpuplay');
		return mode === 'stream' || mode === 'ahead' ? mode : 'gpu';
	} catch {
		return 'gpu';
	}
}

/** The browser harness was opened with `?gpusurface=1`. */
function harnessSurface(): boolean {
	try {
		return new URLSearchParams(location.search).get('gpusurface') === '1';
	} catch {
		return false;
	}
}

/** Tell the backend where the Preview frame is (see {@link PreviewBoundsReport}). */
export async function setPreviewBounds(bounds: PreviewBoundsReport): Promise<void> {
	if (!inTauri()) return;
	await invoke<void>('set_preview_bounds', { bounds });
}

/** What the GPU preview is doing on this machine. The browser harness has none. */
export async function gpuPreviewStatus(): Promise<GpuPreviewStatus> {
	if (!inTauri()) {
		if (harnessSurface()) {
			return {
				enabled: true,
				supported: true,
				technique: 'window',
				ready: true,
				overlays: true,
				adapter: 'browser harness (pretend)',
				software: false,
				reason: null
			};
		}
		return {
			enabled: false,
			supported: false,
			technique: null,
			ready: false,
			overlays: false,
			adapter: null,
			software: false,
			reason: 'The GPU preview is part of the desktop app.'
		};
	}
	return invoke<GpuPreviewStatus>('gpu_preview_status');
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

/** Render the cut at every delivery frame — one file per shape beside
 *  `outputPath`, named by shape (`cut-9x16.mp4`). With `smartCrop` every shot
 *  is framed for every shape first, recorded on the clips as one revision.
 *  `singlePass` (the default) writes every file from one decode of the cut where
 *  the encode allows; false renders them one after another. */
export async function exportVariants(
	outputPath: string,
	formats: Delivery[],
	smartCrop: boolean,
	options: ExportOptions,
	singlePass = true
): Promise<string[]> {
	if (!inTauri()) throw new Error('export is only available in the desktop app');
	return invoke<string[]>('export_variants', { outputPath, formats, smartCrop, singlePass, options });
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
export async function pickExportPath(ext: string, defaultName?: string): Promise<string | null> {
	if (!inTauri()) return null;
	const { save } = await import('@tauri-apps/plugin-dialog');
	const path = await save({
		filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
		defaultPath: defaultName ?? `kerf-export.${ext}`
	});
	return typeof path === 'string' ? path : null;
}

/** Stop the cover frame being written (if any); `exportCover` then rejects with `cover export cancelled`. */
export async function cancelCoverExport(): Promise<void> {
	if (!inTauri()) return;
	return invoke<void>('cancel_cover_export');
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

/** How ready the current cut is for each publishing target, with the cut it was judged on (its
 *  frame and length, in words). `frame` overrides the shape it is judged at and `range` the span —
 *  the export dialog passes the resolution it is about to render when that differs from the
 *  project frame, and the In → out it is about to write.
 *
 *  `kerf_core::platform` decides this in the app. The browser harness runs the
 *  mirror in `platforms.ts` over the dev timeline so the panel is explorable
 *  under `bun run dev`. */
export async function platformCheck(
	frame?: [number, number] | null,
	range?: { start: number; end: number } | null
): Promise<PlatformReport> {
	if (!inTauri()) {
		const fmt = devTimeline.format;
		const first = sampleAssets[0];
		const full = timelineDuration(devTimeline);
		const span = exportSpan(full, range);
		// A title counts when it is on screen somewhere in the span, as it is in the sliced cut the export builds.
		return platformReport({
			duration: span ? span[1] - span[0] : full,
			width: frame?.[0] ?? fmt?.width ?? first?.streams[0]?.width ?? 1920,
			height: frame?.[1] ?? fmt?.height ?? first?.streams[0]?.height ?? 1080,
			has_audio: true,
			has_text: (devTimeline.overlays ?? []).some((o) => !span || (o.end > span[0] && o.start < span[1]))
		});
	}
	return invoke<PlatformReport>('platform_check', {
		width: frame?.[0] ?? null,
		height: frame?.[1] ?? null,
		range: range ?? null
	});
}

let platformTargetsKnown: Promise<PlatformTarget[]> | null = null;

/** Every publishing target Kerf knows (its frame and length limits), as `kerf_core::platform` has them. The
 *  answer never changes in a session, so it is asked once. The browser harness answers with the mirror in
 *  `platforms.ts`. */
export async function platformTargets(): Promise<PlatformTarget[]> {
	if (!inTauri()) return structuredClone(TARGETS);
	platformTargetsKnown ??= invoke<PlatformTarget[]>('platform_targets').catch((e) => {
		platformTargetsKnown = null;
		throw e;
	});
	return platformTargetsKnown;
}

// ---- agent connection (MCP endpoint) ---------------------------------------

/** Where the endpoint is, and how long ago an agent last used it — `null` when
 *  none ever has. A streamable-HTTP client holds no connection between calls,
 *  so there is no socket to report as open; the panel judges from the age. In
 *  the browser harness there is no server at all, hence `null`. */
export async function agentStatus(): Promise<{ endpoint: string; last_seen_secs: number | null; error: string | null }> {
	if (!inTauri()) return { endpoint: 'http://127.0.0.1:7777/mcp', last_seen_secs: null, error: null };
	return invoke<{ endpoint: string; last_seen_secs: number | null; error: string | null }>('agent_status');
}

// ---- agent connection: endpoint, token, switches (mcp_commands.rs) ------------

/** In the browser harness there is no server: this is the state the Agent settings page edits. */
let harnessMcp: McpConnectionInfo | null = null;

function harnessToken(): string {
	return (crypto.randomUUID() + crypto.randomUUID()).replace(/-/g, '');
}

/** The agent switches as `get_settings` carries them (`SettingsView`): the same state the Agent page edits. */
function harnessAgentSettings(): Pick<AppSettings, 'agent_review' | 'mcp_enabled' | 'mcp_addr' | 'mcp_require_token'> {
	const info = harnessMcpInfo({});
	return {
		agent_review: info.agent_review,
		mcp_enabled: info.enabled,
		mcp_addr: info.configured_addr,
		mcp_require_token: info.require_token
	};
}

/** A settings patch that carries agent switches changes the Agent page's state too. */
function applyHarnessAgentSettings(patch: SettingsPatch) {
	const next: McpSettingsPatch = {};
	if (patch.agent_review !== undefined) next.agent_review = patch.agent_review;
	if (patch.mcp_enabled !== undefined) next.mcp_enabled = patch.mcp_enabled;
	if (patch.mcp_require_token !== undefined) next.mcp_require_token = patch.mcp_require_token;
	if ('mcp_addr' in patch) next.mcp_addr = patch.mcp_addr ?? null;
	if (Object.keys(next).length > 0) void setMcpSettings(next).catch(() => {});
}

/** `?tokenproblem=1` seeds the warning the Agent page shows when the token could not be kept. */
function harnessTokenProblem(): string | null {
	if (typeof window === 'undefined' || !new URLSearchParams(window.location.search).has('tokenproblem')) return null;
	return 'The token could not be saved (permission denied). It works until Kerf quits; the next start makes a new one.';
}

function harnessMcpInfo(next: Partial<McpConnectionInfo>): McpConnectionInfo {
	const base: McpConnectionInfo = harnessMcp ?? {
		enabled: true,
		url: 'http://127.0.0.1:7777/mcp',
		require_token: true,
		token: harnessToken(),
		token_problem: harnessTokenProblem(),
		running: true,
		error: null,
		configured_addr: null,
		env_override: false,
		agent_review: true,
		claude_code_command: '',
		generic_json_config: ''
	};
	const merged = { ...base, ...next };
	merged.url = clientUrl(merged.configured_addr ?? '127.0.0.1:7777');
	merged.running = merged.enabled;
	merged.claude_code_command = claudeCodeCommand(merged.url, merged.token, merged.require_token);
	merged.generic_json_config = genericJsonConfig(merged.url, merged.token, merged.require_token);
	harnessMcp = merged;
	return merged;
}

/** The endpoint, the token and ready-made client configuration, and whether the server is up. */
export async function mcpConnectionInfo(): Promise<McpConnectionInfo> {
	if (!inTauri()) return harnessMcpInfo({});
	return invoke<McpConnectionInfo>('mcp_connection_info');
}

/** Make a new token; the old one stops working at once. Resolves to the new token. */
export async function regenerateMcpToken(): Promise<string> {
	if (!inTauri()) {
		const token = harnessToken();
		harnessMcpInfo({ token });
		return token;
	}
	return invoke<string>('regenerate_mcp_token');
}

/** Change the agent switches (enabled, address, token requirement, review of agent edits); resolves
 *  to the connection info as it is afterwards. Rejects when the combination is refused (a network
 *  address with the token requirement off, an address that is not `host:port`). */
export async function setMcpSettings(patch: McpSettingsPatch): Promise<McpConnectionInfo> {
	if (!inTauri()) {
		const next: Partial<McpConnectionInfo> = {};
		if (patch.mcp_enabled !== undefined) next.enabled = patch.mcp_enabled;
		if (patch.mcp_require_token !== undefined) next.require_token = patch.mcp_require_token;
		if (patch.agent_review !== undefined) next.agent_review = patch.agent_review;
		if ('mcp_addr' in patch) next.configured_addr = patch.mcp_addr ?? null;
		const current = harnessMcpInfo({});
		const addr = ('mcp_addr' in patch ? patch.mcp_addr : current.configured_addr) ?? '127.0.0.1:7777';
		const require = patch.mcp_require_token ?? current.require_token;
		if (!/^(127\.\d+\.\d+\.\d+|localhost|\[::1\]):/i.test(addr) && !require) {
			throw new Error(
				`refusing to listen on ${addr}: it is reachable from the network, and requests would carry no token`
			);
		}
		return harnessMcpInfo(next);
	}
	return invoke<McpConnectionInfo>('set_mcp_settings', { patch });
}

/** Tell the backend what the editor shows, for an agent's `get_editor_context`
 *  (`editor-context.ts` throttles it). The browser harness has no agent to ask, so nothing is sent. */
export async function reportUiContext(context: UiContext): Promise<void> {
	if (!inTauri()) return;
	await invoke('report_ui_context', { context });
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
			auto_analysis: readBrowserAutoAnalysis(),
			proxy_size: readBrowserProxySize(),
			preview_source: readBrowserPreviewSource(),
			safe_areas: readBrowserSafeAreas(),
			gpu_preview: harnessSurface(),
			time_display: readBrowserTimeDisplay(),
			reopen_last_project: readBrowserFlag(REOPEN_KEY),
			recent_projects: harnessRecent(),
			layout: readBrowserJson(LAYOUT_KEY),
			theme: readBrowserJson(THEME_KEY),
			workspaces: readBrowserJson(WORKSPACES_KEY),
			keybindings: readBrowserJson(KEYBINDINGS_KEY),
			...harnessAgentSettings(),
			editing: readBrowserJson(EDITING_KEY)
		});
	}
	return invoke<SettingsView>('get_settings');
}

/**
 * Persist the fields that changed and put them into force; returns the resolved
 * view. Only the fields in `patch` are written — the backend merges them into
 * the stored file, so a workspaces write cannot carry a stale copy of the theme.
 */
export async function setSettings(patch: SettingsPatch): Promise<SettingsView> {
	if (!inTauri()) {
		try {
			if (patch.cpu_percent !== undefined) {
				const percent = Math.round(Math.min(100, Math.max(MIN_CPU_PERCENT, patch.cpu_percent)));
				localStorage.setItem(CPU_KEY, String(percent));
			}
			if (patch.auto_analysis !== undefined) localStorage.setItem(AUTO_ANALYSIS_KEY, JSON.stringify(patch.auto_analysis));
			if (patch.proxy_size !== undefined) localStorage.setItem(PROXY_SIZE_KEY, String(patch.proxy_size));
			if (patch.preview_source !== undefined) localStorage.setItem(PREVIEW_SOURCE_KEY, patch.preview_source);
			if (patch.safe_areas !== undefined) localStorage.setItem(SAFE_AREAS_KEY, patch.safe_areas ? '1' : '0');
			if (patch.time_display !== undefined) localStorage.setItem(TIME_DISPLAY_KEY, patch.time_display);
			if (patch.reopen_last_project !== undefined) localStorage.setItem(REOPEN_KEY, patch.reopen_last_project ? '1' : '0');
			if ('layout' in patch) writeBrowserJson(LAYOUT_KEY, patch.layout);
			if ('theme' in patch) writeBrowserJson(THEME_KEY, patch.theme);
			if ('workspaces' in patch) writeBrowserJson(WORKSPACES_KEY, patch.workspaces);
			if ('keybindings' in patch) writeBrowserJson(KEYBINDINGS_KEY, patch.keybindings);
			applyHarnessAgentSettings(patch);
			if ('editing' in patch) writeBrowserJson(EDITING_KEY, patch.editing);
		} catch {
			// A private window with storage blocked still gets a working dialog.
		}
		return getSettings();
	}
	return invoke<SettingsView>('set_settings', { patch });
}

const CPU_KEY = 'kerf.settings.cpuPercent';
const AUTO_ANALYSIS_KEY = 'kerf.settings.autoAnalysis';
const PROXY_SIZE_KEY = 'kerf.settings.proxySize';
const PREVIEW_SOURCE_KEY = 'kerf.settings.previewSource';
const LEGACY_TRANSCRIBE_KEY = 'kerf.settings.transcribe';
const SAFE_AREAS_KEY = 'kerf.settings.safeAreas';
const LAYOUT_KEY = 'kerf.settings.layout';
const THEME_KEY = 'kerf.settings.theme';
const WORKSPACES_KEY = 'kerf.settings.workspaces';
const KEYBINDINGS_KEY = 'kerf.settings.keybindings';
const EDITING_KEY = 'kerf.settings.editing';
const TIME_DISPLAY_KEY = 'kerf.settings.timeDisplay';
const REOPEN_KEY = 'kerf.settings.reopenLastProject';
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

/** The automatic-analysis set the harness remembers. A harness that stored the old on/off
 *  transcription flag carries it forward, as the backend does for its settings file. */
function readBrowserAutoAnalysis(): AutoAnalysis {
	const all: AutoAnalysis = { enabled: true, silence: true, scenes: true, loudness: true, rhythm: true, transcript: true };
	try {
		const stored = localStorage.getItem(AUTO_ANALYSIS_KEY);
		if (stored) return { ...all, ...JSON.parse(stored) };
		if (localStorage.getItem(LEGACY_TRANSCRIBE_KEY) === '0') return { ...all, transcript: false };
	} catch {
		// Ignore — the defaults.
	}
	return all;
}

function readBrowserProxySize(): ProxySize {
	try {
		const n = Number(localStorage.getItem(PROXY_SIZE_KEY));
		if (n === 0 || n === 720 || n === 1080 || n === 1280) {
			if (localStorage.getItem(PROXY_SIZE_KEY) !== null) return n;
		}
	} catch {
		// Ignore — the default.
	}
	return 1280;
}

function readBrowserPreviewSource(): PreviewSource {
	try {
		const v = localStorage.getItem(PREVIEW_SOURCE_KEY);
		if (v === 'original' || v === 'proxy_only') return v;
	} catch {
		// Ignore — the default.
	}
	return 'auto';
}

function readBrowserSafeAreas(): boolean {
	try {
		return localStorage.getItem(SAFE_AREAS_KEY) === '1';
	} catch {
		return false;
	}
}

function readBrowserTimeDisplay(): TimeDisplay {
	try {
		return localStorage.getItem(TIME_DISPLAY_KEY) === 'seconds' ? 'seconds' : 'timecode';
	} catch {
		return 'timecode';
	}
}

function readBrowserFlag(key: string): boolean {
	try {
		return localStorage.getItem(key) === '1';
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
	const mcp = harnessMcpInfo({});
	return {
		...settings,
		agent_review: mcp.agent_review,
		mcp_enabled: mcp.enabled,
		mcp_addr: mcp.configured_addr,
		mcp_require_token: mcp.require_token,
		cpu_cores: cores,
		cpu_threads: Math.min(cores, Math.max(1, Math.round((cores * settings.cpu_percent) / 100))),
		cpu_min_percent: MIN_CPU_PERCENT
	};
}

/**
 * Put every preference back to its default (the old settings file is kept beside it as
 * `settings.reset-<time>.json`) and resolve to the view in force. The recent-projects list is not a
 * preference and stays. In the browser harness the remembered values are cleared.
 */
export async function resetSettings(): Promise<SettingsView> {
	if (!inTauri()) {
		try {
			for (const key of [
				CPU_KEY,
				AUTO_ANALYSIS_KEY,
				PROXY_SIZE_KEY,
				PREVIEW_SOURCE_KEY,
				LEGACY_TRANSCRIBE_KEY,
				SAFE_AREAS_KEY,
				LAYOUT_KEY,
				THEME_KEY,
				WORKSPACES_KEY,
				KEYBINDINGS_KEY,
				EDITING_KEY,
				TIME_DISPLAY_KEY,
				REOPEN_KEY
			]) {
				localStorage.removeItem(key);
			}
			// The review switch is a preference and goes back on; where the server listens is not.
			harnessMcpInfo({ agent_review: true });
		} catch {
			// Storage blocked: there was nothing remembered to clear.
		}
		return getSettings();
	}
	return invoke<SettingsView>('reset_settings');
}

/** Show the settings file in the OS file manager. A no-op in the browser harness. */
export async function revealSettingsFile(): Promise<void> {
	if (!inTauri()) return;
	await invoke('reveal_settings_file');
}

// ---- diagnostics (logs) ----------------------------------------------------

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
