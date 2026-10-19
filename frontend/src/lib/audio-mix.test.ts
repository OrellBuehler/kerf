import { describe, expect, test } from 'bun:test';
import {
	clipGainAt,
	CROSSFADE_STEPS,
	crossfadeIn,
	crossfadePair,
	SPLICE_CROSSFADE_S,
	fadeInOf,
	fadeInTimes,
	fadeOutOf,
	gainAtFullScale,
	gainAutomation,
	gainAutomationBack,
	type GainClip,
	type GainPoint,
	limitedFullScaleDb,
	limiterParams,
	LIMITER_RATIO
} from './audio-mix';
import { panGains } from './mixer';

/** The envelope as `audio.ts` computed it before the fader moved onto the track's bus:
 *  the clip's volume *and* the track's fader in one factor, shaped by the fades.
 *  Kept here verbatim as the reference the new split must equal. */
function oldEnvelope(
	clip: { volume?: number; fade_in?: number; fade_out?: number; transition_in?: { duration: number } | null },
	trackVolume: number,
	clipStart: number,
	clipEnd: number
) {
	const vol = (clip.volume ?? 1) * trackVolume;
	const fi = (clip.fade_in ?? 0) + (clip.transition_in?.duration ?? 0);
	const fo = clip.fade_out ?? 0;
	return (tl: number) => {
		let v = vol;
		if (fi > 0 && tl < clipStart + fi) v *= Math.max(0, (tl - clipStart) / fi);
		if (fo > 0 && tl > clipEnd - fo) v *= Math.max(0, (clipEnd - tl) / fo);
		return v;
	};
}

const clips = [
	{ volume: 1 },
	{ volume: 0.5, fade_in: 1, fade_out: 2 },
	{ volume: 1.7, fade_in: 0.25 },
	{ volume: undefined, fade_out: 3 },
	{ volume: 0.8, fade_in: 0.5, transition_in: { kind: 'crossfade', duration: 1.5 } },
	{ volume: 0, fade_in: 1, fade_out: 1 },
	{ volume: 2, fade_in: 4, fade_out: 4 } // fades that overlap each other on a short clip
] as never[];

describe('clipGainAt', () => {
	test('is the old per-clip envelope, to the sample, for every fader position', () => {
		const start = 10;
		const end = 16;
		for (const clip of clips) {
			for (const trackVolume of [0, 0.25, 0.5, 1, 1.4, 2]) {
				const old = oldEnvelope(clip, trackVolume, start, end);
				for (let t = start - 1; t <= end + 1; t += 0.125) {
					// Folded into the clip (the old way) and ridden by the bus (the new way).
					expect(clipGainAt(clip, t, start, end, trackVolume)).toBe(old(t));
					expect(clipGainAt(clip, t, start, end) * trackVolume).toBeCloseTo(old(t), 12);
				}
			}
		}
	});

	test('fades and a transition in shape the gain, and a clip with none is flat', () => {
		const c = { volume: 0.5, fade_in: 2, fade_out: 2 } as never;
		expect(clipGainAt(c, 0, 0, 10)).toBe(0);
		expect(clipGainAt(c, 1, 0, 10)).toBeCloseTo(0.25, 12);
		expect(clipGainAt(c, 5, 0, 10)).toBe(0.5);
		expect(clipGainAt(c, 9, 0, 10)).toBeCloseTo(0.25, 12);
		expect(clipGainAt(c, 10, 0, 10)).toBe(0);
		const x = { volume: 1, fade_in: 0.5, transition_in: { kind: 'crossfade', duration: 1.5 } } as never;
		expect(fadeInOf(x)).toBe(2);
		expect(clipGainAt(x, 1, 0, 10)).toBe(0.5);
		expect(fadeOutOf({} as never)).toBe(0);
		expect(clipGainAt({} as never, 3, 0, 10)).toBe(1);
	});
});

