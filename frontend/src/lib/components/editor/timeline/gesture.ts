/* The state each timeline gesture holds while it runs, and the pointer travel that turns a press into a drag. */

import type { TrimBounds } from '#lib/ripple-trim';
import type { marqueeMode, Selection } from '#lib/selection';
import type { GestureEdit, TrimTool } from '#lib/trim-tools';
import type { StreamKind } from '#lib/types';

export type Drag = {
	clipId: string; // the clip the pointer grabbed
	kind: StreamKind;
	origTrackId: string;
	origStart: number;
	grabSec: number; // pointer offset within the clip (seconds)
	dur: number;
	/** Every clip that moves with it: the selection when the press was on one of
	 *  several selected clips, else just the clip itself. Fixed at the press. */
	members: ReadonlySet<string>;
	/** The press was on one of several selected clips — a click without a drag
	 *  then narrows the selection to it, as a click always does. */
	group: boolean;
	/** The press had Alt down: that narrowing is to the one clip, partners and all. */
	alone: boolean;
	start: number; // the grabbed clip's ghost start (seconds)
	trackId: string; // the grabbed clip's ghost destination track
	/** The pointer, in lane space — where the refusal's reason is written. */
	nx: number;
	ny: number;
	downX: number; // where the pointer went down, px: "moved" is judged on this
	moved: boolean;
};

/** A run of arrow-key nudges on a focused clip: drawn where the clip will land, as a drag is, and
 *  written as one edit once the keys go quiet (`nudge.ts`). */
export type Nudge = {
	clipId: string;
	trackId: string;
	members: ReadonlySet<string>;
	origStart: number;
	dur: number;
	/** The run so far, in frames of the cut. */
	frames: number;
	/** Alt was down: the clip alone, its linked partners left where they are. */
	alone: boolean;
	/** The edit is on its way to the backend: the clip stays drawn where it is going. */
	writing: boolean;
};

/** The part of a pointer event a gesture reads — so autoscroll can hand a gesture the pointer where
 *  it is now, after the lanes moved under it, without an event. */
export type PointerLike = Pick<PointerEvent, 'clientX' | 'clientY' | 'altKey' | 'shiftKey' | 'buttons'>;

/** Pointer travel (px) before a press on a clip or an edge is a drag. Judged on
 *  the pointer itself, never on the quantized position: at high zoom one pixel
 *  is under a frame, so a 1 px jitter of a click can round to the next frame. */
export const DRAG_SLOP = 3;

export type TrimDrag = {
	clipId: string;
	trackId: string;
	edge: 'l' | 'r';
	/** The dragged edge's bounds, timeline seconds: stopped by the neighbours (`strict`), or
	 *  with ripple on, which pushes them along instead (`loose`). Each is asked twice — with the
	 *  clip's linked partners, whose own neighbours limit the edge they share, and without
	 *  (`*Alone`: Alt, which leaves the partners out). */
	strict: TrimBounds;
	loose: TrimBounds;
	strictAlone: TrimBounds;
	looseAlone: TrimBounds;
	origStart: number;
	origEnd: number;
	pos: number; // current ghost position of the dragged edge
	downX: number; // where the pointer went down, px
	grab: number; // pointer's offset from the edge it grabbed (seconds): the edge follows the pointer, not jumps to it
	moved: boolean;
	/** Why the edge is held where it is, when the pointer asked for more than the bounds allow. */
	limit: string | null;
	/** The pointer, in lane space — where a refusal's reason is written. */
	nx: number;
	ny: number;
};

export type TrimGesture = {
	tool: TrimTool;
	trackId: string;
	/** What it edits; null for a roll pressed away from any cut, which is only a click. */
	edit: GestureEdit | null;
	/** The cut's time (roll) or the clip's start (slide): what the pointer is measured from. */
	origin: number;
	/** The pointer's offset from `origin` at the press (seconds): the cut or the clip
	 *  follows the pointer rather than jumping to it. */
	grab: number;
	/** Where the press was on the time axis. */
	pressTime: number;
	downX: number;
	/** How far the view was scrolled at the press, px: a slip is measured in lane space, so
	 *  what autoscroll has moved since counts as pointer travel. */
	downScroll: number;
	/** The pressed clip's length and speed. */
	dur: number;
	speed: number;
	/** What the pointer asks for (rounded once, not yet held to the range). */
	requested: number;
	moved: boolean;
	/** The pointer in lane space — where the readout is written. */
	nx: number;
	ny: number;
};

export type Marquee = {
	from: 'lane' | 'canvas';
	/** Where the drag began and where the pointer is now, in lane space. */
	ax: number;
	ay: number;
	x: number;
	y: number;
	/** The pointer in client px — so a scroll can re-evaluate without a move. */
	cx: number;
	cy: number;
	downX: number;
	downY: number;
	mode: ReturnType<typeof marqueeMode>;
	/** Alt was down at the press: the rectangle selects the clips it touches and no partners. */
	alone: boolean;
	/** The selection as the press found it; every update is computed from this. */
	base: Selection;
	moved: boolean;
};

export type TitleDrag = {
	id: string;
	/** A text title or an animated SVG title: the same gesture, written by a different edit. */
	kind: 'text' | 'svg';
	mode: 'move' | 'l' | 'r';
	grabSec: number;
	origStart: number;
	origEnd: number;
	start: number;
	end: number;
	moved: boolean;
};
