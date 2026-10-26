// Stabilization in the Inspector: what a clip's status says, and when the panel asks for an analysis.
// The status is the backend's (`StabilizeStatus`, `crate::stabilize`); these are its words and the rules
// the panel follows, kept here so they have tests.

import type { Asset, Clip, ClipStabilizeStatus, Stabilize, StabilizeStatus, Track } from './types';

/** The setting a clip gets when stabilization is switched on: the backend's default. */
export const DEFAULT_STABILIZE: Stabilize = { strength: 0.5 };

/** Whether the Inspector offers stabilization for `clip` on `track`: moving footage on a video track. The backend
 *  refuses the rest with its reason (`why_not_stabilized`); the panel does not show a switch that would only fail. */
export function canStabilize(clip: Clip, track: Track | undefined, asset: Asset | undefined): boolean {
	if (track?.kind !== 'video' || clip.generator || clip.reframe || !asset) return false;
	const video = asset.streams.find((s) => s.kind === 'video');
	if (!video || video.image) return false;
	return !asset.streams.some((s) => s.projection && s.projection !== 'flat');
}

/** The status in a few words, and how it is shown. */
export function statusLabel(status: StabilizeStatus | null): { text: string; tone: 'neutral' | 'success' | 'warning' | 'danger' } {
	switch (status?.state) {
		case 'ready':
			return { text: 'Steady', tone: 'success' };
		case 'basic':
			return { text: 'Basic (deshake)', tone: 'neutral' };
		case 'analyzing':
			return { text: `Analysing ${Math.round(status.fraction * 100)}%`, tone: 'warning' };
		case 'queued':
			return { text: 'Queued', tone: 'warning' };
		case 'missing':
			return { text: 'Needs analysis', tone: 'warning' };
		case 'failed':
			return { text: 'Analysis failed', tone: 'danger' };
		case 'unsupported':
			return { text: 'Not available', tone: 'neutral' };
		case 'off':
		case undefined:
			return { text: 'Off', tone: 'neutral' };
	}
}

/** What the clip's picture does now, for the line under the status. */
export function statusDetail(status: StabilizeStatus | null): string {
	switch (status?.state) {
		case 'ready':
			return 'The preview and the export move every frame against the camera’s shake.';
		case 'basic':
			return 'This FFmpeg has no vid.stab, so FFmpeg’s simpler deshake steadies the clip; it needs no analysis.';
		case 'analyzing':
		case 'queued':
			return 'The camera is being measured; the clip plays as shot until it is done.';
		case 'missing':
			return 'The clip plays as shot until its camera is measured (it was never analysed, or was trimmed past it).';
		case 'failed':
			return `The clip plays as shot: ${status.reason}`;
		case 'unsupported':
			return status.reason;
		default:
			return '';
	}
}

/** The clips whose analysis the user stopped this session: the panel does not start them again by itself. */
const stopped = new Set<string>();

/** The analysis of `clipId` was stopped: leave it to the Analyze button. */
export function noteStopped(clipId: string): void {
	stopped.add(clipId);
}

/** The user asked for `clipId`'s analysis again. */
export function forgetStopped(clipId: string): void {
	stopped.delete(clipId);
}

/** Whether the panel starts an analysis of `clipId` by itself: the clip is stabilized and nothing measured it yet,
 *  and the user did not stop its analysis. A failure is left for the user to retry (the button), so a broken file is
 *  not analysed again on every selection. */
export function wantsAnalysis(status: StabilizeStatus | null, clipId?: string): boolean {
	return status?.state === 'missing' && !(clipId !== undefined && stopped.has(clipId));
}

/** What the export dialog says about the stabilized clips that will export as shot, or `null` when none will. */
export function unsteadyNote(statuses: ClipStabilizeStatus[]): string | null {
	const shaky = statuses.filter((s) => !['off', 'basic', 'ready'].includes(s.state));
	if (!shaky.length) return null;
	const n = shaky.length;
	const clips = n === 1 ? '1 stabilized clip exports' : `${n} stabilized clips export`;
	const measuring = shaky.every((s) => s.state === 'queued' || s.state === 'analyzing');
	const why = measuring
		? 'its camera is still being measured'
		: n === 1
			? 'its camera has not been measured, or measuring it failed'
			: 'their cameras have not been measured, or measuring failed';
	return `${clips} as shot: ${why}. Analyze in the clip's Stabilize section, or export anyway.`;
}

/** The section's summary line. */
export function stabilizeSummary(clip: Clip, status: StabilizeStatus | null): string {
	if (!clip.stabilize) return 'Off';
	const strength = `${Math.round(clip.stabilize.strength * 100)}%`;
	return `${strength} · ${statusLabel(status).text}`;
}
