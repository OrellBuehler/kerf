import { afterAll, beforeEach, describe, expect, test } from 'bun:test';
import {
	applyEdits,
	clearKeyframes,
	cutClipRange,
	extractRange,
	getHistory,
	getRippleMode,
	getTimeline,
	redo,
	revertTo,
	setColor,
	setKeyframes,
	setPropertyTracks,
	setRippleMode,
	setVideoEffects,
	setVolume,
	splitClip,
	trimClip,
	undo,
	unlinkClips
} from './api';
import { MAX_LABEL_CHARS, literal } from './batch-edits';
import type { EditOp } from './types';
import type { Clip, Keyframe, PropertyTrack, Timeline } from './types';

// `applyEdits` over the browser harness, which has to honour the contract of the backend's `apply_edits`: the ops
// of the MCP tool, run through the single-op functions, as ONE revision, all or nothing. The harness cut is V1
// `c1 [0, 12.5)  c2 [12.5, 20.5)` over A1 `c3 [0, 12.5)` (the interview's sound, linked to c1).

const clipOf = (t: Timeline, id: string): Clip => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === id)!;
const revisions = async () => (await getHistory()).length;
const current = async () => (await getHistory()).find((r) => r.current)!;
// An edit after `revertTo` drops the redo branch, so the length of the history is no baseline: the head's seq is.
const head = async () => (await current()).seq;

const key = (time: number, scale: number): Keyframe => ({ time, scale, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 });
const warmUp: PropertyTrack[] = [
	{
		prop: 'brightness',
		keys: [
			{ time: 0, value: 0 },
			{ time: 2, value: 0.2 }
		]
	}
];

beforeEach(async () => {
	await setRippleMode(false);
	await revertTo(0);
});

// The harness is one process-wide state, shared with every test file that runs after this one.
afterAll(async () => {
	await setRippleMode(false);
	await revertTo(0);
});

