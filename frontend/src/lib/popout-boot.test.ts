import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { isEmbeddedPopout } from './popout-boot';

describe('a page opened by another window', () => {
	test('is a popout and boots nothing', () => {
		expect(isEmbeddedPopout({ opener: {} })).toBe(true);
	});

	test('the editor window has no opener', () => {
		expect(isEmbeddedPopout({ opener: null })).toBe(false);
		expect(isEmbeddedPopout({})).toBe(false);
	});

	test('a window that is its own opener is not embedded', () => {
		const w: { opener?: unknown } = {};
		w.opener = w;
		expect(isEmbeddedPopout(w)).toBe(false);
	});
});

describe('the popout page', () => {
	const file = join(import.meta.dir, '..', '..', 'static', 'popout.html');

	test('exists as a real file, so the static fallback never answers its path with the app', () => {
		expect(existsSync(file)).toBe(true);
	});

	test('loads no script', () => {
		expect(readFileSync(file, 'utf8')).not.toMatch(/<script|<link/i);
	});
});
