import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { FieldEdits } from './field-edit';
import {
	BOX_ALPHA,
	ENGINE_COLOR_NAMES,
	alphaOf,
	colorEdit,
	colorText,
	hexAlphaByte,
	opacityPercent,
	parseColor,
	parseOpacity,
	sameColor,
	swatchHex,
	typedColor,
	validColor,
	withOpacity,
	withPicked
} from './title-color';

// The engine's own source, read: the name table and the grammar are `engine/colors.rs`', and a copy that drifts from it
// would show a swatch for a colour the render refuses, or refuse one it draws.
const RUST = readFileSync(new URL('../../../crates/kerf-core/src/engine/colors.rs', import.meta.url), 'utf8');

/** The string literals of the array that follows `marker` in the Rust source (up to its closing bracket). */
function rustList(marker: string): string[] {
	const start = RUST.indexOf(marker);
	expect(start, marker).toBeGreaterThan(-1);
	const open = RUST.indexOf('[', start);
	const close = RUST.indexOf(']', open);
	return [...RUST.slice(open, close).matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => JSON.parse(`"${m[1]}"`) as string);
}

describe('the engine’s colours, as the Rust source has them', () => {
	test('the name table is ffmpeg’s, name for name and number for number', () => {
		const block = RUST.slice(RUST.indexOf('const NAMES'), RUST.indexOf('];', RUST.indexOf('const NAMES')));
		const rows = [...block.matchAll(/\("([a-z]+)", \[(\d+), (\d+), (\d+)\]\)/g)].map((m) => [m[1], [Number(m[2]), Number(m[3]), Number(m[4])]]);
		expect(rows.length).toBeGreaterThan(100);
		expect(Object.entries(ENGINE_COLOR_NAMES) as unknown).toEqual(rows);
	});

	test('every colour the engine’s own test accepts, this accepts, and every one it refuses', () => {
		const ok = rustList('for ok in [');
		const bad = rustList('for bad in [\n            "white,drawtext');
		expect(ok.length).toBeGreaterThan(10);
		expect(bad.length).toBeGreaterThan(40);
		for (const s of ok) expect(validColor(s), JSON.stringify(s)).toBe(true);
		for (const s of bad) expect(validColor(s), JSON.stringify(s)).toBe(false);
	});

	test('what the engine reads a colour’s rgb as, this reads as its swatch', () => {
		for (const [spec, hex] of [
			['green', '#008000'],
			['LimeGreen', '#32cd32'],
			['#FFAA00', '#ffaa00'],
			['#ffaa00cc', '#ffaa00'],
			['0x123456', '#123456'],
			['0xabcdef80', '#abcdef'],
			['white@0.5', '#ffffff'],
			['#00ff00@0x80', '#00ff00']
		])
			expect(parseColor(spec)?.hex, spec).toBe(hex);
		for (const bad of ['grey', '#fff', '0XAABBCC', 'white,black', 'white@2', '']) expect(parseColor(bad), bad).toBeNull();
	});

	test('grey is not a colour, as the engine has it; gray and lightgrey are', () => {
		expect(validColor('grey')).toBe(false);
		expect(parseColor('grey')).toBeNull();
		expect(swatchHex('grey')).toBe('#000000');
		expect(parseColor('gray')?.hex).toBe('#808080');
		expect(parseColor('lightgrey')?.hex).toBe('#d3d3d3');
		expect(validColor('darkgrey')).toBe(false);
	});
});

