import { describe, expect, test } from 'bun:test';
import { COLOR_TOKENS, PRESETS, PRESET_IDS, parseTheme, presetIdFor, type Theme } from './theme';

/** `--name: value` pairs from the token stylesheet, `var()` aliases resolved. */
async function cssTokens(): Promise<Map<string, string>> {
	const css = await Bun.file(new URL('./styles/kerf-tokens.css', import.meta.url)).text();
	const raw = new Map<string, string>();
	for (const m of css.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)) raw.set(m[1], m[2].trim());
	const resolve = (v: string, depth = 0): string => {
		const m = /^var\(--([a-z0-9-]+)\)$/.exec(v);
		if (!m || depth > 8) return v;
		return resolve(raw.get(m[1]) ?? v, depth + 1);
	};
	return new Map([...raw].map(([k, v]) => [k, resolve(v)]));
}

describe('PRESETS', () => {
	test('Kerf Dark is exactly what the stylesheet ships', async () => {
		// The stylesheet is the default the app paints before a theme is applied;
		// the preset is what the picker shows as "Kerf Dark". They must agree.
		const css = await cssTokens();
		for (const t of COLOR_TOKENS) {
			expect(css.get(t), `--${t} is missing from kerf-tokens.css`).toBeDefined();
			expect(css.get(t)!.toLowerCase(), `--${t}`).toBe(PRESETS['kerf-dark'].colors[t]);
		}
	});

	test('every preset defines every token as opaque hex', () => {
		for (const id of PRESET_IDS) {
			for (const t of COLOR_TOKENS) expect(PRESETS[id].colors[t], `${id} ${t}`).toMatch(/^#[0-9a-f]{6}$/);
			expect(presetIdFor(PRESETS[id])).toBe(id);
		}
	});
});

describe('parseTheme', () => {
	test('round-trips a preset through JSON', () => {
		for (const id of PRESET_IDS) {
			expect(parseTheme(JSON.parse(JSON.stringify(PRESETS[id])))).toEqual(PRESETS[id]);
		}
	});

	test('rejects what is not a theme', () => {
		const base = PRESETS['kerf-dark'];
		expect(parseTheme(null)).toBeNull();
		expect(parseTheme('x')).toBeNull();
		expect(parseTheme({})).toBeNull();
		expect(parseTheme({ ...base, version: 2 })).toBeNull();
		expect(parseTheme({ ...base, scheme: 'sepia' })).toBeNull();
		expect(parseTheme({ ...base, name: '' })).toBeNull();
		expect(parseTheme({ ...base, colors: { ...base.colors, 'kerf-500': 'orange' } })).toBeNull();
		expect(parseTheme({ ...base, colors: { ...base.colors, 'kerf-500': '#fff' } })).toBeNull();
	});

	test('fills a missing token from the scheme preset and drops unknown ones', () => {
		const { 'drag-ghost': _dropped, ...rest } = PRESETS['kerf-light'].colors;
		const out = parseTheme({ name: 'Mine', version: 1, scheme: 'light', colors: { ...rest, bogus: '#123456' } });
		expect(out).not.toBeNull();
		expect(out!.colors['drag-ghost']).toBe(PRESETS['kerf-light'].colors['drag-ghost']);
		expect('bogus' in out!.colors).toBe(false);
	});

	test('normalizes hex case', () => {
		const out = parseTheme({ ...PRESETS['kerf-dark'], colors: { ...PRESETS['kerf-dark'].colors, 'kerf-500': '#E29D2E' } });
		expect(out!.colors['kerf-500']).toBe('#e29d2e');
		expect(presetIdFor(out!)).toBe('kerf-dark');
	});
});

describe('presetIdFor', () => {
	test('one changed color makes a theme custom, whatever it is named', () => {
		const t: Theme = { ...PRESETS['kerf-dark'], colors: { ...PRESETS['kerf-dark'].colors, waveform: '#ffffff' } };
		expect(presetIdFor(t)).toBe('custom');
		expect(presetIdFor({ ...PRESETS['kerf-dark'], name: 'Renamed' })).toBe('kerf-dark');
	});
});
