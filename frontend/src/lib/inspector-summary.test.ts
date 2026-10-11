import { describe, expect, test } from 'bun:test';
import { colorKeyCount, colorSummary, isNeutralColor, keyLabel, keyNeighbours, timingSummary } from './inspector-summary';
import { COLOR_LOOKS } from './style-presets';
import { DEFAULT_COLOR, type Clip, type Color } from './types';

const clip = (extra: Partial<Clip> = {}): Clip =>
	({ id: 'c', asset_id: 'a', source_in: 0, source_out: 5, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0, ...extra }) as Clip;

const keyed = (property: string, times: number[]): Partial<Clip> =>
	({ channels: [{ prop: property, keys: times.map((time) => ({ time, value: 0.1 })) }] }) as unknown as Partial<Clip>;

describe('the Color summary', () => {
	test('an ungraded clip reads None, not Custom', () => {
		expect(colorSummary(clip(), DEFAULT_COLOR)).toBe('None');
		expect(colorSummary(undefined, DEFAULT_COLOR)).toBe('None');
		expect(isNeutralColor({ brightness: 0, contrast: 1, saturation: 1, gamma: 1 } as Color)).toBe(true);
	});

	test('a look it matches is named, anything else is Custom', () => {
		const look = COLOR_LOOKS[0];
		expect(colorSummary(clip(), look.color)).toBe(look.label);
		expect(colorSummary(clip(), { ...DEFAULT_COLOR, brightness: 0.13 })).toBe('Custom');
	});

	test('a keyed number makes it Animated, with the moments counted once', () => {
		const c = clip({ color: DEFAULT_COLOR, ...keyed('brightness', [0, 2]) });
		expect(colorSummary(c, DEFAULT_COLOR)).toBe('Animated · 2 keys');
		const both = clip({
			channels: [
				{ prop: 'brightness', keys: [{ time: 1, value: 0.1 }] },
				{ prop: 'contrast', keys: [{ time: 1, value: 1.2 }, { time: 3, value: 1 }] }
			]
		} as unknown as Partial<Clip>);
		expect(colorKeyCount(both)).toBe(2);
		expect(colorSummary(clip(keyed('saturation', [1])), DEFAULT_COLOR)).toBe('Animated · 1 key');
	});

	test('a keyed transform number is not a colour animation', () => {
		expect(colorSummary(clip(keyed('scale', [0, 1])), DEFAULT_COLOR)).toBe('None');
	});
});

describe('the Timing summary and the key labels', () => {
	test('labels both numbers', () => {
		expect(timingSummary(0, 12.5)).toBe('Start 0:00.0 · Dur 0:12.5');
	});

	test('a key toggle names the number as its row does', () => {
		expect(keyLabel('temperature')).toBe('warmth');
		expect(keyLabel('pos_x')).toBe('position x');
		expect(keyLabel('brightness')).toBe('brightness');
	});
});

describe('the keys around the playhead', () => {
	test('the nearest one on each side, whatever order they are in', () => {
		expect(keyNeighbours([4, 0, 2, 9], 3)).toEqual({ prev: 2, next: 4, here: false });
	});

	test('nothing before the first or after the last', () => {
		expect(keyNeighbours([2, 5], 1)).toEqual({ prev: null, next: 2, here: false });
		expect(keyNeighbours([2, 5], 8)).toEqual({ prev: 5, next: null, here: false });
		expect(keyNeighbours([], 1)).toEqual({ prev: null, next: null, here: false });
	});

	test('a key at the playhead is here, so stepping moves past it', () => {
		expect(keyNeighbours([0, 2, 4], 2)).toEqual({ prev: 0, next: 4, here: true });
		expect(keyNeighbours([0, 2, 4], 2.015)).toEqual({ prev: 0, next: 4, here: true });
		expect(keyNeighbours([0, 2, 4], 2.03)).toEqual({ prev: 2, next: 4, here: false });
	});
});
