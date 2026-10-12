/** Frame pacing for streamed playback: which rate to ask for, which composited frames the
 *  preview should show, skip, or give up on, and what to do when the machine cannot keep up.
 *
 *  Split out of `Preview.svelte` so it can be tested without a browser or a
 *  backend — the arithmetic here is a few lines that only a running desktop app
 *  could otherwise exercise, and it froze the preview once already. */

import { DEFAULT_FRAME_PX, FRAME_STEPS } from './preview-size';
import type { PlayAhead } from './types';

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
 *  is not worth showing: two frames at the low rates, but not less than this at 50 and 60 fps. Two
 *  frames there are 33-40 ms, and `ui.time` is only written once per animation frame (16.7 ms of
 *  jitter in every lag, all of it on the same side) before the channel's own jitter and one hiccup
 *  are counted, so 50 ms left about 33 ms for everything else and skipped frames of a healthy stream. */
export const MIN_STALE_AFTER = 0.075;

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

/** How long a run of skips is watched before its drift is read as a rate of healing. */
export const HEAL_WATCH = 0.2;
/** How far past the staleness budget the drift must be for the rate of healing to be believed: a
 *  hair over it heals before it matters, and the estimate is only as good as `ui.time`'s 17 ms. */
export const HEAL_MARGIN = 0.05;
/** What a restart costs the picture at least, seconds frozen: ffmpeg's spawn and the first frame. The
 *  stream's own floor is what its start-up really took, and counts when it is longer. */
export const RESTART_COST = 0.5;

export type FrameVerdict = 'show' | 'skip' | 'resync' | 'degrade';

/**
 * Build the per-stream gate: give it how far behind the audio clock a frame
 * arrived (and, for the rate of healing, when), and it says what to do with that frame.
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
 * Drift is one of four things. A frame a little late is skipped (`skip`), so a picture
 * that trails the sound catches up instead of dragging. One a second late means the
 * stream stalled, so it is restarted from the playhead (`resync`). Frames that are late
 * and later with every one are a stream that renders slower than it plays — at 60 fps on a
 * machine that does 50, drift grows by a sixth of a second per second and skipping alone
 * would freeze the pane for the six seconds until the resync — so after
 * [`OVERLOAD_AFTER`] of it the gate says `degrade`: restart from the playhead at a cost the
 * machine can carry (see [`stepDown`]), dropping frames from the source rather than drifting.
 * And frames that are late but *less* late with every one are a stream catching up, which is
 * fine when it is quick (the burst after a hiccup) and a frozen pane when it is slow: a 600 ms
 * stall caught up at 1.1x real time heals at a ninth of a second per second, six seconds of
 * skipping under the one-second resync. So once a run has been watched for [`HEAL_WATCH`], the
 * gate projects how long the pane would still be frozen, until the drift is back inside the
 * budget, and when that is more than a restart costs it says `resync` instead. Pass `at`
 * (seconds, any origin) to time the run; without it each call is taken to be one frame apart. A stream
 * that was started `lead` seconds ahead of the playhead is in step by its floor *and* that: what a
 * restart costs is the start-up it makes up for, `floor + lead`. Build the gate with `healWatch:
 * Infinity` where a restart for healing must not be offered (see [`createLadder`]).
 */
