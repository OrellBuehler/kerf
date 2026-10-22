import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { MAX_DROPS, createPictureBuffer, type Slot } from './picture-buffer';

interface Load {
	slot: Slot;
	url: string;
	done: boolean;
	ok(): void;
	fail(): void;
}

/** A buffer whose decodes finish when the test says so. */
function rig() {
	const loads: Load[] = [];
	const seen: Array<[Slot | null, boolean]> = [];
	const buffer = createPictureBuffer({
		load: (slot, url) =>
			new Promise<void>((resolve, reject) => loads.push({ slot, url, done: false, ok: resolve, fail: () => reject(new Error('decode')) })),
		show: (slot, covered) => seen.push([slot, covered])
	});
	const settle = () => new Promise((resolve) => setTimeout(resolve, 0));
	return {
		buffer,
		loads,
		seen,
		/** What is on show now. */
		get shown() {
			return seen.at(-1)?.[0] ?? null;
		},
		/** Finish the oldest decode that has not finished. */
		async finish(ok = true) {
			const load = loads.find((l) => !l.done);
			if (!load) throw new Error('nothing is decoding');
			load.done = true;
			if (ok) load.ok();
			else load.fail();
			await settle();
			return load;
		}
	};
}

describe('createPictureBuffer', () => {
	test('a picture is not shown until it has decoded, and goes into the first slot', async () => {
		const r = rig();
		r.buffer.push('a');
		expect(r.loads.map((l) => [l.slot, l.url])).toEqual([[0, 'a']]);
		expect(r.seen).toEqual([]);
		await r.finish();
		expect(r.seen).toEqual([[0, false]]);
	});

	test('the next picture decodes in the other slot while the current one stays on show', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('b');
		expect(r.loads.at(-1)).toMatchObject({ slot: 1, url: 'b' });
		expect(r.shown).toBe(0);
		await r.finish();
		expect(r.shown).toBe(1);
		r.buffer.push('c');
		expect(r.loads.at(-1)).toMatchObject({ slot: 0, url: 'c' });
		expect(r.shown).toBe(1);
		await r.finish();
		expect(r.shown).toBe(0);
	});

	test('a decode that fails keeps the picture on show, and the next one is tried in the same slot', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('bad');
		await r.finish(false);
		expect(r.shown).toBe(0);
		expect(r.seen).toEqual([[0, false]]);
		r.buffer.push('b');
		expect(r.loads.at(-1)).toMatchObject({ slot: 1, url: 'b' });
		await r.finish();
		expect(r.shown).toBe(1);
	});

	test('one decode at a time, and of the pictures that wait only the newest is decoded', async () => {
		const r = rig();
		r.buffer.push('a');
		r.buffer.push('b');
		r.buffer.push('c');
		expect(r.loads.map((l) => l.url)).toEqual(['a']);
		await r.finish();
		// `a` finished with `c` waiting: it is stale and dropped.
		expect(r.seen).toEqual([]);
		expect(r.loads.map((l) => l.url)).toEqual(['a', 'c']);
		await r.finish();
		expect(r.seen).toEqual([[0, false]]);
	});

	test('a picture that finishes after a newer one was asked for is dropped, not shown before it', async () => {
		const r = rig();
		r.buffer.push('stream-1');
		await r.finish();
		r.buffer.push('stream-2');
		r.buffer.push('still');
		await r.finish();
		expect(r.loads.map((l) => l.url)).toEqual(['stream-1', 'stream-2', 'still']);
		// stream-2 is not shown over the still that followed it.
		expect(r.seen).toEqual([[0, false]]);
		await r.finish();
		expect(r.seen).toEqual([
			[0, false],
			[1, false]
		]);
		// The dropped picture's slot is the one the still used: the still's own picture is untouched.
		expect(r.loads.at(-1)).toMatchObject({ slot: 1, url: 'still' });
	});

	test('a decode slower than the stream still puts a picture on show every MAX_DROPS + 1 frames', async () => {
		const r = rig();
		let pushed = 0;
		const push = () => r.buffer.push(`f${pushed++}`);
		push();
		const shownAt: number[] = [];
		for (let i = 0; i < 3 * (MAX_DROPS + 1); i++) {
			push(); // a frame arrives while the previous decode runs
			const before = r.seen.length;
			const load = await r.finish();
			if (r.seen.length > before) shownAt.push(Number(load.url.slice(1)));
		}
		expect(shownAt.length).toBe(3);
		// In the order they were asked for.
		expect([...shownAt].sort((a, b) => a - b)).toEqual(shownAt);
	});

	test('a picture already on show is not decoded again', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('a');
		r.buffer.push('a');
		expect(r.loads.length).toBe(1);
		expect(r.seen).toEqual([[0, false]]);
	});

	test('a picture over the GPU’s hides the one before it until it is ready', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('b', true);
		expect(r.seen.at(-1)).toEqual([null, true]);
		await r.finish();
		expect(r.seen.at(-1)).toEqual([1, false]);
		// The next one is an ordinary swap: the picture stays until it is ready.
		r.buffer.push('c');
		expect(r.seen.at(-1)).toEqual([1, false]);
	});

	test('the same picture again over the GPU’s is decoded again and shown', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('a', true);
		expect(r.seen.at(-1)).toEqual([null, true]);
		expect(r.loads.at(-1)).toMatchObject({ slot: 1, url: 'a' });
		await r.finish();
		expect(r.seen.at(-1)).toEqual([1, false]);
	});

	test('a picture over the GPU’s that cannot be decoded gives the one before it back', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('bad', true);
		await r.finish(false);
		expect(r.seen.at(-1)).toEqual([0, false]);
	});

	test('a failure with a newer picture waiting keeps the cover for it', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('bad', true);
		r.buffer.push('b');
		await r.finish(false);
		expect(r.seen.at(-1)).toEqual([null, true]);
		await r.finish();
		expect(r.seen.at(-1)).toEqual([1, false]);
	});

	test('reset forgets the pictures and ignores the decode in flight', async () => {
		const r = rig();
		r.buffer.push('a');
		await r.finish();
		r.buffer.push('b');
		r.buffer.reset();
		expect(r.seen.at(-1)).toEqual([null, false]);
		await r.finish();
		expect(r.seen.at(-1)).toEqual([null, false]);
		// Fresh elements: the first picture is in the first slot again, though `a` was shown before.
		r.buffer.push('a');
		expect(r.loads.at(-1)).toMatchObject({ slot: 0, url: 'a' });
		await r.finish();
		expect(r.seen.at(-1)).toEqual([0, false]);
	});
});

describe('the Preview’s two images', () => {
	const source = readFileSync(join(import.meta.dir, 'components/editor/Preview.svelte'), 'utf8');

	test('stay mounted whichever route draws, decode before they are shown, and are hidden, not removed, for the surface', () => {
		// Both are rendered by an `{#each}` of their own, not by the `{#if gpuShown}` chain that swaps the route's picture in and out.
		expect(source).toContain('{#each [0, 1] as const as slot (slot)}');
		expect(source).toContain('bind:this={pictureEls[slot]}');
		expect(source).toContain("visibility:{pictureSlot === slot && !gpuShown && !streamShown ? 'visible' : 'hidden'}");
		// Nothing sets a `src` through the template: that is the undecoded image the matte shows through.
		expect(source).not.toMatch(/<img[^>]*src=\{frameUrl\}/);
		expect(source).toContain('await el.decode();');
		// The frame is told of every picture, an identical one included, and of what it covers.
		expect(source).toContain('pictures.push(url, over)');
		// The aspect is reported once the picture is on show.
		expect(source).toContain('frames.pictureAspect(el.naturalWidth / el.naturalHeight)');
	});
});
