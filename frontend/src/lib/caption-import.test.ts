import { describe, expect, test } from 'bun:test';
import {
	MAX_CAPTION_CUES,
	MAX_CAPTION_FILE_BYTES,
	MAX_CAPTION_WORDS,
	MAX_CUE_CHARS,
	MAX_IMPORTED_CAPTIONS,
	MAX_LINE_CHARS,
	describeImport,
	detectFormat,
	importCaptionsInto,
	parseAss,
	parseCaptions,
	parseFormat,
	parseSrt,
	parseTimestamp,
	resolveBase
} from './caption-import';
import { captionsForTimeline, placeCues, resolveCaptions } from './captions';
import type { CaptionImportSummary, Clip, TextOverlay, Timeline, TranscriptSegment } from './types';

// The Rust side (`kerf_core::captions_import`) is the authority; these cases are
// the same ones its tests pin, so the browser harness cannot drift from it.

const texts = (p: { cues: { text: string }[] }) => p.cues.map((c) => c.text);
const cue = (start: number, end: number, text: string) => ({ start, end, text });

describe('timestamps', () => {
	test('take either separator and any fraction width', () => {
		expect(parseTimestamp('00:00:01,500')).toBe(1.5);
		expect(parseTimestamp('00:00:01.500')).toBe(1.5);
		expect(parseTimestamp('0:00:01.50')).toBe(1.5);
		expect(parseTimestamp('00:00:01,5')).toBe(1.5);
		expect(parseTimestamp('01:30')).toBe(90);
		expect(parseTimestamp('00:00:02')).toBe(2);
		expect(parseTimestamp('100:00:00,000')).toBe(360_000);
		expect(parseTimestamp('01:02:03,004')).toBeCloseTo(3723.004, 9);
	});

	test('what is not a time is not one', () => {
		for (const bad of ['', 'garbage', '00:00:0a,000', '-00:00:01,000', '00:00:01,5x', '12', '1:2:3:4', '00::01', '9999999:00:00']) {
			expect(parseTimestamp(bad)).toBeNull();
		}
		expect(parseTimestamp('00:00:01,123456789012345678901234567890')).not.toBeNull();
	});
});

