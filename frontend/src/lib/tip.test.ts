import { describe, expect, test } from 'bun:test';
import {
	LEAVE_GRACE,
	SHOW_DELAY,
	WARM_DELAY,
	WARM_WINDOW,
	createTipController,
	needsName,
	placeTip,
	tipText,
	toTip,
	type TipSpec,
	type Via
} from './tip';
import { accelParts } from './accel';

describe('what a tip says', () => {
	test('a string is its label; the lines after the first are the detail', () => {
		expect(toTip('Razor')).toEqual({ text: 'Razor' });
		expect(toTip('Ripple on\nA trim pulls the later clips along.')).toEqual({
			text: 'Ripple on',
			detail: 'A trim pulls the later clips along.'
		});
		expect(toTip('  Zoom in  ')).toEqual({ text: 'Zoom in' });
		expect(toTip('Drawn by FFmpeg\n  an effect it does not draw\n\n  a transition ')).toEqual({
			text: 'Drawn by FFmpeg',
			detail: 'an effect it does not draw\na transition'
		});
	});

	test('nothing to say is no tip', () => {
		for (const nothing of [undefined, null, false, '', '   ', '\nonly detail', { text: '' }, { text: '  ' }])
			expect(toTip(nothing as never)).toBeNull();
	});

	test('a spec keeps its keys and detail, trimmed, and drops empty ones', () => {
		expect(toTip({ text: 'Undo', keys: ' Ctrl+Z ', detail: '' })).toEqual({ text: 'Undo', keys: 'Ctrl+Z' });
	});

	test('the plain text is the label with its keys, for a screen reader', () => {
		expect(tipText({ text: 'Undo', keys: 'Ctrl+Z', detail: 'x' })).toBe('Undo (Ctrl+Z)');
		expect(tipText('Close')).toBe('Close');
		expect(tipText(undefined)).toBe('');
	});
});

describe('an element takes the tip as its name only when it has none', () => {
	const base = { interactive: true, text: '', ariaLabel: false, labelled: false };
	test('an icon button with nothing else does', () => {
		expect(needsName(base)).toBe(true);
	});
	test('text, an aria-label or a label already name it', () => {
		expect(needsName({ ...base, text: 'Export' })).toBe(false);
		expect(needsName({ ...base, ariaLabel: true })).toBe(false);
		expect(needsName({ ...base, labelled: true })).toBe(false);
	});
	test('a span or a div is not named by a tooltip', () => {
		expect(needsName({ ...base, interactive: false })).toBe(false);
	});
});

describe('where a tip goes', () => {
	const view = { width: 1000, height: 600 };
	const tip = { width: 120, height: 28 };
	test('centred under its element', () => {
		expect(placeTip({ left: 100, top: 100, width: 40, height: 24 }, tip, view)).toEqual({ left: 60, top: 130, side: 'below' });
	});
	test('over it when there is no room beneath', () => {
		const at = placeTip({ left: 100, top: 570, width: 40, height: 24 }, tip, view);
		expect(at.side).toBe('above');
		expect(at.top).toBe(570 - 6 - 28);
	});
	test('kept inside the window at either edge', () => {
		expect(placeTip({ left: 0, top: 100, width: 20, height: 20 }, tip, view).left).toBe(6);
		expect(placeTip({ left: 990, top: 100, width: 10, height: 20 }, tip, view).left).toBe(1000 - 120 - 6);
	});
	test('a tip taller than the room either way still lands on screen', () => {
		const tall = { width: 120, height: 580 };
		const at = placeTip({ left: 100, top: 300, width: 40, height: 24 }, tall, view);
		expect(at.top).toBeGreaterThanOrEqual(6);
		expect(at.top + 580).toBeLessThanOrEqual(600 - 6 + 580);
	});
});

describe('shortcuts as keycaps', () => {
	test('modifiers stay as printed and the key stands alone', () => {
		expect(accelParts('Ctrl+Shift+Z')).toEqual({ head: 'Ctrl+Shift+', key: 'Z', cap: false, arrow: false });
		expect(accelParts('⇧⌘Z')).toEqual({ head: '⇧⌘', key: 'Z', cap: false, arrow: false });
		expect(accelParts('Space')).toEqual({ head: '', key: 'Space', cap: false, arrow: false });
	});
	test('a punctuation key is a keycap, whatever precedes it', () => {
		expect(accelParts(',')).toMatchObject({ head: '', key: ',', cap: true });
		expect(accelParts('Ctrl+,')).toMatchObject({ head: 'Ctrl+', key: ',', cap: true });
		expect(accelParts('Ctrl++')).toMatchObject({ head: 'Ctrl+', key: '+', cap: true });
		expect(accelParts('+')).toMatchObject({ head: '', key: '+', cap: true });
		expect(accelParts('-')).toMatchObject({ key: '-', cap: true });
		expect(accelParts('⌘,')).toMatchObject({ head: '⌘', key: ',', cap: true });
	});
	test('an arrow is drawn larger, not capped', () => {
		expect(accelParts('Shift+←')).toEqual({ head: 'Shift+', key: '←', cap: false, arrow: true });
		expect(accelParts('⇧→')).toEqual({ head: '⇧', key: '→', cap: false, arrow: true });
	});
});

// ---- the timing ---------------------------------------------------------------------

