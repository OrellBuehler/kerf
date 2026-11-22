import { describe, expect, test } from 'bun:test';
import { findHistory, type Searchable } from './history-ui';

const el = (text: string): Element => ({ textContent: text }) as unknown as Element;

/** A document with these headings, and optionally an element that marks the section. */
function doc(headings: string[], marked?: Element): Searchable {
	return {
		querySelector: (sel) => (sel === '[data-history]' ? (marked ?? null) : null),
		querySelectorAll: (sel) => (sel === '.k-overline' ? headings.map(el) : [])
	};
}

describe('finding the history section', () => {
	test('the element that marks it wins', () => {
		const marked = el('whatever it holds');
		expect(findHistory(doc(['Queue', 'History'], marked))).toBe(marked);
	});

	test('failing that, the heading that says History, however it is spelled', () => {
		const found = findHistory(doc(['Queue', ' HISTORY ']));
		expect(found?.textContent).toBe(' HISTORY ');
	});

	test('a panel with no history says nothing is there', () => {
		expect(findHistory(doc(['Queue', 'Quick edits']))).toBeNull();
		expect(findHistory(doc([]))).toBeNull();
	});
});
