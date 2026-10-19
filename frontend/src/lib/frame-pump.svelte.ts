// The Preview panel's frame fetching, out of the component so its reactivity can be tested: on the
// JPEG route one frame in flight at a time with the latest wanted time waiting, on the GPU route a
// request per wanted time (the backend keeps only the newest: `get_preview_frame` is latest-wins and
// answers `superseded` for the ones it replaced), and the answer shown as a picture
// (`frameUrl`), as nothing at all (`gpuShown`: the native surface is what is seen) or, with the canvas
// painter on (`CANVAS_PAINTER`), as the last frame of the playback stream on a canvas of its own (`streamShown`).
//
// **GPU playback** (`playback` set) is a third route: while the cut plays forward at 1x and the
// session is the GPU's, `run` runs once per animation frame (the playhead moves) and asks the backend
// for the picture of the time that is audible now (`GpuPlayback.next`: the clock less the sound's output
// latency plus the smoothed round trip, one request per frame of the cut), with `hint = forward`. The
// first answer decides the session; one that says `stream` (or a frame that fails, or a GPU that cannot
// hold the rate) hands it to the stream through `playHandover`, and so does the hand-over the answers
// announce (`ahead`) when the playhead gets to it.
//
// **A gesture's frames** (`draft` set: an Inspector slider, a title or clips being dragged) go the GPU
// route with what the gesture would write (`preview-draft.ts`), so the backend draws the frame as the cut
// will be once it is let go. The draft is read only on the GPU route: on the JPEG route it changes
// nothing and asks for nothing, which is today's behaviour (the frame of the edit once it is written).
//
// The one rule that matters: **what the effect depends on is what it reads in `run`**, and `pump`
// reads nothing reactive before its first await. A frame fetch is expensive (an FFmpeg composite),
// so an effect that re-ran because something unrelated changed (the trim monitor, the GPU status
// object being replaced, a dialog opening) would decode a frame nobody asked for. The route is read
// as its two primitives in `run` — the only parts of it that should bring a frame back — and handed
// to `pump` as values.

import { isProxyWaitMessage } from './proxy-wait';
import type { GpuPlayback } from './gpu-playback';
import type { PlayAhead, PlayAsk, PreviewDraft, PreviewFrameResult, PreviewOrder } from './types';

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
	previewFrame(
		time: number,
		overlays: boolean,
		width: number,
		order: PreviewOrder,
		play?: PlayAsk,
		draft?: PreviewDraft | null
	): Promise<PreviewFrameResult>;
	/** What a gesture under way would write, or null: read on the GPU route only. */
	draft?(): PreviewDraft | null;
	/** What the backend answered (the status bar names the renderer). */
	answered(result: PreviewFrameResult): void;
	/** The GPU path failed outright (the command rejected): the frame is the JPEG and the surface goes. */
	gpuFailed(error: unknown): void;
	/** GPU playback: the session being decided or played, or null. Read in `run`, so a session that
	 *  starts or ends brings a frame back (the settled one, at a pause). */
	playback?(): GpuPlayback | null;
	/** What a forward request reads: the playhead (the audio clock), how late the sound is, and the
	 *  rate the cut plays at. */
	playClock?(): { clock: number; latency: number; fps: number };
	/** Seconds a stream takes to start, for knowing how early to hand over to it. */
	startup?(): number | undefined;
	/** The session goes to the stream: at once (`now`: the engine said so, a frame failed, the GPU
	 *  cannot hold the rate) or when the playhead has got to where the GPU stops. `ahead` says where
	 *  from, when the engine knew. */
	playHandover?(playback: GpuPlayback, ahead: PlayAhead | null, now: boolean, why: string): void;
	/** Under "Proxy only" the backend refused to start the session: the preview waits for a proxy. */
	playWaiting?(playback: GpuPlayback, message: string): void;
	/** A clock in seconds (any origin) the round trips are timed by; `performance.now() / 1000`. */
	now?(): number;
}

