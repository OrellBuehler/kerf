/* Which magnet a gesture landed on — pure. The numbers a gesture snaps to are `frames.ts`'s and
 * `beats.ts`'s business (a magnet within reach wins unrounded, a bar before a nearer beat); this
 * is the other half the user needs: *that* it snapped, and to what. `markAt` takes where an
 * edge of the gesture finished and the marks that were in play and names the one it sits exactly
 * on, and `snapLines` does it for a gesture's edges, so the timeline can draw a line at the magnet
 * with its name. It does not decide anything: with snapping off no marks are in play, and an edge
 * that merely rounded to a frame is not "on" one. */

export type MagnetKind = 'start' | 'playhead' | 'bar' | 'beat' | 'edge' | 'title';

export interface Magnet {
	/** Timeline seconds the mark stands at — where the line is drawn. */
	at: number;
	kind: MagnetKind;
	/** What the line says: `Playhead`, `Start`, `Bar 12`, `Beat`, `Edge V1`. */
	label: string;
}

/** What an edge may differ from a mark by and still be on it, seconds: float noise only. A snap
 *  puts the edge on the mark unrounded, so a real landing is exact to the last bit or two. */
export const LAND_EPS = 1e-9;

export interface MagnetSources {
	/** The playhead, seconds. */
	playhead: number;
	/** Every beat and every bar start of the music on the cut (the ruler's grid), seconds. */
	beats?: readonly number[];
	bars?: readonly number[];
	/** The edges of the other clips on the lane being edited, and that lane's name. */
	edges?: readonly number[];
	lane?: string;
	/** The edges of titles and captions, for a title's own drag. */
	titles?: readonly number[];
}

/**
 * The marks a gesture can snap to, in the order that decides a tie — the playhead first, then the
 * cut's start, then a bar over a beat over a clip edge. The same set `Timeline.svelte`'s
 * `snapPoint` / `snapStart` offer to the snap, so what is named is what was in play.
 */
export function magnetList(src: MagnetSources): Magnet[] {
	const out: Magnet[] = [{ at: src.playhead, kind: 'playhead', label: 'Playhead' }, { at: 0, kind: 'start', label: 'Start' }];
	(src.bars ?? []).forEach((at, i) => out.push({ at, kind: 'bar', label: `Bar ${i + 1}` }));
	for (const at of src.beats ?? []) out.push({ at, kind: 'beat', label: 'Beat' });
	for (const at of src.edges ?? []) out.push({ at, kind: 'edge', label: src.lane ? `Edge ${src.lane}` : 'Edge' });
	for (const at of src.titles ?? []) out.push({ at, kind: 'title', label: 'Title edge' });
	return out;
}

/** The first mark an edge is exactly on (`LAND_EPS`), or null. */
export function markAt(edge: number, magnets: readonly Magnet[]): Magnet | null {
	if (!Number.isFinite(edge)) return null;
	for (const m of magnets) if (Math.abs(m.at - edge) <= LAND_EPS) return m;
	return null;
}

/** A line to draw: where, and what it is called. */
export interface SnapLine {
	at: number;
	label: string;
}

/**
 * The lines for a gesture whose edges finished at `edges` (one for a cut or a trimmed edge, two for
 * a span's head and tail): one per edge that sits on a mark, the same time once (a clip butted
 * against the playhead has its head and the playhead's line on one place).
 */
export function snapLines(edges: readonly number[], magnets: readonly Magnet[]): SnapLine[] {
	const out: SnapLine[] = [];
	for (const edge of edges) {
		const m = markAt(edge, magnets);
		if (m && !out.some((l) => Math.abs(l.at - m.at) <= LAND_EPS)) out.push({ at: m.at, label: m.label });
	}
	return out;
}
