import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';

// The three calls the library's browser leans on beyond the lists: the file facts it sorts by (`asset_files`), one
// asset played alone (`start_asset_playback`, the timeline's stream by another command) and a template drawn at any
// time (`template_frames`). The desktop half checks what is sent and what comes back; the harness half what stands in
// for them under bun, where there is no Tauri.

const calls: { cmd: string; args?: Record<string, unknown> }[] = [];
const channels: { onmessage: (message: unknown) => void }[] = [];
const session = 3;

const realCore = { ...(await import('@tauri-apps/api/core')) };

mock.module('@tauri-apps/api/core', () => ({
	invoke: async (cmd: string, args?: Record<string, unknown>) => {
		calls.push({ cmd, args });
		if (cmd === 'playback_session') return session;
		if (cmd === 'asset_files') return [{ asset_id: 'a', size_bytes: 12 }];
		if (cmd === 'template_frames') return ['data:image/png;base64,AA', 'data:image/png;base64,BB'];
		return null;
	},
	Channel: class {
		onmessage: (message: unknown) => void = () => {};
		constructor() {
			channels.push(this);
		}
	}
}));

const { assetFiles, listAssets, listTemplates, startAssetPlayback, startPlayback, templateFrames, templatePoster } =
	await import('./api');
const g = globalThis as unknown as { window?: unknown };

const started = async () => {
	for (let i = 0; i < 20 && channels.length === 0; i++) await new Promise((r) => setTimeout(r, 1));
	return channels[0];
};

beforeEach(() => {
	calls.length = 0;
	channels.length = 0;
});
afterEach(() => {
	delete g.window;
});
afterAll(() => {
	delete g.window;
	mock.module('@tauri-apps/api/core', () => realCore);
});

describe('library calls (desktop)', () => {
	beforeEach(() => {
		g.window = { __TAURI_INTERNALS__: {} };
	});

	test('the file facts are the backend’s, asked for with no arguments', async () => {
		expect(await assetFiles()).toEqual([{ asset_id: 'a', size_bytes: 12 }]);
		expect(calls).toEqual([{ cmd: 'asset_files', args: undefined }]);
	});

	test('an asset plays under the same id protocol as the timeline and stops by its id', async () => {
		const stopTimeline = startPlayback(0, { fps: null, width: null }, () => {});
		await started();
		const stop = startAssetPlayback('asset-1', 4.5, { fps: 24, width: 960 }, () => {});
		for (let i = 0; i < 20 && channels.length < 2; i++) await new Promise((r) => setTimeout(r, 1));
		const timeline = calls.find((c) => c.cmd === 'start_playback')!.args!;
		const asset = calls.find((c) => c.cmd === 'start_asset_playback')!.args!;
		expect(asset).toMatchObject({ assetId: 'asset-1', start: 4.5, fps: 24, maxWidth: 960 });
		expect(asset.playbackId).toBe((timeline.playbackId as number) + 1);
		stop();
		await new Promise((r) => setTimeout(r, 5));
		expect(calls.filter((c) => c.cmd === 'stop_playback')).toEqual([
			{ cmd: 'stop_playback', args: { playbackId: asset.playbackId } }
		]);
		stopTimeline();
	});

	test('a frame of an asset is a frame, and a refusal reaches onError', async () => {
		const times: number[] = [];
		const errors: string[] = [];
		const stop = startAssetPlayback('asset-1', 0, { fps: null, width: null }, (f) => times.push(f.time), (m) => errors.push(m));
		const channel = await started();
		const bytes = new Uint8Array(8 + 4);
		new DataView(bytes.buffer).setFloat64(0, 2.25, true);
		bytes.set([0xff, 0xd8, 0xff, 0xd9], 8);
		channel.onmessage(bytes.buffer);
		expect(times).toEqual([2.25]);
		expect(errors).toEqual([]);
		stop();
	});

	test('template frames send the id, the times and the width, and come back in order', async () => {
		expect(await templateFrames('t1', [0, 1.5])).toEqual(['data:image/png;base64,AA', 'data:image/png;base64,BB']);
		await templateFrames('t1', [2], 320);
		expect(calls).toEqual([
			{ cmd: 'template_frames', args: { templateId: 't1', times: [0, 1.5], maxWidth: 480 } },
			{ cmd: 'template_frames', args: { templateId: 't1', times: [2], maxWidth: 320 } }
		]);
	});
});

describe('library calls (browser harness)', () => {
	test('every asset has a size and dates, the same ones each time and not all alike', async () => {
		const assets = await listAssets();
		const files = await assetFiles();
		expect(files.map((f) => f.asset_id)).toEqual(assets.map((a) => a.id));
		for (const f of files) {
			expect(f.size_bytes).toBeGreaterThan(0);
			expect(Number.isNaN(Date.parse(f.modified!))).toBe(false);
			expect(Date.parse(f.created!)).toBeLessThanOrEqual(Date.parse(f.modified!));
		}
		expect(await assetFiles()).toEqual(files);
		expect(new Set(files.map((f) => f.size_bytes)).size).toBe(files.length);
		expect(new Set(files.map((f) => f.modified)).size).toBe(files.length);
		const video = files[0].size_bytes!;
		expect(video).toBeGreaterThan(files[2].size_bytes!);
	});

	test('a sound alone has no picture to play, and an unknown asset is an error, both through onError', async () => {
		const assets = await listAssets();
		const music = assets.find((a) => a.streams.every((s) => s.kind === 'audio'))!;
		const errors: string[] = [];
		startAssetPlayback(music.id, 0, { fps: null, width: null }, () => {}, (m) => errors.push(m))();
		startAssetPlayback('nope', 0, { fps: null, width: null }, () => {}, (m) => errors.push(m))();
		await Promise.resolve();
		expect(errors).toEqual([`${music.name} has no picture to play`, 'no asset nope']);
	});

	test('template frames are the poster once per time, and at most 64 at once', async () => {
		const list = await listTemplates();
		const id = list.templates.find((t) => t.has_poster)!.id;
		const poster = (await templatePoster(id))!;
		expect(await templateFrames(id, [0, 1, 2])).toEqual([poster, poster, poster]);
		expect(await templateFrames(id, [])).toEqual([]);
		await expect(templateFrames(id, new Array(65).fill(0))).rejects.toThrow('at most 64');
		await expect(templateFrames('00000000-0000-0000-0000-000000000000', [0])).rejects.toThrow();
	});
});
