import { describe, expect, test } from 'bun:test';
import {
	MAX_CURVE_POINTS,
	applyCurves,
	curveEdit,
	curveShape,
	evalShape,
	gradeIsIdentity,
	gradeWith,
	lutEdit,
	lutName,
	addPoint,
	movePoint,
	nearestPoint,
	normalizeCurve,
	normalizeGrade,
	normalizeWheel,
	wheelEdit,
	wheelNeutral,
	wheelRgb
} from './grade';
import type { Clip, CurvePoint, Grade, Timeline } from './types';

// The Rust tests of `model/grade.rs`, replayed: the editor draws the curve the export renders.

const pts = (p: [number, number][]): CurvePoint[] => p.map(([x, y]) => ({ x, y }));

describe('curves', () => {
	test('identity curves are dropped', () => {
		expect(normalizeCurve(pts([[0, 0], [1, 1]]))).toEqual([]);
		expect(normalizeCurve(pts([[0, 0], [0.5, 0.5], [1, 1]]))).toEqual([]);
		expect(normalizeCurve(pts([[0.3, 0.7]]))).toEqual([]);
		expect(normalizeCurve(pts([[0.2, 0.2], [0.8, 0.8]])).length).toBe(2);
		const g = { curves: { master: pts([[0, 0], [1, 1]]) }, wheels: { shadows: { x: 0, y: 0, lightness: 0 }, midtones: { x: 0, y: 0, lightness: 0 }, highlights: { x: 0, y: 0, lightness: 0 } } };
		expect(gradeIsIdentity(g)).toBe(true);
		expect(normalizeGrade(g)).toEqual({});
	});

	test('points are clamped, sorted, deduplicated and capped', () => {
		expect(normalizeCurve(pts([[1.5, -1], [0.5, 0.2], [NaN, 0.3], [0.5, 0.9]]))).toEqual(pts([[0, 0.3], [0.5, 0.2], [1, 0]]));
		const many = Array.from({ length: 40 }, (_, i) => ({ x: i / 39, y: 0.5 }));
		expect(normalizeCurve(many).length).toBe(MAX_CURVE_POINTS);
	});

	test('the curve goes through its points and never overshoots', () => {
		const shape = curveShape(pts([[0, 0], [0.25, 0.1], [0.5, 0.9], [0.75, 0.92], [1, 1]]))!;
		for (const p of shape.points) expect(Math.abs(evalShape(shape, p.x) - p.y)).toBeLessThan(1e-12);
		let prev = -1;
		for (let i = 0; i <= 1000; i++) {
			const y = evalShape(shape, i / 1000);
			expect(y).toBeGreaterThanOrEqual(prev - 1e-12);
			prev = y;
		}
		const peak = curveShape(pts([[0, 0], [0.5, 0.8], [1, 0]]))!;
		for (let i = 0; i <= 1000; i++) expect(evalShape(peak, i / 1000)).toBeLessThanOrEqual(0.8 + 1e-12);
	});

	test('the values the Rust model is pinned to', () => {
		const shape = curveShape(pts([[0, 0], [0.25, 0.1], [0.5, 0.9], [0.75, 0.92], [1, 1]]))!;
		const want: [number, number][] = [
			[0.1, 0.016457208444468113],
			[0.3, 0.21877970302616673],
			[0.4, 0.6379377269512614],
			[0.6, 0.914919187180245],
			[0.9, 0.9610172732017916]
		];
		for (const [x, y] of want) expect(Math.abs(evalShape(shape, x) - y)).toBeLessThan(1e-15);
	});

	test('a straight line is exactly linear and the ends hold', () => {
		const invert = curveShape(pts([[0, 1], [1, 0]]))!;
		for (let i = 0; i <= 255; i++) expect(Math.abs(evalShape(invert, i / 255) - (1 - i / 255))).toBeLessThan(1e-12);
		const mid = curveShape(pts([[0.2, 0.3], [0.8, 0.6]]))!;
		expect(evalShape(mid, 0)).toBe(0.3);
		expect(evalShape(mid, 1)).toBe(0.6);
	});

	test('the master runs on the channel’s result', () => {
		const c = { master: pts([[0, 1], [1, 0]]), red: pts([[0, 0], [1, 0.5]]) };
		expect(Math.abs(applyCurves(c, 0, 1) - 0.5)).toBeLessThan(1e-12);
		expect(Math.abs(applyCurves(c, 1, 0.25) - 0.75)).toBeLessThan(1e-12);
	});
});

