import { describe, expect, test } from 'bun:test';
import { assetFacts } from './asset-facts';
import { mediaInfo } from './media-info';
import type { Asset, AnalysisStatus, AssetAnalysis, ProxyStatus, Timeline } from './types';

const asset = (extra: Partial<Asset> = {}): Asset =>
	({
		id: 'a',
		path: '/home/user/footage/day1/interview.mp4',
		name: 'interview.mp4',
		duration: 120,
		streams: [
			{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps: 30 },
			{ index: 1, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 }
		],
		imported_at: '2026-10-01T12:00:00Z',
		...extra
	}) as Asset;

const timeline = (uses: number): Pick<Timeline, 'tracks'> =>
	({ tracks: [{ kind: 'video', clips: Array.from({ length: uses }, () => ({ asset_id: 'a' })) }] }) as unknown as Pick<Timeline, 'tracks'>;

const run = (a: Asset, over: Partial<Parameters<typeof assetFacts>[0]> = {}, uses = 2) =>
	assetFacts({ asset: a, info: mediaInfo(a, timeline(uses)), analysis: null, status: null, analyzing: false, proxy: null, ...over });

const status = (states: Record<string, 'done' | 'failed' | 'not_run'>): AnalysisStatus => ({
	asset_id: 'a',
	kinds: (['silence', 'scenes', 'loudness', 'rhythm', 'transcript'] as const).map((kind) => ({
		kind,
		state: states[kind] ?? 'not_run',
		...(states[kind] === 'failed' ? { reason: 'ffmpeg exited with 1' } : {})
	}))
});

describe('the facts of an asset', () => {
	test('the file: length, frame, rate, codec, sound, where it is used', () => {
		const { media } = run(asset());
		expect(media.map((f) => f.label)).toEqual(['Duration', 'Frame', 'Rate', 'Codec', 'Audio', 'Used in', 'Imported', 'Path']);
		expect(media.find((f) => f.label === 'Frame')!.value).toBe('1920×1080 · 16:9');
		expect(media.find((f) => f.label === 'Used in')!.value).toBe('2 clips');
		expect(run(asset(), {}, 1).media.find((f) => f.label === 'Used in')!.value).toBe('1 clip');
		expect(run(asset(), {}, 0).media.find((f) => f.label === 'Used in')!.value).toBe('0 clips');
	});

	test('the path is the last two segments, with the whole of it in the tooltip', () => {
		const path = run(asset()).media.find((f) => f.label === 'Path')!;
		expect(path.value).toBe('…/day1/interview.mp4');
		expect(path.title).toBe('/home/user/footage/day1/interview.mp4');
	});

	test('a song has no frame, rate or projection to list', () => {
		const song = asset({ streams: [{ index: 0, kind: 'audio', codec: 'mp3', sample_rate: 44100, channels: 2 }] as Asset['streams'] });
		const labels = run(song).media.map((f) => f.label);
		expect(labels).not.toContain('Frame');
		expect(labels).not.toContain('Rate');
		expect(labels).not.toContain('Projection');
	});

	test('360 footage, a voiceover and a stitched pair say so', () => {
		const a = asset({
			streams: [{ index: 0, kind: 'video', codec: 'h264', width: 3840, height: 1920, fps: 30, projection: 'equirect' }] as Asset['streams'],
			source_paths: ['/x/lens-a.insv', '/x/lens-b.insv']
		});
		const labels = run(a).media.map((f) => f.label);
		expect(labels).toContain('Projection');
		expect(run(a).media.find((f) => f.label === 'Stitched')).toMatchObject({ value: '2 lens files', title: '/x/lens-a.insv\n/x/lens-b.insv' });
	});

	test('an unparsable import date is left out, not printed as Invalid Date', () => {
		expect(run(asset({ imported_at: 'not a date' })).media.map((f) => f.label)).not.toContain('Imported');
	});
});

describe('the analysis and the proxy', () => {
	test('how much has run is the summary; the rows are the failures with their reasons, then what was found', () => {
		const found = { silence_segments: [{ start: 1, end: 3 }], scene_changes: [], transcript: [], onsets: [], tempo: null, loudness: null, audio_class: null } as unknown as AssetAnalysis;
		const { analyzed, analysis } = run(asset(), { status: status({ silence: 'done', loudness: 'failed' }), analysis: found });
		expect(analyzed).toBe('1 of 5');
		expect(analysis[0]).toEqual({ label: 'Loudness', value: 'failed', title: 'ffmpeg exited with 1' });
		expect(analysis.map((f) => f.label)).toContain('Silence');
		expect(analysis.map((f) => f.label)).not.toContain('Analyzed');
	});

	test('a pass that is running reads running', () => {
		expect(run(asset(), { analyzing: true }).analyzed).toBe('running…');
		expect(run(asset()).analyzed).toBe('none');
	});

	test('the proxy rows are the bin menu’s own', () => {
		const ready: ProxyStatus = { asset_id: 'a', state: 'ready', width: 1280, bytes: 40 * 1024 * 1024 } as ProxyStatus;
		expect(run(asset(), { proxy: ready }).proxy).toEqual([{ label: 'Proxy', value: 'ready · 1280 px · 40.0 MB' }]);
		expect(run(asset()).proxy).toEqual([]);
	});
});
