import { describe, expect, test } from 'bun:test';
import { FilmstripCache, geometryOf, type SheetDecoder, type StripFetcher } from './filmstrip-cache';
import type { Filmstrip, FilmstripSheet } from './types';

const sheet = (first: number, count: number, tag = ''): FilmstripSheet => ({
	first_frame: first,
	count,
	width: 480,
	height: 96,
	data_url: `data:image/jpeg;base64,${tag}${first}`
});

/** A strip of `sheets` sheets. */
const strip = (sheets = 2): Filmstrip => ({
	interval: 0.5,
	frame_width: 160,
	frame_height: 96,
	frames: sheets * 3,
	columns: 3,
	sheets: Array.from({ length: sheets }, (_, i) => sheet(i * 3, 3))
});

/** A fetcher whose calls the test settles by hand. */
function manual() {
	const calls: { asset: string; ok: (f: Filmstrip) => void; no: (e: Error) => void }[] = [];
	const fetcher: StripFetcher = (asset) => new Promise<Filmstrip>((ok, no) => calls.push({ asset, ok, no }));
	return { calls, fetcher };
}

/** A decoder that records what it decoded and what was closed. */
function decoder(opts: { bytes?: number; failOn?: (s: FilmstripSheet) => boolean } = {}) {
	const decoded: string[] = [];
	const closed: string[] = [];
	const dec: SheetDecoder = async (s) => {
		if (opts.failOn?.(s)) throw new Error(`cannot decode ${s.data_url}`);
		decoded.push(s.data_url);
		return { image: { url: s.data_url } as unknown as CanvasImageSource, bytes: opts.bytes ?? 1000, close: () => closed.push(s.data_url) };
	};
	return { dec, decoded, closed };
}

const tick = () => new Promise((r) => setTimeout(r, 0));

describe('geometryOf', () => {
	test('is the strip without its pixels', () => {
		const g = geometryOf(strip(2));
		expect(g.sheets).toEqual([
			{ first_frame: 0, count: 3, width: 480, height: 96 },
			{ first_frame: 3, count: 3, width: 480, height: 96 }
		]);
		expect(JSON.stringify(g)).not.toContain('data:');
		expect(g.columns).toBe(3);
	});
});

