import { describe, expect, test } from 'bun:test';
import {
	channelChanges,
	clampProperty,
	clearTransformAnimation,
	colorAt,
	insertPropertyKey,
	isKeyed,
	propertyAt,
	propertyKeys,
	propertyKeysProblem,
	propertyKeysShifted,
	propertyProblem,
	rebaseChannels,
	rebaseHead,
	setPropertyEasing,
	setPropertyKeys,
	transformAt,
	volumeAt
} from './channels';
import { rebaseAnimation } from './edit-modes';
import { clipById } from './link-groups';
import { cutClipRange, detachAudio, splitClip } from './links';
import type { Clip, Easing, Keyframe, PropertyKey, Timeline } from './types';

const key = (time: number, value: number, easing?: Easing): PropertyKey => ({ time, value, ...(easing ? { easing } : {}) });
const bundle = (time: number, scale: number, opacity: number, easing?: Easing): Keyframe => ({
	time,
	scale,
	pos_x: 0,
	pos_y: 0,
	rotation: 0,
	opacity,
	...(easing ? { easing } : {})
});
const clip = (): Clip => ({
	id: 'c',
	asset_id: 'a',
	source_in: 0,
	source_out: 10,
	timeline_start: 0,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	speed: 1
});

/** The clip `channels.rs`'s `mirrored()` builds. */
function mirrored(): Clip {
	const c = clip();
	c.keyframes = [bundle(0, 1, 1, 'ease_out'), bundle(2, 2, 0.5)];
	setPropertyKeys(c, 'volume', [
		key(0.2, 0.2, 'ease_in'),
		key(1.1, 1.8),
		key(1.7, 0.4, 'hold'),
		key(2.2013, 1.0)
	]);
	setPropertyKeys(c, 'brightness', [
		key(0, -0.3, 'ease_in_out'),
		key(2, 0.4, { bezier: { x1: 0.2, y1: 0.9, x2: 0.3, y2: 1 } }),
		key(3, 0)
	]);
	return c;
}

describe('the numbers kerf-core pins', () => {
	// `channels_match_the_frontend_mirror_bit_for_bit` in model/channels.rs, same values.
	test('samples', () => {
		const c = mirrored();
		for (const [t, want] of [
			[0, 0.2],
			[0.5, 0.449862273587892],
			[1.1, 1.8],
			[1.4, 1.1000000000000005],
			[1.7, 0.4],
			[1.9, 0.4],
			[2.2013, 1.0],
			[3, 1.0]
		]) {
			expect(volumeAt(c, t)).toBe(want);
		}
		for (const [t, want] of [
			[0.25, -0.27559255044876013],
			[0.9, -0.0092929134307706],
			[1.6, 0.3402597151672505],
			[2.5, 0.020021298908220964]
		]) {
			expect(colorAt(c, t).brightness).toBe(want);
		}
		const tf = transformAt(c, 1);
		expect([tf.scale, tf.opacity]).toEqual([1.6846431874269898, 0.6576784062865051]);
	});

	test('a head cut inside an eased segment pins the pose and bakes the rest into plain keys', () => {
		const got = rebaseHead(propertyKeys(mirrored(), 'volume'), 0.9);
		expect(got.map((k) => [k.time, k.value, k.easing ?? 'linear'])).toEqual([
			[0, 1.2578044843619944, 'linear'],
			[0.050000000000000155, 1.3834554717250953, 'linear'],
			[0.1250000000000001, 1.5842902877063407, 'linear'],
			[0.20000000000000007, 1.8, 'linear'],
			[0.7999999999999999, 0.4, 'hold'],
			[1.3013, 1.0, 'linear']
		]);
	});

	test('a key put inside a segment splits it, on a track and off the bundle', () => {
		const c = mirrored();
		insertPropertyKey(c, 'volume', key(0.7, 0.55));
		expect(propertyKeys(c, 'volume').map((k) => [k.time, k.value, k.easing ?? 'linear'])).toEqual([
			[0.2, 0.2, { bezier: { x1: 0.31544631633977743, y1: 0, x2: 0.681034420783558, y2: 0.4617901153530115 } }],
			[0.7, 0.55, { bezier: { x1: 0.5568358766841033, y1: 0.4548965203318149, x2: 1, y2: 1 } }],
			[1.1, 1.8, 'linear'],
			[1.7, 0.4, 'hold'],
			[2.2013, 1.0, 'linear']
		]);
		const d = mirrored();
		insertPropertyKey(d, 'scale', key(1, 1.5));
		expect(propertyKeys(d, 'scale').map((k) => [k.time, k.value, k.easing ?? 'linear'])).toEqual([
			[0, 1, { bezier: { x1: 0, y1: 0, x2: 0.4542081650096481, y2: 0.5719165400747731 } }],
			[1, 1.5, { bezier: { x1: 0.3264332254175324, y1: 0.5558502990076779, x2: 0.6856271141503777, y2: 1 } }],
			[2, 2, 'linear']
		]);
	});
});

