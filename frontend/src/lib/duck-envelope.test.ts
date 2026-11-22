import { describe, expect, test } from 'bun:test';
import { Ducker, duckDb, MONO_FOLD, reductionDb, type DuckMode } from './duck';
import { bucketPeak, cutAt, deepest, dipPath, duckCurve, HOP, keyPower, PEAK_POWER_SCALE, type Peaks } from './duck-envelope';
import { duckPlan } from './duck-plan';
import type { Clip, Timeline, Track } from './types';

const RATE = 48000;

const clip = (id: string, asset_id: string, extra: Partial<Clip> = {}): Clip =>
	({ id, asset_id, source_in: 0, source_out: 10, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0, ...extra }) as Clip;
const track = (id: string, kind: 'video' | 'audio', clips: Clip[], extra: Partial<Track> = {}): Track =>
	({ id, kind, name: id.toUpperCase(), clips, ...extra }) as Track;
const SOUND = new Set(['voice', 'music']);

/** Peaks of a window that holds `level` in every bucket, mono. */
const flat = (level: number, seconds: number, t0 = 0): Peaks => {
	const n = Math.ceil(seconds * 100);
	return { t0, rate: 100, max: [new Array(n).fill(level)], min: [new Array(n).fill(-level)] };
};

describe('reading a key off a window of peaks', () => {
	test('a bucket is the larger magnitude of its extremes, the mean over the channels; outside the window is silence', () => {
		const p: Peaks = { t0: 0, rate: 100, max: [[0.2, 0.1], [0.4, 0.3]], min: [[-0.3, -0.1], [-0.2, -0.5]] };
		expect(bucketPeak(p, 0)).toBeCloseTo((0.3 + 0.4) / 2, 12);
		expect(bucketPeak(p, 1)).toBeCloseTo((0.1 + 0.5) / 2, 12);
		expect(bucketPeak(p, 2)).toBe(0);
		expect(bucketPeak(p, -1)).toBe(0);
	});

	const tl = (extra: Partial<Track> = {}, master?: Timeline['master']): Timeline => ({
		tracks: [track('v1', 'video', [clip('voice', 'voice', { source_out: 4, timeline_start: 1 })], extra), track('a1', 'audio', [clip('bed', 'music')], { duck: true })],
		master
	});
	const plan = (t: Timeline) => duckPlan(t, SOUND);

	test('each hop of a keyed clip carries its detection power: the peak through the mono the mode reads, scaled', () => {
		const t = tl();
		const p = keyPower(t, plan(t), (c) => (c.id === 'voice' ? flat(0.2, 4) : null), 6);
		expect(p.length).toBe(600);
		expect(p[50]).toBe(0);
		expect(p[150]).toBeCloseTo(0.2 * 0.2 * PEAK_POWER_SCALE, 6);
		expect(p[500]).toBe(0);
		const g = tl(undefined, { volume: 1, limiter: false, ceiling_db: -1.5, duck_depth_db: -12 });
		const q = keyPower(g, plan(g), (c) => (c.id === 'voice' ? flat(0.2, 4) : null), 6);
		// The gate reads the mono fold, which is 3 dB up on a centred key.
		expect(q[150]).toBeCloseTo((0.4 * MONO_FOLD) ** 2 * PEAK_POWER_SCALE, 6);
	});

	test('the key is the clip as it plays: its volume, its fades, the track’s fader and pan — and speed moves where the peaks are read', () => {
		const t: Timeline = {
			tracks: [
				track('v1', 'video', [clip('voice', 'voice', { source_out: 4, timeline_start: 0, volume: 0.5, fade_in: 1 })], { volume: 0.5, pan: 1 }),
				track('a1', 'audio', [clip('bed', 'music')], { duck: true })
			]
		};
		const peaks = flat(0.4, 4);
		const p = keyPower(t, plan(t), (c) => (c.id === 'voice' ? peaks : null), 4);
		// Past the fade: 0.4 × 0.5 (clip) × 0.5 (fader), panned right: the left leg is 0, the right at unity.
		expect(p[200]).toBeCloseTo(((0.4 * 0.5 * 0.5) / 2) ** 2 * PEAK_POWER_SCALE, 6);
		// In the fade-in it is lower still.
		expect(p[10]).toBeLessThan(p[200] / 10);
		// At 2x the clip is half as long and reads its window twice as fast.
		const fast: Timeline = { tracks: [track('v1', 'video', [clip('voice', 'voice', { source_out: 4, speed: 2 })]), track('a1', 'audio', [clip('bed', 'music')], { duck: true })] };
		const ramp: Peaks = { t0: 0, rate: 100, max: [Array.from({ length: 400 }, (_, i) => (i < 200 ? 0.1 : 0.4))], min: [Array.from({ length: 400 }, (_, i) => -(i < 200 ? 0.1 : 0.4))] };
		const q = keyPower(fast, plan(fast), () => ramp, 4);
		expect(q[50]).toBeCloseTo(0.1 * 0.1 * PEAK_POWER_SCALE, 6);
		expect(q[150]).toBeCloseTo(0.4 * 0.4 * PEAK_POWER_SCALE, 6);
		expect(q[250]).toBe(0);
	});

	test('a disabled clip, a ducked track’s own sound and a window not fetched yet key nothing', () => {
		const t: Timeline = {
			tracks: [
				track('v1', 'video', [clip('voice', 'voice', { enabled: false }), clip('other', 'voice', { timeline_start: 5 })]),
				track('a1', 'audio', [clip('bed', 'music')], { duck: true })
			]
		};
		const p = keyPower(t, plan(t), (c) => (c.id === 'other' ? null : flat(0.5, 10)), 10);
		expect(Math.max(...p)).toBe(0);
	});
});

