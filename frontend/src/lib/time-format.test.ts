import { describe, expect, test } from 'bun:test';
import {
	DEFAULT_TIME_MODE,
	formatField,
	formatRange,
	formatSecondsField,
	formatTime,
	isTimeMode,
	parseFrameTime,
	parseTime,
	roundToFrame
} from './time-format';
import { formatTimecode } from './timecode';
import { clock } from './clock';

describe('a readout', () => {
	test('is frame timecode by default', () => {
		expect(DEFAULT_TIME_MODE).toBe('timecode');
		expect(formatTime(12.5, 30)).toBe('00:00:12:15');
		expect(formatTime(12.5, 30)).toBe(formatTimecode(12.5, 30));
		expect(formatTime(0, 24)).toBe('00:00:00:00');
		expect(formatTime(3725.5, 25)).toBe('01:02:05:12');
	});

	test('seconds mode is the m:ss.d the review card prints', () => {
		expect(formatTime(12.5, 30, 'seconds')).toBe('0:12.5');
		expect(formatTime(12.5, 30, 'seconds')).toBe(clock(12.5));
		expect(formatTime(3725.5, 25, 'seconds')).toBe('1:02:05.5');
	});

	test('frames: false cuts the timecode at the second (a ruler label)', () => {
		expect(formatTime(75.5, 30, 'timecode', { frames: false })).toBe('00:01:15');
	});

	test('a range is two readouts', () => {
		expect(formatRange(0, 8, 30)).toBe('00:00:00:00 – 00:00:08:00');
		expect(formatRange(0, 8, 30, 'seconds')).toBe('0:00.0 – 0:08.0');
	});

	test('nonsense is zero, and a missing rate is 30', () => {
		for (const t of [-1, NaN, Infinity]) expect(formatTime(t, 30)).toBe('00:00:00:00');
		expect(formatTime(1, NaN)).toBe('00:00:01:00');
		expect(formatTime(1, 0)).toBe('00:00:01:00');
	});

	test('modes are known', () => {
		expect(isTimeMode('timecode')).toBe(true);
		expect(isTimeMode('seconds')).toBe(true);
		expect(isTimeMode('frames')).toBe(false);
		expect(isTimeMode(undefined)).toBe(false);
	});
});

describe('a field shows what can be typed back, on a frame', () => {
	test('no long tail: 4.633333 is the frame it is', () => {
		expect(formatSecondsField(139 / 30, 30)).toBe('4.633');
		expect(formatSecondsField(4.633333, 30)).toBe('4.633');
		expect(formatSecondsField(12.5, 30)).toBe('12.5');
		expect(formatSecondsField(0, 30)).toBe('0');
		expect(formatSecondsField(8, 24)).toBe('8');
	});

	test('a time between frames is put on the nearest one', () => {
		expect(roundToFrame(0.016, 30)).toBe(0);
		expect(roundToFrame(0.017, 30)).toBeCloseTo(1 / 30, 9);
		expect(roundToFrame(4.6, 30)).toBeCloseTo(4.6, 9);
		expect(roundToFrame(-3, 30)).toBe(0);
		expect(roundToFrame(NaN, 30)).toBe(0);
	});

	test('the field of each mode', () => {
		expect(formatField(139 / 30, 30)).toBe('00:00:04:19');
		expect(formatField(139 / 30, 30, 'seconds')).toBe('4.633');
	});
});

describe('what a typed time means', () => {
	test('seconds, with or without the unit', () => {
		expect(parseTime('12.5', 30)).toBe(12.5);
		expect(parseTime(' 12.5s ', 30)).toBe(12.5);
		expect(parseTime('.5', 30)).toBe(0.5);
		expect(parseTime('0', 30)).toBe(0);
		expect(parseTime('90', 30)).toBe(90);
	});

	test('frames', () => {
		expect(parseTime('375f', 30)).toBe(12.5);
		expect(parseTime('48F', 24)).toBe(2);
	});

	test('timecode: the last field is frames', () => {
		expect(parseTime('00:00:12:15', 30)).toBe(12.5);
		expect(parseTime('00:12:15', 30)).toBe(12.5);
		expect(parseTime('12:15', 30)).toBe(12.5);
		expect(parseTime('01:02:03:04', 25)).toBeCloseTo(3723 + 4 / 25, 9);
		expect(parseTime('00;00;12;15', 30)).toBe(12.5);
	});

	test('clock: a colon time with a decimal point is minutes and seconds', () => {
		expect(parseTime('0:12.5', 30)).toBe(12.5);
		expect(parseTime('1:02.5', 30)).toBe(62.5);
		expect(parseTime('1:02:03.25', 30)).toBe(3723.25);
	});

	test('what is not a time is null', () => {
		for (const bad of ['', '  ', 'abc', '-3', '12.5.5', '1:2:3:4:5', ':12', '12:', '00:00:01:45', '75:10', '00:61:00:00', '1.5:3', 'f', '12ff', '1e3'])
			expect([bad, parseTime(bad, 30)]).toEqual([bad, null]);
	});

	test('what a readout prints reads back as the same frame, at the usual rates', () => {
		for (const fps of [24, 25, 30, 50, 60]) {
			for (const frame of [0, 1, fps - 1, fps, 139, 1000, 54321]) {
				const t = frame / fps;
				expect([fps, frame, parseTime(formatTimecode(t, fps), fps)]).toEqual([fps, frame, expect.closeTo(t, 6)]);
				expect(parseTime(formatField(t, fps, 'seconds'), fps)).toBeCloseTo(t, 3);
			}
		}
	});

	test('at a fractional rate timecode counts whole nominal seconds, and reads back as the frame it names', () => {
		const fps = 29.97;
		for (const frame of [0, 1, 29, 30, 31, 1000]) {
			const t = frame / fps;
			expect(parseFrameTime(formatTimecode(t, fps), fps)).toBeCloseTo(t, 6);
		}
	});

	test('a field that is a frame position puts what was typed on a frame', () => {
		expect(parseFrameTime('4.6', 30)).toBeCloseTo(4.6, 9);
		expect(parseFrameTime('4.62', 30)).toBeCloseTo(139 / 30, 9);
		expect(parseFrameTime('nope', 30)).toBeNull();
	});
});
