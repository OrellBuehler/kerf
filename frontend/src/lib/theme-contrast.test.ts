import { describe, expect, test } from 'bun:test';
import { contrastRatio } from './contrast';
import { PRESETS, PRESET_IDS, COLOR_TOKENS, type ColorToken } from './theme';
import { PAIRS, TINTED, describeFinding, findings, findingsByToken, problems } from './theme-contrast';

const dark = PRESETS['kerf-dark'].colors;
const withColors = (over: Partial<Record<ColorToken, string>>) => ({ ...dark, ...over });

describe('what the editor says is hard to read', () => {
	test('nothing, for any built-in: the editor and the guard are held to the same list', () => {
		for (const id of PRESET_IDS) expect(findings(PRESETS[id].colors), id).toEqual([]);
	});

	test('a text colour that has become the surface\'s own is reported on every surface it sits on, with its numbers', () => {
		const out = findings(withColors({ 'text-muted': '#1a1f26' }));
		const muted = out.filter((f) => f.fg === 'text-muted');
		expect(muted.length).toBeGreaterThan(5);
		for (const f of muted) {
			expect(f.min).toBeGreaterThanOrEqual(3);
			expect(f.ratio).toBeLessThan(f.min);
			expect(f.tokens).toContain('text-muted');
			expect(f.tokens).toContain(f.bg);
			expect(f.text).toBe(`Muted text on ${f.text.slice('Muted text on '.length)}`);
		}
		const onPanel = muted.find((f) => f.bg === 'surface-panel')!;
		expect(onPanel.ratio).toBeCloseTo(contrastRatio('#1a1f26', dark['surface-panel']), 5);
		expect(describeFinding(onPanel)).toBe(`Muted text on Panel: ${onPanel.ratio.toFixed(1)}:1, needs 4.5:1`);
	});

	test('a pair is held to its own bar: reading text 4.5, a mark 3', () => {
		// 3.6:1 passes a mark and fails text.
		const grey = withColors({ 'text-secondary': '#6b7685' });
		const ratio = contrastRatio(grey['text-secondary'], dark['surface-panel']);
		expect(ratio).toBeGreaterThan(3);
		expect(ratio).toBeLessThan(4.5);
		expect(findings(grey).some((f) => f.fg === 'text-secondary' && f.bg === 'surface-panel')).toBe(true);
		expect(findings(withColors({ 'text-disabled': '#6b7685' })).some((f) => f.fg === 'text-disabled')).toBe(false);
	});

	test('a tint is judged where it sits, and names the hue it is made of', () => {
		const out = findings(withColors({ 'green-400': '#14181e' }));
		const tinted = out.filter((f) => f.what.includes('success badge'));
		expect(tinted.length).toBeGreaterThan(0);
		expect(tinted[0].tokens).toEqual(expect.arrayContaining(['green-400', 'green-500']));
		expect(tinted[0].text).toContain('Success text on Success tint on');
	});

	test('the mixes are judged too: a caption block, an animated title block and a clip name chip', () => {
		const out = findings(withColors({ 'text-on-video': '#808080' }));
		const whats = new Set(out.map((f) => f.what));
		expect(whats.has("a generated caption's block")).toBe(true);
		expect(whats.has("an animated title's block")).toBe(true);
		expect(whats.has("a clip's name chip")).toBe(true);
		const chip = out.find((f) => f.what === "a clip's name chip")!;
		expect(chip.tokens).toEqual(expect.arrayContaining(['text-on-video', 'scrim']));
	});

	test('a garish theme gets a long, orderly list and no exception', () => {
		const colors = Object.fromEntries(COLOR_TOKENS.map((t, i) => [t, `#${((i * 2654435) & 0xffffff).toString(16).padStart(6, '0')}`])) as Record<ColorToken, string>;
		const out = findings(colors);
		expect(out.length).toBeGreaterThan(10);
		for (const f of out) {
			expect(Number.isFinite(f.ratio)).toBe(true);
			expect(f.text.length).toBeGreaterThan(5);
			expect(f.tokens.length).toBeGreaterThanOrEqual(2);
		}
	});
});

describe('by token, and folded', () => {
	const broken = findings(withColors({ 'text-primary': '#14181e', 'text-muted': '#1a1f26' }));

	test('each token has the findings it is part of, worst first', () => {
		const by = findingsByToken(broken);
		const panel = by.get('surface-panel')!;
		expect(panel.some((f) => f.fg === 'text-primary')).toBe(true);
		expect(panel.some((f) => f.fg === 'text-muted')).toBe(true);
		for (let i = 1; i < panel.length; i++) expect(panel[i - 1].ratio / panel[i - 1].min).toBeLessThanOrEqual(panel[i].ratio / panel[i].min);
		expect(by.get('kerf-500')).toBeUndefined();
		expect(findingsByToken([]).size).toBe(0);
	});

	test('a text colour that fails on many surfaces is one line, with the worst named and a count', () => {
		const lines = problems(broken);
		const primary = lines.find((p) => p.fg === 'text-primary' && p.what === 'body text on a surface')!;
		expect(primary.count).toBeGreaterThan(5);
		const worst = Math.min(...broken.filter((f) => f.fg === 'text-primary' && f.what === 'body text on a surface').map((f) => f.ratio / f.min));
		expect(primary.worst.ratio / primary.worst.min).toBe(worst);
		expect(lines.length).toBeLessThan(broken.length);
		for (let i = 1; i < lines.length; i++) expect(lines[i - 1].worst.ratio / lines[i - 1].worst.min).toBeLessThanOrEqual(lines[i].worst.ratio / lines[i].worst.min);
		expect(problems([])).toEqual([]);
	});
});

describe('the list is the guard\'s', () => {
	test('every tint names a token the theme has, and every pair names two', () => {
		const tokens = new Set<string>(COLOR_TOKENS);
		for (const p of PAIRS) expect(tokens.has(p.fg) && tokens.has(p.bg), `${p.fg} on ${p.bg}`).toBe(true);
		for (const t of TINTED) expect(tokens.has(t.fg) && tokens.has(t.tint.hue), t.what).toBe(true);
	});

	test('a check of a whole theme is cheap enough to run on every picker event', () => {
		const start = performance.now();
		for (let i = 0; i < 200; i++) findings(dark);
		expect(performance.now() - start).toBeLessThan(1500);
	});
});
