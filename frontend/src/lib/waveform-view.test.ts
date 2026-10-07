import { describe, expect, test } from 'bun:test';
import {
	bucketCells,
	canvasScale,
	capDpr,
	CELL_RATE,
	CLIP_PEAK,
	clipCanvasRect,
	columnPeaks,
	isClipped,
	isReversed,
	laneCount,
	LEVEL_CELLS,
	MAX_CANVAS_PX,
	pxAtSource,
	RUNGS,
	RUNG_SLACK,
	secondsPerDevicePx,
	sourceAt,
	sourceWindow,
	speedMag,
	STEREO_MIN_HEIGHT,
	tileData,
	tileKey,
	tileSeconds,
	tileSpec,
	tilesFor,
	tilesForColumns,
	TILE_BUCKETS,
	visibleLaneRange,
	VIEW_QUANT,
	type ClipWindow,
	type TileData
} from './waveform-view';

function rng(seed: number) {
	let s = seed >>> 0;
	return () => {
		s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
		return s / 4294967296;
	};
}

/** A tile source where bucket `k` of the `cells` grid peaks at a value that
 *  names it: the same on every call, different for every bucket. */
function ramp(cells: number, channels = 1) {
	const calls: number[] = [];
	const tile = (index: number): TileData => {
		calls.push(index);
		const mk = (sign: number, off: number) =>
			Array.from({ length: channels }, (_, c) =>
				Float32Array.from({ length: TILE_BUCKETS }, (_, j) => sign * (((index * TILE_BUCKETS + j) % 997) / 1000 + 0.001 + c * off))
			);
		return { channels, buckets: TILE_BUCKETS, min: mk(-1, 0.0005), max: mk(1, 0.0005) };
	};
	return { tile, calls, cells };
}

describe('timeline px <-> source seconds', () => {
	const fwd: ClipWindow = { source_in: 10, source_out: 30 };

	test('forward: the left edge is source_in, the right edge source_out', () => {
		expect(sourceAt(fwd, 0, 50)).toBe(10);
		expect(sourceAt(fwd, 20 * 50, 50)).toBe(30);
		expect(sourceAt(fwd, 125, 50)).toBe(12.5);
	});

	test('reversed: the left edge is source_out, counting down', () => {
		const rev = { ...fwd, speed: -1 };
		expect(sourceAt(rev, 0, 50)).toBe(30);
		expect(sourceAt(rev, 20 * 50, 50)).toBe(10);
		expect(sourceAt(rev, 125, 50)).toBe(27.5);
		expect(isReversed(rev)).toBe(true);
		expect(isReversed(fwd)).toBe(false);
	});

	test('speed stretches the source under a pixel, either direction', () => {
		const fast = { source_in: 0, source_out: 40, speed: 2 }; // 20 s on the timeline
		expect(sourceAt(fast, 100, 50)).toBe(4); // 2 s in, 2x
		const slow = { source_in: 0, source_out: 5, speed: 0.5 }; // 10 s on the timeline
		expect(sourceAt(slow, 100, 50)).toBe(1);
		const backFast = { source_in: 0, source_out: 40, speed: -2 };
		expect(sourceAt(backFast, 100, 50)).toBe(36);
		expect(speedMag({ speed: 0 })).toBe(0.01);
		expect(speedMag({})).toBe(1);
	});

	test('pxAtSource is the inverse of sourceAt', () => {
		for (const speed of [1, 2, 0.5, -1, -2.5, undefined]) {
			const c = { source_in: 3, source_out: 33, speed };
			for (const px of [0, 17.5, 400, 1111]) {
				expect(pxAtSource(c, sourceAt(c, px, 36), 36)).toBeCloseTo(px, 9);
			}
		}
	});

	test('a window is ascending whichever way the clip plays', () => {
		expect(sourceWindow(fwd, 50, 150, 50)).toEqual([11, 13]);
		expect(sourceWindow({ ...fwd, speed: -1 }, 50, 150, 50)).toEqual([27, 29]);
	});

	test('device pixels: a pixel spans |speed| / (pxPerSec x dpr) of source', () => {
		expect(secondsPerDevicePx(100, 1, 1)).toBe(0.01);
		expect(secondsPerDevicePx(100, 2, 1)).toBe(0.005);
		expect(secondsPerDevicePx(100, 1, -4)).toBe(0.04);
		expect(secondsPerDevicePx(100, 1)).toBe(0.01);
	});
});

