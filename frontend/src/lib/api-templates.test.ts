import { beforeEach, describe, expect, test } from 'bun:test';
import {
	addSvgOverlay,
	applyTemplate,
	deleteTemplate,
	exportTemplate,
	getHistory,
	importTemplate,
	listTemplates,
	revertTo,
	saveTemplate,
	setTemplateValues,
	svgElementIds,
	templateImageValue,
	templatePoster,
	updateSvgOverlay
} from './api';
import { applyValues, readValues, resetDevTemplates, SAMPLE_TEMPLATE_ID } from './template-dev';
import type { SvgOverlay, TemplateControl, Timeline } from './types';

// Under bun there is no Tauri, so these drive the browser harness's template library. It has to keep the
// backend's contract (kerf-core `template/`): the same refusals, partial values, a title that remembers its
// controls and reads its values back from its own artwork, one revision per edit, and a library that never
// overwrites on import.

const ART =
	'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180">' +
	'<defs><linearGradient id="g"><stop id="top" offset="0" stop-color="#112233"/></linearGradient></defs>' +
	'<rect id="plate" width="320" height="180" fill="url(#g)"/>' +
	'<text id="who" x="10" y="90" font-size="24" fill="#ffffff">Ada</text>' +
	'<image id="logo" x="0" y="0" width="40" height="40" href="data:image/png;base64,AAAA"/>' +
	'</svg>';

const text = (id: string, element = id): TemplateControl => ({ id, label: id, kind: 'text', targets: [{ element }] });
const color = (id: string, element: string, attribute = 'fill'): TemplateControl => ({
	id,
	label: id,
	kind: 'color',
	targets: [{ element, attribute }]
});

beforeEach(async () => {
	await revertTo(0);
	resetDevTemplates();
});

const titles = (t: Timeline): SvgOverlay[] => t.svg_overlays ?? [];
const head = async () => (await getHistory()).find((h) => h.current)!;

async function plainTitle(): Promise<SvgOverlay> {
	const t = await addSvgOverlay({ custom: ART }, 2, 6);
	return titles(t).at(-1)!;
}

async function sample() {
	return (await listTemplates()).templates.find((t) => t.id === SAMPLE_TEMPLATE_ID)!;
}

describe('the library (browser harness)', () => {
	test('starts with one sample template, by name, with controls and a poster', async () => {
		const list = await listTemplates();
		expect(list.problems).toEqual([]);
		expect(list.templates.map((t) => t.id)).toEqual([SAMPLE_TEMPLATE_ID]);
		const info = list.templates[0];
		expect(info.has_poster).toBe(true);
		expect(info.controls.map((c) => c.kind)).toEqual(['text', 'text', 'color', 'number']);
		expect(info.duration).toBeGreaterThan(0);
		expect(info.width).toBe(1920);
		expect(info.height).toBe(1080);
		const poster = await templatePoster(info.id);
		expect(poster?.startsWith('data:image/')).toBe(true);
	});

	test('a list is a copy: changing it does not change the library', async () => {
		const list = await listTemplates();
		list.templates[0].controls.length = 0;
		expect((await sample()).controls.length).toBe(4);
	});

	test('an id the library lacks, or is not an id, rejects', async () => {
		await expect(templatePoster('5b6f1c1e-0c64-4f6e-9a54-000000000000')).rejects.toThrow(/no template with the id/);
		await expect(templatePoster('../etc/passwd')).rejects.toThrow(/lower-case UUID/);
		await expect(deleteTemplate('5b6f1c1e-0c64-4f6e-9a54-000000000000')).rejects.toThrow(/no template/);
	});
});

