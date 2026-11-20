import { describe, expect, test } from 'bun:test';
import {
	ROW_CAP,
	clockTime,
	failedRows,
	fmtEta,
	fmtRealTime,
	hudLine,
	isQueued,
	metaLine,
	pipelineLabel,
	queueRows,
	queueSentence,
	realTimeSpeed,
	summarize
} from './proxy-queue';
import type { ProxyQueue, QueuedProxy } from './types';

// A queue as `get_proxy_queue` answers it, and the words the panel, the Preview's chip and the status bar make of it. The
// clock is a local date (so the tests hold in any time zone) on a day with no change of clock around it.

const HD = 1920 * 1080;
const NOW = new Date(2026, 10, 20, 14, 38, 0);
const EN = 'en-GB';

const job = (over: Partial<QueuedProxy> & { asset_id: string; position: number }): QueuedProxy => ({
	name: `${over.asset_id}.mp4`,
	state: 'queued',
	fraction: 0,
	duration: 60,
	width: 1920,
	height: 1080,
	pipeline: 'software',
	...over
});

const queueOf = (jobs: QueuedProxy[], over: Partial<ProxyQueue> = {}): ProxyQueue => ({
	jobs,
	remaining: jobs.length,
	pipeline: 'software',
	paused: false,
	failed: [],
	...over
});

/** One build under way and three waiting, at ten times real time over 1080p. */
function busy(over: Partial<ProxyQueue> = {}): ProxyQueue {
	return queueOf(
		[
			job({ asset_id: 'a', position: 1, state: 'building', fraction: 0.5, duration: 100, eta_secs: 5 }),
			job({ asset_id: 'b', position: 2, duration: 60, eta_secs: 11 }),
			job({ asset_id: 'c', position: 3, duration: 600, eta_secs: 71 }),
			job({ asset_id: 'd', position: 4, duration: 30, width: 3840, height: 2160, eta_secs: 83 })
		],
		{ eta_secs: 83, rate: HD * 10, ...over }
	);
}

describe('a time to wait', () => {
	test('is said as roughly as it is known', () => {
		expect(fmtEta(0)).toBe('1 s');
		expect(fmtEta(0.2)).toBe('1 s');
		expect(fmtEta(7)).toBe('7 s');
		expect(fmtEta(59.4)).toBe('59 s');
		expect(fmtEta(59.6)).toBe('1 min');
		expect(fmtEta(90)).toBe('2 min');
		expect(fmtEta(42 * 60)).toBe('42 min');
		expect(fmtEta(59 * 60 + 40)).toBe('1 h');
		expect(fmtEta(65 * 60)).toBe('1 h 05 min');
		expect(fmtEta(2 * 3600)).toBe('2 h');
		// minutes past the first hour go to the nearest five
		expect(fmtEta(67 * 60)).toBe('1 h 05 min');
		expect(fmtEta(68 * 60)).toBe('1 h 10 min');
		expect(fmtEta(9 * 3600 + 59 * 60)).toBe('10 h');
		expect(fmtEta(9 * 3600 + 57 * 60)).toBe('9 h 55 min');
		expect(fmtEta(14 * 3600 + 20 * 60)).toBe('14 h');
		expect(fmtEta(47 * 3600)).toBe('47 h');
		expect(fmtEta(2 * 86400 + 3 * 3600)).toBe('2 d 3 h');
		expect(fmtEta(3 * 86400)).toBe('3 d');
		expect(fmtEta(3 * 86400 - 600)).toBe('3 d');
	});

	test('never negative, and a nonsense number is the smallest', () => {
		expect(fmtEta(-5)).toBe('1 s');
	});
});

describe('the time it will be done', () => {
	test('is the nearest minute, on the clock the user reads', () => {
		expect(clockTime(NOW, 42 * 60, EN)).toBe('15:20');
		// 14:38:00 + 41 min 40 s = 15:19:40, which is 15:20
		expect(clockTime(NOW, 41 * 60 + 40, EN)).toBe('15:20');
		expect(clockTime(NOW, 41 * 60 + 20, EN)).toBe('15:19');
		expect(clockTime(NOW, 0, EN)).toBe('14:38');
	});

	test('says tomorrow, then the weekday, then the date', () => {
		expect(clockTime(NOW, 10 * 3600, EN)).toBe('tomorrow 00:38');
		expect(clockTime(NOW, 24 * 3600 + 3600, EN)).toBe('tomorrow 15:38');
		// Friday 20 November 2026: two days on is a Sunday
		expect(clockTime(NOW, 2 * 86400, EN)).toBe('Sun 14:38');
		expect(clockTime(NOW, 9 * 86400, EN)).toBe('29 Nov 14:38');
	});

	test('follows the locale’s way of writing a clock', () => {
		expect(clockTime(NOW, 42 * 60, 'en-US')).toMatch(/^3:20\s?PM$/);
	});
});

