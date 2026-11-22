import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, jest, mock, spyOn, test } from 'bun:test';
import './test-runes';
import { PRESET_LAYOUTS, presetLayout, sameArrangement } from './layout';
import {
	addBlocked,
	addWorkspace,
	checkName,
	defaultWorkspaces,
	libraryTabFor,
	moveWorkspace,
	newWorkspaceKey,
	removeWorkspace,
	renameWorkspace,
	withLayout,
	withLibraryTab,
	withoutWorkspaces,
	type WorkspaceId,
	type WorkspaceKey,
	type WorkspacesState
} from './workspaces';

// The live side of a workspace reset: `workspace` driving a dock. The dock is a
// stand-in that keeps the layout it was given and reports it back, the way
// dockview's `fromJSON` / `toJSON` do; the settings are the real state
// transitions (`workspaces.ts`) behind a plain object, so what is checked is what
// the app does between "Reset" and the next time the layout is written.

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const clone = (v: unknown): any => JSON.parse(JSON.stringify(v));

const toasts: Array<[string, string]> = [];
const fakeSettings = {
	workspaces: defaultWorkspaces() as WorkspacesState,
	libraryCollapsed: false,
	writes: 0,
	get libraryTab() {
		return libraryTabFor(this.workspaces, this.workspaces.active);
	},
	saveWorkspaceAs(name: string, layout: never, tab: never) {
		const blocked = addBlocked(this.workspaces);
		if (blocked) return { ok: false as const, reason: blocked };
		const checked = checkName(this.workspaces, name);
		if (!checked.ok) return checked;
		const key = newWorkspaceKey(this.workspaces);
		this.workspaces = addWorkspace(this.workspaces, key, checked.name, layout, tab);
		this.writes++;
		return { ok: true as const, key };
	},
	renameWorkspace(key: WorkspaceKey, name: string) {
		const checked = checkName(this.workspaces, name, key);
		if (!checked.ok) return checked;
		this.workspaces = renameWorkspace(this.workspaces, key, checked.name);
		this.writes++;
		return checked;
	},
	deleteWorkspace(key: WorkspaceKey) {
		this.workspaces = removeWorkspace(this.workspaces, key);
		this.writes++;
	},
	moveWorkspace(key: WorkspaceKey, to: number) {
		this.workspaces = moveWorkspace(this.workspaces, key, to);
		this.writes++;
	},
	saveWorkspaceLayout(id: WorkspaceKey, layout: never) {
		this.workspaces = withLayout(this.workspaces, id, layout);
		this.writes++;
	},
	resetWorkspace(id: WorkspaceId) {
		this.workspaces = withoutWorkspaces(this.workspaces, [id]);
		this.writes++;
	},
	resetAllWorkspaces() {
		this.workspaces = withoutWorkspaces(this.workspaces, ['edit', 'color', 'audio', 'motion', 'deliver']);
		this.writes++;
	},
	setActiveWorkspace(id: WorkspaceKey) {
		this.workspaces = { ...this.workspaces, active: id };
	}
};

// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
mock.module('./notifications.svelte', () => ({
	toast: Object.assign((m: string) => toasts.push(['note', m]), {
		success: (m: string) => toasts.push(['success', m]),
		info: (m: string) => toasts.push(['info', m]),
		error: (m: string) => toasts.push(['error', m]),
		warning: (m: string) => toasts.push(['warning', m]),
		dismiss: () => {}
	})
}));
// The real settings, kept to put back: a module mock outlives its file.
const realSettings = { ...(await import('./settings.svelte')) };

