/* Scrolling the timeline for a gesture that has reached its edge — pure. A clip dragged, an edge
 * trimmed, a marquee, a roll / slip / slide, a scrub or a marker moved can only go where the
 * window shows, and a long cut is many windows wide. So while one of them is held, the view
 * scrolls toward the edge the pointer is pushing on, faster the further into the band it is
 * (`edgeSpeed`), and the gesture is told to read the pointer again, since the lanes moved under
 * it (`scrolled`).
 *
 * Two guards make it a convenience and not a hazard. The speed is a *rate* (px/s over the real
 * frame time, remainders carried, since a browser drops a fractional scroll), so a slow 60 Hz and
 * a fast 144 Hz screen travel alike. And a gesture that began inside the band does not run away
 * with the first pixel of travel: it waits until the pointer has been in the interior, or moved
 * `ARM_PX` toward that edge from where it was pressed. */

/** How deep the band along each edge is, css px. */
export const EDGE_BAND_PX = 48;
/** Speed at the bottom of the band (and beyond the edge), px/s, per axis. */
export const MAX_SPEED = { x: 1400, y: 700 } as const;
/** How far toward an edge a press that began in its band has to travel before it scrolls, px. */
export const ARM_PX = 16;
/** The longest frame the scroll takes into account, ms: a stalled frame must not be a leap. */
const MAX_DT_MS = 50;

/** The signed speed (px/s, positive = toward `hi`) for a pointer at `pos` on an axis whose visible
 *  span is `lo`..`hi`: 0 in the interior, growing linearly through the band at each end to `max`
 *  at its inner edge and beyond (the pointer left the window altogether). */
export function edgeSpeed(pos: number, lo: number, hi: number, max: number, band = EDGE_BAND_PX): number {
	if (!Number.isFinite(pos) || !(hi > lo) || !(max > 0)) return 0;
	const b = Math.min(band, (hi - lo) / 2);
	if (!(b > 0)) return 0;
	if (pos < lo + b) return -max * Math.min(1, (lo + b - pos) / b);
	if (pos > hi - b) return max * Math.min(1, (pos - (hi - b)) / b);
	return 0;
}

/** The part of the window the lanes are visible in, client px. */
export interface ScrollRegion {
	left: number;
	right: number;
	top: number;
	bottom: number;
}

export interface AutoscrollEnv {
	region(): ScrollRegion | null;
	/** Where the pointer is now, client px. */
	pointer(): { x: number; y: number } | null;
	/** Scroll the view by whole pixels; the pixels it actually moved (0 at an end). */
	scrollBy(dx: number, dy: number): { x: number; y: number };
	/** The view moved under a held pointer: re-read it. */
	scrolled(): void;
	now(): number;
	request(cb: () => void): number;
	cancel(id: number): void;
}

export interface Autoscroll {
	/** Begin watching, with the pointer at `origin` (where the gesture was pressed). */
	start(origin: { x: number; y: number }): void;
	/** Stop; a stopped one holds no frame callback. */
	stop(): void;
	readonly running: boolean;
}

/**
 * The loop. `axes` limits it to the directions the gesture can use — a scrub only scrolls
 * sideways, a clip dragged between tracks both ways.
 */
export function createAutoscroll(env: AutoscrollEnv, axes: { x: boolean; y: boolean } = { x: true, y: true }): Autoscroll {
	let id: number | null = null;
	let running = false;
	let last = 0;
	let origin = { x: 0, y: 0 };
	// Per axis: has the pointer been clear of the band, or armed itself by travelling toward it.
	let clear = { x: false, y: false };
	let carry = { x: 0, y: 0 };

	const speedOf = (axis: 'x' | 'y', pos: number, lo: number, hi: number): number => {
		const v = edgeSpeed(pos, lo, hi, MAX_SPEED[axis]);
		if (v === 0) {
			clear[axis] = true;
			carry[axis] = 0;
			return 0;
		}
		if (!clear[axis]) {
			// Pressed inside the band: it waits for the pointer to head for this edge.
			if ((pos - origin[axis]) * Math.sign(v) >= ARM_PX) clear[axis] = true;
			else return 0;
		}
		return v;
	};

	const tick = () => {
		id = null;
		const now = env.now();
		const dt = Math.min(Math.max(now - last, 0), MAX_DT_MS) / 1000;
		last = now;
		const region = env.region();
		const p = env.pointer();
		if (region && p) {
			const vx = axes.x ? speedOf('x', p.x, region.left, region.right) : 0;
			const vy = axes.y ? speedOf('y', p.y, region.top, region.bottom) : 0;
			const want = { x: vx * dt + carry.x, y: vy * dt + carry.y };
			const whole = { x: Math.trunc(want.x), y: Math.trunc(want.y) };
			if (whole.x !== 0 || whole.y !== 0) {
				const moved = env.scrollBy(whole.x, whole.y);
				carry = {
					x: moved.x === 0 ? 0 : want.x - whole.x,
					y: moved.y === 0 ? 0 : want.y - whole.y
				};
				if (moved.x !== 0 || moved.y !== 0) env.scrolled();
			} else {
				carry = want;
			}
		}
		// `stop()` during `scrolled()` (the gesture ended on that re-read) must not be undone.
		if (running) id = env.request(tick);
	};

	return {
		start(o) {
			if (running) return;
			running = true;
			origin = { ...o };
			clear = { x: false, y: false };
			carry = { x: 0, y: 0 };
			last = env.now();
			id = env.request(tick);
		},
		stop() {
			running = false;
			if (id !== null) env.cancel(id);
			id = null;
		},
		get running() {
			return running;
		}
	};
}

/** Where to scroll so the playhead stays in view, or null when it already is. While playing the
 *  view follows it continuously, pinned just inside the edge it is running toward (the page flip of
 *  75 % of a window it replaces made the picture jump under the eye); for a jump to somewhere off
 *  screen (a marker, Home / End, a click in the minimap) the playhead lands a quarter in, and the
 *  caller scrolls there smoothly. */
export function followScroll(o: {
	laneX: number;
	scrollLeft: number;
	viewW: number;
	playing: boolean;
}): { left: number; smooth: boolean } | null {
	const { laneX, scrollLeft, viewW } = o;
	if (!Number.isFinite(laneX) || !(viewW > 0)) return null;
	const margin = Math.min(80, viewW * 0.15);
	const behind = laneX < scrollLeft + margin;
	const ahead = laneX > scrollLeft + viewW - margin;
	if (!behind && !ahead) return null;
	if (o.playing) return { left: Math.max(0, behind ? laneX - margin : laneX - (viewW - margin)), smooth: false };
	return { left: Math.max(0, laneX - viewW * 0.25), smooth: true };
}

/** The lane's width while a gesture is held: it grows with the gesture so there is room to scroll
 *  into past the end of the cut, and a clip can be dropped there. */
export function liveEnd(duration: number, ...ends: (number | null | undefined)[]): number {
	let end = Math.max(0, duration);
	for (const e of ends) if (e !== null && e !== undefined && Number.isFinite(e) && e > end) end = e;
	return end;
}
