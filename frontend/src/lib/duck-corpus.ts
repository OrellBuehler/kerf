/* The differential corpus for the preview's ducking (`duck.ts`): seeded keys put through the real `ffmpeg` with the
 * export's own duck chains — read out of `audio_graph.rs` / `filter_graph.rs`, so a chain that moved there is the
 * chain written here — and the gain each put on a constant bus, sampled every `STRIDE` samples. `duck.test.ts`
 * replays `fixtures/duck-corpus.json` through `Ducker` and fails, naming the case, when the two disagree.
 *
 * Regenerate after an intended change, with the ffmpeg the export is blessed against (`crates/kerf-app/binaries`, or
 * `KERF_FFMPEG`): `KERF_BLESS_DUCK=1 bun test src/lib/duck.test.ts` (not `KERF_BLESS_CORPUS`, which is the Rust corpora's).
 *
 * The keys are written as segments both sides build the same way, from an LCG on `u32` and a round to float32 — no `sin`,
 * whose last digit differs between libms. This module has no test of its own: `duck.test.ts` uses it. */

import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gateCut, type DuckMode } from './duck';

export const RATE = 48000;
/** One recorded gain every this many samples (250 Hz): the gain is smooth at that scale — its fastest move is the compressor's 20 ms. */
export const STRIDE = 192;
/** The last stretch of an output that is not recorded or compared: its length is not stable from one run of ffmpeg to the next. */
export const TAIL_MARGIN_S = 0.25;

/** A stretch of key: `seconds` of noise whose left and right amplitudes are `l` and `r`. */
export type Segment = [seconds: number, l: number, r: number];

export interface Case {
	name: string;
	mode: DuckMode;
	/** The chain, as the export writes it, with its input pads. */
	filter: string;
	/** The key, and how long the ducked bus is (a constant 1) in seconds. */
	key: Segment[];
	bus_seconds: number;
	/** The gain at every `STRIDE`th frame of the output, linear, up to `TAIL_MARGIN_S` short of the bus's end: ffmpeg drops a
	 *  few whole frames off the end of a sidechain filter's output and not always the same number, so the tail is not recorded. */
	gain: number[];
}

export interface Corpus {
	about: string;
	ffmpeg: string;
	rate: number;
	stride: number;
	cases: Case[];
}

/** The key's samples, interleaved stereo, as the generator and the replay both make them. */
export function keySamples(segments: Segment[], seed = 7): Float32Array {
	let state = seed >>> 0;
	const rnd = () => {
		state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
		return (state / 4294967296) * 2 - 1;
	};
	const frames = segments.reduce((n, [s]) => n + Math.round(s * RATE), 0);
	const out = new Float32Array(frames * 2);
	let at = 0;
	for (const [seconds, l, r] of segments) {
		for (let i = 0, n = Math.round(seconds * RATE); i < n; i++, at++) {
			const w = rnd();
			out[2 * at] = l * w;
			out[2 * at + 1] = r * (0.7 * w + 0.3 * rnd());
		}
	}
	return out;
}

/** What the corpus plays: a key that steps through silence, speech, a murmur, a shout and back, and variants. */
const STEPS: Segment[] = [
	[0.5, 0, 0],
	[1.0, 0.3, 0.25],
	[0.8, 0.02, 0.02],
	[1.2, 0.8, 0.7],
	[1.5, 0.05, 0.06],
	[1.0, 0, 0],
	[0.5, 0.15, 0.1],
	[1.5, 0, 0]
];
/** Levels around the compressor's threshold and its knee, and a one-sided key. */
const KNEE: Segment[] = [
	[0.4, 0, 0],
	[0.8, 0.06, 0.06],
	[0.8, 0.09, 0.09],
	[0.8, 0.13, 0.13],
	[0.8, 0.25, 0],
	[0.8, 0, 0.25],
	[0.8, 0.5, 0.5],
	[1.2, 0.03, 0.03]
];
const SHORT: Segment[] = [
	[1.0, 0.25, 0.25],
	[2.0, 0, 0]
];
const BUS = (segments: Segment[]) => segments.reduce((n, [s]) => n + s, 0);