describe('applyEdits in the browser harness', () => {
	const tidy: EditOp[] = [
		{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } },
		{ tool: 'trim', args: { clip_id: 'c2', source_out: 5, ripple: false } },
		{ tool: 'set_keyframes', args: { clip_id: 'c2', keyframes: [key(0, 1), key(2, 1.5)] } },
		{ tool: 'set_property_tracks', args: { clip_id: 'c2', tracks: warmUp } }
	];

	test('several edits are one revision, named by the label, and one undo takes them all back', async () => {
		const before = await getTimeline();
		const was = await head();
		const done = await applyEdits(tidy, 'Tidy the b-roll');
		expect(await head()).toBe(was + 1);
		expect((await getHistory()).length).toBe(was + 2);
		expect(await current()).toMatchObject({ label: 'Tidy the b-roll', source: 'user' });
		expect(done).toMatchObject({ applied: true, label: 'Tidy the b-roll' });
		const c2 = clipOf(done.timeline, 'c2');
		expect([c2.volume, c2.source_out, c2.keyframes?.length]).toEqual([0.5, 5, 2]);
		expect(await getTimeline()).toEqual(done.timeline);

		expect(await undo()).toEqual(before);
		expect(await head()).toBe(was);
	});

	test('what it leaves is what the single-op functions leave, step by step', async () => {
		const batched = (await applyEdits(tidy)).timeline;
		await revertTo(0);
		await setVolume('c2', 0.5);
		await trimClip('c2', undefined, 5);
		await setKeyframes('c2', [key(0, 1), key(2, 1.5)]);
		const stepped = await setPropertyTracks('c2', warmUp);
		expect(batched).toEqual(stepped);
	});

	test('the answer of an op is what the tool answers: the clip it edited, as it ended up', async () => {
		const done = await applyEdits(tidy);
		expect(done.results).toHaveLength(4);
		expect(done.results[0]).toMatchObject({ id: 'c2', volume: 0.5 });
		expect(done.results[1]).toMatchObject({ id: 'c2', source_out: 5 });
		expect(done.results[3]).toEqual(clipOf(done.timeline, 'c2'));
	});

	test('a cut answers its pieces, cut down to their place, and a split its halves', async () => {
		await unlinkClips(['c1']);
		const cut = await applyEdits([{ tool: 'cut_clip_range', args: { clip_id: 'c1', from: 2, to: 3 } }]);
		const [pieces] = cut.results as { id: string; timeline_start: number; timeline_end: number; transform?: unknown }[][];
		expect(pieces.length).toBeGreaterThanOrEqual(2);
		for (const piece of pieces) {
			const there = clipOf(cut.timeline, piece.id);
			expect([piece.timeline_start, piece.timeline_end]).toEqual([there.timeline_start, there.timeline_start + (there.source_out - there.source_in)]);
			expect(piece.transform).toBeUndefined();
		}
		// The same pieces the single call leaves.
		await revertTo(0);
		await unlinkClips(['c1']);
		const single = await cutClipRange('c1', 2, 3, false);
		expect(single.tracks[0].clips.map((c) => [c.timeline_start, c.source_in, c.source_out])).toEqual(
			cut.timeline.tracks[0].clips.map((c) => [c.timeline_start, c.source_in, c.source_out])
		);

		await revertTo(0);
		const split = await applyEdits([
			{ tool: 'split_at', args: { clip_id: 'c2', at: 14, link: false } },
			{ tool: 'set_volume', args: { clip_id: '$0.right.id', volume: 0.25 } }
		]);
		const { left, right } = split.results[0] as { left: { id: string }; right: { id: string } };
		expect(left.id).toBe('c2');
		expect(clipOf(split.timeline, right.id).volume).toBe(0.25);
		expect(clipOf(split.timeline, 'c2').volume).toBe(1);
	});

	test('with no label one edit is named as it is, several by their count and kinds', async () => {
		await applyEdits([{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } }]);
		const one = await current();
		await revertTo(0);
		await setVolume('c2', 0.5);
		expect(one.label).toBe((await current()).label);

		await revertTo(0);
		const done = await applyEdits(tidy);
		expect(done.label).toBe('4 edits: Set volume, Trim clip, Set keyframes, …');
		expect((await current()).label).toBe(done.label!);

		await revertTo(0);
		expect((await applyEdits(tidy, '   ')).label).toBe('4 edits: Set volume, Trim clip, Set keyframes, …');
		await revertTo(0);
		expect((await applyEdits(tidy, '  Tidy  ')).label).toBe('Tidy');
	});

	test('the name is cut to the longest a revision keeps, and a blank one is no name', async () => {
		const done = await applyEdits([{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } }], 'a'.repeat(250));
		expect(done.label).toBe('a'.repeat(MAX_LABEL_CHARS - 1) + '…');
		expect((await current()).label).toBe(done.label!);
	});

	test('text a user typed that starts with a dollar is its own word through `literal`, and a reference without it', async () => {
		const done = await applyEdits([{ tool: 'set_transition', args: { clip_id: 'c2', kind: literal('$100'), duration: 1 } }]);
		expect(clipOf(done.timeline, 'c2').transition_in).toEqual({ kind: '$100', duration: 1 } as never);
		await expect(applyEdits([{ tool: 'set_transition', args: { clip_id: 'c2', kind: '$100', duration: 1 } }])).rejects.toThrow(
			'op 0 (set_transition): "$100" refers to op 100, which has not run yet'
		);
	});

	test('a batch of two hundred and six cuts is one revision', async () => {
		await unlinkClips(['c1']);
		const was = await head();
		// c1 holds 12.5 s of footage: cut the later pieces first, so every cut is of the clip that keeps the id.
		const cuts: EditOp[] = Array.from({ length: 206 }, (_, i) => {
			const from = 0.5 + (205 - i) * 0.05;
			return { tool: 'cut_clip_range', args: { clip_id: 'c1', from, to: from + 0.02, link: false } };
		});
		const done = await applyEdits(cuts, 'Cut every "um"');
		expect(done.results).toHaveLength(206);
		expect(await head()).toBe(was + 1);
		expect(done.timeline.tracks[0].clips.length).toBeGreaterThan(100);
	});

	test('a recorded batch drops the redo branch, as an edit does', async () => {
		await setVolume('c2', 0.5);
		await undo();
		await applyEdits([{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.25 } }]);
		const again = await redo();
		expect(clipOf(again, 'c2').volume).toBe(0.25);
	});
});

