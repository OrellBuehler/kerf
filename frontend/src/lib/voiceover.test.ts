import { describe, expect, test } from 'bun:test';
import {
	DEFAULT_PREFS,
	PARAGRAPH_GAP,
	SENTENCE_GAP,
	WORDS_PER_SECOND,
	approxMB,
	clampSpeed,
	defaultCaptionStyle,
	estimateSeconds,
	fmtSpeed,
	isVoiceoverCancelled,
	parsePrefs,
	scriptSegments,
	splitSentences,
	stageLabel,
	voiceInfo,
	voiceName,
	wordCount
} from './voiceover';

describe('splitSentences', () => {
	test('splits on sentence punctuation and keeps the terminator', () => {
		expect(splitSentences('Hello there. How are you? Fine!').map((s) => s.text)).toEqual([
			'Hello there.',
			'How are you?',
			'Fine!'
		]);
	});
	test('does not split inside a number or on a missing space', () => {
		expect(splitSentences('It grew 3.5 times.').map((s) => s.text)).toEqual(['It grew 3.5 times.']);
	});
	test('a trailing fragment with no punctuation is still a sentence', () => {
		expect(splitSentences('One. Two without stop').map((s) => s.text)).toEqual(['One.', 'Two without stop']);
	});
	test('a closing quote stays with its sentence', () => {
		expect(splitSentences('She said "go." Then left.').map((s) => s.text)).toEqual(['She said "go."', 'Then left.']);
	});
	test('a blank line ends a sentence and marks the next as a paragraph', () => {
		const s = splitSentences('First line\n\nSecond one. Third.\r\n\r\nFourth');
		expect(s.map((x) => x.text)).toEqual(['First line', 'Second one.', 'Third.', 'Fourth']);
		expect(s.map((x) => x.paragraph)).toEqual([false, true, false, true]);
	});
	test('a single newline is just a space', () => {
		expect(splitSentences('wrapped\nline.').map((s) => s.text)).toEqual(['wrapped line.']);
	});
	test('empty and whitespace-only scripts have none', () => {
		expect(splitSentences('')).toEqual([]);
		expect(splitSentences(' \n\n  \n')).toEqual([]);
	});
});

describe('scriptSegments', () => {
	test('times each sentence by word count and leaves the sentence gap between', () => {
		const segs = scriptSegments('One two three four five. Six seven.', 1);
		const first = 5 / WORDS_PER_SECOND;
		expect(segs[0].start).toBe(0);
		expect(segs[0].end).toBeCloseTo(first, 3);
		expect(segs[1].start).toBeCloseTo(first + SENTENCE_GAP, 3);
		expect(segs[1].end).toBeCloseTo(first + SENTENCE_GAP + 2 / WORDS_PER_SECOND, 3);
	});
	test('a blank line leaves the longer paragraph gap', () => {
		const segs = scriptSegments('Alpha beta.\n\nGamma delta.', 1);
		expect(segs[1].start - segs[0].end).toBeCloseTo(PARAGRAPH_GAP, 3);
	});
	test('speed shortens the reading but not the gaps', () => {
		const slow = scriptSegments('One two three. Four five six.', 1);
		const fast = scriptSegments('One two three. Four five six.', 2);
		expect(fast[0].end).toBeCloseTo(slow[0].end / 2, 3);
		expect(fast[1].start - fast[0].end).toBeCloseTo(SENTENCE_GAP, 3);
	});
	test('segments never overlap and run in order', () => {
		const segs = scriptSegments('A b c. D e!\n\nF g h i? J.', 1.3);
		for (let i = 1; i < segs.length; i++) expect(segs[i].start).toBeGreaterThanOrEqual(segs[i - 1].end);
	});
});

describe('estimateSeconds', () => {
	test('is the end of the last sentence, and zero for nothing', () => {
		expect(estimateSeconds('')).toBe(0);
		expect(estimateSeconds('one two three four five')).toBeCloseTo(5 / WORDS_PER_SECOND, 3);
	});
});

describe('voices', () => {
	test('name and traits come from the id', () => {
		expect(voiceName('af_heart')).toBe('Heart');
		expect(voiceInfo('bm_george')).toEqual({ id: 'bm_george', name: 'George', accent: 'gb', gender: 'male', downloaded: false });
		expect(voiceInfo('af_nova', true)).toMatchObject({ accent: 'us', gender: 'female', downloaded: true });
	});
});

describe('helpers', () => {
	test('speed is clamped and printed without trailing zeros', () => {
		expect(clampSpeed(9)).toBe(2);
		expect(clampSpeed(0.1)).toBe(0.5);
		expect(clampSpeed(NaN)).toBe(1);
		expect(fmtSpeed(1)).toBe('1.0×');
		expect(fmtSpeed(1.25)).toBe('1.25×');
		expect(fmtSpeed(0.95)).toBe('0.95×');
	});
	test('word count ignores runs of whitespace', () => {
		expect(wordCount('  a  b\n\nc ')).toBe(3);
		expect(wordCount('')).toBe(0);
	});
	test('download size rounds to whole megabytes', () => {
		expect(approxMB(104857600)).toBe('~100 MB');
		expect(approxMB(10)).toBe('~1 MB');
	});
	test('every stage has a label', () => {
		expect(stageLabel('download_model')).toContain('model');
		expect(stageLabel('synthesize')).toBe('Reading the script…');
	});
	test('a vertical frame defaults to word punch', () => {
		expect(defaultCaptionStyle({ width: 1080, height: 1920 })).toBe('word_punch');
		expect(defaultCaptionStyle({ width: 1920, height: 1080 })).toBe('lines');
		expect(defaultCaptionStyle({ width: 1080, height: 1080 })).toBe('lines');
		expect(defaultCaptionStyle(null)).toBe('lines');
	});
});

describe('isVoiceoverCancelled', () => {
	test('reads both a bare string and an Error', () => {
		expect(isVoiceoverCancelled('voiceover cancelled')).toBe(true);
		expect(isVoiceoverCancelled(new Error('voiceover cancelled'))).toBe(true);
		expect(isVoiceoverCancelled('model download failed')).toBe(false);
	});
});

describe('parsePrefs', () => {
	test('missing or corrupt storage gives the defaults', () => {
		expect(parsePrefs(null)).toEqual(DEFAULT_PREFS);
		expect(parsePrefs('not json')).toEqual(DEFAULT_PREFS);
		expect(parsePrefs('null')).toEqual(DEFAULT_PREFS);
	});
	test('valid fields are kept and out-of-range speed is clamped', () => {
		expect(parsePrefs('{"voice":"bf_emma","speed":3,"caption":false,"captionStyle":"lines"}')).toEqual({
			voice: 'bf_emma',
			speed: 2,
			caption: false,
			captionStyle: 'lines'
		});
	});
	test('wrong types fall back field by field', () => {
		expect(parsePrefs('{"voice":7,"speed":"fast","caption":1,"captionStyle":"x"}')).toEqual(DEFAULT_PREFS);
	});
});
