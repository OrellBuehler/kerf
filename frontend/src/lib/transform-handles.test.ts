import { describe, expect, test } from 'bun:test';
import {
	changed,
	contains,
	croppable,
	cropPose,
	geometryOf,
	layersAt,
	movePose,
	pictureBox,
	readout,
	renderCanvas,
	resizeCursor,
	rotatePose,
	scalePose,
	snapTargets,
	sweep,
	toBase,
	topLayerAt,
	type Canvas,
	type Snapping
} from './transform-handles';
import { DEFAULT_TRANSFORM, type Asset, type Clip, type Timeline, type Track, type Transform } from './types';

const asset = (id: string, width: number, height: number, pix_fmt = 'yuv420p'): Asset => ({
	id,
	path: `/${id}.mp4`,
	name: `${id}.mp4`,
	duration: 60,
	imported_at: '',
	streams: [{ index: 0, kind: 'video', codec: 'h264', width, height, pix_fmt }]
});

const clip = (id: string, assetId: string, start = 0, transform: Partial<Transform> = {}): Clip => ({
	id,
	asset_id: assetId,
	source_in: 0,
	source_out: 10,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	transform: { ...DEFAULT_TRANSFORM, ...transform }
});

const track = (id: string, clips: Clip[], extra: Partial<Track> = {}): Track => ({ id, kind: 'video', name: id, clips, ...extra });

const tf = (t: Partial<Transform> = {}): Transform => ({ ...DEFAULT_TRANSFORM, ...t });
const off: Snapping = { on: false, targets: { xs: [], ys: [] }, threshold: 0 };
const W169: Canvas = { width: 1920, height: 1080, fit: 'contain' };

describe('renderCanvas follows export_format', () => {
	test('the first picture decides, a delivery frame overrides, sides are even', () => {
		const assets = [asset('a', 1281, 721), asset('b', 640, 480)];
		const tl: Timeline = { tracks: [track('v1', [clip('c1', 'a')]), track('v2', [clip('c2', 'b')])] };
		expect(renderCanvas(tl, assets)).toEqual({ width: 1280, height: 720, fit: 'contain' });
		expect(renderCanvas({ ...tl, format: { width: 1080, height: 1920, fit: 'cover' } }, assets)).toEqual({ width: 1080, height: 1920, fit: 'cover' });
		const muted: Timeline = { tracks: [track('v1', [clip('c1', 'a')], { muted: true }), track('v2', [clip('c2', 'b')])] };
		expect(renderCanvas(muted, assets)).toMatchObject({ width: 640, height: 480 });
		expect(renderCanvas({ tracks: [] }, assets)).toMatchObject({ width: 1920, height: 1080 });
	});
});

describe('the picture box is where the geometry puts it', () => {
	test('an identity layer in a letterbox is the letterboxed picture', () => {
		const g = geometryOf([640, 360], [1, 1], { width: 360, height: 640, fit: 'contain' }, tf())!;
		expect(pictureBox(g)).toEqual({ cx: 180, cy: 218 + 101, w: 360, h: 202, angle: 0 });
	});

	test('a picture-in-picture is centred in its overlaid layer, on overlay`s rounding', () => {
		const g = geometryOf([640, 360], [1, 1], { width: 640, height: 360, fit: 'contain' }, tf({ scale: 0.5, pos_x: 0.1 }))!;
		expect(g.origin).toEqual([224, 90]);
		expect(pictureBox(g)).toEqual({ cx: 384, cy: 180, w: 320, h: 180, angle: 0 });
	});

	test('a translucent picture of odd size keeps the box it has opaque', () => {
		const canvas = { width: 1279, height: 721, fit: 'contain' as const };
		const opaque = geometryOf([720, 1280], [1, 1], canvas, tf({ scale: 0.5 }))!;
		const translucent = geometryOf([720, 1280], [1, 1], canvas, tf({ scale: 0.5, opacity: 0.5 }));
		expect(translucent).not.toBeNull();
		expect(pictureBox(translucent!)).toEqual(pictureBox(opaque));
		expect(geometryOf([720, 1280], [1, 1], canvas, tf({ crop_left: 0.6, crop_right: 0.6, opacity: 0.5 }))).toBeNull();
	});

	test('a turned picture keeps its own size and turns about its centre', () => {
		const g = geometryOf([640, 360], [1, 1], { width: 640, height: 360, fit: 'contain' }, tf({ rotation: 90 }))!;
		const b = pictureBox(g);
		expect([b.w, b.h, b.angle]).toEqual([640, 360, Math.PI / 2]);
		expect(Math.abs(b.cx - 320)).toBeLessThanOrEqual(1);
		expect(contains(b, 320, 10)).toBe(true);
		expect(contains(b, 10, 180)).toBe(false);
	});
});

