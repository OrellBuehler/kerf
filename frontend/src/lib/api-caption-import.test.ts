import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import type { CaptionImportResult } from './types';

// The desktop half of importing captions: which file dialog is opened, and what
// `import_captions` / `import_captions_text` are handed. The Rust commands take
// `path` / `text`, `format`, `base`, `asset_id` and `options` (Tauri turns the
// camelCase keys into those), and `null` for what was not chosen — so a rename on
// either side shows up here rather than as a refused import in the app.

const calls: { cmd: string; args: unknown }[] = [];
const opened: unknown[] = [];
let dialogAnswer: string | string[] | null = null;
let reply: unknown = null;

// Put back for the other files once this one is done.
const realCore = { ...(await import('@tauri-apps/api/core')) };
const realDialog = { ...(await import('@tauri-apps/plugin-dialog')) };

mock.module('@tauri-apps/api/core', () => ({
	invoke: async (cmd: string, args: unknown) => {
		calls.push({ cmd, args });
		return reply;
	}
}));
mock.module('@tauri-apps/plugin-dialog', () => ({
	open: async (opts: unknown) => {
		opened.push(opts);
		return dialogAnswer;
	},
	ask: async () => true,
	save: async () => null
}));

const { importCaptions, importCaptionsText, pickCaptionFile } = await import('./api');
const g = globalThis as unknown as { window?: unknown };

const result = {
	timeline: { tracks: [], overlays: [] },
	summary: { format: 'srt', cues: 1, placed: 1, captions: 1, skipped_lines: 0, dropped_outside: 0, dropped_overlap: 0, replaced: 0 }
} as CaptionImportResult;

beforeEach(() => {
	calls.length = 0;
	opened.length = 0;
	dialogAnswer = null;
	reply = result;
	g.window = { __TAURI_INTERNALS__: {} };
});
afterEach(() => {
	delete g.window;
});
afterAll(() => {
	delete g.window;
	mock.module('@tauri-apps/api/core', () => realCore);
	mock.module('@tauri-apps/plugin-dialog', () => realDialog);
});

describe('picking a subtitle file (desktop)', () => {
	test('opens the native dialog on subtitle extensions and answers with the path', async () => {
		dialogAnswer = '/home/me/subs/movie.srt';
		expect(await pickCaptionFile()).toEqual({ kind: 'path', path: '/home/me/subs/movie.srt', name: 'movie.srt' });
		expect(opened).toEqual([{ multiple: false, filters: [{ name: 'Subtitles', extensions: ['srt', 'ass', 'ssa'] }] }]);
	});

	test('names a Windows path by its file', async () => {
		dialogAnswer = 'C:\\Users\\me\\subs\\movie.ass';
		expect(await pickCaptionFile()).toEqual({ kind: 'path', path: 'C:\\Users\\me\\subs\\movie.ass', name: 'movie.ass' });
	});

	test('a cancelled dialog is null, not an error', async () => {
		dialogAnswer = null;
		expect(await pickCaptionFile()).toBeNull();
	});
});

describe('import_captions', () => {
	test('is handed the path and, unset, nulls', async () => {
		expect(await importCaptions('/subs/movie.srt')).toEqual(result);
		expect(calls).toEqual([
			{ cmd: 'import_captions', args: { path: '/subs/movie.srt', base: null, assetId: null, options: null } }
		]);
	});

	test('carries the timing and the look', async () => {
		await importCaptions('/subs/movie.srt', { base: 'source', assetId: 'asset-1', options: { style: 'word_punch' } });
		expect(calls[0].args).toEqual({
			path: '/subs/movie.srt',
			base: 'source',
			assetId: 'asset-1',
			options: { style: 'word_punch' }
		});
	});
});

describe('import_captions_text', () => {
	test('is handed the text, its format and the same options', async () => {
		await importCaptionsText('1\n00:00:01,000 --> 00:00:02,000\nHi\n', {
			format: 'srt',
			base: 'timeline',
			options: { style: 'lines' }
		});
		expect(calls).toEqual([
			{
				cmd: 'import_captions_text',
				args: {
					text: '1\n00:00:01,000 --> 00:00:02,000\nHi\n',
					format: 'srt',
					base: 'timeline',
					assetId: null,
					options: { style: 'lines' }
				}
			}
		]);
	});

	test('leaves the format to the backend when none was given', async () => {
		await importCaptionsText('text');
		expect((calls[0].args as { format: unknown }).format).toBeNull();
	});
});

describe('in a browser', () => {
	test('a path cannot be read, and says what to use instead', async () => {
		delete g.window;
		await expect(importCaptions('/subs/movie.srt')).rejects.toThrow('importCaptionsText');
		expect(calls).toEqual([]);
	});
});
