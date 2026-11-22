import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
	acrossTracks,
	alongTrack,
	clipAt,
	clipsUnder,
	editPoints,
	nearestClip,
	noEditPoint,
	pickAtPlayhead,
	stepEdit,
	stepSelection,
	targetedTracks
} from './edit-points';
import { displayTracks } from './track-order';
import type { Clip, Timeline, Track } from './types';

const FPS = 30;
const clip = (id: string, start: number, dur: number, extra: Partial<Clip> = {}): Clip => ({
	id,
	asset_id: 'a',
	source_in: 0,
	source_out: dur,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const track = (id: string, clips: Clip[], extra: Partial<Track> = {}): Track => ({ id, kind: 'video', name: id.toUpperCase(), clips, ...extra });

// V2: v2a 4-6.  V1: c1 0-4, c2 4-9 (touching), c3 12-15.  A1: a1 0-9.  A2: empty.  A3: a3 12-15.
const v1 = track('v1', [clip('c1', 0, 4), clip('c2', 4, 5), clip('c3', 12, 3)]);
const v2 = track('v2', [clip('v2a', 4, 2)]);
const a1 = track('a1', [clip('a1c', 0, 9)], { kind: 'audio' });
const a2 = track('a2', [], { kind: 'audio' });
const a3 = track('a3', [clip('a3c', 12, 3)], { kind: 'audio' });
const tl: Timeline = { tracks: [v1, v2, a1, a2, a3] };
const drawn = displayTracks(tl.tracks);

describe('edit points', () => {
	test('every start and end, once, ascending — a touching pair is one cut', () => {
		expect(editPoints(tl)).toEqual([0, 4, 6, 9, 12, 15]);
	});

	test('only the targeted tracks count', () => {
		expect(editPoints(tl, new Set(['v1']))).toEqual([0, 4, 9, 12, 15]);
		expect(editPoints(tl, new Set(['a1']))).toEqual([0, 9]);
		expect(editPoints(tl, new Set(['a2']))).toEqual([]);
	});

	test('the selection says which tracks: those that hold it, all when nothing is selected', () => {
		expect(targetedTracks(tl, [])).toBeNull();
		expect([...targetedTracks(tl, ['c2'])!]).toEqual(['v1']);
		expect([...targetedTracks(tl, ['c2', 'a3c'])!].sort()).toEqual(['a3', 'v1']);
		expect(targetedTracks(tl, ['gone'])).toBeNull();
	});

	test('next and previous walk the cuts and say so at the ends', () => {
		expect(stepEdit(tl, 0, 1, FPS, 15)).toBe(4);
		expect(stepEdit(tl, 4, 1, FPS, 15)).toBe(6);
		expect(stepEdit(tl, 6, 1, FPS, 15)).toBe(9);
		expect(stepEdit(tl, 15, 1, FPS, 15)).toBeNull();
		expect(stepEdit(tl, 15, -1, FPS, 15)).toBe(12);
		expect(stepEdit(tl, 4, -1, FPS, 15)).toBe(0);
		expect(stepEdit(tl, 0, -1, FPS, 15)).toBeNull();
		expect(noEditPoint(1)).toBe('There is no later edit point');
		expect(noEditPoint(-1)).toBe('There is no earlier edit point');
	});

	test('a track-scoped walk skips the cuts of other tracks', () => {
		expect(stepEdit(tl, 0, 1, FPS, 15, new Set(['v1']))).toBe(4);
		expect(stepEdit(tl, 4, 1, FPS, 15, new Set(['v1']))).toBe(9);
		expect(stepEdit(tl, 9, 1, FPS, 15, new Set(['a1']))).toBeNull();
	});

	test('a cut between two frames is not still ahead of a playhead that has just gone to it', () => {
		// 4.01 s is a third of a frame past frame 120 and the playhead lands on frame 121 for it (rounded up).
		const off: Timeline = { tracks: [track('v1', [clip('x', 0, 4.01), clip('y', 4.01, 3)])] };
		const first = stepEdit(off, 0, 1, FPS, 7.01)!;
		expect(first).toBeCloseTo(121 / FPS, 9);
		expect(stepEdit(off, first, 1, FPS, 7.01)).toBeCloseTo(7.01, 6);
		expect(stepEdit(off, first, -1, FPS, 7.01)).toBe(0);
	});

	test('a cut at the very end of the cut is where "go to end" lands', () => {
		expect(stepEdit(tl, 12, 1, FPS, 15)).toBe(15);
	});
});

describe('the clip under the playhead', () => {
	test('a clip owns its start and not its end, so at a cut it is the clip that begins there', () => {
		expect(clipAt(v1, 4)!.id).toBe('c2');
		expect(clipAt(v1, 3.999)!.id).toBe('c1');
		expect(clipAt(v1, 9)).toBeUndefined();
		expect(clipAt(v1, 10)).toBeUndefined();
	});

	test('every track, topmost drawn first', () => {
		expect(clipsUnder(drawn, 5).map((u) => u.clip.id)).toEqual(['v2a', 'c2', 'a1c']);
		expect(clipsUnder(drawn, 10)).toEqual([]);
	});

	test('the topmost clip not yet selected: each press walks one further down the stack, then round again', () => {
		expect(pickAtPlayhead(drawn, 5, [])).toBe('v2a');
		expect(pickAtPlayhead(drawn, 5, ['v2a'])).toBe('c2');
		expect(pickAtPlayhead(drawn, 5, ['c2'])).toBe('v2a');
		expect(pickAtPlayhead(drawn, 5, ['v2a', 'c2'])).toBe('a1c');
		// Everything under the playhead selected: the topmost again.
		expect(pickAtPlayhead(drawn, 5, ['v2a', 'c2', 'a1c'])).toBe('v2a');
		// A selection that is not under the playhead does not count.
		expect(pickAtPlayhead(drawn, 5, ['c1'])).toBe('v2a');
		expect(pickAtPlayhead(drawn, 10, [])).toBeNull();
	});

	test('a linked pair selected together is one stop', () => {
		const pair: Timeline = { tracks: [track('v1', [clip('p', 0, 9, { link_id: 'L' })]), track('a1', [clip('q', 0, 9, { link_id: 'L' })], { kind: 'audio' }), track('v2', [clip('r', 0, 9)])] };
		const d = displayTracks(pair.tracks);
		// Drawn: v2, v1, a1. Pressing again with V1 and its sound selected goes to V2, not to the sound that is already in.
		expect(pickAtPlayhead(d, 3, ['p', 'q'])).toBe('r');
		expect(pickAtPlayhead(d, 3, ['r'])).toBe('p');
	});
});

describe('moving the selection', () => {
	test('along a track: the clip before and after, none past the ends', () => {
		expect(alongTrack(v1, 'c2', 1)).toBe('c3');
		expect(alongTrack(v1, 'c2', -1)).toBe('c1');
		expect(alongTrack(v1, 'c1', -1)).toBeNull();
		expect(alongTrack(v1, 'c3', 1)).toBeNull();
		expect(alongTrack(v1, 'nope', 1)).toBeNull();
	});

	test('along a track does not trust the order the model holds the clips in', () => {
		const shuffled = track('v1', [clip('c3', 12, 3), clip('c1', 0, 4), clip('c2', 4, 5)]);
		expect(alongTrack(shuffled, 'c1', 1)).toBe('c2');
		expect(alongTrack(shuffled, 'c2', 1)).toBe('c3');
	});

	test('the nearest clip of a lane to a time: under it, else the closer edge', () => {
		expect(nearestClip(v1, 5)!.id).toBe('c2');
		expect(nearestClip(v1, 10)!.id).toBe('c2');
		expect(nearestClip(v1, 11)!.id).toBe('c3');
		expect(nearestClip(a2, 3)).toBeUndefined();
	});

	test('up and down use the order the lanes are drawn in, and skip a lane with no clip', () => {
		// Drawn: v2, v1, a1, a2 (empty), a3.
		expect(drawn.map((t) => t.id)).toEqual(['v2', 'v1', 'a1', 'a2', 'a3']);
		expect(acrossTracks(drawn, 'c2', -1)).toBe('v2a');
		expect(acrossTracks(drawn, 'c2', 1)).toBe('a1c');
		expect(acrossTracks(drawn, 'a1c', 1)).toBe('a3c');
		expect(acrossTracks(drawn, 'v2a', -1)).toBeNull();
		expect(acrossTracks(drawn, 'a3c', 1)).toBeNull();
	});

	test('across lands on the clip nearest the middle of the one it leaves', () => {
		// c3 sits at 12-15: on the lane below, the audio clip nearest 13.5 is a1c's neighbour on a3 (a3c, under it) after a1.
		expect(acrossTracks(drawn, 'c3', 1)).toBe('a1c');
		// a3c (12-15) up: a2 is empty, a1c is the only clip there, so it is nearest by edge.
		expect(acrossTracks(drawn, 'a3c', -1)).toBe('a1c');
		expect(acrossTracks(drawn, 'a1c', -1)).toBe('c2');
	});

	test('a step from the primary: previous, next, above, below, with a sentence when there is none', () => {
		expect(stepSelection(drawn, 'c2', 'next', false, 0)).toEqual({ ok: true, to: { id: 'c3', extend: false } });
		expect(stepSelection(drawn, 'c2', 'prev', false, 0)).toEqual({ ok: true, to: { id: 'c1', extend: false } });
		expect(stepSelection(drawn, 'c2', 'above', false, 0)).toEqual({ ok: true, to: { id: 'v2a', extend: false } });
		expect(stepSelection(drawn, 'c2', 'below', false, 0)).toEqual({ ok: true, to: { id: 'a1c', extend: false } });
		expect(stepSelection(drawn, 'c1', 'prev', false, 0)).toEqual({ ok: false, reason: 'There is no clip before it on this track' });
		expect(stepSelection(drawn, 'c3', 'next', false, 0)).toEqual({ ok: false, reason: 'There is no clip after it on this track' });
		expect(stepSelection(drawn, 'v2a', 'above', false, 0)).toEqual({ ok: false, reason: 'There is no clip on a track above' });
		expect(stepSelection(drawn, 'a3c', 'below', false, 0)).toEqual({ ok: false, reason: 'There is no clip on a track below' });
	});

	test('extending carries the flag; the caller decides how it joins the set', () => {
		expect(stepSelection(drawn, 'c2', 'next', true, 0)).toEqual({ ok: true, to: { id: 'c3', extend: true } });
		expect(stepSelection(drawn, 'c2', 'below', true, 0)).toEqual({ ok: true, to: { id: 'a1c', extend: true } });
	});

	test('with nothing selected the clip under the playhead is picked, whatever the step', () => {
		expect(stepSelection(drawn, null, 'next', false, 5)).toEqual({ ok: true, to: { id: 'v2a', extend: false } });
		expect(stepSelection(drawn, null, 'prev', true, 5)).toEqual({ ok: true, to: { id: 'v2a', extend: false } });
	});

	test('with nothing selected and nothing under the playhead: the nearest clip that way', () => {
		expect(stepSelection(drawn, null, 'next', false, 10)).toEqual({ ok: true, to: { id: 'c3', extend: false } });
		expect(stepSelection(drawn, null, 'prev', false, 10)).toEqual({ ok: true, to: { id: 'c2', extend: false } });
		expect(stepSelection(drawn, null, 'above', false, 10)).toEqual({ ok: true, to: { id: 'v2a', extend: false } });
		expect(stepSelection(drawn, null, 'below', false, 10)).toEqual({ ok: true, to: { id: 'a3c', extend: false } });
	});

	test('an empty cut has nothing to select', () => {
		const none = displayTracks([track('v1', [])]);
		expect(stepSelection(none, null, 'next', false, 0).ok).toBe(false);
	});

	test('a primary that is no longer in the cut is the same as none', () => {
		expect(stepSelection(drawn, 'gone', 'next', false, 5)).toEqual({ ok: true, to: { id: 'v2a', extend: false } });
	});
});

describe('a step from the clip the keyboard is on', () => {
	test('starts at that clip, whichever clip is the primary selection', () => {
		// The selection's primary is c1; the keyboard is on c2. "Previous clip" from c2 is c1, from c1 there is none.
		expect(stepSelection(drawn, 'c2', 'prev', false, 0)).toEqual({ ok: true, to: { id: 'c1', extend: false } });
		expect(stepSelection(drawn, 'c1', 'prev', false, 0).ok).toBe(false);
		expect(stepSelection(drawn, 'c2', 'below', true, 0)).toEqual({ ok: true, to: { id: 'a1c', extend: true } });
	});

	test('a focused clip hands its own id to the navigation keys, and the ops take it as the origin', () => {
		const timeline = readFileSync(join(import.meta.dir, 'components/editor/Timeline.svelte'), 'utf8');
		expect(timeline).toContain('runNav(nav, c.id)');
		const ops = readFileSync(join(import.meta.dir, 'ops.ts'), 'utf8');
		expect(ops).toContain('stepSelection(displayTracks(editor.timeline.tracks), from, step, extend, ui.time)');
	});
});
