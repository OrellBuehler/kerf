/* The curve editor's gestures and keys — pure, so the component only draws and wires events, and the tests drive the real event
 * orders. The component hands this an `io` (what is on screen, what a gesture shows while it is in flight, how to write) and the
 * pointer's place in the unit square; every write is a *function of the channel as the cut has it* that names the point by its input
 * (`curve-edit.ts`), never by its index in the list that happened to be drawn.
 *
 * The gestures, as other curve editors have them: a click on empty space adds a point (one edit, written on release, so a drag that
 * follows is part of it); a press on a point takes hold of it (one edit if it moved); a double-click on a point takes it out; a
 * double-click on empty space adds the one point its first click added, and removes nothing — the second click of it lands on that
 * very point, which is not "an existing point" for the double-click that made it. A right-click on a point takes it out. */

import { KeyRun, type Timers } from './key-run';
import { applyRoute, routeKey, type KeyFacts } from './key-route';
import { KEY_COMMIT_MS } from './slider-gesture';
import { addPoint, movePoint, type CurveChannel } from './grade';
import { addAfter, addAt, afterRemoval, curveKey, moveAtX, nudgePoint, pickPoint, pointWords, removeAtX, removePoint, selectPoint } from './curve-edit';
import type { CurvePoint } from './types';

/** What a write does to one channel as the cut has it (the identity's two ends when it has no points): the points to write, or `null` for none. */
export type PointsChange = (points: CurvePoint[]) => CurvePoint[] | null;

/** The clip and channel a gesture began on. */
export interface CurveTarget {
	clipId: string;
	channel: CurveChannel;
}

export interface CurveIO {
	/** The clip and channel on screen now. */
	target(): CurveTarget;
	/** The points drawn now: a gesture's draft, else the cut's (the identity's two ends for none). */
	points(): CurvePoint[];
	/** The selected point's place in `points()`. */
	selected(): number | null;
	select(index: number | null): void;
	/** What the canvas shows while a gesture or a run of keys is in flight; `null` ends it. */
	draft(target: CurveTarget, points: CurvePoint[] | null): void;
	/** Write a change to the channel of `target` as the cut has it when the write's turn comes. */
	put(target: CurveTarget, change: PointsChange): unknown;
	/** Said to a screen reader: the point as it is now. Called on a press, a release and a key, never on a pointer move. */
	announce(text: string): void;
	/** An edit is in flight, or the proposal is on screen. */
	locked(): boolean;
	/** The side of the square the points are drawn in, px (for how near a pointer must be). */
	side(): number;
}

export interface CurveKeyEvent extends KeyFacts {
	undo: boolean;
	preventDefault(): void;
	stopPropagation(): void;
}

/** The second click of a double-click is this soon after the first one let go, at most (the system's own interval is 500). */
export const DOUBLE_CLICK_MS = 600;

interface Held {
	target: CurveTarget;
	/** The points the gesture started from (with the new point in, when it added one). */
	base: CurvePoint[];
	points: CurvePoint[];
	index: number;
	/** The point as it was when it was taken hold of; `null` for one this gesture added. */
	from: CurvePoint | null;
	before: number | null;
	moved: boolean;
}

interface Pressed {
	x0: number;
	points: CurvePoint[];
	index: number;
}

export class CurveControl {
	readonly #io: CurveIO;
	readonly #clock: () => number;
	#held: Held | null = null;
	/** The point a click has just added, and when it let go. */
	#added: { x: number; at: number } | null = null;
	#runTarget: CurveTarget = { clipId: '', channel: 'master' };
	readonly run: KeyRun<Pressed>;

