import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { CaptionFilePick } from './api';
import type { CaptionImportResult } from './types';

// `importCaptionFile` is the whole of an import as the Titles controls and the
// lane menu run it: pick a file, ask before replacing captions, import, say what
// happened. It runs here over the browser harness's backend (the same cut as
// `state-selection.test.ts`: V1 `c1 [0, 12.5)` `c2 [12.5, 20.5)` over A1 `c3
// [0, 12.5)`, c1's detached sound, `c2` showing broll from its start), with the picker, the question and
// the toasts standing in for what only a window can do.

const real = { ...(await import('./api')) };

const asked: { message: string; title?: string }[] = [];
const shown: { kind: string; text: string }[] = [];

let pick: () => Promise<CaptionFilePick | null>;
let answer = true;
/** The question as the user sees it: recorded, then answered with `answer`. */
const asking = async (message: string, title?: string) => {
	asked.push({ message, title });
	return answer;
};
let confirm: (message: string, title?: string) => Promise<boolean> = asking;
/** What `importCaptions` (by path — the desktop app's) does; the harness has no disk. */
let byPath: typeof real.importCaptions = real.importCaptions;

// Installed while the module loads, not in `beforeAll`: the imports below are
// evaluated first, and `title-actions` must see the stand-ins.
mock.module('./api', () => ({
	...real,
	importCaptions: (...args: Parameters<typeof real.importCaptions>) => byPath(...args),
	pickCaptionFile: () => pick(),
	confirmAction: (message: string, title?: string) => confirm(message, title)
}));
const note = (kind: string) => (text: string) => void shown.push({ kind, text });
// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	})
}));

afterAll(() => {
	mock.module('./api', () => real);
});

const { editor } = await import('./state.svelte');
const { ui } = await import('./editor-ui.svelte');
const { importCaptionFile, makeCaptions } = await import('./title-actions');
const { revertTo } = real;

const text = (srt: string, name = 'subs.srt'): CaptionFilePick => ({ kind: 'text', text: srt, name, format: 'srt' });
const CLEAN = '1\n00:00:01,000 --> 00:00:03,000\nHello there\n\n2\n00:00:04,000 --> 00:00:06,000\nGeneral Kenobi\n';
const RAGGED = `${CLEAN}\nnot a cue\n\n3\n00:09:00,000 --> 00:09:02,000\nWay past the end\n`;
const captions = () => editor.overlays.filter((o) => o.generated);

beforeEach(async () => {
	await revertTo(0);
	await editor.load();
	editor.clearSelection();
	ui.captionStyle = 'lines';
	ui.captionImportKeepLines = null;
	ui.captionImportOffset = null;
	answer = true;
	confirm = asking;
	byPath = real.importCaptions;
	asked.length = 0;
	shown.length = 0;
	pick = async () => text(CLEAN);
});

