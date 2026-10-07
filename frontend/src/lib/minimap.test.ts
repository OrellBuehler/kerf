import { describe, expect, test } from 'bun:test';
import {
	EDGE_PX,
	EDGE_REACH_PX,
	MERGE_BELOW_PX,
	MIN_BLOCK_PX,
	MIN_WINDOW_PX,
	centerOn,
	hitTest,
	mapSpan,
	minWindowSeconds,
	moveTo,
	resizeLeft,
	resizeRight,
	rowLayout,
	targetForRect,
	timeToX,
	trackBlocks,
	viewSpan,
	windowRect,
	xToTime,
	type MapGeo,
	type View
} from './minimap';
import { MIN_SPAN_SEC, ZOOM_MAX, ZOOM_MIN, clampZoom, zoomCeiling } from './zoom';

// A 100 s cut on a 1000 px strip: 10 px a second. The timeline shows 400 px of
// lane at 20 px/s — 20 s of cut — so the box is 200 px wide.
const DUR = 100;
const geo: MapGeo = { span: mapSpan(DUR), width: 1000 };
const view = (over: Partial<View> = {}): View => ({ scrollLeft: 0, viewW: 400, pxPerSec: 20, ...over });
const close = (a: number, b: number, eps = 1e-6) => expect(Math.abs(a - b)).toBeLessThan(eps);

describe('the strip', () => {
	test('spans the cut, never less than the timeline lays out for an empty one', () => {
		expect(mapSpan(100)).toBe(100);
		expect(mapSpan(3)).toBe(MIN_SPAN_SEC);
		expect(mapSpan(0)).toBe(MIN_SPAN_SEC);
		expect(mapSpan(NaN)).toBe(MIN_SPAN_SEC);
	});

	test('time and px are one linear map', () => {
		expect(timeToX(50, geo)).toBe(500);
		expect(xToTime(250, geo)).toBe(25);
		for (const t of [0, 1.5, 33.3, 100]) close(xToTime(timeToX(t, geo), geo), t);
	});
});

describe('view -> rectangle', () => {
	test('the seconds on screen, and where they sit on the strip', () => {
		const v = view({ scrollLeft: 200 }); // 10 s .. 30 s
		expect(viewSpan(v)).toEqual({ from: 10, to: 30 });
		expect(windowRect(v, geo)).toEqual({ x: 100, w: 200 });
	});

	test('zooming in narrows the box, zooming out widens it', () => {
		expect(windowRect(view({ pxPerSec: 40 }), geo).w).toBe(100); // 10 s
		expect(windowRect(view({ pxPerSec: 10 }), geo).w).toBe(400); // 40 s
	});

	test('a view wider than the cut fills the strip, a view past the end is held inside it', () => {
		expect(windowRect(view({ pxPerSec: 2 }), geo)).toEqual({ x: 0, w: 1000 }); // 200 s of 100
		const r = windowRect(view({ scrollLeft: 1800 }), geo); // 90 s .. 110 s
		expect(r.w).toBeGreaterThanOrEqual(MIN_WINDOW_PX);
		expect(r.x + r.w).toBeLessThanOrEqual(1000);
		close(r.x, 900);
		close(r.w, 100); // the part of the cut that is visible
	});

	test('is never thinner than the minimum (an hour of cut at a normal zoom)', () => {
		const hour: MapGeo = { span: 3600, width: 1000 };
		const r = windowRect(view({ scrollLeft: 50_000 }), hour); // 20 s of 3600 s = 5.6 px
		expect(r.w).toBe(MIN_WINDOW_PX);
		expect(r.x).toBeGreaterThan(0);
		expect(r.x + r.w).toBeLessThanOrEqual(1000);
		const end = windowRect(view({ scrollLeft: 72_000 - 400 }), hour); // at the very end
		expect(end.x + end.w).toBeLessThanOrEqual(1000);
		expect(end.w).toBe(MIN_WINDOW_PX);
	});

	test('a scroll position that is not a number does not break the strip', () => {
		const r = windowRect(view({ scrollLeft: -50 }), geo);
		expect(r.x).toBe(0);
		expect(Number.isFinite(r.w)).toBe(true);
	});
});

