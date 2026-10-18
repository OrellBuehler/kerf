import { describe, expect, test } from 'bun:test';
import {
	FALLBACK_INK,
	boxPadding,
	containRect,
	retimeKeyframes,
	dragPosition,
	inkBox,
	inkFromContext,
	isVisibleAt,
	packRows,
	sampleOverlay,
	scaledSize,
	snapSpanStart,
	snapTime,
	titleFamily,
	trimSpan,
	withKeyframeAt
} from './titles';
import type { TextOverlay } from './types';

function overlay(patch: Partial<TextOverlay> = {}): TextOverlay {
	return { id: 'o', text: 'Brasil 2025', start: 2, end: 6, pos_x: 0.5, pos_y: 0.5, size: 0.06, color: 'white', bold: false, ...patch };
}

describe('sampleOverlay', () => {
	test('a static title is where it says and fully opaque', () => {
		expect(sampleOverlay(overlay({ pos_x: 0.2, pos_y: 0.8 }), 3)).toEqual({ x: 0.2, y: 0.8, opacity: 1 });
	});

	test('interpolates keyframes over the overlay-local clock', () => {
		const o = overlay({
			keyframes: [
				{ time: 0, pos_x: 0.2, pos_y: 0.5, opacity: 0 },
				{ time: 2, pos_x: 0.6, pos_y: 0.5, opacity: 1 }
			]
		});
		const s = sampleOverlay(o, 3);
		expect(s.x).toBeCloseTo(0.4);
		expect(s.opacity).toBeCloseTo(0.5);
		expect(sampleOverlay(o, 99).x).toBeCloseTo(0.6);
	});
});

describe('dragPosition', () => {
	test('converts pixels to frame fractions', () => {
		expect(dragPosition({ x: 0.5, y: 0.5 }, 50, -100, 200, 400)).toEqual({ x: 0.75, y: 0.25 });
	});

	test('keeps the centre inside the frame', () => {
		expect(dragPosition({ x: 0.9, y: 0.1 }, 500, -500, 100, 100)).toEqual({ x: 1, y: 0 });
	});

	test('survives a frame with no size yet', () => {
		expect(dragPosition({ x: 0.3, y: 0.3 }, 10, 10, 0, 0)).toEqual({ x: 0.3, y: 0.3 });
	});
});

describe('scaledSize', () => {
	test('scales with the distance from the centre', () => {
		expect(scaledSize(0.06, 100, 150)).toBeCloseTo(0.09);
		expect(scaledSize(0.06, 100, 50)).toBeCloseTo(0.03);
	});

	test('clamps, but never below where the title already is', () => {
		expect(scaledSize(0.06, 100, 1)).toBe(0.02);
		expect(scaledSize(0.06, 100, 9000)).toBe(0.3);
		expect(scaledSize(0.4, 100, 100)).toBe(0.4);
	});

	test('a grab on the centre changes nothing', () => {
		expect(scaledSize(0.06, 0, 80)).toBe(0.06);
	});
});

describe('withKeyframeAt', () => {
	const animated = overlay({
		keyframes: [
			{ time: 0, pos_x: 0.2, pos_y: 0.5, opacity: 0 },
			{ time: 2, pos_x: 0.6, pos_y: 0.5, opacity: 1 }
		]
	});

	test('adds a keyframe at the playhead carrying the opacity in force', () => {
		const ks = withKeyframeAt(animated, 3, 0.9, 0.1);
		expect(ks.map((k) => k.time)).toEqual([0, 1, 2]);
		expect(ks[1]).toMatchObject({ pos_x: 0.9, pos_y: 0.1 });
		expect(ks[1].opacity).toBeCloseTo(0.5);
	});

	test('moves a keyframe the playhead is already on instead of stacking a second', () => {
		const ks = withKeyframeAt(animated, 4, 0.9, 0.9);
		expect(ks).toHaveLength(2);
		expect(ks[1]).toEqual({ time: 2, pos_x: 0.9, pos_y: 0.9, opacity: 1 });
	});
});

describe('packRows', () => {
	test('overlapping titles stack, sequential ones share a row', () => {
		const a = overlay({ id: 'a', start: 0, end: 2 });
		const b = overlay({ id: 'b', start: 2, end: 4 });
		const c = overlay({ id: 'c', start: 1, end: 3 });
		const rows = packRows([a, b, c]);
		expect(rows.get('a')).toBe(0);
		expect(rows.get('b')).toBe(0);
		expect(rows.get('c')).toBe(1);
	});
});

describe('snapping', () => {
	test('snapTime takes the nearest candidate within the threshold', () => {
		expect(snapTime(1.04, [0, 1, 2], 0.1)).toBe(1);
		expect(snapTime(1.5, [0, 1, 2], 0.1)).toBe(1.5);
	});

	test('snapSpanStart lands either edge of the span', () => {
		expect(snapSpanStart(0.95, 2, [1], 0.1)).toBe(1);
		expect(snapSpanStart(3.02, 2, [5], 0.1)).toBe(3);
		expect(snapSpanStart(-3, 2, [], 0.1)).toBe(0);
	});
});

