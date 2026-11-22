import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ACTIONS } from './keymap';
import {
	BAR_MENU,
	DOCK_CONTENT,
	DOCK_GROUP,
	EDITOR_SURFACE,
	PANEL_TAB,
	areaOf,
	cutDenied,
	cutDeniedNotice,
	elementOf,
	forgetArea,
	isPagingKey,
	isScrollKey,
	lastWorkedArea,
	mayEditCut,
	needsEditorSurface,
	noteArea,
	onEditorSurface,
	setLastArea,
	setMainDocument,
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

/** What a fake element is made of: the selectors it sits inside (any of a comma list matches, as `closest` does), what it
 *  holds, the tab it is (`data-tab-panel-id`) and the document it belongs to. */
interface Fake {
	inside?: string[];
	holds?: string[];
	tab?: string;
	activeTab?: string;
}
type Doc = { body: unknown; documentElement: unknown; querySelector?(sel: string): unknown; has?: string[] };
const newDoc = (has: string[] = []): Doc => ({ body: null, documentElement: null, has, querySelector: function (this: Doc, sel: string) { return this.has?.includes(sel) ? {} : null; } });

function make(f: Fake = {}, doc: Doc = newDoc()): FocusTarget {
	const inside = f.inside ?? [];
	const tab = f.tab !== undefined ? { getAttribute: (n: string) => (n === 'data-tab-panel-id' ? f.tab! : null) } : null;
	const group = inside.includes(DOCK_GROUP)
		? { querySelector: (sel: string) => (sel === '.dv-tab.dv-active-tab' && f.activeTab !== undefined ? { getAttribute: () => f.activeTab! } : null) }
		: null;
	return {
		closest: (sel: string) => {
			if (sel === PANEL_TAB) return tab;
			if (sel === DOCK_GROUP) return group;
			return sel.split(',').some((one) => inside.includes(one.trim())) ? {} : null;
		},
		querySelector: (sel: string) => ((f.holds ?? []).includes(sel) ? {} : null),
		ownerDocument: doc as FocusTarget['ownerDocument']
	};
}
/** The page body (or root) of a document: what has the focus when a panel has dropped its own. */
function pageBody(doc: Doc = newDoc(), root = false): FocusTarget {
	const el = make({}, doc);
	if (root) doc.documentElement = el;
	else doc.body = el;
	return el;
}
const TIMELINE = make({ inside: [EDITOR_SURFACE] });
const PANEL = make({ inside: [DOCK_CONTENT] });
const MAIN = newDoc();
setMainDocument(MAIN);

describe('which area a key is in', () => {
	test('the timeline and the preview are the editor; the dock container around one is too', () => {
		expect(areaOf(TIMELINE)).toBe('editor');
		expect(areaOf(make({ inside: [DOCK_CONTENT], holds: [EDITOR_SURFACE] }))).toBe('editor');
	});

	test('any other dock panel is a panel', () => {
		expect(areaOf(PANEL)).toBe('panel');
	});

	test('the timeline’s own dock tab is the timeline, and so is the preview’s — before either has been activated', () => {
		expect(areaOf(make({ inside: [DOCK_GROUP], tab: 'timeline' }))).toBe('editor');
		expect(areaOf(make({ inside: [DOCK_GROUP], tab: 'preview' }))).toBe('editor');
		// …and another panel’s tab is that panel, the timeline's group or not.
		expect(areaOf(make({ inside: [DOCK_GROUP], tab: 'inspector' }))).toBe('panel');
		expect(areaOf(make({ inside: [DOCK_GROUP], tab: 'agent' }))).toBe('panel');
	});

	test('a group’s header — the empty space beside its tabs — is the panel its active tab opens', () => {
		expect(areaOf(make({ inside: [DOCK_GROUP], activeTab: 'timeline' }))).toBe('editor');
		expect(areaOf(make({ inside: [DOCK_GROUP], activeTab: 'inspector' }))).toBe('panel');
		expect(areaOf(make({ inside: [DOCK_GROUP] }))).toBe('panel');
	});

	test('nothing in particular: no element, the editor window’s body or root, the menu bar’s own menus', () => {
		expect(areaOf(null)).toBe('neutral');
		expect(areaOf(pageBody(MAIN))).toBe('neutral');
		expect(areaOf(pageBody(newDoc(), true), MAIN)).toBe('panel');
		expect(areaOf(make({ inside: [BAR_MENU.split(',')[0].trim()] }))).toBe('neutral');
		expect(areaOf(make({ inside: [BAR_MENU.split(',')[1].trim()] }))).toBe('neutral');
	});

	test('the title bar’s buttons, the status bar, a banner, a toast, a dialog: nothing in particular too, not a panel', () => {
		expect(areaOf(make({ inside: [] }))).toBe('neutral');
	});

	test('a detached window’s body is a panel’s — it holds the panels popped out into it — unless the editor itself is what it holds', () => {
		const popout = newDoc();
		expect(areaOf(pageBody(popout), MAIN)).toBe('panel');
		const withTimeline = newDoc([EDITOR_SURFACE]);
		expect(areaOf(pageBody(withTimeline), MAIN)).toBe('editor');
	});
});

describe('the last place the user worked', () => {
	test('is the timeline to begin with, and after another project is opened (`forgetArea`)', () => {
		forgetArea();
		expect(lastWorkedArea()).toBe('editor');
		noteArea(PANEL);
		expect(lastWorkedArea()).toBe('panel');
		forgetArea();
		expect(lastWorkedArea()).toBe('editor');
	});

	test('a press or a focus in a panel is where they are; a button in the title bar, a dialog, nothing are not', () => {
		forgetArea();
		noteArea(PANEL);
		noteArea(make({ inside: [] }));
		noteArea(null);
		expect(lastWorkedArea()).toBe('panel');
		noteArea(TIMELINE);
		expect(lastWorkedArea()).toBe('editor');
		noteArea(make({ inside: [] }));
		expect(lastWorkedArea()).toBe('editor');
		forgetArea();
	});

	test('a press on the timeline’s own tab or header puts the user back in the timeline', () => {
		forgetArea();
		noteArea(PANEL);
		noteArea(make({ inside: [DOCK_GROUP], tab: 'timeline' }));
		expect(lastWorkedArea()).toBe('editor');
		forgetArea();
	});

	test('a drop on the timeline, and the key that moves the focus to it, say so (`setLastArea`)', () => {
		forgetArea();
		noteArea(PANEL);
		setLastArea('editor');
		expect(lastWorkedArea()).toBe('editor');
		forgetArea();
	});
});

describe('who may edit the cut', () => {
	const body = pageBody(MAIN);

	test('the timeline and the preview, whatever came last', () => {
		for (const last of ['editor', 'panel'] as const) expect(mayEditCut(TIMELINE, last)).toBe(true);
	});

	test('a panel never does — the Inspector, the transcript, the dope sheet, the library, the mixer', () => {
		for (const last of ['editor', 'panel'] as const) expect(mayEditCut(PANEL, last)).toBe(false);
	});

	test('nothing in particular does only when the timeline was the last place worked in: a panel that dropped its focus does not hand the key on', () => {
		expect(mayEditCut(body, 'editor')).toBe(true);
		expect(mayEditCut(body, 'panel')).toBe(false);
		expect(mayEditCut(null, 'editor')).toBe(true);
		expect(mayEditCut(null, 'panel')).toBe(false);
	});

	test('the title bar is nothing in particular: after Export, Settings or the bell is closed with Esc and focus is back on its button, Delete still reaches the selected clip', () => {
		const titleButton = make({ inside: [] });
		expect(mayEditCut(titleButton, 'editor')).toBe(true);
		expect(mayEditCut(titleButton, 'panel')).toBe(false);
	});

	test('the menu bar’s menus likewise, so a chord there still goes on from the timeline', () => {
		expect(mayEditCut(make({ inside: ['[role="menubar"]'] }), 'editor')).toBe(true);
		expect(mayEditCut(make({ inside: ['[role="menubar"]'] }), 'panel')).toBe(false);
	});

	test('the body of a detached window does not: Delete there after clicking a clip in the main window is not the page’s', () => {
		const popout = pageBody(newDoc());
		expect(mayEditCut(popout, 'editor', MAIN)).toBe(false);
		expect(mayEditCut(pageBody(newDoc([EDITOR_SURFACE])), 'editor', MAIN)).toBe(true);
	});

	test('an element in another window is still an element', () => {
		expect(elementOf(null)).toBeNull();
		expect(elementOf({ closest: () => null } as unknown as EventTarget)).not.toBeNull();
		expect(elementOf({} as EventTarget)).toBeNull();
	});
});

describe('a key that is not taken says so', () => {
	test('the notice names the action and what to do, and the key that gets there', () => {
		expect(cutDeniedNotice('Delete selection', null)).toBe('Delete selection edits the timeline — click it first');
		expect(cutDeniedNotice('Split at playhead', 'F6')).toBe('Split at playhead edits the timeline — click it first, or press F6');
	});

	test('it is said unless the user is working with a text selection, whose copy and cut are the browser’s own', () => {
		expect(cutDenied('edit.delete', { textSelected: false }).notice).toBe(true);
		expect(cutDenied('edit.copy', { textSelected: true }).notice).toBe(false);
		expect(cutDenied('edit.cut', { textSelected: true }).notice).toBe(false);
	});

	test('Select all is taken, so it does not highlight the page; copy, cut and the rest are left to the browser', () => {
		expect(cutDenied('edit.selectAll', { textSelected: false }).preventDefault).toBe(true);
		for (const id of ['edit.copy', 'edit.cut', 'edit.paste', 'edit.delete', 'edit.split']) expect(cutDenied(id, { textSelected: false }).preventDefault).toBe(false);
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
			'sfx.add'
		]);
	});

	test('transport, undo and redo, view, tools and files stay global', () => {
		for (const id of ['edit.undo', 'edit.redo', 'edit.history', 'edit.clearSelection', 'marker.add', 'view.focusTimeline', 'playback.toggle', 'playback.nextEdit', 'view.zoomIn', 'tool.razor', 'tool.rippleMode', 'file.save', 'file.export', 'range.markIn', 'marker.next', 'workspace.edit', 'app.settings']) {
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
		// …while it is open *and its window still is* (`holdsKeys`: a menu whose window closed is closed, not kept for good).
		expect(onKey.indexOf('if (contextMenu.holdsKeys()) return;')).toBeGreaterThan(0);
		expect(onKey.indexOf('if (contextMenu.holdsKeys()) return;')).toBeLessThan(onKey.indexOf('settings.actionFor(e)'));
		// A denied key says why, and Select all is taken (`cutDenied`); a text field still keeps every key but Focus the timeline.
		expect(onKey).toContain('cutKeyRefused(id)');
		expect(onKey).toContain('denied.preventDefault');
		expect(onKey).toContain("settings.actionFor(e) !== 'view.focusTimeline'");
		expect(onKey).toContain("actionDef(id)?.edits === 'cut'");
		expect(onKey).toContain('mayEditCut(target)');
		expect(page).toContain("windows.listen('pointerdown', area, true)");
		expect(page).toContain("windows.listen('focusin', area, true)");
	});

	test('the page leaves the keys that scroll to a panel that has the focus', () => {
		expect(read('../routes/+page.svelte')).toContain('needsEditorSurface(id, e.key) && !onEditorSurface(target)');
	});
});
