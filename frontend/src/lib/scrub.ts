/* The transport's scrub bar — pure. A bar is a time axis: the pointer's x on it is a time, and a
 * drag moves the playhead as it goes. What this holds is the arithmetic and the one rule that makes
 * a live drag affordable: the picture under the playhead is an FFmpeg composite, so the playhead is
 * moved at most once per animation frame, with the newest pointer position, however fast the pointer
 * reports. */

/** The time under the pointer on a bar `width` px wide whose left edge is at `left`, held to the cut. */
export function scrubTime(clientX: number, left: number, width: number, duration: number): number {
	if (!(width > 0) || !(duration > 0) || !Number.isFinite(clientX) || !Number.isFinite(left)) return 0;
	return Math.min(duration, Math.max(0, ((clientX - left) / width) * duration));
}

/** How far along the bar a time is, 0..1: 0 for a cut with no length. */
export function scrubFraction(time: number, duration: number): number {
	if (!(duration > 0) || !Number.isFinite(time)) return 0;
	return Math.min(1, Math.max(0, time / duration));
}

/** The bar's range, as fractions, from the in and out marks — only when both are set and enclose something. */
export function scrubRange(markIn: number | null, markOut: number | null, duration: number): { from: number; to: number } | null {
	if (markIn === null || markOut === null || !(markOut > markIn) || !(duration > 0)) return null;
	return { from: scrubFraction(markIn, duration), to: scrubFraction(markOut, duration) };
}

/** What `frameLimited` needs of a window: its way of asking for a frame, and of taking the ask back. */
export interface FrameClock {
	request(cb: () => void): number;
	cancel(id: number): void;
}

/**
 * `apply` at most once per frame, with the newest value pushed since. `flush` applies a waiting
 * value now (a drag's last position, before the gesture ends); `cancel` forgets it (a gesture
 * given up, which must not move the playhead after the fact).
 */
export function frameLimited<T>(apply: (value: T) => void, clock: FrameClock) {
	let waiting: { value: T } | null = null;
	let id: number | null = null;
	const drop = () => {
		if (id !== null) clock.cancel(id);
		id = null;
	};
	return {
		push(value: T) {
			waiting = { value };
			if (id !== null) return;
			id = clock.request(() => {
				id = null;
				const w = waiting;
				waiting = null;
				if (w) apply(w.value);
			});
		},
		flush() {
			drop();
			const w = waiting;
			waiting = null;
			if (w) apply(w.value);
		},
		cancel() {
			drop();
			waiting = null;
		},
		get waiting(): boolean {
			return waiting !== null;
		}
	};
}

/** One marker's tick on the bar. */
export interface MarkerTick {
	/** How far along, 0..1. */
	at: number;
	color: string | null;
	name: string;
}

/** The ticks of the markers along the bar: those outside the cut are left out, and two on the same
 *  spot are one tick (the first keeps its colour and name). */
export function markerTicks(
	markers: readonly { time: number; color?: string | null; name?: string }[],
	duration: number
): MarkerTick[] {
	if (!(duration > 0)) return [];
	const seen = new Set<number>();
	const out: MarkerTick[] = [];
	for (const m of markers) {
		if (!Number.isFinite(m.time) || m.time < 0 || m.time > duration) continue;
		const at = scrubFraction(m.time, duration);
		if (seen.has(at)) continue;
		seen.add(at);
		out.push({ at, color: m.color || null, name: m.name ?? '' });
	}
	return out.sort((a, b) => a.at - b.at);
}

/** Where a chip `width` px wide centred on a point `x` px along a bar `barWidth` px wide may sit: held
 *  inside the bar, so the time is never cut off at either end. */
export function chipLeft(x: number, width: number, barWidth: number): number {
	if (!(barWidth > 0)) return 0;
	return Math.min(Math.max(x - width / 2, 0), Math.max(0, barWidth - width));
}
