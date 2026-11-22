import { describe, expect, test } from 'bun:test';
import { bubbleFor } from './menu-bubble';

const W = 1366;

describe('where an off row’s reason is drawn', () => {
	test('beside a menu with nothing open next to it: on the right', () => {
		expect(bubbleFor({ left: 304, right: 562 }, null, W)).toEqual({ side: 'right', at: 568 });
	});

	test('with the submenu open on its right, away from it: on the left of the menu', () => {
		expect(bubbleFor({ left: 304, right: 562 }, { left: 560, right: 828 }, W)).toEqual({ side: 'left', at: 298 });
	});

	test('a row in the submenu goes away from the menu it came from', () => {
		expect(bubbleFor({ left: 560, right: 828 }, { left: 304, right: 562 }, W)).toEqual({ side: 'right', at: 834 });
	});

	test('a submenu opened to the left (the menu is against the right edge): a menu row’s reason goes beyond the submenu, not over it', () => {
		// Menu 1100–1358, submenu 840–1108. Away from the submenu is the right, which has no room; the parent row is
		// beside the submenu, so the bubble hangs from the submenu's far edge, on the left of it.
		const b = bubbleFor({ left: 1100, right: 1358 }, { left: 840, right: 1108 }, W);
		expect(b.side).toBe('left');
		expect(b.at).toBe(834);
		// …which is clear of both panels.
		expect(b.at).toBeLessThan(840);
	});

	test('a row in that submenu goes on the left of it, away from the menu — never over the parent row', () => {
		const b = bubbleFor({ left: 840, right: 1108 }, { left: 1100, right: 1358 }, W);
		expect(b).toEqual({ side: 'left', at: 834 });
	});

	test('no sibling and no room on the preferred side: the other side', () => {
		expect(bubbleFor({ left: 1000, right: 1300 }, null, W)).toEqual({ side: 'left', at: 994 });
	});

	test('no room anywhere: the side with more of it', () => {
		expect(bubbleFor({ left: 150, right: 600 }, null, 800).side).toBe('right');
		expect(bubbleFor({ left: 300, right: 400 }, null, 500).side).toBe('left');
	});
});