beforeAll(() => {
	mock.module('./settings.svelte', () => ({ settings: fakeSettings }));
});
afterAll(() => {
	mock.module('./settings.svelte', () => realSettings);
	jest.useRealTimers();
	delete (globalThis as { requestAnimationFrame?: unknown }).requestAnimationFrame;
	delete (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
});

type Handler = () => void;
function fakeDock(failing = false) {
	let current: any = null;
	const handlers: Handler[] = [];
	const dock = {
		fromJSONCalls: [] as any[],
		failing,
		get current() {
			return current;
		},
		fromJSON(layout: any) {
			dock.fromJSONCalls.push(clone(layout));
			if (dock.failing) throw new Error('refused');
			current = clone(layout);
		},
		toJSON: () => clone(current),
		layout() {},
		get panels() {
			const out: any[] = [];
			const visit = (n: any) => (n.type === 'leaf' ? n.data.views.forEach((id: string) => out.push({ id })) : n.data.forEach(visit));
			if (current) visit(current.grid.root);
			return out;
		},
		getPanel: () => undefined,
		activeGroup: undefined,
		onDidAddPanel: () => ({ dispose() {} }),
		onDidRemovePanel: () => ({ dispose() {} }),
		onDidLayoutChange(h: Handler) {
			handlers.push(h);
			return { dispose() {} };
		},
		onDidPopoutGroupPositionChange: () => ({ dispose() {} }),
		onDidPopoutGroupSizeChange: () => ({ dispose() {} }),
		popoutRestorationPromise: Promise.resolve() as Promise<void>,
		/** What dockview does after most things: report a change. */
		changed() {
			handlers.forEach((h) => h());
		},
		/** The user drags the first sash of the top row 80 px. */
		drag() {
			const row = current.grid.root.data[0].data;
			row[0].size += 80;
			row[1].size -= 80;
			dock.changed();
		}
	};
	return dock;
}

class FakeObserver {
	observe() {}
	disconnect() {}
}

async function boot(state: WorkspacesState, failing = false) {
	fakeSettings.workspaces = state;
	fakeSettings.writes = 0;
	toasts.length = 0;
	const { workspace } = await import('./workspace.svelte');
	const dock = fakeDock(failing);
	workspace.detach();
	workspace.attach(dock as never, { clientWidth: 1200, clientHeight: 700 } as never);
	// Two frames: the layout settles and becomes the reference.
	jest.advanceTimersByTime(100);
	return { workspace, dock };
}

afterEach(() => {
	jest.useRealTimers();
});

beforeEach(() => {
	jest.useFakeTimers();
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	(globalThis as any).requestAnimationFrame = (cb: () => void) => setTimeout(cb, 16);
	(globalThis as any).ResizeObserver = FakeObserver;
});

const arranged = (id: WorkspaceId) => {
	const l = clone(PRESET_LAYOUTS[id]);
	l.grid.root.data[0].data[0].size += 60;
	l.grid.root.data[0].data[1].size -= 60;
	return l;
};

describe('Reset workspace', () => {
	test('puts the preset on the dock and forgets the stored arrangement', async () => {
		const { workspace, dock } = await boot(withLayout(defaultWorkspaces(), 'edit', arranged('edit')));
		expect(sameArrangement(dock.current, arranged('edit'))).toBe(true);
		workspace.reset();
		expect(sameArrangement(dock.current, PRESET_LAYOUTS.edit)).toBe(true);
		expect(fakeSettings.workspaces.layouts.edit).toBeUndefined();
		expect(fakeSettings.workspaces.offered.edit).toBeUndefined();
		expect(toasts).toEqual([['success', 'Edit workspace reset to its default arrangement']]);
	});

	test('is not undone by the save that follows it', async () => {
		const { workspace, dock } = await boot(withLayout(defaultWorkspaces(), 'edit', arranged('edit')));
		workspace.reset();
		// Dockview reports the rebuild a moment later, then the layout settles.
		dock.changed();
		jest.advanceTimersByTime(100);
		dock.changed();
		jest.advanceTimersByTime(2000);
		expect(fakeSettings.workspaces.layouts.edit).toBeUndefined();
	});

	test('drops a change still waiting to be saved instead of writing it over the reset', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		dock.drag(); // on the debounce, not yet written
		expect(fakeSettings.workspaces.layouts.edit).toBeUndefined();
		workspace.reset();
		jest.advanceTimersByTime(2000);
		expect(fakeSettings.workspaces.layouts.edit).toBeUndefined();
		expect(sameArrangement(dock.current, PRESET_LAYOUTS.edit)).toBe(true);
	});

	test('a rearrangement after the reset is written again, as the new one', async () => {
		const { workspace, dock } = await boot(withLayout(defaultWorkspaces(), 'edit', arranged('edit')));
		workspace.reset();
		jest.advanceTimersByTime(100);
		dock.drag();
		jest.advanceTimersByTime(600);
		expect(fakeSettings.workspaces.layouts.edit).toBeDefined();
		expect(sameArrangement(fakeSettings.workspaces.layouts.edit!, arranged('edit'))).toBe(false);
	});

	test('forgets the library tab picked in the workspace, and no other workspace’s', async () => {
		let state = withLayout(defaultWorkspaces(), 'edit', arranged('edit'));
		state = withLibraryTab(withLibraryTab(state, 'edit', 'transcript'), 'color', 'titles');
		const { workspace } = await boot(state);
		workspace.reset();
		expect(libraryTabFor(fakeSettings.workspaces, 'edit')).toBe('media');
		expect(libraryTabFor(fakeSettings.workspaces, 'color')).toBe('titles');
	});

	test('on a workspace that is already the preset says so rather than seeming to do nothing', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		workspace.reset();
		expect(toasts).toEqual([['info', 'Edit workspace is already in its default arrangement']]);
	});

	test('says it reset when something was stored, even if it looks the same', async () => {
		const { workspace } = await boot(withLayout(defaultWorkspaces(), 'edit', presetLayout('edit')));
		workspace.reset();
		expect(toasts[0][0]).toBe('success');
	});

	test('leaves the other workspaces’ arrangements alone', async () => {
		const state = withLayout(withLayout(defaultWorkspaces(), 'edit', arranged('edit')), 'color', arranged('color'));
		const { workspace } = await boot(state);
		workspace.reset();
		expect(fakeSettings.workspaces.layouts.color).toBeDefined();
	});

	test('resets the workspace that is on screen after a switch', async () => {
		const state = withLayout(withLayout(defaultWorkspaces(), 'edit', arranged('edit')), 'color', arranged('color'));
		const { workspace, dock } = await boot(state);
		workspace.switchTo('color');
		jest.advanceTimersByTime(100);
		expect(sameArrangement(dock.current, arranged('color'))).toBe(true);
		workspace.reset();
		expect(sameArrangement(dock.current, PRESET_LAYOUTS.color)).toBe(true);
		expect(fakeSettings.workspaces.layouts.color).toBeUndefined();
		expect(fakeSettings.workspaces.layouts.edit).toBeDefined();
	});

	test('tells the user when the dock would take neither the layout nor the preset', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		toasts.length = 0;
		dock.failing = true;
		const quiet = spyOn(console, 'error').mockImplementation(() => {});
		expect(() => workspace.reset()).not.toThrow();
		quiet.mockRestore();
		expect(toasts.map((t) => t[0])).toContain('error');
	});
});

