// What an export's progress says, in words. The engine reports a fraction of the whole export and
// the stage it is in (`ExportProgress.stage`), or why it has not started (`waiting`): a render that
// has to open and seek a long recording before it writes a frame reads as "opening the footage", not
// as a bar stuck at 0 %. Pure and bun-tested; the dialog, the Deliver panel and the status bar show it.

import type { ExportProgress } from './types';

/** `37s`, `2m 05s` — a time left or a time spent. */
export function fmtSpan(s: number | null | undefined): string {
	if (s == null || !isFinite(s)) return '';
	const r = Math.max(0, Math.round(s));
	return r >= 60 ? `${Math.floor(r / 60)}m ${String(r % 60).padStart(2, '0')}s` : `${r}s`;
}

const percent = (p: ExportProgress) => Math.round(Math.min(1, Math.max(0, p.fraction)) * 100);

/** What an export rendered in parts says it is doing (`rendering part 2 of 5`, then `encoding`: the words of
 *  `kerf_core::ExportPart`'s `Display`); empty for one that runs whole. */
export function exportPartLabel(p: Pick<ExportProgress, 'part'> | null | undefined): string {
	const part = p?.part;
	if (!part) return '';
	return part.encoding ? 'encoding' : `rendering part ${part.index} of ${part.total}`;
}

/** The part, ahead of whatever else the line says (`rendering part 2 of 5 · `). */
const partPrefix = (p: ExportProgress) => (p.part ? `${exportPartLabel(p)} · ` : '');

/** Which file of how many, ahead of a render's percentage (`3 at once · `, `2/3 · `). */
function filePrefix(p: ExportProgress): string {
	if (p.together) return `${p.total} at once · `;
	if (p.total != null && p.total > 1) return `${(p.variant ?? 0) + 1}/${p.total} · `;
	return '';
}

export interface ProgressLine {
	/** The bar's value, or null for an indeterminate one: the export is working but there is no count to show. */
	value: number | null;
	text: string;
}

/** The dialog's line: the bar, and the words beside it. `null` is an export that has not reported yet. */
export function progressLine(p: ExportProgress | null | undefined): ProgressLine {
	if (!p) return { value: null, text: 'Starting…' };
	if (p.waiting) return { value: null, text: `${p.waiting}…` };
	switch (p.stage) {
		case 'metering':
			return { value: p.fraction, text: `Metering loudness · ${percent(p)}%` };
		case 'starting': {
			// ffmpeg is opening the footage and decoding up to a range's start: nothing is counted, so say how long.
			const spent = fmtSpan(p.elapsed_secs);
			return { value: null, text: `${partPrefix(p)}${spent ? `Opening the footage… ${spent}` : 'Opening the footage…'}` };
		}
		case 'finishing':
			return { value: p.fraction, text: 'Finishing the file…' };
		case 'measuring':
			return { value: p.fraction, text: 'Checking the file…' };
		default: {
			const eta = p.eta_secs != null ? ` · ${fmtSpan(p.eta_secs)} left` : '';
			return { value: p.fraction, text: `${partPrefix(p)}${filePrefix(p)}${percent(p)}%${eta}` };
		}
	}
}

/** The status bar's short line (`Exporting 45%`). */
export function statusText(p: ExportProgress | null | undefined): string {
	if (!p) return 'Exporting 0%';
	if (p.waiting) return `Export ${p.waiting}…`;
	switch (p.stage) {
		case 'metering':
			return `Metering loudness ${percent(p)}%`;
		case 'starting':
			return p.part ? `Export opening the footage… (${exportPartLabel(p)})` : 'Export opening the footage…';
		case 'finishing':
			return 'Finishing the file…';
		case 'measuring':
			return 'Checking the file…';
		default:
			return p.part ? `Exporting ${percent(p)}% · ${exportPartLabel(p)}` : `Exporting ${percent(p)}%`;
	}
}