describe('applying a template (browser harness)', () => {
	test('adds a title that carries its template, runs the template length and starts at the playhead', async () => {
		const info = await sample();
		const { timeline, overlay_id, warnings } = await applyTemplate(info.id, {}, 3);
		expect(warnings).toEqual([]);
		const o = titles(timeline).find((t) => t.id === overlay_id)!;
		expect(o.name).toBe(info.name);
		expect(o.start).toBe(3);
		expect(o.end).toBe(3 + info.duration);
		expect([o.x, o.y, o.width, o.height, o.opacity]).toEqual([0, 0, 1, 1, 1]);
		expect(o.preset).toBeUndefined();
		expect(o.template?.template_id).toBe(info.id);
		expect(o.template?.controls).toEqual(info.controls);
		expect(o.template?.values).toEqual({
			headline: 'Thanks for watching',
			tagline: 'See you next time',
			brand: 'darkcyan',
			'headline-size': '120'
		});
	});

	test('partial values are spliced in, escaped, and read back from the artwork', async () => {
		const { timeline, overlay_id } = await applyTemplate(
			SAMPLE_TEMPLATE_ID,
			{ headline: 'Fish & <Chips>', brand: ' #E63946 ', 'headline-size': '150' },
			0,
			5,
			{ name: 'Outro', x: 0.1, opacity: 0.5 }
		);
		const o = titles(timeline).find((t) => t.id === overlay_id)!;
		expect(o.name).toBe('Outro');
		expect(o.x).toBe(0.1);
		expect(o.opacity).toBe(0.5);
		expect(o.end).toBe(5);
		expect(o.svg).toContain('Fish &amp; &lt;Chips&gt;</text>');
		expect(o.svg).toContain('stop-color="#E63946"');
		expect(o.svg).toContain('font-size="150"');
		expect(o.template?.values.headline).toBe('Fish & <Chips>');
		expect(o.template?.values.brand).toBe('#E63946');
		expect(o.template?.values.tagline).toBe('See you next time');
		const info = await sample();
		expect(info.controls.length).toBe(4);
	});

	test('is one revision, undone with the rest', async () => {
		const before = (await head()).seq;
		await applyTemplate(SAMPLE_TEMPLATE_ID, {}, 0);
		const now = await head();
		expect(now.seq).toBe(before + 1);
		expect(now.label).toBe('Add template title “Sample end card”');
	});

	test('refuses what the engine refuses, and changes nothing', async () => {
		const bad = async (values: Record<string, string>, pattern: RegExp, start = 0, end?: number) => {
			const before = (await head()).seq;
			await expect(applyTemplate(SAMPLE_TEMPLATE_ID, values, start, end)).rejects.toThrow(pattern);
			expect((await head()).seq).toBe(before);
		};
		await bad({ nope: 'x' }, /no control called “nope”/);
		await bad({ brand: 'rgb(1, 2, 3)' }, /not a colour/);
		await bad({ brand: 'red@0.5' }, /not a colour/);
		await bad({ 'headline-size': '12' }, /outside what/);
		await bad({ 'headline-size': '120px' }, /not a number/);
		await bad({ headline: 'a\u0007b' }, /control character/);
		await bad({ headline: 'x'.repeat(2001) }, /longer than 2000/);
		await bad({}, /overlay end must be after start/, 5, 5);
		await bad({}, /start must be a number/, Number.NaN);
		await expect(applyTemplate('5b6f1c1e-0c64-4f6e-9a54-000000000000', {}, 0)).rejects.toThrow(/no template/);
		await expect(applyTemplate(SAMPLE_TEMPLATE_ID, {}, 0, null, { width: 0 })).rejects.toThrow(/width and height/);
	});

	test('a number is what Rust’s f64 parser takes, written in plain decimal: no 0x, 0b, 0o or separators', async () => {
		const plain = await plainTitle();
		const size: TemplateControl = { id: 'size', label: 'Size', kind: 'number', targets: [{ element: 'who', attribute: 'font-size' }] };
		const { info } = await saveTemplate(plain.id, 'Sized', [size]);
		const sizeOf = async (value: string) => {
			const { timeline, overlay_id } = await applyTemplate(info.id, { size: value }, 0);
			return titles(timeline).find((t) => t.id === overlay_id)!.template!.values.size;
		};
		const ok: [string, string][] = [
			['100', '100'],
			[' 100 ', '100'],
			['+100', '100'],
			['100.', '100'],
			['.5e2', '50'],
			['1e2', '100'],
			['1E2', '100'],
			['-0', '0'],
			['0.50', '0.5'],
			['-7.25', '-7.25'],
			['1e21', '1000000000000000000000'],
			['1e-7', '0.0000001'],
			['1.5e-25', '0.00000000000000000000000015']
		];
		for (const [given, written] of ok) expect(await sizeOf(given), given).toBe(written);
		for (const bad of ['0x10', '0X10', '0b11', '0o7', '1_000', '.', 'e5', '1e', '--1', '1 0', '', '  ', '5px', '١٢']) {
			await expect(applyTemplate(info.id, { size: bad }, 0), JSON.stringify(bad)).rejects.toThrow(/is not a number/);
		}
		for (const word of ['inf', 'Infinity', '-INF', 'nan', 'NaN', '1e999']) {
			await expect(applyTemplate(info.id, { size: word }, 0), word).rejects.toThrow(/is not a finite number/);
		}
	});

	test('a title is held to 128 KiB after the values are spliced in', async () => {
		const room = 1500;
		const fill = '<!--' + 'x'.repeat(128 * 1024 - ART.length - '<!---->'.length - room) + '-->';
		const big = (await addSvgOverlay({ custom: ART.replace('</svg>', `${fill}</svg>`) }, 0, 4)).svg_overlays!.at(-1)!;
		expect(new TextEncoder().encode(big.svg).length).toBeLessThanOrEqual(128 * 1024);
		const { info } = await saveTemplate(big.id, 'Big', [text('who')]);
		const { overlay_id, timeline } = await applyTemplate(info.id, { who: 'é'.repeat(300) }, 0);
		expect(titles(timeline).find((t) => t.id === overlay_id)!.template!.values.who).toBe('é'.repeat(300));
		const before = (await head()).seq;
		await expect(applyTemplate(info.id, { who: 'é'.repeat(1000) }, 0)).rejects.toThrow(/the SVG is larger than 128 KiB/);
		await expect(setTemplateValues(overlay_id, { who: 'é'.repeat(1000) })).rejects.toThrow(/the SVG is larger than 128 KiB/);
		expect((await head()).seq).toBe(before);
	});

	test('a colour takes a name, none and currentColor but not a function', async () => {
		for (const ok of ['teal', '#abc', '#aabbccdd', 'none', 'currentColor']) {
			const { timeline, overlay_id } = await applyTemplate(SAMPLE_TEMPLATE_ID, { brand: ok }, 0);
			expect(titles(timeline).find((t) => t.id === overlay_id)!.template?.values.brand).toBe(ok);
		}
	});

	test('a timeline holds 50 animated titles, templates included', async () => {
		let last: Promise<unknown> = Promise.resolve();
		for (let i = 0; i < 50; i++) last = applyTemplate(SAMPLE_TEMPLATE_ID, {}, i);
		await last;
		await expect(applyTemplate(SAMPLE_TEMPLATE_ID, {}, 50)).rejects.toThrow(/at most 50/);
	});
});

