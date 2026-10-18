// The Agent panel's quick edits — pure. Four of the presets are edits Kerf can make by itself, now
// ("run now": silences, the beat, captions, framing); the rest need an agent's judgement and are queued
// for one. *Assemble rough cut* used to run the very same silence removal as *Remove silences* under a
// bigger name — the one place the panel promised more than it did — so it is an agent task now.
//
// Which footage a quick edit acts on used to be whatever happened to be selected in the library, or the
// first asset, without saying. `quickTarget` makes that one rule and `targetNote` says it.

import type { Asset, Timeline } from './types';

/** The presets Kerf runs itself; anything else is queued for the connected agent. */
export const LOCAL_PRESETS: ReadonlySet<string> = new Set(['Remove silences', 'Cut to the beat', 'Caption the cut', 'Frame for the delivery']);

export const isLocalPreset = (preset: string): boolean => LOCAL_PRESETS.has(preset);

/** The presets that read one asset (its silences); the others work on the whole cut. */
const ONE_ASSET = new Set(['Remove silences']);

export interface QuickTarget {
	id: string;
	name: string;
}

/**
 * The asset a quick edit acts on: the footage of the selected clip, else the library's selection, else
 * the first picture on the cut, else the first asset there is. `null` with an empty library.
 */
export function quickTarget(o: {
	clipAssetId: string | null | undefined;
	assetId: string | null | undefined;
	assets: readonly Pick<Asset, 'id' | 'name'>[];
	timeline: Timeline;
}): QuickTarget | null {
	const byId = (id: string | null | undefined) => (id ? o.assets.find((a) => a.id === id) : undefined);
	const onCut = o.timeline.tracks.flatMap((t) => t.clips.map((c) => c.asset_id)).map(byId).find(Boolean);
	const a = byId(o.clipAssetId) ?? byId(o.assetId) ?? onCut ?? o.assets[0];
	return a ? { id: a.id, name: a.name } : null;
}

/** The line a preset's tooltip and the panel carry about what it acts on. */
export function targetNote(preset: string, target: QuickTarget | null): string {
	if (!isLocalPreset(preset)) return 'Queued for your agent, which proposes the result for you to review';
	if (ONE_ASSET.has(preset)) return target ? `Acts on ${target.name}` : 'Import media first';
	if (preset === 'Cut to the beat') return 'Acts on every cut, to the music on the audio tracks';
	return 'Acts on the whole cut';
}

/** What a finished silence removal says. `changed` is whether the cut differs from before. */
export function silenceOutcome(target: QuickTarget, changed: boolean): { kind: 'success' | 'info'; text: string } {
	return changed
		? { kind: 'success', text: `Removed the silences of ${target.name} from the cut` }
		: { kind: 'info', text: `Nothing to remove — no detected silence of ${target.name} is on the cut` };
}

/** The cut's clips as they stand — what a quick edit compares before and after to tell a no-op from an edit. */
export function cutSignature(timeline: Timeline): string {
	return JSON.stringify(timeline.tracks.map((t) => [t.id, t.clips.map((c) => [c.id, c.timeline_start, c.source_in, c.source_out])]));
}
