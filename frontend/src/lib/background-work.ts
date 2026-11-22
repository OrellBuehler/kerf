// What is happening in the background after an import, in one voice. A proxy (the light copy previews play) is built
// and the file is analyzed (silences, loudness, beat, speech, scene changes), and a person who has just imported a clip
// wants three answers: what is it doing, do I have to wait, what do I do when it fails. The bin row, the Inspector's
// status strip and the empty Preview's card all draw from here, over the statuses the page already holds (`mediaStatus`,
// the `ui` analysis run), so they cannot tell different stories. The words for each state are `proxy-info.ts`' and
// `analysis-steps.ts`'; this puts them in lines. Pure.

import { tidyReason } from './failure-copy';
import { fmtEta } from './proxy-queue';
import type { AnalysisStatus, ProxyStatus } from './types';

export type WorkTone = 'running' | 'queued' | 'failed';

export interface WorkItem {
	id: 'proxy' | 'analysis' | 'proxy-failed' | 'analysis-failed';
	/** The word in front of it: `Proxy`, `Analysis`. */
	label: string;
	tone: WorkTone;
	/** What is happening, one line. */
	text: string;
	/** Under it, for a failure: why, what plays meanwhile, what to do. */
	detail?: string;
	/** The way out, where the surface can offer one. */
	fix?: 'retry-proxy' | 'retry-analysis';
}

const pct = (fraction: number | null | undefined) => Math.round(Math.min(1, Math.max(0, fraction ?? 0)) * 100);
const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** The reason as a clause that a full stop can follow: tidied, and without the one it may already end in. */
const clause = (reason: string) => tidyReason(reason).replace(/[.\s]+$/, '');

/** `building 42% · about 7 s left`. */
function building(status: ProxyStatus): string {
	const eta = status.eta_secs != null && status.eta_secs > 1 ? ` · about ${fmtEta(status.eta_secs)} left` : '';
	return `building ${pct(status.fraction)}%${eta}`;
}

export interface AssetWorkInput {
	proxy: ProxyStatus | null | undefined;
	/** A pass is running on this asset: its progress line, and how many files wait behind it. */
	run: { label: string; queued: number } | null;
	/** The asset waits its turn behind another's analysis. */
	waiting: boolean;
	/** The steps that failed, named, with the backend's reasons. */
	failed: readonly { label: string; reason: string | null | undefined }[];
}

/** The background work of one asset that is worth a line: a proxy being built, waiting or failed; an analysis running,
 *  waiting or failed. Nothing when both are at rest (ready / done / off), so the strip is not there to be ignored. */
export function assetWork(i: AssetWorkInput): WorkItem[] {
	const out: WorkItem[] = [];
	const p = i.proxy;
	if (p?.state === 'building') out.push({ id: 'proxy', label: 'Proxy', tone: 'running', text: building(p) });
	else if (p?.state === 'queued') out.push({ id: 'proxy', label: 'Proxy', tone: 'queued', text: 'queued — built before any analysis that is waiting' });
	else if (p?.state === 'failed')
		out.push({
			id: 'proxy-failed',
			label: 'Proxy',
			tone: 'failed',
			text: 'failed',
			detail: `${p.reason ? `${clause(p.reason)}. ` : ''}Previews play the original instead.`,
			fix: 'retry-proxy'
		});
	if (i.run) {
		out.push({
			id: 'analysis',
			label: 'Analysis',
			tone: 'running',
			text: i.run.queued > 0 ? `${i.run.label} · ${i.run.queued} more queued` : i.run.label
		});
	} else if (i.waiting) {
		out.push({ id: 'analysis', label: 'Analysis', tone: 'queued', text: 'waiting its turn — one file is analyzed at a time' });
	} else if (i.failed.length > 0) {
		const names = i.failed.map((f) => f.label).join(', ');
		const reason = i.failed[0].reason;
		out.push({
			id: 'analysis-failed',
			label: 'Analysis',
			tone: 'failed',
			text: `${names} failed`,
			detail: `${reason ? `${clause(reason)}. ` : ''}The other steps still ran.`,
			fix: 'retry-analysis'
		});
	}
	return out;
}