describe('setting values (browser harness)', () => {
	test('changes only the controls named, keeps the id, timing and placement, in one revision', async () => {
		const { overlay_id } = await applyTemplate(SAMPLE_TEMPLATE_ID, { tagline: 'Bye' }, 1, 4, { x: 0.2 });
		const before = (await head()).seq;
		const done = await setTemplateValues(overlay_id, { headline: 'Hello', brand: 'navy' });
		expect(done.overlay_id).toBe(overlay_id);
		const o = titles(done.timeline).find((t) => t.id === overlay_id)!;
		expect([o.start, o.end, o.x]).toEqual([1, 4, 0.2]);
		expect(o.template?.values).toEqual({
			headline: 'Hello',
			tagline: 'Bye',
			brand: 'navy',
			'headline-size': '120'
		});
		expect(o.svg).toContain('>Hello</text>');
		expect(o.svg).toContain('>Bye</text>');
		const now = await head();
		expect(now.seq).toBe(before + 1);
		expect(now.label).toBe('Set template values');
	});

	test('a refused value leaves the title as it was', async () => {
		const { overlay_id } = await applyTemplate(SAMPLE_TEMPLATE_ID, {}, 0);
		const svg = (await sampleTitle(overlay_id)).svg;
		await expect(setTemplateValues(overlay_id, { headline: 'ok', brand: 'rgb(0,0,0)' })).rejects.toThrow(/not a colour/);
		expect((await sampleTitle(overlay_id)).svg).toBe(svg);
		await expect(setTemplateValues(overlay_id, { nope: 'x' })).rejects.toThrow(/no control called/);
	});

	test('a title that is not from a template, or is not there, rejects', async () => {
		const plain = await plainTitle();
		await expect(setTemplateValues(plain.id, { a: 'b' })).rejects.toThrow(/not a template title/);
		await expect(setTemplateValues('00000000-0000-4000-8000-000000000000', {})).rejects.toThrow(/not found/);
	});

	test('a rename or a move keeps the template; replacing the artwork ends it, as in the engine', async () => {
		const { overlay_id } = await applyTemplate(SAMPLE_TEMPLATE_ID, {}, 0);
		await updateSvgOverlay(overlay_id, { name: 'Renamed', x: 0.1, end: 7 });
		const kept = await sampleTitle(overlay_id);
		expect([kept.name, kept.x, kept.end]).toEqual(['Renamed', 0.1, 7]);
		expect(kept.template?.template_id).toBe(SAMPLE_TEMPLATE_ID);
		await updateSvgOverlay(overlay_id, { svg: ART });
		const ended = await sampleTitle(overlay_id);
		expect(ended.template).toBeUndefined();
		await expect(setTemplateValues(overlay_id, { headline: 'x' })).rejects.toThrow(/not a template title/);
	});

	test('undo brings the old values back', async () => {
		const { overlay_id } = await applyTemplate(SAMPLE_TEMPLATE_ID, {}, 0);
		const applied = (await head()).seq;
		await setTemplateValues(overlay_id, { headline: 'Changed' });
		const back = await revertTo(applied);
		expect(titles(back).find((t) => t.id === overlay_id)!.template?.values.headline).toBe('Thanks for watching');
	});
});

