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

/** Non-drop timecode (minutes:seconds:frames). Count frames before dividing,
	* so fractional rates and floating-point frame stepping never skip a frame. */
export function formatTimecode(seconds: number, fps: number): string {
	const rate = validFps(fps);
	const nominal = Math.max(1, Math.round(rate));
	const frame = Math.floor((Number.isFinite(seconds) ? Math.max(0, seconds) : 0) * rate + 1e-7);
	return [Math.floor(frame / (nominal * 60)), Math.floor(frame / nominal) % 60, frame % nominal]
		.map(n => String(n).padStart(2, '0')).join(':');
}
