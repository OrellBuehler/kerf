/* Timeline zoom — pure. `ui.zoom` is pixels per second; everything that turns a
 * gesture into a new zoom (a step, a wheel notch, "fit") or keeps a point of the
 * cut still while it changes lives here, so `Timeline.svelte` only has to apply it.
 *
 * The range is wide on purpose. The floor lets an hour of cut fit a window
 * (zoom-to-fit would otherwise have nothing to land on); the ceiling is
 * frame-level work — 2000 px/s is 33 px a frame at 60 fps, 67 at 30. Nothing
 * else in the timeline assumes a range: the waveform's rung choice
 * (`bucketCells`) scales to any px/s and bottoms out at the engine's finest
 * level (a 2 ms bucket — 4 px at the ceiling, drawn as a block, not
 * interpolated), and frame snapping works in seconds. What *is* bounded is the
 * lane's own width, because a browser lays out only so many pixels
 * (`LANE_PX_CAP`) — so the ceiling comes down for a very long cut. */

/** Slowest zoom, px/s: a 10-hour cut still fits an 1800 px window. */
export const ZOOM_MIN = 0.05;
/** Fastest zoom, px/s: frame-level work. */
export const ZOOM_MAX = 2000;
/** Where a fresh session starts. */
export const ZOOM_DEFAULT = 36;
/** One step of a +/- key or button: a ratio, because the range spans decades and
 *  a fixed number of pixels is nothing at one end and everything at the other. */
export const ZOOM_STEP = 1.25;

/** Width of the lane past the end of the cut, px (room to drop a clip after it). */
export const LANE_TAIL_PX = 48;
/** The lane is never narrower than this, px, so an empty timeline has a surface. */
export const LANE_MIN_PX = 760;
/** The lane is laid out for at least this many seconds, whatever is on it. */
export const MIN_SPAN_SEC = 8;
/** Widest the lane may be asked to get, px. Layout engines top out in the tens of
 *  millions (and lose sub-pixel precision past 2^24); this stays well inside. */
export const LANE_PX_CAP = 8_000_000;

/** The width of the lane for a cut `duration` seconds long at `pxPerSec`. */
export function laneWidth(duration: number, pxPerSec: number): number {
	return Math.max(LANE_MIN_PX, Math.ceil(Math.max(duration, MIN_SPAN_SEC) * pxPerSec) + LANE_TAIL_PX);
}

/** The fastest zoom a cut of `duration` seconds may be shown at: `ZOOM_MAX`, or less
 *  when the lane would otherwise pass `LANE_PX_CAP`. */
export function zoomCeiling(duration: number): number {
	const span = Math.max(Number.isFinite(duration) ? duration : 0, MIN_SPAN_SEC);
	return Math.max(ZOOM_MIN, Math.min(ZOOM_MAX, LANE_PX_CAP / span));
}

/** `zoom` held inside the range (and, with a `duration`, the lane-width cap). A
 *  value that is not a number is the default rather than NaN-in-the-layout. */
export function clampZoom(zoom: number, duration = 0): number {
	if (!Number.isFinite(zoom) || zoom <= 0) return ZOOM_DEFAULT;
	return Math.min(zoomCeiling(duration), Math.max(ZOOM_MIN, zoom));
}

/** One step in (`dir` 1) or out (-1). */
export function stepZoom(zoom: number, dir: 1 | -1, duration = 0): number {
	return clampZoom(zoom * ZOOM_STEP ** dir, duration);
}

/** Wheel gain: a 100 px notch is ~16%, which is what the stepped zoom this replaced
 *  did, and a trackpad pinch (a stream of deltas of a few px) zooms smoothly
 *  instead of a notch per event. */
const WHEEL_GAIN = 0.0015;
/** One event never zooms more than this ratio, whatever the device reports. */
const WHEEL_MAX_RATIO = 1.5;
/** `WheelEvent.deltaMode`: pixels, lines, pages. */
const WHEEL_UNIT = [1, 33, 400];

/** The zoom ratio one wheel event asks for (> 1 in, < 1 out) — `deltaY` scrolling
 *  up zooms in. `deltaMode` is the event's, so a browser that reports lines
 *  (Firefox) and one that reports pixels agree on how far a notch goes. */
export function wheelZoomFactor(deltaY: number, deltaMode = 0): number {
	if (!Number.isFinite(deltaY)) return 1;
	const px = deltaY * (WHEEL_UNIT[deltaMode] ?? 1);
	const ratio = Math.exp(-px * WHEEL_GAIN);
	return Math.min(WHEEL_MAX_RATIO, Math.max(1 / WHEEL_MAX_RATIO, ratio));
}

/** What the scroller needs to show after a zoom change. */
export interface ZoomView {
	zoom: number;
	scrollLeft: number;
}

/**
 * The view after zooming to `nextZoom` with the point `pointerOffset` px in from
 * the left edge of the visible lane held still: the time under that point before
 * is the time under it after. `scrollLeft` is what the scroller is told; it
 * cannot go negative, and the browser clamps the far end once the lane has been
 * resized (a point near either end of the cut can't stay put — there's no
 * scrolling past it).
 */
export function zoomAround(view: ZoomView, pointerOffset: number, nextZoom: number): ZoomView {
	const time = (view.scrollLeft + pointerOffset) / view.zoom;
	return { zoom: nextZoom, scrollLeft: Math.max(0, time * nextZoom - pointerOffset) };
}

/** `scrollLeft` that puts `time` at `offset` px into the visible lane at `zoom`. */
export function scrollFor(time: number, offset: number, zoom: number): number {
	return Math.max(0, time * zoom - offset);
}

/**
 * The zoom at which the whole cut — and the lane's tail — fits `viewW` px of
 * visible lane; `null` when there is nothing to fit (an empty cut, or no width
 * yet). Held inside the range, so a cut too long for `ZOOM_MIN` shows as much as
 * it can rather than nothing.
 */
export function fitZoom(duration: number, viewW: number): number | null {
	if (!(duration > 0) || !(viewW > 0)) return null;
	const room = viewW - LANE_TAIL_PX;
	if (room <= 0) return clampZoom(ZOOM_MIN, duration);
	return clampZoom(room / duration, duration);
}

// ---- the slider: logarithmic, so every decade gets the same travel -------------

const SPAN = Math.log(ZOOM_MAX / ZOOM_MIN);

/** Slider position, 0..1, of a zoom. */
export function zoomToSlider(zoom: number): number {
	const z = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, Number.isFinite(zoom) && zoom > 0 ? zoom : ZOOM_DEFAULT));
	return Math.log(z / ZOOM_MIN) / SPAN;
}

/** The zoom at slider position `p` (0..1). */
export function sliderToZoom(p: number): number {
	const x = Math.min(1, Math.max(0, Number.isFinite(p) ? p : 0));
	return ZOOM_MIN * Math.exp(x * SPAN);
}

/** A zoom as the tooltip says it: `36 px/s`, `0.4 px/s`, `1,250 px/s`. */
export function zoomLabel(zoom: number): string {
	const z = Number.isFinite(zoom) ? zoom : ZOOM_DEFAULT;
	const text = z >= 100 ? Math.round(z).toLocaleString('en-US') : z >= 10 ? z.toFixed(0) : z >= 1 ? z.toFixed(1) : z.toFixed(2);
	return `${text} px/s`;
}