describe('rectangle -> view', () => {
	test('the view a rectangle stands for', () => {
		// 100 px .. 300 px = 10 s .. 30 s, 400 px visible: 20 px/s, scrolled to 10 s
		expect(targetForRect({ x: 100, w: 200 }, 400, geo, DUR)).toEqual({ zoom: 20, scrollLeft: 200 });
	});

	test('is the inverse of view -> rectangle', () => {
		for (const v of [view(), view({ scrollLeft: 340 }), view({ pxPerSec: 55, scrollLeft: 1234 }), view({ pxPerSec: 7.5, scrollLeft: 90 })]) {
			const t = targetForRect(windowRect(v, geo), v.viewW, geo, DUR);
			close(t.zoom, v.pxPerSec, 1e-9);
			close(t.scrollLeft, v.scrollLeft, 1e-6);
		}
	});

	test('the zoom is held inside the range, and the right anchor keeps its edge', () => {
		// a 1 px box would ask for an absurd zoom
		const t = targetForRect({ x: 500, w: 1 }, 400, geo, DUR);
		expect(t.zoom).toBe(clampZoom(400 / 0.1, DUR));
		expect(t.zoom).toBeLessThanOrEqual(ZOOM_MAX);
		// anchored right: the box's right edge (time 50.1) stays at the view's right edge
		const r = targetForRect({ x: 500, w: 1 }, 400, geo, DUR, 'right');
		close((r.scrollLeft + 400) / r.zoom, 50.1, 1e-6);
		// anchored left: its left edge (time 50) is at the left
		const l = targetForRect({ x: 500, w: 1 }, 400, geo, DUR, 'left');
		close(l.scrollLeft / l.zoom, 50, 1e-6);
	});

	test('a box as wide as the strip asks for the cut to fit, held at the slowest zoom', () => {
		const t = targetForRect({ x: 0, w: 1000 }, 400, geo, DUR);
		expect(t.zoom).toBe(4);
		expect(t.scrollLeft).toBe(0);
		const huge: MapGeo = { span: 1e9, width: 1000 };
		expect(targetForRect({ x: 0, w: 1000 }, 400, huge, 1e9).zoom).toBeGreaterThanOrEqual(ZOOM_MIN);
	});
});

describe('dragging the body', () => {
	test('moves the view and keeps the zoom exactly', () => {
		const t = moveTo(view(), geo, 300); // box left edge at 300 px = 30 s
		expect(t).toEqual({ zoom: 20, scrollLeft: 600 });
	});

	test('is stopped at both ends of the cut', () => {
		expect(moveTo(view(), geo, -500).scrollLeft).toBe(0);
		// 20 s on screen of 100: the left edge can be 80 s at most
		const t = moveTo(view(), geo, 5000);
		expect(t.scrollLeft).toBe(80 * 20);
		expect(t.zoom).toBe(20);
	});

	test('a view wider than the cut cannot be moved', () => {
		const wide = view({ pxPerSec: 2 });
		expect(moveTo(wide, geo, 400)).toEqual({ zoom: 2, scrollLeft: 0 });
	});

	test('does not change the zoom even when the box was widened to its minimum', () => {
		const hour: MapGeo = { span: 3600, width: 1000 };
		const v = view({ scrollLeft: 5000 }); // box = 8 px, not the 5.6 it stands for
		expect(moveTo(v, hour, 400).zoom).toBe(20);
	});

	test('a press on the bare strip centres the view there', () => {
		const t = centerOn(view(), geo, 500); // 50 s, 20 s on screen: 40 s .. 60 s
		expect(t).toEqual({ zoom: 20, scrollLeft: 800 });
		expect(centerOn(view(), geo, 0).scrollLeft).toBe(0); // can't centre before the start
		expect(centerOn(view(), geo, 1000).scrollLeft).toBe(80 * 20); // nor past the end
	});

	test('moving round-trips through the rectangle', () => {
		const v = view({ scrollLeft: 100 });
		const t = moveTo(v, geo, windowRect(v, geo).x + 70);
		const r = windowRect({ scrollLeft: t.scrollLeft, viewW: v.viewW, pxPerSec: t.zoom }, geo);
		close(r.x, windowRect(v, geo).x + 70);
		close(r.w, windowRect(v, geo).w);
	});
});

