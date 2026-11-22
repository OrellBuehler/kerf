import { describe, expect, test } from 'bun:test';
import { timelineRoot, timelineStop, type Searchable } from './timeline-focus';

const el = (name: string): Element => ({ name }) as unknown as Element;
/** A root whose selectors answer from a table. */
const root = (table: Record<string, Element | null>): Searchable => ({ querySelector: (sel) => table[sel] ?? null });

describe('where Focus the timeline lands', () => {
	const tab = el('tab stop');
	const any = el('some clip');
	const tool = el('select tool');

	test('on the clip that is the lanes’ one tab stop', () => {
		expect(timelineStop(root({ '.kclip:not([tabindex="-1"])': tab, '.kclip': any, '[role="toolbar"] button:not([disabled])': tool }))).toBe(tab);
	});

	test('failing that, any clip button', () => {
		expect(timelineStop(root({ '.kclip': any, '[role="toolbar"] button:not([disabled])': tool }))).toBe(any);
	});

	test('with no clip in view — an empty cut, a long lane drawn as blocks — the toolbar’s first button, so the keyboard is still in the timeline', () => {
		expect(timelineStop(root({ '[role="toolbar"] button:not([disabled])': tool }))).toBe(tool);
	});

	test('nothing to land on is none', () => {
		expect(timelineStop(root({}))).toBeNull();
	});

	test('the timeline is the editor surface that holds the lanes, not the preview', () => {
		const surface = el('surface');
		const lane = { closest: (sel: string) => (sel === '[data-editor-surface]' ? surface : null) } as unknown as Element;
		expect(timelineRoot({ querySelector: (sel) => (sel === '[data-lane]' ? lane : null) })).toBe(surface);
		expect(timelineRoot(root({}))).toBeNull();
	});
});
