import { describe, expect, test } from 'bun:test';
import { lutFacts, lutProblem } from './lut-problem';

describe('what a LUT that cannot be used says', () => {
	test('a file that is gone is missing, on either platform, and says what it costs', () => {
		const unix = lutProblem('invalid argument: cannot open LUT /luts/teal.cube: No such file or directory (os error 2)', 'teal.cube');
		expect(unix.kind).toBe('missing');
		expect(unix.short).toBe('File missing');
		expect(unix.text).toContain("Kerf can't find teal.cube.");
		expect(unix.text).toContain('an export will not start');
		expect(unix.text).toContain('preview leaves the look out');
		const win = lutProblem('invalid argument: cannot open LUT C:\\luts\\teal.cube: The system cannot find the file specified. (os error 2)', 'teal.cube');
		expect(win.kind).toBe('missing');
		const dir = lutProblem('invalid argument: cannot open LUT D:\\gone\\a.cube: The system cannot find the path specified. (os error 3)', 'a.cube');
		expect(dir.kind).toBe('missing');
	});

	test('a file that is there but cannot be opened is not called missing', () => {
		const p = lutProblem('invalid argument: cannot open LUT /luts/teal.cube: Permission denied (os error 13)', 'teal.cube');
		expect(p.kind).toBe('unreadable');
		expect(p.text).toContain("can't open teal.cube");
		expect(lutProblem('invalid argument: cannot read LUT /luts/a.cube: Is a directory (os error 21)', 'a.cube').kind).toBe('unreadable');
	});

	test('a file that is not a LUT says where it went wrong, and the engine’s own words are kept for the tooltip', () => {
		const p = lutProblem('invalid argument: not a usable .cube LUT (line 12): a 1D table or shaper is not supported, only a 3D LUT', 'film.cube');
		expect(p.kind).toBe('unusable');
		expect(p.text).toContain('film.cube is not a 3D .cube LUT Kerf can read (line 12: a 1D table or shaper is not supported, only a 3D LUT).');
		expect(p.detail.startsWith('not a usable .cube LUT')).toBe(true);
		const none = lutProblem('invalid argument: not a usable .cube LUT: no LUT_3D_SIZE', 'x.cube');
		expect(none.text).toContain('x.cube is not a 3D .cube LUT Kerf can read.');
	});

	test('too large, a relative path and anything else still say what happens to the picture', () => {
		expect(lutProblem('invalid argument: LUT /a.cube is larger than 32 MB', 'a.cube').short).toBe('Too large');
		const rel = lutProblem('invalid argument: the LUT path must be absolute: looks/a.cube', 'a.cube');
		expect(rel.kind).toBe('missing');
		const other = lutProblem('something odd', 'a.cube');
		expect(other.kind).toBe('unusable');
		expect(other.text).toContain('a.cube cannot be used: something odd.');
		expect(other.text).toContain('an export will not start');
	});

	test('a prefix said twice is taken off once', () => {
		expect(lutProblem('invalid argument: invalid argument: cannot open LUT /a: No such file or directory (os error 2)', 'a.cube').detail).toBe(
			'cannot open LUT /a: No such file or directory (os error 2)'
		);
	});

	test('a LUT that reads is its size and its title', () => {
		expect(lutFacts({ size: 33, title: 'Teal and orange' })).toBe('33³ · Teal and orange');
		expect(lutFacts({ size: 17 })).toBe('17³');
		expect(lutFacts({ size: 17, title: null })).toBe('17³');
	});
});
