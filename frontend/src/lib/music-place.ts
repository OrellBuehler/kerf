// Where composed music lands on the timeline — the faithful TS mirror of `project/music.rs`'s placement
// (`music_bed`, `music_start`, `put_music`, `replace_bed`, `check_music_fits`). Pure, and used only by the
// browser harness in `api.ts`: the desktop app asks the backend. The messages are the core's own, and the
// bun tests (`music-place.test.ts`) replay its cases.
//
// A new piece goes on the audio track named `M` (made on first use), at `at` or after what the track holds.
// Replacing replaces the **whole bed**: every clip of the named clip's asset on its track (the segments Fit to
// video made of one piece) becomes one clip of the new render, at the earliest of their starts with the named
// clip's id, the first clip's level, fade-in, effects and keys and the last one's fade-out. Either way the clip
// is one the edit itself put somewhere, so it may not end up on another clip of its track (`checkOwnLanes`,
// after the ripple, which may have made the room).

import { clipDuration } from './types';
import { checkOwnLanes, rippleLanes } from './ripple';
import type { Clip, Timeline, Track } from './types';

/** The audio track composed music lands on (`MUSIC_TRACK`). */
export const MUSIC_TRACK = 'M';

const invalid = (why: string) => new Error(`invalid argument: ${why}`);

/** Where a piece goes: `ComposeRequest`'s placement fields. */
export interface MusicPlacement {
	track_id?: string;
	at?: number;
	replace_clip_id?: string;
}

/** What placing needs to know about the asset it lands: its id and the length of its render. */
export interface PlacedAsset {
	id: string;
	duration: number;
}

/** Whether an asset was composed from a score (it carries one). */
export type IsComposed = (assetId: string) => boolean | undefined;

const trackEnd = (t: Track): number => t.clips.reduce((m, c) => Math.max(m, c.timeline_start + clipDuration(c)), 0);

/** Rust's `f64::total_cmp` for the finite numbers a timeline holds: it orders -0 before +0. */
function totalCmp(a: number, b: number): number {
	if (a < b) return -1;
	if (a > b) return 1;
	const negA = Object.is(a, -0);
	const negB = Object.is(b, -0);
	return negA === negB ? 0 : negA ? -1 : 1;
}

/** `Project::music_bed`: the clips a replacement replaces, in time order — every clip of the named clip's asset
 *  on its track. Refused unless the clip is on an unlocked audio track, its asset was composed and none of the
 *  bed is linked to a picture. */
export function musicBed(timeline: Timeline, clipId: string, composed: IsComposed): Clip[] {
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
	if (!track) throw new Error(`clip not found: ${clipId}`);
	if (track.kind !== 'audio') throw invalid('only music on an audio track can be replaced');
	if (track.locked) throw invalid(`${track.name} is locked`);
	const assetId = track.clips.find((c) => c.id === clipId)!.asset_id;
	if (composed(assetId) === undefined) throw new Error(`asset not found: ${assetId}`);
	if (!composed(assetId)) throw invalid('that clip is not composed music: only a clip of a composed asset can be replaced');
	const bed = track.clips.filter((c) => c.asset_id === assetId);
	if (bed.some((c) => c.link_id)) throw invalid('unlink the music from its picture before composing it again');
	return [...bed].sort((a, b) => totalCmp(a.timeline_start, b.timeline_start));
}

/** `music_start`: where music placed with these arguments would start — the track exists, is audio and is not
 *  locked; at `at`, else after what the track holds (0 for an `M` track not made yet). */
export function musicStart(timeline: Timeline, trackId: string | undefined, at: number | undefined): number {
	let track: Track | undefined;
	if (trackId !== undefined) {
		track = timeline.tracks.find((t) => t.id === trackId);
		if (!track) throw new Error(`track not found: ${trackId}`);
		if (track.kind !== 'audio') throw invalid('music goes on an audio track');
	} else {
		track = timeline.tracks.find((t) => t.kind === 'audio' && t.name === MUSIC_TRACK);
	}
	if (track?.locked) throw invalid(`${track.name} is locked`);
	return at !== undefined ? Math.max(at, 0) : track ? trackEnd(track) : 0;
}

