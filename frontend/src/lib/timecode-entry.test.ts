import { describe, expect, test } from 'bun:test';
import {
	MAX_TIMECODE_FRAME,
	formatFrames,
	formatTimecode,
	nominalTimecodeRate,
	parseTimecode,
	timecodeFields,
	timecodeFrameAt,
	timecodeFrames,
	timecodeRate
} from './timecode';

// The twins of kerf-core's `timecode.rs` tests, case for case.
const NTSC = 30000 / 1001;
const NTSC60 = 60000 / 1001;

describe('timecode entry and drop-frame (mirror of timecode.rs)', () => {
	test('drop-frame is 29.97 and 59.94 however the rate is written', () => {
		for (const fps of [NTSC, 29.97, 29.97002997, NTSC60, 59.94]) {
			expect([fps, timecodeRate(fps, true).drop > 0]).toEqual([fps, true]);
			expect([fps, timecodeRate(fps, false).drop > 0]).toEqual([fps, false]);
		}
		for (const fps of [23.976, 24, 25, 30, 50, 60, 119.88, 0, -29.97, NaN, Infinity]) {
			expect([fps, timecodeRate(fps, true).drop > 0]).toEqual([fps, false]);
		}
		expect(timecodeRate(NTSC, true)).toEqual({ base: 30, drop: 2 });
		expect(timecodeRate(59.94, true)).toEqual({ base: 60, drop: 4 });
		expect(timecodeRate(23.976, true)).toEqual({ base: 24, drop: 0 });
		expect(nominalTimecodeRate(NaN)).toBe(30);
		expect(nominalTimecodeRate(1e300)).toBe(1000);
	});

	test('drop-frame labels', () => {
		const r = timecodeRate(NTSC, true);
		for (const [frame, label] of [
			[0, '00;00;00;00'],
			[1799, '00;00;59;29'],
			[1800, '00;01;00;02'],
			[3598, '00;02;00;02'],
			[17981, '00;09;59;29'],
			[17982, '00;10;00;00'],
			[17983, '00;10;00;01'],
			[107_892, '01;00;00;00']
		] as const) {
			expect(formatFrames(r, frame)).toBe(label);
		}
		expect(formatFrames(timecodeRate(NTSC, false), 1800)).toBe('00:01:00:00');
		expect(formatFrames(timecodeRate(NTSC60, true), 3600)).toBe('00;01;00;04');
		expect(formatFrames(timecodeRate(25, true), 90_000)).toBe('01:00:00:00');
	});

	test('every drop-frame label reads back as its frame', () => {
		const bad: unknown[] = [];
		for (const fps of [NTSC, NTSC60]) {
			const r = timecodeRate(fps, true);
			for (const [from, to] of [
				[0, 40_000],
				[1_000_000, 1_002_000]
			]) {
				for (let frame = from; frame < to; frame++) {
					const [h, m, s, f] = timecodeFields(r, frame);
					if (timecodeFrames(r, h, m, s, f) !== frame || parseTimecode(formatFrames(r, frame), r, 0) !== frame) bad.push([fps, frame]);
				}
			}
		}
		expect(bad).toEqual([]);
	});

	test('a dropped label is the first frame of its minute', () => {
		const r = timecodeRate(NTSC, true);
		expect(parseTimecode('00;01;00;00', r, 0)).toBe(1800);
		expect(parseTimecode('00;01;00;01', r, 0)).toBe(1800);
		expect(parseTimecode('00;10;00;00', r, 0)).toBe(17982);
	});

	test('seconds format at the frame they fall in', () => {
		expect(formatTimecode(10, 25)).toBe('00:00:10:00');
		expect(formatTimecode(1, NTSC)).toBe('00;00;00;29');
		expect(formatTimecode(1.001, NTSC)).toBe('00;00;01;00');
		expect(formatTimecode(60.06, NTSC)).toBe('00;01;00;02');
		expect(formatTimecode(-3, 24)).toBe('00:00:00:00');
		expect(formatTimecode(NaN, 24)).toBe('00:00:00:00');
		expect(formatTimecode(2, NaN)).toBe('00:00:02:00');
		expect(timecodeFrameAt(1e300, 30)).toBe(MAX_TIMECODE_FRAME);
		expect(timecodeFrameAt(2 / 3, 3)).toBe(2);
	});

	test('typed forms', () => {
		const r = timecodeRate(25, true);
		for (const [text, current, frame] of [
			['00:00:10:00', 0, 250],
			['00;00;10;00', 0, 250],
			['0.0.10.0', 0, 250],
			['0,0,10,0', 0, 250],
			['1000', 0, 250],
			['10000', 0, 25 * 60],
			['1', 0, 1],
			['1.00', 0, 25],
			['1:', 0, 25],
			[':5', 0, 5],
			['  12:00  ', 0, 300],
			['90', 0, 90],
			['1:90', 0, 115],
			['+15', 100, 115],
			['-15', 100, 85],
			['-1.00', 100, 75],
			['+1:00:00', 0, 1500],
			['+1:00:00:00', 0, 90_000],
			['-500', 100, 0],
			['01020304', 0, (3600 + 2 * 60 + 3) * 25 + 4]
		] as const) {
			expect([text, parseTimecode(text, r, current)]).toEqual([text, frame]);
		}
		for (const text of [
			'',
			' ',
			'+',
			'-',
			'abc',
			'1:2:3:4:5',
			'123456789',
			'1:a',
			'1 2',
			'0x10',
			'+0x1',
			'1e3',
			'１２',
			'1:+5',
			'--1',
			'1234567890:0',
			'+1234567890',
			':',
			'.',
			'::',
			':;.,',
			'+:',
			'-.',
			'+::'
		]) {
			expect([text, parseTimecode(text, r, 0)]).toEqual([text, null]);
		}
	});

	test('typed drop-frame', () => {
		const r = timecodeRate(NTSC, true);
		expect(parseTimecode('00;01;00;02', r, 0)).toBe(1800);
		expect(parseTimecode('00:01:00:02', r, 0)).toBe(1800);
		expect(parseTimecode('1000200', r, 0)).toBe(107_952);
		expect(parseTimecode('+1.00', r, 10)).toBe(40);
		// A field past its range carries first, and the label it makes is read as drop-frame.
		expect(parseTimecode('0:0:0:45', r, 0)).toBe(45);
		expect(parseTimecode('0:0:60:0', r, 0)).toBe(1800);
		expect(parseTimecode('0:0:0:1800', r, 0)).toBe(1800);
		expect(parseTimecode('0:0:59:45', r, 0)).toBe(parseTimecode('00;01;00;15', r, 0));
		expect(parseTimecode('0:70:0:0', r, 0)).toBe(parseTimecode('1:10:0:0', r, 0));
		expect(parseTimecode('0:70:0:0', r, 0)).toBe(125_874);
	});

	test('huge entries are held, not wrapped', () => {
		const r = timecodeRate(NTSC, true);
		expect(parseTimecode('999999999:59:59:29', r, 0)).toBe(MAX_TIMECODE_FRAME);
		expect(parseTimecode('+999999999', r, MAX_TIMECODE_FRAME)).toBe(MAX_TIMECODE_FRAME);
		expect(parseTimecode('-999999999', r, Number.MIN_SAFE_INTEGER)).toBe(0);
		expect(formatFrames(r, Number.MAX_SAFE_INTEGER)).toBe(formatFrames(r, MAX_TIMECODE_FRAME));
		expect(parseTimecode('999999999:999999999:999999999:999999999', timecodeRate(1e300, true), 0)).toBe(MAX_TIMECODE_FRAME);
	});
});
