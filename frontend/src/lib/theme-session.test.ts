import { describe, expect, test } from 'bun:test';
import { PRESETS, type Theme } from './theme';
import {
	HISTORY,
	begin,
	canRedo,
	canUndo,
	change,
	colorChanged,
	colorDefault,
	dirty,
	endEdit,
	redo,
	resetColor,
	resetShape,
	revertAll,
	sameTheme,
	setChoice,
	setColor,
	setName,
	setScheme,
	setShape,
	shapeChanged,
	undo
} from './theme-session';

const copyOf = (id: keyof typeof PRESETS): Theme => ({ ...PRESETS[id], name: 'Mine', base: id, colors: { ...PRESETS[id].colors }, shape: { ...PRESETS[id].shape } });
const start = (id: keyof typeof PRESETS = 'kerf-dark') => begin('u-1', copyOf(id));

describe('a sitting', () => {
	test('starts clean: nothing to save, nothing to undo', () => {
		const s = start();
		expect(dirty(s)).toBe(false);
		expect(canUndo(s)).toBe(false);
		expect(canRedo(s)).toBe(false);
		expect(s.draft).toBe(s.saved);
	});

	test('a colour changes the draft, never what was saved', () => {
		const s = setColor(start(), 'kerf-500', '#ff0000');
		expect(s.draft.colors['kerf-500']).toBe('#ff0000');
		expect(s.saved.colors['kerf-500']).toBe(PRESETS['kerf-dark'].colors['kerf-500']);
		expect(dirty(s)).toBe(true);
	});

	test('what is not a colour, or is the colour already, changes nothing', () => {
		const s = start();
		for (const bad of ['red', '#fff', '', '#12345', 'rgb(1,2,3)', '#gggggg']) expect(setColor(s, 'kerf-500', bad)).toBe(s);
		expect(setColor(s, 'kerf-500', PRESETS['kerf-dark'].colors['kerf-500'].toUpperCase())).toBe(s);
		expect(setColor(s, 'kerf-500', '#E29D2E')).toBe(s);
		expect(setColor(setColor(s, 'kerf-500', '#ABCDEF'), 'kerf-500', '#abcdef').past.length).toBe(1);
	});

	test('a shape value is held to its range and its step; NaN is ignored', () => {
		const s = start();
		expect(setShape(s, 'line-width', 99).draft.shape['line-width']).toBe(3);
		expect(setShape(s, 'radius-scale', 0.9).draft.shape['radius-scale']).toBe(1);
		expect(setShape(s, 'radius-scale', 0.6).draft.shape['radius-scale']).toBe(0.5);
		expect(setShape(s, 'line-width', Number.NaN)).toBe(s);
		expect(setShape(s, 'line-width', Infinity)).toBe(s);
	});

	test('choices, the name and the scheme are edits too', () => {
		let s = start();
		s = setChoice(s, 'density', 'compact');
		s = setChoice(s, 'font-ui', 'inter');
		s = setChoice(s, 'font-mono', 'jetbrains-mono');
		s = setChoice(s, 'slider-thumb-style', 'bar');
		s = setName(s, 'Night');
		s = setScheme(s, 'light');
		expect(s.draft.shape.density).toBe('compact');
		expect(s.draft.shape['font-ui']).toBe('inter');
		expect(s.draft.shape['font-mono']).toBe('jetbrains-mono');
		expect(s.draft.shape['slider-thumb-style']).toBe('bar');
		expect(s.draft.name).toBe('Night');
		expect(s.draft.scheme).toBe('light');
		expect(s.past.length).toBe(6);
		expect(setChoice(s, 'density', 'compact')).toBe(s);
	});

	test('a choice that is not one of the known ones changes nothing', () => {
		const s = start();
		expect(setChoice(s, 'density', 'cramped' as never)).toBe(s);
		expect(setChoice(s, 'font-ui', 'comic-sans' as never)).toBe(s);
		expect(setChoice(s, 'font-ui', 'jetbrains-mono' as never)).toBe(s);
		expect(setChoice(s, 'font-mono', 'inter' as never)).toBe(s);
		expect(setChoice(s, 'slider-thumb-style', 'star' as never)).toBe(s);
		expect(setChoice(s, 'font-ui', { x: 1 } as never)).toBe(s);
	});
});