export interface Chains {
	/** The compressor's chain, key padded, as the export writes it (`[akside]apad[akpad]` into `[aduck][akpad]sidechaincompress=…[aducked]`):
	 *  the text the corpus was written with. When the Rust has not padded the key yet this is the unpadded chain's options in the padded shape. */
	compressor: string;
	/** Whether the Rust source pads the compressor's key yet (`fix/duck-key-end` in the engine). */
	compressorPadded: boolean;
	/** `duck_gate_chains` with `{cut}` filled and no stereo upmix, between the pads `[aduck]` / `[akside]` and `[aducked]`. */
	gate: (depthDb: number) => string;
}

/** The pad the padded compressor chain is spelled with in the engine. */
const PAD = 'akpad';

/** The export's duck chains, read from the Rust. A source that has neither the padded compressor chain nor the unpadded one in this
 *  shape throws. */
export function chainsFromRust(root: string): Chains {
	const filterGraph = readFileSync(join(root, 'crates/kerf-core/src/engine/cli/filter_graph.rs'), 'utf8');
	const audioGraph = readFileSync(join(root, 'crates/kerf-core/src/engine/cli/audio_graph.rs'), 'utf8');
	let compressor: string;
	let compressorPadded: boolean;
	const pad = /"(\[akside\]apad\[(\w+)\])"/.exec(filterGraph);
	const padded = pad && new RegExp(`"(\\[aduck\\]\\[${pad[2]}\\]sidechaincompress=[^"]*\\[aducked\\])"`).exec(filterGraph);
	if (pad && padded) {
		compressor = `${pad[1]};${padded[1]}`;
		compressorPadded = true;
	} else {
		const bare = /"\[aduck\]\[akside\](sidechaincompress=[^"]*)\[aducked\]"/.exec(filterGraph)?.[1];
		if (!bare) throw new Error("filter_graph.rs no longer has the compressor's duck chain in a shape duck-corpus.ts reads");
		compressor = `[akside]apad[${PAD}];[aduck][${PAD}]${bare}[aducked]`;
		compressorPadded = false;
	}
	const start = audioGraph.indexOf('fn duck_gate_chains(');
	const body = audioGraph.slice(start, audioGraph.indexOf('\n}\n', start));
	const lines = [...body.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]).filter((l) => l.includes('['));
	if (lines.length === 0 || !lines.some((l) => l.includes('{cut:.6}'))) throw new Error("audio_graph.rs no longer has duck_gate_chains in the shape duck-corpus.ts reads");
	return {
		compressor,
		compressorPadded,
		gate: (depthDb) =>
			lines.map((l) => l.replace('{cut:.6}', gateCut(depthDb).toFixed(6)).replace('{upmix}', '')).join(';')
	};
}

/** The ffmpeg the corpus is blessed with, or `null`. */
export function blessedFfmpeg(root: string): string | null {
	if (process.env.KERF_FFMPEG) return process.env.KERF_FFMPEG;
	const dir = join(root, 'crates/kerf-app/binaries');
	if (!existsSync(dir)) return null;
	const found = readdirSync(dir).find((n) => /^ffmpeg-/.test(n) && !n.endsWith('.sha256'));
	return found ? join(dir, found) : null;
}

