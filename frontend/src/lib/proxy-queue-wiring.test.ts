import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { buildMenus, entryLabel, flatten, type MenuState } from './menus';
import { ACTIONS, allowsRepeat } from './keymap';
import { PANELS } from './layout';

// How the proxy queue panel is wired into the components that have no test of their own: that it is a dock panel and a
// Window entry, that it reads the queue only while something is looking, that its destructive button asks first and is
// quiet, that a pointer click does not leave focus on a button, and that the four places that wait on a proxy lead to
// it. Read off the source, as `sfx-wiring.test.ts` does; the words and the rules are tested where they live
// (`proxy-queue`, `proxy-queue-state`, `confirm-copy`, `layout`, `menus`).

const read = (path: string) => readFileSync(join(import.meta.dir, path), 'utf8');
const EDITOR = 'components/editor/';
const panel = read(`${EDITOR}ProxyQueue.svelte`);
const workspaceComponent = read(`${EDITOR}Workspace.svelte`);
const preview = read(`${EDITOR}Preview.svelte`);
const statusBar = read(`${EDITOR}StatusBar.svelte`);
const chips = read(`${EDITOR}MediaChips.svelte`);
const state = read('proxy-queue.svelte.ts');
const page = read('../routes/+page.svelte');

const menuState: MenuState = {
	saved: false,
	canUndo: false,
	canRedo: false,
	hasClips: true,
	exporting: false,
	hasSelection: false,
	rippleMode: false,
	snap: true,
	minimap: true,
	safeAreas: false,
	motionPath: true,
	hasFrame: true,
	tool: 'pointer',
	delivery: 'source',
	allHeight: 'medium',
	workspace: 'edit',
	openPanels: ['preview', 'timeline'],
	detachedPanels: [],
	desktop: true,
	previewing: false,
	recent: []
};

describe('the panel is a dock panel', () => {
	test('is mounted by the workspace under its id, with its own registry entry', () => {
		expect(workspaceComponent).toContain("import ProxyQueue from './ProxyQueue.svelte';");
		expect(workspaceComponent).toContain('proxyQueue: ProxyQueue');
		expect(PANELS.proxyQueue.title).toBe('Proxy queue');
	});

	test('opens beside the preview, as the panels that are not the library do', () => {
		expect(read('workspace.svelte.ts')).toContain("proxyQueue: 'right'");
	});

	test('is in the Window menu as an action, so it has a key to bind and the menu prints it', () => {
		const win = buildMenus(menuState).find((m) => m.id === 'window')!;
		const entry = flatten(win.items).find((e) => e.kind === 'action' && e.id === 'window.proxyQueue');
		expect(entry && entryLabel(entry as never)).toBe('Proxy queue');
		expect(page).toContain("'window.proxyQueue': () => workspace.toggle('proxyQueue')");
		// A toggle that is one press, one run, and nothing is bound until the user binds it.
		expect(allowsRepeat('window.proxyQueue')).toBe(false);
		expect(ACTIONS.find((a) => a.id === 'window.proxyQueue')!.defaults).toEqual([]);
	});

	test('can go to a window of its own like any panel: the panel keeps to the editor window’s own', () => {
		// Anything it measures or listens to goes through the realm, never the global window / document.
		expect(panel).not.toMatch(/\b(?:window|document)\./);
		expect(panel).toContain('intersectionObserverFor(root');
	});
});