describe('importCaptionFile', () => {
	test('imports into an empty cut without asking, and says so', async () => {
		expect(await importCaptionFile()).toBe(true);
		expect(asked).toEqual([]);
		expect(captions().map((o) => [o.text, o.start])).toEqual([
			['Hello there', 1],
			['General Kenobi', 4]
		]);
		expect(shown).toEqual([{ kind: 'success', text: 'Imported 2 captions from 2 of 2 cues' }]);
	});

	test('a cue that did not land makes the notice a warning', async () => {
		pick = async () => text(RAGGED);
		expect(await importCaptionFile()).toBe(true);
		expect(shown).toHaveLength(1);
		expect(shown[0].kind).toBe('warning');
		expect(shown[0].text).toBe('Imported 2 captions from 2 of 3 cues (1 outside the cut, 1 unreadable)');
	});

	test('asks before replacing captions, naming the count and the file', async () => {
		await importCaptionFile();
		shown.length = 0;
		pick = async () => text(CLEAN.replace('Hello there', 'Hello again'), 'second.srt');
		expect(await importCaptionFile()).toBe(true);
		expect(asked).toEqual([
			{
				message: 'Replace the 2 captions already on the cut with those in second.srt? Titles you added by hand are kept.',
				title: 'Import captions'
			}
		]);
		expect(captions().map((o) => o.text)).toEqual(['Hello again', 'General Kenobi']);
		expect(shown[0].text).toBe('Imported 2 captions from 2 of 2 cues · replaced 2');
	});

	test('declining leaves the captions as they were, and says nothing', async () => {
		await importCaptionFile();
		shown.length = 0;
		answer = false;
		pick = async () => text(CLEAN.replace('Hello there', 'Hello again'));
		expect(await importCaptionFile()).toBe(false);
		expect(asked).toHaveLength(1);
		expect(captions().map((o) => o.text)).toEqual(['Hello there', 'General Kenobi']);
		expect(shown).toEqual([]);
	});

	test('a title typed by hand is neither counted nor replaced', async () => {
		await editor.addTitle('Mine', 0, 3);
		expect(await importCaptionFile()).toBe(true);
		expect(asked).toEqual([]);
		expect(editor.overlays.filter((o) => !o.generated).map((o) => o.text)).toEqual(['Mine']);
	});

	test('a cancelled picker is silent', async () => {
		pick = async () => null;
		expect(await importCaptionFile()).toBe(false);
		expect(asked).toEqual([]);
		expect(shown).toEqual([]);
		expect(captions()).toEqual([]);
	});

	test('a picker that fails is an error notice', async () => {
		pick = async () => {
			throw new Error('subtitle file is larger than 5 MiB — not a subtitle file');
		};
		expect(await importCaptionFile()).toBe(false);
		expect(shown).toEqual([{ kind: 'error', text: 'subtitle file is larger than 5 MiB — not a subtitle file' }]);
	});

	test('a file with no captions in it is an error notice and changes nothing', async () => {
		pick = async () => text('nothing here\nat all\n');
		expect(await importCaptionFile()).toBe(false);
		expect(shown).toHaveLength(1);
		expect(shown[0].kind).toBe('error');
		expect(shown[0].text).toContain('no captions found');
		expect(captions()).toEqual([]);
	});

	test('timed to a source clip, the file follows the footage through the cut', async () => {
		// `c2` shows broll from its start at 12.5 s, so source second 1 is timeline 13.5.
		pick = async () => text('1\n00:00:01,000 --> 00:00:03,000\nEstablishing shot\n');
		const broll = editor.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === 'c2')!.asset_id;
		expect(await importCaptionFile({ base: 'source', assetId: broll })).toBe(true);
		expect(captions().map((o) => [o.text, o.start])).toEqual([['Establishing shot', 13.5]]);
	});

	test('the caption look is the one the chips are set to', async () => {
		ui.captionStyle = 'word_punch';
		expect(await importCaptionFile()).toBe(true);
		expect(captions().map((o) => o.text)).toEqual(['Hello', 'there', 'General', 'Kenobi']);
	});

	// A cue of eight words: the generator would split it into two lines of four.
	const LONG = '1\n00:00:01,000 --> 00:00:09,000\none two three four five six seven eight\n';

	test("keeps the file's lines on an unframed cut until told otherwise", async () => {
		pick = async () => text(LONG);
		await importCaptionFile();
		expect(captions().map((o) => o.text)).toEqual(['one two three four five six seven eight']);
		// Unticked, the generator's short lines come back.
		ui.captionImportKeepLines = false;
		await importCaptionFile();
		expect(captions().map((o) => o.text)).toEqual(['one two three four', 'five six seven eight']);
	});

	test('a tall frame starts with the file re-split, and ticking the box keeps its lines', async () => {
		await editor.setDeliveryFormat({ width: 1080, height: 1920, fit: 'cover' });
		pick = async () => text(LONG);
		await importCaptionFile();
		expect(captions().map((o) => o.text)).toEqual(['one two three four', 'five six seven eight']);
		ui.captionImportKeepLines = true;
		await importCaptionFile();
		expect(captions().map((o) => o.text)).toEqual(['one two three four five six seven eight']);
	});

	test('word punch never keeps lines, whatever the box says', async () => {
		ui.captionStyle = 'word_punch';
		ui.captionImportKeepLines = true;
		pick = async () => text(LONG);
		await importCaptionFile();
		expect(captions().map((o) => o.text)).toEqual(['one', 'two', 'three', 'four', 'five', 'six', 'seven', 'eight']);
	});

	test('the offset shifts the file onto the cut', async () => {
		// A broadcast file whose clock starts at one hour.
		pick = async () => text('1\n01:00:01,000 --> 01:00:03,000\nHello there\n');
		expect(await importCaptionFile()).toBe(false);
		expect(shown[0].kind).toBe('error');
		shown.length = 0;
		ui.captionImportOffset = -3600;
		expect(await importCaptionFile()).toBe(true);
		expect(captions().map((o) => [o.text, o.start, o.end])).toEqual([['Hello there', 1, 3]]);
	});

	test('a cue too short to read is its own line in the notice', async () => {
		pick = async () => text(`${CLEAN}\n3\n00:00:07,000 --> 00:00:07,050\nA blink\n`);
		expect(await importCaptionFile()).toBe(true);
		expect(shown).toEqual([{ kind: 'warning', text: 'Imported 2 captions from 2 of 3 cues (1 too short)' }]);
	});

	test('leaves the selection where it was', async () => {
		editor.selectClips(['c1']);
		await importCaptionFile();
		expect(editor.selectedClipIds).toEqual(['c1']);
		expect(editor.selectedOverlayId).toBeNull();
	});

	test('a file picked by path goes to the backend as a path, with the timing and the look', async () => {
		const calls: [string, unknown][] = [];
		byPath = async (path, req) => {
			calls.push([path, req]);
			return real.importCaptionsText(CLEAN, req);
		};
		ui.captionStyle = 'word_punch';
		pick = async () => ({ kind: 'path', path: '/subs/movie.srt', name: 'movie.srt' });
		expect(await importCaptionFile({ base: 'source', assetId: 'asset-1' })).toBe(false);
		// `asset-1` is not in the cut, so the harness backend refuses; what matters is what it was asked.
		expect(calls).toEqual([['/subs/movie.srt', { base: 'source', assetId: 'asset-1', options: { style: 'word_punch' } }]]);
		expect(shown.map((s) => s.kind)).toEqual(['error']);

		calls.length = 0;
		shown.length = 0;
		expect(await importCaptionFile()).toBe(true);
		expect(calls).toEqual([['/subs/movie.srt', { base: 'timeline', options: { style: 'word_punch' } }]]);
		expect(shown.map((s) => s.kind)).toEqual(['success']);
	});

	test('a second import while one is waiting on its question is dropped', async () => {
		await importCaptionFile();
		shown.length = 0;
		let release: (v: boolean) => void = () => {};
		let onAsked: () => void = () => {};
		const waiting = new Promise<void>((r) => (onAsked = r));
		confirm = () => {
			onAsked();
			return new Promise<boolean>((r) => (release = r));
		};
		const first = importCaptionFile();
		await waiting;
		expect(await importCaptionFile()).toBe(false);
		release(true);
		expect(await first).toBe(true);
		expect(shown.map((s) => s.kind)).toEqual(['success']);
	});
});

