import { describe, expect, test } from 'bun:test';
import { aboutRows, platformName, versionInfo, LICENSE, REPOSITORY } from './about';

const release = { version: '0.24.0', platform: 'Windows', development: false };

describe('the About dialog', () => {
	test('names the version, the platform, the build and the licence', () => {
		expect(aboutRows(release)).toEqual([
			{ label: 'Version', value: '0.24.0' },
			{ label: 'Platform', value: 'Windows' },
			{ label: 'Build', value: 'release' },
			{ label: 'License', value: 'PolyForm Noncommercial 1.0.0' }
		]);
	});

	test('a development build says so, and an unread version is not a blank', () => {
		expect(aboutRows({ version: '', platform: 'Linux', development: true })).toEqual([
			{ label: 'Version', value: 'unknown' },
			{ label: 'Platform', value: 'Linux' },
			{ label: 'Build', value: 'development' },
			{ label: 'License', value: LICENSE }
		]);
	});

	test('the copied text is what is on screen, plus where the project lives', () => {
		expect(versionInfo(release)).toBe(
			['Kerf', 'Version: 0.24.0', 'Platform: Windows', 'Build: release', 'License: PolyForm Noncommercial 1.0.0', REPOSITORY].join('\n')
		);
	});

	test('the platform is read from what the webview reports', () => {
		expect(platformName({ platform: 'MacIntel', userAgent: 'Mozilla/5.0 (Macintosh)' })).toBe('macOS');
		expect(platformName({ platform: 'Win32', userAgent: 'Mozilla/5.0 (Windows NT 10.0)' })).toBe('Windows');
		expect(platformName({ platform: 'Linux x86_64', userAgent: 'Mozilla/5.0 (X11; Linux x86_64)' })).toBe('Linux');
		expect(platformName({ platform: 'Linux armv8l', userAgent: 'Mozilla/5.0 (Linux; Android 14)' })).toBe('unknown');
		expect(platformName(undefined)).toBe('unknown');
	});
});