describe('a crossfade-in is constant-power, as the export draws it', () => {
	const x = { volume: 1, transition_in: { kind: 'crossfade', duration: 2 }, power_pair: true } as never;
	test('sin over the overlap, so it and the outgoing cos sum to unit power', () => {
		expect(crossfadeIn(x)).toBe(true);
		for (const f of [0, 0.25, 0.5, 0.75, 1]) {
			const inc = clipGainAt(x, 10 + 2 * f, 10, 20);
			expect(inc).toBeCloseTo(Math.sin((f * Math.PI) / 2), 12);
			expect(inc ** 2 + Math.cos((f * Math.PI) / 2) ** 2).toBeCloseTo(1, 12);
		}
		expect(fadeInTimes(x, 10)).toHaveLength(CROSSFADE_STEPS);
		expect(fadeInTimes(x, 10).at(-1)).toBe(12);
	});
	test('only a pair of two different sounds is constant-power (`clip_timing` replays)', () => {
		type C = { id: string; asset_id: string; source_in: number; source_out: number; timeline_start: number; speed?: number; enabled?: boolean; transition_in?: unknown };
		const fade = { kind: 'crossfade', duration: 1 };
		const pair = (a: [number, number, number], b: [number, number, number], asset = 's', extra: C[] = []) => {
			const out: C = { id: 'a', asset_id: 's', source_in: a[0], source_out: a[1], speed: a[2], timeline_start: 0 };
			const len = (a[1] - a[0]) / Math.abs(a[2]);
			const inn: C = { id: 'b', asset_id: asset, source_in: b[0], source_out: b[1], speed: b[2], timeline_start: len, transition_in: fade };
			return crossfadePair([out, ...extra, inn] as never, inn as never);
		};
		expect(pair([0, 10, 1], [20, 30, 1])).toBe(true);
		expect(pair([0, 10, 1], [10, 20, 1], 'o')).toBe(true);
		expect(pair([0, 20, 2], [20, 30, 1])).toBe(true);
		expect(pair([20, 30, -1], [0, 10, 1])).toBe(true);
		expect(pair([0, 10, 1], [11.5, 20, 1])).toBe(true);
		expect(pair([0, 10, 1], [10, 20, 1])).toBe(false);
		expect(pair([0, 10, 1], [10.5, 20, 1])).toBe(false);
		expect(pair([0, 10, 1], [9.5, 20, 1])).toBe(false);
		expect(pair([0, 20, 2], [21.5, 30, 2])).toBe(false);
		expect(pair([20, 30, -1], [10, 20, -1])).toBe(false);
		// A de-click splice of one recording (a music fit's, between bars that sound alike).
		const spliced = (asset: string) => {
			const out = { id: 'a', asset_id: 's', source_in: 0, source_out: 10, timeline_start: 0 };
			const inn = { id: 'b', asset_id: asset, source_in: 20, source_out: 30, timeline_start: 10, transition_in: { kind: 'crossfade', duration: SPLICE_CROSSFADE_S } };
			return crossfadePair([out, inn] as never, inn as never);
		};
		expect(spliced('s')).toBe(false);
		expect(spliced('o')).toBe(true);
		// A disabled clip is not in the render, so it is nobody's partner.
		const ghost: C = { id: 'g', asset_id: 'z', source_in: 0, source_out: 1, timeline_start: 9.5, enabled: false };
		expect(pair([0, 10, 1], [20, 30, 1], 's', [ghost])).toBe(true);
		// No partner: a fade up from black.
		const lone = { id: 'b', asset_id: 's', source_in: 0, source_out: 10, timeline_start: 5, transition_in: fade };
		expect(crossfadePair([lone] as never, lone as never)).toBe(false);
		const plain = { volume: 1, transition_in: fade } as never;
		expect(crossfadeIn(plain)).toBe(false);
		expect(clipGainAt(plain, 10.5, 10, 20)).toBeCloseTo(0.5, 12);
	});
	test('a clip fade beside it, a dip, or no transition stays linear', () => {
		const mixed = { volume: 1, fade_in: 0.5, transition_in: { kind: 'crossfade', duration: 1.5 } } as never;
		const dip = { volume: 1, transition_in: { kind: 'dip_to_black', duration: 2 } } as never;
		for (const c of [mixed, dip, { volume: 1, fade_in: 2 } as never]) {
			expect(crossfadeIn(c)).toBe(false);
			expect(clipGainAt(c, 11, 10, 20)).toBeCloseTo(0.5, 12);
			expect(fadeInTimes(c, 10)).toEqual([12]);
		}
	});
});

