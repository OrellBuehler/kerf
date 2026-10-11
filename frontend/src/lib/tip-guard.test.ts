import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parse } from 'svelte/compiler';
import { posix, SRC, uiSources } from './test-sources';

// The native `title=` tooltip is the OS's light-grey box on a dark editor, appears after a second,
// cannot show a key and never shows for the keyboard. `use:tip` (`tip.ts`) replaced it, and this
// keeps it replaced: a plain element with a `title` attribute fails here, and so does a component
// that would pass one on to a plain element.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

/** Plain elements that may carry a native `title`, by file, and why. Nothing today. */
const NATIVE_TITLE_ALLOWED: Record<string, string> = {};

/** Components that take a `title` and route it to `use:tip` (or, for the last two, mean something
 *  else by it: a dialog's heading, a section's heading). A component not listed would spread it
 *  onto its <button> as a native tooltip. */
const TITLE_PROPS = new Set(['Btn', 'IconBtn', 'ToolbarToggle', 'Modal', 'InspectorSection']);

interface Node {
	type: string;
	name?: string;
	start: number;
	attributes?: { type: string; name: string; start: number; value?: unknown }[];
	[key: string]: unknown;
}

/** Every element of a component with a `title` attribute: its tag, kind and line. */
function titled(file: string): { tag: string; plain: boolean; at: string }[] {
	const source = read(file);
	const ast = parse(source, { modern: true });
	const found: { tag: string; plain: boolean; at: string }[] = [];
	const walk = (n: unknown) => {
		if (!n || typeof n !== 'object') return;
		if (Array.isArray(n)) return n.forEach(walk);
		const node = n as Node;
		if ((node.type === 'RegularElement' || node.type === 'Component') && node.attributes) {
			for (const a of node.attributes) {
				if (a.type === 'Attribute' && a.name === 'title') {
					const line = source.slice(0, a.start).split('\n').length;
					found.push({ tag: node.name ?? '?', plain: node.type === 'RegularElement', at: `${file}:${line}` });
				}
			}
		}
		for (const k of Object.keys(node)) if (k !== 'parent') walk(node[k]);
	};
	walk(ast.fragment);
	return found;
}

const svelteFiles = uiSources(SRC)
	.filter((f) => f.endsWith('.svelte'))
	.map(posix);

describe('tooltips', () => {
	test('no plain element carries a native title', () => {
		const offenders = svelteFiles.flatMap((f) =>
			f in NATIVE_TITLE_ALLOWED ? [] : titled(f).filter((t) => t.plain).map((t) => `${t.at}  <${t.tag}> — use:tip`)
		);
		expect(offenders).toEqual([]);
	});

	test('every exception is still needed', () => {
		for (const file of Object.keys(NATIVE_TITLE_ALLOWED))
			expect(titled(file).some((t) => t.plain), `${file} has no native title — drop it from NATIVE_TITLE_ALLOWED`).toBe(true);
	});

	test('a component given a title is one that routes it', () => {
		const offenders = svelteFiles.flatMap((f) =>
			titled(f)
				.filter((t) => !t.plain && !TITLE_PROPS.has(t.tag))
				.map((t) => `${t.at}  <${t.tag}> would pass its title on to a plain element`)
		);
		expect(offenders).toEqual([]);
	});

	test('the buttons route their title to the tip, not to the element', () => {
		for (const name of ['Btn', 'IconBtn', 'ToolbarToggle']) {
			const source = read(`${EDITOR}${name}.svelte`);
			expect(source, name).toContain('use:tip={title}');
			expect(source, name).not.toMatch(/\s\{title\}|\stitle=/);
		}
	});

	test('nothing sets a title attribute from script', () => {
		const offenders = uiSources(SRC)
			.map(posix)
			.filter((f) => /\.(ts|svelte)$/.test(f))
			.flatMap((f) =>
				read(f)
					.split('\n')
					.map((line, i) => ({ line, at: `${f}:${i + 1}` }))
					.filter(({ line }) => /setAttribute\(\s*['"]title['"]|\.title\s*=(?!=)/.test(line) && !/document\.title/.test(line))
					.map(({ at }) => at)
			);
		expect(offenders).toEqual([]);
	});

	test('a static attribute never carries a backslash-n: it would render as those two characters', () => {
		// `title="Label\nDetail"` is not a newline in a template attribute (it is in an expression). A
		// tip's detail is `{ text, detail }`, or a string in an expression where `\n` is real.
		const offenders = svelteFiles.flatMap((f) => {
			const source = read(f);
			const ast = parse(source, { modern: true });
			const out: string[] = [];
			const walk = (n: unknown) => {
				if (!n || typeof n !== 'object') return;
				if (Array.isArray(n)) return n.forEach(walk);
				const node = n as Node & { value?: unknown };
				if ((node.type === 'RegularElement' || node.type === 'Component') && node.attributes) {
					for (const a of node.attributes) {
						if (a.type !== 'Attribute' || !Array.isArray(a.value)) continue;
						const text = (a.value as { type: string; raw?: string }[]).filter((p) => p.type === 'Text').map((p) => p.raw ?? '').join('');
						if (text.includes('\\n')) out.push(`${f}:${source.slice(0, a.start).split('\n').length}  ${a.name}="…\\n…"`);
					}
				}
				for (const k of Object.keys(node)) if (k !== 'parent') walk(node[k]);
			};
			walk(ast.fragment);
			return out;
		});
		expect(offenders).toEqual([]);
	});
});
