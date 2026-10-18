import { describe, expect, test } from 'bun:test';
import {
	addTarget,
	ambiguousFor,
	appliedLine,
	attributeChoices,
	changeKind,
	checkDraft,
	controlCount,
	controlProblem,
	defaultAttribute,
	deleteMessage,
	draftFrom,
	duplicateIds,
	editId,
	elementAttributes,
	elementsFor,
	exportFileName,
	fontsLine,
	holds,
	imageHref,
	importLine,
	isControlId,
	newDraft,
	normalizeValue,
	numberStep,
	numericAttributes,
	pickColor,
	pickElement,
	relabel,
	removeTarget,
	savedLine,
	setAttribute,
	slugId,
	swatchOf,
	targetsLine,
	templateTooltip,
	toControl,
	validSvgColor,
	valueRefusal,
	type DraftContext,
	type DraftControl
} from './template-ui';
import type { ImportOutcome, TemplateControl, TemplateInfo } from './types';

// The pure side of the template dialog and the Inspector's Template section: ids, attributes, the refusals the
// engine will make (kerf-core `template/validate.rs`) said before the round trip, and the words around an outcome.

const PNG = 'data:image/png;base64,iVBORw0KGgo=';
const SVG =
	'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1920 1080">' +
	'<defs><linearGradient id="bg"><stop id="top" offset="0" stop-color="teal"/></linearGradient></defs>' +
	'<rect id="plate" width="1920" height="1080" fill="url(#bg)" opacity="0.5"/>' +
	'<text id="headline" x="960" y="500" font-family="Geist" font-size="120" fill="white">Thanks</text>' +
	`<image id="logo" x="0" y="0" width="40" height="40" href="${PNG}"/>` +
	'<image id="old" xlink:href="data:image/png;base64,AAAA" width="4" height="4"/>' +
	'<circle id="dot" r="12" cx="5" cy="5"/>' +
	'</svg>';
const ELEMENTS = [
	{ id: 'bg', tag: 'linearGradient' },
	{ id: 'top', tag: 'stop' },
	{ id: 'plate', tag: 'rect' },
	{ id: 'headline', tag: 'text' },
	{ id: 'logo', tag: 'image' },
	{ id: 'old', tag: 'image' },
	{ id: 'dot', tag: 'circle' }
];
const ctx: DraftContext = { svg: SVG, elements: ELEMENTS };

const draft = (over: Partial<DraftControl> = {}): DraftControl => ({
	key: 1,
	id: 'tagline',
	idTouched: true,
	label: 'Tagline',
	kind: 'text',
	targets: [{ element: 'headline', attribute: '', touched: false }],
	min: '',
	max: '',
	...over
});

describe('slugId', () => {
	test('lower case, accents dropped, runs of anything else a hyphen', () => {
		expect(slugId('Brand colour')).toBe('brand-colour');
		expect(slugId('  Über  Größe!! ')).toBe('uber-gro-e');
		expect(slugId('Tagline_2 / main')).toBe('tagline-2-main');
	});

	test('begins with a letter and is never empty', () => {
		expect(slugId('2nd line')).toBe('c-2nd-line');
		expect(slugId('')).toBe('control');
		expect(slugId('!!!')).toBe('control');
		expect(slugId('日本語')).toBe('control');
	});

	test('is a valid control id, at most 64 characters, without a trailing hyphen', () => {
		const long = slugId('word '.repeat(40));
		expect(long.length).toBeLessThanOrEqual(64);
		expect(long.endsWith('-')).toBe(false);
		for (const label of ['Brand colour', '2nd line', '', 'x'.repeat(200), 'a-b_c']) expect(isControlId(slugId(label))).toBe(true);
	});

	test('steps past an id already taken, staying within the limit', () => {
		expect(slugId('Tagline', ['tagline'])).toBe('tagline-2');
		expect(slugId('Tagline', new Set(['tagline', 'tagline-2']))).toBe('tagline-3');
		const taken = ['x'.repeat(64)];
		const next = slugId('x'.repeat(100), taken);
		expect(next.length).toBeLessThanOrEqual(64);
		expect(next).not.toBe(taken[0]);
		expect(next.endsWith('-2')).toBe(true);
	});

	test('control ids are what the engine reads', () => {
		for (const ok of ['a', 'Tagline', 'brand-colour', 'a_b', 'x'.repeat(64)]) expect(isControlId(ok)).toBe(true);
		for (const bad of ['', '1a', '-a', 'a b', 'ä', 'x'.repeat(65)]) expect(isControlId(bad)).toBe(false);
	});
});

