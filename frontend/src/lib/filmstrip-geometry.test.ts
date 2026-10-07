import { describe, expect, test } from 'bun:test';
import {
	FILMSTRIP_HEIGHT,
	INTERVAL_LADDER,
	MAX_FILMSTRIP_FRAMES,
	MAX_FRAME_WIDTH,
	MAX_SHEET_WIDTH,
	frameAt,
	framesFor,
	locate,
	pickInterval,
	planFilmstrip,
	sheetLayout,
	stripGeometry,
	thumbWidth,
	timeOf
} from './filmstrip-geometry';
import type { FilmstripPlan } from './filmstrip-geometry';
import type { StreamInfo } from './types';

// Case for case, the unit tests of `kerf_core::engine::filmstrip`: a rule changed
// in the engine has to change in the mirror, or one of these names it.

const video = (width?: number, height?: number, image = false): StreamInfo => ({
	index: 0,
	kind: 'video',
	codec: image ? 'png' : 'h264',
	width,
	height,
	fps: 25,
	image
});
const audio: StreamInfo = { index: 1, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 };
const asset = (duration: number, streams: StreamInfo[]) => ({ id: 'a1', duration, streams });

/** `plan_for` of the Rust tests: 170 px thumbnails. */
const planFor = (interval: number, frames: number, still = false): FilmstripPlan => ({
	interval,
	frames,
	frame_width: 170,
	frame_height: 96,
	still
});

describe('interval and geometry', () => {
	test('the interval is the finest rung that keeps the strip short', () => {
		// Short clips get the finest sampling.
		expect(pickInterval(10)).toBe(0.5);
		expect(pickInterval(150)).toBe(0.5); // exactly 300 frames at 0.5 s is allowed
		// One frame past the cap steps to the next rung.
		expect(pickInterval(150.5)).toBe(1);
		expect(pickInterval(300)).toBe(1);
		expect(pickInterval(600)).toBe(2);
		expect(pickInterval(3600)).toBe(15);
		expect(pickInterval(7200)).toBe(30);
		expect(pickInterval(86_400)).toBe(300);
		// Degenerate durations are one second — and one frame.
		for (const d of [0, -3, NaN, Infinity]) {
			expect(pickInterval(d)).toBe(1);
			expect(framesFor(d, 1)).toBe(1);
		}
	});

	test('the ladder is the documented one', () => {
		expect([...INTERVAL_LADDER]).toEqual([0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600]);
		expect([FILMSTRIP_HEIGHT, MAX_FILMSTRIP_FRAMES, MAX_SHEET_WIDTH, MAX_FRAME_WIDTH]).toEqual([96, 300, 8192, 1024]);
	});

	test('the strip never exceeds the frame cap', () => {
		// Swept across every scale from a blink to a year-long recording.
		for (let d = 0.01; d < 3.2e7; d *= 1.37) {
			const interval = pickInterval(d);
			const frames = framesFor(d, interval);
			expect(frames).toBeGreaterThanOrEqual(1);
			expect(frames).toBeLessThanOrEqual(MAX_FILMSTRIP_FRAMES);
			// ...and it is the *finest* such rung: the next one down would not fit.
			const finer = [...INTERVAL_LADDER].reverse().find((r) => r < interval);
			if (finer !== undefined) expect(framesFor(d, finer)).toBeGreaterThan(MAX_FILMSTRIP_FRAMES);
		}
		// Past the ladder's top rung, whole seconds still hold the cap.
		const d = 3600 * MAX_FILMSTRIP_FRAMES * 7.5;
		expect(framesFor(d, pickInterval(d))).toBeLessThanOrEqual(MAX_FILMSTRIP_FRAMES);
	});

	test('an exact multiple of the interval is not one frame too many', () => {
		expect(framesFor(10, 0.5)).toBe(20);
		expect(framesFor(10, 2)).toBe(5);
		expect(framesFor(10.1, 2)).toBe(6);
		expect(framesFor(0.04, 0.5)).toBe(1);
		// Float noise on the boundary must not add a frame.
		expect(framesFor(10.00000001, 0.5)).toBe(20);
	});

	test('the thumbnail follows the displayed aspect in even pixels', () => {
		expect(thumbWidth(1920, 1080)).toBe(170);
		expect(thumbWidth(1280, 720)).toBe(170);
		// A portrait phone clip (displayed size, rotation already applied).
		expect(thumbWidth(1080, 1920)).toBe(54);
		expect(thumbWidth(1080, 1080)).toBe(96);
		// A 360 equirect frame.
		expect(thumbWidth(5760, 2880)).toBe(192);
		// Unknown picture: 16:9.
		expect(thumbWidth()).toBe(170);
		expect(thumbWidth(null, null)).toBe(170);
		expect(thumbWidth(0, 0)).toBe(170);
		// Extremes are squeezed into range rather than refused.
		expect(thumbWidth(100_000, 10)).toBe(MAX_FRAME_WIDTH);
		expect(thumbWidth(1, 100_000)).toBe(2);
		for (const [w, h] of [
			[1920, 1080],
			[1000, 999],
			[7, 5],
			[4096, 1716],
			[720, 1280],
			[1, 1]
		]) {
			expect(thumbWidth(w, h) % 2).toBe(0);
		}
	});
});

