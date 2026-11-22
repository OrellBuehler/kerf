import { afterAll, afterEach, beforeEach, describe, expect, test } from 'bun:test';
import './test-runes';
import { getHistory, revertTo, setRippleMode, unlinkClips } from './api';

// `editor.applyEdits` over the browser harness's cut — V1 `c1 [0, 12.5)` `c2 [12.5, 20.5)` over A1 `c3 [0, 12.5)`:
// several edits as one revision, the timeline and the history on screen following it, a refusal reported as every
// other edit's is and a previewed proposal held off before anything is asked.

const { editor } = await import('./state.svelte');
const { EditRefused, PreviewReadOnly } = await import('./state.svelte');
const revision = async () => (await getHistory()).find((r) => r.current)!.seq;
const clip = (id: string) => editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id)!;

beforeEach(async () => {
	await setRippleMode(false);
	await revertTo(0);
	await unlinkClips(['c1']);
	await editor.load();
	editor.clearSelection();
});
afterEach(() => {
	editor.previewingStaged = false;
});
// The harness and the editor are one process-wide state, shared with every test file that runs after this one.
afterAll(async () => {
	editor.previewingStaged = false;
	await setRippleMode(false);
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
});

describe('editor.applyEdits', () => {
	test('is one revision, shown at once, and answers each op', async () => {
		const before = await revision();
		const results = await editor.applyEdits(
			[
				{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } },
				{ tool: 'cut_clip_range', args: { clip_id: 'c1', from: 2, to: 3, link: false } },
				{ tool: 'set_color', args: { clip_id: 'c2', brightness: 0.1 } }
			],
			'Tidy up'
		);
		expect(await revision()).toBe(before + 1);
		expect(results).toHaveLength(3);
		expect(results[0]).toMatchObject({ id: 'c2', volume: 0.5 });
		expect(clip('c2')).toMatchObject({ volume: 0.5, color: { brightness: 0.1 } });
		expect(editor.timeline.tracks[0].clips.length).toBeGreaterThan(2);
		expect(editor.history.find((r) => r.current)?.label).toBe('Tidy up');
		expect(editor.history.filter((r) => r.label === 'Tidy up')).toHaveLength(1);
	});

	test('a failing op rejects as an EditRefused naming it and leaves the cut and the history as they were', async () => {
		const before = await revision();
		const shown = structuredClone(editor.timeline);
		const failed = editor
			.applyEdits([
				{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } },
				{ tool: 'trim', args: { clip_id: 'c2', source_in: -1 } }
			])
			.catch((e) => e);
		const error = await failed;
		expect(error).toBeInstanceOf(EditRefused);
		expect((error as Error).message).toContain('op 1 (trim)');
		expect(editor.timeline).toEqual(shown);
		expect(clip('c2').volume).toBe(1);
		expect(await revision()).toBe(before);
	});

	test('is held off while a proposal is previewed, before the backend is asked', async () => {
		const before = await revision();
		editor.previewingStaged = true;
		await expect(editor.applyEdits([{ tool: 'set_volume', args: { clip_id: 'c2', volume: 0.5 } }])).rejects.toBeInstanceOf(PreviewReadOnly);
		editor.previewingStaged = false;
		expect(await revision()).toBe(before);
		expect(clip('c2').volume).toBe(1);
	});
});
