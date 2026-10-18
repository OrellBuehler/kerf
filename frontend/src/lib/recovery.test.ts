import { describe, expect, test } from 'bun:test';
import { recoveryChoice, recoveryCopy, recoverySummary } from './recovery';
import type { RecoveryInfo } from './types';

const info = (over: Partial<RecoveryInfo> = {}): RecoveryInfo => ({
	id: 'abc',
	modified_at: '2026-10-18T09:30:00Z',
	assets: 2,
	clips: 3,
	edits: 7,
	last_edit: 'Add clip',
	...over
});

describe('the recovery prompt', () => {
	test('says what the session holds, counted', () => {
		expect(recoverySummary(info())).toBe('3 clips, 2 media files');
		expect(recoverySummary(info({ clips: 1, assets: 1 }))).toBe('1 clip, 1 media file');
		expect(recoverySummary(info({ clips: 0, assets: 0, edits: 1 }))).toBe('1 edit');
		expect(recoverySummary(info({ clips: 0, assets: 0, edits: 0 }))).toBe('');
	});

	test('names when it was last changed and what is in it', () => {
		const copy = recoveryCopy(info(), () => 'yesterday');
		expect(copy.title).toBe('Recover unsaved project?');
		expect(copy.message).toBe('Kerf closed before an untitled project was saved (last changed yesterday). It holds 3 clips, 2 media files.');
		expect(recoveryCopy(info({ modified_at: null, clips: 0, assets: 0, edits: 0 })).message).toBe(
			'Kerf closed before an untitled project was saved.'
		);
	});

	test('its buttons are verbs and a dismissed dialog never discards', () => {
		const copy = recoveryCopy(info());
		for (const b of [copy.recover, copy.discard, copy.later]) expect(b).not.toMatch(/^(yes|no|ok|cancel)$/i);
		expect(recoveryChoice('Recover', copy)).toBe('recover');
		expect(recoveryChoice('Discard', copy)).toBe('discard');
		expect(recoveryChoice('Not now', copy)).toBe('later');
		expect(recoveryChoice('Cancel', copy)).toBe('later');
		expect(recoveryChoice('', copy)).toBe('later');
	});
});