/** `replace_bed`: the whole bed `clipId` belongs to becomes one clip of `asset`, in `timeline` (mutated). */
function replaceBed(timeline: Timeline, clipId: string, asset: PlacedAsset): Clip {
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
	if (!track) throw new Error(`clip not found: ${clipId}`);
	if (track.locked) throw invalid(`${track.name} is locked`);
	const assetId = track.clips.find((c) => c.id === clipId)!.asset_id;
	const bed = track.clips.filter((c) => c.asset_id === assetId).sort((a, b) => totalCmp(a.timeline_start, b.timeline_start));
	const first = bed[0];
	// `max_by` keeps the last of equal ends.
	const last = bed.reduce((m, c) => (totalCmp(c.timeline_start + clipDuration(c), m.timeline_start + clipDuration(m)) >= 0 ? c : m), bed[0]);
	const clip: Clip = structuredClone(first);
	clip.id = clipId;
	clip.asset_id = asset.id;
	clip.source_in = 0;
	clip.source_out = asset.duration;
	clip.speed = 1;
	delete clip.speed_ramp;
	clip.timeline_start = first.timeline_start;
	const duration = clipDuration(clip);
	clip.fade_in = Math.min(first.fade_in, duration);
	clip.fade_out = Math.min(last.fade_out, duration);
	// The new clip takes the place of the first one of the bed in the track's order.
	const at = track.clips.findIndex((c) => c.id === first.id);
	track.clips = track.clips.filter((c) => c.asset_id !== assetId);
	track.clips.splice(Math.min(at, track.clips.length), 0, clip);
	return clip;
}

/** `Project::put_music`: put `asset`'s music on `timeline` (mutated) as clip `clipId` — the one placement rule, shared
 *  by the rehearsal before the render and the placing after it. `newId` names a track made on first use. */
export function putMusic(
	timeline: Timeline,
	asset: PlacedAsset,
	place: MusicPlacement,
	clipId: string,
	composed: IsComposed,
	newId: () => string
): Clip {
	if (place.replace_clip_id !== undefined) {
		musicBed(timeline, place.replace_clip_id, composed);
		return replaceBed(timeline, place.replace_clip_id, asset);
	}
	let track: Track | undefined;
	if (place.track_id !== undefined) {
		track = timeline.tracks.find((t) => t.id === place.track_id);
		if (!track) throw new Error(`track not found: ${place.track_id}`);
		if (track.kind !== 'audio') throw invalid('music goes on an audio track');
	} else {
		track = timeline.tracks.find((t) => t.kind === 'audio' && t.name === MUSIC_TRACK);
		if (!track) {
			track = { id: newId(), kind: 'audio', name: MUSIC_TRACK, clips: [] };
			timeline.tracks.push(track);
		}
	}
	if (track.locked) throw invalid(`${track.name} is locked`);
	const start = place.at !== undefined ? Math.max(place.at, 0) : trackEnd(track);
	const clip = {
		id: clipId,
		asset_id: asset.id,
		source_in: 0,
		source_out: asset.duration,
		timeline_start: start,
		volume: 1,
		fade_in: 0,
		fade_out: 0
	} as Clip;
	track.clips.push(clip);
	return structuredClone(clip);
}

/** `check_music_fits`: rehearse the placement on a copy of the cut with a render of `duration` seconds, under the
 *  ripple the edit will have: refused now — before anything renders — what placing would refuse (the track is
 *  locked, the start lands on another clip, a longer piece would run into the next one). With ripple on a piece
 *  that only *pushes* what is there passes. */
export function rehearseMusic(
	timeline: Timeline,
	place: MusicPlacement,
	duration: number,
	ripple: boolean,
	composed: IsComposed,
	newId: () => string
) {
	const clipId = place.replace_clip_id ?? newId();
	let after = structuredClone(timeline);
	putMusic(after, { id: newId(), duration }, place, clipId, composed, newId);
	if (ripple) after = rippleLanes(after, timeline);
	checkOwnLanes(after, timeline, [clipId]);
}
