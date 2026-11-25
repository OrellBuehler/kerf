import { describe, expect, test } from 'bun:test';
import { focusLeaves, popoverKey } from './mixer-popover';

describe('what a key does while a Mixer popover is open', () => {
	test('Escape closes it, and is its own', () => {
		expect(popoverKey({ key: 'Escape' })).toEqual({ stop: true, close: true });
	});

	test('Tab passes: the focus may leave, and leaving closes it', () => {
		expect(popoverKey({ key: 'Tab' })).toEqual({ stop: false, close: false });
		expect(popoverKey({ key: 'Tab', ctrlKey: true })).toEqual({ stop: false, close: false });
	});

	test('undo, redo and save reach the page: a modifier chord the popover does not use is not its own', () => {
		for (const key of ['z', 'Z', 'y', 's']) {
			expect(popoverKey({ key, ctrlKey: true }), `Ctrl+${key}`).toEqual({ stop: false, close: false });
			expect(popoverKey({ key, metaKey: true }), `Cmd+${key}`).toEqual({ stop: false, close: false });
		}
	});

	test('Delete and Backspace never reach the page, with a modifier or without', () => {
		for (const key of ['Delete', 'Backspace']) {
			for (const mods of [{}, { ctrlKey: true }, { metaKey: true }]) expect(popoverKey({ key, ...mods }), key).toEqual({ stop: true, close: false });
		}
	});

	test('every other key is the popover’s: a letter, Space, Enter, the arrows are kept from the page’s shortcuts', () => {
		for (const key of ['k', 'j', 'c', ' ', 'Enter', 'ArrowLeft', 'ArrowUp', 'Home', 'PageDown']) {
			expect(popoverKey({ key }), key).toEqual({ stop: true, close: false });
		}
	});
});

describe('when moving the focus closes a Mixer popover', () => {
	// A stand-in for two DOM subtrees: `Node` is the global the rule tests against.
	class FakeNode {
		constructor(readonly children: FakeNode[] = []) {}
		contains(n: unknown): boolean {
			return n === this || this.children.some((c) => c.contains(n));
		}
	}
	const realNode = globalThis.Node;
	const withNode = (f: () => void) => {
		(globalThis as { Node: unknown }).Node = FakeNode;
		try {
			f();
		} finally {
			(globalThis as { Node: unknown }).Node = realNode;
		}
	};

	test('to something outside the box and outside its button — a Tab out — it does', () => {
		withNode(() => {
			const inside = new FakeNode();
			const box = new FakeNode([inside]);
			const anchor = new FakeNode();
			const outside = new FakeNode();
			expect(focusLeaves(outside, box as never, anchor as never)).toBe(true);
			expect(focusLeaves(inside, box as never, anchor as never)).toBe(false);
		});
	});

	test('to the button that opened it it does not: a press there is the button’s own toggle', () => {
		withNode(() => {
			const box = new FakeNode();
			const anchor = new FakeNode([new FakeNode()]);
			expect(focusLeaves(anchor, box as never, anchor as never)).toBe(false);
			expect(focusLeaves(anchor.children[0], box as never, anchor as never)).toBe(false);
		});
	});

	test('to nothing it does not: a click on a part of the box that takes no focus, or the window losing it, is not a Tab out', () => {
		withNode(() => {
			expect(focusLeaves(null, new FakeNode() as never, new FakeNode() as never)).toBe(false);
		});
	});
});
