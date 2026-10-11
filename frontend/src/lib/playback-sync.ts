/** Frame pacing for streamed playback: which rate to ask for, which composited frames the
 *  preview should show, skip, or give up on, and what to do when the machine cannot keep up.
 *
 *  Split out of `Preview.svelte` so it can be tested without a browser or a
 *  backend — the arithmetic here is a few lines that only a running desktop app
 *  could otherwise exercise, and it froze the preview once already. */

import { DEFAULT_FRAME_PX, FRAME_STEPS } from './preview-size';

/** The fastest a stream plays: a display paints no more than this, so frames past it are composited
 *  and shipped for nothing. Mirrors `MAX_STREAM_FPS` in kerf-core. */
export const MAX_PLAYBACK_FPS = 60;
/** The slowest. Mirrors `MIN_STREAM_FPS`. */
export const MIN_PLAYBACK_FPS = 1;

/** The rate a cut of `timelineFps` plays at: its own, held to what a display paints. The engine
 *  reads the same number off the cut when the page does not state one (`playback_params`). */
export function playbackFps(timelineFps: number): number {
	const fps = Number.isFinite(timelineFps) && timelineFps > 0 ? timelineFps : 30;
	return Math.min(MAX_PLAYBACK_FPS, Math.max(MIN_PLAYBACK_FPS, fps));
}

/** The least a frame may trail the audio clock, beyond the stream's own transport floor, before it
 *  is not worth showing: two frames at the low rates, but not less than this at 60 fps, where two
 *  frames are 33 ms and one main-thread hiccup is more than that. */
export const MIN_STALE_AFTER = 0.05;

/** How much further behind the audio clock a frame may *drift* than the stream's
 *  own transport floor, and still be worth showing. Roughly two frames: enough
 *  to absorb IPC jitter without letting the picture visibly trail the sound. */
export function staleAfter(fps: number): number {
	return Math.max(2 / playbackFps(fps), MIN_STALE_AFTER);
}

/** Drift at which the stream is abandoned and restarted from the playhead,
 *  rather than kept and played out behind the sound. */
export const RESYNC_AFTER = 1.0;

/** How long of frames the gate skips one after the other, while the drift is still growing, before it
 *  decides the stream cannot keep up with the rate it was given. A burst after a hiccup is a run of
 *  skips too, but its drift shrinks with every frame; a stream that renders slower than it plays is
 *  further behind with every one. */
export const OVERLOAD_AFTER = 0.4;

export type FrameVerdict = 'show' | 'skip' | 'resync' | 'degrade';

/**
 * Build the per-stream gate: give it how far behind the audio clock a frame
 * arrived, and it says what to do with that frame.
 *
 * Every frame arrives *some* amount late — ffmpeg's startup, then IPC and the
 * decode per frame — and the backend paces from the first frame it emits, so
 * that cost lands as a constant offset on the whole stream rather than the
 * picture falling behind. Measured against zero it exceeds the two-frame budget
 * on its own (~60 ms of ffmpeg startup against 83 ms of budget, before the
 * webview sees a byte), which drops every frame forever and leaves the pane on
 * whatever still it had. So lag is judged against the smallest this stream has
 * managed — its transport floor — and only growth beyond that counts as drift.
 *
 * Drift is one of three things. A frame a little late is skipped (`skip`), so a picture
 * that trails the sound catches up instead of dragging. One a second late means the
 * stream stalled, so it is restarted from the playhead (`resync`). And frames that are late
 * and later with every one are a stream that renders slower than it plays — at 60 fps on a
 * machine that does 50, drift grows by a sixth of a second per second and skipping alone
 * would freeze the pane for the six seconds until the resync — so after
 * [`OVERLOAD_AFTER`] of it the gate says `degrade`: restart from the playhead at a cost the
 * machine can carry (see [`stepDown`]), dropping frames from the source rather than drifting.
 */
