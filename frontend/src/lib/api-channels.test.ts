import { beforeEach, describe, expect, test } from 'bun:test';
import {
	addKeyframe,
	clearKeyframes,
	copyKeyframes,
	cutClipRange,
	revertTo,
	setFade,
	setKeyframeEasing,
	setKeyframes,
	setPropertyKeyframes,
	splitClip
} from './api';
import { colorAt, propertyKeys, transformAt, volumeAt } from './channels';
import type { Clip, Keyframe, PropertyKey, Timeline } from './types';

// The browser harness's answers must hold the contract the backend does (`set_property_keyframes`,
// `copy_keyframes`, `set_keyframe_easing` with a property in project.rs), or the editor would be
// built against behaviour the desktop app never shows.

const clipOf = (t: Timeline, id: string): Clip => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === id)!;
const k = (time: number, value: number, easing?: PropertyKey['easing']): PropertyKey => ({ time, value, ...(easing ? { easing } : {}) });

beforeEach(async () => {
	await revertTo(0);
	await clearKeyframes('c1');
});

describe('property keyframes (browser harness)', () => {
	test('a number gets keys of its own and loses them with none', async () => {
		const t = await setPropertyKeyframes('c1', 'brightness', [k(4, 0.5), k(1, -0.5, 'hold')]);
		const c = clipOf(t, 'c1');
		expect(propertyKeys(c, 'brightness').map((x) => [x.time, x.easing ?? 'linear'])).toEqual([
			[1, 'hold'],
			[4, 'linear']
		]);
		expect(colorAt(c, 2).brightness).toBe(-0.5);
		const off = await setPropertyKeyframes('c1', 'brightness', []);
		expect(clipOf(off, 'c1').channels).toBeUndefined();
	});

	test('a refused list writes nothing', async () => {
		await setPropertyKeyframes('c1', 'volume', [k(0, 1), k(2, 0.5)]);
		for (const [prop, keys, why] of [
			['opacity', [k(0, 1.5)], 'within 0..=1'],
			['scale', [k(0, 0)], '> 0'],
			['volume', [k(0, 40)], 'within 0..=4'],
			['pos_x', [k(-1, 0)], 'must be >= 0'],
			['pos_x', [k(0, 0, { bezier: { x1: 0.5, y1: 1.5, x2: 0.5, y2: 0.5 } })], 'no overshoot']
		] as const) {
			await expect(setPropertyKeyframes('c1', prop, [...keys])).rejects.toThrow(why);
		}
		const t = await addKeyframe('c1', 3, { scale: 1 });
		expect(propertyKeys(clipOf(t, 'c1'), 'volume').length).toBe(2);
	});

	test('keying one transform number off the bundle leaves the others, and add_keyframe feeds its track', async () => {
		await addKeyframe('c1', 0, { scale: 1, opacity: 1 });
		await addKeyframe('c1', 4, { scale: 2, opacity: 0 });
		let t = await setPropertyKeyframes('c1', 'opacity', [k(0, 1), k(2, 0.25), k(4, 1)]);
		let c = clipOf(t, 'c1');
		expect(c.keyframes?.length).toBe(2);
		expect(transformAt(c, 2).opacity).toBeCloseTo(0.25, 12);
		expect(transformAt(c, 2).scale).toBeCloseTo(1.5, 12);
		t = await addKeyframe('c1', 3, { opacity: 0.9 });
		c = clipOf(t, 'c1');
		expect(c.keyframes?.length).toBe(3);
		expect(transformAt(c, 3).opacity).toBeCloseTo(0.9, 12);
		expect(c.channels?.find((x) => x.prop === 'opacity')?.keys.length).toBe(4);
		// The easing of the transform key reaches the bundle and the track; one number's alone.
		t = await setKeyframeEasing('c1', 0, 'hold');
		c = clipOf(t, 'c1');
		expect(c.keyframes?.[0].easing).toBe('hold');
		expect(propertyKeys(c, 'opacity')[0].easing).toBe('hold');
		t = await setKeyframeEasing('c1', 0, 'ease_in', 'scale');
		c = clipOf(t, 'c1');
		expect(c.keyframes?.[0].easing).toBe('hold');
		expect(propertyKeys(c, 'scale')[0].easing).toBe('ease_in');
		await expect(setKeyframeEasing('c1', 0, 'hold', 'volume')).rejects.toThrow('no volume keyframe');
		await expect(setKeyframeEasing('c1', 8, 'hold')).rejects.toThrow('no keyframe');
		// Clearing the transform takes the tracks of its numbers and leaves the colour.
		await setPropertyKeyframes('c1', 'gamma', [k(0, 1), k(1, 2)]);
		t = await clearKeyframes('c1');
		c = clipOf(t, 'c1');
		expect(c.keyframes).toEqual([]);
		expect(c.channels?.map((x) => x.prop)).toEqual(['gamma']);
	});

	test('copying moves an animation to another clip, shifted', async () => {
		await setPropertyKeyframes('c1', 'volume', [k(1, 0), k(3, 2)]);
		await setPropertyKeyframes('c3', 'volume', [k(0, 1), k(5, 1)]);
		let t = await copyKeyframes('c1', 'c3', ['volume'], 0.5);
		expect(propertyKeys(clipOf(t, 'c3'), 'volume').map((x) => [x.time, x.value])).toEqual([
			[1.5, 0],
			[3.5, 2]
		]);
		await expect(copyKeyframes('c1', 'c1')).rejects.toThrow('same clip');
		await expect(copyKeyframes('c1', 'c3', ['gamma'])).rejects.toThrow('no gamma keyframes');
		await expect(copyKeyframes('c1', 'c3', [], Number.NaN)).rejects.toThrow('offset');
		await expect(copyKeyframes('nope', 'c3')).rejects.toThrow('not found');
		await expect(copyKeyframes('c3', 'c1', ['temperature'])).rejects.toThrow('no temperature keyframes');
		t = await copyKeyframes('c1', 'c3', [], -2);
		expect(volumeAt(clipOf(t, 'c3'), 0)).toBe(1);
	});

	test('a split carries the animation into the right half', async () => {
		const before = clipOf(await setPropertyKeyframes('c1', 'volume', [k(0, 0), k(4, 2)]), 'c1');
		const dur = (before.source_out - before.source_in) / Math.abs(before.speed ?? 1);
		const at = before.timeline_start + dur / 2;
		const t = await splitClip('c1', at);
		const clips = t.tracks.flatMap((tr) => tr.clips).filter((c) => c.asset_id === before.asset_id && c.id !== 'c3');
		const right = clips.find((c) => c.timeline_start === at);
		expect(right).toBeDefined();
		expect(volumeAt(right!, 0.25)).toBeCloseTo(volumeAt(before, dur / 2 + 0.25), 9);
	});

	test('set_keyframes clears the bundle only and drops a track that was only holding a number off it', async () => {
		const key = (time: number, scale: number): Keyframe => ({ time, scale, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 });
		await setKeyframes('c1', [key(0, 1), key(4, 2)]);
		await setPropertyKeyframes('c1', 'opacity', []);
		let t = await setPropertyKeyframes('c1', 'scale', [k(0, 1), k(4, 3)]);
		expect(clipOf(t, 'c1').channels?.length).toBe(2);
		t = await setKeyframes('c1', []);
		const c = clipOf(t, 'c1');
		expect(c.keyframes).toEqual([]);
		// The scale's own keys survive (the clip is still animated); the empty opacity track is gone.
		expect(c.channels?.map((x) => x.prop)).toEqual(['scale']);
		expect(transformAt(c, 2).scale).toBe(2);
		// clear_keyframes makes the whole transform static.
		expect(clipOf(await clearKeyframes('c1'), 'c1').channels).toBeUndefined();
	});

	test('a split keeps each fade on the half that holds its edge', async () => {
		await setFade('c1', 1, 2);
		const before = clipOf(await setPropertyKeyframes('c1', 'volume', [k(0, 0), k(4, 2)]), 'c1');
		const dur = (before.source_out - before.source_in) / Math.abs(before.speed ?? 1);
		const at = before.timeline_start + dur / 2;
		const t = await splitClip('c1', at, false);
		const halves = t.tracks.flatMap((tr) => tr.clips).filter((c) => c.asset_id === before.asset_id && c.id !== 'c3');
		const left = halves.find((c) => c.id === 'c1')!;
		const right = halves.find((c) => c.timeline_start === at)!;
		expect([left.fade_in, left.fade_out]).toEqual([1, 0]);
		expect([right.fade_in, right.fade_out]).toEqual([0, 2]);
	});

	test('a cut range tail opens on the pose the clip had there', async () => {
		const before = clipOf(await setPropertyKeyframes('c1', 'volume', [k(0, 0.2, 'hold'), k(5, 1)]), 'c1');
		const mag = Math.abs(before.speed ?? 1);
		const from = before.source_in + 2 * mag;
		const to = before.source_in + 4 * mag;
		const t = await cutClipRange('c1', from, to, false);
		const tail = t.tracks
			.flatMap((tr) => tr.clips)
			.find((c) => c.id !== 'c1' && c.id !== 'c3' && c.asset_id === before.asset_id && c.timeline_start === before.timeline_start + 2);
		expect(tail).toBeDefined();
		// The tail starts 4 s into the whole clip's animation, not at its first key.
		for (const local of [0, 0.5, 1, 2.5]) {
			expect(volumeAt(tail!, local)).toBeCloseTo(volumeAt(before, local + 4), 9);
		}
	});
});
