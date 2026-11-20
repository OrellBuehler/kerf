// What the proxy queue says: the panel's rows and header, and the short lines the Preview's waiting chip and the
// status bar add to it. Pure, so the phrasing, the arithmetic and the rules for which row may be moved are held by
// `proxy-queue.test.ts`; the state that reads the queue is `proxy-queue.svelte.ts`.
//
// A job's `eta_secs` is the seconds from *the moment the queue was read* until it is done, counting the jobs before it.
// So a clock time is anchored at the moment of the read (`readAt`) and does not move while the answer ages, and a job with
// no `eta_secs` has no estimate yet: nothing here makes one up. The one number derived here, a build's speed in "N× real
// time", is the rate the engine's own estimates use (`rate` is source pixel-seconds built per wall second), divided by the
// job's picture size: it is said only for a build that is running on the pipeline the rate was measured on.

import { tidyReason } from './failure-copy';
import { fmtDuration } from './media-info';
import type { Pipeline, ProxyQueue, ProxyStatus, QueuedProxy } from './types';

/** How many rows the panel draws at first, and how many more each "Show more" adds: a queue of hundreds stays cheap. */
export const ROW_CAP = 100;

/** How often a build's progress is read again while it runs: the engine sends a progress event for every percent. */
export const PROGRESS_EVERY_MS = 1000;

/** The sentence a link to the queue ends its tooltip with. */
export const OPEN_QUEUE_HINT = 'Click to open the proxy queue.';

// ---- naming ----------------------------------------------------------------------------------------------------------

export const pipelineLabel = (p: Pipeline): string => (p === 'gpu' ? 'Fast (GPU)' : 'Software');

export const pipelineHint = (p: Pipeline): string =>
	p === 'gpu'
		? 'The proxies are encoded on the graphics hardware. A file it cannot take is built the Software way, and says so.'
		: 'The proxies are encoded on the processor (x264).';

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** Whether a proxy in this state is in the queue, so a badge for it can open it. */
export function isQueued(state: ProxyStatus['state'] | undefined): boolean {
	return state === 'building' || state === 'queued';
}

// ---- time ------------------------------------------------------------------------------------------------------------

const two = (n: number) => String(n).padStart(2, '0');

/** A time to wait, as rounded as it is sure of: `40 s`, `42 min`, `1 h 05 min`, `14 h`, `2 d 3 h`. */
export function fmtEta(secs: number): string {
	const total = Math.max(0, secs);
	const whole = Math.round(total);
	if (whole < 60) return `${Math.max(1, whole)} s`;
	let minutes = Math.round(whole / 60);
	if (minutes < 60) return `${minutes} min`;
	// Past an hour the minutes are to the nearest five: "1 h 05 min", not a precision the estimate does not have.
	minutes = Math.round(minutes / 5) * 5;
	if (minutes < 600) {
		const left = minutes % 60;
		return left === 0 ? `${Math.floor(minutes / 60)} h` : `${Math.floor(minutes / 60)} h ${two(left)} min`;
	}
	if (minutes < 2880) return `${Math.round(minutes / 60)} h`;
	let days = Math.floor(minutes / 1440);
	let hours = Math.round((minutes % 1440) / 60);
	if (hours === 24) {
		days += 1;
		hours = 0;
	}
	return hours === 0 ? `${days} d` : `${days} d ${hours} h`;
}

const formats = new Map<string, Intl.DateTimeFormat>();
/** A formatter per locale and style, made once: a panel of a hundred rows asks for a clock time in each. */
function format(locale: string | undefined, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
	const key = `${locale ?? ''}|${JSON.stringify(options)}`;
	let f = formats.get(key);
	if (!f) formats.set(key, (f = new Intl.DateTimeFormat(locale, options)));
	return f;
}

/** The time it will be, to the minute (`15:20`, in the user's own way of writing a clock), with the day when it is not
 *  today: `tomorrow 02:10`, `Sat 02:10`, and the date past a week. */
export function clockTime(now: Date, secs: number, locale?: string): string {
	// Half a minute on, so the minute shown is the nearest one and not the one that has begun.
	const at = new Date(now.getTime() + Math.max(0, secs) * 1000 + 30_000);
	const time = format(locale, { timeStyle: 'short' }).format(at);
	const day = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
	const days = Math.round((day(at) - day(now)) / 86_400_000);
	if (days <= 0) return time;
	if (days === 1) return `tomorrow ${time}`;
	if (days < 7) return `${format(locale, { weekday: 'short' }).format(at)} ${time}`;
	return `${format(locale, { day: 'numeric', month: 'short' }).format(at)} ${time}`;
}

