/* Publishing-target readiness, for the **browser dev harness only**.
 *
 * `kerf_core::platform` is authoritative: the desktop app always asks the
 * backend, and the UI renders whatever it gets back. This mirror exists so the
 * readiness panel can be built and driven under `bun run dev`, the same way the
 * harness mirrors the rest of the project's ops — not as a second engine.
 *
 * Keep the numbers in step with `crates/kerf-core/src/platform.rs`. */

import type { DeliveryCheck, DeliveryIssue, PlatformTarget } from './types';

const VERTICAL: [number, number][] = [[9, 16]];

export const TARGETS: PlatformTarget[] = [
	{
		id: 'reels',
		label: 'Instagram Reels',
		width: 1080,
		height: 1920,
		accepts: VERTICAL,
		max_secs: 20 * 60,
		reach_max_secs: 3 * 60,
		min_secs: 3,
		notes: 'Uploads accept up to 20 min, but past 3 min a Reel is only shown to existing followers.'
	},
	{
		id: 'shorts',
		label: 'YouTube Shorts',
		width: 1080,
		height: 1920,
		accepts: [
			[9, 16],
			[4, 5],
			[1, 1]
		],
		max_secs: 3 * 60,
		reach_max_secs: null,
		min_secs: null,
		notes: 'Hard 3 min cap since Oct 2024; anything longer is published as a regular video instead.'
	},
	{
		id: 'tiktok',
		label: 'TikTok',
		width: 1080,
		height: 1920,
		accepts: VERTICAL,
		max_secs: 60 * 60,
		reach_max_secs: 10 * 60,
		min_secs: 3,
		notes: 'Uploads accept up to 60 min. Under 3 min the file must stay below 500 MB, 3-10 min below 2 GB.'
	},
	{
		id: 'ig_feed',
		label: 'Instagram feed',
		width: 1080,
		height: 1350,
		accepts: [
			[4, 5],
			[1, 1]
		],
		max_secs: 20 * 60,
		reach_max_secs: 3 * 60,
		min_secs: 3,
		notes: '4:5 takes the most vertical space in the feed. Feed video is distributed as a Reel.'
	},
	{
		id: 'youtube',
		label: 'YouTube',
		width: 1920,
		height: 1080,
		accepts: [[16, 9]],
		max_secs: null,
		reach_max_secs: null,
		min_secs: null,
		notes: 'No practical length limit; 16:9 fills the player without bars.'
	}
];

export interface CutSummary {
	duration: number;
	width: number;
	height: number;
	has_audio: boolean;
	has_text: boolean;
}

/** `m:ss`, how a length is spoken about. */
export function fmtDur(secs: number): string {
	const s = Math.max(0, Math.round(secs));
	return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

function ratioLabel(w: number, h: number): string {
	const gcd = (a: number, b: number): number => (b ? gcd(b, a % b) : a);
	const d = gcd(w, h) || 1;
	return `${w / d}:${h / d}`;
}

export function check(target: PlatformTarget, cut: CutSummary): DeliveryCheck {
	const issues: DeliveryIssue[] = [];

	if (cut.duration <= 0) {
		issues.push({ severity: 'error', kind: 'empty', message: 'The timeline is empty — there is nothing to publish.' });
	}
	if (target.min_secs != null && cut.duration > 0 && cut.duration < target.min_secs) {
		issues.push({
			severity: 'error',
			kind: 'length',
			message: `${fmtDur(cut.duration)} is shorter than ${target.label}'s ${target.min_secs}s minimum.`
		});
	}
	if (target.max_secs != null && cut.duration > target.max_secs) {
		issues.push({
			severity: 'error',
			kind: 'length',
			message: `${fmtDur(cut.duration)} is over ${target.label}'s ${fmtDur(target.max_secs)} limit — trim ${fmtDur(cut.duration - target.max_secs)} to fit.`
		});
	}
	const withinHard = target.max_secs == null || cut.duration <= target.max_secs;
	if (target.reach_max_secs != null && cut.duration > target.reach_max_secs && withinHard) {
		issues.push({
			severity: 'warning',
			kind: 'length',
			message: `Over ${fmtDur(target.reach_max_secs)}, ${target.label} stops showing this to people who don't already follow you. Cutting ${fmtDur(cut.duration - target.reach_max_secs)} would keep it in the feed.`
		});
	}

	const have = cut.height > 0 ? cut.width / cut.height : 0;
	const want = target.width / target.height;
	const fits = target.accepts.some(([w, h]) => Math.abs(have - w / h) <= (w / h) * 0.01);
	if (have > 0 && !fits) {
		const accepted = target.accepts.map(([w, h]) => ratioLabel(w, h)).join(' or ');
		issues.push({
			severity: 'warning',
			kind: 'shape',
			message: `This cut is ${ratioLabel(cut.width, cut.height)} (${cut.width}×${cut.height}); ${target.label} shows ${accepted}, so it will be letterboxed. Set the delivery frame to ${target.width}×${target.height}.`
		});
	} else if (have > 0 && cut.height < target.height && Math.abs(have - want) <= want * 0.01) {
		issues.push({
			severity: 'warning',
			kind: 'resolution',
			message: `${cut.width}×${cut.height} is below ${target.label}'s ${target.width}×${target.height}; the platform will upscale it and it will look soft.`
		});
	}

	if (!cut.has_text && cut.has_audio) {
		issues.push({
			severity: 'tip',
			kind: 'captions',
			message: 'The feed autoplays muted. Captions or a title would carry this for the people who never turn sound on.'
		});
	}

	return {
		target: target.id,
		label: target.label,
		ok: !issues.some((i) => i.severity === 'error'),
		issues
	};
}

export function checkAll(cut: CutSummary): DeliveryCheck[] {
	return TARGETS.map((t) => check(t, cut));
}
