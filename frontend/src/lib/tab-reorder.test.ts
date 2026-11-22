import { describe, expect, test } from 'bun:test';
import { DRAG_THRESHOLD, dropIndex, markerX, pastThreshold } from './tab-reorder';

// Four buttons 60 wide, 4 apart: 0–60, 64–124, 128–188, 192–252. The one being dragged is not among them.
const others = [
	{ left: 0, right: 60 },
	{ left: 64, right: 124 },
	{ left: 128, right: 188 }
];

describe('when a press is a drag', () => {
	test('only after the pointer has gone a few pixels, in any direction', () => {
		expect(pastThreshold(0, 0)).toBe(false);
		expect(pastThreshold(DRAG_THRESHOLD - 1, 0)).toBe(false);
		expect(pastThreshold(DRAG_THRESHOLD, 0)).toBe(true);
		expect(pastThreshold(0, -DRAG_THRESHOLD)).toBe(true);
		expect(pastThreshold(4, 4)).toBe(true);
		expect(pastThreshold(3, 3)).toBe(false);
	});
});

describe('where a dragged button lands', () => {
	test('before the first button whose middle is right of the pointer', () => {
		expect(dropIndex(others, -40)).toBe(0);
		expect(dropIndex(others, 29)).toBe(0);
		expect(dropIndex(others, 31)).toBe(1);
		expect(dropIndex(others, 93)).toBe(1);
		expect(dropIndex(others, 95)).toBe(2);
	});

	test('last once the pointer is past the middle of the last one', () => {
		expect(dropIndex(others, 157)).toBe(2);
		expect(dropIndex(others, 159)).toBe(3);
		expect(dropIndex(others, 900)).toBe(3);
	});

	test('with no other button there is one place', () => {
		expect(dropIndex([], 10)).toBe(0);
	});

	test('the middle itself is not past it', () => {
		expect(dropIndex(others, 30)).toBe(0);
	});
});

describe('where the marker is drawn', () => {
	test('at the left edge of the button it lands before, and the right edge of the last when it lands last', () => {
		expect(markerX(others, 0)).toBe(0);
		expect(markerX(others, 1)).toBe(64);
		expect(markerX(others, 2)).toBe(128);
		expect(markerX(others, 3)).toBe(188);
		expect(markerX([], 0)).toBeNull();
	});
});
