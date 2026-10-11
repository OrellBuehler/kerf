import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { compileModule } from 'svelte/compiler';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { GpuPlayback } from './gpu-playback';
import type { PlayAhead, PlayAsk, PlayInfo, PreviewFrameResult, PreviewOrder } from './types';

// The Preview's frame fetching is the one place a stray reactive read costs an FFmpeg composite,
// so its reactivity is tested for real: the module is compiled with Svelte's own compiler and run
// on its runtime (bun cannot compile `.svelte.ts` itself), driven by a harness that builds the
// route the way `Preview.svelte` does — a derived object that is *new* whenever any input is
// recomputed — and counts what the pump asked for.

const dir = join(import.meta.dir, `.frame-pump-test-${process.pid}`);
const transpiler = new Bun.Transpiler({ loader: 'ts' });

function emit(name: string, source: string, runes: boolean) {
	const js = transpiler.transformSync(source);
	writeFileSync(join(dir, name), runes ? compileModule(js, { generate: 'client', filename: name }).js.code : js);
}

const HARNESS = `
import { routePreview } from './preview-bounds.js';
import { createFramePump } from './frame-pump.svelte.js';

export function harness(env) {
	let time = $state(0);
	let timeline = $state.raw({});
	let previewEpoch = $state(0);
	let boundsEpoch = $state(0);
	let streaming = $state(false);
	let trimMonitor = $state(false);
	let titles = $state.raw([]);
	let covered = $state(false);
	let width = $state(env.width === undefined ? 960 : env.width);
	let playback = $state.raw(null);
	let status = $state.raw({ supported: true, overlays: env.overlaysCapable ?? false });
	const route = $derived(
		routePreview({
			enabled: env.enabled,
			supported: status.supported,
			detached: false,
			overlaysCapable: status.overlays,
			streaming,
			empty: false,
			titlesShown: titles.length > 0,
			trimMonitor,
			guides: false,
			covered
		})
	);
	const routeVia = $derived(route.via);
	const routeOverlays = $derived(route.overlays);
	const frames = createFramePump({
		time: () => time,
		timeline: () => timeline,
		previewEpoch: () => previewEpoch,
		routeVia: () => routeVia,
		routeOverlays: () => routeOverlays,
		boundsEpoch: () => boundsEpoch,
		hasClips: () => true,
		streaming: () => streaming,
		width: () => width,
		timelineFrame: env.timelineFrame,
		previewFrame: env.previewFrame,
		answered: (result) => {
			env.answered?.(result);
			// What gpuPreview.note() ends in: the status is read again, and replaced by a new object.
			status = { ...status };
		},
		gpuFailed: (error) => env.gpuFailed?.(error),
		playback: () => playback,
		playClock: () => ({ clock: time, latency: env.latency ?? 0, fps: 30 }),
		startup: () => env.startup,
		playHandover: (pb, ahead, now, why) => env.playHandover?.(pb, ahead, now, why),
		playWaiting: (pb, message) => env.playWaiting?.(pb, message),
		now: () => env.now?.() ?? performance.now() / 1000
	});
	const stop = $effect.root(() => {
		$effect(() => frames.run());
	});
	return {
		frames,
		stop,
		set time(v) { time = v; },
		set timeline(v) { timeline = v; },
		set previewEpoch(v) { previewEpoch = v; },
		set boundsEpoch(v) { boundsEpoch = v; },
		set streaming(v) { streaming = v; },
		set trim(v) { trimMonitor = v; },
		set titles(v) { titles = v; },
		set covered(v) { covered = v; },
		set width(v) { width = v; },
		set status(v) { status = v; },
		set playback(v) { playback = v; }
	};
}
`;

let harness: (env: Env) => Harness;

interface Env {
	enabled: boolean;
	overlaysCapable?: boolean;
	width?: number | null;
	timelineFrame: (t: number, width: number) => Promise<string | null>;
	previewFrame: (t: number, overlays: boolean, width: number, order: PreviewOrder, play?: PlayAsk) => Promise<PreviewFrameResult>;
	answered?: (r: PreviewFrameResult) => void;
	gpuFailed?: (e: unknown) => void;
	latency?: number;
	startup?: number;
	playHandover?: (pb: GpuPlayback, ahead: PlayAhead | null, now: boolean, why: string) => void;
	playWaiting?: (pb: GpuPlayback, message: string) => void;
	now?: () => number;
}

