import { describe, expect, mock, test } from 'bun:test';
import './test-runes';
import { readFileSync } from 'node:fs';
import { posix, uiSources } from './test-sources';

const revealed: string[] = [];
const real = { ...(await import('./api')) };
mock.module('./api', () => ({ ...real, revealPath: async (path: string) => void revealed.push(path) }));
const { folderOption, SHOW_IN_FOLDER } = await import('./folder-option');

describe('a notice about a file', () => {
	test('offers the folder as a button for the toast and as the path for the log', async () => {
		const o = folderOption('/home/me/Videos/cut.mp4');
		expect(o.reveal).toBe('/home/me/Videos/cut.mp4');
		expect(o.action.label).toBe(SHOW_IN_FOLDER);
		o.action.onClick();
		await Promise.resolve();
		expect(revealed).toEqual(['/home/me/Videos/cut.mp4']);
	});

	test('a refused reveal is not an unhandled rejection', async () => {
		mock.module('./api', () => ({ ...real, revealPath: async () => Promise.reject(new Error('no such folder')) }));
		const { folderOption: again } = (await import('./folder-option?refused' as string)) as typeof import('./folder-option');
		expect(() => again('/gone/cut.mp4').action.onClick()).not.toThrow();
		await Promise.resolve();
	});
});

describe('every toast about a file written to disk keeps where it is', () => {
	test('no source draws a Show in folder toast button of its own: they all come from `folderOption`, which carries the path for the log', () => {
		const offenders = uiSources()
			.filter((f) => posix(f) !== 'lib/folder-option.ts')
			.filter((f) => /action:\s*\{\s*label:\s*'Show in folder'/.test(readFileSync(f, 'utf8')))
			.map(posix);
		expect(offenders).toEqual([]);
	});

	test('the file results are the cover frame, the thumbnail, the project, a side file, the transcript and an export', () => {
		const uses = (file: string) => readFileSync(new URL(`./${file}`, import.meta.url), 'utf8').match(/folderOption\(/g)?.length ?? 0;
		expect(uses('file-actions.ts')).toBeGreaterThanOrEqual(4);
		expect(uses('ops.ts')).toBeGreaterThanOrEqual(1);
		expect(uses('components/editor/ExportDialog.svelte')).toBe(1);
	});
});