async function sampleTitle(id: string): Promise<SvgOverlay> {
	const { getTimeline } = await import('./api');
	return titles(await getTimeline()).find((t) => t.id === id)!;
}

describe('keeping a title as a template (browser harness)', () => {
	test('lists the new template, which can be applied with its controls', async () => {
		const plain = await plainTitle();
		const saved = await saveTemplate(plain.id, ' Intro ', [text('who'), color('plate-colour', 'plate')], 'A name card');
		expect(saved.warnings).toEqual([]);
		expect(saved.info.name).toBe('Intro');
		expect(saved.info.description).toBe('A name card');
		expect([saved.info.width, saved.info.height]).toEqual([320, 180]);
		expect(saved.info.duration).toBe(4);
		const list = await listTemplates();
		expect(list.templates.map((t) => t.name)).toEqual(['Intro', 'Sample end card']);

		const { timeline, overlay_id } = await applyTemplate(saved.info.id, { who: 'Grace' }, 0);
		const o = titles(timeline).find((t) => t.id === overlay_id)!;
		expect(o.template?.values).toEqual({ who: 'Grace', 'plate-colour': 'url(#g)' });
		expect(o.svg).toContain('>Grace</text>');
	});

	test('refuses a control aimed at nothing, a wrong attribute, a repeated place or a bad id, and writes nothing', async () => {
		const plain = await plainTitle();
		const refuse = async (controls: TemplateControl[], pattern: RegExp, name = 'X') => {
			await expect(saveTemplate(plain.id, name, controls)).rejects.toThrow(pattern);
			expect((await listTemplates()).templates.length).toBe(1);
		};
		await refuse([text('a', 'nothing-here')], /targets the element “nothing-here”, which the title does not have/);
		await refuse([color('c', 'plate', 'stroke')], /sets “stroke” on “plate”, which has no such attribute/);
		await refuse([color('c', 'plate', 'width')], /cannot write “width”/);
		await refuse([{ id: 'c', label: 'c', kind: 'color', targets: [{ element: 'plate' }] }], /needs an attribute/);
		await refuse([{ id: 't', label: 't', kind: 'text', targets: [{ element: 'who', attribute: 'fill' }] }], /take no attribute/);
		await refuse([text('t', 'plate')], /text control but “plate” is a <rect>/);
		await refuse([text('a', 'who'), text('b', 'who')], /more than one control writes the text of “who”/);
		await refuse([text('a', 'who'), text('a', 'who')], /two controls are called/);
		await refuse([text('9lives', 'who')], /not a control id/);
		await refuse([{ id: 'n', label: 'n', kind: 'color', targets: [{ element: 'plate', attribute: 'fill' }], min: 1 }], /only a number/);
		await expect(saveTemplate('00000000-0000-4000-8000-000000000000', 'X', [])).rejects.toThrow(/not found/);
	});

	test('a text control cannot hold another control’s target: an outer text and a tspan inside it', async () => {
		const nested = await addSvgOverlay(
			{
				custom:
					'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180"><text id="outer" x="1" y="1" fill="white">Hello <tspan id="inner" fill="red">there</tspan></text></svg>'
			},
			0,
			4
		);
		const o = titles(nested).at(-1)!;
		const save = (controls: TemplateControl[]) => saveTemplate(o.id, 'Nested', controls);
		await expect(save([text('a', 'outer'), text('b', 'inner')])).rejects.toThrow(
			/the text of “outer” is a control, and it holds “inner”, which another control sets/
		);
		await expect(save([text('a', 'outer'), color('c', 'inner')])).rejects.toThrow(/the text of “outer” is a control, and it holds “inner”/);
		await expect(save([color('c', 'outer'), text('b', 'inner')])).resolves.toBeDefined();
		await expect(save([text('a', 'outer'), color('c', 'outer')])).resolves.toBeDefined();
		await expect(save([text('b', 'inner')])).resolves.toBeDefined();
	});

	test('an attribute name is an XML name with at most an xlink prefix', async () => {
		const plain = await plainTitle();
		const number = (attribute: string): TemplateControl => ({
			id: 'n',
			label: 'N',
			kind: 'number',
			targets: [{ element: 'who', attribute }]
		});
		for (const bad of ['a b', 'font:size', 'xlink:', 'xlink:a:b', '1a', '-a', 'a;b', 'a'.repeat(65), 'é', 'xml:lang', ':a', '']) {
			await expect(saveTemplate(plain.id, 'X', [number(bad)])).rejects.toThrow(/cannot be set by a control/);
		}
		for (const forbidden of ['id', 'class', 'style', 'href', 'xlink:href', 'xmlns', 'xmlns:xlink', 'onclick', 'onload']) {
			await expect(saveTemplate(plain.id, 'X', [number(forbidden)])).rejects.toThrow(/cannot be set by a control/);
		}
		await expect(saveTemplate(plain.id, 'X', [number('font-size')])).resolves.toBeDefined();
		const image = (attribute: string): TemplateControl => ({ id: 'i', label: 'I', kind: 'image', targets: [{ element: 'logo', attribute }] });
		await expect(saveTemplate(plain.id, 'X', [image('onclick')])).rejects.toThrow(/cannot be set by a control/);
		await expect(saveTemplate(plain.id, 'X', [image('href')])).resolves.toBeDefined();
	});

	test('a name, a description and a label may not carry a control character, nor run long', async () => {
		const plain = await plainTitle();
		const control = (label: string): TemplateControl => ({ ...text('who'), label });
		await expect(saveTemplate(plain.id, 'a\u0007b', [])).rejects.toThrow(/the name has a control character/);
		await expect(saveTemplate(plain.id, 'x'.repeat(121), [])).rejects.toThrow(/the name is longer than 120/);
		await expect(saveTemplate(plain.id, 'X', [], 'a\u0000b')).rejects.toThrow(/the description has a control character/);
		await expect(saveTemplate(plain.id, 'X', [], 'x'.repeat(2001))).rejects.toThrow(/the description is longer than 2000/);
		await expect(saveTemplate(plain.id, 'X', [control('a\u001fb')])).rejects.toThrow(/the label of “who” has a control character/);
		await expect(saveTemplate(plain.id, 'X', [control('   ')])).rejects.toThrow(/the label of “who” is empty/);
		await expect(saveTemplate(plain.id, 'X', [control('x'.repeat(81))])).rejects.toThrow(/the label of “who” is longer than 80/);
		const tabbed = await saveTemplate(plain.id, 'Two\tcolumns', [control('Line\none')], 'a\nb');
		expect(tabbed.info.name).toBe('Two\tcolumns');
		expect(tabbed.info.description).toBe('a\nb');
		expect((await listTemplates()).templates.length).toBe(2);
		await expect(saveTemplate(plain.id, 'X', [{ ...text('who'), targets: [{ element: 'who\u0007' }] }])).rejects.toThrow(/a target of “who” has a control character/);
	});

	test('a blank name is the title’s own name, as the engine has it; the description is trimmed', async () => {
		const plain = await plainTitle();
		const unnamed = await saveTemplate(plain.id, '  ', [], '  about  ');
		expect(unnamed.info.name).toBe('SVG');
		expect(unnamed.info.description).toBe('about');
		await updateSvgOverlay(plain.id, { name: '  Opener ' });
		expect((await saveTemplate(plain.id, '', [])).info.name).toBe('Opener');
		expect((await saveTemplate(plain.id, ' Mine ', [])).info.name).toBe('Mine');
	});

	test('the template runs as long as the title does, a slice’s offset included', async () => {
		const plain = await plainTitle();
		expect((await saveTemplate(plain.id, 'Whole', [])).info.duration).toBe(4);
		const sliced = (await addSvgOverlay({ custom: ART }, 1, 3)).svg_overlays!.at(-1)!;
		expect((await saveTemplate(sliced.id, 'Short', [])).info.duration).toBe(2);
	});

	test('an image control is aimed at an <image> href and never mirrored into the values', async () => {
		const plain = await plainTitle();
		const logo: TemplateControl = { id: 'logo', label: 'Logo', kind: 'image', targets: [{ element: 'logo', attribute: 'href' }] };
		const saved = await saveTemplate(plain.id, 'Logo card', [logo, text('who')]);
		const picture = await templateImageValue('/home/me/logo.png');
		expect(picture.startsWith('data:image/png;base64,')).toBe(true);
		const { timeline, overlay_id } = await applyTemplate(saved.info.id, { logo: picture }, 0);
		const o = titles(timeline).find((t) => t.id === overlay_id)!;
		expect(o.svg).toContain(picture);
		expect(o.template?.values).toEqual({ who: 'Ada' });
		await expect(applyTemplate(saved.info.id, { logo: 'https://example.com/x.png' }, 0)).rejects.toThrow(/data: URL/);
	});

	test('a picture path has to be an absolute path to an image', async () => {
		await expect(templateImageValue('logo.png')).rejects.toThrow(/absolute/);
		await expect(templateImageValue('/home/me/notes.txt')).rejects.toThrow(/PNG, JPEG, GIF or WebP/);
	});

	test('the ids of a title and their tags are offered as targets', async () => {
		expect(await svgElementIds(ART)).toEqual([
			{ id: 'g', tag: 'linearGradient' },
			{ id: 'top', tag: 'stop' },
			{ id: 'plate', tag: 'rect' },
			{ id: 'who', tag: 'text' },
			{ id: 'logo', tag: 'image' }
		]);
		expect(await svgElementIds('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><g/></svg>')).toEqual([]);
		await expect(svgElementIds('not svg at all')).rejects.toThrow(/not valid XML/);
	});
});