interface Harness {
	frames: {
		frameUrl: string | null;
		gpuShown: boolean;
		imgAspect: number | null;
		streamShown: boolean;
		streamDrawn(aspect: number): void;
		showStreamed(url: string): void;
	};
	stop: () => void;
	time: number;
	timeline: unknown;
	previewEpoch: number;
	boundsEpoch: number;
	streaming: boolean;
	trim: boolean;
	titles: unknown[];
	covered: boolean;
	width: number | null;
	status: unknown;
	playback: GpuPlayback | null;
}

beforeAll(async () => {
	rmSync(dir, { recursive: true, force: true });
	mkdirSync(dir, { recursive: true });
	emit('preview-bounds.js', readFileSync(join(import.meta.dir, 'preview-bounds.ts'), 'utf8'), false);
	emit('proxy-wait.js', readFileSync(join(import.meta.dir, 'proxy-wait.ts'), 'utf8'), false);
	emit('frame-pump.svelte.js', readFileSync(join(import.meta.dir, 'frame-pump.svelte.ts'), 'utf8'), true);
	writeFileSync(join(dir, 'harness.svelte.js'), compileModule(HARNESS, { generate: 'client', filename: 'harness.svelte.js' }).js.code);
	harness = (await import(join(dir, 'harness.svelte.js'))).harness;
});

afterAll(() => rmSync(dir, { recursive: true, force: true }));

const settle = () => new Promise((resolve) => setTimeout(resolve, 15));
const tick = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** A recorder of what the pump asked for. */
function recorder(
	opts: {
		jpeg?: (t: number) => Promise<string | null>;
		gpu?: (t: number, o: boolean, play?: PlayAsk) => Promise<PreviewFrameResult>;
	} = {}
) {
	const asked: string[] = [];
	const widths: number[] = [];
	const orders: PreviewOrder[] = [];
	const plays: Array<PlayAsk | undefined> = [];
	return {
		asked,
		widths,
		orders,
		plays,
		timelineFrame: async (t: number, width: number) => {
			asked.push(`jpeg@${t}`);
			widths.push(width);
			return opts.jpeg ? opts.jpeg(t) : `data:jpeg-${t}`;
		},
		previewFrame: async (t: number, overlays: boolean, width: number, order: PreviewOrder, play?: PlayAsk) => {
			asked.push(`${play ? 'play' : 'gpu'}@${t}${overlays ? '+overlays' : ''}`);
			widths.push(width);
			orders.push(order);
			plays.push(play);
			return opts.gpu
				? opts.gpu(t, overlays, play)
				: { renderer: 'ffmpeg' as const, frame: `data:ffmpeg-${t}`, reasons: ['a refusal'], timings: null };
		}
	};
}

describe('with the GPU preview off the frame effect is today\'s', () => {
	test('a frame per playhead move and per edit, and nothing for anything else', async () => {
		const r = recorder();
		const h = harness({ enabled: false, ...r });
		await settle();
		expect(r.asked).toEqual(['jpeg@0']);

		// The trim tool, an overlay being selected, a dialog opening over the frame, the GPU status
		// being read and replaced: none of these is a different picture.
		h.trim = true;
		await settle();
		h.trim = false;
		h.titles = [{ id: 'a' }];
		await settle();
		h.titles = [{ id: 'a' }, { id: 'b' }];
		h.titles = [];
		h.covered = true;
		await settle();
		h.covered = false;
		h.status = { supported: true, overlays: false };
		h.status = { supported: false, overlays: true };
		await settle();
		expect(r.asked).toEqual(['jpeg@0']);

		// What is: the playhead, an edit, a proxy that landed.
		h.time = 1.5;
		await settle();
		h.timeline = {};
		await settle();
		h.previewEpoch = 1;
		await settle();
		expect(r.asked).toEqual(['jpeg@0', 'jpeg@1.5', 'jpeg@1.5', 'jpeg@1.5']);
		expect(h.frames.frameUrl).toBe('data:jpeg-1.5');
		expect(h.frames.gpuShown).toBe(false);
		h.stop();
	});
});

