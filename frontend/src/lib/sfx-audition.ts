// Playing a rendered sound effect to the user's ears: decode the WAV the backend rendered (`previewSoundEffect` — the CSP has
// no `media-src` and there is no asset protocol, so the bytes come over the bridge) and play it once, on a context of its own.
// It is not the preview engine (`audio.ts`): an audition has no timeline, no tracks and no meters, and it must work with
// the transport stopped. A second press stops it, a new sound replaces the one playing, and a sound that finishes says so.
//
// The context is the browser's `AudioContext`, made on the first audition (a gesture started it, so it may run) and kept;
// tests hand in a fake.

export interface PlayerSource {
	buffer: unknown;
	onended: (() => void) | null;
	connect(destination: unknown): void;
	start(when?: number): void;
	stop(): void;
	disconnect?(): void;
}

export interface PlayerContext {
	state: string;
	destination: unknown;
	resume(): Promise<void>;
	decodeAudioData(bytes: ArrayBuffer): Promise<unknown>;
	createBufferSource(): PlayerSource;
}

export type AuditionState = 'idle' | 'playing';

export class Auditioner {
	#ctx: PlayerContext | null = null;
	#source: PlayerSource | null = null;
	#ticket = 0;
	#state: AuditionState = 'idle';
	/** Told whenever the state changes: playing began, or it stopped or ran out. */
	onchange: ((state: AuditionState) => void) | null = null;

	constructor(private readonly make: () => PlayerContext = () => new AudioContext() as unknown as PlayerContext) {}

	get state(): AuditionState {
		return this.#state;
	}

	#set(state: AuditionState) {
		if (this.#state === state) return;
		this.#state = state;
		this.onchange?.(state);
	}

	/** Play `bytes` (a WAV) from the start, replacing what is playing. Resolves once it has begun; rejects if it cannot be
	 *  decoded or the platform has no audio — the caller says so. A newer `play` or a `stop` made while this one was decoding
	 *  wins: nothing is played from this one. */
	async play(bytes: ArrayBuffer): Promise<void> {
		const ticket = ++this.#ticket;
		this.#end();
		const ctx = (this.#ctx ??= this.make());
		if (ctx.state === 'suspended') await ctx.resume().catch(() => {});
		const buffer = await ctx.decodeAudioData(bytes);
		if (ticket !== this.#ticket) return;
		const source = ctx.createBufferSource();
		source.buffer = buffer;
		source.connect(ctx.destination);
		source.onended = () => {
			if (this.#source !== source) return;
			this.#source = null;
			this.#set('idle');
		};
		this.#source = source;
		source.start();
		this.#set('playing');
	}

	/** Stop what is playing, and anything still decoding. */
	stop(): void {
		this.#ticket++;
		this.#end();
	}

	#end() {
		const source = this.#source;
		this.#source = null;
		if (source) {
			source.onended = null;
			try {
				source.stop();
			} catch {
				// Already ended: a source that has played out throws on stop.
			}
			source.disconnect?.();
		}
		this.#set('idle');
	}
}
