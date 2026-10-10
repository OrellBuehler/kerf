import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';

// The type scale is a set of tokens (`--text-2xs` … `--text-4xl`, in
// `styles/kerf-tokens.css`), and for a long time nothing used them: 9, 10.5, 11.5
// and 12.5 px had crept in beside the real steps, `--type-caption` was used and
// never defined (so its text silently took its parent's size), and the smallest
// text in the app was a 9 px mono digit. This guard keeps the scale a scale:
//
//   - a `font-size` is a token (`var(--text-sm)`) or a literal px that is one of the
//     steps, never below the floor (the smallest step, `--text-2xs`);
//   - a `font:` shorthand holds no off-scale px either;
//   - a `var(--type-…)` / `var(--text-…)` names something the stylesheet defines.
//
// Dynamic values (`font-size:{n}px`, the title overlay's `cqh` units) are the
// caller's own arithmetic and are not read.

const SRC = join(import.meta.dir, '..');
const TOKENS = readFileSync(join(SRC, 'lib/styles/kerf-tokens.css'), 'utf8');

/** `--name: value;` declarations of the token stylesheet. */
const declared = new Map<string, string>(
	[...TOKENS.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()])
);

/** The steps of the scale: `--text-2xs` … `--text-4xl`, by name, in px. */
const STEPS = new Map<string, number>(
	[...declared].flatMap(([name, value]) => {
		const m = /^text-(\d?(?:xs|sm|md|lg|xl|base))$/.exec(name);
		const px = /^(\d+)px$/.exec(value);
		return m && px ? [[m[1], Number(px[1])] as [string, number]] : [];
	})
);
const SCALE = new Set(STEPS.values());
const FLOOR = STEPS.get('2xs')!;

/** Files that may size text off the scale, and why. Paths are relative to `src/`. */
const ALLOWED: Record<string, string> = {
	'lib/styles/kerf-tokens.css': 'where the scale is defined'
};

function withoutComments(text: string): string {
	return text
		.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
		.replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '))
		.replace(/(^|\s)\/\/.*$/gm, '$1');
}

function sources(dir: string): string[] {
	return readdirSync(dir).flatMap((name) => {
		const path = join(dir, name);
		if (statSync(path).isDirectory()) return sources(path);
		return /\.(svelte|ts|css)$/.test(name) && !name.endsWith('.test.ts') ? [path] : [];
	});
}

const posix = (path: string) => relative(SRC, path).split(sep).join('/');

interface Finding {
	line: number;
	value: string;
	why: string;
}

