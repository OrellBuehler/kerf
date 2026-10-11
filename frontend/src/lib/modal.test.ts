import { describe, expect, test } from 'bun:test';
import { focusLost, mayClose, returnTarget, scrimClick } from './modal';

const control = (isConnected = true) => ({ isConnected, focus() {} });
const body = { isConnected: true, focus() {} };

describe('returnTarget', () => {
	test('gives focus back to the control that had it, while it is still there', () => {
		const gear = control();
		expect(returnTarget(gear, body, () => null)).toBe(gear);
	});

	test('a control that has gone (the menu entry that opened the dialog) falls back', () => {
		const tab = control();
		expect(returnTarget(control(false), body, () => tab)).toBe(tab);
	});

	test('so does the page itself: body is where focus was, not a place to return to', () => {
		const tab = control();
		expect(returnTarget(body, body, () => tab)).toBe(tab);
		expect(returnTarget(null, body, () => tab)).toBe(tab);
	});

	test('with nothing to fall back to there is nowhere to go', () => {
		expect(returnTarget(body, body, () => null)).toBeNull();
	});

	test('the fallback is only looked up when it is needed', () => {
		let looked = 0;
		returnTarget(control(), body, () => (looked++, null));
		expect(looked).toBe(0);
	});
});

describe('focusLost', () => {
	const root = { id: 'dialog' };
	test('nothing focused, or the page, wants a control', () => {
		expect(focusLost(null, body, root)).toBe(true);
		expect(focusLost(body, body, root)).toBe(true);
	});

	test('a control anywhere is where the user put it', () => {
		expect(focusLost({ id: 'cancel' }, body, root)).toBe(false);
		expect(focusLost({ id: 'cancel' }, body, root, true)).toBe(false);
	});

	test('the root is left alone, unless a rescue parked focus there and has a control to give it now', () => {
		expect(focusLost(root, body, root)).toBe(false);
		expect(focusLost(root, body, root, true)).toBe(true);
	});
});

describe('mayClose', () => {
	test('Escape, the close button and the scrim all close an ordinary dialog', () => {
		for (const via of ['escape', 'scrim', 'button'] as const) expect(mayClose(via)).toBe(true);
	});

	test('a dialog that holds work sets the scrim aside and keeps Escape and the button', () => {
		const rules = { dismissOnScrim: false };
		expect(mayClose('scrim', rules)).toBe(false);
		expect(mayClose('escape', rules)).toBe(true);
		expect(mayClose('button', rules)).toBe(true);
	});

	test('a busy dialog closes by none of them, whatever the scrim policy', () => {
		for (const via of ['escape', 'scrim', 'button'] as const) {
			expect(mayClose(via, { busy: true })).toBe(false);
			expect(mayClose(via, { busy: true, dismissOnScrim: true })).toBe(false);
			expect(mayClose(via, { busy: true, dismissOnScrim: false })).toBe(false);
		}
	});
});

describe('scrimClick', () => {
	const scrim = { id: 'scrim' };
	const field = { id: 'field' };

	test('a press and a click that both land on the scrim are a click on it', () => {
		expect(scrimClick(true, scrim, scrim)).toBe(true);
	});

	test('a click that lands on the dialog itself is not', () => {
		expect(scrimClick(false, field, scrim)).toBe(false);
		expect(scrimClick(true, field, scrim)).toBe(false);
	});

	test('a press inside that is released over the scrim is not: the click only lands there', () => {
		expect(scrimClick(false, scrim, scrim)).toBe(false);
	});
});