describe('moving templates in and out of the library (browser harness)', () => {
	test('delete removes the entry and leaves the titles made from it working', async () => {
		const { overlay_id } = await applyTemplate(SAMPLE_TEMPLATE_ID, {}, 0);
		await deleteTemplate(SAMPLE_TEMPLATE_ID);
		expect((await listTemplates()).templates).toEqual([]);
		await expect(applyTemplate(SAMPLE_TEMPLATE_ID, {}, 0)).rejects.toThrow(/no template/);
		const done = await setTemplateValues(overlay_id, { tagline: 'Still here' });
		expect(titles(done.timeline).find((t) => t.id === overlay_id)!.template?.values.tagline).toBe('Still here');
	});

	test('export names the file, and an import of it never overwrites', async () => {
		const info = await sample();
		expect(await exportTemplate(info.id, '/tmp/cards/')).toBe('/tmp/cards/Sample-end-card.kerftemplate');
		expect(await exportTemplate(info.id, '/tmp/card')).toBe('/tmp/card.kerftemplate');
		expect(await exportTemplate(info.id, '/tmp/card.kerftemplate')).toBe('/tmp/card.kerftemplate');
		await expect(exportTemplate(info.id, 'card.kerftemplate')).rejects.toThrow(/absolute/);
		await expect(exportTemplate('5b6f1c1e-0c64-4f6e-9a54-000000000000', '/tmp/x.kerftemplate')).rejects.toThrow(/no template/);

		const same = await importTemplate('/tmp/card.kerftemplate');
		expect(same.status).toBe('already_present');
		expect(same.info.id).toBe(info.id);
		expect((await listTemplates()).templates.length).toBe(1);

		await deleteTemplate(info.id);
		const added = await importTemplate('/tmp/card.kerftemplate');
		expect(added.status).toBe('added');
		expect(added.info.id).toBe(info.id);
		expect((await listTemplates()).templates.map((t) => t.id)).toEqual([info.id]);
	});

	test('an import refuses a path that is not a template file or is not there', async () => {
		await expect(importTemplate('/tmp/card.zip')).rejects.toThrow(/not a .kerftemplate file/);
		await expect(importTemplate('/tmp/nothing.kerftemplate')).rejects.toThrow(/does not exist/);
		await expect(importTemplate('card.kerftemplate')).rejects.toThrow(/relative path/);
	});
});

