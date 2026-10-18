import { describe, expect, test } from 'bun:test';
import { exportTargets, filesToReplace } from './export-overwrite';

describe('what an export writes', () => {
	test('the chosen file, or one sibling per delivery shape', () => {
		expect(exportTargets('/out/cut.mp4', [])).toEqual(['/out/cut.mp4']);
		expect(
			exportTargets('/out/cut.mp4', [
				{ width: 1080, height: 1920, fit: 'cover' },
				{ width: 1080, height: 1080, fit: 'contain' }
			])
		).toEqual(['/out/cut-9x16.mp4', '/out/cut-1x1.mp4']);
	});
});

describe('what would be replaced', () => {
	test('files that are there and that the save dialog did not already ask about', () => {
		expect(filesToReplace(['/o/a.mp4'], [false], null)).toEqual([]);
		expect(filesToReplace(['/o/a.mp4'], [true], null)).toEqual(['/o/a.mp4']);
		// the dialog asked about the file it picked
		expect(filesToReplace(['/o/a.mp4'], [true], '/o/a.mp4')).toEqual([]);
		// …but not about the same name with another container
		expect(filesToReplace(['/o/a.mov'], [true], '/o/a.mp4')).toEqual(['/o/a.mov']);
	});

	test('a multi-format export asks about each sibling that exists', () => {
		expect(filesToReplace(['/o/a-9x16.mp4', '/o/a-1x1.mp4', '/o/a-16x9.mp4'], [true, false, true], '/o/a.mp4')).toEqual([
			'/o/a-9x16.mp4',
			'/o/a-16x9.mp4'
		]);
	});

	test('an answer that is missing counts as not there', () => {
		expect(filesToReplace(['/o/a.mp4', '/o/b.mp4'], [true], null)).toEqual(['/o/a.mp4']);
		expect(filesToReplace(['/o/a.mp4'], [], null)).toEqual([]);
	});
});
