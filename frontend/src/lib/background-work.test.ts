import { describe, expect, test } from 'bun:test';
import { WORK_GLOSS, assetWork, isBusy, libraryWork, workNote } from './background-work';
import type { AnalysisStatus, ProxyStatus } from './types';

const proxy = (state: ProxyStatus['state'], extra: Partial<ProxyStatus> = {}): ProxyStatus => ({ asset_id: 'a', state, ...extra });
const none = { proxy: undefined, run: null, waiting: false, failed: [] } as const;

describe('the background work of one asset', () => {
	test('a proxy being built says how far and how long, and is going', () => {
		const [p] = assetWork({ ...none, proxy: proxy('building', { fraction: 0.426, eta_secs: 7 }) });
		expect(p).toMatchObject({ id: 'proxy', label: 'Proxy', tone: 'running', text: 'building 43% · about 7 s left' });
		// no estimate yet: no time claimed
		expect(assetWork({ ...none, proxy: proxy('building', { fraction: 0 }) })[0].text).toBe('building 0%');
	});

	test('a queued proxy says what it waits for', () => {
		expect(assetWork({ ...none, proxy: proxy('queued') })[0]).toMatchObject({ tone: 'queued', text: 'queued — built before any analysis that is waiting' });
	});

	test('a failed proxy says what failed, what plays meanwhile and offers the retry', () => {
		const [p] = assetWork({ ...none, proxy: proxy('failed', { reason: 'could not generate preview proxy: Invalid data' }) });
		expect(p).toMatchObject({ id: 'proxy-failed', tone: 'failed', fix: 'retry-proxy' });
		expect(p.detail).toBe('Could not generate preview proxy: Invalid data. Previews play the original instead.');
		expect(assetWork({ ...none, proxy: proxy('failed') })[0].detail).toBe('Previews play the original instead.');
		// a reason that already ends in a full stop does not get a second
		expect(assetWork({ ...none, proxy: proxy('failed', { reason: 'out of disk space.' }) })[0].detail).toBe('Out of disk space. Previews play the original instead.');
	});

	test('a proxy at rest — ready, off, not built, not needed — is no line', () => {
		for (const state of ['ready', 'off', 'missing', 'not_needed'] as const) expect(assetWork({ ...none, proxy: proxy(state) }), state).toEqual([]);
		expect(assetWork(none)).toEqual([]);
	});

	test('an analysis run is its progress line, with the files behind it counted', () => {
		const run = { label: 'step 1 of 4 · detecting silence 35%', queued: 2 };
		expect(assetWork({ ...none, run })[0]).toMatchObject({ id: 'analysis', tone: 'running', text: 'step 1 of 4 · detecting silence 35% · 2 more queued' });
		expect(assetWork({ ...none, run: { ...run, queued: 0 } })[0].text).toBe('step 1 of 4 · detecting silence 35%');
	});

	test('waiting behind another file is queued; a failure names the steps, the reason and the retry', () => {
		expect(assetWork({ ...none, waiting: true })[0]).toMatchObject({ tone: 'queued', text: 'waiting its turn — one file is analyzed at a time' });
		const [f] = assetWork({ ...none, failed: [{ label: 'Loudness', reason: 'ffmpeg exited with 1' }, { label: 'Rhythm', reason: null }] });
		expect(f).toMatchObject({ id: 'analysis-failed', tone: 'failed', text: 'Loudness, Rhythm failed', fix: 'retry-analysis' });
		expect(f.detail).toBe('FFmpeg exited with 1. The other steps still ran.');
	});

	test('a running pass outranks a failure of an older one; both a proxy and an analysis can show', () => {
		const items = assetWork({
			proxy: proxy('building', { fraction: 0.2 }),
			run: { label: 'transcribing', queued: 0 },
			waiting: false,
			failed: [{ label: 'Silence', reason: null }]
		});
		expect(items.map((w) => w.id)).toEqual(['proxy', 'analysis']);
	});
});

