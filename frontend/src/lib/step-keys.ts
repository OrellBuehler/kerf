/* The keys of a control that steps a number — the mixer's slider and the grade's number boxes — pure, so both are one behaviour and
 * the tests drive the real sequences. A press steps the value in a preview; a run of presses is one edit, written when the keys go
 * quiet, on Enter or on blur, and dropped by Escape or the history's undo (`key-route.ts`). A key the control owns never reaches the
 * page, acting or not (a drag under way): Backspace would delete the clip, End move the playhead. */

import { KeyRun, type Timers } from './key-run';
import { applyRoute, routeKey, type KeyFacts } from './key-route';
import { KEY_COMMIT_MS } from './slider-gesture';

export interface StepIO {
	/** One of the control's own keys, by name. */
	owns(key: string): boolean;
	/** The value one press of an owned key leads to, from `from`. */
	step(from: number, e: KeyFacts): number;
	/** The committed value. */
	value(): number;
	/** Where a run that begins now starts from (the committed value, or what is half typed in a box). */
	start(): number;
	/** Whether a run that ended at `last` having begun at `from` is a change worth an edit. */
	changed(from: number, last: number): boolean;
	/** Show a value while the run is in flight. */
	preview(v: number): void;
	/** The run ended with a change: write it. */
	commit(last: number): void;
	/** The run was dropped, or ended where it began: show the committed value again. */
	abandon(): void;
	/** The control cannot act (a drag is under way) but keeps its keys. */
	held(): boolean;
}

export interface StepKeyEvent extends KeyFacts {
	undo: boolean;
	preventDefault(): void;
	stopPropagation(): void;
}

export class StepKeys {
	readonly #io: StepIO;
	readonly run: KeyRun<number>;

	constructor(io: StepIO, timers?: Timers) {
		this.#io = io;
		this.run = new KeyRun<number>((from, last) => (io.changed(from, last) ? io.commit(last) : io.abandon()), KEY_COMMIT_MS, timers);
	}

	/** A keydown on the control; whether it is the control's. */
	key(e: StepKeyEvent): boolean {
		const io = this.#io;
		const route = routeKey(e, { run: this.run.active, held: io.held(), undo: e.undo, own: (k) => io.owns(k) });
		const handled = applyRoute(route, this.run, e);
		if (route === 'cancel-run') {
			io.abandon();
			return true;
		}
		if (route !== 'own') return handled;
		e.preventDefault();
		e.stopPropagation();
		const from = this.run.from ?? io.value();
		const next = io.step(this.run.last ?? io.start(), e);
		this.run.push(from, next);
		io.preview(next);
		return true;
	}

	/** Focus left the control: a run is written. */
	blur(): void {
		this.run.finish();
	}
}
