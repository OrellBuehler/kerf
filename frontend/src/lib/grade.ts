/* The TS mirror of kerf-core's `model/grade.rs`: what a curve, a wheel and a LUT do, so the curve
 * editor draws the curve the export renders and the wheels say the numbers `colorbalance` gets.
 * Keep the two in step: `grade.test.ts` replays the Rust tests and pins the same numbers. */

import type { ClipLut, CurvePoint, Curves, Grade, Wheel, Wheels } from './types';

/** The most points one curve may have. */
export const MAX_CURVE_POINTS = 16;

export const CURVE_CHANNELS = ['master', 'red', 'green', 'blue'] as const;
export type CurveChannel = (typeof CURVE_CHANNELS)[number];

export const WHEEL_RANGES = ['shadows', 'midtones', 'highlights'] as const;
export type WheelRange = (typeof WHEEL_RANGES)[number];

export const NEUTRAL_WHEEL: Wheel = { x: 0, y: 0, lightness: 0 };
export const NEUTRAL_WHEELS: Wheels = { shadows: NEUTRAL_WHEEL, midtones: NEUTRAL_WHEEL, highlights: NEUTRAL_WHEEL };

/** What an empty curve is drawn as in the editor. */
export const IDENTITY_POINTS: CurvePoint[] = [
	{ x: 0, y: 0 },
	{ x: 1, y: 1 }
];

const finite01 = (v: number) => (Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0);
/** Rust's `f64::round`: half away from zero. */
const roundAway = (v: number) => Math.sign(v) * Math.round(Math.abs(v));

/** Points as the model keeps them: clamped, sorted, one per `x` (the first), at most 16, none for the identity. */
export function normalizeCurve(points: readonly CurvePoint[] | undefined | null): CurvePoint[] {
	const sorted = (points ?? []).map((p) => ({ x: finite01(p.x), y: finite01(p.y) })).sort((a, b) => a.x - b.x);
	const pts: CurvePoint[] = [];
	for (const p of sorted) if (!pts.length || Math.abs(p.x - pts[pts.length - 1].x) >= 1e-6) pts.push(p);
	pts.length = Math.min(pts.length, MAX_CURVE_POINTS);
	const identity =
		pts.length < 2 || (pts[0].x === 0 && pts[pts.length - 1].x === 1 && pts.every((p) => Math.abs(p.y - p.x) < 1e-9));
	return identity ? [] : pts;
}

/** One cubic piece: `a + u * (b + u * (c + u * d))`, `u = (x - x0) * k`. */
export interface CurveSegment {
	x0: number;
	k: number;
	a: number;
	b: number;
	c: number;
	d: number;
}

export interface CurveShape {
	points: CurvePoint[];
	segments: CurveSegment[];
}

/** The monotone cubic (Fritsch–Carlson) through a curve's points, or `null` for the identity. */
export function curveShape(points: readonly CurvePoint[] | undefined | null): CurveShape | null {
	const pts = normalizeCurve(points);
	const m = pts.length;
	if (m < 2) return null;
	const h = pts.slice(0, -1).map((p, i) => pts[i + 1].x - p.x);
	const d = pts.slice(0, -1).map((p, i) => (pts[i + 1].y - p.y) / h[i]);
	const t = new Array<number>(m).fill(0);
	t[0] = d[0];
	t[m - 1] = d[m - 2];
	for (let i = 1; i < m - 1; i++) t[i] = d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2;
	for (let i = 0; i < m - 1; i++) {
		if (d[i] === 0) {
			t[i] = 0;
			t[i + 1] = 0;
			continue;
		}
		const a = t[i] / d[i];
		const b = t[i + 1] / d[i];
		const s = a * a + b * b;
		if (s > 9) {
			const k = 3 / Math.sqrt(s);
			t[i] = k * a * d[i];
			t[i + 1] = k * b * d[i];
		}
	}
	const segments = h.map((hh, i) => {
		const [y0, y1] = [pts[i].y, pts[i + 1].y];
		const [m0, m1] = [hh * t[i], hh * t[i + 1]];
		return { x0: pts[i].x, k: 1 / hh, a: y0, b: m0, c: 3 * (y1 - y0) - 2 * m0 - m1, d: 2 * (y0 - y1) + m0 + m1 };
	});
	return { points: pts, segments };
}

/** The curve at `x`, held flat outside its first and last point. */
export function evalShape(shape: CurveShape, x: number): number {
	const { points, segments } = shape;
	const first = points[0];
	const last = points[points.length - 1];
	if (x < first.x) return first.y;
	if (x >= last.x) return last.y;
	let lo = 0;
	let hi = segments.length;
	while (hi - lo > 1) {
		const mid = (lo + hi) >> 1;
		if (x < points[mid].x) hi = mid;
		else lo = mid;
	}
	const s = segments[lo];
	const u = (x - s.x0) * s.k;
	return s.a + u * (s.b + u * (s.c + u * s.d));
}

/** One curve alone at `x` (the identity when it has no points). */
export function evalCurve(points: readonly CurvePoint[] | undefined | null, x: number): number {
	const shape = curveShape(points);
	return shape ? evalShape(shape, x) : x;
}

