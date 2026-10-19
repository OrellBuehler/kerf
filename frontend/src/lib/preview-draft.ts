// What a gesture under way would write, as a draft the GPU preview draws before anything is written
// (`get_preview_frame`'s `draft`, `crates/kerf-app/src/gpu_preview/draft.rs`). Each builder mirrors the
// edit the gesture writes on release and says `null` where it does not know that edit's outcome
// exactly (a keyed number, whose key the backend places): the preview then shows the cut as it is
// until the gesture is let go, as it did before. Only the GPU route sends a draft; the JPEG route never
// reads one.

import { isKeyed } from './channels';
import { withKeyframeAt } from './titles';
import type { MovePlan } from './multi-move';
import { DEFAULT_COLOR, DEFAULT_MASK, DEFAULT_TRANSFORM, clipDuration } from './types';
import type { Clip, Color, Mask, PreviewDraft, Property, TextOverlay, Timeline, Track, Transform } from './types';

/** The numbers a transform key can hold (`setTf`'s `ANIM_KEYS`); crop is always the static transform. */
const ANIMATABLE: ReadonlySet<string> = new Set(['scale', 'pos_x', 'pos_y', 'rotation', 'opacity']);

/** Where a clip is: its track and itself. */
function find(timeline: Timeline, clipId: string): { trackId: string; clip: Clip } | null {
	for (const t of timeline.tracks) {
		const clip = t.clips.find((c) => c.id === clipId);
		if (clip) return { trackId: t.id, clip };
	}
	return null;
}

/** One clip changed where it is, or `null` when `change` cannot say what the edit leaves. */
export function clipDraft(timeline: Timeline, clipId: string, change: (c: Clip) => Clip | null): PreviewDraft | null {
	const at = find(timeline, clipId);
	const clip = at && change(at.clip);
	return at && clip ? { clips: [{ track_id: at.trackId, clip }], overlays: [] } : null;
}

/** `setTf`: a static transform patch. A number that is keyed (or a clip with whole-transform keys) is
 *  written as a key at the playhead, which this does not draw. */
export function transformDraft(timeline: Timeline, clipId: string, patch: Partial<Transform>): PreviewDraft | null {
	return clipDraft(timeline, clipId, (c) => {
		const keys = Object.keys(patch);
		const animatable = keys.every((k) => ANIMATABLE.has(k));
		if (animatable && ((c.keyframes?.length ?? 0) > 0 || keys.some((k) => isKeyed(c, k as Property)))) return null;
		return { ...c, transform: { ...DEFAULT_TRANSFORM, ...c.transform, ...patch } };
	});
}

/** `setCol`: a static colour patch, unless a patched number is keyed. */
export function colorDraft(timeline: Timeline, clipId: string, patch: Partial<Color>): PreviewDraft | null {
	return clipDraft(timeline, clipId, (c) => {
		if (Object.keys(patch).some((k) => isKeyed(c, k as Property))) return null;
		return { ...c, color: { ...DEFAULT_COLOR, ...c.color, ...patch } };
	});
}

/** `patchMask`: the clip's mask (the default where it has none) with `patch`. */
export function maskDraft(timeline: Timeline, clipId: string, patch: Partial<Mask>): PreviewDraft | null {
	return clipDraft(timeline, clipId, (c) => ({ ...c, mask: { ...DEFAULT_MASK, ...(c.mask ?? {}), ...patch } }));
}

/** `setFade`: a fade held to the clip's length. */
export function fadeDraft(timeline: Timeline, clipId: string, to: { fade_in?: number; fade_out?: number }): PreviewDraft | null {
	return clipDraft(timeline, clipId, (c) => {
		const len = clipDuration(c);
		return {
			...c,
			fade_in: to.fade_in === undefined ? c.fade_in : Math.min(to.fade_in, len),
			fade_out: to.fade_out === undefined ? c.fade_out : Math.min(to.fade_out, len)
		};
	});
}

/** One title changed whole. */
function titleDraft(overlays: readonly TextOverlay[], id: string, change: (o: TextOverlay) => TextOverlay): PreviewDraft | null {
	const o = overlays.find((o) => o.id === id);
	return o ? { clips: [], overlays: [change(o)] } : null;
}

/** `moveOverlay`: a still title takes the position, an animated one a key at timeline time `at`. */
export function titleMoveDraft(overlays: readonly TextOverlay[], id: string, x: number, y: number, at: number): PreviewDraft | null {
	return titleDraft(overlays, id, (o) =>
		o.keyframes?.length ? { ...o, keyframes: withKeyframeAt(o, at, x, y) } : { ...o, pos_x: x, pos_y: y }
	);
}

/** `updateOverlay { size }`. */
export function titleSizeDraft(overlays: readonly TextOverlay[], id: string, size: number): PreviewDraft | null {
	return titleDraft(overlays, id, (o) => ({ ...o, size }));
}

