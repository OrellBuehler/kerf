import { describe, expect, mock, test } from 'bun:test';
import './test-runes';
import { applyPreset } from './export-presets';
import { frameKey, restoreDraft, type ExportDraft } from './export-draft';

// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
const note = () => {};
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note, { success: note, info: note, warning: note, error: note }),
	notifications: {}
}));
const { ui } = await import('./editor-ui.svelte');

const draft = (over: Partial<ExportDraft> = {}): ExportDraft => ({
	opts: { ...applyPreset('youtube_4k'), crf: 18 },
	preset: 'custom',
	customRes: true,
	frame: frameKey({ width: 1920, height: 1080, fit: 'contain' }),
	...over
});

describe('frameKey', () => {
	test('is the project frame, fit included, or null without one', () => {
		expect(frameKey({ width: 1080, height: 1920, fit: 'cover' })).toBe('1080x1920/cover');
		expect(frameKey(null)).toBeNull();
		expect(frameKey(undefined)).toBeNull();
	});

	test("differs when only the fit differs, since the dialog opens with the cut's fit", () => {
		expect(frameKey({ width: 1920, height: 1080, fit: 'cover' })).not.toBe(frameKey({ width: 1920, height: 1080, fit: 'contain' }));
	});
});

describe('restoreDraft', () => {
	test('reopening gives back what was configured', () => {
		const d = draft();
		const back = restoreDraft(d, d.frame)!;
		expect(back.opts).toEqual(d.opts);
		expect(back.preset).toBe('custom');
		expect(back.customRes).toBe(true);
	});

	test('the copy is its own: editing the open dialog does not rewrite the remembered one', () => {
		const d = draft();
		const back = restoreDraft(d, d.frame)!;
		back.opts.crf = 40;
		back.opts.resolution = [2, 2];
		expect(d.opts.crf).toBe(18);
		expect(d.opts.resolution).toEqual(applyPreset('youtube_4k').resolution);
	});

	test('nothing remembered opens as ever', () => {
		expect(restoreDraft(null, null)).toBeNull();
	});

	test('a project re-cut for another frame since is not opened on the old one', () => {
		const d = draft();
		expect(restoreDraft(d, frameKey({ width: 1080, height: 1920, fit: 'cover' }))).toBeNull();
		expect(restoreDraft(d, null)).toBeNull();
	});

	test('a project with no frame keeps its draft', () => {
		const d = draft({ frame: null });
		expect(restoreDraft(d, null)?.opts).toEqual(d.opts);
	});
});

describe('a different project', () => {
	test('forgets the export options of the last one', () => {
		ui.exportDraft = draft();
		ui.projectReplaced();
		expect(ui.exportDraft).toBeNull();
	});
});
