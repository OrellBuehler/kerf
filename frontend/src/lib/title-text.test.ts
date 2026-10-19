import { describe, expect, test } from 'bun:test';
import {
	boxWidthRefusal,
	convertTitleTextIn,
	HARNESS_NOTE,
	setTitleTextBoxIn,
	titleTexts,
	withTextBox
} from './title-text';
import type { SvgOverlay, Timeline } from './types';

const doc = (body: string) => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 400 200">${body}</svg>`;

describe('a text box', () => {
	test('is written as set_text_box writes it, replacing the one the text had', () => {
		const src = doc(
			'<text id="t" x="5" y="50" inline-size="80" data-kerf-fit="shrink" style="fill:red; inline-size: 9px;opacity:.5">Hi</text>'
		);
		const wrapped = withTextBox(src, 't', { width: 120.5, fit: 'wrap' });
		expect(wrapped).toBe(doc('<text id="t" x="5" y="50" style="fill:red;opacity:.5" inline-size="120.5">Hi</text>'));
		const shrunk = withTextBox(wrapped, 't', { width: 300, fit: 'shrink' });
		expect(shrunk).toContain('inline-size="300" data-kerf-fit="shrink">Hi');
		expect(titleTexts(shrunk)).toEqual([{ id: 't', outlined: false, box: { width: 300, fit: 'shrink' } }]);
		const none = withTextBox(shrunk, 't', { width: null, fit: 'shrink' });
		expect(none).toBe(doc('<text id="t" x="5" y="50" style="fill:red;opacity:.5">Hi</text>'));
		expect(withTextBox(doc('<text id="t" x="5" y="50"/>'), 't', { width: 10, fit: 'wrap' })).toBe(
			doc('<text id="t" x="5" y="50" inline-size="10"/>')
		);
	});

	test('goes on one text, by its id, of a sane width', () => {
		const src = doc('<rect id="r" width="1" height="1"/><text id="t">a</text><text id="d">b</text><text id="d">c</text>');
		expect(() => withTextBox(src, 'r', { width: 10, fit: 'wrap' })).toThrow('<rect>');
		expect(() => withTextBox(src, 'x', { width: 10, fit: 'wrap' })).toThrow('no element');
		expect(() => withTextBox(src, 'd', { width: 10, fit: 'wrap' })).toThrow('more than one');
		expect(boxWidthRefusal('')).toBeNull();
		expect(boxWidthRefusal('120')).toBeNull();
		expect(boxWidthRefusal('0')).toContain('wider than nothing');
		expect(boxWidthRefusal('abc')).toContain('wider than nothing');
		expect(boxWidthRefusal('200000')).toContain('at most');
	});

	test('lists the texts with an id, in order, and reads a box in a style', () => {
		const src = doc(
			'<text id="a" style="inline-size: 50px">a</text><text>no id</text><g data-kerf-text="" id="b" inline-size="9">b</g><s:text xmlns:s="http://www.w3.org/2000/svg" id="c">c</s:text>'
		);
		expect(titleTexts(src)).toEqual([
			{ id: 'a', outlined: false, box: { width: 50, fit: 'wrap' } },
			{ id: 'b', outlined: true, box: { width: 9, fit: 'wrap' } },
			{ id: 'c', outlined: false, box: { width: null, fit: 'wrap' } }
		]);
	});
});

describe('the harness', () => {
	const overlay = (svg: string, extra: Partial<SvgOverlay> = {}): SvgOverlay =>
		({ id: 'o', name: 'Card', svg, start: 0, end: 4, x: 0, y: 0, width: 1, height: 1, opacity: 1, ...extra }) as SvgOverlay;
	const timeline = (o: SvgOverlay): Timeline => ({ tracks: [], svg_overlays: [o] }) as unknown as Timeline;

	test('writes a box and says it cannot set outlines', () => {
		const tl = timeline(overlay(doc('<text id="t">Hi</text>')));
		const { overlay: o, warnings } = setTitleTextBoxIn(tl, 'o', 't', { width: 90, fit: 'shrink' });
		expect(o?.svg).toContain('inline-size="90" data-kerf-fit="shrink"');
		expect(warnings).toEqual([HARNESS_NOTE]);
		expect(convertTitleTextIn(tl, 'o', 'outlines').warnings).toEqual([HARNESS_NOTE]);
	});

	test('takes outlines away, refuses a preset and a template going back to text', () => {
		const outlined = doc('<g data-kerf-text="" id="t">Hi<g data-kerf-glyphs=""><path d="M0 0h1z"/></g></g>');
		const tl = timeline(overlay(outlined));
		expect(convertTitleTextIn(tl, 'o', 'text').overlay?.svg).toBe(doc('<text id="t">Hi</text>'));
		expect(() => convertTitleTextIn(tl, 'o', 'text')).toThrow('already');
		const preset = timeline(overlay(outlined, { preset: { kind: 'fade_up', params: { text: 'Hi' } } } as Partial<SvgOverlay>));
		expect(() => setTitleTextBoxIn(preset, 'o', 't', { width: 9, fit: 'wrap' })).toThrow('preset');
		const template = timeline(
			overlay(outlined, { template: { template_id: 'x', name: 'T', controls: [], values: {} } } as Partial<SvgOverlay>)
		);
		expect(() => convertTitleTextIn(template, 'o', 'text')).toThrow('template');
	});
});
