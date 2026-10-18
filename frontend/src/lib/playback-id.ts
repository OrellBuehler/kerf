/* The ids playback streams are told apart by.
 *
 * The backend cancels a stream *by the id the page gave it* (start and stop are separate async calls
 * that can arrive out of order), and keeps the highest id it has seen start as "the one that should
 * run" (`Playbacks` in `crates/kerf-app/src/lib.rs`). A counter that restarts at 1 whenever the page
 * reloads would then never outrank the ids the page before it left behind, and its stops would be
 * mistaken for stops of streams it never started. So an id is two numbers: a **session** the backend
 * hands out per page load (`playback_session`, always higher than the last), and the page's own
 * running count. Pure so the arithmetic is a bun test. */

/** Playback ids a page may draw per session: the low 32 bits of an id. */
export const PLAYBACK_ID_SPAN = 2 ** 32;

/** Sessions that keep every id an exact JavaScript number (below 2^53). */
export const PLAYBACK_MAX_SESSION = 2 ** 21 - 1;

/** The id of this page's `n`th playback in `session`. */
export function playbackId(session: number, n: number): number {
	if (!Number.isInteger(session) || session < 0 || session > PLAYBACK_MAX_SESSION) {
		throw new Error(`playback session ${session} is outside what an id can carry`);
	}
	return session * PLAYBACK_ID_SPAN + (Math.max(0, Math.floor(n)) % PLAYBACK_ID_SPAN);
}
