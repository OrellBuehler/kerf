import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/alimiter-corpus.json';
import { Alimiter, alimiterDelay, LIMITER_ATTACK_MS, LIMITER_RELEASE_MS, limiterLimit } from './alimiter';

// The differential test for the port of FFmpeg's `alimiter`: kerf-core put each signal through the
// real ffmpeg with the export's own limiter text (`engine/cli/alimiter_corpus.rs` →
// `fixtures/alimiter-corpus.json`) and every case is replayed here, to the bit — a long case by the
// FNV-1a of its output's bytes (`out_fnv1a`), the rest sample by sample. Regenerate with
// `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- --ignored alimiter_corpus`
// (the variable is shared: it re-blesses the links corpus too when that test runs).

type Shape = { kind: 'dc' } | { kind: 'tri'; period: number } | { kind: 'noise'; seed: number };

interface Segment {
	frames: number;
	shape: Shape;
	from: [number, number];
	to: [number, number];
}

interface Case {
	name: string;
	rate: number;
	ceiling_db: number;
	filter: string;
	segments: Segment[];
	/** The output, left and right — or, for a long case, its length and hash. */
	out?: [number[], number[]];
	frames?: number;
	out_fnv1a?: string;
}

/** `fnv1a` in `alimiter_corpus.rs`: FNV-1a (32-bit) over interleaved little-endian doubles. */
function fnv1a(l: Float64Array, r: Float64Array): string {
	const both = new Float64Array(l.length * 2);
	for (let i = 0; i < l.length; i++) {
		both[2 * i] = l[i];
		both[2 * i + 1] = r[i];
	}
	const view = new DataView(both.buffer);
	let hash = 0x811c9dc5;
	for (let i = 0; i < both.length; i++) {
		const at = i * 8;
		// The bytes of each double, little-endian, whatever the machine's own order.
		const lo = view.getUint32(at, true);
		const hi = view.getUint32(at + 4, true);
		for (const word of [lo, hi])
			for (let b = 0; b < 4; b++) {
				hash ^= (word >>> (8 * b)) & 0xff;
				hash = Math.imul(hash, 0x01000193) >>> 0;
			}
	}
	return hash.toString(16).padStart(8, '0');
}

const cases = corpus.cases as unknown as Case[];

/** `signal` in `alimiter_corpus.rs`: the same segments, the same arithmetic. */
function signal(segments: Segment[]): [number[], number[]] {
	const l: number[] = [];
	const r: number[] = [];
	for (const s of segments) {
		let state = s.shape.kind === 'noise' ? s.shape.seed : 0;
		const lcg = () => {
			state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
			return (state / 4294967296) * 2 - 1;
		};
		for (let n = 0; n < s.frames; n++) {
			const amp = (a: number, b: number) => a + ((b - a) * n) / s.frames;
			let sl = 1;
			let sr = 1;
			if (s.shape.kind === 'tri') sl = sr = 4 * Math.abs((n % s.shape.period) / s.shape.period - 0.5) - 1;
			else if (s.shape.kind === 'noise') {
				sl = lcg();
				sr = lcg();
			}
			l.push(amp(s.from[0], s.to[0]) * sl);
			r.push(amp(s.from[1], s.to[1]) * sr);
		}
	}
	return [l, r];
}

/** What `latency=1` makes of the stream: the input followed by `delay` frames of silence, its first
 *  `delay` frames of output dropped. */
function limitedAligned(c: Case, lim = new Alimiter(c.rate, 2, limiterLimit(c.ceiling_db))): [Float64Array, Float64Array] {
	const [l, r] = signal(c.segments);
	const frames = l.length + lim.delay;
	const inL = Float64Array.from({ length: frames }, (_, i) => l[i] ?? 0);
	const inR = Float64Array.from({ length: frames }, (_, i) => r[i] ?? 0);
	const outL = new Float64Array(frames);
	const outR = new Float64Array(frames);
	// In blocks of a render quantum, as the worklet runs it.
	for (let at = 0; at < frames; at += 128) {
		const n = Math.min(128, frames - at);
		lim.process([inL.subarray(at), inR.subarray(at)], [outL.subarray(at), outR.subarray(at)], n);
	}
	return [outL.slice(lim.delay), outR.slice(lim.delay)];
}

