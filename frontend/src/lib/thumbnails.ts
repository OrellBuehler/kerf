// One decoded frame per asset, so a bin row shows the footage rather than an
// icon. The library remounts the bin on every tab switch, unfold and workspace
// switch, so the cache has to outlive the component: each remount would
// otherwise decode every asset again (a `get_frame` apiece).
//
// Plain TS rather than a runes singleton — nothing renders from the cache
// directly; a component copies what it finds into its own state.

export class ThumbCache {
	#done = new Map<string, string | null>();
	#inflight = new Map<string, Promise<string | null>>();

	/** What is known: the frame, `null` for "no frame" (audio, or a decoder that
	 *  answered with none, as the browser harness's does), `undefined` if never
	 *  asked — or asked and failed, which is not remembered. */
	peek(id: string): string | null | undefined {
		return this.#done.get(id);
	}

	/** The frame for `id`, decoding it with `fetch` only if nobody has: a repeat
	 *  call is answered from the cache, and a call made while the first is still
	 *  decoding shares its promise. An answer, even "no frame", is remembered; a
	 *  failure (`fetch` rejecting) is not — it resolves `null` for now and the
	 *  next mount asks again, since a decode that fell over once (a busy ffmpeg, a
	 *  file still being written) usually works the next time. */
	load(id: string, fetch: () => Promise<string | null>): Promise<string | null> {
		const done = this.#done.get(id);
		if (done !== undefined) return Promise.resolve(done);
		const pending = this.#inflight.get(id);
		if (pending) return pending;
		const p: Promise<string | null> = fetch().then(
			(url) => {
				// An asset removed while it was decoding must not come back.
				if (this.#inflight.get(id) === p) {
					this.#inflight.delete(id);
					this.#done.set(id, url);
				}
				return url;
			},
			() => {
				if (this.#inflight.get(id) === p) this.#inflight.delete(id);
				return null;
			}
		);
		this.#inflight.set(id, p);
		return p;
	}

	/** Remember "no frame" without asking (an audio asset has none to decode). */
	none(id: string) {
		this.#done.set(id, null);
	}

	/** Drop everything but the assets still in the project. */
	prune(keep: Iterable<string>) {
		const alive = new Set(keep);
		for (const id of [...this.#done.keys()]) if (!alive.has(id)) this.#done.delete(id);
		for (const id of [...this.#inflight.keys()]) if (!alive.has(id)) this.#inflight.delete(id);
	}

	get size(): number {
		return this.#done.size;
	}
}

export const thumbnails = new ThumbCache();
