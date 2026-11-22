import { describe, expect, test } from 'bun:test';
import { StepKeys, type StepIO, type StepKeyEvent } from './step-keys';
import { sliderKey } from './slider-gesture';
import { stepBy, type RowSpec } from './slider-row';
import type { Timers } from './key-run';

// The mixer slider's and the grade's number boxes' keys, as a keyboard sends them.

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

const SPEC: RowSpec = { min: -1, max: 1, step: 0.01, neutral: 0, digits: 2 };

/** A slider row: the committed value, what is shown, and what has been written (revisions). */
function control(opts: { dragging?: boolean } = {}) {
	const t = clock();
	let committed = 0;
	let shown: number = committed;
	const revisions: number[] = [];
	const abandoned: number[] = [];
	const io: StepIO = {
		owns: (k) => sliderKey(k) !== null,
		step: (from, e) => stepBy(from, sliderKey(e.key, { shift: e.shiftKey, alt: e.altKey })!, SPEC),
		value: () => committed,
		start: () => committed,
		changed: (a, b) => Math.abs(a - b) > 1e-4,
		preview: (v) => (shown = v),
		commit: (v) => {
			committed = v;
			shown = v;
			revisions.push(v);
		},
		abandon: () => {
			shown = committed;
			abandoned.push(committed);
		},
		held: () => !!opts.dragging
	};
	const keys = new StepKeys(io, t.timers);
	return { keys, clock: t, revisions, abandoned, shown: () => shown, committed: () => committed };
}

function key(k: string, mods: Partial<{ ctrl: boolean; meta: boolean; shift: boolean; alt: boolean; undo: boolean }> = {}) {
	const log = { prevented: 0, stopped: 0 };
	const ev: StepKeyEvent = {
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

describe('a run of keys on a number', () => {
	test('is one edit, written when the keys go quiet', () => {
		const c = control();
		for (let i = 0; i < 4; i++) c.keys.key(key('ArrowRight').ev);
		expect(c.shown()).toBe(0.04);
		expect(c.revisions).toEqual([]);
		c.clock.advance(460);
		expect(c.revisions).toEqual([0.04]);
	});

	test('Ctrl+Z, as the keyboard sends it (Control down, then Z): zero revisions, the display restored, and no undo goes on', () => {
		const c = control();
		for (let i = 0; i < 3; i++) c.keys.key(key('ArrowRight').ev);
		expect(c.shown()).toBe(0.03);
		const control_ = key('Control', { ctrl: true });
		const z = key('z', { ctrl: true, undo: true });
		expect(c.keys.key(control_.ev)).toBe(false);
		expect(control_.log).toEqual({ prevented: 0, stopped: 0 });
		expect(c.keys.run.active).toBe(true);
		expect(c.keys.key(z.ev)).toBe(true);
		expect(z.log).toEqual({ prevented: 1, stopped: 1 });
		c.clock.advance(1000);
		expect(c.revisions).toEqual([]);
		expect(c.shown()).toBe(0);
		expect(c.abandoned.length).toBe(1);
	});

	test('on a Mac the sequence is Meta then Z, and the user’s own undo key is whatever the keymap says', () => {
		const c = control();
		c.keys.key(key('ArrowRight').ev);
		expect(c.keys.key(key('Meta', { meta: true }).ev)).toBe(false);
		const z = key('z', { meta: true, undo: true });
		expect(c.keys.key(z.ev)).toBe(true);
		expect(c.shown()).toBe(0);
		// a Z that is not undo any more is another shortcut: it writes the run and goes on
		const d = control();
		d.keys.key(key('ArrowRight').ev);
		const other = key('z', { ctrl: true, undo: false });
		expect(d.keys.key(other.ev)).toBe(false);
		expect(d.revisions).toEqual([0.01]);
		expect(other.log.stopped).toBe(0);
	});

	test('with no run, undo is the page’s and the control stops nothing', () => {
		const c = control();
		const seq = [key('Control', { ctrl: true }), key('z', { ctrl: true, undo: true })];
		for (const k of seq) expect(c.keys.key(k.ev)).toBe(false);
		expect(seq.every((k) => k.log.prevented === 0 && k.log.stopped === 0)).toBe(true);
	});

	test('a bare Shift or Alt held for a bigger or finer step does not end the run', () => {
		const c = control();
		c.keys.key(key('ArrowRight').ev);
		for (const m of ['Shift', 'Alt']) expect(c.keys.key(key(m).ev)).toBe(false);
		expect(c.keys.run.active).toBe(true);
		c.keys.key(key('ArrowRight', { shift: true }).ev);
		c.clock.advance(460);
		expect(c.revisions).toEqual([0.06]);
	});

	test('Escape drops the run, and with none it is the page’s', () => {
		const c = control();
		c.keys.key(key('ArrowRight').ev);
		const esc = key('Escape');
		expect(c.keys.key(esc.ev)).toBe(true);
		expect(esc.log.stopped).toBe(1);
		expect(c.shown()).toBe(0);
		const again = key('Escape');
		expect(c.keys.key(again.ev)).toBe(false);
		expect(again.log.stopped).toBe(0);
	});

	test('Enter writes it now, and a run that ends where it began writes nothing', () => {
		const c = control();
		c.keys.key(key('ArrowRight').ev);
		expect(c.keys.key(key('Enter').ev)).toBe(true);
		expect(c.revisions).toEqual([0.01]);
		c.keys.key(key('ArrowRight').ev);
		c.keys.key(key('ArrowLeft').ev);
		c.keys.blur();
		expect(c.revisions).toEqual([0.01]);
		expect(c.shown()).toBe(0.01);
	});
});

describe('the keys a slider or a number box owns', () => {
	test('during a drag each is stopped and none does anything; the page’s keys still go on', () => {
		const c = control({ dragging: true });
		for (const k of ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'PageUp', 'PageDown', 'Home', 'End']) {
			const ev = key(k);
			expect(c.keys.key(ev.ev), k).toBe(true);
			expect(ev.log, k).toEqual({ prevented: 1, stopped: 1 });
		}
		for (const k of [' ', 'j', 'Delete', 'Backspace', 'Tab']) {
			const ev = key(k);
			expect(c.keys.key(ev.ev), k).toBe(false);
			expect(ev.log.stopped, k).toBe(0);
		}
		c.clock.advance(1000);
		expect(c.revisions).toEqual([]);
	});
});
