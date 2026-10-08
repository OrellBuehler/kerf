/* The track mixer's arithmetic, shared by the timeline's mixer strip and the
 * Web Audio preview.
 *
 * `panGains` is the TS mirror of `Track::pan_gains` in
 * crates/kerf-core/src/model.rs — faithful, not approximate, because preview
 * playback is meant to be what the export sounds like. It is a **balance**: the
 * side you turn towards stays at unity and the other is attenuated away, so
 * leaning a track never makes it louder. */

/** Left / right gains for a pan position, -1 (hard left) to 1 (hard right). */
export function panGains(pan: number): [number, number] {
	const p = Math.min(1, Math.max(-1, pan || 0));
	return p < 0 ? [1, 1 + p] : [1 - p, 1];
}

/** The loudest a level control (a clip's volume slider and line, a track fader)
 *  offers: +6 dB. One number, so the Inspector's slider, the timeline's fader and
 *  the volume line on the clip all end at the same place. A clip set above it by
 *  an agent keeps its value — controls clamp what they *show*, not what is stored. */
export const MAX_GAIN = 2;

/** A linear gain as dB; silence is `-Infinity`. */
export const gainToDb = (v: number): number => (v > 0 ? 20 * Math.log10(v) : -Infinity);

/** dB as a linear gain (`-Infinity` is silence). */
export const dbToGain = (db: number): number => (db === -Infinity ? 0 : 10 ** (db / 20));

/** What a clip's drawn (and heard) level is: its own gain through the track's
 *  fader, multiplied — the fader rides each clip *after* the clip's gain, as the
 *  export's graph does, so the two are one linear factor. Both default to unity. */
export const effectiveGain = (clipVolume: number | undefined, trackVolume: number | undefined): number =>
	Math.max(0, clipVolume ?? 1) * Math.max(0, trackVolume ?? 1);

/** A track fader as dB — the unit a level is actually judged in. */
export function gainLabel(v: number): string {
	if (v <= 0.0001) return '−∞ dB';
	const db = 20 * Math.log10(v);
	return `${db > 0 ? '+' : ''}${db.toFixed(1)} dB`;
}

/** A pan position as the L/R reading a mixer shows. */
export function panLabel(p: number): string {
	if (Math.abs(p) < 0.005) return 'centre';
	return `${p < 0 ? 'L' : 'R'}${Math.round(Math.abs(p) * 100)}`;
}

/** True when a track's mix is untouched, and so contributes nothing to render. */
export function isUnityMix(volume: number | undefined, pan: number | undefined): boolean {
	return (volume ?? 1) === 1 && (pan ?? 0) === 0;
}

// ---- the fader's travel ------------------------------------------------------
//
// A level is judged in dB, so a fader's travel is a taper over dB rather than over
// the linear gain a slider would hand back: with `0..2` linear, the bottom half of
// the travel is everything above +0 dB and the whole useful range below unity
// (-40 .. 0) is squeezed into the other half. Both faders — the timeline header's
// and the Mixer's — read and write through this one mapping, so the same level is
// the same place on either of them, and so a meter's scale can line up with the
// fader beside it.
//
// The taper is piecewise linear in dB, the way a console's printed scale is:
// finer around unity, coarser as it goes quiet, a stop at the bottom that is
// silence. Above unity it is linear to the fader's top.

/** Where unity (0 dB) sits on the travel, 0 (bottom) to 1 (top). */
export const FADER_UNITY_POS = 0.75;

/** The quietest marked level; the last stop below it is silence (`-Infinity`). */
export const FADER_FLOOR_DB = -60;

/** The bottom of the travel up to here is silence. The `-60 dB` mark sits on it. */
export const FADER_FLOOR_POS = 0.03;

/** The top of a track's fader in dB: `MAX_GAIN` (+6 dB). */
export const FADER_MAX_DB = gainToDb(MAX_GAIN);

/** The taper's marks — `[position, dB]` — below unity, ascending in both. */
export const FADER_MARKS: readonly (readonly [number, number])[] = [
	[FADER_FLOOR_POS, FADER_FLOOR_DB],
	[0.08, -50],
	[0.15, -40],
	[0.25, -30],
	[0.38, -20],
	[0.55, -10],
	[FADER_UNITY_POS, 0]
];