describe('wheels', () => {
	test('a wheel is a hue and a lightness', () => {
		expect(wheelRgb({ x: 1, y: 0, lightness: 0 })).toEqual([1, -0.5, -0.5]);
		expect(wheelRgb({ x: -0.5, y: Math.sqrt(3) / 2, lightness: 0 })).toEqual([-0.5, 1, -0.5]);
		expect(wheelRgb({ x: 0, y: 0, lightness: 0.2 })).toEqual([0.2, 0.2, 0.2]);
		expect(normalizeWheel({ x: 3, y: 4, lightness: 5 })).toEqual({ x: 0.6, y: 0.8, lightness: 1 });
		expect(wheelNeutral({ x: 0, y: 0, lightness: 0 })).toBe(true);
	});
});

describe('the LUT', () => {
	test('at zero it is neutral but kept', () => {
		const g = { lut: { path: '/a.cube', intensity: 0.001 } };
		expect(gradeIsIdentity(g)).toBe(true);
		expect(normalizeGrade(g)).toEqual({ lut: { path: '/a.cube', intensity: 0 } });
	});

	test('its name is the file name on either separator', () => {
		expect(lutName('/a/b/look.cube')).toBe('look.cube');
		expect(lutName('C:\\luts\\film.cube')).toBe('film.cube');
	});
});

describe('the curve editor', () => {
	const base = pts([[0, 0], [0.5, 0.5], [1, 1]]);
	test('a dragged point stays between its neighbours', () => {
		expect(movePoint(base, 1, 1.5, 0.7)[1]).toEqual({ x: 0.99, y: 0.7 });
		expect(movePoint(base, 1, -1, 2)[1]).toEqual({ x: 0.01, y: 1 });
		expect(movePoint(base, 0, 0.3, 0.2)[0]).toEqual({ x: 0.3, y: 0.2 });
	});

	test('a point is added in order, and not on top of another or past the cap', () => {
		expect(addPoint(base, 0.25, 0.4)).toEqual({ points: pts([[0, 0], [0.25, 0.4], [0.5, 0.5], [1, 1]]), index: 1 });
		expect(addPoint(base, 0.505, 0.4)).toBeNull();
		const full = Array.from({ length: MAX_CURVE_POINTS }, (_, i) => ({ x: i / 15, y: 0 }));
		expect(addPoint(full, 0.51, 0.5)).toBeNull();
	});

	test('the nearest point in reach is the one picked', () => {
		expect(nearestPoint(base, 0.48, 0.52, 0.05)).toBe(1);
		expect(nearestPoint(base, 0.3, 0.3, 0.05)).toBe(-1);
	});
});

// ---- an edit goes to the clip it began on --------------------------------------

