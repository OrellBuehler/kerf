import { afterEach, describe, expect, test } from 'bun:test';
import {
	MAX_SAMPLES,
	MOTION_PATH_KEY,
	addKeyTracks,
	centreOf,
	dragKey,
	hereKey,
	keyAt,
	keyTimes,
	loadMotionPath,
	moveKeyTracks,
	nearestOnPath,
	pathAt,
	pathData,
	pathFrame,
	pathKeys,
	pathTicks,
	positionKeyed,
	positionOf,
	sampleTimes,
	samplePath,
	saveMotionPath,
	seekTarget,
	snapKeyTime,
	tickPath,
	withTracks,
	type PathFrame
} from './motion-path';
import { propertyAt, propertyKeys, setPropertyKeys, transformAt } from './channels';
import { geometryOf, pictureBox, snapTargets, type Canvas, type Snapping } from './transform-handles';
import { DEFAULT_TRANSFORM, type Clip, type Keyframe, type PropertyKey } from './types';

const key = (time: number, value: number, easing?: PropertyKey['easing']): PropertyKey => ({ time, value, ...(easing ? { easing } : {}) });
const bundle = (time: number, pos_x: number, pos_y: number, easing?: Keyframe['easing']): Keyframe => ({
	time,
	scale: 1,
	pos_x,
	pos_y,
	rotation: 0,
	opacity: 1,
	...(easing ? { easing } : {})
});

const canvas: Canvas = { width: 1920, height: 1080, fit: 'contain' };
const frame: PathFrame = { canvas, shift: { x: 0, y: 0 } };
const FPS = 30;

function clip(extra: Partial<Clip> = {}): Clip {
	return {
		id: 'c',
		asset_id: 'a',
		source_in: 0,
		source_out: 4,
		timeline_start: 10,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		transform: { ...DEFAULT_TRANSFORM },
		...extra
	} as Clip;
}

/** pos_x keyed at 0 / 2 / 4, pos_y at 0 / 1 / 4: the union is 0, 1, 2, 4. */
const moving = () =>
	clip({
		channels: [
			{ prop: 'pos_x', keys: [key(0, -0.2, 'ease_in_out'), key(2, 0.1), key(4, 0.3)] },
			{ prop: 'pos_y', keys: [key(0, 0.1), key(1, -0.1, 'ease_out'), key(4, 0.2)] }
		]
	});

const noSnap: Snapping = { on: false, targets: { xs: [], ys: [] }, threshold: 0 };

describe('the canvas mapping', () => {
	test('a position is a fraction of the frame from its centre, and the shift is added', () => {
		expect(centreOf(frame, 0, 0)).toEqual({ x: 960, y: 540 });
		expect(centreOf(frame, 0.25, -0.5)).toEqual({ x: 1440, y: 0 });
		const shifted: PathFrame = { canvas, shift: { x: -1, y: 2 } };
		expect(centreOf(shifted, 0.25, -0.5)).toEqual({ x: 1439, y: 2 });
	});

	test('positionOf undoes centreOf', () => {
		const f: PathFrame = { canvas, shift: { x: 1, y: -2 } };
		const p = positionOf(f, centreOf(f, 0.123, -0.456));
		expect(p.pos_x).toBeCloseTo(0.123, 12);
		expect(p.pos_y).toBeCloseTo(-0.456, 12);
	});

	test('the path meets the box the geometry draws, at the playhead, rounding and all', () => {
		// A 640x360 picture letterboxed on a 360x640 canvas sits one pixel off the canvas centre.
		const c: Canvas = { width: 360, height: 640, fit: 'contain' };
		for (const pose of [
			{ ...DEFAULT_TRANSFORM },
			{ ...DEFAULT_TRANSFORM, scale: 0.5, pos_x: 0.1, pos_y: -0.2 },
			{ ...DEFAULT_TRANSFORM, scale: 0.37, rotation: 33, pos_x: -0.3, pos_y: 0.07 }
		]) {
			const box = pictureBox(geometryOf([640, 360], [1, 1], c, pose)!);
			const f = pathFrame(c, box, pose);
			const at = centreOf(f, pose.pos_x, pose.pos_y);
			expect(at.x).toBeCloseTo(box.cx, 9);
			expect(at.y).toBeCloseTo(box.cy, 9);
		}
	});
});

