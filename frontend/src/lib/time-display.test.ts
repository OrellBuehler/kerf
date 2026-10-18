import { afterEach, describe, expect, test } from 'bun:test';
import './test-runes';

// The reactive wrapper: one mode, every readout. (`editor.fps` is 30 with nothing on the timeline.)
const { timeDisplay } = await import('./time-display.svelte');

afterEach(() => {
	timeDisplay.mode = 'timecode';
});

describe('timeDisplay', () => {
	test('reads frame timecode until it is told otherwise', () => {
		expect(timeDisplay.mode).toBe('timecode');
		expect(timeDisplay.fmt(12.5)).toBe('00:00:12:15');
		expect(timeDisplay.range(0, 8)).toBe('00:00:00:00 – 00:00:08:00');
		expect(timeDisplay.field(139 / 30)).toBe('00:00:04:19');
	});

	test('one switch changes every readout', () => {
		timeDisplay.mode = 'seconds';
		expect(timeDisplay.fmt(12.5)).toBe('0:12.5');
		expect(timeDisplay.range(0, 8)).toBe('0:00.0 – 0:08.0');
		expect(timeDisplay.field(139 / 30)).toBe('4.633');
	});

	test('a source reads at its own rate', () => {
		expect(timeDisplay.fmt(1, 24)).toBe('00:00:01:00');
		expect(timeDisplay.fmt(0.5, 24)).toBe('00:00:00:12');
	});

	test('typed text is read in either mode, and lands on a frame', () => {
		for (const mode of ['timecode', 'seconds'] as const) {
			timeDisplay.mode = mode;
			expect(timeDisplay.parse('00:00:12:15')).toBe(12.5);
			expect(timeDisplay.parse('12.5')).toBe(12.5);
			expect(timeDisplay.parseFrame('4.62')).toBeCloseTo(139 / 30, 9);
			expect(timeDisplay.parse('not a time')).toBeNull();
		}
	});
});
