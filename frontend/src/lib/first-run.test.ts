import { describe, expect, test } from 'bun:test';
import { firstClipNotice, firstRunAction, firstRunSteps, landingAfterImport, resetFirstClipCue } from './first-run';

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

	test('the import step says the files are read in the background, not on the person’s time', () => {
		expect(firstRunSteps({ assets: 0, clips: 0 })[0].body).toContain('reads them in the background');
	});

	test('it is three steps in the order import, cut, export', () => {
		expect(firstRunSteps({ assets: 0, clips: 0 }).map((s) => s.id)).toEqual(['import', 'cut', 'export']);
	});

	test('a step names its key as the user’s keymap prints it, and says nothing when there is none', () => {
		const [imp, , exp] = firstRunSteps({ assets: 0, clips: 0, importKeys: 'Ctrl+I', exportKeys: 'Ctrl+E' });
		expect(imp.body).toBe('Drop files on the window, or choose them (Ctrl+I). Kerf reads them in the background.');
		expect(exp.body).toBe('Render the cut to a file for the platform you are making it for (Ctrl+E).');
		const [bare] = firstRunSteps({ assets: 0, clips: 0 });
		expect(bare.body).toBe('Drop files on the window, or choose them. Kerf reads them in the background.');
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

describe('where the first import leaves the person', () => {
	test('the first import brings the Media tab forward, opening the library when it is closed', () => {
		expect(landingAfterImport({ hadAssets: false, libraryOpen: true, tab: 'media' })).toEqual({ openLibrary: false, mediaTab: false, unfold: false });
		expect(landingAfterImport({ hadAssets: false, libraryOpen: true, tab: 'titles' })).toEqual({ openLibrary: false, mediaTab: true, unfold: false });
		expect(landingAfterImport({ hadAssets: false, libraryOpen: false, tab: 'media' })).toEqual({ openLibrary: true, mediaTab: false, unfold: false });
	});

	test('a library folded to its icon rail is unfolded: the rail shows no list to see the file arrive in', () => {
		expect(landingAfterImport({ hadAssets: false, libraryOpen: true, tab: 'media', collapsed: true })).toEqual({ openLibrary: false, mediaTab: false, unfold: true });
		expect(landingAfterImport({ hadAssets: false, libraryOpen: true, tab: 'titles', collapsed: true })).toMatchObject({ mediaTab: true, unfold: true });
	});

	test('a later import leaves the layout alone', () => {
		expect(landingAfterImport({ hadAssets: true, libraryOpen: false, tab: 'titles', collapsed: true })).toEqual({ openLibrary: false, mediaTab: false, unfold: false });
	});

	test('the first clip on the timeline points at Play once, and says what landed every time', () => {
		resetFirstClipCue();
		expect(firstClipNotice('a.mp4')).toEqual({ text: 'Added a.mp4 to the timeline', play: true });
		expect(firstClipNotice('b.mp4')).toEqual({ text: 'Added b.mp4 to the timeline', play: false });
		resetFirstClipCue();
		expect(firstClipNotice('c.mp4').play).toBe(true);
	});
});
