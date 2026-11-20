import { afterAll, beforeAll, beforeEach, describe, expect, test } from 'bun:test';
import type { ProxyQueue, ProxyStatus, QueuedProxy } from './types';

// The browser harness stands in for kerf-app's proxy queue (`get_proxy_queue`, `prioritize_proxy`, `cancel_proxy`,
// `cancel_proxy_queue`, `pause_proxy_queue`, `resume_proxy_queue` and the `proxy-queue-changed` event). A panel is
// explored against it under `bun run dev`, so what it answers keeps the shape of `ProxyQueue`'s serde type and the
// rules of the core's line: the build under way first, a job asked for next goes in front of the waiting ones, a
// cancelled one is not failed but not built, a pause lets the build under way finish and starts nothing, an
// estimate counts the jobs before it only. `?proxy=queued` seeds a live queue (the interview building, the b-roll and
// three more clips behind it, one failure), `?proxy=waiting` the same with an export holding the machine.

const g = globalThis as unknown as { window?: unknown; localStorage?: Storage };
const original = { window: g.window, localStorage: g.localStorage };
const store = new Map<string, string>();

type Api = typeof import('./api');

let instances = 0;
/** A fresh copy of the harness, read with the flags set (they are read when it loads). */
async function load(search: string): Promise<Api> {
	g.window = { location: { search } };
	const specifier = `./api?proxyqueue${instances++}`;
	return (await import(specifier)) as Api;
}

beforeAll(() => {
	g.localStorage = {
		getItem: (k: string) => store.get(k) ?? null,
		setItem: (k: string, v: string) => void store.set(k, String(v)),
		removeItem: (k: string) => void store.delete(k)
	} as Storage;
});

beforeEach(() => store.clear());

afterAll(() => {
	g.window = original.window;
	g.localStorage = original.localStorage;
	if (original.window === undefined) delete g.window;
	if (original.localStorage === undefined) delete g.localStorage;
});

const INTERVIEW = '11111111-1111-1111-1111-111111111111';
const BROLL = '22222222-2222-2222-2222-222222222222';
const SONG = '33333333-3333-3333-3333-333333333333';
const DRONE = '44444444-4444-4444-4444-444444444444';
const TIMELAPSE = '55555555-5555-5555-5555-555555555555';
const GOPRO = '66666666-6666-6666-6666-666666666666';
const CROWD = '77777777-7777-7777-7777-777777777777';
const CORRUPT = '88888888-8888-8888-8888-888888888888';

const order = (q: ProxyQueue) => q.jobs.map((j) => j.asset_id);
const job = (q: ProxyQueue, id: string): QueuedProxy => q.jobs.find((j) => j.asset_id === id)!;
/** What a listener was told, once the microtasks of the call that caused it have run. */
async function listen(api: Api) {
	const calls = { queue: 0, progress: [] as ProxyStatus[] };
	const stops = [await api.onProxyQueueChanged(() => void calls.queue++), await api.onProxyProgress((s) => void calls.progress.push(s))];
	return { calls, stop: () => stops.forEach((s) => s()) };
}

