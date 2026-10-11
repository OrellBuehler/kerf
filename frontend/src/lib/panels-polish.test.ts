import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// The review's panel findings that were a few lines of markup each, and came back whenever someone
// pasted a neighbour: a tile that ran into its border, a review card whose Apply was below the fold,
// a master fader squeezed under three controls, the 360 override on every flat clip. These hold the
// shapes that fixed them; the numbers (a fader's travel, a summary's words) are tested beside the
// pure modules (`master-fx`, `inspector-summary`, `agent-status`, `first-run`).

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';
const CONTROLS = read('lib/styles/controls.css').replace(/\/\*[\s\S]*?\*\//g, '');

/** The declarations of the first rule for `selector` in a stylesheet. */
function rule(css: string, selector: string): string {
	const at = css.indexOf(`${selector} {`);
	expect(at, `${selector} is not in the stylesheet`).toBeGreaterThan(-1);
	return css.slice(css.indexOf('{', at) + 1, css.indexOf('}', at));
}

describe('the library', () => {
	test('a tile is one line that ends in an ellipsis, in a grid cell wide enough for "Compressor"', () => {
		const tile = rule(CONTROLS, '.k-tile');
		expect(tile).toContain('white-space: nowrap');
		expect(tile).toContain('text-overflow: ellipsis');
		for (const tab of ['EffectsTab', 'AudioTab']) {
			const source = read(`${EDITOR}${tab}.svelte`);
			expect(source, tab).toContain('<Tile');
			expect(source, tab).toMatch(/minmax\((9\d|1\d\d)px,1fr\)/);
			expect(source, tab).not.toContain('minmax(76px');
		}
	});

	test('the Titles controls are equal buttons in a grid, with the two caption looks the only chips', () => {
		const titles = read(`${EDITOR}TitlesControls.svelte`);
		expect(titles).toContain('grid-template-columns:repeat(2,minmax(0,1fr))');
		expect(titles).toContain('repeat(auto-fill,minmax(104px,1fr))');
		expect(titles).not.toMatch(/flex:1[^;"]*"[^>]*>\s*\+ Text/);
		expect((titles.match(/<Chip\b/g) ?? []).length).toBe(2);
	});

	test('the count and the ways in are the library’s header, not a second heading in the bin', () => {
		const panel = read(`${EDITOR}LibraryPanel.svelte`);
		expect(panel).toContain('assetCount');
		expect(panel).toContain("settings.tip('Import media…', 'file.import')");
		const bin = read(`${EDITOR}MediaBin.svelte`);
		expect(bin).not.toMatch(/\{assets\.length\} assets/);
		expect(bin).toContain('<MediaChips assetId={a.asset.id} compact />');
	});

	test('a bin row says what a badge means: the use count has a tooltip', () => {
		expect(read(`${EDITOR}MediaBin.svelte`)).toMatch(/Used in \$\{a\.info\.uses/);
	});

	test('the right-click menu of an asset is actions only: its facts are the Inspector’s', () => {
		const bin = read(`${EDITOR}MediaBin.svelte`);
		expect(bin).not.toContain("type: 'info'");
		expect(read(`${EDITOR}Inspector.svelte`)).toContain('<AssetInspector asset={editor.selectedAsset} />');
	});
});

describe('the agent panel', () => {
	const agent = read(`${EDITOR}AgentPanel.svelte`);

	test('the review card’s buttons are pinned to the bottom of the scroll, which needs the card clipped, not overflow:hidden', () => {
		const actions = /data-review-actions\s+style="([^"]*)"/.exec(agent);
		expect(actions, 'the review actions row').not.toBeNull();
		expect(actions![1]).toContain('position:sticky');
		expect(actions![1]).toContain('bottom:0');
		const card = /border-left:calc\(var\(--line-width\) \* 2\) solid var\(--agent-500\);([^"]*)"/.exec(agent);
		expect(card![1]).toContain('overflow:clip');
		expect(card![1]).not.toContain('overflow:hidden');
	});

	test('one status line, and no second "Agent queue" header over it', () => {
		expect(agent).toContain('agentLine(');
		expect(agent).not.toContain('Agent queue');
		expect(agent).not.toContain("working ? 'working' : 'idle'");
	});

	test('the composer is one 32 px row and its hint is the field’s description when the panel is short', () => {
		expect(agent).toContain('height:32px;padding:0 4px 0 10px');
		expect(agent).toContain('aria-describedby="agent-composer-hint"');
		expect(agent).toContain('showComposerHint(panelHeight)');
		expect(agent).toContain("'sr-only'");
	});

	test('the buttons are Btns on the one scale, not hand-drawn at 30 px', () => {
		expect(agent).not.toMatch(/height:30px/);
		expect(agent).not.toContain('border-radius:var(--radius-full)');
	});
});

describe('the shell says there is a proposal', () => {
	test('the dock sets a count on the Agent tab and the tab’s name says why', () => {
		const dock = read(`${EDITOR}Workspace.svelte`);
		expect(dock).toContain('proposalIndicator(editor.staged)');
		expect(dock).toContain('data-pending');
		expect(dock).toContain('.dv-tab[data-tab-panel-id="agent"]');
		expect(read('lib/styles/dockview-kerf.css')).toContain('.dv-default-tab-content[data-pending]::after');
	});

	test('the title bar has Export, with the keymap’s key, and a review chip that opens the Agent panel', () => {
		const bar = read(`${EDITOR}TitleBar.svelte`);
		expect(bar).toContain("settings.tip('Export…', 'file.export'");
		expect(bar).toContain('ui.openExport()');
		expect(bar).toContain('variant="primary"');
		expect(bar).toContain("workspace.show('agent')");
		expect(bar).toContain('proposalIndicator(editor.staged)');
	});
});

describe('the mixer', () => {
	const master = read(`${EDITOR}MasterStrip.svelte`);

	test('the limiter, ceiling and duck mode are inside the folded FX block, not stacked over the fader', () => {
		const open = master.indexOf('{#if fxOpen}');
		const fader = master.indexOf('<FaderMeter');
		expect(open).toBeGreaterThan(-1);
		expect(fader).toBeGreaterThan(open);
		const block = master.slice(open, fader);
		for (const part of ['class="limiter"', 'class="ceiling"', 'aria-label="Duck mode"']) expect(block, part).toContain(part);
		expect(master).toContain('loadMasterFxOpen()');
	});

	test('the depth slider only exists under the speech gate, and the footnote is gone', () => {
		expect(master).toMatch(/\{#if gate\}\s*<div class="depth">/);
		expect(read(`${EDITOR}Mixer.svelte`)).not.toContain('class="foot"');
	});

	test('a muted strip leaves its M / S / D at full strength', () => {
		expect(read(`${EDITOR}MixerStrip.svelte`)).toContain(':not(header):not(.toggles)');
	});
});

describe('the inspector', () => {
	const inspector = read(`${EDITOR}Inspector.svelte`);

	test('the 360 override is under Advanced, and only a clip with nothing 360 about it gets that section', () => {
		expect(inspector).toContain('title="Advanced"');
		expect(inspector).toContain('{#if !reframe && !sourceProjection}');
		expect(inspector).toContain('{#if picture && (reframe || sourceProjection)}');
		expect(inspector).not.toContain('Not detected as 360');
	});

	test('the Color section reads its summary from `colorSummary`, not "Custom" for a clip nobody touched', () => {
		expect(inspector).toContain('summary={colorSummary(clip, col)}');
		expect(inspector).not.toContain("?? 'Custom'");
	});

	test('the labels say what they are: Left / Right / Top / Bottom, Position X, Center, Transition', () => {
		for (const label of ["'Left'", "'Right'", "'Top'", "'Bottom'", "'Position X'", "'Center X'"]) expect(inspector, label).toContain(`rangeRow(${label}`);
		for (const gone of ["'Crop L'", "'Pos X'", "'Centre X'", 'Transition (in)']) expect(inspector, gone).not.toContain(gone);
	});

	test('a text field is set in the sans, and a field has one height', () => {
		expect(inspector).toContain('const textCss = `${FIELD};padding:0 8px`');
		expect(inspector).toMatch(/const FIELD = '[^']*height:var\(--control-sm\)/);
	});

	test('a checkbox is a CheckRow, box first', () => {
		expect(inspector).not.toContain('type="checkbox"');
	});
});

describe('the buttons', () => {
	test('a small Btn is 28 px, the height of every field beside it', () => {
		expect(rule(CONTROLS, ".k-btn[data-size='sm']")).toContain('--k-h: var(--control-sm)');
	});

	test('a chip is a toggle at that height, a tile too', () => {
		expect(rule(CONTROLS, '.k-chip')).toContain('height: var(--control-sm)');
		expect(rule(CONTROLS, '.k-tile')).toContain('height: var(--control-sm)');
	});
});
