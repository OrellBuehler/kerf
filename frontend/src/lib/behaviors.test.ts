import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/behaviors-corpus.json';
import {
	BEHAVIOR_KINDS,
	MAX_BEHAVIORS,
	MAX_BEHAVIOR_HZ,
	MAX_BEHAVIOR_PHASE,
	MAX_BREATHE,
	MAX_DRIFT,
	MAX_SHAKE,
	MAX_SPIN_TURNS,
	MAX_SWAY_DEGREES,
	SHAKE_X,
	SHAKE_Y,
	behaviorLabel,
	behaviorProblem,
	behaviorsProblem,
	behaviorsSummary,
	defaultBehavior,
	full,
	isNeutral,
	liveBehaviors,
	mix,
	rebaseBehaviors,
	sanitized,
	shakePhase,
	shifted
} from './behaviors';
import { bakeBehaviors, bakeBehaviorsOn, defaultBakeFps, setBehaviors } from './behavior-ops';
import { rebaseAnimation } from './edit-modes';
import {
	baseAt,
	baseTransformAt,
	isDriven,
	pictureMoves,
	placementAnimated,
	propertyAt,
	rotationTurns,
	transformAnimated,
	transformAt,
	zoomAnimated
} from './channels';
import { DEFAULT_TRANSFORM } from './types';
import type { Asset, Behavior, Clip, Easing, Keyframe, Property, PropertyTrack, Timeline, Transform } from './types';

// The differential test for the browser harness's live behaviours. `behaviors.ts` / `behavior-ops.ts` and the
// resolver split in `channels.ts` (`propertyAt` the picture, `baseAt` the pose without behaviours) are a hand port of
// kerf-core's `model/behavior.rs` and `project/behavior.rs`; kerf-core writes the answers down
// (`fixtures/behaviors-corpus.json`, regenerated with `KERF_BLESS_CORPUS=1 cargo test -p kerf-core
// --no-default-features -- behaviors_corpus`) and every one is replayed here: the limits and the shake table, the
// hash behind a seed, the base and picture pose at moments of every case and the flags built on them, what is held
// and what is refused, a head cut's rebase and the bake's keys and refusals. A rule changed in Rust without the port
// following fails this test, naming the case.

interface Slim {
	duration: number;
	timeline_start: number;
	transform: Omit<Transform, 'crop_left' | 'crop_right' | 'crop_top' | 'crop_bottom'>;
	keyframes: Keyframe[];
	channels: PropertyTrack[];
	behaviors: Behavior[];
}

type Pose = { scale: number; pos_x: number; pos_y: number; rotation: number };

const c = corpus as unknown as {
	limits: Record<string, number>;
	shake_waves: { x: number[][]; y: number[][] };
	seed_phases: { seed: number; phases: number[] }[];
	samples: {
		name: string;
		clip: Slim;
		times: number[];
		base: Pose[];
		picture: Pose[];
		flags: Record<string, unknown>;
	}[];
	held: { behavior: Behavior; ok: boolean; sanitized: Behavior; neutral: boolean }[];
	rebase: {
		name: string;
		clip: Slim;
		by: number;
		behaviors: Behavior[];
		keyframes: Keyframe[];
		channels: PropertyTrack[];
		times: number[];
		picture: Pose[];
	}[];
	bake: {
		name: string;
		clip: Slim;
		fps: number;
		max_keys: number;
		expect:
			| { ok: { properties: Property[]; keys: number; channels: PropertyTrack[]; behaviors: Behavior[] } }
			| { err: string };
	}[];
};

/** A clip of the part the resolver reads, as the corpus carries it. */
const clipOf = (s: Slim): Clip =>
	({
		id: 'c',
		asset_id: 'a',
		source_in: 0,
		source_out: s.duration,
		timeline_start: s.timeline_start,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		transform: { ...DEFAULT_TRANSFORM, ...s.transform },
		keyframes: structuredClone(s.keyframes),
		channels: structuredClone(s.channels),
		behaviors: structuredClone(s.behaviors)
	}) as unknown as Clip;

/** Rust's `sin` and V8's differ by an ulp at most; a pose is read to a billionth. */
const near = (got: number, want: number, what: string) => {
	expect(Math.abs(got - want), `${what}: ${got} vs ${want}`).toBeLessThan(1e-9);
};

const poseOf = (t: Transform): Pose => ({ scale: t.scale, pos_x: t.pos_x, pos_y: t.pos_y, rotation: t.rotation });

