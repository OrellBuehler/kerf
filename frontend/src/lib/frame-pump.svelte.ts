// The Preview panel's frame fetching, out of the component so its reactivity can be tested: one
// frame in flight at a time, the latest wanted time waiting, and the answer shown as a picture
// (`frameUrl`) or as nothing at all (`gpuShown`: the native surface is what is seen).
//
// The one rule that matters: **what the effect depends on is what it reads in `run`**, and `pump`
// reads nothing reactive before its first await. A frame fetch is expensive (an FFmpeg composite),
// so an effect that re-ran because something unrelated changed (the trim monitor, the GPU status
// object being replaced, a dialog opening) would decode a frame nobody asked for. The route is read
// as its two primitives in `run` — the only parts of it that should bring a frame back — and handed
// to `pump` as values.

import type { PreviewFrameResult } from './types';

export interface FramePumpDeps {
	/** The playhead. */
	time(): number;
	/** Identity of the cut being shown: a new one means the picture may have changed. */
	timeline(): unknown;
	/** Bumped when a proxy finished generating. */
	previewEpoch(): number;
	/** How this frame is to be produced: the GPU's (asked of the backend, which may still answer with
	 *  the JPEG) or the JPEG path as it always was. */
	routeVia(): 'gpu' | 'jpeg';
	/** The page has something to draw over the picture. */
	routeOverlays(): boolean;
	/** Bumped when the backend has taken a new place for the surface. */
	boundsEpoch(): number;
	hasClips(): boolean;
	/** Playback is streaming frames into the pane: no frame is fetched meanwhile. */
	streaming(): boolean;
	timelineFrame(time: number): Promise<string | null>;
	previewFrame(time: number, overlays: boolean): Promise<PreviewFrameResult>;
	/** What the backend answered (the status bar names the renderer). */
	answered(result: PreviewFrameResult): void;
	/** The GPU path failed outright (the command rejected): the frame is the JPEG and the surface goes. */
	gpuFailed(error: unknown): void;
}

export function createFramePump(deps: FramePumpDeps) {
	let frameUrl = $state<string | null>(null);
	/** The frame on show was drawn in the native surface: nothing is painted over it. */
	let gpuShown = $state(false);
	/** The aspect of the picture on show (the canvas the compositor drew, or the JPEG). */
	let imgAspect = $state<number | null>(null);
	let inFlight = false;
	let queued: { time: number; via: 'gpu' | 'jpeg'; overlays: boolean } | null = null;

	// Single-flight decode: only ever one composite in flight, and `queued` always holds the
	// *latest* wanted frame. Scrubbing collapses to one render + one pending target instead of a
	// backlog of stale frames that must all drain before the frame under the cursor appears.
	async function pump() {
		if (inFlight || queued === null) return;
		const want = queued;
		queued = null;
		inFlight = true;
		try {
			if (want.via === 'gpu') {
				// The GPU preview is on: the backend draws the frame in the native surface when the plan
				// allows it (and says so), else it hands back FFmpeg's JPEG for this frame.
				let result: PreviewFrameResult | null = null;
				try {
					result = await deps.previewFrame(want.time, want.overlays);
				} catch (error) {
					// The command itself failed (a panic in the GPU path, an error building the inputs): this
					// frame is the JPEG, and the surface must not stay over it.
					deps.gpuFailed(error);
					const url = await deps.timelineFrame(want.time);
					if (url && !deps.streaming()) {
						frameUrl = url;
						gpuShown = false;
					}
				}
				// Playback may have taken over while this was decoding.
				if (result && !deps.streaming()) {
					deps.answered(result);
					if (result.renderer === 'gpu') {
						gpuShown = true;
						// The canvas the compositor drew has the delivery's shape.
						if (result.timings?.width && result.timings.height) imgAspect = result.timings.width / result.timings.height;
					} else if (result.frame) {
						gpuShown = false;
						frameUrl = result.frame;
					}
				}
			} else {
				// The *composited* timeline still — every visible clip with its color, effects, transform
				// and overlays applied, so Inspector edits show up live. (Desktop only — null in the
				// browser.)
				const url = await deps.timelineFrame(want.time);
				// Playback may have taken over while this was decoding; a still landing on top of live
				// frames would show as a stutter.
				if (url && !deps.streaming()) {
					frameUrl = url;
					gpuShown = false;
				}
			}
		} catch {
			/* ignore decode errors — keep the last good frame */
		}
		inFlight = false;
		if (queued !== null) void pump(); // a newer target arrived mid-decode — go to it
	}

	return {
		get frameUrl() {
			return frameUrl;
		},
		get gpuShown() {
			return gpuShown;
		},
		get imgAspect() {
			return imgAspect;
		},
		/** A frame of the playback stream: the picture, never the surface. */
		showStreamed(jpeg: string) {
			gpuShown = false;
			frameUrl = jpeg;
		},
		/** The picture on screen is not the surface's any more (the setting went off). */
		surfaceGone() {
			gpuShown = false;
		},
		/** The `<img>` loaded: the aspect it has. */
		pictureAspect(aspect: number) {
			imgAspect = aspect;
		},
		/**
		 * The body of the effect that keeps the frame in step with the playhead *and* the edit state:
		 * every playhead move, every timeline change (an Inspector edit reassigns it), a proxy that
		 * became ready, a different route and a surface that moved. Suspended while the stream is
		 * feeding frames, so the two do not fight over the pane.
		 */
		run() {
			const time = deps.time();
			void deps.timeline();
			void deps.previewEpoch();
			const via = deps.routeVia();
			// Whether the page draws over the picture only matters to the backend: on the JPEG path (the
			// setting off, a dialog over the frame) it is not read, so it cannot bring a frame back.
			const overlays = via === 'gpu' && deps.routeOverlays();
			void deps.boundsEpoch();
			if (!deps.hasClips()) {
				frameUrl = null;
				gpuShown = false;
				queued = null;
				return;
			}
			if (deps.streaming()) return;
			queued = { time, via, overlays };
			void pump();
		}
	};
}
