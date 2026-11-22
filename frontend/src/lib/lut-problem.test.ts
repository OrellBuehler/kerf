import { describe, expect, test } from 'bun:test';
import { lutFacts, lutProblem } from './lut-problem';

describe('what a LUT that cannot be used says', () => {
	test('a file that is gone is missing, on either platform, and says what it costs', () => {
		const unix = lutProblem('invalid argument: cannot open LUT /luts/teal.cube: No such file or directory (os error 2)');
		expect(unix.kind).toBe('missing');
		expect(unix.short).toBe('File missing');
		expect(unix.line).toBe('File not found — export will not start.');
		expect(unix.text).toContain("Kerf can't find this file.");
		expect(unix.text).toContain('an export will not start');
		expect(unix.text).toContain('preview leaves the look out');
		const win = lutProblem('invalid argument: cannot open LUT C:\\luts\\teal.cube: The system cannot find the file specified. (os error 2)');
		expect(win.kind).toBe('missing');
		const dir = lutProblem('invalid argument: cannot open LUT D:\\gone\\a.cube: The system cannot find the path specified. (os error 3)');
		expect(dir.kind).toBe('missing');
	});

	test('the file’s name is never in the words: the row above shows it, and a name with no break in it clipped the warning', () => {
		const messages = [
			'invalid argument: cannot open LUT /luts/kodak_2383_d65_cinematic_film_emulation_final_v12.cube: No such file or directory (os error 2)',
			'invalid argument: cannot open LUT /luts/kodak_2383_d65_cinematic_film_emulation_final_v12.cube: Permission denied (os error 13)',
			'invalid argument: not a usable .cube LUT (line 12): a 1D table or shaper is not supported, only a 3D LUT',
			'invalid argument: LUT /luts/kodak_2383_d65_cinematic_film_emulation_final_v12.cube is larger than 32 MB',
			'invalid argument: the LUT path must be absolute: kodak_2383_d65.cube'
		];
		for (const m of messages) {
			const p = lutProblem(m);
			for (const said of [p.short, p.line, p.text]) {
				expect(said.includes('kodak'), said).toBe(false);
				expect(said.includes('.cube LUT /'), said).toBe(false);
			}
		}
	});

	test('the line is one short sentence that says the cost that matters', () => {
		for (const m of [
			'invalid argument: cannot open LUT /a.cube: No such file or directory (os error 2)',
			'invalid argument: cannot open LUT /a.cube: Permission denied (os error 13)',
			'invalid argument: not a usable .cube LUT: no LUT_3D_SIZE',
			'invalid argument: LUT /a.cube is larger than 32 MB',
			'something odd'
		]) {
			const p = lutProblem(m);
			expect(p.line.length, p.line).toBeLessThanOrEqual(44);
			expect(p.line).toContain('export will not start');
			expect(p.line.includes('\n')).toBe(false);
		}
	});

	test('a file that is there but cannot be opened is not called missing', () => {
		const p = lutProblem('invalid argument: cannot open LUT /luts/teal.cube: Permission denied (os error 13)');
		expect(p.kind).toBe('unreadable');
		expect(p.text).toContain("can't open this file");
		expect(lutProblem('invalid argument: cannot read LUT /luts/a.cube: Is a directory (os error 21)').kind).toBe('unreadable');
	});

	test('a file that is not a LUT says where it went wrong, and the engine’s own words are kept for the tooltip', () => {
		const p = lutProblem('invalid argument: not a usable .cube LUT (line 12): a 1D table or shaper is not supported, only a 3D LUT');
		expect(p.kind).toBe('unusable');
		expect(p.text).toContain('This is not a 3D .cube LUT Kerf can read (line 12: a 1D table or shaper is not supported, only a 3D LUT).');
		expect(p.detail.startsWith('not a usable .cube LUT')).toBe(true);
		const none = lutProblem('invalid argument: not a usable .cube LUT: no LUT_3D_SIZE');
		expect(none.text).toContain('This is not a 3D .cube LUT Kerf can read.');
	});

	test('too large, a relative path and anything else still say what happens to the picture', () => {
		expect(lutProblem('invalid argument: LUT /a.cube is larger than 32 MB').short).toBe('Too large');
		expect(lutProblem('invalid argument: the LUT path must be absolute: looks/a.cube').kind).toBe('missing');
		const other = lutProblem('something odd');
		expect(other.kind).toBe('unusable');
		expect(other.text).toContain('This file cannot be used: something odd.');
		expect(other.text).toContain('an export will not start');
	});

	test('a prefix said twice is taken off once', () => {
		expect(lutProblem('invalid argument: invalid argument: cannot open LUT /a: No such file or directory (os error 2)').detail).toBe(
			'cannot open LUT /a: No such file or directory (os error 2)'
		);
	});

	test('a LUT that reads is its size and its title', () => {
		expect(lutFacts({ size: 33, title: 'Teal and orange' })).toBe('33³ · Teal and orange');
		expect(lutFacts({ size: 17 })).toBe('17³');
		expect(lutFacts({ size: 17, title: null })).toBe('17³');
	});
});
