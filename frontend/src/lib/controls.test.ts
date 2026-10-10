import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

// An inline `style=` cannot say `:hover`, and an inline background beats any stylesheet
// rule — so a control that carries its colors inline never reacts to the pointer (69 of
// 80 controls did not). The shared ones are classes in `styles/controls.css`; this holds
// the pieces together: the stylesheet has the states, the components use the classes and
// carry no colors of their own, and the old blanket rules are gone.

const SRC = join(import.meta.dir, '..');
const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
/** CSS without its comments, which are allowed to name what they replaced. */
const code = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, '');
const CONTROLS = code(read('lib/styles/controls.css'));
const EDITOR = 'lib/components/editor/';

/** Every selector of the stylesheet that mentions `needle`. */
const selectors = (css: string, needle: string): string[] =>
	[...css.matchAll(/(^|\})\s*([^{}@/][^{}]*)\{/g)].map((m) => m[2].trim()).filter((s) => s.includes(needle));

/** The opening tag of every <button> in a component's markup, as one string. */
const buttonTags = (svelte: string): string => [...svelte.matchAll(/<button\b[^>]*>/g)].map((m) => m[0]).join(' ');

describe('controls.css', () => {
	test('a Btn, an IconBtn and a workspace tab each have hover and pressed states', () => {
		for (const cls of ['.k-btn', '.k-ibtn', '.k-tab']) {
			const all = selectors(CONTROLS, cls);
			expect(all.some((s) => s.includes(':hover')), `${cls}:hover`).toBe(true);
			expect(all.some((s) => s.includes(':active')), `${cls}:active`).toBe(true);
		}
	});

	test('a Btn and an IconBtn have a disabled look of their own, not the blanket opacity', () => {
		for (const cls of ['.k-btn', '.k-ibtn']) {
			const rule = new RegExp(`${cls.replace('.', '\\.')}:disabled\\s*\\{([^}]*)\\}`).exec(CONTROLS);
			expect(rule, `${cls}:disabled`).not.toBeNull();
			expect(rule![1]).toContain('color: var(--text-disabled)');
			expect(rule![1]).toContain('opacity: 1');
			expect(rule![1]).toContain('cursor: default');
		}
	});

	test('on is an attribute, so the theme and the component agree on it', () => {
		const on = selectors(CONTROLS, '.k-ibtn').join(' ');
		for (const hook of ["[aria-pressed='true']", "[aria-selected='true']", '[data-on]']) expect(on).toContain(hook);
		expect(selectors(CONTROLS, '.k-tab').join(' ')).toContain("[aria-pressed='true']");
	});

	test('what is not a shared control gets the veil, and a disabled one a default cursor', () => {
		expect(CONTROLS).toMatch(/button:not\(:disabled\)[^{]*:hover[^{]*\{\s*box-shadow: var\(--hover-fill\)/);
		expect(CONTROLS).toMatch(/button:not\(:disabled\)[^{]*:active[^{]*\{\s*box-shadow: var\(--press-fill\)/);
		expect(CONTROLS).toMatch(/button:disabled\s*\{[^}]*cursor: default !important/);
		expect(CONTROLS).not.toContain('not-allowed');
	});

	test('the veil leaves the timeline clips and menu rows to themselves', () => {
		const veil = selectors(CONTROLS, 'button:not(:disabled)')[0];
		expect(veil).toContain(':not(.kclip)');
		expect(veil).toContain(":not([role^='menuitem'])");
		// dockview's close X and overflow menu have a hover of their own.
		expect(CONTROLS).toMatch(/\[role='button'\]:not\(\[aria-disabled='true'\]\):not\(\[class\*='dv-'\]\):hover/);
		expect(veil).toContain(":not([class*='dv-'])");
	});
});

describe('the shared components use them', () => {
	test('Btn, IconBtn and the workspace tabs take their colors from the classes', () => {
		const btn = read(`${EDITOR}Btn.svelte`);
		const icon = read(`${EDITOR}IconBtn.svelte`);
		const tabs = read(`${EDITOR}WorkspaceTabs.svelte`);
		expect(btn).toContain("'k-btn'");
		expect(icon).toContain("'k-ibtn'");
		expect(tabs).toContain('class="k-tab"');
		for (const [name, source] of [['Btn', btn], ['IconBtn', icon], ['WorkspaceTabs', tabs]]) {
			expect(buttonTags(source), `${name} carries a color inline`).not.toMatch(/(background|color|border)[a-z-]*:/);
		}
	});

	test('the title bar buttons and the library rail are .k-ibtn and carry no colors inline', () => {
		for (const file of ['TitleBar.svelte', 'LibraryPanel.svelte']) {
			const source = read(`${EDITOR}${file}`);
			expect(source, file).toContain('class="k-ibtn"');
		}
		const bar = read(`${EDITOR}TitleBar.svelte`);
		// The gear, bell and version chip: the idle color is the class's (text-secondary, not the
		// disabled grey they were drawn in), and an open dialog or a waiting update is an attribute.
		for (const hook of ['data-on=', 'data-accent=']) expect(bar).toContain(hook);
		expect(bar).not.toContain('--text-disabled');
		const rail = read(`${EDITOR}LibraryPanel.svelte`);
		expect(rail).toContain('data-tone="quiet"');
		expect(buttonTags(rail), 'the rail carries a color inline').not.toMatch(/(background|color|border)[a-z-]*:/);
	});

	test('an IconBtn is a toggle only when it is given `active`', () => {
		const icon = read(`${EDITOR}IconBtn.svelte`);
		expect(icon).toContain('active = undefined');
		expect(icon).toContain('aria-pressed={active}');
	});
});

describe('the old rules are gone', () => {
	const layout = code(read('routes/layout.css'));

	test('no blanket 50 % disabled style or red-circle cursor', () => {
		expect(layout).not.toMatch(/button:disabled\s*\{[^}]*opacity/);
		expect(layout).not.toContain('not-allowed');
	});

	test('one focus outline, from the token, for every focusable control', () => {
		expect(layout).toMatch(/:where\(button,[\s\S]*?\):focus-visible\s*\{\s*outline: var\(--focus-outline\);\s*outline-offset: var\(--focus-offset\)/);
		expect(layout).not.toContain('outline-ring/50');
		// The slider thumb draws the same color and width as a ring.
		const tokens = read('lib/styles/kerf-tokens.css');
		expect(tokens).toMatch(/--focus-ring: [^;]*var\(--focus-width\)[^;]*var\(--focus-color\)/);
		expect(tokens).toMatch(/--focus-outline: var\(--focus-width\) solid var\(--focus-color\)/);
		// Dock tabs and the menu bar draw it inset instead of outside.
		expect(read('lib/styles/dockview-kerf.css')).toContain('outline: var(--focus-outline) !important');
		expect(read(`${EDITOR}MenuBar.svelte`)).toContain('outline: var(--focus-outline)');
	});

	test('dock tabs react to the pointer', () => {
		const dock = read('lib/styles/dockview-kerf.css');
		expect(dock).toMatch(/\.dv-tab\.dv-inactive-tab:hover\s*\{[^}]*surface-hover/);
		expect(dock).toMatch(/\.dv-tab\.dv-inactive-tab:active\s*\{[^}]*surface-active/);
	});

	test('a control drawn in the disabled color is not also dimmed by the blanket opacity', () => {
		// "Mark all read" and "Clear" go `text-disabled` when they have nothing to act on; the blanket
		// `button:disabled` 50 % on top made them dimmer than a disabled control is meant to be.
		const log = read(`${EDITOR}NotificationCenter.svelte`);
		const buttons = [...log.matchAll(/<button\b[\s\S]*?>\s*(?:Mark all read|Clear)\s*<\/button/g)].map((m) => m[0]);
		expect(buttons.length).toBe(2);
		for (const tag of buttons) expect(tag, tag.slice(0, 80)).toContain('opacity:1');
	});

	test('a disabled context-menu row is not dimmed on top of its disabled color', () => {
		const menu = read(`${EDITOR}ContextMenu.svelte`);
		expect(menu).not.toMatch(/opacity:\{[^}]*0\.\d+/);
		expect(menu).toContain('opacity:1');
	});
});
