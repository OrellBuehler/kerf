import { describe, expect, test } from 'bun:test';
import { entryId, mergeSubset, subsetMessage } from './staged-subset';
import type { Clip, DiffEntry, SvgOverlay, Timeline, TimelineDiff } from './types';

// The cases are the ones in crates/kerf-core/src/model/subset.rs, with the diff written by hand
// (the harness has no diff engine: its entries are authored like the backend's).

const A = '11111111-1111-1111-1111-111111111111';

function clip(id: string, start: number, len: number, extra: Partial<Clip> = {}): Clip {
	return { id, asset_id: A, source_in: 0, source_out: len, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0, ...extra };
}

/** V1 holding `a` (0..4) and `b` (4..8), A1 empty. */
function cut(): Timeline {
	return {
		tracks: [
			{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('a', 0, 4), clip('b', 4, 4)] },
			{ id: 'a1', kind: 'audio', name: 'A1', clips: [] }
		]
	};
}

const diffOf = (...entries: DiffEntry[]): TimelineDiff => ({
	entries,
	duration_before: 8,
	duration_after: 8,
	clips_before: 2,
	clips_after: 2
});

const changed = (id: string, track = 'v1'): DiffEntry => ({ kind: 'clip_changed', summary: `Changed ${id}`, clip_id: id, track_id: track, at: 0 });

describe('entryId', () => {
	test('names a change after its kind and what it is about, as the backend does', () => {
		expect(entryId({ kind: 'clip_moved', summary: '', clip_id: 'c1', track_id: 't1' })).toBe('clip_moved:c1');
		expect(entryId({ kind: 'overlay_added', summary: '', overlay_id: 'o1' })).toBe('overlay_added:o1');
		expect(entryId({ kind: 'marker_removed', summary: '', marker_id: 'm1' })).toBe('marker_removed:m1');
		expect(entryId({ kind: 'track_added', summary: '', track_id: 't2' })).toBe('track_added:t2');
		expect(entryId({ kind: 'master_changed', summary: '' })).toBe('master_changed');
		expect(entryId({ id: 'given', kind: 'format_changed', summary: '' })).toBe('given');
	});
});