describe('which clips have a path', () => {
	test('a position that is keyed moves; a static one, or any other number keyed, does not', () => {
		expect(positionKeyed(clip())).toBe(false);
		expect(positionKeyed(clip({ channels: [{ prop: 'opacity', keys: [key(0, 1), key(1, 0)] }] }))).toBe(false);
		expect(positionKeyed(clip({ channels: [{ prop: 'pos_y', keys: [key(0, 0), key(1, 0.1)] }] }))).toBe(true);
	});

	test('the legacy bundle drives position too, unless a track takes the number over', () => {
		const c = clip({ keyframes: [bundle(0, 0, 0), bundle(2, 0.2, -0.1)] });
		expect(positionKeyed(c)).toBe(true);
		expect(keyTimes(c, 4)).toEqual([0, 2]);
		const detached = clip({ keyframes: [bundle(0, 0, 0), bundle(2, 0.2, -0.1)], channels: [{ prop: 'pos_x', keys: [] }, { prop: 'pos_y', keys: [] }] });
		expect(positionKeyed(detached)).toBe(false);
	});
});

describe('the keys', () => {
	test('a position key is the union of both numbers’ moments, in order, once each', () => {
		expect(keyTimes(moving(), 4)).toEqual([0, 1, 2, 4]);
		const same = clip({
			channels: [
				{ prop: 'pos_x', keys: [key(1, 0), key(2 + 1e-7, 1)] },
				{ prop: 'pos_y', keys: [key(2, 0), key(3, 1)] }
			]
		});
		expect(keyTimes(same, 4)).toEqual([1, 2, 3]);
	});

	test('keys past the end of the clip are not on its path', () => {
		expect(keyTimes(moving(), 3)).toEqual([0, 1, 2]);
		expect(keyTimes(moving(), 3.999999)).toEqual([0, 1, 2, 4]);
	});

	test('each key reads the number it has no key for off its curve, and says which it has', () => {
		const keys = pathKeys(moving(), frame, 4);
		expect(keys.map((k) => k.time)).toEqual([0, 1, 2, 4]);
		expect(keys.map((k) => k.keyed)).toEqual([
			{ pos_x: true, pos_y: true },
			{ pos_x: false, pos_y: true },
			{ pos_x: true, pos_y: false },
			{ pos_x: true, pos_y: true }
		]);
		const c = moving();
		for (const k of keys) {
			expect(k.pos_x).toBeCloseTo(propertyAt(c, 'pos_x', k.time), 12);
			expect(k.pos_y).toBeCloseTo(propertyAt(c, 'pos_y', k.time), 12);
			expect(k.x).toBeCloseTo(1920 * (0.5 + k.pos_x), 9);
			expect(k.y).toBeCloseTo(1080 * (0.5 + k.pos_y), 9);
		}
		expect(keys[1].pos_y).toBe(-0.1);
	});

	test('a number the bundle drives is read from it, and an axis nobody keys is its static value', () => {
		const c = clip({ keyframes: [bundle(0, 0, 0), bundle(2, 0.2, -0.1)], transform: { ...DEFAULT_TRANSFORM, pos_y: 0.4 }, channels: [{ prop: 'pos_y', keys: [] }] });
		const keys = pathKeys(c, frame, 4);
		expect(keys.map((k) => [k.time, k.pos_x, k.pos_y])).toEqual([
			[0, 0, 0.4],
			[2, 0.2, 0.4]
		]);
		expect(keys.every((k) => k.keyed.pos_x && !k.keyed.pos_y)).toBe(true);
	});

	test('the key at the playhead is the nearest within a frame of it', () => {
		const keys = pathKeys(moving(), frame, 4);
		expect(hereKey(keys, 1, FPS)).toBe(1);
		expect(hereKey(keys, 1 + 1 / FPS - 1e-6, FPS)).toBe(1);
		expect(hereKey(keys, 1.5, FPS)).toBe(-1);
		expect(hereKey(keys, 0, FPS)).toBe(0);
		const close = pathKeys(clip({ channels: [{ prop: 'pos_x', keys: [key(1, 0), key(1.02, 1)] }] }), frame, 4);
		expect(hereKey(close, 1.015, FPS)).toBe(1);
		expect(hereKey(close, 1.005, FPS)).toBe(0);
		expect(hereKey([], 1, FPS)).toBe(-1);
	});

	test('a key is found by the dot under a point, the nearest within reach', () => {
		const keys = pathKeys(moving(), frame, 4);
		expect(keyAt(keys, { x: keys[2].x + 3, y: keys[2].y - 4 }, 6)).toBe(2);
		expect(keyAt(keys, { x: keys[2].x + 30, y: keys[2].y }, 6)).toBe(-1);
		expect(keyAt([], { x: 0, y: 0 }, 6)).toBe(-1);
	});

	test('clicking the key at the clip’s very end lands on its last frame, still inside the clip', () => {
		expect(seekTarget(10, 4, 2, FPS)).toBe(12);
		expect(seekTarget(10, 4, 4, FPS)).toBeCloseTo(10 + 4 - 1 / FPS, 12);
		expect(seekTarget(10, 4, 4 + 1e-7, FPS)).toBeCloseTo(10 + 4 - 1 / FPS, 12);
		expect(seekTarget(10, 0.01, 0.01, FPS)).toBe(10);
	});
});