describe('dragging an edge', () => {
	test('the left edge zooms and keeps the right edge on its time', () => {
		const v = view({ scrollLeft: 200 }); // 10 s .. 30 s, box 100..300
		const t = resizeLeft(v, geo, 200, DUR); // left edge to 20 s: 10 s on screen
		expect(t.zoom).toBe(40);
		close((t.scrollLeft + v.viewW) / t.zoom, 30); // the right edge is still 30 s
		close(t.scrollLeft / t.zoom, 20);
	});

	test('the right edge zooms and keeps the left edge on its time', () => {
		const v = view({ scrollLeft: 200 });
		const t = resizeRight(v, geo, 500, DUR); // right edge to 50 s: 40 s on screen
		expect(t.zoom).toBe(10);
		close(t.scrollLeft / t.zoom, 10); // the left edge is still 10 s
	});

	test('dragging an edge outward zooms out, inward zooms in', () => {
		const v = view({ scrollLeft: 400 }); // 20 s .. 40 s, box 200..400
		expect(resizeLeft(v, geo, 100, DUR).zoom).toBeLessThan(20);
		expect(resizeLeft(v, geo, 300, DUR).zoom).toBeGreaterThan(20);
		expect(resizeRight(v, geo, 600, DUR).zoom).toBeLessThan(20);
		expect(resizeRight(v, geo, 300, DUR).zoom).toBeGreaterThan(20);
	});

	test('an edge dragged past the other one stops at the most the zoom range can show', () => {
		const v = view({ scrollLeft: 200 });
		const l = resizeLeft(v, geo, 900, DUR); // far past the right edge
		close(l.zoom, zoomCeiling(DUR), 1e-6);
		close((l.scrollLeft + v.viewW) / l.zoom, 30, 1e-6);
		const r = resizeRight(v, geo, 0, DUR); // far before the left edge
		close(r.zoom, zoomCeiling(DUR), 1e-6);
		close(r.scrollLeft / r.zoom, 10, 1e-6);
		expect(minWindowSeconds(400, DUR)).toBeCloseTo(400 / ZOOM_MAX);
	});

	test('the left edge cannot go before the start, the right edge not past the end of the cut', () => {
		const v = view({ scrollLeft: 200 });
		const l = resizeLeft(v, geo, -300, DUR);
		expect(l.scrollLeft).toBe(0); // 30 s on screen from 0
		close(l.zoom, 400 / 30);
		const r = resizeRight(v, geo, 5000, DUR);
		close(r.zoom, 400 / 90); // 10 s .. 100 s
	});

	test('a view hanging past the end of the cut anchors its right edge at the end', () => {
		const v = view({ scrollLeft: 1800 }); // 90 s .. 110 s
		const t = resizeLeft(v, geo, 800, DUR); // left edge to 80 s
		close(t.zoom, 400 / 20); // 80 s .. 100 s
		close((t.scrollLeft + v.viewW) / t.zoom, 100);
	});

	test('every result stays inside the zoom range', () => {
		const v = view({ scrollLeft: 300 });
		for (const x of [-1e6, -10, 0, 1, 250, 500, 999, 1000, 1e6]) {
			for (const t of [resizeLeft(v, geo, x, DUR), resizeRight(v, geo, x, DUR), moveTo(v, geo, x), centerOn(v, geo, x)]) {
				expect(Number.isFinite(t.zoom) && Number.isFinite(t.scrollLeft)).toBe(true);
				expect(t.zoom).toBeGreaterThanOrEqual(ZOOM_MIN);
				expect(t.zoom).toBeLessThanOrEqual(zoomCeiling(DUR));
				expect(t.scrollLeft).toBeGreaterThanOrEqual(0);
			}
		}
	});
});