describe('how fast a build runs', () => {
	const building = { state: 'building' as const, width: 1920, height: 1080, pipeline: 'software' as const };
	const rated = { rate: HD * 10, pipeline: 'software' as const };

	test('is the rate the estimates use, over the picture the job is weighed by', () => {
		expect(realTimeSpeed(building, rated)).toBe(10);
		expect(realTimeSpeed({ ...building, width: 3840, height: 2160 }, rated)).toBe(2.5);
	});

	test('is not said without a measured rate, for a job that is waiting, on another pipeline or with no picture size', () => {
		expect(realTimeSpeed(building, { pipeline: 'software' })).toBeNull();
		expect(realTimeSpeed(building, { rate: null, pipeline: 'software' })).toBeNull();
		expect(realTimeSpeed(building, { rate: 0, pipeline: 'software' })).toBeNull();
		expect(realTimeSpeed(building, { rate: NaN, pipeline: 'software' })).toBeNull();
		expect(realTimeSpeed({ ...building, state: 'queued' }, rated)).toBeNull();
		expect(realTimeSpeed({ ...building, pipeline: 'gpu' }, rated)).toBeNull();
		expect(realTimeSpeed({ ...building, width: 0 }, rated)).toBeNull();
		expect(realTimeSpeed({ ...building, height: 0 }, rated)).toBeNull();
	});

	test('reads as a multiple of real time', () => {
		expect(fmtRealTime(10)).toBe('10× real time');
		expect(fmtRealTime(12.4)).toBe('12× real time');
		expect(fmtRealTime(2.5)).toBe('2.5× real time');
		expect(fmtRealTime(1)).toBe('1× real time');
		expect(fmtRealTime(0.84)).toBe('0.8× real time');
		expect(fmtRealTime(0.04)).toBe('<0.1× real time');
	});
});

describe('the header', () => {
	test('before the first read it says so, with nothing queued it says that', () => {
		expect(summarize(null, NOW, EN)).toMatchObject({ tone: 'loading', count: 'Reading the queue…', time: '', note: '' });
		expect(summarize(queueOf([]), NOW, EN)).toMatchObject({ tone: 'empty', count: 'No proxies queued', time: '', note: '' });
	});

	test('a pause outlasts an empty queue, and says what it holds back', () => {
		expect(summarize(queueOf([], { paused: true }), NOW, EN)).toEqual({
			tone: 'paused',
			count: 'No proxies queued',
			time: '',
			note: 'Paused. Proxies queued from now on wait until you resume.'
		});
	});

	test('a queue under way says how long, and when it will be done', () => {
		expect(summarize(busy(), NOW, EN)).toEqual({ tone: 'building', count: '4 proxies', time: 'about 1 min, done ~14:39', note: '' });
		const long = busy({ eta_secs: 42 * 60 });
		expect(summarize(long, NOW, EN).time).toBe('about 42 min, done ~15:20');
	});

	test('with no rate yet it estimates, and does not guess', () => {
		const q = busy({ eta_secs: undefined, rate: undefined });
		expect(summarize(q, NOW, EN)).toMatchObject({ tone: 'building', time: 'estimating…' });
		expect(summarize(busy({ eta_secs: null }), NOW, EN).time).toBe('estimating…');
	});

	test('one proxy is not "1 proxies"', () => {
		expect(summarize(queueOf([job({ asset_id: 'a', position: 1 })]), NOW, EN).count).toBe('1 proxy');
	});

	test('paused: no time (the engine gives none), and the note says what still happens', () => {
		const paused = summarize(busy({ paused: true, eta_secs: undefined }), NOW, EN);
		expect(paused).toMatchObject({ tone: 'paused', time: '' });
		expect(paused.note).toBe('Paused. The build under way finishes; nothing new starts until you resume.');
		const idle = queueOf([job({ asset_id: 'a', position: 1 })], { paused: true });
		expect(summarize(idle, NOW, EN).note).toBe('Paused. Nothing starts until you resume.');
	});

	test('waiting on another job: says on what, and puts the time after the wait with no clock time', () => {
		const waiting = summarize(busy({ waiting_on: 'an export or another job you started' }), NOW, EN);
		expect(waiting.tone).toBe('waiting');
		expect(waiting.note).toBe('Waiting on an export or another job you started.');
		expect(waiting.time).toBe('then about 1 min');
		const unknown = summarize(busy({ waiting_on: 'another proxy', eta_secs: undefined, rate: undefined }), NOW, EN);
		expect(unknown).toMatchObject({ tone: 'waiting', time: '', note: 'Waiting on another proxy.' });
	});

	test('a pause outranks waiting: a paused queue is not waiting for anything', () => {
		expect(summarize(busy({ paused: true, waiting_on: 'an export' }), NOW, EN).tone).toBe('paused');
	});
});

