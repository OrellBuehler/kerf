import { describe, expect, test } from 'bun:test';
import { frameAt, locate, planFilmstrip, stripGeometry, timeOf, type StripGeometry } from './filmstrip-geometry';
import {
	MIN_FILM_HEIGHT,
	MIN_SLOT_PX,
	filmVisible,
	planSlots,
	slotRange,
	slotTime,
	slotWidth,
	type PlanArgs
} from './filmstrip-view';
import type { StreamInfo } from './types';

const video = (width: number, height: number, image = false): StreamInfo => ({
	index: 0,
	kind: 'video',
	codec: image ? 'png' : 'h264',
	width,
	height,
	image
});

/** A real-shaped strip: what the backend delivers for an asset. */
const stripOf = (duration: number, w = 1920, h = 1080, image = false): StripGeometry =>
	stripGeometry(planFilmstrip({ id: 'a', duration, streams: [video(w, h, image)] }));

const clip = (source_in: number, source_out: number, speed?: number) => ({ source_in, source_out, speed });

describe('what shows thumbnails', () => {
	test('a clip box needs 28 px', () => {
		expect(MIN_FILM_HEIGHT).toBe(28);
		expect(filmVisible(27.9)).toBe(false);
		expect(filmVisible(28)).toBe(true);
		expect(filmVisible(102)).toBe(true);
		expect(filmVisible(0)).toBe(false);
		expect(filmVisible(NaN)).toBe(false);
	});
});

describe('slots', () => {
	test('a slot is the thumbnail\'s aspect at the clip height, in whole pixels', () => {
		const s = stripOf(60); // 170 x 96
		expect(s.frame_width).toBe(170);
		expect(slotWidth(s, 96)).toBe(170);
		expect(slotWidth(s, 54)).toBe(96); // 170 / 96 * 54 = 95.6
		expect(slotWidth(s, 102)).toBe(181);
		// a vertical video: 54 x 96
		expect(slotWidth(stripOf(60, 1080, 1920), 54)).toBe(30);
	});

	test('a degenerate strip does not become slivers', () => {
		expect(slotWidth({ frame_width: 2, frame_height: 96 }, 28)).toBe(MIN_SLOT_PX);
		expect(slotWidth({ frame_width: 170, frame_height: 0 }, 54)).toBeGreaterThanOrEqual(MIN_SLOT_PX);
	});

	test('slotRange is the slots a pixel range touches', () => {
		expect(slotRange(0, 100, 96)).toEqual({ first: 0, last: 1 });
		expect(slotRange(0, 96, 96)).toEqual({ first: 0, last: 0 }); // ends exactly on the seam
		expect(slotRange(96, 97, 96)).toEqual({ first: 1, last: 1 });
		expect(slotRange(500, 700, 96)).toEqual({ first: 5, last: 7 });
		expect(slotRange(5, 5, 96)).toBeNull();
		expect(slotRange(10, 5, 96)).toBeNull();
		expect(slotRange(0, 10, 0)).toBeNull();
	});
});

