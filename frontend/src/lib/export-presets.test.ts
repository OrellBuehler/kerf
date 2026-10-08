import { describe, expect, test } from 'bun:test';
import { applyPreset, buildSummary, LOUDNESS_TARGETS, loudnessChoice, withLoudness } from './export-presets';
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