describe('a text control sets only its own text (browser harness)', () => {
	const text = (id: string): TemplateControl => ({ id, label: id, kind: 'text', targets: [{ element: id }] });
	const SVG = `<svg viewBox="0 0 10 10"><text id="tag" opacity="0"><animate attributeName="opacity" from="0" to="1" dur="1s" fill="freeze"/>Hello</text><text id="styled">Hi <tspan fill="red">there</tspan>!</text><text id="cdata">A<![CDATA[ <B> ]]>C</text><text id="kids"><tspan>x</tspan></text></svg>`;

	test('a child animation survives a new value', () => {
		const out = applyValues(SVG, [text('tag')], { tag: 'Bye' });
		expect(out).toContain('<text id="tag" opacity="0"><animate attributeName="opacity" from="0" to="1" dur="1s" fill="freeze"/>Bye</text>');
		expect(readValues(out, [text('tag')])).toEqual({ tag: 'Bye' });
	});

	test('a styled child stays and the other runs are emptied', () => {
		expect(readValues(SVG, [text('styled')])).toEqual({ styled: 'Hi !' });
		expect(applyValues(SVG, [text('styled')], { styled: 'Yo' })).toContain('<text id="styled">Yo<tspan fill="red">there</tspan></text>');
	});

	test('cdata is read as text and replaced with its run', () => {
		expect(readValues(SVG, [text('cdata')])).toEqual({ cdata: 'A <B> C' });
		expect(applyValues(SVG, [text('cdata')], { cdata: 'x' })).toContain('<text id="cdata">x</text>');
	});

	test('an element with only children gets the value before its end tag', () => {
		expect(applyValues(SVG, [text('kids')], { kids: 'y' })).toContain('<text id="kids"><tspan>x</tspan>y</text>');
	});
});