describe('writing a grade to the clip an edit began on', () => {
	// Clip A is graded one way, clip B another. A field of A commits after the selection has moved to B
	// (a typed LUT intensity is written by the blur the click on B causes), so the grade must be built
	// from A's own, never from what the panel shows (B's).
	const wheel = (x: number, y: number, lightness = 0) => ({ x, y, lightness });
	const gradeA: Grade = {
		curves: { master: pts([[0, 0.1], [1, 1]]) },
		wheels: { shadows: wheel(0.2, 0.1), midtones: wheel(0, 0), highlights: wheel(0, 0) },
		lut: { path: '/luts/a.cube', intensity: 0.5 }
	};
	const gradeB: Grade = {
		curves: { red: pts([[0, 0], [0.5, 0.8], [1, 1]]) },
		wheels: { shadows: wheel(-0.5, 0.4), midtones: wheel(0.3, 0.3), highlights: wheel(0, 0, 0.2) }
	};
	const mk = (id: string, grade?: Grade): Clip => ({
		id,
		asset_id: 'a',
		source_in: 0,
		source_out: 4,
		timeline_start: 0,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		grade
	});
	const timeline = (): Timeline => ({
		tracks: [
			{ id: 't', kind: 'video', name: 'V1', clips: [mk('a', gradeA), mk('b', gradeB), mk('plain')] }
		]
	});

	test('the grade is the clip’s own, normalised, with the patch laid over it', () => {
		const g = gradeWith(timeline(), 'a', () => ({ wheels: null }));
		expect(g).toEqual({ curves: normalizeGrade(gradeA).curves, lut: { path: '/luts/a.cube', intensity: 0.5 }, wheels: null });
		// A clip with no grade starts from nothing.
		expect(gradeWith(timeline(), 'plain', () => ({ lut: { path: '/l.cube', intensity: 1 } }))).toEqual({ lut: { path: '/l.cube', intensity: 1 } });
	});

	test('a clip that is gone, or a patch with nothing to say, writes nothing', () => {
		expect(gradeWith(timeline(), 'gone', () => ({ wheels: null }))).toBeNull();
		expect(gradeWith(timeline(), 'a', () => null)).toBeNull();
	});

	test('a LUT intensity typed on A and committed after B is selected leaves B’s grade out of A', () => {
		const next = lutEdit(timeline(), 'a', (lut) => (lut ? { ...lut, intensity: 0.8 } : null));
		expect(next?.lut).toEqual({ path: '/luts/a.cube', intensity: 0.8 });
		expect(next?.curves).toEqual(normalizeGrade(gradeA).curves);
		expect(next?.wheels).toEqual(normalizeGrade(gradeA).wheels);
		expect(JSON.stringify(next)).not.toContain('"red"');
		// Clearing it, and choosing a file, keep the rest too.
		expect(lutEdit(timeline(), 'a', () => null)).toEqual({ ...normalizeGrade(gradeA), lut: null });
		expect(lutEdit(timeline(), 'a', (lut) => ({ path: '/luts/new.cube', intensity: lut?.intensity ?? 1 }))?.lut).toEqual({
			path: '/luts/new.cube',
			intensity: 0.5
		});
		// B has no LUT: a file chosen on B starts at full strength and does not take A's.
		expect(lutEdit(timeline(), 'b', (lut) => ({ path: '/luts/new.cube', intensity: lut?.intensity ?? 1 }))?.lut).toEqual({
			path: '/luts/new.cube',
			intensity: 1
		});
	});

	test('a curve is written onto the clip it began on, and nothing when that clip already has it', () => {
		const tl = timeline();
		const next = curveEdit(tl, 'a', 'green', pts([[0, 0], [0.5, 0.7], [1, 1]]));
		expect(next?.curves?.green).toEqual(pts([[0, 0], [0.5, 0.7], [1, 1]]));
		expect(next?.curves?.master).toEqual(pts([[0, 0.1], [1, 1]]));
		expect(next?.curves?.red).toBeUndefined();
		expect(next?.lut).toEqual({ path: '/luts/a.cube', intensity: 0.5 });
		// A's own master curve again: nothing to write. B's red curve is B's.
		expect(curveEdit(tl, 'a', 'master', pts([[0, 0.1], [1, 1]]))).toBeNull();
		expect(curveEdit(tl, 'a', 'red', pts([[0, 0], [0.5, 0.8], [1, 1]]))).not.toBeNull();
		expect(curveEdit(tl, 'b', 'red', pts([[0, 0], [0.5, 0.8], [1, 1]]))).toBeNull();
		// Straightening a channel the clip does not curve is not an edit; one it does is.
		expect(curveEdit(tl, 'a', 'blue', [])).toBeNull();
		expect(curveEdit(tl, 'a', 'master', [])?.curves?.master).toEqual([]);
	});

	test('a wheel keeps the clip’s other wheels and the parts of it that were not moved', () => {
		const tl = timeline();
		// The lightness slider of A's shadows, released after B was selected: A's puck stays where it was.
		const light = wheelEdit(tl, 'a', 'shadows', { lightness: 0.4 });
		expect(light?.wheels?.shadows).toEqual(wheel(0.2, 0.1, 0.4));
		expect(light?.wheels?.midtones).toEqual(wheel(0, 0));
		expect(light?.curves).toEqual(normalizeGrade(gradeA).curves);
		expect(light?.lut).toEqual({ path: '/luts/a.cube', intensity: 0.5 });
		// A puck moved on B's midtones: B's lightness, not A's, stays.
		const puck = wheelEdit(tl, 'b', 'highlights', { x: 0.5, y: -0.5 });
		expect(puck?.wheels?.highlights).toEqual(wheel(0.5, -0.5, 0.2));
		expect(puck?.wheels?.shadows).toEqual(wheel(-0.5, 0.4));
		// Where the wheel already is writes nothing; a clip with no wheels starts from neutral ones.
		expect(wheelEdit(tl, 'a', 'shadows', { x: 0.2, y: 0.1 })).toBeNull();
		expect(wheelEdit(tl, 'plain', 'midtones', { x: 0, y: 0 })).toBeNull();
		expect(wheelEdit(tl, 'plain', 'midtones', { x: 0.3 })?.wheels).toEqual({
			shadows: wheel(0, 0),
			midtones: wheel(0.3, 0),
			highlights: wheel(0, 0)
		});
	});
});
