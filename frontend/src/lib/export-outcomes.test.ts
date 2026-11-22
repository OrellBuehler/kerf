import { describe, expect, test } from 'bun:test';
import { failed, fileFacts, folderOf, headline, isStop, KEEP_OUTCOMES, reasonLines, remember, succeeded, when, writtenFile } from './export-outcomes';
import type { ExportReport } from './types';

const report = (path: string, extra: Partial<ExportReport> = {}): ExportReport => ({
	path,
	size_bytes: 40_000_000,
	container: 'mov,mp4,m4a,3gp,3g2,mj2',
	duration_secs: 20.5,
	bit_rate: null,
	video: null,
	audio: null,
	loudness: null,
	target: null,
	true_peak_over_db: null,
	problem: null,
	...extra
});

describe('folderOf', () => {
	test('takes the folder off a path on either separator', () => {
		expect(folderOf('/home/me/Videos/cut.mp4')).toBe('/home/me/Videos');
		expect(folderOf('C:\\Users\\me\\cut.mp4')).toBe('C:\\Users\\me');
		expect(folderOf('cut.mp4')).toBe('');
	});
	test('a root keeps its separator', () => {
		expect(folderOf('/cut.mp4')).toBe('/');
		expect(folderOf('C:\\cut.mp4')).toBe('C:\\');
	});
});

describe('what a finished export keeps', () => {
	test('the file, its folder, its size and how long it runs', () => {
		const o = succeeded([report('/home/me/Videos/cut.mp4')], 1000, 1);
		expect(o).toMatchObject({ ok: true, id: 1, at: 1000, over: false, notes: null });
		if (!o.ok) throw new Error('unreachable');
		expect(o.files).toEqual([{ path: '/home/me/Videos/cut.mp4', name: 'cut.mp4', folder: '/home/me/Videos', bytes: 40_000_000, seconds: 20.5 }]);
	});

	test('one row per file of a multi-format export, in the order written', () => {
		const o = succeeded([report('/v/cut-16x9.mp4'), report('/v/cut-9x16.mp4')], 0, 2);
		if (!o.ok) throw new Error('unreachable');
		expect(o.files.map((f) => f.name)).toEqual(['cut-16x9.mp4', 'cut-9x16.mp4']);
		expect(headline(o)).toBe('Exported 2 files');
	});

	test('a single file is named', () => {
		expect(headline(succeeded([report('/v/cut.mp4')], 0, 3))).toBe('Exported cut.mp4');
	});

	test('a true peak over the ceiling is carried with the measurement', () => {
		const r = report('/v/cut.mp4', {
			loudness: { integrated_lufs: -13.2, true_peak_dbtp: -0.4, loudness_range_lu: 6.1 } as never,
			target: { lufs: -14, true_peak_dbtp: -1 },
			true_peak_over_db: 0.6
		});
		const o = succeeded([r], 0, 4);
		expect(o).toMatchObject({ ok: true, over: true });
		if (!o.ok) throw new Error('unreachable');
		expect(o.notes).toContain('0.6 dB over');
	});

	test('a size or length the probe could not read is left out of the facts', () => {
		const clock = (s: number) => `${Math.round(s)} s`;
		expect(fileFacts(writtenFile(report('/v/a.mp4')), clock)).toBe('38.1 MB · 21 s');
		expect(fileFacts(writtenFile(report('/v/a.mp4', { size_bytes: null })), clock)).toBe('21 s');
		expect(fileFacts(writtenFile(report('/v/a.mp4', { size_bytes: null, duration_secs: null })), clock)).toBe('');
	});
});

