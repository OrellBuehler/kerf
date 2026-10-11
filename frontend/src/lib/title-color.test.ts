import { describe, expect, test } from 'bun:test';
import { alphaOf, hexAlphaByte, parseColor, swatchHex, withPicked } from './title-color';

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