describe('alimiter corpus', () => {
	test('has cases at four rates, a 0 dB ceiling and long ones kept as hashes', () => {
		expect(new Set(cases.map((c) => c.rate))).toEqual(new Set([8000, 44100, 48000, 96000]));
		expect(cases.some((c) => c.ceiling_db === 0)).toBe(true);
		expect(cases.filter((c) => c.out_fnv1a).length).toBeGreaterThanOrEqual(2);
	});

	test('the long 48 kHz case takes the gain back past unity many times (the `att > 1` reset)', () => {
		const c = cases.find((x) => x.name.startsWith('at 48 kHz, long'))!;
		const lim = new Alimiter(c.rate, 2, limiterLimit(c.ceiling_db));
		limitedAligned(c, lim);
		expect(lim.resets).toBeGreaterThanOrEqual(12);
	});

	for (const c of cases) {
		test(c.name, () => {
			expect(c.filter).toBe(`alimiter=limit=${limiterLimit(c.ceiling_db)}:attack=${LIMITER_ATTACK_MS}:release=${LIMITER_RELEASE_MS}:level=0:latency=1`);
			const [l, r] = limitedAligned(c);
			if (c.out_fnv1a) {
				expect(l.length).toBe(c.frames!);
				expect(fnv1a(l, r)).toBe(c.out_fnv1a);
				return;
			}
			const out = c.out!;
			for (const [side, got, want] of [
				['left', l, out[0]],
				['right', r, out[1]]
			] as const) {
				expect(got.length).toBe(want.length);
				const first = got.findIndex((v, i) => !Object.is(v, want[i]));
				expect(first === -1 ? null : { side, frame: first, got: got[first], want: want[first] }).toBeNull();
			}
		});
	}
});

describe('Alimiter', () => {
	test('the lookahead is the attack in whole frames, as the C rounds it', () => {
		expect(new Alimiter(48000, 2, 1).delay).toBe(239);
		expect(new Alimiter(44100, 2, 1).bufferSize).toBe(440);
		expect(new Alimiter(8000, 2, 1).bufferSize).toBe(80);
		for (const rate of [8000, 44100, 48000, 96000]) expect(alimiterDelay(rate)).toBe(new Alimiter(rate, 2, 1).delay / rate);
	});

	test('a signal under the ceiling comes out as it went in, late by the lookahead', () => {
		const lim = new Alimiter(8000, 2, 0.5);
		const input = Float64Array.from({ length: 200 }, (_, i) => ((i % 9) - 4) / 10);
		const out = [new Float64Array(200), new Float64Array(200)];
		lim.process([input, input], out, 200);
		expect(Array.from(out[0].subarray(lim.delay))).toEqual(Array.from(input.subarray(0, 200 - lim.delay)));
		expect(Array.from(out[1].subarray(0, lim.delay)).every((v) => v === 0)).toBe(true);
	});

	test('nothing passes the ceiling, and a missing channel is silence', () => {
		const lim = new Alimiter(8000, 2, 0.25);
		const input = Float64Array.from({ length: 400 }, (_, i) => (i % 2 ? 1 : -1) * (i / 100));
		const out = [new Float64Array(400), new Float64Array(400)];
		lim.process([input, undefined], out, 400);
		expect(Math.max(...out[0].map(Math.abs))).toBeLessThanOrEqual(0.25);
		expect(out[1].every((v) => v === 0)).toBe(true);
	});

	test('a reset is a new instance, bit for bit, made in the arrays it has — even mid-limit', () => {
		const loud = cases.find((x) => x.name.startsWith('noise pushed far'))!;
		const next = cases.find((x) => x.name.startsWith('a burst rides up'))!;
		const reused = new Alimiter(next.rate, 2, limiterLimit(-12));
		// Stop halfway through limiting something hot, so every part of the state is in use.
		const [l, r] = signal(loud.segments);
		const half = Math.floor(l.length / 2);
		reused.process([Float64Array.from(l), Float64Array.from(r)], [new Float64Array(half), new Float64Array(half)], half);
		reused.reset(limiterLimit(next.ceiling_db));
		const fresh = new Alimiter(next.rate, 2, limiterLimit(next.ceiling_db));
		const [al, ar] = limitedAligned(next, reused);
		const [bl, br] = limitedAligned(next, fresh);
		expect(Array.from(al)).toEqual(Array.from(bl));
		expect(Array.from(ar)).toEqual(Array.from(br));
		expect(reused.resets).toBe(fresh.resets);
	});

	test('the limit is the ceiling as the export writes it', () => {
		expect(limiterLimit(-1.5)).toBe(0.841395);
		expect(limiterLimit(Number.NaN)).toBe(0.841395);
		expect(limiterLimit(12)).toBe(1);
		expect(limiterLimit(-90)).toBe(0.063096);
	});
});
