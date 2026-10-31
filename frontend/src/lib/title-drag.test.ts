import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { frameTime, onFrame } from './frames';
import { MIN_TITLE } from './titles';
import { markerDragStep, titleDragStep, type MarkerGrab, type TitleGrab } from './title-drag';

const SLOP = 3;
const PX = 100; // px per second

const grab = (over: Partial<TitleGrab> = {}): TitleGrab => ({
	mode: 'move',
	origStart: 4,
	origEnd: 6,
	grabSec: 1, // the pointer went down at 5.0 s, 1 s into a title that starts at 4 s
	downX: 500,
	moved: false,
	...over
});
const step = (d: TitleGrab, clientX: number, over: Partial<Parameters<typeof titleDragStep>[1]> = {}) =>
	titleDragStep(d, { clientX, time: clientX / PX, slopPx: SLOP, snap: true, points: [], threshold: 8 / PX, ...over });

describe('titleDragStep — a press is not a drag', () => {
	test('under the slop nothing changes, even with a magnet that would pull the result off', () => {
		// The pointer sits 1 s into the title; a magnet 6 px from its start would snap the title to it.
		const d = grab();
		for (const x of [500, 501, 502, 498]) {
			const s = step(d, x, { points: [4.06] });
			expect(s).toEqual({ start: 4, end: 6, moved: false });
		}
	});

	test('at the slop it becomes a drag, and snaps from then on', () => {
		const d = grab();
		const s = step(d, 503, { points: [4.06] });
		expect(s.moved).toBe(true);
		expect(s.start).toBe(4.06);
		expect(s.end).toBeCloseTo(6.06, 9);
	});

	test('once a drag, it stays one though the pointer comes back', () => {
		const d = grab({ moved: true });
		expect(step(d, 500).moved).toBe(true);
	});

	test('a move keeps the offset it was grabbed at, with snapping off too', () => {
		const d = grab();
		const s = step(d, 600, { snap: false });
		expect(s.start).toBeCloseTo(600 / PX - 1, 9);
		expect(s.end - s.start).toBeCloseTo(2, 9);
		expect(step(grab({ grabSec: 4.5 }), 504, { snap: false }).start).toBeCloseTo(5.04 - 4.5, 9);
	});

	test('never before 0', () => {
		expect(step(grab({ moved: true }), 10, { snap: false }).start).toBe(0);
		expect(step(grab({ moved: true }), 10).start).toBe(0);
	});
});

describe('titleDragStep — trims', () => {
	test('a left edge grabbed a few pixels inside it does not jump to the pointer', () => {
		// grabbed 4 px = 0.04 s inside the left edge: a click leaves the title as it was...
		const d = grab({ mode: 'l', grabSec: 0.04 });
		expect(step(d, 501)).toEqual({ start: 4, end: 6, moved: false });
		// ...and a drag moves the edge by the pointer's travel, not to the pointer.
		const s = step(d, 520, { snap: false });
		expect(s.start).toBeCloseTo(520 / PX - 0.04, 9);
		expect(s.end).toBe(6);
	});

	test('a right edge keeps its grab offset from the end', () => {
		const d = grab({ mode: 'r', grabSec: -0.03, downX: 600 });
		const s = step(d, 640, { snap: false });
		expect(s.end).toBeCloseTo(640 / PX + 0.03, 9);
		expect(s.start).toBe(4);
	});

	test('the edge snaps by where it would stand, and the title keeps a minimum length', () => {
		const d = grab({ mode: 'l', grabSec: 0, moved: true });
		expect(step(d, 405, { points: [4.02] }).start).toBe(4.02);
		expect(step(d, 700, { snap: false }).start).toBeCloseTo(6 - MIN_TITLE, 9);
		const r = grab({ mode: 'r', grabSec: 0, moved: true });
		expect(step(r, 401, { snap: false }).end).toBeCloseTo(4 + MIN_TITLE, 9);
	});
});

describe('markerDragStep', () => {
	const m = (over: Partial<MarkerGrab> = {}): MarkerGrab => ({ origTime: 3.7, grabSec: 0.013, downX: 370, moved: false, ...over });
	const at = (d: MarkerGrab, clientX: number, fps = 30) => markerDragStep(d, { clientX, time: clientX / PX, slopPx: SLOP, fps });

	test('a click, or a jitter under the slop, leaves the marker where it is — whatever frame it is on', () => {
		const odd = m({ origTime: 3.7123 });
		for (const x of [370, 371, 372, 368]) expect(at(odd, x)).toEqual({ time: 3.7123, moved: false });
	});

	test('a drag lands on a frame of the cut, from the pointer less the offset it was grabbed at', () => {
		for (const fps of [24, 25, 30, 30000 / 1001, 60]) {
			const s = at(m(), 412.3, fps);
			expect(s.moved).toBe(true);
			expect(onFrame(s.time, fps)).toBe(true);
			expect(Math.abs(s.time - (4.123 - 0.013))).toBeLessThanOrEqual(0.5 / fps + 1e-9);
		}
		expect(at(m(), 412.3, 30).time).toBe(frameTime(123, 30));
	});

	test('stays a drag, and never goes before 0', () => {
		expect(at(m({ moved: true }), 370).moved).toBe(true);
		expect(at(m({ moved: true, grabSec: 0.5 }), 10).time).toBe(0);
	});
});

describe('the timeline uses them', () => {
	const src = readFileSync(join(import.meta.dir, 'components/editor/Timeline.svelte'), 'utf8');
	const bodyOf = (name: string): string => {
		const at = src.indexOf(`function ${name}(`);
		expect(at, name).toBeGreaterThan(-1);
		return src.slice(at, src.indexOf('\n\tfunction ', at + 1));
	};

	test('a title is dragged through titleDragStep, with the grab offset and the press position recorded', () => {
		const move = bodyOf('onTitleDragMove');
		expect(move).toContain('titleDragStep(d,');
		expect(move).not.toContain('snapSpanStart');
		const down = bodyOf('onTitlePointerDown');
		expect(down).toContain('downX: e.clientX');
		expect(down).toMatch(/grabSec: laneTime\(e\.clientX, left\) - \(mode === 'r' \? o\.end : o\.start\)/);
	});

	test('a title or a marker that did not move writes nothing', () => {
		expect(bodyOf('onTitleDragUp')).toContain('if (!d.moved) return;');
		expect(src).toContain('if (m && d.moved && Math.abs(m.time - d.time) > 1e-6)');
	});

	test('a marker’s time is the quantized step, not the raw pointer', () => {
		expect(src).toContain('markerDragStep(markerDrag,');
		expect(src).not.toContain('markerDrag = { ...markerDrag, time: rulerTime(e.clientX) }');
	});
});
