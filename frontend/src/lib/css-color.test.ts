import { describe, expect, test } from 'bun:test';
import { cssColorHex } from './css-color';

// The canvas is the browser's; this holds the way it is asked. A stand-in context keeps the one rule a canvas
// has that matters here: a fill style it cannot parse is ignored, and the old one stays.

const KNOWN: Record<string, string> = { teal: '#008080', darkcyan: '#008b8b', black: '#000000', white: '#ffffff', transparent: 'rgba(0, 0, 0, 0)' };

function canvas() {
	let current = KNOWN.black;
	return {
		get fillStyle() {
			return current;
		},
		set fillStyle(value: string | CanvasGradient | CanvasPattern) {
			if (typeof value === 'string' && value in KNOWN) current = KNOWN[value];
		}
	};
}

describe('a color name as a swatch', () => {
	test('a name the browser knows is its #rrggbb, whatever case it is typed in', () => {
		expect(cssColorHex('darkcyan', canvas())).toBe('#008b8b');
		expect(cssColorHex(' Teal ', canvas())).toBe('#008080');
	});

	test('a name it does not know, a color that is not opaque and anything but letters have no swatch', () => {
		for (const other of ['notacolor', 'transparent', '', 'none-ish', 'rgb', '#abc', 'dark cyan', '12']) expect(cssColorHex(other, canvas())).toBeNull();
	});

	test('with no canvas to ask there is no answer', () => {
		expect(cssColorHex('teal', null)).toBeNull();
		expect(cssColorHex('teal')).toBeNull();
	});
});
