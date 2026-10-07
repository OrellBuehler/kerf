import { describe, expect, test } from 'bun:test';
import { ThumbCache } from './thumbnails';

const deferred = <T>() => {
	let resolve!: (v: T) => void;
	let reject!: (e: unknown) => void;
	const promise = new Promise<T>((res, rej) => ((resolve = res), (reject = rej)));
	return { promise, resolve, reject };
};

describe('ThumbCache', () => {
	test('decodes an asset once however often the bin remounts', async () => {
		const cache = new ThumbCache();
		let calls = 0;
		const fetch = async () => (calls++, 'data:a');
		expect(await cache.load('a', fetch)).toBe('data:a');
		expect(await cache.load('a', fetch)).toBe('data:a');
		expect(await cache.load('a', fetch)).toBe('data:a');
		expect(calls).toBe(1);
		expect(cache.peek('a')).toBe('data:a');
	});

	test('a remount while the first decode is still running shares it', async () => {
		const cache = new ThumbCache();
		const d = deferred<string | null>();
		let calls = 0;
		const fetch = () => (calls++, d.promise);
		const first = cache.load('a', fetch);
		const second = cache.load('a', fetch);
		expect(calls).toBe(1);
		expect(cache.peek('a')).toBeUndefined();
		d.resolve('data:a');
		expect(await first).toBe('data:a');
		expect(await second).toBe('data:a');
		expect(cache.peek('a')).toBe('data:a');
	});

	test('"no frame" from a decoder that answered is remembered — the harness has none to give', async () => {
		const cache = new ThumbCache();
		let calls = 0;
		expect(await cache.load('browser', async () => (calls++, null))).toBeNull();
		expect(await cache.load('browser', async () => (calls++, 'x'))).toBeNull();
		expect(calls).toBe(1);
		expect(cache.peek('browser')).toBeNull();
	});

	test('a failed decode is not remembered: the next ask tries again, and a success then sticks', async () => {
		const cache = new ThumbCache();
		let calls = 0;
		expect(await cache.load('a', () => (calls++, Promise.reject(new Error('ffmpeg busy'))))).toBeNull();
		expect(cache.peek('a')).toBeUndefined();
		expect(await cache.load('a', async () => (calls++, 'data:a'))).toBe('data:a');
		expect(cache.peek('a')).toBe('data:a');
		expect(await cache.load('a', async () => (calls++, 'data:other'))).toBe('data:a');
		expect(calls).toBe(2);
	});

	test('everyone waiting on a decode that fails gets null, and nothing is left in flight', async () => {
		const cache = new ThumbCache();
		const d = deferred<string | null>();
		let calls = 0;
		const first = cache.load('a', () => (calls++, d.promise));
		const second = cache.load('a', () => (calls++, d.promise));
		d.reject(new Error('gone'));
		expect(await first).toBeNull();
		expect(await second).toBeNull();
		expect(calls).toBe(1);
		// Not stuck: a fresh ask decodes.
		expect(await cache.load('a', async () => (calls++, 'data:a'))).toBe('data:a');
		expect(calls).toBe(2);
	});

	test('an audio asset is marked without a decode', () => {
		const cache = new ThumbCache();
		cache.none('song');
		expect(cache.peek('song')).toBeNull();
	});

	test('prune drops entries for assets that are gone, and keeps the rest', async () => {
		const cache = new ThumbCache();
		await cache.load('a', async () => 'data:a');
		await cache.load('b', async () => 'data:b');
		cache.none('c');
		cache.prune(['b']);
		expect(cache.peek('a')).toBeUndefined();
		expect(cache.peek('b')).toBe('data:b');
		expect(cache.peek('c')).toBeUndefined();
		expect(cache.size).toBe(1);
	});

	test('an asset removed while it decodes does not come back', async () => {
		const cache = new ThumbCache();
		const d = deferred<string | null>();
		const p = cache.load('a', () => d.promise);
		cache.prune([]);
		d.resolve('data:a');
		expect(await p).toBe('data:a');
		expect(cache.peek('a')).toBeUndefined();
		// And a later import of something that reuses the id decodes afresh.
		let calls = 0;
		await cache.load('a', async () => (calls++, 'data:new'));
		expect(calls).toBe(1);
	});
});
