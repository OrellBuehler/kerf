/* The proxy queue as the page reads it (Svelte 5 runes): the panel, the Preview's waiting chip and the status bar's source
   note all draw from this one copy of `get_proxy_queue`.

   It listens only while something needs it. `watch()` is how a view says so — the panel while it is on screen, the chip
   while it is drawn — and it hands back the way to say it no longer does; the first view to ask makes the one pair of
   subscriptions and reads the queue, the last to let go drops both. Nothing is read in between, and a queue nobody looks at
   costs the page nothing.

   Two events keep it current, and they are different things. `proxy-queue-changed` is the order, the membership, the pause
   or a failure moving: it is read at once. `proxy-progress` is every percent of a build, which moves the estimates and
   nothing else: it is read at most once a second (`PROGRESS_EVERY_MS`), the first at once and the rest as one when the
   second is up. Reads are single-flight (one at a time, a change that arrives meanwhile is one more after it) and ordered
   (`ReadOrder`: the one that started last writes), because the answer to Pause is itself a queue and must not be
   overwritten by a read that began before it.

   The backend is a parameter so the rules above are held by a test that has no backend and no clock of its own. */

import * as bridge from './api';
import { mediaStatus } from './media-status.svelte';
import { ReadOrder } from './generation';
import { describeError } from './log';
import { PROGRESS_EVERY_MS } from './proxy-queue';
import { singleFlight } from './single-flight';
import type { ProxyQueue, ProxyStatus } from './types';

/** What the queue asks of the bridge (`api.ts`). */
export interface QueueBackend {
	getProxyQueue(): Promise<ProxyQueue>;
	onProxyQueueChanged(cb: () => void): Promise<() => void>;
	onProxyProgress(cb: (s: ProxyStatus) => void): Promise<() => void>;
	prioritizeProxy(assetId: string): Promise<ProxyStatus>;
	cancelProxy(assetId: string): Promise<ProxyStatus>;
	cancelProxyQueue(): Promise<ProxyStatus[]>;
	pauseProxyQueue(): Promise<ProxyQueue>;
	resumeProxyQueue(): Promise<ProxyQueue>;
	rebuildProxy(assetId: string): Promise<ProxyStatus>;
}

/** The time, and a way to wait for it. */
export interface Clock {
	now(): number;
	after(run: () => void, ms: number): unknown;
	cancel(handle: unknown): void;
}

const realClock: Clock = {
	now: () => Date.now(),
	after: (run, ms) => setTimeout(run, ms),
	cancel: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>)
};

export class ProxyQueueState {
	/** The queue as last read: `null` until the first read answers. Raw, so a queue of hundreds is not made reactive job by job. */
	queue = $state.raw<ProxyQueue | null>(null);
	/** When it was read, in ms: the estimates count from then. */
	readAt = $state(0);
	/** Why the last read failed; `null` while they work. The queue shown is the last one that was read. */
	error = $state<string | null>(null);
	/** The assets a call is in flight for, so a second press on the same row does nothing. */
	busy = $state.raw<ReadonlySet<string>>(new Set());
	/** Whether a call about the whole queue (pause, resume, cancel all) is in flight. */
	working = $state(false);

	readonly #api: QueueBackend;
	readonly #clock: Clock;
	readonly #noteProxy: (status: ProxyStatus) => void;
	readonly #order = new ReadOrder();
	readonly #flight = singleFlight(() => this.#read());
	#watchers = 0;
	/** Bumps whenever the subscriptions are made or dropped, so an answer from an earlier set is ignored. */
	#epoch = 0;
	#stops: Array<() => void> = [];
	#lastRead = Number.NEGATIVE_INFINITY;
	#timer: unknown = null;

	constructor(
		api: QueueBackend = bridge,
		clock: Clock = realClock,
		noteProxy: (status: ProxyStatus) => void = (s) => mediaStatus.noteProxy(s)
	) {
		this.#api = api;
		this.#clock = clock;
		this.#noteProxy = noteProxy;
	}

	/** Whether any view is watching: the subscriptions are up. */
	get watched(): boolean {
		return this.#watchers > 0;
	}

