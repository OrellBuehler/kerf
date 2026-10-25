// Split, roll, slip and slide from the keyboard — pure. The razor and the three trim tools are drags; these
// are the same edits at the playhead and the selection, a frame per press, so they can be done without
// the pointer and repeated exactly. What to do is planned here (and what to say when there is nothing to
// do: a key that does nothing silently is a key that seems broken); `ops.ts` runs the plan.
//
// Every number is the backend's: a roll and a slide move by `delta` timeline seconds, a slip by `delta`
// **source** seconds (`trim-tools.ts`), and a split is placed where the razor places it (`splitPoint`).
// A roll / slip / slide is first run on a scratch copy of the lane through `previewEdit` — the same port the
// drags' ghosts use — so a press that the backend would turn down is told why instead of being sent.

import { quantizeTime, splitPoint } from './frames';
import { cutsOf, previewEdit, sourceLimits, type Cut, type GestureEdit } from './trim-tools';
import { linkPartners } from './link-groups';
import type { Asset, Clip, Timeline } from './types';
import { clipDuration } from './types';
import { meanSpeed } from './speed-ramp';

const endOf = (c: Clip) => c.timeline_start + clipDuration(c);
const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

// ---- split -----------------------------------------------------------------------

export interface PlayheadSplit {
	clipId: string;
	trackName: string;
	at: number;
}

export interface SplitPlan {
	/** One entry per link group and track: the backend carries a split to the partners the time is inside. */
	splits: PlayheadSplit[];
	/** Clips under the playhead that cannot be cut, each with the reason. */
	problems: string[];
	/** How many of the clips in scope the playhead is inside. */
	under: number;
	/** Whether the scope was the selection (else every clip under the playhead). */
	selection: boolean;
}

/**
 * The cut a *Split at playhead* makes: every **selected** clip the playhead is inside — or, with nothing
 * selected, every clip under it on an unlocked track — at the frame the playhead is on, held half a frame
 * inside the clip like the razor. A clip and its linked partners are one cut (the backend splits the
 * partners itself), so a link group appears once.
 */
export function planPlayheadSplit(timeline: Timeline, selected: readonly string[], time: number, fps: number): SplitPlan {
	const want = selected.length > 0 ? new Set(selected) : null;
	const plan: SplitPlan = { splits: [], problems: [], under: 0, selection: want !== null };
	const carried = new Set<string>();
	for (const track of timeline.tracks) {
		for (const clip of track.clips) {
			if (want && !want.has(clip.id)) continue;
			const start = clip.timeline_start;
			const end = endOf(clip);
			if (!(time > start && time < end)) continue;
			plan.under++;
			if (track.locked) {
				// Said for a clip the user picked; a lane they locked is simply not part of "everything under it".
				if (want) plan.problems.push(`${track.name} is locked`);
				continue;
			}
			const at = splitPoint(quantizeTime(time, { fps }), start, end, fps);
			if (at === null) {
				plan.problems.push(`${track.name}: that clip is too short to cut on a frame`);
				continue;
			}
			if (clip.link_id && carried.has(clip.link_id)) continue;
			if (clip.link_id) carried.add(clip.link_id);
			plan.splits.push({ clipId: clip.id, trackName: track.name, at });
		}
	}
	return plan;
}

/** What to say when `plan` has nothing to cut. */
export function splitNotice(plan: SplitPlan, selectedCount: number): string {
	if (plan.under === 0) {
		return selectedCount > 0
			? `Move the playhead into the selected clip${selectedCount === 1 ? '' : 's'} to split ${selectedCount === 1 ? 'it' : 'them'}`
			: 'Move the playhead onto a clip to split it';
	}
	return plan.problems.length === 1 ? capital(plan.problems[0]) : plan.problems.map(capital).join(' · ');
}

/** The toast after a split: how many clips were cut. */
export const splitDone = (n: number): string => (n === 1 ? 'Split at the playhead' : `Split ${n} clips at the playhead`);

// ---- roll, slip, slide -----------------------------------------------------------

export type KeyTool = 'roll' | 'slip' | 'slide';

export interface KeyEditPlan {
	/** What to run: the tool, the clips and the amount in the backend's units. */
	edit: GestureEdit;
	delta: number;
	/** Whether the partners of the clip carry it (it has linked clips). */
	link: boolean;
	/** For a roll: where the cut stands once rolled, for the playhead to follow. */
	cutAt: number | null;
}