const closePose = (got: Pose, want: Pose, what: string) => {
	for (const k of ['scale', 'pos_x', 'pos_y', 'rotation'] as const) near(got[k], want[k], `${what} ${k}`);
};

const sorted = (o: unknown): unknown => JSON.parse(JSON.stringify(o, (_, v) => (v && typeof v === 'object' && !Array.isArray(v) ? Object.fromEntries(Object.entries(v).sort(([a], [b]) => a.localeCompare(b))) : v)));

describe('the limits and the table', () => {
	test('the limits are the core’s', () => {
		const mine: Record<string, number> = {
			max_behaviors: MAX_BEHAVIORS,
			max_hz: MAX_BEHAVIOR_HZ,
			max_sway_degrees: MAX_SWAY_DEGREES,
			max_breathe: MAX_BREATHE,
			max_spin_turns: MAX_SPIN_TURNS,
			max_drift: MAX_DRIFT,
			max_shake: MAX_SHAKE,
			max_phase: MAX_BEHAVIOR_PHASE
		};
		expect(mine).toEqual(c.limits);
	});

	test('the shake table is the core’s', () => {
		expect(SHAKE_X.map((r) => [...r])).toEqual(c.shake_waves.x);
		expect(SHAKE_Y.map((r) => [...r])).toEqual(c.shake_waves.y);
	});

	test('a seed gives the same six phases on every machine', () => {
		for (const { seed, phases } of c.seed_phases) {
			phases.forEach((want, k) => near(shakePhase(seed, k), want, `seed ${seed} wave ${k}`));
		}
		expect(mix(0, 0)).toBe(Math.floor((1.859279059967247 / (Math.PI * 2)) * 4_294_967_296 + 0.5));
		for (const seed of [0, 1, 0xdeadbeef, 4_294_967_295]) {
			for (let k = 0; k < 6; k++) {
				const m = mix(seed, k);
				expect(Number.isInteger(m) && m >= 0 && m <= 4_294_967_295).toBe(true);
			}
		}
	});
});

describe('the resolver replays the core', () => {
	for (const s of c.samples) {
		test(s.name, () => {
			const clip = clipOf(s.clip);
			s.times.forEach((t, i) => {
				closePose(poseOf(baseTransformAt(clip, t)), s.base[i], `base at ${t}`);
				closePose(poseOf(transformAt(clip, t)), s.picture[i], `picture at ${t}`);
				near(propertyAt(clip, 'rotation', t), s.picture[i].rotation, `propertyAt rotation at ${t}`);
				near(baseAt(clip, 'scale', t), s.base[i].scale, `baseAt scale at ${t}`);
			});
			const f = s.flags;
			expect(transformAnimated(clip), 'is_animated').toBe(f.is_animated as boolean);
			expect(zoomAnimated(clip), 'zoom_animated').toBe(f.zoom_animated as boolean);
			expect(placementAnimated(clip), 'placement_animated').toBe(f.placement_animated as boolean);
			expect(pictureMoves(clip), 'picture_moves').toBe(f.picture_moves as boolean);
			expect(rotationTurns(clip), 'rotation_turns').toBe(f.rotation_turns as boolean);
			expect(liveBehaviors(clip.behaviors).length > 0, 'has_behaviors').toBe(f.has_behaviors as boolean);
			expect(sorted(liveBehaviors(clip.behaviors))).toEqual(sorted(f.live));
		});
	}

	test('a behaviour is a term on top of the base, not a part of it', () => {
		const clip = clipOf(c.samples.find((s) => s.name === 'every kind on keys')!.clip);
		const t = 1.3;
		expect(baseAt(clip, 'rotation', t)).not.toBe(propertyAt(clip, 'rotation', t));
		clip.behaviors = [];
		expect(baseAt(clip, 'rotation', t)).toBe(propertyAt(clip, 'rotation', t));
		expect(isDriven(clip, 'rotation')).toBe(true);
		expect(isDriven(clip, 'opacity')).toBe(false);
	});
});