describe('reading a title colour', () => {
	test('hex in either spelling, with or without an alpha byte; the 0x is lower case', () => {
		expect(parseColor('#FFAA00')).toEqual({ hex: '#ffaa00', alpha: null });
		expect(parseColor('0xAABBCC')).toEqual({ hex: '#aabbcc', alpha: null });
		expect(parseColor('#ffaa00cc')).toEqual({ hex: '#ffaa00', alpha: null });
		expect(parseColor('0XAABBCC')).toBeNull();
	});

	test('a name, case-insensitively, with an @alpha', () => {
		expect(parseColor('white')).toEqual({ hex: '#ffffff', alpha: null });
		expect(parseColor('Black@0.55')?.hex).toBe('#000000');
		expect(parseColor('Black@0.55')?.alpha).toBeCloseTo(0.55, 2);
		expect(parseColor('green')?.hex).toBe('#008000');
		expect(parseColor('Crimson')?.hex).toBe('#dc143c');
		expect(parseColor('chartreuse')?.hex).toBe('#7fff00');
	});

	test('nothing the engine refuses: empty, a name it does not have, a broken alpha, a short hex', () => {
		for (const t of ['', '  ', 'grey', 'rebeccapurple', 'white@', 'white@x', 'white@2', 'white@1e-1', '#fff', null, undefined])
			expect(parseColor(t as string), String(t)).toBeNull();
	});
});

describe('the alpha byte', () => {
	test('is the last two digits of an 8-digit hex, and nothing else', () => {
		expect(hexAlphaByte('#ffaa00CC')).toBe('cc');
		expect(hexAlphaByte('0x11223344')).toBe('44');
		for (const t of ['#ffaa00', 'white', 'black@0.5', '', null, undefined, '0X11223344']) expect(hexAlphaByte(t as string)).toBeNull();
	});
});

describe('the alpha', () => {
	test('is read as the engine reads it: a decimal cut to a byte, or a hex byte', () => {
		expect(alphaOf('black@0.55')).toBeCloseTo(140 / 255, 6);
		expect(alphaOf('black@.5')).toBeCloseTo(127 / 255, 6);
		expect(alphaOf('black@1')).toBe(1);
		expect(alphaOf('black@0')).toBe(0);
		expect(alphaOf('black@0x80')).toBeCloseTo(128 / 255, 6);
		expect(alphaOf('black@0xFF')).toBe(1);
		expect(alphaOf('black@0x8')).toBeCloseTo(8 / 255, 6);
	});

	test('none for none, and for what the engine refuses', () => {
		for (const t of ['black', 'black@', 'black@2', 'black@-1', 'black@+0.5', 'black@1e-1', 'black@0X80', 'black@0x100', 'black@0x', 'black@.'])
			expect(alphaOf(t), t).toBeNull();
	});
});

describe('writing a pick back', () => {
	test('keeps the alpha the field had, in the spelling it had', () => {
		expect(withPicked('black@0.55', '#336699')).toBe('#336699@0.55');
		expect(withPicked('#ffffff@0.5', '#00FF00')).toBe('#00ff00@0.5');
		expect(withPicked('black@0x80', '#336699')).toBe('#336699@0x80');
	});

	test('a field with no alpha stays solid, and a box with none starts at its fallback', () => {
		expect(withPicked('white', '#112233')).toBe('#112233');
		expect(withPicked('', '#112233', 0.55)).toBe('#112233@0.55');
		expect(withPicked(null, '#112233', 0.55)).toBe('#112233@0.55');
	});

	test('a colour the field already holds stays as solid as it was: only an empty one starts translucent', () => {
		expect(withPicked('black', '#112233', 0.55)).toBe('#112233');
		expect(withPicked('#000000', '#112233', 0.55)).toBe('#112233');
		expect(withPicked('  ', '#112233', 0.55)).toBe('#112233@0.55');
	});

	test('a broken alpha is dropped rather than copied', () => {
		expect(withPicked('white@', '#112233', 0.55)).toBe('#112233');
		expect(withPicked('white@2', '#112233', 0.55)).toBe('#112233');
	});

	test('an 8-digit hex keeps its alpha byte, in either prefix; an @ beside it wins, as it does in the engine', () => {
		expect(withPicked('#ffaa00cc', '#112233')).toBe('#112233cc');
		expect(withPicked('0xAABBCC80', '#112233')).toBe('#11223380');
		expect(withPicked('#FFAA00CC', '#ABCDEF', 0.55)).toBe('#abcdefcc');
		expect(withPicked('#ffaa00cc@0.25', '#112233')).toBe('#112233@0.25');
	});

	test('what it writes is what the engine accepts', () => {
		for (const current of ['white@0.5', '', 'red', '#ffaa00cc', 'black@0x80', 'white@', 'grey@0.5', null])
			expect(validColor(withPicked(current, '#ABCDEF', 0.55)), String(current)).toBe(true);
	});
});