function rig() {
	let t = 0;
	const timers = new Map<number, { at: number; fn: () => void }>();
	let n = 0;
	const log: string[] = [];
	const ctl = createTipController<string>({
		now: () => t,
		after: (fn, ms) => {
			const id = ++n;
			timers.set(id, { at: t + ms, fn });
			return id;
		},
		cancel: (h) => void timers.delete(h as number),
		show: (el, spec: TipSpec, via: Via) => log.push(`show ${el} ${spec.text} ${via}`),
		hide: () => log.push('hide')
	});
	const advance = (ms: number) => {
		const to = t + ms;
		for (;;) {
			const due = [...timers.entries()].filter(([, v]) => v.at <= to).sort((a, b) => a[1].at - b[1].at)[0];
			if (!due) break;
			t = due[1].at;
			timers.delete(due[0]);
			due[1].fn();
		}
		t = to;
	};
	return { ctl, log, advance, timers: () => timers.size };
}

describe('the controller: pointer', () => {
	test('shows after the delay and not before', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY - 1);
		expect(log).toEqual([]);
		advance(1);
		expect(log).toEqual(['show a A pointer']);
	});

	test('leaving before it shows cancels it', () => {
		const { ctl, log, advance, timers } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(100);
		ctl.leave('a');
		advance(1000);
		expect(log).toEqual([]);
		expect(timers()).toBe(0);
	});

	test('leaving hides it after a grace, so the pointer can cross onto the tip', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		ctl.leave('a');
		advance(LEAVE_GRACE - 1);
		expect(log).toEqual(['show a A pointer']);
		advance(1);
		expect(log).toEqual(['show a A pointer', 'hide']);
	});

	test('moving to the next control while one shows swaps at once, with no hide between', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.set('b', { text: 'B' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		ctl.leave('a');
		advance(20);
		ctl.enter('b');
		advance(0);
		expect(log).toEqual(['show a A pointer', 'show b B pointer']);
		advance(1000);
		expect(log).toEqual(['show a A pointer', 'show b B pointer']);
	});

	test('just after one has gone the next is quick, a while after it is not', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.set('b', { text: 'B' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		ctl.leave('a');
		advance(LEAVE_GRACE);
		ctl.enter('b');
		advance(WARM_DELAY);
		expect(log.at(-1)).toBe('show b B pointer');
		ctl.leave('b');
		advance(LEAVE_GRACE + WARM_WINDOW + 1);
		log.length = 0;
		ctl.enter('a');
		advance(WARM_DELAY);
		expect(log).toEqual([]);
		advance(SHOW_DELAY);
		expect(log).toEqual(['show a A pointer']);
	});

	test('a press on the control hides its tip and keeps it away until the pointer has left', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		ctl.press('a');
		expect(log).toEqual(['show a A pointer', 'hide']);
		advance(5000);
		ctl.enter('a');
		advance(5000);
		expect(log).toEqual(['show a A pointer', 'hide']);
		ctl.leave('a');
		ctl.enter('a');
		advance(SHOW_DELAY);
		expect(log.at(-1)).toBe('show a A pointer');
	});

	test('a press before it showed means it never does', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(100);
		ctl.press('a');
		advance(5000);
		expect(log).toEqual([]);
	});

	test('an element with nothing to say never shows, and one that loses it takes its tip down', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', null);
		ctl.enter('a');
		advance(5000);
		expect(log).toEqual([]);
		ctl.set('b', { text: 'B' });
		ctl.enter('b');
		advance(SHOW_DELAY);
		ctl.set('b', null);
		expect(log).toEqual(['show b B pointer', 'hide']);
	});

	test('changing what it says while it shows redraws it', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'Snapping on' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		ctl.set('a', { text: 'Snapping off' });
		expect(log).toEqual(['show a Snapping on pointer', 'show a Snapping off pointer']);
	});

	test('an element removed while its tip shows takes it down', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		ctl.drop('a');
		expect(log).toEqual(['show a A pointer', 'hide']);
		expect(ctl.shown).toBe(false);
	});
});

describe('the controller: keyboard', () => {
	test('focus from the keyboard shows at once; a click that focuses does not', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.focus('a', false);
		advance(5000);
		expect(log).toEqual([]);
		ctl.blur('a');
		ctl.focus('a', true);
		advance(0);
		expect(log).toEqual(['show a A focus']);
	});

	test('blur hides it', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.focus('a', true);
		advance(0);
		ctl.blur('a');
		expect(log).toEqual(['show a A focus', 'hide']);
	});

	test('Escape hides it and, for a focused control, is not passed on', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.focus('a', true);
		advance(0);
		expect(ctl.escape()).toEqual({ hidden: true, consume: true });
		expect(log).toEqual(['show a A focus', 'hide']);
		ctl.focus('a', true);
		advance(1000);
		expect(log).toEqual(['show a A focus', 'hide']);
	});

	test('Escape over a tip only the pointer holds up goes on to whatever wanted it', () => {
		const { ctl, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		expect(ctl.escape()).toEqual({ hidden: true, consume: false });
	});

	test('Escape with no tip up is nothing', () => {
		const { ctl } = rig();
		expect(ctl.escape()).toEqual({ hidden: false, consume: false });
	});

	test('leaving with the pointer does not take down a tip the keyboard is holding', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.focus('a', true);
		advance(0);
		ctl.enter('a');
		ctl.leave('a');
		advance(5000);
		expect(log).toEqual(['show a A focus']);
	});

	test('a scroll or a press elsewhere takes down whatever is up', () => {
		const { ctl, log, advance } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		ctl.dismissAll();
		expect(log).toEqual(['show a A pointer', 'hide']);
	});
});
