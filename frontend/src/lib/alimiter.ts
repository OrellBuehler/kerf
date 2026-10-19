/* The master limiter of the preview: FFmpeg's `alimiter` (`libavfilter/af_alimiter.c`, the same
 * from 4.4 to 9.0 but for `latency`) ported sample for sample, as the export spells it
 * (`limiter_filter` in kerf-core: `alimiter=limit=…:attack=5:release=100:level=0[:latency=1]`, so
 * no auto-level, no auto-release, unity in and out). It runs in an AudioWorklet
 * (`limiter-worklet.ts`), and it is pure, so `alimiter.test.ts` replays what the real filter
 * wrote for the same signals (`fixtures/alimiter-corpus.json`, written by kerf-core's
 * `alimiter_corpus.rs` through the export's own filter text).
 *
 * Every step is the C's, in its order and in doubles, integer arithmetic where the C's is
 * integer, so the output is the filter's bit for bit. The one thing a stream cannot do is the
 * export's `latency=1`, which drops the first `delay` frames of the output and pads the end: live,
 * the output trails the input by `delay` frames (5 ms), which the picture is told about
 * (`AudioEngine.outputLatency`). */

import { MASTER_DEFAULT_CEILING_DB, clampCeiling } from './levels';

/** The export's `attack`, milliseconds. */
export const LIMITER_ATTACK_MS = 5;
/** The export's `release`, milliseconds. */
export const LIMITER_RELEASE_MS = 100;

/** The name the worklet registers its processor under (`limiter-worklet.ts`). */
export const LIMITER_PROCESSOR = 'kerf-alimiter';

/** What the main thread posts to the worklet (and builds it with): the linear limit. */
export interface LimiterMessage {
	limit: number;
}

/** `MasterBus::limit_linear` rounded as `limiter_filter` writes it (six decimals), which is
 *  the number `alimiter` parses. A ceiling that is not a number is the default one. */
export function limiterLimit(ceilingDb: number): number {
	const db = Number.isFinite(ceilingDb) ? clampCeiling(ceilingDb) : MASTER_DEFAULT_CEILING_DB;
	const limit = 10 ** (db / 20);
	return Math.round(limit * 1e6) / 1e6;
}

/** The lookahead in interleaved samples, a whole number of frames, as the C rounds it (`buffer_size`). */
export function alimiterBufferSize(sampleRate: number, channels: number, attackMs = LIMITER_ATTACK_MS): number {
	const size = Math.trunc(sampleRate * (attackMs / 1000) * channels);
	return size - (size % channels);
}

/** Seconds the limiter's output trails its input live: the frames `latency=1` trims. */
export function alimiterDelay(sampleRate: number, channels = 2): number {
	const size = alimiterBufferSize(sampleRate, channels);
	return size > 0 ? (size / channels - 1) / sampleRate : 0;
}

const fmax = (a: number, b: number) => (a > b ? a : b);
const fmin = (a: number, b: number) => (a > b ? b : a);
const trunc = Math.trunc;

/** One `alimiter` instance: its state is the C's `AudioLimiterContext`. */
export class Alimiter {
	readonly channels: number;
	readonly sampleRate: number;
	/** The lookahead in interleaved samples, a whole number of frames (`buffer_size`). */
	readonly bufferSize: number;
	/** Frames the output trails the input (`latency=1` trims as many). */
	readonly delay: number;
	limit: number;
	readonly #release: number;
	readonly #buffer: Float64Array;
	readonly #nextpos: Int32Array;
	readonly #nextdelta: Float64Array;
	/** A frame of output before its clip, in doubles (an output array may be single precision). */
	readonly #dst: Float64Array;
	#pos = 0;
	#att = 1;
	#delta = 0;
	#nextiter = 0;
	#nextlen = 0;