/** A dB level's place on the travel, `0..1`. Silence is 0; anything under the
 *  floor mark rests on it (the fader cannot draw "−75"); above `maxDb` it is 1. */
export function dbToFader(db: number, maxDb: number = FADER_MAX_DB): number {
	if (Number.isNaN(db) || db === -Infinity) return 0;
	if (db >= 0) return Math.min(1, FADER_UNITY_POS + ((1 - FADER_UNITY_POS) * db) / Math.max(maxDb, 1e-9));
	if (db <= FADER_FLOOR_DB) return FADER_FLOOR_POS;
	for (let i = FADER_MARKS.length - 1; i > 0; i--) {
		const [p0, d0] = FADER_MARKS[i - 1];
		const [p1, d1] = FADER_MARKS[i];
		if (db >= d0) return p0 + ((db - d0) / (d1 - d0)) * (p1 - p0);
	}
	return FADER_FLOOR_POS;
}

/** A place on the travel as dB: the inverse of `dbToFader`, `-Infinity` on the
 *  bottom stop. Not rounded — `snapDb` is what a gesture writes. */
export function faderToDb(pos: number, maxDb: number = FADER_MAX_DB): number {
	const p = Math.min(1, Math.max(0, Number.isNaN(pos) ? 0 : pos));
	if (p < FADER_FLOOR_POS) return -Infinity;
	if (p >= FADER_UNITY_POS) return ((p - FADER_UNITY_POS) / (1 - FADER_UNITY_POS)) * maxDb;
	for (let i = 1; i < FADER_MARKS.length; i++) {
		const [p0, d0] = FADER_MARKS[i - 1];
		const [p1, d1] = FADER_MARKS[i];
		if (p <= p1) return d0 + ((p - p0) / (p1 - p0)) * (d1 - d0);
	}
	return 0;
}

/** How close to 0 dB a drag lands on exactly unity: an untouched mix is omitted
 *  from the render graph, so finding it by hand should not take a steady hand. */
export const UNITY_SNAP_DB = 0.25;

/** What a gesture writes: a level to a tenth of a dB (what the label says), unity
 *  caught within `UNITY_SNAP_DB`, silence kept as `-Infinity`. */
export function snapDb(db: number): number {
	if (db === -Infinity || Number.isNaN(db)) return -Infinity;
	if (Math.abs(db) <= UNITY_SNAP_DB) return 0;
	return Math.round(db * 10) / 10;
}

/** A fader's linear top as dB (for a fader that is not a track's). */
const maxDbOf = (maxGain: number) => gainToDb(Math.max(maxGain, 1));

/** A gain's place on the travel of a fader whose top is `maxGain`. */
export const gainToFader = (gain: number, maxGain: number = MAX_GAIN): number =>
	dbToFader(gainToDb(gain), maxDbOf(maxGain));

/** The gain a place on the travel means: snapped (`snapDb`), and exactly `maxGain`
 *  at the top stop (not the 6.0 dB the rounding would give — the Inspector's slider
 *  and the clip's volume line end at `MAX_GAIN`, so the fader does too). */
export function faderToGain(pos: number, maxGain: number = MAX_GAIN): number {
	const top = maxDbOf(maxGain);
	const db = faderToDb(pos, top);
	if (db >= top - 0.05) return maxGain;
	return dbToGain(snapDb(db));
}

/** How far one keypress moves a fader, in dB. */
export const FADER_STEP_DB = { fine: 0.1, normal: 1, coarse: 3, page: 6 } as const;
export type FaderStep = keyof typeof FADER_STEP_DB;

/**
 * A fader nudged by `steps` marks of `size` dB (negative is down). A nudge lands on
 * the dB grid of its own size — from −6.02 dB, a 1 dB step up is −5, not −5.02 — so
 * the keys give round numbers and cross unity exactly; it leaves silence for the
 * floor mark and returns to silence under it; and it stops at `maxGain`.
 */