describe('with the GPU preview on', () => {
	test('a frame the backend hands back as the JPEG is one fetch, however often the status is replaced', async () => {
		const r = recorder();
		const h = harness({ enabled: true, overlaysCapable: true, ...r });
		await settle();
		expect(r.asked).toEqual(['gpu@0']);
		// The answer replaced the status object (note -> refresh); that is not a reason to ask again.
		await settle();
		await settle();
		expect(r.asked).toEqual(['gpu@0']);
		h.time = 2;
		await settle();
		await settle();
		expect(r.asked).toEqual(['gpu@0', 'gpu@2']);
		expect(h.frames.frameUrl).toBe('data:ffmpeg-2');
		h.stop();
	});

	test('the GPU route is not single-flight: every wanted time goes out, and a superseded answer changes nothing', async () => {
		const resolvers: Array<(r: PreviewFrameResult) => void> = [];
		const answered: string[] = [];
		const r = recorder({ gpu: () => new Promise((resolve) => resolvers.push(resolve)) });
		const h = harness({ enabled: true, overlaysCapable: true, ...r, answered: (result) => answered.push(result.renderer) });
		await settle();
		h.time = 1;
		await settle();
		h.time = 2;
		await settle();
		h.time = 3;
		await settle();
		expect(r.asked, 'nothing waits for the frame in flight').toEqual(['gpu@0', 'gpu@1', 'gpu@2', 'gpu@3']);
		const superseded: PreviewFrameResult = { renderer: 'superseded', frame: null, reasons: [], timings: null };
		const ffmpeg = (t: number): PreviewFrameResult => ({ renderer: 'ffmpeg', frame: `data:ffmpeg-${t}`, reasons: ['a refusal'], timings: null });
		// What the backend does: it finishes the frame in hand, tells the ones it replaced, draws the newest.
		resolvers[1](superseded);
		resolvers[2](superseded);
		await settle();
		expect(answered).toEqual([]);
		expect(h.frames.frameUrl).toBeNull();
		resolvers[0](ffmpeg(0));
		await settle();
		expect(h.frames.frameUrl).toBe('data:ffmpeg-0');
		resolvers[3](ffmpeg(3));
		await settle();
		expect(h.frames.frameUrl).toBe('data:ffmpeg-3');
		expect(answered).toEqual(['ffmpeg', 'ffmpeg']);
		h.stop();
	});

	test('every GPU request says which of the pump\'s it is, so the backend can drop one that arrives late', async () => {
		const r = recorder();
		const h = harness({ enabled: true, overlaysCapable: true, ...r });
		await settle();
		h.time = 1;
		await settle();
		h.time = 2;
		await settle();
		expect(r.orders).toHaveLength(3);
		const [first, second, third] = r.orders;
		expect(second.client, 'one name for the life of the pump').toBe(first.client);
		expect(third.client).toBe(first.client);
		expect(first.client.length).toBeGreaterThan(4);
		expect(second.seq).toBeGreaterThan(first.seq);
		expect(third.seq).toBeGreaterThan(second.seq);
		h.stop();
	});

	test('an answer older than the one on show is dropped, whichever route it came by', async () => {
		const resolvers: Array<(r: PreviewFrameResult) => void> = [];
		const r = recorder({ gpu: () => new Promise((resolve) => resolvers.push(resolve)) });
		const h = harness({ enabled: true, overlaysCapable: false, ...r });
		await settle();
		h.time = 1;
		await settle();
		const ffmpeg = (t: number): PreviewFrameResult => ({ renderer: 'ffmpeg', frame: `data:ffmpeg-${t}`, reasons: [], timings: null });
		// The newer request is answered first; the older one then lands and must not replace it.
		resolvers[1](ffmpeg(1));
		await settle();
		resolvers[0](ffmpeg(0));
		await settle();
		expect(h.frames.frameUrl).toBe('data:ffmpeg-1');

		// A dialog opens over the frame: the JPEG route asks while a GPU request is still out, and its
		// picture is what stays when the GPU answer finally arrives.
		h.time = 2;
		await settle();
		h.covered = true;
		await settle();
		expect(r.asked.slice(-2)).toEqual(['gpu@2', 'jpeg@2']);
		resolvers[2](ffmpeg(2));
		await settle();
		expect(h.frames.frameUrl).toBe('data:jpeg-2');
		h.stop();
	});

	test('a frame the GPU drew shows nothing over the surface, and the canvas shape comes with it', async () => {
		const r = recorder({
			gpu: async () => ({
				renderer: 'gpu',
				frame: null,
				reasons: [],
				timings: { width: 416, height: 234, decode_ms: 0, composite_ms: 1, present_ms: 1 }
			})
		});
		const h = harness({ enabled: true, overlaysCapable: true, ...r });
		await settle();
		expect(h.frames.gpuShown).toBe(true);
		expect(h.frames.imgAspect).toBeCloseTo(416 / 234, 6);
		h.stop();
	});

	test('the trim tool changes the route only where the surface cannot be drawn over', async () => {
		// Under a surface the page draws over, the overlay is passed on: one new request.
		const capable = recorder();
		const a = harness({ enabled: true, overlaysCapable: true, ...capable });
		await settle();
		a.trim = true;
		await settle();
		a.trim = false;
		await settle();
		expect(capable.asked).toEqual(['gpu@0', 'gpu@0+overlays', 'gpu@0']);
		a.stop();

		// Over a surface that sits above the page it is the JPEG route, once each way.
		const above = recorder();
		const b = harness({ enabled: true, overlaysCapable: false, ...above });
		await settle();
		b.trim = true;
		await settle();
		b.trim = true;
		b.titles = [{ id: 'x' }];
		await settle();
		b.trim = false;
		b.titles = [];
		await settle();
		expect(above.asked).toEqual(['gpu@0', 'jpeg@0', 'gpu@0']);
		b.stop();
	});

	test('a dialog over the frame is the JPEG route and leaving it brings the surface back', async () => {
		const r = recorder();
		const h = harness({ enabled: true, overlaysCapable: false, ...r });
		await settle();
		h.covered = true;
		await settle();
		h.covered = false;
		await settle();
		expect(r.asked).toEqual(['gpu@0', 'jpeg@0', 'gpu@0']);
		h.stop();
	});

	test('a surface that moved asks for the frame again, once', async () => {
		const r = recorder();
		const h = harness({ enabled: true, overlaysCapable: true, ...r });
		await settle();
		h.boundsEpoch = 1;
		await settle();
		expect(r.asked).toEqual(['gpu@0', 'gpu@0']);
		h.stop();
	});

	test('a command that rejects is the JPEG for that frame and the surface is told to go', async () => {
		const failures: unknown[] = [];
		const r = recorder({
			gpu: async () => {
				throw new Error('the GPU path panicked');
			}
		});
		const h = harness({ enabled: true, overlaysCapable: true, ...r, gpuFailed: (e) => failures.push(e) });
		await settle();
		await settle();
		expect(r.asked).toEqual(['gpu@0', 'jpeg@0']);
		expect(failures).toHaveLength(1);
		expect(h.frames.frameUrl).toBe('data:jpeg-0');
		expect(h.frames.gpuShown).toBe(false);
		h.stop();
	});
});

