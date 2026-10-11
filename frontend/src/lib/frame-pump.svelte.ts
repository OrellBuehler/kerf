// The Preview panel's frame fetching, out of the component so its reactivity can be tested: on the
// JPEG route one frame in flight at a time with the latest wanted time waiting, on the GPU route a
// request per wanted time (the backend keeps only the newest: `get_preview_frame` is latest-wins and
// answers `superseded` for the ones it replaced), and the answer shown as a picture
// (`frameUrl`), as nothing at all (`gpuShown`: the native surface is what is seen) or, with the canvas
// painter on (`CANVAS_PAINTER`), as the last frame of the playback stream on a canvas of its own (`streamShown`).
//
// The one rule that matters: **what the effect depends on is what it reads in `run`**, and `pump`
// reads nothing reactive before its first await. A frame fetch is expensive (an FFmpeg composite),
// so an effect that re-ran because something unrelated changed (the trim monitor, the GPU status
// object being replaced, a dialog opening) would decode a frame nobody asked for. The route is read
// as its two primitives in `run` — the only parts of it that should bring a frame back — and handed
// to `pump` as values.

import { isProxyWaitMessage } from './proxy-wait';
import type { PreviewFrameResult, PreviewOrder } from './types';

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
	/** How wide a picture is asked for, px — the frame's box at the screen's pixel ratio, in the
	 *  steps of `preview-size.ts` — or null until the frame has been measured: nothing is asked
	 *  for before that, so a first frame is not decoded at a guess and again at the real size. */
	width(): number | null;
	timelineFrame(time: number, width: number): Promise<string | null>;
	/** `order` says which request of this pump it is, so the backend can tell a request that arrived
	 *  after a newer one (the calls are separate tasks over the IPC) and not draw it. */
	previewFrame(time: number, overlays: boolean, width: number, order: PreviewOrder): Promise<PreviewFrameResult>;
	/** What the backend answered (the status bar names the renderer). */
	answered(result: PreviewFrameResult): void;
	/** The GPU path failed outright (the command rejected): the frame is the JPEG and the surface goes. */
	gpuFailed(error: unknown): void;
}

export function createFramePump(deps: FramePumpDeps) {
	let frameUrl = $state<string | null>(null);
	/** The frame on show was drawn in the native surface: nothing is painted over it. */
	let gpuShown = $state(false);
	/** The frame on show is the playback stream's, drawn on the canvas: the picture until a fetched
	 *  frame lands on top of it. */
	let streamShown = $state(false);
	/** The aspect of the picture on show (the canvas the compositor drew, or the JPEG). */
	let imgAspect = $state<number | null>(null);
	let inFlight = false;
	let queued: { time: number; via: 'gpu' | 'jpeg'; overlays: boolean; width: number } | null = null;
	/** Requests made so far, and the newest one whose answer is on show: an answer older than that is
	 *  dropped (the GPU route has several in flight, and the JPEG route may have one beside them). */
	let asked = 0;
	let shown = 0;
	/** This pump's name to the backend, which numbers by it: a reloaded page or a second window starts
	 *  counting again from zero without being behind the first. */
	const client = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;

	/** Show a JPEG that is the newest answer so far. */
	function showUrl(url: string | null, seq: number) {
		if (!url || deps.streaming() || seq < shown) return;
		shown = seq;
		frameUrl = url;
		gpuShown = false;
		streamShown = false;
	}

	// The GPU route is not single-flight: the backend draws the newest request and tells the ones it
	// replaced (`superseded`), so a drag asks for every position the playhead passes and the renderer
	// never waits for a round trip between two frames. The backend draws in the native surface when the
	// plan allows it (and says so), else it hands back FFmpeg's JPEG for the frame.
	async function askGpu(want: { time: number; overlays: boolean; width: number }, seq: number) {
		let result: PreviewFrameResult | null = null;
		try {
			result = await deps.previewFrame(want.time, want.overlays, want.width, { client, seq });
		} catch (error) {
			// Under "Proxy only" the backend refuses a frame whose proxy is still building. That is
			// the preview waiting, not the GPU path failing: the picture on show (and the surface,
			// if it is up) stays as it is until the proxy lands and the frame is asked for again.
			if (isProxyWaitMessage(error)) return;
			// The command itself failed (a panic in the GPU path, an error building the inputs): this
			// frame is the JPEG, and the surface must not stay over it.
			deps.gpuFailed(error);
			try {
				showUrl(await deps.timelineFrame(want.time, want.width), seq);
			} catch {
				/* keep the last good frame */
			}
			return;
		}
		// A newer request took this one's place before it was drawn: that request's answer is the
		// one to show.
		if (result.renderer === 'superseded') return;
		// Playback may have taken over while this was decoding, or a newer frame is already on show.
		if (deps.streaming() || seq < shown) return;
		shown = seq;
		deps.answered(result);
		if (result.renderer === 'gpu') {
			gpuShown = true;
			streamShown = false;
			// The canvas the compositor drew has the delivery's shape.
			if (result.timings?.width && result.timings.height) imgAspect = result.timings.width / result.timings.height;
		} else if (result.frame) {
			gpuShown = false;
			streamShown = false;
			frameUrl = result.frame;
		}
	}

	// Single-flight on the JPEG route: only ever one composite in flight, and `queued` always holds the
	// *latest* wanted frame. Scrubbing collapses to one render + one pending target instead of a
	// backlog of stale frames that must all drain before the frame under the cursor appears.
	async function pump() {
		if (queued === null) return;
		if (queued.via === 'gpu') {
			const { time, overlays, width } = queued;
			queued = null;
			try {
				await askGpu({ time, overlays, width }, ++asked);
			} catch {
				/* ignore decode errors — keep the last good frame */
			}
			return;
		}
		if (inFlight) return;
		const want = queued;
		queued = null;
		inFlight = true;
		const seq = ++asked;
		try {
			// The *composited* timeline still — every visible clip with its color, effects, transform
			// and overlays applied, so Inspector edits show up live. (Desktop only — null in the
			// browser.) Playback may have taken over while this was decoding; a still landing on top of
			// live frames would show as a stutter.
			showUrl(await deps.timelineFrame(want.time, want.width), seq);
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
		get streamShown() {
			return streamShown;
		},
		/** A frame of the playback stream, as the data URL the pane's `<img>` shows: the picture,
		 *  never the surface, in the element the settled still then replaces it in. */
		showStreamed(url: string) {
			gpuShown = false;
			streamShown = false;
			frameUrl = url;
		},
		/** A frame of the playback stream was drawn on the canvas, `aspect` wide for its height: it is
		 *  the picture, never the surface, and stays so until a fetched frame replaces it. */
		streamDrawn(aspect: number) {
			gpuShown = false;
			streamShown = true;
			imgAspect = aspect;
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
			// A picture of a different size is a different frame: a wider box (a panel dragged, a
			// window moved to another screen) asks again, at the new width.
			const width = deps.width();
			if (!deps.hasClips()) {
				frameUrl = null;
				gpuShown = false;
				streamShown = false;
				queued = null;
				return;
			}
			if (deps.streaming() || width === null) return;
			queued = { time, via, overlays, width };
			void pump();
		}
	};
}
