import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { StagedEdit, Timeline } from './types';

// The proposal preview as a state machine: the cut on screen is the proposal's while it is previewed, the
// live one is parked, the timeline is read-only meanwhile, and an apply that is refused leaves the preview
// exactly where it was. The backend is faked; what is under test is `EditorState`.

const real = { ...(await import('./api')) };

const live: Timeline = { tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: [{ id: 'c1', asset_id: 'a', source_in: 0, source_out: 10, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 }] }] };
const proposed: Timeline = { tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: [{ id: 'c1', asset_id: 'a', source_in: 0, source_out: 6, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 }] }] };

const staged = (n = 1): StagedEdit => ({
	base_seq: 0,
	task_id: null,
	note: null,
	edits: [],
	created_at: '',
	updated_at: '',
	stale: false,
	diff: { entries: Array.from({ length: n }, () => ({ kind: 'clip_retrimmed' as const, summary: 'x', clip_id: 'c1', at: 0 })), duration_before: 10, duration_after: 6, clips_before: 1, clips_after: 1 }
});

const calls: string[] = [];
let stagedNow: StagedEdit | null = staged();
let applyFails: string | null = null;
let stagedReads: (() => Promise<StagedEdit | null>)[] = [];

const backend = () => ({
	...real,
	getTimeline: async () => structuredClone(live),
	getStagedEdit: async () => (stagedReads.length > 0 ? stagedReads.shift()!() : stagedNow && structuredClone(stagedNow)),
	getStagedTimeline: async () => (stagedNow ? structuredClone(proposed) : null),
	applyStagedEdit: async () => {
		calls.push('apply');
		if (applyFails) throw new Error(applyFails);
		stagedNow = null;
		return structuredClone(proposed);
	},
	discardStagedEdit: async () => {
		calls.push('discard');
		stagedNow = null;
		return structuredClone(live);
	},
	splitClip: async () => {
		calls.push('split');
		return structuredClone(live);
	},
	getHistory: async () => [],
	onExportProgress: async () => () => {},
	exportTimeline: (..._a: unknown[]) => exporting
});
let exporting: Promise<string> = Promise.resolve('/out.mp4');

mock.module('./api', () => backend());
afterAll(() => {
	mock.module('./api', () => real);
});

const { editor, EditRefused, PREVIEW_READ_ONLY } = await import('./state.svelte');

beforeEach(async () => {
	calls.length = 0;
	stagedNow = staged();
	applyFails = null;
	stagedReads = [];
	exporting = Promise.resolve('/out.mp4');
	await editor.load();
	editor.error = null;
});

describe('previewing a proposal', () => {
	test('shows the proposed cut and parks the live one', async () => {
		await editor.previewStaged();
		expect(editor.previewingStaged).toBe(true);
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(6);
		expect(editor.liveTimeline.tracks[0].clips[0].source_out).toBe(10);
		expect(editor.editable).toBe(false);
		expect(editor.locked).toBe(true);
	});

	test('does nothing when the proposal was settled while it was being fetched', async () => {
		stagedNow = null;
		editor.staged = null;
		await editor.previewStaged();
		expect(editor.previewingStaged).toBe(false);
	});

	test('exiting puts the parked live cut back', async () => {
		await editor.previewStaged();
		await editor.exitStagedPreview();
		expect(editor.previewingStaged).toBe(false);
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(10);
		expect(editor.editable).toBe(true);
	});
});

describe('the timeline is read-only while the proposal is on screen', () => {
	test('an edit is refused before the backend is asked, and the preview stays', async () => {
		await editor.previewStaged();
		await expect(editor.split('c1', 3)).rejects.toBeInstanceOf(EditRefused);
		await expect(editor.split('c1', 3)).rejects.toThrow(PREVIEW_READ_ONLY);
		expect(calls).toEqual([]);
		expect(editor.previewingStaged).toBe(true);
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(6);
	});

	test('so is deleting the selection, which reads and selects before it edits', async () => {
		await editor.previewStaged();
		editor.selectClips(['c1']);
		await expect(editor.removeSelected(false)).rejects.toThrow(PREVIEW_READ_ONLY);
		expect(editor.selectedClipIds).toEqual(['c1']);
	});

	test('and after exiting, the same edit goes through', async () => {
		await editor.previewStaged();
		await editor.exitStagedPreview();
		await editor.split('c1', 3);
		expect(calls).toEqual(['split']);
	});
});

