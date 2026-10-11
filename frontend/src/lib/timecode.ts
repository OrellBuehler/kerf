import type { Asset, Timeline } from './types';

const validFps = (fps: number | undefined): number =>
	fps !== undefined && Number.isFinite(fps) && fps > 0 ? fps : 30;

/** Match the engine's default export timebase: first clip carrying video,
	* across all tracks, with a 30 fps fallback. Media selection is irrelevant. */
export function timelineFps(timeline: Timeline, assets: Asset[]): number {
	for (const track of timeline.tracks) {
		for (const clip of track.clips) {
			const stream = assets.find(a => a.id === clip.asset_id)?.streams.find(s => s.kind === 'video');
			if (stream) return validFps(stream.fps);
		}
	}
	return 30;
}

/** The project's one timecode: non-drop `HH:MM:SS:FF`. Hours are always shown, so a cut under
	* a minute reads `00:00:20:15` and cannot be taken for minutes:seconds:frames. Count frames
	* before dividing, so fractional rates and floating-point frame stepping never skip a frame.
	* `frames: false` is the same count cut at the second (`HH:MM:SS`), for a ruler label that a
	* whole-second step does not resolve below. */
export function formatTimecode(seconds: number, fps: number, o: { frames?: boolean } = {}): string {
	const rate = validFps(fps);
	const nominal = Math.max(1, Math.round(rate));
	const frame = Math.floor((Number.isFinite(seconds) ? Math.max(0, seconds) : 0) * rate + 1e-7);
	const fields = [Math.floor(frame / (nominal * 3600)), Math.floor(frame / (nominal * 60)) % 60, Math.floor(frame / nominal) % 60];
	if (o.frames !== false) fields.push(frame % nominal);
	return fields.map(n => String(n).padStart(2, '0')).join(':');
}
