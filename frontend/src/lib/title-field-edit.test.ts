import { describe, expect, test } from 'bun:test';
import { FieldEdits } from './field-edit';
import { colorHandlers } from './title-color';
import { commitTitleField, type TitleEditor } from './title-field-edit';

// The end of an edit to a title's field, as the page does it: the field takes focus (the scope is noted), is typed into
// (`input`, the text is noted), the click that blurs it selects another title (the selection moves and the field is drawn
// again with that title's value), and only then does `change` arrive. A small model of the page, and the very handlers its
// colour controls hand to `commitTitleField`.

interface Title {
	id: string;
	color: string;
	bg: string | null;
}
type Scope = { overlayId: string };
type Handler = { patch: (typed: string, title: Title) => unknown; shown: (title: Title) => string };

function page(titles: Title[], selected: string) {
	const byId = new Map(titles.map((t) => [t.id, t]));
	const edits = new FieldEdits<Scope, object>();
	const state = { selected, locked: false };
	const fields: { el: { value: string; isConnected: boolean }; draw: (t: Title) => string }[] = [];
	const scope = (): Scope => ({ overlayId: state.selected });
	const editor: TitleEditor<Scope, Title> = {
		edits,
		titleOf: (s) => byId.get(s.overlayId),
		shownTitle: () => byId.get(state.selected),
		locked: () => state.locked
	};
	return {
		state,
		byId,
		/** A field drawn from the selected title, by `draw` (what its control shows of a title). */
		field(draw: (t: Title) => string) {
			const el = { value: draw(byId.get(state.selected)!), isConnected: true };
			fields.push({ el, draw });
			return el;
		},
		/** The field took focus (`onfocus={noteFocus}`). */
		focus: (el: object) => edits.focus(el, scope()),
		/** Something was typed or picked (`oninput={noteTyped}`): the text is noted, and the field holds it. */
		input(el: { value: string }, text: string) {
			el.value = text;
			edits.input(el, text);
		},
		/** Something was typed or picked in a field that does not note it (the bug: no `oninput={noteTyped}`). */
		unnoted(el: { value: string }, text: string) {
			el.value = text;
		},
		/** The click selects another title: every field is drawn again from it. */
		select(id: string) {
			state.selected = id;
			for (const f of fields) f.el.value = f.draw(byId.get(id)!);
		},
		/** The field commits (`onchange`): what `patchTitle` does. */
		change(el: { value: string; isConnected: boolean }, handler: Handler) {
			return commitTitleField(editor, el, { scope: scope(), text: el.value }, handler.patch as never, handler.shown);
		}
	};
}

const A: Title = { id: 'a', color: '#aa0000', bg: '#112233@0.9' };
const B: Title = { id: 'b', color: '#00aa00', bg: '#00ff00@0.2' };
const box = colorHandlers('bg');
const text = colorHandlers('color');

describe('a colour picked in the swatch lands on the title the pick began on', () => {
	test('input, then the selection moves, then change: A gets the colour that was picked, not the one B shows', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const swatch = p.field((t) => box.pick.shown(t));
		p.focus(swatch);
		p.input(swatch, '#336699');
		p.select('b');
		expect(swatch.value).toBe('#00ff00');
		expect(p.change(swatch, box.pick)).toEqual({ id: 'a', patch: { bg: '#336699@0.9' } });
	});

	test('a swatch that does not note its input reads whatever it shows by then, which is the other title’s colour', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const swatch = p.field((t) => box.pick.shown(t));
		p.focus(swatch);
		p.unnoted(swatch, '#336699');
		p.select('b');
		// what this page's swatches had before `oninput={noteTyped}`: the pick is lost and B's colour goes onto A
		expect(p.change(swatch, box.pick)).toEqual({ id: 'a', patch: { bg: '#00ff00@0.9' } });
	});

	test('the text colour’s swatch the same', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const swatch = p.field((t) => text.pick.shown(t));
		p.focus(swatch);
		p.input(swatch, '#ffffff');
		p.select('b');
		expect(p.change(swatch, text.pick)).toEqual({ id: 'a', patch: { color: '#ffffff' } });
	});
});

