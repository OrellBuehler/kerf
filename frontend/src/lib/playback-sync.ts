/** Frame pacing for streamed playback: which composited frames the preview
 *  should show, skip, or give up on.
 *
 *  Split out of `Preview.svelte` so it can be tested without a browser or a
 *  backend — the arithmetic here is three lines that only a running desktop app
 *  could otherwise exercise, and it froze the preview once already. */

/** Frames per second to composite during playback. 24 is the lowest rate that
 *  still reads as motion, and compositing several tracks with effects is real
 *  work — asking for 60 on a busy timeline just produces late frames. */
export const PLAYBACK_FPS = 24;

/** How much further behind the audio clock a frame may *drift* than the stream's
 *  own transport floor, and still be worth showing. Roughly two frames: enough
 *  to absorb IPC jitter without letting the picture visibly trail the sound. */
export const STALE_AFTER = 2 / PLAYBACK_FPS;

/** Drift at which the stream is abandoned and restarted from the playhead,
 *  rather than kept and played out behind the sound. */
export const RESYNC_AFTER = 1.0;

export type FrameVerdict = 'show' | 'skip' | 'resync';

/**
 * Build the per-stream gate: give it how far behind the audio clock a frame
 * arrived, and it says what to do with that frame.
 *
 * Every frame arrives *some* amount late — ffmpeg's startup, then base64, JSON
 * and IPC per frame — and the backend paces from the first frame it emits, so
 * that cost lands as a constant offset on the whole stream rather than the
 * picture falling behind. Measured against zero it exceeds the two-frame budget
 * on its own (~60 ms of ffmpeg startup against 83 ms of budget, before the
 * webview sees a byte), which drops every frame forever and leaves the pane on
 * whatever still it had. So lag is judged against the smallest this stream has
 * managed — its transport floor — and only growth beyond that counts as drift.
 */
export function createFrameGate({ staleAfter = STALE_AFTER, resyncAfter = RESYNC_AFTER } = {}) {
	let floor: number | null = null;
	return function verdict(lag: number): FrameVerdict {
		// The floor tracks the minimum rather than trusting the first frame alone:
		// that one carries ffmpeg's spawn and is usually the slowest of the stream,
		// so taking it as the baseline would leave the budget permanently generous.
		floor = floor === null ? lag : Math.min(floor, lag);
		const drift = lag - floor;
		if (drift > resyncAfter) return 'resync';
		if (drift > staleAfter) return 'skip';
		return 'show';
	};
}
