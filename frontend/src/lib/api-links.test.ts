import { beforeEach, describe, expect, test } from 'bun:test';
import {
	cutClipRange,
	detachAudio,
	extractAudio,
	getHistory,
	getTimeline,
	linkClips,
	moveClip,
	reattachAudio,
	removeClip,
	removeClips,
	reorderClip,
	revertTo,
	setRippleMode,
	setSpeed,
	setTrackLocked,
	slideClip,
	splitClip,
	trimClip,
	undo,
	unlinkClips
} from './api';
import { linkPartners } from './link-groups';
import type { Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's answer to the
// linked-A/V surface — the same contract as the backend's (the pure math is
// `links.ts` / `link-groups.ts` / `edit-modes.ts` / `ripple.ts`, tested case for case
// against the Rust tests in `links.test.ts`). The harness cut is V1 `c1 [0, 12.5)` of
// the 120 s interview (source 0..12.5, picture + sound) abutting `c2 [12.5, 20.5)` of
// the 45 s b-roll (no sound), over A1 `c3`, the interview's whole 120 s of audio — so A1
// is busy under c1 and a detached sound lands on a new track.

const clip = (t: Timeline, id: string) => t.tracks.flatMap((tr) => tr.clips).find((c) => c.id === id);
const headSeq = async () => (await getHistory()).find((r) => r.current)!.seq;
const lastLabel = async () => (await getHistory()).at(-1)!.label;
/** The audio clip linked to `c1` after a detach, and the timeline it is on. */
const soundOf = (t: Timeline, id: string) => clip(t, linkPartners(t, id)[0])!;

beforeEach(async () => {
	await setRippleMode(false);
	await revertTo(0);
});

describe('linked A/V (browser harness)', () => {
	test('detaching is one revision, mutes the picture, links the pair and undo puts it all back', async () => {
		const before = await headSeq();
		const t = await detachAudio('c1');
		expect(await headSeq()).toBe(before + 1);
		expect(await lastLabel()).toBe('Detach audio');
		expect(clip(t, 'c1')!.source_audio).toBe(false);
		const sound = soundOf(t, 'c1');
		expect([sound.source_in, sound.source_out, sound.timeline_start]).toEqual([0, 12.5, 0]);
		expect(t.tracks).toHaveLength(3); // A1 was busy under c1, so the sound got a track of its own
		expect(t.tracks[2].name).toBe('A2');
		const undone = await undo();
		expect(undone.tracks).toHaveLength(2);
		expect(clip(undone, 'c1')!.source_audio).toBeUndefined();
		expect(clip(undone, 'c1')!.link_id).toBeUndefined();
	});

	test('a clip with no sound, or one already detached, is refused and records nothing', async () => {
		const before = await headSeq();
		await expect(detachAudio('c2')).rejects.toThrow('no audio stream');
		expect(await headSeq()).toBe(before);
		await detachAudio('c1');
		await expect(detachAudio('c1')).rejects.toThrow('already detached');
		expect(await headSeq()).toBe(before + 1);
	});

	test('reattaching deletes the sound and unmutes the picture, naming either clip', async () => {
		let t = await detachAudio('c1');
		const soundId = soundOf(t, 'c1').id;
		t = await reattachAudio(soundId);
		expect(clip(t, soundId)).toBeUndefined();
		expect(clip(t, 'c1')!.source_audio).toBeUndefined();
		expect(await lastLabel()).toBe('Reattach audio');
	});

	test('a move carries the partner, as one revision, and link false moves one alone', async () => {
		let t = await detachAudio('c1');
		const soundId = soundOf(t, 'c1').id;
		// Free space on both tracks: a gap after c2 and nothing after the sound.
		t = await moveClip('c2', 40);
		expect(clip(t, 'c2')!.timeline_start).toBe(40);
		const before = await headSeq();
		t = await moveClip('c1', 5, undefined, false);
		expect([clip(t, 'c1')!.timeline_start, clip(t, soundId)!.timeline_start]).toEqual([5, 0]);
		await revertTo(before);
		t = await moveClip('c1', 5);
		expect([clip(t, 'c1')!.timeline_start, clip(t, soundId)!.timeline_start]).toEqual([5, 5]);
		expect(await lastLabel()).toBe('Move 2 clips');
		expect(await headSeq()).toBe(before + 1);
	});

	test('a locked partner refuses the move and leaves no trace', async () => {
		const t = await detachAudio('c1');
		const audioTrack = t.tracks.find((tr) => tr.clips.some((c) => c.id === soundOf(t, 'c1').id))!;
		await setTrackLocked(audioTrack.id, true);
		const revisions = (await getHistory()).length;
		const json = JSON.stringify(await getTimeline());
		await expect(moveClip('c1', 5)).rejects.toThrow('locked');
		await expect(trimClip('c1', undefined, 10)).rejects.toThrow('locked');
		await expect(splitClip('c1', 4)).rejects.toThrow('locked');
		await expect(removeClip('c1')).rejects.toThrow('locked');
		await expect(setSpeed('c1', 2)).rejects.toThrow('locked');
		await expect(slideClip('c1', 1)).rejects.toThrow('locked');
		expect((await getHistory()).length).toBe(revisions);
		expect(JSON.stringify(await getTimeline())).toBe(json);
	});

	test('a trim carries the shared edge, and ripple takes the partner of the next shot along', async () => {
		const t = await detachAudio('c1');
		const soundId = soundOf(t, 'c1').id;
		await setRippleMode(true);
		const trimmed = await trimClip('c1', undefined, 8);
		expect(clip(trimmed, 'c1')!.source_out).toBe(8);
		expect(clip(trimmed, soundId)!.source_out).toBe(8);
		expect(clip(trimmed, 'c2')!.timeline_start).toBe(8);
	});

	test('a split cuts the partner too and the new halves are a pair of their own', async () => {
		let t = await detachAudio('c1');
		const soundId = soundOf(t, 'c1').id;
		t = await splitClip('c1', 4);
		expect(await lastLabel()).toBe('Split clip');
		expect(clip(t, soundId)!.source_out).toBe(4);
		const right = t.tracks[0].clips.find((c) => c.timeline_start === 4)!;
		const rightSound = clip(t, linkPartners(t, right.id)[0])!;
		expect([rightSound.timeline_start, rightSound.source_in]).toEqual([4, 4]);
		expect(linkPartners(t, 'c1')).toEqual([soundId]);
		t = await splitClip('c1', 2, false);
		expect(linkPartners(t, 'c1')).toEqual([soundId]);
		expect(t.tracks[2].clips).toHaveLength(2); // the sound was not cut at 2
	});

	test('removing a clip removes its partner and the history says how many', async () => {
		let t = await detachAudio('c1');
		const soundId = soundOf(t, 'c1').id;
		t = await removeClip('c1');
		expect(await lastLabel()).toBe('Remove 2 clips');
		expect(clip(t, 'c1')).toBeUndefined();
		expect(clip(t, soundId)).toBeUndefined();
		t = await undo();
		t = await removeClips(['c1'], undefined, false);
		expect(clip(t, soundId)).toBeDefined();
		expect(await lastLabel()).toBe('Remove clip');
	});

	test('cutting a range cuts the sound and a speed change retimes it', async () => {
		let t = await detachAudio('c1');
		const soundId = soundOf(t, 'c1').id;
		t = await setSpeed('c1', 2);
		expect(clip(t, soundId)!.speed).toBe(2);
		t = await undo();
		t = await cutClipRange('c1', 3, 5);
		expect(clip(t, 'c1')!.source_out).toBe(3);
		expect(clip(t, soundId)!.source_out).toBe(3);
	});

	test('link and unlink are one revision each', async () => {
		const before = await headSeq();
		const t = await linkClips(['c1', 'c3']);
		expect(clip(t, 'c1')!.link_id).toBe(clip(t, 'c3')!.link_id);
		expect(await lastLabel()).toBe('Link 2 clips');
		await expect(linkClips(['c1', 'c3'])).rejects.toThrow('already linked');
		const u = await unlinkClips(['c3']);
		expect(clip(u, 'c1')!.link_id).toBeUndefined();
		expect(await lastLabel()).toBe('Unlink clips');
		expect(await headSeq()).toBe(before + 2);
		await expect(unlinkClips(['c1'])).rejects.toThrow('none of those clips is linked');
	});

	test('extracting audio detaches the clips of an asset that is cut, rather than doubling it', async () => {
		const t = await extractAudio('11111111-1111-1111-1111-111111111111');
		expect(clip(t, 'c1')!.source_audio).toBe(false);
		expect(linkPartners(t, 'c1')).toHaveLength(1);
		expect(await lastLabel()).toBe('Extract audio');
	});
});

describe('the sync guard (browser harness)', () => {
	test('a reorder that would part a linked pair is refused, and nothing is recorded', async () => {
		await detachAudio('c1');
		const before = await headSeq();
		const json = JSON.stringify(await getTimeline());
		await expect(reorderClip('v1', 'c2', 0)).rejects.toThrow('out of step');
		expect(await headSeq()).toBe(before);
		expect(JSON.stringify(await getTimeline())).toBe(json);
	});

	test('an unlinked project reorders exactly as it always did', async () => {
		const t = await reorderClip('v1', 'c2', 0);
		expect(t.tracks[0].clips.map((c) => c.id)).toEqual(['c2', 'c1']);
	});
});
