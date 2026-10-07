import { beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// The four linked-A/V actions as the keymap and the clip menu run them (`ops.ts`), over the
// browser harness's cut: V1 `c1 [0, 12.5)` (the interview's picture, sound detached) and
// `c2 [12.5, 20.5)` over A1 `c3 [0, 12.5)`, c1's sound, linked to it. The toasts stand in for
// what only a window shows (`notifications.svelte` pulls in svelte-sonner's components).

const shown: { kind: string; text: string; undo?: () => void }[] = [];
const note =
	(kind: string) =>
	(text: string, opts?: { action?: { onClick: () => void } }) =>
		void shown.push({ kind, text, undo: opts?.action?.onClick });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	})
}));

const { editor } = await import('./state.svelte');
const { detachSelection, linkSelection, reattachSelection, unlinkSelection } = await import('./ops');
const { getHistory, revertTo, setTrackLocked } = await import('./api');

const head = async () => (await getHistory()).find((r) => r.current)!.seq;
const labels = async () => (await getHistory()).map((r) => r.label);
const clipIds = (track: number) => editor.timeline.tracks[track].clips.map((c) => c.id);

beforeEach(async () => {
	await setTrackLocked('a1', false);
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
	shown.length = 0;
});

describe('reattach and detach', () => {
	test('reattach: the sound clip goes, the picture plays its own again — and Undo takes it back', async () => {
		editor.selectClip('c1'); // the pair
		await reattachSelection();
		expect(clipIds(1)).toEqual([]);
		expect(editor.timeline.tracks[0].clips[0].source_audio).toBeUndefined();
		expect(shown.map((s) => [s.kind, s.text])).toEqual([['note', 'Audio reattached on 1 clip']]);
		shown[0].undo!();
		await new Promise((r) => setTimeout(r, 20));
		expect(clipIds(1)).toEqual(['c3']);
	});

	test('detach: each picture still playing its sound gets a linked clip, selected with it; Undo takes them all back', async () => {
		await reattachSelectionOf('c1');
		shown.length = 0;
		const before = await head();
		editor.selectClip('c1');
		await detachSelection();
		expect(await head()).toBe(before + 1);
		expect((await labels()).at(-1)).toBe('Detach audio');
		const pic = editor.timeline.tracks[0].clips[0];
		expect(pic.source_audio).toBe(false);
		expect(editor.timeline.tracks[1].clips).toHaveLength(1);
		// picture and its new sound are the selection, the picture primary
		expect(editor.selectedClipIds).toHaveLength(2);
		expect(editor.selectedClipId).toBe('c1');
		expect(shown.map((s) => s.text)).toEqual(['Audio detached from 1 clip']);
	});

	test('nothing to detach says why and changes nothing', async () => {
		editor.selectClip('c1'); // already detached
		const before = await head();
		await detachSelection();
		expect(await head()).toBe(before);
		expect(shown).toEqual([{ kind: 'info', text: 'Its sound is already detached', undo: undefined }]);
		shown.length = 0;
		editor.clearSelection();
		await detachSelection();
		expect(shown[0].text).toBe('Select a picture clip to detach its sound');
	});

	test('a locked picture track is refused up front', async () => {
		await reattachSelectionOf('c1');
		await setTrackLocked('v1', true);
		await editor.load();
		shown.length = 0;
		editor.selectClip('c1');
		await detachSelection();
		expect(shown.map((s) => [s.kind, s.text])).toEqual([['info', 'Track V1 is locked']]);
		await setTrackLocked('v1', false);
	});

	test('reattach with nothing detached says so', async () => {
		await reattachSelectionOf('c1');
		shown.length = 0;
		editor.selectClip('c1');
		await reattachSelection();
		expect(shown[0]).toMatchObject({ kind: 'info', text: 'No selected clip has detached sound' });
	});
});

/** Reattach c1's sound through the editor, leaving no selection and no toast. */
async function reattachSelectionOf(id: string) {
	await editor.reattachAudio(id);
	editor.clearSelection();
}

describe('link and unlink', () => {
	test('unlink the pair, then link them again: one revision each, the notices count', async () => {
		editor.selectClip('c1');
		const before = await head();
		await unlinkSelection();
		expect((await labels()).at(-1)).toBe('Unlink clips');
		expect(shown.at(-1)?.text).toBe('Unlinked 2 clips');
		editor.selectClips(['c1', 'c3']);
		await linkSelection();
		expect((await labels()).at(-1)).toBe('Link 2 clips');
		expect(shown.at(-1)?.text).toBe('Linked 2 clips');
		expect(await head()).toBe(before + 2);
	});

	test('a selection that cannot be linked says why, in the backend’s words, without asking it', async () => {
		editor.selectClips(['c1', 'c2']); // both on V1
		const before = await head();
		await linkSelection();
		expect(shown[0]).toMatchObject({ kind: 'info' });
		expect(shown[0].text).toContain('Two of the clips are on track V1');
		editor.selectClips(['c1', 'c3']); // already one group
		shown.length = 0;
		await linkSelection();
		expect(shown[0].text).toBe('Those clips are already linked');
		expect(await head()).toBe(before);
	});

	test('unlink with nothing linked selected says so', async () => {
		editor.selectClips(['c2']);
		await unlinkSelection();
		expect(shown[0].text).toBe('This clip is not linked');
	});
});