describe('reading the title for attributes', () => {
	test('an element by its id, with its attributes', () => {
		const rect = elementAttributes(SVG, 'plate')!;
		expect(rect.get('fill')).toBe('url(#bg)');
		expect(rect.has('opacity')).toBe(true);
		expect(elementAttributes(SVG, 'nope')).toBeNull();
		expect(elementAttributes(SVG, 'top')?.get('stop-color')).toBe('teal');
	});

	test('single quotes, and a > inside a value, are read', () => {
		const svg = `<svg xmlns='http://www.w3.org/2000/svg'><text id='t' data-x="a>b" fill='red'>x</text></svg>`;
		expect(elementAttributes(svg, 't')?.get('fill')).toBe('red');
	});

	test('the numbers an element could be driven by', () => {
		expect(numericAttributes(SVG, 'dot').sort()).toEqual(['cx', 'cy', 'r']);
		expect(numericAttributes(SVG, 'headline').sort()).toEqual(['font-size', 'x', 'y']);
		expect(numericAttributes(SVG, 'gone')).toEqual([]);
	});

	test('an image control shows the data URL in the artwork, and only one an <img> may take', () => {
		const logo = { targets: [{ element: 'logo', attribute: 'href' }] };
		expect(imageHref(SVG, logo)).toBe(PNG);
		expect(imageHref(SVG, { targets: [{ element: 'old', attribute: 'xlink:href' }] })).toBe('data:image/png;base64,AAAA');
		expect(imageHref(SVG, { targets: [{ element: 'logo', attribute: 'xlink:href' }] })).toBeNull();
		expect(imageHref(SVG, { targets: [{ element: 'gone', attribute: 'href' }] })).toBeNull();
		expect(imageHref(SVG, { targets: [] })).toBeNull();
		const hostile = '<svg xmlns="http://www.w3.org/2000/svg"><image id="a" href="data:image/svg+xml;base64,AAAA"/><image id="b" href="https://example.com/x.png"/></svg>';
		expect(imageHref(hostile, { targets: [{ element: 'a', attribute: 'href' }] })).toBeNull();
		expect(imageHref(hostile, { targets: [{ element: 'b', attribute: 'href' }] })).toBeNull();
	});
});

describe('which attribute a control sets', () => {
	test('a color is what the element draws with: a stop its stop-color, else its fill', () => {
		expect(defaultAttribute('color', 'stop', new Set(['stop-color', 'offset']))).toBe('stop-color');
		expect(defaultAttribute('color', 'rect', new Set(['fill', 'opacity']))).toBe('fill');
		expect(defaultAttribute('color', 'path', new Set(['stroke', 'd']))).toBe('stroke');
		expect(defaultAttribute('color', 'rect', null)).toBe('fill');
		expect(defaultAttribute('color', 'stop', null)).toBe('stop-color');
	});

	test('an image is its href, or the xlink spelling when that is all it has', () => {
		expect(defaultAttribute('image', 'image', new Set(['href']))).toBe('href');
		expect(defaultAttribute('image', 'image', new Set(['xlink:href']))).toBe('xlink:href');
		expect(defaultAttribute('image', 'image', null)).toBe('href');
	});

	test('a font is font-family, text has none, a number is the shape’s usual size when it has one', () => {
		expect(defaultAttribute('font', 'text', null)).toBe('font-family');
		expect(defaultAttribute('text', 'text', null)).toBe('');
		expect(defaultAttribute('number', 'text', new Set(['font-size', 'x']))).toBe('font-size');
		expect(defaultAttribute('number', 'circle', new Set(['r', 'cx']))).toBe('r');
		expect(defaultAttribute('number', 'g', new Set(['transform']))).toBe('');
		expect(defaultAttribute('number', 'rect', null)).toBe('width');
	});

	test('the closed sets are the engine’s', () => {
		expect(attributeChoices('color')).toEqual(['fill', 'stroke', 'stop-color', 'flood-color', 'lighting-color', 'color']);
		expect(attributeChoices('image')).toEqual(['href', 'xlink:href']);
		expect(attributeChoices('font')).toEqual(['font-family']);
		expect(attributeChoices('number')).toBeNull();
		expect(attributeChoices('text')).toEqual([]);
	});

	test('a text control aims at text, an image control at an image, the rest at anything with an id', () => {
		expect(elementsFor('text', ELEMENTS).map((e) => e.id)).toEqual(['headline']);
		expect(elementsFor('image', ELEMENTS).map((e) => e.id)).toEqual(['logo', 'old']);
		expect(elementsFor('color', ELEMENTS)).toHaveLength(ELEMENTS.length);
		expect(elementsFor('number', [...ELEMENTS, { id: 'extra', tag: 'circle' }])).toHaveLength(ELEMENTS.length + 1);
	});
});

