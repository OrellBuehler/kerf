import { describe, expect, test } from 'bun:test';
import {
	blankPixels,
	clipText,
	density,
	harnessScopes,
	histogramHeights,
	isScopeKind,
	paintGrid,
	summaryLines,
	vectorPoint,
	ycbcr
} from './scopes-view';
import type { ScopeGrid } from './types';

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
		const lines = summaryLines(s.summary);
		expect(lines.map((l) => l.label)).toEqual(['Luma', 'Red', 'Green', 'Blue', 'Colour']);
		expect(lines.some((l) => l.warn)).toBe(false);
		expect(isScopeKind('parade')).toBe(true);
		expect(isScopeKind('nope')).toBe(false);
	});

	test('a clipped channel warns', () => {
		const s = harnessScopes([]).summary;
		s.luma = { ...s.luma, clipped_high: 1, clipped_high_pct: 0.00005 };
		const luma = summaryLines(s)[0];
		expect(luma.warn).toBe(true);
		expect(luma.text).toContain('clipped 1 px');
	});
});