	constructor(io: CurveIO, clock: () => number = () => performance.now(), timers?: Timers) {
		this.#io = io;
		this.#clock = clock;
		this.run = new KeyRun<Pressed>(
			(from, last) => {
				const [a, b] = [from.points[from.index], last.points[last.index]];
				if (!a || !b || (a.x === b.x && a.y === b.y)) {
					io.draft(this.#runTarget, null);
					return;
				}
				io.put(this.#runTarget, (cur) => moveAtX(cur, from.x0, b));
			},
			KEY_COMMIT_MS,
			timers
		);
	}

	/** A drag is under way. */
	get dragging(): boolean {
		return this.#held !== null;
	}

	// ---- the pointer -------------------------------------------------------------

	/** A press at `(x, y)` of the unit square. False when it takes nothing (an edit is in flight, or there is no room for a point):
	 *  the component starts no drag. */
	down(x: number, y: number): boolean {
		const io = this.#io;
		if (io.locked()) return false;
		this.run.finish();
		const base = io.points();
		let index = pickPoint(base, x, y, io.side());
		let points = base;
		let from: CurvePoint | null = null;
		if (index < 0) {
			const next = addPoint(base, x, y);
			if (!next) return false;
			points = next.points;
			index = next.index;
		} else {
			from = base[index];
		}
		const target = io.target();
		this.#held = { target, base: points, points, index, from, before: io.selected(), moved: false };
		io.select(index);
		io.draft(target, points);
		if (from) io.announce(pointWords(points, index));
		return true;
	}

	move(x: number, y: number): void {
		const h = this.#held;
		if (!h) return;
		h.moved = true;
		h.points = movePoint(h.base, h.index, x, y);
		this.#io.draft(h.target, h.points);
	}

	/** The button was let go: one edit if the press added a point or moved one. */
	up(): void {
		const h = this.#held;
		if (!h) return;
		this.#held = null;
		const end = h.points[h.index];
		if (!h.from) {
			this.#added = { x: end.x, at: this.#clock() };
			this.#io.announce(pointWords(h.points, h.index));
			this.#io.put(h.target, (cur) => addAt(cur, end));
			return;
		}
		if (!h.moved) {
			this.#io.draft(h.target, null);
			return;
		}
		this.#io.announce(pointWords(h.points, h.index));
		const x0 = h.from.x;
		this.#io.put(h.target, (cur) => moveAtX(cur, x0, end));
	}

	/** Escape, pointercancel or a lost focus: nothing is written and what was selected is again. */
	cancel(): void {
		const h = this.#held;
		if (!h) return;
		this.#held = null;
		this.#io.draft(h.target, null);
		this.#io.select(h.before);
	}

	/** The `dblclick` event at `(x, y)`; whether it was the editor's (the component stops it). */
	doubleClick(x: number, y: number): boolean {
		const io = this.#io;
		if (io.locked()) return false;
		const points = io.points();
		const i = pickPoint(points, x, y, io.side());
		if (i < 0) return false;
		const first = this.#added;
		// The point under a double-click that began on empty space is the one its first click added: it stays.
		if (first && Math.abs(first.x - points[i].x) < 1e-6 && this.#clock() - first.at < DOUBLE_CLICK_MS) return true;
		this.#remove(i);
		return true;
	}

	/** A right-click at `(x, y)`: takes the point under it out. Whether there was one. */
	contextMenu(x: number, y: number): boolean {
		const io = this.#io;
		if (io.locked()) return false;
		const i = pickPoint(io.points(), x, y, io.side());
		if (i < 0) return false;
		this.#remove(i);
		return true;
	}

	/** Take the selected point out (the Remove point button, and Delete). */
	removeSelected(): void {
		this.run.finish();
		const sel = this.#io.selected();
		if (sel !== null) this.#remove(sel);
	}

	/** Take point `index` of what is drawn out of the channel of the clip on screen, by its input. */
	#remove(index: number): void {
		const io = this.#io;
		this.run.finish();
		const points = io.points();
		const p = points[index];
		if (!p) return;
		const target = io.target();
		const rest = removePoint(points, index);
		const sel = afterRemoval(index, points.length);
		io.select(sel);
		io.draft(target, rest);
		io.announce(`Point removed. ${pointWords(rest, sel ?? -1)}`);
		const x = p.x;
		io.put(target, (cur) => removeAtX(cur, x));
	}

	// ---- the keys ----------------------------------------------------------------

	/** A keydown on the editor; whether it is the editor's, which the component then stops (`preventDefault` and `stopPropagation`
	 *  are called here for what is stopped here). */
	key(e: CurveKeyEvent): boolean {
		const io = this.#io;
		const route = routeKey(e, {
			run: this.run.active,
			held: io.locked() || this.#held !== null,
			undo: e.undo,
			own: (k) => curveKey(k) !== null
		});
		const handled = applyRoute(route, this.run, e);
		if (route === 'cancel-run') {
			io.draft(this.#runTarget, null);
			io.announce(pointWords(io.points(), io.selected() ?? -1));
			return true;
		}
		if (route !== 'own') return handled;
		const key = curveKey(e.key, { shift: e.shiftKey, alt: e.altKey });
		if (!key) return false;
		e.preventDefault();
		e.stopPropagation();
		const points = io.points();
		const sel = io.selected();
		switch (key.kind) {
			case 'select': {
				this.run.finish();
				const i = selectPoint(sel, points.length, key.to);
				io.select(i);
				io.announce(pointWords(points, i ?? -1));
				break;
			}
			case 'move': {
				const base = this.run.last?.points ?? points;
				const i = this.run.last?.index ?? sel ?? selectPoint(null, base.length, 'first');
				if (i === null) break;
				if (sel === null) io.select(i);
				if (!this.run.active) this.#runTarget = io.target();
				const next = nudgePoint(base, i, key.dx, key.dy, key.size);
				const from = this.run.from ?? { x0: base[i].x, points, index: i };
				this.run.push(from, { x0: from.x0, points: next, index: i });
				io.draft(this.#runTarget, next);
				io.announce(pointWords(next, i));
				break;
			}
			case 'remove':
				this.removeSelected();
				break;
			case 'add': {
				const added = addAfter(points, sel ?? 0);
				if (!added) break;
				const target = io.target();
				const p = added.points[added.index];
				io.select(added.index);
				io.draft(target, added.points);
				io.announce(pointWords(added.points, added.index));
				io.put(target, (cur) => addAt(cur, p));
				break;
			}
		}
		return true;
	}

	/** Focus left the editor: a run of keys is written. */
	blur(): void {
		this.run.finish();
	}

	/** The editor went away: what was pressed is written, and no drag is left behind. */
	destroy(): void {
		this.run.finish();
		this.#held = null;
	}
}
