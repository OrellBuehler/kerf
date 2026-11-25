import { describe, expect, test } from 'bun:test';
import { CurveControl, DOUBLE_CLICK_MS, type CurveIO, type CurveKeyEvent, type CurveTarget, type PointsChange } from './curve-control';
import { IDENTITY_POINTS, normalizeCurve, type CurveChannel } from './grade';
import { curveChange, gradeWriter } from './grade-write';
import type { Timers } from './key-run';
import type { CurvePoint, Grade } from './types';

// The curve editor's gestures and keys, driven in the order a browser sends them, against the real grade writer: what is asserted is
// the curve the cut ends with and how many edits it took.

const pts = (p: [number, number][]): CurvePoint[] => p.map(([x, y]) => ({ x, y }));

/** A timer set the test advances by hand. */
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
		now: () => now,
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

/** The editor over one clip's master curve. `landing` is how long a write takes to land: `0` is at once, otherwise the test lets it
 *  land with `land()`, so the draft and the cut disagree in between, which is when a double-click's second half arrives. */
function editor(initial: CurvePoint[], opts: { landing?: boolean; busy?: boolean; locked?: () => boolean } = {}) {
	const t = clock();
	let grade: Grade = initial.length ? { curves: { master: initial } } : {};
	const edits: Grade[] = [];
	const pending: (() => void)[] = [];
	const write = gradeWriter({
		gradeOf: () => grade,
		set: (_id, g) =>
			new Promise<void>((resolve) => {
				const apply = () => {
					grade = g;
					edits.push(g);
					resolve();
				};
				if (opts.landing) pending.push(apply);
				else apply();
			}),
		report: (e) => {
			throw e;
		}
	});
	const target: CurveTarget = { clipId: 'c1', channel: 'master' };
	let draft: CurvePoint[] | null = null;
	let selected: number | null = null;
	const said: string[] = [];
	const stored = () => grade.curves?.master ?? [];
	const io: CurveIO = {
		target: () => target,
		points: () => draft ?? (stored().length ? stored() : IDENTITY_POINTS),
		selected: () => selected,
		select: (i) => (selected = i),
		draft: (_t, p) => (draft = p),
		put: (to: CurveTarget, change: PointsChange) => {
			const mine = draft;
			return write(to.clipId, curveChange(to.channel as CurveChannel, change)).then(() => {
				if (draft === mine) draft = null;
			});
		},
		announce: (s) => void said.push(s),
		locked: opts.locked ?? (() => pending.length > 0 && !!opts.busy),
		side: () => 200
	};
	const control = new CurveControl(io, t.now, t.timers);
	const unit = (x: number, y: number) => ({ x, y });
	const flush = async () => {
		for (let i = 0; i < 5; i++) await Promise.resolve();
	};
	return {
		control,
		clock: t,
		edits,
		said,
		stored,
		selected: () => selected,
		draft: () => draft,
		/** The cut's master curve as the model keeps it. */
		curve: () => normalizeCurve(stored()),
		/** The sequence a double-click is: pointerdown, pointerup, pointerdown, pointerup, then dblclick. */
		async doubleClick(x: number, y: number, between = 80) {
			const p = unit(x, y);
			if (control.down(p.x, p.y)) control.up();
			await flush();
			t.advance(between);
			if (control.down(p.x, p.y)) control.up();
			await flush();
			control.doubleClick(p.x, p.y);
			await flush();
		},
		async click(x: number, y: number) {
			if (control.down(x, y)) control.up();
			await flush();
		},
		land: async () => {
			while (pending.length) pending.shift()!();
			await flush();
		},
		flush,
		unit
	};
}

