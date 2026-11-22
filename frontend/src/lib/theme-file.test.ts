import { describe, expect, test } from 'bun:test';
import { PRESETS, PRESET_IDS, themeJson } from './theme';
import { THEME_FILE_MAX, readThemeFile, themeFileName, themeProblem } from './theme-file';

const file = (over: Record<string, unknown> = {}, colors: Record<string, unknown> = {}) =>
	JSON.stringify({ ...JSON.parse(themeJson(PRESETS['kerf-dark'])), ...over, colors: { ...PRESETS['kerf-dark'].colors, ...colors } });

describe('reading a theme file', () => {
	test('what an export writes imports as the same theme, for every built-in', () => {
		for (const id of PRESET_IDS) {
			const out = readThemeFile(themeJson(PRESETS[id]));
			expect(out.ok, id).toBe(true);
			expect(out.ok && out.theme, id).toEqual(PRESETS[id]);
		}
	});

	test('a file that is not JSON, not an object or too large is refused with a sentence', () => {
		for (const [text, says] of [
			['', "isn't JSON"],
			['{nope', "isn't JSON"],
			['<html>', "isn't JSON"],
			['null', 'JSON object'],
			['[1,2]', 'JSON object'],
			['"theme"', 'JSON object'],
			['42', 'JSON object']
		] as const) {
			const out = readThemeFile(text);
			expect(out.ok, text).toBe(false);
			expect(!out.ok && out.reason, text).toContain(says);
		}
		const big = readThemeFile(' '.repeat(THEME_FILE_MAX + 1));
		expect(!big.ok && big.reason).toContain('too large');
	});

	test('the sentence says which field, and what it should have been', () => {
		const say = (text: string) => {
			const out = readThemeFile(text);
			return out.ok ? '' : out.reason;
		};
		expect(say(file({ version: undefined }))).toContain('no "version"');
		expect(say(file({ version: 2 }))).toContain('version 2');
		expect(say(file({ version: '1' }))).toContain('version "1"');
		expect(say(file({ scheme: 'sepia' }))).toContain('"scheme"');
		expect(say(file({ name: '' }))).toContain('no name');
		expect(say(file({ name: '\u0000‮' }))).toContain('no name');
		expect(say(JSON.stringify({ ...JSON.parse(themeJson(PRESETS['kerf-dark'])), colors: 'x' }))).toContain('"colors"');
		expect(say(JSON.stringify({ ...JSON.parse(themeJson(PRESETS['kerf-dark'])), colors: [] }))).toContain('"colors"');
		expect(say(file({}, { 'kerf-500': 'orange' }))).toContain('"kerf-500" is "orange"; a color is written #rrggbb');
		expect(say(file({}, { waveform: '#fff' }))).toContain('"waveform"');
		expect(say(file({}, { waveform: 12 }))).toContain('is 12');
	});

	test('a refusal quotes a hostile value short, and without markup running on', () => {
		const out = readThemeFile(file({}, { 'kerf-500': '</script><script>alert(1)</script>'.repeat(20) }));
		expect(out.ok).toBe(false);
		const reason = !out.ok ? out.reason : '';
		expect(reason.length).toBeLessThan(120);
		expect(reason).toContain('…');
	});

	test('a theme with every unknown key, a huge name and nested junk imports as a clean theme', () => {
		const out = readThemeFile(
			file(
				{ name: 'N'.repeat(10_000), junk: { a: [1, 2, { b: 3 }] }, shape: { 'line-width': 1e308, density: { x: 1 }, 'font-ui': ['inter'], 'radius-scale': -4, nope: 1 } },
				{ nope: '#000000' }
			)
		);
		expect(out.ok).toBe(true);
		if (!out.ok) return;
		expect(out.theme.name.length).toBe(48);
		expect(out.theme.shape['line-width']).toBe(3);
		expect(out.theme.shape.density).toBe('default');
		expect(out.theme.shape['font-ui']).toBe('geist');
		expect(out.theme.shape['radius-scale']).toBe(0);
		expect(Object.keys(out.theme.colors).includes('nope')).toBe(false);
		expect(Object.keys(out.theme).sort()).toEqual(['colors', 'name', 'scheme', 'shape', 'version']);
	});

	test('a prototype-polluting file does nothing to anything', () => {
		const text = '{"__proto__":{"polluted":1},"constructor":{"prototype":{"polluted":2}},"name":"x","version":1,"scheme":"dark","colors":{"__proto__":{"polluted":3}},"shape":{"__proto__":{"polluted":4}}}';
		const out = readThemeFile(text);
		expect(out.ok).toBe(true);
		expect(({} as Record<string, unknown>).polluted).toBeUndefined();
		expect(Object.prototype.hasOwnProperty.call(out.ok ? out.theme.colors : {}, 'polluted')).toBe(false);
	});

	test('a file from before the newer fields imports as the theme it was', () => {
		const old = JSON.parse(themeJson(PRESETS['kerf-light']));
		for (const k of ['radius-scale', 'density', 'font-ui', 'font-mono']) delete old.shape[k];
		for (const k of ['diff-add', 'diff-remove', 'diff-shift']) delete old.colors[k];
		const out = readThemeFile(JSON.stringify(old));
		expect(out.ok && out.theme).toEqual(PRESETS['kerf-light']);
	});
});

describe('the name an export offers', () => {
	test('lower case, dashes between words, a fallback for a name with nothing to say', () => {
		expect(themeFileName('Kerf Dark')).toBe('kerf-dark.kerf-theme.json');
		expect(themeFileName('  My  theme!! ')).toBe('my-theme.kerf-theme.json');
		expect(themeFileName('日本語')).toBe('theme.kerf-theme.json');
		expect(themeFileName('')).toBe('theme.kerf-theme.json');
	});

	test('a path in a name is not one in the file name, and a long one is cut', () => {
		expect(themeFileName('../../etc/passwd')).toBe('etc-passwd.kerf-theme.json');
		expect(themeFileName('a\\b:c*d?e')).toBe('a-b-c-d-e.kerf-theme.json');
		expect(themeFileName('word '.repeat(30)).length).toBeLessThanOrEqual(40 + '.kerf-theme.json'.length);
		expect(themeFileName('x'.repeat(100))).not.toMatch(/-\.kerf/);
	});
});

describe('themeProblem', () => {
	test('says nothing about a theme that reads', () => {
		for (const id of PRESET_IDS) expect(themeProblem(JSON.parse(themeJson(PRESETS[id])))).toBeNull();
	});
});