describe('SubRip', () => {
	test('a plain file reads', () => {
		const p = parseSrt('1\n00:00:01,000 --> 00:00:03,500\nHello there\n\n2\n00:00:04,000 --> 00:00:05,000\nGeneral Kenobi\n');
		expect(p.skipped).toBe(0);
		expect(p.cues).toEqual([cue(1, 3.5, 'Hello there'), cue(4, 5, 'General Kenobi')]);
	});

	test('a BOM and CRLF and CR line ends are tolerated', () => {
		const crlf = '﻿1\r\n00:00:01,000 --> 00:00:02,000\r\nOne\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nTwo\r\n';
		expect(texts(parseSrt(crlf))).toEqual(['One', 'Two']);
		const cr = '1\r00:00:01,000 --> 00:00:02,000\rOne\r\r2\r00:00:03,000 --> 00:00:04,000\rTwo\r';
		expect(texts(parseSrt(cr))).toEqual(['One', 'Two']);
		expect(parseSrt(crlf).skipped).toBe(0);
	});

	test('indices are never trusted', () => {
		const p = parseSrt(
			'99999999999999999999999999\n00:00:01,000 --> 00:00:02,000\nHuge index\n\n' +
				'7\n00:00:03,000 --> 00:00:04,000\nOut of order index\n\n' +
				'00:00:05,000 --> 00:00:06,000\nNo index at all\n\n' +
				'0\n00:00:07,000 --> 00:00:08,000\nIndex zero\n'
		);
		expect(p.skipped).toBe(0);
		expect(texts(p)).toEqual(['Huge index', 'Out of order index', 'No index at all', 'Index zero']);
	});

	test('multi-line cues keep their line breaks', () => {
		expect(parseSrt('1\n00:00:01,000 --> 00:00:03,000\nFirst line\nSecond line\n').cues[0].text).toBe('First line\nSecond line');
	});

	test('cues with no blank line between them split and give the index back', () => {
		const p = parseSrt(
			'1\n00:00:01,000 --> 00:00:02,000\nAlpha\n2\n00:00:03,000 --> 00:00:04,000\nBravo\n3\n00:00:05,000 --> 00:00:06,000\nCharlie'
		);
		expect(texts(p)).toEqual(['Alpha', 'Bravo', 'Charlie']);
		expect(p.skipped).toBe(0);
		expect(parseSrt('1\n00:00:01,000 --> 00:00:02,000\nThe year is\n1984\n').cues[0].text).toBe('The year is\n1984');
	});

	test('basic markup and override blocks are stripped, the author brackets stay', () => {
		const p = parseSrt(
			'1\n00:00:01,000 --> 00:00:03,000\n{\\an8}<i>Italic</i> and <b>bold</b> and <font color="#ff0000">red</font><br>next\n\n' +
				'2\n00:00:04,000 --> 00:00:05,000\nKeep <laughs> and {braces} and 2 < 3 and <3\n'
		);
		expect(p.cues[0].text).toBe('Italic and bold and red\nnext');
		expect(p.cues[1].text).toBe('Keep <laughs> and {braces} and 2 < 3 and <3');
	});

	test('positioning after the end time is ignored, and a triple dash is a dash', () => {
		expect(parseSrt('1\n00:00:01,000 --> 00:00:02,000  X1:100 X2:200 Y1:300 Y2:400\nHi\n').cues).toEqual([cue(1, 2, 'Hi')]);
		expect(parseSrt('1\n00:00:01,000 ---> 00:00:02,000\nHi\n').cues).toEqual([cue(1, 2, 'Hi')]);
	});

	test('overlapping cues are all kept and sorted', () => {
		const p = parseSrt(
			'1\n00:00:05,000 --> 00:00:08,000\nLate\n\n2\n00:00:01,000 --> 00:00:06,000\nEarly and long\n\n3\n00:00:02,000 --> 00:00:03,000\nInside\n'
		);
		expect(texts(p)).toEqual(['Early and long', 'Inside', 'Late']);
	});

	test('zero and negative durations and empty cues are skipped and counted', () => {
		const p = parseSrt(
			'1\n00:00:01,000 --> 00:00:01,000\nZero\n\n' +
				'2\n00:00:05,000 --> 00:00:04,000\nBackwards\n\n' +
				'3\n00:00:06,000 --> 00:00:07,000\n\n' +
				'4\n00:00:08,000 --> 00:00:09,000\n{\\an8}<i></i>\n\n' +
				'5\n00:00:10,000 --> 00:00:11,000\nGood\n'
		);
		expect(texts(p)).toEqual(['Good']);
		expect(p.skipped).toBe(4);
	});

	test('a broken timing line costs one skip, not its text lines', () => {
		const p = parseSrt('1\n00:00:01,000 --> nonsense\nThis cue\nhas two lines\n\n2\n00:00:03,000 --> 00:00:04,000\nStill read\n');
		expect(texts(p)).toEqual(['Still read']);
		expect(p.skipped).toBe(1);
	});

	test('a stray line is counted and an arrow in text is text', () => {
		const p = parseSrt('WEBVTT\n\n1\n00:00:01,000 --> 00:00:03,000\nGo from a --> b\n');
		expect(p.skipped).toBe(1);
		expect(p.cues[0].text).toBe('Go from a --> b');
	});

	test('an absurdly long cue is skipped, not chunked', () => {
		const long = 'word '.repeat(MAX_CUE_CHARS);
		const p = parseSrt(`1\n00:00:01,000 --> 00:00:03,000\n${long}\n\n2\n00:00:04,000 --> 00:00:05,000\nFine\n`);
		expect(texts(p)).toEqual(['Fine']);
		expect(p.skipped).toBe(1);
	});

	test('empty and cueless input is an empty parse', () => {
		expect(parseSrt('')).toEqual({ cues: [], skipped: 0 });
		expect(parseSrt('﻿\n\n\n').cues).toEqual([]);
		expect(parseSrt('just a note\nno cues here').cues).toEqual([]);
	});
});

const ASS =
	'﻿[Script Info]\n; a comment\nTitle: Test\nScriptType: v4.00+\n\n' +
	'[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,20\n\n' +
	'[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n' +
	'Dialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,Hello, world\n' +
	'Comment: 0,0:00:02.00,0:00:03.00,Default,,0,0,0,,not a caption\n' +
	'Dialogue: 0,0:00:04.00,0:00:05.00,Default,,0,0,0,,{\\an8\\i1}Top{\\i0}\\Nand bottom\n';

