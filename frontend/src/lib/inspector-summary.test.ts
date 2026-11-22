import { describe, expect, test } from 'bun:test';
import { formatTime } from './time-format';
import {
	analysisPill,
	colorKeyCount,
	colorSummary,
	isNeutralColor,
	keyLabel,
	keyframeOf,
	keyNeighbours,
	svgPlacementSummary,
	playheadOnClip,
	svgSourceSummary,
	timingSummary,
	transformKeyRows,
	transformSummary
} from './inspector-summary';
import { analysisMeter, type Chip } from './analysis-steps';
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

	test('reads its times the way the interface does (a formatter in, the mode’s spelling out)', () => {
		expect(timingSummary(0, 12.5, (s) => formatTime(s, 30))).toBe('Start 00:00:00:00 · Dur 00:00:12:15');
	});

	test('a key toggle names the number as its row does', () => {
		expect(keyLabel('temperature')).toBe('warmth');
		expect(keyLabel('pos_x')).toBe('position x');
	});

	test('a sentence about one key takes the right article', () => {
		expect(keyframeOf('scale')).toBe('a scale keyframe');
		expect(keyframeOf('opacity')).toBe('an opacity keyframe');
		expect(keyframeOf('pos_x')).toBe('a position x keyframe');
		expect(keyframeOf('temperature')).toBe('a warmth keyframe');
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

describe('an animated title’s summaries', () => {
	const rect = { x: 0, y: 0, width: 1, height: 1, opacity: 1 };

	test('the Placement line: the whole frame, else the size and the corner', () => {
		expect(svgPlacementSummary(rect)).toBe('Full frame');
		expect(svgPlacementSummary({ ...rect, x: 0.3, y: 0.42, width: 0.4, height: 0.16 })).toBe('40% × 16% at 30%, 42%');
	});

	test('a translucent title says how much', () => {
		expect(svgPlacementSummary({ ...rect, opacity: 0.6 })).toBe('Full frame · 60% opaque');
		expect(svgPlacementSummary({ x: 0.1, y: 0.1, width: 0.5, height: 0.5, opacity: 0.25 })).toBe('50% × 50% at 10%, 10% · 25% opaque');
	});

	test('the artwork line names the preset, else says custom', () => {
		const preset = { kind: 'lower_third' as const, params: { text: 'Ann', color: 'white', accent: 'gold' } };
		expect(svgSourceSummary({ preset })).toBe('Lower third');
		expect(svgSourceSummary({})).toBe('Custom SVG');
		expect(svgSourceSummary({ template: { template_id: 't', name: 'End card', controls: [], values: {} } })).toBe('Template');
	});

	test('the Timing line is the clip’s', () => {
		expect(timingSummary(1, 4)).toBe('Start 0:01.0 · Dur 0:04.0');
	});
});

describe('the analysis pill of a clip’s header', () => {
	const chip = (kind: string, state: string): Chip => ({ kind, label: kind, state, title: `${kind} ${state}` }) as unknown as Chip;
	const pill = (...states: string[]) => analysisPill(analysisMeter(states.map((s, i) => chip(`k${i}`, s))));

	test('says nothing for an asset with nothing to analyze', () => {
		expect(analysisPill(null)).toBeNull();
		expect(analysisPill(analysisMeter([]))).toBeNull();
	});

	test('reads done / of as a fraction, green only when everything has run, and says Analyze when nothing has', () => {
		expect(pill('done', 'done', 'done')).toEqual({ text: '3/3', tone: 'success' });
		expect(pill('done', 'not_run', 'not_run')).toEqual({ text: '1/3', tone: 'neutral' });
		expect(pill('not_run', 'not_run')).toEqual({ text: 'Analyze', tone: 'neutral' });
	});

	test('a pass that is running, one that failed, and a switch that is off', () => {
		expect(pill('done', 'running')).toEqual({ text: 'Analyzing', tone: 'agent' });
		expect(pill('done', 'failed')?.tone).toBe('danger');
		expect(pill('done', 'failed')?.text).toBe('1 failed');
		expect(pill('off', 'off')).toEqual({ text: 'Off', tone: 'neutral' });
	});

	test('a kind that is switched off is not counted against the rest', () => {
		expect(pill('done', 'done', 'off')).toEqual({ text: '2/2', tone: 'success' });
	});
});

describe('a pose keyed either way is one thing to the panel', () => {
	const bundle = (...times: number[]): Partial<Clip> =>
		({ keyframes: times.map((time) => ({ time, scale: 1 + time, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 })) }) as unknown as Partial<Clip>;
	const pose = { scale: 1, rotation: 0 };

	test('an unkeyed clip reads its pose, and has no rows', () => {
		expect(transformSummary(clip(), pose)).toBe('100% · 0°');
		expect(transformKeyRows(clip())).toEqual([]);
	});

	test('the bundle’s keys are keys: the summary counts them and the rows show them', () => {
		expect(transformSummary(clip(bundle(0, 2)), pose)).toBe('Animated · 2 keys');
		expect(transformKeyRows(clip(bundle(0, 2))).map((r) => [r.time, r.bundle, r.tracks.length])).toEqual([
			[0, true, 0],
			[2, true, 0]
		]);
	});

	test('a diamond on one number is a key too: Animated, one row, and which number holds it', () => {
		const c = clip(keyed('scale', [1.5]));
		expect(transformSummary(c, pose)).toBe('Animated · 1 key');
		expect(transformKeyRows(c)).toEqual([
			{ time: 1.5, scale: 0.1, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1, bundle: false, tracks: ['scale'] }
		]);
	});

	test('the two ways merge by time: keys at one moment are one row, and the count is of moments', () => {
		const c = clip({ ...bundle(0, 2), ...keyed('rotation', [2, 4]) });
		const rows = transformKeyRows(c);
		expect(rows.map((r) => r.time)).toEqual([0, 2, 4]);
		expect(rows.map((r) => [r.bundle, r.tracks])).toEqual([
			[true, []],
			[true, ['rotation']],
			[false, ['rotation']]
		]);
		expect(transformSummary(c, pose)).toBe('Animated · 3 keys');
	});

	test('colour, volume and mask keys are not the pose’s', () => {
		expect(transformSummary(clip(keyed('brightness', [1])), pose)).toBe('100% · 0°');
		expect(transformKeyRows(clip(keyed('volume', [1])))).toEqual([]);
	});

	test('how the pose leaves a moment is the key’s easing, from the bundle first', () => {
		const c = clip({
			keyframes: [{ time: 0, scale: 1, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1, easing: 'ease_in' }],
			channels: [{ prop: 'opacity', keys: [{ time: 0, value: 1, easing: 'hold' }] }]
		} as unknown as Partial<Clip>);
		expect(transformKeyRows(c)[0].easing).toBe('ease_in');
	});

	test('the playhead is on a clip from its first frame to its end, and not before or after', () => {
		const c = { timeline_start: 4 };
		expect(playheadOnClip(c, 10, 4)).toBe(true);
		expect(playheadOnClip(c, 10, 7)).toBe(true);
		expect(playheadOnClip(c, 10, 10)).toBe(true);
		expect(playheadOnClip(c, 10, 3.99)).toBe(false);
		expect(playheadOnClip(c, 10, 10.01)).toBe(false);
		expect(playheadOnClip(c, 10, 0)).toBe(false);
	});
});
