import { describe, expect, test } from 'bun:test';
import { exportButtonState, NOTHING_TO_EXPORT } from './export-button';

describe('the Export button', () => {
	test('is off for an empty cut, and says why', () => {
		expect(exportButtonState({ clips: 0, running: false })).toEqual({ disabled: true, reason: NOTHING_TO_EXPORT });
	});

	test('is on once a clip is on the timeline', () => {
		expect(exportButtonState({ clips: 1, running: false })).toEqual({ disabled: false });
		expect(exportButtonState({ clips: 12, running: false })).toEqual({ disabled: false });
	});

	test('stays on while a render runs, even if the cut was emptied under it: it reopens the dialog with Stop', () => {
		expect(exportButtonState({ clips: 0, running: true })).toEqual({ disabled: false });
	});
});