/** Whether the work in `items` is still going (not just a failure to read). */
export const isBusy = (items: readonly WorkItem[]) => items.some((w) => w.tone !== 'failed');

/** The sentence under the lines, while something is going: whether to wait. Empty when nothing is. */
export function workNote(items: readonly WorkItem[]): string {
	if (!isBusy(items)) return '';
	return items.some((w) => w.id === 'proxy')
		? 'You can start cutting now: editing never waits for this. Previews play the original until the proxy is ready.'
		: 'You can start cutting now: editing never waits for the analysis.';
}

export interface LibraryWorkInput {
	assets: readonly { id: string; name: string }[];
	proxies: Readonly<Record<string, ProxyStatus | undefined>>;
	analyses: Readonly<Record<string, AnalysisStatus | undefined>>;
	/** The analysis run now: which asset, its progress line, how many files wait behind it. Null when none runs. */
	run: { id: string | null; label: string | null; queued: number } | null;
}

/** What the two are for, in one line — said where the first imports wait (the empty Preview's card), before the Inspector's
 *  sections that say it at length. */
export const WORK_GLOSS = 'A proxy is a light copy that keeps previews smooth. Analysis finds the silences, the beat and the speech to cut by.';

/** The same lines for the whole library — what the empty Preview says after an import, before anything is on the
 *  timeline: the proxy being built (the first by name, the rest counted), the analysis running (with its file named),
 *  and whatever failed, with where to go to try again. */
export function libraryWork(i: LibraryWorkInput): WorkItem[] {
	const out: WorkItem[] = [];
	const known = i.assets.map((a) => ({ a, proxy: i.proxies[a.id], status: i.analyses[a.id] }));
	const building = known.filter((k) => k.proxy?.state === 'building');
	const waiting = known.filter((k) => k.proxy?.state === 'queued');
	if (building.length > 0) {
		const first = building[0];
		const more = building.length - 1 + waiting.length;
		out.push({
			id: 'proxy',
			label: 'Proxy',
			tone: 'running',
			text: `${first.a.name} · building ${pct(first.proxy?.fraction)}%${more > 0 ? ` · ${more} more waiting` : ''}`
		});
	} else if (waiting.length > 0) {
		out.push({ id: 'proxy', label: 'Proxy', tone: 'queued', text: `${plural(waiting.length, 'file waits', 'files wait')} its turn` });
	}
	if (i.run) {
		const name = i.assets.find((a) => a.id === i.run!.id)?.name;
		const head = [name, i.run.label ?? 'analyzing'].filter(Boolean).join(' · ');
		out.push({ id: 'analysis', label: 'Analysis', tone: 'running', text: i.run.queued > 0 ? `${head} · ${i.run.queued} more queued` : head });
	}
	const proxyFailed = known.filter((k) => k.proxy?.state === 'failed');
	if (proxyFailed.length > 0)
		out.push({
			id: 'proxy-failed',
			label: 'Proxy',
			tone: 'failed',
			text: `${proxyFailed.length === 1 ? proxyFailed[0].a.name : plural(proxyFailed.length, 'file', 'files')} failed`,
			detail: 'Previews play the original instead. Select it in the library to try again.'
		});
	const analysisFailed = known.filter((k) => i.run?.id !== k.a.id && k.status?.kinds.some((s) => s.state === 'failed'));
	if (analysisFailed.length > 0)
		out.push({
			id: 'analysis-failed',
			label: 'Analysis',
			tone: 'failed',
			text: `${analysisFailed.length === 1 ? analysisFailed[0].a.name : plural(analysisFailed.length, 'file', 'files')} failed`,
			detail: 'The other steps still ran. Select it in the library to try again.'
		});
	return out;
}