describe('a click on empty space adds one point, a double-click on a point takes it out', () => {
	test('a click adds a point in one edit, written on release, and selects it', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		await e.click(0.6, 0.8);
		expect(e.edits.length).toBe(1);
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [0.6, 0.8], [1, 1]]));
		expect(e.selected()).toBe(2);
	});

	test('a double-click on empty space adds exactly one point and removes nothing (the cut had the first click’s point by then)', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		await e.doubleClick(0.6, 0.8);
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [0.6, 0.8], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('…and when the first click’s write has not landed yet (the draft has a point the cut has not) and the editor is held meanwhile', async () => {
		// the review's case: on [(0,0), (0.3,0.6), (1,1)] a double-click at x = 0.6 wrote [(0,0), (0.3,0.6)]
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]), { landing: true, busy: true });
		await e.doubleClick(0.6, 0.8);
		expect(e.draft()).not.toBeNull();
		await e.land();
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [0.6, 0.8], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('…and when the editor is not held while it lands, so the second click lands on the draft’s point', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]), { landing: true });
		await e.doubleClick(0.6, 0.8);
		await e.land();
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [0.6, 0.8], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('a double-click on empty space of a curve with no points makes a curve of three, not none', async () => {
		const e = editor([]);
		await e.doubleClick(0.5, 0.7);
		expect(e.curve()).toEqual(pts([[0, 0], [0.5, 0.7], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('a double-click on a point that was there takes that point out, by its input, in one edit', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [0.7, 0.8], [1, 1]]));
		await e.doubleClick(0.3, 0.6);
		expect(e.curve()).toEqual(pts([[0, 0], [0.7, 0.8], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('…and the same when a write is still in flight when the second click lands', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [0.7, 0.8], [1, 1]]), { landing: true });
		await e.doubleClick(0.7, 0.8);
		await e.land();
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('a double-click on the end of a curve takes the end out and leaves the others', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [0.7, 0.8], [1, 1]]));
		await e.doubleClick(1, 1);
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [0.7, 0.8]]));
	});

	test('a double-click on the point a click added a while ago takes it out: it is an existing point by then', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		await e.click(0.6, 0.8);
		e.clock.advance(DOUBLE_CLICK_MS + 50);
		await e.doubleClick(0.6, 0.8);
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [1, 1]]));
	});

	test('a double-click on the point just added by a drag stays too: the point is where the drag let go', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.6, 0.8);
		e.control.move(0.65, 0.85);
		e.control.up();
		await e.flush();
		await e.doubleClick(0.65, 0.85);
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [0.65, 0.85], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('a right-click on a point takes it out; on empty space nothing', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [0.7, 0.8], [1, 1]]));
		expect(e.control.contextMenu(0.5, 0.2)).toBe(false);
		expect(e.control.contextMenu(0.3, 0.6)).toBe(true);
		await e.flush();
		expect(e.curve()).toEqual(pts([[0, 0], [0.7, 0.8], [1, 1]]));
		expect(e.edits.length).toBe(1);
	});

	test('a click on a point only selects it, and a drag of it is one edit; Escape during a drag writes nothing and puts the selection back', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.3, 0.6);
		e.control.up();
		await e.flush();
		expect(e.edits.length).toBe(0);
		expect(e.selected()).toBe(1);
		e.control.down(0.3, 0.6);
		for (let i = 1; i <= 20; i++) e.control.move(0.3 + i * 0.01, 0.6);
		e.control.up();
		await e.flush();
		expect(e.edits.length).toBe(1);
		expect(e.curve()[1].x).toBeCloseTo(0.5, 9);
		// abandoned
		const f = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		f.control.down(0.6, 0.8);
		f.control.move(0.7, 0.9);
		f.control.cancel();
		await f.flush();
		expect(f.edits.length).toBe(0);
		expect(f.selected()).toBeNull();
		expect(f.draft()).toBeNull();
	});

	test('an edit is in flight: a press, a double-click and a right-click do nothing', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]), { locked: () => true });
		expect(e.control.down(0.6, 0.8)).toBe(false);
		expect(e.control.doubleClick(0.3, 0.6)).toBe(false);
		expect(e.control.contextMenu(0.3, 0.6)).toBe(false);
		expect(e.edits.length).toBe(0);
	});
});

