// Stabilization in the Inspector: what a clip's status says, and when the panel asks for an analysis.
// The status is the backend's (`StabilizeStatus`, `crate::stabilize`); these are its words and the rules
// the panel follows, kept here so they have tests.

import type { Asset, Clip, Stabilize, StabilizeStatus, Track } from './types';

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

/** Whether the panel starts an analysis by itself: the clip is stabilized and nothing measured it yet. A failure is
 *  left for the user to retry (the button), so a broken file is not analysed again on every selection. */
export function wantsAnalysis(status: StabilizeStatus | null): boolean {
	return status?.state === 'missing';
}

/** The section's summary line. */
export function stabilizeSummary(clip: Clip, status: StabilizeStatus | null): string {
	if (!clip.stabilize) return 'Off';
	const strength = `${Math.round(clip.stabilize.strength * 100)}%`;
	return `${strength} · ${statusLabel(status).text}`;
}
