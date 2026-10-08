// Roll, slip and slide as *gestures* — pure. `edit-modes.ts` is the edit (the
// faithful port of kerf-core's `roll_edit` / `slip_clip` / `slide_clip`); this is
// what a drag needs on top of it, so the timeline component is left with pointer
// plumbing and nothing it could get wrong:
//
//  - *where the pointer may go* — a cut to grab (`cutsOf` / `nearestCut`), and the
//    range an edit clamps to (`holdToRange`), so the pointer is held where the
//    backend would go no further and says why;
//  - *what it would leave* — `previewEdit` runs the very mirror the harness and the
//    backend's rules are tested against on a scratch copy of the lane, so the ghost
//    is the outcome, not a guess at it (the way `rippleTrimPreview` does for a trim);
//  - *how to say so* — the labels, the reason a drag stopped, and the two frames the
//    Preview shows either side of the edit (`monitorFor`).
//
// The units are the backend's: roll and slide move by `delta` timeline seconds, slip
// by `delta` **source** seconds (positive = the clip starts later in its footage —
// dragging the content to the *left*, which `slipDelta` turns the pointer into).
// Frame quantization is the caller's, once per gesture, from the raw pointer
// (`frames.ts`); nothing here rounds a position.

import {
	ADJACENT_EPS,
	MIN_EDIT_CLIP,
	rollEdit,
	rollEditLinked,
	rollRange,
	rollRangeLinked,
	slideClip,
	slideClipLinked,
	slideRange,
	slideRangeLinked,
	slipClip,
	slipClipLinked,
	slipRange,
	slipRangeLinked,
	type DeltaRange,
	type SourceLimits
} from './edit-modes';
import { toFixedEven } from './format-fixed';
import { quantizeTime, snapToFrame, splitPoint } from './frames';
import { DIFF_EPS } from './ripple';
import { gestureReason } from './link-ui';
import { formatTimecode } from './timecode';
import type { Asset, Clip, SplitSide, StreamKind, Timeline, Track } from './types';
import { clipDuration } from './types';

/** The tools that move a boundary rather than a whole clip. */
export type TrimTool = 'roll' | 'slip' | 'slide';

/** What one gesture edits: the cut between two clips, or one clip. */
export type GestureEdit =
	| { tool: 'roll'; a: string; b: string }
	| { tool: 'slip'; clipId: string }
	| { tool: 'slide'; clipId: string };

export const TOOL_VERB: Record<TrimTool, string> = { roll: 'Roll', slip: 'Slip', slide: 'Slide' };

/** The backend's own phrase for each (`cannot roll the cut later: …`). */
const ACT: Record<TrimTool, string> = { roll: 'roll the cut', slip: 'slip the footage', slide: 'slide the clip' };

/** What a tool does, for its tooltip. Shortcuts are not spelled here — the toolbar
 *  asks the keymap — and all three leave ripple mode alone, which is worth saying:
 *  the toggle is lit for the trim next to them. */
export const TOOL_HINT: Record<TrimTool, string> = {
	roll: 'drag a cut between two touching clips to move it. One clip gains what the other loses, so nothing after the pair moves. Ignores ripple mode.',
	slip: 'drag a clip to show a different part of its footage in the same place; its edges and length stay put. Ignores ripple mode.',
	slide:
		'drag a clip along its track. The clips touching it give way (one grows, one shrinks) and its footage is unchanged. Ignores ripple mode.'
};

/** How close the pointer must be to a cut to take hold of it, css px each side. */
export const CUT_REACH_PX = 8;

const endOf = (c: Clip) => c.timeline_start + clipDuration(c);
const speedMag = (c: Pick<Clip, 'speed'>) => Math.max(Math.abs(c.speed ?? 1), 0.01);
const reversed = (c: Pick<Clip, 'speed'>) => (c.speed ?? 1) < 0;
/** A deep copy that reads through a reactive proxy. `structuredClone` refuses one, and
 *  the timeline the editor holds *is* `$state` — the edits write to what they are handed,
 *  so a preview has to work on a plain copy. (The data is JSON all the way down.) */