describe('ASS / SSA', () => {
	test('a plain file reads and commas in the text survive; styles and comments are ignored', () => {
		const p = parseAss(ASS);
		expect(p.skipped).toBe(0);
		expect(p.cues).toEqual([cue(1, 3.5, 'Hello, world'), cue(4, 5, 'Top\nand bottom')]);
	});

	test('the Format line decides which column is which', () => {
		const reordered = '[Events]\nFormat: Start, End, Text, Layer, Style\nDialogue: 0:00:01.00,0:00:02.00,Reordered,0,Default\n';
		expect(parseAss(reordered).cues).toEqual([cue(1, 2, 'Reordered')]);
		const ssa =
			'[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n' +
			'Dialogue: Marked=0,0:00:01.00,0:00:02.00,*Default,NTP,0000,0000,0000,,SSA line\n';
		expect(parseAss(ssa).cues).toEqual([cue(1, 2, 'SSA line')]);
		expect(() => parseAss('[Events]\nFormat: Layer, Style, Text\nDialogue: 0,Default,hi\n')).toThrow('Start, End and Text');
	});

	test('a headerless run of dialogue lines reads with the default columns', () => {
		expect(parseAss('Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Bare\n').cues).toEqual([cue(1, 2, 'Bare')]);
	});

	test('escapes, overrides and drawings are resolved', () => {
		const p = parseAss(
			'[Events]\n' +
				'Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,One\\NTwo\\nThree\\hFour\n' +
				'Dialogue: 0,0:00:03.00,0:00:04.00,D,,0,0,0,,{\\pos(10,10)\\fad(100,100)\\k20}Karaoke {comment}text\n' +
				'Dialogue: 0,0:00:05.00,0:00:06.00,D,,0,0,0,,{\\p1}m 0 0 l 100 0 100 100{\\p0}\n' +
				'Dialogue: 0,0:00:07.00,0:00:08.00,D,,0,0,0,,Before{\\p1}m 0 0 l 1 1{\\p0}After\n' +
				'Dialogue: 0,0:00:09.00,0:00:10.00,D,,0,0,0,,Open { brace\n'
		);
		expect(p.cues[0].text).toBe('One\nTwo\nThree Four');
		expect(p.cues[1].text).toBe('Karaoke text');
		expect(p.cues[2].text).toBe('BeforeAfter');
		expect(p.cues[3].text).toBe('Open { brace');
		expect(p.skipped).toBe(1);
	});

	test('dialogue is ordered by time and malformed lines are counted', () => {
		const p = parseAss(
			'[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n' +
				'Dialogue: 0,0:00:09.00,0:00:10.00,D,,0,0,0,,Last\n' +
				'Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,First\n' +
				'Dialogue: 0,0:00:03.00,0:00:03.00,D,,0,0,0,,Zero length\n' +
				'Dialogue: 0,garbage,0:00:05.00,D,,0,0,0,,Bad time\n' +
				'Dialogue: 0,0:00:06.00\n' +
				'Dialogue: 0,0:00:07.00,0:00:08.00,D,,0,0,0,,\n' +
				'Picture: 0,0:00:02.00,0:00:03.00,pic.png\n' +
				'Unknown: whatever\n'
		);
		expect(texts(p)).toEqual(['First', 'Last']);
		expect(p.skipped).toBe(4);
	});
});