describe('under "Proxy only"', () => {
	const waiting = () =>
		new Error('waiting for the preview proxy of GOPR0042.MP4 — building it (42%) (Settings › Preview › Preview source is Proxy only)');

	test('a refused frame is the preview waiting: not a GPU failure, no JPEG fallback, the picture stays', async () => {
		const failures: unknown[] = [];
		let refuse = false;
		const r = recorder({
			gpu: async (t) => {
				if (refuse) throw waiting();
				return { renderer: 'gpu' as const, frame: null, reasons: [], timings: { width: 1920, height: 1080, decode_ms: 1, composite_ms: 1, present_ms: 1 } };
			}
		});
		const h = harness({ enabled: true, overlaysCapable: true, ...r, gpuFailed: (e) => failures.push(e) });
		await settle();
		await settle();
		expect(h.frames.gpuShown).toBe(true);
		// The next frame is refused: the surface is not told to go, nothing falls back to the JPEG
		// (which would be refused as well), and what is on show stays.
		refuse = true;
		h.time = 1;
		await settle();
		await settle();
		expect(r.asked).toEqual(['gpu@0', 'gpu@1']);
		expect(failures).toEqual([]);
		expect(h.frames.gpuShown).toBe(true);
		// The proxy lands: the frame is asked for again and shown.
		refuse = false;
		h.previewEpoch = 1;
		await settle();
		await settle();
		expect(r.asked).toEqual(['gpu@0', 'gpu@1', 'gpu@1']);
		h.stop();
	});

	test('a pump that was refused still takes the next frame it is asked for', async () => {
		let refuse = true;
		const r = recorder({
			jpeg: async (t) => {
				if (refuse) throw waiting();
				return `data:jpeg-${t}`;
			}
		});
		const h = harness({ enabled: false, ...r });
		await settle();
		refuse = false;
		h.time = 2;
		await settle();
		expect(r.asked).toEqual(['jpeg@0', 'jpeg@2']);
		expect(h.frames.frameUrl).toBe('data:jpeg-2');
		h.stop();
	});
});

