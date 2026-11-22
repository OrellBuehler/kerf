import { describe, expect, test } from 'bun:test';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import corpus from './fixtures/duck-corpus.json';
import {
	COMPRESSOR,
	COMPRESSOR_ENDS_WITH_KEY,
	duckDb,
	Ducker,
	duckEnvelope,
	duckMode,
	GATE,
	GATE_KEY_FLOOR,
	GATE_SMOOTH_HZ,
	gateCut,
	MONO_FOLD,
	type DuckMode
} from './duck';
import { blessedFfmpeg, chainsFromRust, corpusPath, keySamples, RATE, STRIDE, writeCorpus, type Case } from './duck-corpus';

const ROOT = join(import.meta.dir, '../../..');
const dbOf = (g: number) => 20 * Math.log10(g);

/** The gain `mode` puts on a bus, per sample, for the stereo key `k`. */
function gains(mode: DuckMode, k: Float32Array, frames = k.length / 2): Float32Array {
	const d = new Ducker(RATE, mode);
	const out = new Float32Array(frames);
	// Past the end of the key is silence, as the export's padded key is.
	for (let i = 0; i < frames; i++) out[i] = d.step(k[2 * i] ?? 0, k[2 * i + 1] ?? 0);
	return out;
}

/** Mono tone as a stereo key: `amp` at `hz` from `from` to `to` seconds of `total`. */
function tone(total: number, from: number, to: number, hz: number, amp: number): Float32Array {
	const k = new Float32Array(Math.round(total * RATE) * 2);
	for (let n = Math.round(from * RATE); n < Math.round(to * RATE); n++) {
		const v = Math.fround(amp * Math.sin((2 * Math.PI * hz * n) / RATE));
		k[2 * n] = v;
		k[2 * n + 1] = v;
	}
	return k;
}

const meanDb = (g: Float32Array, a: number, b: number) => {
	let s = 0;
	const from = Math.round(a * RATE);
	const to = Math.round(b * RATE);
	for (let i = from; i < to; i++) s += g[i];
	return dbOf(s / (to - from));
};

// ---- the Rust it mirrors ---------------------------------------------------------------------------------