describe('trimSpan', () => {
	test('keeps a minimum length and never goes before zero', () => {
		expect(trimSpan(2, 6, 'l', 9)).toEqual({ start: 5.9, end: 6 });
		expect(trimSpan(2, 6, 'l', -4)).toEqual({ start: 0, end: 6 });
		expect(trimSpan(2, 6, 'r', 1)).toEqual({ start: 2, end: 2.1 });
	});
});

describe('boxPadding', () => {
	test('only a title with a box colour is padded', () => {
		expect(boxPadding(overlay(), 1080)).toBe(0);
		expect(boxPadding(overlay({ bg: 'black@0.5' }), 1200)).toBeCloseTo(0.01);
	});
});

describe('containRect', () => {
	test('a wide picture in a tall frame is letterboxed top and bottom', () => {
		const r = containRect(16 / 9, 9 / 16);
		expect(r.width).toBe(100);
		expect(r.height).toBeCloseTo(31.64, 1);
		expect(r.top).toBeCloseTo((100 - r.height) / 2);
	});

	test('a matching picture fills the frame', () => {
		expect(containRect(1, 1)).toEqual({ left: 0, top: 0, width: 100, height: 100 });
	});

	test('a tall picture in a wide frame is pillarboxed', () => {
		const r = containRect(9 / 16, 16 / 9);
		expect(r.height).toBe(100);
		expect(r.left).toBeCloseTo((100 - r.width) / 2);
	});
});

describe('retimeKeyframes', () => {
	const fade = [
		{ time: 0, pos_x: 0.5, pos_y: 0.4, opacity: 0 },
		{ time: 0.5, pos_x: 0.5, pos_y: 0.4, opacity: 1 },
		{ time: 2.5, pos_x: 0.5, pos_y: 0.4, opacity: 1 },
		{ time: 3, pos_x: 0.5, pos_y: 0.4, opacity: 0 }
	];

	test('shortening keeps the fade-out on the new end', () => {
		expect(retimeKeyframes(fade, 3, 2).map((k) => k.time)).toEqual([0, 0.5, 1.5, 2]);
	});

	test('lengthening moves the fade-out out with the end', () => {
		expect(retimeKeyframes(fade, 3, 5).map((k) => k.time)).toEqual([0, 0.5, 4.5, 5]);
	});

	test('an unchanged length is left alone', () => {
		expect(retimeKeyframes(fade, 3, 3)).toEqual(fade);
	});

	test('a title cut shorter than its fades keeps the keyframes in order inside it', () => {
		const times = retimeKeyframes(fade, 3, 0.4).map((k) => k.time);
		expect(times).toEqual([...times].sort((a, b) => a - b));
		expect(Math.max(...times)).toBeLessThanOrEqual(0.5);
		expect(times.at(-1)).toBeCloseTo(0.4);
	});
});

