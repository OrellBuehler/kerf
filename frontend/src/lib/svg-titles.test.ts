import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import fixture from './fixtures/svg-presets.json';
import { validateSvgSource } from './svg-dev';
import {
	RECT_MIN,
	SVG_PRESETS,
	dragRect,
	isFullFrame,
	rectOf,
	rectPatch,
	roundRect,
	sampleSvg,
	svgDataUrl,
	svgDisplayName,
	svgFrame,
	svgPresetLabel,
	svgTooltip,
	takesParam,
	type Rect
} from './svg-titles';
import { posix, SRC, uiSources } from './test-sources';
import type { SvgPresetInfo } from './types';

describe('the presets', () => {
	test('are kerf-core’s four, in its order and under its labels', () => {
		const infos = fixture.infos as SvgPresetInfo[];
		expect(SVG_PRESETS.map((p) => [p.kind, p.label])).toEqual(infos.map((i) => [i.kind, i.label]));
	});

	test('a new one says something, and a preset that takes a subtitle is offered one', () => {
		const infos = fixture.infos as SvgPresetInfo[];
		for (const p of SVG_PRESETS) {
			expect(p.sample.text.length).toBeGreaterThan(0);
			const takes = infos.find((i) => i.kind === p.kind)!.params.some((q) => q.name === 'subtitle');
			if (p.sample.subtitle) expect(takes, p.kind).toBe(true);
		}
	});

	test('labels', () => {
		expect(svgPresetLabel('lower_third')).toBe('Lower third');
	});

	test('the Inspector shows the fields a preset takes, and all of them while it does not know', () => {
		const infos = fixture.infos as SvgPresetInfo[];
		const wordPop = infos.find((i) => i.kind === 'word_pop');
		expect(takesParam(wordPop, 'subtitle')).toBe(false);
		expect(takesParam(wordPop, 'accent')).toBe(true);
		expect(takesParam(undefined, 'subtitle')).toBe(true);
	});
});

describe('naming', () => {
	const preset = { kind: 'fade_up' as const, params: { text: 'Hello', color: 'white', accent: 'gold' } };

	test('a title is called its name, else what a preset says, else SVG', () => {
		expect(svgDisplayName({ name: ' Intro ', preset })).toBe('Intro');
		expect(svgDisplayName({ name: '', preset })).toBe('Hello');
		expect(svgDisplayName({ name: '  ', preset: undefined })).toBe('SVG');
	});

	test('the lane tooltip says how it was made', () => {
		expect(svgTooltip({ name: '', preset })).toBe('Animated title: Hello · Fade up');
		expect(svgTooltip({ name: 'Logo', preset: undefined })).toBe('Animated title: Logo · custom SVG');
	});
});

