import { describe, expect, test } from 'bun:test';
import { firstRunAction, firstRunSteps } from './first-run';

const states = (assets: number, clips: number) => firstRunSteps({ assets, clips }).map((s) => s.state);

describe('the first-run steps', () => {
	test('an empty project starts at the import', () => {
		expect(states(0, 0)).toEqual(['current', 'todo', 'todo']);
	});

	test('with media in the library the cut is next', () => {
		expect(states(3, 0)).toEqual(['done', 'current', 'todo']);
	});

	test('with a clip on the timeline the export is what is left', () => {
		expect(states(3, 2)).toEqual(['done', 'done', 'current']);
	});

	test('it is three steps in the order import, cut, export', () => {
		expect(firstRunSteps({ assets: 0, clips: 0 }).map((s) => s.id)).toEqual(['import', 'cut', 'export']);
	});

	test('a step names its key as the user’s keymap prints it, and says nothing when there is none', () => {
		const [imp, , exp] = firstRunSteps({ assets: 0, clips: 0, importKeys: 'Ctrl+I', exportKeys: 'Ctrl+E' });
		expect(imp.body).toBe('Drop files on the window, or choose them (Ctrl+I).');
		expect(exp.body).toBe('Render the cut to a file for the platform you are making it for (Ctrl+E).');
		const [bare] = firstRunSteps({ assets: 0, clips: 0 });
		expect(bare.body).toBe('Drop files on the window, or choose them.');
	});
});

describe('the empty state’s one button', () => {
	const assets = [
		{ id: 'a', name: 'interview.mp4' },
		{ id: 'b', name: 'broll.mp4' }
	];

	test('an empty library asks for an import', () => {
		expect(firstRunAction({ assets: [], selectedId: null, libraryOpen: true })).toEqual({ kind: 'import', label: 'Import media…' });
		expect(firstRunAction({ assets: [], selectedId: null, libraryOpen: false }).kind).toBe('import');
	});

	test('media in the library and the library on screen: put the selected media on the timeline — not "Open Library"', () => {
		expect(firstRunAction({ assets, selectedId: 'b', libraryOpen: true })).toEqual({ kind: 'add', label: 'Add selected to timeline', assetId: 'b' });
	});

	test('with nothing (or something gone) selected it is the first asset', () => {
		expect(firstRunAction({ assets, selectedId: null, libraryOpen: true })).toMatchObject({ kind: 'add', assetId: 'a' });
		expect(firstRunAction({ assets, selectedId: 'gone', libraryOpen: true })).toMatchObject({ kind: 'add', assetId: 'a' });
	});

	test('only a library that is not on screen offers to open it', () => {
		expect(firstRunAction({ assets, selectedId: 'a', libraryOpen: false })).toEqual({ kind: 'open-library', label: 'Open Library' });
	});
});
