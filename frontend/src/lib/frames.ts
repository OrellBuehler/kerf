/* Frame quantization for timeline gestures.
 *
 * Keyframes stay in seconds; what is quantized is the *gesture*. A trim, a move
 * or a split lands on a frame of the timeline's frame rate (`timelineFps` — the
 * same rate `export_format` renders at), because a cut that falls between two
 * frames shows the neighbour for one of them whatever the file says.
 *
 * Every function here rounds ONCE, from the raw pointer position, never from the
 * ghost's previous (already rounded) position or from the clip's state after an
 * earlier gesture: a frame is `k / fps` computed from an integer `k`, so two
 * gestures that mean the same frame produce the same double, and a long run of
 * edits cannot accumulate drift. What a gesture then derives (`trimEdit`) comes
 * from that one rounded position.
 *
 * Magnetic snapping (0 / playhead / beats / clip edges, `ui.snap`) still wins when
 * it is within reach; frame quantization is what applies otherwise. It is not
 * switched off with the magnet: time has no meaning between frames. */

/** A time this close to a clip edge is that edge (seconds) — see `quantizeTime`. */
export const WELD_EPS = 1e-6;

/** Slack for float noise when asking "is this on a frame boundary". */
const EPS = 1e-9;

const usable = (fps: number) => Number.isFinite(fps) && fps > 0;

/** The frame a time falls on, nearest. */
export const frameIndex = (t: number, fps: number): number => Math.round(t * fps);

/** The time of frame `k`: one division of an integer, so equal frames are equal doubles. */
export const frameTime = (k: number, fps: number): number => k / fps + 0; // `+ 0` turns -0 into 0

/** `t` moved to the nearest frame boundary; untouched when there is no usable rate. */
export function snapToFrame(t: number, fps: number): number {
	return usable(fps) && Number.isFinite(t) ? frameTime(frameIndex(t, fps), fps) : t;
}

/** Whether `t` sits on a frame boundary (within float noise). */
export function onFrame(t: number, fps: number): boolean {
	if (!usable(fps)) return false;
	const x = t * fps;
	return Math.abs(x - Math.round(x)) < EPS * Math.max(1, Math.abs(x));
}

/** The candidate nearest `time` and strictly closer than `threshold`, else null. */
export function nearestWithin(time: number, candidates: readonly number[], threshold: number): number | null {
	let best: number | null = null;
	let bestD = threshold;
	for (const c of candidates) {
		const d = Math.abs(c - time);
		if (d < bestD) {
			bestD = d;
			best = c;
		}
	}
	return best;
}

export interface QuantizeOptions {
	/** The timeline's frame rate. */
	fps: number;
	/** Snap targets that win when within `threshold` — empty when snapping is off. */
	magnets?: readonly number[];
	/** How close a magnet has to be, seconds. */
	threshold?: number;
	/** Clip edges a result must butt against *exactly* when it lands within
	 *  `WELD_EPS` of one, magnets or not. `move_clip` rejects an overlap by a
	 *  strict float comparison, and an edge computed as `start + length / speed`
	 *  can sit a hair past the frame boundary it is meant to be: a clip placed
	 *  on that frame would then be refused as overlapping by 1e-16 s. */
	welds?: readonly number[];
}

/** One point in time (a trimmed edge, a split): a magnet within reach wins as
 *  is; otherwise the nearest frame, welded to an edge it lands on. */
export function quantizeTime(time: number, o: QuantizeOptions): number {
	const magnet = nearestWithin(time, o.magnets ?? [], o.threshold ?? 0);
	if (magnet !== null) return magnet;
	const frame = snapToFrame(time, o.fps);
	return nearestWithin(frame, o.welds ?? [], WELD_EPS) ?? frame;
}

/** The start of a span of `dur` seconds being moved: a magnet for either edge
 *  (`magnets` are *start* targets, so a caller that wants the tail on a beat adds
 *  `beat - dur` itself), else the nearest frame for the start, welded at either
 *  end. Never before 0. */
export function quantizeSpanStart(start: number, dur: number, o: QuantizeOptions): number {
	const magnet = nearestWithin(start, o.magnets ?? [], o.threshold ?? 0);
	if (magnet !== null) return Math.max(0, magnet);
	const frame = snapToFrame(start, o.fps);
	const welds = o.welds ?? [];
	const head = nearestWithin(frame, welds, WELD_EPS);
	if (head !== null) return Math.max(0, head);
	const tail = nearestWithin(frame + dur, welds, WELD_EPS);
	return Math.max(0, tail !== null ? tail - dur : frame);
}

/** Clamp a quantized edge into the range the clip may be trimmed to. The bounds
 *  (a neighbour's edge, the end of the source) are real limits and win over the
 *  grid, so a clamped edge can sit between two frames. */
export const clampEdge = (t: number, min: number, max: number): number => Math.min(max, Math.max(min, t));

/**
 * Where a razor cut at `at` may actually fall in the clip `[start, end]`: `at`
 * itself when it leaves at least half a frame on both sides, else the nearest
 * *frame boundary* that does, else `null` (the clip is too short to be cut on a
 * frame at all). `at` should already be quantized — a magnet's time is kept.
 */
export function splitPoint(at: number, start: number, end: number, fps: number): number | null {
	// With no rate to quantize to, the only requirement is the backend's own:
	// strictly inside the clip.
	if (!usable(fps)) return at > start && at < end ? at : null;
	const half = 0.5 / fps;
	if (at >= start + half - EPS && at <= end - half + EPS) return at;
	const lo = Math.ceil((start + half) * fps - EPS);
	const hi = Math.floor((end - half) * fps + EPS);
	if (lo > hi) return null;
	return frameTime(Math.min(hi, Math.max(lo, frameIndex(at, fps))), fps);
}

/** A fade length for a drag that asked for `raw` seconds: on a frame, never
 *  negative, and never past `max` (the room the clip has left). A length that
 *  rounds to nothing is no fade at all. */
export function quantizeFade(raw: number, max: number, fps: number): number {
	const cap = Math.max(0, max);
	const v = snapToFrame(Math.max(0, raw), fps);
	if (v <= cap) return v;
	return usable(fps) ? frameTime(Math.floor(cap * fps + EPS), fps) : cap;
}

/** The pieces of a clip a trim edits. */
export interface TrimSource {
	source_in: number;
	source_out: number;
	timeline_start: number;
	speed?: number;
}

/** The fields a trim writes — only the ones it changes. */
export interface TrimEdit {
	source_in?: number;
	source_out?: number;
	timeline_start?: number;
}

/**
 * The edit for dragging one edge of `clip` to timeline time `pos` — the single
 * rounded position of the gesture, from which *every* field is derived, so the
 * clip's timeline start, source in and source out cannot disagree about it.
 *
 * The end of playback is `source_out` going forward and `source_in` reversed (a
 * reversed clip plays its source backwards), and a left-edge trim moves
 * `timeline_start` with it so the right edge stays put.
 */
export function trimEdit(clip: TrimSource, edge: 'l' | 'r', pos: number): TrimEdit {
	const speed = clip.speed ?? 1;
	const mag = Math.max(Math.abs(speed), 0.01);
	if (edge === 'r') {
		const newDur = pos - clip.timeline_start;
		return speed < 0
			? { source_in: clip.source_out - newDur * mag }
			: { source_out: clip.source_in + newDur * mag };
	}
	const delta = (pos - clip.timeline_start) * mag; // > 0 shortens from the left
	return speed < 0
		? { source_out: clip.source_out - delta, timeline_start: pos }
		: { source_in: clip.source_in + delta, timeline_start: pos };
}
