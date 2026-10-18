// The long jobs a person starts on purpose and may want to stop — framing the shots (smart crop), a cover frame
// at full size, a speech model download, an import (a 360 lens pair stitching takes minutes). The status bar
// names the one in front and offers Stop; this says which, in what words. Pure, so the words are held by a test.

export type JobId = 'smart-crop' | 'cover' | 'model' | 'import';

export interface JobInputs {
	smartCrop: { fraction: number | null; stopping: boolean } | null;
	cover: { stopping: boolean } | null;
	model: { name: string; fraction: number | null; stopping: boolean } | null;
	importing: { fraction: number | null; stopping: boolean } | null;
}

export interface JobLine {
	id: JobId;
	/** Over the Stop button. */
	label: string;
	/** What Stop does, said before it is pressed. */
	tip: string;
	stopping: boolean;
}

const pct = (f: number | null): string => (f === null ? '' : ` ${Math.round(Math.min(1, Math.max(0, f)) * 100)}%`);

/** The job to show, most specific first (a smart crop and a model download are the ones a person is waiting on). */
export function jobLine(j: JobInputs): JobLine | null {
	if (j.smartCrop)
		return {
			id: 'smart-crop',
			label: `Framing the shots${pct(j.smartCrop.fraction)}`,
			tip: 'Stop framing — the shots stay as they were',
			stopping: j.smartCrop.stopping
		};
	if (j.cover)
		return {
			id: 'cover',
			label: 'Saving the cover frame…',
			tip: 'Stop saving the cover frame and delete the partial file',
			stopping: j.cover.stopping
		};
	if (j.model)
		return {
			id: 'model',
			label: `Downloading the ${j.model.name} speech model${pct(j.model.fraction)}`,
			tip: 'Stop the download — what was fetched is thrown away',
			stopping: j.model.stopping
		};
	if (j.importing)
		return {
			id: 'import',
			label: j.importing.fraction === null ? 'Importing media…' : `Stitching the 360 pair${pct(j.importing.fraction)}`,
			tip: 'Stop importing — a half-written stitch is deleted',
			stopping: j.importing.stopping
		};
	return null;
}