describe('the resolver', () => {
	test('an unkeyed property is its static value and a track is read in range', () => {
		const c = clip();
		c.color = { brightness: 0.25, contrast: 1, saturation: 1, gamma: 1, temperature: 0 };
		c.volume = 0.5;
		expect(isKeyed(c, 'brightness')).toBe(false);
		expect(colorAt(c, 3)).toEqual(c.color);
		expect(volumeAt(c, 3)).toBe(0.5);
		c.channels = [{ prop: 'saturation', keys: [key(2, 99), key(0, Number.NaN)] }];
		expect(propertyKeys(c, 'saturation').map((k) => [k.time, k.value])).toEqual([
			[0, 1],
			[2, 3]
		]);
		expect(clampProperty('scale', -3)).toBe(1e-6);
		expect(clampProperty('volume', 40)).toBe(4);
	});

	test('a channel animates one number and leaves the rest alone', () => {
		const c = clip();
		c.transform = { scale: 1.5, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1, crop_left: 0, crop_right: 0, crop_top: 0, crop_bottom: 0 };
		setPropertyKeys(c, 'opacity', [key(0, 0), key(2, 1)]);
		const t = transformAt(c, 1);
		expect([t.opacity, t.scale]).toEqual([0.5, 1.5]);
		expect(isKeyed(c, 'scale')).toBe(false);
	});

	test('a property without a track reads the bundle; an empty track holds it off', () => {
		const c = clip();
		c.keyframes = [bundle(0, 1, 1), bundle(2, 2, 0)];
		expect(propertyAt(c, 'scale', 1)).toBe(1.5);
		setPropertyKeys(c, 'scale', []);
		expect(isKeyed(c, 'scale')).toBe(false);
		expect(propertyAt(c, 'scale', 1)).toBe(1);
		expect(propertyAt(c, 'opacity', 1)).toBe(0.5);
		c.keyframes = [];
		rebaseChannels(c, 0);
		setPropertyKeys(c, 'scale', []);
		expect(c.channels).toBeUndefined();
	});

	test('range checks mirror Project::set_property_keyframes', () => {
		expect(propertyProblem('opacity', 1.5)).toContain('opacity keyframe values must be within 0..=1');
		expect(propertyProblem('scale', 0)).toContain('> 0');
		expect(propertyProblem('pos_x', Infinity)).toContain('finite');
		expect(propertyProblem('gamma', 0.05)).toBeDefined();
		expect(propertyProblem('temperature', -1)).toBeUndefined();
		expect(propertyKeysProblem('volume', [key(-1, 1)])).toBe('keyframe time must be >= 0');
		expect(propertyKeysProblem('volume', [key(0, 1, { bezier: { x1: 0.5, y1: 1.5, x2: 0.5, y2: 0.5 } })])).toContain('no overshoot');
		expect(propertyKeysProblem('volume', Array.from({ length: 1001 }, (_, i) => key(i, 1)))).toContain('at most 1000');
	});
});

