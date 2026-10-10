// The source files the guard tests scan: every .svelte / .ts / .css / .html under `src/`, tests
// excluded. One walk, so a new folder is covered by every guard at once.
import { readdirSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';

/** `frontend/src`. */
export const SRC = join(import.meta.dir, '..');

/** Every UI source under `dir` (default `src/`), as absolute paths. */
export function uiSources(dir: string = SRC): string[] {
	return readdirSync(dir).flatMap((name) => {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) return uiSources(path);
		return /\.(svelte|ts|css|html)$/.test(name) && !name.endsWith('.test.ts') ? [path] : [];
	});
}

/** A path under `src/` with forward slashes, as the guards' allow-lists spell them. */
export const posix = (path: string) => relative(SRC, path).split(sep).join('/');
