// The Preview's empty state is the first thing a new project shows: three steps, and where the project
// stands in them. Pure, so the card says the same thing whether the bin is empty or the cut is.

export type StepState = 'done' | 'current' | 'todo';

export interface FirstRunStep {
	id: 'import' | 'cut' | 'export';
	title: string;
	/** What to do, one line. */
	body: string;
	state: StepState;
}

/** What the card's one main button does. */
export type FirstRunAction =
	| { kind: 'import'; label: string }
	/** Put the selected media on the timeline — what the next step says to do. */
	| { kind: 'add'; label: string; assetId: string }
	/** The library is not on screen: bring it back. */
	| { kind: 'open-library'; label: string };

/**
 * The button of the empty state. An empty library asks for an import; media in the library and the library on
 * screen means the clip is one press from the timeline — the button used to say *Open Library* over a library
 * that was already open beside it, doing nothing. Only a closed library offers to open it.
 */
export function firstRunAction(o: { assets: readonly { id: string; name: string }[]; selectedId: string | null | undefined; libraryOpen: boolean }): FirstRunAction {
	if (o.assets.length === 0) return { kind: 'import', label: 'Import media…' };
	if (!o.libraryOpen) return { kind: 'open-library', label: 'Open Library' };
	const picked = o.assets.find((a) => a.id === o.selectedId) ?? o.assets[0];
	return { kind: 'add', label: 'Add selected to timeline', assetId: picked.id };
}

/**
 * The three steps — bring media in, cut it, export — and which one the project is on. Importing is done once
 * the library holds an asset, cutting once a clip is on the timeline (and the card is then not shown at all:
 * the Preview has a picture to draw); exporting stays to do.
 */
export function firstRunSteps(o: { assets: number; clips: number; importKeys?: string; exportKeys?: string }): FirstRunStep[] {
	const imported = o.assets > 0;
	const cut = o.clips > 0;
	const keyed = (text: string, keys?: string) => (keys ? `${text} (${keys})` : text);
	return [
		{
			id: 'import',
			title: 'Import media',
			body: keyed('Drop files on the window, or choose them', o.importKeys) + '. Kerf reads them in the background.',
			state: imported ? 'done' : 'current'
		},
		{
			id: 'cut',
			title: 'Cut it on the timeline',
			body: 'Add the media you picked, or drag it from the library onto a track, then trim, split and move it.',
			state: cut ? 'done' : imported ? 'current' : 'todo'
		},
		{
			id: 'export',
			title: 'Export',
			body: keyed('Render the cut to a file for the platform you are making it for', o.exportKeys) + '.',
			state: cut ? 'current' : 'todo'
		}
	];
}

/**
 * Where the first import leaves the person: on the clip. An import into an empty library is the one that must be
 * seen arriving, so the library comes forward on its Media tab — opened when it is closed, unfolded from its icon
 * rail when it is folded (the rail shows no list to see the file in), and switched from another tab; a later
 * import goes where the user already is, and says what landed in a toast.
 */
export function landingAfterImport(o: { hadAssets: boolean; libraryOpen: boolean; tab: string; collapsed?: boolean }): {
	openLibrary: boolean;
	mediaTab: boolean;
	unfold: boolean;
} {
	if (o.hadAssets) return { openLibrary: false, mediaTab: false, unfold: false };
	return { openLibrary: !o.libraryOpen, mediaTab: o.tab !== 'media', unfold: !!o.collapsed };
}

/** What the first clip on the timeline says: that it landed, and — once a launch — where to go next. The card that put it
 *  there is gone the moment it lands, and nothing else points at Play. The cue is a toast's own button (dismissible, over
 *  nothing), only the first time this session: the card only shows while the cut is empty, so that is the first clip. */
let cued = false;
export function firstClipNotice(name: string): { text: string; play: boolean } {
	const play = !cued;
	cued = true;
	return { text: `Added ${name} to the timeline`, play };
}

/** Forget that the cue was given (a test's, or a new project's). */
export function resetFirstClipCue() {
	cued = false;
}
