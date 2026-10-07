import { describe, expect, test } from 'bun:test';
import {
	COLOR_TOKENS,
	SHAPE_NAMES,
	SUPERSEDED_KERF_LIGHT,
	shapeProps,
	PRESETS,
	PRESET_IDS,
	parseTheme,
	presetIdFor,
	upgradeStoredTheme,
	type Theme
} from './theme';

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

	test('Kerf Dark shape is exactly what the stylesheet ships', async () => {
		const css = await cssTokens();
		const props = shapeProps(PRESETS['kerf-dark'].shape);
		for (const [k, v] of Object.entries(props)) {
			expect(css.get(k.slice(2)), k).toBe(v);
		}
		expect(Object.keys(props).length).toBe(SHAPE_NAMES.length + 2);
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

describe('shape', () => {
	const base = PRESETS['kerf-dark'];

	test('an old theme with no shape takes the scheme preset', () => {
		const { shape: _s, ...old } = base;
		expect(parseTheme(old)!.shape).toEqual(base.shape);
		expect(parseTheme({ ...old, scheme: 'light' })!.shape).toEqual(PRESETS['kerf-light'].shape);
	});

	test('clamps, snaps, ignores junk and drops unknown keys', () => {
		const out = parseTheme({
			...base,
			shape: { 'line-width': 99, 'slider-thumb': 2, 'playhead-width': 2.6, 'slider-track': 'x', 'slider-thumb-style': 'bar', bogus: 1 }
		})!;
		expect(out.shape['line-width']).toBe(3);
		expect(out.shape['slider-thumb']).toBe(8);
		expect(out.shape['playhead-width']).toBe(3);
		expect(out.shape['slider-track']).toBe(base.shape['slider-track']);
		expect(out.shape['slider-thumb-style']).toBe('bar');
		expect('bogus' in out.shape).toBe(false);
	});

	test('a bad thumb style falls back to the preset', () => {
		expect(parseTheme({ ...base, shape: { 'slider-thumb-style': 'star' } })!.shape['slider-thumb-style']).toBe('round');
	});

	test('a bar thumb is narrower than it is tall', () => {
		const p = shapeProps({ ...base.shape, 'slider-thumb-style': 'bar' });
		expect(p['--slider-thumb-w']).toBe('6px');
		expect(p['--slider-thumb-radius']).toBe('2px');
	});

	test('one changed shape value makes a theme custom', () => {
		expect(presetIdFor({ ...base, shape: { ...base.shape, 'slider-thumb': 20 } })).toBe('custom');
		expect(presetIdFor({ ...base, shape: { ...base.shape, 'slider-thumb-style': 'bar' } })).toBe('custom');
	});
});

describe('presetIdFor', () => {
	test('one changed color makes a theme custom, whatever it is named', () => {
		const t: Theme = { ...PRESETS['kerf-dark'], colors: { ...PRESETS['kerf-dark'].colors, waveform: '#ffffff' } };
		expect(presetIdFor(t)).toBe('custom');
		expect(presetIdFor({ ...PRESETS['kerf-dark'], name: 'Renamed' })).toBe('kerf-dark');
	});
});

describe('upgradeStoredTheme', () => {
	const light = PRESETS['kerf-light'];
	/** A theme as an earlier build stored it: the old Kerf Light colors, the way
	 *  the settings file carries them. */
	const storedOld = (i: number, over: Partial<Theme> = {}): Theme => ({ ...light, colors: { ...SUPERSEDED_KERF_LIGHT[i] }, ...over });

	test('every superseded Light is complete, distinct, and not the current one', () => {
		expect(SUPERSEDED_KERF_LIGHT.length).toBeGreaterThanOrEqual(2);
		for (const old of SUPERSEDED_KERF_LIGHT) {
			expect(Object.keys(old).sort()).toEqual([...COLOR_TOKENS].sort());
			for (const t of COLOR_TOKENS) expect(old[t], t).toMatch(/^#[0-9a-f]{6}$/);
			// A superseded set equal to the current preset would be a migration to itself.
			expect(COLOR_TOKENS.some((t) => old[t] !== light.colors[t])).toBe(true);
		}
		const keys = SUPERSEDED_KERF_LIGHT.map((o) => COLOR_TOKENS.map((t) => o[t]).join());
		expect(new Set(keys).size).toBe(keys.length);
	});

	test('an untouched earlier Kerf Light becomes the current preset', () => {
		for (let i = 0; i < SUPERSEDED_KERF_LIGHT.length; i++) {
			const out = upgradeStoredTheme(storedOld(i));
			expect(out).toEqual(light);
			expect(presetIdFor(out)).toBe('kerf-light');
		}
	});

	test('it does so for what the settings file really hands back', () => {
		// JSON round trip, then parseTheme (which lower-cases hex), then the upgrade.
		const raw = JSON.parse(JSON.stringify(storedOld(0)).replace(/"#([0-9a-f]{6})"/g, (_, h) => `"#${h.toUpperCase()}"`));
		const out = upgradeStoredTheme(parseTheme(raw)!);
		expect(out).toEqual(light);
	});

	test('the name and shape it was saved with are kept', () => {
		const shape = { ...light.shape, 'slider-thumb': 20 };
		const out = upgradeStoredTheme(storedOld(0, { name: 'My light', shape }));
		expect(out.name).toBe('My light');
		expect(out.shape).toEqual(shape);
		expect(out.colors).toEqual(light.colors);
	});

	test('one color the user changed leaves the whole theme alone', () => {
		for (let i = 0; i < SUPERSEDED_KERF_LIGHT.length; i++) {
			const edited = storedOld(i);
			edited.colors = { ...edited.colors, waveform: '#123456' };
			expect(upgradeStoredTheme(edited)).toBe(edited);
		}
	});

	test('the current Light, the other presets and anything else pass through untouched', () => {
		for (const id of PRESET_IDS) expect(upgradeStoredTheme(PRESETS[id])).toBe(PRESETS[id]);
		const custom: Theme = { ...PRESETS['kerf-dark'], name: 'Mine', colors: { ...PRESETS['kerf-dark'].colors, waveform: '#ffffff' } };
		expect(upgradeStoredTheme(custom)).toBe(custom);
		// The old Light's colors under a dark scheme are somebody's own mix, not the preset.
		const darkWithOldLight: Theme = { ...storedOld(0), scheme: 'dark' };
		expect(upgradeStoredTheme(darkWithOldLight)).toBe(darkWithOldLight);
	});

	test('upgrading twice changes nothing more', () => {
		const once = upgradeStoredTheme(storedOld(0));
		expect(upgradeStoredTheme(once)).toBe(once);
	});
});
