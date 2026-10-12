// GPU playback in the page: the pull model's bookkeeping, out of the component so it can be tested.
//
// The webview stays the clock. While the cut plays forward at 1x the Preview asks the backend for the
// picture of the time that is being *heard* (the audio clock less the output latency of the sound,
// `output-latency.ts`) plus what the request itself will have cost by the time its answer is on
// the screen (the smoothed round trip of the last requests), once per animation frame, and the
// backend draws the newest request and tells the ones it replaced (`superseded`). A request that
// asks for the frame already asked for is not made: the cut has a frame every 33 ms and the display
// one every 16.
//
// The first request of a session *decides* it (`renderer: 'gpu'` or `'stream'`), so only that one is
// in flight until it is answered; afterwards every answer can say where the GPU will stop drawing
// (`play.ahead`), and the page hands the session to the stream before it gets there
// (`handoverDue`) — or at once, when a frame fails, or when the GPU cannot hold the rate
// (`'stream'` with `overloaded`).

import type { PlayAhead, PreviewFrameResult } from './types';
import { MAX_OUTPUT_LATENCY } from './output-latency';

/** How many of the last round trips and latencies the median is taken over. */
export const SAMPLES = 9;

/** The most a picture is moved ahead for the cost of its own request, seconds: a round trip that long is
 *  a stalled frame, not the cost of the next one. */
export const MAX_ROUND_TRIP = 0.25;

/** The GPU gives the session up when the median round trip is this many frame periods... */
export const OVERLOAD_PERIODS = 3;
/** ...for this long, seconds of the clock the answers are timed by. */
export const OVERLOAD_AFTER = 1;

/** The GPU is given up on when it has not answered anything for this long, seconds: a decode that hangs
 *  blocks the one worker that draws, and the picture would freeze until the engine's own timeouts (3 s). */
export const STALL_AFTER = 1.5;
/** ...or this long before it has answered the first request (a file that takes a while to open). */
export const STALL_AFTER_FIRST = 5;

/** The start-up of a stream when none has been measured, seconds: the stream is started this long
 *  before the playhead reaches its handover, so its first frame is there when it does. */
export const DEFAULT_STARTUP = 0.15;

/** A median over the last `size` samples. */
export class Median {
	private values: number[] = [];

	constructor(private readonly size = SAMPLES) {}

	add(v: number) {
		if (!Number.isFinite(v)) return;
		this.values.push(v);
		if (this.values.length > this.size) this.values.shift();
	}

	/** The median, or `null` before any sample. */
	get value(): number | null {
		if (this.values.length === 0) return null;
		const sorted = [...this.values].sort((a, b) => a - b);
		return sorted[Math.floor(sorted.length / 2)];
	}
}

/**
 * The timeline time the picture requested now should show: the clock (`ui.time`, the audio clock)
 * less what the sound has not got through yet, plus what the request will cost. Both are held to
 * what is believable, so a bad reading moves the picture a little and not a second.
 */
export function pictureTime(clock: number, outputLatency: number, roundTrip: number): number {
	const heard = Math.min(MAX_OUTPUT_LATENCY, Math.max(0, Number.isFinite(outputLatency) ? outputLatency : 0));
	const trip = Math.min(MAX_ROUND_TRIP, Math.max(0, Number.isFinite(roundTrip) ? roundTrip : 0));
	return Math.max(0, clock - heard + trip);
}

/** The frame of a cut at `fps` that holds time `t` (a time a millionth of a frame short of a boundary is
 *  on it, as `Rational::frame_containing` in the engine counts). */
export function frameIndex(t: number, fps: number): number {
	return Math.floor(Math.max(0, t) * fps + 1e-6);
}

/**
 * The playhead a GPU play session is held at until the audible time reaches it. From rest (Play pressed on a
 * paused frame, a seek) that is the playhead: the picture on the screen is there and the sound of it is
 * heard `outputLatency` later. When a running play is rebuilt (an edit, a resize: the effect ran again) the
 * picture on the screen is the audible one already, so the session is held at that and not ahead of it.
 */
export function playStart(playhead: number, outputLatency: number, running: boolean): number {
	if (!running) return playhead;
	const heard = Math.min(MAX_OUTPUT_LATENCY, Math.max(0, Number.isFinite(outputLatency) ? outputLatency : 0));
	return Math.max(0, playhead - heard);
}

/** What an answer means for the page. */
export type PlayVerdict =
	/** Draw nothing over the surface: the GPU drew a new frame. */
	| 'show'
	/** The frame on the screen is still the one. */
	| 'repeat'
	/** The answer says nothing (a newer request replaced this one). */
	| 'ignore'
	/** The stream plays this session from here (`ahead` says where from, if the engine knew). */
	| 'stream';

/** One request to make. */
export interface PlayStep {
	/** The time the picture should show. */
	time: number;
	/** Its frame of the cut, as the page counts them. */
	frame: number;
}

let sessions = 0;

/** One GPU play session as the page sees it. */
export class GpuPlayback {
	/** This session's number to the backend (a page-wide count: a new session never shares one). */
	readonly session = ++sessions;
	/** `pending` until the first answer decides it. */
	decided: 'pending' | 'gpu' = 'pending';
	/** Where the GPU stops, as the last answer said (`null`: nowhere in sight). */
	ahead: PlayAhead | null = null;
	/** The GPU could not hold the rate, and gave the session up. */
	overloaded = false;
	/** Frames drawn and frames the backend said were the one on the screen already. */
	drawn = 0;
	repeated = 0;
	private lastAsked = -1;
	private deciding = false;
	private over = false;
	private handing = false;
	private slowSince: number | null = null;
	private pending = 0;
	private waitingSince: number | null = null;
	private trips = new Median();
	private heard = new Median();

