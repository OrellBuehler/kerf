/* Which tracks the Mixer has a strip for, and what each strip shows — pure, so the
 * timeline's track header and the Mixer panel cannot disagree about which tracks
 * can be heard.
 *
 * A track is *audible* when it can put sound in the mix: an audio track always
 * (even an empty one — its fader is the thing you reach for before dropping a clip
 * on it), a video track only while one of its clips plays sound of its own. A
 * picture whose sound was **detached** onto an audio track is silent, so a video
 * track made only of those has no strip: its sound rides the audio track it was
 * detached to, and that track's fader is the one that moves it.
 *
 * It mirrors `clip_sounds` in the export graph: `Clip.source_audio`, written only
 * when false, marks a detached picture. */

import { trackRenders } from './levels';
import type { StreamKind, Timeline, Track } from './types';

/** The part of a clip that decides whether it makes sound. `source_audio` is
 *  written only when it is `false` (the backend omits the default). */
export interface SoundingClip {
	asset_id: string;
	source_audio?: boolean | null;
}

/** Whether a clip puts sound in the mix: its asset has an audio stream *and* the clip
 *  still plays it (`source_audio` not false, see above). A detached picture does not. */
export function clipSounds(clip: SoundingClip, audibleAssets: ReadonlySet<string>): boolean {
	return clip.source_audio !== false && audibleAssets.has(clip.asset_id);
}

/** Whether a track can be heard at all: an audio track, or a video track whose clips
 *  carry sound that they still play. A track that cannot gets no fader — a mixer
 *  strip on a silent track is furniture, not a control. */
export function trackHasSound(track: Pick<Track, 'kind' | 'clips'>, audibleAssets: ReadonlySet<string>): boolean {
	if (track.kind === 'audio') return true;
	return track.clips.some((c) => clipSounds(c, audibleAssets));
}

/** How a strip stands in the render: heard, muted, or shadowed by another track's
 *  solo (the export drops both before it mixes). */
export type StripState = 'live' | 'muted' | 'shadowed';

/** One track's strip: its mix controls with every default filled in. */
export interface MixStrip {
	id: string;
	name: string;
	kind: StreamKind;
	/** The fader, linear (1 is unity). */
	volume: number;
	/** The balance, -1 (left) to 1 (right). */
	pan: number;
	muted: boolean;
	solo: boolean;
	duck: boolean;
	locked: boolean;
	state: StripState;
	/** Clips on the track, for the strip's tooltip. */
	clips: number;
}

/** The strips of `timeline`, in track order: one per track that can be heard. */
export function mixerStrips(timeline: Pick<Timeline, 'tracks'>, audibleAssets: ReadonlySet<string>): MixStrip[] {
	const whole = timeline as Timeline;
	const out: MixStrip[] = [];
	for (const t of timeline.tracks) {
		if (!trackHasSound(t, audibleAssets)) continue;
		const renders = trackRenders(whole, t);
		out.push({
			id: t.id,
			name: t.name,
			kind: t.kind,
			volume: t.volume ?? 1,
			pan: t.pan ?? 0,
			muted: !!t.muted,
			solo: !!t.solo,
			duck: !!t.duck,
			locked: !!t.locked,
			state: t.muted ? 'muted' : renders ? 'live' : 'shadowed',
			clips: t.clips.length
		});
	}
	return out;
}

/** What a strip's name says about why it is quiet, for its tooltip. */
export function stateNote(strip: Pick<MixStrip, 'state' | 'kind'>): string | null {
	if (strip.state === 'muted') return strip.kind === 'video' ? 'Hidden — its sound is off too' : 'Muted';
	if (strip.state === 'shadowed') return `Not heard — another ${strip.kind} track is soloed`;
	return null;
}
