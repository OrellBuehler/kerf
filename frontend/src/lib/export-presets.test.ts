import { describe, expect, test } from 'bun:test';
import {
	FRAME_RATES,
	PRESETS,
	applyPreset,
	buildSummary,
	frameRateOptions,
	LOUDNESS_TARGETS,
	loudnessChoice,
	withLoudness
} from './export-presets';
import type { LoudnessPreset } from './types';

describe('the loudness target', () => {
	test('lists the engine presets, in the order the select shows them', () => {
		expect(LOUDNESS_TARGETS.map((t) => t.id)).toEqual(['off', 'youtube', 'spotify', 'apple', 'broadcast']);
		expect(LOUDNESS_TARGETS.map((t) => t.lufs)).toEqual([null, -14, -14, -16, -23]);
		// Every label says the number the engine normalizes to.
		for (const t of LOUDNESS_TARGETS) if (t.lufs != null) expect(t.label).toContain(`${t.lufs}`.replace('-', '−'));
	});

	test('an export is off until a target is chosen', () => {
		expect(loudnessChoice(applyPreset('web_1080p'))).toBe('off');
		expect(loudnessChoice({})).toBe('off');
		expect(loudnessChoice({ loudness: null })).toBe('off');
	});

	test('the old loudnorm flag is the YouTube target, and loudness wins over it as in the engine', () => {
		expect(loudnessChoice({ loudnorm: true })).toBe('youtube');
		expect(loudnessChoice({ loudnorm: false })).toBe('off');
		expect(loudnessChoice({ loudnorm: true, loudness: 'broadcast' })).toBe('broadcast');
		// `null` is "not set", not "off": the flag still speaks (the engine's `Option::or`).
		expect(loudnessChoice({ loudnorm: true, loudness: null })).toBe('youtube');
	});

	test('choosing a target writes it and clears the flag, so Off really is off', () => {
		const flagged = { ...applyPreset('web_1080p'), loudnorm: true };
		for (const id of ['youtube', 'spotify', 'apple', 'broadcast'] as LoudnessPreset[]) {
			const next = withLoudness(flagged, id);
			expect(next.loudness).toBe(id);
			expect(next.loudnorm).toBe(false);
			expect(loudnessChoice(next)).toBe(id);
		}
		const off = withLoudness(flagged, 'off');
		expect(off.loudness).toBeNull();
		expect(loudnessChoice(off)).toBe('off');
		// Nothing else about the options moves.
		expect({ ...off, loudness: undefined, loudnorm: undefined }).toEqual({ ...flagged, loudness: undefined, loudnorm: undefined });
	});

	test('the summary names the target next to the audio', () => {
		const base = applyPreset('web_1080p');
		expect(buildSummary(base, true, true)).not.toContain('LUFS');
		expect(buildSummary(withLoudness(base, 'apple'), true, true)).toContain('-16 LUFS');
		expect(buildSummary({ ...base, loudnorm: true }, true, true)).toContain('-14 LUFS');
		// No sound in the file, no target to name.
		expect(buildSummary(withLoudness(base, 'apple'), true, false)).not.toContain('LUFS');
		expect(buildSummary({ ...withLoudness(base, 'apple'), include_audio: false }, true, true)).not.toContain('LUFS');
	});
});

describe('the frame-rate select', () => {
	// The dialog's select shows `String(fps)` (or "source"); a value with no option renders blank.
	const shown = (fps: number | null | undefined) =>
		frameRateOptions(fps).some((f) => (f.value ? String(f.value) : 'source') === (fps ? String(fps) : 'source'));

	test('every preset opens on a frame rate the select has an option for', () => {
		for (const p of PRESETS) expect(shown(applyPreset(p.id).fps), p.id).toBe(true);
	});

	test('the GIF preset lands on 15, and 12 and 10 are there to go lower', () => {
		const values = FRAME_RATES.map((f) => f.value);
		expect(applyPreset('gif').fps).toBe(15);
		expect(values).toContain(15);
		expect(values).toContain(12);
		expect(values).toContain(10);
	});

	test('a rate the list does not carry is added rather than leaving the select blank', () => {
		expect(shown(18)).toBe(true);
		expect(frameRateOptions(18).at(-1)).toEqual({ label: '18', value: 18 });
		expect(frameRateOptions(18)).toHaveLength(FRAME_RATES.length + 1);
	});

	test('a listed rate, or the source rate, adds nothing', () => {
		expect(frameRateOptions(30)).toBe(FRAME_RATES);
		expect(frameRateOptions(null)).toBe(FRAME_RATES);
		expect(frameRateOptions(undefined)).toBe(FRAME_RATES);
	});
});
