import { describe, expect, test } from 'bun:test';
import { SHOW_DELAY, createTipController, holdTips, type TipSpec, type Via } from './tip';

// An open context menu holds every tooltip back: the one its own info row raised sat above the menu and
// covered the label of the row beside it (the clip menu's "Analyzed 3 of 5" hid "Analyze").

function rig() {
	let t = 0;
	let blocked = false;
	const timers = new Map<number, { at: number; fn: () => void }>();
	let n = 0;
	const log: string[] = [];
	const ctl = createTipController<string>({
		blocked: () => blocked,
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
	return { ctl, log, advance, timers: () => timers.size, block: (on: boolean) => (blocked = on) };
}

describe('tips while a context menu is open', () => {
	test('none is scheduled, for the pointer or the keyboard', () => {
		const { ctl, log, advance, timers, block } = rig();
		ctl.set('a', { text: 'A' });
		block(true);
		ctl.enter('a');
		ctl.focus('a', true);
		expect(timers()).toBe(0);
		advance(SHOW_DELAY * 3);
		expect(log).toEqual([]);
	});

	test('one that is up goes when the hold starts, and tips come back after it', () => {
		const { ctl, log, advance, block } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY);
		expect(log).toEqual(['show a A pointer']);
		block(true);
		ctl.hold();
		expect(log).toEqual(['show a A pointer', 'hide']);
		ctl.leave('a');
		block(false);
		ctl.enter('a');
		advance(SHOW_DELAY);
		expect(log.at(-1)).toBe('show a A pointer');
	});

	test('one that was only waiting its delay is dropped by the hold', () => {
		const { ctl, log, advance, block } = rig();
		ctl.set('a', { text: 'A' });
		ctl.enter('a');
		advance(SHOW_DELAY - 10);
		block(true);
		ctl.hold();
		advance(SHOW_DELAY);
		expect(log).toEqual([]);
	});

	test('the module’s switch is idempotent and safe with nothing showing', () => {
		holdTips(true);
		holdTips(true);
		holdTips(false);
		holdTips(false);
	});
});