	/**
	 * `from` is the playhead the play was started from. The picture is held there until what is audible
	 * reaches it: the sound of a position is heard `outputLatency` after the clock gets there, so for that
	 * long the clock less the latency is *before* the playhead, and asking for it would replay the frames
	 * just before (up to nine of them with a Bluetooth headset) the picture on the screen is already at.
	 */
	constructor(
		private readonly fps: number,
		private readonly from = 0
	) {}

	/** The output latency of the sound the picture is held to, seconds (0 before any is read). */
	get latency(): number {
		return Math.min(MAX_OUTPUT_LATENCY, Math.max(0, this.heard.value ?? 0));
	}

	/**
	 * What the clock is read less to be the time the picture on the screen is: the latency, but not before the
	 * playhead the play started from (the picture is held there until the sound gets to it), and nothing at all
	 * until a frame was drawn — a session the GPU declined has drawn nothing for a stream to carry on from.
	 */
	offsetAt(clock: number): number {
		if (this.drawn === 0) return 0;
		return Math.max(0, Math.min(this.latency, clock - this.from));
	}

	/** The smoothed round trip of the requests that drew a frame, seconds (0 before any). */
	get roundTrip(): number {
		return this.trips.value ?? 0;
	}

	get ended(): boolean {
		return this.over;
	}

	/**
	 * The request to make for this animation frame, or `null`: the session is over, the first request
	 * has not been answered, or the picture's frame is the one last asked for (or an earlier one — the
	 * estimate moved, and a frame is never asked for twice, or backwards).
	 */
	next(clock: number, outputLatency: number, fps = this.fps): PlayStep | null {
		if (this.over || (this.decided === 'pending' && this.deciding)) return null;
		this.heard.add(outputLatency);
		const time = Math.max(this.from, pictureTime(clock, this.heard.value ?? 0, this.roundTrip));
		const frame = frameIndex(time, fps);
		if (frame <= this.lastAsked) return null;
		this.lastAsked = frame;
		this.deciding = this.decided === 'pending';
		return { time, frame };
	}

	/**
	 * Take an answer. `trip` is seconds from asking to the answer, `at` a clock in seconds the page
	 * times the overload by (any origin).
	 */
	answer(result: PreviewFrameResult, trip: number, at: number): PlayVerdict {
		if (this.over) return 'ignore';
		// Anything the backend says, a replaced request included, is it being there.
		this.pending = Math.max(0, this.pending - 1);
		this.waitingSince = this.pending > 0 ? at : null;
		switch (result.renderer) {
			case 'superseded':
				// The first request, replaced before it was decided: ask again.
				this.deciding = false;
				return 'ignore';
			case 'gpu': {
				this.decided = 'gpu';
				this.deciding = false;
				this.ahead = result.play?.ahead ?? null;
				if (result.play?.repeat) {
					this.repeated++;
					return 'repeat';
				}
				this.drawn++;
				this.trips.add(trip);
				if (this.cannotHold(at)) {
					this.overloaded = true;
					this.over = true;
					return 'stream';
				}
				return 'show';
			}
			default:
				// `stream` — or an `ffmpeg` frame, which a forward request is never meant to get: the
				// stream plays on from here, where the engine says.
				this.over = true;
				this.ahead = result.play?.ahead ?? null;
				return 'stream';
		}
	}

	/** A request is going out at `at` (the clock `answer` is given). */
	noteSent(at: number) {
		if (this.pending === 0) this.waitingSince = at;
		this.pending++;
	}

	/** Whether a request has been waiting too long for the backend to say anything ([`STALL_AFTER`]): a
	 *  clock that stands still asks for nothing, and nothing asked for is not waited on. */
	stalled(at: number): boolean {
		if (this.over || this.waitingSince === null) return false;
		return at - this.waitingSince > (this.decided === 'gpu' ? STALL_AFTER : STALL_AFTER_FIRST);
	}

	/** Whether the median round trip has been over `OVERLOAD_PERIODS` frame periods for `OVERLOAD_AFTER`. */
	private cannotHold(at: number): boolean {
		const median = this.trips.value;
		if (median === null || median <= OVERLOAD_PERIODS / this.fps) {
			this.slowSince = null;
			return false;
		}
		this.slowSince ??= at;
		return at - this.slowSince >= OVERLOAD_AFTER;
	}

	/**
	 * The place the GPU stops, when the playhead is within `startup` seconds of where the stream has
	 * to start to carry on from it (and so the stream should be started now); `null` while it is not,
	 * and when the GPU stops nowhere in sight.
	 */
	handoverDue(clock: number, startup: number = DEFAULT_STARTUP): PlayAhead | null {
		if (this.over || this.handing || !this.ahead) return null;
		// In the time that is heard, which the GPU's picture and the stream that takes over from it are in.
		return clock - this.offsetAt(clock) >= this.ahead.handover - Math.max(0, startup) ? this.ahead : null;
	}

	/** The stream has been started to take the session over (see {@link GpuPlayback.handOver}). */
	get handingOver(): boolean {
		return this.handing;
	}

	/** The stream has been started to carry on from where the GPU stops: the GPU keeps drawing until
	 *  the stream's first frame is on the screen, and the hand-over is not due again. */
	handOver() {
		this.handing = true;
	}

	/** The session is over (paused, sought, handed to the stream). */
	end() {
		this.over = true;
	}
}
