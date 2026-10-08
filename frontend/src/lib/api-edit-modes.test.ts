import { beforeEach, describe, expect, test } from 'bun:test';
import {
	addClip,
	getHistory,
	getTimeline,
	listAssets,
	revertTo,
	rollEdit,
	setRippleMode,
	setTrackLocked,
	slideClip,
	slipClip,
	splitRemove,
	splitRemoveClips,
	undo
} from './api';
import type { Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's answer to the
// edit-mode surface — the same contract as the backend's (the pure math is
// `edit-modes.ts`, tested case for case against the Rust tests). The harness cut
// is V1 `c1 [0, 12.5)` of the 120 s interview (source 0..12.5) abutting
// `c2 [12.5, 20.5)` of the 45 s b-roll (source 0..8), over A1 `c3`, the interview's
// sound for the same span — linked to c1 (its picture is muted: sound detached).

const starts = (t: Timeline, track = 0) => t.tracks[track].clips.map((c) => c.timeline_start);
const clip = (t: Timeline, id: string) => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === id)!;
/** The revision the project is at. (Not the history's length: after `revertTo` it still holds the redo branch.) */
const headSeq = async () => (await getHistory()).find((r) => r.current)!.seq;

beforeEach(async () => {
	await setRippleMode(false);
	await revertTo(0);
});

describe('roll / slip / slide (browser harness)', () => {
	test('a roll moves the cut, is one revision, and undo puts it back', async () => {
		const before = await headSeq();
		const t = await rollEdit('c1', 'c2', 2);
		expect(clip(t, 'c1').source_out).toBe(14.5);
		expect([clip(t, 'c2').timeline_start, clip(t, 'c2').source_in]).toEqual([14.5, 2]);
		expect(await headSeq()).toBe(before + 1);
		expect((await getHistory()).at(-1)!.label).toBe('Roll edit');
		expect(starts(await undo(), 0)).toEqual([0, 12.5]);
	});

	test('a roll clamps to the footage, and one with nowhere to go is refused and records nothing', async () => {
		// c2 starts at the head of its asset: nothing to extend into when the cut moves earlier.
		const revisions = (await getHistory()).length;
		await expect(rollEdit('c1', 'c2', -1)).rejects.toThrow('incoming clip has no footage left');
		expect((await getHistory()).length).toBe(revisions);
		expect(starts(await getTimeline(), 0)).toEqual([0, 12.5]);
		// c1's 120 s asset has 107.5 s past its out-point but c2 is 8 s: later is capped by c2's length.
		const t = await rollEdit('c1', 'c2', 50);
		expect(clip(t, 'c2').timeline_start).toBeCloseTo(20.45, 9);
	});

	test('a slip shifts the window and nothing else, in source seconds', async () => {
		const t = await slipClip('c1', 5);
		const c1 = clip(t, 'c1');
		expect([c1.source_in, c1.source_out, c1.timeline_start]).toEqual([5, 17.5, 0]);
		expect((await getHistory()).at(-1)!.label).toBe('Slip clip');
		await expect(slipClip('c1', -50)).resolves.toBeDefined(); // clamps at the start of the footage
		await expect(slipClip('c1', -1)).rejects.toThrow("before the clip's in-point");
	});

	test('a slide moves the clip while the touching neighbour gives way', async () => {
		const t = await slideClip('c1', 3); // first clip: later opens a gap before it, c2 is trimmed
		expect(starts(t, 0)).toEqual([3, 15.5]);
		expect(clip(t, 'c2').source_in).toBe(3);
		expect((await getHistory()).at(-1)!.label).toBe('Slide clip');
		await expect(slideClip('c1', 0)).rejects.toThrow('zero');
	});

	test('none of them ripple, whatever the mode', async () => {
		const run = async (ripple: boolean) => {
			await revertTo(0);
			await setRippleMode(ripple);
			await rollEdit('c1', 'c2', 2);
			await slipClip('c1', 1);
			return starts(await slideClip('c2', 1), 0);
		};
		expect(await run(true)).toEqual(await run(false));
	});

	test('a locked track refuses them, as the backend does', async () => {
		await setTrackLocked('v1', true);
		await expect(rollEdit('c1', 'c2', 1)).rejects.toThrow('V1 is locked');
		await expect(slipClip('c1', 1)).rejects.toThrow('V1 is locked');
		await expect(slideClip('c1', 1)).rejects.toThrow('V1 is locked');
		await expect(splitRemove('c1', 5, 'left')).rejects.toThrow('V1 is locked');
		await expect(splitRemoveClips([{ clip_id: 'c1', at: 5 }], 'left')).rejects.toThrow('V1 is locked');
	});
});

