import { describe, expect, test } from 'bun:test';
import {
	clampCeiling,
	clampMasterVolume,
	DEFAULT_MASTER,
	estimateLevels,
	isNeutralMaster,
	levelNotes,
	masterOf,
	trackRenders
} from './levels';
import type { Asset, LevelReading, Loudness, Timeline, TrackLevels } from './types';

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
		expect(masterOf({ master: { volume: 0.5 } as never })).toEqual({ volume: 0.5, limiter: false, ceiling_db: -1 });
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

	test('a track that clips the sum is named, and a cut with no audio says so', () => {
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
