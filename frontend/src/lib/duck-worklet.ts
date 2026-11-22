/* The preview's ducking as an AudioWorklet processor: `Ducker` (the export's `sidechaincompress` / speech gate)
 * on the bus of the tracks flagged Duck, keyed by the rest of the mix. Two stereo inputs — 0 the ducked bus, 1 the key —
 * and one stereo output, the bus times the gain the key asks for. Bundled by Vite into a script of its own
 * (`duck-worklet-url.ts`, `?worker&url`) and served from the app's origin, which the CSP's `script-src 'self'` admits.
 *
 * The main thread posts the mode (a depth change glides in, so a hand on the depth slider is heard as a move and
 * not as a step) and `reset` when playback starts. The processor posts the gain it is putting on the bus, a few times a second, for the
 * Mixer's meter. It adds no latency: what it reads and writes is the same render quantum, so the picture's clock and the
 * output latency are as they were. */

import { DUCK_GLIDE_MS, DUCK_PROCESSOR, Ducker, duckMode, type DuckMessage, type DuckReport } from './duck';

/** Blocks (128 frames) between reports: about 20 ms at 48 kHz. */
const REPORT_EVERY = 8;

declare const sampleRate: number;
declare class AudioWorkletProcessor {
	readonly port: MessagePort;
}
declare function registerProcessor(name: string, ctor: new (options: AudioWorkletNodeOptions) => AudioWorkletProcessor): void;

class DuckProcessor extends AudioWorkletProcessor {
	readonly #ducker: Ducker;
	#blocks = 0;
	#low = 1;

	constructor(options: AudioWorkletNodeOptions) {
		super();
		const mode = (options.processorOptions as DuckMessage | undefined)?.mode ?? duckMode({});
		this.#ducker = new Ducker(sampleRate, mode, 1, DUCK_GLIDE_MS);
		this.port.onmessage = (e: MessageEvent<DuckMessage>) => {
			const m = e.data;
			if (m?.mode) this.#ducker.configure(m.mode);
			if (m?.reset) {
				this.#ducker.reset();
				this.#low = 1;
			}
		};
	}

	process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
		const out = outputs[0];
		if (!out?.length) return true;
		const bus = inputs[0] ?? [];
		const key = inputs[1] ?? [];
		const frames = out[0].length;
		const kl = key[0];
		// A mono key is both sides.
		const kr = key[1] ?? kl;
		const ducker = this.#ducker;
		for (let n = 0; n < frames; n++) {
			const gain = ducker.step(kl?.[n] ?? 0, kr?.[n] ?? 0);
			if (gain < this.#low) this.#low = gain;
			for (let c = 0; c < out.length; c++) out[c][n] = (bus[c]?.[n] ?? 0) * gain;
		}
		if (++this.#blocks >= REPORT_EVERY) {
			this.#blocks = 0;
			this.port.postMessage({ gain: this.#low } satisfies DuckReport);
			this.#low = 1;
		}
		return true;
	}
}

registerProcessor(DUCK_PROCESSOR, DuckProcessor);