describe('split and remove (browser harness)', () => {
	test('off, the gap stays where the removed half was; the survivor keeps its id', async () => {
		// c1's linked sound is cut at the same moment: it is a group edit, one revision.
		const t = await splitRemove('c1', 5, 'left');
		const c1 = clip(t, 'c1');
		expect([c1.timeline_start, c1.source_in, c1.source_out]).toEqual([5, 5, 12.5]);
		expect(starts(t, 0)).toEqual([5, 12.5]);
		expect(starts(t, 1)).toEqual([5]);
		expect((await getHistory()).at(-1)!.label).toBe('Split and remove left (2 clips)');

		const right = await splitRemove('c2', 16.5, 'right');
		expect(clip(right, 'c2').source_out).toBe(4);
		expect((await getHistory()).at(-1)!.label).toBe('Split and remove right');
	});

	test('with links off only the named clip is cut', async () => {
		const t = await splitRemove('c1', 5, 'left', false);
		expect(starts(t, 0)).toEqual([5, 12.5]);
		expect(starts(t, 1)).toEqual([0]);
		expect((await getHistory()).at(-1)!.label).toBe('Split and remove left');
	});

	test('on, the track closes up behind it and a left removal keeps the clips start', async () => {
		const [interview] = await listAssets();
		await addClip(interview.id, 0, 4, 'v1', 24); // c4, with a 3.5 s gap before it
		await setRippleMode(true);
		const t = await splitRemove('c2', 16.5, 'left'); // 4 s of c2's 8 go
		expect(clip(t, 'c2').timeline_start).toBe(12.5);
		expect(starts(t, 0)).toEqual([0, 12.5, 20]);
		expect(starts(t, 1)).toEqual([0]); // no sync lock
	});

	test('a group cut is ONE revision, undone in one step, and a refused one records nothing', async () => {
		// c1 [0,12.5) on V1 over c3 [0,12.5) on A1: a picture and its sound.
		const before = await headSeq();
		const t = await splitRemoveClips(
			[
				{ clip_id: 'c1', at: 5 },
				{ clip_id: 'c3', at: 5 }
			],
			'left'
		);
		expect([clip(t, 'c1').timeline_start, clip(t, 'c3').timeline_start]).toEqual([5, 5]);
		expect(await headSeq()).toBe(before + 1);
		expect((await getHistory()).at(-1)!.label).toBe('Split and remove left (2 clips)');
		const undone = await undo();
		expect([clip(undone, 'c1').timeline_start, clip(undone, 'c3').timeline_start]).toEqual([0, 0]);

		const seq = await headSeq();
		await expect(
			splitRemoveClips(
				[
					{ clip_id: 'c1', at: 5 },
					{ clip_id: 'c3', at: 500 }
				],
				'right'
			)
		).rejects.toThrow('not inside the clip');
		expect(await headSeq()).toBe(seq);
		expect(clip(await getTimeline(), 'c1').source_out).toBe(12.5);
	});

	test('a cut outside the clip is refused', async () => {
		await expect(splitRemove('c1', 30, 'right')).rejects.toThrow('not inside the clip');
	});
});