describe('a queue (?proxy=queued)', () => {
	test('the seeded queue: the interview building, four clips waiting in order, one failure, nothing paused', async () => {
		const api = await load('?proxy=queued');
		const q = await api.getProxyQueue();
		expect(order(q)).toEqual([INTERVIEW, BROLL, DRONE, TIMELAPSE, GOPRO, CROWD]);
		expect(q.jobs.map((j) => [j.position, j.state])).toEqual([
			[1, 'building'],
			[2, 'queued'],
			[3, 'queued'],
			[4, 'queued'],
			[5, 'queued'],
			[6, 'queued']
		]);
		expect(q.remaining).toBe(6);
		expect(q.paused).toBe(false);
		expect(q.pipeline).toBe('software');
		expect('waiting_on' in q).toBe(false);
		expect(job(q, INTERVIEW).fraction).toBeGreaterThanOrEqual(0.42);
		expect(job(q, BROLL)).toMatchObject({ name: 'broll.mp4', fraction: 0, width: 3840, height: 2160, duration: 45, pipeline: 'software' });
		expect(q.failed).toEqual([
			{ asset_id: CORRUPT, name: 'corrupt-take.mp4', reason: 'could not generate preview proxy: Invalid data found when processing input' }
		]);
	});

	test('each job has a time that counts the jobs before it, and the queue has the last of them', async () => {
		const api = await load('?proxy=queued');
		const q = await api.getProxyQueue();
		const etas = q.jobs.map((j) => j.eta_secs);
		expect(etas.every((e) => typeof e === 'number' && e > 0)).toBe(true);
		for (let i = 1; i < etas.length; i++) expect(etas[i]!).toBeGreaterThan(etas[i - 1]!);
		expect(q.eta_secs).toBe(etas.at(-1)!);
		expect(q.rate).toBeGreaterThan(0);
		// The b-roll's own length is the gap between it and the build before it: 4K, 45 s, at the harness's speed.
		const own = (job(q, BROLL).eta_secs ?? 0) - (job(q, INTERVIEW).eta_secs ?? 0);
		expect(own).toBeCloseTo((3840 * 2160 * 45) / (1920 * 1080 * 10), 5);
	});

	test('the statuses agree with the queue: building, queued, failed', async () => {
		const api = await load('?proxy=queued');
		const byId = new Map((await api.getProxyStatuses()).map((p) => [p.asset_id, p]));
		expect(byId.get(INTERVIEW)).toMatchObject({ state: 'building' });
		for (const id of [BROLL, DRONE, TIMELAPSE, GOPRO, CROWD]) expect(byId.get(id)?.state).toBe('queued');
		expect(byId.get(CORRUPT)).toMatchObject({ state: 'failed' });
		expect((await api.listAssets()).map((a) => a.id)).toContain(CROWD);
	});

	test('the build under way advances, and its time shrinks with it', async () => {
		const api = await load('?proxy=queued');
		const before = job(await api.getProxyQueue(), INTERVIEW);
		const seen: ProxyStatus[] = [];
		const stop = await api.onProxyProgress((s) => void seen.push(s));
		const until = Date.now() + 3000;
		let now = before;
		while (Date.now() < until && now.fraction <= before.fraction) {
			await new Promise((r) => setTimeout(r, 100));
			now = job(await api.getProxyQueue(), INTERVIEW);
		}
		stop();
		expect(now.fraction).toBeGreaterThan(before.fraction);
		expect(now.eta_secs!).toBeLessThan(before.eta_secs!);
		expect(seen.some((s) => s.asset_id === INTERVIEW && s.state === 'building' && (s.fraction ?? 0) > before.fraction)).toBe(true);
	});

	test('asked for next, a job goes in front of the waiting ones, behind the build under way, and the line is announced once', async () => {
		const api = await load('?proxy=queued');
		const { calls, stop } = await listen(api);
		const status = await api.prioritizeProxy(CROWD);
		expect(status).toMatchObject({ asset_id: CROWD, state: 'queued' });
		expect(order(await api.getProxyQueue())).toEqual([INTERVIEW, CROWD, BROLL, DRONE, TIMELAPSE, GOPRO]);
		expect(calls.queue).toBe(1);
		// Asks stack, the latest first.
		await api.prioritizeProxy(GOPRO);
		expect(order(await api.getProxyQueue())).toEqual([INTERVIEW, GOPRO, CROWD, BROLL, DRONE, TIMELAPSE]);
		expect(calls.queue).toBe(2);
		// The positions and the times follow the order.
		const q = await api.getProxyQueue();
		expect(q.jobs.map((j) => j.position)).toEqual([1, 2, 3, 4, 5, 6]);
		expect(job(q, GOPRO).eta_secs!).toBeLessThan(job(q, CROWD).eta_secs!);
		stop();
	});

	test('a build under way is not restarted or moved by being asked for', async () => {
		const api = await load('?proxy=queued');
		const { calls, stop } = await listen(api);
		const before = job(await api.getProxyQueue(), INTERVIEW);
		const status = await api.prioritizeProxy(INTERVIEW);
		expect(status.state).toBe('building');
		expect(status.fraction!).toBeGreaterThanOrEqual(before.fraction);
		expect(order(await api.getProxyQueue())[0]).toBe(INTERVIEW);
		expect(calls.queue).toBe(0);
		stop();
	});

	test('a failed proxy asked for is queued in front and no longer failed', async () => {
		const api = await load('?proxy=queued');
		const status = await api.prioritizeProxy(CORRUPT);
		expect(status.state).toBe('queued');
		const q = await api.getProxyQueue();
		expect(order(q).slice(0, 2)).toEqual([INTERVIEW, CORRUPT]);
		expect(q.failed).toEqual([]);
	});

	test('a file that needs no proxy is refused in the core’s words, and an unknown asset is refused', async () => {
		const api = await load('?proxy=queued');
		await expect(api.prioritizeProxy(SONG)).rejects.toThrow('music.mp3 gets no proxy: audio only');
		await expect(api.prioritizeProxy('99999999-9999-9999-9999-999999999999')).rejects.toThrow('asset not found');
		await expect(api.cancelProxy('99999999-9999-9999-9999-999999999999')).rejects.toThrow('asset not found');
	});

	test('proxies that are off cannot be asked for', async () => {
		const api = await load('?proxy=queued');
		await api.setSettings({ proxy_size: 0 });
		await expect(api.prioritizeProxy(CORRUPT)).rejects.toThrow('proxies are turned off');
	});

	test('a cancelled job leaves the queue as not built, says so, and the queue is announced', async () => {
		const api = await load('?proxy=queued');
		const { calls, stop } = await listen(api);
		const gone = await api.cancelProxy(DRONE);
		expect(gone).toMatchObject({ asset_id: DRONE, state: 'missing' });
		expect(calls.progress.at(-1)).toMatchObject({ asset_id: DRONE, state: 'missing' });
		expect(calls.queue).toBe(1);
		const q = await api.getProxyQueue();
		expect(order(q)).toEqual([INTERVIEW, BROLL, TIMELAPSE, GOPRO, CROWD]);
		expect(q.remaining).toBe(5);
		expect(q.failed.map((f) => f.asset_id)).toEqual([CORRUPT]);
		expect((await api.getProxyStatuses()).find((p) => p.asset_id === DRONE)?.state).toBe('missing');
		// Nothing to cancel is not an error: the status comes back as it is.
		expect((await api.cancelProxy(DRONE)).state).toBe('missing');
		expect(calls.queue).toBe(1);
		stop();
	});

	test('cancelling the build under way starts the next one', async () => {
		const api = await load('?proxy=queued');
		const gone = await api.cancelProxy(INTERVIEW);
		expect(gone.state).toBe('missing');
		const q = await api.getProxyQueue();
		expect(q.jobs[0]).toMatchObject({ asset_id: BROLL, state: 'building', position: 1 });
		expect(q.jobs.slice(1).every((j) => j.state === 'queued')).toBe(true);
		expect((await api.getProxyStatuses()).find((p) => p.asset_id === BROLL)?.state).toBe('building');
	});

	test('cancelling everything answers each asset’s status, empties the queue and keeps the failures', async () => {
		const api = await load('?proxy=queued');
		const { calls, stop } = await listen(api);
		const done = await api.cancelProxyQueue();
		expect(done.map((s) => s.asset_id).sort()).toEqual([INTERVIEW, BROLL, DRONE, TIMELAPSE, GOPRO, CROWD].sort());
		expect(done.every((s) => s.state === 'missing')).toBe(true);
		expect(calls.progress.filter((s) => s.state === 'missing')).toHaveLength(6);
		expect(calls.queue).toBe(1);
		const q = await api.getProxyQueue();
		expect(q).toEqual({ jobs: [], remaining: 0, rate: q.rate, pipeline: 'software', paused: false, failed: q.failed });
		expect(q.failed.map((f) => f.asset_id)).toEqual([CORRUPT]);
		expect(await api.cancelProxyQueue()).toEqual([]);
		stop();
	});

	test('a pause leaves the build under way and gives the rest no time; resuming brings the times back', async () => {
		const api = await load('?proxy=queued');
		const { calls, stop } = await listen(api);
		const paused = await api.pauseProxyQueue();
		expect(paused.paused).toBe(true);
		expect(paused.jobs[0]).toMatchObject({ asset_id: INTERVIEW, state: 'building' });
		expect(typeof paused.jobs[0].eta_secs).toBe('number');
		expect(paused.jobs.slice(1).every((j) => !('eta_secs' in j))).toBe(true);
		expect('eta_secs' in paused).toBe(false);
		expect(calls.queue).toBe(1);
		// Pausing a paused queue changes nothing and says nothing.
		expect((await api.pauseProxyQueue()).paused).toBe(true);
		expect(calls.queue).toBe(1);
		// A rebuild while paused waits its turn; so does asking for one next.
		expect((await api.rebuildProxy(CORRUPT)).state).toBe('queued');
		expect((await api.getProxyQueue()).jobs.at(-1)?.asset_id).toBe(CORRUPT);
		const resumed = await api.resumeProxyQueue();
		expect(resumed.paused).toBe(false);
		expect(resumed.jobs.every((j) => typeof j.eta_secs === 'number')).toBe(true);
		expect(typeof resumed.eta_secs).toBe('number');
		expect(calls.queue).toBe(3);
		stop();
	});

	test('while paused the next job does not start when the build under way ends; resuming starts it', async () => {
		const api = await load('?proxy=queued');
		await api.pauseProxyQueue();
		// The build under way is cancelled; nothing starts in its place while paused.
		await api.cancelProxy(INTERVIEW);
		let q = await api.getProxyQueue();
		expect(q.jobs.every((j) => j.state === 'queued')).toBe(true);
		q = await api.resumeProxyQueue();
		expect(q.jobs[0]).toMatchObject({ asset_id: BROLL, state: 'building' });
	});

	test('a rebuild queues behind the others and starts when its turn comes; a delete leaves the line', async () => {
		const api = await load('?proxy=queued');
		const queued = await api.rebuildProxy(CORRUPT);
		expect(queued.state).toBe('queued');
		expect((await api.getProxyQueue()).failed).toEqual([]);
		expect(order(await api.getProxyQueue()).at(-1)).toBe(CORRUPT);
		const gone = await api.deleteProxy(BROLL);
		expect(gone.state).toBe('off');
		expect(order(await api.getProxyQueue())).not.toContain(BROLL);
	});

	test('a removed asset leaves the queue', async () => {
		const api = await load('?proxy=queued');
		const { calls, stop } = await listen(api);
		await api.removeAsset(GOPRO, true);
		expect(order(await api.getProxyQueue())).not.toContain(GOPRO);
		expect(calls.queue).toBeGreaterThan(0);
		stop();
	});

	test('a listener that has been stopped hears nothing more', async () => {
		const api = await load('?proxy=queued');
		let heard = 0;
		const stop = await api.onProxyQueueChanged(() => void heard++);
		await api.pauseProxyQueue();
		expect(heard).toBe(1);
		stop();
		await api.resumeProxyQueue();
		expect(heard).toBe(1);
	});
});