describe('what the Preview’s chip adds', () => {
	test('how many are queued, and how long when that is known', () => {
		expect(hudLine(busy({ eta_secs: 42 * 60 }))).toBe('4 in the queue · about 42 min');
		expect(hudLine(busy({ eta_secs: undefined }))).toBe('4 in the queue');
	});

	test('a paused queue says so and gives no time; one held up by another job does not promise one', () => {
		expect(hudLine(busy({ paused: true, eta_secs: undefined }))).toBe('4 in the queue · paused');
		expect(hudLine(busy({ waiting_on: 'an export' }))).toBe('4 in the queue');
	});

	test('nothing to say with nothing read or nothing queued', () => {
		expect(hudLine(null)).toBeNull();
		expect(hudLine(queueOf([]))).toBeNull();
	});
});

describe('a sentence for a tooltip', () => {
	test('is the count with the time, the note, or neither', () => {
		expect(queueSentence(busy({ eta_secs: 42 * 60 }), NOW, EN)).toBe('4 proxies in the queue: about 42 min, done ~15:20.');
		expect(queueSentence(busy({ eta_secs: undefined }), NOW, EN)).toBe('4 proxies in the queue.');
		expect(queueSentence(busy({ paused: true }), NOW, EN)).toBe('4 proxies in the queue. Paused. The build under way finishes; nothing new starts until you resume.');
		expect(queueSentence(null, NOW, EN)).toBe('');
		expect(queueSentence(queueOf([]), NOW, EN)).toBe('');
	});
});

