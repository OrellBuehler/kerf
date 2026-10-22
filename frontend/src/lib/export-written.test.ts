import { describe, expect, test } from 'bun:test';
import { factsText, loudnessText, overText, writtenNotice } from './export-written';
import type { ExportReport, LevelReading } from './types';

const reading = (over: Partial<LevelReading> = {}): LevelReading => ({
	integrated_lufs: -14.2,
	loudness_range_lu: 6.1,
	short_term_max_lufs: -11.8,
	peak_dbfs: -1.9,
	true_peak_dbtp: -1.2,
	...over
});

/** The mp4 of the reported export, measured. */
const report = (over: Partial<ExportReport> = {}): ExportReport => ({
	path: '/renders/cut.mp4',
	size_bytes: 38_900_000,
	container: 'mov,mp4,m4a,3gp,3g2,mj2',
	duration_secs: 31.5,
	bit_rate: 10_356_000,
	video: { codec: 'h264', width: 1920, height: 1080, fps: 30, pix_fmt: 'yuv420p', bit_rate: 10_293_593 },
	audio: { codec: 'aac', sample_rate: 48_000, channels: 2, bit_rate: 317_375 },
	loudness: reading(),
	target: { lufs: -14, true_peak_dbtp: -1 },
	true_peak_over_db: null,
	problem: null,
	...over
});

describe('loudnessText', () => {
	test('reads the integrated loudness, the true peak and the range of the file', () => {
		expect(loudnessText(reading())).toBe('-14.2 LUFS · true peak -1.2 dBTP · LRA 6.1 LU');
	});

	test('leaves out what the meter could not read, and calls an empty reading silence', () => {
		expect(loudnessText(reading({ loudness_range_lu: null }))).toBe('-14.2 LUFS · true peak -1.2 dBTP');
		expect(loudnessText(reading({ integrated_lufs: null, true_peak_dbtp: null, loudness_range_lu: null }))).toBe('silent');
		expect(loudnessText(null)).toBeNull();
	});

	test('rounds a tie to the even digit, as the backend prints it', () => {
		expect(loudnessText(reading({ integrated_lufs: -14.25, true_peak_dbtp: null, loudness_range_lu: null }))).toBe('-14.2 LUFS');
	});
});

describe('overText', () => {
	test('spells out a true peak over the target ceiling', () => {
		const r = report({ loudness: reading({ true_peak_dbtp: -0.4 }), true_peak_over_db: 0.6 });
		expect(overText(r)).toBe('True peak -0.4 dBTP is 0.6 dB over the -1.0 dBTP ceiling');
	});

	test('says nothing when it is within the ceiling, or there is no target to judge by', () => {
		expect(overText(report())).toBeNull();
		expect(overText(report({ target: null, true_peak_over_db: 0.6 }))).toBeNull();
		expect(overText(report({ loudness: null, true_peak_over_db: 0.6 }))).toBeNull();
	});
});

describe('factsText', () => {
	test('names the file kind, the picture, the sound and the size', () => {
		expect(factsText(report())).toBe('mp4 · h264 1920×1080 30 fps · aac 48 kHz stereo 317 kb/s · 38.9 MB');
	});

	test('a sound-only file has no picture, and a picture-only one no sound', () => {
		const wav = report({
			path: '/r/take.wav',
			video: null,
			audio: { codec: 'pcm_s16le', sample_rate: 44_100, channels: 1, bit_rate: null },
			size_bytes: null
		});
		expect(factsText(wav)).toBe('wav · pcm_s16le 44.1 kHz mono');
		expect(factsText(report({ audio: null, path: '/r/clip' , container: 'matroska,webm', size_bytes: null }))).toBe(
			'matroska · h264 1920×1080 30 fps'
		);
	});
});

describe('writtenNotice', () => {
	test('one file: its loudness under the headline', () => {
		expect(writtenNotice([report()])).toEqual({
			over: false,
			description: '-14.2 LUFS · true peak -1.2 dBTP · LRA 6.1 LU'
		});
	});

	test('a true peak over the ceiling is a warning that says by how much', () => {
		const n = writtenNotice([report({ loudness: reading({ true_peak_dbtp: -0.4 }), true_peak_over_db: 0.6 })]);
		expect(n.over).toBe(true);
		expect(n.description).toBe('True peak -0.4 dBTP is 0.6 dB over the -1.0 dBTP ceiling (-14.2 LUFS)');
	});

	test('several files get a line each, named', () => {
		const n = writtenNotice([
			report({ path: '/r/cut-9x16.mp4' }),
			report({ path: '/r/cut-1x1.mp4', loudness: reading({ true_peak_dbtp: -0.2 }), true_peak_over_db: 0.8 })
		]);
		expect(n.over).toBe(true);
		expect(n.description?.split('\n')).toEqual([
			'cut-9x16.mp4: -14.2 LUFS · true peak -1.2 dBTP · LRA 6.1 LU',
			'cut-1x1.mp4: True peak -0.2 dBTP is 0.8 dB over the -1.0 dBTP ceiling (-14.2 LUFS)'
		]);
	});

	test('a measurement that failed says so without alarm; a file without sound adds nothing', () => {
		const failed = writtenNotice([report({ loudness: null, problem: 'the file could not be probed: no such file' })]);
		expect(failed).toEqual({ over: false, description: 'not measured: the file could not be probed: no such file' });
		expect(writtenNotice([report({ audio: null, loudness: null })])).toEqual({ over: false, description: undefined });
		expect(writtenNotice([])).toEqual({ over: false, description: undefined });
	});
});
