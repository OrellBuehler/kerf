import { describe, expect, test } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { POPOUT_URL } from './layout';
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

	test('is at the path the backend opens windows for', () => {
		// The shell opens a window for `window.open` only at this path (`POPOUT_PATH`); a
		// page that asks for another is refused and the panel never leaves the editor.
		const rust = readFileSync(join(import.meta.dir, '../../../crates/kerf-app/src/popout.rs'), 'utf8');
		const path = rust.match(/const POPOUT_PATH: &str = "([^"]+)";/)?.[1];
		expect(path).toBe(POPOUT_URL);
		// …and the file is served there from the static folder.
		expect(join(import.meta.dir, '..', '..', 'static', POPOUT_URL)).toBe(file);
	});
});
