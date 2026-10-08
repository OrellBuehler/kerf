import { describe, expect, test } from 'bun:test';
import {
	ceilingLabel,
	ceilingToPos,
	clampCeiling,
	clampMasterVolume,
	DEFAULT_MASTER,
	estimateLevels,
	isNeutralMaster,
	levelNotes,
	MASTER_DEFAULT_CEILING_DB,
	MASTER_MIN_CEILING_DB,
	masterOf,
	nudgeCeiling,
	posToCeiling,
	trackRenders
} from './levels';
import type { Asset, LevelReading, Loudness, MasterBus, Timeline, TrackLevels } from './types';

const reading = (integrated: number | null, truePeak: number | null): LevelReading => ({
	integrated_lufs: integrated,
	loudness_range_lu: 0,
	short_term_max_lufs: null,
	peak_dbfs: null,
	true_peak_dbtp: truePeak
});

describe('the master bus', () => {
	test('an absent or partial master is the default, and the default is neutral', () => {
		expect(masterOf({})).toEqual(DEFAULT_MASTER);
		expect(masterOf({ master: { volume: 0.5 } as never })).toEqual({ volume: 0.5, limiter: false, ceiling_db: -1.5 });
		expect(isNeutralMaster(DEFAULT_MASTER)).toBe(true);
		// A ceiling the limiter is not using changes nothing, like the engine.
		expect(isNeutralMaster({ ...DEFAULT_MASTER, ceiling_db: -6 })).toBe(true);
		expect(isNeutralMaster({ ...DEFAULT_MASTER, limiter: true })).toBe(false);
		expect(isNeutralMaster({ ...DEFAULT_MASTER, volume: 0.9 })).toBe(false);
	});

	test('controls clamp where the engine does', () => {
		expect(clampMasterVolume(9)).toBe(4);
		expect(clampMasterVolume(-1)).toBe(0);
		expect(clampMasterVolume(0.7)).toBe(0.7);
		expect(clampCeiling(-90)).toBe(-24);
		expect(clampCeiling(6)).toBe(0);
		expect(clampCeiling(-3)).toBe(-3);
	});
});

// The same cases, and the same words, as `the_levels_notes_*` in kerf-core's model tests.
describe('levelNotes', () => {
	const notes = (i: number | null, tp: number | null) => levelNotes(reading(i, tp), []);

	test('judges the mix against the streaming target', () => {
		expect(notes(-14, -3)[0]).toContain('close to');
		expect(notes(-13.2, -3)[0]).toContain('close to');
		expect(notes(-9, -3)[0]).toContain('5.0 LU over');
		expect(notes(-19, -3)[0]).toContain('5.0 LU under');
		expect(notes(-16.5, -3)[0]).toContain('close to');
		expect(notes(null, null)[0]).toContain('silent');
	});

	test('the true-peak ceiling is its own note and names the fix', () => {
		const hot = notes(-14, 0.4);
		expect(hot).toHaveLength(2);
		expect(hot[1]).toContain('0.4 dBTP');
		expect(hot[1]).toContain('set_master_limiter');
		expect(notes(-14, -1)).toHaveLength(1);
	});

	test('a true peak over the line with the limiter already on lowers its ceiling instead of asking for it again', () => {
		const on = (ceiling_db: number): MasterBus => ({ volume: 1, limiter: true, ceiling_db });
		const hot = (tp: number, bus: MasterBus) => levelNotes(reading(-14, tp), [], bus);
		// 11 kHz at a -1 dBFS ceiling, as measured: -0.2 dBTP is 0.8 over, so -2.3. Word for
		// word the string in kerf-core's `a_true_peak_over_the_ceiling_with_the_limiter_on_…`.
		const n = hot(-0.2, on(-1));
		expect(n).toHaveLength(2);
		expect(n[1]).toBe(
			'True peak -0.2 dBTP is over the -1 dBTP platforms ask for and can clip when re-encoded. The master limiter is already on, but it holds the sample peak at -1.0 dBFS and the peak between samples runs above that. Lower its ceiling to about -2.3 dBFS (set_master_limiter).'
		);
		expect(hot(0.4, on(MASTER_DEFAULT_CEILING_DB))[1]).toContain('about -3.4 dBFS');
		// Never below what the limiter can be given; at the floor there is nothing to lower.
		expect(hot(10, on(-20))[1]).toContain('about -24.0 dBFS');
		const floor = hot(0.4, on(MASTER_MIN_CEILING_DB));
		expect(floor[1]).toContain('lowest ceiling');
		expect(floor[1]).not.toContain('about');
		expect(hot(0.4, on(Number.NaN))[1]).toContain('about -3.4 dBFS');
		// Limiter off: still told to turn it on, and a stored ceiling alone is not "on".
		expect(hot(0.4, { volume: 1, limiter: false, ceiling_db: -6 })[1]).toContain('Turn on the master limiter');
		expect(hot(0.4, DEFAULT_MASTER)[1]).toBe(
			'True peak 0.4 dBTP is over the -1 dBTP platforms ask for and can clip when re-encoded. Turn on the master limiter (set_master_limiter) or lower the master.'
		);
		// Under the line: no note, limiter or not.
		expect(hot(-1, on(-1.5))).toHaveLength(1);
	});

	test('a track that peaks over full scale is named, and a cut with no audio says so', () => {
		const strip = (name: string, peak: number): TrackLevels => ({
			track_id: name,
			name,
			kind: 'audio',
			ducked: false,
			heard: true,
			level: { ...reading(null, null), peak_dbfs: peak }
		});
		const n = levelNotes(reading(-14, -3), [strip('A1', -6), strip('Music', 1.5)]);
		expect(n).toHaveLength(2);
		expect(n[1]).toContain('Music');
		expect(n[1]).toContain('+1.5 dBFS');
		// The mix is float, so a hot strip is not a clip yet: the note must not claim one.
		expect(n[1]).not.toContain('will clip');
		expect(levelNotes(null, [])).toEqual(['The cut has no audio to measure.']);
	});
});