/** What is wrong with the text sizing in `text`, if anything. */
function sizeProblems(text: string): Finding[] {
	const code = withoutComments(text);
	const lineOf = (index: number) => code.slice(0, index).split('\n').length;
	const out: Finding[] = [];

	for (const m of code.matchAll(/font-size\s*:\s*([^;"'}]+)/g)) {
		const v = m[1].trim();
		const token = /^var\(--text-([a-z0-9]+)\)$/.exec(v);
		const px = /^(\d+(?:\.\d+)?)px$/.exec(v);
		if (token) {
			if (!STEPS.has(token[1])) out.push({ line: lineOf(m.index), value: v, why: `--text-${token[1]} is not a step of the scale` });
		} else if (px) {
			const n = Number(px[1]);
			if (n < FLOOR) out.push({ line: lineOf(m.index), value: v, why: `below the ${FLOOR}px floor` });
			else if (!SCALE.has(n)) out.push({ line: lineOf(m.index), value: v, why: `${n}px is not a step (${[...SCALE].join(', ')})` });
		} else if (/^(0|inherit|initial|unset)$/.test(v) || v.includes('{') || /c[qw][hw]\b/.test(v)) {
			continue;
		} else {
			out.push({ line: lineOf(m.index), value: v, why: 'use a --text-* token or a literal step in px' });
		}
	}

	for (const m of code.matchAll(/(?<![\w-])font\s*:\s*([^;"'}]+)/g)) {
		for (const px of m[1].matchAll(/(\d+(?:\.\d+)?)px/g)) {
			const n = Number(px[1]);
			if (n < FLOOR || !SCALE.has(n)) out.push({ line: lineOf(m.index), value: m[0].trim(), why: `${n}px is not a step of the scale` });
		}
	}
	return out;
}

/** `var(--type-…)` and `var(--text-…)` names the stylesheet does not define. */
function undefinedTokens(text: string): { line: number; name: string }[] {
	const code = withoutComments(text);
	return [...code.matchAll(/var\(\s*--((?:type|text)-[a-z0-9-]+)/g)]
		.filter((m) => !declared.has(m[1]))
		.map((m) => ({ line: code.slice(0, m.index).split('\n').length, name: `--${m[1]}` }));
}

describe('the type scale', () => {
	test('the scale is read from the stylesheet and runs 10 to 46', () => {
		expect([...STEPS.keys()].sort()).toEqual(['2xl', '2xs', '3xl', '4xl', 'base', 'lg', 'md', 'sm', 'xl', 'xs']);
		expect(FLOOR).toBe(10);
		expect([...SCALE].sort((a, b) => a - b)).toEqual([10, 11, 12, 13, 14, 16, 20, 26, 34, 46]);
	});

	test('the checker finds off-scale sizes and leaves the rest alone', () => {
		const bad = (s: string) => sizeProblems(s).map((f) => f.value);
		expect(bad('a{font-size:9px}')).toEqual(['9px']);
		expect(bad('style="font-size:10.5px;color:red"')).toEqual(['10.5px']);
		expect(bad('style="font-size:12.5px"')).toEqual(['12.5px']);
		expect(bad('font-size: 11.5px;')).toEqual(['11.5px']);
		expect(bad('font-size:0.8rem')).toEqual(['0.8rem']);
		expect(bad('font: 600 11.5px/1 sans-serif')).toEqual(['font: 600 11.5px/1 sans-serif']);

		expect(bad('font-size:12px;font-size:var(--text-sm);font-size:inherit')).toEqual([]);
		expect(bad('font-size:{titleMetrics.font}px;font-size:{size * 100}cqh;font-size:calc(100cqh * .1)')).toEqual([]);
		expect(bad('font:var(--type-label);font: 500 12px/1.2 var(--font-sans)')).toEqual([]);
		expect(bad('font-size:var(--text-huge)')).toEqual(['var(--text-huge)']);
		expect(bad('/* font-size:9px */ const a = 1; // font-size:9px\n')).toEqual([]);
		expect(bad('font-family:var(--font-mono);line-height:9px')).toEqual([]);
	});

	test('the checker finds tokens that were never defined', () => {
		const names = (s: string) => undefinedTokens(s).map((t) => t.name);
		expect(names('font:var(--type-caption)')).toEqual([]);
		expect(names('font:var(--type-captions);color:var(--text-muted);x:var(--text-nope)')).toEqual(['--type-captions', '--text-nope']);
	});

	test('no component or stylesheet sizes text off the scale', () => {
		const files = sources(SRC).map(posix);
		expect(files.length).toBeGreaterThan(100);
		const offenders: string[] = [];
		for (const file of files) {
			if (file in ALLOWED) continue;
			for (const f of sizeProblems(readFileSync(join(SRC, file), 'utf8'))) {
				offenders.push(`${file}:${f.line}  ${f.value}  — ${f.why}`);
			}
		}
		expect(offenders).toEqual([]);
	});

	test('every --type-* and --text-* a component reads is defined', () => {
		const offenders: string[] = [];
		for (const file of sources(SRC).map(posix)) {
			for (const t of undefinedTokens(readFileSync(join(SRC, file), 'utf8'))) {
				offenders.push(`${file}:${t.line}  var(${t.name}) is not defined in kerf-tokens.css`);
			}
		}
		expect(offenders).toEqual([]);
	});
});