export function plainCopy<T>(v: T): T {
	if (Array.isArray(v)) return v.map(plainCopy) as T;
	if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, plainCopy(x)])) as T;
	return v;
}

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
const messageOf = (e: unknown) =>
	gestureReason((e instanceof Error ? e.message : String(e)).replace(/^invalid argument: /, ''));

/** How far each asset's footage reaches — `Project::source_limits`: its duration,
 *  or `Infinity` for a still (it loops, so it never runs out). */
export function sourceLimits(assets: readonly Pick<Asset, 'id' | 'duration' | 'streams'>[]): SourceLimits {
	return new Map(assets.map((a) => [a.id, a.streams.some((s) => s.image) ? Infinity : a.duration]));
}

// ---- cuts -------------------------------------------------------------------

/** A cut a roll can take: two clips of one track that touch, the earlier first. */
export interface Cut {
	trackId: string;
	/** The outgoing clip (ends at the cut). */
	a: string;
	/** The incoming clip (starts at it). */
	b: string;
	/** Timeline seconds — the incoming clip's start. */
	time: number;
}

/** Every cut on a track, in time order. Two clips are a cut when the second starts
 *  where the first ends (within `ADJACENT_EPS`, the engine's own test for a
 *  transition partner): a gap is no cut, nor is an overlap. */
export function cutsOf(track: Track): Cut[] {
	const clips = [...track.clips].sort((x, y) => x.timeline_start - y.timeline_start);
	const out: Cut[] = [];
	for (let i = 0; i + 1 < clips.length; i++) {
		const [a, b] = [clips[i], clips[i + 1]];
		if (Math.abs(b.timeline_start - endOf(a)) < ADJACENT_EPS) out.push({ trackId: track.id, a: a.id, b: b.id, time: b.timeline_start });
	}
	return out;
}

/** The cut nearest `time` and no further than `reach` seconds from it, else null. */
export function nearestCut(cuts: readonly Cut[], time: number, reach: number): Cut | null {
	let best: Cut | null = null;
	let bestD = Infinity;
	for (const c of cuts) {
		const d = Math.abs(c.time - time);
		if (d <= reach && d < bestD) {
			bestD = d;
			best = c;
		}
	}
	return best;
}

/** What a slide sees either side of a clip: the neighbour, and whether it touches
 *  (a touching one gives way; one across a gap is left alone). Mirrors `slidePlan`. */
export interface SlideNeighbours {
	prev: Clip | null;
	next: Clip | null;
	prevTouches: boolean;
	nextTouches: boolean;
}

export function slideNeighbours(track: Track, clipId: string): SlideNeighbours | null {
	const clips = track.clips;
	const ci = clips.findIndex((c) => c.id === clipId);
	if (ci < 0) return null;
	// By start, stable — the backend's order.
	const order = clips.map((_, i) => i).sort((x, y) => clips[x].timeline_start - clips[y].timeline_start);
	const pos = order.indexOf(ci);
	const clip = clips[ci];
	const prev = pos > 0 ? clips[order[pos - 1]] : null;
	const next = pos + 1 < order.length ? clips[order[pos + 1]] : null;
	return {
		prev,
		next,
		prevTouches: !!prev && Math.abs(clip.timeline_start - endOf(prev)) < ADJACENT_EPS,
		nextTouches: !!next && Math.abs(next.timeline_start - endOf(clip)) < ADJACENT_EPS
	};
}

/** The clips that travel with a slid clip — itself and the neighbours that touch it.
 *  Their edges move with it, so they are no magnet to snap it to. */
export function slideMembers(track: Track, clipId: string): Set<string> {
	const members = new Set([clipId]);
	const n = slideNeighbours(track, clipId);
	if (n?.prev && n.prevTouches) members.add(n.prev.id);
	if (n?.next && n.nextTouches) members.add(n.next.id);
	return members;
}

/** Whether every clip a gesture edits is still on the timeline. A drag whose clip is
 *  removed under it (an agent's edit, an undo, Delete) has nothing left to write and is
 *  abandoned rather than finished into an error. */
