import { describe, expect, test } from 'bun:test';
import {
	needsSourceHandle,
	TRANSITION_GROUPS,
	TRANSITION_OPTIONS,
	transitionLabel
} from './transitions';
import type { TransitionKind } from './types';

describe('the transition list', () => {
	test('offers every kind the engine renders, once each', () => {
		// Mirrors `TransitionKind::ALL`; a kind added to one side and not the
		// other is exactly the drift this asserts against.
		const expected: TransitionKind[] = [
			'crossfade',
			'dip_to_black',
			'dip_to_white',
			'push_down',
			'push_left',
			'push_right',
			'push_up',
			'slide_down',
			'slide_left',
			'slide_right',
			'slide_up'
		];
		expect(TRANSITION_OPTIONS.map((o) => o.id).sort()).toEqual(expected.sort());
		expect(new Set(TRANSITION_OPTIONS.map((o) => o.id)).size).toBe(TRANSITION_OPTIONS.length);
	});

	test('every group is named and non-empty', () => {
		for (const g of TRANSITION_GROUPS) {
			expect(g.label.length).toBeGreaterThan(0);
			expect(g.hint.length).toBeGreaterThan(0);
			expect(g.options.length).toBeGreaterThan(0);
		}
	});
});

describe('transitionLabel', () => {
	test('names a known kind and falls back to the wire name', () => {
		expect(transitionLabel('push_up')).toBe('Push up');
		expect(transitionLabel('whip_pan' as never)).toBe('whip_pan');
	});
});

describe('needsSourceHandle', () => {
	test('only a dip renders without the outgoing clip playing under it', () => {
		expect(needsSourceHandle('dip_to_black')).toBe(false);
		expect(needsSourceHandle('dip_to_white')).toBe(false);
		expect(needsSourceHandle('crossfade')).toBe(true);
		expect(needsSourceHandle('slide_up')).toBe(true);
		expect(needsSourceHandle('push_left')).toBe(true);
	});
});