describe('the live region does not chatter', () => {
	test('a drag of twenty moves says the point twice at most: when it is taken hold of and when it is let go', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.3, 0.6);
		for (let i = 1; i <= 20; i++) e.control.move(0.3 + i * 0.01, 0.6);
		expect(e.said.length).toBe(1);
		e.control.up();
		await e.flush();
		expect(e.said.length).toBe(2);
		expect(e.said[1]).toBe('Point 2 of 3: in 0.50, out 0.60');
	});

	test('a click that adds a point says it once, on release', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.6, 0.8);
		for (let i = 1; i <= 20; i++) e.control.move(0.6, 0.8 + i * 0.005);
		expect(e.said).toEqual([]);
		e.control.up();
		await e.flush();
		expect(e.said.length).toBe(1);
	});
});

/** A keydown as the browser makes it. */
function key(k: string, mods: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; alt: boolean; undo: boolean }> = {}) {
	const log = { prevented: 0, stopped: 0 };
	const ev: CurveKeyEvent = {
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

describe('the curve editor’s keys', () => {
	/** The sequence a keyboard sends for Ctrl+Z: the Control's own keydown, then Z's. */
	const undoSequence = () => [key('Control', { ctrl: true }), key('z', { ctrl: true, undo: true })];

	test('a run of arrows is one edit, written when the keys go quiet', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.3, 0.6);
		e.control.up();
		for (let i = 0; i < 4; i++) e.control.key(key('ArrowRight').ev);
		e.control.key(key('ArrowUp', { shift: true }).ev);
		expect(e.edits.length).toBe(0);
		e.clock.advance(449);
		expect(e.edits.length).toBe(0);
		e.clock.advance(2);
		await e.flush();
		expect(e.edits.length).toBe(1);
		expect(e.curve()[1]).toEqual({ x: 0.34, y: 0.65 });
	});

	test('Ctrl+Z, as the keyboard sends it (Control down, then Z), drops the run: no edit, the canvas restored, and the undo goes no further', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.3, 0.6);
		e.control.up();
		for (let i = 0; i < 3; i++) e.control.key(key('ArrowRight').ev);
		expect(e.draft()).not.toBeNull();
		const [control, z] = undoSequence();
		expect(e.control.key(control.ev)).toBe(false);
		expect(control.log).toEqual({ prevented: 0, stopped: 0 });
		expect(e.control.run.active).toBe(true);
		expect(e.control.key(z.ev)).toBe(true);
		expect(z.log.prevented).toBe(1);
		expect(z.log.stopped).toBe(1);
		e.clock.advance(1000);
		await e.flush();
		expect(e.edits.length).toBe(0);
		expect(e.draft()).toBeNull();
		expect(e.curve()[1]).toEqual({ x: 0.3, y: 0.6 });
	});

	test('with no run, Ctrl+Z is the page’s: the editor stops nothing', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		const seq = undoSequence();
		for (const k of seq) expect(e.control.key(k.ev)).toBe(false);
		expect(seq.every((k) => k.log.prevented === 0 && k.log.stopped === 0)).toBe(true);
	});

	test('a bare Shift or Alt in the middle of a run does not end it, and another shortcut writes it first and goes on', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.3, 0.6);
		e.control.up();
		e.control.key(key('ArrowRight').ev);
		for (const m of ['Shift', 'Alt', 'Control', 'Meta']) expect(e.control.key(key(m, { ctrl: m === 'Control' }).ev)).toBe(false);
		expect(e.control.run.active).toBe(true);
		const save = key('s', { ctrl: true });
		expect(e.control.key(save.ev)).toBe(false);
		await e.flush();
		expect(e.edits.length).toBe(1);
		expect(save.log.stopped).toBe(0);
	});

	test('Escape drops a run; with none it is the page’s', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.3, 0.6);
		e.control.up();
		e.control.key(key('ArrowUp').ev);
		const esc = key('Escape');
		expect(e.control.key(esc.ev)).toBe(true);
		expect(esc.log.stopped).toBe(1);
		await e.flush();
		expect(e.edits.length).toBe(0);
		expect(e.draft()).toBeNull();
		const again = key('Escape');
		expect(e.control.key(again.ev)).toBe(false);
		expect(again.log.stopped).toBe(0);
	});

	test('Delete takes the selected point out by its input, in one edit; Backspace the same', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [0.7, 0.8], [1, 1]]));
		e.control.down(0.3, 0.6);
		e.control.up();
		expect(e.control.key(key('Delete').ev)).toBe(true);
		await e.flush();
		expect(e.curve()).toEqual(pts([[0, 0], [0.7, 0.8], [1, 1]]));
		expect(e.edits.length).toBe(1);
		e.control.key(key('PageDown').ev);
		e.control.key(key('Backspace').ev);
		await e.flush();
		expect(e.curve()).toEqual(pts([[0, 0], [0.7, 0.8]]));
	});

	test('Enter adds a point half-way along the curve; with a run pending it writes the run instead', async () => {
		const e = editor(pts([[0, 0], [0.4, 0.8], [1, 1]]));
		e.control.down(0.4, 0.8);
		e.control.up();
		e.control.key(key('Enter').ev);
		await e.flush();
		expect(e.curve().length).toBe(4);
		expect(e.edits.length).toBe(1);
		e.control.key(key('ArrowRight').ev);
		const enter = key('Enter');
		expect(e.control.key(enter.ev)).toBe(true);
		await e.flush();
		expect(e.edits.length).toBe(2);
		expect(e.curve().length).toBe(4);
	});

	test('a key run lands on the point it began on, by its input, whatever the draft says', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [0.7, 0.8], [1, 1]]));
		e.control.down(0.7, 0.8);
		e.control.up();
		e.control.key(key('ArrowLeft').ev);
		e.control.blur();
		await e.flush();
		expect(e.curve()).toEqual(pts([[0, 0], [0.3, 0.6], [0.69, 0.8], [1, 1]]));
	});
});

