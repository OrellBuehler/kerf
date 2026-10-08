/* How a loudness measurement (`Levels`) is worded on the Mixer: the numbers each
 * strip reads, the one-line verdict beside them, what range was measured, and
 * whether the measurement is still about the cut that is on screen. Pure — the
 * panel only lays these out. */

import { LEVELS_TARGET_LUFS } from './levels';
import type { LevelReading, Levels } from './types';

/** `-14.2`, or `−∞` for a silent mix. Never `-0.0`. */
export function fmtNumber(v: number | null | undefined, digits = 1): string {
	if (v === null || v === undefined || !Number.isFinite(v)) return '−∞';
	const x = Math.abs(v) < 0.5 * 10 ** -digits ? 0 : v;
	return x.toFixed(digits);
}

/** A strip's reading, ready to lay out. */
export interface ReadingText {
	lufs: string;
	truePeak: string;
	range: string;
	/** The loudest 3 s, or `null` for a span too short to have one. */
	shortTerm: string | null;
}

/** The words for one meter's reading. A silent or missing one reads as dashes. */
export function readingText(r: LevelReading | null | undefined): ReadingText {
	return {
		lufs: fmtNumber(r?.integrated_lufs),
		truePeak: fmtNumber(r?.true_peak_dbtp),
		range: r?.loudness_range_lu === null || r?.loudness_range_lu === undefined ? '—' : fmtNumber(r.loudness_range_lu),
		shortTerm: r?.short_term_max_lufs === null || r?.short_term_max_lufs === undefined ? null : fmtNumber(r.short_term_max_lufs)
	};
}

export type Tone = 'ok' | 'warn' | 'info';

/** A one-line verdict on the mix against the streaming target, with the thresholds
 *  the notes use (over by more than 1 LU is loud, under by more than 3 is quiet). */
export function verdict(levels: Pick<Levels, 'master' | 'target_lufs'>): { text: string; tone: Tone } {
	const i = levels.master?.integrated_lufs;
	const target = levels.target_lufs ?? LEVELS_TARGET_LUFS;
	if (!levels.master) return { text: 'No audio in the cut', tone: 'info' };
	if (i === null || i === undefined) return { text: 'The mix is silent', tone: 'info' };
	const over = i - target;
	const head = `${fmtNumber(i)} LUFS`;
	if (over > 1) return { text: `${head} — ${fmtNumber(over)} LU louder than the ${target} target`, tone: 'warn' };
	if (-over > 3) return { text: `${head} — ${fmtNumber(-over)} LU quieter than the ${target} target`, tone: 'warn' };
	return { text: `${head} — on the ${target} target`, tone: 'ok' };
}

/** The error a stopped measurement rejects with (`LEVELS_CANCELLED` in kerf-app). */
export const LEVELS_CANCELLED = 'levels cancelled';

/** Whether a rejected measurement was stopped by the user rather than broken. */
export function isLevelsCancelled(e: unknown): boolean {
	return (e instanceof Error ? e.message : String(e)) === LEVELS_CANCELLED;
}

/** The tone of one of the engine's advice lines. The first line judges loudness and
 *  says "close to" when it is fine; every other line is a problem to fix. */
export function noteTone(note: string): Tone {
	if (/close to the/.test(note)) return 'ok';
	if (/silent|no audio/i.test(note)) return 'info';
	return 'warn';
}

/** The range a measurement covers: the in / out marks when both are set and in order —
 *  the rule the export dialog's range follows, so the number is for the file the
 *  export would write — otherwise the whole cut (`null`). */
export function measureRange(markIn: number | null, markOut: number | null): { start: number; end: number } | null {
	if (markIn !== null && markOut !== null && markOut > markIn) return { start: markIn, end: markOut };
	return null;
}

/** `0:12` / `1:05` for a time in seconds. */
export function clock(seconds: number): string {
	const s = Math.max(0, Math.round(seconds));
	return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** What was measured, in words: "Whole cut · 2:00" or "In → out · 0:10–0:25". */
export function scopeLabel(range: { start: number; end: number } | null, duration: number): string {
	if (range) return `In → out · ${clock(range.start)}–${clock(range.end)}`;
	return `Whole cut · ${clock(duration)}`;
}

/** The project state a measurement was taken against. */
export interface MeasureStamp {
	/** The edit-history revision that was current. */
	seq: number | null;
	/** The project file (null while unsaved). */
	path: string | null;
}

/** Whether a measurement is about something other than what is on screen: any edit
 *  since (a new revision), or another project altogether. A stale reading is kept —
 *  it is still what was measured — and said to be out of date. */
export function isStale(measured: MeasureStamp, now: MeasureStamp): boolean {
	return measured.seq !== now.seq || measured.path !== now.path;
}
