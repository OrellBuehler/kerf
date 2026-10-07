import { describe, expect, test } from 'bun:test';
import type { WaveformRange } from './types';
import { WaveformCache, type Fetcher } from './waveform-cache';
import { tileSpec, TILE_BUCKETS, type TileSpec } from './waveform-view';

/** A fetcher whose calls the test settles by hand. */
function manual() {
	const calls: { asset: string; start: number; end: number; buckets: number; ok: (r: WaveformRange) => void; no: (e: Error) => void }[] = [];
	const fetcher: Fetcher = (asset, start, end, buckets) =>
		new Promise<WaveformRange>((ok, no) => calls.push({ asset, start, end, buckets, ok, no }));
	return { calls, fetcher };
}

const range = (buckets = TILE_BUCKETS, channels = 2): WaveformRange => ({
	channels,
	buckets,
	duration: 100,
	peaks_per_second: 100,
	min: Array.from({ length: channels }, () => new Array<number>(buckets).fill(-0.5)),
	max: Array.from({ length: channels }, () => new Array<number>(buckets).fill(0.5))
});

const tick = () => new Promise((r) => setTimeout(r, 0));
const tiles = (...indexes: number[]): TileSpec[] => indexes.map((i) => tileSpec(5, i));

describe('WaveformCache', () => {
	test('fetches a tile once and serves it from the cache after', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		let told = 0;
		cache.want('clip-1', 'a', tiles(0), () => told++);
		expect(m.calls).toHaveLength(1);
		expect(m.calls[0]).toMatchObject({ asset: 'a', start: 0, buckets: TILE_BUCKETS });
		expect(cache.get('a', tiles(0)[0])).toBeUndefined();
		m.calls[0].ok(range());
		await tick();
		expect(told).toBe(1);
		const hit = cache.get('a', tiles(0)[0])!;
		expect(hit.channels).toBe(2);
		expect(hit.max[0][0]).toBe(0.5);
		cache.want('clip-1', 'a', tiles(0), () => told++);
		expect(m.calls).toHaveLength(1); // no second request
		expect(told).toBe(1);
	});

	test('two clips wanting the same tile share one request and both hear about it', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		const told: string[] = [];
		cache.want('one', 'a', tiles(0), () => told.push('one'));
		cache.want('two', 'a', tiles(0), () => told.push('two'));
		expect(m.calls).toHaveLength(1);
		m.calls[0].ok(range());
		await tick();
		expect(told.sort()).toEqual(['one', 'two']);
	});

	test('a request already in flight is joined, not repeated, when the view moves on', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		cache.want('one', 'a', tiles(0, 1), () => {});
		cache.want('one', 'a', tiles(0, 1), () => {}); // a scroll tick that changed nothing
		cache.want('one', 'a', tiles(1, 2), () => {});
		expect(m.calls.map((c) => c.start)).toEqual([tiles(0)[0].start, tiles(1)[0].start, tiles(2)[0].start]);
	});

	test('runs a few at a time, the newest interest first', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher, { concurrency: 2 });
		cache.want('one', 'a', tiles(0, 1, 2, 3), () => {});
		expect(m.calls).toHaveLength(2);
		expect(cache.inflight).toBe(2);
		expect(cache.queued).toBe(2);
		// the view moves to tile 3; it overtakes what is still waiting
		cache.want('one', 'a', tiles(3, 4), () => {});
		m.calls[0].ok(range());
		await tick();
		const started = m.calls.map((c) => Math.round(c.start / tiles(1)[0].start));
		expect(started.slice(0, 2)).toEqual([0, 1]);
		// of what was queued, 3 and 4 are wanted and 2 is not: 2 never runs
		expect(started).not.toContain(2);
		expect(started[2]).toBe(3); // the newest call's list, in its own order
		expect(started).toHaveLength(3);
	});

	test('a tile nobody wants any more is dropped before it costs a request', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher, { concurrency: 1 });
		cache.want('one', 'a', tiles(0, 1, 2), () => {});
		expect(m.calls).toHaveLength(1);
		cache.want('one', 'a', tiles(0), () => {}); // scrolled back
		expect(cache.queued).toBe(0);
		m.calls[0].ok(range());
		await tick();
		expect(m.calls).toHaveLength(1);
	});

	test('a tile another clip still wants stays queued when one clip lets go', () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher, { concurrency: 1 });
		cache.want('one', 'a', tiles(0, 1), () => {});
		cache.want('two', 'a', tiles(1), () => {});
		cache.want('one', 'a', tiles(0), () => {});
		expect(cache.queued).toBe(1);
		cache.release('two');
		expect(cache.queued).toBe(0);
	});

	test('release drops what an unmounted clip had queued', () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher, { concurrency: 1 });
		cache.want('gone', 'a', tiles(0, 1, 2), () => {});
		cache.release('gone');
		expect(cache.queued).toBe(0);
		cache.release('never-existed');
	});

	test('a failed asset is remembered, not asked for again on every scroll', async () => {
		const m = manual();
		let now = 1000;
		const failed: string[] = [];
		const cache = new WaveformCache(m.fetcher, { now: () => now, retryAfterMs: 30_000, onFail: (a, msg) => failed.push(`${a}: ${msg}`) });
		let told = 0;
		cache.want('one', 'broken', tiles(0, 1), () => told++);
		expect(m.calls).toHaveLength(2);
		m.calls[0].no(new Error('no audio stream'));
		await tick();
		expect(cache.failure('broken')).toBe('no audio stream');
		expect(failed).toEqual(['broken: no audio stream']);
		expect(told).toBeGreaterThanOrEqual(1);

		// scrolling around for a while asks for nothing
		for (let i = 0; i < 20; i++) cache.want('one', 'broken', tiles(i, i + 1), () => {});
		expect(m.calls).toHaveLength(2);
		expect(cache.queued).toBe(0);

		// the other asset is unaffected
		cache.want('two', 'fine', tiles(0), () => {});
		expect(m.calls).toHaveLength(3);
		expect(m.calls[2].asset).toBe('fine');
	});

	test('queued tiles of a failed asset never run, and a second failure is not reported twice', async () => {
		const m = manual();
		const failed: string[] = [];
		const cache = new WaveformCache(m.fetcher, { concurrency: 2, onFail: (a) => failed.push(a) });
		cache.want('one', 'broken', tiles(0, 1, 2, 3), () => {});
		expect(m.calls).toHaveLength(2);
		m.calls[0].no(new Error('boom'));
		await tick();
		m.calls[1].no(new Error('boom'));
		await tick();
		expect(m.calls).toHaveLength(2); // 2 and 3 were dropped
		expect(failed).toEqual(['broken']);
	});

	test('after the cooldown the asset is tried again', async () => {
		const m = manual();
		let now = 0;
		const failed: string[] = [];
		const cache = new WaveformCache(m.fetcher, { now: () => now, retryAfterMs: 30_000, onFail: (a) => failed.push(a) });
		cache.want('one', 'a', tiles(0), () => {});
		m.calls[0].no(new Error('busy'));
		await tick();
		now = 29_999;
		cache.want('one', 'a', tiles(0), () => {});
		expect(m.calls).toHaveLength(1);
		expect(cache.failure('a')).toBe('busy');
		now = 30_001;
		expect(cache.failure('a')).toBeUndefined();
		cache.want('one', 'a', tiles(0), () => {});
		expect(m.calls).toHaveLength(2);
		m.calls[1].ok(range());
		await tick();
		expect(cache.get('a', tiles(0)[0])).toBeDefined();
		expect(cache.failure('a')).toBeUndefined();
		expect(failed).toEqual(['a']); // told the user once, however many tries
	});

	test('a fetcher that throws instead of rejecting is a failure too', async () => {
		const cache = new WaveformCache(() => {
			throw new Error('sync boom');
		});
		cache.want('one', 'a', tiles(0), () => {});
		await tick();
		expect(cache.failure('a')).toBe('sync boom');
		expect(cache.inflight).toBe(0);
	});

	test('keeps the most recently used tiles when it has to let go', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher, { maxTiles: 3, concurrency: 10 });
		cache.want('one', 'a', tiles(0, 1, 2, 3, 4), () => {});
		for (const c of m.calls) c.ok(range());
		await tick();
		expect(cache.size).toBe(3);
		expect(cache.get('a', tiles(0)[0])).toBeUndefined();
		expect(cache.get('a', tiles(1)[0])).toBeUndefined();
		// reading 2 makes it the freshest, so the next arrival evicts 3
		expect(cache.get('a', tiles(2)[0])).toBeDefined();
		cache.want('one', 'a', tiles(9), () => {});
		m.calls[m.calls.length - 1].ok(range());
		await tick();
		expect(cache.get('a', tiles(2)[0])).toBeDefined();
		expect(cache.get('a', tiles(3)[0])).toBeUndefined();
	});

	test('tiles of different assets, rungs and windows are different entries', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		cache.want('o', 'a', [tileSpec(5, 0)], () => {});
		cache.want('o2', 'b', [tileSpec(5, 0)], () => {});
		cache.want('o3', 'a', [tileSpec(20, 0)], () => {});
		expect(m.calls).toHaveLength(3);
	});

	test('want after the tiles are already cached notifies, since nothing else will', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		// clip one asks first; its tiles land while clip two is still waiting out its debounce
		cache.want('one', 'a', tiles(0, 1), () => {});
		m.calls[0].ok(range());
		m.calls[1].ok(range());
		await tick();
		let told = 0;
		cache.want('two', 'a', tiles(0, 1), () => told++);
		expect(m.calls).toHaveLength(2); // nothing fetched...
		expect(told).toBe(0); // ...and not on the caller's stack
		await tick();
		expect(told).toBe(1); // ...but it is told, so it redraws from the cache
	});

	test('an owner that let go before the microtask is not told', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		cache.want('one', 'a', tiles(0), () => {});
		m.calls[0].ok(range());
		await tick();
		let told = 0;
		cache.want('two', 'a', tiles(0), () => told++);
		cache.release('two');
		await tick();
		expect(told).toBe(0);
		// ...and one that asked for something else since is told by its newer request only
		cache.want('three', 'a', tiles(0), () => told++);
		cache.want('three', 'a', tiles(5), () => told++);
		await tick();
		expect(told).toBe(0);
	});

	test('a want with nothing to wait for says nothing', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		let told = 0;
		cache.want('one', 'a', [], () => told++);
		await tick();
		expect(told).toBe(0);
	});

	test('a want for an asset that is being held off is told, so the view can say so', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		cache.want('one', 'a', tiles(0), () => {});
		m.calls[0].no(new Error('no audio'));
		await tick();
		let told = 0;
		cache.want('two', 'a', tiles(3), () => told++);
		await tick();
		expect(told).toBe(1);
		expect(cache.failure('a')).toBe('no audio');
		expect(m.calls).toHaveLength(1);
	});

	test('a file that keeps failing is tried less and less often, up to a cap', async () => {
		const m = manual();
		let now = 0;
		const cache = new WaveformCache(m.fetcher, { now: () => now, retryAfterMs: 30_000, maxRetryMs: 4 * 60_000 });
		const attempt = async () => {
			cache.want('one', 'a', tiles(0), () => {});
			m.calls[m.calls.length - 1].no(new Error('still broken'));
			await tick();
		};
		await attempt();
		const waits: number[] = [];
		for (let i = 0; i < 6; i++) {
			waits.push(cache.retryIn('a')!);
			now += cache.retryIn('a')! + 1; // wait it out, then try again
			expect(cache.failure('a')).toBeUndefined();
			await attempt();
		}
		// 30 s, 60 s, 120 s, 240 s, and then it stops growing
		expect(waits).toEqual([30_000, 60_000, 120_000, 240_000, 240_000, 240_000]);
		// while held off nothing is requested, however often it is asked
		const calls = m.calls.length;
		for (let i = 0; i < 50; i++) cache.want('one', 'a', tiles(i), () => {});
		expect(m.calls).toHaveLength(calls);
		expect(cache.retryIn('other')).toBeUndefined();
	});

	test('requests that fail together are one failure, and a tile arriving starts the count over', async () => {
		const m = manual();
		let now = 0;
		const cache = new WaveformCache(m.fetcher, { now: () => now, retryAfterMs: 30_000, concurrency: 3 });
		cache.want('one', 'a', tiles(0, 1, 2), () => {});
		for (const c of m.calls) c.no(new Error('boom'));
		await tick();
		expect(cache.retryIn('a')).toBe(30_000); // not 120 s for three failures in one burst

		now = 30_001;
		cache.want('one', 'a', tiles(0), () => {});
		m.calls[m.calls.length - 1].no(new Error('boom'));
		await tick();
		expect(cache.retryIn('a')).toBe(60_000);

		now += 60_001;
		cache.want('one', 'a', tiles(0), () => {});
		m.calls[m.calls.length - 1].ok(range());
		await tick();
		expect(cache.retryIn('a')).toBeUndefined();
		// the next failure starts from the base again
		cache.want('one', 'a', tiles(7), () => {});
		m.calls[m.calls.length - 1].no(new Error('later'));
		await tick();
		expect(cache.retryIn('a')).toBe(30_000);
	});

	test('clear forgets everything', async () => {
		const m = manual();
		const cache = new WaveformCache(m.fetcher);
		cache.want('one', 'a', tiles(0), () => {});
		m.calls[0].ok(range());
		await tick();
		cache.clear();
		expect(cache.size).toBe(0);
		cache.want('one', 'a', tiles(0), () => {});
		expect(m.calls).toHaveLength(2);
	});
});