describe('the track bus holds the same mix the clip used to carry', () => {
	test('fader × pan on the bus equals the old per-clip factors on both sides', () => {
		// The old graph: clip gain (volume × fader × fades) → left/right pan gains.
		// The new one: clip gain (volume × fades) → bus gain (fader) → the same pan gains.
		for (const volume of [0, 0.3, 1, 1.9]) {
			for (const pan of [-1, -0.4, 0, 0.25, 1]) {
				const [gl, gr] = panGains(pan);
				const old = oldEnvelope({ volume: 0.7, fade_in: 1 } as never, volume, 0, 8)(3.5);
				const fresh = clipGainAt({ volume: 0.7, fade_in: 1 } as never, 3.5, 0, 8) * volume;
				expect(fresh * gl).toBeCloseTo(old * gl, 12);
				expect(fresh * gr).toBeCloseTo(old * gr, 12);
			}
		}
	});
});

describe('limiterParams', () => {
	test('a hard knee at the ceiling, the steepest ratio, a fast attack', () => {
		const p = limiterParams(-3);
		expect(p.threshold).toBe(-3);
		expect(p.knee).toBe(0);
		expect(p.ratio).toBe(LIMITER_RATIO);
		expect(p.ratio).toBe(20); // the node's own maximum
		expect(p.attack).toBeLessThan(0.01);
		expect(p.release).toBeGreaterThan(p.attack);
	});

	test('the ceiling is clamped to what the node accepts', () => {
		expect(limiterParams(6).threshold).toBe(0);
		expect(limiterParams(-300).threshold).toBe(-100);
		expect(limiterParams(Number.NaN).threshold).toBe(0);
	});

	test('the trim cancels the compressor’s automatic makeup, whatever the ceiling', () => {
		for (const ceiling of [0, -0.5, -1, -3, -6, -12, -24]) {
			const p = limiterParams(ceiling);
			// A browser's compressor makes up (1 / full-range gain) ** 0.6; the trim undoes exactly that.
			const makeup = (1 / gainAtFullScale(p.threshold)) ** 0.6;
			expect(p.trim * makeup).toBeCloseTo(1, 12);
			expect(p.trim).toBeLessThanOrEqual(1);
		}
		expect(limiterParams(0).trim).toBe(1); // a ceiling at full scale reduces nothing there
	});

	test('a full-scale signal comes out within a dB of the ceiling it was given', () => {
		// threshold + (0 - threshold) / 20: 0.05 dB over per dB of ceiling depth.
		expect(limitedFullScaleDb(-1)).toBeCloseTo(-0.95, 12);
		expect(limitedFullScaleDb(-12)).toBeCloseTo(-11.4, 12);
		expect(limitedFullScaleDb(-24)).toBeCloseTo(-22.8, 12);
		for (const c of [-1, -3, -12, -24]) expect(limitedFullScaleDb(c) - c).toBeLessThan(1.3);
		// …and the gain at full scale is that level, as a linear number.
		expect(20 * Math.log10(gainAtFullScale(-12))).toBeCloseTo(-11.4, 9);
	});
});

