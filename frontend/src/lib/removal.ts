// What deleting clips says it did — pure, so the wording is pinned by a test
// (`ops.ts` carries the toast, and cannot be imported by one).

/** `Clip removed`, `3 clips ripple-deleted`, with a trailing note for the clips a
 *  locked track kept. */
export function removalNotice(removed: number, skipped: number, ripple: boolean): string {
	const what = removed === 1 ? 'Clip' : `${removed} clips`;
	const kept = skipped > 0 ? ` · ${skipped} on a locked track left alone` : '';
	return `${what} ${ripple ? 'ripple-deleted' : 'removed'}${kept}`;
}