describe('Reset all workspaces', () => {
	test('forgets every arrangement and rebuilds the one on screen', async () => {
		let state = defaultWorkspaces();
		for (const id of ['edit', 'color', 'audio'] as const) state = withLayout(state, id, arranged(id));
		const { workspace, dock } = await boot(state);
		workspace.resetAll();
		expect(fakeSettings.workspaces.layouts).toEqual({});
		expect(sameArrangement(dock.current, PRESET_LAYOUTS.edit)).toBe(true);
		expect(toasts).toEqual([['success', 'All workspaces reset to their default arrangements']]);
		jest.advanceTimersByTime(2000);
		expect(fakeSettings.workspaces.layouts).toEqual({});
	});
});

describe('workspaces the user saves', () => {
	test('saving keeps what is on screen under a name, shows it and does not rebuild the dock', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		dock.drag();
		const built = dock.fromJSONCalls.length;
		expect(workspace.saveAs('Review')).toBeNull();
		const key = fakeSettings.workspaces.custom[0].id;
		expect(workspace.shown).toBe(key);
		expect(fakeSettings.workspaces.active).toBe(key);
		// What was stored is what the dock shows, to the share.
		expect(sameArrangement(fakeSettings.workspaces.layouts[key]!, dock.current)).toBe(true);
		expect(dock.fromJSONCalls).toHaveLength(built);
		expect(toasts).toEqual([['success', 'Saved this arrangement as the Review workspace']]);
	});

	test('the workspace it was made in keeps its own arrangement, including a change not yet written', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		dock.drag(); // on the debounce
		expect(fakeSettings.workspaces.layouts.edit).toBeUndefined();
		workspace.saveAs('Review');
		expect(fakeSettings.workspaces.layouts.edit).toBeDefined();
		expect(sameArrangement(fakeSettings.workspaces.layouts.edit!, dock.current)).toBe(true);
	});

	test('the new one starts with the library tab that was showing', async () => {
		const { workspace } = await boot(withLibraryTab(defaultWorkspaces(), 'edit', 'transcript'));
		workspace.saveAs('Review');
		expect(libraryTabFor(fakeSettings.workspaces, fakeSettings.workspaces.custom[0].id)).toBe('transcript');
	});

	test('a name that is taken or empty, or no room, is refused with the reason and nothing is saved', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		expect(workspace.saveAs('color')).toContain('already a workspace called Color');
		expect(workspace.saveAs('  ')).toContain('name');
		expect(fakeSettings.workspaces.custom).toEqual([]);
		expect(workspace.shown).toBe('edit');
		expect(toasts).toEqual([]);
	});

	test('a change after saving is written to the saved workspace, not to the one it was made in', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		workspace.saveAs('Review');
		const key = fakeSettings.workspaces.custom[0].id;
		jest.advanceTimersByTime(100);
		dock.drag();
		jest.advanceTimersByTime(600);
		expect(sameArrangement(fakeSettings.workspaces.layouts[key]!, dock.current)).toBe(true);
		expect(fakeSettings.workspaces.layouts.edit).toBeUndefined();
	});

	test('switching to one builds its layout; switching back builds the one it came from', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		dock.drag();
		workspace.saveAs('Review');
		const key = workspace.shown;
		jest.advanceTimersByTime(100);
		workspace.switchTo('color');
		expect(sameArrangement(dock.current, PRESET_LAYOUTS.color)).toBe(true);
		jest.advanceTimersByTime(100);
		workspace.switchTo(key);
		expect(sameArrangement(dock.current, fakeSettings.workspaces.layouts[key]!)).toBe(true);
	});

	test('a button that is not there cannot be switched to', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		workspace.switchTo('w-nobody');
		expect(workspace.shown).toBe('edit');
	});

	test('renaming changes the button and says why when it cannot', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		workspace.saveAs('Review');
		const key = workspace.shown;
		expect(workspace.rename(key, 'Rough cut')).toBeNull();
		expect(fakeSettings.workspaces.custom[0].name).toBe('Rough cut');
		expect(workspace.rename(key, 'Audio')).toContain('already a workspace called Audio');
		expect(fakeSettings.workspaces.custom[0].name).toBe('Rough cut');
	});

	test('deleting the one on screen builds the one beside it and drops its pending write', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		workspace.saveAs('Review');
		const key = workspace.shown;
		jest.advanceTimersByTime(100);
		dock.drag(); // pending on the debounce
		toasts.length = 0;
		workspace.remove(key);
		jest.advanceTimersByTime(2000);
		expect(fakeSettings.workspaces.custom).toEqual([]);
		expect(fakeSettings.workspaces.layouts[key]).toBeUndefined();
		expect(workspace.shown).toBe('deliver');
		expect(sameArrangement(dock.current, PRESET_LAYOUTS.deliver)).toBe(true);
		expect(toasts).toEqual([['success', 'Deleted the Review workspace']]);
	});

	test('deleting another one leaves the dock as it is', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		workspace.saveAs('Review');
		const first = workspace.shown;
		jest.advanceTimersByTime(100);
		workspace.switchTo('edit');
		jest.advanceTimersByTime(100);
		const built = dock.fromJSONCalls.length;
		workspace.remove(first);
		expect(workspace.shown).toBe('edit');
		expect(dock.fromJSONCalls).toHaveLength(built);
	});

	test('a built-in workspace cannot be deleted', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		workspace.remove('edit');
		expect(workspace.shown).toBe('edit');
		expect(toasts).toEqual([]);
	});

	test('resetting a saved workspace is declined, with the way out, and touches nothing', async () => {
		const { workspace, dock } = await boot(defaultWorkspaces());
		dock.drag();
		workspace.saveAs('Review');
		const key = workspace.shown;
		const before = clone(fakeSettings.workspaces.layouts[key]);
		toasts.length = 0;
		workspace.reset();
		expect(toasts[0][0]).toBe('info');
		expect(toasts[0][1]).toContain('Review');
		expect(fakeSettings.workspaces.layouts[key]).toEqual(before);
		expect(sameArrangement(dock.current, before)).toBe(true);
	});

	test('resetting all workspaces keeps the saved ones and their pending change, and does not rebuild the dock', async () => {
		let state = withLayout(defaultWorkspaces(), 'color', arranged('color'));
		state = addWorkspace(state, 'w-aaaa1111', 'Review', arranged('edit'), 'media');
		const { workspace, dock } = await boot(state);
		const built = dock.fromJSONCalls.length;
		dock.drag(); // on the debounce, for the saved workspace
		workspace.resetAll();
		expect(fakeSettings.workspaces.layouts.color).toBeUndefined();
		expect(toasts).toEqual([['success', 'Edit, Color, Audio, Motion and Deliver reset to their default arrangements; yours are kept']]);
		expect(fakeSettings.workspaces.custom.map((c) => c.id)).toEqual(['w-aaaa1111']);
		expect(sameArrangement(fakeSettings.workspaces.layouts['w-aaaa1111']!, dock.current)).toBe(true);
		expect(dock.fromJSONCalls).toHaveLength(built);
	});

	test('the buttons are moved in the settings and the order is the stored one', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		workspace.move('deliver', 0);
		expect(fakeSettings.workspaces.order).toEqual(['deliver', 'edit', 'color', 'audio', 'motion']);
	});

	test('the next and previous workspace follow the buttons, saved ones included', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		workspace.saveAs('Review');
		const key = workspace.shown;
		jest.advanceTimersByTime(100);
		workspace.step(1);
		expect(workspace.shown).toBe('edit');
		jest.advanceTimersByTime(100);
		workspace.step(-1);
		expect(workspace.shown).toBe(key);
	});

	test('a saved workspace the dock cannot build falls back to Edit’s preset', async () => {
		const state = addWorkspace(defaultWorkspaces(), 'w-aaaa1111', 'Review', arranged('edit'), 'media');
		const { dock } = await boot({ ...state, active: 'edit' });
		const { workspace } = await import('./workspace.svelte');
		const quiet = spyOn(console, 'error').mockImplementation(() => {});
		// The dock takes the first thing it is given and refuses once.
		let refused = false;
		const real = dock.fromJSON.bind(dock);
		dock.fromJSON = (layout: any) => {
			if (!refused) {
				refused = true;
				throw new Error('refused');
			}
			real(layout);
		};
		workspace.switchTo('w-aaaa1111');
		quiet.mockRestore();
		expect(sameArrangement(dock.current, PRESET_LAYOUTS.edit)).toBe(true);
	});

	test('asking for a name opens the dialog, and says why not when there is no room', async () => {
		const { workspace } = await boot(defaultWorkspaces());
		workspace.askToSave();
		expect(workspace.naming).toEqual({ mode: 'save' });
		workspace.naming = null;
		let full = defaultWorkspaces();
		for (let i = 0; i < 5; i++) full = addWorkspace(full, `w-key${i}abc`, `W${i}`, arranged('edit'), 'media');
		const again = await boot({ ...full, active: 'edit' });
		again.workspace.askToSave();
		expect(again.workspace.naming).toBeNull();
		expect(toasts[0][0]).toBe('info');
		expect(toasts[0][1]).toContain('5 workspaces');
	});

	test('only a saved workspace can be asked to be renamed', async () => {
		const state = addWorkspace(defaultWorkspaces(), 'w-aaaa1111', 'Review', arranged('edit'), 'media');
		const { workspace } = await boot({ ...state, active: 'edit' });
		workspace.askToRename('audio');
		expect(workspace.naming).toBeNull();
		workspace.askToRename('w-aaaa1111');
		expect(workspace.naming).toEqual({ mode: 'rename', key: 'w-aaaa1111' });
	});
});