export function createFrameGate({ fps = 24, resyncAfter = RESYNC_AFTER, overloadAfter = OVERLOAD_AFTER } = {}) {
	const rate = playbackFps(fps);
	const stale = staleAfter(rate);
	const overloadRun = Math.max(2, Math.ceil(overloadAfter * rate));
	let floor: number | null = null;
	let skips = 0;
	let runStart = 0;
	return function verdict(lag: number): FrameVerdict {
		// The floor tracks the minimum rather than trusting the first frame alone:
		// that one carries ffmpeg's spawn and is usually the slowest of the stream,
		// so taking it as the baseline would leave the budget permanently generous.
		floor = floor === null ? lag : Math.min(floor, lag);
		const drift = lag - floor;
		if (drift > resyncAfter) return 'resync';
		if (drift > stale) {
			if (skips === 0) runStart = drift;
			skips++;
			if (skips >= overloadRun && drift >= runStart) return 'degrade';
			return 'skip';
		}
		skips = 0;
		return 'show';
	};
}

/** A cost the stream can be restarted at: the rate it plays and the width it renders. */
export interface Rung {
	fps: number;
	width: number;
}

/** Below this the picture is a slide show: width goes before rate until here. */
const SMOOTH_FPS = 30.5;
/** The narrowest the ladder goes before the rate is halved again. */
const SHARP_WIDTH = 960;
/** The slowest the ladder plays; another halving is not worth the motion it takes. */
const FLOOR_FPS = 15.5;
const FLOOR_WIDTH = FRAME_STEPS[0];

/**
 * The next cheaper way to play, or `null` when there is none. Frames are dropped from the
 * source, not from the stream: a restart at half the rate composites half as many, where
 * skipping the late ones of a full-rate stream saves nothing the machine was short of.
 * Smooth motion matters more than a sharp picture that is moving, so: a rate above 30 is
 * halved first (60 → 30), then the width goes down a step at a time to 960 (1920 → 1440 → 960),
 * then the rate is halved again (30 → 15), then the width to the narrowest step. Pure and
 * deterministic, so the same machine settles on the same rung every time.
 */
export function stepDown(rung: Rung): Rung | null {
	if (rung.fps > SMOOTH_FPS) return { ...rung, fps: rung.fps / 2 };
	if (rung.width > SHARP_WIDTH) {
		const narrower = [...FRAME_STEPS].reverse().find((step) => step < rung.width && step >= SHARP_WIDTH);
		return { ...rung, width: narrower ?? SHARP_WIDTH };
	}
	if (rung.fps > FLOOR_FPS) return { ...rung, fps: rung.fps / 2 };
	if (rung.width > FLOOR_WIDTH) return { ...rung, width: FLOOR_WIDTH };
	return null;
}

/** `base` stepped down `steps` times, stopping at the cheapest there is. */
export function rungAfter(base: Rung, steps: number): Rung {
	let rung = base;
	for (let i = 0; i < steps; i++) {
		const next = stepDown(rung);
		if (!next) break;
		rung = next;
	}
	return rung;
}

/** The rung a cut starts at: the rate it plays at, and the frame width asked of the backend
 *  (the pane's, held to the source), or the default one until the pane has been measured. */
export function startRung(timelineFps: number, width: number | null): Rung {
	return { fps: playbackFps(timelineFps), width: width ?? DEFAULT_FRAME_PX };
}

/** Two restarts this close together are not hiccups but a machine that cannot hold the rate. */
export const REPEAT_RESYNC_WINDOW_MS = 10_000;

/**
 * How many steps down a stream is, given what just happened. `steps` is where it was; a
 * `degrade` always goes one further, and so does a `resync` that follows another within
 * [`REPEAT_RESYNC_WINDOW_MS`] (`sinceLastMs`). One stall in a long session is a stall, not a
 * verdict on the machine.
 */
export function stepsAfter(verdict: 'resync' | 'degrade', steps: number, sinceLastMs: number): number {
	if (verdict === 'degrade') return steps + 1;
	return sinceLastMs < REPEAT_RESYNC_WINDOW_MS ? steps + 1 : steps;
}

/** How long a step down is remembered. A stream that fell behind may have met a busy moment rather
 *  than a machine that cannot do it, and a cut left a step softer for good would never find out;
 *  one that really cannot is stepped down again within a second of starting. */
export const DEGRADE_MEMORY_MS = 60_000;

/** The steps a stream starts with: where it was, unless its last restart was long enough ago. */
export function stepsToStart(steps: number, sinceLastRestartMs: number): number {
	return sinceLastRestartMs > DEGRADE_MEMORY_MS ? 0 : steps;
}