describe('sampling the curve', () => {
	test('every frame of the clip, its end and every key', () => {
		const t = sampleTimes(1, FPS, [0, 0.5, 1]);
		expect(t.length).toBe(31);
		expect(t[0]).toBe(0);
		expect(t.at(-1)).toBe(1);
		expect(t).toContain(0.5);
		expect(t.every((v, i) => i === 0 || v > t[i - 1])).toBe(true);
		const off = sampleTimes(1, FPS, [0.41]);
		expect(off).toContain(0.41);
		expect(off.length).toBe(32);
	});

	test('a long clip is sampled every few frames, never more than the cap', () => {
		const t = sampleTimes(600, FPS, [3, 17.5]);
		expect(t.length).toBeLessThanOrEqual(MAX_SAMPLES);
		expect(t[0]).toBe(0);
		expect(t.at(-1)).toBe(600);
		expect(t).toContain(17.5);
		expect(sampleTimes(0, FPS, [])).toEqual([0]);
		expect(sampleTimes(2, NaN, [1])).toEqual([0, 1, 2]);
	});

	test('the path is the eased curve: keys land on their dots and an ease leaves the straight line', () => {
		const c = moving();
		const s = samplePath(c, frame, 4, FPS);
		for (const k of pathKeys(c, frame, 4)) {
			const at = s.find((p) => Math.abs(p.t - k.time) < 1e-9)!;
			expect(at.x).toBeCloseTo(k.x, 9);
			expect(at.y).toBeCloseTo(k.y, 9);
		}
		// pos_x eases in and out from -0.2 to 0.1 over the first two seconds: slow at the start, passing the middle at the half.
		const early = s.find((p) => Math.abs(p.t - 0.2) < 1e-9)!;
		expect(early.x).toBeLessThan(1920 * (0.5 - 0.2 + 0.3 * 0.1));
		const mid = s.find((p) => Math.abs(p.t - 1) < 1e-9)!;
		expect(mid.x).toBeCloseTo(1920 * (0.5 - 0.05), 3);
		const end = s.at(-1)!;
		expect([end.x, end.y]).toEqual([1920 * 0.8, 1080 * 0.7]);
	});

	test('it reads the same curve as the resolver, sample by sample', () => {
		const c = moving();
		for (const p of samplePath(c, frame, 4, FPS)) {
			const t = transformAt(c, p.t);
			expect(p.x).toBeCloseTo(1920 * (0.5 + t.pos_x), 9);
			expect(p.y).toBeCloseTo(1080 * (0.5 + t.pos_y), 9);
		}
	});

	test('a hold is the straight jump to where the next key lands', () => {
		const c = clip({ channels: [{ prop: 'pos_x', keys: [key(0, -0.2, 'hold'), key(1, 0.2)] }] });
		const s = samplePath(c, frame, 2, FPS);
		const before = s.filter((p) => p.t < 1);
		expect(before.every((p) => p.x === 1920 * 0.3)).toBe(true);
		expect(s.find((p) => p.t === 1)!.x).toBe(1920 * 0.7);
	});

	test('the legacy bundle’s keys draw the same way', () => {
		const c = clip({ keyframes: [bundle(0, -0.2, 0, 'ease_in_out'), bundle(2, 0.2, 0.1)] });
		const s = samplePath(c, frame, 4, FPS);
		expect(s.find((p) => p.t === 1)!.x).toBeCloseTo(1920 * 0.5, 3);
		expect(s.find((p) => p.t === 2)!.y).toBeCloseTo(1080 * 0.6, 9);
		expect(s.at(-1)!.x).toBeCloseTo(1920 * 0.7, 9);
	});

	test('pathData is an SVG path through the points', () => {
		expect(pathData([])).toBe('');
		expect(pathData([{ x: 1, y: 2 }, { x: 3.456, y: 4 }])).toBe('M1.00 2.00 L3.46 4.00');
	});
});

