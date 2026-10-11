// How a failure and an outcome read in a toast. The backend's errors carry the layer they came from
// (`invalid argument: …`, `media engine error: …`) and start lower-case, which reads as a log line
// pasted into the interface; a toast leads with what was being done and then says why, in a
// sentence — `Export failed — Permission denied (os error 13)` — and a finished file is named by its
// file, not by the path it sits at.

const LAYERS = /^(?:invalid argument|media engine error|io error|engine error|internal error)\s*:\s*/i;

/** The reason in a sentence: the layer's prefix off (however many are stacked), the first letter up. */
export function tidyReason(raw: string): string {
	let text = raw.trim();
	for (let prev = ''; prev !== text; ) {
		prev = text;
		text = text.replace(LAYERS, '');
	}
	// The tools are written as their authors write them, so they stay that way at the start of a sentence.
	const tool = /^(ffmpeg|ffprobe)\b/i.exec(text);
	if (tool) return (tool[1].toLowerCase() === 'ffmpeg' ? 'FFmpeg' : 'FFprobe') + text.slice(tool[1].length);
	return text.charAt(0).toUpperCase() + text.slice(1);
}

/** `Export failed — Permission denied (os error 13)`; just the lead when there is no reason. */
export function failure(lead: string, error: unknown): string {
	const raw = error instanceof Error ? error.message : String(error);
	const reason = tidyReason(raw);
	return reason ? `${lead} — ${reason}` : lead;
}

/** The last part of a path, on either kind of separator. */
export function fileName(path: string): string {
	return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** `Exported cut-1x1.mp4`, or `Exported 3 files` for a delivery to several shapes. */
export function exportedLine(outs: readonly string[]): string {
	return outs.length === 1 ? `Exported ${fileName(outs[0])}` : `Exported ${outs.length} files`;
}

export const savedLine = (path: string) => `Saved ${fileName(path)}`;
export const coverLine = (path: string) => `Saved cover ${fileName(path)}`;
