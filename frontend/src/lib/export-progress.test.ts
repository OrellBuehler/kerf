import { describe, expect, test } from 'bun:test';
import { exportPartLabel, fmtSpan, progressLine, statusText } from './export-progress';
import type { ExportProgress } from './types';

const at = (over: Partial<ExportProgress>): ExportProgress => ({ fraction: 0, elapsed_secs: 0, ...over });

describe('progressLine', () => {
	test('an export that has not reported is starting, with no count', () => {
		expect(progressLine(null)).toEqual({ value: null, text: 'Starting…' });
	});

	test('a queued export says what it waits for and has no count', () => {
		expect(progressLine(at({ waiting: 'waiting for the preview proxy' }))).toEqual({
			value: null,
			text: 'waiting for the preview proxy…'
		});
		// Waiting wins over a stage the same report carries.
		expect(progressLine(at({ waiting: 'rendering animated titles', stage: 'metering' })).value).toBeNull();
	});

	test('metering the mix counts its share of the bar', () => {
		expect(progressLine(at({ fraction: 0.04, stage: 'metering' }))).toEqual({ value: 0.04, text: 'Metering loudness · 4%' });
	});

	test('ffmpeg opening the footage is an indeterminate bar with the time it has taken', () => {
		const line = progressLine(at({ fraction: 0.08, stage: 'starting', elapsed_secs: 12.4 }));
		expect(line).toEqual({ value: null, text: 'Opening the footage… 12s' });
		expect(progressLine(at({ stage: 'starting', elapsed_secs: 125 })).text).toBe('Opening the footage… 2m 05s');
		expect(progressLine(at({ stage: 'starting', elapsed_secs: NaN })).text).toBe('Opening the footage…');
	});

	test('the render is the percentage and the time left, as it always was', () => {
		expect(progressLine(at({ fraction: 0.456, stage: 'rendering', eta_secs: 83 }))).toEqual({
			value: 0.456,
			text: '46% · 1m 23s left'
		});
		expect(progressLine(at({ fraction: 0.5 })).text).toBe('50%');
		// A report from a backend that predates stages is a render.
		expect(progressLine(at({ fraction: 0.5, eta_secs: 7 })).text).toBe('50% · 7s left');
	});

	test('several files say which one, or that all of them are going at once', () => {
		expect(progressLine(at({ fraction: 0.5, variant: 1, total: 3, stage: 'rendering' })).text).toBe('2/3 · 50%');
		expect(progressLine(at({ fraction: 0.25, variant: 0, total: 3, together: true, stage: 'rendering' })).text).toBe(
			'3 at once · 25%'
		);
		expect(progressLine(at({ fraction: 0.5, variant: 0, total: 1, stage: 'rendering' })).text).toBe('50%');
	});

	test('closing the file and reading it back are told as what they are', () => {
		expect(progressLine(at({ fraction: 0.99, stage: 'finishing' })).text).toBe('Finishing the file…');
		expect(progressLine(at({ fraction: 0.98, stage: 'measuring' })).text).toBe('Checking the file…');
	});

	test('an export rendered in parts says which part, then that it is encoding', () => {
		const part = { index: 2, total: 5, encoding: false };
		expect(progressLine(at({ fraction: 0.3, stage: 'rendering', part })).text).toBe('rendering part 2 of 5 · 30%');
		expect(progressLine(at({ fraction: 0.3, stage: 'starting', elapsed_secs: 3, part })).text).toBe(
			'rendering part 2 of 5 · Opening the footage… 3s'
		);
		expect(progressLine(at({ fraction: 0.7, stage: 'rendering', eta_secs: 40, part: { index: 5, total: 5, encoding: true } })).text).toBe(
			'encoding · 70% · 40s left'
		);
		expect(statusText(at({ fraction: 0.3, stage: 'rendering', part }))).toBe('Exporting 30% · rendering part 2 of 5');
		expect(statusText(at({ fraction: 0.3, stage: 'starting', part }))).toBe('Export opening the footage… (rendering part 2 of 5)');
		// An export that runs whole says nothing of parts.
		expect(exportPartLabel(at({}))).toBe('');
		expect(exportPartLabel(at({ part: null }))).toBe('');
		expect(exportPartLabel(null)).toBe('');
	});

	test('a fraction outside 0..1 never shows as more than 100 %', () => {
		expect(progressLine(at({ fraction: 1.4 })).text).toBe('100%');
		expect(progressLine(at({ fraction: -0.2 })).text).toBe('0%');
	});
});

describe('statusText', () => {
	test('the status bar names the stage in a few words', () => {
		expect(statusText(null)).toBe('Exporting 0%');
		expect(statusText(at({ waiting: 'waiting for another job to finish' }))).toBe('Export waiting for another job to finish…');
		expect(statusText(at({ fraction: 0.04, stage: 'metering' }))).toBe('Metering loudness 4%');
		expect(statusText(at({ stage: 'starting' }))).toBe('Export opening the footage…');
		expect(statusText(at({ fraction: 0.456, stage: 'rendering' }))).toBe('Exporting 46%');
		expect(statusText(at({ fraction: 0.99, stage: 'finishing' }))).toBe('Finishing the file…');
		expect(statusText(at({ fraction: 0.98, stage: 'measuring' }))).toBe('Checking the file…');
	});
});

describe('fmtSpan', () => {
	test('seconds under a minute, minutes and seconds past it', () => {
		expect(fmtSpan(0)).toBe('0s');
		expect(fmtSpan(59.4)).toBe('59s');
		expect(fmtSpan(60)).toBe('1m 00s');
		expect(fmtSpan(3725)).toBe('62m 05s');
		expect(fmtSpan(-4)).toBe('0s');
		expect(fmtSpan(null)).toBe('');
		expect(fmtSpan(Infinity)).toBe('');
	});
});
