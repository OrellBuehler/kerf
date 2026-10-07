// A stand-in for `get_waveform_range` in the browser harness, which has no
// decoder. The desktop app answers from a peak pyramid of the real file
// (`kerf_core::waveform_range`); this answers with a plausible, deterministic
// waveform of the same shape so the timeline's clip waveforms, lanes and
// clipping colour can all be developed under `bun run dev`.
//
// Deterministic in the strong sense: a bucket's peaks are a pure function of the
// asset id, the lane and the *absolute* source time of the 2 ms cells it covers,
// so the same window always reads the same, and two windows over the same
// footage — a different zoom, a trim that moved a few pixels — agree about the
// audio they share. Nothing flickers when a clip is re-asked for.
//
// Shaped like the engine's answer rather than like noise, so the same code paths
// get exercised: lanes are `[channel][bucket]` in -1..1 rounded to four places;
// a bucket outside the media is exactly 0 / 0; `peaks_per_second` is the coarsest
// stored level (10 / 25 / 100 / 500) that still has a source bucket per column;
// and source cells are *partitioned* among columns by where each begins, so a
// peak lands in one column instead of smearing across two.

import type { TimeRange, WaveformRange } from './types';

/** Buckets per second of every stored level, finest first (`LEVEL_RATES`). */
export const SAMPLE_LEVEL_RATES = [500, 100, 25, 10] as const;

/** The most buckets one range returns (`MAX_RANGE_BUCKETS`). */
export const SAMPLE_MAX_BUCKETS = 4096;

/** The finest level's rate: a "cell" is 2 ms, the unit the waveform is made of. */
const CELL_RATE = SAMPLE_LEVEL_RATES[0];

/** Cells sampled per column at most. A column spanning minutes would otherwise
 *  hash tens of thousands of cells; a stride over them finds the same peaks. */
const MAX_CELLS_PER_BUCKET = 24;

/** What the harness knows about an asset's audio. */
export interface SampleAudio {
	/** Any stable string; seeds the waveform so two assets do not look alike. */
	id: string;
	/** Length of the audio in seconds. */
	duration: number;
	/** 1 draws mono, anything else stereo — the engine keeps at most two lanes. */
	channels: number;
	/** Stretches the analysis found silent: they read as a noise floor. */
	silence?: TimeRange[];
	/** `music` pulses on the beat; anything else is shaped like speech. */
	kind?: 'speech' | 'music';
	/** Tempo of a `music` source; defaults to 120. */
	bpm?: number;
}

/** FNV-1a over the string, so the seed is stable across runs and engines. */
function seedOf(text: string): number {
	let h = 0x811c9dc5;
	for (let i = 0; i < text.length; i++) {
		h ^= text.charCodeAt(i);
		h = Math.imul(h, 0x01000193);
	}
	return h >>> 0;
}

/** A uniform number in [0, 1) from three integers — an integer hash, no state. */
function hash01(seed: number, a: number, b: number): number {
	let h = Math.imul(seed ^ 0x9e3779b9, 0x85ebca6b) ^ Math.imul(a + 0x7f4a7c15, 0xc2b2ae35) ^ Math.imul(b | 0, 0x27d4eb2f);
	h ^= h >>> 15;
	h = Math.imul(h, 0x2c1b3c6d);
	h ^= h >>> 12;
	h = Math.imul(h, 0x297a2d39);
	h ^= h >>> 15;
	return (h >>> 0) / 4294967296;
}

/** Four decimal places, and never `-0` (which `toEqual` tells apart from `0`). */
function round4(v: number): number {
	return Math.round(v * 10_000) / 10_000 + 0;
}

/**
 * The stretch of the sample audio that clips: a short, flat-topped burst a
 * third of the way in, so the clipping colour is always somewhere to be seen.
 */
export function clippedSpan(duration: number): TimeRange {
	const start = duration * 0.3;
	return { start, end: start + Math.min(1.5, duration * 0.05) };
}

/** Loudness of the source at `t`, before the per-cell wobble: 0 is silent, 1 full scale. */
function envelope(audio: SampleAudio, seed: number, t: number): number {
	for (const s of audio.silence ?? []) {
		if (t >= s.start && t < s.end) return 0.012;
	}
	const phrase = 0.62 + 0.38 * Math.sin(t * 0.86 + (seed % 628) / 100);
	if (audio.kind === 'music') {
		// A decaying hit on every beat over a steady bed.
		const beats = (t * (audio.bpm ?? 120)) / 60;
		const hit = Math.exp(-(beats - Math.floor(beats)) * 4);
		return Math.min(0.85, 0.5 * phrase * (0.55 + 0.45 * hit) + 0.12);
	}
	// Syllables: about four a second, with a breath between phrases.
	const syllable = Math.abs(Math.sin(t * Math.PI * 3.7 + (seed % 314) / 50)) ** 1.4;
	return Math.min(0.8, phrase * (0.18 + 0.62 * syllable));
}