	constructor(sampleRate: number, channels: number, limit: number, attackMs = LIMITER_ATTACK_MS, releaseMs = LIMITER_RELEASE_MS) {
		this.sampleRate = sampleRate;
		this.channels = channels;
		this.limit = limit;
		this.#release = releaseMs / 1000;
		const size = alimiterBufferSize(sampleRate, channels, attackMs);
		if (size <= 0) throw new Error('Attack is too small.');
		this.bufferSize = size;
		this.delay = size / channels - 1;
		const obuffer = trunc((sampleRate * channels) / 10) + channels;
		const room = Math.max(obuffer, size + channels);
		this.#buffer = new Float64Array(room);
		this.#nextdelta = new Float64Array(room);
		this.#nextpos = new Int32Array(room).fill(-1);
		this.#dst = new Float64Array(channels);
	}

	/** Limit `frames` frames of `input` (one array per channel; a missing channel is silence) into
	 *  `output` (one per channel, which may be the input's own). */
	process(input: readonly (ArrayLike<number> | undefined)[], output: { [i: number]: number }[], frames: number): void {
		const channels = this.channels;
		const bufferSize = this.bufferSize;
		const buffer = this.#buffer;
		const nextpos = this.#nextpos;
		const nextdelta = this.#nextdelta;
		const release = this.#release;
		const limit = this.limit;
		const sampleRate = this.sampleRate;
		const dst = this.#dst;

		for (let n = 0; n < frames; n++) {
			let peak = 0;
			for (let c = 0; c < channels; c++) {
				const sample = input[c]?.[n] ?? 0;
				buffer[this.#pos + c] = sample;
				peak = fmax(peak, Math.abs(sample));
			}

			if (peak > limit) {
				const patt = fmin(limit / peak, 1);
				const rdelta = (1.0 - patt) / (sampleRate * release);
				const delta = ((limit / peak - this.#att) / bufferSize) * channels;
				let found = false;

				if (delta < this.#delta) {
					this.#delta = delta;
					nextpos[0] = this.#pos;
					nextpos[1] = -1;
					nextdelta[0] = rdelta;
					this.#nextlen = 1;
					this.#nextiter = 0;
				} else {
					let i: number;
					for (i = this.#nextiter; i < this.#nextiter + this.#nextlen; i++) {
						const j = i % bufferSize;
						let ppeak = 0;
						if (nextpos[j] >= 0) for (let c = 0; c < channels; c++) ppeak = fmax(ppeak, Math.abs(buffer[nextpos[j] + c]));
						const pdelta = (limit / peak - limit / ppeak) / trunc(((bufferSize - nextpos[j] + this.#pos) % bufferSize) / channels);
						if (pdelta < nextdelta[j]) {
							nextdelta[j] = pdelta;
							found = true;
							break;
						}
					}
					if (found) {
						this.#nextlen = i - this.#nextiter + 1;
						nextpos[(this.#nextiter + this.#nextlen) % bufferSize] = this.#pos;
						nextdelta[(this.#nextiter + this.#nextlen) % bufferSize] = rdelta;
						nextpos[(this.#nextiter + this.#nextlen + 1) % bufferSize] = -1;
						this.#nextlen++;
					}
				}
			}

			const out = (this.#pos + channels) % bufferSize;
			peak = 0;
			for (let c = 0; c < channels; c++) peak = fmax(peak, Math.abs(buffer[out + c]));

			this.#att += this.#delta;

			for (let c = 0; c < channels; c++) dst[c] = buffer[out + c] * this.#att;

			if (out === nextpos[this.#nextiter]) {
				this.#delta = nextdelta[this.#nextiter];
				this.#att = limit / peak;
				this.#nextlen -= 1;
				nextpos[this.#nextiter] = -1;
				this.#nextiter = (this.#nextiter + 1) % bufferSize;
			}

			if (this.#att > 1) {
				this.#att = 1;
				this.#delta = 0;
				this.#nextiter = 0;
				this.#nextlen = 0;
				nextpos[0] = -1;
			}

			if (this.#att <= 0) {
				this.#att = 0.0000000000001;
				this.#delta = (1.0 - this.#att) / (sampleRate * release);
			}

			if (this.#att !== 1 && 1 - this.#att < 0.0000000000001) this.#att = 1;

			if (this.#delta !== 0 && Math.abs(this.#delta) < 0.00000000000001) this.#delta = 0;

			for (let c = 0; c < channels; c++) output[c][n] = fmin(fmax(dst[c], -limit), limit);

			this.#pos = out;
		}
	}
}
