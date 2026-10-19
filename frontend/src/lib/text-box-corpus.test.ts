import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/text-box-corpus.json';
import { titleTexts, withTextBox } from './title-text';
import type { TextBox } from './types';

// The differential test for `withTextBox` and the box `titleTexts` reads, ports of kerf-core's `set_text_box` and
// `text_box_of`: kerf-core writes down titles, a box set on one of their texts and what it wrote (or refused) and reads
// back (`svg/text_box.rs` → `fixtures/text-box-corpus.json`), and every case is replayed here. Regenerate with
// `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- text_box_corpus` after an intended change.

interface Case {
	name: string;
	svg: string;
	element: string;
	box: TextBox;
	expect: { ok: { svg: string; read: TextBox | null } } | { err: string };
}

describe('the text box corpus', () => {
	const cases = (corpus as { cases: Case[] }).cases;

	test('has answers and refusals', () => {
		expect(cases.filter((c) => 'ok' in c.expect).length).toBeGreaterThanOrEqual(10);
		expect(cases.filter((c) => 'err' in c.expect).length).toBeGreaterThanOrEqual(4);
	});

	for (const c of cases) {
		test(c.name, () => {
			if ('err' in c.expect) {
				expect(() => withTextBox(c.svg, c.element, c.box)).toThrow(c.expect.err);
				return;
			}
			const out = withTextBox(c.svg, c.element, c.box);
			expect(out).toBe(c.expect.ok.svg);
			expect(titleTexts(out).find((t) => t.id === c.element)?.box ?? null).toEqual(c.expect.ok.read);
		});
	}
});
