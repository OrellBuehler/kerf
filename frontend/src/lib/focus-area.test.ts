import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ACTIONS } from './keymap';
import {
	BAR_MENU,
	DOCK_PANEL,
	EDITOR_SURFACE,
	areaOf,
	elementOf,
	forgetArea,
	isPagingKey,
	isScrollKey,
	lastWorkedArea,
	mayEditCut,
	needsEditorSurface,
	noteArea,
	onEditorSurface,
	type FocusTarget
} from './focus-area';

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

describe('which actions are the surface\'s alone', () => {
	test('↑ ↓ and the page keys scroll', () => {
		for (const k of ['PageUp', 'PageDown', 'ArrowUp', 'ArrowDown']) expect(isScrollKey(k)).toBe(true);
		for (const k of ['ArrowLeft', 'ArrowRight', 'Home', 'End', ' ', 'd']) expect(isScrollKey(k)).toBe(false);
	});

	test('a proposal\'s change steps, on the page keys only', () => {
		expect(needsEditorSurface('review.nextChange', 'PageDown')).toBe(true);
		expect(needsEditorSurface('review.nextChange', ']')).toBe(false);
	});

	test('the edit-point and clip-selection keys, on ↑ ↓', () => {
		expect(needsEditorSurface('playback.nextEdit', 'ArrowDown')).toBe(true);
		expect(needsEditorSurface('playback.prevEdit', 'ArrowUp')).toBe(true);
		expect(needsEditorSurface('select.laneAbove', 'ArrowUp')).toBe(true);
		// Rebound to a letter they scroll nothing.
		expect(needsEditorSurface('playback.nextEdit', 'n')).toBe(false);
		expect(needsEditorSurface('select.nextClip', 'ArrowRight')).toBe(false);
	});

	test('nothing else is', () => {
		expect(needsEditorSurface('playback.stepForward', 'ArrowRight')).toBe(false);
		expect(needsEditorSurface('marker.next', 'ArrowDown')).toBe(false);
	});
});

/** An element that sits inside these selectors (any of a comma list matches, as `closest` does) and contains these. */
const within = (inside: string[], holds: string[] = [], doc: { body: unknown; documentElement: unknown } = { body: {}, documentElement: {} }): FocusTarget => ({
	closest: (sel: string) => (sel.split(',').some((one) => inside.includes(one.trim())) ? {} : null),
	querySelector: (sel: string) => (holds.includes(sel) ? {} : null),
	ownerDocument: doc
});
const TIMELINE = within([EDITOR_SURFACE]);
const PANEL = within(['.dv-content-container']);
/** The page body (or root) of a document: what has the focus when a panel has dropped its own. */
function pageBody(root = false): FocusTarget {
	const doc: { body: unknown; documentElement: unknown } = { body: null, documentElement: null };
	const el: FocusTarget = { closest: () => null, querySelector: () => null, ownerDocument: doc };
	if (root) doc.documentElement = el;
	else doc.body = el;
	return el;
}

describe('which area a key is in', () => {
	test('the timeline and the preview are the editor; the dock container around one is too', () => {
		expect(areaOf(TIMELINE)).toBe('editor');
		expect(areaOf(within(['.dv-content-container'], [EDITOR_SURFACE]))).toBe('editor');
	});

	test('any other dock panel, and a panel’s tabs, are a panel', () => {
		expect(areaOf(PANEL)).toBe('panel');
		expect(areaOf(within(['.dv-groupview']))).toBe('panel');
		expect(DOCK_PANEL).toContain('.dv-content-container');
	});

	test('nothing in particular: no element, the page body or root, the menu bar’s own menus', () => {
		expect(areaOf(null)).toBe('neutral');
		expect(areaOf(pageBody())).toBe('neutral');
		expect(areaOf(pageBody(true))).toBe('neutral');
		expect(areaOf(within([BAR_MENU.split(',')[0].trim()]))).toBe('neutral');
		expect(areaOf(within([BAR_MENU.split(',')[1].trim()]))).toBe('neutral');
	});

	test('everything else — the title bar’s buttons, the status bar, a banner, a dialog — is chrome', () => {
		expect(areaOf(within([]))).toBe('chrome');
	});
});

describe('the last place the user worked', () => {
	test('a press or a focus in the timeline or a panel is where they are; chrome and nothing are not', () => {
		forgetArea();
		expect(lastWorkedArea()).toBeNull();
		noteArea(PANEL);
		expect(lastWorkedArea()).toBe('panel');
		noteArea(within([]));
		noteArea(null);
		expect(lastWorkedArea()).toBe('panel');
		noteArea(TIMELINE);
		expect(lastWorkedArea()).toBe('editor');
		// A dialog, a toast or a button in the title bar leaves it as it was.
		noteArea(within([]));
		expect(lastWorkedArea()).toBe('editor');
		forgetArea();
	});
});