describe('what is held and what is refused', () => {
	c.held.forEach((h, i) => {
		test(`${h.behavior.kind} #${i}`, () => {
			expect(behaviorProblem(h.behavior) === undefined, JSON.stringify(h.behavior)).toBe(h.ok);
			expect(sorted(sanitized(h.behavior))).toEqual(sorted(h.sanitized));
			expect(isNeutral(sanitized(h.behavior))).toBe(h.neutral);
		});
	});

	test('the words of a refusal are the core’s', () => {
		expect(behaviorProblem({ kind: 'sway', amount: 181, speed: 1 })).toBe('sway amount must be a number within 0..=180 degrees (got 181)');
		expect(behaviorProblem({ kind: 'breathe', amount: 0.91, speed: 1 })).toBe('breathe amount must be a number within 0..=0.9 (got 0.91)');
		expect(behaviorProblem({ kind: 'spin', speed: 10.5 })).toBe('spin speed must be a number within -10..=10 turns a second (got 10.5)');
		expect(behaviorProblem({ kind: 'drift', speed: 1, angle: 36001 })).toBe('drift angle must be a number within -36000..=36000 degrees (got 36001)');
		expect(behaviorProblem({ kind: 'shake', amount: 0.26, speed: 3 })).toBe('shake amount must be a number within 0..=0.25 of the frame (got 0.26)');
		expect(behaviorProblem({ kind: 'shake', amount: 0.1, speed: 3, phase: 1_000_001 })).toBe(
			'shake phase must be a number within -1000000..=1000000 seconds (got 1000001)'
		);
		expect(behaviorProblem({ kind: 'sway', amount: NaN, speed: 1 })).toBe('sway amount must be a number within 0..=180 degrees (got NaN)');
		const nine = Array.from({ length: 9 }, () => ({ kind: 'spin', speed: 0.1 }) as Behavior);
		expect(behaviorsProblem(nine)).toBe('at most 8 behaviours per clip (got 9)');
		expect(behaviorsProblem(nine.slice(0, 8))).toBeUndefined();
	});
});

describe('a head move carries the motion on', () => {
	for (const r of c.rebase) {
		test(r.name, () => {
			const clip = clipOf(r.clip);
			rebaseAnimation(clip, r.by);
			expect(sorted(clip.behaviors)).toEqual(sorted(r.behaviors));
			expect(clip.keyframes ?? []).toEqual(r.keyframes);
			expect(sorted(clip.channels ?? [])).toEqual(sorted(r.channels));
			r.times.forEach((t, i) => closePose(poseOf(transformAt(clip, t)), r.picture[i], `picture at ${t}`));
		});
	}

	test('a cut is the same picture one second further on, whatever it was', () => {
		const clip = clipOf(c.samples.find((s) => s.name === 'every kind')!.clip);
		const before = poseOf(transformAt(clip, 3.4));
		rebaseAnimation(clip, 2.5);
		closePose(poseOf(transformAt(clip, 0.9)), before, 'the cut piece');
	});

	test('shifting is a clamp on the phase and nothing else', () => {
		expect(shifted({ kind: 'spin', speed: 0.1 }, 2).phase).toBe(2);
		expect(shifted({ kind: 'spin', speed: 0.1, phase: 5 }, -7).phase).toBe(-2);
		expect(shifted({ kind: 'spin', speed: 0.1, phase: 999_999 }, 5).phase).toBe(MAX_BEHAVIOR_PHASE);
		expect(shifted({ kind: 'spin', speed: 0.1, phase: 1 }, NaN).phase).toBe(1);
		const clip = { behaviors: [{ kind: 'spin', speed: 0.1 } as Behavior] };
		rebaseBehaviors(clip, 0);
		expect(clip.behaviors).toEqual([{ kind: 'spin', speed: 0.1 }]);
		rebaseBehaviors(clip, 1.5);
		expect(clip.behaviors).toEqual([{ kind: 'spin', speed: 0.1, phase: 1.5 }]);
	});
});

describe('baking replays the core', () => {
	for (const b of c.bake) {
		test(b.name, () => {
			const clip = clipOf(b.clip);
			if ('err' in b.expect) {
				const before = structuredClone(clip);
				expect(() => bakeBehaviors(clip, b.fps, b.max_keys, b.clip.duration)).toThrow(b.expect.err);
				expect(clip, 'a refusal leaves the clip as it was').toEqual(before);
				return;
			}
			const want = b.expect.ok;
			const baked = bakeBehaviors(clip, b.fps, b.max_keys, b.clip.duration);
			expect(baked.properties).toEqual(want.properties);
			expect(baked.keys).toBe(want.keys);
			expect(baked.fps).toBe(b.fps);
			expect(clip.behaviors ?? []).toEqual(want.behaviors);
			expect(clip.channels?.map((t) => t.prop)).toEqual(want.channels.map((t) => t.prop));
			for (const track of want.channels) {
				const got = clip.channels!.find((t) => t.prop === track.prop)!;
				expect(got.keys.length, track.prop).toBe(track.keys.length);
				track.keys.forEach((k, i) => {
					near(got.keys[i].time, k.time, `${track.prop} key ${i} time`);
					near(got.keys[i].value, k.value, `${track.prop} key ${i}`);
					expect(got.keys[i].easing, `${track.prop} key ${i} easing`).toBe((k as { easing?: Easing }).easing);
				});
			}
		});
	}

	test('the keys are exact at the frames and the clip is the picture it was', () => {
		const clip = clipOf(c.samples.find((s) => s.name === 'every kind on keys')!.clip);
		clip.timeline_start = 0.123;
		const want = (t: number) => poseOf(transformAt(clip, t));
		const at: number[] = [0, 1 / 30 - 0.123, 2 / 30 - 0.123, 1.5, 9.99];
		const expected = at.map(want);
		clip.source_out = 10;
		bakeBehaviors(clip, 30, 1000, 10);
		expect(clip.behaviors ?? []).toEqual([]);
		for (const [i, t] of at.entries()) {
			if (t < 0) continue;
			const frame = Math.abs(t * 30 + 0.123 * 30 - Math.round(t * 30 + 0.123 * 30)) < 1e-9;
			if (frame) closePose(poseOf(transformAt(clip, t)), expected[i], `frame at ${t}`);
		}
	});
});

