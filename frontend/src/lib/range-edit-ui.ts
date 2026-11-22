// What the range edits ask and say — pure. *Add edit to all tracks*, *Lift*, *Extract* and *Ripple delete gap* are
// one revision each in the engine (`range-edit.ts`); this is the part around them: which range the in / out marks
// are (frame-quantized, so the engine's "less than a frame left" refusal is not met from a gesture), the plain
// sentence when there is none, and the words after — with everything the edit did not do said, so nothing about it
// happens silently: the locked tracks it left alone, the clips it left whole, the sound the sync lock trimmed back.

import { snapToFrame } from './frames';
import { toFixedEven } from './format-fixed';
import { measureRange } from './levels-view';
import { sliver } from './range-edit';
import type { CutAll, GapClosed, LockTrim, RangeEdit, SkippedTrack, Timeline } from './types';
import { clipDuration } from './types';

/** A position as the interface writes times (`timeDisplay.fmt`). */
export type Fmt = (seconds: number) => string;

const plural = (n: number, one: string, many = `${one}s`): string => `${n} ${n === 1 ? one : many}`;
const list = (items: readonly string[]): string => (items.length <= 2 ? items.join(' and ') : `${items.slice(0, -1).join(', ')} and ${items[items.length - 1]}`);

/** A length in seconds, to the centisecond: `2.40 s`. */
export const seconds = (s: number): string => `${toFixedEven(Math.max(0, s), 2)} s`;

// ---- the marked range ----------------------------------------------------------------

export type RangePlan = { ok: true; from: number; to: number } | { ok: false; reason: string };

/** The keys the in / out points have, for the sentence that says to set them (`settings.shortcut`). */
export interface MarkKeys {
	in?: string;
	out?: string;
}

/**
 * The range *Lift* and *Extract* act on: the in and out marks, each on the frame it is nearest. Both must be set
 * and in order (`measureRange`, the rule range export follows); otherwise the plain sentence about what is missing.
 */
export function planMarkedRange(markIn: number | null, markOut: number | null, fps: number, verb: 'lift' | 'extract', keys: MarkKeys = {}): RangePlan {
	const how = keys.in && keys.out ? ` (${keys.in} and ${keys.out} at the playhead)` : '';
	if (markIn === null && markOut === null) return { ok: false, reason: `There is no range to ${verb}: set an in point and an out point first${how}` };
	if (markOut === null) return { ok: false, reason: `The range has an in point but no out point: set one to ${verb} it${keys.out ? ` (${keys.out})` : ''}` };
	if (markIn === null) return { ok: false, reason: `The range has an out point but no in point: set one to ${verb} it${keys.in ? ` (${keys.in})` : ''}` };
	const range = measureRange(markIn, markOut);
	if (!range) return { ok: false, reason: 'The out point is not after the in point' };
	const from = snapToFrame(range.start, fps);
	const to = snapToFrame(range.end, fps);
	// Two marks inside one frame are the same frame: there is nothing between them to take.
	if (!(to > from + 1e-9)) return { ok: false, reason: 'The range is shorter than a frame — move the out point further from the in point' };
	return { ok: true, from, to };
}

// ---- the words after ------------------------------------------------------------------

/** What an edit says: the line, and what else is worth knowing (shown under it, kept in the log). */
export interface Said {
	text: string;
	detail?: string;
	/** Something was left undone or changed that was not asked for: said as a warning, not a note. */
	warn: boolean;
}

/** `Track A2 is locked, so nothing there was touched`. */
export function skippedLine(skipped: readonly SkippedTrack[]): string | null {
	if (skipped.length === 0) return null;
	const names = skipped.map((s) => s.name);
	return skipped.length === 1 ? `${names[0]} is locked and was left alone` : `${list(names)} are locked and were left alone`;
}

/** The sound the sync lock trimmed back to make room for a linked sound the edit carried along. */
export function lockTrimmedLine(trimmed: readonly LockTrim[]): string | null {
	if (trimmed.length === 0) return null;
	const parts = trimmed.map((t) => `${seconds(t.seconds)} off a clip on ${t.track}`);
	return `The sync lock trimmed ${list(parts)} to make room`;
}

/** Clips a cut left whole because one half would have been shorter than a frame. */
export function notCutLine(names: readonly string[]): string | null {
	if (names.length === 0) return null;
	return `${plural(names.length, 'clip')} left whole — ${names.length === 1 ? 'a half' : 'one half'} would have been under a frame${names.length <= 3 ? `: ${list(names)}` : ''}`;
}

const joined = (lines: readonly (string | null)[]): string | undefined => {
	const parts = lines.filter((l): l is string => !!l);
	return parts.length > 0 ? parts.join('. ') : undefined;
};

/** After *Add edit to all tracks*. `nameOf` names a clip by id for the ones left whole. */
export function cutAllSaid(r: CutAll, fmt: Fmt, nameOf: (clipId: string) => string = (id) => id): Said {
	const detail = joined([skippedLine(r.skipped), notCutLine(r.not_cut.map(nameOf))]);
	return { text: `Cut ${plural(r.cuts.length, 'clip')} at ${fmt(r.at)}`, detail, warn: !!detail };
}