// ---- how near the prediction is to the machine on the samples -----------------------------------------------

/** A key that speaks: a harmonic stack on a syllabic rhythm, words with pauses between, and a room's tone under them. */
function speech(seconds: number, seed = 5): Float32Array {
	const k = new Float32Array(Math.round(seconds * RATE) * 2);
	let state = seed >>> 0;
	const rnd = () => {
		state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
		return (state / 4294967296) * 2 - 1;
	};
	// Words: [from, to, loudness] in seconds.
	const words: [number, number, number][] = [[0.6, 1.5, 0.22], [1.75, 2.2, 0.1], [2.5, 3.9, 0.3], [4.6, 5.2, 0.05], [5.4, 6.4, 0.18]];
	for (let n = 0; n < k.length / 2; n++) {
		const t = n / RATE;
		let x = 0.003 * rnd();
		for (const [a, b, amp] of words) {
			if (t < a || t >= b) continue;
			const edge = Math.min(1, (t - a) / 0.03, (b - t) / 0.05);
			const syl = 0.65 + 0.35 * Math.sin(2 * Math.PI * 4.2 * (t - a));
			let stack = 0;
			for (let h = 1; h <= 7; h++) stack += Math.sin(2 * Math.PI * 130 * h * t) / h;
			x += amp * edge * syl * (stack / 1.6);
		}
		const v = Math.fround(x);
		k[2 * n] = v;
		k[2 * n + 1] = v;
	}
	return k;
}

function peaksOf(k: Float32Array): Peaks {
	const per = Math.round(HOP * RATE);
	const n = Math.floor(k.length / 2 / per);
	const max: number[][] = [[], []];
	const min: number[][] = [[], []];
	for (let h = 0; h < n; h++) {
		for (let c = 0; c < 2; c++) {
			let hi = -1;
			let lo = 1;
			for (let i = h * per; i < (h + 1) * per; i++) {
				hi = Math.max(hi, k[2 * i + c]);
				lo = Math.min(lo, k[2 * i + c]);
			}
			max[c].push(hi);
			min[c].push(lo);
		}
	}
	return { t0: 0, rate: 1 / HOP, max, min };
}

