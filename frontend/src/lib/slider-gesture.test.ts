import { describe, expect, test } from 'bun:test';
import { grabOffset, moved, positionAt, sliderKey } from './slider-gesture';

describe('positionAt', () => {
	const v = { start: 100, size: 200 }; // a vertical slider from y=100 (top) to y=300 (bottom)
	const h = { start: 50, size: 100 };

	test('a vertical slider is fullest at the top', () => {
		expect(positionAt(300, v, true)).toBe(0);
		expect(positionAt(100, v, true)).toBe(1);
		expect(positionAt(200, v, true)).toBe(0.5);
	});

	test('a horizontal slider is fullest on the right', () => {
		expect(positionAt(50, h, false)).toBe(0);
		expect(positionAt(150, h, false)).toBe(1);
		expect(positionAt(75, h, false)).toBe(0.25);
	});

	test('clamps past either end, and ignores a collapsed track', () => {
		expect(positionAt(-500, v, true)).toBe(1);
		expect(positionAt(900, v, true)).toBe(0);
		expect(positionAt(10, h, false)).toBe(0);
		expect(positionAt(999, h, false)).toBe(1);
		expect(positionAt(10, { start: 0, size: 0 }, true)).toBe(0);
		expect(positionAt(10, { start: 0, size: Number.NaN }, true)).toBe(0);
	});

	test('a drag moves the thumb by the pointer’s travel, not under its centre', () => {
		// The thumb is at 0.5 (y=200); the pointer takes hold 6 px below its centre.
		const grab = grabOffset(206, v, true, 0.5);
		expect(grab).toBe(6);
		// Taking hold changes nothing…
		expect(positionAt(206, v, true, grab)).toBe(0.5);
		// …and moving 20 px up moves it 20 px up.
		expect(positionAt(186, v, true, grab)).toBeCloseTo(0.6, 12);
		// The same on a horizontal slider.
		const g = grabOffset(105, h, false, 0.5);
		expect(g).toBe(-5); // the pointer is 5 px right of the centre
		expect(positionAt(105, h, false, g)).toBe(0.5);
		expect(positionAt(115, h, false, g)).toBeCloseTo(0.6, 12);
	});
});

describe('sliderKey', () => {
	test('arrows step — up and right are more — and the modifiers size the step', () => {
		expect(sliderKey('ArrowUp')).toEqual({ kind: 'step', dir: 1, size: 'normal' });
		expect(sliderKey('ArrowRight')).toEqual({ kind: 'step', dir: 1, size: 'normal' });
		expect(sliderKey('ArrowDown')).toEqual({ kind: 'step', dir: -1, size: 'normal' });
		expect(sliderKey('ArrowLeft')).toEqual({ kind: 'step', dir: -1, size: 'normal' });
		expect(sliderKey('ArrowUp', { shift: true })).toEqual({ kind: 'step', dir: 1, size: 'coarse' });
		expect(sliderKey('ArrowDown', { alt: true })).toEqual({ kind: 'step', dir: -1, size: 'fine' });
		// Fine wins over coarse when both are held: precision is the one asked for on purpose.
		expect(sliderKey('ArrowUp', { shift: true, alt: true })).toEqual({ kind: 'step', dir: 1, size: 'fine' });
	});

	test('page keys take the biggest step, and Home / End go to the ends', () => {
		expect(sliderKey('PageUp')).toEqual({ kind: 'step', dir: 1, size: 'page' });
		expect(sliderKey('PageDown', { shift: true })).toEqual({ kind: 'step', dir: -1, size: 'page' });
		expect(sliderKey('Home')).toEqual({ kind: 'edge', to: 0 });
		expect(sliderKey('End')).toEqual({ kind: 'edge', to: 1 });
	});

	test('anything else is not the slider’s', () => {
		for (const k of ['a', 'Enter', 'Escape', ' ', 'Tab', 'Delete', 'j', 'k', 'l']) expect(sliderKey(k)).toBeNull();
	});
});

describe('moved', () => {
	test('a click on the thumb or a wobble is not an edit', () => {
		expect(moved(0.5, 0.5)).toBe(false);
		expect(moved(0.5, 0.50005)).toBe(false);
		expect(moved(0.5, 0.501)).toBe(true);
	});
});