describe('speed ticks', () => {
	test('equal steps of time, so the gaps between them are the speed', () => {
		const c = clip({ channels: [{ prop: 'pos_x', keys: [key(0, -0.3, 'ease_in_out'), key(2, 0.3)] }] });
		const ticks = pathTicks(c, frame, 2, FPS, []);
		expect(ticks.length).toBeGreaterThan(40);
		const gap = (i: number) => ticks[i + 1].x - ticks[i].x;
		expect(gap(0)).toBeLessThan(gap(Math.floor(ticks.length / 2) - 1));
		expect(gap(ticks.length - 2)).toBeLessThan(gap(Math.floor(ticks.length / 2) - 1));
		expect(ticks.every((k, i) => i === 0 || k.t > ticks[i - 1].t)).toBe(true);
	});

	test('the mark runs across the path, as a unit normal', () => {
		const c = clip({ channels: [{ prop: 'pos_x', keys: [key(0, -0.3), key(2, 0.3)] }] });
		const [k] = pathTicks(c, frame, 2, FPS, []);
		expect(k.nx).toBeCloseTo(0, 9);
		expect(Math.abs(k.ny)).toBeCloseTo(1, 9);
		expect(tickPath([k], 5)).toBe(`M${k.x.toFixed(2)} ${(k.y - k.ny * 5).toFixed(2)} L${k.x.toFixed(2)} ${(k.y + k.ny * 5).toFixed(2)}`);
		expect(tickPath([], 5)).toBe('');
	});

	test('none at a key’s dot, none where the clip stands still, a few frames apart on a long clip', () => {
		const c = clip({ channels: [{ prop: 'pos_x', keys: [key(0, -0.3), key(1, 0.3), key(3, 0.3)] }] });
		const ticks = pathTicks(c, frame, 3, FPS, [0, 1, 3]);
		expect(ticks.some((k) => Math.abs(k.t - 1) < 1e-9)).toBe(false);
		expect(ticks.some((k) => Math.abs(k.t) < 1e-9)).toBe(false);
		expect(ticks.some((k) => k.t > 1.1)).toBe(false);
		const long = pathTicks(clip({ channels: [{ prop: 'pos_x', keys: [key(0, -0.3), key(300, 0.3)] }] }), frame, 300, FPS, []);
		expect(long.length).toBeLessThanOrEqual(90);
		expect(long.length).toBeGreaterThan(20);
		expect(pathTicks(c, frame, 0, FPS, [])).toEqual([]);
		expect(pathTicks(c, frame, 3, NaN, [])).toEqual([]);
	});
});

describe('hit testing the path', () => {
	const line = [
		{ t: 0, x: 0, y: 0 },
		{ t: 1, x: 100, y: 0 },
		{ t: 3, x: 100, y: 200 }
	];

	test('the nearest point of the path and the moment it is at', () => {
		const a = nearestOnPath(line, { x: 25, y: 10 })!;
		expect([a.t, a.x, a.y, a.distance]).toEqual([0.25, 25, 0, 10]);
		const b = nearestOnPath(line, { x: 130, y: 100 })!;
		expect([b.t, b.x, b.y, b.distance]).toEqual([2, 100, 100, 30]);
	});

	test('past either end it is the end, and a stationary clip is its one point', () => {
		expect(nearestOnPath(line, { x: -50, y: 0 })).toMatchObject({ t: 0, x: 0, y: 0, distance: 50 });
		expect(nearestOnPath(line, { x: 100, y: 500 })).toMatchObject({ t: 3, distance: 300 });
		expect(nearestOnPath([{ t: 2, x: 5, y: 5 }], { x: 5, y: 8 })).toMatchObject({ t: 2, distance: 3 });
		const still = [
			{ t: 0, x: 5, y: 5 },
			{ t: 1, x: 5, y: 5 }
		];
		expect(nearestOnPath(still, { x: 9, y: 8 })).toMatchObject({ t: 0, distance: 5 });
		expect(nearestOnPath([], { x: 0, y: 0 })).toBeNull();
	});

	test('a moment is brought to a frame of the cut, not of the clip, and held to the clip', () => {
		expect(snapKeyTime(10.01, 1.0, 4, FPS)).toBeCloseTo(11 - 10.01, 12);
		expect(snapKeyTime(10, 1.016, 4, FPS)).toBeCloseTo(31 / FPS - 0, 1);
		expect(snapKeyTime(10, -3, 4, FPS)).toBe(0);
		expect(snapKeyTime(10, 9, 4, FPS)).toBe(4);
	});
});