export function createFramePump(deps: FramePumpDeps) {
	let frameUrl = $state<string | null>(null);
	/** The frame on show was drawn in the native surface: nothing is painted over it. */
	let gpuShown = $state(false);
	/** The draft of the gesture the frame on show was drawn from (the GPU route's answer to a request
	 *  that carried one), `null` for any other picture: what the page draws over the frame of a drag goes
	 *  by what is in it. */
	let drawnDraft = $state.raw<PreviewDraft | null>(null);
	/** The frame on show is the playback stream's, drawn on the canvas: the picture until a fetched
	 *  frame lands on top of it. */
	let streamShown = $state(false);
	/** The aspect of the picture on show (the canvas the compositor drew, or the JPEG). */
	let imgAspect = $state<number | null>(null);
	let inFlight = false;
	let queued: { time: number; via: 'gpu' | 'jpeg'; overlays: boolean; width: number; draft: PreviewDraft | null } | null = null;
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
		drawnDraft = null;
		streamShown = false;
	}

	// The GPU route is not single-flight: the backend draws the newest request and tells the ones it
	// replaced (`superseded`), so a drag asks for every position the playhead passes and the renderer
	// never waits for a round trip between two frames. The backend draws in the native surface when the
	// plan allows it (and says so), else it hands back FFmpeg's JPEG for the frame.
	async function askGpu(want: { time: number; overlays: boolean; width: number; draft: PreviewDraft | null }, seq: number) {
		let result: PreviewFrameResult | null = null;
		try {
			result = await deps.previewFrame(want.time, want.overlays, want.width, { client, seq }, undefined, want.draft);
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
			drawnDraft = want.draft;
			streamShown = false;
			// The canvas the compositor drew has the delivery's shape.
			if (result.timings?.width && result.timings.height) imgAspect = result.timings.width / result.timings.height;
		} else if (result.frame) {
			gpuShown = false;
			drawnDraft = null;
			streamShown = false;
			frameUrl = result.frame;
		}
	}

	// A frame of GPU playback: see the header. Not single-flight either; the session says what to ask.
	async function forward(playback: GpuPlayback, overlays: boolean, width: number) {
		const clock = deps.playClock?.();
		if (!clock) return;
		const now = deps.now ?? (() => performance.now() / 1000);
		const due = playback.handoverDue(clock.clock, deps.startup?.());
		if (due) {
			deps.playHandover?.(playback, due, false, due.why);
			return;
		}
		if (playback.stalled(now())) {
			deps.gpuFailed(new Error('the GPU preview stopped answering during playback'));
			deps.playHandover?.(playback, playback.ahead, true, 'the GPU stopped answering');
			return;
		}
		const step = playback.next(clock.clock, clock.latency, clock.fps);
		if (!step) return;
		const seq = ++asked;
		const sent = now();
		playback.noteSent(sent);
		let result: PreviewFrameResult;
		try {
			result = await deps.previewFrame(step.time, overlays, width, { client, seq }, { session: playback.session });
		} catch (error) {
			if (deps.playback?.() !== playback) return;
			if (isProxyWaitMessage(error)) {
				deps.playWaiting?.(playback, error instanceof Error ? error.message : String(error));
				return;
			}
			// The command itself failed: the stream plays on from the playhead, and the surface must not stay over it.
			deps.gpuFailed(error);
			deps.playHandover?.(playback, null, true, error instanceof Error ? error.message : String(error));
			return;
		}
		if (deps.playback?.() !== playback) return;
		const verdict = playback.answer(result, now() - sent, now());
		if (verdict === 'ignore' || verdict === 'repeat') return;
		if (verdict === 'stream') {
			deps.answered(result);
			const why = playback.overloaded
				? 'the GPU cannot hold the rate'
				: (result.reasons[0] ?? playback.ahead?.why ?? 'the stream plays from here');
			deps.playHandover?.(playback, playback.ahead, true, why);
			return;
		}
		if (seq < shown) return;
		shown = seq;
		deps.answered(result);
		gpuShown = true;
		drawnDraft = null;
		streamShown = false;
		if (result.timings?.width && result.timings.height) imgAspect = result.timings.width / result.timings.height;
	}

	// Single-flight on the JPEG route: only ever one composite in flight, and `queued` always holds the
	// *latest* wanted frame. Scrubbing collapses to one render + one pending target instead of a
	// backlog of stale frames that must all drain before the frame under the cursor appears.
	async function pump() {
		if (queued === null) return;
		if (queued.via === 'gpu') {
			const { time, overlays, width, draft } = queued;
			queued = null;
			try {
				await askGpu({ time, overlays, width, draft }, ++asked);
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
		get drawnDraft() {
			return drawnDraft;
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
			drawnDraft = null;
			streamShown = false;
			frameUrl = url;
		},
		/** A frame of the playback stream was drawn on the canvas, `aspect` wide for its height: it is
		 *  the picture, never the surface, and stays so until a fetched frame replaces it. */
		streamDrawn(aspect: number) {
			gpuShown = false;
			drawnDraft = null;
			streamShown = true;
			imgAspect = aspect;
		},
		/** The picture on screen is not the surface's any more (the setting went off). */
		surfaceGone() {
			gpuShown = false;
			drawnDraft = null;
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
			// The same for a gesture's draft: the JPEG route draws the cut as it is written.
			const draft = via === 'gpu' ? (deps.draft?.() ?? null) : null;
			void deps.boundsEpoch();
			// A picture of a different size is a different frame: a wider box (a panel dragged, a
			// window moved to another screen) asks again, at the new width.
			const width = deps.width();
			if (!deps.hasClips()) {
				frameUrl = null;
				gpuShown = false;
				drawnDraft = null;
				streamShown = false;
				queued = null;
				return;
			}
			if (deps.streaming() || width === null) return;
			const playback = deps.playback?.() ?? null;
			if (playback) {
				void forward(playback, overlays, width);
				return;
			}
			queued = { time, via, overlays, width, draft };
			void pump();
		}
	};
}