describe('hostile input', () => {
	/** Fail if `f` takes five seconds or more (wide on purpose: catch the algorithm, not a busy machine). These inputs were quadratic before their
	 *  scans were bounded (a megabyte of `<` took tens of seconds). */
	const withinASecond = <T>(what: string, f: () => T): T => {
		const started = performance.now();
		const out = f();
		const took = performance.now() - started;
		expect(`${what}: ${took < 5000 ? 'quick' : `${Math.round(took)} ms`}`).toBe(`${what}: quick`);
		return out;
	};
	const srtCueOf = (line: string, lines: number) => `1\n00:00:01,000 --> 00:00:03,000\n${Array(lines).fill(line).join('\n')}\n`;

	test('pathological markup parses in linear time', () => {
		const width = MAX_LINE_CHARS - 1;
		const cases: [string, string][] = [
			['unmatched <', '<'.repeat(width)],
			['< with a letter', '<a'.repeat(width / 2)],
			['< each 300 chars apart', `<${'a'.repeat(299)}`.repeat(Math.floor(width / 300))],
			['unterminated override blocks', '{\\'.repeat(width / 2)],
			['braces that are not overrides', '{'.repeat(width)],
			['a far } after many {\\', `${'{\\'.repeat(width / 2 - 1)}}`],
			['closing tags', '</i'.repeat(Math.floor(width / 3))]
		];
		for (const [what, line] of cases) {
			withinASecond(what, () => expect(parseSrt(srtCueOf(line, 64)).cues).toEqual([]));
			const ass = `[Events]\n${`Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,${line}\n`.repeat(64)}`;
			withinASecond(what, () => expect(parseAss(ass).cues).toEqual([]));
		}
		// Real text between the brackets still comes out.
		expect(parseSrt(srtCueOf('<<<< hello >>>> <i>world</i>', 1)).cues[0].text).toBe('<<<< hello >>>> world');
	});

	test('a line over the cap is refused before it is cleaned', () => {
		for (const fill of ['<', '{', '{\\', ' ']) {
			const oneLine = fill.repeat(Math.floor(MAX_CAPTION_FILE_BYTES / fill.length));
			withinASecond('a 5 MiB line', () => {
				expect(parseSrt(oneLine).cues).toEqual([]);
				const ass = parseAss(`Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,${oneLine}`);
				expect([ass.cues.length, ass.skipped]).toEqual([0, 1]);
			});
		}
		const long = 'x'.repeat(MAX_LINE_CHARS + 1);
		const p = parseSrt(`1\n00:00:01,000 --> 00:00:02,000\nfine\n${long}\n\n2\n00:00:03,000 --> 00:00:04,000\nStill read\n`);
		expect(texts(p)).toEqual(['Still read']);
		expect(p.skipped).toBe(1);
		const ass = parseAss(`[Events]\nComment: 0,${long}\nDialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,ok\n`);
		expect([ass.cues.length, ass.skipped]).toEqual([1, 0]);
		expect(parseSrt(srtCueOf('y'.repeat(MAX_CUE_CHARS), 1)).cues).toHaveLength(1);
	});

	test('more cues than an import takes stop being read', () => {
		const flood = '1\n00:00:01,000 --> 00:00:02,000\nx\n\n'.repeat(20_000);
		expect(() => withinASecond('a flood of cues', () => parseCaptions(flood))).toThrow('more than 10000 cues');
	});

	test('the words in one import are capped', () => {
		const thousand = Array(1000).fill('a').join(' ');
		const cues = (n: number) =>
			Array.from({ length: n }, (_, i) => `${i}\n00:00:${String(i % 60).padStart(2, '0')},000 --> 00:00:${String(i % 60).padStart(2, '0')},500\n${thousand}\n\n`).join('');
		expect(() => parseCaptions(cues(101))).toThrow(`101000 words; Kerf imports at most ${MAX_CAPTION_WORDS}`);
		expect(() => parseCaptions(cues(50))).not.toThrow();
	});

	test('control characters never reach a caption', () => {
		const srt = '1\n00:00:01,000 --> 00:00:03,000\nHel\0lo \u001b[31mred\u0007\tworld\u0085end\u007f\n';
		expect(parseSrt(srt).cues[0].text).toBe('Hello [31mred world end');
		const ass = '[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,a\0b{\\i1}\u0001c\\Nd\u001fe\n';
		expect(parseAss(ass).cues[0].text).toBe('abc\nde');
		const blank = parseSrt('1\n00:00:01,000 --> 00:00:03,000\n\0\u001b\n');
		expect([blank.cues.length, blank.skipped]).toEqual([0, 1]);
		expect(parseSrt('1\n00:00:01,000 --> 00:00:03,000\nCafé ☕ 日本語 🎬\n').cues[0].text).toBe('Café ☕ 日本語 🎬');
	});

	test('a CR-only file is still told apart', () => {
		const ass =
			'[Script Info]\rTitle: x\r\r[Events]\rFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\rDialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,Hi\r';
		expect(detectFormat(ass)).toBe('ass');
		expect(parseCaptions(ass).parsed.cues).toHaveLength(1);
		expect(parseCaptions('1\r00:00:01,000 --> 00:00:02,000\rHi\r').format).toBe('srt');
	});
});

describe('entry points', () => {
	test('the format is guessed from the text', () => {
		expect(detectFormat(ASS)).toBe('ass');
		expect(detectFormat('[events]\nDialogue: x')).toBe('ass');
		expect(detectFormat('1\n00:00:01,000 --> 00:00:02,000\nHi')).toBe('srt');
		expect(detectFormat('Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,hi')).toBe('ass');
		expect(detectFormat('1\n00:00:01,000 --> 00:00:02,000\nDialogue: a play')).toBe('srt');
		expect(detectFormat('')).toBe('srt');
		expect(parseFormat('.SSA')).toBe('ass');
		expect(parseFormat('vtt')).toBeNull();
	});

	test('a named format wins over a guess, and the cue count is capped', () => {
		expect(parseCaptions(ASS).format).toBe('ass');
		const told = parseCaptions(ASS, 'srt');
		expect(told.format).toBe('srt');
		expect(told.parsed.cues).toEqual([]);
		const many = Array.from({ length: MAX_CAPTION_CUES + 1 }, (_, i) => `${i}\n00:00:00,000 --> 00:00:01,000\nx\n\n`).join('');
		expect(() => parseCaptions(many)).toThrow('at most');
	});

	test('oversized text is refused', () => {
		expect(() => parseCaptions('a'.repeat(MAX_CAPTION_FILE_BYTES + 1))).toThrow('larger than 5 MiB');
	});

	test('a time base resolves from loose arguments and refuses contradictions', () => {
		expect(resolveBase()).toEqual({ kind: 'timeline' });
		expect(resolveBase('timeline')).toEqual({ kind: 'timeline' });
		expect(resolveBase(' Timeline ')).toEqual({ kind: 'timeline' });
		expect(resolveBase('', null)).toEqual({ kind: 'timeline' });
		expect(resolveBase(undefined, 'a1')).toEqual({ kind: 'source', assetId: 'a1' });
		expect(resolveBase('SOURCE', 'a1')).toEqual({ kind: 'source', assetId: 'a1' });
		expect(() => resolveBase('source')).toThrow('asset_id');
		expect(() => resolveBase('timeline', 'a1')).toThrow('only applies');
		expect(() => resolveBase('sideways')).toThrow('"timeline" or "source"');
	});
});

