import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// What the Keyframes panel, its graph and the Scopes panel promise in their markup, held where a refactor would lose
// it: a keyboard model, targets a finger can hit, a vocabulary that is the Inspector's, and the kit's own controls.

const read = (name: string) => readFileSync(join(import.meta.dir, 'components/editor', name), 'utf8');
const sheet = read('DopeSheet.svelte');
const graph = read('DopeGraph.svelte');
const ramp = read('SpeedRamp.svelte');
const scopes = read('Scopes.svelte');

describe('the Keyframes panel', () => {
	test('names its rows from the Inspector’s table and draws no glyph where an icon belongs', () => {
		expect(sheet).toContain("from '#lib/property-labels'");
		expect(sheet).not.toContain('◇');
		expect(graph).not.toContain('◇');
	});

	test('the bar holds controls, not the status that used to be truncated out of it', () => {
		const bar = /<div class="bar">[\s\S]*?<\/span>\s*<\/div>/.exec(sheet)?.[0] ?? '';
		expect(bar.length).toBeGreaterThan(100);
		expect(bar).not.toContain('animated');
		expect(bar).not.toContain('No keyframes');
		expect(sheet).toContain('class="corner"');
	});

	test('the empty state has an action, and keys are never put while the playhead is off the clip', () => {
		expect(sheet).toMatch(/<EmptyState[\s\S]*?<Btn[\s\S]*?Key the pose here/);
		expect(sheet).toContain('Go to the clip');
		expect(sheet.match(/keyRefusal\(/g)?.length).toBeGreaterThanOrEqual(3);
	});

	test('a run of key presses is one edit that Escape and an undo chord drop', () => {
		expect(sheet).toContain('KEY_COMMIT_MS');
		expect(sheet).toContain("action === 'edit.undo'");
		expect(sheet).toMatch(/Escape[\s\S]{0,200}dropRun\(\)/);
		expect(sheet).toContain('nudgeOf(what)');
		// held inside the clip and refused where it would land on a key (`fitRun`)
		expect(sheet).toContain('fitRun(');
	});

	test('a run is taken and cleared before it is written, so two commits write once', () => {
		const finish = /async function finishRun\(\) \{[\s\S]*?\n\t\}/.exec(sheet)?.[0] ?? '';
		expect(finish).toMatch(/const r = pending;/);
		expect(finish.indexOf('setRun(null)')).toBeGreaterThan(finish.indexOf('const r = pending;'));
		expect(finish.indexOf('setRun(null)')).toBeLessThan(finish.indexOf('write('));
	});

	test('a write puts its selection only on the clip it was written to', () => {
		expect(sheet).toMatch(/writing === mine && editor\.selectedClip\?\.id === c\.id/);
	});

	test('focus stays in the panel when a write takes its element away: keys have stable names, and the panel puts it back', () => {
		expect(sheet).toContain('(ids[j])');
		expect(sheet).toContain('keyIds(');
		expect(sheet).toContain('restoreFocus');
		expect(sheet).toContain('onfocusin');
		expect(graph).toContain('(ids[j])');
		expect(graph).toContain('data-focus-prop');
	});

	test('a key’s glyph wins over a handle under it, and a handle tap selects the key', () => {
		expect(graph).toContain('keyUnder(');
		expect(graph).toMatch(/under\.d < toHandle/);
		expect(graph).toMatch(/else if \(tap\) onpick\(/);
	});

	test('the lane clips its own contents, so focusing a key cannot scroll the sheet sideways', () => {
		expect(sheet).toMatch(/\.lanes\s*\{[^}]*overflow: clip;/);
		expect(sheet).toContain('scrollLeft');
	});

	test('a pasted key is one the clip has, and the paste says why when none is', () => {
		expect(sheet).toContain('clipProperties(');
		expect(sheet).toContain('fitBoard(');
		expect(sheet).toContain('pasteWhy');
	});

	test('Enter that writes a run is not also the key’s own press', () => {
		expect(sheet).toMatch(/pending && e\.key === 'Enter'/);
	});

	test('a key is named by its easing', () => {
		expect(sheet).toContain('easingName(');
		expect(graph).toContain('easingName(');
	});

	test('the speed ramp is a row of marks that only seek and a label that opens the Inspector', () => {
		expect(sheet).toContain('data-ramp');
		expect(sheet).toContain('revealSection');
		expect(sheet).not.toContain('setSpeedRamp');
	});

	test('every key and handle is 24 px to hit', () => {
		expect(graph).toMatch(/const HIT = 24;/);
		expect(graph).toMatch(/\.key,\s*\.handle\s*\{[^}]*width: 24px;[^}]*height: 24px;/);
		expect(ramp).toMatch(/const HIT = 24;/);
		expect(ramp).toMatch(/\.key\s*\{[^}]*width: 24px;[^}]*height: 24px;/);
		// the sheet's 14 px diamond grows by a pseudo-element: 14 + 2 * 5
		expect(sheet).toMatch(/\.key::after\s*\{[^}]*inset: -5px;/);
	});

	test('strokes use the line tokens, not a literal width', () => {
		expect(graph).not.toMatch(/\d+(\.\d+)?px solid/);
		expect(graph).not.toContain('1.5px');
	});
});

describe('the Scopes panel', () => {
	test('picks its scope with the kit’s chips, and with a roving icon group where no word fits', () => {
		expect(scopes).toContain("import ChoiceChips from './ChoiceChips.svelte'");
		expect(scopes).toContain('<ChoiceChips');
		// no hand-built radio buttons: the icon tier is `IconBtn`s under `roving`, one tab stop
		expect(scopes).not.toMatch(/<button[^>]*role="radio"/);
		expect(scopes).toContain('use:roving');
		expect(scopes).toContain('tabindex={i === stop ? 0 : -1}');
	});

	test('draws again when the display’s pixel ratio changes, and lets go of its listener', () => {
		expect(scopes).toContain('dprQuery(');
		expect(scopes).toContain("addEventListener('change', read)");
		expect(scopes).toContain("removeEventListener('change', read)");
	});

	test('draws its text in the theme’s mono face at the scale’s smallest step', () => {
		expect(scopes).not.toMatch(/sans-serif/);
		expect(scopes).toContain("'--text-2xs'");
		expect(scopes).toContain("'--font-mono'");
	});

	test('asks nothing of the backend where there is no picture', () => {
		expect(scopes).toMatch(/subject !== 'picture'/);
	});
});