// ---- detached windows ---------------------------------------------------------

/** The Edit preset with its inspector group in a window, as dockview writes it. */
function detached(): any {
	const l = clone(PRESET_LAYOUTS.edit);
	const visit = (n: any): any => (n.type === 'leaf' ? (n.data.id === 'inspector' ? n : undefined) : n.data.map(visit).find(Boolean));
	const leaf = visit(l.grid.root);
	const views = leaf.data.views;
	leaf.data.views = [];
	delete leaf.data.activeView;
	leaf.visible = false;
	l.popoutGroups = [
		{ data: { id: 'win-1', views, activeView: views[0] }, gridReferenceGroup: 'inspector', position: { left: 2400, top: 90, width: 640, height: 700 }, url: '/popout.html' }
	];
	return l;
}

const flush = async () => {
	for (let i = 0; i < 6; i++) await Promise.resolve();
};

describe('a workspace with a panel in a window of its own', () => {
	test('announces its windows, in order, and builds the dock once that is done', async () => {
		const { popout } = await import('./popout.svelte');
		let release!: () => void;
		const announce = spyOn(popout, 'announce').mockImplementation(() => new Promise<void>((done) => (release = done)));
		const settle = spyOn(popout, 'settle').mockImplementation(() => {});
		try {
			const { dock } = await boot(withLayout(defaultWorkspaces(), 'edit', detached()));
			expect(announce).toHaveBeenCalledTimes(1);
			expect(announce.mock.calls[0][0]).toEqual([{ left: 2400, top: 90, width: 640, height: 700 }]);
			expect(dock.fromJSONCalls).toHaveLength(0);
			await flush();
			expect(dock.fromJSONCalls).toHaveLength(0);
			release();
			await flush();
			expect(dock.fromJSONCalls).toHaveLength(1);
			expect(dock.fromJSONCalls[0].popoutGroups).toHaveLength(1);
			// The windows are up: what was announced and not taken is let go.
			await flush();
			expect(settle).toHaveBeenCalled();
		} finally {
			announce.mockRestore();
			settle.mockRestore();
		}
	});

	test('a workspace without windows is built at once, with nothing announced', async () => {
		const { popout } = await import('./popout.svelte');
		const announce = spyOn(popout, 'announce').mockResolvedValue();
		try {
			const { dock } = await boot(defaultWorkspaces());
			expect(announce).not.toHaveBeenCalled();
			expect(dock.fromJSONCalls).toHaveLength(1);
		} finally {
			announce.mockRestore();
		}
	});

	test('writes nothing while its windows are still opening, and the arrangement once they are up', async () => {
		const { popout } = await import('./popout.svelte');
		const announce = spyOn(popout, 'announce').mockResolvedValue();
		const settle = spyOn(popout, 'settle').mockImplementation(() => {});
		try {
			let up!: () => void;
			const { dock } = await boot(withLayout(defaultWorkspaces(), 'edit', detached()));
			dock.popoutRestorationPromise = new Promise<void>((done) => (up = done));
			await flush();
			// dockview reports each window opening as a layout change; none is the user's.
			dock.changed();
			jest.advanceTimersByTime(2000);
			expect(fakeSettings.writes).toBe(0);
			up();
			await flush();
			jest.advanceTimersByTime(100);
			dock.drag();
			jest.advanceTimersByTime(600);
			expect(fakeSettings.workspaces.layouts.edit?.popoutGroups).toHaveLength(1);
		} finally {
			announce.mockRestore();
			settle.mockRestore();
		}
	});

	test('a window that never loads does not keep the dock from saving for good', async () => {
		const { popout } = await import('./popout.svelte');
		const announce = spyOn(popout, 'announce').mockResolvedValue();
		const settle = spyOn(popout, 'settle').mockImplementation(() => {});
		try {
			const { dock } = await boot(withLayout(defaultWorkspaces(), 'edit', detached()));
			await flush();
			dock.popoutRestorationPromise = new Promise<void>(() => {});
			// (the dock above was built with the resolved promise; give the next restore a hung one)
			const { workspace } = await import('./workspace.svelte');
			workspace.switchTo('color');
			workspace.switchTo('edit');
			await flush();
			jest.advanceTimersByTime(9000);
			await flush();
			jest.advanceTimersByTime(100);
			dock.drag();
			jest.advanceTimersByTime(600);
			expect(fakeSettings.writes).toBeGreaterThan(0);
		} finally {
			announce.mockRestore();
			settle.mockRestore();
		}
	});

	test('choosing another workspace before the windows are announced builds only that one', async () => {
		const { popout } = await import('./popout.svelte');
		let release!: () => void;
		const announce = spyOn(popout, 'announce').mockImplementation(() => new Promise<void>((done) => (release = done)));
		try {
			const state = withLayout(withLayout(defaultWorkspaces(), 'edit', detached()), 'color', arranged('color'));
			const { workspace, dock } = await boot(state);
			workspace.switchTo('color');
			expect(dock.fromJSONCalls).toHaveLength(1);
			expect(sameArrangement(dock.current, arranged('color'))).toBe(true);
			release();
			await flush();
			// The overtaken restore did nothing.
			expect(dock.fromJSONCalls).toHaveLength(1);
			expect(sameArrangement(dock.current, arranged('color'))).toBe(true);
		} finally {
			announce.mockRestore();
		}
	});
});
