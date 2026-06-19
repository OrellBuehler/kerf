/* Mock project data ported from the Kerf editor design kit. The timeline,
   transcript, preview and waveforms now render real backend data; what remains
   here is the agent queue / activity log narrative (Kerf has no in-app agent —
   a connected LLM claims tasks over MCP) plus the browser-only fallback bin. */

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

export const PRESETS = ['Remove silences', 'Assemble rough cut', 'Find best 60s', 'Color match'];

export const PHASES: [EditorPhase, string][] = [
	['empty', 'Empty'],
	['analyzing', 'Agent working'],
	['review', 'Review cut'],
	['editing', 'Applied']
];
