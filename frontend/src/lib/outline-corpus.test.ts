import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/outline-corpus.json';
import { isOutlined, textForm, titleTexts } from './title-text';

// The differential test for `textForm`, the port of kerf-core's `restore_text`: kerf-core writes down titles and what
// setting their text as glyph outlines made of them (`svg/outline.rs` → `fixtures/outline-corpus.json`), and every
// case is replayed here. Regenerate with `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features --
// outline_corpus` after an intended change; this test then says what the port has to follow.

interface Case {
	name: string;
	outlined: string;
	text: string;
}

describe('the outline corpus', () => {
	const cases = (corpus as { cases: Case[] }).cases;

	test('has cases with outlines and one without', () => {
		expect(cases.filter((c) => isOutlined(c.outlined)).length).toBeGreaterThanOrEqual(10);
		expect(cases.some((c) => c.outlined === c.text)).toBe(true);
	});

	for (const c of cases) {
		test(`${c.name}: the text form is the title as it was`, () => {
			expect(textForm(c.outlined)).toBe(c.text);
			expect(textForm(c.text)).toBe(c.text);
		});
	}

	test('an outlined text keeps its id and its box on its start tag', () => {
		const boxed = cases.find((c) => c.name === 'a box that shrinks')!;
		expect(titleTexts(boxed.outlined)).toEqual([{ id: 'n', outlined: true, box: { width: 80, fit: 'shrink' } }]);
		expect(titleTexts(boxed.text)).toEqual([{ id: 'n', outlined: false, box: { width: 80, fit: 'shrink' } }]);
	});
});
