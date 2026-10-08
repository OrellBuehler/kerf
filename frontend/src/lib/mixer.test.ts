import { describe, expect, test } from 'bun:test';
import {
	dbToFader,
	dbToGain,
	effectiveGain,
	FADER_FLOOR_DB,
	FADER_FLOOR_POS,
	FADER_MARKS,
	FADER_MAX_DB,
	FADER_UNITY_POS,
	faderTicks,
	faderToDb,
	faderToGain,
	gainLabel,
	gainToDb,
	gainToFader,
	isUnityMix,
	labelTicks,
	MAX_GAIN,
	nudgeGain,
	nudgePan,
	panGains,
	panLabel,
	panSides,
	panToPos,
	posToPan,
	sideLabel,
	snapDb
} from './mixer';

describe('panGains', () => {
	test('centre is exactly unity on both sides', () => {
		// The same assertion as the Rust test: an untouched track must not be
		// touched, or every existing mix comes back changed.
		expect(panGains(0)).toEqual([1, 1]);
		expect(panGains(undefined as never)).toEqual([1, 1]);
	});

	test('is a balance, never a boost', () => {
		expect(panGains(-1)).toEqual([1, 0]);
		expect(panGains(1)).toEqual([0, 1]);
		expect(panGains(-0.5)).toEqual([1, 0.5]);
		for (const p of [-1, -0.5, 0, 0.25, 1]) {
			const [l, r] = panGains(p);
			expect(l).toBeLessThanOrEqual(1);
			expect(r).toBeLessThanOrEqual(1);
		}
	});

	test('clamps rather than inverting out of range', () => {
		expect(panGains(9)).toEqual([0, 1]);
		expect(panGains(-9)).toEqual([1, 0]);
	});
});

describe('labels', () => {
	test('a fader reads in dB, silence included', () => {
		expect(gainLabel(1)).toBe('0.0 dB'); // unity reads as 0, the way a mixer shows it
		expect(gainLabel(0.5)).toBe('-6.0 dB');
		expect(gainLabel(0)).toBe('−∞ dB');
		expect(gainLabel(2)).toBe('+6.0 dB');
	});

	test('a pan reads as a mixer shows it', () => {
		expect(panLabel(0)).toBe('centre');
		expect(panLabel(-1)).toBe('L100');
		expect(panLabel(0.3)).toBe('R30');
	});
});

describe('isUnityMix', () => {
	test('an unset mix is unity', () => {
		expect(isUnityMix(undefined, undefined)).toBe(true);
		expect(isUnityMix(1, 0)).toBe(true);
		expect(isUnityMix(0.5, 0)).toBe(false);
		expect(isUnityMix(1, -0.2)).toBe(false);
	});
});

describe('dB and linear gain', () => {
	test('round-trip, with silence at -Infinity', () => {
		expect(gainToDb(1)).toBe(0);
		expect(gainToDb(0.5)).toBeCloseTo(-6.0206, 4);
		expect(gainToDb(2)).toBeCloseTo(6.0206, 4);
		expect(gainToDb(0)).toBe(-Infinity);
		expect(dbToGain(-Infinity)).toBe(0);
		expect(dbToGain(0)).toBe(1);
		for (const v of [0.01, 0.25, 0.7, 1, 1.9, 3.9]) expect(dbToGain(gainToDb(v))).toBeCloseTo(v, 12);
	});

	test('agrees with the label a fader reads', () => {
		expect(gainLabel(dbToGain(-12))).toBe('-12.0 dB');
		expect(gainLabel(dbToGain(3))).toBe('+3.0 dB');
	});
});

describe('effectiveGain', () => {
	test('is the clip gain through the track fader, both unity by default', () => {
		expect(effectiveGain(undefined, undefined)).toBe(1);
		expect(effectiveGain(0.5, 0.5)).toBe(0.25);
		expect(effectiveGain(2, 0.5)).toBe(1);
		expect(effectiveGain(1, undefined)).toBe(1);
		expect(effectiveGain(undefined, 0.8)).toBe(0.8);
	});

	test('never negative', () => {
		expect(effectiveGain(-1, 1)).toBe(0);
		expect(effectiveGain(1, -2)).toBe(0);
	});
});

