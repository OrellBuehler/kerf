import { describe, expect, test } from 'bun:test';
import {
	BLOCK_MAX_PX,
	CULL_MIN_CLIPS,
	blockAt,
	blockMark,
	clipBox,
	clipPadding,
	laneDrawing,
	mayPromote,
	pinnedClips,
	pressOnEmpty
} from './timeline-cull';

/** `n` one-second clips back to back. */
function lane(n: number) {
	return Array.from({ length: n }, (_, i) => ({ id: `c${i}`, timeline_start: i, dur: 1 }));
}
const dur = (c: { dur: number }) => c.dur;
const none: ReadonlySet<string> = new Set();
const ids = (xs: readonly { id: string }[]) => xs.map((c) => c.id);

describe('laneDrawing', () => {
	test('a lane shorter than the threshold is drawn whole, as buttons', () => {
		const short = lane(CULL_MIN_CLIPS - 1);
		const d = laneDrawing(short, dur, 1, 0, 200, none);
		expect(d.buttons).toBe(short);
		expect(d.blocks).toEqual([]);
	});

	test('a long lane draws the clips that meet the window, in its order', () => {
		// 100 px a second: the window 250..520 px meets clips 2, 3, 4 and 5.
		expect(ids(laneDrawing(lane(200), dur, 100, 250, 520, none).buttons)).toEqual(['c2', 'c3', 'c4', 'c5']);
		// Touching is not meeting: a clip that ends on the window's start, or starts on its end.
		expect(ids(laneDrawing(lane(200), dur, 100, 300, 500, none).buttons)).toEqual(['c3', 'c4']);
	});

	test('a clip too short to see is drawn at its minimum width, as a block', () => {
		const clips = [...lane(60), { id: 'blip', timeline_start: 99.99, dur: 0.0001 }];
		// 0.01 px of footage at 100 px/s, drawn 6 px wide from 9999 px: in a window from 10002.
		const d = laneDrawing(clips, dur, 100, 10002, 10100, none);
		expect([ids(d.buttons), ids(d.blocks)]).toEqual([[], ['blip']]);
	});

	test('the pinned clips are buttons wherever they are, and nothing else held off screen is', () => {
		const d = laneDrawing(lane(200), dur, 100, 5000, 5200, new Set(['c0', 'c150']));
		expect(ids(d.buttons)).toEqual(['c0', 'c50', 'c51', 'c150']);
		// A select-all over a long cut at a reading zoom mounts what is in view, not the cut.
		const all = laneDrawing(lane(1000), dur, 100, 5000, 5200, none);
		expect(all.buttons.length + all.blocks.length).toBe(2);
	});

	test('a clip is a block by its width, not by how many are in view', () => {
		// 11 px a second: every clip under the block width, all in view.
		const narrow = laneDrawing(lane(2000), dur, BLOCK_MAX_PX - 1, 0, 30000, new Set(['c7']));
		expect(ids(narrow.buttons)).toEqual(['c7']);
		expect(narrow.blocks.length).toBe(1999);
		// At the block width they are clips again, however many: at 53 px/s on a 2560 px window, ~50.
		const readable = laneDrawing(lane(2000), dur, 53, 0, 2560, none);
		expect([readable.buttons.length, readable.blocks.length]).toEqual([49, 0]);
		// One wide clip among narrow neighbours stays a clip.
		const mixed = [...lane(100), { id: 'wide', timeline_start: 100, dur: 3 }];
		const d = laneDrawing(mixed, dur, 5, 0, 1000, none);
		expect(ids(d.buttons)).toEqual(['wide']);
		expect(d.blocks.length).toBe(100);
	});

	test('no scale draws everything', () => {
		const long = lane(100);
		expect(laneDrawing(long, dur, 0, 0, 100, none).buttons).toBe(long);
	});
});

describe('pinnedClips', () => {
	test('the pressed, hovered, focused and nudged clips, and nothing that is absent', () => {
		expect([...pinnedClips({ pressed: ['a', null, undefined, 'b'], hovered: 'c', focused: null, nudged: 'a' })].sort()).toEqual([
			'a',
			'b',
			'c'
		]);
		expect(pinnedClips({}).size).toBe(0);
	});
});

describe('blockMark', () => {
	const state = {
		selected: new Set(['s']),
		diff: new Map([
			['s', 'added'],
			['a', 'added'],
			['h', 'shifted']
		]),
		touched: new Set(['t', 'a']),
		lit: new Set(['l', 't'])
	};
	test('a block shows what its button would, the selection first', () => {
		expect(['s', 'a', 'h', 't', 'l', 'x'].map((id) => blockMark(id, state))).toEqual([
			'selected',
			'added',
			'shifted',
			'touched',
			'lit',
			null
		]);
		expect(blockMark('a', { selected: new Set(), lit: new Set() })).toBeNull();
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

describe('pressOnEmpty', () => {
	const laneEl = { hasAttribute: () => false };
	const canvas = { hasAttribute: (n: string) => n === 'data-lane-blocks' };
	const clipEl = { hasAttribute: () => false };
	test('the lane itself, and its block canvas where no block is, are empty surface', () => {
		expect(pressOnEmpty(laneEl as unknown as EventTarget, laneEl as unknown as EventTarget, () => false)).toBe(true);
		expect(pressOnEmpty(canvas as unknown as EventTarget, laneEl as unknown as EventTarget, () => false)).toBe(true);
		expect(pressOnEmpty(canvas as unknown as EventTarget, laneEl as unknown as EventTarget, () => true)).toBe(false);
		expect(pressOnEmpty(clipEl as unknown as EventTarget, laneEl as unknown as EventTarget, () => false)).toBe(false);
		expect(pressOnEmpty(null, laneEl as unknown as EventTarget, () => false)).toBe(false);
	});
});

test('a block is promoted only with no button down and no gesture under way', () => {
	expect(mayPromote(0, false)).toBe(true);
	expect(mayPromote(1, false)).toBe(false);
	expect(mayPromote(0, true)).toBe(false);
});

test("a narrow clip's button is no wider than its box (and the block it promotes)", () => {
	for (const w of [6, 11, 19]) expect(w - 2 * clipPadding(w)).toBe(w);
	expect(clipPadding(40)).toBe(7);
	expect(clipBox({ id: 'x', timeline_start: 0 }, 0.01, 100).right).toBe(6);
});
