import { describe, expect, test } from 'bun:test';
import { BLOCK_MIN_CLIPS, CULL_MIN_CLIPS, blockAt, laneDrawing } from './timeline-cull';

/** `n` one-second clips back to back. */
function lane(n: number) {
	return Array.from({ length: n }, (_, i) => ({ id: `c${i}`, timeline_start: i, dur: 1 }));
}
const dur = (c: { dur: number }) => c.dur;
const none = () => false;
const ids = (xs: readonly { id: string }[]) => xs.map((c) => c.id);

describe('laneDrawing', () => {
	test('a lane shorter than the threshold is drawn whole, as buttons', () => {
		const short = lane(CULL_MIN_CLIPS - 1);
		const d = laneDrawing(short, dur, 100, 0, 200, none);
		expect(d.buttons).toBe(short);
		expect(d.blocks).toEqual([]);
	});

	test('a long lane draws the clips that meet the window, in its order', () => {
		// 100 px a second: the window 250..520 px meets clips 2, 3, 4 and 5.
		expect(ids(laneDrawing(lane(200), dur, 100, 250, 520, none).buttons)).toEqual(['c2', 'c3', 'c4', 'c5']);
		// Touching is not meeting: a clip that ends on the window's start, or starts on its end.
		expect(ids(laneDrawing(lane(200), dur, 100, 300, 500, none).buttons)).toEqual(['c3', 'c4']);
	});

	test('a clip too short to see is drawn at its minimum width', () => {
		const clips = [...lane(60), { id: 'blip', timeline_start: 99.99, dur: 0.0001 }];
		// 0.01 px of footage at 100 px/s, drawn 6 px wide from 9999 px: in a window from 10002.
		expect(ids(laneDrawing(clips, dur, 100, 10002, 10100, none).buttons)).toEqual(['blip']);
	});

	test('a clip something holds stays a button however far it is', () => {
		const held = new Set(['c0', 'c150']);
		const d = laneDrawing(lane(200), dur, 100, 5000, 5200, (id) => held.has(id));
		expect(ids(d.buttons)).toEqual(['c0', 'c50', 'c51', 'c150']);
	});

	test('past the block threshold the clips in view are canvas blocks, the held ones buttons', () => {
		// 1 px a second over 2000 one-second clips, all in a 2000 px window: blocks.
		const clips = lane(2000);
		const d = laneDrawing(clips, dur, 1, 0, 2000, (id) => id === 'c7');
		expect(ids(d.buttons)).toEqual(['c7']);
		expect(d.blocks.length).toBe(1999);
		expect(d.blocks.some((c) => c.id === 'c7')).toBe(false);
		// At the threshold exactly it is still buttons.
		const at = laneDrawing(lane(BLOCK_MIN_CLIPS), dur, 1, 0, 2000, none, 1);
		expect([at.buttons.length, at.blocks.length]).toEqual([BLOCK_MIN_CLIPS, 0]);
		const over = laneDrawing(lane(BLOCK_MIN_CLIPS + 1), dur, 1, 0, 2000, none, 1);
		expect([over.buttons.length, over.blocks.length]).toEqual([0, BLOCK_MIN_CLIPS + 1]);
	});

	test('no scale draws everything', () => {
		const long = lane(100);
		expect(laneDrawing(long, dur, 0, 0, 100, none).buttons).toBe(long);
	});
});

describe('blockAt', () => {
	test('finds the block under a lane x, the later one where they overlap', () => {
		const blocks = [
			{ id: 'a', timeline_start: 0, dur: 2 },
			{ id: 'b', timeline_start: 1, dur: 2 },
			{ id: 'tiny', timeline_start: 10, dur: 0.001 }
		];
		expect(blockAt(blocks, dur, 10, 5)?.id).toBe('a');
		expect(blockAt(blocks, dur, 10, 15)?.id).toBe('b');
		expect(blockAt(blocks, dur, 10, 30)).toBeNull();
		// Drawn 6 px wide, and found there.
		expect(blockAt(blocks, dur, 10, 104)?.id).toBe('tiny');
	});
});
