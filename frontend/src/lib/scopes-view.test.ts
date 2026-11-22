import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
	FULL_LABELS_FROM,
	LEVELS,
	RINGS,
	SCOPE_KINDS,
	SHORT_LABELS_FROM,
	blankPixels,
	clipText,
	clipping,
	colourText,
	density,
	harnessScopes,
	histogramHeights,
	isScopeKind,
	kindOptions,
	labelY,
	legendParts,
	legendText,
	levelY,
	paintGrid,
	readoutLines,
	scaleSide,
	scopeSubject,
	showsScale,
	vectorPoint,
	ycbcr
} from './scopes-view';
import type { Clip, ScopeGrid, ScopeKind, Timeline } from './types';

describe('scopes view', () => {
	test('density is a log curve that never hides a hit', () => {
		expect(density(0, 100)).toBe(0);
		expect(density(1, 1_000_000)).toBeGreaterThan(0.25);
		expect(density(100, 100)).toBe(1);
		expect(density(10, 100)).toBeLessThan(density(50, 100));
	});

	test('a grid paints its bottom row at the bottom and colours stack', () => {
		const grid: ScopeGrid = { columns: 2, rows: 2, counts: [5, 0, 0, 5] };
		const px = blankPixels(2, 2);
		paintGrid(px, grid, [200, 0, 0]);
		paintGrid(px, grid, [0, 0, 200]);
		const at = (x: number, y: number) => Array.from(px.data.slice((y * 2 + x) * 4, (y * 2 + x) * 4 + 4));
		expect(at(0, 1)).toEqual([200, 0, 200, 255]);
		expect(at(1, 0)).toEqual([200, 0, 200, 255]);
		expect(at(0, 0)).toEqual([0, 0, 0, 0]);
		expect(at(1, 1)).toEqual([0, 0, 0, 0]);
	});

	test('a grid painted into a box stays in it', () => {
		const grid: ScopeGrid = { columns: 1, rows: 1, counts: [1] };
		const px = blankPixels(6, 1);
		paintGrid(px, grid, [10, 20, 30], 2, 2);
		expect([0, 1, 2, 3, 4, 5].map((x) => px.data[x * 4 + 3])).toEqual([0, 0, 255, 255, 0, 0]);
	});

	test('histogram heights share one scale', () => {
		const h = { red: [0, 4], green: [1, 0], blue: [0, 0], luma: [0, 0] };
		const out = histogramHeights(h, ['red', 'green']);
		expect(out.red).toEqual([0, 1]);
		expect(out.green[0]).toBeCloseTo(0.5);
	});

	test('the vectorscope centre is the middle and red sits up and left of it', () => {
		expect(vectorPoint(0, 0, 100)).toEqual({ x: 50, y: 50 });
		const [, cb, cr] = ycbcr('bt709', 1, 0, 0);
		const p = vectorPoint(cb, cr, 100);
		expect(p.x).toBeLessThan(50);
		expect(p.y).toBeLessThan(50);
	});

	test('ycbcr follows the engine’s weights', () => {
		expect(ycbcr('bt709', 1, 0, 0)[0]).toBeCloseTo(0.2126);
		expect(ycbcr('bt601', 1, 0, 0)[0]).toBeCloseTo(0.299);
		expect(ycbcr('bt2020', 0, 0, 1)[0]).toBeCloseTo(0.0593);
		expect(ycbcr('bt709', 1, 0, 0)[2]).toBeCloseTo(0.5);
	});

	test('clipping is counted in pixels until it is a share', () => {
		expect(clipText(0, 0)).toBe('none');
		expect(clipText(1, 0.00005)).toBe('1 px');
		expect(clipText(5000, 12.34)).toBe('12.3%');
	});

	test('the harness answers colour bars with the grids asked for', () => {
		const s = harnessScopes(['waveform', 'vectorscope']);
		expect(s.waveform?.counts.some((c) => c > 0)).toBe(true);
		expect(s.vectorscope?.counts.filter((c) => c > 0).length).toBe(7);
		expect(s.parade).toBeUndefined();
		expect(s.histogram).toBeUndefined();
		expect(readoutLines('waveform', s.summary).some((l) => l.warn)).toBe(false);
		expect(isScopeKind('parade')).toBe(true);
		expect(isScopeKind('nope')).toBe(false);
	});

	test('a clipped channel warns', () => {
		const s = harnessScopes([]).summary;
		s.luma = { ...s.luma, clipped_high: 1, clipped_high_pct: 0.00005 };
		const clip = readoutLines('waveform', s).find((l) => l.label === 'Clipping')!;
		expect(clip.warn).toBe(true);
		expect(clip.text).toBe('clipped 1 px');
	});
});

