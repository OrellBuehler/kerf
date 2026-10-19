import { describe, expect, test } from 'bun:test';
import { clipKind, duplicateBlocked, landingLocks, lockedReason, pastePlacements } from './track-locks';
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

const assets = [
	{ id: 'a', streams: [{ kind: 'video' }, { kind: 'audio' }] },
	{ id: 'music', streams: [{ kind: 'audio' }] },
	{ id: 'silent', streams: [{ kind: 'video' }] }
] as never[];

describe('where a paste lands', () => {
	const board = [
		{ track_id: 'v1', clip: clip('a') },
		{ track_id: 'a1', clip: clip('s') }
	];

	test('on the tracks the clips came from, when they are open', () => {
		expect(pastePlacements(cut(), board, assets)).toEqual({ placements: board });
	});

	test('a locked source track hands its clips to the first unlocked track of their kind', () => {
		const aimed = pastePlacements(cut({ v1: true }), board, assets);
		expect('placements' in aimed && aimed.placements.map((p) => p.track_id)).toEqual(['v2', 'a1']);
	});

	test('with no unlocked track of a kind left, the reason', () => {
		expect(pastePlacements(cut({ v1: true, v2: true }), board, assets)).toEqual({ reason: 'Every video track is locked' });
	});
});

describe('a clip whose track is gone', () => {
	const gone = (id: string, asset: string, extra = {}) => ({ track_id: 'a9', clip: { ...clip(id), asset_id: asset, ...extra } as Clip });

	test('goes on a track of its own kind, from what its footage has', () => {
		const aimed = pastePlacements(cut(), [gone('m', 'music'), gone('p', 'silent')], assets);
		expect('placements' in aimed && aimed.placements.map((p) => p.track_id)).toEqual(['a1', 'v1']);
		expect(clipKind({ ...clip('t'), generator: { kind: 'svg' } } as never, assets)).toBe('video');
	});

	test('footage with both, or none the bin still holds, cannot say — and the paste is refused, not guessed', () => {
		expect(clipKind(gone('x', 'a').clip, assets)).toBeNull();
		expect(clipKind(gone('y', 'missing').clip, assets)).toBeNull();
		const refused = pastePlacements(cut(), [gone('x', 'a')], assets);
		expect('reason' in refused && refused.reason).toContain('does not say which kind of track');
	});
});

describe('a forced accept keeps the user’s locks', () => {
	test('an inherited lock the user lifted stays lifted; one the proposal added, and the user’s own, hold', () => {
		const base = cut({ v1: true });
		const live = cut({ a1: true });
		const landing = cut({ v1: true, v2: true });
		landingLocks(landing, base, live);
		expect(landing.tracks.map((t) => [t.name, !!t.locked])).toEqual([
			['V1', false],
			['V2', true],
			['A1', true]
		]);
	});
});