describe('drafting a control', () => {
	test('the id follows the label until it is edited by hand, and stays clear of the others', () => {
		const a = relabel(newDraft(1, ctx), 'Brand colour', []);
		expect(a.id).toBe('brand-colour');
		const other = draft({ key: 2, id: 'brand-colour' });
		expect(relabel(newDraft(3, ctx), 'Brand colour', [other]).id).toBe('brand-colour-2');
		expect(relabel({ ...a, idTouched: true }, 'Something else', []).id).toBe('brand-colour');
		expect(relabel(a, '   ', []).id).toBe('');
	});

	test('an id typed in, or emptied, is left as it is: the label never rewrites it again', () => {
		const a = relabel(newDraft(1, ctx), 'Brand colour', []);
		const typed = editId(a, 'logo-colour');
		expect(typed).toMatchObject({ id: 'logo-colour', idTouched: true });
		const emptied = editId(typed, '');
		expect(emptied).toMatchObject({ id: '', idTouched: true });
		expect(relabel(emptied, 'Another label', []).id).toBe('');
		expect(controlProblem(emptied, [emptied], ctx)).toContain('Give it an id');
	});

	test('a new control with a single choice of element has it picked', () => {
		expect(newDraft(1, ctx, 'text').targets[0]).toEqual({ element: 'headline', attribute: '', touched: false });
		expect(newDraft(1, ctx, 'color').targets[0].element).toBe('');
		expect(newDraft(1, ctx, 'font').targets[0].attribute).toBe('font-family');
	});

	test('picking an element sets its likely attribute, but not over one chosen by hand', () => {
		let d = newDraft(1, ctx, 'color');
		d = pickElement(d, 0, 'top', ctx);
		expect(d.targets[0]).toEqual({ element: 'top', attribute: 'stop-color', touched: false });
		d = pickElement(d, 0, 'plate', ctx);
		expect(d.targets[0].attribute).toBe('fill');
		d = setAttribute(d, 0, 'stroke');
		d = pickElement(d, 0, 'headline', ctx);
		expect(d.targets[0]).toEqual({ element: 'headline', attribute: 'stroke', touched: true });
	});

	test('changing the kind clears a target the new kind cannot aim at and drops bounds', () => {
		let d = pickElement(newDraft(1, ctx, 'color'), 0, 'plate', ctx);
		d = changeKind(d, 'image', ctx);
		expect(d.targets[0].element).toBe('');
		d = pickElement(d, 0, 'logo', ctx);
		expect(d.targets[0]).toEqual({ element: 'logo', attribute: 'href', touched: false });
		const num = { ...pickElement(newDraft(2, ctx, 'number'), 0, 'dot', ctx), min: '1', max: '9' };
		expect(num.targets[0].attribute).toBe('r');
		expect(changeKind(num, 'color', ctx)).toMatchObject({ min: '', max: '' });
		expect(changeKind(num, 'text', ctx).targets[0].attribute).toBe('');
	});

	test('a kind that can only aim at one element takes it', () => {
		const d = changeKind(pickElement(newDraft(1, ctx, 'color'), 0, 'plate', ctx), 'text', ctx);
		expect(d.targets[0]).toEqual({ element: 'headline', attribute: '', touched: false });
	});

	test('targets are added up to sixteen and removed down to one', () => {
		let d = draft();
		for (let i = 0; i < 20; i++) d = addTarget(d);
		expect(d.targets).toHaveLength(16);
		for (let i = 0; i < 20; i++) d = removeTarget(d, 0);
		expect(d.targets).toHaveLength(1);
	});

	test('a draft becomes the control the engine takes: trimmed, no attribute on text, bounds on a number only', () => {
		expect(toControl(draft({ id: ' tagline ', label: ' Tagline ' }))).toEqual({
			id: 'tagline',
			label: 'Tagline',
			kind: 'text',
			targets: [{ element: 'headline' }]
		});
		const num = toControl(
			draft({ kind: 'number', id: 'size', label: 'Size', min: '40', max: ' 200 ', targets: [{ element: 'headline', attribute: ' font-size ', touched: true }] })
		);
		expect(num).toEqual({ id: 'size', label: 'Size', kind: 'number', targets: [{ element: 'headline', attribute: 'font-size' }], min: 40, max: 200 });
		const color = toControl(draft({ kind: 'color', min: '1', max: '2', targets: [{ element: 'plate', attribute: 'fill', touched: true }] }));
		expect(color.min).toBeUndefined();
		expect(color.max).toBeUndefined();
		expect(toControl(draft({ kind: 'number', min: 'x', max: '', targets: [{ element: 'dot', attribute: 'r', touched: true }] })).min).toBeUndefined();
	});

	test('a saved control is a draft and back', () => {
		const c: TemplateControl = {
			id: 'size',
			label: 'Size',
			kind: 'number',
			targets: [{ element: 'headline', attribute: 'font-size' }, { element: 'dot', attribute: 'r' }],
			min: 0.5,
			max: 200
		};
		expect(toControl(draftFrom(c, 7))).toEqual(c);
		const text: TemplateControl = { id: 'tag', label: 'Tag', kind: 'text', targets: [{ element: 'headline' }] };
		expect(toControl(draftFrom(text, 8))).toEqual(text);
	});
});

