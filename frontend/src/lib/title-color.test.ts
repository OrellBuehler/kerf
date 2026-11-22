import { describe, expect, test } from 'bun:test';
import { alphaOf, colorText, hexAlphaByte, opacityPercent, parseColor, parseOpacity, sameColor, swatchHex, typedColor, withOpacity, withPicked } from './title-color';

describe('reading a title colour', () => {
	test('hex in either spelling, with or without an alpha byte', () => {
		expect(parseColor('#FFAA00')).toEqual({ hex: '#ffaa00', alpha: null });
		expect(parseColor('0xAABBCC')).toEqual({ hex: '#aabbcc', alpha: null });
		expect(parseColor('#ffaa00cc')).toEqual({ hex: '#ffaa00', alpha: null });
	});

	test('a name, case-insensitively, with an @alpha', () => {
		expect(parseColor('white')).toEqual({ hex: '#ffffff', alpha: null });
		expect(parseColor('Black@0.55')).toEqual({ hex: '#000000', alpha: 0.55 });
		expect(parseColor('green')?.hex).toBe('#008000');
	});

	test('nothing it cannot draw: empty, an unlisted name, a broken alpha, a short hex', () => {
		for (const t of ['', '  ', 'chartreuse', 'white@', 'white@x', '#fff', null, undefined]) expect(parseColor(t as string)).toBeNull();
	});
});

describe('the alpha byte', () => {
	test('is the last two digits of an 8-digit hex, and nothing else', () => {
		expect(hexAlphaByte('#ffaa00CC')).toBe('cc');
		expect(hexAlphaByte('0x11223344')).toBe('44');
		for (const t of ['#ffaa00', 'white', 'black@0.5', '', null, undefined]) expect(hexAlphaByte(t as string)).toBeNull();
	});
});

describe('the alpha', () => {
	test('is the number after the @, or none', () => {
		expect(alphaOf('black@0.55')).toBe(0.55);
		expect(alphaOf('black@.5')).toBe(0.5);
		expect(alphaOf('black')).toBeNull();
		expect(alphaOf('black@')).toBeNull();
	});
});

