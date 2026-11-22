import { describe, expect, test } from 'bun:test';
import { WheelKeys, type WheelIO, type WheelKeyEvent } from './wheel-keys';
import { gradeWriter } from './grade-write';
import { wheelChange } from './grade-write';
import type { Timers } from './key-run';
import type { Grade, Wheel } from './types';

// A focused colour wheel's keys, as a keyboard sends them, against the real grade writer.

function clock() {
	let now = 0;
	let next = 1;
	const waiting = new Map<number, { at: number; fn: () => void }>();
	const timers: Timers = {
		set: (fn, ms) => {
			const id = next++;
			waiting.set(id, { at: now + ms, fn });
			return id;
		},
		clear: (h) => void waiting.delete(h as number)
	};
	return {
		timers,
		advance(ms: number) {
			now += ms;
			for (const [id, w] of [...waiting]) {
				if (w.at <= now) {
					waiting.delete(id);
					w.fn();
				}
			}
		}
	};
}

function wheel(opts: { locked?: boolean; dragging?: boolean; screen?: () => string } = {}) {
	const t = clock();
	const grades: Record<string, Grade> = { a: {}, b: { wheels: { shadows: { x: 0, y: 0, lightness: 0.3 }, midtones: { x: 0, y: 0, lightness: 0 }, highlights: { x: 0, y: 0, lightness: 0 } } } };
	const edits: [string, Grade][] = [];
	const write = gradeWriter({
		gradeOf: (id) => grades[id],
		set: async (id, g) => {
			grades[id] = g;
			edits.push([id, g]);
		},
		report: (e) => {
			throw e;
		}
	});
	let screen = 'a';
	let draft: { clip: string; wheel: Wheel } | null = null;
	const io: WheelIO = {
		clipId: () => screen,
		wheel: () => grades[screen].wheels?.shadows ?? { x: 0, y: 0, lightness: 0 },
		draft: (clip, w) => (draft = w ? { clip, wheel: w } : null),
		put: (clip, change) => write(clip, wheelChange('shadows', change)),
		locked: () => !!opts.locked,
		dragging: () => !!opts.dragging
	};
	const keys = new WheelKeys(io, t.timers);
	return { keys, clock: t, grades, edits, draft: () => draft, show: (id: string) => (screen = id), flush: async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); } };
}

function key(k: string, mods: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; alt: boolean; undo: boolean }> = {}) {
	const log = { prevented: 0, stopped: 0 };
	const ev: WheelKeyEvent = {
		key: k,
		ctrlKey: !!mods.ctrl,
		metaKey: !!mods.meta,
		shiftKey: !!mods.shift,
		altKey: !!mods.alt,
		undo: !!mods.undo,
		preventDefault: () => void log.prevented++,
		stopPropagation: () => void log.stopped++
	};
	return { ev, log };
}

