// A bin row's state, as pills. The row has room for little, and what it says is two *states* — the proxy's and the analysis's —
// next to *attributes* (360, voiceover, how many clips use it). A state is a round pill with an icon for its look; an attribute is
// a square label with none, so the two are told apart by shape and mark, not only by colour. At a narrow row the two state pills
// are one: the one that needs the person most, and a count of the rest. Pure.

import type { AnalysisMeter } from './analysis-steps';
import type { ProxyBadge } from './proxy-info';
import { isQueued } from './proxy-queue';
import type { ProxyStatus } from './types';

/** How a state reads, in order of how much it needs the person: a failure first, a finished one last. */
export type PillLook = 'failed' | 'running' | 'queued' | 'idle' | 'off' | 'done';

export interface StatePill {
	id: 'proxy' | 'analysis';
	text: string;
	look: PillLook;
	/** The tooltip's first line, and what it means for the person under it. */
	title: string;
	detail: string;
	/** The proxy is in the queue: its pill opens the queue. */
	queue?: boolean;
}

const ORDER: Record<PillLook, number> = { failed: 0, running: 1, queued: 2, idle: 3, off: 4, done: 5 };

/** The proxy's pill from its badge and status; `null` for nothing to say (a still, an audio file). */
export function proxyPill(badge: ProxyBadge | null, status: ProxyStatus | null | undefined): StatePill | null {
	if (!badge || !status) return null;
	const look: PillLook =
		status.state === 'building' ? 'running' : status.state === 'queued' ? 'queued' : status.state === 'failed' ? 'failed' : status.state === 'ready' ? 'done' : 'off';
	return { id: 'proxy', text: badge.text, look, title: badge.title, detail: badge.detail, queue: isQueued(status.state) };
}

/** The analysis's pill from its meter; `null` when the file has nothing to analyze. */
export function analysisStatePill(meter: AnalysisMeter | null): StatePill | null {
	if (!meter) return null;
	const look: PillLook =
		meter.tone === 'running' ? 'running' : meter.tone === 'queued' ? 'queued' : meter.tone === 'failed' ? 'failed' : meter.tone === 'done' ? 'done' : meter.total === 0 ? 'off' : 'idle';
	const [title, ...rest] = meter.title.split('\n');
	return { id: 'analysis', text: meter.text, look, title, detail: [...rest, meter.detail].filter(Boolean).join('\n') };
}

export interface MergedStatus {
	/** The one pill a narrow row draws. */
	head: StatePill;
	/** How many other states there are that are not at rest, as `+1`. */
	more: number;
	/** What the others say, for the tooltip. */
	others: StatePill[];
}

/** The row's state as one pill: the neediest of the two. Both at rest is `ready` (a check; the tooltip lists both); one
 *  that needs something is that one, with a count of the other if it is not at rest either. */
export function mergedStatus(pills: readonly (StatePill | null)[]): MergedStatus | null {
	const list = pills.filter((p): p is StatePill => p !== null);
	if (list.length === 0) return null;
	if (list.length === 2 && list.every((p) => p.look === 'done')) {
		return {
			head: {
				id: list[0].id,
				text: 'ready',
				look: 'done',
				title: list.map((p) => p.title).join(' · '),
				detail: list.map((p) => p.detail).filter(Boolean).join('\n')
			},
			more: 0,
			others: []
		};
	}
	const sorted = [...list].sort((a, b) => ORDER[a.look] - ORDER[b.look]);
	const [head, ...rest] = sorted;
	const others = rest.filter((p) => p.look !== 'done');
	return { head, more: others.length, others };
}
