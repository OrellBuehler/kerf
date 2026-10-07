/* The waveform tile cache: the peaks `waveform-view.ts` draws from, fetched on
 * demand and kept.
 *
 * What it guarantees, because the timeline asks for tiles on every scroll and zoom
 * tick for every visible clip:
 *  - a tile is fetched once — cached by asset + source window + bucket count, and
 *    a request already in flight is joined, not repeated;
 *  - only a few requests run at once, newest interest first, and a tile nobody
 *    wants any more (the view scrolled on) is dropped from the queue before it
 *    ever costs an IPC round trip;
 *  - an asset whose request failed is remembered, so a broken file is not asked
 *    for again on every scroll — it is tried again only after a cooldown, and
 *    reported once;
 *  - memory is bounded (least recently used goes first).
 *
 * The fetcher is injected so this has no idea what a Tauri command is. */

import type { WaveformRange } from './types';
import { tileData, tileKey, type TileData, type TileSpec } from './waveform-view';

export type Fetcher = (assetId: string, start: number, end: number, buckets: number) => Promise<WaveformRange>;

export interface CacheOptions {
	/** Tiles kept before the least recently used are dropped. */
	maxTiles?: number;
	/** Requests in flight at once. */
	concurrency?: number;
	/** How long a failed asset is left alone, ms. */
	retryAfterMs?: number;
	/** The clock, for tests. */
	now?: () => number;
	/** Called once per asset the first time a request for it fails. */
	onFail?: (assetId: string, message: string) => void;
}

interface Job {
	key: string;
	assetId: string;
	spec: TileSpec;
	/** The latest `want` call that asked for it: the most recent call runs first... */
	call: number;
	/** ...and within a call, the earlier in its list. */
	order: number;
}

interface Owner {
	notify: () => void;
	/** Tile key -> its asset, for the tiles this owner is still waiting on. */
	keys: Map<string, string>;
}

export class WaveformCache {
	readonly #fetch: Fetcher;
	readonly #maxTiles: number;
	readonly #concurrency: number;
	readonly #retryAfter: number;
	readonly #now: () => number;
	readonly #onFail?: (assetId: string, message: string) => void;

	/** Insertion order is recency: a read re-inserts. */
	readonly #tiles = new Map<string, TileData>();
	readonly #inflight = new Set<string>();
	readonly #queue = new Map<string, Job>();
	readonly #failed = new Map<string, { at: number; message: string }>();
	readonly #reported = new Set<string>();
	readonly #owners = new Map<string, Owner>();
	#seq = 0;
	#running = 0;

	constructor(fetcher: Fetcher, o: CacheOptions = {}) {
		this.#fetch = fetcher;
		this.#maxTiles = o.maxTiles ?? 512;
		this.#concurrency = Math.max(1, o.concurrency ?? 3);
		this.#retryAfter = o.retryAfterMs ?? 30_000;
		this.#now = o.now ?? Date.now;
		this.#onFail = o.onFail;
	}

	/** How long a failed asset is left alone, ms — when a view may usefully ask again. */
	get retryAfterMs(): number {
		return this.#retryAfter;
	}

	/** Tiles held. */
	get size(): number {
		return this.#tiles.size;
	}

	/** Requests running right now. */
	get inflight(): number {
		return this.#running;
	}

	/** Requests waiting for a slot. */
	get queued(): number {
		return this.#queue.size;
	}

	/** A cached tile, marked recently used. Never fetches. */
	get(assetId: string, spec: Pick<TileSpec, 'start' | 'end' | 'buckets'>): TileData | undefined {
		const key = tileKey(assetId, spec);
		const hit = this.#tiles.get(key);
		if (hit) {
			this.#tiles.delete(key);
			this.#tiles.set(key, hit);
		}
		return hit;
	}

	/** Why an asset's waveform is unavailable, while its failure is still being
	 *  remembered; `undefined` once it may be tried again (or never failed). */
	failure(assetId: string): string | undefined {
		const f = this.#failed.get(assetId);
		if (!f) return undefined;
		return this.#now() - f.at < this.#retryAfter ? f.message : undefined;
	}

