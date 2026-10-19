/* Titles on tracks (generated clips) — pure. A title clip is a clip whose `generator` draws an animated SVG
 * title instead of decoding an asset (`asset_id` is `GENERATED_ASSET`); its source is the title's own clock. What
 * kerf-core decides about one is mirrored here for the browser harness and the editor: where a new one goes
 * (`place` in `project/title_clips.rs`), its name, and a title — floating or on a track — looked up by id. */
import { GENERATED_ASSET } from './types';
import type { Clip, TemplateInstance, Timeline, TitleClip } from './types';

/** The most seconds a title runs (`MAX_SVG_SECONDS`, 24 h). */
export const MAX_TITLE_SECONDS = 24 * 3600;

/** Whether `c` is a title on a track. */
export const isTitleClip = (c: Pick<Clip, 'generator'>): boolean => c.generator?.kind === 'title';

/** The title a clip draws, when it is one. */
export const titleOf = (c: Pick<Clip, 'generator'>): TitleClip | null => (c.generator?.kind === 'title' ? c.generator : null);

/** A title's name to show: its own, else its template's, else a placeholder (`TitleClip::display_name`). */
export function titleName(t: Pick<TitleClip, 'name' | 'template'>): string {
	return t.name.trim() || t.template?.name || 'SVG';
}

/** A title found by id, floating (`svg_overlays`) or on a track: what the template controls and the save dialog read. */
export interface TitleRef {
	id: string;
	name: string;
	svg: string;
	template?: TemplateInstance | null;
	/** The title is a clip on a track (its length is its clip's). */
	onTrack: boolean;
	/** How long its document runs: a floating title's span plus what a slice cut off its front, a clip's out-point. */
	duration: number;
}

export function findTitle(timeline: Timeline, id: string): TitleRef | undefined {
	const o = timeline.svg_overlays?.find((t) => t.id === id);
	if (o) {
		return {
			id,
			name: o.name.trim() || o.preset?.params.text || 'SVG',
			svg: o.svg,
			template: o.template,
			onTrack: false,
			duration: Math.max(o.end - o.start, 0) + (o.offset ?? 0)
		};
	}
	for (const track of timeline.tracks) {
		const clip = track.clips.find((c) => c.id === id);
		const title = clip && titleOf(clip);
		if (clip && title) {
			return { id, name: titleName(title), svg: title.svg, template: title.template, onTrack: true, duration: clip.source_out };
		}
	}
	return undefined;
}

/** A new title clip: `title` from `start` for `duration` seconds, its document played from 0. */
export function titleClip(title: TitleClip, start: number, duration: number): Clip {
	return {
		id: crypto.randomUUID(),
		asset_id: GENERATED_ASSET,
		source_in: 0,
		source_out: duration,
		timeline_start: start,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		generator: { kind: 'title', ...title }
	};
}

/** Refuse a title clip's span as kerf-core does (`check_title_span`). */
export function checkTitleSpan(start: number, duration: number): void {
	if (!Number.isFinite(start) || !Number.isFinite(duration)) throw new Error('start and duration must be numbers of seconds');
	if (duration <= 0) throw new Error('a title runs for more than no time');
	if (duration > MAX_TITLE_SECONDS) throw new Error(`a title runs within its first ${MAX_TITLE_SECONDS / 3600} hours`);
}

const clipEnd = (c: Clip): number => c.timeline_start + (c.source_out - c.source_in) / Math.abs(c.speed ?? 1);

/** Put `clip` on the track `trackId` names (it must be video), or the lowest unlocked video track with nothing in the
 *  way over its span, or a new video track on top named after the count (`V3`). Returns the track's id. Mutates
 *  `timeline`. An explicit track that already holds a clip over the span refuses, as the engine's lane check does. */
export function placeTitleClip(timeline: Timeline, clip: Clip, trackId?: string | null): string {
	const start = clip.timeline_start;
	const end = clipEnd(clip);
	const free = (t: Timeline['tracks'][number]) =>
		!t.locked && t.clips.every((c) => clipEnd(c) <= start || c.timeline_start >= end);
	let track = trackId ? timeline.tracks.find((t) => t.id === trackId) : timeline.tracks.find((t) => t.kind === 'video' && free(t));
	if (trackId) {
		if (!track) throw new Error(`track not found: ${trackId}`);
		if (track.kind !== 'video') throw new Error(`${track.name} is not a video track; a title goes on a video track`);
		if (track.clips.some((c) => clipEnd(c) > start + 1e-9 && c.timeline_start < end - 1e-9)) {
			throw new Error(`the clip would overlap another clip on ${track.name}`);
		}
	}
	if (!track) {
		const videos = timeline.tracks.filter((t) => t.kind === 'video').length;
		track = { id: crypto.randomUUID(), kind: 'video', name: `V${videos + 1}`, clips: [] } as Timeline['tracks'][number];
		let at = -1;
		timeline.tracks.forEach((t, i) => {
			if (t.kind === 'video') at = i;
		});
		timeline.tracks.splice(at + 1, 0, track);
	}
	track.clips.push(clip);
	track.clips.sort((a, b) => a.timeline_start - b.timeline_start);
	return track.id;
}