describe('showing artwork', () => {
	test('an <img> data URL carries the document and keeps its markup out of the URL', () => {
		const svg = '<svg xmlns="http://www.w3.org/2000/svg"><text>a #1 & "b"</text></svg>';
		const url = svgDataUrl(svg);
		expect(url.startsWith('data:image/svg+xml,')).toBe(true);
		expect(url).not.toMatch(/[<>"# ]/);
		expect(decodeURIComponent(url.slice('data:image/svg+xml,'.length))).toBe(svg);
	});

	test('no component inlines markup: nothing in the app writes HTML from a string', () => {
		const offenders: string[] = [];
		for (const file of uiSources(SRC)) {
			const text = readFileSync(file, 'utf8');
			if (/\{@html\b|\.innerHTML\s*=|insertAdjacentHTML|\.outerHTML\s*=/.test(text)) offenders.push(posix(file));
		}
		expect(offenders).toEqual([]);
	});

	test('the pieces that show artwork do it through an <img>', () => {
		const controls = readFileSync(join(SRC, 'lib/components/editor/TitlesControls.svelte'), 'utf8');
		expect(controls).toContain('<img src={thumbs[p.kind]}');
		const actions = readFileSync(join(SRC, 'lib/title-actions.ts'), 'utf8');
		expect(actions).toContain('.then(svgDataUrl)');
	});
});

describe('the frame', () => {
	test('is the delivery frame, else 1080p', () => {
		expect(svgFrame({ width: 1080, height: 1920 })).toEqual({ width: 1080, height: 1920 });
		expect(svgFrame(null)).toEqual({ width: 1920, height: 1080 });
		expect(svgFrame({ width: 0, height: 0 })).toEqual({ width: 1920, height: 1080 });
	});
});

describe('the sample document', () => {
	test('is an animated SVG whose viewBox is the frame', () => {
		const svg = sampleSvg(1080, 1920);
		expect(svg).toContain('width="1080" height="1920" viewBox="0 0 1080 1920"');
		expect(svg).toContain('<animate ');
		expect(svg).toContain('<animateTransform ');
	});

	test('the engine would take it as it is, and say nothing', () => {
		for (const [w, h] of [[1920, 1080], [1080, 1920], [1080, 1080]]) {
			const svg = sampleSvg(w, h);
			const check = validateSvgSource(svg);
			expect(check.warnings).toEqual([]);
			expect(check.svg).toBe(svg);
		}
	});

	test('draws inside the frame and carries no hex colour or script', () => {
		const svg = sampleSvg(1920, 1080);
		expect(svg).not.toMatch(/#[0-9a-f]{3,8}\b/i);
		expect(svg).not.toMatch(/<script|on\w+=|href=/i);
		const rect = /<rect x="([\d.]+)" y="([\d.]+)" width="([\d.]+)" height="([\d.]+)"/.exec(svg)!;
		const [x, y, w, h] = rect.slice(1).map(Number);
		expect(x).toBeGreaterThan(0);
		expect(x + w).toBeLessThan(1920);
		expect(y + h).toBeLessThan(1080);
	});

	test('a degenerate frame still makes a document', () => {
		expect(sampleSvg(0, 0)).toContain('viewBox="0 0 16 16"');
	});
});

describe('the placement box', () => {
	const full: Rect = { x: 0, y: 0, width: 1, height: 1 };
	const box: Rect = { x: 0.25, y: 0.5, width: 0.5, height: 0.25 };

	test('moving follows the pointer across the frame', () => {
		const r = dragRect(box, 'move', 96, -54, 960, 540);
		expect(r.x).toBeCloseTo(0.35, 9);
		expect(r.y).toBeCloseTo(0.4, 9);
		expect(r.width).toBe(0.5);
		expect(r.height).toBe(0.25);
	});

	test('moving holds the box inside the frame', () => {
		expect(dragRect(box, 'move', 9999, 9999, 960, 540)).toEqual({ x: 0.5, y: 0.75, width: 0.5, height: 0.25 });
		expect(dragRect(box, 'move', -9999, -9999, 960, 540)).toEqual({ x: 0, y: 0, width: 0.5, height: 0.25 });
	});

	test('a box larger than the frame slides across it rather than being pulled in', () => {
		const big: Rect = { x: -0.1, y: 0, width: 1.2, height: 1 };
		expect(dragRect(big, 'move', -9999, 0, 960, 540).x).toBeCloseTo(-0.2, 9);
		expect(dragRect(big, 'move', 9999, 0, 960, 540).x).toBe(0);
	});

	test('a corner moves with the opposite one held', () => {
		const se = dragRect(box, 'se', 96, 54, 960, 540);
		expect(se.x).toBe(0.25);
		expect(se.y).toBe(0.5);
		expect(se.width).toBeCloseTo(0.6, 9);
		expect(se.height).toBeCloseTo(0.35, 9);
		const nw = dragRect(box, 'nw', 96, 54, 960, 540);
		expect(nw.x + nw.width).toBeCloseTo(0.75, 9);
		expect(nw.y + nw.height).toBeCloseTo(0.75, 9);
		expect(nw.x).toBeCloseTo(0.35, 9);
		const ne = dragRect(box, 'ne', 96, -54, 960, 540);
		expect(ne.x).toBe(0.25);
		expect(ne.y + ne.height).toBeCloseTo(0.75, 9);
		expect(ne.y).toBeCloseTo(0.4, 9);
		const sw = dragRect(box, 'sw', -96, 54, 960, 540);
		expect(sw.x + sw.width).toBeCloseTo(0.75, 9);
		expect(sw.y).toBe(0.5);
	});

	test('a corner cannot cross the opposite one, nor leave the frame', () => {
		const squeezed = dragRect(box, 'se', -9999, -9999, 960, 540);
		expect(squeezed.width).toBeCloseTo(RECT_MIN, 9);
		expect(squeezed.height).toBeCloseTo(RECT_MIN, 9);
		const grown = dragRect(box, 'se', 9999, 9999, 960, 540);
		expect(grown.x + grown.width).toBeCloseTo(1, 9);
		expect(grown.y + grown.height).toBeCloseTo(1, 9);
		const flung = dragRect(box, 'nw', -9999, -9999, 960, 540);
		expect(flung.x).toBe(0);
		expect(flung.y).toBe(0);
	});

	test('an unmeasured frame moves nothing', () => {
		expect(dragRect(box, 'move', 50, 50, 0, 0)).toEqual(box);
		expect(dragRect(box, 'se', 50, 50, 0, 0)).toEqual(box);
	});

	test('rounds to the four decimals the backend is written in', () => {
		expect(roundRect({ x: 0.30000000000000004, y: 1 / 3, width: 0.123456, height: 1 })).toEqual({
			x: 0.3,
			y: 0.3333,
			width: 0.1235,
			height: 1
		});
	});

	test('a patch names only what changed, and is empty when nothing did', () => {
		expect(rectPatch(box, { ...box, x: 0.3 })).toEqual({ x: 0.3 });
		expect(rectPatch(box, { x: 0.3, y: 0.5, width: 0.6, height: 0.25 })).toEqual({ x: 0.3, width: 0.6 });
		expect(rectPatch(box, rectOf(box))).toEqual({});
	});

	test('a title that fills the frame says so', () => {
		expect(isFullFrame(full)).toBe(true);
		expect(isFullFrame(box)).toBe(false);
	});
});