describe('who may edit the cut', () => {
	const body = pageBody();

	test('the timeline and the preview, whatever came last', () => {
		for (const last of [null, 'editor', 'panel'] as const) expect(mayEditCut(TIMELINE, last)).toBe(true);
	});

	test('a panel never does — the Inspector, the transcript, the dope sheet, the Library, the mixer', () => {
		for (const last of [null, 'editor', 'panel'] as const) expect(mayEditCut(PANEL, last)).toBe(false);
	});

	test('nothing in particular does only when the timeline was the last place worked in: a panel that dropped its focus does not hand the key on', () => {
		expect(mayEditCut(body, 'editor')).toBe(true);
		expect(mayEditCut(body, 'panel')).toBe(false);
		expect(mayEditCut(body, null)).toBe(false);
		expect(mayEditCut(null, 'editor')).toBe(true);
		expect(mayEditCut(null, 'panel')).toBe(false);
	});

	test('chrome does not, even with the timeline last', () => {
		expect(mayEditCut(within([]), 'editor')).toBe(false);
	});

	test('the menu bar’s menus count as nothing in particular, so a chord there still goes on from the timeline', () => {
		expect(mayEditCut(within(['[role="menubar"]']), 'editor')).toBe(true);
		expect(mayEditCut(within(['[role="menubar"]']), 'panel')).toBe(false);
	});

	test('an element in another window is still an element', () => {
		expect(elementOf(null)).toBeNull();
		expect(elementOf({ closest: () => null } as unknown as EventTarget)).not.toBeNull();
		expect(elementOf({} as EventTarget)).toBeNull();
	});
});

describe('every action decides whether it edits the cut', () => {
	test('each one says so, and only those', () => {
		for (const a of ACTIONS) expect([a.id, a.edits === 'cut' || a.edits === 'none']).toEqual([a.id, true]);
	});

	test('the ones that change the cut or the clip selection are exactly these: adding to the list is a decision', () => {
		const cut = ACTIONS.filter((a) => a.edits === 'cut').map((a) => a.id);
		expect(cut).toEqual([
			'file.importCaptions',
			'edit.selectAll',
			'edit.copy',
			'edit.cut',
			'edit.paste',
			'edit.duplicate',
			'edit.split',
			'edit.splitAll',
			'edit.lift',
			'edit.extract',
			'edit.delete',
			'edit.rippleDelete',
			'edit.trimStart',
			'edit.trimEnd',
			'edit.rippleTrimStart',
			'edit.rippleTrimEnd',
			'edit.rippleDeleteGap',
			'edit.detachAudio',
			'edit.reattachAudio',
			'edit.link',
			'edit.unlink',
			'title.add',
			'title.generateCaptions',
			'select.atPlayhead',
			'select.prevClip',
			'select.nextClip',
			'select.laneAbove',
			'select.laneBelow',
			'select.extendPrev',
			'select.extendNext',
			'select.extendAbove',
			'select.extendBelow',
			'edit.rollBack',
			'edit.rollForward',
			'edit.slipBack',
			'edit.slipForward',
			'edit.slideBack',
			'edit.slideForward',
			'marker.add',
			'sfx.add'
		]);
	});

	test('transport, undo and redo, view, tools and files stay global', () => {
		for (const id of ['edit.undo', 'edit.redo', 'edit.history', 'edit.clearSelection', 'playback.toggle', 'playback.nextEdit', 'view.zoomIn', 'tool.razor', 'tool.rippleMode', 'file.save', 'file.export', 'range.markIn', 'marker.next', 'workspace.edit', 'app.settings']) {
			expect([id, ACTIONS.find((a) => a.id === id)?.edits]).toEqual([id, 'none']);
		}
	});
});

describe('the wiring', () => {
	const read = (rel: string) => readFileSync(join(import.meta.dir, rel), 'utf8');

	test('the timeline and the preview mark themselves the editor surface', () => {
		expect(read('components/editor/Timeline.svelte')).toContain('data-editor-surface');
		expect(read('components/editor/Preview.svelte')).toContain('data-editor-surface');
	});

	test('the page ignores every key while a context menu is open, and gates the editing actions by where the keys are', () => {
		const page = read('../routes/+page.svelte');
		const onKey = page.slice(page.indexOf('function onKey('));
		expect(onKey.indexOf('if (contextMenu.visible) return;')).toBeGreaterThan(0);
		expect(onKey.indexOf('if (contextMenu.visible) return;')).toBeLessThan(onKey.indexOf('settings.actionFor(e)'));
		expect(onKey).toContain("actionDef(id)?.edits === 'cut'");
		expect(onKey).toContain('mayEditCut(target)');
		expect(page).toContain("windows.listen('pointerdown', area, true)");
		expect(page).toContain("windows.listen('focusin', area, true)");
	});

	test('the page leaves the keys that scroll to a panel that has the focus', () => {
		expect(read('../routes/+page.svelte')).toContain('needsEditorSurface(id, e.key) && !onEditorSurface(target)');
	});
});