describe('moving a key', () => {
	test('a number with a key there takes the new value, and the key keeps how it leaves', () => {
		const c = moving();
		const tracks = moveKeyTracks(c, 0, { pos_x: -0.4, pos_y: 0.3 }, keyTimes(c, 4));
		expect(tracks.map((t) => t.prop)).toEqual(['pos_x', 'pos_y']);
		expect(tracks[0].keys).toEqual([key(0, -0.4, 'ease_in_out'), key(2, 0.1), key(4, 0.3)]);
		expect(tracks[1].keys).toEqual([key(0, 0.3), key(1, -0.1, 'ease_out'), key(4, 0.2)]);
	});

	test('only the numbers that moved are written', () => {
		const c = moving();
		const tracks = moveKeyTracks(c, 2, { pos_x: 0.15 }, keyTimes(c, 4));
		expect(tracks.map((t) => t.prop)).toEqual(['pos_x']);
		expect(moveKeyTracks(c, 2, {}, keyTimes(c, 4))).toEqual([]);
		expect(moveKeyTracks(c, 2, { pos_x: NaN }, keyTimes(c, 4))).toEqual([]);
	});

	test('a number with no key at that moment gets one, splitting its segment so the rest of its curve holds', () => {
		const c = moving();
		const [x, y] = moveKeyTracks(c, 1, { pos_x: 0, pos_y: 0 }, keyTimes(c, 4));
		expect(x.keys.map((k) => k.time)).toEqual([0, 1, 2, 4]);
		expect(x.keys[1].value).toBe(0);
		expect(typeof x.keys[0].easing).toBe('object');
		expect(y.keys.map((k) => k.value)).toEqual([0.1, 0, 0.2]);
		expect(y.keys[1].easing).toBe('ease_out');
	});

	test('a number nobody keys is keyed at every position key, still but where it was moved', () => {
		const c = clip({
			transform: { ...DEFAULT_TRANSFORM, pos_y: 0.25 },
			channels: [{ prop: 'pos_x', keys: [key(0, -0.2, 'ease_in_out'), key(1, 0), key(2, 0.2, 'hold')] }]
		});
		const tracks = moveKeyTracks(c, 1, { pos_y: 0.4 }, keyTimes(c, 4));
		expect(tracks).toEqual([
			{
				prop: 'pos_y',
				keys: [key(0, 0.25, 'ease_in_out'), key(1, 0.4), key(2, 0.25, 'hold')]
			}
		]);
		const drawn = withTracks(c, tracks);
		expect(propertyAt(drawn, 'pos_y', 0)).toBe(0.25);
		expect(propertyAt(drawn, 'pos_y', 1)).toBe(0.4);
		expect(propertyAt(drawn, 'pos_y', 2)).toBe(0.25);
		expect(propertyAt(drawn, 'pos_x', 0.5)).toBe(propertyAt(c, 'pos_x', 0.5));
	});

	test('a number the legacy bundle drives is detached alone: its track appears, the other four stay in the bundle', () => {
		const c = clip({ keyframes: [bundle(0, 0, 0), bundle(2, 0.2, -0.1)] });
		const tracks = moveKeyTracks(c, 2, { pos_x: 0.3 }, keyTimes(c, 4));
		expect(tracks).toEqual([{ prop: 'pos_x', keys: [key(0, 0), key(2, 0.3)] }]);
		const after = clip({ keyframes: c.keyframes });
		setPropertyKeys(after, 'pos_x', tracks[0].keys);
		expect(propertyKeys(after, 'pos_x').map((k) => k.value)).toEqual([0, 0.3]);
		expect(propertyKeys(after, 'pos_y').map((k) => k.value)).toEqual([0, -0.1]);
		expect(after.keyframes).toHaveLength(2);
	});

	test('withTracks is the clip as the edit leaves it, and leaves the clip alone', () => {
		const c = moving();
		const before = JSON.stringify(c);
		const tracks = moveKeyTracks(c, 4, { pos_x: 0.5, pos_y: 0.5 }, keyTimes(c, 4));
		const drawn = withTracks(c, tracks);
		expect(propertyAt(drawn, 'pos_x', 4)).toBe(0.5);
		expect(propertyAt(drawn, 'pos_y', 4)).toBe(0.5);
		expect(drawn.channels).toHaveLength(2);
		expect(JSON.stringify(c)).toBe(before);
		const only = withTracks(c, [tracks[0]]);
		expect(propertyAt(only, 'pos_y', 4)).toBe(0.2);
	});

	test('the written tracks are what a real edit would leave (the harness applies them the same way)', () => {
		const c = moving();
		const tracks = moveKeyTracks(c, 1, { pos_x: 0.05, pos_y: -0.3 }, keyTimes(c, 4));
		const real = moving();
		for (const t of tracks) setPropertyKeys(real, t.prop, t.keys);
		const drawn = withTracks(c, tracks);
		for (const t of [0, 0.3, 1, 1.5, 2, 3, 4]) {
			expect(propertyAt(drawn, 'pos_x', t)).toBe(propertyAt(real, 'pos_x', t));
			expect(propertyAt(drawn, 'pos_y', t)).toBe(propertyAt(real, 'pos_y', t));
		}
	});
});