/** Put `key` (stereo) and a constant-1 bus of `bus_seconds` through `filter` (in the export's pads), and read the output. */
function render(ffmpeg: string, filter: string, key: Float32Array, busSeconds: number): Float32Array {
	const dir = mkdtempSync(join(tmpdir(), 'kerf-duck-corpus-'));
	try {
		const bus = new Float32Array(Math.round(busSeconds * RATE)).fill(1);
		writeFileSync(join(dir, 'bus.raw'), Buffer.from(bus.buffer));
		writeFileSync(join(dir, 'key.raw'), Buffer.from(key.buffer));
		const args = ['-hide_banner', '-loglevel', 'error', '-y'];
		args.push('-f', 'f32le', '-ar', String(RATE), '-ac', '1', '-i', join(dir, 'bus.raw'));
		args.push('-f', 'f32le', '-ar', String(RATE), '-ac', '2', '-i', join(dir, 'key.raw'));
		args.push('-filter_complex', `[0:a]anull[aduck];[1:a]anull[akside];${filter};[aducked]anull[out]`);
		args.push('-map', '[out]', '-f', 'f32le', '-ar', String(RATE), '-ac', '1', join(dir, 'out.raw'));
		const r = spawnSync(ffmpeg, args, { encoding: 'utf8', timeout: 120_000 });
		if (r.status !== 0) throw new Error(`ffmpeg failed: ${r.stderr}`);
		const bytes = readFileSync(join(dir, 'out.raw'));
		return new Float32Array(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

/** The corpus, written through the real ffmpeg. */
export function writeCorpus(root: string, ffmpeg: string): Corpus {
	const chains = chainsFromRust(root);
	const version = spawnSync(ffmpeg, ['-version'], { encoding: 'utf8' }).stdout.split('\n')[0];
	const specs: { name: string; mode: DuckMode; key: Segment[]; bus: number }[] = [
		{ name: 'compressor: a key that steps through silence, speech, a murmur and a shout', mode: { kind: 'compressor' }, key: STEPS, bus: BUS(STEPS) },
		{ name: 'compressor: levels around the threshold and the knee, and a one-sided key', mode: { kind: 'compressor' }, key: KNEE, bus: BUS(KNEE) },
		{ name: 'compressor: the key is padded, so the bus plays to its end when the key stops', mode: { kind: 'compressor' }, key: SHORT, bus: 6 },
		{ name: 'gate -12 dB: the same steps', mode: { kind: 'gate', depthDb: -12 }, key: STEPS, bus: BUS(STEPS) },
		{ name: 'gate -6 dB: the same steps', mode: { kind: 'gate', depthDb: -6 }, key: STEPS, bus: BUS(STEPS) },
		{ name: 'gate -30 dB: levels around the threshold', mode: { kind: 'gate', depthDb: -30 }, key: KNEE, bus: BUS(KNEE) },
		{ name: 'gate -12 dB: the key is padded, so the bus runs on when the key stops', mode: { kind: 'gate', depthDb: -12 }, key: SHORT, bus: 6 }
	];
	const cases = specs.map((s): Case => {
		const filter = s.mode.kind === 'compressor' ? chains.compressor : chains.gate(s.mode.depthDb);
		const out = render(ffmpeg, filter, keySamples(s.key), s.bus);
		// The output has to run to the bus's end less the tail: a key that is not padded ends it early, and that is what a recorded gain would hide.
		const frames = Math.floor((Math.round(s.bus * RATE) - Math.round(TAIL_MARGIN_S * RATE)) / STRIDE) * STRIDE;
		if (out.length < frames) throw new Error(`${s.name}: ffmpeg wrote ${out.length} frames, fewer than the ${frames} the bus runs to — its key is not padded`);
		const gain: number[] = [];
		for (let i = 0; i < frames; i += STRIDE) gain.push(Math.round(out[i] * 1e6) / 1e6);
		return { name: s.name, mode: s.mode, filter, key: s.key, bus_seconds: s.bus, gain };
	});
	return {
		about: 'Written through the real ffmpeg by duck-corpus.ts, which reads the duck chains out of audio_graph.rs and filter_graph.rs; replayed by duck.test.ts. Regenerate: KERF_BLESS_DUCK=1 bun test src/lib/duck.test.ts',
		ffmpeg: version,
		rate: RATE,
		stride: STRIDE,
		cases
	};
}

/** The fixture's path. */
export const corpusPath = (root: string) => join(root, 'frontend/src/lib/fixtures/duck-corpus.json');
