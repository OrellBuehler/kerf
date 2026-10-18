import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';
import { deleteTemplate, importTemplate, listTemplates, onTemplatesChanged, saveTemplate, exportTemplate, addSvgOverlay, revertTo } from './api';
import { resetDevTemplates, SAMPLE_TEMPLATE_ID } from './template-dev';

// Where the template controls are mounted and what they listen to. The behaviour that can run without a window is
// tested where it lives (`template-ui.test.ts`, `template-actions.test.ts`, `api-templates.test.ts`).

const read = (path: string) => readFileSync(join(SRC, path), 'utf8');
const EDITOR = 'lib/components/editor/';

describe('the Templates section', () => {
	test('is part of the Titles controls, so the library tab and the Inspector’s Titles lane both have it', () => {
		const titles = read(`${EDITOR}TitlesControls.svelte`);
		expect(titles).toContain("import TemplatesSection from './TemplatesSection.svelte'");
		expect(titles).toContain('<TemplatesSection />');
		expect(read(`${EDITOR}LibraryPanel.svelte`)).toContain('<TitlesControls />');
		expect(read(`${EDITOR}Inspector.svelte`)).toContain('<TitlesControls />');
	});

	test('reads the library when it opens and again on `templates-changed`, and lets go of the listener', () => {
		const section = read(`${EDITOR}TemplatesSection.svelte`);
		expect(section).toContain('onTemplatesChanged(() => void load())');
		expect(section).toMatch(/return \(\) => \{\s*gone = true;\s*stop\(\);/);
		expect(read('lib/api.ts')).toContain("listen('templates-changed'");
	});

	test('a card’s picture is an image through a data URL, never the SVG itself', () => {
		const card = read(`${EDITOR}TemplateCard.svelte`);
		expect(card).toContain('<img');
		expect(card).toContain('posterOf(');
		expect(card).not.toMatch(/\{@html|innerHTML/);
	});

	test('a card offers Export… and Delete…, the ellipsis because each asks something first', () => {
		const card = read(`${EDITOR}TemplateCard.svelte`);
		expect(card).toContain("label: 'Export…'");
		expect(card).toContain("label: 'Delete…'");
		expect(card).toContain('oncontextmenu');
	});

	test('the empty library says how to make a template', () => {
		const section = read(`${EDITOR}TemplatesSection.svelte`);
		expect(section).toContain('No templates yet');
		expect(section).toContain('Save as template…');
	});

	test('the files that would not read are listed, not hidden', () => {
		expect(read(`${EDITOR}TemplatesSection.svelte`)).toContain('data-template-problems');
	});
});

describe('the Inspector', () => {
	const inspector = read(`${EDITOR}Inspector.svelte`);

	test('a template title shows its controls first; a custom one offers to be kept as a template', () => {
		expect(inspector).toContain('{#if o.template}<TemplateSection {o} />{/if}');
		expect(inspector).toContain('{#if !o.template && !o.preset}<TemplateSection {o} />{/if}');
	});

	test('a field commits on change, not on every key, and to the title it began on', () => {
		const section = read(`${EDITOR}TemplateSection.svelte`);
		expect(section).not.toMatch(/\boninput=\{[^}]*write\(/);
		expect(section.match(/onchange=\{[^}]*write\(/g)?.length).toBeGreaterThanOrEqual(5);
		expect(section).toContain('new FieldEdits<string>()');
		expect(section).toContain("edits.end(el, { scope: o.id, text: el.value })");
		expect(section).toContain('setTemplateValue(id, control, next)');
	});

	test('an edit in flight keeps the focus: pending, never a native disabled', () => {
		const section = read(`${EDITOR}TemplateSection.svelte`);
		expect(section).not.toMatch(/(?<!aria-)disabled=\{editor\.(busy|locked)/);
		expect(section).toContain('pending={editor.locked');
	});
});

describe('the Save as template dialog', () => {
	test('is mounted once, by the page, and puts the app behind it out of reach', () => {
		const page = read('routes/+page.svelte');
		expect(page).toContain('<SaveTemplateDialog overlayId={ui.saveTemplateDialog.overlayId}');
		expect(page).toContain('ui.saveTemplateDialog !== null');
	});

	test('shows the engine’s refusal where the fields are and keeps what was typed', () => {
		const dialog = read(`${EDITOR}SaveTemplateDialog.svelte`);
		expect(dialog).toContain('data-save-error');
		expect(dialog).toContain('busy={saving}');
		expect(dialog).toContain('dismissOnScrim={false}');
		expect(dialog).toContain('data-rescue');
	});
});

describe('`templates-changed` in the browser harness', () => {
	const ART =
		'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180"><text id="who" x="1" y="1">Ada</text></svg>';

	test('the harness announces its own saves, imports and deletes, as the app does', async () => {
		await revertTo(0);
		resetDevTemplates();
		let heard = 0;
		const off = await onTemplatesChanged(() => void heard++);
		const title = (await addSvgOverlay({ custom: ART }, 0, 4)).svg_overlays!.at(-1)!;

		await saveTemplate(title.id, 'Card', [{ id: 'who', label: 'Who', kind: 'text', targets: [{ element: 'who' }] }]);
		expect(heard).toBe(1);

		const path = await exportTemplate(SAMPLE_TEMPLATE_ID, '/harness/sample.kerftemplate');
		expect((await importTemplate(path)).status).toBe('already_present');
		expect(heard).toBe(1);

		await deleteTemplate(SAMPLE_TEMPLATE_ID);
		expect(heard).toBe(2);
		expect((await importTemplate(path)).status).toBe('added');
		expect(heard).toBe(3);

		off();
		await deleteTemplate(SAMPLE_TEMPLATE_ID);
		expect(heard).toBe(3);
		expect((await listTemplates()).templates.map((t) => t.name)).toEqual(['Card']);
		resetDevTemplates();
	});
});