describe('the time a slot shows', () => {
	// 10 px/s, 100 px slots: slot i covers clip seconds [10i, 10i + 10).
	const slot = 100;
	const pps = 10;

	test('forward: the time under the middle of the slot, through source_in', () => {
		const c = clip(20, 70);
		expect(slotTime(c, 0, slot, 500, pps)).toBe(25); // 20 + 5
		expect(slotTime(c, 1, slot, 500, pps)).toBe(35);
		expect(slotTime(c, 4, slot, 500, pps)).toBe(65);
	});

	test('speed scales source seconds per pixel', () => {
		const c = clip(0, 100, 2);
		expect(slotTime(c, 0, slot, 500, pps)).toBe(10); // 5 s of clip is 10 s of source
		expect(slotTime(c, 1, slot, 500, pps)).toBe(30);
		const slow = clip(0, 10, 0.5); // half speed: 5 s of clip is 2.5 s of source
		expect(slotTime(slow, 0, slot, 200, pps)).toBe(2.5);
	});

	test('reversed: counts down from source_out, so the slots run backwards', () => {
		const c = clip(20, 70, -1);
		expect(slotTime(c, 0, slot, 500, pps)).toBe(65);
		expect(slotTime(c, 1, slot, 500, pps)).toBe(55);
		expect(slotTime(c, 4, slot, 500, pps)).toBe(25);
		// and at speed: reversed 2x
		const fast = clip(0, 100, -2);
		expect(slotTime(fast, 0, slot, 500, pps)).toBe(90);
		expect(slotTime(fast, 1, slot, 500, pps)).toBe(70);
	});

	test('the last slot, cut by the clip\'s edge, shows the middle of what is left of it', () => {
		const c = clip(0, 25); // 250 px: slot 2 is [200, 300) but only 50 px of it is clip
		expect(slotTime(c, 2, slot, 250, pps)).toBe(22.5); // centre of 200..250 = 225 px = 22.5 s
		// a clip narrower than one slot is one slot, centred on the clip
		expect(slotTime(clip(0, 4), 0, slot, 40, pps)).toBe(2);
	});

	test('never reads past the clip\'s source window', () => {
		const c = clip(10, 12);
		for (const i of [0, 1, 5, 50]) {
			const t = slotTime(c, i, slot, 20, pps);
			expect(t).toBeGreaterThanOrEqual(10);
			expect(t).toBeLessThanOrEqual(12);
		}
		const r = clip(10, 12, -1);
		for (const i of [0, 1, 5, 50]) {
			const t = slotTime(r, i, slot, 20, pps);
			expect(t).toBeGreaterThanOrEqual(10);
			expect(t).toBeLessThanOrEqual(12);
		}
	});
});