describe('a keyed volume', () => {
	const clip = (over: Partial<GainClip> = {}): GainClip => ({
		volume: 0.77,
		fade_in: 0,
		fade_out: 0,
		channels: [
			{
				prop: 'volume',
				keys: [
					{ time: 0.2, value: 0.2 },
					{ time: 1.1, value: 1.8 },
					{ time: 1.7, value: 0.4, easing: 'hold' },
					{ time: 2.2, value: 1 }
				]
			}
		],
		...over
	});

	test('is the gain over the clip’s own time, instead of the static one', () => {
		const c = clip();
		expect(clipGainAt(c, 10.2, 10, 15)).toBeCloseTo(0.2, 12);
		expect(clipGainAt(c, 11.1, 10, 15)).toBeCloseTo(1.8, 12);
		expect(clipGainAt(c, 11.9, 10, 15)).toBeCloseTo(0.4, 12);
		expect(clipGainAt(c, 12.5, 10, 15, 0.5)).toBeCloseTo(0.5, 12);
		// A clip with no track keeps its static volume; the fades still shape a keyed one.
		expect(clipGainAt({ ...c, channels: undefined }, 11, 10, 15)).toBe(0.77);
		expect(clipGainAt(clip({ fade_out: 2 }), 14, 10, 15)).toBeCloseTo(0.5, 12);
	});

	test('is ramped through the curve’s points and the fade edges', () => {
		const near = (got: GainPoint[], want: [number, number, boolean?][]) => {
			expect(got.length).toBe(want.length);
			got.forEach((p, i) => {
				expect(p.time).toBeCloseTo(want[i][0], 9);
				expect(p.value).toBeCloseTo(want[i][1], 6);
				expect(!!p.jump).toBe(!!want[i][2]);
			});
		};
		// 0.2 → 1.8 over [0.2, 1.1], down to the hold at 0.4 (1.7 to 2.2), which steps up to 1.
		near(gainAutomation(clip(), 10, 15, 10), [
			[10.2, 0.2],
			[11.1, 1.8],
			[11.7, 0.4],
			[12.2, 0.4],
			[12.2, 1, true],
			[15, 1]
		]);
		// A fade-in and a fade-out add their edges (the last ramp is the fade-out to silence).
		const faded = gainAutomation(clip({ fade_in: 0.5, fade_out: 1 }), 10, 15, 11);
		expect(faded.length).toBe(6);
		[11.1, 11.7, 12.2, 12.2, 14, 15].forEach((t, i) => expect(faded[i].time).toBeCloseTo(t, 9));
		expect(faded[faded.length - 1].value).toBeCloseTo(0, 12);
		near(gainAutomation(clip(), 10, 15, 14), [[15, 1]]);
		expect(gainAutomation(clip({ channels: undefined }), 10, 15, 10)).toEqual([]);
	});

	test('steps where the curve holds, instead of ramping across the whole segment', () => {
		// 0.2 held for 2 s, then 1.0 at 2 s, then 0.5 at 4 s: the export steps at 2 s.
		const held = clip({
			channels: [
				{
					prop: 'volume',
					keys: [
						{ time: 0, value: 0.2, easing: 'hold' },
						{ time: 2, value: 1 },
						{ time: 4, value: 0.5 }
					]
				}
			]
		});
		const points = gainAutomation(held, 10, 14, 10);
		// The ramp arrives at 12 s still on 0.2, the jump lands on 1.0, then it ramps to 0.5.
		expect(points.map((p) => [p.time, !!p.jump])).toEqual([
			[12, false],
			[12, true],
			[14, false]
		]);
		expect(points[0].value).toBeCloseTo(0.2, 6);
		expect(points[1].value).toBeCloseTo(1, 6);
		expect(points[2].value).toBeCloseTo(0.5, 12);
		// Played from inside the hold the start value is the hold's and the step is still ahead;
		// from the step itself the start value is already the one it lands on and nothing steps.
		expect(clipGainAt(held, 11, 10, 14)).toBe(0.2);
		expect(gainAutomation(held, 10, 14, 11).map((p) => p.jump ?? false)).toEqual([false, true, false]);
		expect(clipGainAt(held, 12, 10, 14)).toBe(1);
		expect(gainAutomation(held, 10, 14, 12).map((p) => [p.time, p.jump ?? false])).toEqual([[14, false]]);
		// Two keys at one time are a step too, and a fade's edge on the step keeps both values.
		const twin = clip({
			fade_in: 2,
			channels: [
				{
					prop: 'volume',
					keys: [
						{ time: 0, value: 1 },
						{ time: 2, value: 0.5 },
						{ time: 2, value: 1.5 },
						{ time: 4, value: 1.5 }
					]
				}
			]
		});
		const steps = gainAutomation(twin, 10, 14, 10);
		expect(steps.map((p) => [p.time, !!p.jump])).toEqual([
			[12, false],
			[12, true],
			[14, false]
		]);
		expect(steps[0].value).toBeCloseTo(0.5, 6);
		expect(steps[1].value).toBeCloseTo(1.5, 6);
		// Equal values at one time (a zero-length segment) are no step.
		const flat = clip({
			channels: [
				{
					prop: 'volume',
					keys: [
						{ time: 0, value: 1 },
						{ time: 2, value: 1, easing: 'hold' },
						{ time: 2, value: 1 }
					]
				}
			]
		});
		expect(gainAutomation(flat, 10, 14, 10).every((p) => !p.jump)).toBe(true);
	});
});