describe('the picker', () => {
	test('shows the field, or black for one it cannot draw', () => {
		expect(swatchHex('#336699@0.5')).toBe('#336699');
		expect(swatchHex('')).toBe('#000000');
		expect(swatchHex('rebeccapurple')).toBe('#000000');
	});
});

describe('what the colour field shows', () => {
	test('a hex, spelled out from a name, with the opacity left to its own field', () => {
		expect(colorText('white')).toBe('#ffffff');
		expect(colorText('black@0.55')).toBe('#000000');
		expect(colorText('0xAABBCC')).toBe('#aabbcc');
		expect(colorText('#ffaa00cc')).toBe('#ffaa00');
		expect(colorText('chartreuse')).toBe('#7fff00');
	});

	test('nothing for no colour, and what the engine refuses as it was typed', () => {
		expect(colorText('')).toBe('');
		expect(colorText(null)).toBe('');
		expect(colorText('  ')).toBe('');
		expect(colorText('grey')).toBe('grey');
		expect(colorText('grey@0.4')).toBe('grey@0.4');
	});
});

describe('the opacity of a colour', () => {
	test('a percentage, as the engine draws it: its @alpha, its alpha byte, or full when it has none', () => {
		expect(opacityPercent('black@0.55')).toBe(55);
		expect(opacityPercent('#ffffff@0.5')).toBe(50);
		expect(opacityPercent('#ffaa0080')).toBe(50);
		expect(opacityPercent('0xffaa00ff')).toBe(100);
		expect(opacityPercent('white')).toBe(100);
		expect(opacityPercent('chartreuse@0.4')).toBe(40);
		expect(opacityPercent('yellow@0x80')).toBe(50);
		expect(opacityPercent('yellow@0xFF')).toBe(100);
		expect(opacityPercent('yellow@0x00')).toBe(0);
		expect(opacityPercent('#ffaa00cc@0.25')).toBe(25);
	});

	test('none for no colour and for one the engine refuses; there is nothing to give an opacity to', () => {
		expect(opacityPercent('')).toBeNull();
		expect(opacityPercent(undefined)).toBeNull();
		for (const t of ['white@', 'white@3', 'white@-1', 'grey', 'grey@0.5', '#fff']) expect(opacityPercent(t), t).toBeNull();
	});

	test('what is typed: 55, 55 % or 55%, a whole percent, held to 0..100', () => {
		expect(parseOpacity('55')).toBe(55);
		expect(parseOpacity('55%')).toBe(55);
		expect(parseOpacity(' 55 % ')).toBe(55);
		expect(parseOpacity('54.6')).toBe(55);
		expect(parseOpacity('250')).toBe(100);
		expect(parseOpacity('0')).toBe(0);
		for (const t of ['', '%', 'abc', '-5', '5 5', '0.5x']) expect(parseOpacity(t), t).toBeNull();
	});
});

