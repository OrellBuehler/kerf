import { describe, expect, test } from 'bun:test';
import { DRAG_SLOP, grabOffset, isDrag, moved, positionAt, sliderKey } from './slider-gesture';

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

describe('a click is not a drag', () => {
	test('a wobble inside the slop is a click: a double-click to reset is two of them, and writes one edit', () => {
		expect(DRAG_SLOP).toBeGreaterThanOrEqual(4);
		expect(isDrag(0, 0)).toBe(false);
		expect(isDrag(3, 0)).toBe(false);
		expect(isDrag(2, 2)).toBe(false);
		expect(isDrag(0, -3.9)).toBe(false);
	});

	test('past it is a drag, whichever way it goes', () => {
		expect(isDrag(0, 4)).toBe(true);
		expect(isDrag(-4, 0)).toBe(true);
		expect(isDrag(3, 3)).toBe(true);
	});
});

describe('a drag begins where the press found the value', () => {
	const ext = { start: 100, size: 200 };

	test('the first move past the slop does not move the value: the grab is taken up at the pointer where the slop was crossed', () => {
		for (const vertical of [true, false]) {
			const from = 0.4;
			// The press, with the thumb's centre a few px from the pointer; then the pointer moves by the slop and the drag begins.
			const pressAt = 180;
			const grab0 = grabOffset(pressAt, ext, vertical, from);
			const crossedAt = pressAt + (vertical ? DRAG_SLOP : -DRAG_SLOP) * (vertical ? 1 : -1);
			// What it did: the value followed the pointer from the press's grab, so it was already a slop's worth away from `from`.
			expect(Math.abs(positionAt(crossedAt, ext, vertical, grab0) - from)).toBeCloseTo(DRAG_SLOP / ext.size, 9);
			// What it does: the grab is taken at the crossing, so the value there is exactly where it was.
			const grab = grabOffset(crossedAt, ext, vertical, from);
			expect(positionAt(crossedAt, ext, vertical, grab)).toBeCloseTo(from, 12);
			// And it goes on from there one px of pointer for one px of travel.
			const next = positionAt(crossedAt + (vertical ? -1 : 1), ext, vertical, grab);
			expect(Math.abs(next - from)).toBeCloseTo(1 / ext.size, 9);
		}
	});
});
