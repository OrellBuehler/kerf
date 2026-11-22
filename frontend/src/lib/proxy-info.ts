// What the page says about preview proxies: the badge on a bin row, the facts in its context
// menu, the settings' wording and the status bar's "which file is this frame from". Pure.

import { fmtEta, isQueued } from './proxy-queue';
export { PROXY_WAIT_PREFIX, isProxyWaitMessage } from './proxy-wait';
import { clipDuration } from './speed-ramp';
import type { Asset, PreviewSource, ProxySize, ProxyStatus, Timeline } from './types';

export type BadgeTone = 'neutral' | 'kerf' | 'agent' | 'success' | 'warning' | 'danger';

/** What a proxy is, in one sentence — the words every surface that mentions one shares (the bin's badge, the Inspector's
 *  section, the status bar's note, the first-run card). The word is kept: Settings, the queue and the Window menu say it. */
export const PROXY_WHAT = 'A proxy is a light copy of the video that Kerf plays while you edit. Export always reads your original.';

/** Said wherever a proxy is not ready: whether to wait, and what plays meanwhile. */
export const PROXY_NO_WAIT = 'Previews play the original until it is ready, which can scrub slowly. Editing never waits for it.';

export interface ProxyBadge {
	text: string;
	tone: BadgeTone;
	/** The tooltip: the whole sentence, with the reason when there is one. */
	title: string;
	/** Under it: what it means for the person now — whether to wait, what plays, what to do. */
	detail: string;
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
			const eta = status.eta_secs != null && status.eta_secs > 1 ? ` · about ${fmtEta(status.eta_secs)} left` : '';
			return {
				text: `proxy ${pct(status.fraction)}%`,
				tone: 'kerf',
				title: `Building the preview proxy, ${pct(status.fraction)}% done${eta}.`,
				detail: `${PROXY_WHAT} ${PROXY_NO_WAIT}`
			};
		}
		case 'queued':
			return {
				text: 'proxy queued',
				tone: 'neutral',
				title: 'The preview proxy is waiting its turn — it is built before any analysis that is waiting.',
				detail: `${PROXY_WHAT} ${PROXY_NO_WAIT}`
			};
		case 'ready':
			return {
				text: 'proxy ready',
				tone: 'success',
				title: `Preview proxy ready${status.width ? ` · ${px(status.width)}` : ''}${status.bytes ? ` · ${fmtBytes(status.bytes)}` : ''}.`,
				detail: 'Previews play it; export reads the original.'
			};
		case 'failed':
			return {
				text: 'proxy failed',
				tone: 'danger',
				title: `Couldn’t build the preview proxy${reason}.`,
				detail: 'Previews play the original instead. Select the file and press Retry proxy in its Inspector.'
			};
		case 'missing':
			return {
				text: 'proxy not built',
				tone: 'neutral',
				title: 'No preview proxy yet.',
				detail: 'Previews play the original. Select the file and press Build proxy in its Inspector.'
			};
		case 'off':
			return {
				text: 'proxy off',
				tone: 'neutral',
				title: `No preview proxy is used${reason}.`,
				detail: 'Previews play the original.'
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
					value: `building ${pct(status.fraction)}%${status.eta_secs != null && status.eta_secs > 1 ? ` · about ${fmtEta(status.eta_secs)} left` : ''}`
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
			return [{ label: 'Proxy', value: 'not built' }];
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
		hint: 'Full resolution, always. Proxies are not built, and scrubbing 4K and 5K footage is slow.'
	},
	{
		source: 'proxy_only',
		label: 'Proxy only',
		hint: 'The Preview panel and playback never decode the original: a clip whose proxy is still building waits for it. Thumbnails, the source monitor, zoom and an agent’s frames can still read the original.'
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
			const end = c.timeline_start + clipDuration(c);
			if (c.enabled === false || t < c.timeline_start || t >= end || seen.has(c.asset_id)) continue;
			seen.add(c.asset_id);
			const asset = assets.find((a) => a.id === c.asset_id);
			if (asset) out.push(asset);
		}
	}
	return out;
}

/**
 * The widest picture the preview reads at `t`, px: for each video clip under the playhead, its proxy's
 * width when that is the file the preview decodes (`ready`, and the source is not "always original" or
 * proxies off), else the original's; the widest of them. `null` when it is not known (no clip, a
 * source with no stated width) — a bound only ever applies to what is known. What is asked of the
 * backend is capped at this: a 1440 px frame composited from a 1280 px proxy is the same picture,
 * upscaled, at several times the decode cost on every scrub frame.
 */
export function previewSourceWidth(
	timeline: Pick<Timeline, 'tracks'>,
	assets: readonly Asset[],
	statuses: Readonly<Record<string, ProxyStatus | undefined>>,
	source: PreviewSource,
	size: ProxySize,
	t: number
): number | null {
	const mode: PreviewSource = size === 0 ? 'original' : source;
	let widest: number | null = null;
	for (const asset of assetsAt(timeline, assets, t)) {
		const video = asset.streams.find((s) => s.kind === 'video');
		if (!video) continue;
		const status = statuses[asset.id];
		const read = mode !== 'original' && status?.state === 'ready' && status.width ? status.width : video.width;
		if (!read || !(read > 0)) return null; // one source of unknown width: no bound is safe
		widest = Math.max(widest ?? 0, read);
	}
	return widest;
}

export interface PreviewSourceNote {
	/** `preview from proxy (1280 px)`, `preview from original (proxy 42%)`. */
	text: string;
	tone: 'proxy' | 'original' | 'waiting' | 'failed';
	title: string;
	/** The proxy it is about is in the queue (building or waiting): the note then opens the proxy queue. */
	queue: boolean;
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
			text: size === 0 ? 'preview from original (proxies off)' : 'preview from original (full resolution)',
			tone: 'original',
			queue: false,
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
			text: `preview from proxy${width ? ` (${width} px)` : ''}`,
			tone: 'proxy',
			queue: false,
			title: 'The frame comes from the proxy, the light preview copy. Export reads the original.'
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
					: 'proxy not built';
	const why = first.status?.reason ? ` ${first.status.reason}` : '';
	const queue = isQueued(first.status?.state);
	if (mode === 'proxy_only') {
		return {
			text: `preview waiting for proxy (${detail.replace(/^proxy /, '')})`,
			tone: 'waiting',
			queue,
			title: `Preview source is Proxy only, so ${first.asset.name} is not shown until its proxy is ready (${detail}).${why}`
		};
	}
	const failed = first.status?.state === 'failed';
	return {
		text: `preview from original (${detail})`,
		tone: failed ? 'failed' : 'original',
		queue,
		title: failed
			? `${first.asset.name}: the proxy could not be built, so its preview comes from the original file.${why} Select it in the library and press Retry proxy in the Inspector.`
			: `${first.asset.name} is decoded from the original file until its proxy is ready (${detail}). ${PROXY_NO_WAIT}${why}`
	};
}
