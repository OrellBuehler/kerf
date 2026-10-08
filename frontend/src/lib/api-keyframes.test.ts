import { beforeEach, describe, expect, test } from 'bun:test';
import { addKeyframe, clearKeyframes, revertTo, setKeyframeEasing, setKeyframes } from './api';
import type { Keyframe, Timeline } from './types';

// The browser harness's answers must hold the contract the backend does (`add_keyframe`,
// `set_keyframes` in project.rs), or the editor would be built against behaviour the desktop app
// never shows.

const keysOf = (t: Timeline) => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === 'c1')?.keyframes ?? [];
const shapes = (t: Timeline) => keysOf(t).map((k) => [k.time, typeof k.easing === 'object' ? 'curve' : (k.easing ?? 'linear')]);

beforeEach(async () => {
	await revertTo(0);
	await clearKeyframes('c1');
});

describe('keyframes (browser harness)', () => {
	test('a key added inside a hold keeps holding', async () => {
		await addKeyframe('c1', 0, { scale: 1 });
		await addKeyframe('c1', 4, { scale: 3 });
		await setKeyframeEasing('c1', 0, 'hold');
		const t = await addKeyframe('c1', 2, { scale: 1 });
		expect(shapes(t)).toEqual([[0, 'hold'], [2, 'hold'], [4, 'linear']]);
	});

	test('a key added inside a curve splits it into two curves', async () => {
		await addKeyframe('c1', 0, { scale: 1 });
		await addKeyframe('c1', 4, { scale: 3 });
		await setKeyframeEasing('c1', 0, 'ease_in_out');
		const t = await addKeyframe('c1', 1, { scale: 1.2 });
		expect(shapes(t)).toEqual([[0, 'curve'], [1, 'curve'], [4, 'linear']]);
	});

	test('re-keying a moment keeps how it leaves', async () => {
		await addKeyframe('c1', 0, { scale: 1 });
		await addKeyframe('c1', 4, { scale: 3 });
		await setKeyframeEasing('c1', 0, 'ease_out');
		const t = await addKeyframe('c1', 0, { scale: 1.5 });
		expect(keysOf(t)[0]).toMatchObject({ scale: 1.5, easing: 'ease_out' });
	});

	test('set_keyframes refuses a bezier that overshoots, and writes nothing', async () => {
		const key = (time: number, easing?: Keyframe['easing']): Keyframe => ({
			time,
			scale: 1,
			pos_x: 0,
			pos_y: 0,
			rotation: 0,
			opacity: 1,
			...(easing ? { easing } : {})
		});
		await setKeyframes('c1', [key(0, { bezier: { x1: 0.2, y1: 0.9, x2: 0.3, y2: 1 } }), key(2)]);
		await expect(setKeyframes('c1', [key(0, { bezier: { x1: 0.2, y1: 1.4, x2: 0.8, y2: 0.5 } }), key(2)])).rejects.toThrow(
			'no overshoot'
		);
		const t = await addKeyframe('c1', 3, { scale: 1 });
		expect(keysOf(t).length).toBe(3);
		expect(typeof keysOf(t)[0].easing).toBe('object');
	});
});