describe('device pixel ratio', () => {
	test('is at least 1 and at most 2', () => {
		expect(capDpr(1)).toBe(1);
		expect(capDpr(1.5)).toBe(1.5);
		expect(capDpr(2)).toBe(2);
		expect(capDpr(3)).toBe(2);
		expect(capDpr(0.8)).toBe(1);
		for (const bad of [NaN, 0, -1, Infinity - Infinity]) expect(capDpr(bad)).toBe(1);
	});

	test('a canvas is only drawn below that ratio when it would be enormous', () => {
		expect(canvasScale(4000, 2)).toBe(2);
		expect(canvasScale(MAX_CANVAS_PX, 2)).toBeLessThanOrEqual(1.0001);
		expect(canvasScale(MAX_CANVAS_PX * 4, 2) * MAX_CANVAS_PX * 4).toBeLessThanOrEqual(MAX_CANVAS_PX * 4 * 1.0001);
		expect(canvasScale(10_000, 2) * 10_000).toBeLessThanOrEqual(MAX_CANVAS_PX + 1);
	});
});

describe('bucket count from the on-screen width', () => {
	test('rungs are whole numbers of the stored levels', () => {
		for (const c of LEVEL_CELLS) expect(RUNGS).toContain(c);
		for (let i = 1; i < RUNGS.length; i++) expect(RUNGS[i]).toBeGreaterThan(RUNGS[i - 1]);
		// 500 / 100 / 25 / 10 a second, as the backend stores them
		expect(LEVEL_CELLS.map((c) => CELL_RATE / c)).toEqual([500, 100, 25, 10]);
	});

	test('a bucket is never wider than the slack allows, and never needlessly narrow', () => {
		for (let sec = 0.0005; sec < 8; sec *= 1.07) {
			const cells = bucketCells(sec);
			const widthPx = cells / CELL_RATE / sec; // device px one bucket spans
			if (cells > RUNGS[0]) expect(widthPx).toBeLessThanOrEqual(RUNG_SLACK + 1e-9);
			// the next rung up would be too wide
			const next = RUNGS[RUNGS.indexOf(cells) + 1];
			if (next !== undefined) expect(next / CELL_RATE / sec).toBeGreaterThan(RUNG_SLACK);
		}
	});

	test('zooming out never asks for finer buckets', () => {
		let last = 0;
		for (let sec = 0.001; sec < 4; sec *= 1.03) {
			const cells = bucketCells(sec);
			expect(cells).toBeGreaterThanOrEqual(last);
			last = cells;
		}
	});

	test('the timeline zoom range at both ratios', () => {
		// pxPerSec 96 @2x: 5.2 ms under a pixel -> the finest level
		expect(bucketCells(secondsPerDevicePx(96, 2, 1))).toBe(1);
		// 36 px/s @1x (the default zoom): 27.8 ms -> the 25/s level
		expect(bucketCells(secondsPerDevicePx(36, 1, 1))).toBe(20);
		// 8 px/s @1x: 125 ms -> the 10/s level
		expect(bucketCells(secondsPerDevicePx(8, 1, 1))).toBe(50);
		// 8x speed zoomed out: past the stored levels, doubling
		expect(bucketCells(secondsPerDevicePx(8, 1, 8))).toBe(400);
	});
});

