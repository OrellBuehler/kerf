import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// Wiring of the read-only proposal preview that only a window shows: which components say a refusal as a notice, which
// controls are held, which keys the banner names. The behaviour that can run without a window is tested where it lives
// (`ops-preview.test.ts`, `menus.test.ts`, `menu-items.test.ts`, `state-proposal.test.ts`).

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

/** The text of one `function name(` up to the next function at the same indent (or the end of the file). */
function body(source: string, name: string): string {
	const at = source.search(new RegExp(`(?:async )?function ${name}\\(`));
	expect(at, name).toBeGreaterThanOrEqual(0);
	const rest = source.slice(at + 1);
	const next = rest.search(/\n\t(?:async )?function /);
	return next < 0 ? rest : rest.slice(0, next);
}

describe('a refusal because the proposal is previewed is a notice, not a red toast', () => {
	test('the components that run an edit report through `report`, not a toast of their own', () => {
		for (const file of ['Inspector', 'Timeline', 'MediaBin', 'MixerStrip', 'MasterStrip', 'TranscriptPanel', 'AssetInspector', 'PreviewEmpty', 'FitMusicDialog', 'Preview']) {
			const source = read(`${EDITOR}${file}.svelte`);
			// (The transcript’s Transcribe button is analysis, not an edit, and keeps its own error toast.)
			if (file !== 'TranscriptPanel') expect(source, file).not.toMatch(/toast\.error\((?:e|err) instanceof Error \? (?:e|err)\.message : String\((?:e|err)\)\)/);
			expect(source, file).toMatch(/import \{[^}]*\breport\b[^}]*\} from '\#lib\/ops'/);
		}
		for (const file of ['title-actions.ts', 'menu-commands.ts']) expect(read(`lib/${file}`), file).toMatch(/\breport\b/);
	});

	test('the page says the same for the keys and the toasts that nobody caught', () => {
		const page = read('routes/+page.svelte');
		expect(page).not.toContain('clipErr');
		expect(page).toContain('.catch(report)');
		expect(page).toContain('if (ev.reason instanceof PreviewReadOnly) toast.info(ev.reason.message);');
	});
});

