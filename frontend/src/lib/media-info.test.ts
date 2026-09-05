import { describe, expect, test } from 'bun:test';
import { analysisFacts, fmtAspect, fmtDuration, fmtFps, mediaInfo, shortPath, specLine, thumbTime } from './media-info';
import type { Asset, AssetAnalysis } from './types';

const video: Asset = {
	id: 'a',
	path: '/home/me/footage/day1/interview.mp4',
	name: 'interview.mp4',
	duration: 754.2,
	imported_at: '2026-09-05T10:00:00Z',
	streams: [
		{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps: 29.97 },
		{ index: 1, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 }
	]
};

const music: Asset = {
	id: 'b',
	path: 'C:\\Users\\me\\Music\\bed.wav',
	name: 'bed.wav',
	duration: 95,
	imported_at: '2026-09-05T10:00:00Z',
	streams: [{ index: 0, kind: 'audio', codec: 'pcm_s16le', sample_rate: 44100, channels: 1 }]
};

const timeline = {
	tracks: [
		{ clips: [{ asset_id: 'a' }, { asset_id: 'a' }] },
		{ clips: [{ asset_id: 'b' }] }
	]
} as never;

describe('fmtDuration', () => {
	test('tenths under ten seconds, m:ss under an hour, h:mm:ss above', () => {
		expect(fmtDuration(3.25)).toBe('3.3s');
		expect(fmtDuration(754.2)).toBe('12:34');
		expect(fmtDuration(3725)).toBe('1:02:05');
		expect(fmtDuration(-1)).toBe('0.0s');
	});
});

describe('fmtFps / fmtAspect', () => {
	test('integers stay integers, NTSC keeps two decimals', () => {
		expect(fmtFps(25)).toBe('25 fps');
		expect(fmtFps(29.97002997)).toBe('29.97 fps');
	});
	test('common aspects are named, others reduce to n:1', () => {
		expect(fmtAspect(1920, 1080)).toBe('16:9');
		expect(fmtAspect(1080, 1920)).toBe('9:16');
		expect(fmtAspect(5760, 2880)).toBe('2:1');
		expect(fmtAspect(1000, 700)).toBe('1.43:1');
	});
});

describe('mediaInfo', () => {
	test('a video asset reads its video and audio streams', () => {
		const info = mediaInfo(video, timeline);
		expect(info.kind).toBe('video');
		expect(info.resolution).toBe('1920×1080');
		expect(info.fps).toBe('29.97 fps');
		expect(info.audio).toBe('48 kHz stereo aac');
		expect(info.aspect).toBe('16:9');
		expect(info.uses).toBe(2);
		expect(info.projection).toBeNull();
		expect(specLine(info)).toBe('1920×1080 · 29.97 fps · h264 · stereo');
	});
	test('an audio asset leads with its sample rate', () => {
		const info = mediaInfo(music, timeline);
		expect(info.kind).toBe('audio');
		expect(info.resolution).toBeNull();
		expect(specLine(info)).toBe('pcm_s16le · 44.1 kHz mono');
	});
	test('a still has no frame rate and a 360 source names its projection', () => {
		const still = { ...video, streams: [{ index: 0, kind: 'video', codec: 'png', width: 800, height: 600, image: true }] } as Asset;
		expect(mediaInfo(still, timeline).kind).toBe('image');
		expect(mediaInfo(still, timeline).fps).toBeNull();
		const sphere = {
			...video,
			source_paths: ['/a_00_.mp4', '/a_10_.mp4'],
			streams: [{ index: 0, kind: 'video', codec: 'hevc', width: 5760, height: 2880, fps: 30, projection: 'equirect' }]
		} as Asset;
		const info = mediaInfo(sphere, timeline);
		expect(info.projection).toBe('360 equirect');
		expect(info.stitched).toBe(true);
	});
});

describe('analysisFacts', () => {
	const analysis: AssetAnalysis = {
		asset_id: 'a',
		silence_segments: [
			{ start: 0, end: 2 },
			{ start: 10, end: 13 }
		],
		scene_changes: [5, 20, 40],
		transcript: [
			{ start: 0, end: 2, text: 'hello there world' },
			{ start: 2, end: 4, text: 'again' }
		],
		loudness: { integrated_lufs: -18.34, loudness_range: 6, true_peak_dbtp: -1.02, threshold_lufs: -28 },
		onsets: [],
		tempo: { bpm: 120.4, beats: [], confidence: 0.82 },
		audio_class: { class: 'speech', confidence: 0.91 }
	};
	test('phrases every populated field', () => {
		const facts = analysisFacts(analysis, 100);
		expect(facts.map((f) => f.label)).toEqual(['Audio', 'Loudness', 'Tempo', 'Silence', 'Scenes', 'Transcript']);
		expect(facts.find((f) => f.label === 'Loudness')?.value).toBe('-18.3 LUFS · peak -1.0 dBTP');
		expect(facts.find((f) => f.label === 'Silence')?.value).toBe('2 gaps · 5.0s · 5%');
		expect(facts.find((f) => f.label === 'Scenes')?.value).toBe('4 shots');
		expect(facts.find((f) => f.label === 'Transcript')?.value).toBe('2 lines · 4 words');
	});
	test('nothing analyzed is an empty list, and speech without lines says so', () => {
		expect(analysisFacts(null, 10)).toEqual([]);
		const quiet = { ...analysis, transcript: [] };
		expect(analysisFacts(quiet, 10).find((f) => f.label === 'Transcript')?.value).toBe('no speech');
		const song = { ...quiet, audio_class: { class: 'music', confidence: 0.9 } } as AssetAnalysis;
		expect(analysisFacts(song, 10).some((f) => f.label === 'Transcript')).toBe(false);
	});
});

describe('shortPath / thumbTime', () => {
	test('keeps the last two segments on either separator', () => {
		expect(shortPath('/home/me/footage/day1/interview.mp4')).toBe('…/day1/interview.mp4');
		expect(shortPath('C:\\Users\\me\\Music\\bed.wav')).toBe('…/Music/bed.wav');
		expect(shortPath('/a.mp4')).toBe('/a.mp4');
	});
	test('samples 10% in, except for very short media', () => {
		expect(thumbTime(100)).toBe(10);
		expect(thumbTime(1)).toBe(0);
	});
});
