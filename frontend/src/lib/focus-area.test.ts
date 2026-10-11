import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { EDITOR_SURFACE, isPagingKey, onEditorSurface, type FocusTarget } from './focus-area';

/** An element that is inside the given kind of panel, or none. */
const el = (inside: boolean, doc: { body: unknown; documentElement: unknown } = { body: {}, documentElement: {} }): FocusTarget => ({
	closest: (sel: string) => (inside && sel === EDITOR_SURFACE ? {} : null),
	ownerDocument: doc
});

describe('onEditorSurface', () => {
	test('nothing focused is the editor surface', () => {
		expect(onEditorSurface(null)).toBe(true);
	});

	test('the page body or root, in any window, is the editor surface', () => {
		const doc = { body: {}, documentElement: {} };
		const body: FocusTarget = { closest: () => null, ownerDocument: doc };
		doc.body = body;
		expect(onEditorSurface(body)).toBe(true);
		const root: FocusTarget = { closest: () => null, ownerDocument: doc };
		doc.documentElement = root;
		expect(onEditorSurface(root)).toBe(true);
	});

	test('something inside the timeline or the preview is', () => {
		expect(onEditorSurface(el(true))).toBe(true);
	});

	test('something inside any other panel — the Agent queue, the Library, a list — is not', () => {
		expect(onEditorSurface(el(false))).toBe(false);
	});

	test('the dock container around the timeline is (a click on empty space focuses it); the one around the Agent panel is not', () => {
		const around = (holds: boolean): FocusTarget => ({
			closest: () => null,
			querySelector: (sel: string) => (holds && sel === EDITOR_SURFACE ? {} : null),
			ownerDocument: { body: {}, documentElement: {} }
		});
		expect(onEditorSurface(around(true))).toBe(true);
		expect(onEditorSurface(around(false))).toBe(false);
	});

	test('the surface selector is the attribute the panels set', () => {
		expect(EDITOR_SURFACE).toBe('[data-editor-surface]');
	});
});

describe('isPagingKey', () => {
	test('Page Up and Page Down', () => {
		expect(isPagingKey('PageUp')).toBe(true);
		expect(isPagingKey('PageDown')).toBe(true);
		for (const k of ['[', ']', 'ArrowUp', 'Home', 'End', ' ']) expect(isPagingKey(k)).toBe(false);
	});
});

describe('the wiring', () => {
	const read = (rel: string) => readFileSync(join(import.meta.dir, rel), 'utf8');

	test('the timeline and the preview mark themselves the editor surface', () => {
		expect(read('components/editor/Timeline.svelte')).toContain('data-editor-surface');
		expect(read('components/editor/Preview.svelte')).toContain('data-editor-surface');
	});

	test('the page leaves Page Up / Page Down to a panel that has the focus', () => {
		expect(read('../routes/+page.svelte')).toContain("id.startsWith('review.') && isPagingKey(e.key) && !onEditorSurface(target)");
	});
});
