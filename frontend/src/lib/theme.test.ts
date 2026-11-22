import { describe, expect, test } from 'bun:test';
import {
	COLOR_GROUPS,
	COLOR_TOKENS,
	DENSITIES,
	NAME_MAX,
	SHAPE_NAMES,
	SHAPE_TOKENS,
	SUPERSEDED_KERF_LIGHT,
	baseFor,
	cleanName,
	clampShape,
	densityProps,
	normalizeHex,
	shapeProps,
	shapeText,
	themeProps,
	tokenHint,
	tokenLabel,
	PRESETS,
	PRESET_IDS,
	parseTheme,
	presetIdFor,
	upgradeStoredTheme,
	type Theme
} from './theme';
import { DEFAULT_MONO_FONT, DEFAULT_UI_FONT, MONO_FONTS, UI_FONTS, faceFor, fontProps } from './theme-fonts';

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

	test('a stored preset that lacks a token a later build added is still that preset', () => {
		// A theme file written before `clip-accent` existed: every other color, none of that one.
		for (const id of PRESET_IDS) {
			const { 'clip-accent': _added, ...old } = PRESETS[id].colors;
			const stored = JSON.parse(JSON.stringify({ ...PRESETS[id], colors: old }));
			const out = parseTheme(stored)!;
			expect(out.colors['clip-accent'], id).toBe(PRESETS[id].colors['clip-accent']);
			expect(presetIdFor(out), id).toBe(id);
			expect(out.shape, id).toEqual(PRESETS[id].shape);
		}
	});

	test('a theme the user changed takes what it lacks from its scheme, not from a preset it no longer is', () => {
		const { 'clip-accent': _added, ...old } = PRESETS['high-contrast'].colors;
		const edited = { ...PRESETS['high-contrast'], colors: { ...old, waveform: '#123456' } };
		const out = parseTheme(edited)!;
		expect(out.colors['clip-accent']).toBe(PRESETS['kerf-dark'].colors['clip-accent']);
		expect(presetIdFor(out)).toBe('custom');
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

	test('lengths are px, the border weights and shadow strength are percentages', () => {
		const p = shapeProps(PRESETS['high-contrast'].shape);
		expect(p['--line-width']).toBe('1.5px');
		expect(p['--border-alpha-subtle']).toBe('30%');
		expect(p['--border-alpha-default']).toBe('45%');
		expect(p['--border-alpha-strong']).toBe('70%');
		expect(p['--shadow-strength']).toBe('100%');
		expect(shapeProps(PRESETS['kerf-light'].shape)['--shadow-strength']).toBe('30%');
	});

	test('border weights and shadow strength are held to their ranges and steps', () => {
		const out = parseTheme({ ...base, shape: { 'border-alpha-strong': 140, 'border-alpha-subtle': 7.4, 'shadow-strength': 62 } })!;
		expect(out.shape['border-alpha-strong']).toBe(100);
		expect(out.shape['border-alpha-subtle']).toBe(7);
		expect(out.shape['shadow-strength']).toBe(60);
	});

	test('a High contrast stored before the border and shadow values existed is still High contrast', () => {
		const hc = PRESETS['high-contrast'];
		const stored = JSON.parse(JSON.stringify(hc));
		for (const k of ['border-alpha-subtle', 'border-alpha-default', 'border-alpha-strong', 'shadow-strength']) delete stored.shape[k];
		delete stored.colors['shadow-color'];
		const out = parseTheme(stored)!;
		expect(out).toEqual(hc);
		expect(presetIdFor(out)).toBe('high-contrast');
	});

	test('every preset stored before the border and shadow values existed reads back as itself', () => {
		for (const id of PRESET_IDS) {
			const stored = JSON.parse(JSON.stringify(PRESETS[id]));
			for (const k of ['border-alpha-subtle', 'border-alpha-default', 'border-alpha-strong', 'shadow-strength']) delete stored.shape[k];
			delete stored.colors['shadow-color'];
			const out = parseTheme(stored)!;
			expect(out, id).toEqual(PRESETS[id]);
			expect(presetIdFor(out), id).toBe(id);
		}
	});

	test('an edited theme takes the scheme\'s own border and shadow values, not another preset\'s', () => {
		const hc = PRESETS['high-contrast'];
		const stored = JSON.parse(JSON.stringify({ ...hc, colors: { ...hc.colors, waveform: '#123456' } }));
		for (const k of ['border-alpha-subtle', 'border-alpha-default', 'border-alpha-strong', 'shadow-strength']) delete stored.shape[k];
		const out = parseTheme(stored)!;
		expect(out.shape['border-alpha-default']).toBe(PRESETS['kerf-dark'].shape['border-alpha-default']);
		expect(out.shape['line-width']).toBe(hc.shape['line-width']);
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

describe('the colour list', () => {
	test('every token is named once, with a plain label and a hint', () => {
		expect(new Set(COLOR_TOKENS).size).toBe(COLOR_TOKENS.length);
		for (const g of COLOR_GROUPS) {
			expect(g.blurb.length, g.label).toBeGreaterThan(10);
			for (const t of g.tokens) {
				expect(tokenLabel(t.name), t.name).toBe(t.label);
				expect(t.label.length, t.name).toBeGreaterThan(2);
				expect(tokenHint(t.name).length, t.name).toBeGreaterThan(5);
				expect(t.label, `${t.name} reads like a token name`).not.toMatch(/\d{3}\)?$|^[a-z]+-[a-z]+$/);
			}
		}
		const labels = COLOR_GROUPS.flatMap((g) => g.tokens.map((t) => t.label));
		expect(new Set(labels).size, 'two tokens share a label').toBe(labels.length);
	});

	test('the groups are the ones a person looks for', () => {
		expect(COLOR_GROUPS.map((g) => g.label)).toEqual([
			'Surfaces',
			'Text',
			'Accent',
			'Agent',
			'States',
			'Edges and depth',
			'Timeline and clips',
			'Review diff'
		]);
	});

	test('the review colours are the success, error and warning hues in every preset', () => {
		for (const id of PRESET_IDS) {
			const c = PRESETS[id].colors;
			expect(c['diff-add'], id).toBe(c['green-500']);
			expect(c['diff-remove'], id).toBe(c['red-500']);
			expect(c['diff-shift'], id).toBe(c['orange-500']);
		}
	});

	test('a theme stored before the review colours existed draws them in its own hues', () => {
		const mine = { ...PRESETS['kerf-dark'].colors, 'green-500': '#00aa00', 'red-500': '#aa0000', 'orange-500': '#aaaa00' } as Record<string, string>;
		delete mine['diff-add'];
		delete mine['diff-remove'];
		delete mine['diff-shift'];
		const out = parseTheme({ name: 'Mine', version: 1, scheme: 'dark', colors: mine })!;
		expect(out.colors['diff-add']).toBe('#00aa00');
		expect(out.colors['diff-remove']).toBe('#aa0000');
		expect(out.colors['diff-shift']).toBe('#aaaa00');
		// One it did carry is its own.
		mine['diff-add'] = '#123456';
		expect(parseTheme({ name: 'Mine', version: 1, scheme: 'dark', colors: mine })!.colors['diff-add']).toBe('#123456');
	});
});

describe('names and colours read from a file', () => {
	test('a name is made safe to show and to write into a file name', () => {
		expect(cleanName('  My   theme \n')).toBe('My theme');
		expect(cleanName('a\u0000b\u202ec\u0007d')).toBe('a b c d');
		expect(cleanName('x'.repeat(500)).length).toBe(NAME_MAX);
		expect(cleanName('\u0000\u0001   ')).toBe('');
		expect(cleanName(42)).toBe('');
		expect(cleanName(null)).toBe('');
		// Characters, not UTF-16 units: a name is not cut through a surrogate pair.
		expect(Array.from(cleanName('🎨'.repeat(100))).length).toBe(NAME_MAX);
	});

	test('a colour is exactly #rrggbb', () => {
		expect(normalizeHex('#AbCdEf')).toBe('#abcdef');
		for (const bad of ['#fff', '#ffff', '#ff00ff00', 'ff00ff', 'red', 'rgb(0,0,0)', '#gggggg', '#ff00ff ', ' #ff00ff', 'url(x)', '', 7, null, undefined, {}, ['#ffffff']])
			expect(normalizeHex(bad), String(bad)).toBeNull();
	});

	test('a theme file with one bad colour, a bad version, or the wrong shape of colours is refused whole', () => {
		const base = PRESETS['kerf-dark'];
		const withColor = (v: unknown) => ({ ...base, colors: { ...base.colors, 'kerf-500': v } });
		for (const bad of ['#fff', 'red', 'rgb(1,2,3)', 'var(--x)', '#e29d2e; background: red', 12, null, {}]) expect(parseTheme(withColor(bad)), String(bad)).toBeNull();
		expect(parseTheme({ ...base, colors: [] })).toBeNull();
		expect(parseTheme({ ...base, colors: 'x' })).toBeNull();
		expect(parseTheme({ ...base, version: '1' })).toBeNull();
		expect(parseTheme({ ...base, name: 7 })).toBeNull();
		expect(parseTheme([base])).toBeNull();
	});

	test('what a file says about its own name and base is cleaned, and the rest of it ignored', () => {
		const hostile = JSON.parse(
			JSON.stringify({ ...PRESETS['kerf-dark'], name: '  <b>x</b>\u0000 ', base: 'high-contrast', __proto__: { polluted: 1 }, extra: { deep: [1, 2] } })
		);
		hostile.constructor = 'x';
		const out = parseTheme(hostile)!;
		expect(out.name).toBe('<b>x</b>');
		expect(out.base).toBe('high-contrast');
		expect(Object.keys(out).sort()).toEqual(['base', 'colors', 'name', 'scheme', 'shape', 'version']);
		expect(({} as Record<string, unknown>).polluted).toBeUndefined();
		expect(parseTheme({ ...PRESETS['kerf-dark'], base: 'not-a-preset' })!.base).toBeUndefined();
		expect(parseTheme({ ...PRESETS['kerf-dark'], base: { id: 1 } })!.base).toBeUndefined();
	});

	test('a theme that names a built-in as its base is put back to it', () => {
		expect(baseFor({ scheme: 'dark', base: 'high-contrast' })).toBe('high-contrast');
		expect(baseFor({ scheme: 'dark' })).toBe('kerf-dark');
		expect(baseFor({ scheme: 'light' })).toBe('kerf-light');
	});
});

describe('corners, density and typefaces', () => {
	const base = PRESETS['kerf-dark'];

	test('the corner scale is a multiplier the stylesheet reads', async () => {
		const css = await cssTokens();
		expect(css.get('radius-scale')).toBe('1');
		expect(shapeProps(base.shape)['--radius-scale']).toBe('1');
		expect(shapeProps({ ...base.shape, 'radius-scale': 1.5 })['--radius-scale']).toBe('1.5');
		const raw = await Bun.file(new URL('./styles/kerf-tokens.css', import.meta.url)).text();
		for (const [name, px] of [['xs', 2], ['sm', 4], ['md', 6], ['lg', 8], ['xl', 12]] as const)
			expect(raw, `--radius-${name}`).toContain(`--radius-${name}: calc(${px}px * var(--radius-scale));`);
		expect(raw).toContain('--radius-full: 999px;');
	});

	test('the scale is held to its range, in quarters, and shown as a percentage', () => {
		expect(clampShape('radius-scale', 9)).toBe(2);
		expect(clampShape('radius-scale', -1)).toBe(0);
		expect(clampShape('radius-scale', 1.1)).toBe(1);
		expect(clampShape('radius-scale', 1.2)).toBe(1.25);
		const t = SHAPE_TOKENS.find((x) => x.name === 'radius-scale')!;
		expect(shapeText(t, 1.25)).toBe('125%');
		expect(shapeText(t, 0)).toBe('0%');
		expect(shapeText(SHAPE_TOKENS.find((x) => x.name === 'line-width')!, 1.5)).toBe('1.5px');
		expect(parseTheme({ ...base, shape: { 'radius-scale': '2' } })!.shape['radius-scale']).toBe(1);
	});

	test('Default density is exactly what the stylesheet ships', async () => {
		const css = await cssTokens();
		for (const [k, v] of Object.entries(densityProps('default'))) expect(css.get(k.slice(2)), k).toBe(v);
		expect(base.shape.density).toBe('default');
	});

	test('the three densities set the same properties, in order, and never a font size', () => {
		const keys = Object.keys(densityProps('default')).sort();
		expect(DENSITIES.map((d) => d.id)).toEqual(['compact', 'default', 'comfortable']);
		for (const d of DENSITIES) {
			expect(Object.keys(d.props).sort(), d.id).toEqual(keys);
			expect(keys.filter((k) => /text|font|type|leading/.test(k))).toEqual([]);
		}
		const px = (d: string, k: string) => parseInt(densityProps(d as 'default')[k]);
		for (const k of keys) {
			expect(px('compact', k), k).toBeLessThanOrEqual(px('default', k));
			expect(px('default', k), k).toBeLessThan(px('comfortable', k));
		}
		expect(keys.filter((k) => px('compact', k) < px('default', k)).length, 'compact tightens most of them').toBeGreaterThanOrEqual(7);
	});

	test('no density makes a pointer target shorter than 24 px (WCAG 2.5.8)', () => {
		for (const d of DENSITIES)
			for (const k of ['--control-sm', '--control-md', '--control-lg', '--control-tool', '--row-h', '--tabstrip-h'])
				expect(parseInt(d.props[k]), `${d.id} ${k}`).toBeGreaterThanOrEqual(24);
	});

	test('a density the file does not know is the theme\'s own', () => {
		expect(parseTheme({ ...base, shape: { density: 'cramped' } })!.shape.density).toBe('default');
		expect(parseTheme({ ...base, shape: { density: 'compact' } })!.shape.density).toBe('compact');
		expect(densityProps('bogus' as 'default')).toEqual(densityProps('default'));
	});

	test('Default typefaces are exactly what the stylesheet ships', async () => {
		const raw = await Bun.file(new URL('./styles/kerf-tokens.css', import.meta.url)).text();
		const props = fontProps(DEFAULT_UI_FONT, DEFAULT_MONO_FONT);
		for (const [k, v] of Object.entries(props)) expect(raw, k).toContain(`${k}: ${v};`);
		expect(base.shape['font-ui']).toBe('geist');
		expect(base.shape['font-mono']).toBe('geist-mono');
	});

	test('the choices are listed once, bundled ones name the face to wait for, and an unknown id is the default', () => {
		for (const list of [UI_FONTS, MONO_FONTS]) {
			expect(new Set(list.map((f) => f.id)).size).toBe(list.length);
			for (const f of list) {
				expect(f.stack, f.id).toMatch(/,/);
				if (f.face) expect(f.stack, f.id).toContain(f.face.match(/"([^"]+)"/)![1]);
			}
		}
		expect(faceFor('inter', 'jetbrains-mono')).toEqual(['500 12px "Inter Variable"', '500 12px "JetBrains Mono Variable"']);
		expect(faceFor('system', 'system-mono')).toEqual([]);
		expect(fontProps('nope' as 'geist', 'nope' as 'geist-mono')).toEqual(fontProps('geist', 'geist-mono'));
		expect(parseTheme({ ...base, shape: { 'font-ui': 'comic-sans', 'font-mono': 'inter' } })!.shape['font-ui']).toBe('geist');
		expect(parseTheme({ ...base, shape: { 'font-ui': 'inter', 'font-mono': 'jetbrains-mono' } })!.shape['font-ui']).toBe('inter');
		expect(parseTheme({ ...base, shape: { 'font-ui': 'inter', 'font-mono': 'jetbrains-mono' } })!.shape['font-mono']).toBe('jetbrains-mono');
	});

	test('the display face follows the interface face, except in Kerf\'s own pairing', () => {
		expect(fontProps('geist', 'geist-mono')['--font-display']).toContain('Space Grotesk');
		expect(fontProps('inter', 'geist-mono')['--font-display']).toBe(fontProps('inter', 'geist-mono')['--font-sans']);
		expect(fontProps('space-grotesk', 'geist-mono')['--font-display']).toBe(fontProps('space-grotesk', 'geist-mono')['--font-sans']);
	});

	test('a theme writes its colours, shape, density and typefaces, all on the root', () => {
		const props = themeProps({ ...base, shape: { ...base.shape, density: 'compact', 'font-ui': 'inter', 'radius-scale': 0 } });
		for (const t of COLOR_TOKENS) expect(props[`--${t}`], t).toBe(base.colors[t]);
		expect(props['--control-sm']).toBe('24px');
		expect(props['--font-sans']).toContain('Inter Variable');
		expect(props['--radius-scale']).toBe('0');
		expect(props['--line-width']).toBe('1px');
		expect(props['--slider-thumb-w']).toBe('14px');
	});

	test('a new theme field changes what a preset is', () => {
		expect(presetIdFor({ ...base, shape: { ...base.shape, density: 'compact' } })).toBe('custom');
		expect(presetIdFor({ ...base, shape: { ...base.shape, 'font-ui': 'inter' } })).toBe('custom');
		expect(presetIdFor({ ...base, shape: { ...base.shape, 'radius-scale': 1.25 } })).toBe('custom');
	});

	test('a theme stored before these fields existed reads back as the preset it was, unchanged', () => {
		for (const id of PRESET_IDS) {
			const stored = JSON.parse(JSON.stringify(PRESETS[id]));
			for (const k of ['radius-scale', 'density', 'font-ui', 'font-mono']) delete stored.shape[k];
			for (const k of ['diff-add', 'diff-remove', 'diff-shift']) delete stored.colors[k];
			const out = parseTheme(stored)!;
			expect(out, id).toEqual(PRESETS[id]);
			expect(presetIdFor(out), id).toBe(id);
		}
	});
});
