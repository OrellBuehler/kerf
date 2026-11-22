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
	test('picks its scope with the kit’s chips, so the arrows work', () => {
		expect(scopes).toContain("import ChoiceChips from './ChoiceChips.svelte'");
		expect(scopes).toContain('<ChoiceChips');
		expect(scopes).not.toContain('role="radio"');
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
