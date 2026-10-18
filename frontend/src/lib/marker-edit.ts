// The marker dialog's fields turned into one `MarkerPatch`: only what changed, with the length
// read as seconds and refused the way `Marker::apply` refuses it.

import type { Marker, MarkerKind, MarkerPatch } from './types';

/** The longest a marker's comment may be (`kerf_core::MAX_MARKER_COMMENT`). */
export const MAX_MARKER_COMMENT = 10_000;

export interface MarkerDraft {
	name: string;
	kind: MarkerKind;
	durationText: string;
	comment: string;
}

export function markerPatch(marker: Marker, draft: MarkerDraft): { patch: MarkerPatch } | { error: string } {
	const text = draft.durationText.trim().replace(',', '.');
	const duration = text === '' ? 0 : Number(text);
	if (!Number.isFinite(duration) || duration < 0) return { error: 'The length is a number of seconds, 0 or more.' };
	if ([...draft.comment].length > MAX_MARKER_COMMENT) return { error: `A comment holds at most ${MAX_MARKER_COMMENT} characters.` };
	const name = draft.name.trim();
	if (!name) return { error: 'A marker needs a name.' };
	const patch: MarkerPatch = {};
	if (name !== marker.name) patch.name = name;
	if (draft.kind !== (marker.kind ?? 'comment')) patch.kind = draft.kind;
	if (Math.abs(duration - (marker.duration ?? 0)) > 1e-9) patch.duration = duration;
	if (draft.comment.trimEnd() !== (marker.comment ?? '')) patch.comment = draft.comment;
	return { patch };
}

/** The ruler chip's tooltip: name, time, and the comment when there is one. */
export function markerTip(marker: Marker, at: string): string {
	const kind = marker.kind === 'chapter' ? ' (chapter)' : '';
	const span = marker.duration ? ` for ${marker.duration}s` : '';
	return `${marker.name}${kind} — ${at}${span}${marker.comment ? `\n${marker.comment}` : ''}`;
}
