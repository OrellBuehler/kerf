// Where the reason an off menu row gives is drawn — pure. It stands beside the panel the row is in, never over a row of
// it, and never over the panel that is open beside it (a submenu and the menu it came from are neighbours: a bubble on
// the neighbour's side would cover the parent row, or the row that opened it).

export interface Span {
	left: number;
	right: number;
}

/** Which side the bubble goes on and where its near edge is: `at` is its left edge for `right`, its right edge for `left`. */
export interface Bubble {
	side: 'right' | 'left';
	at: number;
}

/**
 * Where the bubble for a row of `panel` goes. It prefers the side *away from* the open sibling panel (the submenu when
 * the row is in the menu, the menu when it is in the submenu), hung from the panel's own edge. With no room there for
 * `width` px — a menu against the window's right edge whose submenu opened to its left — it goes beyond the sibling, on
 * the same side, so that it still covers neither; and with no sibling it takes the other side. Failing all of them, the
 * side with the most room.
 */
export function bubbleFor(panel: Span, sibling: Span | null, viewWidth: number, width = 264, gap = 6): Bubble {
	const away: 'right' | 'left' = sibling && sibling.left > panel.left ? 'left' : 'right';
	const roomAt = (side: 'right' | 'left', edge: number) => (side === 'right' ? viewWidth - edge - gap : edge - gap);
	const hang = (side: 'right' | 'left', edge: number): Bubble => ({ side, at: side === 'right' ? edge + gap : edge - gap });
	const own = away === 'right' ? panel.right : panel.left;
	if (roomAt(away, own) >= width) return hang(away, own);
	if (sibling) {
		// Beyond the neighbour on the side that was too tight: past it, there may be room again.
		const past = away === 'right' ? Math.max(panel.right, sibling.right) : Math.min(panel.left, sibling.left);
		if (roomAt(away, past) >= width) return hang(away, past);
		// The other way round, past the sibling that sits there: also away from it.
		const opposite = away === 'right' ? 'left' : 'right';
		const beyond = opposite === 'right' ? Math.max(panel.right, sibling.right) : Math.min(panel.left, sibling.left);
		if (roomAt(opposite, beyond) >= width) return hang(opposite, beyond);
	} else {
		const opposite = away === 'right' ? 'left' : 'right';
		const edge = opposite === 'right' ? panel.right : panel.left;
		if (roomAt(opposite, edge) >= width) return hang(opposite, edge);
	}
	const left = roomAt('left', panel.left);
	const right = roomAt('right', panel.right);
	return right >= left ? hang('right', panel.right) : hang('left', panel.left);
}