describe('all or nothing', () => {
	test('an op that fails undoes the ones before it, records nothing and names itself', async () => {
		await setVolume('c2', 0.5);
		await undo(); // a redo branch to keep
		const before = await getTimeline();
		const history = await getHistory();
		await expect(
			applyEdits([
				{ tool: 'set_volume', args: { clip_id: 'c1', volume: 0.5 } },
				{ tool: 'split_at', args: { clip_id: 'c2', at: 14, link: false } },
				{ tool: 'trim', args: { clip_id: 'c2', source_in: -1 } }
			])
		).rejects.toThrow('op 2 (trim): ');
		expect(await getTimeline()).toEqual(before);
		expect(clipOf(await getTimeline(), 'c1').volume).toBe(1);
		expect(await getHistory()).toEqual(history);
		expect(clipOf(await redo(), 'c2').volume).toBe(0.5);
	});

	test('an unknown clip, a missing argument and a bad reference refuse the batch the same way', async () => {
		const before = await getTimeline();
		const revs = await revisions();
		for (const [bad, said] of [
			[{ tool: 'set_volume', args: { clip_id: 'nope', volume: 0.5 } }, 'op 1 (set_volume): '],
			[{ tool: 'set_volume', args: { clip_id: 'c1' } }, 'op 1 (set_volume): failed to deserialize parameters: missing field `volume`'],
			[{ tool: 'set_volume', args: { clip_id: '$3.id', volume: 0.5 } }, 'op 1 (set_volume): "$3.id" refers to op 3, which has not run yet']
		] as [EditOp, string][]) {
			const edits = [{ tool: 'set_volume', args: { clip_id: 'c1', volume: 0.5 } }, bad];
			await expect(applyEdits(edits)).rejects.toThrow(said);
			expect(await getTimeline()).toEqual(before);
		}
		expect(await revisions()).toBe(revs);
	});

	test('the project ripple mode is back as it was after a call that forced it, failed or not', async () => {
		await setRippleMode(true);
		await applyEdits([{ tool: 'trim', args: { clip_id: 'c2', source_out: 5, ripple: false } }]);
		expect(await getRippleMode()).toBe(true);
		await setRippleMode(false);
		await expect(
			applyEdits([
				{ tool: 'trim', args: { clip_id: 'c2', source_out: 5, ripple: true } },
				{ tool: 'set_volume', args: { clip_id: 'nope', volume: 1 } }
			])
		).rejects.toThrow('op 1');
		expect(await getRippleMode()).toBe(false);
	});

	test('what is refused before anything runs changes nothing', async () => {
		await expect(applyEdits([{ tool: 'set_volume', args: null as never }])).rejects.toThrow('expected a map');
		const before = await getTimeline();
		const revs = await revisions();
		const run = (edits: EditOp[]) => applyEdits(edits);
		await expect(run([])).rejects.toThrow('at least one edit');
		await expect(run([{ tool: 'export' }])).rejects.toThrow('op 0 (export): not an edit');
		await expect(run([{ tool: 'set_volume', args: { clip_id: 'c1', volume: 1 } }, { tool: 'trim', args: { clip_id: 'c1', rippel: true } }])).rejects.toThrow(
			'op 1 (trim): unknown argument "rippel" for trim'
		);
		await expect(run([{ tool: 'set_grade', args: { clip_id: 'c1', lut_path: '/a.cube' } }])).rejects.toThrow('lut_path');
		await expect(run([{ tool: 'add_marker', args: { time: 1, name: 'x' } }])).rejects.toThrow('not available in the browser harness');
		expect(await getTimeline()).toEqual(before);
		expect(await revisions()).toBe(revs);
	});
});

