import { describe, expect, test } from 'bun:test';
import { errorView, installNote, updateTitle } from './update-copy';

const found = { version: '0.25.0' };

describe('updateTitle', () => {
	test('a failed check is not "up to date"', () => {
		expect(updateTitle('error', null)).toBe("Couldn't check for updates");
		expect(updateTitle('error', null)).not.toMatch(/up to date/);
	});

	test('a failed install still names the update that failed', () => {
		expect(updateTitle('error', found)).toBe('Kerf 0.25.0 is available');
	});

	test('the quiet states', () => {
		expect(updateTitle('current', null)).toBe('Kerf is up to date');
		expect(updateTitle('idle', null)).toBe('Kerf is up to date');
		expect(updateTitle('checking', null)).toBe('Checking for updates');
		expect(updateTitle('available', found)).toBe('Kerf 0.25.0 is available');
		expect(updateTitle('downloading', found)).toBe('Downloading update');
		expect(updateTitle('ready', found)).toBe('Update installed');
	});
});

describe('errorView', () => {
	const raw = 'error sending request for url (https://example.test/latest.json): dns error';

	test('a failed check says what happened, with the raw error under Details', () => {
		const v = errorView('check', raw);
		expect(v.headline).toBe("Couldn't reach the update server. Check your connection and try again.");
		expect(v.details).toBe("Error sending request for url (https://example.test/latest.json): dns error");
	});

	test('any other error is the headline itself, capitalised, with nothing to disclose', () => {
		expect(errorView('install', 'permission denied (os error 13)')).toEqual({
			headline: 'Permission denied (os error 13)',
			details: null
		});
		expect(errorView(null, "Couldn't open https://example.test — no browser")).toEqual({
			headline: "Couldn't open https://example.test — no browser",
			details: null
		});
	});

	test('a message already capitalised, or empty, survives', () => {
		expect(errorView('install', 'Failed to install').headline).toBe('Failed to install');
		expect(errorView('install', '').headline).toBe('');
	});
});

describe('installNote', () => {
	test('the AppImage note is for a failed install on Linux', () => {
		expect(installNote('install', true)).toMatch(/AppImage/);
	});

	test('no other platform is told about the AppImage', () => {
		expect(installNote('install', false)).toBeNull();
	});

	test('a failed check has no install to fall back from', () => {
		expect(installNote('check', true)).toBeNull();
		expect(installNote(null, true)).toBeNull();
	});
});
