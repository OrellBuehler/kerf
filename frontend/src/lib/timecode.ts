import { trackRenders } from './levels';
import { clipDuration, type Asset, type Timeline } from './types';

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

/** The rate the cut plays and exports at: [`timelineFps`] read from the cut **as it renders**, as the
	* engine reads it (`export_format` of `Timeline::for_render` — the muted and solo-shadowed tracks
	* (`trackRenders`) and the disabled clips are not in it, so their footage does not set the rate).
	* What playback's rate, ladder and gate follow; timecode keeps [`timelineFps`], which is a frame
	* count of the cut as it is edited. */
export function renderedFps(timeline: Timeline, assets: Asset[]): number {
	for (const track of timeline.tracks) {
		if (!trackRenders(timeline, track)) continue;
		for (const clip of track.clips) {
			if (clip.enabled === false) continue;
			const stream = assets.find(a => a.id === clip.asset_id)?.streams.find(s => s.kind === 'video');
			if (stream) return validFps(stream.fps);
		}
	}
	return 30;
}

/** How long the cut is **as it renders** (`Timeline::for_render().duration()`): the end of the last
	* enabled clip on a track that renders. What a stream may start before — the engine refuses a start at
	* or past it ("nothing to play past the playhead") — where the timeline's own length also counts the
	* clips that are muted, shadowed by a solo or disabled. */
export function renderedDuration(timeline: Timeline): number {
	let end = 0;
	for (const track of timeline.tracks) {
		if (!trackRenders(timeline, track)) continue;
		for (const clip of track.clips) {
			if (clip.enabled === false) continue;
			end = Math.max(end, clip.timeline_start + clipDuration(clip));
		}
	}
	return end;
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