export function subjectsPresent(timeline: Timeline, edit: GestureEdit): boolean {
	const ids = edit.tool === 'roll' ? [edit.a, edit.b] : [edit.clipId];
	return ids.every((id) => timeline.tracks.some((t) => t.clips.some((c) => c.id === id)));
}

// ---- range, preview ---------------------------------------------------------

/** `requested` held inside `range`: what is applied, whether the range cut it
 *  short, and what stopped it. A request within float noise of a limit is not a clamp. */
export function holdToRange(range: DeltaRange, requested: number): { applied: number; clamped: boolean; why: string } {
	const want = Number.isFinite(requested) ? requested : 0;
	const applied = Math.min(Math.max(want, range.min), range.max);
	const clamped = Math.abs(applied - want) > DIFF_EPS;
	return { applied, clamped, why: !clamped ? '' : want > applied ? range.whyMax : range.whyMin };
}

/** A clip an edit changes, where it would stand. */
export interface GhostClip {
	id: string;
	/** Which part it plays: the cut's outgoing / incoming clip (roll), the slid or
	 *  slipped clip, or the neighbour giving way either side of it — or, for a linked
	 *  edit, `partner`: a clip on another track that the edit carries along. */
	role: 'a' | 'b' | 'clip' | 'prev' | 'next' | 'partner';
	start: number;
	dur: number;
	/** The clip as the edit leaves it (its source window included). */
	clip: Clip;
	/** The track it stands on, and that track's name. */
	trackId: string;
	track: string;
}

export interface EditPreview {
	edit: GestureEdit;
	/** What the pointer asked for, in the edit's own units. */
	requested: number;
	/** What the edit would do: `requested` held inside the range, 0 when it can go nowhere. */
	applied: number;
	/** `applied` is short of `requested` — the drag has hit a limit. */
	clamped: boolean;
	/** What stopped it (when clamped), or why the edit would be refused (when not `ok`). */
	why: string;
	/** The backend would take it. False for a clip that is gone, a locked track, a
	 *  pair that is no longer a cut — drawn red, and releasing does nothing. */
	ok: boolean;
	/** Every clip the edit changes, where it would stand; empty when nothing moves. */
	ghosts: GhostClip[];
}

/** The clips an edit changed, as ghosts: those on the subject's own track (`own`) play the
 *  roles the tool gives them, the rest — a linked edit's partners and what gives way beside
 *  them — are `partner`. `where` says which track a clip is on after the edit. */
function ghostsOf(
	edit: GestureEdit,
	clips: Clip[],
	own: ReadonlySet<string>,
	where: (clipId: string) => { id: string; name: string }
): GhostClip[] {
	const ghost = (c: Clip, role: GhostClip['role']): GhostClip => {
		const t = where(c.id);
		return { id: c.id, role, start: c.timeline_start, dur: clipDuration(c), clip: c, trackId: t.id, track: t.name };
	};
	const mine = clips.filter((c) => own.has(c.id));
	const rest = clips.filter((c) => !own.has(c.id)).map((c) => ghost(c, 'partner'));
	if (edit.tool === 'roll') return [...mine.map((c, i) => ghost(c, i === 0 ? 'a' : 'b')), ...rest];
	if (edit.tool === 'slip') return [...mine.map((c) => ghost(c, 'clip')), ...rest];
	// A slide answers [previous?, the clip, next?] in timeline order.
	const at = mine.findIndex((c) => c.id === edit.clipId);
	return [...mine.map((c, i) => ghost(c, i < at ? 'prev' : i > at ? 'next' : 'clip')), ...rest];
}

/**
 * What a gesture that has asked for `requested` leaves behind: the edit applied to a
 * scratch copy of the lane by the same port the backend's rules are tested through,
 * after holding the request to the range it clamps to. Never throws and never touches
 * `timeline` — a refusal comes back as `ok: false` with the backend's reason.
 */
