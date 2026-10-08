// "Alt pressed and released on its own" — the gesture that focuses the menu bar —
// as a small state machine, so what must *not* count can be tested without a window.
//
// It counts only when nothing else happened while Alt was down: no other key, no
// pointer press, no wheel, and the window kept focus. The timeline reads Alt live
// as its "leave the links alone" override on a drag, a trim or a razor cut, so a
// tap that arrives during one of those is the user holding a modifier, not asking
// for the menu: while any pointer button is held Alt is never armed, and a release
// during a drag finds nothing armed.

export interface AltKey {
	key: string;
	repeat: boolean;
	ctrlKey: boolean;
	metaKey: boolean;
	shiftKey: boolean;
	/** AltGr is a typing key on many layouts, not Alt. */
	altGraph: boolean;
}

export interface AltTap {
	press(e: AltKey): void;
	/** Whether this release finishes a tap of Alt on its own. */
	release(key: string): boolean;
	pointerDown(): void;
	pointerUp(): void;
	/** `buttons` of a pointer move: nothing held is the end of a press whose release was missed. */
	pointerMove(buttons: number): void;
	wheel(): void;
	/** The window lost focus (Alt+Tab): an Alt held across it is not a tap. */
	blur(): void;
}

export function createAltTap(): AltTap {
	let armed = false;
	let held = false;
	return {
		press(e) {
			if (e.key === 'Alt') {
				armed = !held && !e.repeat && !e.ctrlKey && !e.metaKey && !e.shiftKey && !e.altGraph;
			} else {
				armed = false;
			}
		},
		release(key) {
			if (key !== 'Alt') return false;
			const tap = armed && !held;
			armed = false;
			return tap;
		},
		pointerDown() {
			held = true;
			armed = false;
		},
		pointerUp() {
			held = false;
		},
		pointerMove(buttons) {
			held = buttons !== 0;
			if (held) armed = false;
		},
		wheel() {
			armed = false;
		},
		blur() {
			armed = false;
			held = false;
		}
	};
}
