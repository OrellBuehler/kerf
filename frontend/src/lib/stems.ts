// The pure side of stem separation: the words the dialog and the menus use, when the menus
// offer it, and `placeStems` — the browser harness's copy of `Project::place_stems`. Kerf-core
// runs Demucs for real; the harness fakes the audio and keeps what the editor does with the
// result honest: the four stems as library assets and, under a clip, four new tracks with the
// clip's own sound switched off.

import type { Asset, Clip, StemsPlaced, StemsProgress, StemsStatus, Timeline, Track } from './types';
import { approxMB } from './voiceover';

/** The sources the model separates, in its output order (`kerf_core::STEM_NAMES`). Each is
 *  also a track name, capitalized, when the stems are laid under a clip. */
export const STEM_NAMES = ['drums', 'bass', 'other', 'vocals'] as const;

/** The size the harness pretends the model is (`MODEL_APPROX_BYTES` in `engine/stems.rs`). */
export const DEV_MODEL_BYTES = 174 * 1024 * 1024;

/** What the backend rejects with when a run is stopped. */
export const STEMS_CANCELLED = 'stems cancelled';

/** Whether a rejection is the user's stop rather than a failure. Tauri rejects with a bare
 *  string, the harness with an `Error`. */
export function isStemsCancelled(e: unknown): boolean {
	return (e instanceof Error ? e.message : String(e)) === STEMS_CANCELLED;
}

const STAGE_LABELS: Record<StemsProgress['stage'], string> = {
	download_runtime: 'Downloading audio runtime…',
	download_model: 'Downloading separation model…',
	separate: 'Separating the sound…',
	encode: 'Saving the stems…'
};

export function stageLabel(stage: StemsProgress['stage']): string {
	return STAGE_LABELS[stage] ?? 'Working…';
}

/** `Drums`, as the track a stem lands on is named (`capitalized` in `project.rs`). */
export const capitalized = (s: string): string => (s ? s[0].toUpperCase() + s.slice(1) : s);

/** The heads-up before the first run: what it still has to download, or `null` when both the
 *  runtime and the model are already on disk. */
export function downloadNote(status: Pick<StemsStatus, 'runtime_ready' | 'model_ready' | 'model_bytes'>): string | null {
	const parts: string[] = [];
	if (!status.model_ready) parts.push(`the separation model (${approxMB(status.model_bytes)})`);
	if (!status.runtime_ready) parts.push('the audio runtime');
	return parts.length === 0 ? null : `The first separation downloads ${parts.join(' and ')}.`;
}

/** Whether a clip offers *Separate stems under this clip*, and if it cannot be used right now, why. */
export interface StemsOffer {
	/** The clip's asset has sound to separate. */
	show: boolean;
	/** Why the action is greyed out — the backend's own refusals, said before the click. */
	reason: string | null;
}

/**
 * `show` is whether the asset has an audio stream. The reasons mirror what `Project::place_stems`
 * refuses once the stems are made: a locked track, and a picture whose sound has been detached
 * onto a linked audio clip (separate that clip instead).
 */
export function stemsOffer(track: Track | undefined, clip: Clip, asset: Pick<Asset, 'streams'> | undefined): StemsOffer {
	if (!asset?.streams.some((s) => s.kind === 'audio')) return { show: false, reason: null };
	if (track?.locked) return { show: true, reason: `Track ${track.name} is locked` };
	if (track?.kind !== 'audio' && clip.link_id) {
		return { show: true, reason: "Its sound is on a linked audio clip — separate that clip" };
	}
	return { show: true, reason: null };
}

/** The toast after a run: what was split and, under a clip, where the parts went. */
export function describeStems(sourceName: string, placed: Pick<StemsPlaced, 'clips'>): string {
	const parts = `${STEM_NAMES.slice(0, -1).join(', ')} and ${STEM_NAMES[STEM_NAMES.length - 1]}`;
	return placed.clips.length > 0
		? `Split ${sourceName} into ${parts} — on ${placed.clips.length} new tracks`
		: `Split ${sourceName} into ${parts} — in the library`;
}

/** The stem assets the harness makes for `source` — what `separate_stems_media` returns, with
 *  no audio behind them. The path is the source's, so a second run finds them again by path
 *  (`insert_or_get_asset`), as the cache makes the real ones. */
export function devStemAssets(source: Asset, newId: () => string, importedAt: string): Asset[] {
	return STEM_NAMES.map((name) => ({
		id: newId(),
		path: `/stems/${source.id}/${name}.flac`,
		name: `${source.name} · ${name}`,
		duration: source.duration,
		streams: [{ index: 0, kind: 'audio', codec: 'flac', sample_rate: 44100, channels: 2 }],
		imported_at: importedAt
	}));
}

/**
 * `Project::place_stems` with a clip: lay the stems under `clipId`, **in place** on `timeline` —
 * each on a new audio track named after it, at the clip's position, span, speed, gain and fades —
 * and switch the clip's own sound off (an audio clip disabled, a picture's `source_audio` off) so
 * the mix is not heard twice. Throws before touching anything when the clip is not of `source`,
 * its track is locked, or it is a picture whose sound is on a linked audio clip. `stems` are the
 * library's assets for them, in `STEM_NAMES` order. Returns the clips it placed.
 */
export function placeStems(timeline: Timeline, source: string, stems: readonly Asset[], clipId: string, newId: () => string): Clip[] {
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
	const clip = track?.clips.find((c) => c.id === clipId);
	if (!track || !clip) throw new Error(`clip not found: ${clipId}`);
	if (clip.asset_id !== source) throw new Error('invalid argument: that clip is not of the separated asset');
	if (track.locked) throw new Error(`invalid argument: ${track.name} is locked`);
	const onAudio = track.kind === 'audio';
	if (!onAudio && clip.link_id) {
		throw new Error("invalid argument: this picture's sound is on a linked audio clip — separate that clip instead");
	}
	const placed: Clip[] = [];
	// Zipped with the names, as the core does: a stem without a name (or a name without a stem) is dropped.
	STEM_NAMES.forEach((name, i) => {
		const stem = stems[i];
		if (!stem) return;
		const c: Clip = {
			id: newId(),
			asset_id: stem.id,
			source_in: clip.source_in,
			source_out: Math.min(clip.source_out, stem.duration),
			timeline_start: clip.timeline_start,
			volume: clip.volume,
			fade_in: clip.fade_in,
			fade_out: clip.fade_out,
			speed: clip.speed ?? 1
		};
		timeline.tracks.push({ id: newId(), kind: 'audio', name: capitalized(name), clips: [structuredClone(c)] });
		placed.push(c);
	});
	if (onAudio) clip.enabled = false;
	else clip.source_audio = false;
	return placed;
}