describe('what is wrong with a control', () => {
	const colorDraft = (over: Partial<DraftControl> = {}) =>
		draft({ id: 'brand', label: 'Brand', kind: 'color', targets: [{ element: 'plate', attribute: 'fill', touched: true }], ...over });

	test('a good one has no problem', () => {
		expect(controlProblem(draft(), [draft()], ctx)).toBeNull();
		expect(controlProblem(colorDraft(), [colorDraft()], ctx)).toBeNull();
		const image = draft({ kind: 'image', id: 'logo', label: 'Logo', targets: [{ element: 'logo', attribute: 'href', touched: false }] });
		expect(controlProblem(image, [image], ctx)).toBeNull();
	});

	test('a label, an id, and an id nobody else has', () => {
		expect(controlProblem(draft({ label: '  ' }), [], ctx)).toContain('label');
		expect(controlProblem(draft({ label: 'x'.repeat(81) }), [], ctx)).toContain('80');
		expect(controlProblem(draft({ id: '1x' }), [], ctx)).toContain('id');
		expect(controlProblem(draft({ id: '' }), [], ctx)).toContain('id');
		const a = draft({ key: 1, id: 'same' });
		const b = draft({ key: 2, id: 'same', targets: [{ element: 'headline', attribute: '', touched: false }] });
		expect(controlProblem(b, [a, b], ctx)).toContain('already uses the id');
	});

	test('the element has to be picked, to exist, and to suit the kind', () => {
		expect(controlProblem(draft({ targets: [{ element: '', attribute: '', touched: false }] }), [], ctx)).toContain('Pick the element');
		expect(controlProblem(draft({ targets: [{ element: 'ghost', attribute: '', touched: false }] }), [], ctx)).toContain('no element');
		expect(controlProblem(draft({ targets: [{ element: 'plate', attribute: '', touched: false }] }), [], ctx)).toContain('not text');
		expect(controlProblem(colorDraft({ kind: 'image', targets: [{ element: 'plate', attribute: 'href', touched: true }] }), [], ctx)).toContain('<image>');
	});

	test('an attribute for every kind but text, from the kind’s set, that the element has', () => {
		expect(controlProblem(colorDraft({ targets: [{ element: 'plate', attribute: '', touched: false }] }), [], ctx)).toContain('which attribute');
		expect(controlProblem(colorDraft({ targets: [{ element: 'plate', attribute: 'width', touched: true }] }), [], ctx)).toContain('cannot set');
		expect(controlProblem(colorDraft({ targets: [{ element: 'headline', attribute: 'stroke', touched: true }] }), [], ctx)).toContain('has no “stroke”');
		const num = (attribute: string) => draft({ kind: 'number', id: 'n', label: 'N', targets: [{ element: 'dot', attribute, touched: true }] });
		expect(controlProblem(num('r'), [], ctx)).toBeNull();
		expect(controlProblem(num('id'), [], ctx)).toContain('cannot set');
		expect(controlProblem(num('onclick'), [], ctx)).toContain('cannot set');
		expect(controlProblem(num('style'), [], ctx)).toContain('cannot set');
		expect(controlProblem(num('a b'), [], ctx)).toContain('cannot set');
	});

	test('a number’s bounds are numbers, the low one not above the high one', () => {
		const num = (min: string, max: string) =>
			draft({ kind: 'number', id: 'n', label: 'N', min, max, targets: [{ element: 'dot', attribute: 'r', touched: true }] });
		expect(controlProblem(num('1', '10'), [], ctx)).toBeNull();
		expect(controlProblem(num('', ''), [], ctx)).toBeNull();
		expect(controlProblem(num('abc', ''), [], ctx)).toContain('Min is not a number');
		expect(controlProblem(num('', '5px'), [], ctx)).toContain('Max is not a number');
		expect(controlProblem(num('10', '1'), [], ctx)).toContain('above');
	});

	test('two controls, or two targets of one, cannot write the same place', () => {
		const a = draft({ key: 1, id: 'a', label: 'A' });
		const b = draft({ key: 2, id: 'b', label: 'B' });
		expect(controlProblem(a, [a, b], ctx)).toBeNull();
		expect(controlProblem(b, [a, b], ctx)).toContain('already sets the text of “headline”');
		const twice = draft({ targets: [{ element: 'headline', attribute: '', touched: false }, { element: 'headline', attribute: '', touched: false }] });
		expect(controlProblem(twice, [twice], ctx)).toContain('more than once');
		const fill = colorDraft({ key: 3, id: 'c', label: 'C' });
		const stroke = colorDraft({ key: 4, id: 'd', label: 'D', targets: [{ element: 'plate', attribute: 'opacity', touched: true }] });
		expect(controlProblem(stroke, [fill, stroke], ctx)).not.toContain('already sets');
	});

	test('the whole form: a name, and every control clear', () => {
		const good = checkDraft('End card', '', [draft()], ctx);
		expect(good).toEqual({ name: null, description: null, controls: [null], ok: true });
		expect(checkDraft('  ', '', [], ctx)).toEqual({ name: null, description: null, controls: [], ok: true });
		expect(checkDraft('x'.repeat(121), '', [], ctx).name).toContain('120');
		expect(checkDraft('Card', 'y'.repeat(2001), [], ctx).ok).toBe(false);
		expect(checkDraft('Card', '', [draft({ label: '' })], ctx).ok).toBe(false);
		expect(checkDraft('Card', '', [], ctx).ok).toBe(true);
	});

	test('a blank name is allowed: the engine takes the title’s own, as `build_template` has it', () => {
		expect(checkDraft('', '', [draft()], ctx).ok).toBe(true);
		expect(checkDraft('   ', '  ', [], ctx).ok).toBe(true);
	});

	test('no control characters in a name, a description or a label (a tab and a line break are fine)', () => {
		expect(checkDraft('Car\u0007d', '', [], ctx).name).toContain('control characters');
		expect(checkDraft('Card', 'a\u0000b', [], ctx).description).toContain('control characters');
		expect(checkDraft('Card', 'two\nlines\tand a tab', [], ctx).ok).toBe(true);
		expect(checkDraft('A\tB', '', [], ctx).ok).toBe(true);
		expect(controlProblem(draft({ label: 'Tag\u001Bline' }), [], ctx)).toContain('control characters');
		expect(controlProblem(draft({ label: 'Tag\tline' }), [], ctx)).toBeNull();
	});

	test('lengths are counted on the trimmed text, in characters, as the engine does', () => {
		expect(checkDraft(`  ${'x'.repeat(120)}  `, '', [], ctx).name).toBeNull();
		expect(checkDraft('x'.repeat(121), '', [], ctx).name).toContain('120');
		expect(checkDraft('Card', `${'y'.repeat(2000)}   `, [], ctx).description).toBeNull();
		expect(checkDraft('Card', '', [draft({ label: '😀'.repeat(80) })], ctx).ok).toBe(true);
		expect(checkDraft('Card', '', [draft({ label: '😀'.repeat(81) })], ctx).ok).toBe(false);
	});

	test('a control is described by where it writes', () => {
		expect(targetsLine({ targets: [{ element: 'headline' }] })).toBe('text of headline');
		expect(targetsLine({ targets: [{ element: 'top', attribute: 'stop-color' }] })).toBe('stop-color of top');
		expect(targetsLine({ targets: [{ element: 'a' }, { element: 'b' }, { element: 'c' }, { element: 'd' }] })).toBe('text of a, text of b +2');
	});
});