describe('either way', () => {
	test('one fetch in flight, and a burst of playhead moves collapses to the last one', async () => {
		let release: (() => void) | undefined;
		const gate = new Promise<void>((resolve) => (release = resolve));
		let first = true;
		const r = recorder({
			jpeg: async (t) => {
				if (first) {
					first = false;
					await gate;
				}
				return `data:jpeg-${t}`;
			}
		});
		const h = harness({ enabled: false, ...r });
		await settle();
		expect(r.asked).toEqual(['jpeg@0']);
		h.time = 1;
		await settle();
		h.time = 2;
		await settle();
		h.time = 3;
		await settle();
		expect(r.asked, 'nothing else starts while one is in flight').toEqual(['jpeg@0']);
		release?.();
		await tick(40);
		expect(r.asked).toEqual(['jpeg@0', 'jpeg@3']);
		expect(h.frames.frameUrl).toBe('data:jpeg-3');
		h.stop();
	});

	test('playback streaming suspends fetching, and ending it fetches the frame once', async () => {
		const r = recorder();
		const h = harness({ enabled: true, overlaysCapable: true, ...r });
		await settle();
		h.streaming = true;
		await settle();
		h.time = 1;
		h.time = 2;
		await settle();
		expect(r.asked).toEqual(['gpu@0']);
		h.streaming = false;
		await settle();
		await settle();
		expect(r.asked).toEqual(['gpu@0', 'gpu@2']);
		h.stop();
	});
});

describe('the width a frame is asked at', () => {
	test('is the measured one, on the JPEG path and on the GPU path', async () => {
		const jpeg = recorder();
		const a = harness({ enabled: false, width: 1440, ...jpeg });
		await settle();
		expect(jpeg.widths).toEqual([1440]);
		a.stop();

		const gpu = recorder();
		const b = harness({ enabled: true, width: 1920, ...gpu });
		await settle();
		expect(gpu.widths).toEqual([1920]);
		b.stop();
	});

	test('a different width asks for the frame again, at the new width, and the same width does not', async () => {
		const r = recorder();
		const h = harness({ enabled: false, ...r });
		await settle();
		h.width = 960;
		await settle();
		expect(r.asked).toEqual(['jpeg@0']);
		h.width = 1440;
		await settle();
		expect(r.asked).toEqual(['jpeg@0', 'jpeg@0']);
		expect(r.widths).toEqual([960, 1440]);
		h.stop();
	});

	test('asks nothing before the frame has been measured, then the real width once it has', async () => {
		const r = recorder();
		const h = harness({ enabled: false, width: null, ...r });
		await settle();
		h.time = 3;
		await settle();
		expect(r.asked).toEqual([]);
		h.width = 1440;
		await settle();
		expect(r.asked).toEqual(['jpeg@3']);
		expect(r.widths).toEqual([1440]);
		h.stop();
	});
});

describe('the playback stream through the pane\u2019s one <img>', () => {
	test('a streamed frame is the picture, never the surface, and the settled still replaces it in the same place', async () => {
		const r = recorder({ gpu: async () => ({ renderer: 'gpu', frame: null, reasons: [], timings: { width: 1920, height: 1080, decode_ms: 1, composite_ms: 1, present_ms: 1 } }) });
		const h = harness({ enabled: true, ...r });
		await settle();
		expect(h.frames.gpuShown).toBe(true);
		h.streaming = true;
		h.frames.showStreamed('data:image/jpeg;base64,AAA');
		expect(h.frames.gpuShown).toBe(false);
		expect(h.frames.streamShown).toBe(false);
		expect(h.frames.frameUrl).toBe('data:image/jpeg;base64,AAA');
		h.frames.showStreamed('data:image/jpeg;base64,BBB');
		expect(h.frames.frameUrl).toBe('data:image/jpeg;base64,BBB');
		h.stop();
	});

	test('what the pump fetches after playback ends lands in frameUrl, the one the stream was in', async () => {
		const r = recorder();
		const h = harness({ enabled: false, ...r });
		await settle();
		h.streaming = true;
		h.frames.showStreamed('data:streamed');
		h.time = 4;
		await settle();
		expect(h.frames.frameUrl).toBe('data:streamed');
		h.streaming = false;
		await settle();
		expect(h.frames.frameUrl).toBe('data:jpeg-4');
		expect(h.frames.streamShown).toBe(false);
		h.stop();
	});
});