export function createFrameGate({
	fps = 24,
	resyncAfter = RESYNC_AFTER,
	overloadAfter = OVERLOAD_AFTER,
	healWatch = HEAL_WATCH,
	lead = 0
} = {}) {
	const rate = playbackFps(fps);
	const stale = staleAfter(rate);
	const overloadRun = Math.max(2, Math.ceil(overloadAfter * rate));
	let floor: number | null = null;
	let skips = 0;
	let runStart = 0;
	let runAt = 0;
	let calls = 0;
	const seen = { shown: 0, skipped: 0 };
	function verdict(lag: number, at: number = calls / rate): FrameVerdict {
		calls++;
		// The floor tracks the minimum rather than trusting the first frame alone:
		// that one carries ffmpeg's spawn and is usually the slowest of the stream,
		// so taking it as the baseline would leave the budget permanently generous.
		floor = floor === null ? lag : Math.min(floor, lag);
		const drift = lag - floor;
		if (drift > resyncAfter) return 'resync';
		if (drift > stale) {
			if (skips === 0) {
				runStart = drift;
				runAt = at;
			}
			skips++;
			if (skips >= overloadRun && drift >= runStart) return 'degrade';
			const watched = at - runAt;
			if (watched >= healWatch && drift - stale >= HEAL_MARGIN) {
				const healing = (runStart - drift) / watched;
				if (healing > 0 && (drift - stale) / healing > Math.max(RESTART_COST, floor + lead)) return 'resync';
			}
			seen.skipped++;
			return 'skip';
		}
		skips = 0;
		seen.shown++;
		return 'show';
	}
	return Object.assign(verdict, {
		/** The least lag this stream has managed, seconds: what its start-up and transport really cost,
		 *  or `null` before any frame. */
		floor: (): number | null => floor,
		/** What it has said so far, for the log. */
		stats: () => ({ ...seen })
	});
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

/** The most a stream is started ahead of the playhead, seconds. A start-up this long is a stream that
 *  was never in step to begin with (or a stall that happened to land in it), not one to make up for,
 *  and starting too far ahead leaves the picture leading the sound, which is worse than trailing it. */
export const MAX_LEAD = 0.25;

/** Where a stream starts: the playhead, and the start-up this cut has been seen to take ahead of it —
 *  the sound goes on while ffmpeg spawns, and a stream started at the playhead would show its first
 *  frame that long behind the sound and keep it there (the gate judges lag against its floor, so
 *  nothing else would ever notice). Never past the last frame of the cut as it renders, which the
 *  engine refuses. */
export function startAt(playhead: number, startup: number | null, duration: number, fps: number): number {
	const lead = startup !== null && Number.isFinite(startup) ? Math.min(MAX_LEAD, Math.max(0, startup)) : 0;
	return Math.min(playhead + lead, Math.max(playhead, duration - 1 / playbackFps(fps)));
}

/** How far behind where a plain start-up puts it a takeover stream may begin, seconds, to start at the engine's
 *  hand-over: a rAF or two of lateness in the page's trigger. More is a hand-over that was found late, and the
 *  stream's frames would land that far behind the sound for the rest of the play: the paced stream keeps the offset
 *  it starts with, and the gate judges drift against its own floor, so it never notices a constant one. */
export const MAX_BEHIND = 0.05;

/**
 * Where a stream that takes a session over from the GPU starts. The sound the viewer hears is the
 * clock less the output latency, and the GPU draws that picture, so the stream carries on in the same
 * time (`heard`), and from `startup` ahead of it as any stream does.
 *
 * When the refused frame sits in a fade or a transition (`ahead.handover` is before `ahead.at`: the engine's
 * `SpanPlan::handover` is where no window is open, and is the frame itself when none is), the stream starts no
 * later than that, or a slice that began inside the window would drop it and the clip would pop in. That is
 * honoured only when the hand-over is on time, within [`MAX_BEHIND`] of where the start-up puts the stream:
 * **synchronisation wins over the transition when it is late** — a hand-over found late is a stream that
 * starts at the playhead and shows the window cut, not one that trails the sound by the lateness for good. With
 * no window the hand-over is the frame itself (an answer to a request is a round trip old), and starting at
 * it would only be early for nothing.
 */
export function takeoverStart(
	heard: number,
	startup: number | null,
	duration: number,
	fps: number,
	ahead: { at: number; handover: number } | null
): number {
	const from = startAt(heard, startup, duration, fps);
	if (!ahead || !Number.isFinite(ahead.at) || !Number.isFinite(ahead.handover)) return from;
	if (ahead.handover >= ahead.at - 0.5 / playbackFps(fps)) return from;
	const early = Math.max(0, ahead.handover);
	return from - early <= MAX_BEHIND ? Math.min(from, early) : from;
}

/**
 * What a takeover stream is told about where the GPU stops. A hand-over **at once** (the GPU was given up on:
 * overloaded, silent) says nothing about a refusal that is still seconds ahead — the stream must show from
 * the start, not wait for frames before it — so that is dropped; one whose refusal is due (a frame that
 * failed, a session that ended, a refusal the hand-over came late for: at or before the time last asked
 * for, a frame on) is kept, and the surface holds the GPU's last picture until the stream reaches it.
 */
export function takeoverAhead(
	ahead: PlayAhead | null,
	now: boolean,
	heard: number,
	roundTrip: number,
	fps: number
): PlayAhead | null {
	if (!ahead || !now) return ahead;
	return ahead.at <= heard + Math.max(0, roundTrip) + 1 / playbackFps(fps) ? ahead : null;
}

/** What the ladder says about a stream about to start. */
export interface StreamStart {
	/** The cost it plays at. */
	rung: Rung;
	/** What is asked of the backend: the cut's own rate (`null`) until a step down, and the pane's width. */
	request: { fps: number | null; width: number | null };
	/** The timeline time it starts at. */
	from: number;
	/** What the clock is read less, seconds, to be the time that is heard: the output latency of a stream
	 *  that takes over from the GPU (which draws the audible picture), else 0. */
	offset: number;
	/** How far ahead of the playhead that is, seconds: the gate adds it to its floor for what a restart costs. */
	lead: number;
	/** Whether a restart has anywhere cheaper to go: the gate is built without `degrade` when not. */
	cheaper: boolean;
	/** Whether the gate may restart the stream to heal a slow catch-up. Not on a stream that a restart for
	 *  lag started less than [`REPEAT_RESYNC_WINDOW_MS`] ago: a stall that comes with every start (a decoder's
	 *  first second, a cold cache) would be restarted over and over, each restart paying the stall again and
	 *  the ladder stepping down for it. Build the gate with `healWatch: Infinity` when this is false. */
	heal: boolean;
}

/** What a ladder is for: a new cut, a proxy that landed or another pane width starts again at the top. */
export interface LadderKey {
	timeline: unknown;
	epoch: number;
	width: number | null;
}

/**
 * The bookkeeping of playing at the cost a machine can carry, out of the component so it can be tested:
 * how many steps down this cut is at this width, when it last restarted for lag, and how long a stream
 * takes to start, which is how far ahead of the playhead the next one begins. `start` is called whenever
 * a stream (re)starts and says how; `restarted` when the gate asked for a lag restart and `ended` when
 * any stream ends. The steps are a counter read at the start, not state anything watches, and are
 * forgotten after [`DEGRADE_MEMORY_MS`] (`stepsToStart`).
 *
 * **The start-up is the least this cut has been seen to take** (per key; the first Play has none, so it
 * starts at the playhead): every stream is in step by its floor, so its start-up is that floor plus the
 * head start it was given, and the minimum is what no start is likely to beat — a start-up that comes in
 * under the last one would have put the picture ahead of the sound.
 */
export function createLadder(clock: () => number = () => performance.now()) {
	let key: LadderKey | null = null;
	let steps = 0;
	let lastRestart = -Infinity;
	let startup: number | null = null;
	let applied = 0;
	function observe(floor: number | null) {
		if (floor === null || !Number.isFinite(floor)) return;
		const seen = floor + applied;
		startup = startup === null ? seen : Math.min(startup, seen);
	}
	return {
		start(
			next: LadderKey,
			cut: {
				fps: number;
				width: number | null;
				playhead: number;
				duration: number;
				/** The sound's output latency, for a stream that takes a GPU session over; default 0. */
				latency?: number;
				/** Where the GPU stops, for the same: the stream starts at its hand-over when a window is open there. */
				ahead?: { at: number; handover: number } | null;
			}
		): StreamStart {
			if (!key || key.timeline !== next.timeline || key.epoch !== next.epoch || key.width !== next.width) {
				key = next;
				steps = 0;
				lastRestart = -Infinity;
				startup = null;
				applied = 0;
			}
			const now = clock();
			steps = stepsToStart(steps, now - lastRestart);
			const rung = rungAfter(startRung(cut.fps, cut.width), steps);
			const offset = Number.isFinite(cut.latency) ? Math.max(0, cut.latency ?? 0) : 0;
			const heard = Math.max(0, cut.playhead - offset);
			const from = takeoverStart(heard, startup, cut.duration, rung.fps, cut.ahead ?? null);
			applied = from - heard;
			return {
				rung,
				request: steps > 0 ? { fps: rung.fps, width: rung.width } : { fps: null, width: cut.width },
				from,
				offset,
				lead: Math.max(0, applied),
				cheaper: stepDown(rung) !== null,
				heal: now - lastRestart >= REPEAT_RESYNC_WINDOW_MS
			};
		},
		/** The gate said `resync` or `degrade`; `floor` is its floor. */
		restarted(verdict: 'resync' | 'degrade', floor: number | null) {
			observe(floor);
			const now = clock();
			steps = stepsAfter(verdict, steps, now - lastRestart);
			lastRestart = now;
		},
		/** A stream ended, for whatever reason; `floor` is its gate's, `null` if it never showed a frame. */
		ended(floor: number | null) {
			observe(floor);
		},
		get steps() {
			return steps;
		},
		/** The start-up a stream of this cut has been seen to take, seconds, held to [`MAX_LEAD`]: how far
		 *  ahead of the playhead the hand-over from the GPU starts a stream. `null` before any. */
		get startup(): number | null {
			return startup === null || !Number.isFinite(startup) ? null : Math.min(MAX_LEAD, Math.max(0, startup));
		}
	};
}
