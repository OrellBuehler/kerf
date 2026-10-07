import { afterEach, beforeEach, describe, expect, test } from 'bun:test';
import './test-runes';
import { getHistory, revertTo, setRippleMode, setTrackLocked } from './api';

// The editor's selection actions over the browser harness's cut — V1 `c1 [0, 12.5)`
// `c2 [12.5, 20.5)` over A1 `c3 [0, 120)` — which enforces the same locking and
// all-or-nothing rules as the backend.

const { editor } = await import('./state.svelte');
const ids = () => editor.timeline.tracks.flatMap((t) => t.clips.map((c) => c.id));
const revision = async () => (await getHistory()).find((r) => r.current)!.seq;

beforeEach(async () => {
	await setRippleMode(false);
	await setTrackLocked('v1', false);
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
	editor.clipboard = [];
});
afterEach(async () => {
	await setTrackLocked('v1', false);
	await revertTo(0);
});

describe('removeSelected', () => {
	test('is one edit for the whole selection', async () => {
		editor.selectClips(['c1', 'c3']);
		const before = await revision();
		expect(await editor.removeSelected(false)).toEqual({ removed: 2, skipped: 0 });
		expect(ids()).toEqual(['c2']);
		expect((await revision()) - before).toBe(1);
	});

	test('leaves a clip on a locked track where it is, selected, and says so', async () => {
		await setTrackLocked('v1', true);
		await editor.load();
		editor.selectClips(['c1', 'c3']);
		expect(await editor.removeSelected(false)).toEqual({ removed: 1, skipped: 1 });
		expect(ids()).toEqual(['c1', 'c2']);
		expect(editor.selectedClipIds).toEqual(['c1']);
	});

	test('with only locked clips selected it removes nothing and writes no revision', async () => {
		await setTrackLocked('v1', true);
		await editor.load();
		editor.selectClips(['c1', 'c2']);
		const before = await revision();
		expect(await editor.removeSelected(false)).toEqual({ removed: 0, skipped: 2 });
		expect(ids()).toEqual(['c1', 'c2', 'c3']);
		expect(await revision()).toBe(before);
		expect(editor.selectedClipIds).toEqual(['c1', 'c2']);
	});
});

describe('copySelection (a cut takes only what it can remove)', () => {
	test('a plain copy takes everything selected', async () => {
		await setTrackLocked('v1', true);
		await editor.load();
		editor.selectClips(['c1', 'c3']);
		expect(editor.copySelection()).toBe(2);
	});

	test('editableOnly leaves out clips on a locked track', async () => {
		await setTrackLocked('v1', true);
		await editor.load();
		editor.selectClips(['c1', 'c3']);
		expect(editor.copySelection(true)).toBe(1);
		expect(editor.clipboard.map((p) => p.clip.id)).toEqual(['c3']);
	});

	test('editableOnly with nothing left does not clobber the clipboard', async () => {
		editor.selectClips(['c3']);
		editor.copySelection();
		await setTrackLocked('v1', true);
		await editor.load();
		editor.selectClips(['c1']);
		expect(editor.copySelection(true)).toBe(0);
		expect(editor.clipboard.map((p) => p.clip.id)).toEqual(['c3']);
	});
});

describe('the selection set', () => {
	test('a timeline that no longer has a clip drops it from the selection', async () => {
		editor.selectClips(['c1', 'c2', 'c3'], 'c2');
		await editor.remove('c2');
		expect(editor.selectedClipIds).toEqual(['c1', 'c3']);
		expect(editor.selectedClipId).toBe('c3');
	});

	test('setPrimary moves the Inspector without touching the set', async () => {
		editor.selectClips(['c1', 'c2'], 'c2');
		editor.setPrimary('c1');
		expect(editor.selectedClipIds).toEqual(['c1', 'c2']);
		expect(editor.selectedClipId).toBe('c1');
		editor.setPrimary('c3'); // not selected: ignored
		expect(editor.selectedClipId).toBe('c1');
	});

	test('a click narrows, Ctrl toggles, Shift extends along the track', async () => {
		editor.selectClip('c1');
		editor.selectClip('c3', 'toggle');
		expect([...editor.selectedClipIds].sort()).toEqual(['c1', 'c3']);
		expect(editor.selectedClipId).toBe('c3');
		editor.selectClip('c2', 'range'); // the primary (c3) is on another track: just adds
		expect([...editor.selectedClipIds].sort()).toEqual(['c1', 'c2', 'c3']);
		editor.selectClip('c1');
		expect(editor.selectedClipIds).toEqual(['c1']);
	});

	test('moveClips is one edit and a refused group changes nothing', async () => {
		const before = await revision();
		await editor.moveClips([
			{ clip_id: 'c1', timeline_start: 3 },
			{ clip_id: 'c2', timeline_start: 15.5 }
		]);
		expect((await revision()) - before).toBe(1);
		const c1 = editor.timeline.tracks[0].clips.find((c) => c.id === 'c1')!;
		expect(c1.timeline_start).toBe(3);
		const refused = editor.moveClips([
			{ clip_id: 'c1', timeline_start: 16 }, // onto c2, which is not moving
			{ clip_id: 'c3', timeline_start: 1 }
		]);
		await expect(refused).rejects.toThrow('overlap');
		expect((await revision()) - before).toBe(1);
	});
});
