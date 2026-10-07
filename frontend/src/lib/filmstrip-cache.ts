/* The filmstrip cache: the decoded sheets `filmstrip-view.ts` draws from, fetched
 * on demand and kept.
 *
 * What it guarantees, because every video clip on screen wants its asset's strip
 * on every view change and a split makes two clips of one asset:
 *  - an asset's strip is asked of the backend once — its sheets are decoded into
 *    images once, and every clip of the asset (and every later redraw) shares
 *    them; a request already in flight is joined, not repeated;
 *  - only a couple of assets load at once, the newest interest first, and one that
 *    nobody wants any more (the view scrolled on) is dropped from the queue before
 *    it costs a backend round trip;
 *  - an asset whose strip failed is remembered, so a broken file is not asked for
 *    again on every scroll — it is tried again after a cooldown that doubles with
 *    every failure in a row (`Backoff`), and reported once;
 *  - memory is bounded *in bytes* (a sheet is a few MB decoded, and an asset has
 *    up to a few dozen): the least recently used asset goes first, the one just
 *    used never does, so a working set larger than the budget degrades to
 *    reloading rather than to nothing.
 *
 * The strip's geometry is kept without its `data:` URLs (the JPEGs are decoded
 * and the strings are dead weight). Fetching and decoding are injected, so this
 * has no idea what a Tauri command or an `Image` is. */

import { Backoff } from './backoff';
import type { StripGeometry } from './filmstrip-geometry';
import type { Filmstrip, FilmstripSheet } from './types';

/** A sheet decoded and ready to blit. `bytes` is its memory cost (width x height x
 *  4); `close` hands it back where the platform wants that (an `ImageBitmap`). */
export interface SheetImage {
	image: CanvasImageSource;
	bytes: number;
	close?: () => void;
}

export type StripFetcher = (assetId: string) => Promise<Filmstrip>;
export type SheetDecoder = (sheet: FilmstripSheet) => Promise<SheetImage>;

/** An asset's strip as the view reads it: the geometry and a decoded image per sheet. */
export interface LoadedStrip {
	strip: StripGeometry;
	/** One per `strip.sheets`, in order. */
	images: CanvasImageSource[];
	bytes: number;
}

export interface FilmCacheOptions {
	/** Decoded sheet memory kept before the least recently used asset goes. */
	maxBytes?: number;
	/** Assets loading at once. */
	concurrency?: number;
	/** How long a failed asset is left alone the first time, ms (doubles per failure). */
	retryAfterMs?: number;
	/** The longest a failing asset is left alone, ms. */
	maxRetryMs?: number;
	/** The clock, for tests. */
	now?: () => number;
	/** Called once per asset the first time loading it fails. */
	onFail?: (assetId: string, message: string) => void;
}

/** Decoded sheets kept: 192 MB — a few dozen long assets' worth, far more than the
 *  handful a timeline puts on screen at once. */
export const DEFAULT_MAX_BYTES = 192 * 1024 * 1024;

interface Entry extends LoadedStrip {
	close: (() => void)[];
}

interface Job {
	assetId: string;
	/** The latest `want` that asked for it: the most recent runs first. */
	call: number;
}

interface Owner {
	assetId: string;
	notify: () => void;
}

/** A strip's geometry without the pixels it travelled with. */
export function geometryOf(f: Filmstrip): StripGeometry {
	return {
		interval: f.interval,
		frame_width: f.frame_width,
		frame_height: f.frame_height,
		frames: f.frames,
		columns: f.columns,
		sheets: f.sheets.map(({ first_frame, count, width, height }) => ({ first_frame, count, width, height }))
	};
}

export class FilmstripCache {
	readonly #fetch: StripFetcher;
	readonly #decode: SheetDecoder;
	readonly #maxBytes: number;
	readonly #concurrency: number;
	readonly #onFail?: (assetId: string, message: string) => void;
	readonly #backoff: Backoff;

