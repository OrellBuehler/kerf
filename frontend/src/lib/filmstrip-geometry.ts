// The geometry of a filmstrip, mirrored from `kerf_core::engine::filmstrip`.
//
// The desktop app answers `get_filmstrip` from a real decode, and the webview
// only ever *reads* the result: `frameAt` / `locate` turn a source time into a
// thumbnail and the thumbnail into a sheet and an x offset. The rest of this file
// — the interval ladder, the thumbnail width, the sheet layout, the plan — is
// what the backend does when it *makes* a strip, and the browser harness
// (`sample-filmstrip.ts`, which has no decoder) has to make one the same way, or
// the timeline would be developed against a strip shaped differently from the
// one it will get. It is a faithful port, not a lookalike: `filmstrip-geometry
// .test.ts` replays the Rust unit tests case for case, so a rule changed in
// kerf-core has to change here or a test names it.
//
// Pure: no DOM, no I/O.

import type { Asset } from './types';

/** Height of every thumbnail, in pixels (`FILMSTRIP_HEIGHT`). */
export const FILMSTRIP_HEIGHT = 96;

/** The most thumbnails one strip holds, whatever the asset's length. */
export const MAX_FILMSTRIP_FRAMES = 300;

/** The widest a single JPEG sheet may be, in pixels. */
export const MAX_SHEET_WIDTH = 8192;

/** The widest a single thumbnail may be: an extreme panorama is squeezed. */
export const MAX_FRAME_WIDTH = 1024;

/** The sampling intervals, in seconds, a strip can have, finest first. */
export const INTERVAL_LADDER = [0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600] as const;

/** Thumbnails a `duration`-second asset has at `interval`: one per started
 *  interval, and always at least one. */
export function framesFor(duration: number, interval: number): number {
	if (!Number.isFinite(duration) || duration <= 0) return 1;
	// The small epsilon keeps an exact multiple (10 s at 0.5 s) from becoming one
	// frame more through float error.
	const n = Math.max(1, Math.ceil(duration / interval - 1e-6));
	return Math.min(n, 0xffff_ffff);
}

/**
 * The sampling interval for a `duration`-second asset: the finest rung of the
 * ladder (0.5 s … 1 h) that keeps the strip within 300 thumbnails. A duration
 * past the ladder's reach gets whole seconds. An unknown or non-positive
 * duration is a one second interval — and one frame.
 */
export function pickInterval(duration: number): number {
	if (!Number.isFinite(duration) || duration <= 0) return 1;
	const rung = INTERVAL_LADDER.find((r) => framesFor(duration, r) <= MAX_FILMSTRIP_FRAMES);
	return rung ?? Math.ceil(duration / MAX_FILMSTRIP_FRAMES);
}

/**
 * Width of a thumbnail for a `width`x`height` picture (the *displayed* size,
 * rotation applied): 96 px's worth of the aspect, rounded to an even number and
 * kept in `2..=1024`. A picture of unknown size is taken as 16:9.
 */
export function thumbWidth(width?: number | null, height?: number | null): number {
	const [w, h] = width != null && height != null && width > 0 && height > 0 ? [width, height] : [16, 9];
	const even = Math.round((FILMSTRIP_HEIGHT * w) / h / 2) * 2;
	return Math.min(Math.max(even, 2), MAX_FRAME_WIDTH);
}

/**
 * `{ columns, sheets }` for `frames` thumbnails `frameWidth` px wide: as few
 * sheets as 8192 px allows, with the frames spread evenly over them so the last
 * is not nearly all padding.
 */
export function sheetLayout(frames: number, frameWidth: number): { columns: number; sheets: number } {
	const total = Math.max(frames, 1);
	const widest = Math.max(Math.floor(MAX_SHEET_WIDTH / Math.max(frameWidth, 1)), 1);
	const sheets = Math.ceil(total / widest);
	const columns = Math.ceil(total / sheets);
	// Recounted from `columns`, so the last sheet always holds at least one.
	return { columns, sheets: Math.ceil(total / columns) };
}

