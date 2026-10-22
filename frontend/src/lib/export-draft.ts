// The export dialog's options, remembered for the session. They used to start over each time the
// dialog opened, so a click on the scrim — or a Cancel that was meant for something else — threw
// away a preset, the Deliver-to shapes' companion settings and everything under Advanced.

import type { Delivery, ExportOptions } from './types';

export interface ExportDraft {
	opts: ExportOptions;
	/** The preset id the select shows (`custom` once anything was changed by hand). */
	preset: string;
	/** The resolution row is on "Custom…". */
	customRes: boolean;
	/** The project frame these were set for (`frameKey`). */
	frame: string | null;
}

/** The project's delivery frame as something comparable; `null` when it has none. Its frame rate is part of it
 *  (a draft remembers the rate it exported at, and the project's has changed since), and a frame that states none
 *  is keyed as it always was. */
export function frameKey(frame: Pick<Delivery, 'width' | 'height' | 'fit' | 'fps'> | null | undefined): string | null {
	return frame ? `${frame.width}x${frame.height}/${frame.fit}${frame.fps != null ? `@${frame.fps}` : ''}` : null;
}

/** The draft to open on, or `null` to start from the project's frame as ever: with no draft, or
 *  when the project has been re-cut for another frame since — a remembered 9:16 preset must not
 *  open on a cut that is now 16:9, which is what `initialExport` exists to prevent. */
export function restoreDraft(draft: ExportDraft | null, frame: string | null): ExportDraft | null {
	if (!draft || draft.frame !== frame) return null;
	return { ...draft, opts: structuredClone(draft.opts) };
}