// The fader's travel. Both faders — the timeline header's and the Mixer's — go through
// this one mapping, so a level is the same place on either.
describe('the fader taper', () => {
	test('unity sits at three quarters, the top at the track maximum, silence at the bottom', () => {
		expect(dbToFader(0)).toBe(FADER_UNITY_POS);
		expect(dbToFader(FADER_MAX_DB)).toBe(1);
		expect(dbToFader(-Infinity)).toBe(0);
		expect(faderToDb(0)).toBe(-Infinity);
		expect(faderToDb(FADER_UNITY_POS)).toBeCloseTo(0, 12);
		expect(faderToDb(1)).toBeCloseTo(FADER_MAX_DB, 12);
		expect(FADER_MAX_DB).toBeCloseTo(6.0206, 4);
	});

	test('is the printed scale at its marks', () => {
		for (const [pos, db] of FADER_MARKS) {
			expect(dbToFader(db)).toBeCloseTo(pos, 12);
			expect(faderToDb(pos)).toBeCloseTo(db, 9);
		}
		// Finer around unity than down the scale: 10 dB near the top is more travel than near the bottom.
		expect(dbToFader(0) - dbToFader(-10)).toBeGreaterThan(dbToFader(-40) - dbToFader(-50));
	});

	test('is strictly increasing: a higher place is always a louder level', () => {
		let last = faderToDb(FADER_FLOOR_POS);
		for (let i = 31; i <= 1000; i++) {
			const db = faderToDb(i / 1000);
			expect(db).toBeGreaterThan(last);
			last = db;
		}
	});

	test('the bottom stop is silence, and under the floor mark a level rests on it', () => {
		expect(faderToDb(FADER_FLOOR_POS - 0.001)).toBe(-Infinity);
		expect(faderToDb(FADER_FLOOR_POS)).toBeCloseTo(FADER_FLOOR_DB, 12);
		expect(dbToFader(-75)).toBe(FADER_FLOOR_POS);
		expect(dbToFader(Number.NaN)).toBe(0);
	});

	test('clamps its places and its top', () => {
		expect(faderToDb(-3)).toBe(-Infinity);
		expect(faderToDb(9)).toBeCloseTo(FADER_MAX_DB, 12);
		expect(faderToDb(Number.NaN)).toBe(-Infinity);
		expect(dbToFader(30)).toBe(1);
	});

	test('a gain round-trips through the travel to within the label’s tenth of a dB', () => {
		for (const g of [0.001, 0.01, 0.05, 0.25, 0.5, 0.7071, 1, 1.4125, 1.9]) {
			const back = faderToGain(gainToFader(g));
			expect(Math.abs(gainToDb(back) - gainToDb(g))).toBeLessThanOrEqual(0.06);
		}
		expect(faderToGain(gainToFader(0))).toBe(0);
		expect(faderToGain(gainToFader(1))).toBe(1);
	});

	test('what a drag writes is a tenth of a dB, and unity is caught', () => {
		expect(faderToGain(FADER_UNITY_POS)).toBe(1);
		// A hair either side of unity is unity: an untouched mix leaves the graph alone.
		expect(faderToGain(FADER_UNITY_POS + 0.004)).toBe(1);
		expect(faderToGain(FADER_UNITY_POS - 0.002)).toBe(1);
		expect(snapDb(-6.04)).toBe(-6);
		expect(snapDb(-6.06)).toBe(-6.1);
		expect(snapDb(0.2)).toBe(0);
		expect(snapDb(0.3)).toBe(0.3);
		expect(snapDb(-Infinity)).toBe(-Infinity);
		expect(gainLabel(faderToGain(dbToFader(-12.34)))).toBe('-12.3 dB');
	});

	test('the top stop is exactly the maximum, for a track and for the master', () => {
		expect(faderToGain(1)).toBe(MAX_GAIN);
		expect(faderToGain(0.999)).toBe(MAX_GAIN); // within the label's resolution of the top
		expect(faderToGain(1, 4)).toBe(4);
		expect(gainToFader(4, 4)).toBe(1);
		// +6 dB is lower on a +12 dB fader than on a +6 dB one.
		expect(gainToFader(2, 4)).toBeLessThan(gainToFader(2, 2));
		expect(gainToFader(2, 4)).toBeCloseTo(0.75 + 0.25 * 0.5, 3);
		expect(gainToFader(1, 4)).toBe(FADER_UNITY_POS);
	});

	test('a stored value above the top rests on it, and a round trip does not change what is stored', () => {
		// Controls clamp what they show, not what is stored: a clip at 3.0 shows at the top.
		expect(gainToFader(3)).toBe(1);
		expect(gainLabel(3)).toBe('+9.5 dB');
	});

	test('the scale marks sit where the fader puts them', () => {
		const ticks = faderTicks(FADER_MAX_DB);
		expect(ticks.map((t) => t.db)).toEqual([6, 3, 0, -6, -12, -20, -30, -40, -50, -60]);
		for (const t of ticks) expect(t.pos).toBe(dbToFader(t.db, FADER_MAX_DB));
		expect(faderTicks(gainToDb(4))[0].db).toBe(12);
	});
});