/** What a lift or an extract did to the clips, in the order it did it. */
function effects(r: RangeEdit): string {
	const parts = [
		r.removed > 0 ? `${plural(r.removed, 'clip')} removed` : null,
		r.trimmed > 0 ? `${plural(r.trimmed, 'clip')} trimmed` : null,
		r.split > 0 ? `${plural(r.split, 'clip')} split` : null
	].filter((p): p is string => !!p);
	return parts.length > 0 ? parts.join(', ') : 'nothing on the tracks was in it';
}

/** After *Lift* and *Extract*. */
export function rangeSaid(kind: 'lift' | 'extract', r: RangeEdit, fmt: Fmt): Said {
	const span = `${fmt(r.from)} to ${fmt(r.to)}`;
	const head =
		kind === 'lift'
			? `Lifted ${span}: ${effects(r)}`
			: `Extracted ${span}: ${effects(r)}${r.moved > 0 ? `, ${plural(r.moved, 'clip')} moved up ${seconds(r.closed)}` : ''}`;
	const detail = joined([skippedLine(r.skipped), lockTrimmedLine(r.lock_trimmed)]);
	return { text: head, detail, warn: !!detail };
}

/** After *Ripple delete gap*. `track` is the name of the track it was on. */
export function gapSaid(r: GapClosed, track: string): Said {
	const detail = joined([lockTrimmedLine(r.lock_trimmed)]);
	return { text: `Closed a ${seconds(r.to - r.from)} gap on ${track} — ${plural(r.moved, 'clip')} moved up`, detail, warn: !!detail };
}

// ---- where a cut through every track would land ---------------------------------------------------------

export interface CutLanes {
	/** The unlocked tracks a cut at that time would go through: those with a clip it is inside (with room for a frame
	 *  on each side), and — with links — those holding a clip linked to one of those that it is also inside. Empty when
	 *  `blocked`: the engine refuses the whole cut then. */
	cut: string[];
	/** The names of the locked tracks it would have gone through, left alone. */
	locked: string[];
	/** The locked track that holds a linked partner of a clip the cut would split: the engine refuses the **whole** cut
	 *  then (`a linked clip is on locked track A1 — unlock it first`), not a partial one that leaves that track alone.
	 *  Null when nothing is in the way; never set with links off (Alt). */
	blocked: string | null;
}

/**
 * The lanes *Add edit to all tracks* at `at` would cut, for the razor's line before anything is pressed, and for
 * saying there is nothing to cut before the engine is asked. The rule is `splitAllTracks`' own (a clip is cut when
 * `at` is inside it by more than a frame's sliver either side; with links, a partner on a locked track that the cut
 * would split refuses the lot); the answer after the edit is the engine's. `fps` is the rate the engine reads
 * (`renderedFps`), so the sliver is the same one.
 */
export function cutAllLanes(tl: Timeline, at: number, fps: number, links = true): CutLanes {
	const keep = sliver(1 / fps);
	const endOf = (c: { timeline_start: number } & Parameters<typeof clipDuration>[0]) => c.timeline_start + clipDuration(c);
	const cuttable = (c: Parameters<typeof endOf>[0]) => c.timeline_start + keep < at && at < endOf(c) - keep;
	const inside = (c: Parameters<typeof endOf>[0]) => c.timeline_start < at && at < endOf(c);
	const cut = new Set<string>();
	const locked = new Set<string>();
	const linkedIds = new Set<string>();
	for (const t of tl.tracks) {
		if (t.locked) continue;
		for (const c of t.clips) {
			if (!cuttable(c)) continue;
			cut.add(t.id);
			if (links && c.link_id) linkedIds.add(c.link_id);
		}
	}
	let blocked: string | null = null;
	for (const t of tl.tracks) {
		if (!t.locked) continue;
		for (const c of t.clips) {
			if (!cuttable(c)) continue;
			locked.add(t.name);
			if (links && c.link_id && linkedIds.has(c.link_id)) blocked ??= t.name;
		}
	}
	if (links && linkedIds.size > 0) {
		for (const t of tl.tracks) if (!t.locked && t.clips.some((c) => c.link_id && linkedIds.has(c.link_id) && inside(c))) cut.add(t.id);
	}
	const names = tl.tracks.filter((t) => locked.has(t.name)).map((t) => t.name);
	if (blocked) return { cut: [], locked: names, blocked };
	return { cut: tl.tracks.filter((t) => cut.has(t.id)).map((t) => t.id), locked: names, blocked: null };
}

/** What the refusal for a locked linked partner says, and how to get past it. */
export const blockedLine = (track: string): string =>
	`${track} is locked and holds a clip linked to what would be cut — unlock it, or hold Alt to cut the other clips on their own`;

/** Why a cut through every track has nothing to cut, or null when it has something. `source` is where the time came
 *  from: the playhead (the key, the menus) or a point the pointer is on (the razor with Shift), which the sentence names. */
export function cutAllWhy(lanes: CutLanes, source: 'playhead' | 'pointer' = 'playhead'): string | null {
	if (lanes.blocked) return blockedLine(lanes.blocked);
	if (lanes.cut.length > 0) return null;
	const here = source === 'pointer' ? 'that point' : 'the playhead';
	if (lanes.locked.length > 0) return `${list(lanes.locked)} ${lanes.locked.length === 1 ? 'is' : 'are'} locked — nothing else is under ${here} to cut`;
	return source === 'pointer' ? 'No clip is under that point on an unlocked track — nothing to cut' : 'Move the playhead onto a clip to cut every track there';
}