describe('an id more than one element carries', () => {
	const dupes = [
		{ id: 'title', tag: 'text' },
		{ id: 'title', tag: 'text' },
		{ id: 'sub', tag: 'text' },
		{ id: 'bar', tag: 'rect' },
		{ id: 'bar', tag: 'circle' },
		{ id: 'logo', tag: 'image' }
	];
	const art = '<svg xmlns="http://www.w3.org/2000/svg"><text id="title">A</text><text id="title">B</text><text id="sub">C</text></svg>';
	const dupCtx: DraftContext = { svg: art, elements: dupes };

	test('is not offered as a target, and is listed apart with how many carry it', () => {
		expect(duplicateIds(dupes)).toEqual(new Map([['title', 2], ['bar', 2]]));
		expect(elementsFor('text', dupes).map((e) => e.id)).toEqual(['sub']);
		expect(elementsFor('color', dupes).map((e) => e.id)).toEqual(['sub', 'logo']);
		expect(ambiguousFor('text', dupes)).toEqual([{ id: 'title', count: 2 }]);
		expect(ambiguousFor('image', dupes)).toEqual([]);
		expect(ambiguousFor('color', dupes)).toEqual([{ id: 'title', count: 2 }, { id: 'bar', count: 2 }]);
	});

	test('is a problem for a control that already aims at it, as the engine counts the elements', () => {
		const d = draft({ targets: [{ element: 'title', attribute: '', touched: false }] });
		expect(controlProblem(d, [d], dupCtx)).toContain('2 elements are called “title”');
		const ok = draft({ targets: [{ element: 'sub', attribute: '', touched: false }] });
		expect(controlProblem(ok, [ok], dupCtx)).toBeNull();
		expect(newDraft(1, dupCtx, 'text').targets[0].element).toBe('sub');
	});
});

