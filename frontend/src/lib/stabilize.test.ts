import { describe, expect, test } from 'bun:test';
import {
	canStabilize,
	DEFAULT_STABILIZE,
	forgetStopped,
	noteStopped,
	stabilizeSummary,
	statusDetail,
	statusLabel,
	unsteadyNote,
	wantsAnalysis
} from './stabilize';
import type { Asset, Clip, StabilizeStatus, Track } from './types';

const clip = (patch: Partial<Clip> = {}): Clip => ({
	id: 'c',
	asset_id: 'a',
	source_in: 0,
	source_out: 4,
	timeline_start: 0,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...patch
});

const asset = (stream: Record<string, unknown>): Asset =>
	({
		id: 'a',
		path: '/m/a.mp4',
		name: 'a.mp4',
		duration: 10,
		streams: [{ kind: 'video', codec: 'h264', width: 1920, height: 1080, ...stream }],
		imported_at: ''
	}) as unknown as Asset;

const track = (kind: 'video' | 'audio'): Track => ({ id: 't', kind, name: 'V1', clips: [] }) as unknown as Track;

describe('stabilization in the inspector', () => {
	test('only moving footage on a video track is offered, as the backend allows', () => {
		const footage = asset({});
		expect(canStabilize(clip(), track('video'), footage)).toBe(true);
		expect(canStabilize(clip(), track('audio'), footage)).toBe(false);
		expect(canStabilize(clip(), track('video'), asset({ image: true }))).toBe(false);
		expect(canStabilize(clip(), track('video'), asset({ projection: 'equirect' }))).toBe(false);
		expect(canStabilize(clip(), track('video'), undefined)).toBe(false);
		expect(canStabilize(clip({ generator: { kind: 'title', name: 't', svg: '' } }), track('video'), footage)).toBe(false);
	});

	test('the status reads in a few words with a tone, and analysing shows its fraction', () => {
		expect(statusLabel({ state: 'ready' })).toEqual({ text: 'Steady', tone: 'success' });
		expect(statusLabel({ state: 'analyzing', fraction: 0.374 })).toEqual({ text: 'Analysing 37%', tone: 'warning' });
		expect(statusLabel({ state: 'failed', reason: 'x' }).tone).toBe('danger');
		expect(statusLabel(null).text).toBe('Off');
		expect(statusDetail({ state: 'failed', reason: 'no picture' })).toContain('no picture');
		expect(statusDetail({ state: 'unsupported', reason: 'a still image does not move' })).toBe('a still image does not move');
	});

	test('the panel analyses a clip nothing measured, and leaves a failure to the user', () => {
		const states: [StabilizeStatus, boolean][] = [
			[{ state: 'missing' }, true],
			[{ state: 'failed', reason: 'x' }, false],
			[{ state: 'ready' }, false],
			[{ state: 'basic' }, false],
			[{ state: 'analyzing', fraction: 0.1 }, false],
			[{ state: 'queued' }, false],
			[{ state: 'off' }, false]
		];
		for (const [status, wanted] of states) expect(wantsAnalysis(status)).toBe(wanted);
		expect(wantsAnalysis(null)).toBe(false);
	});

	test('an analysis the user stopped is not started again until they ask for it', () => {
		const missing: StabilizeStatus = { state: 'missing' };
		expect(wantsAnalysis(missing, 'k')).toBe(true);
		noteStopped('k');
		expect(wantsAnalysis(missing, 'k')).toBe(false);
		expect(wantsAnalysis(missing, 'other')).toBe(true);
		forgetStopped('k');
		expect(wantsAnalysis(missing, 'k')).toBe(true);
	});

	test('the export dialog names the stabilized clips that will export as shot', () => {
		expect(unsteadyNote([])).toBeNull();
		expect(unsteadyNote([{ clip_id: 'a', state: 'ready' }, { clip_id: 'b', state: 'basic' }])).toBeNull();
		expect(unsteadyNote([{ clip_id: 'a', state: 'missing' }])).toContain('1 stabilized clip exports as shot');
		expect(unsteadyNote([{ clip_id: 'a', state: 'analyzing', fraction: 0.5 }])).toContain('still being measured');
		const two = unsteadyNote([
			{ clip_id: 'a', state: 'failed', reason: 'x' },
			{ clip_id: 'b', state: 'missing' },
			{ clip_id: 'c', state: 'ready' }
		]);
		expect(two).toContain('2 stabilized clips export as shot');
	});

	test('the summary names the strength and where it stands', () => {
		expect(stabilizeSummary(clip(), null)).toBe('Off');
		expect(stabilizeSummary(clip({ stabilize: DEFAULT_STABILIZE }), { state: 'ready' })).toBe('50% · Steady');
		expect(stabilizeSummary(clip({ stabilize: { strength: 1, zoom: 8 } }), { state: 'missing' })).toBe('100% · Needs analysis');
	});
});
