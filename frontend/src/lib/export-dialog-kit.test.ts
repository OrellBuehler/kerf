import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { posix, SRC, uiSources } from './test-sources';

// The export dialog, the voiceover dialog and the Deliver panel are drawn from the shared kit, not from
// controls of their own: the review found 32 px native selects beside 28 px `.k-select`s, a checkbox at the far
// right where `.k-check` puts it first, the browser's ▶ marker beside the Inspector's chevron, and ⚠ / ▸ typed
// as text. These hold the vocabulary where it was fixed.

const EDITOR = 'lib/components/editor/';
const read = (name: string) => readFileSync(join(SRC, EDITOR, name), 'utf8');
const REBUILT = ['ExportDialog.svelte', 'DeliverPanel.svelte', 'DeliverTo.svelte', 'Readiness.svelte', 'ProjectFrame.svelte', 'ExportOutcomes.svelte'];
// The voiceover dialog's own chips and checkbox belong to the controls kit's pass; its select and glyph are held here.
const DRAWN = [...REBUILT, 'VoiceoverDialog.svelte'];

/** The markup and script with comments taken out, so a rule written down as a comment is not read as use. */
const code = (name: string) => {
	let text = read(name);
	for (let before = ''; before !== text; ) {
		before = text;
		text = text.replace(/<!--[\s\S]*?-->/g, '');
	}
	return text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|\s)\/\/.*$/gm, '$1');
};

describe('the dialogs and the panel use the kit', () => {
	test('every select is a `.k-select`', () => {
		for (const f of DRAWN) {
			for (const tag of code(f).match(/<select\b[^>]*>/g) ?? []) expect(tag, f).toContain('class="k-select"');
		}
	});

	test('no select is sized on its own (the old 32 px, 13 px, 180 px recipe)', () => {
		for (const f of DRAWN) {
			expect(code(f), f).not.toContain('selectCss');
			expect(code(f), f).not.toMatch(/min-height:\s*32px/);
			expect(code(f), f).not.toMatch(/min-width:\s*180px/);
		}
	});

	test('a checkbox is a CheckRow, box first', () => {
		for (const f of REBUILT) expect(code(f), f).not.toContain('type="checkbox"');
	});

	test('a disclosure is an InspectorSection, not the browser’s marker', () => {
		for (const f of REBUILT) {
			expect(code(f), f).not.toMatch(/<details\b/);
			expect(code(f), f).not.toMatch(/<summary\b/);
		}
		expect(code('ExportDialog.svelte')).toContain('<InspectorSection title="Advanced encoding"');
		expect(code('ExportDialog.svelte')).toContain('<InspectorSection title="ffmpeg command"');
		expect(code('Readiness.svelte')).toContain('<InspectorSection');
	});

	test('an icon is an Icon: no ⚠ ▸ ▾ ▶ typed as text', () => {
		for (const f of DRAWN) expect(code(f), f).not.toMatch(/[⚠▸▾▶]/);
	});

	test('a choice among options is a chip group, not a row of hand-drawn buttons', () => {
		expect(code('ExportDialog.svelte')).toContain('<ChoiceChips');
		expect(code('ExportDialog.svelte')).not.toMatch(/<button\b/);
	});

	test('the dialog’s rows are on the one form grid', () => {
		const dialog = code('ExportDialog.svelte');
		expect(dialog).toContain('<FormRow');
		// The only grid of its own is the checkbox row's, which uses the same two columns.
		expect(dialog.match(/grid-template-columns/g)?.length ?? 0).toBeLessThanOrEqual(1);
	});

	test('the voiceover dialog’s selects are the kit’s too', () => {
		expect(code('VoiceoverDialog.svelte')).not.toContain('selectCss');
	});
});

