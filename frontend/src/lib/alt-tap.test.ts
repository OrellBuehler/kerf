import { describe, expect, test } from 'bun:test';
import { createAltTap, type AltKey } from './alt-tap';

const key = (k: string, over: Partial<AltKey> = {}): AltKey => ({
	key: k,
	repeat: false,
	ctrlKey: false,
	metaKey: false,
	shiftKey: false,
	altGraph: false,
	...over
});
const ALT = key('Alt');

describe('Alt on its own', () => {
	test('pressed and released with nothing between is a tap', () => {
		const t = createAltTap();
		t.press(ALT);
		expect(t.release('Alt')).toBe(true);
	});

	test('a tap is spent: a second release without a press is not one', () => {
		const t = createAltTap();
		t.press(ALT);
		expect(t.release('Alt')).toBe(true);
		expect(t.release('Alt')).toBe(false);
	});

	test('another key between the press and the release is a chord, not a tap', () => {
		const t = createAltTap();
		t.press(ALT);
		t.press(key('f'));
		expect(t.release('Alt')).toBe(false);
	});

	test('a key other than Alt coming up is never a tap', () => {
		const t = createAltTap();
		t.press(ALT);
		expect(t.release('f')).toBe(false);
	});

	test('auto-repeat, Ctrl / ⌘ / Shift held with it, and AltGr are not Alt on its own', () => {
		for (const over of [{ repeat: true }, { ctrlKey: true }, { metaKey: true }, { shiftKey: true }, { altGraph: true }]) {
			const t = createAltTap();
			t.press(key('Alt', over));
			expect(t.release('Alt'), JSON.stringify(over)).toBe(false);
		}
	});

	test('a press, a wheel turn or a click while it is down cancels it', () => {
		const press = createAltTap();
		press.press(ALT);
		press.pointerDown();
		press.pointerUp();
		expect(press.release('Alt')).toBe(false);
		const wheel = createAltTap();
		wheel.press(ALT);
		wheel.wheel();
		expect(wheel.release('Alt')).toBe(false);
	});
});

describe('Alt during a drag (the timeline reads it live as "leave the links alone")', () => {
	test('pressed and released with the button held is not a tap', () => {
		const t = createAltTap();
		t.pointerDown();
		t.press(ALT);
		expect(t.release('Alt')).toBe(false);
	});

	test('pressed mid-drag and released after the drag ended is not a tap either', () => {
		const t = createAltTap();
		t.pointerDown();
		t.press(ALT);
		t.pointerUp();
		expect(t.release('Alt')).toBe(false);
	});

	test('pressed before the drag and released in it is not a tap', () => {
		const t = createAltTap();
		t.press(ALT);
		t.pointerDown();
		expect(t.release('Alt')).toBe(false);
	});

	test('a release that was never seen does not leave the pointer held for good', () => {
		const t = createAltTap();
		t.pointerDown(); // released outside the window: no pointerup arrives
		t.pointerMove(0);
		t.press(ALT);
		expect(t.release('Alt')).toBe(true);
	});

	test('a move with a button down keeps it held, and cancels an armed Alt', () => {
		const t = createAltTap();
		t.press(ALT);
		t.pointerMove(1);
		expect(t.release('Alt')).toBe(false);
		t.pointerMove(1);
		t.press(ALT);
		expect(t.release('Alt')).toBe(false);
	});

	test('a plain tap works again once the drag is over', () => {
		const t = createAltTap();
		t.pointerDown();
		t.press(ALT);
		t.release('Alt');
		t.pointerUp();
		t.press(ALT);
		expect(t.release('Alt')).toBe(true);
	});
});

describe('Alt across a change of window', () => {
	test('Alt held while the window loses focus (Alt+Tab) is not a tap on the way back', () => {
		const t = createAltTap();
		t.press(ALT);
		t.blur();
		expect(t.release('Alt')).toBe(false);
	});

	test('a blur also forgets a held pointer', () => {
		const t = createAltTap();
		t.pointerDown();
		t.blur();
		t.press(ALT);
		expect(t.release('Alt')).toBe(true);
	});
});
