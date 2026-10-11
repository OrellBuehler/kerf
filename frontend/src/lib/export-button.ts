// The Export button of the title bar and of the empty Preview: there is nothing to render until a clip is on
// the timeline, so it says so instead of opening a dialog with nothing to export. A render that is already
// running keeps it live — the button is how its dialog is reopened, and Stop is there.

export interface ExportButtonState {
	disabled: boolean;
	/** Why it is off, for its tooltip; absent while it is on. */
	reason?: string;
}

export const NOTHING_TO_EXPORT = 'Nothing to export yet — put a clip on the timeline';

export function exportButtonState(o: { clips: number; running: boolean }): ExportButtonState {
	if (o.running || o.clips > 0) return { disabled: false };
	return { disabled: true, reason: NOTHING_TO_EXPORT };
}

/** How many clips a timeline holds, over every track. */
export function clipsIn(timeline: { tracks: ReadonlyArray<{ clips: ReadonlyArray<unknown> }> }): number {
	return timeline.tracks.reduce((n, t) => n + t.clips.length, 0);
}