describe('text that holds another control’s target', () => {
	const NESTED =
		'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">' +
		'<!-- <text id="fake"> --><g id="card"><text id="outer" fill="red">Hello <tspan id="inner" fill="blue">world</tspan></text>' +
		'<text id="beside">x</text></g><![CDATA[ <text id="cdata"> ]]><rect id="leaf" fill="red"/></svg>';
	const elements = [
		{ id: 'card', tag: 'g' },
		{ id: 'outer', tag: 'text' },
		{ id: 'inner', tag: 'tspan' },
		{ id: 'beside', tag: 'text' },
		{ id: 'leaf', tag: 'rect' }
	];
	const nctx: DraftContext = { svg: NESTED, elements };
	const text = (key: number, id: string, element: string) =>
		draft({ key, id, label: id, targets: [{ element, attribute: '', touched: false }] });
	const color = (key: number, id: string, element: string, attribute = 'fill') =>
		draft({ key, id, label: id, kind: 'color', targets: [{ element, attribute, touched: true }] });

	test('knows which element is inside which, through comments, CDATA and self-closing tags', () => {
		expect(holds(NESTED, 'outer', 'inner')).toBe(true);
		expect(holds(NESTED, 'card', 'inner')).toBe(true);
		expect(holds(NESTED, 'card', 'beside')).toBe(true);
		expect(holds(NESTED, 'card', 'leaf')).toBe(false);
		expect(holds(NESTED, 'inner', 'outer')).toBe(false);
		expect(holds(NESTED, 'outer', 'beside')).toBe(false);
		expect(holds(NESTED, 'outer', 'leaf')).toBe(false);
		expect(holds(NESTED, 'outer', 'outer')).toBe(false);
		expect(holds(NESTED, 'outer', 'fake')).toBe(false);
		expect(holds(NESTED, 'outer', 'nowhere')).toBe(false);
	});

	test('a text control on the outer text cannot sit beside a control on the inner element', () => {
		const outer = text(1, 'greeting', 'outer');
		const inner = color(2, 'accent', 'inner');
		expect(controlProblem(outer, [outer, inner], nctx)).toContain('replaces the text of “outer”');
		expect(controlProblem(outer, [outer, inner], nctx)).toContain('“inner”');
		expect(controlProblem(outer, [outer, inner], nctx)).toContain('“accent”');
		expect(checkDraft('Card', '', [outer, inner], nctx).ok).toBe(false);
		const innerText = text(3, 'word', 'inner');
		expect(controlProblem(outer, [outer, innerText], nctx)).toContain('“word”');
		const group = { ...text(4, 'all', 'card'), kind: 'color' as const, targets: [{ element: 'card', attribute: 'fill', touched: true }] };
		expect(controlProblem(outer, [outer, group], nctx)).toBeNull();
	});

	test('the order of the controls does not matter, and one control’s own targets count', () => {
		const outer = text(1, 'greeting', 'outer');
		const inner = color(2, 'accent', 'inner');
		expect(controlProblem(outer, [inner, outer], nctx)).toContain('replaces the text of “outer”');
		const both = draft({ targets: [{ element: 'outer', attribute: '', touched: false }, { element: 'inner', attribute: '', touched: false }] });
		expect(controlProblem(both, [both], nctx)).toContain('this control also sets');
	});

	test('neighbours and the inner control’s own fill on the outer element are fine', () => {
		const outer = text(1, 'greeting', 'outer');
		const beside = text(2, 'other', 'beside');
		const fill = color(3, 'ink', 'outer');
		const leaf = color(4, 'leaf', 'leaf');
		expect(controlProblem(outer, [outer, beside, fill, leaf], nctx)).toBeNull();
		expect(checkDraft('Card', '', [outer, beside, fill, leaf], nctx).ok).toBe(true);
	});
});

describe('a color the engine takes', () => {
	test('hex in its four lengths, the SVG names in any case, none and currentColor spelled so', () => {
		for (const ok of ['#fff', '#FFFF', '#ffffff', '#FFFFFF80', 'red', 'RED', 'CornflowerBlue', 'transparent', 'none', 'currentColor']) {
			expect(validSvgColor(ok)).toBe(true);
		}
	});

	test('nothing else: not a word SVG does not know, a function, a hex of another length, FFmpeg’s spellings', () => {
		for (const bad of ['', '#', '#ggg', '#12', '#12345', '#1234567', 'reddish', 'rgb', 'None', 'currentcolor', 'CURRENTCOLOR', 'rgb(1,2,3)', 'hsl(10 40% 30%)', 'red@0.5', '0xff0000', 'red;', 'red blue', 'a(b)', '"red"', 'javascript:1']) {
			expect(validSvgColor(bad)).toBe(false);
		}
	});

	test('a field says why in words: a function, a word SVG does not know, anything else', () => {
		expect(valueRefusal({ kind: 'color' }, 'reddish')).toContain('not a color name');
		expect(valueRefusal({ kind: 'color' }, 'rgb(1, 2, 3)')).toContain('not as a function');
		expect(valueRefusal({ kind: 'color' }, 'None')).not.toBeNull();
		expect(valueRefusal({ kind: 'color' }, ' CornflowerBlue ')).toBeNull();
		expect(valueRefusal({ kind: 'color' }, '#12345')).toContain('#rrggbb');
	});
});