describe('sheet layout', () => {
	test('sheets are as few and as even as the width cap allows', () => {
		// 170 px thumbnails: 48 fit in 8192.
		expect(sheetLayout(20, 170)).toEqual({ columns: 20, sheets: 1 });
		expect(sheetLayout(48, 170)).toEqual({ columns: 48, sheets: 1 });
		// 49 would leave one lonely thumbnail on a second sheet; it is split evenly.
		expect(sheetLayout(49, 170)).toEqual({ columns: 25, sheets: 2 });
		expect(sheetLayout(300, 170)).toEqual({ columns: 43, sheets: 7 });
		expect(sheetLayout(1, 170)).toEqual({ columns: 1, sheets: 1 });
		// An empty strip is clamped, never zero columns.
		expect(sheetLayout(0, 170)).toEqual({ columns: 1, sheets: 1 });
		// Portrait: 54 px, so 151 per sheet.
		expect(sheetLayout(300, 54)).toEqual({ columns: 150, sheets: 2 });
	});

	test('layouts always cover the frames within the width cap', () => {
		for (const width of [2, 54, 96, 170, 192, 700, 1024, 4000, 8192]) {
			for (let frames = 1; frames <= MAX_FILMSTRIP_FRAMES; frames++) {
				const { columns, sheets } = sheetLayout(frames, width);
				expect(columns).toBeGreaterThanOrEqual(1);
				expect(sheets).toBeGreaterThanOrEqual(1);
				expect(columns * width).toBeLessThanOrEqual(Math.max(MAX_SHEET_WIDTH, width));
				// Every sheet holds something, and together they hold exactly `frames`.
				const last = frames - (sheets - 1) * columns;
				expect(last).toBeGreaterThanOrEqual(1);
				expect(last).toBeLessThanOrEqual(columns);
				// As few sheets as the cap allows.
				const widest = Math.max(Math.floor(MAX_SHEET_WIDTH / width), 1);
				expect(sheets).toBe(Math.ceil(frames / widest));
			}
		}
	});
});

