// The Preview panel's frame fetching, out of the component so its reactivity can be tested: on the
// JPEG route one frame in flight at a time with the latest wanted time waiting, on the GPU route a
// request per wanted time (the backend keeps only the newest: `get_preview_frame` is latest-wins and
// answers `superseded` for the ones it replaced), and the answer shown as a picture
// (`frameUrl`), as nothing at all (`gpuShown`: the native surface is what is seen) or, with the canvas
// painter on (`CANVAS_PAINTER`), as the last frame of the playback stream on a canvas of its own (`streamShown`).
// `frameUrl` is the one slot for the stream's frames and the fetched stills, in the order they were handed
// over; the Preview paints it through two stacked `<img>`s (`picture-buffer.ts`), told of each by `frameSeq`
// (an identical URL is still a new frame) and of whether it replaced the surface's or the canvas's by `frameCovers`.
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

import { untrack } from 'svelte';
import { isProxyWaitMessage } from './proxy-wait';
import type { GpuPlayback } from './gpu-playback';
import type { PlayAhead, PlayAsk, PreviewDraft, PreviewFrameResult, PreviewOrder } from './types';

/** How long a request on the GPU route may wait for the backend before the worker that draws is taken for stuck
 *  (a decode or a driver that does not come back), milliseconds. The picture is then FFmpeg's, from the other
 *  command, and the page stops asking the GPU until an answer comes. This is for a worker that has answered before:
 *  a frame that is only slow in the middle of a session (a probe measured for a new kind of clip, a first decode
 *  of a heavy file) can still pass it, and then costs a JPEG for that frame and the ones until it answers
 *  (`recovered` asks again for the frame that is wanted then). */
export const GPU_ANSWER_LIMIT_MS = 8000;
/** The limit for the first request after the GPU preview was switched on, until a frame has been drawn, milliseconds:
 *  the backend is made (`BUILD_DEADLINE` 10 s), what the plan needs is measured (each probe bounded at 20 s, the whole
 *  of them at 45 s) and the first frame decoded (30 s) before it answers, so a limit under the sum would call a
 *  slow start a stuck worker. A hang in device creation is cut short by the build deadline, not by this. Until it
 *  the pane is not left empty: at [`GPU_ANSWER_LIMIT_MS`] the JPEG of the frame stands in for it, provisionally,
 *  and nothing is latched (not the stuck state, not the status note, not the Play exclusion) before this. */
export const GPU_FIRST_ANSWER_LIMIT_MS = 90_000;

/** A frame has been drawn by the backend since the GPU preview was last switched on: the backend is warm, and the
 *  short limit is a worker's. Outside the pump, so a panel that is mounted again (a workspace switch) against a
 *  backend that is up is not given the long limit once more; [`createFramePump`]'s `coldStart` is what forgets it. */
let backendDrawn = false;

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
	/** [`GPU_ANSWER_LIMIT_MS`] and [`GPU_FIRST_ANSWER_LIMIT_MS`]; a test shortens them. */
	answerLimitMs?: number;
	firstAnswerLimitMs?: number;
	/** The worker that draws stopped answering (`true`: this frame and the ones after it are FFmpeg's, the surface
	 *  goes) or answered again (`false`). It is not a failure of one frame (`gpuFailed`): nothing the page does
	 *  restarts a worker that is stuck in a driver, so the page says to restart Kerf rather than look as if switching
	 *  the setting off and on tried again. */
	workerStuck?(stuck: boolean): void;
	/** A frame was drawn into the native surface while the page showed another picture (an answer that came late): the
	 *  surface has a picture on it that is not the one wanted, and must not be on show. */
	surfaceStale?(): void;
}

const LATE = Symbol('the backend did not answer in time');