export function previewEdit(
	timeline: Timeline,
	edit: GestureEdit,
	requested: number,
	footage: SourceLimits,
	links = false
): EditPreview {
	const subject = edit.tool === 'roll' ? edit.a : edit.clipId;
	const base: EditPreview = { edit, requested, applied: 0, clamped: false, why: '', ok: true, ghosts: [] };
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === subject));
	if (!track) return { ...base, ok: false, why: `clip not found: ${subject}` };
	// With links the edit reaches the partners' lanes too, so the sandbox holds every track
	// that has a linked clip (a partner's lane is one of them) beside the subject's own.
	const lanes = links ? timeline.tracks.filter((t) => t === track || t.clips.some((c) => c.link_id)) : [track];
	const sandbox: Timeline = { tracks: plainCopy(lanes) };
	const own = new Set(track.clips.map((c) => c.id));
	try {
		const range =
			edit.tool === 'roll'
				? (links ? rollRangeLinked : rollRange)(sandbox, edit.a, edit.b, footage)
				: edit.tool === 'slip'
					? (links ? slipRangeLinked : slipRange)(sandbox, edit.clipId, footage)
					: (links ? slideRangeLinked : slideRange)(sandbox, edit.clipId, footage);
		const held = holdToRange(range, requested);
		const idle = { ...base, clamped: held.clamped, why: held.why };
		if (Math.abs(held.applied) <= DIFF_EPS) return idle;
		const out =
			edit.tool === 'roll'
				? (links ? rollEditLinked : rollEdit)(sandbox, edit.a, edit.b, held.applied, footage)
				: edit.tool === 'slip'
					? (links ? slipClipLinked : slipClip)(sandbox, edit.clipId, held.applied, footage)
					: (links ? slideClipLinked : slideClip)(sandbox, edit.clipId, held.applied, footage);
		const where = (id: string) => {
			const t = sandbox.tracks.find((x) => x.clips.some((c) => c.id === id)) ?? sandbox.tracks[0];
			return { id: t.id, name: t.name };
		};
		return { ...idle, applied: out.applied, ghosts: ghostsOf(edit, out.clips, own, where) };
	} catch (e) {
		return { ...base, ok: false, why: messageOf(e) };
	}
}

/** The source-seconds `delta` for a slip gesture whose pointer has travelled `dt`
 *  timeline seconds (positive = right). The content follows the pointer, so
 *  dragging right shows *earlier* footage — a negative `delta` — and a reversed
 *  clip needs no special case: the backend's sign means the same on screen for
 *  both. Rounded to a frame of the cut, once, then scaled by speed (a 2× clip
 *  moves its picture two source seconds for every second the pointer covers). */
export function slipDelta(dt: number, clip: Pick<Clip, 'speed'>, fps: number): number {
	return 0 - snapToFrame(dt, fps) * speedMag(clip);
}

// ---- words ---------------------------------------------------------------------

/** `+12 f · +0.40 s` — a signed shift in frames and seconds. */
export function deltaLabel(delta: number, fps: number): string {
	if (!Number.isFinite(delta) || Math.abs(delta) < DIFF_EPS) return '0 f';
	const rate = Number.isFinite(fps) && fps > 0 ? fps : 30;
	const sign = delta < 0 ? '−' : '+';
	const mag = Math.abs(delta);
	return `${sign}${Math.round(mag * rate)} f · ${sign}${mag.toFixed(2)} s`;
}

export interface Readout {
	title: string;
	/** The second line: what stopped it, or why it would be refused. */
	detail: string | null;
	/** `ok` plain, `limit` the drag is held at a limit, `refused` the edit would be turned down. */
	tone: 'ok' | 'limit' | 'refused';
}

