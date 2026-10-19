/* The preview's master limiter as an AudioWorklet processor: `Alimiter` (the export's `alimiter`,
 * sample for sample) on the stereo master. Bundled by Vite into a script of its own
 * (`limiter-worklet-url.ts`, `?worker&url`) and served from the app's origin, which the CSP's
 * `script-src 'self'` admits — no `blob:` or `data:` module. The main thread posts the limit
 * (`limiterLimit` of the ceiling) whenever it changes. */

import { Alimiter, LIMITER_PROCESSOR, type LimiterMessage } from './alimiter';

// The AudioWorkletGlobalScope's own, which `lib.dom` does not declare.
declare const sampleRate: number;
declare class AudioWorkletProcessor {
	readonly port: MessagePort;
}
declare function registerProcessor(name: string, ctor: new (options: AudioWorkletNodeOptions) => AudioWorkletProcessor): void;

class LimiterProcessor extends AudioWorkletProcessor {
	#lim: Alimiter;

	constructor(options: AudioWorkletNodeOptions) {
		super();
		const limit = (options.processorOptions as LimiterMessage | undefined)?.limit ?? 1;
		this.#lim = new Alimiter(sampleRate, 2, limit);
		this.port.onmessage = (e: MessageEvent<LimiterMessage>) => {
			if (Number.isFinite(e.data?.limit) && e.data.limit > 0) this.#lim.limit = e.data.limit;
		};
	}

	process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
		const out = outputs[0];
		if (out?.length) this.#lim.process(inputs[0] ?? [], out, out[0].length);
		return true;
	}
}

registerProcessor(LIMITER_PROCESSOR, LimiterProcessor);