/** What the curves do to one channel (0 red, 1 green, 2 blue): its curve, then the master, clamped. */
export function applyCurves(curves: Curves, channel: 0 | 1 | 2, x: number): number {
	const own = [curves.red, curves.green, curves.blue][channel];
	return Math.min(1, Math.max(0, evalCurve(curves.master, evalCurve(own, x))));
}

export function normalizeCurves(curves: Curves | null | undefined): Curves | null {
	if (!curves) return null;
	const out: Curves = {};
	for (const ch of CURVE_CHANNELS) {
		const pts = normalizeCurve(curves[ch]);
		if (pts.length) out[ch] = pts;
	}
	return Object.keys(out).length ? out : null;
}

const tenk = (v: number) => {
	const r = roundAway(v * 1e4) / 1e4;
	return r === 0 ? 0 : r;
};

export function normalizeWheel(w: Wheel): Wheel {
	const f = (v: number) => (Number.isFinite(v) ? v : 0);
	const [x, y] = [f(w.x), f(w.y)];
	const len = Math.hypot(x, y);
	const s = len > 1 ? 1 / len : 1;
	return { x: tenk(x * s), y: tenk(y * s), lightness: tenk(Math.min(1, Math.max(-1, f(w.lightness)))) };
}

/** `colorbalance`'s red, green and blue for a wheel's range. */
export function wheelRgb(wheel: Wheel): [number, number, number] {
	const w = normalizeWheel(wheel);
	const h = Math.sqrt(3) / 2;
	const raw = [w.x, -w.x / 2 + h * w.y, -w.x / 2 - h * w.y];
	return raw.map((v) => tenk(Math.min(1, Math.max(-1, v + w.lightness)))) as [number, number, number];
}

export const wheelNeutral = (w: Wheel) => wheelRgb(w).every((v) => v === 0);

export function normalizeWheels(wheels: Wheels | null | undefined): Wheels | null {
	if (!wheels) return null;
	const out = {
		shadows: normalizeWheel(wheels.shadows ?? NEUTRAL_WHEEL),
		midtones: normalizeWheel(wheels.midtones ?? NEUTRAL_WHEEL),
		highlights: normalizeWheel(wheels.highlights ?? NEUTRAL_WHEEL)
	};
	return WHEEL_RANGES.every((r) => wheelNeutral(out[r])) ? null : out;
}

export function normalizeLut(lut: ClipLut | null | undefined): ClipLut | null {
	if (!lut) return null;
	const i = Number.isFinite(lut.intensity) ? Math.min(1, Math.max(0, lut.intensity)) : 1;
	return { path: lut.path, intensity: roundAway(i * 100) / 100 };
}

/** The grade as the backend keeps it. */
export function normalizeGrade(grade: Grade | null | undefined): Grade {
	const out: Grade = {};
	const curves = normalizeCurves(grade?.curves);
	const wheels = normalizeWheels(grade?.wheels);
	const lut = normalizeLut(grade?.lut);
	if (curves) out.curves = curves;
	if (wheels) out.wheels = wheels;
	if (lut) out.lut = lut;
	return out;
}

/** The grade leaves the picture alone (a LUT at 0 % included). */
export function gradeIsIdentity(grade: Grade | null | undefined): boolean {
	const g = normalizeGrade(grade);
	return !g.curves && !g.wheels && (!g.lut || g.lut.intensity === 0);
}

/** The file name of a path, either separator. */
export const lutName = (path: string) => path.split(/[\\/]/).pop() || path;

/** The Inspector's one-line summaries. */
export function curvesSummary(grade: Grade | undefined): string {
	const c = normalizeCurves(grade?.curves);
	if (!c) return 'None';
	return CURVE_CHANNELS.filter((ch) => c[ch]?.length)
		.map((ch) => ch[0].toUpperCase() + ch.slice(1))
		.join(' · ');
}

export function wheelsSummary(grade: Grade | undefined): string {
	const w = normalizeWheels(grade?.wheels);
	if (!w) return 'Neutral';
	return WHEEL_RANGES.filter((r) => !wheelNeutral(w[r]))
		.map((r) => r[0].toUpperCase() + r.slice(1))
		.join(' · ');
}

export function lutSummary(grade: Grade | undefined): string {
	const l = normalizeLut(grade?.lut);
	return l ? `${lutName(l.path)} · ${Math.round(l.intensity * 100)}%` : 'None';
}

/** The puck's colour on the wheel: the direction `colorbalance` pushes, as 0..255 RGB at full strength. */
export function wheelHue(x: number, y: number): [number, number, number] {
	const len = Math.hypot(x, y);
	if (len < 1e-9) return [128, 128, 128];
	const h = Math.sqrt(3) / 2;
	const [r, g, b] = [x / len, -x / len / 2 + (h * y) / len, -x / len / 2 - (h * y) / len];
	const lift = Math.min(1, len);
	const to = (v: number) => Math.round(128 + 127 * v * lift);
	return [to(r), to(g), to(b)];
}