describe('the playback stream\u2019s canvas and the fetched frame take the pane turn about', () => {
	test('a drawn stream frame is the picture, never the surface, and says its shape', async () => {
		const r = recorder({ gpu: async () => ({ renderer: 'gpu', frame: null, reasons: [], timings: { width: 1920, height: 1080, decode_ms: 1, composite_ms: 1, present_ms: 1 } }) });
		const h = harness({ enabled: true, ...r });
		await settle();
		expect(h.frames.gpuShown).toBe(true);
		h.streaming = true;
		h.frames.streamDrawn(16 / 9);
		expect(h.frames.streamShown).toBe(true);
		expect(h.frames.gpuShown).toBe(false);
		expect(h.frames.imgAspect).toBeCloseTo(16 / 9, 10);
		h.stop();
	});

	test('the settled frame that lands after playback ends takes the pane back from it', async () => {
		const r = recorder();
		const h = harness({ enabled: false, ...r });
		await settle();
		h.streaming = true;
		h.frames.streamDrawn(1.5);
		h.time = 4;
		await settle();
		// Streaming: nothing is fetched, and the last streamed frame stays on show.
		expect(r.asked).toEqual(['jpeg@0']);
		expect(h.frames.streamShown).toBe(true);
		h.streaming = false;
		await settle();
		expect(r.asked).toEqual(['jpeg@0', 'jpeg@4']);
		expect(h.frames.streamShown).toBe(false);
		expect(h.frames.frameUrl).toBe('data:jpeg-4');
		h.stop();
	});

	test('a frame the GPU draws, or the backend hands back, takes it back too', async () => {
		let draws = true;
		const r = recorder({
			gpu: async (t) =>
				draws
					? { renderer: 'gpu', frame: null, reasons: [], timings: { width: 1280, height: 720, decode_ms: 1, composite_ms: 1, present_ms: 1 } }
					: { renderer: 'ffmpeg', frame: `data:ffmpeg-${t}`, reasons: ['a refusal'], timings: null }
		});
		const h = harness({ enabled: true, ...r });
		await settle();
		h.streaming = true;
		h.frames.streamDrawn(1.5);
		h.streaming = false;
		h.time = 1;
		await settle();
		expect(h.frames.streamShown).toBe(false);
		expect(h.frames.gpuShown).toBe(true);

		h.streaming = true;
		h.frames.streamDrawn(1.5);
		h.streaming = false;
		draws = false;
		h.time = 2;
		await settle();
		expect(h.frames.streamShown).toBe(false);
		expect(h.frames.frameUrl).toBe('data:ffmpeg-2');
		h.stop();
	});
});

// ---- GPU playback -------------------------------------------------------------------------

const timings = { width: 960, height: 540, decode_ms: 3, composite_ms: 1, present_ms: 1 };

function playInfo(over: Partial<PlayInfo> = {}): PlayInfo {
	return { session: 1, start: 0, frame: 0, repeat: false, ahead: null, ...over };
}

const drew = (over: Partial<PlayInfo> = {}): PreviewFrameResult => ({
	renderer: 'gpu',
	frame: null,
	reasons: [],
	timings,
	play: playInfo(over)
});