describe('estimateLevels', () => {
	const asset = (id: string, audio = true): Asset => ({
		id,
		path: `/${id}.mp4`,
		name: `${id}.mp4`,
		duration: 60,
		streams: audio ? [{ index: 0, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 }] : [],
		imported_at: ''
	});
	const clip = (asset_id: string, start: number, len: number, volume = 1) => ({
		id: `c-${asset_id}-${start}`,
		asset_id,
		source_in: 0,
		source_out: len,
		timeline_start: start,
		volume,
		fade_in: 0,
		fade_out: 0
	});
	const loud: Record<string, Loudness> = {
		a: { integrated_lufs: -20, loudness_range: 5, true_peak_dbtp: -6, threshold_lufs: -30 }
	};
	const timeline = (extra: Partial<Timeline> = {}, trackExtra: object = {}): Timeline => ({
		tracks: [
			{ id: 't1', kind: 'audio', name: 'A1', clips: [clip('a', 0, 10)], ...trackExtra },
			{ id: 't2', kind: 'video', name: 'V1', clips: [clip('v', 0, 10)] }
		],
		...extra
	});
	const run = (tl: Timeline, opts = {}) => estimateLevels(tl, [asset('a'), asset('v', false)], (id) => loud[id], opts);

	test('is flagged as an estimate and reads the track through its fader', () => {
		const levels = run(timeline());
		expect(levels.estimated).toBe(true);
		expect(levels.tracks).toHaveLength(1); // the silent video track has no strip
		expect(levels.tracks[0].level?.integrated_lufs).toBeCloseTo(-20, 5);
		expect(levels.master?.integrated_lufs).toBeCloseTo(-20, 5);
		expect(levels.master?.true_peak_dbtp).toBeCloseTo(-6, 5);
		const half = run(timeline({}, { volume: 0.5 }));
		expect(half.tracks[0].level?.integrated_lufs).toBeCloseTo(-26.02, 1);
	});

	test('a hard pan costs one channel of power, and not the peak', () => {
		const left = run(timeline({}, { pan: -1 }));
		expect(left.tracks[0].level?.integrated_lufs).toBeCloseTo(-23.01, 1);
		expect(left.master?.true_peak_dbtp).toBeCloseTo(-6, 5);
	});

	test('the master fader moves the mix and not the strip', () => {
		const levels = run(timeline({ master: { volume: 0.5, limiter: false, ceiling_db: -1 } }));
		expect(levels.master?.integrated_lufs).toBeCloseTo(-26.02, 1);
		expect(levels.tracks[0].level?.integrated_lufs).toBeCloseTo(-20, 5);
	});

	test('the limiter holds the ceiling and takes less than the overshoot off the loudness', () => {
		const levels = run(timeline({ master: { volume: 4, limiter: true, ceiling_db: -12 } }));
		// A fader of 4 is +12.04 dB: the -6 dBTP peak goes to +6.04 and is held at -12, and
		// the loudness gives back half of that overshoot.
		const gain = 20 * Math.log10(4);
		expect(levels.master?.true_peak_dbtp).toBe(-12);
		expect(levels.master?.integrated_lufs).toBeCloseTo(-20 + gain - (-6 + gain + 12) * 0.5, 5);
	});

	test('loudnorm lands on the target', () => {
		const levels = run(timeline(), { loudnorm: true });
		expect(levels.master?.integrated_lufs).toBe(-14);
		expect(levels.loudnorm).toBe(true);
		expect(levels.master?.true_peak_dbtp ?? 0).toBeLessThanOrEqual(-1.5);
	});

	test('a muted track is not heard and a cut with only that has no audio', () => {
		const tl = timeline({}, { muted: true });
		expect(trackRenders(tl, tl.tracks[0])).toBe(false);
		const levels = run(tl);
		expect(levels.tracks[0].heard).toBe(false);
		expect(levels.tracks[0].level).toBeNull();
		expect(levels.master).toBeNull();
		expect(levels.notes).toEqual(['The cut has no audio to measure.']);
	});

	test('a range measures only its span, and short spans have no short-term maximum', () => {
		const levels = run(timeline(), { range: { start: 2, end: 4 } });
		expect(levels.duration).toBe(2);
		expect(levels.master?.short_term_max_lufs).toBeNull();
		expect(run(timeline()).master?.short_term_max_lufs).not.toBeNull();
	});

	test('a solo shadows the other tracks of its kind', () => {
		const tl: Timeline = {
			tracks: [
				{ id: 't1', kind: 'audio', name: 'A1', clips: [clip('a', 0, 10)], solo: true },
				{ id: 't2', kind: 'audio', name: 'A2', clips: [clip('a', 0, 10)] }
			]
		};
		const levels = run(tl);
		expect(levels.tracks.map((t) => t.heard)).toEqual([true, false]);
		expect(levels.master?.integrated_lufs).toBeCloseTo(-20, 5);
	});
});