describe('the harness ops', () => {
	const asset = (fps: number): Asset =>
		({ id: 'a', path: '/a.mp4', name: 'a.mp4', duration: 60, streams: [{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps }] }) as unknown as Asset;
	const clip = (id: string, over: Partial<Clip> = {}): Clip =>
		({ id, asset_id: 'a', source_in: 0, source_out: 4, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0, ...over }) as Clip;
	const timeline = (): Timeline =>
		({
			tracks: [
				{ id: 't1', name: 'V1', kind: 'video', clips: [clip('v1'), clip('v2', { timeline_start: 5 })] },
				{ id: 't2', name: 'V2', kind: 'video', locked: true, clips: [clip('v3')] },
				{ id: 't3', name: 'A1', kind: 'audio', clips: [clip('s1')] }
			]
		}) as unknown as Timeline;
	const sway: Behavior = { kind: 'sway', amount: 5, speed: 0.5 };

	test('set replaces the list on every clip as one edit, clears with an empty one and stores whole behaviours', () => {
		const tl = timeline();
		const done = setBehaviors(tl, ['v1', 'v2', 'v1'], [sway, { kind: 'shake', amount: 0.01, speed: 8, seed: 3 }]);
		expect(done.label).toBe('Set behaviours (2 clips)');
		expect(done.result.map((x) => x.id)).toEqual(['v1', 'v2']);
		expect(done.timeline.tracks[0].clips[0].behaviors).toEqual([
			{ kind: 'sway', amount: 5, speed: 0.5, phase: 0 },
			{ kind: 'shake', amount: 0.01, speed: 8, seed: 3, phase: 0 }
		]);
		expect(tl.tracks[0].clips[0].behaviors, 'the timeline given is untouched').toBeUndefined();
		const cleared = setBehaviors(done.timeline, ['v1'], []);
		expect(cleared.label).toBe('Clear behaviours');
		expect(cleared.timeline.tracks[0].clips[0]).not.toHaveProperty('behaviors');
		expect(setBehaviors(tl, ['v3'], [sway]).timeline.tracks[1].clips[0].behaviors?.length, 'a lock guards layout, not look').toBe(1);
	});

	test('set refuses what the core refuses and changes nothing', () => {
		const tl = timeline();
		const refused = (ids: string[], list: Behavior[]) => {
			try {
				setBehaviors(tl, ids, list);
				return 'went through';
			} catch (e) {
				return (e as Error).message;
			}
		};
		expect(refused([], [sway])).toBe('invalid argument: name at least one clip');
		expect(refused(['v1', 'nope'], [sway])).toBe('clip not found: nope');
		expect(refused(['v1', 's1'], [sway])).toBe('invalid argument: clip s1 is on audio track A1: a sound has no picture to move');
		expect(refused(['v1'], [{ kind: 'sway', amount: 181, speed: 1 }])).toBe(
			'invalid argument: sway amount must be a number within 0..=180 degrees (got 181)'
		);
		expect(refused(['v1'], Array.from({ length: 9 }, () => sway))).toBe('invalid argument: at most 8 behaviours per clip (got 9)');
		expect(JSON.stringify(tl)).toBe(JSON.stringify(timeline()));
	});

	test('bake keys the numbers the behaviours move, at the rate asked, the delivery’s or the footage’s', () => {
		const tl = setBehaviors(timeline(), ['v1'], [sway, { kind: 'drift', speed: 0.1, angle: 90 }]).timeline;
		const done = bakeBehaviorsOn(tl, [asset(25)], ['v1'], 10);
		expect(done.label).toBe('Bake behaviours');
		expect(done.result[0].baked).toEqual({ properties: ['pos_x', 'pos_y', 'rotation'], keys: 41, fps: 10 });
		expect(done.timeline.tracks[0].clips[0].behaviors).toBeUndefined();
		expect(propertyAt(done.timeline.tracks[0].clips[0], 'rotation', 0.5)).toBeCloseTo(5 * Math.sin(Math.PI * 0.5 * 2 * 0.5), 9);
		expect(bakeBehaviorsOn(tl, [asset(25)], ['v1']).result[0].baked.fps, 'the footage’s').toBe(25);
		expect(defaultBakeFps({ ...tl, format: { width: 1920, height: 1080, fit: 'contain', fps: 24 } }, [asset(25)])).toBe(24);
		expect(defaultBakeFps(tl, [])).toBe(30);
		expect(() => bakeBehaviorsOn(tl, [], ['v2'])).toThrow('invalid argument: the clip has no behaviours to bake');
		expect(() => bakeBehaviorsOn(tl, [], ['v1'], 5000)).toThrow('bake at a frame rate within 1..=1000 (got 5000)');
		expect(() => bakeBehaviorsOn(tl, [], [])).toThrow('name at least one clip');
		const long = structuredClone(tl);
		long.tracks[0].clips[0].source_out = 80;
		expect(() => bakeBehaviorsOn(long, [asset(30)], ['v1'], 30)).toThrow('split the clip into pieces under 33.3 s');
	});

	test('a behaviour that moves nothing bakes to nothing and clears', () => {
		const tl = setBehaviors(timeline(), ['v1'], [{ kind: 'sway', amount: 0, speed: 1 }]).timeline;
		const done = bakeBehaviorsOn(tl, [], ['v1'], 30);
		expect(done.result[0].baked).toEqual({ properties: [], keys: 0, fps: 30 });
		expect(done.timeline.tracks[0].clips[0].behaviors).toBeUndefined();
	});
});