describe('GPU playback', () => {
	test('asks once per frame of the cut for the picture that is audible, with the session, and shows the surface', async () => {
		const r = recorder({ gpu: async () => drew() });
		const h = harness({ enabled: true, overlaysCapable: true, latency: 0.05, ...r });
		await settle();
		expect(r.asked).toEqual(['gpu@0']);
		const pb = new GpuPlayback(30);
		h.playback = pb;
		await settle();
		// The first request of a session decides it.
		expect(r.asked.at(-1)).toBe('play@0');
		expect(r.plays.at(-1)).toEqual({ session: pb.session });
		expect(pb.decided).toBe('gpu');
		expect(h.frames.gpuShown).toBe(true);
		// The picture is the one that is audible: the clock less the sound's 50 ms.
		const before = r.asked.length;
		h.time = 1;
		await settle();
		expect(r.asked.length).toBe(before + 1);
		const asked = Number(r.asked.at(-1)?.slice('play@'.length));
		expect(asked).toBeGreaterThan(0.94);
		expect(asked).toBeLessThan(0.951);
		// A 60 Hz display for a fifth of a second, a 30 fps cut: a request for every other animation frame.
		const plays = () => r.asked.filter((a) => a.startsWith('play')).length;
		const start = plays();
		for (let i = 1; i <= 12; i++) {
			h.time = 1 + i / 60;
			await settle();
		}
		expect(plays() - start).toBeGreaterThanOrEqual(5);
		expect(plays() - start).toBeLessThanOrEqual(7);
		// The order of every request is the pump's, counting up.
		const seqs = r.orders.map((o) => o.seq);
		expect(seqs).toEqual([...seqs].sort((a, b) => a - b));
		h.stop();
	});

	test('the first request is the only one out until the session is decided', async () => {
		const resolvers: Array<(x: PreviewFrameResult) => void> = [];
		const r = recorder({
			gpu: (t, _o, play) =>
				play
					? new Promise((resolve) => resolvers.push(resolve))
					: Promise.resolve({ renderer: 'ffmpeg' as const, frame: `data:ffmpeg-${t}`, reasons: [], timings: null })
		});
		const h = harness({ enabled: true, overlaysCapable: true, ...r });
		await settle();
		h.playback = new GpuPlayback(30);
		await settle();
		for (let i = 1; i <= 10; i++) {
			h.time = i / 30;
			await settle();
		}
		expect(r.asked.filter((a) => a.startsWith('play'))).toHaveLength(1);
		resolvers[0](drew());
		await settle();
		h.time = 1;
		await settle();
		expect(r.asked.filter((a) => a.startsWith('play'))).toHaveLength(2);
		h.stop();
	});

	test('a stream verdict hands the session over at once, shows nothing, and says where from', async () => {
		const handovers: Array<[boolean, number | null, string]> = [];
		const answered: string[] = [];
		const stream: PreviewFrameResult = {
			renderer: 'stream',
			frame: null,
			reasons: ['a mask'],
			timings: null,
			play: playInfo({ ahead: { at: 3, handover: 2.8, kind: 'mask', why: 'a mask' } })
		};
		const r = recorder({
			gpu: async (t, _o, play) =>
				play ? stream : { renderer: 'ffmpeg', frame: `data:ffmpeg-${t}`, reasons: [], timings: null }
		});
		const h = harness({
			enabled: true,
			overlaysCapable: true,
			...r,
			answered: (x) => answered.push(x.renderer),
			playHandover: (_pb, ahead, now, why) => handovers.push([now, ahead?.handover ?? null, why])
		});
		await settle();
		const pb = new GpuPlayback(30);
		h.playback = pb;
		await settle();
		expect(handovers).toEqual([[true, 2.8, 'a mask']]);
		expect(answered).toEqual(['ffmpeg', 'stream']);
		expect(h.frames.gpuShown).toBe(false);
		expect(pb.ended).toBe(true);
		// Nothing more is asked of an ended session.
		h.time = 5;
		await settle();
		expect(r.asked.filter((a) => a.startsWith('play'))).toHaveLength(1);
		h.stop();
	});

	test('a request that fails hands over and hides the surface; a proxy wait waits instead', async () => {
		const failed: unknown[] = [];
		const handovers: string[] = [];
		const waiting: string[] = [];
		let reject: unknown = new Error('the GPU preview\'s frame worker stopped');
		const r = recorder({
			gpu: async (t, _o, play) => {
				if (play) throw reject;
				return { renderer: 'ffmpeg', frame: `data:ffmpeg-${t}`, reasons: [], timings: null };
			}
		});
		const h = harness({
			enabled: true,
			overlaysCapable: true,
			...r,
			gpuFailed: (e) => failed.push(e),
			playHandover: (_pb, _ahead, now, why) => handovers.push(`${now}:${why}`),
			playWaiting: (_pb, m) => waiting.push(m)
		});
		await settle();
		h.playback = new GpuPlayback(30);
		await settle();
		expect(failed).toHaveLength(1);
		expect(handovers).toEqual(["true:the GPU preview's frame worker stopped"]);

		reject = 'waiting for the preview proxy of "a.mp4" (40%)';
		h.playback = new GpuPlayback(30);
		await settle();
		expect(waiting).toHaveLength(1);
		expect(failed, 'a wait is not a failure').toHaveLength(1);
		h.stop();
	});

	test('the hand-over the answers announce is made once, when the playhead gets to it, and the GPU keeps drawing', async () => {
		const handovers: Array<[boolean, number]> = [];
		const ahead = { at: 6, handover: 5.5, kind: 'mask', why: 'a mask' };
		const r = recorder({ gpu: async () => drew({ ahead }) });
		const h = harness({
			enabled: true,
			overlaysCapable: true,
			startup: 0.2,
			...r,
			playHandover: (pb, a, now) => {
				handovers.push([now, a?.handover ?? -1]);
				pb.handOver();
			}
		});
		await settle();
		const pb = new GpuPlayback(30);
		h.playback = pb;
		h.time = 1;
		await settle();
		expect(pb.ahead?.handover).toBe(5.5);
		h.time = 5.2;
		await settle();
		expect(handovers).toEqual([]);
		h.time = 5.31;
		await settle();
		expect(handovers).toEqual([[false, 5.5]]);
		// The surface is still the GPU's, and it is asked for frames on: the stream is not on screen yet.
		const before = r.asked.length;
		h.time = 5.5;
		await settle();
		h.time = 5.6;
		await settle();
		expect(handovers).toHaveLength(1);
		expect(r.asked.length).toBeGreaterThan(before);
		expect(h.frames.gpuShown).toBe(true);
		h.stop();
	});

	test('answers for a session that is over are dropped, and a repeat or a superseded answer changes nothing', async () => {
		const resolvers: Array<(x: PreviewFrameResult) => void> = [];
		const answered: string[] = [];
		const r = recorder({ gpu: () => new Promise((resolve) => resolvers.push(resolve)) });
		const h = harness({ enabled: true, overlaysCapable: true, ...r, answered: (x) => answered.push(`${x.renderer}:${x.play?.repeat ?? ''}`) });
		await settle();
		resolvers.shift()?.({ renderer: 'ffmpeg', frame: 'data:x', reasons: [], timings: null });
		const first = new GpuPlayback(30);
		h.playback = first;
		await settle();
		// The session is replaced (a seek) while its first request is out.
		h.playback = new GpuPlayback(30);
		await settle();
		resolvers.shift()?.(drew());
		await settle();
		expect(h.frames.gpuShown, 'the answer of a session that was replaced is not shown').toBe(false);
		expect(first.decided).toBe('pending');
		// The new session: a drawn frame, then a repeat, then a superseded one.
		resolvers.shift()?.(drew());
		await settle();
		expect(h.frames.gpuShown).toBe(true);
		const shownBefore = answered.length;
		h.time = 1;
		await settle();
		resolvers.shift()?.(drew({ repeat: true }));
		await settle();
		h.time = 2;
		await settle();
		resolvers.shift()?.({ renderer: 'superseded', frame: null, reasons: [], timings: null });
		await settle();
		expect(answered.length, 'neither is a new picture').toBe(shownBefore);
		h.stop();
	});

	test('a backend that stops answering is given up on: the session goes to the stream and the GPU is marked failed', async () => {
		let clock = 0;
		const failed: string[] = [];
		const handovers: Array<[boolean, string]> = [];
		const r = recorder({
			gpu: (t, _o, play) =>
				play
					? new Promise<PreviewFrameResult>(() => {})
					: Promise.resolve({ renderer: 'ffmpeg' as const, frame: `data:ffmpeg-${t}`, reasons: [], timings: null })
		});
		const h = harness({
			enabled: true,
			overlaysCapable: true,
			...r,
			now: () => clock,
			gpuFailed: (e) => failed.push(e instanceof Error ? e.message : String(e)),
			playHandover: (_pb, _ahead, now, why) => handovers.push([now, why])
		});
		await settle();
		h.playback = new GpuPlayback(30);
		await settle();
		clock = 3;
		h.time = 0.5;
		await settle();
		expect(handovers).toEqual([]);
		clock = 5.2;
		h.time = 1;
		await settle();
		expect(handovers).toEqual([[true, 'the GPU stopped answering']]);
		expect(failed).toHaveLength(1);
		h.stop();
	});

	test('when the session ends the settled frame comes back through the ordinary route', async () => {
		const r = recorder({ gpu: async () => drew() });
		const h = harness({ enabled: true, overlaysCapable: true, ...r });
		await settle();
		h.playback = new GpuPlayback(30);
		h.time = 2;
		await settle();
		const forward = r.asked.filter((a) => a.startsWith('play')).length;
		expect(forward).toBeGreaterThan(0);
		h.playback = null;
		await settle();
		expect(r.asked.at(-1)).toBe('gpu@2');
		expect(r.plays.at(-1)).toBeUndefined();
		h.stop();
	});
});