describe('the limiter ceiling slider', () => {
	test('travels from -24 on the left to 0 on the right, in half dB', () => {
		expect(ceilingToPos(-24)).toBe(0);
		expect(ceilingToPos(0)).toBe(1);
		expect(ceilingToPos(-12)).toBe(0.5);
		expect(ceilingToPos(-90)).toBe(0);
		expect(ceilingToPos(6)).toBe(1);
		expect(posToCeiling(0)).toBe(-24);
		expect(posToCeiling(1)).toBe(0);
		expect(posToCeiling(0.5)).toBe(-12);
		expect(posToCeiling(0.9583)).toBe(-1);
		expect(posToCeiling(-2)).toBe(-24);
		expect(posToCeiling(9)).toBe(0);
		for (const db of [-24, -12.5, -6, -3, -1, -0.5, 0]) expect(posToCeiling(ceilingToPos(db))).toBe(db);
	});

	test('nudges on the grid of its size, within the range', () => {
		expect(nudgeCeiling(-1, 1)).toBe(-0.5);
		expect(nudgeCeiling(-1, -1)).toBe(-1.5);
		expect(nudgeCeiling(-1.2, 1)).toBe(-1);
		expect(nudgeCeiling(-1, 1, 'fine')).toBe(-0.9);
		expect(nudgeCeiling(-1, -1, 'coarse')).toBe(-3);
		expect(nudgeCeiling(-1, -1, 'page')).toBe(-6);
		expect(nudgeCeiling(0, 1)).toBe(0);
		expect(nudgeCeiling(-24, -1)).toBe(-24);
		expect(Object.is(nudgeCeiling(-0.5, 1), 0)).toBe(true);
	});

	test('reads like the limiter’s label', () => {
		expect(ceilingLabel(-1)).toBe('-1.0 dBFS');
		expect(ceilingLabel(0)).toBe('0.0 dBFS');
		expect(ceilingLabel(-0.01)).toBe('0.0 dBFS');
	});
});