describe('tiles', () => {
	test('a tile is TILE_BUCKETS buckets wide, aligned to the source clock', () => {
		for (const cells of [1, 5, 20, 50, 100]) {
			expect(tileSeconds(cells)).toBe((TILE_BUCKETS * cells) / CELL_RATE);
			const t = tileSpec(cells, 3);
			// one division of an integer, not an accumulated multiple
			expect(t.start).toBe((3 * TILE_BUCKETS * cells) / CELL_RATE);
			expect(t.end).toBe((4 * TILE_BUCKETS * cells) / CELL_RATE);
			expect(t.start).toBeCloseTo(3 * tileSeconds(cells), 9);
			expect(t.buckets).toBe(TILE_BUCKETS);
			expect(tileSpec(cells, 4).start).toBe(t.end); // no seam between neighbours
		}
	});

	test('the tiles for a window cover it and nothing past the media', () => {
		const dur = 600;
		const rand = rng(5);
		for (const cells of [1, 5, 20, 50, 200]) {
			for (let i = 0; i < 200; i++) {
				const a = rand() * 650;
				const b = a + rand() * 60 + 0.01;
				const tiles = tilesFor(a, b, cells, dur);
				if (a >= dur) {
					expect(tiles).toEqual([]);
					continue;
				}
				expect(tiles[0].start).toBeLessThanOrEqual(a);
				expect(tiles[tiles.length - 1].end).toBeGreaterThanOrEqual(Math.min(b, dur));
				for (let k = 1; k < tiles.length; k++) expect(tiles[k].start).toBe(tiles[k - 1].end);
				expect(tiles[tiles.length - 1].start).toBeLessThan(dur);
			}
		}
	});

	test('degenerate windows ask for nothing', () => {
		expect(tilesFor(5, 5, 5, 100)).toEqual([]);
		expect(tilesFor(6, 5, 5, 100)).toEqual([]);
		expect(tilesFor(0, 5, 5, 0)).toEqual([]);
		expect(tilesFor(100, 200, 5, 100)).toEqual([]);
		expect(tilesFor(-4, 1, 5, 100).map((t) => t.index)).toEqual([0]);
	});

	test('small scrolls hit the same tiles: the key set only changes on a tile boundary', () => {
		const cells = 5;
		const T = tileSeconds(cells); // 20.48 s
		const keys = (a: number, b: number) => tilesFor(a, b, cells, 600).map((t) => tileKey('asset', t));
		const base = keys(T + 0.5, T + 7.5);
		expect(keys(T + 1.3, T + 8.3)).toEqual(base);
		expect(keys(T + 0.01, T + 7.4)).toEqual(base);
		expect(keys(2 * T + 1, 2 * T + 8)).not.toEqual(base); // crossed into the next tile
		// the same window on another asset is another key
		expect(tilesFor(T + 0.5, T + 7.5, cells, 600).map((t) => tileKey('other', t))).not.toEqual(base);
	});

	test('the key names asset, source window and bucket count', () => {
		const t = tileSpec(5, 2);
		expect(tileKey('a1', t)).toBe(`a1:${t.start.toFixed(6)}:${t.end.toFixed(6)}:${TILE_BUCKETS}`);
		expect(tileKey('a1', t)).not.toBe(tileKey('a1', { ...t, buckets: 512 }));
		expect(tileKey('a1', t)).not.toBe(tileKey('a1', tileSpec(5, 3)));
		expect(tileKey('a1', t)).not.toBe(tileKey('a1', tileSpec(20, 2)));
	});

	test('a range answer becomes float arrays, ones at full scale intact', () => {
		const d = tileData({
			channels: 2,
			buckets: 3,
			duration: 9,
			peaks_per_second: 100,
			min: [[-1, -0.25, 0], [-0.5, 0, 0]],
			max: [[1, 0.25, 0], [0.5, 0, 0]]
		});
		expect(d.channels).toBe(2);
		expect(d.min[0]).toBeInstanceOf(Float32Array);
		expect(d.max[0][0]).toBe(1);
		expect(d.min[0][0]).toBe(-1);
		expect(d.min[1][0]).toBeCloseTo(-0.5, 7);
	});
});

describe('what part of a clip is on screen', () => {
	test('the viewport range grows by the overscan and snaps outward to the quantum', () => {
		const r = visibleLaneRange(1000, 800);
		expect(r.lo % VIEW_QUANT).toBe(0);
		expect(r.hi % VIEW_QUANT).toBe(0);
		expect(r.lo).toBeLessThanOrEqual(1000 - 400);
		expect(r.hi).toBeGreaterThanOrEqual(1000 + 800 + 400);
		expect(visibleLaneRange(0, 800).lo).toBe(0);
		expect(visibleLaneRange(-50, 800).lo).toBe(0);
	});

	test('scrolling a little does not move the range; scrolling a screen does', () => {
		const a = visibleLaneRange(1000, 800);
		expect(visibleLaneRange(1010, 800)).toEqual(a);
		expect(visibleLaneRange(1100, 800).hi).toBeGreaterThanOrEqual(a.hi);
		expect(visibleLaneRange(2400, 800)).not.toEqual(a);
	});

	test('a clip draws only its on-screen part, in whole clip-local pixels', () => {
		// a clip 20 000 px wide starting at 500, viewport range [1024, 3072]
		expect(clipCanvasRect(500, 20_000, 1024, 3072)).toEqual({ x0: 524, x1: 2572 });
		// a clip that starts inside the range starts at 0
		expect(clipCanvasRect(1500, 400, 1024, 3072)).toEqual({ x0: 0, x1: 400 });
		// fractional clip edges still give whole pixels covering the range
		const r = clipCanvasRect(500.4, 2000.3, 1024, 3072)!;
		expect(Number.isInteger(r.x0) && Number.isInteger(r.x1)).toBe(true);
		expect(500.4 + r.x0).toBeLessThanOrEqual(1024);
	});

	test('a clip off screen draws nothing', () => {
		expect(clipCanvasRect(0, 500, 1024, 3072)).toBeNull();
		expect(clipCanvasRect(4000, 500, 1024, 3072)).toBeNull();
		expect(clipCanvasRect(1024 - 500, 500, 1024, 3072)).toBeNull();
	});
});

