/* The timeline ruler's ticks — pure. With the zoom range wide enough to go from
 * an hour on screen to single frames, the ruler can neither render every tick of
 * the cut (an hour at frame level is 100 000+) nor use a fixed ladder (a 2 s tick
 * at 2000 px/s is 4000 px of nothing between labels). So the step follows the
 * zoom — the smallest one that leaves a label room to be read — and only the
 * ticks inside the part of the lane that is on (or near) the screen exist. */

/** The least room, px, between two labels. */
export const MIN_LABEL_PX = 100;

/** Label steps, seconds, finest first. Sub-second steps exist for frame-level
 *  zoom; past an hour the steps are whole multiples of one. */
export const RULER_STEPS: readonly number[] = [
	0.05, 0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 14400, 36000
];

/** The label step for a zoom: the finest that leaves `minGapPx` between labels. */
export function rulerStep(pxPerSec: number, minGapPx = MIN_LABEL_PX): number {
	if (!(pxPerSec > 0)) return RULER_STEPS[RULER_STEPS.length - 1];
	for (const s of RULER_STEPS) if (s * pxPerSec >= minGapPx - 1e-9) return s;
	return RULER_STEPS[RULER_STEPS.length - 1];
}

/** One tick: `k` is its index on the step grid (a stable key, and what the grid
 *  lines alternate on), `t` its time in seconds. */
export interface Tick {
	k: number;
	t: number;
}

/** The ticks of `step` seconds in `[loSec, hiSec]`. Each time is one multiply of
 *  an integer, so no tick drifts however far along it is. Never more than
 *  `limit`, however the range was asked for. */
export function ticksIn(loSec: number, hiSec: number, step: number, limit = 2000): Tick[] {
	if (!(step > 0) || !(hiSec >= loSec)) return [];
	const first = Math.max(0, Math.ceil(loSec / step - 1e-9));
	const last = Math.floor(hiSec / step + 1e-9);
	const out: Tick[] = [];
	for (let k = first; k <= last && out.length < limit; k++) out.push({ k, t: k * step });
	return out;
}

/** Decimal places a label of this step needs: whole seconds, tenths, hundredths. */
export const labelDecimals = (step: number): number => (step >= 1 ? 0 : step >= 0.1 ? 1 : 2);

/** The label of tick `k` on a `step` grid: `mm:ss`, or `mm:ss.f` / `mm:ss.ff` for
 *  a sub-second step. Counts in whole units before splitting, so `0.3 s` never
 *  reads `00:00.2`. */
export function tickLabel(k: number, step: number): string {
	const d = labelDecimals(step);
	const scale = 10 ** d;
	const units = Math.round(k * step * scale);
	const whole = Math.floor(units / scale);
	const frac = units - whole * scale;
	const m = Math.floor(whole / 60);
	const s = whole % 60;
	const base = `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
	return d === 0 ? base : `${base}.${String(frac).padStart(d, '0')}`;
}

/** Frames are drawn on the ruler once one is at least this wide, px. */
export const FRAME_TICK_PX = 8;

/** Whether the zoom is deep enough for a tick per frame. */
export function showsFrameTicks(pxPerSec: number, fps: number): boolean {
	return fps > 0 && pxPerSec / fps >= FRAME_TICK_PX;
}

/** The frame boundaries in `[loSec, hiSec]` (frame `k` is at `k / fps`). Empty when
 *  the zoom is too shallow to draw them, so asking is always safe. */
export function frameTicksIn(loSec: number, hiSec: number, fps: number, pxPerSec: number, limit = 2000): Tick[] {
	if (!showsFrameTicks(pxPerSec, fps) || !(hiSec >= loSec)) return [];
	const first = Math.max(0, Math.ceil(loSec * fps - 1e-9));
	const last = Math.floor(hiSec * fps + 1e-9);
	const out: Tick[] = [];
	for (let k = first; k <= last && out.length < limit; k++) out.push({ k, t: k / fps });
	return out;
}
