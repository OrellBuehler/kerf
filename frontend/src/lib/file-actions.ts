// File-menu commands that are more than a call to the backend: they ask where to
// put the result and say how it went. Shared by the menu, the shortcut table and
// the preview's own context menu, so each of them does exactly the same thing.

import {
	exportCaptions,
	exportCover,
	exportInterchange,
	exportMarkers,
	inTauri,
	pickCoverPath,
	pickSavePath,
	revealPath
} from './api';
import { describeReport, reportWarns } from './export-report';
import type { ExportedFile, InterchangeFormat, MarkerListFormat } from './types';
import { editor } from './state.svelte';
import { toast } from './notifications.svelte';
import { coverLine, failure, savedLine } from './failure-copy';
import { ui } from './editor-ui.svelte';

/** Write the frame under the playhead as a cover image — the thumbnail a
 *  platform shows before anyone presses play. Rendered at the full delivery
 *  frame from the original media, so it is the picture people actually see,
 *  not the downscaled preview on screen. */
export async function saveCoverFrame(): Promise<void> {
	if (!inTauri()) {
		toast.info('Cover frames are rendered with FFmpeg in the desktop app.');
		return;
	}
	const path = await pickCoverPath();
	if (!path) return;
	try {
		const out = await exportCover(ui.time, path);
		toast.success(coverLine(out), {
			action: { label: 'Show in folder', onClick: () => void revealPath(out).catch(() => {}) }
		});
	} catch (e) {
		toast.error(failure("Couldn't save the cover frame", e));
	}
}

/** Write the project to a file the user picks; true once it is on disk. Says how it went — the
 *  File menu's Save, and the "Save…" an unsaved-project prompt offers, are this. */
export async function saveProject(): Promise<boolean> {
	try {
		if (!(await editor.saveProjectAs())) return false;
	} catch (e) {
		toast.error("Couldn't save the project", { description: e instanceof Error ? e.message : String(e) });
		return false;
	}
	const path = editor.currentPath;
	toast.success(path ? savedLine(path) : `Saved ${editor.projectName}`, {
		...(path ? { action: { label: 'Show in folder', onClick: () => void revealPath(path).catch(() => {}) } } : {})
	});
	return true;
}

/** A side-file's default name: the project's, then `suffix`. */
const sideName = (suffix: string) => `${editor.projectName || 'kerf'}.${suffix}`;

/** Say how a side-file export went: what it wrote and, as the description, what its format
 *  could not carry — a warning when something was dropped or approximated. */
function reportWritten(what: string, file: ExportedFile) {
	const opts = {
		description: describeReport(file.report) || undefined,
		action: { label: 'Show in folder', onClick: () => void revealPath(file.path).catch(() => {}) }
	};
	if (reportWarns(file.report)) toast.warning(`${what} written, with losses`, opts);
	else toast.success(`${what} written`, opts);
}

/** Write the cut's captions as a `.srt` or `.vtt` file the user names. */
export async function exportCaptionFile(): Promise<void> {
	const path = await pickSavePath('Subtitles', ['srt', 'vtt'], sideName('srt'));
	if (!path) return;
	try {
		reportWritten('Captions', { path: await exportCaptions(path), report: { entries: [] } });
	} catch (e) {
		toast.error(failure("Couldn't export the captions", e));
	}
}

/** Hand the cut to another editor as OpenTimelineIO or a CMX 3600 EDL. */
export async function exportInterchangeFile(format: InterchangeFormat): Promise<void> {
	const path = await pickSavePath(format === 'otio' ? 'OpenTimelineIO' : 'Edit decision list', [format], sideName(format));
	if (!path) return;
	try {
		reportWritten(format.toUpperCase(), await exportInterchange(path, format));
	} catch (e) {
		toast.error(failure(`Couldn't export the ${format.toUpperCase()}`, e));
	}
}

/** Write the markers as a CSV or as YouTube chapter text. */
export async function exportMarkerList(format: MarkerListFormat): Promise<void> {
	const csv = format === 'csv';
	const path = await pickSavePath(csv ? 'CSV' : 'Text', [csv ? 'csv' : 'txt'], sideName(csv ? 'markers.csv' : 'chapters.txt'));
	if (!path) return;
	try {
		reportWritten(csv ? 'Markers' : 'YouTube chapters', await exportMarkers(path, format));
	} catch (e) {
		toast.error(failure(`Couldn't export the ${csv ? 'markers' : 'chapters'}`, e));
	}
}