describe('ripple and links are the op`s own', () => {
	test('ripple on the op carries the later clips, as the single call does with the project in ripple mode', async () => {
		const done = await applyEdits([{ tool: 'trim', args: { clip_id: 'c1', source_out: 10, ripple: true, link: false } }]);
		expect(clipOf(done.timeline, 'c2').timeline_start).toBe(10);
		await revertTo(0);
		await setRippleMode(true);
		const single = await trimClip('c1', undefined, 10, undefined, false);
		expect(clipOf(single, 'c2').timeline_start).toBe(10);
	});

	test('without it the project decides, and a linked partner follows a split unless link is false', async () => {
		const linked = await applyEdits([{ tool: 'split_at', args: { clip_id: 'c1', at: 5 } }]);
		expect(linked.timeline.tracks[1].clips).toHaveLength(2);
		await revertTo(0);
		const alone = await applyEdits([{ tool: 'split_at', args: { clip_id: 'c1', at: 5, link: false } }]);
		expect(alone.timeline.tracks[1].clips).toHaveLength(1);
		await revertTo(0);
		expect((await splitClip('c1', 5)).tracks[1].clips).toHaveLength(2);
	});
});

describe('range edits', () => {
	test('a span through several clips is one edit, answered as the tool answers', async () => {
		const done = await applyEdits([{ tool: 'extract_range', args: { from: 10, to: 14 } }], 'Cut the span');
		expect(done.label).toBe('Cut the span');
		expect(done.results[0]).toMatchObject({ from: 10, to: 14, closed: 4 });
		await revertTo(0);
		const single = await extractRange(10, 14);
		expect(done.timeline).toEqual(single.timeline);
		expect(done.results[0]).toEqual(single.result);
	});

	test('they join other edits in one revision, and the clips in their answers are cut down to their place', async () => {
		const was = await head();
		const done = await applyEdits([
			{ tool: 'split_all_tracks', args: { at: 5 } },
			{ tool: 'lift_range', args: { from: 16, to: 18 } },
			{ tool: 'ripple_delete_gap', args: { track_id: 'v1', at: 17 } }
		]);
		expect(await head()).toBe(was + 1);
		const cut = done.results[0] as { cuts: { left: { id: string; timeline_end: number; transform?: unknown } }[] };
		expect(cut.cuts.length).toBeGreaterThan(0);
		expect(cut.cuts[0].left.timeline_end).toBeGreaterThan(0);
		expect(cut.cuts[0].left.transform).toBeUndefined();
		expect(done.results[2]).toMatchObject({ track_id: 'v1' });
		await expect(applyEdits([{ tool: 'lift_range', args: { from: 3, to: 2 } }])).rejects.toThrow('op 0 (lift_range)');
		expect(await head()).toBe(was + 1);
	});
});