describe('FilmstripCache', () => {
	test('loads an asset once, decodes each sheet once, and serves it from the cache after', async () => {
		const m = manual();
		const d = decoder();
		const cache = new FilmstripCache(m.fetcher, d.dec);
		let told = 0;
		cache.want('clip-1', 'a', () => told++);
		expect(m.calls).toHaveLength(1);
		expect(cache.get('a')).toBeUndefined();
		m.calls[0].ok(strip(2));
		await tick();
		expect(told).toBe(1);
		expect(d.decoded).toEqual(['data:image/jpeg;base64,0', 'data:image/jpeg;base64,3']);
		const hit = cache.get('a')!;
		expect(hit.images).toHaveLength(2);
		expect(hit.strip.sheets).toHaveLength(2);
		expect(hit.bytes).toBe(2000);
		expect(cache.size).toBe(1);
		// asked again: no request, no decode, and the owner is told it is there
		cache.want('clip-1', 'a', () => told++);
		await tick();
		expect(m.calls).toHaveLength(1);
		expect(d.decoded).toHaveLength(2);
		expect(told).toBe(2);
	});

	test('the held strip carries no data: URLs', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec);
		cache.want('c', 'a', () => {});
		m.calls[0].ok(strip(1));
		await tick();
		expect(JSON.stringify(cache.get('a')!.strip)).not.toContain('data:');
	});

	test('two clips of one asset share one request and both hear about it', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec);
		const told: string[] = [];
		cache.want('one', 'a', () => told.push('one'));
		cache.want('two', 'a', () => told.push('two'));
		expect(m.calls).toHaveLength(1);
		m.calls[0].ok(strip());
		await tick();
		expect(told.sort()).toEqual(['one', 'two']);
	});

	test('a request in flight is joined, not repeated, when the view moves on', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec);
		for (let i = 0; i < 5; i++) cache.want('one', 'a', () => {}); // a scroll tick each
		expect(m.calls).toHaveLength(1);
		expect(cache.inflight).toBe(1);
		expect(cache.queued).toBe(0);
	});

	test('loads a couple of assets at a time, the newest interest first', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec, { concurrency: 2 });
		cache.want('o1', 'a', () => {});
		cache.want('o2', 'b', () => {});
		cache.want('o3', 'c', () => {});
		cache.want('o4', 'd', () => {});
		expect(m.calls.map((c) => c.asset)).toEqual(['a', 'b']);
		expect(cache.queued).toBe(2);
		// the view moved: `c` is wanted again, so it is the more recent interest
		cache.want('o3', 'c', () => {});
		m.calls[0].ok(strip());
		await tick();
		expect(m.calls.map((c) => c.asset)).toEqual(['a', 'b', 'c']);
		m.calls[1].ok(strip());
		await tick();
		expect(m.calls.map((c) => c.asset)).toEqual(['a', 'b', 'c', 'd']);
	});

	test('an asset nobody wants any more is dropped from the queue before it is asked for', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec, { concurrency: 1 });
		cache.want('o1', 'a', () => {});
		cache.want('o2', 'b', () => {});
		expect(cache.queued).toBe(1);
		cache.release('o2'); // scrolled away
		expect(cache.queued).toBe(0);
		m.calls[0].ok(strip());
		await tick();
		expect(m.calls.map((c) => c.asset)).toEqual(['a']); // `b` was never fetched
	});

	test('an asset another clip still wants stays queued when one releases it', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec, { concurrency: 1 });
		cache.want('o1', 'a', () => {});
		cache.want('o2', 'b', () => {});
		cache.want('o3', 'b', () => {});
		cache.release('o2');
		expect(cache.queued).toBe(1);
		m.calls[0].ok(strip());
		await tick();
		expect(m.calls.map((c) => c.asset)).toEqual(['a', 'b']);
	});

	test('a clip that now wants another asset stops wanting the first', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec, { concurrency: 1 });
		cache.want('o1', 'a', () => {});
		cache.want('o2', 'b', () => {});
		cache.want('o2', 'c', () => {}); // the clip was replaced under the same owner
		expect(cache.queued).toBe(1);
		m.calls[0].ok(strip());
		await tick();
		expect(m.calls.map((c) => c.asset)).toEqual(['a', 'c']);
	});

	describe('failure', () => {
		test('is remembered, reported once, and not retried on every scroll', async () => {
			const m = manual();
			const failed: string[] = [];
			let t = 0;
			const cache = new FilmstripCache(m.fetcher, decoder().dec, { now: () => t, retryAfterMs: 1000, onFail: (a, msg) => failed.push(`${a}: ${msg}`) });
			let told = 0;
			cache.want('c1', 'a', () => told++);
			m.calls[0].no(new Error('no video stream'));
			await tick();
			expect(told).toBe(1);
			expect(cache.failure('a')).toBe('no video stream');
			expect(cache.retryIn('a')).toBe(1000);
			expect(failed).toEqual(['a: no video stream']);
			// scrolling asks again and again: no request, but the owner is told
			for (let i = 0; i < 5; i++) cache.want('c1', 'a', () => told++);
			await tick();
			expect(m.calls).toHaveLength(1);
			expect(told).toBe(2);
			expect(failed).toHaveLength(1);
		});

		test('is tried again after the cooldown, which doubles each time in a row', async () => {
			const m = manual();
			let t = 0;
			const cache = new FilmstripCache(m.fetcher, decoder().dec, { now: () => t, retryAfterMs: 1000, maxRetryMs: 3000 });
			const waits: number[] = [];
			for (let i = 0; i < 4; i++) {
				cache.want('c', 'a', () => {});
				expect(m.calls).toHaveLength(i + 1);
				m.calls[i].no(new Error('nope'));
				await tick();
				waits.push(cache.retryIn('a')!);
				t += waits[i]; // wait it out
				expect(cache.failure('a')).toBeUndefined(); // may be tried again
			}
			expect(waits).toEqual([1000, 2000, 3000, 3000]);
		});

		test('one that lands after a success starts the cooldown over', async () => {
			const m = manual();
			let t = 0;
			const cache = new FilmstripCache(m.fetcher, decoder().dec, { now: () => t, retryAfterMs: 1000 });
			cache.want('c', 'a', () => {});
			m.calls[0].no(new Error('x'));
			await tick();
			t += 1000;
			cache.want('c', 'a', () => {});
			m.calls[1].ok(strip());
			await tick();
			expect(cache.get('a')).toBeDefined();
			expect(cache.failure('a')).toBeUndefined();
		});

		test('a sheet that will not decode fails the whole strip and hands back the rest', async () => {
			const m = manual();
			const d = decoder({ failOn: (s) => s.first_frame === 3 });
			const cache = new FilmstripCache(m.fetcher, d.dec);
			cache.want('c', 'a', () => {});
			m.calls[0].ok(strip(3));
			await tick();
			expect(cache.get('a')).toBeUndefined();
			expect(cache.failure('a')).toContain('cannot decode');
			// the sheets that did decode were closed, not leaked
			expect(d.closed.sort()).toEqual(['data:image/jpeg;base64,0', 'data:image/jpeg;base64,6']);
			expect(cache.bytes).toBe(0);
		});

		test('a fetcher that throws synchronously is a failure too', async () => {
			const cache = new FilmstripCache(() => {
				throw new Error('ipc down');
			}, decoder().dec);
			cache.want('c', 'a', () => {});
			await tick();
			expect(cache.failure('a')).toBe('ipc down');
			expect(cache.inflight).toBe(0);
		});

		test('other assets are unaffected by one failing', async () => {
			const m = manual();
			const cache = new FilmstripCache(m.fetcher, decoder().dec);
			cache.want('c1', 'bad', () => {});
			cache.want('c2', 'good', () => {});
			m.calls[0].no(new Error('x'));
			m.calls[1].ok(strip());
			await tick();
			expect(cache.failure('bad')).toBe('x');
			expect(cache.get('good')).toBeDefined();
		});
	});

	describe('memory', () => {
		const load = async (cache: FilmstripCache, m: ReturnType<typeof manual>, asset: string, sheets = 2) => {
			cache.want(`o-${asset}`, asset, () => {});
			m.calls.find((c) => c.asset === asset)!.ok(strip(sheets));
			await tick();
		};

		test('is bounded in bytes: the least recently used asset goes first and its images are closed', async () => {
			const m = manual();
			const d = decoder({ bytes: 1000 });
			const cache = new FilmstripCache(m.fetcher, d.dec, { maxBytes: 4500, concurrency: 1 });
			await load(cache, m, 'a'); // 2000
			await load(cache, m, 'b'); // 4000
			expect(cache.size).toBe(2);
			await load(cache, m, 'c'); // 6000 > 4500: `a` goes
			expect(cache.get('a')).toBeUndefined();
			expect(cache.get('b')).toBeDefined();
			expect(cache.get('c')).toBeDefined();
			expect(cache.bytes).toBe(4000);
			expect(d.closed.sort()).toEqual(['data:image/jpeg;base64,0', 'data:image/jpeg;base64,3'].sort());
		});

		test('reading an asset makes it recently used', async () => {
			const m = manual();
			const cache = new FilmstripCache(m.fetcher, decoder().dec, { maxBytes: 4500, concurrency: 1 });
			await load(cache, m, 'a');
			await load(cache, m, 'b');
			cache.get('a'); // `b` is now the oldest
			await load(cache, m, 'c');
			expect(cache.get('b')).toBeUndefined();
			expect(cache.get('a')).toBeDefined();
			expect(cache.get('c')).toBeDefined();
		});

		test('the asset just loaded is never evicted, even when it alone is over the budget', async () => {
			const m = manual();
			const cache = new FilmstripCache(m.fetcher, decoder().dec, { maxBytes: 500, concurrency: 1 });
			await load(cache, m, 'a'); // 2000 > 500
			expect(cache.get('a')).toBeDefined();
			await load(cache, m, 'b');
			expect(cache.get('a')).toBeUndefined(); // now `b` is the one in use
			expect(cache.get('b')).toBeDefined();
			expect(cache.size).toBe(1);
		});

		test('a long pan across many assets holds the memory bounded', async () => {
			const m = manual();
			const cache = new FilmstripCache(m.fetcher, decoder().dec, { maxBytes: 10_000, concurrency: 1 });
			let peak = 0;
			for (let i = 0; i < 40; i++) {
				await load(cache, m, `asset-${i}`);
				peak = Math.max(peak, cache.bytes);
			}
			expect(peak).toBeLessThanOrEqual(10_000);
			expect(cache.size).toBeLessThanOrEqual(5);
		});

		test('an evicted asset is loaded again when it is wanted again', async () => {
			const m = manual();
			const cache = new FilmstripCache(m.fetcher, decoder().dec, { maxBytes: 2500, concurrency: 1 });
			await load(cache, m, 'a');
			await load(cache, m, 'b'); // evicts a
			expect(cache.get('a')).toBeUndefined();
			cache.want('o-a', 'a', () => {});
			expect(m.calls.filter((c) => c.asset === 'a')).toHaveLength(2);
		});

		test('prune frees assets that left the project, with their failure records', async () => {
			const m = manual();
			const d = decoder();
			const cache = new FilmstripCache(m.fetcher, d.dec, { concurrency: 3 });
			cache.want('o1', 'a', () => {});
			cache.want('o2', 'b', () => {});
			cache.want('o3', 'bad', () => {});
			m.calls[0].ok(strip());
			m.calls[1].ok(strip());
			m.calls[2].no(new Error('x'));
			await tick();
			expect(cache.size).toBe(2);
			cache.prune(['b']);
			expect(cache.get('a')).toBeUndefined();
			expect(cache.get('b')).toBeDefined();
			expect(cache.failure('bad')).toBeUndefined();
			expect(cache.bytes).toBe(2000);
			expect(d.closed).toHaveLength(2);
		});

		test('clear drops everything; a load that was running lands nowhere and is closed', async () => {
			const m = manual();
			const d = decoder();
			const cache = new FilmstripCache(m.fetcher, d.dec);
			cache.want('o1', 'a', () => {});
			cache.want('o2', 'b', () => {});
			m.calls[0].ok(strip());
			await tick();
			expect(cache.size).toBe(1);
			cache.clear();
			expect(cache.size).toBe(0);
			expect(cache.bytes).toBe(0);
			m.calls[1].ok(strip()); // `b` was in flight through the clear
			await tick();
			expect(cache.get('b')).toBeUndefined();
			expect(d.closed.length).toBe(2 + 2); // a's two sheets, b's two
			// and asking for `b` afterwards loads it afresh
			cache.want('o2', 'b', () => {});
			expect(m.calls.filter((c) => c.asset === 'b')).toHaveLength(2);
		});
	});

	test('an owner released while its asset loads does not hold the load up, and the strip is kept for the next', async () => {
		const m = manual();
		const cache = new FilmstripCache(m.fetcher, decoder().dec);
		let told = 0;
		cache.want('o1', 'a', () => told++);
		cache.release('o1');
		m.calls[0].ok(strip());
		await tick();
		expect(told).toBe(0);
		expect(cache.get('a')).toBeDefined();
	});
});