describe('a value before it is sent', () => {
	test('text up to 2000 characters without control characters; tabs and line breaks pass', () => {
		expect(valueRefusal({ kind: 'text' }, 'Thanks for watching')).toBeNull();
		expect(valueRefusal({ kind: 'text' }, '')).toBeNull();
		expect(valueRefusal({ kind: 'text' }, 'a\tb\nc')).toBeNull();
		expect(valueRefusal({ kind: 'text' }, 'x'.repeat(2001))).toContain('2000');
		expect(valueRefusal({ kind: 'text' }, 'a\u0007b')).toContain('Control');
	});

	test('a color is hex, a name, none or currentColor; a function is explained', () => {
		for (const ok of ['#ff0000', '#f00', '#f00a', '#ff000080', ' teal ', 'none', 'currentColor']) expect(valueRefusal({ kind: 'color' }, ok)).toBeNull();
		expect(valueRefusal({ kind: 'color' }, 'rgb(1, 2, 3)')).toContain('not as a function');
		expect(valueRefusal({ kind: 'color' }, 'hsl(10 40% 30%)')).toContain('not as a function');
		for (const bad of ['', '#12', '#12345', 'red@0.5', '0xff0000', 'tea l']) expect(valueRefusal({ kind: 'color' }, bad)).not.toBeNull();
	});

	test('a number is unitless, finite and inside its bounds', () => {
		const n = { kind: 'number' as const, min: 40, max: 200 };
		expect(valueRefusal(n, '100')).toBeNull();
		expect(valueRefusal(n, ' 40 ')).toBeNull();
		expect(valueRefusal(n, '12.5e1')).toBeNull();
		expect(valueRefusal(n, '')).toContain('Enter');
		expect(valueRefusal(n, 'abc')).toContain('Not a number');
		expect(valueRefusal(n, '5px')).toContain('no unit');
		expect(valueRefusal(n, '39')).toContain('Between 40 and 200');
		expect(valueRefusal(n, '201')).toContain('Between 40 and 200');
		expect(valueRefusal({ kind: 'number', min: 0 }, '-1')).toBe('At least 0.');
		expect(valueRefusal({ kind: 'number', max: 10 }, '11')).toBe('At most 10.');
		expect(valueRefusal({ kind: 'number' }, '1e999')).toContain('finite');
		expect(valueRefusal({ kind: 'number' }, '-5')).toBeNull();
	});

	test('a font is one family name', () => {
		for (const ok of ['Barlow Condensed', 'Inter', 'Noto Sans JP', 'Font_1.2-b']) expect(valueRefusal({ kind: 'font' }, ok)).toBeNull();
		for (const bad of ['', '   ', 'A, B', 'x'.repeat(65), 'a"b', 'a;b']) expect(valueRefusal({ kind: 'font' }, bad)).not.toBeNull();
	});

	test('an image is a data URL; picking one is the only way to make it', () => {
		expect(valueRefusal({ kind: 'image' }, PNG)).toBeNull();
		expect(valueRefusal({ kind: 'image' }, '/home/me/logo.png')).toContain('Pick');
	});

	test('numbers go out in plain decimal, colors and fonts trimmed, text untouched', () => {
		expect(normalizeValue({ kind: 'number' }, ' 100 ')).toBe('100');
		expect(normalizeValue({ kind: 'number' }, '0.50')).toBe('0.5');
		expect(normalizeValue({ kind: 'number' }, '-0')).toBe('0');
		expect(normalizeValue({ kind: 'number' }, '1e21')).toBe('1000000000000000000000');
		expect(normalizeValue({ kind: 'number' }, '1e-7')).toBe('0.0000001');
		expect(normalizeValue({ kind: 'color' }, ' teal ')).toBe('teal');
		expect(normalizeValue({ kind: 'font' }, ' Inter ')).toBe('Inter');
		expect(normalizeValue({ kind: 'text' }, '  spaced  ')).toBe('  spaced  ');
	});

	test('a number field steps finely over a small range and by one over a large one', () => {
		expect(numberStep({ min: 0, max: 1 })).toBe(0.01);
		expect(numberStep({ min: 0, max: 10 })).toBe(0.1);
		expect(numberStep({ min: 40, max: 200 })).toBe(1);
		expect(numberStep({})).toBe(1);
		expect(numberStep({ min: 0 })).toBe(1);
	});
});

