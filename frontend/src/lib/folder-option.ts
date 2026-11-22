// What a notice about a file offers: *Show in folder*. The toast's button is a closure (it needs the path), and a closure cannot
// be kept in the notification log — so the path rides along as data (`reveal`), and the log draws its own button from it an hour
// later, which opens the same folder. Every toast about a file written to disk takes its options from here.

import { revealPath } from './api';

export const SHOW_IN_FOLDER = 'Show in folder';

/** Toast options for the file at `path`: the button, and the path for the log. */
export function folderOption(path: string): { reveal: string; action: { label: string; onClick: () => void } } {
	return { reveal: path, action: { label: SHOW_IN_FOLDER, onClick: () => void revealPath(path).catch(() => {}) } };
}
