/* What a control with a run of key presses does with a key — pure, so the wheel, the curve, the grade's number boxes and the
 * mixer's slider agree, and the tests drive the real sequences. A run of presses (an arrow tapped five times) is one edit that
 * is written when the keys go quiet; until then Escape and the history's undo give it up, and any other shortcut writes it first.
 *
 * Two things a control used to get wrong: (1) a bare modifier is a keydown of its own — the Control that begins Ctrl+Z arrives
 * first, with `ctrlKey` already true, and was taken for "another shortcut", which wrote the run and left Z with nothing to drop
 * (the global undo then took the edit before it); (2) a key the control owns but cannot act on right now (an edit is in flight, a
 * drag is under way) fell through to the page, where Backspace deletes the clip and End moves the playhead. */

import { MODIFIER_KEYS } from './keymap';

export interface KeyFacts {
	key: string;
	ctrlKey: boolean;
	metaKey: boolean;
	shiftKey: boolean;
	altKey: boolean;
}

/** A modifier pressed on its own: not a shortcut, and not anything a run should end for. */
export const isModifierKey = (key: string): boolean => MODIFIER_KEYS.has(key);

export interface RouteContext {
	/** A run of presses is pending: its change is not written yet. */
	run: boolean;
	/** The control cannot act: an edit is in flight, the proposal is on screen, or a drag is under way. */
	held: boolean;
	/** The key is the history's undo as the user's keymap has it (`settings.actionFor(e) === 'edit.undo'`), not a hard-coded Z. */
	undo: boolean;
	/** One of the control's own keys, by name: it is the control's even when it cannot act, so it never reaches the page. */
	own: (key: string) => boolean;
}

/** - `ignore`: a bare modifier, nothing happens and nothing is stopped;
 *  - `pass`: not the control's, on to the page;
 *  - `finish-then-pass`: a shortcut while a run is pending: the run is written first, then the shortcut goes on;
 *  - `cancel-run`: Escape or undo while a run is pending: the run is dropped and the event is the control's, so no undo goes on;
 *  - `finish-run`: Enter while a run is pending: written now;
 *  - `swallow`: one of the control's keys while it is held: nothing is done and nothing goes on to the page;
 *  - `own`: one of the control's keys: the control acts. */
export type KeyRoute = 'ignore' | 'pass' | 'finish-then-pass' | 'cancel-run' | 'finish-run' | 'swallow' | 'own';

export function routeKey(e: KeyFacts, c: RouteContext): KeyRoute {
	if (isModifierKey(e.key)) return 'ignore';
	if (c.undo) return c.run ? 'cancel-run' : 'pass';
	if (e.key === 'Escape') return c.run ? 'cancel-run' : 'pass';
	if (e.ctrlKey || e.metaKey) return c.run ? 'finish-then-pass' : 'pass';
	if (e.key === 'Enter' && c.run) return 'finish-run';
	if (!c.own(e.key)) return 'pass';
	return c.held ? 'swallow' : 'own';
}

export interface RunLike {
	readonly active: boolean;
	finish(): void;
	cancel(): boolean;
}

export interface StoppableEvent {
	preventDefault(): void;
	stopPropagation(): void;
}

/** Does what a route says to the run and the event, and says whether the event is the control's: a dropped run, a run written on
 *  Enter and a held control's key are stopped here (the page must not see them); an `own` key is left for the control to act on
 *  and stop itself. A control that drops a run restores what it shows when this returns true for `cancel-run`. */
export function applyRoute(route: KeyRoute, run: RunLike, e: StoppableEvent): boolean {
	switch (route) {
		case 'ignore':
		case 'pass':
			return false;
		case 'finish-then-pass':
			run.finish();
			return false;
		case 'cancel-run':
			run.cancel();
			e.preventDefault();
			e.stopPropagation();
			return true;
		case 'finish-run':
			run.finish();
			e.preventDefault();
			e.stopPropagation();
			return true;
		case 'swallow':
			e.preventDefault();
			e.stopPropagation();
			return true;
		case 'own':
			return true;
	}
}

/** A keydown as the routers take it: its keys, and whether the user's keymap calls it undo (`settings.actionFor(e) === 'edit.undo'`). */
export function keyEvent(e: KeyboardEvent, undo: boolean): KeyFacts & { undo: boolean } & StoppableEvent {
	return {
		key: e.key,
		ctrlKey: e.ctrlKey,
		metaKey: e.metaKey,
		shiftKey: e.shiftKey,
		altKey: e.altKey,
		undo,
		preventDefault: () => e.preventDefault(),
		stopPropagation: () => e.stopPropagation()
	};
}
