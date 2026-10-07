/* Track heights — pure. A track is one of three named heights, not a number of
 * pixels: the names are what the toolbar and the track header offer, and every
 * other size on the timeline (a stereo waveform's two lanes, whether a clip shows
 * thumbnails, how much the titles lane carries) is a function of the pixels these
 * resolve to — none of it knows about presets.
 *
 *  - **compact** (32 px): an overview. A clip box is 22 px — a label, one folded
 *    waveform lane, no thumbnails, none of the clip's grab handles (they need
 *    28 px) — and the header keeps its buttons but drops the mixer strip.
 *  - **medium** (64 px): what the timeline always was; stereo waveform lanes, a
 *    filmstrip 54 px tall, the mixer strip.
 *  - **large** (112 px): a clip box of 102 px — two readable waveform lanes and
 *    thumbnails at close to their stored 96 px.
 *
 * It is a viewer's choice, not part of the cut: nothing about it belongs in a
 * `.kerf` file, so it is remembered per track id in `localStorage` (a track keeps
 * its height across launches, and a project's tracks keep theirs when reopened).
 * `all` is the global choice — what the "all tracks" control last set, what a
 * track with no choice of its own is, and what the titles lane follows (it is not
 * a track). Setting a track to the global value drops its override, so storage
 * holds only the exceptions, and the table is capped so a long-lived install does
 * not accumulate every track of every project it ever opened. */

export type HeightPreset = 'compact' | 'medium' | 'large';

/** Smallest to largest. */
export const HEIGHT_PRESETS: readonly HeightPreset[] = ['compact', 'medium', 'large'];

export const DEFAULT_PRESET: HeightPreset = 'medium';

/** Lane height of each preset, css px (borders included). */
export const PRESET_PX: Readonly<Record<HeightPreset, number>> = { compact: 32, medium: 64, large: 112 };

export const PRESET_LABEL: Readonly<Record<HeightPreset, string>> = {
	compact: 'Compact',
	medium: 'Medium',
	large: 'Large'
};

/** The mixer strip (level + pan under the buttons) needs this much header, px;
 *  below it the strip is left out rather than clipped in half. */
export const MIXER_MIN_PX = 56;

/** How the titles lane is laid out at a preset: one row's height, the padding
 *  above and below the rows, and the text size inside a title. */
export interface TitleMetrics {
	row: number;
	pad: number;
	font: number;
}

export const TITLE_METRICS: Readonly<Record<HeightPreset, TitleMetrics>> = {
	compact: { row: 18, pad: 3, font: 10 },
	medium: { row: 22, pad: 4, font: 10 },
	large: { row: 30, pad: 6, font: 12 }
};

/** Titles lane height for `rows` stacked rows of titles at a preset. */
export const titleLaneHeight = (preset: HeightPreset, rows: number): number => {
	const m = TITLE_METRICS[preset];
	return Math.max(1, rows) * m.row + m.pad * 2;
};

export const isPreset = (v: unknown): v is HeightPreset => v === 'compact' || v === 'medium' || v === 'large';

export const presetPx = (p: HeightPreset): number => PRESET_PX[p];

export interface TrackHeights {
	/** The global choice. */
	all: HeightPreset;
	/** Track id -> a height that differs from `all`. */
	tracks: Record<string, HeightPreset>;
}

export const DEFAULT_HEIGHTS: Readonly<TrackHeights> = { all: DEFAULT_PRESET, tracks: {} };

/** Most per-track overrides remembered; the oldest choice goes first. */
export const MAX_REMEMBERED = 256;

/** The preset of one track. */
export const heightOf = (h: TrackHeights, trackId: string): HeightPreset => h.tracks[trackId] ?? h.all;

/** The lane height of one track, px. */
export const heightPx = (h: TrackHeights, trackId: string): number => PRESET_PX[heightOf(h, trackId)];

