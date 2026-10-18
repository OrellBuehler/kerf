// The editing defaults (Settings › Editing): what a fresh session starts with and how long the things Kerf
// places for you are. Stored as one opaque object in `Settings.editing` — Rust keeps it as JSON and this
// module is the only place that knows its shape, so a bad or hand-edited value falls back field by field.

/** What the editing section sets. */
export interface EditingPrefs {
	/** Snapping is on when Kerf starts (the timeline's toggle changes it for the session only). */
	snap: boolean;
	/** How long a still image lasts when it is placed on the timeline, seconds. */
	still_seconds: number;
	/** How long a new title lasts, seconds. */
	title_seconds: number;
}

/** `DEFAULT_IMAGE_DURATION` in kerf-core: a still is five seconds long until said otherwise. */
export const DEFAULT_STILL_SECONDS = 5;
export const DEFAULT_TITLE_SECONDS = 3;

export const DEFAULT_EDITING: EditingPrefs = {
	snap: true,
	still_seconds: DEFAULT_STILL_SECONDS,
	title_seconds: DEFAULT_TITLE_SECONDS
};

/** The range the length fields take. */
export const LENGTH_RANGE = { min: 0.5, max: 60, step: 0.5 } as const;

/** A length held to the range and to half a second; `fallback` for anything that is not a number. */
export function clampSeconds(value: unknown, fallback: number): number {
	const n = typeof value === 'number' ? value : typeof value === 'string' && value.trim() !== '' ? Number(value) : NaN;
	if (!Number.isFinite(n)) return fallback;
	const stepped = Math.round(n / LENGTH_RANGE.step) * LENGTH_RANGE.step;
	return Math.min(LENGTH_RANGE.max, Math.max(LENGTH_RANGE.min, stepped));
}

/** What was stored, read field by field: a value that does not fit leaves that field at its default. */
export function parseEditing(raw: unknown): EditingPrefs {
	const o = raw && typeof raw === 'object' && !Array.isArray(raw) ? (raw as Record<string, unknown>) : {};
	return {
		snap: typeof o.snap === 'boolean' ? o.snap : DEFAULT_EDITING.snap,
		still_seconds: clampSeconds(o.still_seconds, DEFAULT_EDITING.still_seconds),
		title_seconds: clampSeconds(o.title_seconds, DEFAULT_EDITING.title_seconds)
	};
}

/** How a length reads in the field and the hint: `5 s`, `2.5 s`. */
export function secondsLabel(s: number): string {
	return `${Number.isInteger(s) ? s : s.toFixed(1)} s`;
}

/** The end of the source window a new clip of an asset is placed with: a still takes the length the
 *  preference says (it loops, so any length is footage), anything else all of its own. */
export function placementOut(asset: { duration: number; streams: readonly { image?: boolean | null }[] }, stillSeconds: number): number {
	return asset.streams.some((s) => s.image) ? stillSeconds : asset.duration;
}
