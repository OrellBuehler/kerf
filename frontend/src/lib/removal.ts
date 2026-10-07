// What deleting and cutting clips say they did — pure, so the wording is pinned by a test
// (`ops.ts` carries the toast, and cannot be imported by one).

/** `Clip removed`, `3 clips ripple-deleted`, with a trailing note for the clips a
 *  locked track kept. */
export function removalNotice(removed: number, skipped: number, ripple: boolean): string {
	const what = removed === 1 ? 'Clip' : `${removed} clips`;
	const kept = skipped > 0 ? ` · ${skipped} on a locked track left alone` : '';
	return `${what} ${ripple ? 'ripple-deleted' : 'removed'}${kept}`;
}

/** What a cut says it did: `Clip cut`, `3 clips cut`, with the same note for a locked track. */
export function cutNotice(cut: number, skipped: number): string {
	const what = cut === 1 ? 'Clip' : `${cut} clips`;
	const kept = skipped > 0 ? ` · ${skipped} on a locked track left alone` : '';
	return `${what} cut${kept}`;
}

/** Why nothing was removed, when the whole selection sat on locked tracks. */
export function lockedNotice(skipped: number): string {
	return skipped === 1 ? 'That clip is on a locked track' : `Those ${skipped} clips are on locked tracks`;
}