	/** Insertion order is recency: a read re-inserts. */
	readonly #entries = new Map<string, Entry>();
	#bytes = 0;
	readonly #queue = new Map<string, Job>();
	/** Asset -> the epoch its load started in. */
	readonly #inflight = new Map<string, number>();
	readonly #reported = new Set<string>();
	readonly #owners = new Map<string, Owner>();
	#seq = 0;
	#running = 0;
	/** Bumped by `clear`, so a load that was running through one lands nowhere. */
	#epoch = 0;

	constructor(fetcher: StripFetcher, decoder: SheetDecoder, o: FilmCacheOptions = {}) {
		this.#fetch = fetcher;
		this.#decode = decoder;
		this.#maxBytes = Math.max(1, o.maxBytes ?? DEFAULT_MAX_BYTES);
		this.#concurrency = Math.max(1, o.concurrency ?? 2);
		this.#onFail = o.onFail;
		this.#backoff = new Backoff({ baseMs: o.retryAfterMs, maxMs: o.maxRetryMs, now: o.now });
	}

	/** Assets held. */
	get size(): number {
		return this.#entries.size;
	}

	/** Decoded bytes held. */
	get bytes(): number {
		return this.#bytes;
	}

	/** Loads running right now. */
	get inflight(): number {
		return this.#running;
	}

	/** Loads waiting for a slot. */
	get queued(): number {
		return this.#queue.size;
	}

	/** An asset's strip, marked recently used. Never fetches. */
	get(assetId: string): LoadedStrip | undefined {
		const hit = this.#entries.get(assetId);
		if (hit) {
			this.#entries.delete(assetId);
			this.#entries.set(assetId, hit);
		}
		return hit;
	}

	/** Why an asset's strip is unavailable, while its failure is still being
	 *  remembered; `undefined` once it may be tried again (or never failed). */
	failure(assetId: string): string | undefined {
		return this.#backoff.failure(assetId);
	}

	/** Milliseconds until a failed asset may be asked for again, or `undefined`
	 *  when it is not being held off — what a view waits before trying again. */
	retryIn(assetId: string): number | undefined {
		return this.#backoff.retryIn(assetId);
	}