export function nudgeGain(gain: number, steps: number, size: FaderStep = 'normal', maxGain: number = MAX_GAIN): number {
	const grid = FADER_STEP_DB[size];
	const top = maxDbOf(maxGain);
	// Silence — or anything under the floor mark, which the fader cannot draw — is "one
	// step under the floor": the first step up lands on the floor mark.
	const level = gain > 0 ? gainToDb(gain) : -Infinity;
	const from = level < FADER_FLOOR_DB ? FADER_FLOOR_DB - grid : level;
	const eps = 1e-9;
	// Already at (or past) the top: up is nowhere, and must not pull a stored value down to it.
	if (steps > 0 && from >= top - eps) return gain;
	const cell = steps >= 0 ? Math.floor(from / grid + eps) : Math.ceil(from / grid - eps);
	const next = (cell + steps) * grid;
	// Off the bottom of the scale is silence; the grid value is exact enough to compare.
	if (next < FADER_FLOOR_DB - eps) return 0;
	if (next >= top) return maxGain;
	return dbToGain(Math.round(next * 1000) / 1000);
}

/** A fader's marks as places on its travel, for the scale beside it: the dB values
 *  a mixer prints, up to the fader's top. */
export function faderTicks(maxDb: number = FADER_MAX_DB): { db: number; pos: number }[] {
	const dbs = [12, 6, 3, 0, -6, -12, -20, -30, -40, -50, -60].filter((d) => d <= maxDb + 0.05);
	return dbs.map((db) => ({ db, pos: dbToFader(db, maxDb) }));
}

// ---- the pan's travel --------------------------------------------------------

/** How far one keypress moves the pan, as a fraction of the half-width. */
export const PAN_STEP = { fine: 0.01, normal: 0.05, coarse: 0.25, page: 0.25 } as const;

/** A pan position (-1 left … 1 right) as a place on a slider's travel, `0..1`. */
export const panToPos = (pan: number): number => (Math.min(1, Math.max(-1, pan || 0)) + 1) / 2;

/** The pan a place on a slider's travel means: to a percent, centre caught (a
 *  pan of exactly 0 is the one the render graph leaves out). */
export function posToPan(pos: number): number {
	const p = Math.min(1, Math.max(0, Number.isNaN(pos) ? 0.5 : pos)) * 2 - 1;
	const snapped = Math.round(p * 100) / 100;
	return Math.abs(snapped) <= 0.03 ? 0 : snapped;
}

/** The pan nudged by `steps` marks of `size`, on that size's own grid, within -1..1. */
export function nudgePan(pan: number, steps: number, size: keyof typeof PAN_STEP = 'normal'): number {
	const grid = PAN_STEP[size];
	const eps = 1e-9;
	const from = Math.min(1, Math.max(-1, pan || 0));
	const cell = steps >= 0 ? Math.floor(from / grid + eps) : Math.ceil(from / grid - eps);
	const next = Math.min(1, Math.max(-1, Math.round((cell + steps) * grid * 1000) / 1000));
	return next === 0 ? 0 : next;
}

/** What a pan does to each side, in dB — what the balance law leaves on the left
 *  and the right ("L 0.0 dB · R -3.1 dB"); `-Infinity` for a side turned away. */
export function panSides(pan: number): { left: number; right: number } {
	const [l, r] = panGains(pan);
	return { left: gainToDb(l), right: gainToDb(r) };
}

/** `-3.1 dB` / `−∞ dB` for one side of a pan, the way `gainLabel` writes a level. */
export const sideLabel = (db: number): string => (db === -Infinity ? '−∞ dB' : `${db > 0 ? '+' : ''}${(Math.abs(db) < 0.05 ? 0 : db).toFixed(1)} dB`);

/** The order a scale's marks earn a label in when the fader is too short for all of
 *  them: unity first, then the marks that read the level fastest. */
const LABEL_PRIORITY = [0, -12, 6, -30, -6, -20, 12, 3, -40, -50, -60];

/** Which of `ticks` have room for a label on a fader whose travel is `travelPx` long:
 *  marks are kept in priority order (unity always, since it is the one every
 *  other is read against) unless an already-kept label is within `minGapPx`. Returned
 *  in the order given. */
export function labelTicks<T extends { db: number; pos: number }>(ticks: T[], travelPx: number, minGapPx = 12): T[] {
	const kept: T[] = [];
	const byPriority = [...ticks].sort((a, b) => {
		const ra = LABEL_PRIORITY.indexOf(a.db);
		const rb = LABEL_PRIORITY.indexOf(b.db);
		return (ra < 0 ? 99 : ra) - (rb < 0 ? 99 : rb);
	});
	for (const t of byPriority) {
		if (kept.every((k) => Math.abs(k.pos - t.pos) * travelPx >= minGapPx)) kept.push(t);
	}
	return ticks.filter((t) => kept.includes(t));
}
