// What a side-file export (captions, OTIO / EDL, marker lists) could not carry, in
// words: the backend's `Report` merges identical losses with a count, and this
// turns it into the description a toast (and the notification centre) shows.

import type { Report } from './types';

/** One line per entry, warnings first, `(×n)` when it happened more than once. */
export function describeReport(report: Report): string {
	const ordered = [...report.entries].sort((a, b) => (a.level === b.level ? 0 : a.level === 'warning' ? -1 : 1));
	return ordered.map((e) => `${e.message}${e.count > 1 ? ` (×${e.count})` : ''}`).join('\n');
}

/** Whether anything was dropped or approximated (an info entry loses nothing). */
export const reportWarns = (report: Report) => report.entries.some((e) => e.level === 'warning');
