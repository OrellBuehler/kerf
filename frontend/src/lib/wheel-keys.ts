/* A colour wheel's keys — pure, so the component only wires events and the tests drive the real sequences. A press of an arrow moves
 * the puck in a draft; a run of them is one edit written when the keys go quiet, on Enter or on blur, and dropped by Escape or the
 * history's undo (`key-route.ts`). Every key the wheel owns is stopped, acting or not (an edit in flight, a drag under way), so
 * none reaches the page: Backspace would delete the clip, End move the playhead. What is written is a function of the wheel of the
 * clip the run began on (`grade-write.ts`), whatever is on screen when it ends. */

import { KeyRun, type Timers } from './key-run';
import { applyRoute, routeKey, type KeyFacts } from './key-route';
import { KEY_COMMIT_MS } from './slider-gesture';
import { nudgeWheel, sameWheel, wheelKey, wheelOwnsKey } from './grade-wheel';
import type { Wheel } from './types';

export interface WheelIO {
	/** The clip on screen. */
	clipId(): string;
	/** Its wheel, as the cut has it. */
	wheel(): Wheel;
	/** What the puck shows while a run is in flight; `null` ends it. */
	draft(clipId: string, wheel: Wheel | null): void;
	/** Change the wheel of `clipId` as the cut has it when the write's turn comes. */
	put(clipId: string, change: (wheel: Wheel) => Wheel): unknown;
	/** An edit is in flight, or the proposal is on screen. */
	locked(): boolean;
	/** A drag of the puck is under way. */
	dragging(): boolean;
}

export interface WheelKeyEvent extends KeyFacts {
	undo: boolean;
	preventDefault(): void;
	stopPropagation(): void;
}

export class WheelKeys {
	readonly #io: WheelIO;
	#clip = '';
	readonly run: KeyRun<Wheel>;

	constructor(io: WheelIO, timers?: Timers) {
		this.#io = io;
		this.run = new KeyRun<Wheel>(
			(from, last) => {
				if (sameWheel(from, last)) {
					io.draft(this.#clip, null);
					return;
				}
				// only the puck: what the disc's keys change, whatever the lightness is by the time they are written
				io.put(this.#clip, (w) => ({ ...w, x: last.x, y: last.y }));
			},
			KEY_COMMIT_MS,
			timers
		);
	}

	/** A keydown on the disc; whether it is the wheel's (the event is stopped here for what is stopped here, and by the caller
	 *  after an own key has acted). */
	key(e: WheelKeyEvent): boolean {
		const io = this.#io;
		const route = routeKey(e, {
			run: this.run.active,
			held: io.locked() || io.dragging(),
			undo: e.undo,
			own: wheelOwnsKey
		});
		const handled = applyRoute(route, this.run, e);
		if (route === 'cancel-run') {
			io.draft(this.#clip, null);
			return true;
		}
		if (route !== 'own') return handled;
		e.preventDefault();
		e.stopPropagation();
		const key = wheelKey(e.key, { shift: e.shiftKey, alt: e.altKey });
		if (!key) return true; // End, Page Up, Page Down: the wheel's, and nothing
		if (!this.run.active) this.#clip = io.clipId();
		const next = nudgeWheel(this.run.last ?? io.wheel(), key);
		this.run.push(io.wheel(), next);
		io.draft(this.#clip, next);
		return true;
	}

	/** Focus left the disc: a run of keys is written. */
	blur(): void {
		this.run.finish();
	}

	/** The disc went away: what was pressed is written. */
	destroy(): void {
		this.run.finish();
	}
}