describe('mergeSubset', () => {
	test('only the picked change lands and the rest of the proposal stays out', () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks[0].clips[0].volume = 0.5;
		proposed.tracks[0].clips[1].volume = 0.25;
		const diff = diffOf(changed('a'), changed('b'));
		const merge = mergeSubset(base, base, proposed, diff, ['clip_changed:a']);
		expect(merge.applied).toEqual(['clip_changed:a']);
		expect(merge.autoIncluded).toEqual([]);
		expect(merge.skipped).toEqual([]);
		expect(merge.timeline.tracks[0].clips[0].volume).toBe(0.5);
		expect(merge.timeline.tracks[0].clips[1].volume).toBe(1);
	});

	test("lands by identity and leaves the user's newer work alone", () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks[0].clips[0].volume = 0.5;
		const live = cut();
		live.tracks[0].clips[1].timeline_start = 6;
		const merge = mergeSubset(live, base, proposed, diffOf(changed('a')), ['clip_changed:a']);
		expect(merge.timeline.tracks[0].clips.find((c) => c.id === 'a')?.volume).toBe(0.5);
		expect(merge.timeline.tracks[0].clips.find((c) => c.id === 'b')?.timeline_start).toBe(6);
	});

	test('a retrimmed clip takes the footage and keeps its place', () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks[0].clips[0].source_out = 2;
		proposed.tracks[0].clips[0].timeline_start = 1;
		const entry: DiffEntry = { kind: 'clip_retrimmed', summary: 'Trimmed', clip_id: 'a', track_id: 'v1', at: 0 };
		const a = mergeSubset(base, base, proposed, diffOf(entry), ['clip_retrimmed:a']).timeline.tracks[0].clips[0];
		expect(a.source_out).toBe(2);
		expect(a.timeline_start).toBe(0);
	});

	test('a linked partner comes along and is reported', () => {
		const base: Timeline = {
			tracks: [
				{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('v', 0, 4, { link_id: 'L' })] },
				{ id: 'a1', kind: 'audio', name: 'A1', clips: [clip('s', 0, 4, { link_id: 'L' })] }
			]
		};
		const proposed = structuredClone(base);
		proposed.tracks[0].clips[0].volume = 0.5;
		proposed.tracks[1].clips[0].volume = 0.5;
		const diff = diffOf(changed('v'), changed('s', 'a1'));
		const merge = mergeSubset(base, base, proposed, diff, ['clip_changed:v']);
		expect(merge.applied).toHaveLength(2);
		expect(merge.autoIncluded).toHaveLength(1);
		expect(merge.autoIncluded[0].id).toBe('clip_changed:s');
		expect(merge.autoIncluded[0].because).toContain('linked');
		expect(merge.timeline.tracks[1].clips[0].volume).toBe(0.5);
	});

	test('a clip added to a new track brings the track', () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks.splice(1, 0, { id: 'v2', kind: 'video', name: 'V2', clips: [clip('x', 0, 2)] });
		const diff = diffOf(
			{ kind: 'track_added', summary: 'Added video track V2', track_id: 'v2' },
			{ kind: 'clip_added', summary: 'Added clip', clip_id: 'x', track_id: 'v2', at: 0 }
		);
		const merge = mergeSubset(base, base, proposed, diff, ['clip_added:x']);
		expect(merge.timeline.tracks.map((t) => t.id)).toEqual(['v1', 'v2', 'a1']);
		expect(merge.timeline.tracks[1].clips.map((c) => c.id)).toEqual(['x']);
		expect(merge.autoIncluded.map((a) => a.id)).toEqual(['track_added:v2']);
	});

	test('a pick that would overlap a clip is refused, naming the place', () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks[0].clips[0].source_out = 6;
		proposed.tracks[0].clips[1].timeline_start = 6;
		const retrim: DiffEntry = { kind: 'clip_retrimmed', summary: 'Trimmed a', clip_id: 'a', track_id: 'v1', at: 0 };
		const move: DiffEntry = { kind: 'clip_moved', summary: 'Moved b', clip_id: 'b', track_id: 'v1', at: 6 };
		const diff = diffOf(retrim, move);
		expect(() => mergeSubset(base, base, proposed, diff, ['clip_retrimmed:a'])).toThrow(/on top of each other on V1/);
		// With the move picked too it is the proposal itself, and lands.
		expect(() => mergeSubset(base, base, proposed, diff, ['clip_retrimmed:a', 'clip_moved:b'])).not.toThrow();
	});

	test('a change to something the user since removed is skipped, not fatal', () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks[0].clips[0].volume = 0.5;
		proposed.tracks[0].clips[1].volume = 0.25;
		const live = cut();
		live.tracks[0].clips.splice(0, 1);
		const merge = mergeSubset(live, base, proposed, diffOf(changed('a'), changed('b')), ['clip_changed:a', 'clip_changed:b']);
		expect(merge.applied).toEqual(['clip_changed:b']);
		expect(merge.skipped).toHaveLength(1);
		expect(merge.skipped[0].id).toBe('clip_changed:a');
		expect(merge.skipped[0].why).toContain('no longer');
	});

	test('unknown and empty picks are refused', () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks[0].clips[0].volume = 0.5;
		const diff = diffOf(changed('a'));
		expect(() => mergeSubset(base, base, proposed, diff, [])).toThrow(/pick at least one change/);
		expect(() => mergeSubset(base, base, proposed, diff, ['clip_changed:nope'])).toThrow(/no change clip_changed:nope/);
	});

	test('text, markers and the master apply by identity too', () => {
		const base = cut();
		const proposed = cut();
		proposed.overlays = [{ id: 'o1', text: 'hello', start: 1, end: 2, pos_x: 0.5, pos_y: 0.5, size: 0.05, color: 'white', bold: false }];
		proposed.markers = [{ id: 'm1', time: 3, name: 'beat' }];
		proposed.master = { volume: 0.5, limiter: false, ceiling_db: -1.5 };
		const diff = diffOf(
			{ kind: 'overlay_added', summary: 'Added text', overlay_id: 'o1', at: 1 },
			{ kind: 'marker_added', summary: 'Added marker', marker_id: 'm1', at: 3 },
			{ kind: 'master_changed', summary: 'Master volume' }
		);
		const merge = mergeSubset(base, base, proposed, diff, diff.entries.map(entryId));
		expect(merge.timeline.overlays?.map((o) => o.id)).toEqual(['o1']);
		expect(merge.timeline.markers?.map((m) => m.id)).toEqual(['m1']);
		expect(merge.timeline.master?.volume).toBe(0.5);
		expect(merge.applied).toHaveLength(3);
	});

	test('a pick that would pull linked clips out of step is refused, naming the tracks', () => {
		const base: Timeline = {
			tracks: [
				{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('v', 0, 4, { link_id: 'L' })] },
				{ id: 'a1', kind: 'audio', name: 'A1', clips: [clip('s', 0, 4, { link_id: 'L' })] }
			]
		};
		// The proposal itself moved the picture and left its sound where it was.
		const apart = structuredClone(base);
		apart.tracks[0].clips[0].timeline_start = 2;
		const move: DiffEntry = { kind: 'clip_moved', summary: 'Moved v', clip_id: 'v', track_id: 'v1', at: 2 };
		expect(() => mergeSubset(base, base, apart, diffOf(move), ['clip_moved:v'])).toThrow(
			/pull the linked clips on V1 and A1 out of step — pick the changes to both, or apply the whole proposal/
		);
		// Moved together, they are still in step and it lands: the partner's move comes with the pick.
		const together = structuredClone(apart);
		together.tracks[1].clips[0].timeline_start = 2;
		const both = diffOf(move, { kind: 'clip_moved', summary: 'Moved s', clip_id: 's', track_id: 'a1', at: 2 });
		const merge = mergeSubset(base, base, together, both, ['clip_moved:v']);
		expect(merge.autoIncluded.map((a) => a.id)).toEqual(['clip_moved:s']);
		expect(merge.timeline.tracks[1].clips[0].timeline_start).toBe(2);
		// A pair that was apart before the pick is not this merge's doing.
		const loose: Timeline = structuredClone(base);
		loose.tracks[1].clips[0].timeline_start = 1;
		const proposedLoose = structuredClone(loose);
		proposedLoose.tracks[0].clips[0].volume = 0.5;
		proposedLoose.tracks[1].clips[0].volume = 0.5;
		expect(() => mergeSubset(loose, loose, proposedLoose, diffOf(changed('v')), ['clip_changed:v'])).not.toThrow();
	});

	test('animated titles apply by identity like text', () => {
		const title = (id: string, start: number): SvgOverlay => ({
			id,
			name: id,
			svg: "<svg xmlns='http://www.w3.org/2000/svg'/>",
			start,
			end: start + 2,
			x: 0,
			y: 0,
			width: 1,
			height: 1,
			opacity: 1
		});
		const base = cut();
		base.svg_overlays = [title('kept', 0), title('dropped', 4)];
		const proposed = structuredClone(base);
		proposed.svg_overlays![0].opacity = 0.5;
		proposed.svg_overlays!.splice(1, 1);
		proposed.svg_overlays!.push(title('added', 8));
		const diff = diffOf(
			{ kind: 'overlay_changed', summary: 'Changed kept', overlay_id: 'kept', at: 0 },
			{ kind: 'overlay_removed', summary: 'Removed dropped', overlay_id: 'dropped', at: 4 },
			{ kind: 'overlay_added', summary: 'Added added', overlay_id: 'added', at: 8 }
		);
		const ids = diff.entries.map(entryId);
		// The user dropped one of them meanwhile: that pick is skipped, the others land.
		const live = structuredClone(base);
		live.svg_overlays!.splice(1, 1);
		const merge = mergeSubset(live, base, proposed, diff, ids);
		expect(merge.skipped).toHaveLength(1);
		expect(merge.skipped[0].why).toContain('animated title');
		expect(merge.timeline.svg_overlays!.map((o) => o.id)).toEqual(['kept', 'added']);
		expect(merge.timeline.svg_overlays![0].opacity).toBe(0.5);
		expect(merge.timeline.overlays ?? []).toEqual([]);
	});

	test('picking everything reproduces the proposal', () => {
		const base = cut();
		const proposed = cut();
		proposed.tracks[0].clips[0].source_out = 3;
		proposed.tracks[0].clips[1].timeline_start = 3;
		proposed.tracks[0].clips[1].volume = 0.4;
		const diff = diffOf(
			{ kind: 'clip_retrimmed', summary: 'Trimmed a', clip_id: 'a', track_id: 'v1', at: 0 },
			{ kind: 'clip_moved', summary: 'Moved b', clip_id: 'b', track_id: 'v1', at: 3 },
			changed('b')
		);
		const merge = mergeSubset(base, base, proposed, diff, diff.entries.map(entryId));
		expect(merge.applied).toHaveLength(3);
		expect(merge.skipped).toEqual([]);
		expect(merge.timeline).toEqual(proposed);
	});

	test('picking a removed track drops it only after its clips moved off', () => {
		const base = cut();
		base.tracks.push({ id: 'v2', kind: 'video', name: 'V2', clips: [clip('m', 10, 2)] });
		const proposed = structuredClone(base);
		proposed.tracks[2].clips = [];
		proposed.tracks[0].clips.push(clip('m', 10, 2));
		proposed.tracks = proposed.tracks.filter((t) => t.id !== 'v2');
		const diff = diffOf(
			{ kind: 'track_removed', summary: 'Removed track V2', track_id: 'v2' },
			{ kind: 'clip_moved', summary: 'Moved m to V1', clip_id: 'm', track_id: 'v1', at: 10 }
		);
		const merge = mergeSubset(base, base, proposed, diff, ['track_removed:v2']);
		expect(merge.autoIncluded.map((a) => a.id)).toEqual(['clip_moved:m']);
		expect(merge.timeline.tracks.map((t) => t.id)).toEqual(['v1', 'a1']);
		expect(merge.timeline.tracks[0].clips.map((c) => c.id)).toContain('m');
	});
});

describe('subsetMessage', () => {
	test('says what landed, what came along and what is left', () => {
		expect(subsetMessage(1, 3, [], [], 2)).toBe('Applied 1 of 3 changes; 2 left to review.');
		expect(subsetMessage(1, 1, [], [], 0)).toBe('Applied 1 of 1 change.');
		expect(
			subsetMessage(2, 3, [{ id: 'x', summary: 'Changed s', because: 'linked' }], [{ id: 'y', summary: 'z', why: 'gone' }], 1)
		).toBe('Applied 2 of 3 changes — 1 came with the ones you picked (Changed s); 1 no longer apply; 1 left to review.');
	});
});
