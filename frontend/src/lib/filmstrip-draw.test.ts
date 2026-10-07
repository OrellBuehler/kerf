import { describe, expect, test } from 'bun:test';
import { drawFilmstrip, type FilmCtx } from './filmstrip-draw';
import type { SlotDraw } from './filmstrip-view';

type Call = { image: unknown; sx: number; sy: number; sw: number; sh: number; dx: number; dy: number; dw: number; dh: number };

/** A recording context. */
function recorder() {
	const calls: Call[] = [];
	const events: string[] = [];
	const ctx: FilmCtx = {
		imageSmoothingEnabled: false,
		imageSmoothingQuality: 'low',
		clearRect(x, y, w, h) {
			events.push(`clear ${x},${y},${w},${h}`);
		},
		drawImage(image, sx, sy, sw, sh, dx, dy, dw, dh) {
			events.push('draw');
			calls.push({ image, sx, sy, sw, sh, dx, dy, dw, dh });
		}
	};
	return { ctx, calls, events };
}

const sheetA = { name: 'sheet A' } as unknown as CanvasImageSource;
const sheetB = { name: 'sheet B' } as unknown as CanvasImageSource;
const sheets = [sheetA, sheetB];
const strip = { frame_width: 170, frame_height: 96 };
const slot = (i: number, dx: number, dw: number, sheet = 0, sx = 0, frame = 0): SlotDraw => ({ slot: i, frame, sheet, sx, dx, dw });

describe('drawFilmstrip', () => {
	test('clears first, then blits each slot\'s thumbnail into its rectangle', () => {
		const { ctx, calls, events } = recorder();
		const n = drawFilmstrip(ctx, [slot(0, 0, 96, 0, 0), slot(1, 96, 96, 1, 340)], {
			width: 192,
			height: 108,
			strip,
			sheet: (i) => sheets[i]
		});
		expect(n).toBe(2);
		expect(events[0]).toBe('clear 0,0,192,108');
		expect(calls).toEqual([
			{ image: sheetA, sx: 0, sy: 0, sw: 170, sh: 96, dx: 0, dy: 0, dw: 96, dh: 108 },
			{ image: sheetB, sx: 340, sy: 0, sw: 170, sh: 96, dx: 96, dy: 0, dw: 96, dh: 108 }
		]);
	});

	test('smooths the downscale', () => {
		const { ctx } = recorder();
		drawFilmstrip(ctx, [], { width: 10, height: 10, strip, sheet: () => undefined });
		expect(ctx.imageSmoothingEnabled).toBe(true);
		expect(ctx.imageSmoothingQuality).toBe('high');
	});

	test('a slot that runs past either edge is still drawn whole (the canvas crops it)', () => {
		const { ctx, calls } = recorder();
		drawFilmstrip(ctx, [slot(0, -40, 96), slot(1, 56, 96), slot(2, 152, 96)], {
			width: 200,
			height: 54,
			strip,
			sheet: () => sheetA
		});
		expect(calls.map((c) => [c.dx, c.dw])).toEqual([
			[-40, 96],
			[56, 96],
			[152, 96]
		]);
	});

	test('slots wholly off the canvas, empty ones, and ones with no sheet are skipped', () => {
		const { ctx, calls } = recorder();
		const n = drawFilmstrip(ctx, [slot(0, -200, 96), slot(1, 300, 96), slot(2, 10, 0), slot(3, 10, 40, 5)], {
			width: 200,
			height: 54,
			strip,
			sheet: (i) => sheets[i]
		});
		expect(n).toBe(0);
		expect(calls).toEqual([]);
	});

	test('an empty plan still clears the canvas', () => {
		const { ctx, events } = recorder();
		expect(drawFilmstrip(ctx, [], { width: 50, height: 20, strip, sheet: () => sheetA })).toBe(0);
		expect(events).toEqual(['clear 0,0,50,20']);
	});
});
