import { describe, expect, test } from 'bun:test';
import { exportPlan, filesLine, hasAudioStream, plannedExport, shapesWritten } from './export-plan';
import { applyPreset, initialExport, presetsFor } from './export-presets';
import { frameKey } from './export-draft';
import type { Asset, Clip, Delivery, Timeline, Track } from './types';

const clip = (asset_id: string): Clip => ({ id: `c-${asset_id}`, asset_id, timeline_start: 0, source_in: 0, source_out: 4 }) as Clip;
const track = (kind: 'video' | 'audio', clips: Clip[]): Track => ({ id: kind, kind, name: kind, clips }) as Track;
const cut = (tracks: Track[]): Timeline => ({ tracks }) as Timeline;
const asset = (id: string, ...kinds: string[]): Pick<Asset, 'id' | 'streams'> => ({ id, streams: kinds.map((kind) => ({ kind })) }) as never;

describe('which files an export writes', () => {
	const picture = cut([track('video', [clip('a')])]);

	test('one per ticked shape for a video export of a cut with a picture, in picker order', () => {
		const w = shapesWritten({ container: 'mp4' }, picture, ['square', 'vertical']);
		expect(w.map((f) => `${f.width}x${f.height}`)).toEqual(['1080x1920', '1080x1080']);
	});

	test('none for an audio container, whatever was ticked', () => {
		expect(shapesWritten({ container: 'mp3' }, picture, ['vertical'])).toEqual([]);
	});

	test('none for a cut with no video clip', () => {
		expect(shapesWritten({ container: 'mp4' }, cut([track('audio', [clip('m')])]), ['vertical'])).toEqual([]);
		expect(shapesWritten({ container: 'mp4' }, cut([track('video', [])]), ['vertical'])).toEqual([]);
	});

	test('nothing ticked is the single export', () => {
		expect(shapesWritten({ container: 'mp4' }, picture, [])).toEqual([]);
	});
});

describe('does the cut have sound', () => {
	const assets = [asset('a', 'video', 'audio'), asset('b', 'video'), asset('m', 'audio')];
	test('any clip whose asset has an audio stream', () => {
		expect(hasAudioStream(cut([track('video', [clip('a')])]), assets)).toBe(true);
		expect(hasAudioStream(cut([track('audio', [clip('m')])]), assets)).toBe(true);
	});
	test('footage without sound, an unknown asset and an empty cut have none', () => {
		expect(hasAudioStream(cut([track('video', [clip('b')])]), assets)).toBe(false);
		expect(hasAudioStream(cut([track('video', [clip('zzz')])]), assets)).toBe(false);
		expect(hasAudioStream(cut([]), assets)).toBe(false);
	});
});

describe('what the dialog would open on', () => {
	const vertical: Delivery = { width: 1080, height: 1920, fit: 'cover' };

	test('the frame’s own preset with no draft', () => {
		expect(plannedExport(null, vertical).id).toBe(initialExport(vertical).id);
		expect(plannedExport(null, null).id).toBe('web_1080p');
	});

	test('what it was last closed on, while the project is still cut for that frame', () => {
		const draft = { opts: applyPreset('web_720p'), preset: 'web_720p', customRes: false, frame: frameKey(vertical) };
		expect(plannedExport(draft, vertical).id).toBe('web_720p');
	});

	test('not a draft made for another frame', () => {
		const draft = { opts: applyPreset('web_720p'), preset: 'web_720p', customRes: false, frame: frameKey(null) };
		expect(plannedExport(draft, vertical).id).toBe(initialExport(vertical).id);
	});
});

describe('the plan', () => {
	const presets = presetsFor([]);
	const base = { id: 'web_1080p', opts: applyPreset('web_1080p'), presets, hasVideo: true, hasAudio: true };

	test('names the preset and counts one file with no shape', () => {
		const p = exportPlan({ ...base, shapes: [] });
		expect(p.preset).toBe('Web 1080p');
		expect(p.files).toBe(1);
		expect(p.summary).toContain('MP4');
	});

	test('counts a file per shape', () => {
		const shapes: Delivery[] = [
			{ width: 1080, height: 1920, fit: 'cover' },
			{ width: 1080, height: 1080, fit: 'cover' }
		];
		expect(exportPlan({ ...base, shapes }).files).toBe(2);
	});

	test('a preset it does not know is custom settings', () => {
		expect(exportPlan({ ...base, id: 'custom', shapes: [] }).preset).toBe('Custom settings');
	});

	test('files read as words', () => {
		expect(filesLine(1)).toBe('1 file');
		expect(filesLine(3)).toBe('3 files');
	});
});