describe('nudging a fader', () => {
	const db = (g: number) => gainToDb(g);

	test('steps in dB, on the grid of its own size', () => {
		expect(db(nudgeGain(1, -1))).toBeCloseTo(-1, 9);
		expect(db(nudgeGain(1, 1))).toBeCloseTo(1, 9);
		// From an off-grid level a step lands on the next whole dB, not 1 dB past it.
		expect(db(nudgeGain(dbToGain(-6.4), 1))).toBeCloseTo(-6, 9);
		expect(db(nudgeGain(dbToGain(-6.4), -1))).toBeCloseTo(-7, 9);
		expect(db(nudgeGain(1, 1, 'fine'))).toBeCloseTo(0.1, 9);
		expect(db(nudgeGain(1, -1, 'coarse'))).toBeCloseTo(-3, 9);
		expect(db(nudgeGain(1, -1, 'page'))).toBeCloseTo(-6, 9);
		expect(db(nudgeGain(dbToGain(-12), 3))).toBeCloseTo(-9, 9);
	});

	test('crosses unity exactly, so a nudge can find the neutral mix', () => {
		expect(nudgeGain(dbToGain(-1), 1)).toBe(1);
		expect(nudgeGain(dbToGain(1), -1)).toBe(1);
		expect(nudgeGain(dbToGain(-0.1), 1, 'fine')).toBe(1);
		expect(nudgeGain(dbToGain(-1.5), 1, 'coarse')).toBe(1);
	});

	test('leaves silence for the floor mark, and returns to silence under it', () => {
		expect(db(nudgeGain(0, 1))).toBeCloseTo(FADER_FLOOR_DB, 9);
		expect(db(nudgeGain(0, 1, 'page'))).toBeCloseTo(FADER_FLOOR_DB, 9);
		expect(db(nudgeGain(0, 1, 'fine'))).toBeCloseTo(FADER_FLOOR_DB, 9);
		expect(nudgeGain(dbToGain(FADER_FLOOR_DB), -1)).toBe(0);
		expect(nudgeGain(0, -1)).toBe(0);
		// Something quieter than the fader can draw is as good as silence to leave from.
		expect(db(nudgeGain(dbToGain(-75), 1))).toBeCloseTo(FADER_FLOOR_DB, 9);
	});

	test('stops at the top, exactly the maximum', () => {
		expect(nudgeGain(2, 1)).toBe(MAX_GAIN);
		// From +5.5 the step lands on +6, a hair under the top; the next press reaches it.
		expect(nudgeGain(dbToGain(5.5), 1)).toBeCloseTo(dbToGain(6), 9);
		expect(nudgeGain(nudgeGain(dbToGain(5.5), 1), 1)).toBe(MAX_GAIN);
		expect(nudgeGain(dbToGain(5), 1)).toBeCloseTo(dbToGain(6), 9);
		expect(nudgeGain(4, 1, 'normal', 4)).toBe(4);
		expect(nudgeGain(dbToGain(11), 1, 'normal', 4)).toBeCloseTo(dbToGain(12), 9);
		// A stored value over the top comes back down to it, never up from it.
		expect(nudgeGain(3, -1)).toBe(MAX_GAIN);
		expect(nudgeGain(3, 1)).toBe(3); // up from past the top is nowhere, not down to it
	});

	test('every key press is a different level (a fader never sticks)', () => {
		let g = nudgeGain(0, 1);
		const seen = new Set<number>();
		for (let i = 0; i < 80 && g < MAX_GAIN; i++) {
			expect(seen.has(g)).toBe(false);
			seen.add(g);
			g = nudgeGain(g, 1);
		}
		expect(g).toBe(MAX_GAIN);
	});
});