describe('setting the opacity', () => {
	test('an @alpha for a hex or a name, which stays what it was', () => {
		expect(withOpacity('#336699', 55)).toBe('#336699@0.55');
		expect(withOpacity('black@0.55', 40)).toBe('black@0.4');
		expect(withOpacity('white', 7)).toBe('white@0.07');
		expect(withOpacity('chartreuse', 20)).toBe('chartreuse@0.2');
		expect(withOpacity('yellow@0x80', 25)).toBe('yellow@0.25');
	});

	test('the alpha byte of an 8-digit hex, in the spelling it had', () => {
		expect(withOpacity('#ffaa00cc', 50)).toBe('#ffaa0080');
		expect(withOpacity('0xFFAA00CC', 0)).toBe('0xFFAA0000');
	});

	test('one alpha only: an 8-digit hex with an @ beside it keeps the @ and loses the byte', () => {
		expect(withOpacity('#ffaa00cc@0.25', 60)).toBe('#ffaa00@0.6');
		expect(withOpacity('#ffaa00cc@0.25', 100)).toBe('#ffaa00');
	});

	test('full opacity takes the alpha off', () => {
		expect(withOpacity('black@0.55', 100)).toBe('black');
		expect(withOpacity('#ffaa0080', 100)).toBe('#ffaa00');
		expect(withOpacity('#ffaa00', 100)).toBe('#ffaa00');
	});

	test('held to 0..100, and an empty field or one the engine refuses comes back as it was', () => {
		expect(withOpacity('white', 140)).toBe('white');
		expect(withOpacity('white', -5)).toBe('white@0');
		expect(withOpacity('', 50)).toBe('');
		expect(withOpacity(null, 50)).toBe('');
		expect(withOpacity('grey', 50)).toBe('grey');
		expect(withOpacity('white@2', 50)).toBe('white@2');
	});

	test('what it writes is what the engine accepts, and reads back as what was asked', () => {
		for (const current of ['white', '#336699', 'black@0.2', '#11223344', '0xAABBCCDD', 'yellow@0x80', 'LimeGreen', '#11223344@0.5'])
			for (const pct of [0, 1, 7, 33, 55, 99, 100]) {
				const out = withOpacity(current, pct);
				expect(validColor(out), `${current} ${pct} -> ${out}`).toBe(true);
				const back = opacityPercent(out);
				expect(Math.abs((back ?? -100) - pct), `${current} ${pct} -> ${out}`).toBeLessThanOrEqual(1);
			}
	});
});

describe('typing into the colour field', () => {
	test('a hex or a name is a pick: it keeps the opacity the field had', () => {
		expect(typedColor('black@0.55', '#336699')).toBe('#336699@0.55');
		expect(typedColor('black@0.55', 'red')).toBe('#ff0000@0.55');
		expect(typedColor('#ffaa00cc', '#336699')).toBe('#336699cc');
		expect(typedColor('white', '#336699')).toBe('#336699');
	});

	test('crimson over black@0.5 stays half transparent, which a short table could not tell', () => {
		expect(typedColor('black@0.5', 'crimson')).toBe('#dc143c@0.5');
		expect(typedColor('black@0.5', 'Crimson')).toBe('#dc143c@0.5');
	});

	test('into an empty box it starts translucent, and into a solid one it stays solid', () => {
		expect(typedColor('', '#000000', 0.55)).toBe('#000000@0.55');
		expect(typedColor('black', '#000000', 0.55)).toBe('#000000');
	});

	test('a spelling that carries its own alpha, one the engine refuses, and nothing, are taken as typed', () => {
		expect(typedColor('white', 'black@0.3')).toBe('black@0.3');
		expect(typedColor('white', '#11223344')).toBe('#11223344');
		expect(typedColor('white', 'grey')).toBe('grey');
		expect(typedColor('white', '  ')).toBe('');
		expect(typedColor('white', '')).toBe('');
	});
});

describe('whether a pick changes anything', () => {
	test('a name and its hex are one colour, at the same opacity', () => {
		expect(sameColor('white', '#ffffff')).toBe(true);
		expect(sameColor('Black@0.55', '#000000@0.55')).toBe(true);
		expect(sameColor('#ffaa00cc', '0xFFAA00CC')).toBe(true);
		expect(sameColor('white', 'white@1')).toBe(true);
		expect(sameColor('white@0x80', 'white@0.5')).toBe(false);
	});

	test('another colour or another opacity is a change', () => {
		expect(sameColor('white', '#fffffe')).toBe(false);
		expect(sameColor('white', 'white@0.5')).toBe(false);
		expect(sameColor('black@0.55', 'black@0.56')).toBe(false);
	});

	test('what the engine refuses is compared as text, and an empty field is not a colour', () => {
		expect(sameColor('grey', ' grey ')).toBe(true);
		expect(sameColor('grey', 'white')).toBe(false);
		expect(sameColor('', '')).toBe(true);
		expect(sameColor('', 'white')).toBe(false);
		expect(sameColor(null, undefined)).toBe(true);
	});
});