describe('the numbers are the export’s', () => {
	const chains = chainsFromRust(ROOT);
	const filterGraph = readFileSync(join(ROOT, 'crates/kerf-core/src/engine/cli/filter_graph.rs'), 'utf8');
	const gate = chains.gate(-12);
	const opt = (chain: string, key: string) => Number(new RegExp(`${key}=([0-9.]+)`).exec(chain)?.[1]);

	test('the compressor is sidechaincompress at the threshold, ratio, attack and release written in filter_graph.rs', () => {
		expect(opt(chains.compressor, 'threshold')).toBe(COMPRESSOR.threshold);
		expect(opt(chains.compressor, 'ratio')).toBe(COMPRESSOR.ratio);
		expect(opt(chains.compressor, 'attack')).toBe(COMPRESSOR.attackMs);
		expect(opt(chains.compressor, 'release')).toBe(COMPRESSOR.releaseMs);
		// Its knee, link and detection are the filter's defaults, which the chain must not set.
		expect(chains.compressor).not.toMatch(/knee|link|detection|makeup|mix|level_/);
	});

	test('the gate is sidechaingate at the threshold, ratio, attack and release, smoothed at 7 and 1.2 Hz, with the floor, written in audio_graph.rs', () => {
		const sg = /sidechaingate=[^,\[]*/.exec(gate)![0];
		expect(opt(sg, 'threshold')).toBe(GATE.threshold);
		expect(opt(sg, 'range')).toBe(GATE.range);
		expect(opt(sg, 'ratio')).toBe(GATE.ratio);
		expect(opt(sg, 'attack')).toBe(GATE.attackMs);
		expect(opt(sg, 'release')).toBe(GATE.releaseMs);
		expect(sg).toContain('knee=1');
		expect(sg).not.toMatch(/detection|link|makeup|level_/);
		expect([...gate.matchAll(/lowpass=f=([0-9.]+):p=1/g)].map((m) => Number(m[1]))).toEqual([GATE_SMOOTH_HZ.fast, GATE_SMOOTH_HZ.slow]);
		expect(gate).toContain(`val(0)+${GATE_KEY_FLOOR.toFixed(6)}`);
		expect(gate).toContain('aformat=channel_layouts=mono');
		expect(gate).toContain("aeval='1-0.748811*max(val(0),val(1))'");
		expect(gate).toContain('[aduckdry][again]amultiply[aducked]');
	});

	test('the compressor path pads no key, which is why its output ends with it — and the gate pads its own', () => {
		// The one `apad` in the export's sound is the gate's: when the compressor's key gets one too, the preview must stop cutting the bed.
		expect(/apad/.test(filterGraph)).toBe(!COMPRESSOR_ENDS_WITH_KEY);
		expect(chains.gate(-12)).toContain('apad');
		expect(chains.compressor).not.toContain('apad');
	});

	test('the speech gate’s depth is the gain it leaves: 1 - cut', () => {
		expect(gateCut(-12)).toBe(0.748811);
		expect(1 - gateCut(-12)).toBeCloseTo(10 ** (-12 / 20), 6);
		expect(gateCut(-40)).toBe(0.99);
		expect(gateCut(-1)).toBe(0.108749);
	});

	test('a master with no depth ducks with the compressor; a depth is held to -40..-1 and a stray one is the default', () => {
		expect(duckMode({})).toEqual({ kind: 'compressor' });
		expect(duckMode({ duck_depth_db: null as never })).toEqual({ kind: 'compressor' });
		expect(duckMode({ duck_depth_db: -9 })).toEqual({ kind: 'gate', depthDb: -9 });
		expect(duckMode({ duck_depth_db: -90 })).toEqual({ kind: 'gate', depthDb: -40 });
		expect(duckMode({ duck_depth_db: 3 })).toEqual({ kind: 'gate', depthDb: -1 });
		expect(duckMode({ duck_depth_db: Number.NaN })).toEqual({ kind: 'gate', depthDb: -12 });
	});

	test('the mono fold is the float of 1/sqrt(2), as swresample has it', () => {
		expect(MONO_FOLD).toBe(0.7071067690849304);
	});
});

// ---- the real filter, replayed ---------------------------------------------------------------------------

describe('the real ffmpeg’s ducking, replayed', () => {
	const cases = corpus.cases as unknown as Case[];

	test('the corpus is for this chain text, the one the Rust writes', () => {
		const chains = chainsFromRust(ROOT);
		for (const c of cases) {
			const want = c.mode.kind === 'compressor' ? chains.compressor : chains.gate(c.mode.depthDb);
			expect(c.filter, c.name).toBe(want);
		}
		expect(corpus.rate).toBe(RATE);
		expect(corpus.stride).toBe(STRIDE);
		expect(cases.length).toBe(7);
	});

	for (const c of cases) {
		test(c.name, () => {
			const g = gains(c.mode, keySamples(c.key), c.gain.length * STRIDE);
			let worst = 0;
			let at = 0;
			c.gain.forEach((want, i) => {
				const diff = Math.abs(dbOf(g[i * STRIDE]) - dbOf(want));
				if (diff > worst) {
					worst = diff;
					at = (i * STRIDE) / RATE;
				}
			});
			// The compressor's arithmetic is the filter's, in doubles: what is left is the float32 the filter writes. The gate's
			// gain goes through two float32 filters and, at -30 dB, a gain of 0.03 where a float's step is a larger part of a dB.
			const limit = c.mode.kind === 'compressor' ? 0.001 : 0.06;
			expect(worst, `${c.name}: worst ${worst.toFixed(4)} dB at ${at.toFixed(3)} s`).toBeLessThan(limit);
		});
	}

	test('where the output ends: with the key for the compressor, with the bus for the gate', () => {
		const frames = (s: number) => Math.round(s * RATE);
		const byName = (re: RegExp) => cases.find((c) => re.test(c.name))!;
		const comp = byName(/ends with the key/);
		const pad = byName(/padded/);
		// ffmpeg writes whole frames: within one of them.
		expect(Math.abs(comp.out_frames - frames(3))).toBeLessThanOrEqual(1024);
		expect(Math.abs(pad.out_frames - frames(6))).toBeLessThanOrEqual(1024);
		expect(comp.out_frames).toBeLessThan(frames(6) - 4096);
	});
});

// ---- the Rust tests' numbers -----------------------------------------------------------------------------

describe('the speech gate ducks by its depth and recovers (the numbers of the_speech_gate_ducks_by_its_depth_and_recovers)', () => {
	// A 220 Hz bed for 6 s under 2 s of a 1 kHz voice at 2 s: the bed is down by the depth, within half a dB, while the voice
	// speaks, and back at unity, within half a dB, after it.
	const key = tone(6, 2, 4, 1000, 0.2);
	for (const depth of [-12, -6, -20]) {
		test(`${depth} dB`, () => {
			const g = gains({ kind: 'gate', depthDb: depth }, key);
			expect(meanDb(g, 0.5, 1.5)).toBeCloseTo(0, 1);
			expect(Math.abs(meanDb(g, 2.5, 3.5) - depth)).toBeLessThan(0.5);
			expect(Math.abs(meanDb(g, 5.0, 5.9))).toBeLessThan(0.5);
		});
	}

	test('it eases down in about 50 ms and back up over about 300 ms after the voice stops — never a step', () => {
		const g = gains({ kind: 'gate', depthDb: -12 }, key);
		const at = (t: number) => dbOf(g[Math.round(t * RATE)]);
		expect(at(2.0)).toBeGreaterThan(-0.5);
		// Onset: the fast one-pole (7 Hz), most of the way in 50 ms and all of it in 150.
		expect(at(2.05)).toBeLessThan(-6);
		expect(at(2.15)).toBeLessThan(-11.5);
		// Held through the release's hangover, then back up with the slow one.
		expect(at(4.1)).toBeLessThan(-10);
		expect(at(4.5)).toBeGreaterThan(-9);
		expect(at(5.2)).toBeGreaterThan(-0.5);
		// No sample-to-sample step bigger than the smoothing allows.
		let step = 0;
		for (let i = 1; i < g.length; i++) step = Math.max(step, Math.abs(g[i] - g[i - 1]));
		expect(step).toBeLessThan(1e-3);
	});

	test('a key that says nothing does not close the gate on a bus, whatever its floor: it never ducks', () => {
		const g = gains({ kind: 'gate', depthDb: -12 }, new Float32Array(2 * RATE));
		expect(Math.min(...g)).toBe(1);
	});
});

describe('the compressor', () => {
	/** A key held at `amp` on both sides: its detection power is `amp` squared, with nothing for the smoothing to settle on but that. */
	const steady = (amp: number) => gains({ kind: 'compressor' }, new Float32Array(4 * RATE * 2).fill(amp));

	test('leaves a quiet key alone and dips by more the louder the key is — eight to one over -26 dBFS', () => {
		const level = (amp: number) => meanDb(steady(amp), 3, 4);
		expect(level(0.01)).toBe(0);
		expect(level(0.5)).toBeLessThan(level(0.2));
		expect(level(0.2)).toBeLessThan(level(0.08));
		expect(level(0.08)).toBeLessThan(level(0.04));
		// Far over the knee: out = thres + (in - thres) / 8, so the gain is (in - thres) * (1/8 - 1) in dB. 0.5 is -6.02 dBFS,
		// 20 dB over the threshold: 17.5 dB down.
		const over = 20 * Math.log10(0.5) - 20 * Math.log10(COMPRESSOR.threshold);
		expect(level(0.5)).toBeCloseTo(-over * (1 - 1 / COMPRESSOR.ratio), 3);
		// At the threshold itself it is inside the soft knee: a little down.
		expect(level(0.05)).toBeLessThan(-0.1);
		expect(level(0.05)).toBeGreaterThan(-2);
	});

	test('attacks in about 20 ms and lets go over about 400 ms', () => {
		const g = gains({ kind: 'compressor' }, tone(3, 0.5, 1.5, 1000, 0.5));
		const at = (t: number) => dbOf(g[Math.round(t * RATE)]);
		const settled = at(1.4);
		expect(settled).toBeLessThan(-12);
		expect(at(0.5)).toBeGreaterThan(-0.5);
		expect(at(0.52)).toBeGreaterThan(settled * 0.9 - 4);
		expect(at(1.5 + 0.05)).toBeLessThan(settled / 2);
		expect(at(2.5)).toBeGreaterThan(-0.5);
	});

	test('reads both channels of the key as the mean of their magnitudes: a key on one side dips less than the same on both', () => {
		const both = gains({ kind: 'compressor' }, new Float32Array(Array.from({ length: 2 * RATE }, (_, i) => (i % 2 === 0 ? 0.4 : 0.4))));
		const one = gains({ kind: 'compressor' }, new Float32Array(Array.from({ length: 2 * RATE }, (_, i) => (i % 2 === 0 ? 0.4 : 0))));
		expect(dbOf(one.at(-1)!)).toBeGreaterThan(dbOf(both.at(-1)!));
		// 0.4 on both sides is a link average of 0.4; one side of 0.4 is 0.2.
		const alone = gains({ kind: 'compressor' }, new Float32Array(Array.from({ length: 2 * RATE }, () => 0.2)));
		expect(one.at(-1)).toBeCloseTo(alone.at(-1)!, 9);
	});
});

describe('a Ducker', () => {
	test('starts over on reset, as the export starts a file', () => {
		const d = new Ducker(RATE, { kind: 'gate', depthDb: -12 });
		const k = tone(1, 0, 1, 1000, 0.3);
		const first = Array.from({ length: 4000 }, (_, i) => d.step(k[2 * i], k[2 * i + 1]));
		d.reset();
		const again = Array.from({ length: 4000 }, (_, i) => d.step(k[2 * i], k[2 * i + 1]));
		expect(again).toEqual(first);
	});

	test('a new depth glides in — a move, not a step — and the state it has heard stays', () => {
		const d = new Ducker(RATE, { kind: 'gate', depthDb: -12 }, 1, 20);
		const k = tone(2, 0, 2, 1000, 0.3);
		let last = 1;
		for (let i = 0; i < RATE; i++) last = d.step(k[2 * i], k[2 * i + 1]);
		expect(dbOf(last)).toBeCloseTo(-12, 1);
		d.configure({ kind: 'gate', depthDb: -24 });
		let prev = last;
		let biggest = 0;
		const after: number[] = [];
		for (let i = RATE; i < RATE + 4800; i++) {
			const g = d.step(k[2 * i], k[2 * i + 1]);
			biggest = Math.max(biggest, Math.abs(g - prev));
			prev = g;
			after.push(g);
		}
		expect(biggest).toBeLessThan(2e-4);
		expect(dbOf(after[48])).toBeGreaterThan(-14);
		expect(dbOf(after.at(-1)!)).toBeCloseTo(-24, 0);
	});

	test('a different kind of mode starts afresh', () => {
		const d = new Ducker(RATE, { kind: 'compressor' });
		d.configure({ kind: 'gate', depthDb: -12 });
		expect(d.mode).toEqual({ kind: 'gate', depthDb: -12 });
		expect(d.step(0, 0)).toBe(1);
	});
});

describe('the lane overlay runs the same machine over a power envelope', () => {
	/** A key that is constant within each `hop`, stepping from one hop to the next: `amps[h]` on both sides. */
	function stepped(amps: number[], hop: number): Float32Array {
		const per = Math.round(hop * RATE);
		const k = new Float32Array(amps.length * per * 2);
		amps.forEach((a, h) => k.fill(a, h * per * 2, (h + 1) * per * 2));
		return k;
	}
	const AMPS = [0, 0, 0.3, 0.3, 0.3, 0.3, 0.02, 0.02, 0.8, 0.8, 0.8, 0.05, 0.05, 0, 0, 0, 0.2, 0.2, 0.2, 0.2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

	test('one sample to a step is the machine itself', () => {
		for (const mode of [{ kind: 'compressor' }, { kind: 'gate', depthDb: -12 }] as DuckMode[]) {
			const k = stepped(AMPS, 0.01);
			const exact = gains(mode, k);
			const power = Float32Array.from({ length: k.length / 2 }, (_, i) => {
				const a = mode.kind === 'compressor' ? (Math.abs(k[2 * i]) + Math.abs(k[2 * i + 1])) * 0.5 : (k[2 * i] + k[2 * i + 1]) * MONO_FOLD;
				return a * a;
			});
			const env = duckEnvelope(power, 1 / RATE, mode, RATE);
			for (let i = 0; i < env.length; i += 997) expect(env[i]).toBeCloseTo(exact[i], 5);
		}
	});

	test('a hop of 10 ms lands the compressor’s gain exactly where the samples do at the hop’s end, for a key that holds through each hop', () => {
		const hop = 0.01;
		const k = stepped(AMPS, hop);
		const exact = gains({ kind: 'compressor' }, k);
		const env = duckEnvelope(AMPS.map((a) => a * a), hop, { kind: 'compressor' }, RATE);
		const per = Math.round(hop * RATE);
		AMPS.forEach((_, h) => expect(env[h]).toBeCloseTo(exact[(h + 1) * per - 1], 6));
	});

	test('and the gate’s to within a hop of where the samples put it', () => {
		const hop = 0.01;
		const mode: DuckMode = { kind: 'gate', depthDb: -12 };
		const k = stepped(AMPS, hop);
		const exact = gains(mode, k);
		const per = Math.round(hop * RATE);
		// The key as the machine reads it — folded, with the floor — and the hop's power is that, squared.
		const env = duckEnvelope(AMPS.map((a) => (2 * a * MONO_FOLD) ** 2), hop, mode, RATE);
		let worst = 0;
		AMPS.forEach((_, h) => {
			let best = Infinity;
			for (const s of [-1, 0, 1]) best = Math.min(best, Math.abs(duckDb(env[h]) - duckDb(exact[Math.min(exact.length - 1, Math.max(0, (h + 1 + s) * per - 1))])));
			worst = Math.max(worst, best);
		});
		expect(worst).toBeLessThan(0.5);
	});

	test('a gate with a floor never ducks on silence, and the compressor never on a quiet key', () => {
		expect(Math.min(...duckEnvelope(new Float32Array(500), 0.01, { kind: 'gate', depthDb: -12 }))).toBe(1);
		expect(Math.min(...duckEnvelope(new Float32Array(500).fill(0.01 * 0.01), 0.01, { kind: 'compressor' }))).toBe(1);
	});
});

// ---- blessing ---------------------------------------------------------------------------------------------

describe('blessing the corpus', () => {
	test.skipIf(!process.env.KERF_BLESS_CORPUS)('writes fixtures/duck-corpus.json through the real ffmpeg', () => {
		const ffmpeg = blessedFfmpeg(ROOT);
		if (!ffmpeg) throw new Error('no ffmpeg: put the blessed one in crates/kerf-app/binaries or set KERF_FFMPEG');
		writeFileSync(corpusPath(ROOT), `${JSON.stringify(writeCorpus(ROOT, ffmpeg))}\n`);
	});
});