describe('the keys and menus that edit stop before they plan, copy or ask', () => {
	test('the page holds undo, redo and the overlays’ delete itself; paste, duplicate and marker are the shared actions', () => {
		const page = read('routes/+page.svelte');
		const run = page.slice(page.indexOf('const run: Record<ActionId'));
		expect(run).toMatch(/'edit\.undo': \(\) => \{\s*if \(holdForPreview\(\)\) return;/);
		expect(run).toMatch(/'edit\.redo': \(\) => \{\s*if \(holdForPreview\(\)\) return;/);
		expect(run).toContain("'edit.paste': () => void pasteClips()");
		expect(run).toContain("'edit.duplicate': () => void duplicateClips()");
		expect(run).toContain("'marker.add': () => void addMarker()");
		expect(page).toMatch(/if \(editor\.selectedOverlayId\) \{\s*if \(holdForPreview\(\)\) return;/);
		expect(page).toMatch(/else if \(editor\.selectedSvgOverlayId\) \{\s*if \(holdForPreview\(\)\) return;/);
	});

	test('the quick edits are held before anything is analyzed or read, in the panel and in its right-click menu', () => {
		const panel = read(`${EDITOR}AgentPanel.svelte`);
		const run = body(panel, 'runPreset');
		expect(run.indexOf('holdForPreview()')).toBeGreaterThan(0);
		expect(run.indexOf('holdForPreview()')).toBeLessThan(run.indexOf('ensureAnalysis'));
		expect(run.indexOf('holdForPreview()')).toBeLessThan(run.indexOf('editor.timeline'));
		const menu = body(panel, 'onAgentContextMenu');
		expect(menu).toContain('heldWhilePreviewing(');
		expect(menu).toContain('QUICK_EDIT_HELD');
	});

	test('the timeline’s menus and toolbar hold what edits, and its track faders go off', () => {
		const tl = read(`${EDITOR}Timeline.svelte`);
		expect(tl).not.toContain('PREVIEW_READ_ONLY }');
		expect(tl.match(/\.\.\.held\(/g)?.length).toBeGreaterThanOrEqual(10);
		for (const label of ["'Undo'", "'Redo'"]) expect(tl).toContain(label);
		expect(tl).toContain('!editor.canUndo || editor.previewingStaged');
		expect(tl).toContain('!editor.canRedo || editor.previewingStaged');
		expect(tl.match(/disabled=\{editor\.locked\}/g)?.length).toBeGreaterThanOrEqual(2);
		expect(body(tl, 'onLaneDrop')).toContain('editor.previewingStaged');
	});

	test('a press held for the preview ends with the press: the pointer going up, being cancelled or the window blurring', () => {
		const tl = read(`${EDITOR}Timeline.svelte`);
		expect(body(tl, 'onPointerUp')).toContain('previewPress = null');
		expect(body(tl, 'resetDragState')).toContain('previewPress = null');
	});

	test('the preview draws no placement boxes over a cut it will not let you move', () => {
		const preview = read(`${EDITOR}Preview.svelte`);
		expect(preview).toMatch(/ui\.playing \|\| empty \|\| editor\.previewingStaged/);
		expect(preview).toMatch(/!empty && !editor\.previewingStaged/);
	});
});

describe('a pending control is pending whatever else its caller says', () => {
	test('Btn and IconBtn set aria-disabled after the caller’s attributes, so `pending` cannot be undone by one', () => {
		for (const file of ['Btn', 'IconBtn']) {
			const source = read(`${EDITOR}${file}.svelte`);
			expect(source.indexOf("aria-disabled={pending ? 'true'"), file).toBeGreaterThan(source.indexOf('{...rest}'));
		}
	});

	test('the Inspector’s Copy and Paste keyframes are pending for both reasons through `pending`, not a second aria-disabled', () => {
		const source = read(`${EDITOR}Inspector.svelte`);
		expect(source).toContain('pending={editor.locked || offer.copy.disabled}');
		expect(source).toContain('pending={editor.locked || offer.paste.disabled}');
		expect(source).not.toMatch(/aria-disabled=\{offer\./);
	});

	test('a select that is only pending still takes keys: every handler of one writes nothing while the cut is locked', () => {
		const source = read(`${EDITOR}Inspector.svelte`);
		const selects = source.match(/<select\s[\s\S]*?<\/select>/g) ?? [];
		const pending = selects.filter((s) => s.includes('aria-disabled={editor.locked'));
		expect(pending.length).toBeGreaterThanOrEqual(6);
		for (const s of pending) {
			const handler = s.slice(s.indexOf('onchange='), s.indexOf('onchange=') + 400);
			expect(handler, s.slice(0, 120)).toMatch(/editor\.locked|patchTitle\(|patchSvg\(|setEasing\(/);
		}
		for (const name of ['patchTitle', 'patchSvg', 'setEasing']) expect(body(source, name), name).toContain('editor.locked');
	});

	test('and the colour boxes, whose picker is swallowed but whose change is not, go through the same guard', () => {
		const source = read(`${EDITOR}Inspector.svelte`);
		const swatches = source.match(/<input\s+type="color"[\s\S]*?\/>/g) ?? [];
		expect(swatches.length).toBeGreaterThanOrEqual(2);
		for (const s of swatches) expect(s).toMatch(/patchTitle\(|patchSvg\(/);
	});
});

describe('the title bar and the banner name what is waiting', () => {
	test('the banner’s tooltip is made of the keys the user has, not a hand-written pair', () => {
		const page = read('routes/+page.svelte');
		const code = page.split('\n').filter((l) => !l.trim().startsWith('//')).join('\n');
		expect(code).not.toContain('Page Up / Page Down');
		expect(page).toContain("reviewStepTip(reviewPoints.length, editor.staged.diff.entries.length, settings.keysFor('review.prevChange'), settings.keysFor('review.nextChange'))");
	});
});