describe('the title handle is the ink of the text, as drawtext boxes it', () => {
	// What `drawtext` boxed at fontsize 100 with the shipped Geist (measured): the height of the string's
	// ink, not the font's 1.30 line height. The canvas reports the same ink; these are its numbers for it.
	const GEIST = { fontAscent: 1.005, fontDescent: 0.295 };
	const ink = (ascent: number, descent: number) => () => ({ ascent, descent, ...GEIST });

	test('the height is the ink above plus below the baseline, not the line height', () => {
		expect(inkBox(overlay({ text: 'H' }), ink(0.71, 0)).height).toBeCloseTo(0.71);
		expect(inkBox(overlay({ text: 'x' }), ink(0.53, 0)).height).toBeCloseTo(0.53);
		expect(inkBox(overlay({ text: 'Hxg' }), ink(0.71, 0.16)).height).toBeCloseTo(0.87);
		expect(inkBox(overlay({ text: '.' }), ink(0.11, 0)).height).toBeCloseTo(0.11);
		for (const text of ['H', 'x', 'Hxg']) {
			expect(inkBox(overlay({ text }), ink(0.7, 0.1)).height).toBeLessThan(GEIST.fontAscent + GEIST.fontDescent);
		}
	});

	test('the glyphs move from the line box centre to the ink centre', () => {
		// Caps only: the ink centre (0.355 above the baseline) is where Geist's own content centre is.
		expect(inkBox(overlay(), ink(0.71, 0)).shift).toBeCloseTo(0);
		// A descender moves the ink centre down towards the baseline, so the glyphs go up (negative) to be centred as the engine centres them.
		expect(inkBox(overlay(), ink(0.71, 0.2)).shift).toBeCloseTo(0.255 - 0.355);
		// Without the font's own metrics the line box is taken to be the ink.
		expect(inkBox(overlay(), () => ({ ascent: 0.7, descent: 0.2 })).shift).toBeCloseTo(0);
	});

	test('what cannot be measured is the fallback height', () => {
		expect(inkBox(overlay(), () => null)).toEqual({ height: FALLBACK_INK, shift: 0 });
		expect(inkBox(overlay({ text: '' }), ink(0, 0))).toEqual({ height: FALLBACK_INK, shift: 0 });
		expect(FALLBACK_INK).toBeCloseTo(0.9);
		// No canvas in bun: the default measure answers null, so the handle still has a height.
		expect(inkBox(overlay())).toEqual({ height: FALLBACK_INK, shift: 0 });
	});

	test('it is measured in the family the engine draws, bold or not', () => {
		const seen: [string, boolean, string][] = [];
		const spy = (text: string, bold: boolean, family: string) => {
			seen.push([text, bold, family]);
			return null;
		};
		inkBox(overlay({ text: 'Hi', bold: true }), spy);
		inkBox(overlay({ text: 'Hi', font: "Arial 'x" }), spy);
		expect(seen).toEqual([
			['Hi', true, "'Geist Variable', sans-serif"],
			['Hi', false, "'Arial x', 'Geist Variable', sans-serif"]
		]);
		expect(titleFamily({})).toBe("'Geist Variable', sans-serif");
	});

	test('a 2D context is read at 100 px, in em, and one that cannot say is null', () => {
		const fonts: string[] = [];
		const ctx = {
			set font(v: string) {
				fonts.push(v);
			},
			measureText: () => ({
				actualBoundingBoxAscent: 71,
				actualBoundingBoxDescent: 16,
				fontBoundingBoxAscent: 100.5,
				fontBoundingBoxDescent: 29.5
			})
		};
		expect(inkFromContext(ctx as never, 'Hxg', true, "'Geist Variable'")).toEqual({
			ascent: 0.71,
			descent: 0.16,
			fontAscent: 1.005,
			fontDescent: 0.295
		});
		expect(fonts).toEqual(["700 100px 'Geist Variable'"]);
		const old = { font: '', measureText: () => ({ actualBoundingBoxAscent: 71, actualBoundingBoxDescent: 16 }) };
		expect(inkFromContext(old as never, 'x', false, 'sans-serif')).toEqual({
			ascent: 0.71,
			descent: 0.16,
			fontAscent: undefined,
			fontDescent: undefined
		});
		const none = { font: '', measureText: () => ({}) };
		expect(inkFromContext(none as never, 'x', false, 'sans-serif')).toBeNull();
	});
});

describe('the title box in the preview', () => {
	test('is sized by its line height, so the box padding adds to the ink as boxborderw does', async () => {
		const { readFileSync } = await import('node:fs');
		const src = readFileSync(new URL('./components/editor/Preview.svelte', import.meta.url), 'utf8');
		const style = src.slice(src.indexOf('\t\t\t\t\t\t\t\tdata-title-box\n'), src.indexOf('</span>', src.indexOf('\t\t\t\t\t\t\t\tdata-title-box\n')));
		expect(style).toContain('line-height:{ink.height}');
		expect(style).not.toMatch(/[;"]height:/);
	});
});

describe('packRows over text and animated titles', () => {
	const svg = (id: string, start: number, end: number) => ({ id, start, end, name: '', svg: '', x: 0, y: 0, width: 1, height: 1, opacity: 1 });

	test('takes anything with an id and a span, and packs the two kinds together', () => {
		const text = overlay({ id: 't', start: 0, end: 4 });
		const rows = packRows([text, svg('s1', 1, 3), svg('s2', 4, 6), svg('s3', 2, 5)]);
		expect(rows.get('t')).toBe(0);
		expect(rows.get('s1')).toBe(1);
		expect(rows.get('s2')).toBe(0);
		expect(rows.get('s3')).toBe(2);
	});

	test('items that tie keep the order they were given in', () => {
		const rows = packRows([overlay({ id: 'text', start: 1, end: 2 }), svg('svg', 1, 2)]);
		expect(rows.get('text')).toBe(0);
		expect(rows.get('svg')).toBe(1);
		const flipped = packRows([svg('svg', 1, 2), overlay({ id: 'text', start: 1, end: 2 })]);
		expect(flipped.get('svg')).toBe(0);
		expect(flipped.get('text')).toBe(1);
	});

	test('does not reorder or touch what it is given', () => {
		const items = [svg('b', 3, 4), svg('a', 0, 1)];
		packRows(items);
		expect(items.map((i) => i.id)).toEqual(['b', 'a']);
	});

	test('an empty lane has no rows', () => {
		expect(packRows([]).size).toBe(0);
	});

	test('isVisibleAt takes a span, as the engine draws between start and end inclusive', () => {
		expect(isVisibleAt(svg('s', 1, 2), 1)).toBe(true);
		expect(isVisibleAt(svg('s', 1, 2), 2)).toBe(true);
		expect(isVisibleAt(svg('s', 1, 2), 2.01)).toBe(false);
	});
});
