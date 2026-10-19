import { describe, expect, test } from 'bun:test';
import { duplicateBlocked, lockedReason, pastePlacements } from './track-locks';
import type { Clip, Timeline } from './types';

const clip = (id: string, start = 0) => ({ id, asset_id: 'a', source_in: 0, source_out: 1, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0 }) as Clip;
const cut = (locks: { v1?: boolean; v2?: boolean; a1?: boolean } = {}) =>
	({
		tracks: [
			{ id: 'v1', kind: 'video', name: 'V1', locked: !!locks.v1, clips: [clip('a'), clip('b', 2)] },
			{ id: 'v2', kind: 'video', name: 'V2', locked: !!locks.v2, clips: [] },
			{ id: 'a1', kind: 'audio', name: 'A1', locked: !!locks.a1, clips: [clip('s')] }
		]
	}) as unknown as Timeline;

describe('what a locked track turns off', () => {
	test('the reason names the track, and an unlocked clip has none', () => {
		expect(lockedReason(cut({ v1: true }), 'a')).toBe('Track V1 is locked');
		expect(lockedReason(cut(), 'a')).toBeUndefined();
		expect(lockedReason(cut({ v1: true }), 'nope')).toBeUndefined();
	});

	test('a duplicate, which stays on its own track, is off when any of its clips is locked in', () => {
		expect(duplicateBlocked(cut({ a1: true }), ['a', 's'])).toBe('Track A1 is locked');
		expect(duplicateBlocked(cut({ a1: true }), ['a', 'b'])).toBeNull();
	});
});

describe('where a paste lands', () => {
	const board = [
		{ track_id: 'v1', clip: clip('a') },
		{ track_id: 'a1', clip: clip('s') }
	];

	test('on the tracks the clips came from, when they are open', () => {
		expect(pastePlacements(cut(), board)).toEqual({ placements: board });
	});

	test('a locked source track hands its clips to the first unlocked track of their kind', () => {
		const aimed = pastePlacements(cut({ v1: true }), board);
		expect('placements' in aimed && aimed.placements.map((p) => p.track_id)).toEqual(['v2', 'a1']);
	});

	test('with no unlocked track of a kind left, the reason', () => {
		expect(pastePlacements(cut({ v1: true, v2: true }), board)).toEqual({ reason: 'Every video track is locked' });
	});
});
