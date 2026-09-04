import { describe, expect, test } from 'bun:test';
import { DEFAULT_LAYOUT, PANEL_IDS, openPanelIds, sanitizeLayout } from './layout';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const clone = (v: unknown): any => JSON.parse(JSON.stringify(v));

describe('DEFAULT_LAYOUT', () => {
	test('shows every panel and survives sanitizing unchanged', () => {
		expect(openPanelIds(DEFAULT_LAYOUT).sort()).toEqual([...PANEL_IDS].sort());
		expect(sanitizeLayout(clone(DEFAULT_LAYOUT))).toEqual(DEFAULT_LAYOUT);
	});
});

describe('sanitizeLayout', () => {
	test('rejects anything that is not a layout', () => {
		for (const raw of [null, undefined, 'x', 42, [], {}, { grid: {}, panels: {} }]) {
			expect(sanitizeLayout(raw)).toBeNull();
		}
	});

	test('rejects a panel id it does not know', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.grid.root.data[2].data.views = ['effects'];
		raw.panels.effects = { id: 'effects', contentComponent: 'effects' };
		expect(sanitizeLayout(raw)).toBeNull();
	});

	test('rejects a view with no panel entry, or one bound to another component', () => {
		const missing = clone(DEFAULT_LAYOUT);
		delete missing.panels.agent;
		expect(sanitizeLayout(missing)).toBeNull();
		const wrong = clone(DEFAULT_LAYOUT);
		wrong.panels.agent.contentComponent = 'preview';
		expect(sanitizeLayout(wrong)).toBeNull();
	});

	test('rejects the same panel shown twice', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.grid.root.data[3].data.views = ['agent', 'media'];
		expect(sanitizeLayout(raw)).toBeNull();
	});

	test('accepts a subset of panels — closing one is a valid layout', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.grid.root.data.pop();
		delete raw.panels.agent;
		const out = sanitizeLayout(raw);
		expect(out).not.toBeNull();
		expect(openPanelIds(out!).sort()).toEqual(['inspector', 'media', 'preview', 'timeline', 'transcript']);
	});

	test('drops panel entries no group shows', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.grid.root.data.pop();
		const out = sanitizeLayout(raw)!;
		expect(out.panels.agent).toBeUndefined();
	});

	test('restores titles and minimum sizes from the registry', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.panels.inspector.title = 'Old name';
		raw.panels.inspector.minimumWidth = 10;
		const out = sanitizeLayout(raw)!;
		expect(out.panels.inspector).toEqual(DEFAULT_LAYOUT.panels.inspector);
	});

	test('drops floating groups, a stale active group and stray group options', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.floatingGroups = [{ data: {}, position: {} }];
		raw.activeGroup = 'gone';
		raw.grid.root.data[0].data.locked = true;
		const out = sanitizeLayout(raw)!;
		expect(out.floatingGroups).toBeUndefined();
		expect(out.activeGroup).toBeUndefined();
		expect(out.grid.root).toEqual(DEFAULT_LAYOUT.grid.root);
	});

	test('falls back to the first view when the active one is not in the group', () => {
		const raw = clone(DEFAULT_LAYOUT);
		raw.grid.root.data[0].data.activeView = 'preview';
		const out = sanitizeLayout(raw)!;
		expect(clone(out).grid.root.data[0].data.activeView).toBe('media');
	});
});
