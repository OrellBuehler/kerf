import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, jest, mock, spyOn, test } from 'bun:test';
import './test-runes';
import { PRESET_LAYOUTS, presetLayout, sameArrangement } from './layout';
import {
	defaultWorkspaces,
	libraryTabFor,
	withLayout,
	withLibraryTab,
	withoutWorkspaces,
	type WorkspaceId,
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
	saveWorkspaceLayout(id: WorkspaceId, layout: never) {
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
	setActiveWorkspace(id: WorkspaceId) {
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