describe('writing a pick back', () => {
	test('keeps the alpha the field had', () => {
		expect(withPicked('black@0.55', '#336699')).toBe('#336699@0.55');
		expect(withPicked('#ffffff@0.5', '#00FF00')).toBe('#00ff00@0.5');
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

	test('a name it has no swatch for still lends its alpha', () => {
		expect(withPicked('chartreuse@0.4', '#112233', 0.55)).toBe('#112233@0.4');
	});

	test('a broken alpha is dropped rather than copied', () => {
		expect(withPicked('white@', '#112233', 0.55)).toBe('#112233');
	});

	test('an 8-digit hex keeps its alpha byte, in either spelling', () => {
		expect(withPicked('#ffaa00cc', '#112233')).toBe('#112233cc');
		expect(withPicked('0xAABBCC80', '#112233')).toBe('#11223380');
		expect(withPicked('#FFAA00CC', '#ABCDEF', 0.55)).toBe('#abcdefcc');
	});

	test('what it writes is what the engine accepts: #rrggbb, #rrggbbaa or #rrggbb@alpha', () => {
		for (const out of [
			withPicked('white@0.5', '#ABCDEF'),
			withPicked('', '#ABCDEF', 0.55),
			withPicked('red', '#ABCDEF'),
			withPicked('#ffaa00cc', '#ABCDEF')
		])
			expect(out).toMatch(/^#[0-9a-f]{6}([0-9a-f]{2}|@\d+(\.\d+)?)?$/);
	});
});

describe('the picker', () => {
	test('shows the field, or black for one it cannot draw', () => {
		expect(swatchHex('#336699@0.5')).toBe('#336699');
		expect(swatchHex('')).toBe('#000000');
		expect(swatchHex('chartreuse')).toBe('#000000');
	});
});

describe('what the colour field shows', () => {
	test('a hex, spelled out from a name, with the opacity left to its own field', () => {
		expect(colorText('white')).toBe('#ffffff');
		expect(colorText('black@0.55')).toBe('#000000');
		expect(colorText('0xAABBCC')).toBe('#aabbcc');
		expect(colorText('#ffaa00cc')).toBe('#ffaa00');
	});

	test('nothing for no colour, and what it cannot draw as it was typed', () => {
		expect(colorText('')).toBe('');
		expect(colorText(null)).toBe('');
		expect(colorText('  ')).toBe('');
		expect(colorText('chartreuse')).toBe('chartreuse');
		expect(colorText('chartreuse@0.4')).toBe('chartreuse@0.4');
	});
});

describe('the opacity of a colour', () => {
	test('a percentage: its @alpha, its alpha byte, or full when it has none', () => {
		expect(opacityPercent('black@0.55')).toBe(55);
		expect(opacityPercent('#ffffff@0.5')).toBe(50);
		expect(opacityPercent('#ffaa0080')).toBe(50);
		expect(opacityPercent('0xffaa00ff')).toBe(100);
		expect(opacityPercent('white')).toBe(100);
		expect(opacityPercent('chartreuse@0.4')).toBe(40);
	});

	test('none for no colour; a broken alpha is a solid colour; one out of range is held to it', () => {
		expect(opacityPercent('')).toBeNull();
		expect(opacityPercent(undefined)).toBeNull();
		expect(opacityPercent('white@')).toBe(100);
		expect(opacityPercent('white@3')).toBe(100);
		expect(opacityPercent('white@-1')).toBe(0);
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
	});

	test('the alpha byte of an 8-digit hex, in the spelling it had', () => {
		expect(withOpacity('#ffaa00cc', 50)).toBe('#ffaa0080');
		expect(withOpacity('0xFFAA00CC', 0)).toBe('0xFFAA0000');
	});

	test('full opacity takes the alpha off', () => {
		expect(withOpacity('black@0.55', 100)).toBe('black');
		expect(withOpacity('#ffaa0080', 100)).toBe('#ffaa00');
		expect(withOpacity('#ffaa00', 100)).toBe('#ffaa00');
	});

	test('held to 0..100, and an empty field has no colour to give one', () => {
		expect(withOpacity('white', 140)).toBe('white');
		expect(withOpacity('white', -5)).toBe('white@0');
		expect(withOpacity('', 50)).toBe('');
		expect(withOpacity(null, 50)).toBe('');
	});

	test('what it writes is what the engine accepts, and reads back as what was asked', () => {
		for (const current of ['white', '#336699', 'black@0.2', '#11223344', '0xAABBCCDD'])
			for (const pct of [0, 1, 7, 33, 55, 99, 100]) {
				const out = withOpacity(current, pct);
				expect(out, `${current} ${pct}`).toMatch(/^(#|0x)?[0-9a-zA-Z]+(@\d+(\.\d+)?)?$/);
				// the 8-digit spelling holds a byte, which can be a percent off
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

	test('into an empty box it starts translucent, and into a solid one it stays solid', () => {
		expect(typedColor('', '#000000', 0.55)).toBe('#000000@0.55');
		expect(typedColor('black', '#000000', 0.55)).toBe('#000000');
	});

	test('a spelling that carries its own alpha, one it cannot draw, and nothing, are taken as typed', () => {
		expect(typedColor('white', 'black@0.3')).toBe('black@0.3');
		expect(typedColor('white', '#11223344')).toBe('#11223344');
		expect(typedColor('white', 'chartreuse')).toBe('chartreuse');
		expect(typedColor('white', '  ')).toBe('');
		expect(typedColor('white', '')).toBe('');
	});
});

describe('whether a pick changes anything', () => {
	test('a name and its hex are one colour, at the same opacity', () => {
		expect(sameColor('white', '#ffffff')).toBe(true);
		expect(sameColor('Black@0.55', '#000000@0.55')).toBe(true);
		expect(sameColor('#ffaa00cc', '0xFFAA00CC')).toBe(true);
	});

	test('another colour or another opacity is a change', () => {
		expect(sameColor('white', '#fffffe')).toBe(false);
		expect(sameColor('white', 'white@0.5')).toBe(false);
		expect(sameColor('black@0.55', 'black@0.56')).toBe(false);
	});

	test('what it cannot draw is compared as text, and an empty field is not a colour', () => {
		expect(sameColor('chartreuse', ' chartreuse ')).toBe(true);
		expect(sameColor('chartreuse', 'white')).toBe(false);
		expect(sameColor('', '')).toBe(true);
		expect(sameColor('', 'white')).toBe(false);
		expect(sameColor(null, undefined)).toBe(true);
	});
});
