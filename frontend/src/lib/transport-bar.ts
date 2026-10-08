// What the Preview's transport bar shows at a given width.
//
// The go-to-start, play / pause and go-to-end buttons, the timecode and the scrub
// bar are the transport itself and are drawn at every width — they are not in this
// rule. Only the secondary readouts (the duration and the timeline's rate) give way,
// and only on a bar that is *measured* narrow: a width that is not known yet (the
// first frame before layout, a bar inside a hidden dock tab, a zero-size rebuild)
// shows the full bar, since hiding on a missing measurement made the bar's content
// depend on when it was read.

/** Narrower than this the duration and the rate are dropped and the gaps close. */
export const TRANSPORT_FULL_PX = 380;

export interface TransportParts {
	duration: boolean;
	rate: boolean;
	/** Tighter gaps and padding. */
	tight: boolean;
}

/** A usable measurement: a finite, positive width. Anything else is "not known". */
export function measuredWidth(width: number | null | undefined): number | undefined {
	return typeof width === 'number' && Number.isFinite(width) && width > 0 ? width : undefined;
}

export function transportParts(width: number | null | undefined): TransportParts {
	const w = measuredWidth(width);
	const narrow = w !== undefined && w < TRANSPORT_FULL_PX;
	return { duration: !narrow, rate: !narrow, tight: narrow };
}