/** A move the timeline would write (`planMove`'s plan): every clip of the group, partners carried along
 *  included, where its ghost is. Nothing while the drop is refused or changes nothing. */
export function moveDraft(timeline: Timeline, plan: MovePlan | null): PreviewDraft | null {
	if (!plan || !plan.ok || plan.noop) return null;
	const clips = [];
	for (const g of plan.ghosts) {
		const at = find(timeline, g.clipId);
		if (!at) return null;
		clips.push({ track_id: g.trackId, clip: { ...at.clip, timeline_start: g.start } });
	}
	return clips.length ? { clips, overlays: [] } : null;
}

/** Two drafts that say the same: a gesture's pointer events that leave the edit as it was (a move held on
 *  one frame) are not new frames to draw. */
export function sameDraft(a: PreviewDraft | null, b: PreviewDraft | null): boolean {
	return same(a, b);
}

function same(a: unknown, b: unknown): boolean {
	if (Object.is(a, b)) return true;
	if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
	if (Array.isArray(a) !== Array.isArray(b)) return false;
	const x = a as Record<string, unknown>;
	const y = b as Record<string, unknown>;
	const keys = (o: Record<string, unknown>) => Object.keys(o).filter((k) => o[k] !== undefined);
	const kx = keys(x);
	return kx.length === keys(y).length && kx.every((k) => same(x[k], y[k]));
}

/** The one draft there is, and who wrote it (an Inspector row, the timeline, the Preview's title box). */
export interface DraftSlot {
	owner: symbol | null;
	draft: PreviewDraft | null;
}

export const EMPTY_SLOT: DraftSlot = { owner: null, draft: null };

/** The slot once `owner` wrote `draft`: its draft replaces whatever is there, but its `null` clears only its
 *  own, so one gesture ending never takes another's frame away. The slot itself comes back when nothing
 *  changes (the same writer, the same draft), so a pointer event that changes nothing asks for no frame. */
export function putDraft(slot: DraftSlot, owner: symbol, draft: PreviewDraft | null): DraftSlot {
	if (draft === null) return slot.owner === owner ? EMPTY_SLOT : slot;
	if (slot.owner === owner && sameDraft(slot.draft, draft)) return slot;
	return { owner, draft };
}

/** Whether the cut still holds everything `draft` names: an undo, or an agent's edit, may have taken a clip
 *  or a title away under a gesture, and the backend refuses a draft that names one it does not hold. */
export function draftHolds(draft: PreviewDraft, timeline: Timeline, overlays: readonly TextOverlay[]): boolean {
	return (
		draft.clips.every((d) => find(timeline, d.clip.id) && timeline.tracks.some((t) => t.id === d.track_id)) &&
		draft.overlays.every((o) => overlays.some((held) => held.id === o.id))
	);
}

/** A frame either side of the playhead, beyond any frame rate's. */
const NEAR = 0.1;

/** Whether `draft` changes the frame at `t`: a clip on a video track, where it is or where it goes, that is
 *  at `t` (or near enough for a transition across one of its cuts to reach it), or a title on screen at `t`
 *  either way. A draft that touches nothing there draws the frame the cut already draws, so it is not sent. */
export function draftTouches(draft: PreviewDraft, timeline: Timeline, overlays: readonly TextOverlay[], t: number): boolean {
	const track = (id: string) => timeline.tracks.find((tr) => tr.id === id);
	/** How far a transition on these tracks reaches across a cut. */
	const reach = (...tracks: (Track | undefined)[]) =>
		NEAR + Math.max(0, ...tracks.flatMap((tr) => tr?.clips.map((c) => c.transition_in?.duration ?? 0) ?? []));
	const covers = (c: Clip, r: number) => t >= c.timeline_start - r && t <= c.timeline_start + clipDuration(c) + r;
	for (const d of draft.clips) {
		const at = find(timeline, d.clip.id);
		if (!at) return true;
		const from = track(at.trackId);
		const to = track(d.track_id);
		if (from?.kind !== 'video' && to?.kind !== 'video') continue;
		const r = reach(from, to) + (d.clip.transition_in?.duration ?? 0);
		if (covers(at.clip, r) || covers(d.clip, r)) return true;
	}
	for (const o of draft.overlays) {
		const was = overlays.find((held) => held.id === o.id);
		if (!was) return true;
		if ([was, o].some((x) => t >= x.start - NEAR && t <= x.end + NEAR)) return true;
	}
	return false;
}

/** A trim of one clip alone (no ripple, no partners following): the fields `trimEdit` writes. */
export function trimDraft(
	timeline: Timeline,
	clipId: string,
	edit: Partial<Pick<Clip, 'source_in' | 'source_out' | 'timeline_start'>>
): PreviewDraft | null {
	return clipDraft(timeline, clipId, (c) => ({ ...c, ...edit }));
}
