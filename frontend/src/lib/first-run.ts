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
			body: keyed('Drop files on the window, or choose them', o.importKeys) + '.',
			state: imported ? 'done' : 'current'
		},
		{
			id: 'cut',
			title: 'Cut it on the timeline',
			body: 'Drag a clip from the library onto a track, then trim, split and move it.',
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
