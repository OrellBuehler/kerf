import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/alimiter-corpus.json';
import { Alimiter, LIMITER_ATTACK_MS, LIMITER_RELEASE_MS, limiterLimit } from './alimiter';

// The differential test for the port of FFmpeg's `alimiter`: kerf-core put each signal through the
// real ffmpeg with the export's own limiter text (`engine/cli/alimiter_corpus.rs` →
// `fixtures/alimiter-corpus.json`) and every case is replayed here, to the bit. Regenerate with
// `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- --ignored alimiter_corpus`.

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
	out: [number[], number[]];
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
function limitedAligned(c: Case): [Float64Array, Float64Array] {
	const [l, r] = signal(c.segments);
	const lim = new Alimiter(c.rate, 2, limiterLimit(c.ceiling_db));
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
	test('has cases at three rates', () => {
		expect(new Set(cases.map((c) => c.rate))).toEqual(new Set([8000, 44100, 48000]));
	});
	for (const c of cases) {
		test(c.name, () => {
			expect(c.filter).toBe(`alimiter=limit=${limiterLimit(c.ceiling_db)}:attack=${LIMITER_ATTACK_MS}:release=${LIMITER_RELEASE_MS}:level=0:latency=1`);
			const [l, r] = limitedAligned(c);
			for (const [side, got, want] of [
				['left', l, c.out[0]],
				['right', r, c.out[1]]
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

	test('the limit is the ceiling as the export writes it', () => {
		expect(limiterLimit(-1.5)).toBe(0.841395);
		expect(limiterLimit(Number.NaN)).toBe(0.841395);
		expect(limiterLimit(12)).toBe(1);
		expect(limiterLimit(-90)).toBe(0.063096);
	});
});