// ---- placement ---------------------------------------------------------------

const clip = (over: Partial<Clip> = {}): Clip =>
	({ id: 'c1', asset_id: 'a1', source_in: 0, source_out: 10, timeline_start: 0, volume: 1, speed: 1, ...over }) as Clip;

const timelineOf = (clips: Clip[], extra: Partial<Timeline> = {}): Timeline => ({
	tracks: [{ id: 't1', kind: 'video', name: 'V1', clips }],
	overlays: [],
	markers: [],
	...extra
});

const seg = (start: number, end: number, text: string): TranscriptSegment => ({ start, end, text });
const rounded = (o: { text: string; start: number; end: number }[]) =>
	o.map((x) => [x.text, Math.round(x.start * 100) / 100, Math.round(x.end * 100) / 100]);
const lines = resolveCaptions();
const accounted = (
	p: { placed: number; droppedOutside: number; droppedShort: number; droppedOverlap: number },
	n: number
) => expect(p.placed + p.droppedOutside + p.droppedShort + p.droppedOverlap).toBe(n);

describe('placeCues', () => {
	test('cues in timeline time land where the file says, as generated captions', () => {
		const p = placeCues(timelineOf([clip()]), [seg(1, 3, 'Hello there'), seg(4, 6, 'General Kenobi')], { kind: 'timeline' }, lines);
		expect(rounded(p.overlays)).toEqual([
			['Hello there', 1, 3],
			['General Kenobi', 4, 6]
		]);
		accounted(p, 2);
		expect(p.overlays.every((o) => o.generated && o.pos_y === lines.pos_y)).toBe(true);
	});

	test('an imported cue is chunked exactly like a transcript segment', () => {
		const timeline = timelineOf([clip({ source_out: 12 })], { format: { width: 1080, height: 1920, fit: 'cover' } });
		const line = seg(0.5, 11, 'Today we are talking about non-destructive editing in Kerf, and a few extraordinarily long words');
		for (const style of ['lines', 'word_punch'] as const) {
			const opts = resolveCaptions({ style });
			const fromTranscript = captionsForTimeline(timeline, { a1: [line] }, opts);
			const fromCues = placeCues(timeline, [line], { kind: 'timeline' }, opts).overlays;
			expect(fromCues.length).toBeGreaterThan(1);
			expect(fromCues).toEqual(fromTranscript);
		}
		const viaSource = placeCues(timeline, [line], { kind: 'source', assetId: 'a1' }, lines).overlays;
		expect(viaSource).toEqual(captionsForTimeline(timeline, { a1: [line] }, lines));
	});

	test('cues past the end of the cut are dropped and the straddler clipped', () => {
		const p = placeCues(
			timelineOf([clip()]),
			[seg(2, 4, 'inside'), seg(9, 12, 'straddles the end'), seg(10, 12, 'starts at the end'), seg(20, 25, 'long gone')],
			{ kind: 'timeline' },
			lines
		);
		accounted(p, 4);
		expect([p.placed, p.droppedOutside]).toEqual([2, 2]);
		expect(p.overlays[p.overlays.length - 1].end).toBeCloseTo(10, 9);
		const sliver = placeCues(timelineOf([clip()]), [seg(9.95, 12, 'barely')], { kind: 'timeline' }, lines);
		expect(sliver.overlays).toEqual([]);
		// It met the cut, for too short a moment to read: not "outside" it.
		expect([sliver.droppedOutside, sliver.droppedShort]).toEqual([0, 1]);
		accounted(sliver, 1);
	});

	test('an empty timeline has no end to run past', () => {
		const p = placeCues(timelineOf([]), [seg(0, 2, 'first'), seg(600, 602, 'much later')], { kind: 'timeline' }, lines);
		expect([p.placed, p.droppedOutside]).toEqual([2, 0]);
	});

	test('a muted track silences source cues but not timeline cues', () => {
		const timeline = timelineOf([clip({ source_out: 6 })]);
		timeline.tracks.push({ id: 't2', kind: 'audio', name: 'A1', clips: [clip({ id: 'm', asset_id: 'music', source_out: 20 })] });
		timeline.tracks[0].muted = true;
		const cues = [seg(1, 3, 'heard')];
		const source = placeCues(timeline, cues, { kind: 'source', assetId: 'a1' }, lines);
		expect(source.overlays).toEqual([]);
		expect(source.droppedOutside).toBe(1);
		expect(placeCues(timeline, cues, { kind: 'timeline' }, lines).placed).toBe(1);
		timeline.tracks[0].muted = false;
		timeline.tracks[0].clips[0].enabled = false;
		expect(placeCues(timeline, cues, { kind: 'source', assetId: 'a1' }, lines).overlays).toEqual([]);
	});

	test('source cues follow trim, move, speed and reverse', () => {
		const cues = [seg(12, 14, 'alpha beta')];
		const place = (c: Clip) => placeCues(timelineOf([c]), cues, { kind: 'source', assetId: 'a1' }, lines).overlays;
		const trimmed = clip({ source_in: 10, source_out: 20, timeline_start: 4 });
		expect(rounded(place(trimmed))).toEqual([['alpha beta', 6, 8]]);
		expect(rounded(place({ ...trimmed, speed: 2 }))).toEqual([['alpha beta', 5, 6]]);
		expect(rounded(place({ ...trimmed, speed: -1 }))).toEqual([['alpha beta', 10, 12]]);
		const early = placeCues(timelineOf([trimmed]), [seg(0, 5, 'trimmed away')], { kind: 'source', assetId: 'a1' }, lines);
		expect(early.overlays).toEqual([]);
		expect(early.droppedOutside).toBe(1);
	});

	test('source cues reach every clip of the asset and only that asset', () => {
		const timeline = timelineOf([
			clip({ id: 'x', source_out: 4 }),
			clip({ id: 'y', asset_id: 'other', source_out: 4, timeline_start: 4 }),
			clip({ id: 'z', source_out: 4, timeline_start: 8 })
		]);
		const p = placeCues(timeline, [seg(1, 3, 'said twice')], { kind: 'source', assetId: 'a1' }, lines);
		expect(rounded(p.overlays)).toEqual([
			['said twice', 1, 3],
			['said twice', 9, 11]
		]);
		expect(p.placed).toBe(1);
		accounted(p, 1);
	});

	test('imported cues never share the screen', () => {
		const cues = [
			seg(0, 4, 'alpha beta'),
			seg(2, 6, 'gamma delta'),
			seg(3, 3.4, 'buried'),
			seg(8, 10, 'twice over'),
			seg(8, 10, 'twice over')
		];
		const p = placeCues(timelineOf([clip({ source_out: 20 })]), cues, { kind: 'timeline' }, lines);
		accounted(p, 5);
		expect([p.placed, p.droppedOutside, p.droppedOverlap]).toEqual([3, 0, 2]);
		expect(rounded(p.overlays)).toEqual([
			['alpha beta', 0, 4],
			['gamma delta', 4, 6],
			['twice over', 8, 10]
		]);
	});

	test('unusable cues count as not placed rather than vanishing', () => {
		const cues = [seg(1, 2, 'fine'), seg(3, 3, 'zero'), seg(5, 4, 'backwards'), seg(6, 7, '   '), seg(NaN, 8, 'not a time')];
		for (const base of [{ kind: 'timeline' }, { kind: 'source', assetId: 'a1' }] as const) {
			const p = placeCues(timelineOf([clip()]), cues, base, lines);
			accounted(p, 5);
			expect([p.placed, p.droppedOutside]).toEqual([1, 4]);
		}
	});
});