describe('the keys a curve owns, held and not', () => {
	const OWN = ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'PageUp', 'PageDown', 'Home', 'End', 'Delete', 'Backspace', 'Enter', 'Insert'];
	const NOT_OWN = [' ', 'j', 'k', 'l', 's', 'c', 'Tab', 'F2'];

	test('while an edit is in flight every key it owns is stopped, so none reaches the page, and none does anything', async () => {
		let locked = true;
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]), { locked: () => locked });
		for (const k of OWN) {
			const ev = key(k);
			expect(e.control.key(ev.ev), k).toBe(true);
			expect(ev.log.prevented, k).toBe(1);
			expect(ev.log.stopped, k).toBe(1);
		}
		locked = false;
		await e.flush();
		expect(e.edits.length).toBe(0);
		expect(e.selected()).toBeNull();
	});

	test('…and during a drag too', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]));
		e.control.down(0.3, 0.6);
		for (const k of ['Delete', 'Backspace', 'End', 'Home', 'PageUp', 'ArrowRight']) {
			const ev = key(k);
			expect(e.control.key(ev.ev), k).toBe(true);
			expect(ev.log.stopped, k).toBe(1);
		}
		e.control.cancel();
		await e.flush();
		expect(e.edits.length).toBe(0);
	});

	test('a key that is not the curve’s goes to the page whether or not the curve is held', () => {
		for (const locked of [false, true]) {
			const e = editor(pts([[0, 0], [0.3, 0.6], [1, 1]]), { locked: () => locked });
			for (const k of NOT_OWN) {
				const ev = key(k);
				expect(e.control.key(ev.ev), k).toBe(false);
				expect(ev.log.stopped, k).toBe(0);
			}
		}
	});

	test('free, it acts on each of its keys and stops each', async () => {
		const e = editor(pts([[0, 0], [0.3, 0.6], [0.7, 0.8], [1, 1]]));
		for (const k of OWN) {
			const ev = key(k);
			expect(e.control.key(ev.ev), k).toBe(true);
			expect(ev.log.stopped, k).toBe(1);
		}
	});
});