describe('captioning from transcripts', () => {
	const generated = () => editor.overlays.filter((o) => o.generated).map((o) => o.text);

	test('asks first when a set is already there — an imported one included — and names the count', async () => {
		await importCaptionFile();
		expect(asked).toEqual([]);
		await makeCaptions();
		expect(asked).toEqual([
			{ message: 'Replace the 2 captions already on the cut? Titles you added by hand are kept.', title: 'Caption the cut' }
		]);
		// Agreed: the imported set is gone, replaced by the transcript's.
		expect(generated()).not.toContain('Hello there');
	});

	test('declining leaves the set alone', async () => {
		await importCaptionFile();
		answer = false;
		await makeCaptions();
		expect(asked).toHaveLength(1);
		expect(generated()).toEqual(['Hello there', 'General Kenobi']);
	});

	test('does not ask when there is nothing to replace, or only a title typed by hand', async () => {
		await editor.addTitle('Mine', 0, 3);
		await makeCaptions();
		expect(asked).toEqual([]);
		expect(generated().length).toBeGreaterThan(0);
	});

	test('counts the live cut, not a proposal on screen', async () => {
		await importCaptionFile();
		asked.length = 0;
		// The Titles list would show the proposal's captions; the question is about
		// the cut that is about to be written over.
		const live = editor.liveTimeline;
		expect(live.overlays?.filter((o) => o.generated)).toHaveLength(2);
		await makeCaptions();
		expect(asked[0].message).toContain('Replace the 2 captions');
	});
});