describe('the plan', () => {
	test('the plan reads the asset', () => {
		expect(planFilmstrip(asset(60, [video(1920, 1080), audio]))).toEqual({
			interval: 0.5,
			frames: 120,
			frame_width: 170,
			frame_height: FILMSTRIP_HEIGHT,
			still: false
		});

		// A long asset is coarser, and capped.
		const long = planFilmstrip(asset(7200, [video(1080, 1920)]));
		expect([long.interval, long.frames, long.frame_width]).toEqual([30, 240, 54]);

		// A still is one thumbnail with no sampling.
		const still = planFilmstrip(asset(5, [video(800, 600, true)]));
		expect([still.frames, still.still, still.frame_width]).toEqual([1, true, 128]);
		// The one thumbnail stands for the whole asset.
		expect(still.interval).toBe(5);
		// Never a zero or sub-second interval to divide by.
		expect(planFilmstrip(asset(0.04, [video(800, 600, true)])).interval).toBe(1);
		expect(planFilmstrip(asset(NaN, [video(800, 600, true)])).interval).toBe(1);

		// No stream info: tried, as 16:9.
		const blind = planFilmstrip(asset(10, []));
		expect([blind.frame_width, blind.frames]).toEqual([170, 20]);
	});

	test('an asset without video has no filmstrip', () => {
		expect(() => planFilmstrip(asset(10, [audio]))).toThrow(/invalid argument: asset a1 has no video stream/);
	});

	test('the strip a plan delivers is laid out in the sheets the engine would write', () => {
		const strip = stripGeometry(planFilmstrip(asset(60, [video(1920, 1080), audio])));
		expect(strip.columns).toBe(40);
		// 120 thumbnails of 170 px: three sheets of 40, each exactly as wide as its row.
		expect(strip.sheets).toEqual([
			{ first_frame: 0, count: 40, width: 6800, height: 96 },
			{ first_frame: 40, count: 40, width: 6800, height: 96 },
			{ first_frame: 80, count: 40, width: 6800, height: 96 }
		]);

		// The last sheet is as wide as the others and holds fewer: the rest is padding.
		const uneven = stripGeometry(planFor(0.5, 49));
		expect(uneven.columns).toBe(25);
		expect(uneven.sheets.map((s) => [s.first_frame, s.count, s.width])).toEqual([
			[0, 25, 4250],
			[25, 24, 4250]
		]);
		expect(stripGeometry(planFor(5, 1, true)).sheets).toEqual([{ first_frame: 0, count: 1, width: 170, height: 96 }]);
	});
});

describe('looking a thumbnail up', () => {
	test('a time maps to the nearest thumbnail and back', () => {
		const strip = stripGeometry(planFor(2, 5)); // t = 0, 2, 4, 6, 8
		expect(timeOf(strip, 0)).toBe(0);
		expect(timeOf(strip, 3)).toBe(6);
		// The nearest sample in time, ties going to the later one (round half up).
		expect(frameAt(strip, 0)).toBe(0);
		expect(frameAt(strip, 0.9)).toBe(0);
		expect(frameAt(strip, 1.1)).toBe(1);
		expect(frameAt(strip, 1)).toBe(1);
		expect(frameAt(strip, 5)).toBe(3);
		expect(frameAt(strip, 7.9)).toBe(4);
		// Outside the strip, or meaningless: the ends.
		expect(frameAt(strip, -5)).toBe(0);
		expect(frameAt(strip, NaN)).toBe(0);
		expect(frameAt(strip, 1e9)).toBe(4);
		expect(frameAt(strip, Infinity)).toBe(4);
		// Round trip: every thumbnail is the nearest to its own time.
		for (let k = 0; k < strip.frames; k++) expect(frameAt(strip, timeOf(strip, k))).toBe(k);
	});

	test('a one-thumbnail strip is that thumbnail for every time', () => {
		const strip = stripGeometry(planFor(5, 1, true));
		for (const t of [-1, 0, 2.5, 5, 1e6, NaN]) expect(frameAt(strip, t)).toBe(0);
	});

	test('a thumbnail is found on its sheet', () => {
		const strip = stripGeometry(planFor(0.5, 120)); // three sheets of 40
		expect(strip.columns).toBe(40);
		const at = (k: number) => {
			const hit = locate(strip, k);
			return hit && [hit.sheet.first_frame, hit.x];
		};
		expect(at(0)).toEqual([0, 0]);
		expect(at(39)).toEqual([0, 39 * 170]);
		expect(at(40)).toEqual([40, 0]);
		expect(at(119)).toEqual([80, 39 * 170]);
		// Past the end, before the start, or not an index.
		expect(locate(strip, 120)).toBeNull();
		expect(locate(strip, -1)).toBeNull();
		expect(locate(strip, 1.5)).toBeNull();
		expect(locate(strip, NaN)).toBeNull();
		// The formula the type's docs give a caller agrees.
		for (let k = 0; k < strip.frames; k++) {
			const hit = locate(strip, k)!;
			expect(hit.sheet.first_frame).toBe(Math.floor(k / strip.columns) * strip.columns);
			expect(hit.x).toBe((k % strip.columns) * strip.frame_width);
		}
	});

	test('the last sheet stops at its count, not at its width', () => {
		const strip = stripGeometry(planFor(0.5, 49)); // 25 + 24, both 25 thumbnails wide
		expect(locate(strip, 48)?.x).toBe(23 * 170);
		expect(locate(strip, 49)).toBeNull();
	});
});
