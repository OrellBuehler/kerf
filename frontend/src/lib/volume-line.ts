/* The arithmetic of a clip's volume line: where a gain sits in the clip's height,
 * and what gain a drag of the line means.
 *
 * The line is on a dB scale — a level is judged in dB, and a linear scale spends
 * almost all its height above -6 dB — from `LINE_DB_MIN` at the bottom to the top
 * of the shared range, `MAX_GAIN` (+6 dB: the Inspector's slider and the track
 * fader stop at the same place). The bottom edge is silence (`-Infinity`), not
 * -36 dB: dragging all the way down mutes the clip, as a fader does.
 *
 * A drag is *relative* to where the line was grabbed, so pressing a few pixels off
 * the line does not make it jump, and it has a detent at exactly 0 dB. Unity is
 * not just a nice number: the export omits a clip's volume from the graph at 1.0,
 * so landing on it exactly is what leaves an untouched clip's render untouched.
 * It is relative to the clip's *real* value too, not the clamped place the line is
 * drawn at: a clip an agent set to 6x draws at the top, and a small drag moves it
 * from 6x rather than collapsing it to the top of the scale. */

import { dbToGain, gainToDb, MAX_GAIN } from './mixer';

/** dB at the bottom of the line's travel — below it a clip is silent. */
export const LINE_DB_MIN = -36;
/** dB at the top: `MAX_GAIN`. */
export const LINE_DB_MAX = gainToDb(MAX_GAIN);
/** Within this many dB of unity a drag lands on exactly 1.0. */
export const UNITY_DETENT_DB = 0.5;
/** The bottom of the travel (a fraction of it) that means silence. */
export const SILENT_FRACTION = 0.015;
/** A dragged value is rounded to this many dB. */
export const DB_STEP = 0.1;

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));

/** Position along the line's travel, 0 (bottom) to 1 (top), of a linear gain. */
export function gainToFraction(gain: number): number {
	if (!(gain > 0)) return 0;
	return clamp01((gainToDb(gain) - LINE_DB_MIN) / (LINE_DB_MAX - LINE_DB_MIN));
}

/** The linear gain at a position along the line's travel. The bottom is silence. */
export function fractionToGain(f: number): number {
	const x = clamp01(f);
	if (x <= SILENT_FRACTION) return 0;
	return dbToGain(LINE_DB_MIN + x * (LINE_DB_MAX - LINE_DB_MIN));
}

/** Below this a drag that is heading down means silence. */
export const SILENT_DB = LINE_DB_MIN + SILENT_FRACTION * (LINE_DB_MAX - LINE_DB_MIN);

/**
 * The gain a drag means: the line was grabbed at `startGain` and the pointer has
 * since moved `dy` pixels down the screen over a travel of `rangePx`. Worked in dB
 * from the clip's real level (a silent clip is grabbed at the bottom), capped at
 * the top of the scale or at the clip's own level when that is higher, rounded to
 * `DB_STEP`, snapped to exactly 1.0 near 0 dB and to exactly `MAX_GAIN` at the top.
 */
export function dragGain(startGain: number, dy: number, rangePx: number): number {
	if (!(rangePx > 0)) return startGain;
	const grabDb = startGain > 0 ? gainToDb(startGain) : LINE_DB_MIN;
	const db = Math.min(Math.max(LINE_DB_MAX, grabDb), grabDb - (dy * (LINE_DB_MAX - LINE_DB_MIN)) / rangePx);
	// Down into the bottom of the travel is a mute; so is staying there from silence.
	if (db <= SILENT_DB && (dy > 0 || startGain <= 0)) return 0;
	if (grabDb > LINE_DB_MAX && db >= grabDb) return startGain; // already past the top: up changes nothing
	if (Math.abs(db) < UNITY_DETENT_DB) return 1;
	if (grabDb <= LINE_DB_MAX && db >= LINE_DB_MAX - DB_STEP / 2) return MAX_GAIN;
	return dbToGain(Math.round(db / DB_STEP) * DB_STEP);
}

/** Vertical geometry of the line inside a clip `height` px tall: the top is kept
 *  clear of the fade handles, the bottom a few pixels in from the edge. */
export function lineTravel(height: number, topInset = 14, bottomInset = 4): { top: number; range: number } {
	const range = Math.max(1, height - topInset - bottomInset);
	return { top: topInset, range };
}

/** The line's y (px from the clip's top) for `gain` in a clip `height` px tall. */
export function lineY(gain: number, height: number, topInset?: number, bottomInset?: number): number {
	const { top, range } = lineTravel(height, topInset, bottomInset);
	return top + (1 - gainToFraction(gain)) * range;
}