describe('never a colour the engine would refuse, out of one it was given', () => {
	test('every change to a field, on a spread of what a title may hold, is a colour or no write', () => {
		const held = ['white', 'Black@0.55', '#336699', '#ffaa00cc', '0x112233', 'yellow@0x80', 'LimeGreen@1', '#abcdef@0.5', ''];
		const typed = ['red', '#00ff00', 'crimson', '70', '55%', '', 'grey', 'black@0.3', '#11223344'];
		for (const color of held)
			for (const t of typed)
				for (const kind of ['text', 'opacity', 'pick'] as const) {
					const patch = colorEdit({ color: 'white', bg: color }, 'bg', { kind, text: kind === 'pick' ? '#abcdef' : t });
					if (patch === null) continue;
					const written = (patch as { bg: string }).bg;
					// what is typed as a bad spelling is the user's to fix, and the engine says so; nothing here makes one
					if (kind === 'text' && !validColor(t.trim()) && t.trim() !== '' && !t.includes('@')) continue;
					if (written === '') continue;
					expect(validColor(written), `${kind} ${JSON.stringify(t)} over ${JSON.stringify(color)} -> ${written}`).toBe(true);
				}
	});
});

describe('one change to a title’s colour', () => {
	const a = { color: '#aa0000', bg: '#112233@0.9' };

	test('a typed hex, opacity, pick or clear is a patch of the field it is for', () => {
		expect(colorEdit(a, 'bg', { kind: 'opacity', text: '70' })).toEqual({ bg: '#112233@0.7' });
		expect(colorEdit(a, 'bg', { kind: 'text', text: '#445566' })).toEqual({ bg: '#445566@0.9' });
		expect(colorEdit(a, 'bg', { kind: 'pick', text: '#445566' })).toEqual({ bg: '#445566@0.9' });
		expect(colorEdit(a, 'bg', { kind: 'clear' })).toEqual({ bg: '' });
		expect(colorEdit(a, 'color', { kind: 'text', text: 'white' })).toEqual({ color: '#ffffff' });
	});

	test('nothing for what changes nothing: the same colour, an opacity that is not a number, a box that is already none', () => {
		expect(colorEdit(a, 'color', { kind: 'text', text: '#aa0000' })).toBeNull();
		expect(colorEdit(a, 'bg', { kind: 'opacity', text: 'abc' })).toBeNull();
		expect(colorEdit(a, 'bg', { kind: 'opacity', text: '90' })).toBeNull();
		expect(colorEdit({ color: 'white' }, 'bg', { kind: 'clear' })).toBeNull();
		expect(colorEdit({ color: 'white', bg: null }, 'bg', { kind: 'opacity', text: '50' })).toBeNull();
	});

	test('a colour typed into an empty box starts at the box’s own opacity', () => {
		expect(colorEdit({ color: 'white' }, 'bg', { kind: 'text', text: '#000000' })).toEqual({ bg: `#000000@${BOX_ALPHA}` });
		expect(colorEdit({ color: 'white' }, 'color', { kind: 'text', text: '#000000' })).toEqual({ color: '#000000' });
	});

	test('the title the edit began on is the one that is written, whichever is selected when the field commits', () => {
		type Scope = { overlayId: string };
		const titles = new Map([
			['a', { color: '#aa0000', bg: '#112233@0.9' }],
			['b', { color: '#00aa00', bg: '#00ff00' }]
		]);
		const edits = new FieldEdits<Scope, object>();
		const box = {};
		// Title A is selected: the box's opacity field takes focus, and 70 is typed into it…
		edits.focus(box, { overlayId: 'a' });
		edits.input(box, '70');
		// …then the click on title B selects it and re-renders the field with B's value; the blur commits.
		const committed = edits.end(box, { scope: { overlayId: 'b' }, text: '100%' });
		expect(committed).toEqual({ scope: { overlayId: 'a' }, text: '70' });
		const began = titles.get(committed.scope.overlayId)!;
		expect(colorEdit(began, 'bg', { kind: 'opacity', text: committed.text })).toEqual({ bg: '#112233@0.7' });
		// the same edit built from what is selected now is the bug: B's colour at A's 70 %
		expect(colorEdit(titles.get('b')!, 'bg', { kind: 'opacity', text: committed.text })).toEqual({ bg: '#00ff00@0.7' });
	});
});