describe('a typed hex and a typed opacity land on the title they were typed for', () => {
	test('hex', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const hex = p.field((t) => box.text.shown(t));
		p.focus(hex);
		p.input(hex, '#445566');
		p.select('b');
		expect(p.change(hex, box.text)).toEqual({ id: 'a', patch: { bg: '#445566@0.9' } });
	});

	test('opacity', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const pct = p.field((t) => box.opacity.shown(t));
		p.focus(pct);
		p.input(pct, '70');
		p.select('b');
		expect(pct.value).toBe('20%');
		expect(p.change(pct, box.opacity)).toEqual({ id: 'a', patch: { bg: '#112233@0.7' } });
	});

	test('with no move at all it is the plain edit', () => {
		const p = page([{ ...A }, { ...B }], 'b');
		const pct = p.field((t) => box.opacity.shown(t));
		p.focus(pct);
		p.input(pct, '50');
		expect(p.change(pct, box.opacity)).toEqual({ id: 'b', patch: { bg: '#00ff00@0.5' } });
	});
});

describe('an edit that writes nothing puts the field back to the row it is drawn from now', () => {
	test('not a number typed into A’s opacity, then B selected: the field shows B’s 20%, not A’s 90%', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const pct = p.field((t) => box.opacity.shown(t));
		expect(pct.value).toBe('90%');
		p.focus(pct);
		p.input(pct, 'abc');
		p.select('b');
		expect(p.change(pct, box.opacity)).toBeNull();
		expect(pct.value).toBe('20%');
	});

	test('the same for the hex field, the swatch and the text colour', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const hex = p.field((t) => box.text.shown(t));
		const swatch = p.field((t) => box.pick.shown(t));
		const color = p.field((t) => text.text.shown(t));
		// A's own colours typed again: nothing to write
		p.focus(hex);
		p.input(hex, '#112233');
		p.focus(swatch);
		p.input(swatch, '#112233');
		p.focus(color);
		p.input(color, '#aa0000');
		p.select('b');
		expect(p.change(hex, box.text)).toBeNull();
		expect(p.change(swatch, box.pick)).toBeNull();
		expect(p.change(color, text.text)).toBeNull();
		expect([hex.value, swatch.value, color.value]).toEqual(['#00ff00', '#00ff00', '#00aa00']);
	});

	test('when the selection did not move it goes back to the title it began on', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const pct = p.field((t) => box.opacity.shown(t));
		p.focus(pct);
		p.input(pct, 'abc');
		expect(p.change(pct, box.opacity)).toBeNull();
		expect(pct.value).toBe('90%');
	});

	test('a cut that is locked writes nothing, and the field goes back to what is on screen', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const pct = p.field((t) => box.opacity.shown(t));
		p.focus(pct);
		p.input(pct, '10');
		p.select('b');
		p.state.locked = true;
		expect(p.change(pct, box.opacity)).toBeNull();
		expect(pct.value).toBe('20%');
	});

	test('the title the edit began on is gone: nothing is written and the field is left as it is', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const pct = p.field((t) => box.opacity.shown(t));
		p.focus(pct);
		p.input(pct, '10');
		p.select('b');
		p.byId.delete('a');
		expect(p.change(pct, box.opacity)).toBeNull();
		expect(pct.value).toBe('20%');
	});

	test('a field that is no longer on the page is not touched', () => {
		const p = page([{ ...A }, { ...B }], 'a');
		const pct = p.field((t) => box.opacity.shown(t));
		p.focus(pct);
		p.input(pct, 'abc');
		p.select('b');
		pct.value = 'detached';
		pct.isConnected = false;
		expect(p.change(pct, box.opacity)).toBeNull();
		expect(pct.value).toBe('detached');
	});
});