// ---- the whole import (what the harness runs) --------------------------------

const SRT = '1\n00:00:01,000 --> 00:00:03,000\nHello there\n\n2\n00:00:04,000 --> 00:00:06,000\nGeneral Kenobi\n';
const env = { assetKnown: (id: string) => id === 'a1' || id === 'a2' };

describe('importCaptionsInto', () => {
	test('replaces the generated set, keeps typed titles, and accounts for every cue', () => {
		const title: TextOverlay = { id: 'title', text: 'Chapter one', start: 0, end: 1.5, pos_x: 0.5, pos_y: 0.5, size: 0.06, color: 'white', bold: false };
		const stale: TextOverlay = { ...title, id: 'old', text: 'old caption', generated: true };
		const timeline = timelineOf([clip()], { overlays: [title, stale] });
		const out = importCaptionsInto(timeline, SRT, {}, env);
		expect(out.kept.map((o) => o.id)).toEqual(['title']);
		expect(rounded(out.overlays)).toEqual([
			['Hello there', 1, 3],
			['General Kenobi', 4, 6]
		]);
		expect(out.summary).toEqual({
			format: 'srt',
			cues: 2,
			placed: 2,
			captions: 2,
			skipped_lines: 0,
			dropped_outside: 0,
			dropped_short: 0,
			dropped_overlap: 0,
			replaced: 1
		});
	});

	test('ASS cues in source time follow the cut', () => {
		const ass =
			'[Script Info]\nTitle: x\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n' +
			'Dialogue: 0,0:00:06.00,0:00:08.00,Default,,0,0,0,,Kept\n' +
			'Dialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Trimmed away\n' +
			'Dialogue: 0,0:00:30.00,0:00:32.00,Default,,0,0,0,,Past the footage\n' +
			'Dialogue: 0,broken\n';
		const timeline = timelineOf([clip({ source_in: 5.5, source_out: 12.5, timeline_start: 2 })]);
		const out = importCaptionsInto(timeline, ass, { base: { kind: 'source', assetId: 'a1' } }, env);
		expect(out.summary.format).toBe('ass');
		expect([out.summary.cues, out.summary.placed, out.summary.dropped_outside, out.summary.skipped_lines]).toEqual([3, 1, 2, 1]);
		expect(rounded(out.overlays)).toEqual([['Kept', 2.5, 4.5]]);
	});

	test('an import that cannot land throws', () => {
		const timeline = timelineOf([clip()]);
		expect(() => importCaptionsInto(timeline, 'not a subtitle file', {}, env)).toThrow('no captions found');
		expect(() => importCaptionsInto(timeline, '1\n00:05:00,000 --> 00:05:02,000\nToo late\n', {}, env)).toThrow('fall inside the cut');
		expect(() => importCaptionsInto(timeline, SRT, { base: { kind: 'source', assetId: 'nope' } }, env)).toThrow('asset not found');
		expect(() => importCaptionsInto(timeline, SRT, { base: { kind: 'source', assetId: 'a2' } }, env)).toThrow('not used by any clip');
		expect(() =>
			importCaptionsInto(timeline, '1\n00:09:00,000 --> 00:09:02,000\nElsewhere\n', { base: { kind: 'source', assetId: 'a1' } }, env)
		).toThrow('land on footage');
		expect(() => importCaptionsInto(timeline, SRT, { format: 'ass' }, env)).toThrow('no captions found in the ASS');
	});
});

