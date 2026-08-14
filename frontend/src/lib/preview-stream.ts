/* Real-time preview playback.
 *
 * The paused preview is a single composited still per seek (see Preview.svelte),
 * which is correct but costs a whole ffmpeg process per frame — a few frames a
 * second at best. For playback the backend instead keeps one long-lived ffmpeg
 * rendering the export filtergraph into an MJPEG pipe, and streams the frames
 * here over a Tauri channel.
 *
 * Timing is not ours: the audio engine already owns the clock that `ui.time`
 * follows, and ffmpeg is paced to realtime with `-re`. So this just decodes
 * frames as they land and hands the newest one to the canvas — picture chases
 * sound, exactly as the still path does. */

import { inTauri } from './api';

/** Preview render width, px. Wide enough to read the frame, small enough that
 *  the whole graph composites at this size rather than the deliverable's. */
const WIDTH = 960;
/** MJPEG `-q:v`. 2 is best, 31 worst; 7 is visually clean at preview size. */
const QUALITY = 7;

class PreviewStream {
	#bitmap: ImageBitmap | null = null;
	/** Bumped on every start/stop so late-arriving decodes from a superseded
	 *  stream can be dropped instead of flashing an out-of-date frame. */
	#generation = 0;
	#running = false;
	#decoding = false;

	/** The most recent decoded frame, or null when nothing has arrived yet. */
	get frame(): ImageBitmap | null {
		return this.#bitmap;
	}

	get running(): boolean {
		return this.#running;
	}

	/** Begin streaming from timeline time `start`. Safe to call repeatedly; each
	 *  call supersedes the previous stream. */
	async start(start: number): Promise<void> {
		if (!inTauri()) return; // no backend in the browser harness
		const mine = ++this.#generation;
		this.#running = true;
		try {
			const { invoke, Channel } = await import('@tauri-apps/api/core');
			if (this.#generation !== mine) return;
			const channel = new Channel<ArrayBuffer | number[]>();
			channel.onmessage = (msg) => void this.#onFrame(msg, mine);
			await invoke('start_preview_stream', { start, width: WIDTH, quality: QUALITY, channel });
		} catch {
			// Streaming is an enhancement: on failure the still path still draws.
			this.#running = false;
		}
	}

	async stop(): Promise<void> {
		this.#generation++;
		this.#running = false;
		this.#bitmap?.close();
		this.#bitmap = null;
		if (!inTauri()) return;
		try {
			const { invoke } = await import('@tauri-apps/api/core');
			await invoke('stop_preview_stream');
		} catch {
			/* nothing to stop */
		}
	}

	/** Decode one JPEG. Frames are dropped rather than queued while a decode is
	 *  in flight: showing the newest frame late beats showing every frame later
	 *  and later, and it bounds memory to one bitmap. */
	async #onFrame(msg: ArrayBuffer | number[], generation: number): Promise<void> {
		if (generation !== this.#generation || this.#decoding) return;
		this.#decoding = true;
		try {
			const bytes = msg instanceof ArrayBuffer ? new Uint8Array(msg) : new Uint8Array(msg);
			const bitmap = await createImageBitmap(new Blob([bytes], { type: 'image/jpeg' }));
			if (generation !== this.#generation) {
				bitmap.close();
				return;
			}
			this.#bitmap?.close();
			this.#bitmap = bitmap;
		} catch {
			/* a torn frame — the next one will do */
		} finally {
			this.#decoding = false;
		}
	}
}

export const previewStream = new PreviewStream();