export type KeyEditResult = { ok: true; plan: KeyEditPlan; note: string | null } | { ok: false; reason: string };

/** How far a cut may be from the playhead and still be the one a roll key means. */
export const ROLL_REACH_SECONDS = 1;

const NEEDS_CLIP = 'Select a clip first';

/** The cut a roll key acts on: of the cuts on the selected clip's track (every track with a selected
 *  clip, or the first picture track when nothing is selected), the one nearest the playhead within reach. */
export function cutAtPlayhead(timeline: Timeline, selected: readonly string[], time: number, reach = ROLL_REACH_SECONDS): Cut | null {
	const want = new Set(selected);
	const lanes = timeline.tracks.filter((t) => (want.size > 0 ? t.clips.some((c) => want.has(c.id)) : t.kind === 'video'));
	let best: Cut | null = null;
	let bestD = Infinity;
	for (const lane of lanes) {
		for (const cut of cutsOf(lane)) {
			const d = Math.abs(cut.time - time);
			if (d <= reach && d < bestD) {
				best = cut;
				bestD = d;
			}
		}
	}
	return best;
}

/**
 * One press of a roll / slip / slide key: `dir` is -1 (earlier) or 1 (later), a frame of the cut. The edit
 * is tried on a scratch copy first; an edit that cannot move (a limit, a locked track, nothing to roll) is a
 * `reason` to say, and one that is held short of a frame says so in `note`.
 *
 *  - **roll** moves the cut nearest the playhead, `dir` frames in timeline seconds;
 *  - **slip** shifts the primary selected clip's footage by `dir` frames of source time at its speed — the
 *    negative sign is "earlier in the footage", the way the clip reads on screen;
 *  - **slide** moves the primary selected clip `dir` frames along its track, its touching neighbours giving way.
 */
export function planKeyEdit(
	tool: KeyTool,
	o: { timeline: Timeline; assets: readonly Pick<Asset, 'id' | 'duration' | 'streams'>[]; selected: readonly string[]; primary: string | null; time: number; fps: number },
	dir: 1 | -1
): KeyEditResult {
	const rate = Number.isFinite(o.fps) && o.fps > 0 ? o.fps : 30;
	const frame = 1 / rate;
	let edit: GestureEdit;
	let delta: number;
	let link = false;
	let subject: string;
	if (tool === 'roll') {
		const cut = cutAtPlayhead(o.timeline, o.selected, o.time);
		if (!cut) return { ok: false, reason: 'Put the playhead on a cut between two touching clips to roll it' };
		edit = { tool, a: cut.a, b: cut.b };
		delta = dir * frame;
		subject = cut.a;
	} else {
		const id = o.primary ?? o.selected[0] ?? null;
		if (!id) return { ok: false, reason: NEEDS_CLIP };
		const clip = o.timeline.tracks.flatMap((t) => t.clips).find((c) => c.id === id);
		if (!clip) return { ok: false, reason: NEEDS_CLIP };
		edit = { tool, clipId: id };
		const speed = meanSpeed(clip);
		// A slip is in source seconds; the footage runs `speed` source seconds per timeline second (a ramp's average).
		delta = tool === 'slip' ? dir * frame * speed : dir * frame;
		subject = id;
	}
	link = linkPartners(o.timeline, subject).length > 0;
	const p = previewEdit(o.timeline, edit, delta, sourceLimits([...o.assets]), link);
	if (!p.ok) return { ok: false, reason: capital(p.why) };
	if (Math.abs(p.applied) < 1e-9) return { ok: false, reason: p.why ? capital(p.why) : 'Nothing moves there' };
	const cutAt =
		tool === 'roll' && edit.tool === 'roll'
			? (p.ghosts.find((g) => g.id === edit.b)?.start ?? null)
			: null;
	return {
		ok: true,
		plan: { edit, delta: p.applied, link, cutAt },
		note: p.clamped ? capital(p.why) : null
	};
}

/** The toast after a roll / slip / slide key. */
export function keyEditDone(tool: KeyTool, dir: 1 | -1): string {
	const way = dir < 0 ? 'back' : 'forward';
	if (tool === 'roll') return `Rolled the cut ${way} a frame`;
	if (tool === 'slip') return `Slipped the footage ${way} a frame`;
	return `Slid the clip ${way} a frame`;
}