describe('placeCues, the details', () => {
	test('a cue that meets the cut too briefly is short, not outside', () => {
		const timeline = timelineOf([clip({ source_in: 10, source_out: 20 })]);
		const onSource = placeCues(
			timeline,
			[seg(11, 13, 'readable'), seg(14, 14.05, 'a blink'), seg(9, 10.05, 'just inside'), seg(30, 32, 'elsewhere')],
			{ kind: 'source', assetId: 'a1' },
			lines
		);
		accounted(onSource, 4);
		expect([onSource.placed, onSource.droppedShort, onSource.droppedOutside, onSource.droppedOverlap]).toEqual([1, 2, 1, 0]);
		const cut = timelineOf([clip()]);
		const onCut = placeCues(
			cut,
			[
				seg(1, 3, 'readable'),
				seg(4, 4.05, 'a blink'),
				seg(-5, 0.05, 'starts early'),
				seg(-5, -1, 'before the start'),
				seg(9.96, 14, 'at the end'),
				seg(10, 12, 'after the end')
			],
			{ kind: 'timeline' },
			lines
		);
		accounted(onCut, 6);
		expect([onCut.placed, onCut.droppedShort, onCut.droppedOutside]).toEqual([1, 3, 2]);
	});

	test('simultaneous cues keep the order of the file; a transcript still sorts alphabetically', () => {
		const timeline = timelineOf([clip()]);
		const cues = [seg(1, 4, 'zebra'), seg(1, 4, 'apple')];
		const p = placeCues(timeline, cues, { kind: 'timeline' }, lines);
		expect(rounded(p.overlays)).toEqual([['zebra', 1, 4]]);
		accounted(p, 2);
		expect(p.droppedOverlap).toBe(1);
		expect(rounded(placeCues(timeline, cues, { kind: 'source', assetId: 'a1' }, lines).overlays)).toEqual([['zebra', 1, 4]]);
		// An identical pair still collapses with another between them.
		const same = placeCues(timeline, [seg(1, 3, 'same'), seg(1, 3, 'other'), seg(1, 3, 'same')], { kind: 'timeline' }, lines);
		accounted(same, 3);
		expect(same.placed).toBe(1);
		expect(rounded(captionsForTimeline(timeline, { a1: [seg(1, 4, 'zebra'), seg(1, 4, 'apple')] }, lines))).toEqual([['apple', 1, 4]]);
	});

	test('a thousand-word cue in word punch places quickly and loses nothing', () => {
		const thousand = Array(1000).fill('a').join(' ');
		const cues = Array.from({ length: 100 }, (_, i) => seg(i * 10, i * 10 + 6, thousand));
		const started = performance.now();
		const p = placeCues(timelineOf([clip({ source_out: 1000 })]), cues, { kind: 'timeline' }, resolveCaptions({ style: 'word_punch' }));
		expect(performance.now() - started).toBeLessThan(5000);
		accounted(p, 100);
		expect(p.placed).toBe(100);
		expect(p.overlays.reduce((n, o) => n + o.text.split(' ').length, 0)).toBe(100 * 1000);
	});
});

