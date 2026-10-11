// How a notice looks by its kind — the glyph and the hue — in one place, so the toast that raises
// it and the line it leaves in the notification log say the same thing. (They once did not: the
// toast drew a circle-check / octagon-x / triangle / info and the log a check / triangle / lightbulb
// / bell, the triangle standing for an error *and* a warning.) The hue's tokens are also written in
// `layout.css` for the toast's own parts (`[data-type] [data-icon]`, its left edge); a test holds the
// two together.

import type { NoticeKind } from './notifications.svelte';

export interface StatusLook {
	/** The `Icon` name; a plain note has the bell in the log and none on a toast. */
	icon: string;
	/** The color of the glyph, and of the toast's edge — the `-400` shade, which holds 4.5:1 on the tinted toast. */
	color: string;
}

export const STATUS_LOOK: Record<NoticeKind, StatusLook> = {
	success: { icon: 'circle-check', color: 'var(--green-400)' },
	error: { icon: 'circle-x', color: 'var(--red-400)' },
	warning: { icon: 'alert-triangle', color: 'var(--orange-400)' },
	info: { icon: 'info', color: 'var(--agent-300)' },
	note: { icon: 'bell', color: 'var(--text-muted)' }
};