	/**
	 * Say which tiles `owner` (one clip's canvas) needs now, replacing what it
	 * asked for before. Missing tiles are queued, and `notify` fires each time one
	 * of them lands or its asset fails. Cheap to call on every view change.
	 */
	want(owner: string, assetId: string, specs: readonly TileSpec[], notify: () => void): void {
		const previous = this.#owners.get(owner);
		const keys = new Map<string, string>();
		const failed = this.failure(assetId) !== undefined;
		if (!failed) this.#failed.delete(assetId); // cooldown over: a clean slate
		const call = ++this.#seq;
		for (const [order, spec] of specs.entries()) {
			const key = tileKey(assetId, spec);
			if (this.#tiles.has(key)) continue;
			keys.set(key, assetId);
			if (failed || this.#inflight.has(key)) continue;
			const queued = this.#queue.get(key);
			if (queued) Object.assign(queued, { call, order });
			else this.#queue.set(key, { key, assetId, spec, call, order });
		}
		this.#owners.set(owner, { notify, keys });
		if (previous) this.#prune(previous.keys.keys());
		this.#pump();
	}

	/** Forget what `owner` wanted (its clip went away or scrolled out). */
	release(owner: string): void {
		const o = this.#owners.get(owner);
		if (!o) return;
		this.#owners.delete(owner);
		this.#prune(o.keys.keys());
	}

	/** Drop everything: the cache, the queue, the failures. In-flight requests finish unseen. */
	clear(): void {
		this.#tiles.clear();
		this.#queue.clear();
		this.#failed.clear();
		this.#reported.clear();
		this.#owners.clear();
	}

	/** Queued jobs that no owner wants any more never run. */
	#prune(keys: Iterable<string>): void {
		for (const key of keys) {
			if (!this.#queue.has(key)) continue;
			let wanted = false;
			for (const o of this.#owners.values()) {
				if (o.keys.has(key)) {
					wanted = true;
					break;
				}
			}
			if (!wanted) this.#queue.delete(key);
		}
	}

	#pump(): void {
		while (this.#running < this.#concurrency && this.#queue.size > 0) {
			let next: Job | undefined;
			for (const job of this.#queue.values()) {
				if (!next || job.call > next.call || (job.call === next.call && job.order < next.order)) next = job;
			}
			if (!next) return;
			this.#queue.delete(next.key);
			this.#start(next);
		}
	}

	#start(job: Job): void {
		this.#running++;
		this.#inflight.add(job.key);
		const { start, end, buckets } = job.spec;
		let request: Promise<WaveformRange>;
		try {
			request = this.#fetch(job.assetId, start, end, buckets);
		} catch (e) {
			request = Promise.reject(e);
		}
		void request.then(
			(range) => {
				this.#release(job);
				this.#put(job.key, tileData(range));
				this.#tell(job.key);
				this.#pump();
			},
			(e) => {
				this.#release(job);
				// Before the pump: whatever else is queued for the file would only fail the same way.
				this.#fail(job.assetId, e instanceof Error ? e.message : String(e));
				this.#pump();
			}
		);
	}

	#release(job: Job): void {
		this.#running--;
		this.#inflight.delete(job.key);
	}

	#put(key: string, tile: TileData): void {
		this.#tiles.delete(key);
		this.#tiles.set(key, tile);
		while (this.#tiles.size > this.#maxTiles) {
			const oldest = this.#tiles.keys().next().value;
			if (oldest === undefined) break;
			this.#tiles.delete(oldest);
		}
	}

	/** Tell the owners that wanted `key`, once each. */
	#tell(key: string): void {
		for (const o of this.#owners.values()) {
			if (o.keys.has(key)) {
				o.keys.delete(key);
				o.notify();
			}
		}
	}

	#fail(assetId: string, message: string): void {
		this.#failed.set(assetId, { at: this.#now(), message });
		// Whatever else was queued for the same file would fail the same way.
		for (const [key, job] of this.#queue) if (job.assetId === assetId) this.#queue.delete(key);
		const told = new Set<Owner>();
		for (const o of this.#owners.values()) {
			for (const [key, asset] of o.keys) {
				// A request still in flight tells its owners itself when it ends.
				if (asset !== assetId || this.#inflight.has(key)) continue;
				o.keys.delete(key);
				told.add(o);
			}
		}
		for (const o of told) o.notify();
		if (!this.#reported.has(assetId)) {
			this.#reported.add(assetId);
			this.#onFail?.(assetId, message);
		}
	}
}