describe('the queue is read only while something looks', () => {
	test('the panel watches while it is on screen, and lets go when it is not', () => {
		expect(panel).toMatch(/\$effect\(\(\) => \{\s*if \(!visible\) return;\s*return proxyQueue\.watch\(\);\s*\}\);/);
	});

	test('the Preview watches while a waiting chip is drawn, and the status bar and the bin only open the panel', () => {
		expect(preview).toMatch(/\$effect\(\(\) => \{\s*if \(!waitingChip\) return;\s*return proxyQueue\.watch\(\);\s*\}\);/);
		expect(statusBar).not.toContain('proxyQueue.watch');
		expect(chips).not.toContain('proxyQueue.watch');
	});

	test('the state subscribes inside `watch`, never when it is made', () => {
		const body = state.slice(state.indexOf('export class ProxyQueueState'));
		expect(body.match(/onProxyQueueChanged\(/g)?.length).toBe(1);
		expect(body.match(/onProxyProgress\(/g)?.length).toBe(1);
		// both calls are in `#listen`, which only `watch` calls
		const listen = body.slice(body.indexOf('#listen() {'), body.indexOf('#unlisten() {'));
		expect(listen).toContain('onProxyQueueChanged(');
		expect(listen).toContain('onProxyProgress(');
		expect(body.match(/this\.#listen\(\)/g)?.length).toBe(1);
	});
});

describe('the buttons', () => {
	test('Cancel all is the destructive one, quiet, and asks first with copy that names its buttons', () => {
		const at = panel.indexOf('proxy-queue-cancel-all');
		const button = panel.slice(panel.lastIndexOf('<Btn', at), panel.indexOf('</Btn', at));
		expect(button).toContain('variant="destructive"');
		expect(button).toContain('data-quiet');
		expect(panel).toContain('cancelProxyQueueCopy(jobs)');
		expect(panel).toMatch(/confirmAction\(copy\.message, copy\.title, copy\)/);
		// nothing is cancelled before the answer
		expect(panel.indexOf('confirmAction(')).toBeLessThan(panel.indexOf('proxyQueue.cancelAll()'));
	});

	test('no other button is destructive: a row’s Cancel is an icon button that names its proxy', () => {
		expect(panel.match(/variant="destructive"/g)?.length).toBe(1);
		expect(panel).toContain('aria-label="Cancel the proxy of {row.name}"');
		expect(panel).toContain('aria-label="Build the proxy of {row.name} next"');
		expect(panel).toContain('aria-label="Rebuild the proxy of {f.name}"');
	});

	test('a pointer click gives focus back (a focused button swallows Space), and a key press keeps it', () => {
		expect(panel).toContain('if (e.detail > 0) (e.currentTarget as HTMLElement | null)?.blur();');
		// every handler that is a click starts by releasing
		for (const fn of ['build', 'cancel', 'rebuild', 'pause', 'cancelAll']) {
			const at = panel.indexOf(`function ${fn}(`);
			expect(at, fn).toBeGreaterThan(-1);
			expect(panel.slice(at, at + 260), fn).toContain('release(e)');
		}
	});

	test('a refusal is a notice through `report`, not a toast of its own', () => {
		expect(panel).toContain("import { report } from '#lib/ops';");
		expect(panel).not.toMatch(/toast\.error\(/);
	});

	test('a busy row is `pending` (it keeps focus), and only a real reason disables a button', () => {
		expect(panel).toContain('pending={proxyQueue.busy.has(row.id)}');
		expect(panel).toContain('pending={proxyQueue.working}');
		expect(panel.match(/disabled=\{/g)?.length).toBe(2);
	});
});

describe('the places that wait on a proxy lead to it', () => {
	test('the Preview’s chips are buttons that open it, and say how many are ahead', () => {
		expect(preview).toContain("workspace.show('proxyQueue')");
		expect(preview).toContain('data-testid="playback-waiting"');
		expect(preview).toContain('data-testid="proxy-waiting"');
		expect(preview.match(/onclick=\{openQueue\}/g)?.length).toBe(2);
		expect(preview).toContain('hudLine(proxyQueue.queue)');
		// the chip is a plain button on the scrim: no colour of its own
		expect(preview).toMatch(/button\.hud \{[^}]*border: 0;/);
	});

	test('the status bar’s source note is a button only while its proxy is in the queue', () => {
		expect(statusBar).toContain("workspace.show('proxyQueue')");
		expect(statusBar).toMatch(/\{#if sourceNote\.queue\}\s*<button/);
		expect(statusBar).toContain('data-queue');
	});

	test('the bin’s proxy badge is a button only while its proxy is in the queue, and no part of the row under it hears it', () => {
		expect(chips).toContain("workspace.show('proxyQueue')");
		expect(chips).toMatch(/\{#if inQueue\}\s*<button/);
		for (const handler of ['onpointerdown', 'onkeydown', 'e.stopPropagation()']) expect(chips, handler).toContain(handler);
		expect(chips).toContain('isQueued(mediaStatus.proxy(assetId)?.state)');
	});

	test('each says that it opens the queue, in the one sentence', () => {
		for (const [name, src] of [['Preview', preview], ['StatusBar', statusBar], ['MediaChips', chips]] as const) expect(src, name).toContain('OPEN_QUEUE_HINT');
	});
});