	/** Start (or join) watching. Returns the way to stop: calling it twice is the same as once. */
	watch(): () => void {
		this.#watchers++;
		if (this.#watchers === 1) this.#listen();
		let held = true;
		return () => {
			if (!held) return;
			held = false;
			this.#watchers--;
			if (this.#watchers === 0) this.#unlisten();
		};
	}

	/** Read the queue now: one read at a time, and one more after it for what arrived meanwhile. */
	refresh(): Promise<void> {
		this.#flight.request();
		return this.#flight.idle();
	}

	// ---- what the views ask for ---------------------------------------------------------------------------------------

	/** Build an asset's proxy before everything else that is waiting. Rejects as the backend does (a file that needs none,
	 *  proxies off); the queue is read again either way. */
	prioritize(assetId: string): Promise<void> {
		return this.#asset(assetId, async () => this.#noteProxy(await this.#api.prioritizeProxy(assetId)));
	}

	/** Cancel one proxy, waiting or building. */
	cancel(assetId: string): Promise<void> {
		return this.#asset(assetId, async () => this.#noteProxy(await this.#api.cancelProxy(assetId)));
	}

	/** Build a failed (or any) proxy again. */
	rebuild(assetId: string): Promise<void> {
		return this.#asset(assetId, async () => this.#noteProxy(await this.#api.rebuildProxy(assetId)));
	}

	/** Cancel every proxy waiting or building. A failure stays listed, and a pause is not lifted. */
	cancelAll(): Promise<void> {
		return this.#queueWide(async () => {
			for (const status of await this.#api.cancelProxyQueue()) this.#noteProxy(status);
		}, true);
	}

	/** Pause the queue (the build under way finishes, nothing new starts) or lift the pause. The answer is the queue. */
	setPaused(paused: boolean): Promise<void> {
		return this.#queueWide(async () => this.#take(await (paused ? this.#api.pauseProxyQueue() : this.#api.resumeProxyQueue())), false);
	}

	// ---- reading ------------------------------------------------------------------------------------------------------

	async #read(): Promise<void> {
		const ticket = this.#order.begin();
		this.#lastRead = this.#clock.now();
		try {
			const queue = await this.#api.getProxyQueue();
			if (this.#order.land(ticket)) this.#set(queue);
		} catch (e) {
			this.error = describeError(e);
		}
	}

	/** An answer that is a queue (Pause's, Resume's) takes its place in line like a read. */
	#take(queue: ProxyQueue) {
		if (this.#order.land(this.#order.begin())) this.#set(queue);
	}

	#set(queue: ProxyQueue) {
		this.queue = queue;
		this.readAt = this.#clock.now();
		this.error = null;
	}

	// ---- listening ----------------------------------------------------------------------------------------------------

	#listen() {
		const epoch = ++this.#epoch;
		// A subscription is made asynchronously: one that was asked for and is no longer wanted is dropped as it arrives.
		const keep = (stop: () => void) => {
			if (epoch === this.#epoch) this.#stops.push(stop);
			else stop();
		};
		const changed = () => {
			if (epoch === this.#epoch) void this.refresh();
		};
		void this.#api.onProxyQueueChanged(changed).then(keep, () => {});
		void this.#api.onProxyProgress(() => this.#progress(epoch)).then(keep, () => {});
		void this.refresh();
	}

	#unlisten() {
		this.#epoch++;
		for (const stop of this.#stops.splice(0)) stop();
		if (this.#timer !== null) {
			this.#clock.cancel(this.#timer);
			this.#timer = null;
		}
	}

	/** A build moved: read at once if the last read is a second old, else once when the second is up (more of them
	 *  meanwhile are the same one). */
	#progress(epoch: number) {
		if (epoch !== this.#epoch || this.#timer !== null) return;
		const wait = this.#lastRead + PROGRESS_EVERY_MS - this.#clock.now();
		if (wait <= 0) {
			void this.refresh();
			return;
		}
		this.#timer = this.#clock.after(() => {
			this.#timer = null;
			if (epoch === this.#epoch) void this.refresh();
		}, wait);
	}

	// ---- one call at a time -------------------------------------------------------------------------------------------

	async #asset(assetId: string, run: () => Promise<void>): Promise<void> {
		if (this.busy.has(assetId)) return;
		this.busy = new Set(this.busy).add(assetId);
		try {
			await run();
		} finally {
			const rest = new Set(this.busy);
			rest.delete(assetId);
			this.busy = rest;
			await this.refresh();
		}
	}

	async #queueWide(run: () => Promise<void>, readAfter: boolean): Promise<void> {
		if (this.working) return;
		this.working = true;
		try {
			await run();
		} finally {
			this.working = false;
			if (readAfter) await this.refresh();
		}
	}
}

export const proxyQueue = new ProxyQueueState();