describe('undo and redo', () => {
	test('each kind of change is a step, and undo walks back through them', () => {
		let s = start();
		s = setColor(s, 'kerf-500', '#111111');
		s = setColor(s, 'waveform', '#222222');
		s = setShape(s, 'line-width', 2);
		expect(s.past.length).toBe(3);
		s = undo(s);
		expect(s.draft.shape['line-width']).toBe(1);
		s = undo(s);
		expect(s.draft.colors.waveform).toBe(PRESETS['kerf-dark'].colors.waveform);
		expect(s.draft.colors['kerf-500']).toBe('#111111');
		s = undo(s);
		expect(sameTheme(s.draft, s.saved)).toBe(true);
		expect(canUndo(s)).toBe(false);
		expect(undo(s)).toBe(s);
	});

	test('the many events of one drag are one step, until the drag ends', () => {
		let s = start();
		for (const hex of ['#100000', '#200000', '#300000', '#400000']) s = setColor(s, 'kerf-500', hex);
		expect(s.past.length).toBe(1);
		expect(undo(s).draft.colors['kerf-500']).toBe(PRESETS['kerf-dark'].colors['kerf-500']);
		s = endEdit(s);
		s = setColor(s, 'kerf-500', '#500000');
		expect(s.past.length).toBe(2);
		s = undo(s);
		expect(s.draft.colors['kerf-500']).toBe('#400000');
	});

	test('a change of another kind in between starts a new step even for the same colour', () => {
		let s = setColor(start(), 'kerf-500', '#100000');
		s = setColor(s, 'waveform', '#222222');
		s = setColor(s, 'kerf-500', '#300000');
		expect(s.past.length).toBe(3);
	});

	test('typing a name is one step', () => {
		let s = start();
		for (const n of ['M', 'My', 'My t', 'My theme']) s = setName(s, n);
		expect(s.past.length).toBe(1);
		expect(undo(s).draft.name).toBe('Mine');
	});

	test('redo replays what undo took back, and a new change forgets it', () => {
		let s = setColor(setColor(start(), 'kerf-500', '#111111'), 'waveform', '#222222');
		s = undo(s);
		expect(canRedo(s)).toBe(true);
		const again = redo(s);
		expect(again.draft.colors.waveform).toBe('#222222');
		expect(canRedo(again)).toBe(false);
		expect(redo(again)).toBe(again);
		const forked = setShape(s, 'line-width', 2);
		expect(canRedo(forked)).toBe(false);
	});

	test('a drag after an undo forgets what could be redone, too', () => {
		let s = setColor(start(), 'kerf-500', '#111111');
		s = undo(s);
		s = setColor(s, 'kerf-500', '#222222');
		expect(s.future).toEqual([]);
		s = setColor(s, 'kerf-500', '#333333');
		expect(s.future).toEqual([]);
	});

	test('the stack is bounded', () => {
		let s = start();
		for (let i = 0; i < HISTORY + 40; i++) s = setColor(endEdit(s), 'kerf-500', `#${(i + 1).toString(16).padStart(6, '0')}`);
		expect(s.past.length).toBe(HISTORY);
	});

	test('undoing everything leaves a theme that is the one saved, so Cancel and Undo agree', () => {
		let s = start();
		for (const [i, t] of (['kerf-500', 'waveform', 'surface-panel'] as const).entries()) s = setColor(s, t, `#00000${i + 1}`);
		while (canUndo(s)) s = undo(s);
		expect(dirty(s)).toBe(false);
	});
});

describe('putting back', () => {
	test('a colour goes back to the built-in the copy was made from, not to the scheme\'s', () => {
		const s = begin('u-1', copyOf('high-contrast'));
		expect(colorDefault(s.draft, 'text-muted')).toBe(PRESETS['high-contrast'].colors['text-muted']);
		const edited = setColor(s, 'text-muted', '#123456');
		expect(colorChanged(edited.draft, 'text-muted')).toBe(true);
		expect(colorChanged(edited.draft, 'text-primary')).toBe(false);
		const back = resetColor(edited, 'text-muted');
		expect(back.draft.colors['text-muted']).toBe(PRESETS['high-contrast'].colors['text-muted']);
		expect(colorChanged(back.draft, 'text-muted')).toBe(false);
		expect(resetColor(back, 'text-muted')).toBe(back);
	});

	test('a theme with no base is put back to its scheme\'s built-in', () => {
		const light: Theme = { ...copyOf('kerf-light'), base: undefined };
		expect(colorDefault(light, 'waveform')).toBe(PRESETS['kerf-light'].colors.waveform);
		expect(colorDefault({ ...light, scheme: 'dark' }, 'waveform')).toBe(PRESETS['kerf-dark'].colors.waveform);
	});

	test('a put back is a step of its own, and can be undone', () => {
		let s = setColor(start(), 'kerf-500', '#111111');
		s = resetColor(s, 'kerf-500');
		expect(s.past.length).toBe(2);
		expect(undo(s).draft.colors['kerf-500']).toBe('#111111');
	});

	test('a shape value goes back, a choice too', () => {
		let s = setChoice(setShape(start(), 'line-width', 2), 'density', 'compact');
		expect(shapeChanged(s.draft, 'line-width')).toBe(true);
		expect(shapeChanged(s.draft, 'density')).toBe(true);
		expect(shapeChanged(s.draft, 'radius-scale')).toBe(false);
		s = resetShape(resetShape(s, 'line-width'), 'density');
		expect(s.draft.shape['line-width']).toBe(1);
		expect(s.draft.shape.density).toBe('default');
		expect(resetShape(s, 'density')).toBe(s);
	});

	test('revert all goes back to the sitting\'s start as one step that can be undone', () => {
		let s = start();
		s = setColor(s, 'kerf-500', '#111111');
		s = setShape(s, 'line-width', 2);
		s = setName(s, 'Other');
		const r = revertAll(s);
		expect(sameTheme(r.draft, r.saved)).toBe(true);
		expect(dirty(r)).toBe(false);
		expect(undo(r).draft.name).toBe('Other');
		expect(revertAll(r)).toBe(r);
	});
});

describe('change', () => {
	test('a theme equal to the draft is not a change', () => {
		const s = start();
		expect(change(s, { ...s.draft })).toBe(s);
		expect(change(s, { ...s.draft, colors: { ...s.draft.colors } }, 'x')).toBe(s);
	});
});
