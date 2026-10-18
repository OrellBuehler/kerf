import { afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// The template actions as the library, the Inspector and the save dialog run them, over the browser harness's
// backend (which applies kerf-core's rules to the dev timeline and the in-memory library). Only the toasts are
// stood in for: `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.

const shown: { kind: string; text: string; description?: string }[] = [];
const note = (kind: string) => (text: string, opts?: { description?: string }) => void shown.push({ kind, text, description: opts?.description });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	})
}));

const { editor } = await import('./state.svelte');
const { ui } = await import('./editor-ui.svelte');
const actions = await import('./template-actions');
const { addSvgOverlay, getHistory, listTemplates, revertTo } = await import('./api');
const { resetDevTemplates, SAMPLE_TEMPLATE_ID } = await import('./template-dev');
const revision = async () => (await getHistory()).find((r) => r.current)!.seq;
const g = globalThis as unknown as { window?: unknown };

const ART =
	'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180">' +
	'<rect id="plate" width="320" height="180" fill="teal"/>' +
	'<text id="who" x="10" y="90" font-size="24" fill="white">Ada</text>' +
	'<image id="logo" x="0" y="0" width="40" height="40" href="data:image/png;base64,AAAA"/>' +
	'</svg>';

async function sample() {
	return (await listTemplates()).templates.find((t) => t.id === SAMPLE_TEMPLATE_ID)!;
}

beforeEach(async () => {
	await revertTo(0);
	resetDevTemplates();
	await editor.load();
	editor.clearSelection();
	ui.time = 0;
	shown.length = 0;
});

afterEach(() => {
	delete g.window;
});

describe('adding a template', () => {
	test('lands at the playhead, as long as the template runs, selected, with its controls remembered', async () => {
		ui.time = 3.5;
		const before = await revision();
		await actions.addTemplateHere(await sample());
		const [o] = editor.svgOverlays;
		expect(o.start).toBe(3.5);
		expect(o.end).toBe(7.5);
		expect(o.template?.template_id).toBe(SAMPLE_TEMPLATE_ID);
		expect(o.template?.controls.map((c) => c.id)).toEqual(['headline', 'tagline', 'brand', 'headline-size']);
		expect(editor.selectedSvgOverlayId).toBe(o.id);
		expect(await revision()).toBe(before + 1);
		expect(shown).toEqual([]);
	});

	test('a template the library lacks is a toast, not a throw', async () => {
		const gone = { ...(await sample()), id: '00000000-0000-4000-8000-000000000000' };
		await actions.addTemplateHere(gone);
		expect(editor.svgOverlays).toHaveLength(0);
		expect(shown.map((s) => s.kind)).toEqual(['error']);
		expect(shown[0].text).toContain('no template');
	});
});

describe('the file round trip', () => {
	test('export asks for a name from the template, writes it, and says which file', async () => {
		expect(await actions.exportTemplateFile(await sample())).toBe(true);
		expect(shown).toEqual([{ kind: 'success', text: 'Exported Sample-end-card.kerftemplate', description: undefined }]);
	});

	test('export, delete, import brings it back; importing it again changes nothing', async () => {
		const info = await sample();
		await actions.exportTemplateFile(info);
		g.window = { confirm: () => true };
		expect(await actions.deleteTemplateFile(info)).toBe(true);
		expect((await listTemplates()).templates).toHaveLength(0);
		shown.length = 0;

		expect(await actions.importTemplateFile()).toBe(true);
		expect((await listTemplates()).templates.map((t) => t.id)).toEqual([SAMPLE_TEMPLATE_ID]);
		expect(shown).toEqual([{ kind: 'success', text: 'Added “Sample end card” to your templates', description: undefined }]);

		shown.length = 0;
		await actions.importTemplateFile();
		expect((await listTemplates()).templates).toHaveLength(1);
		expect(shown[0]).toMatchObject({ kind: 'info', text: '“Sample end card” is already in your templates' });
	});

	test('with nothing to pick, an import does nothing and says nothing', async () => {
		expect(await actions.importTemplateFile()).toBe(false);
		expect(shown).toEqual([]);
	});

	test('a failed export is a toast with the reason', async () => {
		const gone = { ...(await sample()), id: '00000000-0000-4000-8000-000000000000' };
		expect(await actions.exportTemplateFile(gone)).toBe(false);
		expect(shown[0].kind).toBe('error');
		expect(shown[0].text).toContain("Couldn't export");
	});
});

describe('deleting', () => {
	test('asks first: a no keeps the template', async () => {
		g.window = { confirm: () => false };
		expect(await actions.deleteTemplateFile(await sample())).toBe(false);
		expect((await listTemplates()).templates).toHaveLength(1);
		expect(shown).toEqual([]);
	});

	test('the question names the template and says titles made from it keep working', async () => {
		let asked = '';
		g.window = {
			confirm: (m: string) => ((asked = m), true)
		};
		await actions.deleteTemplateFile(await sample());
		expect(asked).toContain('“Sample end card”');
		expect(asked).toContain('keep working');
		expect((await listTemplates()).templates).toHaveLength(0);
		expect(shown[0]).toMatchObject({ kind: 'info', text: 'Deleted “Sample end card” from your templates' });
	});

	test('a title already made from it keeps its artwork and its controls', async () => {
		await actions.addTemplateHere(await sample());
		g.window = { confirm: () => true };
		await actions.deleteTemplateFile(await sample());
		expect(editor.svgOverlays[0].template?.controls).toHaveLength(4);
		await actions.setTemplateValue(editor.svgOverlays[0].id, editor.svgOverlays[0].template!.controls[1], 'Bye');
		expect(editor.svgOverlays[0].template?.values.tagline).toBe('Bye');
	});
});