describe('the words', () => {
	test('a default of each kind is a gentle, valid one', () => {
		for (const kind of BEHAVIOR_KINDS) {
			const b = defaultBehavior(kind, 7);
			expect(behaviorProblem(b), kind).toBeUndefined();
			expect(isNeutral(sanitized(b)), kind).toBe(false);
		}
		expect(defaultBehavior('sway')).toEqual({ kind: 'sway', amount: 5, speed: 0.5, phase: 0 });
		expect(defaultBehavior('breathe')).toEqual({ kind: 'breathe', amount: 0.04, speed: 0.3, phase: 0 });
		expect(defaultBehavior('spin')).toEqual({ kind: 'spin', speed: 0.1, phase: 0 });
		expect(defaultBehavior('drift')).toEqual({ kind: 'drift', speed: 0.02, angle: 0, phase: 0 });
		expect(defaultBehavior('shake', 9)).toEqual({ kind: 'shake', amount: 0.01, speed: 8, seed: 9, phase: 0 });
	});

	test('the summary names the kinds in order and counts a repeat', () => {
		expect(behaviorsSummary(undefined)).toBe('None');
		expect(behaviorsSummary([])).toBe('None');
		expect(behaviorsSummary([defaultBehavior('sway'), defaultBehavior('shake')])).toBe('Sway · Shake');
		expect(behaviorsSummary([defaultBehavior('sway'), defaultBehavior('shake'), defaultBehavior('sway')])).toBe('Sway ×2 · Shake');
	});

	test('a row is described by its numbers', () => {
		expect(behaviorLabel({ kind: 'sway', amount: 5, speed: 0.5 })).toBe('sway 5° at 0.5 Hz');
		expect(behaviorLabel({ kind: 'breathe', amount: 0.04, speed: 0.3 })).toBe('breathe 4% at 0.3 Hz');
		expect(behaviorLabel({ kind: 'spin', speed: -0.25 })).toBe('spin -0.25 turns/s');
		expect(behaviorLabel({ kind: 'drift', speed: 0.02, angle: 90 })).toBe('drift 0.02 frames/s at 90°');
		expect(behaviorLabel({ kind: 'shake', amount: 0.01, speed: 8, seed: 12 })).toBe('shake 1% at 8 Hz (seed 12)');
		expect(full({ kind: 'drift', speed: 1 })).toEqual({ kind: 'drift', speed: 1, angle: 0, phase: 0 });
	});
});
