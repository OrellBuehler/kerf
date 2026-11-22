import { describe, expect, test } from 'bun:test';
import { DRAG_THRESHOLD, contentX, dropIndex, edgeNudge, fadeMask, fadeOf, markerX, pastThreshold } from './tab-reorder';

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

describe('a strip that has scrolled', () => {
	test('the pointer and the buttons are read in the content’s coordinates, so a drag is right however far it has scrolled', () => {
		// The strip starts at x=100 on the screen and has scrolled 300 px: a button whose content spans 400–460 is at 200–260.
		const screenSpan = { left: 200, right: 260 };
		const span = { left: contentX(screenSpan.left, 100, 300), right: contentX(screenSpan.right, 100, 300) };
		expect(span).toEqual({ left: 400, right: 460 });
		expect(contentX(230, 100, 300)).toBe(430);
		// The middle of that button is where a drop before / after it is decided, scrolled or not.
		const others = [{ left: 0, right: 60 }, span, { left: 464, right: 524 }];
		expect(dropIndex(others, contentX(225, 100, 300))).toBe(1);
		expect(dropIndex(others, contentX(235, 100, 300))).toBe(2);
		// Unscrolled it is what it was.
		expect(contentX(230, 100, 0)).toBe(130);
	});

	test('the marker is drawn at a content x, which is the same whatever the scroll', () => {
		const others = [{ left: 0, right: 60 }, { left: 400, right: 460 }];
		expect(markerX(others, 1)).toBe(400);
	});

	test('a drag near an edge scrolls toward it, and only there', () => {
		const strip = { left: 100, right: 500 };
		expect(edgeNudge(110, strip)).toBe(-14);
		expect(edgeNudge(127, strip)).toBe(-14);
		expect(edgeNudge(128, strip)).toBe(0);
		expect(edgeNudge(300, strip)).toBe(0);
		expect(edgeNudge(472, strip)).toBe(0);
		expect(edgeNudge(473, strip)).toBe(14);
		expect(edgeNudge(900, strip)).toBe(14);
		expect(edgeNudge(110, strip, 10, 5)).toBe(0);
		expect(edgeNudge(104, strip, 10, 5)).toBe(-5);
	});

	test('the ends fade where there is more: left once scrolled, right until the end, neither when it all fits', () => {
		expect(fadeOf(0, 300, 300)).toEqual({ left: false, right: false });
		expect(fadeOf(0, 300, 301)).toEqual({ left: false, right: false });
		expect(fadeOf(0, 300, 302)).toEqual({ left: false, right: true });
		expect(fadeOf(1, 300, 302)).toEqual({ left: false, right: false });
		expect(fadeOf(2, 300, 600)).toEqual({ left: true, right: true });
		expect(fadeOf(300, 300, 600)).toEqual({ left: true, right: false });
		// A fractional scroll position at the very end is the end.
		expect(fadeOf(299.6, 300, 600)).toEqual({ left: true, right: false });
	});

	test('the mask fades only the ends that go on, in a colour of the theme and not a literal', () => {
		expect(fadeMask({ left: false, right: false })).toBe('');
		const both = fadeMask({ left: true, right: true });
		expect(both).toContain('transparent 0, var(--surface-app) 28px');
		expect(both).toContain('var(--surface-app) calc(100% - 28px), transparent 100%');
		expect(both).toContain('-webkit-mask-image:');
		const right = fadeMask({ left: false, right: true });
		expect(right).toContain('var(--surface-app) 0,');
		expect(right).not.toContain('transparent 0');
		const left = fadeMask({ left: true, right: false });
		expect(left).toContain('var(--surface-app) 100%');
		expect(left).not.toContain('transparent 100%');
		expect(fadeMask({ left: true, right: true }, 12)).toContain('12px');
		expect(both).not.toMatch(/#[0-9a-f]{3,8}|rgb\(|hsl\(/i);
	});
});
