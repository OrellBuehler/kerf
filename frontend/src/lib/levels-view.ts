/* How a loudness measurement (`Levels`) is worded on the Mixer: the numbers each
 * strip reads, the one-line verdict beside them, what range was measured, and
 * whether the measurement is still about the cut that is on screen. Pure — the
 * panel only lays these out. */

import { clampCeiling, LEVELS_CEILING_MARGIN_DB, LEVELS_TARGET_LUFS, LEVELS_TRUE_PEAK_CEILING_DBTP, MASTER_DEFAULT_CEILING_DB, MASTER_MIN_CEILING_DB } from './levels';
import { LOUDNESS_TARGETS, loudnessChoice } from './export-presets';
import type { ExportOptions, LevelReading, Levels, MasterBus } from './types';

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

/** The loudness a delivery is held to, and which one that is: the streaming target the engine measures by (`target_lufs`), or the one the user chose
 *  in the Export dialog — read from the options the dialog was last closed on (`ui.exportDraft`), so no backend is asked. Every target asks -1 dBTP. */
export interface DeliveryTarget {
	lufs: number;
	/** What to call it beside the number: `the streaming target`, `the Apple target set in Export`. */
	name: string;
	/** Chosen in Export, not the default streaming target. */
	chosen: boolean;
}

export function deliveryTarget(opts: Pick<ExportOptions, 'loudness' | 'loudnorm'> | null | undefined, engineLufs: number = LEVELS_TARGET_LUFS): DeliveryTarget {
	const choice = opts ? loudnessChoice(opts) : 'off';
	const picked = LOUDNESS_TARGETS.find((t) => t.id === choice);
	if (!picked || picked.lufs === null) return { lufs: engineLufs, name: 'the streaming target', chosen: false };
	return { lufs: picked.lufs, name: `the ${picked.label.split(' · ')[0]} target set in Export`, chosen: true };
}

/** One thing a delivery is judged on, read against what platforms ask for: the strip's "target" rows. */
export interface DeliveryRow {
	id: 'loudness' | 'true_peak';
	label: string;
	/** What was measured, with its unit: `-16.4 LUFS`. */
	value: string;
	/** The line it is held to: `target -14 LUFS`, `ceiling -1 dBTP`. */
	target: string;
	/** How far it is from that, in words: `2.4 LU under`, `0.6 dB over`, `on target`. */
	delta: string;
	tone: Tone;
	/** What to do about it, naming the control, or `null` when nothing is needed. */
	advice: string | null;
}

/** The cut's measured loudness and true peak against what a delivery is held to — the streaming target the engine judges
 *  by (`target_lufs`) and the -1 dBTP ceiling platforms ask for — and what to do when it is over. The thresholds are the notes'
 *  (`levelNotes`: more than 1 LU over, more than 3 under, any true peak over the line), and so is the ceiling advice. `null`
 *  when there is nothing to judge (no audio, a silent mix). */
export function deliveryCheck(
	levels: Pick<Levels, 'master' | 'target_lufs'>,
	bus: Pick<MasterBus, 'limiter' | 'ceiling_db' | 'true_peak'>,
	chosen: DeliveryTarget = deliveryTarget(null, levels.target_lufs)
): DeliveryRow[] | null {
	const m = levels.master;
	if (!m || m.integrated_lufs === null || m.integrated_lufs === undefined) return null;
	const target = chosen.lufs;
	const rows: DeliveryRow[] = [];

	const over = m.integrated_lufs - target;
	const loud: DeliveryRow = {
		id: 'loudness',
		label: 'Loudness',
		value: `${fmtNumber(m.integrated_lufs)} LUFS`,
		target: `${chosen.chosen ? 'target' : 'streaming target'} ${target} LUFS`,
		delta: 'on target',
		tone: 'ok',
		advice: null
	};
	// With a target chosen in Export the file is evened out to it either way: the advice is about hearing it at that level here.
	const how = chosen.chosen ? `Export evens the file out to ${target} LUFS; to hear it there` : '';
	if (over > 1) {
		Object.assign(loud, {
			delta: `${fmtNumber(over)} LU over`,
			tone: 'warn',
			advice: chosen.chosen ? `${how}, lower the master by ${fmtNumber(over)} dB.` : `Lower the master by ${fmtNumber(over)} dB, or export with a loudness target.`
		});
	} else if (-over > 3) {
		Object.assign(loud, {
			delta: `${fmtNumber(-over)} LU under`,
			tone: 'warn',
			advice: chosen.chosen ? `${how}, raise the master by ${fmtNumber(-over)} dB.` : `Raise the master by ${fmtNumber(-over)} dB, or export with a loudness target.`
		});
	}
	rows.push(loud);

	const tp = m.true_peak_dbtp;
	if (tp !== null && tp !== undefined && Number.isFinite(tp)) {
		const row: DeliveryRow = {
			id: 'true_peak',
			label: 'True peak',
			value: `${fmtNumber(tp)} dBTP`,
			target: `ceiling ${LEVELS_TRUE_PEAK_CEILING_DBTP} dBTP`,
			delta: 'under the ceiling',
			tone: 'ok',
			advice: null
		};
		const excess = tp - LEVELS_TRUE_PEAK_CEILING_DBTP;
		if (tp > LEVELS_TRUE_PEAK_CEILING_DBTP) {
			row.tone = 'warn';
			row.delta = `${fmtNumber(excess)} dB over`;
			const ceiling = Number.isFinite(bus.ceiling_db) ? clampCeiling(bus.ceiling_db) : MASTER_DEFAULT_CEILING_DB;
			const lower = Math.max(ceiling - excess - LEVELS_CEILING_MARGIN_DB, MASTER_MIN_CEILING_DB);
			const canLower = lower < ceiling;
			row.advice = !bus.limiter
				? 'Turn on the Limiter with True peak (FX), or lower the master.'
				: bus.true_peak
					? canLower
						? `Lower the limiter's ceiling to about ${fmtNumber(lower)} dBFS (FX).`
						: 'The limiter is at its lowest ceiling: lower the master or the loudest track.'
					: canLower
						? `Turn on True peak (FX), or lower the ceiling to about ${fmtNumber(lower)} dBFS.`
						: 'Turn on True peak (FX), or lower the master or the loudest track.';
		}
		rows.push(row);
	}
	return rows;
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

/** What was measured, in words: "Whole cut · 2:00" or "In → out · 0:10–0:25". `fmt` is how the interface prints a
 *  time (`timeDisplay.fmt`) — the in / out marks are positions on the same timeline as the playhead; the default
 *  is this file's `m:ss`. */
export function scopeLabel(
	range: { start: number; end: number } | null,
	duration: number,
	fmt: (seconds: number) => string = clock
): string {
	if (range) return `In → out · ${fmt(range.start)}–${fmt(range.end)}`;
	return `Whole cut · ${fmt(duration)}`;
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