describe('a run of keys on the wheel', () => {
	test('is one edit, written when the keys go quiet', async () => {
		const w = wheel();
		for (let i = 0; i < 5; i++) w.keys.key(key('ArrowRight').ev);
		expect(w.edits.length).toBe(0);
		expect(w.draft()!.wheel.x).toBe(0.05);
		w.clock.advance(460);
		await w.flush();
		expect(w.edits.length).toBe(1);
		expect(w.grades.a.wheels!.shadows.x).toBe(0.05);
	});

	test('Ctrl+Z, as the keyboard sends it (Control down, then Z), drops the run: no edit, the puck restored, and the undo goes no further', async () => {
		const w = wheel();
		for (let i = 0; i < 3; i++) w.keys.key(key('ArrowUp').ev);
		expect(w.draft()).not.toBeNull();
		const control = key('Control', { ctrl: true });
		const z = key('z', { ctrl: true, undo: true });
		expect(w.keys.key(control.ev)).toBe(false);
		expect(control.log).toEqual({ prevented: 0, stopped: 0 });
		expect(w.keys.run.active).toBe(true);
		expect(w.keys.key(z.ev)).toBe(true);
		expect(z.log).toEqual({ prevented: 1, stopped: 1 });
		w.clock.advance(1000);
		await w.flush();
		expect(w.edits.length).toBe(0);
		expect(w.draft()).toBeNull();
	});

	test('the same with the user’s own undo key: it is the keymap that says what undo is, not the letter Z', async () => {
		const w = wheel();
		w.keys.key(key('ArrowUp').ev);
		// a remapped undo (Ctrl+U, say), and a Z that is no longer undo
		const notUndo = key('z', { ctrl: true, undo: false });
		expect(w.keys.key(notUndo.ev)).toBe(false); // another shortcut: writes the run, goes on
		await w.flush();
		expect(w.edits.length).toBe(1);
		const w2 = wheel();
		w2.keys.key(key('ArrowUp').ev);
		const u = key('u', { ctrl: true, undo: true });
		expect(w2.keys.key(u.ev)).toBe(true);
		expect(w2.keys.run.active).toBe(false);
		await w2.flush();
		expect(w2.edits.length).toBe(0);
	});

	test('with no run, undo is the page’s', () => {
		const w = wheel();
		const z = key('z', { ctrl: true, undo: true });
		expect(w.keys.key(key('Control', { ctrl: true }).ev)).toBe(false);
		expect(w.keys.key(z.ev)).toBe(false);
		expect(z.log).toEqual({ prevented: 0, stopped: 0 });
	});

	test('a bare modifier in the middle of a run does not end it; another shortcut writes it first and goes on', async () => {
		const w = wheel();
		w.keys.key(key('ArrowRight').ev);
		for (const m of ['Shift', 'Alt', 'Control', 'Meta']) expect(w.keys.key(key(m, { ctrl: m === 'Control' }).ev), m).toBe(false);
		expect(w.keys.run.active).toBe(true);
		const save = key('s', { ctrl: true });
		expect(w.keys.key(save.ev)).toBe(false);
		await w.flush();
		expect(w.edits.length).toBe(1);
		expect(save.log.stopped).toBe(0);
	});

	test('Escape drops it; with none it is the page’s', async () => {
		const w = wheel();
		w.keys.key(key('ArrowRight').ev);
		const esc = key('Escape');
		expect(w.keys.key(esc.ev)).toBe(true);
		expect(esc.log.stopped).toBe(1);
		await w.flush();
		expect(w.edits.length).toBe(0);
		expect(w.draft()).toBeNull();
		const again = key('Escape');
		expect(w.keys.key(again.ev)).toBe(false);
		expect(again.log.stopped).toBe(0);
	});

	test('Enter writes it now', async () => {
		const w = wheel();
		w.keys.key(key('ArrowRight').ev);
		const enter = key('Enter');
		expect(w.keys.key(enter.ev)).toBe(true);
		await w.flush();
		expect(w.edits.length).toBe(1);
	});

	test('is written to the clip it began on when focus leaves after another clip was selected', async () => {
		const w = wheel();
		w.keys.key(key('ArrowRight').ev);
		w.keys.key(key('ArrowRight').ev);
		w.show('b');
		w.keys.blur();
		await w.flush();
		expect(w.edits.map(([id]) => id)).toEqual(['a']);
		expect(w.grades.a.wheels!.shadows.x).toBe(0.02);
		expect(w.grades.b.wheels!.shadows.x).toBe(0);
		// the lightness of the clip it began on is that clip’s, not the other’s
		expect(w.grades.a.wheels!.shadows.lightness).toBe(0);
	});
});

describe('the keys a focused wheel owns', () => {
	const OWN = ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown', 'Delete', 'Backspace'];
	const NOT_OWN = [' ', 'j', 'k', 'l', 's', 'c', 'x', 'Tab', 'F2', 'Insert'];

	test('free, each is stopped, so none reaches the page; Delete, Backspace and Home put the puck back and keep the lightness', async () => {
		for (const k of OWN) {
			const w = wheel();
			const ev = key(k);
			expect(w.keys.key(ev.ev), k).toBe(true);
			expect(ev.log, k).toEqual({ prevented: 1, stopped: 1 });
			w.keys.blur();
			await w.flush();
		}
		const w = wheel();
		w.show('b');
		w.keys.key(key('ArrowRight').ev);
		w.keys.blur();
		await w.flush();
		expect(w.grades.b.wheels!.shadows).toEqual({ x: 0.01, y: 0, lightness: 0.3 });
		for (const k of ['Delete', 'Backspace', 'Home']) {
			w.keys.key(key('ArrowRight').ev);
			w.keys.blur();
			await w.flush();
			w.keys.key(key(k).ev);
			w.keys.blur();
			await w.flush();
			expect(w.grades.b.wheels!.shadows, k).toEqual({ x: 0, y: 0, lightness: 0.3 });
		}
	});

	test('End and the Page keys are the wheel’s and do nothing to it', async () => {
		const w = wheel();
		for (const k of ['End', 'PageUp', 'PageDown']) {
			const ev = key(k);
			expect(w.keys.key(ev.ev), k).toBe(true);
			expect(ev.log.stopped, k).toBe(1);
		}
		w.keys.blur();
		await w.flush();
		expect(w.edits.length).toBe(0);
	});

	test('while an edit is in flight or a drag is under way each is still stopped, and none does anything', async () => {
		for (const held of [{ locked: true }, { dragging: true }]) {
			const w = wheel(held);
			for (const k of OWN) {
				const ev = key(k);
				expect(w.keys.key(ev.ev), `${k} ${JSON.stringify(held)}`).toBe(true);
				expect(ev.log, k).toEqual({ prevented: 1, stopped: 1 });
			}
			w.keys.blur();
			await w.flush();
			expect(w.edits.length).toBe(0);
			expect(w.draft()).toBeNull();
		}
	});

	test('Space, the letters of the page’s shortcuts, Tab and the function keys are not the wheel’s: they reach the page, held or not', () => {
		for (const held of [{}, { locked: true }, { dragging: true }]) {
			const w = wheel(held);
			for (const k of NOT_OWN) {
				const ev = key(k);
				expect(w.keys.key(ev.ev), `${k} ${JSON.stringify(held)}`).toBe(false);
				expect(ev.log.stopped, k).toBe(0);
			}
		}
	});
});
