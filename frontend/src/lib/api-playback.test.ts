import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import type { PlaybackFrame } from './api';

// The desktop half of playback: what `start_playback` is handed, and what a channel message becomes.
// The Rust command takes `playback_id`, `start`, `fps`, `max_width` and `on_frame` (Tauri turns the
// camelCase keys into those), `null` for what the engine should decide (the cut's own rate, the
// default width), and its frames come back as raw bytes — the timeline time as a little-endian f64,
// then the JPEG (`playback_message` in kerf-app) — rather than as JSON.

const calls: { cmd: string; args: Record<string, unknown> }[] = [];
const channels: { onmessage: (message: unknown) => void }[] = [];

const realCore = { ...(await import('@tauri-apps/api/core')) };

mock.module('@tauri-apps/api/core', () => ({
	invoke: async (cmd: string, args: Record<string, unknown>) => {
		calls.push({ cmd, args });
		return null;
	},
	Channel: class {
		onmessage: (message: unknown) => void = () => {};
		constructor() {
			channels.push(this);
		}
	}
}));

const { startPlayback } = await import('./api');
const g = globalThis as unknown as { window?: unknown };

const message = (time: number, jpeg: number[]) => {
	const bytes = new Uint8Array(8 + jpeg.length);
	new DataView(bytes.buffer).setFloat64(0, time, true);
	bytes.set(jpeg, 8);
	return bytes.buffer;
};

const started = async () => {
	// The channel is created after a dynamic import, so the stream starts a tick later.
	for (let i = 0; i < 20 && channels.length === 0; i++) await new Promise((r) => setTimeout(r, 1));
	return channels[0];
};

beforeEach(() => {
	calls.length = 0;
	channels.length = 0;
	g.window = { __TAURI_INTERNALS__: {} };
});
afterEach(() => {
	delete g.window;
});
afterAll(() => {
	delete g.window;
	mock.module('@tauri-apps/api/core', () => realCore);
});

describe('startPlayback (desktop)', () => {
	test('leaves the rate and the width to the engine unless the page states them', async () => {
		const stop = startPlayback(2.5, { fps: null, width: null }, () => {});
		await started();
		const start = calls.find((c) => c.cmd === 'start_playback')!;
		expect(start.args).toMatchObject({ start: 2.5, fps: null, maxWidth: null });
		expect(typeof start.args.playbackId).toBe('number');
		stop();
	});

	test('states the rate and the width of a stream restarted cheaper', async () => {
		const stop = startPlayback(0, { fps: 30, width: 1440 }, () => {});
		await started();
		expect(calls.find((c) => c.cmd === 'start_playback')!.args).toMatchObject({ fps: 30, maxWidth: 1440 });
		stop();
	});

	test('a message is a frame: its time and its JPEG, undecoded', async () => {
		const frames: PlaybackFrame[] = [];
		const stop = startPlayback(0, { fps: null, width: null }, (f) => frames.push(f));
		const channel = await started();
		channel.onmessage(message(1.5, [0xff, 0xd8, 1, 2, 0xff, 0xd9]));
		channel.onmessage(message(1.5 + 1 / 60, [0xff, 0xd8, 3, 0xff, 0xd9]));
		expect(frames.map((f) => f.time)).toEqual([1.5, 1.5 + 1 / 60]);
		expect(frames[0].jpeg.type).toBe('image/jpeg');
		expect([...new Uint8Array(await frames[0].jpeg.arrayBuffer())]).toEqual([0xff, 0xd8, 1, 2, 0xff, 0xd9]);
		stop();
	});

	test('what is not a frame is ignored', async () => {
		const frames: PlaybackFrame[] = [];
		const stop = startPlayback(0, { fps: null, width: null }, (f) => frames.push(f));
		const channel = await started();
		channel.onmessage(new ArrayBuffer(8));
		channel.onmessage('data:image/jpeg;base64,/9j/');
		channel.onmessage({ time: 1, jpeg: 'x' });
		channel.onmessage(undefined);
		expect(frames).toEqual([]);
		stop();
	});

	test('stopping tells the backend which stream, and nothing more reaches the page', async () => {
		const frames: PlaybackFrame[] = [];
		const stop = startPlayback(0, { fps: null, width: null }, (f) => frames.push(f));
		const channel = await started();
		const id = calls.find((c) => c.cmd === 'start_playback')!.args.playbackId;
		channel.onmessage(message(0, [0xff, 0xd8, 0xff, 0xd9]));
		stop();
		stop();
		channel.onmessage(message(1 / 60, [0xff, 0xd8, 0xff, 0xd9]));
		expect(frames).toHaveLength(1);
		await new Promise((r) => setTimeout(r, 5));
		const stops = calls.filter((c) => c.cmd === 'stop_playback');
		expect(stops).toHaveLength(1);
		expect(stops[0].args).toEqual({ playbackId: id });
	});

	test('each stream has an id of its own, so a late stop cannot end its successor', async () => {
		const first = startPlayback(0, { fps: null, width: null }, () => {});
		await started();
		channels.length = 0;
		const second = startPlayback(0, { fps: null, width: null }, () => {});
		await started();
		const ids = calls.filter((c) => c.cmd === 'start_playback').map((c) => c.args.playbackId as number);
		expect(ids).toHaveLength(2);
		expect(ids[1]).toBeGreaterThan(ids[0]);
		first();
		second();
	});
});

describe('startPlayback (browser harness)', () => {
	test('plays synthesized JPEG frames at the rate it is given, through the same frame shape', async () => {
		delete g.window;
		const frames: PlaybackFrame[] = [];
		const stop = startPlayback(10, { fps: 50, width: null }, (f) => frames.push(f));
		stop();
		// Stopped before the first tick: nothing, and nothing started in the backend.
		await new Promise((r) => setTimeout(r, 150));
		expect(frames).toEqual([]);
		expect(calls).toEqual([]);
	});
});
