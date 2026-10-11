// How a printed shortcut (`Ctrl+Shift+Z`, a Mac glyph chord, `Shift+Left`, `,`) is split for drawing: the
// modifiers as they are, and the key itself on its own so a punctuation key — which is a speck
// of a glyph beside a letter — can sit in a small keycap and an arrow can be drawn larger.
// The one `.k-kbd` style (`styles/controls.css`) is what menus, context menus and tooltips share.

export interface AccelParts {
	/** The modifiers, with their separator: `Ctrl+Shift+`, the Mac glyphs, or nothing. */
	head: string;
	/** The key. */
	key: string;
	/** A single punctuation key: drawn in a keycap. */
	cap: boolean;
	/** An arrow glyph: drawn a step larger. */
	arrow: boolean;
}

// Ctrl, Option, Shift and Command glyphs; the four arrows.
const MAC_MODIFIERS = '\u2303\u2325\u21e7\u2318';
const ARROWS = '\u2190\u2192\u2191\u2193';

export function accelParts(text: string): AccelParts {
	const s = text.trim();
	let head = '';
	let key = s;
	if (s.length > 1 && s.endsWith('+')) {
		// `Ctrl++` is Ctrl and the key `+`.
		head = s.slice(0, -1);
		key = '+';
	} else if (s.includes('+') && s.length > 1) {
		const at = s.lastIndexOf('+') + 1;
		head = s.slice(0, at);
		key = s.slice(at);
	} else {
		let i = 0;
		while (i < s.length - 1 && MAC_MODIFIERS.includes(s[i])) i++;
		head = s.slice(0, i);
		key = s.slice(i);
	}
	const single = [...key].length === 1;
	const arrow = single && ARROWS.includes(key);
	const cap = single && !arrow && /^[^\p{L}\p{N}]$/u.test(key) && !MAC_MODIFIERS.includes(key);
	return { head, key, cap, arrow };
}
