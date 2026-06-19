/* Mock project data ported from the Kerf editor design kit. Used for the
   timeline diff showcase, the transcript, and the agent queue/activity log —
   the parts the current backend does not model yet. */

import type { EditorPhase } from '$lib/editor-ui.svelte';

export type MockAsset = {
	id: string;
	name: string;
	dur: string;
	kind: 'video' | 'audio';
	tag: string;
};

export const MOCK_ASSETS: MockAsset[] = [
	{ id: 'a1', name: 'interview_A.mov', dur: '04:12', kind: 'video', tag: 'A-roll' },
	{ id: 'a2', name: 'broll_city.mp4', dur: '02:38', kind: 'video', tag: 'B-roll' },
	{ id: 'a3', name: 'broll_desk.mp4', dur: '01:54', kind: 'video', tag: 'B-roll' },
	{ id: 'a4', name: 'voiceover_2.wav', dur: '03:46', kind: 'audio', tag: 'VO' },
	{ id: 'a5', name: 'ambient_loop.wav', dur: '06:00', kind: 'audio', tag: 'Music' }
];

export type DiffState = 'keep' | 'cut' | 'normal';
export type ClipBlock = { id: string; label: string; w: number; state: DiffState; kind?: string };

export const V1_REVIEW: ClipBlock[] = [
	{ id: 'v1', label: 'cold open', w: 92, state: 'keep' },
	{ id: 'v2', label: 'umm…', w: 40, state: 'cut' },
	{ id: 'v3', label: 'point 1', w: 150, state: 'keep' },
	{ id: 'v4', label: 'dead air', w: 34, state: 'cut' },
	{ id: 'v5', label: 'b-roll city', w: 120, state: 'keep', kind: 'video' },
	{ id: 'v6', label: 'tangent', w: 70, state: 'cut' },
	{ id: 'v7', label: 'point 2', w: 138, state: 'keep' },
	{ id: 'v8', label: 'outro', w: 96, state: 'keep' }
];

export const V1_EDIT: ClipBlock[] = V1_REVIEW.filter((c) => c.state === 'keep').map((c) => ({
	...c,
	state: 'normal'
}));

export const A1: ClipBlock[] = [
	{ id: 'au1', label: 'VO 01', w: 92, state: 'normal' },
	{ id: 'au2', label: 'VO 02', w: 150, state: 'normal' },
	{ id: 'au3', label: 'VO 03', w: 120, state: 'normal' },
	{ id: 'au4', label: 'VO 04', w: 138, state: 'normal' },
	{ id: 'au5', label: 'VO 05', w: 96, state: 'normal' }
];

export const RULER = ['00:00', '00:05', '00:10', '00:15', '00:20', '00:25', '00:30', '00:35', '00:40'];
export const SCENE_X = [92, 282, 452, 660]; /* detected scene cuts (px in track area) */

export type TranscriptLine = { t: string; s: string; cut: boolean; sil?: boolean };

export const TRANSCRIPT: TranscriptLine[] = [
	{ t: '00:02', s: 'So the whole idea behind Kerf is—', cut: false },
	{ t: '00:05', s: "um, is that editing shouldn't feel like fighting the tool.", cut: true },
	{ t: '00:09', s: 'You bring the footage, and the agent watches it with you.', cut: false },
	{ t: '00:14', s: '[silence 1.8s]', cut: true, sil: true },
	{ t: '00:16', s: 'It finds the dead air, the filler, the false starts.', cut: false }
];

export const FX = ['Color · Neutral LUT', 'Stabilize', 'Auto-ducking', 'Denoise (voice)', 'Crossfade 12f'];

export const QUEUE_META: Record<EditorPhase, string> = {
	empty: '0 tasks',
	analyzing: '1 running',
	review: '1 ready',
	editing: '1 done · 1 queued'
};

export type TaskStatus = 'queued' | 'working' | 'ready' | 'done';

export const STATUS_MAP: Record<TaskStatus, { tone: string; icon: string; label: string }> = {
	queued: { tone: 'neutral', icon: 'clock', label: 'Queued' },
	working: { tone: 'agent', icon: 'loader', label: 'Working' },
	ready: { tone: 'success', icon: 'git-pull-request-arrow', label: 'Ready to review' },
	done: { tone: 'neutral', icon: 'check', label: 'Applied' }
};

export type LogEntry = [time: string, icon: string, text: string, who: string];

export const LOG: Record<EditorPhase, LogEntry[]> = {
	empty: [['—', 'plug-zap', 'Claude Desktop connected over MCP', 'agent']],
	analyzing: [
		['00:03', 'captions', 'Kerf transcribed 04:12 locally', 'local'],
		['00:04', 'scan-line', 'Detected 4 scenes · 14 silences (local)', 'local'],
		['00:05', 'hand', 'Claude claimed “Assemble a rough cut”', 'agent']
	],
	review: [
		['00:05', 'hand', 'Claude claimed “Assemble a rough cut”', 'agent'],
		['00:06', 'file-search', 'Read transcript, silences, scene cuts via MCP', 'agent'],
		['00:08', 'git-pull-request-arrow', 'Staged proposed cut · −1:48 · 23 cuts', 'agent']
	],
	editing: [
		['00:08', 'git-pull-request-arrow', 'Staged proposed cut · −1:48', 'agent'],
		['00:31', 'check', 'You applied the cut', 'you'],
		['00:31', 'history', 'Sources untouched · revert available', 'local']
	]
};

export const PRESETS = ['Remove silences', 'Assemble rough cut', 'Find best 60s', 'Color match'];

export const PHASES: [EditorPhase, string][] = [
	['empty', 'Empty'],
	['analyzing', 'Agent working'],
	['review', 'Review cut'],
	['editing', 'Applied']
];