/** The speed of a build in source seconds per wall second, from the rate the estimates are made with: `null` unless it is
 *  honest — a build under way, a measured rate, the pipeline that rate belongs to, and a picture size to weigh it by. */
export function realTimeSpeed(
	job: Pick<QueuedProxy, 'state' | 'width' | 'height' | 'pipeline'>,
	queue: Pick<ProxyQueue, 'rate' | 'pipeline'>
): number | null {
	if (job.state !== 'building') return null;
	const rate = queue.rate;
	if (typeof rate !== 'number' || !Number.isFinite(rate) || rate <= 0) return null;
	if (job.pipeline !== queue.pipeline) return null;
	const pixels = job.width * job.height;
	if (!Number.isFinite(pixels) || pixels <= 0) return null;
	const speed = rate / pixels;
	return Number.isFinite(speed) && speed > 0 ? speed : null;
}

/** `12× real time`, `1.4× real time`, `<0.1× real time`. */
export function fmtRealTime(speed: number): string {
	if (speed < 0.1) return '<0.1× real time';
	return `${speed < 10 ? Number(speed.toFixed(1)) : Math.round(speed)}× real time`;
}

// ---- the header ------------------------------------------------------------------------------------------------------

export type SummaryTone = 'loading' | 'empty' | 'building' | 'paused' | 'waiting';

export interface QueueSummary {
	tone: SummaryTone;
	/** `214 proxies`. */
	count: string;
	/** How long it will take and when it will be done: `about 42 min, done ~15:20`, or `estimating…`; empty when it
	 *  cannot be said at all (paused, or waiting for the machine). */
	time: string;
	/** Why nothing moves, when nothing does. */
	note: string;
}

/** What the queue's header says. `waiting_on` completes "waiting on …" and the queue's own time counts from when the
 *  machine is free, so it is then said as what comes after the wait and has no clock time. */
export function summarize(queue: ProxyQueue | null, readAt: Date, locale?: string): QueueSummary {
	if (!queue) return { tone: 'loading', count: 'Reading the queue…', time: '', note: '' };
	if (queue.jobs.length === 0) {
		// A pause is not lifted by there being nothing to build: what is queued next waits for it.
		return queue.paused
			? { tone: 'paused', count: 'No proxies queued', time: '', note: 'Paused. Proxies queued from now on wait until you resume.' }
			: { tone: 'empty', count: 'No proxies queued', time: '', note: '' };
	}
	const count = plural(queue.jobs.length, 'proxy', 'proxies');
	const eta = queue.eta_secs;
	const known = typeof eta === 'number' && Number.isFinite(eta);
	if (queue.paused) {
		return {
			tone: 'paused',
			count,
			time: '',
			note: queue.jobs.some((j) => j.state === 'building')
				? 'Paused. The build under way finishes; nothing new starts until you resume.'
				: 'Paused. Nothing starts until you resume.'
		};
	}
	if (queue.waiting_on) {
		return {
			tone: 'waiting',
			count,
			time: known ? `then about ${fmtEta(eta)}` : '',
			note: `Waiting on ${queue.waiting_on}.`
		};
	}
	return {
		tone: 'building',
		count,
		time: known ? `about ${fmtEta(eta)}, done ~${clockTime(readAt, eta, locale)}` : 'estimating…',
		note: ''
	};
}

/** The few words the Preview's waiting chip adds, once the queue is known: `214 in the queue · about 42 min`. The time is only
 *  there when it is known (the chip does not say "estimating"), and is not said while the queue waits for the machine or
 *  is paused, when it would not be true. `null` with nothing queued or nothing read. */
export function hudLine(queue: ProxyQueue | null): string | null {
	if (!queue || queue.jobs.length === 0) return null;
	const n = `${queue.jobs.length} in the queue`;
	if (queue.paused) return `${n} · paused`;
	if (queue.waiting_on) return n;
	const eta = queue.eta_secs;
	return typeof eta === 'number' && Number.isFinite(eta) ? `${n} · about ${fmtEta(eta)}` : n;
}

