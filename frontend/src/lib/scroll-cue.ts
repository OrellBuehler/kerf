// Whether a scrolling area has more under its bottom edge: the cue (a fade and a chevron) that says "there is
// more" where a pinned footer would otherwise make the end of the content look like the end of the panel — the
// Deliver panel at 1366×768 cut its Thumbnails off with nothing to say so. Pure so the arithmetic is tested.

export interface Scrolled {
	scrollTop: number;
	clientHeight: number;
	scrollHeight: number;
}

/** Content is hidden under the bottom edge. `slack` px of it do not count: a sub-pixel remainder is the end. */
export function moreBelow(el: Scrolled, slack = 2): boolean {
	return el.scrollHeight - el.clientHeight > slack && el.scrollTop + el.clientHeight < el.scrollHeight - slack;
}