describe('what a failed export keeps', () => {
	test('the reason in a sentence, with the backend’s layer off', () => {
		const o = failed(new Error('media engine error: ffmpeg exited with status 1: Permission denied (os error 13)'), '/v/cut.mp4', 5, 5);
		expect(o).toEqual({
			id: 5,
			at: 5,
			ok: false,
			reason: 'FFmpeg exited with status 1: Permission denied (os error 13)',
			target: '/v/cut.mp4'
		});
		expect(headline(o)).toBe('Export failed');
	});

	test('a reason that is not an Error is kept as text, and none is an empty reason', () => {
		expect(failed('disk full', null, 0, 1)).toMatchObject({ ok: false, reason: 'Disk full' });
		expect(failed('', null, 0, 1)).toMatchObject({ ok: false, reason: '' });
	});

	test('a stop the user asked for is not one', () => {
		expect(isStop(new Error('export cancelled'))).toBe(true);
		expect(isStop(new Error('export cancelled; all 3 files were removed'))).toBe(true);
		expect(isStop(new Error('ffmpeg exited'))).toBe(false);
	});
});

describe('the session keeps the last few', () => {
	test('newest first, and the oldest falls off', () => {
		let list: ReturnType<typeof remember> = [];
		for (let i = 1; i <= KEEP_OUTCOMES + 2; i++) list = remember(list, failed('x', null, i, i));
		expect(list.length).toBe(KEEP_OUTCOMES);
		expect(list.map((o) => o.id)).toEqual([7, 6, 5, 4, 3]);
	});

	test('it does not change the list it was given', () => {
		const one = [failed('x', null, 1, 1)];
		remember(one, failed('y', null, 2, 2));
		expect(one.length).toBe(1);
	});
});

describe('when', () => {
	const clock = () => '14:02';
	test('is just now for the first minute, then minutes, then the time of day', () => {
		expect(when(1000, 1000, clock)).toBe('just now');
		expect(when(0, 59_000, clock)).toBe('just now');
		expect(when(0, 60_000, clock)).toBe('1 min ago');
		expect(when(0, 59 * 60_000, clock)).toBe('59 min ago');
		expect(when(0, 3600_000, clock)).toBe('14:02');
	});
	test('a clock that runs behind is just now, not negative', () => {
		expect(when(5000, 1000, clock)).toBe('just now');
	});
});

describe('a long reason', () => {
	const tail = ['FFmpeg exited with status 1:', '  Stream mapping:', '    Stream #0:0 -> #0:0 (h264 (native) -> h264 (libx264))', '  Error opening output file /v/cut.mp4.', 'Error opening output files: Permission denied'].join('\n');

	test('is a line, and what follows it', () => {
		expect(reasonLines(tail)).toEqual({
			first: 'FFmpeg exited with status 1',
			rest: '  Stream mapping:\n    Stream #0:0 -> #0:0 (h264 (native) -> h264 (libx264))\n  Error opening output file /v/cut.mp4.\nError opening output files: Permission denied'
		});
	});

	test('a one-line reason has nothing after it, and keeps its own colons inside', () => {
		expect(reasonLines('Permission denied (os error 13)')).toEqual({ first: 'Permission denied (os error 13)', rest: '' });
		expect(reasonLines('FFmpeg exited: status 1')).toEqual({ first: 'FFmpeg exited: status 1', rest: '' });
	});

	test('blank lines around it are not content, and no reason is empty', () => {
		expect(reasonLines('\n\n  Disk full  \n\n\nsecond\n\n')).toEqual({ first: 'Disk full', rest: 'second' });
		expect(reasonLines('')).toEqual({ first: '', rest: '' });
		expect(reasonLines('\n \n')).toEqual({ first: '', rest: '' });
	});

	test('a reason from Windows says the same', () => {
		expect(reasonLines('a\r\nb\r\nc')).toEqual({ first: 'a', rest: 'b\nc' });
	});

	test('is kept whole in the outcome: the dialog and the panel scroll it', () => {
		const o = failed(new Error('media engine error: ' + tail), '/v/cut.mp4', 0, 1);
		expect(o.ok === false && o.reason.split('\n').length).toBe(5);
	});
});
