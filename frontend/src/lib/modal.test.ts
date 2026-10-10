import { describe, expect, test } from 'bun:test';
import { focusLost, returnTarget } from './modal';

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