/** `h` with one track set to `preset`. Setting it to the global choice removes the
 *  override; a changed one moves to the newest end of the table. */
export function setTrack(h: TrackHeights, trackId: string, preset: HeightPreset): TrackHeights {
	const rest = Object.fromEntries(Object.entries(h.tracks).filter(([id]) => id !== trackId));
	if (preset === h.all) return { all: h.all, tracks: rest };
	const entries = [...Object.entries(rest), [trackId, preset] as const];
	return { all: h.all, tracks: Object.fromEntries(entries.slice(-MAX_REMEMBERED)) };
}

/** `h` with every track (and the titles lane) set to `preset`: the global choice
 *  moves and every exception goes. */
export const setAll = (_h: TrackHeights, preset: HeightPreset): TrackHeights => ({ all: preset, tracks: {} });

/** The one preset every track in `trackIds` is at, or `null` when they differ (or
 *  there are none) — what the "all tracks" control shows as lit. */
export function uniformPreset(h: TrackHeights, trackIds: readonly string[]): HeightPreset | null {
	if (trackIds.length === 0) return null;
	const first = heightOf(h, trackIds[0]);
	return trackIds.every((id) => heightOf(h, id) === first) ? first : null;
}

/** The next height up (`dir` 1) or down (-1), stopping at the ends. */
export function stepPreset(p: HeightPreset, dir: 1 | -1): HeightPreset {
	const i = HEIGHT_PRESETS.indexOf(p) + dir;
	return HEIGHT_PRESETS[Math.min(HEIGHT_PRESETS.length - 1, Math.max(0, i))];
}

// ---- storage ------------------------------------------------------------------

export const HEIGHTS_KEY = 'kerf.timeline.heights';

/** A stored value, or the defaults for anything missing or malformed — a bad
 *  entry is dropped on its own rather than costing the rest. */
export function parseHeights(raw: string | null | undefined): TrackHeights {
	if (!raw) return { all: DEFAULT_PRESET, tracks: {} };
	let v: unknown;
	try {
		v = JSON.parse(raw);
	} catch {
		return { all: DEFAULT_PRESET, tracks: {} };
	}
	if (typeof v !== 'object' || v === null || Array.isArray(v)) return { all: DEFAULT_PRESET, tracks: {} };
	const o = v as { all?: unknown; tracks?: unknown };
	const all = isPreset(o.all) ? o.all : DEFAULT_PRESET;
	const kept: [string, HeightPreset][] = [];
	if (typeof o.tracks === 'object' && o.tracks !== null && !Array.isArray(o.tracks)) {
		for (const [id, p] of Object.entries(o.tracks)) {
			// An override equal to the global choice is not an exception.
			if (id && isPreset(p) && p !== all) kept.push([id, p]);
		}
	}
	return { all, tracks: Object.fromEntries(kept.slice(-MAX_REMEMBERED)) };
}

export const serializeHeights = (h: TrackHeights): string => JSON.stringify({ all: h.all, tracks: h.tracks });

/** The remembered heights; a blocked store just means the defaults. */
export function loadHeights(): TrackHeights {
	try {
		return parseHeights(localStorage.getItem(HEIGHTS_KEY));
	} catch {
		return parseHeights(null);
	}
}

export function saveHeights(h: TrackHeights): void {
	try {
		localStorage.setItem(HEIGHTS_KEY, serializeHeights(h));
	} catch {
		/* a convenience, not state — ignore a blocked store */
	}
}

/** Whether the minimap strip is shown (on by default). */
export const MINIMAP_KEY = 'kerf.timeline.minimap';

export function loadMinimap(): boolean {
	try {
		return localStorage.getItem(MINIMAP_KEY) !== '0';
	} catch {
		return true;
	}
}

export function saveMinimap(on: boolean): void {
	try {
		localStorage.setItem(MINIMAP_KEY, on ? '1' : '0');
	} catch {
		/* ignore a blocked store */
	}
}