describe('dragging a dot', () => {
	const c = moving();
	const keys = pathKeys(c, frame, 4);
	const times = keyTimes(c, 4);

	test('by a pixel delta, the position moves by the same fraction of the frame', () => {
		const d = dragKey(c, keys[2], frame, 192, -108, noSnap, false, times);
		expect(d.pos.pos_x).toBeCloseTo(0.1 + 0.1, 4);
		expect(d.pos.pos_y).toBeCloseTo(propertyAt(c, 'pos_y', 2) - 0.1, 4);
		expect(d.tracks.map((t) => t.prop)).toEqual(['pos_x', 'pos_y']);
		expect(d.guides).toEqual([]);
	});

	test('Shift keeps to the axis it moved most along and writes no key for the other', () => {
		const d = dragKey(c, keys[0], frame, 200, 30, noSnap, true, times);
		expect(d.tracks.map((t) => t.prop)).toEqual(['pos_x']);
		expect(d.pos.pos_y).toBe(keys[0].pos_y);
		const v = dragKey(c, keys[0], frame, 30, -200, noSnap, true, times);
		expect(v.tracks.map((t) => t.prop)).toEqual(['pos_y']);
	});

	test('a drag back to where it started writes nothing', () => {
		expect(dragKey(c, keys[3], frame, 0, 0, noSnap, false, times).tracks).toEqual([]);
		expect(dragKey(c, keys[3], frame, 0.01, -0.01, noSnap, false, times).tracks).toEqual([]);
	});

	test('the dot snaps onto the frame’s centre lines and the other pictures within reach, and says to what', () => {
		const snap: Snapping = { on: true, targets: snapTargets(canvas, [{ cx: 400, cy: 300, w: 200, h: 100, angle: 0 }], null), threshold: 8 };
		const key0 = keys[0];
		const toCentre = dragKey(c, key0, frame, 960 - key0.x + 3, 0, snap, false, times);
		expect(toCentre.pos.pos_x).toBeCloseTo(0, 9);
		expect(toCentre.guides).toContainEqual({ axis: 'x', at: 960 });
		const toEdge = dragKey(c, key0, frame, 500 - key0.x + 5, 0, snap, false, times);
		expect(toEdge.guides).toContainEqual({ axis: 'x', at: 500 });
		const free = dragKey(c, key0, frame, 960 - key0.x + 30, 0, snap, false, times);
		expect(free.guides.filter((g) => g.axis === 'x')).toEqual([]);
	});

	test('a dot snapped onto a centre line is written as the position 0, whatever the pixel rounding at the playhead', () => {
		const shifted: PathFrame = { canvas, shift: { x: -1, y: 1 } };
		const there = pathKeys(c, shifted, 4)[2];
		const snap: Snapping = { on: true, targets: snapTargets(canvas, [], null), threshold: 8 };
		const d = dragKey(c, there, shifted, 960 - 1920 * (0.5 + there.pos_x) + 2, 540 - 1080 * (0.5 + there.pos_y) - 3, snap, false, times);
		expect(d.pos).toEqual({ pos_x: 0, pos_y: 0 });
		expect(d.guides).toContainEqual({ axis: 'x', at: 960 });
		expect(d.guides).toContainEqual({ axis: 'y', at: 540 });
	});

	test('snapping off leaves the dot where the pointer put it', () => {
		const snap: Snapping = { on: false, targets: snapTargets(canvas, [], null), threshold: 8 };
		const d = dragKey(c, keys[0], frame, 960 - keys[0].x + 3, 0, snap, false, times);
		expect(d.pos.pos_x).not.toBeCloseTo(0, 6);
		expect(d.guides).toEqual([]);
	});

	test('the draft the drag draws is the path an edit would leave', () => {
		const d = dragKey(c, keys[1], frame, 96, 54, noSnap, false, times);
		const drawn = withTracks(c, d.tracks);
		const after = pathKeys(drawn, frame, 4)[1];
		expect(after.x).toBeCloseTo(keys[1].x + 96, 0);
		expect(after.y).toBeCloseTo(keys[1].y + 54, 0);
		const s = samplePath(drawn, frame, 4, FPS);
		expect(s.find((p) => p.t === 1)!.x).toBeCloseTo(after.x, 9);
		expect(pathAt(drawn, frame)(0)).toEqual(pathAt(c, frame)(0));
	});
});

