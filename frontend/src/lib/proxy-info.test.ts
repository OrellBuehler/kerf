import { describe, expect, test } from 'bun:test';
import { fmtBytes, previewSourceNote, proxyActions, proxyBadge, proxyFacts } from './proxy-info';
import type { Asset, ProxyStatus, Timeline } from './types';

const st = (state: ProxyStatus['state'], extra: Partial<ProxyStatus> = {}): ProxyStatus => ({ asset_id: 'a', state, ...extra });

describe('the badge on a bin row', () => {
	test('building says how far, with the time left once there is a guess', () => {
		const b = proxyBadge(st('building', { fraction: 0.426, eta_secs: 83 }))!;
		expect(b.text).toBe('proxy 43%');
		expect(b.tone).toBe('agent');
		expect(b.title).toContain('43%');
		expect(b.title).toContain('1:23 left');
		expect(b.title).toContain('decode the original');
		// No guess yet: no time claimed.
		expect(proxyBadge(st('building', { fraction: 0 }))!.title).not.toContain('left');
	});

	test('ready, queued, failed and off each have their own', () => {
		expect(proxyBadge(st('ready', { width: 1280, bytes: 38_400_000 }))).toMatchObject({ text: 'proxy', tone: 'success' });
		expect(proxyBadge(st('ready', { width: 1280, bytes: 38_400_000 }))!.title).toContain('1280 px · 36.6 MB');
		expect(proxyBadge(st('queued'))).toMatchObject({ text: 'proxy queued', tone: 'neutral' });
		const failed = proxyBadge(st('failed', { reason: 'could not generate preview proxy: Invalid data' }))!;
		expect(failed).toMatchObject({ text: 'proxy failed', tone: 'danger' });
		expect(failed.title).toContain('Invalid data');
		expect(proxyBadge(st('off', { reason: 'previews are set to always use the original' }))!.title).toContain('always use the original');
		expect(proxyBadge(st('missing'))!.text).toBe('no proxy');
	});

	test('a still or an audio file, and an asset with no status yet, wear nothing', () => {
		expect(proxyBadge(st('not_needed', { reason: 'a still image decodes in one step' }))).toBeNull();
		expect(proxyBadge(undefined)).toBeNull();
	});
});

describe('the context menu', () => {
	test('facts: progress, size on disk, and the reason for a failure', () => {
		expect(proxyFacts(st('building', { fraction: 0.5, eta_secs: 30 }))).toEqual([{ label: 'Proxy', value: 'building 50% · ~0:30 left' }]);
		expect(proxyFacts(st('ready', { width: 720, bytes: 2 * 1024 * 1024 }))).toEqual([{ label: 'Proxy', value: 'ready · 720 px · 2.0 MB' }]);
		const failed = proxyFacts(st('failed', { reason: 'x'.repeat(200) }));
		expect(failed.map((f) => f.label)).toEqual(['Proxy', 'Reason']);
		expect(failed[1].value.length).toBeLessThanOrEqual(56);
		expect(failed[1].title).toHaveLength(200);
		expect(proxyFacts(undefined)).toEqual([]);
	});

	test('rebuild and delete say what they will do, and why not', () => {
		expect(proxyActions(st('ready', { bytes: 10 })).rebuild.label).toBe('Rebuild proxy');
		expect(proxyActions(st('ready', { bytes: 10 })).remove).toMatchObject({ label: 'Delete proxy', disabled: false });
		expect(proxyActions(st('building')).rebuild.label).toBe('Restart proxy');
		expect(proxyActions(st('building')).remove).toMatchObject({ label: 'Cancel proxy', disabled: false });
		expect(proxyActions(st('missing')).rebuild.label).toBe('Build proxy');
		expect(proxyActions(st('missing')).remove).toMatchObject({ disabled: true, reason: 'there is no proxy to delete' });
		expect(proxyActions(st('failed')).remove.disabled).toBe(false);
		expect(proxyActions(st('failed')).rebuild.label).toBe('Retry proxy');
		expect(proxyActions(st('not_needed')).rebuild).toMatchObject({ disabled: true, reason: 'this file needs no proxy' });
	});

	test('sizes read as bytes', () => {
		expect(fmtBytes(500)).toBe('1 KB');
		expect(fmtBytes(3 * 1024 * 1024)).toBe('3.0 MB');
		expect(fmtBytes(1.5 * 1024 ** 3)).toBe('1.50 GB');
	});
});

describe('the status bar line: which file is this frame from', () => {
	const video = (id: string, name: string): Asset => ({
		id,
		path: `/m/${name}`,
		name,
		duration: 60,
		imported_at: '',
		streams: [{ index: 0, kind: 'video', codec: 'hevc', width: 5312, height: 2988, fps: 30 }]
	});
	const gopro = video('g', 'GOPR0042.MP4');
	const still: Asset = { ...video('s', 'title.png'), streams: [{ index: 0, kind: 'video', codec: 'png', width: 10, height: 10, image: true }] };
	const timeline = {
		tracks: [
			{ kind: 'video', clips: [{ asset_id: 'g', source_in: 0, source_out: 10, timeline_start: 0 }] },
			{ kind: 'audio', clips: [{ asset_id: 'g', source_in: 0, source_out: 10, timeline_start: 20 }] }
		]
	} as unknown as Timeline;
	const note = (statuses: Record<string, ProxyStatus>, source: 'auto' | 'original' | 'proxy_only', size: 0 | 720 | 1080 | 1280, t = 5) =>
		previewSourceNote(timeline, [gopro, still], statuses, source, size, t);

	test('a ready proxy is named with its width', () => {
		const n = note({ g: st('ready', { width: 1280 }) }, 'auto', 1280)!;
		expect(n.text).toBe('preview: proxy 1280 px');
		expect(n.tone).toBe('proxy');
	});

	test('until it is ready the original is decoded, and the line says how far the proxy is', () => {
		expect(note({ g: st('building', { fraction: 0.42 }) }, 'auto', 1280)!.text).toBe('preview: original · proxy 42%');
		expect(note({ g: st('queued') }, 'auto', 1280)!.text).toBe('preview: original · proxy queued');
		const failed = note({ g: st('failed', { reason: 'broken' }) }, 'auto', 1280)!;
		expect(failed.text).toBe('preview: original · proxy failed');
		expect(failed.title).toContain('GOPR0042.MP4');
		expect(failed.title).toContain('broken');
	});

	test('the settings override: always original, proxies off, proxy only', () => {
		expect(note({ g: st('ready', { width: 1280 }) }, 'original', 1280)!.text).toBe('preview: original · full resolution');
		expect(note({ g: st('off') }, 'auto', 0)!.text).toBe('preview: original · proxies off');
		const waiting = note({ g: st('building', { fraction: 0.42 }) }, 'proxy_only', 1280)!;
		expect(waiting.text).toBe('preview: waiting for proxy · 42%');
		expect(waiting.tone).toBe('waiting');
		// Ready under proxy-only is just the proxy.
		expect(note({ g: st('ready', { width: 1280 }) }, 'proxy_only', 1280)!.tone).toBe('proxy');
	});

	test('nothing to say over a gap, an audio clip or a still', () => {
		expect(note({ g: st('ready') }, 'auto', 1280, 15)).toBeNull();
		expect(note({ g: st('ready') }, 'auto', 1280, 25)).toBeNull();
		const onlyStill = { tracks: [{ kind: 'video', clips: [{ asset_id: 's', source_in: 0, source_out: 5, timeline_start: 0 }] }] } as unknown as Timeline;
		expect(previewSourceNote(onlyStill, [still], {}, 'auto', 1280, 1)).toBeNull();
	});
});