describe('the rows', () => {
	test('come in the order the jobs run, each with its place and what it is', () => {
		const { rows, more } = queueRows(busy(), NOW, ROW_CAP, EN);
		expect(more).toBe(0);
		expect(rows.map((r) => [r.id, r.position, r.state])).toEqual([
			['a', 1, 'building'],
			['b', 2, 'queued'],
			['c', 3, 'queued'],
			['d', 4, 'queued']
		]);
		expect(rows.map((r) => r.meta)).toEqual(['1920×1080 · 1:40', '1920×1080 · 1:00', '1920×1080 · 10:00', '3840×2160 · 0:30']);
	});

	test('each has its own time: a build says what is left, a job waiting when it is done', () => {
		const { rows } = queueRows(busy(), NOW, ROW_CAP, EN);
		expect(rows.map((r) => r.eta)).toEqual(['5 s left', 'done in 11 s', 'done in 1 min', 'done in 1 min']);
		expect(rows[1].etaTitle).toBe('Done in about 11 s, around 14:38, counting the jobs before it');
		expect(rows[0].etaTitle).toBe('Done in about 5 s, around 14:38');
	});

	test('a job with no estimate makes none up: a build says it is estimating, a job waiting says nothing, and the tip says why', () => {
		const q = busy({ paused: true, eta_secs: undefined });
		q.jobs = q.jobs.map((j) => (j.state === 'queued' ? { ...j, eta_secs: undefined } : j));
		const { rows } = queueRows(q, NOW, ROW_CAP, EN);
		expect(rows.map((r) => r.eta)).toEqual(['5 s left', '', '', '']);
		expect(rows[1].etaTitle).toBe('No estimate while the queue is paused');
		const fresh = queueOf([job({ asset_id: 'a', position: 1, state: 'building', fraction: 0.01 }), job({ asset_id: 'b', position: 2 })]);
		const rowsFresh = queueRows(fresh, NOW, ROW_CAP, EN).rows;
		expect(rowsFresh.map((r) => r.eta)).toEqual(['estimating…', '']);
		expect(rowsFresh.map((r) => r.etaTitle)).toEqual(['No estimate for this build yet', 'No estimate yet']);
	});

	test('a build under way says how far, and how fast only when that is honest', () => {
		expect(queueRows(busy(), NOW, ROW_CAP, EN).rows[0].progress).toBe('50% · about 10× real time');
		expect(queueRows(busy({ rate: undefined }), NOW, ROW_CAP, EN).rows[0].progress).toBe('50%');
		// A job that is waiting has no progress to speak of.
		expect(queueRows(busy(), NOW, ROW_CAP, EN).rows[1].progress).toBe('');
	});

	test('a build that runs on another pipeline than the queue says which, and no speed', () => {
		const q = busy();
		q.jobs[0] = { ...q.jobs[0], pipeline: 'gpu' };
		expect(queueRows(q, NOW, ROW_CAP, EN).rows[0].progress).toBe('50% · Fast (GPU)');
	});

	test('Build next is offered to the jobs waiting behind the first one waiting — not to it, and not to a build', () => {
		expect(queueRows(busy(), NOW, ROW_CAP, EN).rows.map((r) => r.canPrioritize)).toEqual([false, false, true, true]);
		// Two builds under way: the first job waiting is still the first.
		const two = queueOf([
			job({ asset_id: 'a', position: 1, state: 'building' }),
			job({ asset_id: 'b', position: 2, state: 'building' }),
			job({ asset_id: 'c', position: 3 }),
			job({ asset_id: 'd', position: 4 })
		]);
		expect(queueRows(two, NOW, ROW_CAP, EN).rows.map((r) => r.canPrioritize)).toEqual([false, false, false, true]);
		// Nothing building: the first of the waiting is next already.
		const none = queueOf([job({ asset_id: 'c', position: 1 }), job({ asset_id: 'd', position: 2 })]);
		expect(queueRows(none, NOW, ROW_CAP, EN).rows.map((r) => r.canPrioritize)).toEqual([false, true]);
	});

	test('a queue of hundreds is cut to a window, and says how many it left out', () => {
		const many = queueOf(Array.from({ length: 214 }, (_, i) => job({ asset_id: `j${i}`, position: i + 1 })));
		const first = queueRows(many, NOW);
		expect(first.rows).toHaveLength(ROW_CAP);
		expect(first.more).toBe(114);
		expect(first.rows.at(-1)!.id).toBe('j99');
		const wider = queueRows(many, NOW, 2 * ROW_CAP);
		expect(wider.rows).toHaveLength(200);
		expect(wider.more).toBe(14);
		expect(queueRows(many, NOW, 500).more).toBe(0);
		expect(queueRows(many, NOW, 0)).toEqual({ rows: [], more: 214 });
	});

	test('with no queue there are none', () => {
		expect(queueRows(null, NOW)).toEqual({ rows: [], more: 0 });
	});

	test('a source that says nothing of its size or length is not given a made-up one', () => {
		expect(metaLine({ width: 0, height: 0, duration: 90 })).toBe('1:30');
		expect(metaLine({ width: 1920, height: 1080, duration: 0 })).toBe('1920×1080');
		expect(metaLine({ width: 0, height: 0, duration: NaN })).toBe('');
	});
});

describe('what failed', () => {
	test('is the name and the reason as a sentence', () => {
		const q = busy({
			failed: [
				{ asset_id: 'x', name: 'corrupt-take.mp4', reason: 'media engine error: could not generate preview proxy: Invalid data found' },
				{ asset_id: 'y', name: 'blank.mp4', reason: '' }
			]
		});
		expect(failedRows(q)).toEqual([
			{ id: 'x', name: 'corrupt-take.mp4', reason: 'Could not generate preview proxy: Invalid data found' },
			{ id: 'y', name: 'blank.mp4', reason: 'The proxy could not be built' }
		]);
		expect(failedRows(null)).toEqual([]);
	});
});

describe('naming', () => {
	test('the two pipelines', () => {
		expect(pipelineLabel('software')).toBe('Software');
		expect(pipelineLabel('gpu')).toBe('Fast (GPU)');
	});

	test('a proxy is in the queue while it is building or waiting', () => {
		expect(['building', 'queued'].every((s) => isQueued(s as 'building' | 'queued'))).toBe(true);
		for (const s of ['ready', 'failed', 'missing', 'off', 'not_needed', undefined] as const) expect(isQueued(s)).toBe(false);
	});
});