describe('adding a key', () => {
	test('every keyed number gets one at the moment, at the value its curve has there', () => {
		const c = moving();
		const tracks = addKeyTracks(c, 3);
		expect(tracks.map((t) => t.prop)).toEqual(['pos_x', 'pos_y']);
		const drawn = withTracks(c, tracks);
		for (const t of [0, 0.7, 1.5, 2.5, 3, 3.5, 4]) {
			expect(propertyAt(drawn, 'pos_x', t)).toBeCloseTo(propertyAt(c, 'pos_x', t), 12);
			expect(propertyAt(drawn, 'pos_y', t)).toBeCloseTo(propertyAt(c, 'pos_y', t), 12);
		}
		expect(keyTimes(drawn, 4)).toEqual([0, 1, 2, 3, 4]);
	});

	test('a number that has a key there already is left alone, and one nobody keys is not keyed', () => {
		const c = moving();
		expect(addKeyTracks(c, 2).map((t) => t.prop)).toEqual(['pos_y']);
		expect(addKeyTracks(c, 4)).toEqual([]);
		const horizontal = clip({ channels: [{ prop: 'pos_x', keys: [key(0, 0), key(2, 0.2)] }] });
		expect(addKeyTracks(horizontal, 1).map((t) => t.prop)).toEqual(['pos_x']);
		expect(addKeyTracks(clip(), 1)).toEqual([]);
	});

	test('a bundle-driven number is taken over by a track holding its keys and the new one', () => {
		const c = clip({ keyframes: [bundle(0, 0, 0), bundle(2, 0.2, -0.1)] });
		const tracks = addKeyTracks(c, 1);
		expect(tracks.map((t) => t.prop)).toEqual(['pos_x', 'pos_y']);
		expect(tracks[0].keys.map((k) => [k.time, k.value])).toEqual([[0, 0], [1, 0.1], [2, 0.2]]);
	});
});

describe('the view toggle is remembered', () => {
	const data = new Map<string, string>();
	const fake = (throws = false) => {
		(globalThis as unknown as { localStorage: Storage }).localStorage = {
			getItem: (k: string) => {
				if (throws) throw new Error('blocked');
				return data.get(k) ?? null;
			},
			setItem: (k: string, v: string) => {
				if (throws) throw new Error('blocked');
				data.set(k, v);
			}
		} as unknown as Storage;
	};
	afterEach(() => {
		data.clear();
		delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
	});

	test('on until it is turned off', () => {
		fake();
		expect(loadMotionPath()).toBe(true);
		saveMotionPath(false);
		expect(data.get(MOTION_PATH_KEY)).toBe('0');
		expect(loadMotionPath()).toBe(false);
		saveMotionPath(true);
		expect(loadMotionPath()).toBe(true);
	});

	test('a blocked or missing store means on, and never throws', () => {
		fake(true);
		expect(loadMotionPath()).toBe(true);
		expect(() => saveMotionPath(false)).not.toThrow();
		delete (globalThis as unknown as { localStorage?: unknown }).localStorage;
		expect(loadMotionPath()).toBe(true);
		expect(() => saveMotionPath(false)).not.toThrow();
	});
});
