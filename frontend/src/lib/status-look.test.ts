import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { STATUS_LOOK } from './status-look';
import { SRC } from './test-sources';

// A toast and the line it leaves in the notification log used to disagree: a circle-check /
// octagon-x / triangle / info on the toast, a check / triangle / lightbulb / bell in the log, the
// same triangle for an error and a warning. One list now (`status-look.ts`); this holds the
// stylesheet and the two components to it.

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const LAYOUT = read('routes/layout.css');
const KINDS = ['success', 'error', 'warning', 'info'] as const;

/** The declarations of the first rule whose selector ends with `selector`. */
function rule(css: string, selector: string): string {
	const at = css.indexOf(selector + ' {');
	expect(at, `${selector} is not in layout.css`).toBeGreaterThan(-1);
	return css.slice(css.indexOf('{', at) + 1, css.indexOf('}', at));
}

describe('status look', () => {
	test('every kind has a glyph the icon set has, and an error is not a warning', () => {
		const icons = read('lib/components/editor/icons.ts');
		for (const [kind, look] of Object.entries(STATUS_LOOK)) expect(icons, `${kind}: ${look.icon}`).toContain(`'${look.icon}':`);
		expect(STATUS_LOOK.error.icon).not.toBe(STATUS_LOOK.warning.icon);
		expect(new Set(KINDS.map((k) => STATUS_LOOK[k].icon)).size).toBe(4);
	});

	test('a toast draws its glyph and its edge in the hue the log uses', () => {
		for (const kind of KINDS) {
			const hue = STATUS_LOOK[kind].color;
			expect(rule(LAYOUT, `[data-sonner-toaster] [data-sonner-toast][data-type='${kind}'] [data-icon]`), kind).toContain(`color: ${hue}`);
			expect(rule(LAYOUT, `[data-sonner-toaster] [data-sonner-toast][data-styled='true'][data-type='${kind}']`), kind).toContain(
				`border-left: 3px solid ${hue}`
			);
		}
	});

	test('success, error and warning are washed in their own surface, over the toast’s', () => {
		for (const [kind, surface] of [['success', 'success'], ['error', 'danger'], ['warning', 'warning']] as const) {
			const css = rule(LAYOUT, `[data-sonner-toaster] [data-sonner-toast][data-styled='true'][data-type='${kind}']`);
			expect(css, kind).toContain(`linear-gradient(var(--${surface}-surface), var(--${surface}-surface)), var(--surface-raised)`);
		}
	});

	test('the toaster and the log take their glyphs from the one list', () => {
		const toaster = read('lib/components/ui/sonner/sonner.svelte');
		const log = read('lib/components/editor/NotificationCenter.svelte');
		for (const kind of KINDS) expect(toaster, kind).toContain(`STATUS_LOOK.${kind}.icon`);
		expect(log).toContain("from '#lib/status-look'");
		for (const own of ["'alert-triangle'", "'lightbulb'", "'circle-x'", "'circle-check'"]) {
			expect(log, `the log names ${own} itself`).not.toContain(own);
			expect(toaster, `the toaster names ${own} itself`).not.toContain(own);
		}
	});
});