describe('hit testing', () => {
	const r = { x: 100, w: 200 };
	test('edges, body, and the bare strip', () => {
		expect(hitTest(100, r)).toBe('left');
		expect(hitTest(100 + EDGE_PX, r)).toBe('left');
		expect(hitTest(100 - EDGE_REACH_PX, r)).toBe('left'); // just outside still grabs it
		expect(hitTest(300, r)).toBe('right');
		expect(hitTest(300 - EDGE_PX, r)).toBe('right');
		expect(hitTest(300 + EDGE_REACH_PX, r)).toBe('right');
		expect(hitTest(200, r)).toBe('body');
		expect(hitTest(100 + EDGE_PX + 1, r)).toBe('body');
		expect(hitTest(50, r)).toBe('outside');
		expect(hitTest(400, r)).toBe('outside');
	});

	test('a thin box keeps a body to drag', () => {
		const thin = { x: 500, w: MIN_WINDOW_PX };
		expect(hitTest(500, thin)).toBe('left');
		expect(hitTest(500 + MIN_WINDOW_PX, thin)).toBe('right');
		expect(hitTest(500 + MIN_WINDOW_PX / 2, thin)).toBe('body');
	});
});

describe('the cut as blocks', () => {
	test('each clip is a block at its place', () => {
		const b = trackBlocks(
			[
				{ start: 0, end: 20 },
				{ start: 30, end: 40 }
			],
			geo
		);
		expect(b).toEqual([
			{ x: 0, w: 200, clips: 1 },
			{ x: 300, w: 100, clips: 1 }
		]);
	});

	test('clips come out in time order whatever order they went in', () => {
		const b = trackBlocks(
			[
				{ start: 30, end: 40 },
				{ start: 0, end: 20 }
			],
			geo
		);
		expect(b.map((x) => x.x)).toEqual([0, 300]);
	});

	test('a clip is never thinner than the minimum, and is held inside the strip', () => {
		const [b] = trackBlocks([{ start: 10, end: 10.001 }], geo);
		expect(b.w).toBe(MIN_BLOCK_PX);
		const [c] = trackBlocks([{ start: 95, end: 130 }], geo);
		expect(c.x + c.w).toBeLessThanOrEqual(1000);
	});

	test('butted clips wide enough to see stay apart (the cut between them is visible)', () => {
		const b = trackBlocks(
			[
				{ start: 0, end: 10 },
				{ start: 10, end: 20 }
			],
			geo
		);
		expect(b).toHaveLength(2);
	});

	test('a run of clips too fine to tell apart is one block that counts them', () => {
		// 400 one-frame clips in 100 s: each is a quarter pixel
		const clips = Array.from({ length: 400 }, (_, i) => ({ start: i * 0.25, end: i * 0.25 + 0.2 }));
		const b = trackBlocks(clips, geo);
		expect(b.length).toBeLessThan(1000 / 2);
		expect(b.reduce((n, x) => n + x.clips, 0)).toBe(400);
		for (const x of b) expect(x.w).toBeGreaterThanOrEqual(MIN_BLOCK_PX);
	});

	test('however many clips there are, the blocks are bounded by the strip, not the cut', () => {
		const clips = Array.from({ length: 20_000 }, (_, i) => ({ start: i * 0.005, end: i * 0.005 + 0.004 }));
		const b = trackBlocks(clips, geo);
		expect(b.length).toBeLessThan(500);
		expect(b.reduce((n, x) => n + x.clips, 0)).toBe(20_000);
		// and they cover where the clips are: the first block starts at the left, the last ends near x = 1000
		expect(b[0].x).toBe(0);
		close(b.at(-1)!.x + b.at(-1)!.w, 1000, 2);
	});

	test('a thin clip next to a wide one joins it only when they nearly touch', () => {
		const apart = trackBlocks(
			[
				{ start: 0, end: 20 },
				{ start: 20.5, end: 20.6 } // 5 px later: a thin clip of its own
			],
			geo
		);
		expect(apart).toHaveLength(2);
		const touching = trackBlocks(
			[
				{ start: 0, end: 20 },
				{ start: 20, end: 20.1 } // butts the first, 1 px wide
			],
			geo
		);
		expect(touching).toHaveLength(1);
		expect(touching[0]).toMatchObject({ x: 0, clips: 2 });
		expect(touching[0].w).toBeGreaterThanOrEqual(201);
	});

	test('no clips, no blocks', () => {
		expect(trackBlocks([], geo)).toEqual([]);
		expect(MERGE_BELOW_PX).toBeGreaterThan(MIN_BLOCK_PX);
	});
});

