import { describe, expect, test } from 'bun:test';
import './test-runes';
import { PROGRESS_EVERY_MS } from './proxy-queue';
import { ProxyQueueState, type Clock, type QueueBackend } from './proxy-queue.svelte';
import type { ProxyQueue, ProxyStatus } from './types';

// The queue's state over a backend and a clock of its own: it listens only while something watches, reads on a change at
// once and on progress at most once a second, one read at a time, and the newest answer wins. Nothing here touches the
// page's singletons or the harness, so no other test file can reach it nor it them.

const queueOf = (ids: string[], over: Partial<ProxyQueue> = {}): ProxyQueue => ({
	jobs: ids.map((id, i) => ({
		asset_id: id,
		name: `${id}.mp4`,
		state: i === 0 ? 'building' : 'queued',
		fraction: 0,
		position: i + 1,
		duration: 60,
		width: 1920,
		height: 1080,
		pipeline: 'software'
	})),
	remaining: ids.length,
	pipeline: 'software',
	paused: false,
	failed: [],
	...over
});

const status = (asset_id: string, state: ProxyStatus['state'] = 'queued'): ProxyStatus => ({ asset_id, state });

/** The time under the test's control: timers run when it is advanced to them. */
function fakeClock(): Clock & { advance(ms: number): void; at: number; pending(): number } {
	let now = 1_000_000;
	let next = 1;
	const timers = new Map<number, { run: () => void; at: number }>();
	return {
		now: () => now,
		after(run, ms) {
			const id = next++;
			timers.set(id, { run, at: now + ms });
			return id;
		},
		cancel(handle) {
			timers.delete(handle as number);
		},
		advance(ms) {
			const to = now + ms;
			for (;;) {
				const due = [...timers.entries()].filter(([, t]) => t.at <= to).sort((a, b) => a[1].at - b[1].at)[0];
				if (!due) break;
				timers.delete(due[0]);
				now = Math.max(now, due[1].at);
				due[1].run();
			}
			now = to;
		},
		get at() {
			return now;
		},
		pending: () => timers.size
	};
}

/** Everything the queue can ask, answering from `queue`, with the reads and the subscriptions counted. */
function fakeBackend() {
	const log: string[] = [];
	const state = {
		queue: queueOf(['a', 'b', 'c']),
		reads: 0,
		subscribed: { changed: 0, progress: 0 },
		/** A read waits for this when set. */
		gate: null as Promise<void> | null,
		fail: null as Error | null,
		/** A subscription is made after this. */
		slowSubscribe: null as Promise<void> | null
	};
	const changed = new Set<() => void>();
	const progress = new Set<() => void>();
	const backend: QueueBackend = {
		async getProxyQueue() {
			state.reads++;
			const answer = structuredClone(state.queue);
			if (state.gate) await state.gate;
			if (state.fail) throw state.fail;
			return answer;
		},
		async onProxyQueueChanged(cb) {
			if (state.slowSubscribe) await state.slowSubscribe;
			changed.add(cb);
			state.subscribed.changed++;
			return () => void (changed.delete(cb), state.subscribed.changed--);
		},
		async onProxyProgress(cb) {
			if (state.slowSubscribe) await state.slowSubscribe;
			const fn = () => cb(status('a', 'building'));
			progress.add(fn);
			state.subscribed.progress++;
			return () => void (progress.delete(fn), state.subscribed.progress--);
		},
		async prioritizeProxy(id) {
			log.push(`prioritize ${id}`);
			if (state.gate) await state.gate;
			if (state.fail) throw state.fail;
			return status(id, 'queued');
		},
		async cancelProxy(id) {
			log.push(`cancel ${id}`);
			return status(id, 'missing');
		},
		async cancelProxyQueue() {
			log.push('cancel all');
			const cancelled = state.queue.jobs.map((j) => status(j.asset_id, 'missing'));
			if (state.gate) await state.gate;
			return cancelled;
		},
		async pauseProxyQueue() {
			log.push('pause');
			state.queue = { ...state.queue, paused: true };
			return structuredClone(state.queue);
		},
		async resumeProxyQueue() {
			log.push('resume');
			state.queue = { ...state.queue, paused: false };
			return structuredClone(state.queue);
		},
		async rebuildProxy(id) {
			log.push(`rebuild ${id}`);
			return status(id, 'building');
		}
	};
	return {
		backend,
		state,
		log,
		fireChanged: () => [...changed].forEach((cb) => cb()),
		fireProgress: () => [...progress].forEach((cb) => cb())
	};
}

/** Let the promises that are ready run. */
const settle = () => new Promise<void>((r) => setTimeout(r, 0));