/** What a tooltip says about the queue as a whole, in a sentence; empty with nothing queued or nothing read. */
export function queueSentence(queue: ProxyQueue | null, readAt: Date, locale?: string): string {
	if (!queue || queue.jobs.length === 0) return '';
	const s = summarize(queue, readAt, locale);
	const lead = `${s.count} in the queue`;
	if (s.note) return `${lead}. ${s.note}${s.time ? ` ${capital(s.time)}.` : ''}`;
	return s.time && s.time !== 'estimating…' ? `${lead}: ${s.time}.` : `${lead}.`;
}

const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

// ---- the rows --------------------------------------------------------------------------------------------------------

export interface QueueRow {
	id: string;
	name: string;
	state: QueuedProxy['state'];
	position: number;
	/** 0..1, as the build reports it; 0 while queued. */
	fraction: number;
	/** `3840×2160 · 1:30`: what it is, so a long wait can be told from a short one. */
	meta: string;
	/** What a build under way says about itself: `42% · about 12× real time`. Empty for a queued job. */
	progress: string;
	/** When it will be done, to read at a glance: `7 s left` for a build, `done in 5 min` for a job waiting. With no
	 *  estimate a build says `estimating…` and a job waiting says nothing (the header says why there is none). */
	eta: string;
	/** The same in words, with the clock time; what is missing and why when there is no estimate. */
	etaTitle: string;
	/** Whether Build next means something here: a waiting job that is not already the first of those waiting. */
	canPrioritize: boolean;
}

/** `3840×2160 · 1:30` — either half left out when the source says nothing of it. */
export function metaLine(job: Pick<QueuedProxy, 'width' | 'height' | 'duration'>): string {
	const parts: string[] = [];
	if (job.width > 0 && job.height > 0) parts.push(`${job.width}×${job.height}`);
	if (Number.isFinite(job.duration) && job.duration > 0) parts.push(fmtDuration(job.duration));
	return parts.join(' · ');
}

function rowFor(job: QueuedProxy, queue: ProxyQueue, readAt: Date, locale: string | undefined, canPrioritize: boolean): QueueRow {
	const known = typeof job.eta_secs === 'number' && Number.isFinite(job.eta_secs);
	const eta = known ? (job.eta_secs as number) : null;
	const percent = Math.round(Math.min(1, Math.max(0, job.fraction)) * 100);
	let progress = '';
	if (job.state === 'building') {
		const speed = realTimeSpeed(job, queue);
		progress = [
			`${percent}%`,
			speed === null ? '' : `about ${fmtRealTime(speed)}`,
			job.pipeline !== queue.pipeline ? pipelineLabel(job.pipeline) : ''
		]
			.filter(Boolean)
			.join(' · ');
	}
	return {
		id: job.asset_id,
		name: job.name,
		state: job.state,
		position: job.position,
		fraction: job.fraction,
		meta: metaLine(job),
		progress,
		eta: eta === null ? (job.state === 'building' ? 'estimating…' : '') : job.state === 'building' ? `${fmtEta(eta)} left` : `done in ${fmtEta(eta)}`,
		etaTitle:
			eta === null
				? job.state === 'building'
					? 'No estimate for this build yet'
					: queue.paused
						? 'No estimate while the queue is paused'
						: 'No estimate yet'
				: `Done in about ${fmtEta(eta)}, around ${clockTime(readAt, eta, locale)}${job.position > 1 ? ', counting the jobs before it' : ''}`,
		canPrioritize
	};
}

/** The rows to draw, in the order the jobs run, at most `limit` of them; `more` is how many were left out. Build next is
 *  offered to every waiting job but the first (it is already next) and never to a build under way. */
export function queueRows(
	queue: ProxyQueue | null,
	readAt: Date,
	limit: number = ROW_CAP,
	locale?: string
): { rows: QueueRow[]; more: number } {
	if (!queue) return { rows: [], more: 0 };
	const rows: QueueRow[] = [];
	let waiting = 0;
	for (const job of queue.jobs.slice(0, Math.max(0, limit))) {
		const canPrioritize = job.state === 'queued' && waiting > 0;
		if (job.state === 'queued') waiting += 1;
		rows.push(rowFor(job, queue, readAt, locale, canPrioritize));
	}
	return { rows, more: Math.max(0, queue.jobs.length - rows.length) };
}

export interface FailedRow {
	id: string;
	name: string;
	/** The reason in a sentence. */
	reason: string;
}

export function failedRows(queue: ProxyQueue | null): FailedRow[] {
	return (queue?.failed ?? []).map((f) => ({ id: f.asset_id, name: f.name, reason: tidyReason(f.reason) || 'The proxy could not be built' }));
}