	/**
	 * Say which asset `owner` (one clip's canvas) needs now, replacing what it
	 * asked for before. A strip that is not held is queued, and `notify` fires
	 * when it lands or its asset fails. Cheap to call on every view change.
	 */
	want(owner: string, assetId: string, notify: () => void): void {
		const previous = this.#owners.get(owner);
		const entry: Owner = { assetId, notify };
		this.#owners.set(owner, entry);
		if (previous && previous.assetId !== assetId) this.#prune(previous.assetId);
		const held = this.#entries.has(assetId);
		const failed = this.failure(assetId) !== undefined;
		if (!held && !failed) {
			const queued = this.#queue.get(assetId);
			if (queued) queued.call = ++this.#seq;
			else if (this.#inflight.get(assetId) !== this.#epoch) this.#queue.set(assetId, { assetId, call: ++this.#seq });
		}
		// Nothing is coming to tell this owner: the strip is already here (another
		// clip of the asset loaded it a moment ago) or the asset is being held off.
		// Say so, once, off the caller's stack.
		if (held || failed) {
			queueMicrotask(() => {
				if (this.#owners.get(owner) === entry) notify();
			});
		}
		this.#pump();
	}

	/** Forget what `owner` wanted (its clip went away, scrolled out, or got too short). */
	release(owner: string): void {
		const o = this.#owners.get(owner);
		if (!o) return;
		this.#owners.delete(owner);
		this.#prune(o.assetId);
	}

	/** Keep only the assets in `keep`: one removed from the project frees its
	 *  sheets (and its failure record) rather than waiting for the LRU. */
	prune(keep: Iterable<string>): void {
		const alive = new Set(keep);
		for (const id of [...this.#entries.keys()]) if (!alive.has(id)) this.#evict(id);
		for (const id of [...this.#queue.keys()]) if (!alive.has(id)) this.#queue.delete(id);
		this.#backoff.retain(alive);
		for (const id of [...this.#reported]) if (!alive.has(id)) this.#reported.delete(id);
	}

	/** Drop everything: the strips, the queue, the failures. Loads in flight finish unseen. */
	clear(): void {
		this.#epoch++;
		for (const id of [...this.#entries.keys()]) this.#evict(id);
		this.#queue.clear();
		this.#backoff.clear();
		this.#reported.clear();
		this.#owners.clear();
	}

	/** A queued asset no owner wants any more never loads. */
	#prune(assetId: string): void {
		if (!this.#queue.has(assetId)) return;
		for (const o of this.#owners.values()) if (o.assetId === assetId) return;
		this.#queue.delete(assetId);
	}

	#pump(): void {
		while (this.#running < this.#concurrency && this.#queue.size > 0) {
			let next: Job | undefined;
			for (const job of this.#queue.values()) if (!next || job.call > next.call) next = job;
			if (!next) return;
			this.#queue.delete(next.assetId);
			this.#start(next.assetId);
		}
	}

	#start(assetId: string): void {
		const epoch = this.#epoch;
		this.#running++;
		this.#inflight.set(assetId, epoch);
		let request: Promise<Filmstrip>;
		try {
			request = this.#fetch(assetId);
		} catch (e) {
			request = Promise.reject(e);
		}
		void request
			.then((f) => this.#decodeAll(f))
			.then(
				(loaded) => {
					this.#finish(assetId, epoch);
					if (epoch !== this.#epoch) {
						for (const c of loaded.close) c();
						return;
					}
					this.#backoff.succeed(assetId);
					this.#put(assetId, loaded);
					this.#tell(assetId);
					this.#pump();
				},
				(e) => {
					this.#finish(assetId, epoch);
					if (epoch === this.#epoch) this.#fail(assetId, e instanceof Error ? e.message : String(e));
					this.#pump();
				}
			);
	}

	#finish(assetId: string, epoch: number): void {
		this.#running--;
		// A load from before a `clear` must not take the marker of one started since.
		if (this.#inflight.get(assetId) === epoch) this.#inflight.delete(assetId);
	}

	/** Decode every sheet; one that fails fails the strip (a strip with a hole in
	 *  it would draw as a missing stretch of footage), and the ones that did
	 *  decode are handed back. */
	async #decodeAll(f: Filmstrip): Promise<Entry> {
		const settled = await Promise.allSettled(f.sheets.map((s) => this.#decode(s)));
		const images: SheetImage[] = [];
		let failure: unknown;
		for (const r of settled) {
			if (r.status === 'fulfilled') images.push(r.value);
			else failure ??= r.reason;
		}
		if (failure !== undefined) {
			for (const i of images) i.close?.();
			throw failure instanceof Error ? failure : new Error(String(failure));
		}
		return {
			strip: geometryOf(f),
			images: images.map((i) => i.image),
			bytes: images.reduce((n, i) => n + i.bytes, 0),
			close: images.flatMap((i) => (i.close ? [i.close] : []))
		};
	}

	#put(assetId: string, entry: Entry): void {
		const old = this.#entries.get(assetId);
		if (old) this.#evict(assetId);
		this.#entries.set(assetId, entry);
		this.#bytes += entry.bytes;
		// Over budget: the least recently used goes first — never the one just put.
		while (this.#bytes > this.#maxBytes && this.#entries.size > 1) {
			const oldest = this.#entries.keys().next().value;
			if (oldest === undefined || oldest === assetId) break;
			this.#evict(oldest);
		}
	}

	#evict(assetId: string): void {
		const e = this.#entries.get(assetId);
		if (!e) return;
		this.#entries.delete(assetId);
		this.#bytes -= e.bytes;
		for (const c of e.close) c();
	}

	/** Tell the owners that wanted `assetId`. */
	#tell(assetId: string): void {
		for (const o of [...this.#owners.values()]) if (o.assetId === assetId) o.notify();
	}

	#fail(assetId: string, message: string): void {
		this.#backoff.fail(assetId, message);
		this.#tell(assetId);
		if (!this.#reported.has(assetId)) {
			this.#reported.add(assetId);
			this.#onFail?.(assetId, message);
		}
	}
}