function make(over: Partial<ReturnType<typeof fakeBackend>['state']> = {}) {
	const fake = fakeBackend();
	Object.assign(fake.state, over);
	const clock = fakeClock();
	const noted: ProxyStatus[] = [];
	const queue = new ProxyQueueState(fake.backend, clock, (s) => void noted.push(s));
	return { ...fake, clock, noted, queue };
}

describe('watching', () => {
	test('nothing is read or subscribed to until a view watches', async () => {
		const t = make();
		await settle();
		expect(t.state.reads).toBe(0);
		expect(t.state.subscribed).toEqual({ changed: 0, progress: 0 });
		expect(t.queue.queue).toBeNull();
		expect(t.queue.watched).toBe(false);
	});

	test('the first view to watch subscribes to both events and reads the queue once', async () => {
		const t = make();
		t.queue.watch();
		await settle();
		expect(t.state.subscribed).toEqual({ changed: 1, progress: 1 });
		expect(t.state.reads).toBe(1);
		expect(t.queue.queue?.jobs.map((j) => j.asset_id)).toEqual(['a', 'b', 'c']);
		expect(t.queue.readAt).toBe(t.clock.at);
		expect(t.queue.watched).toBe(true);
	});

	test('a second view shares the subscriptions, and they go when the last lets go — a second release is no release', async () => {
		const t = make();
		const one = t.queue.watch();
		const two = t.queue.watch();
		await settle();
		expect(t.state.subscribed).toEqual({ changed: 1, progress: 1 });
		expect(t.state.reads).toBe(1);
		one();
		one();
		expect(t.state.subscribed).toEqual({ changed: 1, progress: 1 });
		expect(t.queue.watched).toBe(true);
		two();
		expect(t.state.subscribed).toEqual({ changed: 0, progress: 0 });
		expect(t.queue.watched).toBe(false);
	});

	test('a view that lets go before the subscriptions are up leaves none behind', async () => {
		let open!: () => void;
		const t = make({ slowSubscribe: new Promise<void>((r) => (open = r)) });
		const stop = t.queue.watch();
		stop();
		open();
		await settle();
		expect(t.state.subscribed).toEqual({ changed: 0, progress: 0 });
	});

	test('watching again after letting go subscribes afresh and reads again', async () => {
		const t = make();
		t.queue.watch()();
		await settle();
		const reads = t.state.reads;
		t.queue.watch();
		await settle();
		expect(t.state.subscribed).toEqual({ changed: 1, progress: 1 });
		expect(t.state.reads).toBe(reads + 1);
	});

	test('once let go, the events do nothing and no timer is left', async () => {
		const t = make();
		const stop = t.queue.watch();
		await settle();
		t.fireProgress();
		t.fireProgress();
		stop();
		expect(t.clock.pending()).toBe(0);
		const reads = t.state.reads;
		t.fireChanged();
		t.fireProgress();
		t.clock.advance(5000);
		await settle();
		expect(t.state.reads).toBe(reads);
	});
});

describe('a change of the queue', () => {
	test('is read at once', async () => {
		const t = make();
		t.queue.watch();
		await settle();
		t.state.queue = queueOf(['b', 'c']);
		t.fireChanged();
		await settle();
		expect(t.state.reads).toBe(2);
		expect(t.queue.queue?.jobs.map((j) => j.asset_id)).toEqual(['b', 'c']);
	});

	test('a burst while a read is under way is one more read, not one each', async () => {
		let release!: () => void;
		const t = make();
		t.queue.watch();
		await settle();
		t.state.gate = new Promise<void>((r) => (release = r));
		t.fireChanged();
		t.fireChanged();
		t.fireChanged();
		await settle();
		expect(t.state.reads).toBe(2);
		t.state.gate = null;
		release();
		await settle();
		expect(t.state.reads).toBe(3);
	});
});

describe('progress', () => {
	test('is read at once when the last read is a second old, then at most once a second', async () => {
		const t = make();
		t.queue.watch();
		await settle();
		expect(t.state.reads).toBe(1);
		// Just after the first read: held until the second is up, and the rest of the burst is that same one read.
		t.clock.advance(300);
		for (let i = 0; i < 20; i++) t.fireProgress();
		await settle();
		expect(t.state.reads).toBe(1);
		expect(t.clock.pending()).toBe(1);
		t.clock.advance(PROGRESS_EVERY_MS - 300 - 1);
		await settle();
		expect(t.state.reads).toBe(1);
		t.clock.advance(1);
		await settle();
		expect(t.state.reads).toBe(2);
		expect(t.clock.pending()).toBe(0);
		// A second after that read, the next one is at once.
		t.clock.advance(PROGRESS_EVERY_MS);
		t.fireProgress();
		await settle();
		expect(t.state.reads).toBe(3);
	});

	test('a read caused by a change counts: progress right after it waits for the second', async () => {
		const t = make();
		t.queue.watch();
		await settle();
		t.clock.advance(PROGRESS_EVERY_MS * 3);
		t.fireChanged();
		await settle();
		expect(t.state.reads).toBe(2);
		t.clock.advance(100);
		t.fireProgress();
		await settle();
		expect(t.state.reads).toBe(2);
		t.clock.advance(PROGRESS_EVERY_MS);
		await settle();
		expect(t.state.reads).toBe(3);
	});
});