describe('the project frame and the files an export writes are not both "delivery"', () => {
	// J-H4: "Deliver to" meant "also write these files" and "Delivery frame" meant "cut for this shape"; the control that set the
	// frame was a toolbar button called Source. These are the words that were retired.
	const RETIRED = [
		[/\bDeliver to\b/, 'Shapes to write'],
		[/Source shape/, 'Footage'],
		[/label:\s*'Delivery frame/, 'Project frame'],
		[/Cutting for \$\{/, 'Project frame: …']
	] as const;

	test('no source says them', () => {
		const offenders = uiSources().flatMap((f) => {
			const text = readFileSync(f, 'utf8');
			return RETIRED.filter(([re]) => re.test(text)).map(([, use]) => `${posix(f)}: say "${use}"`);
		});
		expect(offenders).toEqual([]);
	});

	// The words a person reads, not the ones the code is written in: `Delivery`, `set_delivery_format` and a comment that says
	// "the delivery frame" are the code's. What is left is a string, a tooltip, a toast or a label.
	const ALLOWED: Record<string, string> = {
		'lib/platforms.ts': 'the harness mirror of kerf_core::platform, whose message the engine words (it is never shown: only its kind is read)',
		'lib/delivery-formats.ts': 'the refusal mirrors kerf_core `check_delivery_fps` verbatim'
	};
	const withoutComments = (text: string) =>
		text
			.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
			.replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '))
			.replace(/(^|\s)\/\/.*$/gm, '$1');

	test('no user-visible string says "delivery frame": it is the project frame', () => {
		const offenders = uiSources().flatMap((f) => {
			if (posix(f) in ALLOWED) return [];
			return withoutComments(readFileSync(f, 'utf8'))
				.split('\n')
				.flatMap((line, i) => (/delivery frame/i.test(line) ? [`${posix(f)}:${i + 1}: ${line.trim().slice(0, 100)}`] : []));
		});
		expect(offenders).toEqual([]);
	});

	test('the Frame button, View › Project frame and the Deliver panel use one vocabulary', async () => {
		const { DELIVERY_PRESETS, deliveryLabel, frameLine } = await import('./delivery-formats');
		expect(DELIVERY_PRESETS.map((p) => p.label)).toEqual(['Footage', '16:9', '9:16', '1:1', '4:5']);
		expect(deliveryLabel(null)).toBe('Frame: Footage');
		expect(frameLine(null)).toBe('Follows the footage');
	});
});

describe('the Deliver panel is for the frame and the shapes first', () => {
	// With five exports the old panel put "Project frame" at y=684 of a 442 px fold (1366×768): the controls were below the history.
	test('the project frame and the shapes come before the session’s exports', () => {
		const panel = code('DeliverPanel.svelte');
		const exportsAt = panel.indexOf('Recent exports');
		expect(exportsAt).toBeGreaterThan(0);
		for (const before of ['<ProjectFrame', 'label="Shapes to write"', '<Readiness']) expect(panel.indexOf(before), before).toBeGreaterThan(0), expect(panel.indexOf(before), before).toBeLessThan(exportsAt);
	});

	test('the newest export is shown on its own and the rest are folded', () => {
		const panel = code('DeliverPanel.svelte');
		expect(panel).toContain('ui.exportOutcomes.slice(0, 1)');
		expect(panel).toContain('ui.exportOutcomes.slice(1)');
		expect(panel).toMatch(/<InspectorSection title="Earlier"/);
	});
});

describe('a long failure reason is bounded where it is shown', () => {
	// A real ffmpeg stderr tail is a dozen lines: in the dialog's footer it took 346 of 720 px.
	test('the dialog’s strip and the panel’s card cap the reason, scroll it and let it be copied', () => {
		const strip = code('ExportDialog.svelte');
		expect(strip).toMatch(/max-height:\s*84px;overflow:\s*auto/);
		expect(strip).toContain("copyText(refusal.reason, 'Reason')");
		const card = code('ExportOutcomes.svelte');
		expect(card).toMatch(/max-height:\{REASON_MAX\}px;overflow:auto/);
		expect(card).toContain("copyText(o.reason, 'Reason')");
	});

	test('the toast says the first line; the log keeps the rest as its description', () => {
		const dialog = code('ExportDialog.svelte');
		expect(dialog).toContain('reasonLines(');
		expect(dialog).toContain('{ detail: rest }');
	});
});