describe('planSlots', () => {
	const base = (over: Partial<PlanArgs> = {}): PlanArgs => ({
		strip: stripOf(120),
		clip: clip(0, 120),
		pxPerSec: 36,
		clipWidth: 120 * 36,
		heightPx: 54,
		x0: 0,
		columns: 960,
		scale: 1,
		...over
	});

	test('each slot is a thumbnail, found on its sheet the way the backend lays it out', () => {
		const a = base();
		const plan = planSlots(a);
		expect(plan.length).toBe(10); // 960 px / 96 px
		const strip = stripOf(120); // the strip `base` plans against, with its `columns`
		for (const s of plan) {
			const t = slotTime(a.clip, s.slot, 96, a.clipWidth, a.pxPerSec);
			expect(s.frame).toBe(frameAt(strip, t));
			const where = locate(strip, s.frame)!;
			expect(s.sx).toBe(where.x);
			expect(s.sheet).toBe(strip.sheets.indexOf(where.sheet));
			expect(s.sheet).toBe(Math.floor(s.frame / strip.columns));
			expect(s.sx).toBe((s.frame % strip.columns) * strip.frame_width);
		}
		// 0.5 s thumbnails, 96 px slots at 36 px/s = 2.67 s a slot: the frame ahead of each is later
		const frames = plan.map((s) => s.frame);
		expect(frames).toEqual([...frames].sort((x, y) => x - y));
		expect(new Set(frames).size).toBe(frames.length);
	});

	test('slots abut exactly — no seam, no overlap — at any pixel ratio and offset', () => {
		for (const scale of [1, 1.25, 1.5, 2]) {
			for (const x0 of [0, 37, 250, 1000]) {
				const plan = planSlots(base({ x0, scale, columns: Math.round(900 * scale) }));
				expect(plan.length).toBeGreaterThan(1);
				for (let i = 1; i < plan.length; i++) {
					expect(plan[i].dx).toBe(plan[i - 1].dx + plan[i - 1].dw);
					expect(plan[i].dw).toBeGreaterThan(0);
				}
				// the slots cover the canvas, edge to edge
				expect(plan[0].dx).toBeLessThanOrEqual(0);
				const last = plan[plan.length - 1];
				expect(last.dx + last.dw).toBeGreaterThanOrEqual(Math.round(900 * scale));
			}
		}
	});

	test('a canvas that starts mid-slot begins with that slot, shifted left of the canvas', () => {
		const plan = planSlots(base({ x0: 100, columns: 400 }));
		expect(plan[0].slot).toBe(1); // 96..192
		expect(plan[0].dx).toBe(96 - 100);
		expect(plan[0].dw).toBe(96);
	});

	test('scrolling does not change which thumbnail a slot shows', () => {
		const whole = planSlots(base({ columns: 2000 }));
		const part = planSlots(base({ x0: 480, columns: 500 }));
		for (const s of part) expect(s.frame).toBe(whole.find((w) => w.slot === s.slot)!.frame);
	});

	test('a clip trimmed from the middle of its footage shows that footage', () => {
		const a = base({ clip: clip(60, 90), clipWidth: 30 * 36 });
		const plan = planSlots(a);
		const first = plan[0];
		// slot 0 is 96 px = 2.67 s from 60 s: its centre is at 61.33 s
		expect(timeOf(a.strip, first.frame)).toBeCloseTo(61.5, 5); // the nearest 0.5 s thumbnail
	});

	test('a reversed clip shows its footage backwards, not flipped', () => {
		const fwd = planSlots(base({ clip: clip(0, 60, 1), clipWidth: 60 * 36, columns: 960 }));
		const rev = planSlots(base({ clip: clip(0, 60, -1), clipWidth: 60 * 36, columns: 960 }));
		expect(rev.map((s) => s.frame)).toEqual([...rev.map((s) => s.frame)].sort((x, y) => y - x)); // descending
		expect(fwd.map((s) => s.frame)).toEqual([...fwd.map((s) => s.frame)].sort((x, y) => x - y));
		// the first slot of the reversed clip is the footage's end; the last is near its start
		expect(timeOf(stripOf(120), rev[0].frame)).toBeGreaterThan(55);
		expect(timeOf(stripOf(120), rev[rev.length - 1].frame)).toBeLessThan(35);
	});

	test('a sped-up clip steps through the footage faster', () => {
		const one = planSlots(base({ clip: clip(0, 120, 1), columns: 480 }));
		const two = planSlots(base({ clip: clip(0, 120, 2), columns: 480 }));
		const step = (p: typeof one) => p[p.length - 1].frame - p[0].frame;
		expect(step(two)).toBeGreaterThan(step(one) * 1.8);
	});

	test('a still is its one thumbnail, repeated', () => {
		const strip = stripOf(5, 1920, 1080, true);
		expect(strip.frames).toBe(1);
		const plan = planSlots(base({ strip, clip: clip(0, 5), clipWidth: 180, columns: 480 }));
		expect(plan.length).toBeGreaterThan(3);
		for (const s of plan) expect(s).toMatchObject({ frame: 0, sheet: 0, sx: 0 });
	});

	test('the slot grid follows the height: taller clips, wider slots, fewer of them', () => {
		const short = planSlots(base({ heightPx: 30, columns: 960 }));
		const tall = planSlots(base({ heightPx: 102, columns: 960 }));
		expect(short.length).toBeGreaterThan(tall.length);
		expect(tall[0].dw).toBe(181);
	});

	test('a thumbnail the strip does not hold is skipped, not drawn wrong', () => {
		const strip = stripOf(120);
		const short: StripGeometry = { ...strip, sheets: strip.sheets.slice(0, 1) }; // only the first sheet came back
		const plan = planSlots(base({ strip: short, columns: 4000 }));
		expect(plan.length).toBeGreaterThan(0);
		const held = short.sheets[0];
		for (const s of plan) expect(s.frame).toBeLessThan(held.first_frame + held.count);
		const all = planSlots(base({ columns: 4000 }));
		expect(plan.length).toBeLessThan(all.length); // the slots over the missing sheets are left clear
	});

	test('an empty canvas plans nothing', () => {
		expect(planSlots(base({ columns: 0 }))).toEqual([]);
	});

	test('every slot of a long clip at a deep zoom still picks a thumbnail inside the strip', () => {
		const strip = stripOf(3600); // 15 s thumbnails
		for (const pxPerSec of [0.5, 36, 400, 2000]) {
			const plan = planSlots({
				strip,
				clip: clip(0, 3600),
				pxPerSec,
				clipWidth: 3600 * pxPerSec,
				heightPx: 54,
				x0: 20_000,
				columns: 3000,
				scale: 2
			});
			for (const s of plan) {
				expect(s.frame).toBeGreaterThanOrEqual(0);
				expect(s.frame).toBeLessThan(strip.frames);
			}
		}
	});
});