describe('apply and discard', () => {
	test('an apply the backend refuses leaves the preview — and the parked cut — as they were', async () => {
		await editor.previewStaged();
		applyFails = 'the timeline changed since these edits were staged';
		await expect(editor.applyStaged(false)).rejects.toThrow('the timeline changed since these edits were staged');
		expect(editor.previewingStaged).toBe(true);
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(6);
		expect(editor.liveTimeline.tracks[0].clips[0].source_out).toBe(10);
		expect(editor.staged).not.toBeNull();
		// …and the page does not also put it in the banner: it is the caller's toast.
		expect(editor.error).toBeNull();
	});

	test('a successful apply ends the preview and shows what landed', async () => {
		await editor.previewStaged();
		await editor.applyStaged(false);
		expect(editor.previewingStaged).toBe(false);
		expect(editor.staged).toBeNull();
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(6);
	});

	test('a discard ends the preview on the live cut, even though the timeline is read-only', async () => {
		await editor.previewStaged();
		await editor.discardStaged();
		expect(calls).toEqual(['discard']);
		expect(editor.previewingStaged).toBe(false);
		expect(editor.staged).toBeNull();
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(10);
	});

	test('a refusal is an EditRefused carrying what the backend said', async () => {
		await editor.previewStaged();
		applyFails = 'nope';
		const err = await editor.applyStaged(false).catch((e) => e);
		expect(err).toBeInstanceOf(EditRefused);
		expect(err.message).toBe('nope');
	});
});

describe('reading the proposal', () => {
	test('a slower, older read does not overwrite a newer one', async () => {
		let release!: (v: StagedEdit | null) => void;
		const slow = new Promise<StagedEdit | null>((r) => (release = r));
		stagedReads = [() => slow, async () => staged(5)];
		const older = editor.refreshStaged();
		await editor.refreshStaged();
		expect(editor.staged?.diff.entries.length).toBe(5);
		release(staged(1));
		await older;
		expect(editor.staged?.diff.entries.length).toBe(5);
	});

	test('a read that failed says nothing about the proposal', async () => {
		await editor.refreshStaged();
		expect(editor.staged).not.toBeNull();
		stagedReads = [() => Promise.reject(new Error('busy'))];
		await editor.refreshStaged();
		expect(editor.staged).not.toBeNull();
	});

	test('the proposal going away ends a preview that was on', async () => {
		await editor.previewStaged();
		stagedNow = null;
		await editor.refreshStaged();
		expect(editor.staged).toBeNull();
		expect(editor.previewingStaged).toBe(false);
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(10);
	});

	test('a preview left while the proposal was being re-read is not pulled back onto it', async () => {
		await editor.previewStaged();
		let release!: (v: StagedEdit | null) => void;
		stagedReads = [() => new Promise<StagedEdit | null>((r) => (release = r))];
		const reading = editor.refreshStaged();
		await editor.exitStagedPreview();
		release(staged(2));
		await reading;
		expect(editor.previewingStaged).toBe(false);
		expect(editor.timeline.tracks[0].clips[0].source_out).toBe(10);
	});
});

describe('an export does not freeze the editor', () => {
	test('exportRun is raised, busy is not', async () => {
		let finish!: (p: string) => void;
		exporting = new Promise<string>((r) => (finish = r));
		const run = editor.export('/out.mp4', {} as never);
		await Promise.resolve();
		await Promise.resolve();
		expect(editor.exportRun).not.toBeNull();
		expect(editor.busy).toBe(false);
		finish('/out.mp4');
		await run;
		expect(editor.exportRun).toBeNull();
	});
});