describe('the order of answers', () => {
	test('an older read that comes back after a newer one does not overwrite it', async () => {
		const t = make();
		let first!: () => void;
		t.state.queue = queueOf(['old']);
		t.state.gate = new Promise<void>((r) => (first = r));
		const slow = t.queue.refresh();
		await settle();
		// The first read is in flight with the old answer; the next starts after it (single-flight), so to race them
		// the second is Pause's answer, which is a queue too.
		t.state.gate = null;
		t.state.queue = queueOf(['new']);
		const paused = t.queue.setPaused(true);
		await settle();
		expect(t.queue.queue?.jobs.map((j) => j.asset_id)).toEqual(['new']);
		expect(t.queue.queue?.paused).toBe(true);
		first();
		await slow;
		await paused;
		expect(t.queue.queue?.jobs.map((j) => j.asset_id)).toEqual(['new']);
		expect(t.queue.queue?.paused).toBe(true);
	});

	test('a read that fails keeps the queue that was read and says why; the next one that works clears it', async () => {
		const t = make();
		await t.queue.refresh();
		expect(t.queue.queue?.jobs).toHaveLength(3);
		t.state.fail = new Error('the app is not answering');
		await t.queue.refresh();
		expect(t.queue.queue?.jobs).toHaveLength(3);
		expect(t.queue.error).toBe('the app is not answering');
		t.state.fail = null;
		await t.queue.refresh();
		expect(t.queue.error).toBeNull();
	});
});

describe('steering it', () => {
	test('Build next asks for the asset, tells the page its status and reads the queue again', async () => {
		const t = make();
		await t.queue.prioritize('c');
		expect(t.log).toEqual(['prioritize c']);
		expect(t.noted).toEqual([status('c', 'queued')]);
		expect(t.state.reads).toBe(1);
		expect(t.queue.busy.size).toBe(0);
	});

	test('while a call about an asset is in flight it is busy, and a second press on it does nothing', async () => {
		let release!: () => void;
		const t = make({ gate: new Promise<void>((r) => (release = r)) });
		const first = t.queue.prioritize('c');
		const again = t.queue.prioritize('c');
		await settle();
		expect(t.queue.busy.has('c')).toBe(true);
		expect(t.log).toEqual(['prioritize c']);
		t.state.gate = null;
		release();
		await Promise.all([first, again]);
		expect(t.queue.busy.has('c')).toBe(false);
		expect(t.log).toEqual(['prioritize c']);
	});

	test('a refusal rejects to whoever asked, frees the row and reads the queue again', async () => {
		const t = make({ fail: new Error('proxies are turned off') });
		let seen: unknown = null;
		await t.queue.prioritize('c').catch((e) => (seen = e));
		expect((seen as Error).message).toBe('proxies are turned off');
		expect(t.queue.busy.size).toBe(0);
		expect(t.noted).toEqual([]);
		expect(t.state.reads).toBe(1);
	});

	test('Cancel and Rebuild name their asset and tell the page the status they gave back', async () => {
		const t = make();
		await t.queue.cancel('b');
		await t.queue.rebuild('x');
		expect(t.log).toEqual(['cancel b', 'rebuild x']);
		expect(t.noted).toEqual([status('b', 'missing'), status('x', 'building')]);
	});

	test('Cancel all tells the page every status, is one call at a time and reads the queue after', async () => {
		let release!: () => void;
		const t = make({ gate: new Promise<void>((r) => (release = r)) });
		const one = t.queue.cancelAll();
		const two = t.queue.cancelAll();
		await settle();
		expect(t.queue.working).toBe(true);
		expect(t.log).toEqual(['cancel all']);
		t.state.gate = null;
		t.state.queue = queueOf([]);
		release();
		await Promise.all([one, two]);
		expect(t.queue.working).toBe(false);
		expect(t.noted.map((s) => s.asset_id)).toEqual(['a', 'b', 'c']);
		expect(t.queue.queue?.jobs).toEqual([]);
	});

	test('Pause and Resume take the queue they answer with, and do not read it again', async () => {
		const t = make();
		await t.queue.setPaused(true);
		expect(t.queue.queue?.paused).toBe(true);
		await t.queue.setPaused(false);
		expect(t.queue.queue?.paused).toBe(false);
		expect(t.log).toEqual(['pause', 'resume']);
		expect(t.state.reads).toBe(0);
		expect(t.queue.working).toBe(false);
	});
});