describe('whether to wait', () => {
	test('while something goes, the person is told to start cutting; a failure alone is not a wait', () => {
		const going = assetWork({ ...none, proxy: proxy('building', { fraction: 0.1 }) });
		expect(isBusy(going)).toBe(true);
		expect(workNote(going)).toMatch(/^You can start cutting now: editing never waits/);
		expect(workNote(going)).toContain('Previews play the original until the proxy is ready');
		const onlyAnalysis = assetWork({ ...none, run: { label: 'x', queued: 0 } });
		expect(workNote(onlyAnalysis)).toBe('You can start cutting now: editing never waits for the analysis.');
		const failed = assetWork({ ...none, proxy: proxy('failed') });
		expect(isBusy(failed)).toBe(false);
		expect(workNote(failed)).toBe('');
		expect(workNote([])).toBe('');
	});
});

describe('what the two are for', () => {
	test('one line names both and what each gives', () => {
		expect(WORK_GLOSS).toContain('proxy is a light copy');
		expect(WORK_GLOSS).toContain('silences, the beat and the speech');
	});
});

describe('the same lines for the library', () => {
	const assets = [
		{ id: 'a', name: 'interview.mp4' },
		{ id: 'b', name: 'broll.mp4' },
		{ id: 'c', name: 'drone.mp4' }
	];
	const st = (id: string, state: ProxyStatus['state'], extra: Partial<ProxyStatus> = {}): ProxyStatus => ({ asset_id: id, state, ...extra });
	const analysis = (id: string, failed = false): AnalysisStatus => ({
		asset_id: id,
		kinds: [{ kind: 'silence', state: failed ? 'failed' : 'done' }]
	});
	const input = (over: Partial<Parameters<typeof libraryWork>[0]> = {}) => ({ assets, proxies: {}, analyses: {}, run: null, ...over });

	test('the first proxy is named with its progress and the rest counted', () => {
		const lines = libraryWork(input({ proxies: { a: st('a', 'building', { fraction: 0.42 }), b: st('b', 'queued'), c: st('c', 'queued') } }));
		expect(lines).toEqual([{ id: 'proxy', label: 'Proxy', tone: 'running', text: 'interview.mp4 · building 42% · 2 more waiting' }]);
	});

	test('proxies that all wait say how many', () => {
		expect(libraryWork(input({ proxies: { a: st('a', 'queued') } }))[0]).toMatchObject({ tone: 'queued', text: '1 file waits its turn' });
		expect(libraryWork(input({ proxies: { a: st('a', 'queued'), b: st('b', 'queued') } }))[0].text).toBe('2 files wait its turn');
	});

	test('the analysis run names its file, its step and what is behind it', () => {
		const lines = libraryWork(input({ run: { id: 'b', label: 'step 1 of 4 · detecting silence 35%', queued: 1 } }));
		expect(lines).toEqual([
			{ id: 'analysis', label: 'Analysis', tone: 'running', text: 'broll.mp4 · step 1 of 4 · detecting silence 35% · 1 more queued' }
		]);
	});

	test('failures are lines of their own, with where to go', () => {
		const lines = libraryWork(input({ proxies: { a: st('a', 'failed'), b: st('b', 'failed') }, analyses: { c: analysis('c', true) } }));
		expect(lines.map((l) => [l.id, l.text])).toEqual([
			['proxy-failed', '2 files failed'],
			['analysis-failed', 'drone.mp4 failed']
		]);
		expect(lines[0].detail).toContain('Select it in the library');
	});

	test('the file being analyzed now is not also a failure of an older run; ready and done are no lines', () => {
		expect(libraryWork(input({ run: { id: 'c', label: null, queued: 0 }, analyses: { c: analysis('c', true) } })).map((l) => l.id)).toEqual(['analysis']);
		expect(libraryWork(input({ proxies: { a: st('a', 'ready') }, analyses: { a: analysis('a') } }))).toEqual([]);
		expect(libraryWork(input({ assets: [] }))).toEqual([]);
	});
});
