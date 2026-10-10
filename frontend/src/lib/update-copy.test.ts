import { describe, expect, test } from 'bun:test';
import { installNote, onLinux, updateTitle } from './update-copy';

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

describe('onLinux', () => {
	test('reads the platform the webview reports', () => {
		expect(onLinux({ platform: 'Linux x86_64' })).toBe(true);
		expect(onLinux({ userAgentData: { platform: 'Linux' }, platform: 'x' })).toBe(true);
		expect(onLinux({ userAgent: 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15' })).toBe(true);
	});

	test('not macOS, not Windows, and not Android', () => {
		expect(onLinux({ platform: 'MacIntel' })).toBe(false);
		expect(onLinux({ platform: 'Win32' })).toBe(false);
		expect(onLinux({ platform: 'Linux armv81', userAgent: 'Mozilla/5.0 (Linux; Android 14)' })).toBe(false);
		expect(onLinux({})).toBe(false);
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
