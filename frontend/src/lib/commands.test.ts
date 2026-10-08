import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

// A command the webview invokes but the backend never registered fails only at
// runtime, as a rejected promise on one click. This pins the two lists together.

const SRC = join(import.meta.dir, '..');
const LIB_RS = join(import.meta.dir, '../../../crates/kerf-app/src/lib.rs');

function sources(dir: string): string[] {
	return readdirSync(dir).flatMap((name) => {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) return sources(path);
		return /\.(ts|svelte)$/.test(name) && !name.endsWith('.test.ts') ? [path] : [];
	});
}

const invoked = new Set<string>();
for (const file of sources(SRC)) {
	for (const m of readFileSync(file, 'utf8').matchAll(/\binvoke(?:<[^(]*>)?\(\s*'([a-z_0-9]+)'/g)) invoked.add(m[1]);
}

const rust = readFileSync(LIB_RS, 'utf8');
const block = rust.match(/generate_handler!\[([\s\S]*?)\]/)?.[1] ?? '';
const registered = new Set(
	block
		.split(',')
		.map((s) => s.trim().replace(/^\/\/.*$/gm, '').trim())
		// A command in its own module (`popout::popout_expect`) is registered by its name.
		.map((s) => s.replace(/^(?:\w+::)+/, ''))
		.filter(Boolean)
);

describe('tauri commands', () => {
	test('the scan found both lists', () => {
		expect(invoked.size).toBeGreaterThan(50);
		expect(registered.size).toBeGreaterThan(50);
	});

	test('every invoked command is registered in generate_handler!', () => {
		const missing = [...invoked].filter((name) => !registered.has(name)).sort();
		expect(missing).toEqual([]);
	});
});
