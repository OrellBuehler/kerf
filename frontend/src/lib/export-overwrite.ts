// What an export is about to write, and which of those files are already there — pure. The save dialog asks about
// the file it picked; what it cannot know is that the container was switched afterwards (`cut.mp4` became
// `cut.mov`) or that a multi-format export writes one sibling per shape (`cut-9x16.mp4`) and never the file
// that was picked. Those are asked about before the render starts (`paths_exist` → `overwriteCopy`).

import { variantPath } from './delivery-formats';
import type { Delivery } from './types';

/** The files the export will write: the chosen path, or — with delivery shapes ticked — one beside it per shape. */
export function exportTargets(outputPath: string, formats: readonly Delivery[]): string[] {
	return formats.length > 0 ? formats.map((f) => variantPath(outputPath, f)) : [outputPath];
}

/** The targets that exist and that nobody has agreed to replace yet. `picked` is the file the native save dialog
 *  returned — which asked already (it is skipped); every other file is asked about. `exists` answers `targets`
 *  in order; a missing answer counts as "not there". */
export function filesToReplace(targets: readonly string[], exists: readonly boolean[], picked: string | null): string[] {
	return targets.filter((t, i) => exists[i] === true && t !== picked);
}