describe('the diff', () => {
	// `holding_a_bundle_driven_number_static_is_a_change_and_taking_it_over_unchanged_is_not`
	test('holding a bundle-driven number static is a change; taking it over unchanged is not', () => {
		const before = clip();
		before.keyframes = [bundle(0, 1, 1), bundle(2, 2, 0)];
		const held = structuredClone(before);
		setPropertyKeys(held, 'opacity', []);
		expect(held.channels?.length).toBe(1);
		expect(channelChanges(before, held)).toEqual(['opacity keyframes 2 → 0']);
		expect(channelChanges(held, before)).toEqual(['opacity keyframes 0 → 2']);
		const taken = structuredClone(before);
		setPropertyKeys(taken, 'scale', propertyKeys(before, 'scale'));
		expect(taken.channels?.length).toBe(1);
		expect(channelChanges(before, taken)).toEqual([]);
		const idle = clip();
		idle.channels = [{ prop: 'opacity', keys: [] }];
		expect(channelChanges(clip(), idle)).toEqual([]);
		const retimed = structuredClone(taken);
		retimed.channels![0].keys[1].time = 3;
		expect(channelChanges(taken, retimed)).toEqual(['scale keyframes changed']);
	});
});

describe('edits keep channels where they were', () => {
	/** `edited` plays, `by` seconds in, what `original` did. */
	const sameFrom = (original: Clip, edited: Clip, by: number) => {
		for (const p of ['volume', 'brightness', 'scale', 'opacity'] as const) {
			for (let i = 0; i <= 120; i++) {
				const local = i * 0.025;
				expect(Math.abs(propertyAt(edited, p, local) - propertyAt(original, p, local + by))).toBeLessThan(1e-9);
			}
		}
	};

	test('a head cut keeps every channel exactly where it was', () => {
		for (const by of [0.1, 0.2, 0.9, 1.1, 1.5, 1.7, 2, 2.2013, 3]) {
			const original = mirrored();
			const c = structuredClone(original);
			rebaseAnimation(c, by);
			sameFrom(original, c, by);
		}
	});

	test('pulled earlier, every key shifts later', () => {
		const c = mirrored();
		rebaseAnimation(c, -1.5);
		expect(propertyKeys(c, 'volume')[0].time).toBe(1.7);
		expect(propertyAt(c, 'volume', 0)).toBe(0.2);
		expect(propertyKeys(c, 'scale')[1].time).toBe(3.5);
	});

	test('a split carries the animation into the right half, bundle and channels alike', () => {
		const left = mirrored();
		left.source_out = 10;
		const timeline: Timeline = { tracks: [{ id: 't', kind: 'video', name: 'V1', clips: [left] }], overlays: [], markers: [] } as unknown as Timeline;
		const original = structuredClone(left);
		const [a, b] = splitClip(timeline, left.id, 1.3);
		for (let i = 0; i <= 80; i++) {
			const local = i * 0.025;
			expect(volumeAt(b, local)).toBeCloseTo(volumeAt(original, local + 1.3), 9);
			expect(colorAt(b, local).brightness).toBeCloseTo(colorAt(original, local + 1.3).brightness, 9);
			expect(transformAt(b, local).scale).toBeCloseTo(transformAt(original, local + 1.3).scale, 9);
			expect(volumeAt(a, local)).toBe(volumeAt(original, local));
		}
	});

	test('easing is set on the key that leaves a segment, and a bundle number is taken over', () => {
		const c = clip();
		setPropertyKeys(c, 'contrast', [key(0, 1), key(2, 2)]);
		expect(setPropertyEasing(c, 'contrast', 0.0004, 'hold')).toBe(true);
		expect(propertyAt(c, 'contrast', 1)).toBe(1);
		expect(setPropertyEasing(c, 'contrast', 1, 'hold')).toBe(false);
		expect(setPropertyEasing(c, 'saturation', 0, 'hold')).toBe(false);
		expect(c.channels?.some((t) => t.prop === 'saturation')).toBe(false);
		const b = clip();
		b.keyframes = [bundle(0, 1, 1), bundle(2, 2, 0)];
		expect(setPropertyEasing(b, 'opacity', 0, 'hold')).toBe(true);
		expect(propertyAt(b, 'opacity', 1)).toBe(1);
		expect(b.keyframes[0].easing).toBeUndefined();
		expect(propertyAt(b, 'scale', 1)).toBe(1.5);
	});

	test('clearing the transform takes its tracks and leaves colour and volume', () => {
		const c = mirrored();
		insertPropertyKey(c, 'opacity', key(1, 0.5));
		clearTransformAnimation(c);
		expect(c.keyframes).toEqual([]);
		expect(c.channels?.map((t) => t.prop)).toEqual(['brightness', 'volume']);
	});

	// `a_cut_range_tail_opens_on_the_pose_the_clip_had_there`
	test('a cut range tail opens on the pose the clip had there', () => {
		const build = (c: Clip): Timeline => ({ tracks: [{ id: 't', kind: 'video', name: 'V1', clips: [structuredClone(c)] }] }) as unknown as Timeline;
		const c = clip();
		c.keyframes = [bundle(0, 1, 1, 'ease_in_out'), bundle(6, 2, 0)];
		setPropertyKeys(c, 'brightness', [key(0, -0.5, 'ease_in'), key(8, 0.5)]);
		setPropertyKeys(c, 'volume', [key(0, 0.2, 'hold'), key(5, 1)]);
		const sample = (p: Clip, at: number) => [transformAt(p, at).scale, colorAt(p, at).brightness, volumeAt(p, at)];
		const closeTo = (a: number[], b: number[], what: string) =>
			a.forEach((x, i) => expect(Math.abs(x - b[i]), `${what}: ${a} against ${b}`).toBeLessThan(1e-9));
		// Source 2..4 is cut out of 0..10: the head plays 0..2, the tail 4..10 from 4 s in.
		let t = build(c);
		let [head, tail] = cutClipRange(t, c.id, 2, 4);
		expect([head.timeline_start, tail.timeline_start]).toEqual([0, 2]);
		for (let i = 0; i <= 60; i++) {
			const local = i * 0.05;
			closeTo(sample(head, local), sample(c, local), 'head');
			closeTo(sample(tail, local), sample(c, local + 4), 'tail');
		}
		// A cut at the very head is a head trim of the same amount.
		t = build(c);
		const pieces = cutClipRange(t, c.id, 0, 3);
		expect(pieces.length).toBe(1);
		expect([pieces[0].id, pieces[0].timeline_start]).toEqual([c.id, 0]);
		for (let i = 0; i <= 40; i++) closeTo(sample(pieces[0], i * 0.05), sample(c, i * 0.05 + 3), 'sole tail');
		// Reversed: the tail is the lower span, 6 s of head plus the 2 s removed come first.
		const back = structuredClone(c);
		back.speed = -1;
		t = build(back);
		[head, tail] = cutClipRange(t, back.id, 2, 4);
		for (let i = 0; i <= 40; i++) closeTo(sample(tail, i * 0.05), sample(back, i * 0.05 + 8), 'reversed');
		// At speed 2 the animation is in timeline seconds: the head is 1 s, the cut 1 s.
		const fast = structuredClone(c);
		fast.speed = 2;
		t = build(fast);
		[head, tail] = cutClipRange(t, fast.id, 2, 4);
		for (let i = 0; i <= 40; i++) closeTo(sample(tail, i * 0.05), sample(fast, i * 0.05 + 2), 'fast');
	});

	// `a_keyed_volume_that_the_fader_ratio_would_push_past_the_cap_goes_to_an_equal_fader_lane`
	test('a keyed volume the fader ratio would push past the cap goes to an equal-fader lane', () => {
		const keyed = (top: number) => {
			const c = clip();
			setPropertyKeys(c, 'volume', [key(0, 0.5), key(4, top)]);
			return c;
		};
		const build = (picture: Clip, vFader: number, aFader: number): Timeline =>
			({
				tracks: [
					{ id: 'v', kind: 'video', name: 'V1', volume: vFader, clips: [structuredClone(picture)] },
					{ id: 'a', kind: 'audio', name: 'A1', volume: aFader, clips: [] }
				]
			}) as unknown as Timeline;
		const heard = (t: Timeline, id: string, at: number) => {
			const track = t.tracks.find((x) => x.clips.some((c) => c.id === id))!;
			return (track.volume ?? 1) * volumeAt(clipById(t, id)!, at);
		};
		// Picture fader 4, audio fader 1: 1.5 * 4 is past the cap of 4, so the sound takes a new
		// lane at the picture's fader with its keys as they are, exactly as loud as it was.
		let picture = keyed(1.5);
		let t = build(picture, 4, 1);
		let done = detachAudio(t, picture.id, true);
		expect(done.created_track).toBe(true);
		expect(t.tracks[2].volume).toBe(4);
		expect(propertyKeys(done.clip, 'volume')[1].value).toBe(1.5);
		for (const at of [0, 1, 2.5, 4]) expect(Math.abs(heard(t, done.clip.id, at) - 4 * volumeAt(picture, at))).toBeLessThan(1e-6);
		// Under the cap it takes the ordinary route onto A1, scaled by the ratio: 1.5 * 2 = 3.
		picture = keyed(1.5);
		t = build(picture, 2, 1);
		done = detachAudio(t, picture.id, true);
		expect(done.created_track).toBe(false);
		expect(t.tracks.length).toBe(2);
		expect(propertyKeys(done.clip, 'volume')[1].value).toBe(3);
		for (const at of [0, 2, 4]) expect(Math.abs(heard(t, done.clip.id, at) - 2 * volumeAt(picture, at))).toBeLessThan(1e-6);
		// An existing lane with the picture's fader is used before a new one is made.
		picture = keyed(1.5);
		t = build(picture, 4, 1);
		t.tracks.push({ id: 'a2', kind: 'audio', name: 'A2', volume: 4, clips: [] });
		done = detachAudio(t, picture.id, true);
		expect(done.created_track).toBe(false);
		expect(t.tracks[2].clips.length).toBe(1);
		// A ratio below one never reaches the cap, and a static volume is not capped at all.
		picture = keyed(3);
		expect(detachAudio(build(picture, 1, 2), picture.id, true).created_track).toBe(false);
		const plain = clip();
		plain.volume = 3;
		done = detachAudio(build(plain, 4, 1), plain.id, true);
		expect(done.created_track).toBe(false);
		expect(done.clip.volume).toBe(12);
	});

	test('copying shifts keys, and a negative offset cuts the head', () => {
		const c = clip();
		setPropertyKeys(c, 'volume', [key(1, 0), key(3, 2)]);
		c.keyframes = [bundle(0, 1, 1), bundle(2, 2, 0)];
		const all = propertyKeysShifted(c, [], 0.5);
		expect(all.map((t) => t.prop)).toEqual(['scale', 'pos_x', 'pos_y', 'rotation', 'opacity', 'volume']);
		expect(all.find((t) => t.prop === 'volume')!.keys.map((k) => k.time)).toEqual([1.5, 3.5]);
		const cut = propertyKeysShifted(c, ['volume'], -2);
		expect(cut[0].keys.map((k) => [k.time, k.value])).toEqual([
			[0, 1],
			[1, 2]
		]);
	});

	test('the diff names the property that moved', () => {
		const before = clip();
		const after = structuredClone(before);
		setPropertyKeys(after, 'volume', [key(0, 1), key(2, 0)]);
		expect(channelChanges(before, after)).toEqual(['volume keyframes 0 → 2']);
		const later = structuredClone(after);
		later.channels![0].keys[1].value = 0.5;
		expect(channelChanges(after, later)).toEqual(['volume keyframes changed']);
		const eased = structuredClone(after);
		eased.channels![0].keys[0].easing = 'hold';
		expect(channelChanges(after, eased)).toEqual(['easing changed on 1 volume keyframe']);
	});
});
