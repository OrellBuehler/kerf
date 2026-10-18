import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';

// An edit in flight used to switch a control to native `disabled`, and a disabled element drops keyboard focus
// — the key that was about to press the next control went nowhere. The Inspector's controls take `pending`
// (`aria-disabled` + a swallowed press) instead, so focus stays where the user left it.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

describe('a control that is busy keeps focus', () => {
	test('the atoms take `pending`, render aria-disabled, and ignore the press', () => {
		for (const file of ['Btn', 'IconBtn', 'Chip', 'Tile', 'CheckRow']) {
			const source = read(`${EDITOR}${file}.svelte`);
			expect(source, file).toContain('pending');
			expect(source, file).toContain("aria-disabled={pending ?");
		}
		for (const file of ['Btn', 'IconBtn', 'Chip', 'Tile']) expect(read(`${EDITOR}${file}.svelte`), file).toContain('if (pending) return;');
	});

	test('a pending control is drawn off by the same rules as a disabled one, and not hovered', () => {
		const css = read('lib/styles/controls.css');
		for (const cls of ['k-ibtn', 'k-chip', 'k-tile']) {
			expect(css, cls).toContain(`.${cls}[aria-disabled='true']`);
			expect(css, cls).toContain(`.${cls}:hover:not(:disabled):not([aria-disabled='true'])`);
		}
		expect(css).toContain(".k-check:has(input[aria-disabled='true'])");
	});

	test('the Inspector, the effects and the transitions never switch a control to native disabled for an edit in flight', () => {
		for (const file of ['Inspector', 'AssetInspector', 'EffectsTab', 'TransitionsTab', 'TitlesControls']) {
			const source = read(`${EDITOR}${file}.svelte`);
			expect(source, file).not.toMatch(/disabled=\{editor\.busy/);
			expect(source, file).not.toMatch(/disabled=\{off\}\s*\n?\s*onclick/); // a real reason, not an edit in flight, may disable
		}
		expect(read(`${EDITOR}Inspector.svelte`)).toContain('pending={editor.locked}');
		expect(read(`${EDITOR}Inspector.svelte`)).toContain('inert={editor.previewingStaged}');
	});

	test('the slider’s number box is read-only while it is off, so it can keep focus too', () => {
		const row = read(`${EDITOR}SliderRow.svelte`);
		expect(row).toContain('readonly={disabled}');
		expect(row).toContain("aria-disabled={disabled || undefined}");
	});
});
