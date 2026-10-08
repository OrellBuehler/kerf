import { afterAll, beforeAll, describe, expect, test } from 'bun:test';
import { compileModule } from 'svelte/compiler';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import type { PreviewFrameResult } from './types';

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
	let status = $state.raw({ supported: true, overlays: env.overlaysCapable ?? false });
	const route = $derived(
		routePreview({
			enabled: env.enabled,
			supported: status.supported,
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
		timelineFrame: env.timelineFrame,
		previewFrame: env.previewFrame,
		answered: (result) => {
			env.answered?.(result);
			// What gpuPreview.note() ends in: the status is read again, and replaced by a new object.
			status = { ...status };
		},
		gpuFailed: (error) => env.gpuFailed?.(error)
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
		set status(v) { status = v; }
	};
}
`;

let harness: (env: Env) => Harness;

interface Env {
	enabled: boolean;
	overlaysCapable?: boolean;
	timelineFrame: (t: number) => Promise<string | null>;
	previewFrame: (t: number, overlays: boolean) => Promise<PreviewFrameResult>;
	answered?: (r: PreviewFrameResult) => void;
	gpuFailed?: (e: unknown) => void;
}

interface Harness {
	frames: { frameUrl: string | null; gpuShown: boolean; imgAspect: number | null };
	stop: () => void;
	time: number;
	timeline: unknown;
	previewEpoch: number;
	boundsEpoch: number;
	streaming: boolean;
	trim: boolean;
	titles: unknown[];
	covered: boolean;
	status: unknown;
}

beforeAll(async () => {
	rmSync(dir, { recursive: true, force: true });
	mkdirSync(dir, { recursive: true });
	emit('preview-bounds.js', readFileSync(join(import.meta.dir, 'preview-bounds.ts'), 'utf8'), false);
	emit('frame-pump.svelte.js', readFileSync(join(import.meta.dir, 'frame-pump.svelte.ts'), 'utf8'), true);
	writeFileSync(join(dir, 'harness.svelte.js'), compileModule(HARNESS, { generate: 'client', filename: 'harness.svelte.js' }).js.code);
	harness = (await import(join(dir, 'harness.svelte.js'))).harness;
});

afterAll(() => rmSync(dir, { recursive: true, force: true }));

const settle = () => new Promise((resolve) => setTimeout(resolve, 15));
const tick = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** A recorder of what the pump asked for. */
function recorder(opts: { jpeg?: (t: number) => Promise<string | null>; gpu?: (t: number, o: boolean) => Promise<PreviewFrameResult> } = {}) {
	const asked: string[] = [];
	return {
		asked,
		timelineFrame: async (t: number) => {
			asked.push(`jpeg@${t}`);
			return opts.jpeg ? opts.jpeg(t) : `data:jpeg-${t}`;
		},
		previewFrame: async (t: number, overlays: boolean) => {
			asked.push(`gpu@${t}${overlays ? '+overlays' : ''}`);
			return opts.gpu
				? opts.gpu(t, overlays)
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