describe('lanes', () => {
	test('stereo gets two lanes only when the track is tall enough', () => {
		expect(laneCount(2, STEREO_MIN_HEIGHT)).toBe(2);
		expect(laneCount(2, STEREO_MIN_HEIGHT - 1)).toBe(1);
		expect(laneCount(2, 54)).toBe(2); // the default 64 px track's clip
		expect(laneCount(2, 38)).toBe(1);
		expect(laneCount(2, 400)).toBe(2);
	});

	test('mono is one lane at any height', () => {
		for (const h of [10, 54, 500]) expect(laneCount(1, h)).toBe(1);
		expect(laneCount(0, 100)).toBe(1);
	});
});

describe('columnPeaks', () => {
	test('a forward clip reads one bucket a column at 1:1', () => {
		const { tile } = ramp(5);
		const clip = { source_in: 0, source_out: 2 };
		const cols = columnPeaks({ clip, pxPerSec: 100, dpr: 1, x0: 0, columns: 200, cells: 5, lanes: 1, duration: 120, tile });
		expect(cols.missing).toBe(0);
		for (const d of [0, 1, 57, 199]) {
			expect(cols.max[0][d]).toBeCloseTo(((d % 997) / 1000 + 0.001), 6);
			expect(cols.min[0][d]).toBeCloseTo(-((d % 997) / 1000 + 0.001), 6);
		}
	});

	test('a reversed clip draws mirrored: the same columns, back to front', () => {
		const { tile } = ramp(5);
		const args = { pxPerSec: 100, dpr: 1, x0: 0, columns: 200, cells: 5, lanes: 1, duration: 120, tile };
		const fwd = columnPeaks({ ...args, clip: { source_in: 0, source_out: 2 } });
		const rev = columnPeaks({ ...args, clip: { source_in: 0, source_out: 2, speed: -1 } });
		for (let d = 0; d < 200; d++) {
			expect(rev.max[0][d]).toBe(fwd.max[0][199 - d]);
			expect(rev.min[0][d]).toBe(fwd.min[0][199 - d]);
		}
	});

	test('a clip with an offset source and a canvas that starts part-way in', () => {
		const { tile } = ramp(5);
		// source_in 3 s, canvas starts 40 px in at 100 px/s: column 0 is source 3.40 s = bucket 340
		const cols = columnPeaks({
			clip: { source_in: 3, source_out: 9 },
			pxPerSec: 100,
			dpr: 1,
			x0: 40,
			columns: 10,
			cells: 5,
			lanes: 1,
			duration: 120,
			tile
		});
		for (let d = 0; d < 10; d++) expect(cols.max[0][d]).toBeCloseTo(((340 + d) % 997) / 1000 + 0.001, 6);
	});

	test('coarser than a bucket takes the extremes of every bucket in the column', () => {
		const { tile } = ramp(5);
		// speed 2 at 100 px/s: a column is 20 ms of source = two 10 ms buckets
		const cols = columnPeaks({
			clip: { source_in: 0, source_out: 8, speed: 2 },
			pxPerSec: 100,
			dpr: 1,
			x0: 0,
			columns: 50,
			cells: 5,
			lanes: 1,
			duration: 120,
			tile
		});
		for (let d = 0; d < 50; d++) {
			const a = ((2 * d) % 997) / 1000 + 0.001;
			const b = ((2 * d + 1) % 997) / 1000 + 0.001;
			expect(cols.max[0][d]).toBeCloseTo(Math.max(a, b), 6);
			expect(cols.min[0][d]).toBeCloseTo(-Math.max(a, b), 6);
		}
	});

	test('finer than a bucket reads the bucket under the column', () => {
		const { tile } = ramp(50); // 100 ms buckets
		const cols = columnPeaks({
			clip: { source_in: 0, source_out: 10 },
			pxPerSec: 100,
			dpr: 2, // a device pixel is 5 ms; a bucket 20 of them
			x0: 0,
			columns: 400,
			cells: 50,
			lanes: 1,
			duration: 120,
			tile
		});
		// bucket k covers 100k..100k+100 ms = device columns 20k..20k+19, and every
		// column in it reads that one bucket
		for (const d of [0, 7, 19, 20, 39, 40, 399]) {
			const k = Math.floor(d / 20);
			expect(cols.max[0][d]).toBeCloseTo((k % 997) / 1000 + 0.001, 6);
		}
	});

	test('stereo keeps a lane per channel, or folds them into one', () => {
		const { tile } = ramp(5, 2);
		const args = {
			clip: { source_in: 0, source_out: 2 },
			pxPerSec: 100,
			dpr: 1,
			x0: 0,
			columns: 20,
			cells: 5,
			duration: 120,
			tile
		};
		const two = columnPeaks({ ...args, lanes: 2 });
		const one = columnPeaks({ ...args, lanes: 1 });
		expect(two.min).toHaveLength(2);
		expect(one.min).toHaveLength(1);
		for (let d = 0; d < 20; d++) {
			// channel 1 runs 0.0005 hotter than channel 0
			expect(two.max[1][d]).toBeCloseTo(two.max[0][d] + 0.0005, 6);
			expect(one.max[0][d]).toBeCloseTo(Math.max(two.max[0][d], two.max[1][d]), 7);
			expect(one.min[0][d]).toBeCloseTo(Math.min(two.min[0][d], two.min[1][d]), 7);
		}
	});

	test('a tile that is not loaded leaves its columns counted as missing', () => {
		const { tile } = ramp(5);
		const partial = (i: number) => (i === 0 ? tile(0) : undefined);
		// 20.48 s tiles: 6 s of clip starting 2 s before the boundary crosses into tile 1
		const T = tileSeconds(5);
		const cols = columnPeaks({
			clip: { source_in: T - 2, source_out: T + 4 },
			pxPerSec: 100,
			dpr: 1,
			x0: 0,
			columns: 500,
			cells: 5,
			lanes: 1,
			duration: 120,
			tile: partial
		});
		expect(cols.missing).toBeGreaterThan(0);
		expect(cols.missing).toBeLessThan(500);
	});

	test('past the end of the media is silence, not missing', () => {
		const { tile } = ramp(5);
		const cols = columnPeaks({
			clip: { source_in: 9, source_out: 20 },
			pxPerSec: 100,
			dpr: 1,
			x0: 0,
			columns: 300,
			cells: 5,
			lanes: 1,
			duration: 10, // the media ends 100 columns in
			tile
		});
		expect(cols.missing).toBe(0);
		expect(cols.max[0][50]).toBeGreaterThan(0);
		for (let d = 100; d < 300; d++) {
			expect(cols.max[0][d]).toBe(0);
			expect(cols.min[0][d]).toBe(0);
		}
	});

	test('the tiles a canvas asks for are exactly enough to draw it', () => {
		const rand = rng(21);
		for (let i = 0; i < 300; i++) {
			const speed = [1, 1, 2, 0.5, -1, -3][Math.floor(rand() * 6)];
			const pxPerSec = 8 + rand() * 88;
			const dpr = rand() < 0.5 ? 1 : 2;
			const clip = { source_in: rand() * 100, source_out: 0, speed };
			clip.source_out = clip.source_in + 5 + rand() * 600;
			const columns = Math.round((200 + rand() * 3000) * dpr);
			const x0 = Math.floor(rand() * 500);
			const cells = bucketCells(secondsPerDevicePx(pxPerSec, dpr, speed));
			const duration = clip.source_out + rand() * 50;
			const base = { clip, pxPerSec, dpr, x0, columns, cells, duration };
			const wanted = new Map(tilesForColumns(base).map((t) => [t.index, ramp(cells).tile(t.index)]));
			const cols = columnPeaks({ ...base, lanes: 1, tile: (idx) => wanted.get(idx) });
			expect(cols.missing).toBe(0);
		}
	});
});

describe('clipping', () => {
	test('a peak at full scale is clipped, a hair under the threshold is not', () => {
		expect(isClipped(-1, 1, 1)).toBe(true);
		expect(isClipped(-0.2, 1, 1)).toBe(true);
		expect(isClipped(-1, 0.2, 1)).toBe(true);
		expect(isClipped(-CLIP_PEAK, 0.1, 1)).toBe(true);
		expect(isClipped(-0.9985, 0.9985, 1)).toBe(false);
		expect(isClipped(-0.5, 0.5, 1)).toBe(false);
	});

	test('a source that clips is still clipped turned down; a boost can clip a clean one', () => {
		expect(isClipped(-1, 1, 0.25)).toBe(true);
		expect(isClipped(-0.5, 0.5, 2)).toBe(true);
		expect(isClipped(-0.5, 0.5, 1.9)).toBe(false);
		expect(isClipped(-0.5, 0.5, 0)).toBe(false);
	});
});
