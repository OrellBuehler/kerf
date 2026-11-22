// What an export left behind, kept for the session: the files it wrote — where, how big, how long — or why
// it failed. A toast is gone in a few seconds and a multi-minute render ends while you are elsewhere, so the
// Deliver panel, the export dialog and the notification log keep this instead (`ui.exportOutcomes`). Pure:
// reports and errors in, rows of words out.

import { fileName, tidyReason, exportedLine } from './failure-copy';
import { fmtSize } from './bin-view';
import { writtenNotice } from './export-written';
import type { ExportReport } from './types';

/** One file an export wrote, as the row for it needs it. */
export interface WrittenFile {
	path: string;
	name: string;
	/** The folder it is in, `''` for a bare name. */
	folder: string;
	bytes: number | null;
	/** How long the written file runs, as the probe read it back. */
	seconds: number | null;
}

export type ExportOutcome =
	| {
			id: number;
			/** Epoch ms. */
			at: number;
			ok: true;
			files: WrittenFile[];
			/** What the files measured, one line per file (`writtenNotice`); null when there is nothing to add. */
			notes: string | null;
			/** A file's true peak is over its target's ceiling. */
			over: boolean;
	  }
	| {
			id: number;
			at: number;
			ok: false;
			/** The backend's reason in a sentence (`tidyReason`), `''` when it gave none. */
			reason: string;
			/** What was being written, for the row: the file the export was going to, or `null`. */
			target: string | null;
	  };

/** How many exports the session keeps; older ones fall off the end. */
export const KEEP_OUTCOMES = 5;

/** The folder a path is in, on either kind of separator: `/home/me/Videos/cut.mp4` is `/home/me/Videos`. */
export function folderOf(path: string): string {
	const cut = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
	if (cut < 0) return '';
	// A root (`/`, `C:\`) keeps its separator.
	return cut === 0 || /^[A-Za-z]:$/.test(path.slice(0, cut)) ? path.slice(0, cut + 1) : path.slice(0, cut);
}

export function writtenFile(r: ExportReport): WrittenFile {
	return { path: r.path, name: fileName(r.path), folder: folderOf(r.path), bytes: r.size_bytes, seconds: r.duration_secs };
}

/** The outcome of an export that wrote `reports`. */
export function succeeded(reports: readonly ExportReport[], at: number, id: number): ExportOutcome {
	const { over, description } = writtenNotice(reports);
	return { id, at, ok: true, files: reports.map(writtenFile), notes: description ?? null, over };
}

/** The outcome of an export that failed with `error`, going to `target`. */
export function failed(error: unknown, target: string | null, at: number, id: number): ExportOutcome {
	const raw = error instanceof Error ? error.message : String(error);
	return { id, at, ok: false, reason: tidyReason(raw), target };
}

/** A failure's reason as a line and what follows it. The backend's reason can be the tail of ffmpeg's own output — a dozen lines
 *  — so the line is what a toast and a heading say and the rest is read where it can scroll (and be copied). A trailing colon on
 *  the line (`FFmpeg exited with status 1:`) is dropped: it only introduced the lines under it. */
export function reasonLines(reason: string): { first: string; rest: string } {
	const lines = reason.replace(/\r\n?/g, '\n').split('\n');
	const at = lines.findIndex((l) => l.trim() !== '');
	if (at < 0) return { first: '', rest: '' };
	const first = lines[at].trim().replace(/:$/, '');
	const rest = lines
		.slice(at + 1)
		.join('\n')
		.replace(/^\s*\n/, '')
		.trimEnd();
	return { first, rest };
}

/** A stop the user asked for is not a failure: it leaves nothing to show. */
export function isStop(error: unknown): boolean {
	const raw = error instanceof Error ? error.message : String(error);
	return raw.startsWith('export cancelled');
}

/** `outcome` first, then the ones kept before it, at most `keep`. */
export function remember(list: readonly ExportOutcome[], outcome: ExportOutcome, keep = KEEP_OUTCOMES): ExportOutcome[] {
	return [outcome, ...list].slice(0, keep);
}

/** `Exported cut.mp4`, `Exported 3 files`; `Export failed` for a failure. */
export function headline(o: ExportOutcome): string {
	return o.ok ? exportedLine(o.files.map((f) => f.path)) : 'Export failed';
}

/** `38.9 MB · 0:20`: what a written file weighs and how long it runs; a part that is not known is left out. */
export function fileFacts(f: WrittenFile, clock: (seconds: number) => string): string {
	const parts: string[] = [];
	if (f.bytes != null) parts.push(fmtSize(f.bytes));
	if (f.seconds != null) parts.push(clock(f.seconds));
	return parts.join(' · ');
}

const hhmm = (at: number) => new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

/** When an outcome happened: `just now`, `3 min ago`, then the time of day once it is an hour old. */
export function when(at: number, now: number, clock: (at: number) => string = hhmm): string {
	const s = Math.max(0, Math.round((now - at) / 1000));
	if (s < 60) return 'just now';
	if (s < 3600) return `${Math.floor(s / 60)} min ago`;
	return clock(at);
}