describe('posters', () => {
	test('asked for once per template, again after it is forgotten', async () => {
		const a = actions.posterOf(SAMPLE_TEMPLATE_ID);
		expect(actions.posterOf(SAMPLE_TEMPLATE_ID)).toBe(a);
		expect(await a).toStartWith('data:image/');
		actions.forgetPoster(SAMPLE_TEMPLATE_ID);
		expect(actions.posterOf(SAMPLE_TEMPLATE_ID)).not.toBe(a);
	});

	test('a failed read is not remembered', async () => {
		const id = '00000000-0000-4000-8000-000000000000';
		const first = actions.posterOf(id);
		await expect(first).rejects.toThrow();
		const second = actions.posterOf(id);
		expect(second).not.toBe(first);
		second.catch(() => {});
	});
});

describe('keeping a title as a template', () => {
	async function title() {
		const t = await addSvgOverlay({ custom: ART }, 1, 5);
		return t.svg_overlays!.at(-1)!;
	}

	test('saves it with its controls, says so, and lists it', async () => {
		const o = await title();
		const saved = await actions.saveTitleAsTemplate(o.id, '  Lower card ', ' ', [
			{ id: 'who', label: 'Who', kind: 'text', targets: [{ element: 'who' }] }
		]);
		expect(saved.info).toMatchObject({ name: 'Lower card', description: '', duration: 4 });
		expect(shown).toEqual([{ kind: 'success', text: 'Saved “Lower card” to your templates', description: undefined }]);
		expect((await listTemplates()).templates.map((t) => t.name)).toEqual(['Lower card', 'Sample end card']);
	});

	test('a refusal rejects with the engine’s reason and saves nothing', async () => {
		const o = await title();
		await expect(
			actions.saveTitleAsTemplate(o.id, 'Card', '', [{ id: 'who', label: 'Who', kind: 'text', targets: [{ element: 'ghost' }] }])
		).rejects.toThrow('ghost');
		expect((await listTemplates()).templates).toHaveLength(1);
		expect(shown).toEqual([]);
	});
});

describe('changing a template title’s values', () => {
	async function applied() {
		await actions.addTemplateHere(await sample());
		return editor.svgOverlays[0];
	}
	const control = (o: Awaited<ReturnType<typeof applied>>, id: string) => o.template!.controls.find((c) => c.id === id)!;

	test('a text is one edit, read back into the title’s values', async () => {
		const o = await applied();
		const before = await revision();
		await actions.setTemplateValue(o.id, control(o, 'headline'), 'See you soon');
		expect(editor.svgOverlays[0].template?.values.headline).toBe('See you soon');
		expect(editor.svgOverlays[0].svg).toContain('See you soon');
		expect(await revision()).toBe(before + 1);
	});

	test('a number outside its bounds is refused before anything is written', async () => {
		const o = await applied();
		const before = await revision();
		await expect(actions.setTemplateValue(o.id, control(o, 'headline-size'), '500')).rejects.toThrow('Headline size: Between 40 and 200');
		await expect(actions.setTemplateValue(o.id, control(o, 'headline-size'), '12px')).rejects.toThrow('no unit');
		expect(await revision()).toBe(before);
	});

	test('a number goes out in plain decimal and a color trimmed', async () => {
		const o = await applied();
		await actions.setTemplateValue(o.id, control(o, 'headline-size'), ' 96.50 ');
		await actions.setTemplateValue(o.id, control(o, 'brand'), ' #ff8800 ');
		expect(editor.svgOverlays[0].template?.values).toMatchObject({ 'headline-size': '96.5', brand: '#ff8800' });
	});

	test('a picture is picked, read, and set; the title’s values have no entry for it', async () => {
		const o = await title();
		const saved = await actions.saveTitleAsTemplate(o.id, 'Logo card', '', [
			{ id: 'logo', label: 'Logo', kind: 'image', targets: [{ element: 'logo', attribute: 'href' }] }
		]);
		await actions.addTemplateHere(saved.info);
		const made = editor.svgOverlays.find((x) => x.template)!;
		expect(await actions.setTemplateImage(made.id, made.template!.controls[0])).toBe(true);
		expect(editor.svgOverlays.find((x) => x.id === made.id)!.svg).toContain('data:image/png;base64,iVBOR');
		expect(editor.svgOverlays.find((x) => x.id === made.id)!.template?.values).toEqual({});
	});

	async function title() {
		const t = await addSvgOverlay({ custom: ART }, 1, 5);
		return t.svg_overlays!.at(-1)!;
	}
});
