import { describe, expect, test } from 'bun:test';
import { FieldEdits } from './field-edit';

const field = () => ({});

describe('FieldEdits', () => {
	test('a commit is the scope the field took focus on and the text typed into it', () => {
		const edits = new FieldEdits<string, object>();
		const f = field();
		edits.focus(f, 'clip-a');
		edits.input(f, '1');
		edits.input(f, '15');
		// The click on clip B has moved the selection and re-rendered the field with B's value.
		expect(edits.end(f, { scope: 'clip-b', text: '0.5' })).toEqual({ scope: 'clip-a', text: '15' });
	});

	test('without typing, the field is read as it is: a select or a checkbox changes at once', () => {
		const edits = new FieldEdits<string, object>();
		const f = field();
		edits.focus(f, 'clip-a');
		expect(edits.end(f, { scope: 'clip-a', text: 'Arial' })).toEqual({ scope: 'clip-a', text: 'Arial' });
	});

	test('a field that was never focused is read as it is now', () => {
		const edits = new FieldEdits<string, object>();
		const f = field();
		edits.input(f, 'ignored');
		expect(edits.end(f, { scope: 'clip-b', text: '2' })).toEqual({ scope: 'clip-b', text: '2' });
	});

	test('the typed text is spent by the commit; the scope holds while the field keeps focus', () => {
		const edits = new FieldEdits<string, object>();
		const f = field();
		edits.focus(f, 'clip-a');
		edits.input(f, '3');
		edits.end(f, { scope: 'clip-a', text: '3' });
		// Enter commits and the field stays focused: a second commit with nothing typed reads the field.
		expect(edits.end(f, { scope: 'clip-b', text: '0.5' })).toEqual({ scope: 'clip-a', text: '0.5' });
		edits.input(f, '4');
		expect(edits.end(f, { scope: 'clip-b', text: '0.5' })).toEqual({ scope: 'clip-a', text: '4' });
	});

	test('taking focus again begins a new edit', () => {
		const edits = new FieldEdits<string, object>();
		const f = field();
		edits.focus(f, 'clip-a');
		edits.input(f, 'abandoned');
		edits.focus(f, 'clip-b');
		expect(edits.end(f, { scope: 'clip-b', text: 'shown' })).toEqual({ scope: 'clip-b', text: 'shown' });
	});

	test('fields are independent', () => {
		const edits = new FieldEdits<string, object>();
		const [x, y] = [field(), field()];
		edits.focus(x, 'clip-a');
		edits.focus(y, 'clip-b');
		edits.input(x, 'x');
		edits.input(y, 'y');
		expect(edits.end(x, { scope: 'now', text: '' })).toEqual({ scope: 'clip-a', text: 'x' });
		expect(edits.end(y, { scope: 'now', text: '' })).toEqual({ scope: 'clip-b', text: 'y' });
	});
});