describe('layers at the playhead', () => {
	const assets = [asset('a', 1920, 1080), asset('b', 1920, 1080)];
	const tl: Timeline = {
		tracks: [track('v1', [clip('base', 'a')]), track('v2', [clip('pip', 'b', 0, { scale: 0.25, pos_x: 0.3, pos_y: 0.3 })]), track('v3', [clip('later', 'a', 20)])]
	};
	const layers = layersAt(tl, assets, W169, 1);

	test('bottom to top, only what is on screen', () => {
		expect(layers.map((l) => l.clip.id)).toEqual(['base', 'pip']);
	});

	test('a click picks the topmost picture under it, passing over a locked one', () => {
		const pip = layers[1].box;
		expect(topLayerAt(layers, pip.cx, pip.cy)?.clip.id).toBe('pip');
		expect(topLayerAt(layers, 100, 100)?.clip.id).toBe('base');
		expect(topLayerAt(layers, pip.cx, pip.cy, (c) => c.id === 'pip')?.clip.id).toBe('base');
	});

	test('a keyed clip is placed at its pose at the time', () => {
		const keyed: Clip = { ...clip('k', 'a'), keyframes: [{ time: 0, scale: 1, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 }, { time: 2, scale: 0.5, pos_x: 0.2, pos_y: 0, rotation: 0, opacity: 1 }] };
		const [l] = layersAt({ tracks: [track('v1', [keyed])] }, assets, W169, 1);
		expect([l.pose.scale, l.pose.pos_x, l.local]).toEqual([0.75, 0.1, 1]);
	});
});

describe('move', () => {
	const box = { cx: 960, cy: 540, w: 480, h: 270, angle: 0 };
	const snap: Snapping = { on: true, targets: snapTargets(W169, [], null), threshold: 8 };

	test('a pixel delta is a position delta of the frame', () => {
		expect(movePose(tf(), box, 192, -108, W169, off).patch).toEqual({ pos_x: 0.1, pos_y: -0.1 });
	});

	test('Shift keeps it on the axis it moved most along', () => {
		expect(movePose(tf(), box, 192, 30, W169, off, true).patch).toEqual({ pos_x: 0.1, pos_y: 0 });
	});

	test('an edge or the centre stops on the frame`s lines', () => {
		// The left edge 5 px from the frame's left edge.
		const m = movePose(tf(), box, -715, 3, W169, snap);
		expect(m.patch.pos_x).toBe(-0.375);
		expect(m.patch.pos_y).toBe(0);
		expect(m.guides).toEqual([
			{ axis: 'x', at: 0 },
			{ axis: 'y', at: 540 }
		]);
		expect(movePose(tf(), box, -715, 3, W169, { ...snap, on: false }).guides).toEqual([]);
	});

	test('other pictures and the safe areas are targets', () => {
		const t = snapTargets({ width: 1080, height: 1920, fit: 'cover' }, [{ cx: 300, cy: 300, w: 100, h: 100, angle: 0 }], { top: 0.08, bottom: 0.2, right: 0.14 });
		expect(t.xs).toContain(250);
		expect(t.ys).toContain(1920 * 0.08);
		expect(t.ys).toContain(1920 * 0.8);
		expect(t.xs).toContain(1080 * 0.86);
	});
});

describe('scale', () => {
	const box = { cx: 960, cy: 540, w: 960, h: 540, angle: 0 };
	test('the distance from the centre scales it', () => {
		expect(scalePose(tf({ scale: 0.5 }), box, [1200, 540], [1440, 540], off).patch).toEqual({ scale: 1 });
	});

	test('it stops at its fitted size, and an edge stops on the frame`s', () => {
		const snap: Snapping = { on: true, targets: { xs: [], ys: [] }, threshold: 8 };
		expect(scalePose(tf({ scale: 0.5 }), box, [1200, 540], [1437, 540], snap).patch).toEqual({ scale: 1 });
		const edges: Snapping = { on: true, targets: { xs: [1700], ys: [] }, threshold: 8 };
		const m = scalePose(tf({ scale: 0.5 }), box, [1440, 540], [1695, 540], edges);
		expect(m.patch.scale).toBeCloseTo(0.5 * (740 / 480), 4);
		expect(m.guides).toEqual([{ axis: 'x', at: 1700 }]);
	});
});

describe('rotate', () => {
	const box = { cx: 0, cy: 0, w: 10, h: 10, angle: 0 };
	test('the sweep goes the short way, so a gesture can add it up past a half turn', () => {
		expect(sweep(box, [10, 0], [0, 10])).toBeCloseTo(90);
		expect(sweep(box, [-10, 1], [-10, -1])).toBeCloseTo(11.42, 1);
	});

	test('a right angle catches within 5°, Shift turns in 15° steps', () => {
		expect(rotatePose(tf(), 86, true).patch).toEqual({ rotation: 90 });
		expect(rotatePose(tf(), 84, true).patch).toEqual({ rotation: 84 });
		expect(rotatePose(tf(), 86, false).patch).toEqual({ rotation: 86 });
		expect(rotatePose(tf({ rotation: 10 }), 26, true, true).patch).toEqual({ rotation: 30 });
		expect(rotatePose(tf({ rotation: 180 }), 183, true).patch).toEqual({ rotation: 360 });
	});
});