describe('the pan travel', () => {
	test('centre is the middle, and the ends are the ends', () => {
		expect(panToPos(0)).toBe(0.5);
		expect(panToPos(-1)).toBe(0);
		expect(panToPos(1)).toBe(1);
		expect(panToPos(undefined as never)).toBe(0.5);
		expect(posToPan(0.5)).toBe(0);
		expect(posToPan(0)).toBe(-1);
		expect(posToPan(1)).toBe(1);
		expect(posToPan(Number.NaN)).toBe(0);
	});

	test('a drag writes percents and catches the centre', () => {
		expect(posToPan(0.5 + 0.004)).toBe(0);
		expect(posToPan(0.5 - 0.01)).toBe(0);
		expect(posToPan(0.65)).toBe(0.3);
		expect(posToPan(0.2)).toBe(-0.6);
		for (const p of [-1, -0.45, -0.3, 0.07, 0.5, 1]) expect(posToPan(panToPos(p))).toBe(p);
	});

	test('nudges on the grid of its size, within the ends, and through centre exactly', () => {
		expect(nudgePan(0, 1)).toBe(0.05);
		expect(nudgePan(0, -1)).toBe(-0.05);
		expect(nudgePan(-0.05, 1)).toBe(0);
		expect(nudgePan(0.03, 1)).toBe(0.05);
		expect(nudgePan(0.03, -1)).toBe(0);
		expect(nudgePan(0.3, 1, 'fine')).toBe(0.31);
		expect(nudgePan(0, 1, 'coarse')).toBe(0.25);
		expect(nudgePan(0.95, 1, 'coarse')).toBe(1);
		expect(nudgePan(-0.9, -1, 'coarse')).toBe(-1);
		expect(Object.is(nudgePan(0.05, -1), 0)).toBe(true); // never −0
	});

	test('says what the balance leaves on each side', () => {
		expect(panSides(0)).toEqual({ left: 0, right: 0 });
		const left = panSides(-1);
		expect(left.left).toBe(0);
		expect(left.right).toBe(-Infinity);
		const quarter = panSides(0.5);
		expect(quarter.left).toBeCloseTo(-6.0206, 3);
		expect(quarter.right).toBe(0);
		expect(sideLabel(panSides(0.3).left)).toBe('-3.1 dB');
		expect(sideLabel(-Infinity)).toBe('−∞ dB');
		expect(sideLabel(0)).toBe('0.0 dB');
		expect(sideLabel(-0.01)).toBe('0.0 dB');
	});
});

describe('which marks get a label', () => {
	const ticks = faderTicks(FADER_MAX_DB);

	test('all of them when there is room', () => {
		expect(labelTicks(ticks, 5000).map((t) => t.db)).toEqual(ticks.map((t) => t.db));
	});

	test('unity first when there is not — the mark every other is read against', () => {
		for (const px of [40, 60, 80, 100, 140, 200]) {
			const kept = labelTicks(ticks, px);
			expect(kept.map((t) => t.db)).toContain(0);
			// No two labels closer than the gap.
			for (const a of kept) for (const b of kept) if (a !== b) expect(Math.abs(a.pos - b.pos) * px).toBeGreaterThanOrEqual(12);
		}
	});

	test('a taller fader earns more labels, never fewer', () => {
		let last = 0;
		for (const px of [30, 60, 90, 120, 160, 220, 400]) {
			const n = labelTicks(ticks, px).length;
			expect(n).toBeGreaterThanOrEqual(last);
			last = n;
		}
	});

	test('keeps the order it was given', () => {
		const kept = labelTicks(ticks, 150).map((t) => t.db);
		expect(kept).toEqual([...kept].sort((a, b) => b - a));
	});
});
