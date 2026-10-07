import { describe, expect, test } from 'bun:test';
import {
	CLIP_INSET_PX,
	MIN_CLIP_PX,
	marqueeHits,
	normalizeRect,
	rectsTouch,
	type LaneBox,
	type SpanClip
} from './marquee';

const PX = 10; // px per second

// V1 and A1 as the timeline lays them out: 56 px lanes stacked from y = 40.
const lanes: LaneBox[] = [
	{ trackId: 'v1', top: 40, height: 56 },
	{ trackId: 'a1', top: 96, height: 56 }
];
const clips = new Map<string, SpanClip[]>([
	[
		'v1',
		[
			{ id: 'c1', start: 0, end: 10 },
			{ id: 'c2', start: 10, end: 20 },
			{ id: 'c4', start: 40, end: 50 }
		]
	],
	['a1', [{ id: 'c3', start: 0, end: 60 }]]
]);
const rect = (x0: number, y0: number, x1: number, y1: number) => ({ x0, y0, x1, y1 });

describe('normalizeRect', () => {
	test('orders the corners whichever way the drag went', () => {
		expect(normalizeRect(rect(50, 80, 10, 20))).toEqual(rect(10, 20, 50, 80));
		expect(normalizeRect(rect(10, 20, 50, 80))).toEqual(rect(10, 20, 50, 80));
	});
});

describe('rectsTouch', () => {
	test('overlap, containment and a shared edge all touch', () => {
		expect(rectsTouch(rect(0, 0, 10, 10), rect(5, 5, 15, 15))).toBe(true);
		expect(rectsTouch(rect(0, 0, 10, 10), rect(2, 2, 3, 3))).toBe(true);
		expect(rectsTouch(rect(0, 0, 10, 10), rect(10, 0, 20, 10))).toBe(true);
	});

	test('a gap on either axis does not', () => {
		expect(rectsTouch(rect(0, 0, 10, 10), rect(11, 0, 20, 10))).toBe(false);
		expect(rectsTouch(rect(0, 0, 10, 10), rect(0, 11, 10, 20))).toBe(false);
	});

	test('a shared edge counts on every side, and a gap of a hair does not', () => {
		const a = rect(10, 10, 20, 20);
		for (const b of [rect(20, 10, 30, 20), rect(0, 10, 10, 20), rect(10, 20, 20, 30), rect(10, 0, 20, 10)]) {
			expect(rectsTouch(a, b)).toBe(true);
			expect(rectsTouch(b, a)).toBe(true);
		}
		for (const b of [rect(20.01, 10, 30, 20), rect(0, 10, 9.99, 20), rect(10, 20.01, 20, 30), rect(10, 0, 20, 9.99)]) {
			expect(rectsTouch(a, b)).toBe(false);
			expect(rectsTouch(b, a)).toBe(false);
		}
	});
});

describe('marqueeHits', () => {
	test('selects every clip the rectangle touches, across tracks', () => {
		// From inside c1 on V1 down to inside c3 on A1, as far right as c2.
		expect(marqueeHits(rect(50, 60, 150, 120), lanes, clips, PX)).toEqual(['c1', 'c3', 'c2']);
	});

	test('a rectangle in one lane touches only that lane', () => {
		expect(marqueeHits(rect(0, 50, 1000, 90), lanes, clips, PX)).toEqual(['c1', 'c2', 'c4']);
		expect(marqueeHits(rect(0, 100, 1000, 140), lanes, clips, PX)).toEqual(['c3']);
	});

	test('a drag from the bottom right to the top left is the same rectangle', () => {
		expect(marqueeHits(rect(150, 120, 50, 60), lanes, clips, PX)).toEqual(
			marqueeHits(rect(50, 60, 150, 120), lanes, clips, PX)
		);
	});

	test('touching a clip at all is enough — a one-pixel overlap counts', () => {
		// c2 spans x 100..200; the rectangle ends at 100 exactly.
		expect(marqueeHits(rect(60, 50, 100, 90), lanes, clips, PX)).toEqual(['c1', 'c2']);
		expect(marqueeHits(rect(60, 50, 99, 90), lanes, clips, PX)).toEqual(['c1']);
	});

	test('a rectangle that begins exactly where a clip ends still touches it', () => {
		// c4 spans x 400..500.
		expect(marqueeHits(rect(500, 50, 560, 90), lanes, clips, PX)).toEqual(['c4']);
		expect(marqueeHits(rect(501, 50, 560, 90), lanes, clips, PX)).toEqual([]);
	});

	test('empty space between clips selects nothing', () => {
		expect(marqueeHits(rect(210, 50, 390, 90), lanes, clips, PX)).toEqual([]);
	});

	test('the gutter between a lane edge and its clips is empty space', () => {
		const gutter = CLIP_INSET_PX - 1;
		expect(marqueeHits(rect(0, 40, 1000, 40 + gutter), lanes, clips, PX)).toEqual([]);
		expect(marqueeHits(rect(0, 40, 1000, 40 + CLIP_INSET_PX), lanes, clips, PX)).toEqual(['c1', 'c2', 'c4']);
	});

	test('rows above the first lane (the ruler, the titles) hold no clips', () => {
		expect(marqueeHits(rect(0, 0, 1000, 38), lanes, clips, PX)).toEqual([]);
	});

	test('a rectangle that starts above the lanes and reaches down into them selects what it reaches', () => {
		expect(marqueeHits(rect(0, 0, 30, 60), lanes, clips, PX)).toEqual(['c1']);
	});

	test('results are in time order, ties by lane', () => {
		const out = marqueeHits(rect(0, 0, 2000, 400), lanes, clips, PX);
		expect(out).toEqual(['c1', 'c3', 'c2', 'c4']);
	});

	test('a locked lane is skipped', () => {
		const locked = lanes.map((l) => (l.trackId === 'a1' ? { ...l, locked: true } : l));
		expect(marqueeHits(rect(0, 0, 2000, 400), locked, clips, PX)).toEqual(['c1', 'c2', 'c4']);
	});

	test('follows the zoom: the same rectangle touches more at a lower zoom', () => {
		const r = rect(210, 50, 390, 90);
		expect(marqueeHits(r, lanes, clips, 10)).toEqual([]);
		expect(marqueeHits(r, lanes, clips, 5)).toEqual(['c4']);
	});

	test('a clip narrower than the minimum is as wide as it is drawn', () => {
		const tiny = new Map([['v1', [{ id: 's', start: 5, end: 5.01 }]]]);
		// 0.01 s at 10 px/s is 0.1 px, drawn MIN_CLIP_PX wide.
		expect(marqueeHits(rect(50 + MIN_CLIP_PX - 1, 50, 80, 90), lanes, tiny, 10)).toEqual(['s']);
		expect(marqueeHits(rect(50 + MIN_CLIP_PX + 1, 50, 80, 90), lanes, tiny, 10)).toEqual([]);
	});

	test('a lane with no clips, or no entry, is fine', () => {
		expect(marqueeHits(rect(0, 0, 100, 400), [{ trackId: 'v9', top: 40, height: 56 }], clips, PX)).toEqual([]);
		expect(marqueeHits(rect(0, 0, 100, 400), [], clips, PX)).toEqual([]);
	});

	test('a lane too short to hold a clip holds none', () => {
		expect(marqueeHits(rect(0, 0, 1000, 400), [{ trackId: 'v1', top: 40, height: 4 }], clips, PX)).toEqual([]);
	});
});