describe('the color swatch', () => {
	test('any hex spelling shows as #rrggbb; anything else leaves it to the component', () => {
		expect(swatchOf('#336699')).toBe('#336699');
		expect(swatchOf('#336699CC')).toBe('#336699');
		expect(swatchOf('#369')).toBe('#336699');
		expect(swatchOf('#369c')).toBe('#336699');
		for (const other of ['teal', 'none', 'currentColor', '', null, undefined, '#12']) expect(swatchOf(other)).toBeNull();
	});

	test('a pick is lower case and keeps the alpha a hex spelling had', () => {
		expect(pickColor('teal', '#FF8800')).toBe('#ff8800');
		expect(pickColor('#336699', '#ff8800')).toBe('#ff8800');
		expect(pickColor('#33669980', '#ff8800')).toBe('#ff880080');
		expect(pickColor('#369a', '#ff8800')).toBe('#ff8800aa');
		expect(pickColor(undefined, '#ff8800')).toBe('#ff8800');
	});
});

describe('the library’s words', () => {
	const info = (over: Partial<TemplateInfo> = {}): TemplateInfo => ({
		id: '5b6f1c1e-0c64-4f6e-9a54-3f3a8c1d7a01',
		name: 'End card',
		description: '',
		duration: 4,
		width: 1920,
		height: 1080,
		controls: [],
		fonts: [],
		has_poster: true,
		size_bytes: 1000,
		...over
	});

	test('controls are counted', () => {
		expect(controlCount(0)).toBe('0 controls');
		expect(controlCount(1)).toBe('1 control');
		expect(controlCount(4)).toBe('4 controls');
	});

	test('fonts in a line, two and a count', () => {
		expect(fontsLine([])).toBe('');
		expect(fontsLine(['Inter'])).toBe('Inter');
		expect(fontsLine(['Inter', 'Barlow'])).toBe('Inter, Barlow');
		expect(fontsLine(['A', 'B', 'C', 'D'])).toBe('A, B +2');
	});

	test('saving says so, or says what could not be carried', () => {
		expect(savedLine({ info: info(), warnings: [] })).toEqual({ kind: 'success', text: 'Saved “End card” to your templates' });
		const line = savedLine({ info: info(), warnings: ['the font X is restricted', 'no poster'] });
		expect(line.kind).toBe('warning');
		expect(line.detail).toBe('the font X is restricted no poster');
	});

	test('importing never overwrites, and says when it did not add or kept a copy', () => {
		const out = (status: ImportOutcome['status'], warnings: string[] = []): ImportOutcome => ({ info: info(), status, warnings });
		expect(importLine(out('added'))).toEqual({ kind: 'success', text: 'Added “End card” to your templates', detail: undefined });
		expect(importLine(out('already_present')).kind).toBe('info');
		expect(importLine(out('already_present')).text).toContain('already in your templates');
		const copy = importLine(out('added_as_copy'));
		expect(copy.kind).toBe('warning');
		expect(copy.text).toContain('as a copy');
		expect(copy.detail).toContain('same id');
		expect(importLine(out('added', ['font missing'])).kind).toBe('warning');
	});

	test('applying is quiet unless the engine had notes', () => {
		expect(appliedLine('End card', [])).toBeNull();
		expect(appliedLine('End card', ['Barlow is not installed'])).toEqual({
			kind: 'warning',
			text: '“End card” has notes',
			detail: 'Barlow is not installed'
		});
	});

	test('the tooltip leads with the description, then the controls and the fonts', () => {
		const c: TemplateControl = { id: 'a', label: 'Tagline', kind: 'text', targets: [{ element: 'a' }] };
		expect(templateTooltip(info({ description: 'A card', controls: [c], fonts: ['Inter'] }))).toBe('A card\nTagline\nCarries Inter');
		expect(templateTooltip(info())).toBe('End card\nNo controls: it can be applied but not changed');
	});

	test('delete asks with the name and says titles keep working', () => {
		expect(deleteMessage('End card')).toContain('“End card”');
		expect(deleteMessage('End card')).toContain('keep working');
	});

	test('the file offered in the save dialog', () => {
		expect(exportFileName('End card')).toBe('End-card.kerftemplate');
		expect(exportFileName('Über / Größe')).toBe('Uber-Gro-e.kerftemplate');
		expect(exportFileName('日本')).toBe('template.kerftemplate');
		expect(exportFileName('x'.repeat(100)).length).toBeLessThanOrEqual(60 + '.kerftemplate'.length);
	});
});