/** What the live readout beside the pointer (and the monitor's header) says. */
export function readoutFor(p: EditPreview, fps: number): Readout {
	const verb = TOOL_VERB[p.edit.tool];
	if (!p.ok) return { title: verb, detail: p.why, tone: 'refused' };
	if (p.ghosts.length === 0) {
		// Nothing to show: either not moved yet, or pinned against a limit it cannot leave.
		return p.clamped ? { title: `${verb} — at the limit`, detail: p.why, tone: 'limit' } : { title: verb, detail: null, tone: 'ok' };
	}
	const detail = p.clamped ? p.why : null;
	const tone = p.clamped ? 'limit' : 'ok';
	const tc = (s: number) => formatTimecode(s, fps);
	const g = p.ghosts;
	// A linked edit says whose lanes come along: `· with A1`.
	const carried = [...new Set(g.filter((x) => x.role === 'partner').map((x) => x.track))];
	const along = carried.length > 0 ? ` · with ${carried.join(', ')}` : '';
	if (p.edit.tool === 'roll') {
		const b = g.find((x) => x.role === 'b');
		return { title: `${verb} ${deltaLabel(p.applied, fps)}${b ? ` · cut ${tc(b.start)}` : ''}${along}`, detail, tone };
	}
	const me = g.find((x) => x.role === 'clip');
	if (p.edit.tool === 'slip') {
		const mag = me ? speedMag(me.clip) : 1;
		const span = me ? ` · in ${tc(me.clip.source_in)} → out ${tc(me.clip.source_out)}` : '';
		return { title: `${verb} ${deltaLabel(p.applied / mag, fps)}${span}${along}`, detail, tone };
	}
	return { title: `${verb} ${deltaLabel(p.applied, fps)}${me ? ` · now at ${tc(me.start)}` : ''}${along}`, detail, tone };
}

/** What the toast says when the backend would turn the edit down (`track V1 is locked`). */
export const refusalNotice = (p: EditPreview): string => capital(p.why);

/** The notice for a drag that was held at a limit — said once, on release. The
 *  Tauri commands answer with the refreshed timeline rather than the backend's
 *  `EditOutcome`, so `clamped` is decided here, from the same range the backend
 *  clamps to. */
export function limitNotice(p: EditPreview, fps: number): string {
	const tool = p.edit.tool;
	if (Math.abs(p.applied) <= DIFF_EPS) return `Can't ${ACT[tool]} ${p.requested > 0 ? 'later' : 'earlier'} — ${p.why}`;
	return `${TOOL_VERB[tool]} stopped at ${deltaLabel(tool === 'slip' ? p.applied / speedMag(p.ghosts[0]?.clip ?? {}) : p.applied, fps)} — ${p.why}`;
}

// ---- the monitor: the frames either side of the edit ---------------------------

/** The source time of a clip's first or last *shown* frame. A forward clip opens on
 *  `source_in` and closes one frame short of `source_out`; a reversed one plays the
 *  window backwards, so the two swap. A frame is the cut's (`fps`), scaled by speed. */
export function edgeFrameTime(clip: Clip, edge: 'first' | 'last', fps: number): number {
	const step = Number.isFinite(fps) && fps > 0 ? speedMag(clip) / fps : 0;
	const nearOut = Math.max(clip.source_in, clip.source_out - step);
	const head = reversed(clip) ? nearOut : clip.source_in;
	const tail = reversed(clip) ? clip.source_in : nearOut;
	return Math.max(0, edge === 'first' ? head : tail);
}

export interface MonitorCell {
	key: string;
	/** `Out` is a clip's last frame at the edit, `In` the next one's first. */
	label: 'Out' | 'In';
	clipId: string;
	assetId: string;
	/** Source seconds. */
	time: number;
	timecode: string;
}

export interface TrimMonitor extends Readout {
	cells: MonitorCell[];
}

/**
 * What the Preview shows while a gesture is live: the frames at the edit as it would
 * be left — a roll's outgoing last frame beside the incoming first, a slip's new in
 * and out, a slide's two neighbours' changed edges. `null` when there is no picture
 * to show (an audio track, a slide whose neighbours do not touch, a drag not yet
 * anywhere). Frames come from the source (`get_frame`), not the composite: it is the
 * *footage* that moves.
 */
export function monitorFor(p: EditPreview, fps: number, kind: StreamKind): TrimMonitor | null {
	if (kind !== 'video' || p.ghosts.length === 0) return null;
	const cell = (g: GhostClip, label: 'Out' | 'In'): MonitorCell => {
		const time = edgeFrameTime(g.clip, label === 'Out' ? 'last' : 'first', fps);
		return { key: `${label}:${g.id}`, label, clipId: g.id, assetId: g.clip.asset_id, time, timecode: formatTimecode(time, fps) };
	};
	const by = (role: GhostClip['role']) => p.ghosts.find((g) => g.role === role);
	const cells: MonitorCell[] = [];
	const [a, b, prev, me, next] = [by('a'), by('b'), by('prev'), by('clip'), by('next')];
	if (p.edit.tool === 'roll') {
		if (a) cells.push(cell(a, 'Out'));
		if (b) cells.push(cell(b, 'In'));
	} else if (p.edit.tool === 'slip') {
		if (me) cells.push(cell(me, 'In'), cell(me, 'Out'));
	} else {
		if (prev) cells.push(cell(prev, 'Out'));
		if (next) cells.push(cell(next, 'In'));
	}
	return cells.length === 0 ? null : { ...readoutFor(p, fps), cells };
}