describe('importCaptionsInto, the details', () => {
	const place = (text: string, extra: object = {}, tl = timelineOf([clip({ source_out: 20 })])) =>
		importCaptionsInto(tl, text, extra, env);

	test('an offset moves every cue before it is placed, and is refused when it is not a number of seconds', () => {
		const broadcast = '1\n01:00:01,000 --> 01:00:03,000\nHello there\n\n2\n01:00:04,000 --> 01:00:06,000\nGeneral Kenobi\n';
		expect(() => place(broadcast)).toThrow('fall inside the cut');
		const out = place(broadcast, { offset: -3600 });
		expect(rounded(out.overlays)).toEqual([
			['Hello there', 1, 3],
			['General Kenobi', 4, 6]
		]);
		const early = '1\n00:00:00,000 --> 00:00:04,000\nStarts at zero\n\n2\n00:00:01,000 --> 00:00:02,000\nBefore it\n';
		const clipped = place(early, { offset: -2 });
		expect([clipped.summary.placed, clipped.summary.dropped_outside]).toEqual([1, 1]);
		expect(clipped.overlays[0].start).toBe(0);
		for (const bad of [NaN, Infinity, 1e9, -1e9]) expect(() => place(SRT, { offset: bad })).toThrow('offset');
	});

	test('when nothing could be shown the error says why', () => {
		expect(() => place('1\n00:00:04,000 --> 00:00:04,050\nA blink\n')).toThrow('too short to read');
		const out = place('1\n00:00:01,000 --> 00:00:03,000\nReadable\n\n2\n00:00:04,000 --> 00:00:04,050\nA blink\n\n3\n00:09:00,000 --> 00:09:03,000\nWay past the end\n');
		const s = out.summary;
		expect([s.cues, s.placed, s.dropped_outside, s.dropped_short, s.dropped_overlap]).toEqual([3, 1, 1, 1, 0]);
	});

	test('an import that would write too many captions is refused whole', () => {
		const ts = (t: number) =>
			`${String(Math.floor(t / 3600)).padStart(2, '0')}:${String(Math.floor(t / 60) % 60).padStart(2, '0')}:${String(Math.floor(t) % 60).padStart(2, '0')},000`;
		const text = Array.from({ length: 9000 }, (_, i) => `${i}\n${ts(i * 1.5)} --> ${ts(i * 1.5 + 1)}\nalpha bravo charlie delta echo\n\n`).join('');
		const empty = timelineOf([]);
		expect(() => place(text, { options: { style: 'word_punch' } }, empty)).toThrow(`at most ${MAX_IMPORTED_CAPTIONS} at a time`);
		const lines = place(text, {}, empty);
		expect(lines.summary.captions).toBeLessThanOrEqual(MAX_IMPORTED_CAPTIONS);
		expect(lines.summary.placed).toBeGreaterThan(8000);
	});

	test('cues on an empty timeline are bounded by the day, not by the file', () => {
		const out = place('1\n00:00:01,000 --> 00:00:03,000\nToday\n\n2\n30:00:00,000 --> 30:00:03,000\nA day and a bit on\n', {}, timelineOf([]));
		expect([out.summary.placed, out.summary.dropped_outside]).toEqual([1, 1]);
	});
});

describe('describeImport', () => {
	const base: CaptionImportSummary = {
		format: 'srt',
		cues: 10,
		placed: 10,
		captions: 14,
		skipped_lines: 0,
		dropped_outside: 0,
		dropped_short: 0,
		dropped_overlap: 0,
		replaced: 0
	};
	test('says what was too short', () => {
		expect(describeImport({ ...base, placed: 8, dropped_short: 2 })).toBe('Imported 14 captions from 8 of 10 cues (2 too short)');
	});
	test('names only what happened', () => {
		expect(describeImport(base)).toBe('Imported 14 captions from 10 of 10 cues');
		expect(describeImport({ ...base, captions: 1, placed: 1, cues: 1 })).toBe('Imported 1 caption from 1 of 1 cues');
		expect(describeImport({ ...base, placed: 6, dropped_outside: 3, dropped_overlap: 1, skipped_lines: 2, replaced: 5 })).toBe(
			'Imported 14 captions from 6 of 10 cues (3 outside the cut, 1 overlapping, 2 unreadable) · replaced 5'
		);
	});
});