describe('crop keeps the rest of the picture where it was', () => {
	const edgesOf = (canvas: Canvas, src: [number, number], pose: Transform) => {
		const b = pictureBox(geometryOf(src, [1, 1], canvas, pose)!);
		return { left: b.cx - b.w / 2, right: b.cx + b.w / 2, top: b.cy - b.h / 2, bottom: b.cy + b.h / 2 };
	};
	const lay = (canvas: Canvas, src: [number, number], pose: Transform) => {
		const geometry = geometryOf(src, [1, 1], canvas, pose)!;
		return { src, geometry, box: pictureBox(geometry) };
	};

	test('the right edge of a full-frame picture', () => {
		const m = cropPose(tf(), lay(W169, [1920, 1080], tf()), 'right', [1536, 500], W169, off);
		expect(m.patch).toEqual({ crop_right: 0.2, scale: 1, pos_x: -0.1, pos_y: 0 });
		const e = edgesOf(W169, [1920, 1080], tf(m.patch));
		expect(Math.abs(e.right - 1536)).toBeLessThanOrEqual(1);
		expect(Math.abs(e.left - 0)).toBeLessThanOrEqual(1);
	});

	test('a width-bound picture is scaled back so its content does not grow', () => {
		const tall: Canvas = { width: 1080, height: 1920, fit: 'contain' };
		const before = edgesOf(tall, [1920, 1080], tf());
		const m = cropPose(tf(), lay(tall, [1920, 1080], tf()), 'left', [270, 960], tall, off);
		expect(m.patch.crop_left).toBe(0.25);
		expect(m.patch.scale).toBe(0.75);
		const e = edgesOf(tall, [1920, 1080], tf(m.patch));
		expect(Math.abs(e.left - 270)).toBeLessThanOrEqual(1);
		expect(Math.abs(e.right - before.right)).toBeLessThanOrEqual(1);
		expect(Math.abs(e.top - before.top)).toBeLessThanOrEqual(1);
	});

	test('a turned picture crops along its own edge', () => {
		const start = tf({ scale: 0.5, rotation: 90 });
		const l = lay(W169, [1920, 1080], start);
		// Turned a quarter clockwise, the picture's top edge faces right.
		const m = cropPose(start, l, 'top', [l.box.cx + 100, l.box.cy], W169, off);
		const after = pictureBox(geometryOf([1920, 1080], [1, 1], W169, tf({ ...start, ...m.patch }))!);
		// The far (bottom) edge stays: it faces left, at cx - h/2 (to `overlay`'s even pixel).
		expect(Math.abs(after.cx - after.h / 2 - (l.box.cx - l.box.h / 2))).toBeLessThanOrEqual(2);
		expect(Math.abs(after.cx + after.h / 2 - (l.box.cx + 100))).toBeLessThanOrEqual(2);
	});

	test('a crop never takes the whole picture', () => {
		const m = cropPose(tf(), lay(W169, [1920, 1080], tf()), 'right', [-500, 500], W169, off);
		expect(m.patch.crop_right).toBe(0.98);
	});

	test('only a contain fit with an unkeyed placement is cropped in place', () => {
		const c = clip('c', 'a');
		expect(croppable(c, W169)).toBe(true);
		expect(croppable(c, { ...W169, fit: 'cover' })).toBe(false);
		expect(croppable(c, { ...W169, fit: 'blur' })).toBe(true);
		expect(croppable({ ...c, channels: [{ prop: 'pos_x', keys: [{ time: 0, value: 0 }] }] }, W169)).toBe(false);
		expect(croppable({ ...c, channels: [{ prop: 'opacity', keys: [{ time: 0, value: 1 }] }] }, W169)).toBe(true);
	});
});

test('a gesture that changes nothing writes nothing', () => {
	expect(changed(tf({ pos_x: 0.1 }), { pos_x: 0.1, pos_y: 0 })).toEqual({});
	expect(changed(tf(), { pos_x: 0.1, pos_y: 0 })).toEqual({ pos_x: 0.1 });
});

