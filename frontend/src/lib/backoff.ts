/* Remembering what failed, so it is not asked for again on every scroll.
 *
 * The timeline asks a cache for something on every view change, for every clip
 * on screen; a file that cannot be read fails the same way each time, and
 * asking again per scroll tick costs an IPC round trip (and often a decode) to
 * learn nothing. A key that failed is held off for a cooldown that doubles with
 * every failure in a row, up to a cap, and is forgotten the moment something
 * for it succeeds. Failures that land together (several requests for one file
 * in flight at once) are one failure, not several — otherwise a burst of three
 * would put the third cooldown on the first mistake.
 *
 * This is the rule `waveform-cache.ts` applies to its assets, as a unit the
 * filmstrip cache shares the shape of. Pure but for the clock, which is
 * injectable. */

export interface BackoffOptions {
	/** How long a key is left alone the first time, ms. Each further failure in a
	 *  row doubles it, up to `maxMs`. */
	baseMs?: number;
	/** The longest a key is left alone, ms. */
	maxMs?: number;
	/** The clock, for tests. */
	now?: () => number;
}

interface Failure {
	at: number;
	message: string;
	/** Failures in a row: it sets the cooldown. */
	count: number;
}

export class Backoff {
	readonly #base: number;
	readonly #max: number;
	readonly #now: () => number;
	readonly #failed = new Map<string, Failure>();

	constructor(o: BackoffOptions = {}) {
		this.#base = o.baseMs ?? 30_000;
		this.#max = Math.max(this.#base, o.maxMs ?? 10 * 60_000);
		this.#now = o.now ?? Date.now;
	}

	/** How long a key is left alone after its `count`th failure in a row. */
	cooldown(count: number): number {
		return Math.min(this.#base * 2 ** Math.max(0, count - 1), this.#max);
	}

	/** Milliseconds until `key` may be asked for again, or `undefined` when it is
	 *  not being held off — what a view waits before trying again. */
	retryIn(key: string): number | undefined {
		const f = this.#failed.get(key);
		if (!f) return undefined;
		const left = f.at + this.cooldown(f.count) - this.#now();
		return left > 0 ? left : undefined;
	}

	/** Why `key` failed, while the failure is still being held against it;
	 *  `undefined` once it may be tried again (or never failed). */
	failure(key: string): string | undefined {
		return this.retryIn(key) === undefined ? undefined : this.#failed.get(key)?.message;
	}

	/** Failures in a row for `key` (0 when it has none on record). */
	count(key: string): number {
		return this.#failed.get(key)?.count ?? 0;
	}

	/** Record a failure. One that lands while the previous is still held against
	 *  the key is the same burst; one after the cooldown has run out is the next
	 *  in a row. */
	fail(key: string, message: string): void {
		const prev = this.#failed.get(key);
		const burst = prev !== undefined && this.retryIn(key) !== undefined;
		this.#failed.set(key, {
			at: burst ? prev.at : this.#now(),
			message,
			count: burst ? prev.count : (prev?.count ?? 0) + 1
		});
	}

	/** It worked: the next failure starts over from the first cooldown. */
	succeed(key: string): void {
		this.#failed.delete(key);
	}

	/** Keep only the keys in `keep`. */
	retain(keep: Iterable<string>): void {
		const alive = new Set(keep);
		for (const key of [...this.#failed.keys()]) if (!alive.has(key)) this.#failed.delete(key);
	}

	clear(): void {
		this.#failed.clear();
	}
}
