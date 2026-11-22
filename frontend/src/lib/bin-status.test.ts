import { describe, expect, test } from 'bun:test';
import { analysisStatePill, mergedStatus, proxyPill, type StatePill } from './bin-status';
import { analysisChips, analysisMeter, chipsFor } from './analysis-steps';
import { proxyBadge } from './proxy-info';
import type { AnalysisKind, AnalysisState, AnalysisStatus, ProxyStatus } from './types';

const proxy = (state: ProxyStatus['state'], extra: Partial<ProxyStatus> = {}) => {
	const status: ProxyStatus = { asset_id: 'a', state, ...extra };
	return proxyPill(proxyBadge(status), status);
};
const kinds: AnalysisKind[] = ['silence', 'scenes', 'loudness', 'rhythm', 'transcript'];
const analysis = (states: Partial<Record<AnalysisKind, AnalysisState>>, opts: { queued?: boolean; analyzing?: boolean } = {}) => {
	const status: AnalysisStatus = { asset_id: 'a', kinds: kinds.map((kind) => ({ kind, state: states[kind] ?? 'not_run' })) };
	return analysisStatePill(analysisMeter(chipsFor(analysisChips(status), { audio: true, image: false, video: true }), opts));
};
const done = Object.fromEntries(kinds.map((k) => [k, 'done' as const]));
const off = Object.fromEntries(kinds.map((k) => [k, 'off' as const]));

describe('the two states as pills', () => {
	test('a proxy: building runs, queued waits, ready is done, failed fails, the rest are off — and a still has none', () => {
		expect(proxy('building', { fraction: 0.42 })).toMatchObject({ id: 'proxy', text: 'proxy 42%', look: 'running', queue: true });
		expect(proxy('queued')).toMatchObject({ look: 'queued', queue: true });
		expect(proxy('ready')).toMatchObject({ text: 'proxy ready', look: 'done', queue: false });
		expect(proxy('failed')).toMatchObject({ look: 'failed' });
		expect(proxy('missing')).toMatchObject({ look: 'off' });
		expect(proxy('off')).toMatchObject({ look: 'off' });
		expect(proxy('not_needed')).toBeNull();
	});

	test('an analysis: running, queued, failed, done, not run and off', () => {
		expect(analysis({}, { analyzing: true })).toMatchObject({ id: 'analysis', text: 'analyzing…', look: 'running' });
		expect(analysis({}, { queued: true })).toMatchObject({ text: 'analysis queued', look: 'queued' });
		expect(analysis({ silence: 'failed' })).toMatchObject({ text: 'analysis failed', look: 'failed' });
		expect(analysis(done)).toMatchObject({ text: 'analyzed', look: 'done' });
		expect(analysis({})).toMatchObject({ text: 'not analyzed', look: 'idle' });
		expect(analysis(off)).toMatchObject({ text: 'analysis off', look: 'off' });
		expect(analysisStatePill(null)).toBeNull();
	});
});

describe('a narrow row draws one pill', () => {
	const only = (p: StatePill | null) => p as StatePill;

	test('the neediest of the two leads, and the other is counted', () => {
		const m = mergedStatus([proxy('building', { fraction: 0.2 }), analysis({}, { analyzing: true })])!;
		expect(m.head.id).toBe('proxy');
		expect(m.more).toBe(1);
		expect(m.others.map((p) => p.id)).toEqual(['analysis']);
		// a failure beats a running one
		const f = mergedStatus([proxy('building', { fraction: 0.2 }), analysis({ silence: 'failed' })])!;
		expect(f.head.look).toBe('failed');
		expect(f.head.id).toBe('analysis');
		expect(f.more).toBe(1);
		// a queued one beats one not run
		expect(mergedStatus([analysis({}), proxy('queued')])!.head.id).toBe('proxy');
	});

	test('both at rest read ready, with both in the tooltip and nothing counted', () => {
		const m = mergedStatus([proxy('ready'), analysis(done)])!;
		expect(m.head).toMatchObject({ text: 'ready', look: 'done' });
		expect(m.head.title).toContain('Preview proxy ready');
		expect(m.head.title).toContain('Analysis — 5 of 5 done');
		expect(m.more).toBe(0);
	});

	test('one at rest and one waiting for the person: the one that waits, and the finished one is not counted', () => {
		const m = mergedStatus([proxy('ready'), analysis({})])!;
		expect(m.head.text).toBe('not analyzed');
		expect(m.more).toBe(0);
	});

	test('a file with one pill keeps it as it is; with none, nothing', () => {
		const song = mergedStatus([null, analysis(done)])!;
		expect(song.head).toMatchObject({ text: 'analyzed', look: 'done' });
		expect(song.more).toBe(0);
		expect(mergedStatus([null, null])).toBeNull();
		expect(only(proxy('ready')).id).toBe('proxy');
	});
});