test('a number the gesture left alone is not written, though the start had more decimals', () => {
	const start = tf({ pos_x: 0.123456, pos_y: -0.0987654, rotation: 12.3456 });
	const box = { cx: 960, cy: 540, w: 480, h: 270, angle: 0 };
	const shifted = movePose(start, box, 192, 30, W169, off, true).patch;
	expect(changed(start, shifted)).toEqual({ pos_x: 0.2235 });
	expect(changed(start, movePose(start, box, 0, 0, W169, off).patch)).toEqual({});
	expect(changed(start, { rotation: 12.35 })).toEqual({});
});

test('cursors turn with the box', () => {
	expect(resizeCursor(0, 0)).toBe('ew-resize');
	expect(resizeCursor(45, 0)).toBe('nwse-resize');
	expect(resizeCursor(0, Math.PI / 2)).toBe('ns-resize');
	expect(resizeCursor(135, 0)).toBe('nesw-resize');
});

test('readouts', () => {
	expect(readout('move', tf({ pos_x: 0.125, pos_y: -0.05 }))).toBe('x 12.5% · y -5%');
	expect(readout('rotate', tf({ rotation: 44.96 }))).toBe('45°');
	expect(readout('crop', tf({ crop_right: 0.2 }), 'right')).toBe('crop right 20%');
});

describe('live behaviours: the box is at the picture, a drag writes the pose', () => {
	const assets = [asset('a', 1920, 1080)];
	// A quarter-turn of a 1 Hz sway at 0.25 s: the picture is 5° off the pose it rides on.
	const swaying: Clip = {
		...clip('s', 'a', 0, { scale: 0.5, pos_x: 0.1, rotation: 10 }),
		behaviors: [
			{ kind: 'sway', amount: 5, speed: 1 },
			{ kind: 'breathe', amount: 0.2, speed: 1 },
			{ kind: 'drift', speed: 0.1, angle: 90 }
		]
	};
	const at = 0.25;
	const [layer] = layersAt({ tracks: [track('v1', [swaying])] }, assets, W169, at);

	test('the layer has the picture and the pose it rides on', () => {
		expect(layer.base).toMatchObject({ scale: 0.5, pos_x: 0.1, pos_y: 0, rotation: 10 });
		expect(layer.pose.rotation).toBeCloseTo(15, 9);
		expect(layer.pose.scale).toBeCloseTo(0.5 * 1.2, 9);
		expect(layer.pose.pos_y).toBeCloseTo(0.025, 9);
		expect(layer.pose.pos_x).toBeCloseTo(0.1, 9);
		expect(layer.box.angle).toBeCloseTo((15 * Math.PI) / 180, 9);
		const plain = layersAt({ tracks: [track('v1', [clip('p', 'a', 0, { scale: 0.5, pos_x: 0.1, rotation: 10 })])] }, assets, W169, at)[0];
		expect(plain.base).toEqual(plain.pose);
	});

	test('what is written puts the picture where the pointer took it, the behaviours on top', () => {
		// Drag the picture 96 px right and 54 px down: the position moves by a twentieth of the frame each way.
		const moved = movePose(layer.pose, layer.box, 96, 54, W169, off).patch;
		expect(moved.pos_x).toBeCloseTo(0.15, 4);
		const written = toBase(layer, moved);
		expect(written.pos_x).toBeCloseTo(0.15, 4);
		expect(written.pos_y).toBeCloseTo(0.05, 4);
		const after: Clip = { ...swaying, transform: { ...swaying.transform!, ...written } };
		const [next] = layersAt({ tracks: [track('v1', [after])] }, assets, W169, at);
		expect(next.pose.pos_x).toBeCloseTo(moved.pos_x!, 3);
		expect(next.pose.pos_y).toBeCloseTo(moved.pos_y!, 3);
		// A turn and a zoom the same way.
		const turned = toBase(layer, rotatePose(layer.pose, 20, false).patch);
		expect(turned.rotation).toBeCloseTo(10 + 20, 2);
		const grown = toBase(layer, { scale: 0.9 });
		expect(grown.scale).toBeCloseTo(0.9 / 1.2, 4);
	});

	test('without behaviours a patch is written as it was', () => {
		const plain = layersAt({ tracks: [track('v1', [clip('p', 'a', 0, { scale: 0.5, pos_x: 0.1 })])] }, assets, W169, at)[0];
		const patch = { pos_x: 0.2, pos_y: -0.05, rotation: 7, scale: 0.7, crop_left: 0.1 };
		expect(toBase(plain, patch)).toEqual(patch);
	});

	test('a clip a behaviour moves is not cropped in place', () => {
		expect(croppable(clip('c', 'a'), W169)).toBe(true);
		expect(croppable({ ...clip('c', 'a'), behaviors: [{ kind: 'sway', amount: 5, speed: 1 }] }, W169)).toBe(false);
		expect(croppable({ ...clip('c', 'a'), behaviors: [{ kind: 'sway', amount: 0, speed: 1 }] }, W169), 'one that moves nothing is not live').toBe(true);
	});
});
