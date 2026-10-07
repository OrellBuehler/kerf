import { describe, expect, test } from 'bun:test';
import { drawWaveform, type Ctx2D, type WavePalette } from './waveform-draw';
import type { Columns } from './waveform-view';

/** A canvas context that writes down what it was asked to do. */
function recorder() {
	const ops: string[] = [];
	const fills: { style: string; points: [number, number][] }[] = [];
	let style = '';
	let path: [number, number][] = [];
	const ctx: Ctx2D = {
		get fillStyle() {
			return style;
		},
		set fillStyle(v) {
			style = String(v);
		},
		clearRect: (...a: number[]) => void ops.push(`clear ${a.join(',')}`),
		beginPath: () => {
			path = [];
		},
		moveTo: (x, y) => void path.push([x, y]),
		lineTo: (x, y) => void path.push([x, y]),
		closePath: () => void ops.push('close'),
		fill: () => void fills.push({ style, points: path })
	};
	return { ctx, ops, fills };
}

const palette: WavePalette = { wave: 'wave-colour', clip: 'clip-colour' };

function columns(lanes: number, count: number, peak: (lane: number, d: number) => [number, number]): Columns {
	const c: Columns = {
		count,
		lanes,
		min: Array.from({ length: lanes }, () => new Float32Array(count)),
		max: Array.from({ length: lanes }, () => new Float32Array(count)),
		missing: 0
	};
	for (let l = 0; l < lanes; l++)
		for (let d = 0; d < count; d++) {
			const [lo, hi] = peak(l, d);
			c.min[l][d] = lo;
			c.max[l][d] = hi;
		}
	return c;
}

const opts = (over: Partial<Parameters<typeof drawWaveform>[2]> = {}) => ({
	width: 100,
	height: 40,
	gain: 1,
	dpr: 1,
	palette,
	...over
});

describe('drawWaveform', () => {
	test('is one filled shape per lane, not a stroke per sample', () => {
		const { ctx, fills } = recorder();
		drawWaveform(ctx, columns(1, 100, () => [-0.5, 0.5]), opts());
		expect(fills).toHaveLength(1);
		expect(fills[0].style).toBe('wave-colour');
		// up the tops, back along the bottoms: a vertex per column each way
		expect(fills[0].points.length).toBeGreaterThanOrEqual(200);
	});

	test('clears the canvas first', () => {
		const { ctx, ops } = recorder();
		drawWaveform(ctx, columns(1, 10, () => [-0.1, 0.1]), opts({ width: 10 }));
		expect(ops[0]).toBe('clear 0,0,10,40');
	});

	test('stereo is two lanes, each centred in its half', () => {
		const { ctx, fills } = recorder();
		drawWaveform(ctx, columns(2, 50, () => [-0.5, 0.5]), opts({ width: 50, height: 60 }));
		expect(fills).toHaveLength(2);
		const mid = (f: (typeof fills)[0]) => {
			const ys = f.points.map((p) => p[1]);
			return (Math.min(...ys) + Math.max(...ys)) / 2;
		};
		expect(mid(fills[0])).toBeCloseTo(15, 5); // a quarter of the way down
		expect(mid(fills[1])).toBeCloseTo(45, 5); // three quarters
		// and neither lane strays into the other
		expect(Math.max(...fills[0].points.map((p) => p[1]))).toBeLessThanOrEqual(30);
		expect(Math.min(...fills[1].points.map((p) => p[1]))).toBeGreaterThanOrEqual(30);
	});

	test('the gain scales what is drawn, volume through fader', () => {
		const height = (gain: number) => {
			const { ctx, fills } = recorder();
			drawWaveform(ctx, columns(1, 20, () => [-0.4, 0.4]), opts({ width: 20, gain }));
			const ys = fills[0].points.map((p) => p[1]);
			return Math.max(...ys) - Math.min(...ys);
		};
		const unity = height(1);
		expect(height(0.5)).toBeCloseTo(unity / 2, 5);
		expect(height(0.25)).toBeCloseTo(unity / 4, 5);
		expect(height(1.5)).toBeCloseTo(unity * 1.5, 5);
	});

	test('a boosted clip peaks at the lane edge instead of overflowing it', () => {
		const { ctx, fills } = recorder();
		drawWaveform(ctx, columns(1, 20, () => [-0.9, 0.9]), opts({ width: 20, gain: 4 }));
		const ys = fills[0].points.map((p) => p[1]);
		expect(Math.min(...ys)).toBeGreaterThanOrEqual(0);
		expect(Math.max(...ys)).toBeLessThanOrEqual(40);
	});

	test('silence still draws a line a device pixel thick', () => {
		for (const dpr of [1, 2]) {
			const { ctx, fills } = recorder();
			drawWaveform(ctx, columns(1, 20, () => [0, 0]), opts({ width: 20, dpr }));
			const ys = fills[0].points.map((p) => p[1]);
			expect(Math.max(...ys) - Math.min(...ys)).toBeGreaterThanOrEqual(dpr - 1e-6);
		}
	});

	test('peaks at full scale are repainted in the clip colour, one shape per run', () => {
		const { ctx, fills } = recorder();
		const cols = columns(1, 100, (_, d) => (d >= 10 && d < 20) || d === 50 || (d >= 90 && d < 100) ? [-1, 1] : [-0.3, 0.3]);
		const clipped = drawWaveform(ctx, cols, opts());
		expect(clipped).toBe(10 + 1 + 10);
		const wave = fills.filter((f) => f.style === 'wave-colour');
		const hot = fills.filter((f) => f.style === 'clip-colour');
		expect(wave).toHaveLength(1);
		expect(hot).toHaveLength(3); // three runs, not twenty-one columns
		// the first run spans exactly columns 10..20
		const xs = hot[0].points.map((p) => p[0]);
		expect(Math.min(...xs)).toBe(10);
		expect(Math.max(...xs)).toBe(20);
		// painted after the waveform, so it ends up on top
		expect(fills.indexOf(hot[0])).toBeGreaterThan(fills.indexOf(wave[0]));
	});

	test('a clean clip paints no clip colour', () => {
		const { ctx, fills } = recorder();
		expect(drawWaveform(ctx, columns(1, 50, () => [-0.9, 0.9]), opts({ width: 50 }))).toBe(0);
		expect(fills.some((f) => f.style === 'clip-colour')).toBe(false);
	});

	test('turning a clean clip up past full scale flags it; turning a clipped one down does not hide it', () => {
		const clean = recorder();
		expect(drawWaveform(clean.ctx, columns(1, 10, () => [-0.6, 0.6]), opts({ width: 10, gain: 2 }))).toBe(10);
		const hot = recorder();
		expect(drawWaveform(hot.ctx, columns(1, 10, () => [-1, 1]), opts({ width: 10, gain: 0.25 }))).toBe(10);
	});

	test('nothing to draw draws nothing', () => {
		const { ctx, fills } = recorder();
		expect(drawWaveform(ctx, columns(1, 0, () => [0, 0]), opts({ width: 0 }))).toBe(0);
		expect(fills).toHaveLength(0);
	});

	test('no colour is a literal: the palette is the only source', () => {
		const { ctx, fills } = recorder();
		drawWaveform(ctx, columns(1, 10, (_, d) => (d < 3 ? [-1, 1] : [-0.2, 0.2])), opts({ width: 10 }));
		for (const f of fills) expect(['wave-colour', 'clip-colour']).toContain(f.style);
	});
});
