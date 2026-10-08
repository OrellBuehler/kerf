import { describe, expect, test } from 'bun:test';
import {
	clipGainAt,
	fadeInOf,
	fadeOutOf,
	gainAtFullScale,
	gainBreakpoints,
	type GainClip,
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
		const near = (got: number[], want: number[]) => {
			expect(got.length).toBe(want.length);
			got.forEach((t, i) => expect(t).toBeCloseTo(want[i], 9));
		};
		near(gainBreakpoints(clip(), 10, 15, 10), [10.2, 11.1, 11.7, 12.2, 15]);
		near(gainBreakpoints(clip({ fade_in: 0.5, fade_out: 1 }), 10, 15, 11), [11.1, 11.7, 12.2, 14, 15]);
		near(gainBreakpoints(clip(), 10, 15, 14), [15]);
		expect(gainBreakpoints(clip({ channels: undefined }), 10, 15, 10)).toEqual([]);
	});
});