describe('an export holds the machine (?proxy=waiting)', () => {
	test('nothing is building, there is no time to give, and the queue says what it waits for', async () => {
		const api = await load('?proxy=waiting');
		const q = await api.getProxyQueue();
		expect(q.jobs.every((j) => j.state === 'queued')).toBe(true);
		expect(order(q)).toEqual([INTERVIEW, BROLL, DRONE, TIMELAPSE, GOPRO, CROWD]);
		expect(q.waiting_on).toBe('an export or another job you started');
		expect(q.jobs.every((j) => !('eta_secs' in j))).toBe(true);
		expect('eta_secs' in q).toBe(false);
		expect('rate' in q).toBe(false);
		// Asking for one does not start it: the machine is not free.
		expect((await api.prioritizeProxy(CROWD)).state).toBe('queued');
		expect(order(await api.getProxyQueue())[0]).toBe(CROWD);
	});

	test('a paused queue is not waiting for anything', async () => {
		const api = await load('?proxy=waiting');
		const paused = await api.pauseProxyQueue();
		expect(paused.paused).toBe(true);
		expect('waiting_on' in paused).toBe(false);
		expect((await api.resumeProxyQueue()).waiting_on).toBe('an export or another job you started');
	});
});

describe('without a flag the harness builds as it always did', () => {
	test('the seeded build stays at 42 % and a failed proxy shows in the queue', async () => {
		const api = await load('?proxy=failed');
		const q = await api.getProxyQueue();
		expect(q.jobs).toHaveLength(1);
		expect(q.jobs[0]).toMatchObject({ asset_id: INTERVIEW, state: 'building', position: 1, fraction: 0.42 });
		expect(q.failed.map((f) => f.asset_id)).toEqual([BROLL]);
		expect((await api.getProxyStatuses()).find((p) => p.asset_id === INTERVIEW)).toMatchObject({ state: 'building', fraction: 0.42 });
	});

	test('a rebuild starts at once beside the seeded build, and asking for the failed one does too', async () => {
		const api = await load('');
		const started = await api.rebuildProxy(BROLL);
		expect(started).toMatchObject({ state: 'building', fraction: 0 });
		const q = await api.getProxyQueue();
		expect(order(q)).toEqual([INTERVIEW, BROLL]);
		expect(q.jobs.every((j) => j.state === 'building')).toBe(true);
		expect((await api.prioritizeProxy(BROLL)).state).toBe('building');
		const cancelled = await api.cancelProxyQueue();
		expect(cancelled.map((s) => s.asset_id).sort()).toEqual([INTERVIEW, BROLL].sort());
		expect((await api.getProxyQueue()).jobs).toEqual([]);
	});

	test('the seeded sample proxies are the same as ever with no flag at all', async () => {
		const api = await load('');
		const byId = new Map((await api.getProxyStatuses()).map((p) => [p.asset_id, p]));
		expect(byId.get(INTERVIEW)).toMatchObject({ state: 'building', fraction: 0.42, elapsed_secs: 31, eta_secs: 43 });
		expect(byId.get(BROLL)).toMatchObject({ state: 'ready', bytes: 38_400_000 });
		expect(byId.has(DRONE)).toBe(false);
	});

	test('an empty library has an empty queue', async () => {
		const api = await load('?empty=1&proxy=queued');
		expect(await api.getProxyQueue()).toEqual({ jobs: [], remaining: 0, rate: expect.any(Number), pipeline: 'software', paused: false, failed: [] });
	});
});