export function createFramePump(deps: FramePumpDeps) {
	let frameUrl = $state<string | null>(null);
	/** Bumped for every picture handed to the pane, `frameUrl` unchanged or not (a still can be byte for byte
	 *  the one before the GPU played). */
	let frameSeq = $state(0);
	let frameCount = 0;
	/** `frameUrl` took the pane from a picture the `<img>`s did not show (the GPU's surface, the stream's
	 *  canvas): what they hold is older than what was last seen. */
	let frameCovers = $state(false);
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
	/** The GPU worker has not answered a request for [`GPU_ANSWER_LIMIT_MS`]: frames are the JPEG's, the backend
	 *  is not asked again, until the answer that was late comes (a worker that never comes back keeps the page on
	 *  the JPEG route for good, which is the way back a stuck driver needs). */
	let gpuStuck = false;
	/** The newest frame the page wanted, whatever route: what is asked again when a late answer says the worker is back. */
	let latest: { time: number; via: 'gpu' | 'jpeg'; overlays: boolean; width: number; draft: PreviewDraft | null } | null = null;
	/** The limits of the requests in flight, so none outlives the pump. */
	const limits = new Set<ReturnType<typeof setTimeout>>();
	let disposed = false;
	/** This pump's name to the backend, which numbers by it: a reloaded page or a second window starts
	 *  counting again from zero without being behind the first. */
	const client = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;

	/** A picture for the pane. Call before `gpuShown` is cleared: it says what the picture replaces. */
	function setFrame(url: string | null) {
		frameCovers = url !== null && (gpuShown || streamShown);
		frameUrl = url;
		// Not `frameSeq++`: that reads it, and `run` (an effect) hands over a null.
		frameSeq = ++frameCount;
	}

	/** Show a JPEG that is the newest answer so far. */
	function showUrl(url: string | null, seq: number) {
		if (!url || deps.streaming() || seq < shown) return;
		shown = seq;
		setFrame(url);
		gpuShown = false;
		drawnDraft = null;
		streamShown = false;
	}

	/** The answer that was late came: the worker is back. If it drew into the surface while the page showed the JPEG,
	 *  that surface holds a picture of a time that has passed, which must not be on show: it is hidden, and the frame
	 *  that is wanted now is asked for again (a late JPEG from the worker is only dropped: the surface was hidden).
	 *  Not when a newer answer of the worker's is on show already (a second late answer, after the first was recovered
	 *  from): that surface holds the frame that is wanted. */
	function recovered(answer: PreviewFrameResult | null) {
		gpuStuck = false;
		if (answer?.renderer === 'gpu') backendDrawn = true;
		deps.workerStuck?.(false);
		if (gpuShown) return;
		if (answer?.renderer === 'gpu') deps.surfaceStale?.();
		if (latest && latest.via === 'gpu' && !deps.streaming() && !deps.playback?.()) {
			queued = latest;
			void pump();
		}
	}

	// The GPU route is not single-flight: the backend draws the newest request and tells the ones it
	// replaced (`superseded`), so a drag asks for every position the playhead passes and the renderer
	// never waits for a round trip between two frames. The backend draws in the native surface when the
	// plan allows it (and says so), else it hands back FFmpeg's JPEG for the frame.
	async function askGpu(want: { time: number; overlays: boolean; width: number; draft: PreviewDraft | null }, seq: number) {
		let result: PreviewFrameResult | null = null;
		let call: Promise<PreviewFrameResult>;
		try {
			call = deps.previewFrame(want.time, want.overlays, want.width, { client, seq }, undefined, want.draft);
		} catch (error) {
			call = Promise.reject(error);
		}
		// Until the backend has drawn a frame the limit is the long one, and at the short one the JPEG of this frame stands
		// in for it (provisionally: nothing is latched); a warm backend has the short limit and is judged by it.
		const shortMs = deps.answerLimitMs ?? GPU_ANSWER_LIMIT_MS;
		const longMs = backendDrawn ? shortMs : (deps.firstAnswerLimitMs ?? GPU_FIRST_ANSWER_LIMIT_MS);
		const timers: Array<ReturnType<typeof setTimeout>> = [];
		const after = (ms: number, then: () => void) => {
			const timer = setTimeout(() => {
				limits.delete(timer);
				then();
			}, ms);
			limits.add(timer);
			timers.push(timer);
		};
		let settled = false;
		let painted = false;
		/** The JPEG of this frame, from the command that does not go through the worker, unless the worker has answered. */
		const standIn = async () => {
			if (painted) return;
			painted = true;
			try {
				const url = await deps.timelineFrame(want.time, want.width);
				if (!settled && !disposed) showUrl(url, seq);
			} catch {
				/* keep the last good frame */
			}
		};
		const late = new Promise<typeof LATE>((resolve) => {
			if (longMs > shortMs) after(shortMs, () => void standIn());
			after(longMs, () => resolve(LATE));
		});
		const stop = () => {
			settled = true;
			for (const timer of timers) {
				clearTimeout(timer);
				limits.delete(timer);
			}
		};
		try {
			const outcome = await Promise.race([call, late]);
			if (outcome === LATE) {
				for (const timer of timers) {
					clearTimeout(timer);
					limits.delete(timer);
				}
				if (disposed) return;
				// The worker that draws is not answering: this frame is FFmpeg's, from the command that does not
				// go through it, and so is every one until it answers (the one that was late says when).
				gpuStuck = true;
				deps.workerStuck?.(true);
				void call.then(
					(r) => {
						if (!disposed && r.renderer !== 'superseded') recovered(r);
					},
					() => {
						if (!disposed) recovered(null);
					}
				);
				await standIn();
				return;
			}
			stop();
			result = outcome;
			// The backend has drawn a frame: from here it is warm, and the short limit is its. (A JPEG it answers with
			// is not that: the self-test and the plan's refusals come back as JPEGs from a backend that has drawn nothing.)
			if (outcome.renderer === 'gpu') backendDrawn = true;
		} catch (error) {
			stop();
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
			setFrame(result.frame);
			gpuShown = false;
			drawnDraft = null;
			streamShown = false;
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
			playback.giveUp();
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
			playback.giveUp();
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
		// A GPU worker that stopped answering is not asked again: its frames are the JPEG route's.
		if (queued.via === 'gpu' && !gpuStuck) {
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
		get frameSeq() {
			return frameSeq;
		},
		get frameCovers() {
			return frameCovers;
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
			// Newer than every request made so far: a still or the GPU's frame still on its way back is stale.
			shown = asked + 1;
			setFrame(url);
			gpuShown = false;
			drawnDraft = null;
			streamShown = false;
		},
		/** A frame of the playback stream was drawn on the canvas, `aspect` wide for its height: it is
		 *  the picture, never the surface, and stays so until a fetched frame replaces it. */
		streamDrawn(aspect: number) {
			gpuShown = false;
			drawnDraft = null;
			streamShown = true;
			imgAspect = aspect;
		},
		/** The worker that draws stopped answering (and has not since): the Play decision leaves the GPU out, or a play
		 *  from rest would wait the first-answer limit for a worker that is not there. */
		get gpuStuck() {
			return gpuStuck;
		},
		/** The setting was switched off: the next request after it is the first of a backend that is made again, and the
		 *  long limit is its. A worker that is stuck stays so (it cannot be restarted from here). */
		coldStart() {
			backendDrawn = false;
		},
		/** The panel is gone: no limit is left running, and nothing a late answer would do is done. */
		dispose() {
			disposed = true;
			for (const limit of limits) clearTimeout(limit);
			limits.clear();
			queued = null;
			latest = null;
		},
		/** The picture on screen is not the surface's any more (the setting went off). */
		surfaceGone() {
			gpuShown = false;
			drawnDraft = null;
		},
		/**
		 * Ask where, from `time` on, the GPU can take the play back from the stream (`hint: 'ready'`): nothing is
		 * drawn. It is a request of this pump's like any other (its `client` and the next `seq`), so one that reaches
		 * the backend after a newer request of the page (the paused frame once the play stops) is answered
		 * `superseded` and does nothing.
		 */
		ready(time: number, overlays: boolean, width: number, session: number): Promise<PreviewFrameResult> {
			return deps.previewFrame(time, overlays, width, { client, seq: ++asked }, { session, ready: true });
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
				if (untrack(() => frameUrl) !== null) setFrame(null);
				gpuShown = false;
				drawnDraft = null;
				streamShown = false;
				queued = null;
				latest = null;
				return;
			}
			if (deps.streaming() || width === null) return;
			const playback = deps.playback?.() ?? null;
			if (playback) {
				void forward(playback, overlays, width);
				return;
			}
			queued = { time, via, overlays, width, draft };
			latest = queued;
			void pump();
		}
	};
}
