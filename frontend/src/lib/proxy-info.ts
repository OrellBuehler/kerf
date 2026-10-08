// What the page says about preview proxies: the badge on a bin row, the facts in its context
// menu, the settings' wording and the status bar's "which file is this frame from". Pure.

import { fmtDuration } from './media-info';
import type { Asset, PreviewSource, ProxySize, ProxyStatus, Timeline } from './types';

export type BadgeTone = 'neutral' | 'kerf' | 'agent' | 'success' | 'warning' | 'danger';

export interface ProxyBadge {
	text: string;
	tone: BadgeTone;
	/** The tooltip: the whole sentence, with the reason when there is one. */
	title: string;
}

/** `1280 px`, `3072 px`. */
function px(width: number | null | undefined): string {
	return width ? `${width} px` : '';
}

/** `38.2 MB`, `1.4 GB`. */
export function fmtBytes(bytes: number): string {
	if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
	if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function pct(fraction: number | null | undefined): number {
	return Math.round(Math.min(1, Math.max(0, fraction ?? 0)) * 100);
}

/** The badge a bin row wears for its proxy, or `null` for nothing to say (no status yet; a
 *  still or an audio file, which have none). */
export function proxyBadge(status: ProxyStatus | null | undefined): ProxyBadge | null {
	if (!status || status.state === 'not_needed') return null;
	const reason = status.reason ? ` — ${status.reason}` : '';
	switch (status.state) {
		case 'building': {
			const eta = status.eta_secs != null && status.eta_secs > 1 ? ` · about ${fmtDuration(status.eta_secs)} left` : '';
			return {
				text: `proxy ${pct(status.fraction)}%`,
				tone: 'agent',
				title: `Building the preview proxy, ${pct(status.fraction)}% done${eta}. Previews decode the original until it is ready.`
			};
		}
		case 'queued':
			return {
				text: 'proxy queued',
				tone: 'neutral',
				title: 'The preview proxy is waiting its turn — it is built before any analysis that is waiting.'
			};
		case 'ready':
			return {
				text: 'proxy',
				tone: 'success',
				title: `Preview proxy ready${status.width ? ` · ${px(status.width)}` : ''}${status.bytes ? ` · ${fmtBytes(status.bytes)}` : ''}. Previews decode it; export reads the original.`
			};
		case 'failed':
			return {
				text: 'proxy failed',
				tone: 'danger',
				title: `The preview proxy could not be built${reason}. Previews decode the original. Right-click to rebuild it.`
			};
		case 'missing':
			return {
				text: 'no proxy',
				tone: 'neutral',
				title: 'No preview proxy yet. Previews decode the original.'
			};
		case 'off':
			return {
				text: 'proxy off',
				tone: 'neutral',
				title: `No preview proxy is used${reason}.`
			};
	}
}

export interface ProxyFact {
	label: string;
	value: string;
	title?: string;
}

/** The rows the bin's context menu lists for the proxy. */
export function proxyFacts(status: ProxyStatus | null | undefined): ProxyFact[] {
	if (!status) return [];
	switch (status.state) {
		case 'not_needed':
			return [{ label: 'Proxy', value: 'not needed', title: status.reason ?? undefined }];
		case 'building':
			return [
				{
					label: 'Proxy',
					value: `building ${pct(status.fraction)}%${status.eta_secs != null && status.eta_secs > 1 ? ` · ~${fmtDuration(status.eta_secs)} left` : ''}`
				}
			];
		case 'queued':
			return [{ label: 'Proxy', value: 'queued', title: 'Built before any analysis that is waiting' }];
		case 'ready':
			return [
				{
					label: 'Proxy',
					value: ['ready', px(status.width), status.bytes ? fmtBytes(status.bytes) : ''].filter(Boolean).join(' · ')
				}
			];
		case 'failed': {
			const rows: ProxyFact[] = [{ label: 'Proxy', value: 'failed', title: status.reason ?? undefined }];
			if (status.reason) rows.push({ label: 'Reason', value: shorten(status.reason), title: status.reason });
			return rows;
		}
		case 'off':
			return [{ label: 'Proxy', value: 'off', title: status.reason ?? undefined }];
		case 'missing':
			return [{ label: 'Proxy', value: 'not built yet' }];
	}
}

function shorten(text: string, max = 56): string {
	const line = text.replace(/\s+/g, ' ').trim();
	return line.length <= max ? line : `${line.slice(0, max - 1)}…`;
}

/** What the context menu offers for the proxy: whether a rebuild or a delete makes sense. */
export function proxyActions(status: ProxyStatus | null | undefined): {
	rebuild: { label: string; disabled: boolean; reason?: string };
	remove: { label: string; disabled: boolean; reason?: string };
} {
	const notNeeded = status?.state === 'not_needed';
	const busy = status?.state === 'building' || status?.state === 'queued';
	const hasFile = status?.state === 'ready' || (status?.bytes ?? 0) > 0;
	return {
		rebuild: {
			label: busy ? 'Restart proxy' : status?.state === 'ready' ? 'Rebuild proxy' : status?.state === 'failed' ? 'Retry proxy' : 'Build proxy',
			disabled: notNeeded,
			reason: notNeeded ? 'this file needs no proxy' : undefined
		},
		remove: {
			label: busy ? 'Cancel proxy' : 'Delete proxy',
			disabled: notNeeded || (!busy && !hasFile && status?.state !== 'failed'),
			reason: notNeeded ? 'this file needs no proxy' : !busy && !hasFile ? 'there is no proxy to delete' : undefined
		}
	};
}

// ---- settings ----------------------------------------------------------------

export const PROXY_SIZES: readonly { size: ProxySize; label: string; hint: string }[] = [
	{ size: 720, label: '720 px', hint: 'Smallest and fastest to build; the preview is soft on a large monitor.' },
	{ size: 1080, label: '1080 px', hint: 'Sharper, a little slower to build and to scrub.' },
	{ size: 1280, label: '1280 px', hint: 'The default: sharp enough to judge framing, light enough to scrub 4K and 5K footage.' },
	{ size: 0, label: 'Off', hint: 'No proxies are built; previews decode the original.' }
];

export const PREVIEW_SOURCES: readonly { source: PreviewSource; label: string; hint: string }[] = [
	{
		source: 'auto',
		label: 'Auto',
		hint: 'The proxy once it is ready, the original until then.'
	},
	{
		source: 'original',
		label: 'Always original',
		hint: 'Full resolution, always. Proxies are not built, and scrubbing long-GOP 4K and 5K footage is slow.'
	},
	{
		source: 'proxy_only',
		label: 'Proxy only',
		hint: 'Never decode the original: a clip whose proxy is still building waits for it.'
	}
];

// ---- the status bar: which file is this frame from -----------------------------------------

/** The video clips under the playhead, as the assets they play. */
function assetsAt(timeline: Pick<Timeline, 'tracks'>, assets: readonly Asset[], t: number): Asset[] {
	const seen = new Set<string>();
	const out: Asset[] = [];
	for (const track of timeline.tracks) {
		if (track.kind !== 'video') continue;
		for (const c of track.clips) {
			const speed = Math.max(Math.abs(c.speed ?? 1), 0.01);
			const end = c.timeline_start + Math.max(0, c.source_out - c.source_in) / speed;
			if (c.enabled === false || t < c.timeline_start || t >= end || seen.has(c.asset_id)) continue;
			seen.add(c.asset_id);
			const asset = assets.find((a) => a.id === c.asset_id);
			if (asset) out.push(asset);
		}
	}
	return out;
}

export interface PreviewSourceNote {
	/** `preview: proxy 1280 px`, `preview: original · proxy 42%`. */
	text: string;
	tone: 'proxy' | 'original' | 'waiting';
	title: string;
}

/**
 * Which file the frame under the playhead is decoded from, for the status bar: the proxy of
 * the clips there, or their original and why. `null` when no clip with a picture is under the
 * playhead (a gap, an audio clip, a still). Judged from the statuses the page already holds —
 * nothing is asked of the backend per frame.
 */
export function previewSourceNote(
	timeline: Pick<Timeline, 'tracks'>,
	assets: readonly Asset[],
	statuses: Readonly<Record<string, ProxyStatus | undefined>>,
	source: PreviewSource,
	size: ProxySize,
	t: number
): PreviewSourceNote | null {
	const here = assetsAt(timeline, assets, t).filter(
		(a) => a.streams.some((s) => s.kind === 'video' && !s.image) && statuses[a.id]?.state !== 'not_needed'
	);
	if (here.length === 0) return null;
	const mode: PreviewSource = size === 0 ? 'original' : source;
	const states = here.map((a) => ({ asset: a, status: statuses[a.id] }));
	if (mode === 'original') {
		return {
			text: size === 0 ? 'preview: original · proxies off' : 'preview: original · full resolution',
			tone: 'original',
			title:
				size === 0
					? 'Proxies are off (Settings › Preview), so the preview decodes the original file.'
					: 'The preview source is set to always use the original (Settings › Preview).'
		};
	}
	const notReady = states.filter((s) => s.status?.state !== 'ready');
	if (notReady.length === 0) {
		const width = states[0].status?.width;
		return {
			text: `preview: proxy${width ? ` ${width} px` : ''}`,
			tone: 'proxy',
			title: 'The frame is decoded from the all-intra preview proxy. Export reads the original.'
		};
	}
	const first = notReady[0];
	const detail =
		first.status?.state === 'building'
			? `proxy ${pct(first.status.fraction)}%`
			: first.status?.state === 'queued'
				? 'proxy queued'
				: first.status?.state === 'failed'
					? 'proxy failed'
					: 'no proxy';
	const why = first.status?.reason ? ` ${first.status.reason}` : '';
	if (mode === 'proxy_only') {
		return {
			text: `preview: waiting for proxy · ${detail.replace(/^proxy /, '')}`,
			tone: 'waiting',
			title: `Preview source is Proxy only, so ${first.asset.name} is not shown until its proxy is ready (${detail}).${why}`
		};
	}
	return {
		text: `preview: original · ${detail}`,
		tone: 'original',
		title: `${first.asset.name} is decoded from the original file until its proxy is ready (${detail}).${why}`
	};
}