describe('the look and the keyframes', () => {
	test('the Inspector’s two keyframe stores are cleared in one revision', async () => {
		await setKeyframes('c2', [key(0, 1), key(1, 2)]);
		await setPropertyTracks('c2', warmUp);
		const was = await head();
		const done = await applyEdits(
			[
				{ tool: 'set_keyframes', args: { clip_id: 'c2', keyframes: [] } },
				{ tool: 'set_property_tracks', args: { clip_id: 'c2', tracks: [{ prop: 'brightness', keys: [] }] } }
			],
			'Remove keyframe'
		);
		expect(await head()).toBe(was + 1);
		const c2 = clipOf(done.timeline, 'c2');
		expect(c2.keyframes ?? []).toEqual([]);
		expect((c2.channels ?? []).filter((c) => c.prop === 'brightness')).toEqual([]);
	});

	test('effects, colour and a grade are patches and replacements as the tools say', async () => {
		const done = await applyEdits([
			{ tool: 'set_video_effects', args: { clip_id: 'c2', effects: [{ type: 'grayscale' }] } },
			{ tool: 'set_audio_effects', args: { clip_id: 'c2', effects: [{ type: 'highpass', hz: 90 }] } },
			{ tool: 'set_color', args: { clip_id: 'c2', brightness: 0.1, saturation: 1.2 } },
			{ tool: 'set_grade', args: { clip_id: 'c2', wheels: { shadows: { x: -0.2 } }, curves: { master: [{ x: 0, y: 0 }, { x: 1, y: 0.9 }] } } },
			{ tool: 'set_grade', args: { clip_id: 'c2', curves: {} } },
			{ tool: 'set_blend_mode', args: { clip_id: 'c2', mode: 'Hard light' } },
			{ tool: 'set_mask', args: { clip_id: 'c2', shape: 'ellipse', x: 0.3 } },
			{ tool: 'set_mask', args: { clip_id: 'c2', shape: 'ellipse', y: 0.6 } }
		]);
		const c2 = clipOf(done.timeline, 'c2');
		expect(c2.effects).toEqual([{ type: 'grayscale' }]);
		expect(c2.audio).toEqual([{ type: 'highpass', hz: 90 }]);
		expect([c2.color?.brightness, c2.color?.saturation, c2.color?.contrast]).toEqual([0.1, 1.2, 1]);
		expect(c2.grade?.wheels?.shadows).toEqual({ x: -0.2, y: 0, lightness: 0 });
		expect(c2.grade?.curves).toBeUndefined();
		expect(c2.blend_mode).toBe('hard_light');
		expect(c2.mask).toMatchObject({ shape: 'ellipse', x: 0.3, y: 0.6 });
		await revertTo(0);
		await setVideoEffects('c2', [{ type: 'grayscale' }]);
		await setColor('c2', { brightness: 0.1, saturation: 1.2 });
		expect(clipOf(await getTimeline(), 'c2').color).toEqual(c2.color);
	});

	test('a grade that names no LUT keeps the clip’s, and an intensity alone needs one', async () => {
		await expect(applyEdits([{ tool: 'set_grade', args: { clip_id: 'c2', lut_intensity: 0.5 } }])).rejects.toThrow('has no LUT');
		await expect(applyEdits([{ tool: 'set_blend_mode', args: { clip_id: 'c2', mode: 'soft_light' } }])).rejects.toThrow('invalid blend mode');
		await expect(applyEdits([{ tool: 'set_mask', args: { clip_id: 'c2', shape: 'star' } }])).rejects.toThrow('invalid mask shape');
	});

	test('keyframes can be cleared with the others in one step', async () => {
		await setKeyframes('c2', [key(0, 1), key(1, 2)]);
		const done = await applyEdits([
			{ tool: 'clear_keyframes', args: { clip_id: 'c2' } },
			{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } }
		]);
		expect(clipOf(done.timeline, 'c2').keyframes ?? []).toEqual([]);
		await revertTo(0);
		await setKeyframes('c2', [key(0, 1), key(1, 2)]);
		expect(clipOf(await clearKeyframes('c2'), 'c2').keyframes ?? []).toEqual([]);
	});
});

describe('a dry run', () => {
	test('answers the cut the edits would make and keeps nothing', async () => {
		const before = await getTimeline();
		const history = await getHistory();
		const done = await applyEdits(
			[
				{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } },
				{ tool: 'remove', args: { clip_id: 'c1', ripple: false, link: false } }
			],
			'Never kept',
			{ dryRun: true }
		);
		expect(done.applied).toBe(false);
		expect(done.label).toBeUndefined();
		expect(done.results).toHaveLength(2);
		expect(clipOf(done.timeline, 'c2').volume).toBe(0.5);
		expect(done.timeline.tracks[0].clips.map((c) => c.id)).toEqual(['c2']);
		expect(await getTimeline()).toEqual(before);
		expect(await getHistory()).toEqual(history);
	});

	test('still refuses what a real run refuses', async () => {
		await expect(
			applyEdits([{ tool: 'set_volume', args: { clip_id: 'nope', volume: 0.5 } }], null, { dryRun: true })
		).rejects.toThrow('op 0 (set_volume)');
	});
});
