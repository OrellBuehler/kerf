/* A run of key presses is one edit: the presses of an arrow held or tapped, written out when the keys have been
 * quiet for a moment, the control loses focus or Enter is pressed — and not at all when it is given up (Escape, an
 * undo). `MixSlider` does this inside its own markup; the wheels, the curve and the grade's number fields share this one,
 * with the timers injectable so the tests can run it without waiting. */

import { KEY_COMMIT_MS } from './slider-gesture';

export interface Timers {
	set(fn: () => void, ms: number): unknown;
	clear(handle: unknown): void;
}

const REAL: Timers = { set: (fn, ms) => setTimeout(fn, ms), clear: (h) => clearTimeout(h as ReturnType<typeof setTimeout>) };

export class KeyRun<T> {
	#from: T | null = null;
	#last: T | null = null;
	#timer: unknown = null;
	#live = false;
	readonly #done: (from: T, last: T) => void;
	readonly #ms: number;
	readonly #timers: Timers;

	/** `done` is called once with where the run began and where it ended, when the run is finished. */
	constructor(done: (from: T, last: T) => void, ms = KEY_COMMIT_MS, timers: Timers = REAL) {
		this.#done = done;
		this.#ms = ms;
		this.#timers = timers;
	}

	/** A run is under way. */
	get active(): boolean {
		return this.#live;
	}

	/** Where the run is now, or `null` when there is none. */
	get last(): T | null {
		return this.#live ? this.#last : null;
	}

	/** Where the run began, or `null` when there is none. */
	get from(): T | null {
		return this.#live ? this.#from : null;
	}

	/** One more press: the run is now at `last`. `from` is only read for the first press of a run. */
	push(from: T, last: T): void {
		if (!this.#live) {
			this.#live = true;
			this.#from = from;
		}
		this.#last = last;
		if (this.#timer !== null) this.#timers.clear(this.#timer);
		this.#timer = this.#timers.set(() => this.finish(), this.#ms);
	}

	/** Write the run out now, as one edit. A run with nothing under way does nothing. */
	finish(): void {
		if (!this.#live) return;
		const [from, last] = [this.#from as T, this.#last as T];
		this.#drop();
		this.#done(from, last);
	}

	/** Give the run up: nothing is written. Whether there was one. */
	cancel(): boolean {
		if (!this.#live) return false;
		this.#drop();
		return true;
	}

	#drop(): void {
		if (this.#timer !== null) this.#timers.clear(this.#timer);
		this.#timer = null;
		this.#live = false;
		this.#from = null;
		this.#last = null;
	}
}