/** The extremes of one 2 ms cell of one lane: `[lowest, highest]`. */
function cell(audio: SampleAudio, seed: number, lane: number, k: number, clip: TimeRange): [number, number] {
	const t = (k + 0.5) / CELL_RATE;
	if (t >= clip.start && t < clip.end) {
		// Flat-topped: most cells sit exactly on full scale (what a clipped
		// sample reads as), the rest just under it.
		const down = hash01(seed, lane * 2, k);
		const up = hash01(seed, lane * 2 + 1, k);
		return [down < 0.85 ? -1 : -(0.9 + 0.1 * down), up < 0.85 ? 1 : 0.9 + 0.1 * up];
	}
	const level = envelope(audio, seed, t) * (lane === 0 ? 1 : 0.93);
	const lo = hash01(seed, lane * 2, k);
	const hi = hash01(seed, lane * 2 + 1, k);
	return [-level * (0.6 + 0.4 * lo), level * (0.6 + 0.4 * hi)];
}

/** The coarsest stored level that is at least `perSecond` dense, else the finest. */
function pickRate(perSecond: number): number {
	for (let i = SAMPLE_LEVEL_RATES.length - 1; i >= 0; i--) {
		if (SAMPLE_LEVEL_RATES[i] >= perSecond - 1e-9) return SAMPLE_LEVEL_RATES[i];
	}
	return SAMPLE_LEVEL_RATES[0];
}

/**
 * `buckets` min/max pairs over `[start, end)` source seconds of `audio` — the
 * browser harness's `get_waveform_range`.
 *
 * Buckets are evenly spaced across the window *including* any part of it outside
 * the media, which reads 0 / 0, so the caller's time-to-column mapping stays
 * linear. `buckets` is rounded and capped at {@link SAMPLE_MAX_BUCKETS}; an
 * empty, inverted or non-finite window gives `buckets` silent ones.
 */
export function synthWaveformRange(audio: SampleAudio, start: number, end: number, buckets: number): WaveformRange {
	const count = Math.min(SAMPLE_MAX_BUCKETS, Math.max(0, Math.round(Number.isFinite(buckets) ? buckets : 0)));
	const channels = audio.channels === 1 ? 1 : 2;
	const span = end - start;
	const window = Number.isFinite(start) && Number.isFinite(end) && span > 0;
	const rate = window && count > 0 ? pickRate(count / span) : CELL_RATE;
	const out: WaveformRange = {
		channels,
		buckets: count,
		duration: audio.duration,
		peaks_per_second: rate,
		min: Array.from({ length: channels }, () => new Array<number>(count).fill(0)),
		max: Array.from({ length: channels }, () => new Array<number>(count).fill(0))
	};
	if (!window || count === 0) return out;

	const seed = seedOf(audio.id);
	const clip = clippedSpan(audio.duration);
	// Source buckets of the picked level, as cells: a level bucket spans
	// `CELL_RATE / rate` of them, and the media ends at the last partial one.
	const cellsPerBucket = CELL_RATE / rate;
	const totalCells = Math.ceil(audio.duration * CELL_RATE - 1e-6);
	const EPS = 1e-6;
	for (let j = 0; j < count; j++) {
		const s = start + (span * j) / count;
		const e = start + (span * (j + 1)) / count;
		// The level buckets this column owns: those that *begin* inside it, or
		// the one under its midpoint when it is finer than the level.
		let lo = Math.ceil(s * rate - EPS);
		let hi = Math.ceil(e * rate - EPS);
		if (hi <= lo) {
			lo = Math.floor(((s + e) * 0.5) * rate);
			hi = lo + 1;
		}
		const first = Math.max(0, lo * cellsPerBucket);
		const last = Math.min(totalCells, hi * cellsPerBucket);
		if (first >= last) continue;
		const stride = Math.max(1, Math.ceil((last - first) / MAX_CELLS_PER_BUCKET));
		for (let c = 0; c < channels; c++) {
			let low = 0;
			let high = 0;
			for (let k = first; k < last; k += stride) {
				const [l, h] = cell(audio, seed, c, k, clip);
				if (l < low) low = l;
				if (h > high) high = h;
			}
			out.min[c][j] = round4(low);
			out.max[c][j] = round4(high);
		}
	}
	return out;
}