describe('the prediction against the machine on the samples (a speaking key, its peaks at 100 a second)', () => {
	const k = speech(8);
	const peaks = peaksOf(k);
	const seconds = 8;
	const timeline = (master?: Timeline['master']): Timeline => ({
		tracks: [track('v1', 'video', [clip('voice', 'voice', { source_out: seconds })]), track('a1', 'audio', [clip('bed', 'music', { source_out: seconds })], { duck: true })],
		master
	});

	function compare(mode: DuckMode, master?: Timeline['master']) {
		const t = timeline(master);
		const plan = duckPlan(t, SOUND);
		expect(plan.mode).toEqual(mode);
		const curve = duckCurve(t, plan, () => peaks, seconds);
		const exact = new Ducker(RATE, mode);
		const per = Math.round(HOP * RATE);
		const g = new Float32Array(k.length / 2);
		for (let i = 0; i < g.length; i++) g[i] = exact.step(k[2 * i], k[2 * i + 1]);
		let sum = 0;
		let n = 0;
		let worst = 0;
		let slackWorst = 0;
		for (let h = 0; h < curve.length; h++) {
			const want = duckDb(g[Math.min(g.length - 1, (h + 1) * per - 1)]);
			const got = duckDb(curve[h]);
			// Within 30 ms either side: where an edge lands is a hop or two out.
			let best = Infinity;
			for (let s = -3; s <= 3; s++) best = Math.min(best, Math.abs(got - duckDb(g[Math.min(g.length - 1, Math.max(0, (h + 1 + s) * per - 1))])));
			sum += got - want;
			n++;
			worst = Math.max(worst, Math.abs(got - want));
			slackWorst = Math.max(slackWorst, best);
		}
		return { bias: sum / n, worst, slackWorst, deepestWant: Math.max(...g.map((x) => -duckDb(x))), deepestGot: deepest(curve) };
	}

	test('the compressor: within half a dB on average, the same deepest dip to within 2 dB, and no hop more than 2.5 dB from the exact curve of 30 ms either side', () => {
		const r = compare({ kind: 'compressor' });
		expect(Math.abs(r.bias)).toBeLessThan(0.5);
		expect(Math.abs(r.deepestGot - r.deepestWant)).toBeLessThan(2);
		expect(r.slackWorst).toBeLessThan(2.5);
	});

	for (const depth of [-12, -6, -20]) {
		test(`the speech gate at ${depth} dB: the same edges to within a hop or two`, () => {
			const r = compare({ kind: 'gate', depthDb: depth }, { volume: 1, limiter: false, ceiling_db: -1.5, duck_depth_db: depth });
			expect(Math.abs(r.bias)).toBeLessThan(0.3);
			expect(Math.abs(r.deepestGot - r.deepestWant)).toBeLessThan(0.1);
			expect(r.slackWorst).toBeLessThan(1.6);
		});
	}
});

describe('drawing the dip', () => {
	test('a reduction in dB, and the deepest dip of a window', () => {
		expect(reductionDb(1)).toBe(0);
		expect(reductionDb(10 ** (-12 / 20))).toBeCloseTo(12, 6);
		const c = Float32Array.from([1, 0.5, 0.25, 1]);
		expect(deepest(c)).toBeCloseTo(12.04, 2);
		expect(deepest(c, 0, 0.02)).toBeCloseTo(6.02, 2);
	});

	test('the path runs along the top, down by the reduction over 24 dB, and a flat run is two points', () => {
		const c = new Float32Array(100).fill(1);
		for (let h = 20; h < 60; h++) c[h] = 10 ** (-12 / 20);
		const d = dipPath(c, 0, 1);
		expect(d.startsWith('M0.000,0 L0.000,0')).toBe(true);
		expect(d.endsWith('Z')).toBe(true);
		// 12 dB of 24: half-way down.
		expect(d).toContain('0.200,0.5');
		// A long flat stretch is not a point per hop.
		expect(d.split(' L').length).toBeLessThan(12);
		expect(dipPath(c, 5, 5)).toBe('');
	});

	test('the compressor path ends where its key does — drawn where that is before the track’s end', () => {
		expect(cutAt({ keyEnd: 6 }, 10)).toBe(6);
		expect(cutAt({ keyEnd: 10 }, 10)).toBeNull();
		expect(cutAt({ keyEnd: 12 }, 10)).toBeNull();
		expect(cutAt({ keyEnd: null }, 10)).toBeNull();
	});
});