// ---- trim to the playhead -------------------------------------------------------

/** Where a clip would be cut for `Trim start/end to playhead`: the playhead put on a
 *  frame and held half a frame inside the clip, exactly the razor's rule — or why not.
 *  The backend also refuses to leave under `MIN_EDIT_CLIP` of the clip, which is
 *  said here so a keypress gets a sentence rather than an error code. */
export function playheadCut(clip: Clip, time: number, fps: number, side: SplitSide): { at: number } | { why: string } {
	const start = clip.timeline_start;
	const end = endOf(clip);
	if (!(time > start && time < end)) return { why: 'the playhead is not inside the clip' };
	const at = splitPoint(quantizeTime(time, { fps }), start, end, fps);
	if (at === null) return { why: 'that clip is too short to cut on a frame' };
	const kept = side === 'left' ? end - at : at - start;
	if (kept < MIN_EDIT_CLIP - DIFF_EPS)
		return { why: `that would leave only ${toFixedEven(kept, 2)}s of the clip — remove the clip instead` };
	return { at };
}

export interface PlayheadTrim {
	clipId: string;
	trackName: string;
	at: number;
}

export interface TrimPlan {
	/** The selected clips under the playhead that can be cut, in timeline order of the
	 *  tracks — at most one per track, which is what the backend's group trim takes. */
	trims: PlayheadTrim[];
	/** Selected clips under the playhead that cannot, each with the reason. */
	problems: string[];
	/** How many of the selected clips the playhead is inside. */
	under: number;
}

/**
 * Which clips `Trim start to playhead` / `Trim end to playhead` act on: every
 * *selected* clip the playhead is inside, on an unlocked track — a V1 clip and its A1
 * partner selected together are trimmed together, which is what the keypress means
 * with both selected. Clips the playhead is not in are not touched (and not
 * reported); ones it is in but that cannot be cut are `problems`.
 */
export function planPlayheadTrim(timeline: Timeline, selected: readonly string[], time: number, fps: number, side: SplitSide): TrimPlan {
	const want = new Set(selected);
	const plan: TrimPlan = { trims: [], problems: [], under: 0 };
	for (const track of timeline.tracks) {
		let cutHere = false;
		for (const clip of track.clips) {
			if (!want.has(clip.id) || !(time > clip.timeline_start && time < endOf(clip))) continue;
			plan.under++;
			if (track.locked) {
				plan.problems.push(`${track.name} is locked`);
				continue;
			}
			const cut = playheadCut(clip, time, fps, side);
			if ('why' in cut) plan.problems.push(`${track.name}: ${cut.why}`);
			// A lane with two clips under one playhead (an old project that overlaps itself):
			// the backend trims one clip per track in a group, so the second is said, not sent.
			else if (cutHere) plan.problems.push(`${track.name}: two selected clips are under the playhead — trim them one at a time`);
			else {
				plan.trims.push({ clipId: clip.id, trackName: track.name, at: cut.at });
				cutHere = true;
			}
		}
	}
	return plan;
}

/** What to say when `plan` has nothing to cut. */
export function trimNotice(plan: TrimPlan, selectedCount: number, side: SplitSide): string {
	const edge = side === 'left' ? 'start' : 'end';
	if (selectedCount === 0) return `Select a clip first — trim ${edge} to playhead works on the selected clip under it`;
	if (plan.under === 0) return `Move the playhead into the selected clip${selectedCount === 1 ? '' : 's'} to trim the ${edge}`;
	return plan.problems.length === 1 ? capital(plan.problems[0]) : plan.problems.map(capital).join(' · ');
}