describe('the readout', () => {
	const summary = () => harnessScopes([]).summary;

	test('each scope says what it shows, once', () => {
		const s = summary();
		expect(readoutLines('waveform', s).map((l) => l.label)).toEqual(['Luma', 'Clipping']);
		expect(readoutLines('histogram', s).map((l) => l.label)).toEqual(['Luma', 'Clipping']);
		expect(readoutLines('parade', s).map((l) => l.label)).toEqual(['Red', 'Green', 'Blue']);
		expect(readoutLines('vectorscope', s).map((l) => l.label)).toEqual(['Colour']);
		expect(readoutLines('waveform', s)[0].text).toBe('5–75%, mean 43%');
	});

	test('"none" is said once and a parade says clipping only where there is some', () => {
		const s = summary();
		const all = (['waveform', 'parade', 'vectorscope', 'histogram'] as ScopeKind[]).flatMap((k) => readoutLines(k, s));
		expect(all.filter((l) => l.text.includes('none'))).toHaveLength(2);
		expect(readoutLines('parade', s).every((l) => !l.text.includes('crushed') && !l.text.includes('clipped'))).toBe(true);
		s.red = { ...s.red, clipped_high: 40, clipped_high_pct: 2.5, clipped_low: 3, clipped_low_pct: 0 };
		const red = readoutLines('parade', s)[0];
		expect(red.text).toBe('0–75%, mean 43% · crushed 3 px · clipped 2.5%');
		expect(red.warn).toBe(true);
		expect(readoutLines('parade', s)[1].warn).toBe(false);
	});

	test('clipping names only the sides that clip', () => {
		const c = summary().luma;
		expect(clipping(c)).toBe('');
		expect(clipping({ ...c, clipped_low: 12 })).toBe('crushed 12 px');
		expect(clipping({ ...c, clipped_low: 12, clipped_high: 9000, clipped_high_pct: 20 })).toBe('crushed 12 px · clipped 20.0%');
	});

	test('the colour line is neutral or a cast', () => {
		const s = summary();
		expect(colourText(s)).toBe('neutral · saturation 69%');
		s.chroma = { ...s.chroma, saturation: 0.1, hue_deg: 33.4 };
		expect(colourText(s)).toBe('cast 33° at 10% · saturation 69%');
	});

	test('the legend says which frame, from what, how big and read with which matrix', () => {
		expect(legendText({ width: 1920, height: 1080, matrix: 'bt709' }, '00:00:04:12')).toBe(
			'Frame at 00:00:04:12 · original media · 1920×1080 · BT.709'
		);
		expect(legendText({ width: 3840, height: 2160, matrix: 'bt2020' }, '0:04')).toContain('BT.2020');
		expect(legendParts({ width: 1920, height: 1080, matrix: 'bt709' }, '0:04')).toEqual(['Frame at 0:04', 'original media', '1920×1080', 'BT.709']);
	});
});

describe('the kind picker', () => {
	test('names the scopes in full where four chips fit a row, short below', () => {
		expect(kindOptions(427).map((o) => o.label)).toEqual(['Waveform', 'RGB parade', 'Vectorscope', 'Histogram']);
		expect(kindOptions(FULL_LABELS_FROM).map((o) => o.label)).toEqual(['Waveform', 'RGB parade', 'Vectorscope', 'Histogram']);
		expect(kindOptions(343).map((o) => o.label)).toEqual(['Wave', 'Parade', 'Vector', 'Histo']);
		expect(kindOptions(285).map((o) => o.label)).toEqual(['Wave', 'Parade', 'Vector', 'Histo']);
		expect(kindOptions(SHORT_LABELS_FROM).map((o) => o.label)).toEqual(['Wave', 'Parade', 'Vector', 'Histo']);
		expect(kindOptions(240).map((o) => o.label)).toEqual(['Wave', 'RGB', 'Vec', 'Hist']);
		// not measured yet: the full names, never a flash of the short ones
		expect(kindOptions(0).map((o) => o.label)[0]).toBe('Waveform');
	});

	test('a short name keeps the full one in its tooltip, and every kind is one the engine knows', () => {
		for (const k of SCOPE_KINDS) {
			expect(k.hint.startsWith(k.label)).toBe(true);
			expect(isScopeKind(k.id)).toBe(true);
		}
		expect(new Set(SCOPE_KINDS.map((k) => k.short)).size).toBe(4);
		expect(new Set(SCOPE_KINDS.map((k) => k.tiny)).size).toBe(4);
	});
});

