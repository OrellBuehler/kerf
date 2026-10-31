import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC, posix, uiSources } from './test-sources';

// An edit in flight used to switch a control to native `disabled`, and a disabled element drops keyboard focus
// — the key that was about to press the next control went nowhere. The Inspector's controls take `pending`
// (`aria-disabled` + a swallowed press) instead, so focus stays where the user left it.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

/** Each start tag of the given elements and components, whole: `>` inside a `{…}` (an arrow function) does not end one. */
function tags(source: string, names: string[]): string[] {
	const out: string[] = [];
	for (const m of source.matchAll(new RegExp(`<(?:${names.join('|')})\\b`, 'g'))) {
		let depth = 0;
		let end = m.index!;
		for (; end < source.length; end++) {
			const c = source[end];
			if (c === '{') depth++;
			else if (c === '}') depth--;
			else if (c === '>' && depth === 0) break;
		}
		out.push(source.slice(m.index!, end + 1));
	}
	return out;
}

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

	test('no control anywhere is switched to native disabled by an edit in flight or a proposal on screen', () => {
		// The native elements and the atoms whose `disabled` is the DOM's. (`SliderRow`, `SfxShape` and `MixSlider` take a
		// `disabled` of their own that is read-only + aria-disabled, so `disabled={editor.locked}` on one is right.)
		const natives = ['button', 'input', 'select', 'textarea', 'Btn', 'IconBtn', 'Chip', 'Tile', 'ToolbarToggle', 'CheckRow', 'Switch'];
		// The timeline's track header and its "Add a title" still do (UI-21, the timeline's own pass).
		const later = new Set(['lib/components/editor/Timeline.svelte']);
		const found: string[] = [];
		for (const file of uiSources().filter((f) => f.endsWith('.svelte') && !later.has(posix(f)))) {
			for (const tag of tags(readFileSync(file, 'utf8'), natives)) {
				if (/(?<![-\w])disabled=\{[^}]*editor\.(busy|locked|previewingStaged)/.test(tag)) found.push(`${posix(file)}: ${tag.replace(/\s+/g, ' ').slice(0, 90)}`);
			}
		}
		expect(found).toEqual([]);
	});

	test('the Audio tab’s presets are pending for an edit and disabled only for a real reason', () => {
		const tab = read(`${EDITOR}AudioTab.svelte`);
		expect(tab).not.toMatch(/const off = /);
		expect(tab).toContain('disabled={reason !== null}');
		expect(tab).toContain('pending={hold}');
		expect(tab).toContain('const hold = $derived(editor.locked)');
	});

	test('the thumbnail’s range, colour box and template menu are aria-disabled and swallow the press, not disabled', () => {
		const section = read(`${EDITOR}ThumbnailSection.svelte`);
		const controls = tags(section, ['input', 'select']).filter((t) => /type="range"|type="color"|aria-label="Add a template layer"/.test(t));
		expect(controls).toHaveLength(3);
		for (const t of controls) {
			expect(t).toContain('aria-disabled={off || undefined}');
			expect(t).toMatch(/onmousedown=\{swallow\}|onclick=\{swallow\}/);
			expect(t).not.toMatch(/(?<![-\w])disabled=/);
		}
		// Every field that is read-only for an edit in flight says so too.
		for (const t of tags(section, ['input', 'textarea']).filter((x) => x.includes('readonly={editor.locked}'))) expect(t).toContain('aria-disabled={editor.locked || undefined}');
	});

	test('the slider’s number box is read-only while it is off, so it can keep focus too', () => {
		const row = read(`${EDITOR}SliderRow.svelte`);
		expect(row).toContain('readonly={disabled}');
		expect(row).toContain("aria-disabled={disabled || undefined}");
	});
});
