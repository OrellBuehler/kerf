import { describe, expect, test } from 'bun:test';
import { coverLine, exportedLine, failure, fileName, savedLine, tidyReason } from './failure-copy';

describe('a reason as a sentence', () => {
	test('the layer the backend named it from is dropped, and the first letter raised', () => {
		expect(tidyReason('invalid argument: this asset has no sound to separate')).toBe('This asset has no sound to separate');
		expect(tidyReason('media engine error: ffmpeg exited with status 1')).toBe('FFmpeg exited with status 1');
		expect(tidyReason('io error: permission denied (os error 13)')).toBe('Permission denied (os error 13)');
		expect(tidyReason('media engine error: io error: no space left on device')).toBe('No space left on device');
	});
	test('a message that is already a sentence is left as it is', () => {
		expect(tidyReason("Couldn't open the file")).toBe("Couldn't open the file");
		expect(tidyReason('  spaced  ')).toBe('Spaced');
		expect(tidyReason('')).toBe('');
	});
	test('only a leading layer goes, not one that is part of the sentence', () => {
		expect(tidyReason('the invalid argument: x was refused')).toBe('The invalid argument: x was refused');
	});
});

describe('a failure leads with what was being done', () => {
	test('the lead, a dash, the reason', () => {
		expect(failure('Export failed', new Error('io error: permission denied (os error 13)'))).toBe('Export failed — Permission denied (os error 13)');
		expect(failure('Voiceover failed', 'model download was interrupted')).toBe('Voiceover failed — Model download was interrupted');
		expect(failure('Stem separation failed', new Error('invalid argument: this asset has no sound to separate'))).toBe(
			'Stem separation failed — This asset has no sound to separate'
		);
	});
	test('with nothing to say it is just the lead', () => {
		expect(failure('Export failed', new Error(''))).toBe('Export failed');
	});
	test('it never says "Could not"', () => {
		expect(failure('Export failed', new Error('x'))).not.toMatch(/could not/i);
	});
});

describe('a finished file is named by its file', () => {
	test('either separator', () => {
		expect(fileName('/home/me/Videos/cut-1x1.mp4')).toBe('cut-1x1.mp4');
		expect(fileName('C:\\Users\\me\\Videos\\cut.mp4')).toBe('cut.mp4');
		expect(fileName('cut.mp4')).toBe('cut.mp4');
		expect(fileName('/trailing/')).toBe('trailing');
	});
	test('one file or several', () => {
		expect(exportedLine(['/v/my-project-final-cut-v3.mp4'])).toBe('Exported my-project-final-cut-v3.mp4');
		expect(exportedLine(['/v/a-9x16.mp4', '/v/a-1x1.mp4', '/v/a.mp4'])).toBe('Exported 3 files');
	});
	test('saved and cover', () => {
		expect(savedLine('/p/cut.kerf')).toBe('Saved cut.kerf');
		expect(coverLine('/p/cover.png')).toBe('Saved cover cover.png');
	});
});