describe('rows', () => {
	test('tracks stack top to bottom, each as tall as the strip leaves it', () => {
		const rows = rowLayout(2, 36);
		expect(rows).toHaveLength(2);
		expect(rows[0].y).toBeLessThan(rows[1].y);
		expect(rows[0].h).toBe(rows[1].h);
		for (const r of rows) expect(r.y + r.h).toBeLessThanOrEqual(36);
	});

	test('a few tracks are capped at a comfortable height, with the usual gap', () => {
		expect(rowLayout(1, 36)[0].h).toBe(12);
		const two = rowLayout(2, 36);
		expect(two[1].y - (two[0].y + two[0].h)).toBe(2);
		const many = rowLayout(8, 36);
		expect(many[0].h).toBeLessThan(12);
		expect(many[7].y + many[7].h).toBeLessThanOrEqual(36);
	});

	test('past ten tracks the rows still fit: the gap gives first, then every row is a pixel', () => {
		const strip = 36;
		const pad = 4;
		for (const n of [10, 11, 12, 14, 20, 28]) {
			const rows = rowLayout(n, strip);
			expect(rows).toHaveLength(n);
			const last = rows[n - 1];
			expect(last.y + last.h).toBeLessThanOrEqual(strip - pad + 1e-9);
			for (const r of rows) expect(r.h).toBeGreaterThanOrEqual(1 - 1e-9); // 28 rows of 28 px: a hairline each
			for (let i = 1; i < n; i++) expect(rows[i].y).toBeGreaterThanOrEqual(rows[i - 1].y + rows[i - 1].h - 1e-9); // none overlap
		}
		// the gap is what shrinks: 12 tracks have no room for the 2 px they were given
		const twelve = rowLayout(12, strip);
		expect(twelve[1].y - (twelve[0].y + twelve[0].h)).toBeLessThan(2);
		expect(twelve[0].h).toBeGreaterThanOrEqual(1);
	});

	test('more tracks than pixels share the height fractionally rather than running off the strip', () => {
		for (const n of [29, 40, 100, 500]) {
			const rows = rowLayout(n, 36);
			expect(rows).toHaveLength(n);
			const last = rows[n - 1];
			expect(last.y + last.h).toBeLessThanOrEqual(36 - 4 + 1e-9);
			expect(rows[0].y).toBe(4);
			for (const r of rows) expect(r.h).toBeGreaterThan(0);
		}
	});

	test('fits whatever the strip height or padding', () => {
		for (const height of [0, 10, 36, 80]) {
			for (const n of [1, 3, 9, 15, 60]) {
				for (const pad of [0, 4, 20]) {
					const rows = rowLayout(n, height, { pad });
					const bottom = rows.length ? rows[rows.length - 1].y + rows[rows.length - 1].h : 0;
					// a strip with no room (padding past its height) places rows at the padding, empty
					expect(bottom).toBeLessThanOrEqual(Math.max(height - pad, pad) + 1e-9);
				}
			}
		}
	});

	test('no tracks, no rows', () => {
		expect(rowLayout(0, 36)).toEqual([]);
	});
});