/** What a strip for one asset is made of (`Plan` in Rust). */
export interface FilmstripPlan {
	interval: number;
	/** Thumbnails asked for. */
	frames: number;
	frame_width: number;
	frame_height: number;
	/** A still image: no source timeline, so one thumbnail. */
	still: boolean;
}

/**
 * What a strip for `asset` is made of, from the asset alone. Throws (the
 * backend's `InvalidArgument` text) when the asset has streams and none is
 * video; an asset with no stream info at all is tried, as 16:9.
 */
export function planFilmstrip(asset: Pick<Asset, 'id' | 'duration' | 'streams'>): FilmstripPlan {
	const video = asset.streams.find((s) => s.kind === 'video');
	if (!video && asset.streams.length > 0) {
		throw new Error(`invalid argument: asset ${asset.id} has no video stream`);
	}
	const still = asset.streams.some((s) => s.image);
	// Rust's `f64::max` ignores a NaN; `Math.max` does not.
	const interval = still ? (Number.isNaN(asset.duration) ? 1 : Math.max(asset.duration, 1)) : pickInterval(asset.duration);
	return {
		interval,
		frames: still ? 1 : framesFor(asset.duration, interval),
		frame_width: thumbWidth(video?.width, video?.height),
		frame_height: FILMSTRIP_HEIGHT,
		still
	};
}

/** The layout of one sheet: a `FilmstripSheet` without its pixels. */
export interface SheetGeometry {
	first_frame: number;
	count: number;
	width: number;
	height: number;
}

/** A `Filmstrip` without its pixels. */
export interface StripGeometry {
	interval: number;
	frame_width: number;
	frame_height: number;
	frames: number;
	columns: number;
	sheets: SheetGeometry[];
}

/** The strip a decode of `plan` delivers when every planned thumbnail arrives
 *  (a real video can come up short; the harness's never does). */
export function stripGeometry(plan: FilmstripPlan): StripGeometry {
	const { columns, sheets } = sheetLayout(plan.frames, plan.frame_width);
	return {
		interval: plan.interval,
		frame_width: plan.frame_width,
		frame_height: plan.frame_height,
		frames: plan.frames,
		columns,
		sheets: Array.from({ length: sheets }, (_, i) => ({
			first_frame: i * columns,
			count: Math.min(columns, plan.frames - i * columns),
			width: columns * plan.frame_width,
			height: plan.frame_height
		}))
	};
}

/** The source time thumbnail `frame` shows (`Filmstrip::time_of`). */
export function timeOf(strip: Pick<StripGeometry, 'interval'>, frame: number): number {
	return frame * strip.interval;
}

/**
 * The thumbnail to draw for source time `t`: the sample nearest in time, ties to
 * the later one, clamped into the strip (a time before the start, or a NaN, is
 * the first thumbnail; one past the end is the last). `Filmstrip::frame_at`.
 */
export function frameAt(strip: Pick<StripGeometry, 'interval' | 'frames'>, t: number): number {
	const last = Math.max(strip.frames - 1, 0);
	const k = Math.round(t / strip.interval);
	return Number.isNaN(k) || k <= 0 ? 0 : Math.min(k, last);
}

/**
 * The sheet holding thumbnail `frame` and the thumbnail's x offset on it, or
 * `null` past the end of the strip (or for a frame that is not a whole,
 * non-negative index). `Filmstrip::locate`.
 */
export function locate<S extends SheetGeometry>(
	strip: { frame_width: number; sheets: S[] },
	frame: number
): { sheet: S; x: number } | null {
	if (!Number.isInteger(frame) || frame < 0) return null;
	const sheet = strip.sheets.find((s) => frame >= s.first_frame && frame - s.first_frame < s.count);
	return sheet ? { sheet, x: (frame - sheet.first_frame) * strip.frame_width } : null;
}
