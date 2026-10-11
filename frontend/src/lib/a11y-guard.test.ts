import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parse } from 'svelte/compiler';
import { posix, SRC, uiSources } from './test-sources';

// What a screen reader is told about a control the eye reads at a glance: a button that is only a
// picture or a ✕ has a name, a chip that is on or off says which, a rail says where you are, and a
// text area shows where keyboard focus is. Each of these was found missing by the review and is a
// few lines to keep right — this keeps them right.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

interface Node {
	type: string;
	name?: string;
	start: number;
	end: number;
	attributes?: { type: string; name: string; value: unknown; start: number; end: number }[];
	fragment?: Node;
	nodes?: Node[];
	[key: string]: unknown;
}

const svelteFiles = uiSources(SRC)
	.filter((f) => f.endsWith('.svelte'))
	.map(posix);

/** Every element (plain or component) of a component: its node, the source and the line. */
function elements(file: string): { node: Node; source: string; at: string }[] {
	const source = read(file);
	const ast = parse(source, { modern: true });
	const out: { node: Node; source: string; at: string }[] = [];
	const walk = (n: unknown) => {
		if (!n || typeof n !== 'object') return;
		if (Array.isArray(n)) return n.forEach(walk);
		const node = n as Node;
		if (node.type === 'RegularElement' || node.type === 'Component')
			out.push({ node, source, at: `${file}:${source.slice(0, node.start).split('\n').length}` });
		for (const k of Object.keys(node)) if (k !== 'parent') walk(node[k]);
	};
	walk(ast.fragment);
	return out;
}

const attr = (node: Node, name: string) => node.attributes?.find((a) => a.type === 'Attribute' && a.name === name);
const hasAttr = (node: Node, name: string) => !!attr(node, name) || !!node.attributes?.some((a) => a.type === 'SpreadAttribute');
const directive = (node: Node, name: string) => node.attributes?.some((a) => a.type === 'UseDirective' && a.name === name);

/** The text a button is made of, in the source: its children with components and tags taken out. */
function textOf(node: Node, source: string): string {
	const out: string[] = [];
	const walk = (n: unknown) => {
		if (!n || typeof n !== 'object') return;
		if (Array.isArray(n)) return n.forEach(walk);
		const x = n as Node;
		if (x.type === 'Text') out.push(String((x as { data?: string }).data ?? ''));
		else if (x.type === 'ExpressionTag' || x.type === 'RenderTag') out.push('{}');
		else if (x.type === 'IfBlock' || x.type === 'EachBlock' || x.type === 'KeyBlock') out.push('{}');
		else if (x.type !== 'Component') for (const k of Object.keys(x)) if (k !== 'parent' && k !== 'attributes') walk(x[k]);
	};
	walk((node.fragment as Node | undefined)?.nodes ?? []);
	void source;
	return out.join('').replace(/\s+/g, ' ').trim();
}

const GLYPHS = /^[×✕✖✗xX+–—‹›«»▸▾▴▼]$/;

describe('buttons are named', () => {
	test('a plain button that is a picture or one glyph has an aria-label, a tooltip that names it, or labelled-by', () => {
		const offenders: string[] = [];
		for (const file of svelteFiles) {
			for (const { node, source, at } of elements(file)) {
				if (node.type !== 'RegularElement' || node.name !== 'button') continue;
				if (hasAttr(node, 'aria-label') || hasAttr(node, 'aria-labelledby') || directive(node, 'tip')) continue;
				const text = textOf(node, source);
				if (text === '' || GLYPHS.test(text)) offenders.push(`${at}  <button> reads as "${text || 'nothing'}"`);
			}
		}
		expect(offenders).toEqual([]);
	});

	test('a close or remove button that is a glyph is a picture with a name, not a ✕ the reader says as "multiplication"', () => {
		const offenders: string[] = [];
		for (const file of svelteFiles) {
			for (const { node, source, at } of elements(file)) {
				if (node.type !== 'RegularElement' || node.name !== 'button') continue;
				const text = textOf(node, source);
				if (/[×✕✖✗]/.test(text) && text.length <= 2) offenders.push(`${at}  <button>${text}</button> — use <Icon n="x"> and an aria-label`);
			}
		}
		expect(offenders).toEqual([]);
	});

	test('the buttons that take a title name themselves from it', () => {
		for (const name of ['IconBtn', 'ToolbarToggle'])
			expect(read(`${EDITOR}${name}.svelte`), name).toContain('aria-label={');
	});
});

describe('a chip that is on or off says which', () => {
	test('every button drawn with `chip(on)` carries a pressed, checked or selected state', () => {
		const offenders: string[] = [];
		for (const file of svelteFiles) {
			for (const { node, source, at } of elements(file)) {
				if (node.type !== 'RegularElement' || node.name !== 'button') continue;
				const style = attr(node, 'style');
				if (!style) continue;
				const text = source.slice(style.start, style.end);
				const call = /\bchip\(([^()]*(?:\([^()]*\)[^()]*)*)\)/.exec(text);
				if (!call) continue;
				const arg = call[1].trim();
				// `chip()` and `chip(true)` / `chip(false)` are a look, not a state.
				if (arg === '' || arg === 'true' || arg === 'false') continue;
				const stated = ['aria-pressed', 'aria-checked', 'aria-selected', 'aria-current'].some((a) => hasAttr(node, a));
				if (!stated) offenders.push(`${at}  <button> is chip(${arg}) and says nothing about being on`);
			}
		}
		expect(offenders).toEqual([]);
	});

	test('a group of radio chips is one tab stop that the arrows move through', () => {
		const settings = read(`${EDITOR}SettingsDialog.svelte`);
		const groups = [...settings.matchAll(/role="radiogroup"[^>]*>/g)].map((m) => m[0]);
		expect(groups.length).toBe(2);
		for (const g of groups) expect(g).toContain('use:roving');
		for (const radio of settings.matchAll(/role="radio"[\s\S]*?aria-checked=\{[^}]*\}\s*tabindex=\{[^}]*\? 0 : -1\}/g)) expect(radio[0]).toBeTruthy();
		expect(settings.match(/role="radio"/g)?.length).toBe(settings.match(/\? 0 : -1\}/g)?.length);
	});
});

describe('the settings rail says where you are', () => {
	test('it is a named nav, and the open section is aria-current', () => {
		const settings = read(`${EDITOR}SettingsDialog.svelte`);
		expect(settings).toContain('<nav\n\t\t\taria-label="Settings sections"');
		expect(settings).toContain("aria-current={section === s.id ? 'page' : undefined}");
	});
});

describe('keyboard focus is drawn on a text area too', () => {
	test('the one focus rule lists textarea, so the voiceover script shows the 2 px ring and not the browser’s', () => {
		const layout = read('routes/layout.css');
		const rule = /^:where\(([\s\S]*?)\):focus-visible\s*\{[^}]*outline:\s*var\(--focus-outline\)/m.exec(layout);
		expect(rule, 'the global focus rule').not.toBeNull();
		for (const el of ['button', 'input', 'select', 'textarea']) expect(rule![1]).toContain(el);
	});

	test('the voiceover script sets no outline of its own', () => {
		const vo = read(`${EDITOR}VoiceoverDialog.svelte`);
		const area = /<textarea[\s\S]*?<\/textarea>/.exec(vo)![0];
		expect(area).not.toMatch(/outline/);
	});
});
