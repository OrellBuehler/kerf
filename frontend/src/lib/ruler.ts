/* The timeline ruler's ticks — pure. With the zoom range wide enough to go from
 * an hour on screen to single frames, the ruler can neither render every tick of
 * the cut (an hour at frame level is 100 000+) nor use a fixed ladder (a 2 s tick
 * at 2000 px/s is 4000 px of nothing between labels). So the step follows the
 * zoom — the smallest one that leaves a label room to be read — and only the
 * ticks inside the part of the lane that is on (or near) the screen exist.
 *
 * A ruler is read against the timecode beside it, so its steps are *timecode*
 * steps: a second is `nominal` frames (30 at 29.97 too, the non-drop count), and a
 * step under a second is a whole number of frames. A label is then the project's
 * own `HH:MM:SS` / `HH:MM:SS:FF` of its tick (`timecode.ts`; drop-frame `HH;MM;SS;FF` at 29.97
 * and 59.94, always with its frames) and never reads a frame
 * off the one it is, whatever the rate. Between two labelled (major) ticks sit
 * minor ticks, a taller one halfway when the division is even; once a frame is wide
 * enough the frame ticks are the minors. */

import { formatTimecode, timecodeRate } from './timecode';
import type { TimeMode } from './time-format';

/** The least room, px, between two labels. */
export const MIN_LABEL_PX = 100;

/** Label steps of a second and more, in timecode seconds, finest first; past an hour
 *  they are whole multiples of one. */
export const RULER_SECONDS: readonly number[] = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 14400, 36000];

/** Minor ticks between two labelled ticks of each step (in timecode seconds): the
 *  ones that divide it into round numbers. */
const MINOR_DIVISIONS: ReadonlyMap<number, number> = new Map([
	[1, 10],
	[2, 10],
	[5, 10],
	[10, 10],
	[15, 3],
	[30, 6],
	[60, 6],
	[120, 4],
	[300, 5],
	[600, 10],
	[900, 3],
	[1800, 6],
	[3600, 6],
	[7200, 4],
	[14400, 4],
	[36000, 10]
]);

/** Minor ticks are drawn only when they are at least this far apart, px. */
export const MIN_MINOR_PX = 6;

/** The frames a timecode second holds at `fps`: 30 at 29.97, the non-drop count. */
export const nominalRate = (fps: number): number => Math.max(1, Math.round(Number.isFinite(fps) && fps > 0 ? fps : 30));

/** Where the ruler puts its ticks at a zoom. */
export interface RulerGrid {
	/** Seconds between two labelled (major) ticks. */
	step: number;
	/** The label carries a frame field: the step is under a timecode second. */
	frames: boolean;
	/** Minor ticks per step, the major included: 1 when there are none to draw. */
	divisions: number;
}

/** Frame counts a sub-second step may be, finest first, for a rate: 1, 2, 5, 10 and the half second. */
function frameSteps(fps: number): number[] {
	const nominal = nominalRate(fps);
	const steps = new Set([1, 2, 5, 10, Math.round(nominal / 2)].filter((f) => f >= 1 && f < nominal));
	return [...steps].sort((a, b) => a - b);
}

function minorDivisions(seconds: number, stepPx: number): number {
	const n = MINOR_DIVISIONS.get(seconds) ?? 1;
	return stepPx / n >= MIN_MINOR_PX ? n : 1;
}

/** The ruler at a zoom: the finest step that leaves `minGapPx` between labels. */
export function rulerGrid(pxPerSec: number, fps = 30, minGapPx = MIN_LABEL_PX): RulerGrid {
	const rate = Number.isFinite(fps) && fps > 0 ? fps : 30;
	const nominal = nominalRate(rate);
	const room = (step: number) => pxPerSec > 0 && step * pxPerSec >= minGapPx - 1e-9;
	for (const f of frameSteps(rate)) {
		if (room(f / rate)) return { step: f / rate, frames: true, divisions: 1 };
	}
	for (const s of RULER_SECONDS) {
		const step = (s * nominal) / rate;
		if (room(step)) return { step, frames: false, divisions: minorDivisions(s, step * pxPerSec) };
	}
	const last = RULER_SECONDS[RULER_SECONDS.length - 1];
	return { step: (last * nominal) / rate, frames: false, divisions: 1 };
}

/** The label step for a zoom, seconds (the grid's `step`). */
export function rulerStep(pxPerSec: number, fps = 30, minGapPx = MIN_LABEL_PX): number {
	return rulerGrid(pxPerSec, fps, minGapPx).step;
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

/** A minor tick: on the minor grid (`k` counts them from 0), taller when it is halfway. */
export interface MinorTick extends Tick {
	mid: boolean;
}

/** The minor ticks of a grid in `[loSec, hiSec]`: every division of a step but the ones
 *  a labelled tick already stands on. */
export function minorTicksIn(loSec: number, hiSec: number, grid: RulerGrid, limit = 2000): MinorTick[] {
	const n = grid.divisions;
	if (n < 2) return [];
	const out: MinorTick[] = [];
	for (const t of ticksIn(loSec, hiSec, grid.step / n, limit * 2)) {
		if (t.k % n === 0) continue;
		out.push({ k: t.k, t: t.t, mid: n % 2 === 0 && t.k % n === n / 2 });
		if (out.length >= limit) break;
	}
	return out;
}

/** A tick's time in seconds mode, like the readouts there (`m:ss.d`) but without a tenth that a whole-second step never has: `m:ss`
 *  (`h:mm:ss` from an hour) on a whole-second step, and below a second the fraction too, to the millisecond with
 *  trailing zeros dropped (`0:00.5`, `0:00.033`) — one decimal would label two frame ticks alike. */
function secondsLabel(t: number, fine: boolean): string {
	const ms = Math.round(Math.max(0, Number.isFinite(t) ? t : 0) * 1000);
	const whole = fine ? Math.floor(ms / 1000) : Math.round(ms / 1000);
	const h = Math.floor(whole / 3600);
	const m = Math.floor((whole % 3600) / 60);
	const ss = String(whole % 60).padStart(2, '0');
	const frac = fine ? `.${String(ms % 1000).padStart(3, '0').replace(/0+$/, '') || '0'}` : '';
	return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}${frac}` : `${m}:${ss}${frac}`;
}

/** The label of tick `k` on a grid: its time as the interface prints one. In timecode mode (the default) the project
 *  timecode — `HH:MM:SS` for a whole-second step, `HH:MM:SS:FF` below it; at a drop-frame rate it is the drop-frame label,
 *  frames and all, as the playhead's reads (Premiere's ruler too): the ticks stay on whole frames of nominal seconds, and a
 *  label cut at the second would read off by the frames dropped so far. In seconds mode `m:ss`, with the fraction below a
 *  second. */
export function tickLabel(k: number, grid: RulerGrid, fps: number, mode: TimeMode = 'timecode'): string {
	if (mode === 'seconds') return secondsLabel(k * grid.step, grid.frames);
	return formatTimecode(k * grid.step, fps, { frames: grid.frames || timecodeRate(fps).drop > 0 });
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