describe('a gain met backwards (reverse shuttle)', () => {
	const keyed: GainClip = {
		volume: 0.77,
		fade_in: 0,
		fade_out: 0,
		channels: [
			{
				prop: 'volume',
				keys: [
					{ time: 0.2, value: 0.2 },
					{ time: 1.1, value: 1.8 },
					{ time: 1.7, value: 0.4, easing: 'hold' },
					{ time: 2.2, value: 1 }
				]
			}
		]
	};

	test('ramps through the forward points in the order they are met, a step met from above', () => {
		const points = gainAutomationBack(keyed, 10, 15, 15);
		expect(points.map((p) => [Number(p.time.toFixed(9)), !!p.jump])).toEqual([
			[12.2, false],
			[12.2, true],
			[11.7, false],
			[11.1, false],
			[10.2, false],
			[10, false]
		]);
		// Arriving at the step from above the gain is the one it lands on going forwards (1), and the
		// jump sets the hold's (0.4); the rest is the curve.
		expect(points[0].value).toBeCloseTo(1, 6);
		expect(points[1].value).toBeCloseTo(0.4, 6);
		expect(points[2].value).toBeCloseTo(0.4, 6);
		expect(points[3].value).toBeCloseTo(1.8, 6);
		expect(points[4].value).toBeCloseTo(0.2, 6);
		// Played back from inside the hold, the step is behind and nothing jumps.
		expect(gainAutomationBack(keyed, 10, 15, 12).every((p) => !p.jump)).toBe(true);
	});

	test('a plain clip is its fades the other way round', () => {
		const plain: GainClip = { volume: 0.8, fade_in: 1, fade_out: 2 };
		const points = gainAutomationBack(plain, 10, 15, 15);
		expect(points.map((p) => [p.time, p.value])).toEqual([
			[13, 0.8],
			[11, 0.8],
			[10, 0]
		]);
		// From inside the fade-out only what is still behind the playhead.
		expect(gainAutomationBack(plain, 10, 15, 12).map((p) => p.time)).toEqual([11, 10]);
		// A constant-power fade-in is met through every step of its curve, from the top.
		const pair: GainClip = { volume: 1, fade_in: 0, fade_out: 0, transition_in: { kind: 'crossfade', duration: 1 }, power_pair: true };
		const curve = gainAutomationBack(pair, 10, 15, 15).map((p) => p.time);
		expect(curve).toEqual([...fadeInTimes(pair, 10)].reverse().concat(10));
		expect(gainAutomationBack(pair, 10, 15, 15).at(-1)!.value).toBe(0);
	});
});
