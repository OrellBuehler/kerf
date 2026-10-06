import type { TextKeyframe, TextOverlay } from './types';

/** Shortest a title may be made by dragging or typing its edges, seconds. */
export const MIN_TITLE = 0.1;
/** Bounds a title's font height may be dragged between (the Inspector slider's range, widened). */
export const TITLE_SIZE_MIN = 0.02;
export const TITLE_SIZE_MAX = 0.3;
/** `drawtext` lays a line out about this tall, as a multiple of `fontsize`. */
export const LINE_HEIGHT = 1.17;
/** `drawtext`'s `boxborderw`, in output pixels, drawn around the text when a box colour is set. */
export const BOX_BORDER_PX = 12;
/** Height the box padding is taken against when the project has no delivery frame. */
export const DEFAULT_FRAME_H = 1080;

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);
const round3 = (v: number) => Math.round(v * 1000) / 1000;

export const isVisibleAt = (o: TextOverlay, t: number) => t >= o.start && t <= o.end;

function lerp(points: [number, number][], at: number): number | undefined {
	if (points.length === 0) return undefined;
	if (points.length === 1 || at <= points[0][0]) return points[0][1];
	for (let i = 0; i < points.length - 1; i++) {
		const [t0, v0] = points[i];
		const [t1, v1] = points[i + 1];
		if (at < t1) return t1 <= t0 ? v0 : v0 + ((v1 - v0) * (at - t0)) / (t1 - t0);
	}
	return points[points.length - 1][1];
}

/** Mirror of `TextOverlay::sample`: where the text is drawn and how opaque it is at timeline time `t`. */
export function sampleOverlay(o: TextOverlay, t: number): { x: number; y: number; opacity: number } {
	const ks = [...(o.keyframes ?? [])].sort((a, b) => a.time - b.time);
	if (ks.length === 0) return { x: o.pos_x, y: o.pos_y, opacity: 1 };
	const local = t - o.start;
	return {
		x: lerp(ks.map((k) => [k.time, k.pos_x]), local) ?? o.pos_x,
		y: lerp(ks.map((k) => [k.time, k.pos_y]), local) ?? o.pos_y,
		opacity: lerp(ks.map((k) => [k.time, k.opacity]), local) ?? 1
	};
}

/** Padding the engine's text box adds around the line, as a fraction of frame height. */
export function boxPadding(o: TextOverlay, frameH?: number): number {
	return o.bg ? BOX_BORDER_PX / (frameH && frameH > 0 ? frameH : DEFAULT_FRAME_H) : 0;
}

/** The centre of a title after the pointer travelled `dx`, `dy` pixels across a `w` x `h` frame. */
export function dragPosition(
	from: { x: number; y: number },
	dx: number,
	dy: number,
	w: number,
	h: number
): { x: number; y: number } {
	return {
		x: clamp(from.x + (w > 0 ? dx / w : 0), 0, 1),
		y: clamp(from.y + (h > 0 ? dy / h : 0), 0, 1)
	};
}

/** The font height after dragging a corner from `startDist` to `dist` pixels away from the box centre. */
export function scaledSize(startSize: number, startDist: number, dist: number): number {
	if (!(startDist > 0) || !Number.isFinite(dist)) return startSize;
	return clamp(startSize * (dist / startDist), TITLE_SIZE_MIN, Math.max(TITLE_SIZE_MAX, startSize));
}

/** An animated title's keyframes with the pose at timeline time `t` set to `(x, y)`: a keyframe
 *  within a frame of `t` is moved, otherwise one is added carrying the opacity already in force. */
export function withKeyframeAt(o: TextOverlay, t: number, x: number, y: number): TextKeyframe[] {
	const local = round3(Math.max(0, t - o.start));
	const keep = (o.keyframes ?? []).filter((k) => Math.abs(k.time - local) >= 0.02).map((k) => ({ ...k }));
	const existing = (o.keyframes ?? []).find((k) => Math.abs(k.time - local) < 0.02);
	const opacity = existing?.opacity ?? sampleOverlay(o, t).opacity;
	return [...keep, { time: local, pos_x: x, pos_y: y, opacity }].sort((a, b) => a.time - b.time);
}

/** Row index per overlay so that overlapping ones stack instead of covering each other. */
export function packRows(overlays: TextOverlay[]): Map<string, number> {
	const rows = new Map<string, number>();
	const ends: number[] = [];
	for (const o of [...overlays].sort((a, b) => a.start - b.start || a.end - b.end)) {
		let r = ends.findIndex((end) => end <= o.start + 1e-6);
		if (r < 0) r = ends.length;
		ends[r] = o.end;
		rows.set(o.id, r);
	}
	return rows;
}

/** The candidate within `threshold` of `time`, else `time`. */
export function snapTime(time: number, candidates: number[], threshold: number): number {
	let best = time;
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

/** A span of length `dur` starting at `start`, with whichever edge is nearer a candidate landed on it. */
export function snapSpanStart(start: number, dur: number, candidates: number[], threshold: number): number {
	const head = snapTime(start, candidates, threshold);
	const tail = snapTime(start + dur, candidates, threshold) - dur;
	const headMoved = head !== start;
	const tailMoved = Math.abs(tail - start) > 1e-9;
	const best = headMoved && (!tailMoved || Math.abs(head - start) <= Math.abs(tail - start)) ? head : tailMoved ? tail : start;
	return Math.max(0, best);
}

/** Move one edge of a span to `to`, keeping the span at least `MIN_TITLE` long and inside `[0, ∞)`. */
export function trimSpan(
	start: number,
	end: number,
	edge: 'l' | 'r',
	to: number
): { start: number; end: number } {
	if (edge === 'l') return { start: clamp(to, 0, end - MIN_TITLE), end };
	return { start, end: Math.max(to, start + MIN_TITLE) };
}

/** Where a picture of `imgAspect` lands, as percentages of a frame of `frameAspect`, when it is
 *  fitted with `object-fit: contain` — the area title fractions are measured against. */
export function containRect(imgAspect: number, frameAspect: number): { left: number; top: number; width: number; height: number } {
	if (!(imgAspect > 0) || !(frameAspect > 0)) return { left: 0, top: 0, width: 100, height: 100 };
	if (imgAspect > frameAspect) {
		const h = (frameAspect / imgAspect) * 100;
		return { left: 0, top: (100 - h) / 2, width: 100, height: h };
	}
	const w = (imgAspect / frameAspect) * 100;
	return { left: (100 - w) / 2, top: 0, width: w, height: 100 };
}

/** Keep an animation's fade-in and fade-out when the title's length changes. Keyframe times are
 *  relative to `start`: those in the first half stay anchored to the start, those in the second
 *  half move with the end, so a preset title still fades in and out at any length. */
export function retimeKeyframes(
	keyframes: TextKeyframe[],
	oldDur: number,
	newDur: number
): TextKeyframe[] {
	if (keyframes.length === 0 || Math.abs(newDur - oldDur) < 1e-9) return keyframes.map((k) => ({ ...k }));
	const delta = newDur - oldDur;
	const sorted = [...keyframes].sort((a, b) => a.time - b.time);
	let floor = 0;
	return sorted.map((k) => {
		const tail = k.time > oldDur / 2;
		const time = round3(clamp(tail ? k.time + delta : k.time, floor, Math.max(newDur, floor)));
		floor = time;
		return { ...k, time };
	});
}