describe('what there is to measure', () => {
	const clip = (start: number, secs: number, over: Partial<Clip> = {}) =>
		({ id: `c${start}`, asset_id: 'a', source_in: 0, source_out: secs, timeline_start: start, volume: 1, ...over }) as unknown as Clip;
	const tl = (video: Clip[], audio: Clip[] = [], extra: Partial<Timeline> = {}): Timeline =>
		({
			tracks: [
				{ id: 'v', kind: 'video', name: 'V1', clips: video },
				{ id: 'a', kind: 'audio', name: 'A1', clips: audio }
			],
			...extra
		}) as Timeline;

	test('a cut with nothing in it is empty, whatever the playhead says', () => {
		expect(scopeSubject(tl([]), 0)).toBe('empty');
		expect(scopeSubject(tl([]), 30)).toBe('empty');
	});

	test('a clip on a picture track is a picture from its first frame to just before its end', () => {
		const t = tl([clip(2, 4)]);
		expect(scopeSubject(t, 2)).toBe('picture');
		expect(scopeSubject(t, 5.99)).toBe('picture');
		expect(scopeSubject(t, 6)).toBe('gap');
		expect(scopeSubject(t, 1.5)).toBe('gap');
	});

	test('sound alone draws nothing', () => {
		expect(scopeSubject(tl([], [clip(0, 10)]), 3)).toBe('gap');
	});

	test('a title draws on a gap', () => {
		const t = tl([clip(0, 2)], [], { overlays: [{ id: 't', text: 'hi', start: 5, end: 7 }] as Timeline['overlays'] });
		expect(scopeSubject(t, 6)).toBe('picture');
		expect(scopeSubject(t, 4)).toBe('gap');
		const svg = tl([], [], { svg_overlays: [{ id: 's', start: 1, end: 3 }] as Timeline['svg_overlays'] });
		expect(scopeSubject(svg, 2)).toBe('picture');
		expect(scopeSubject(svg, 3)).toBe('gap');
	});

	test('a ramped clip is as long as its ramp makes it', () => {
		const ramped = clip(0, 4, { speed: 1, speed_ramp: [{ time: 0, speed: 2 }] });
		expect(scopeSubject(tl([ramped]), 1.9)).toBe('picture');
		expect(scopeSubject(tl([ramped]), 2.1)).toBe('gap');
	});
});

describe('the graticule', () => {
	test('is ruled at the quarters, the top one at the top', () => {
		expect(LEVELS).toEqual([0, 25, 50, 75, 100]);
		expect(levelY(100, 4, 100)).toBe(4);
		expect(levelY(0, 4, 100)).toBe(104);
		expect(levelY(50, 0, 80)).toBe(40);
	});

	test('a label at the edge of the plot is pulled inside it', () => {
		expect(labelY(4, 10, 200)).toBe(6);
		expect(labelY(196, 10, 200)).toBe(194);
		expect(labelY(100, 10, 200)).toBe(100);
		expect(labelY(5, 10, 8)).toBe(6);
	});

	test('numbers go where the trace is not', () => {
		expect(scaleSide('waveform')).toBe('left');
		expect(scaleSide('parade')).toBe('left');
		expect(scaleSide('histogram')).toBe('foot');
		expect(scaleSide('vectorscope')).toBe('none');
		expect(showsScale(139)).toBe(false);
		expect(showsScale(140)).toBe(true);
		expect(RINGS.map((r) => r.label)).toEqual(['75', '100']);
	});

	test('its lines and labels are drawn in what the matte shows through, not in the border colour a light theme turns dark', () => {
		const tokens = readFileSync(join(import.meta.dir, 'styles/kerf-tokens.css'), 'utf8');
		for (const name of ['scope-graticule', 'scope-label']) {
			const value = new RegExp(`--${name}:\\s*([^;]+);`).exec(tokens)?.[1] ?? '';
			expect(value, name).toContain('--text-on-video');
			expect(value, name).not.toContain('--border-base');
		}
	});
});
