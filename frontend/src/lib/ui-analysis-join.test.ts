import { afterAll, beforeAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import { joinsRun } from './analysis-steps';
import type { AnalysisKind } from './types';

// One analysis pass at a time (`ui.runAnalysis`): the same footage asked for again — from the Inspector, the bin or a
// quick edit — joins the pass in flight instead of starting a parallel transcription, and the shared `ui.analyzing`
// is cleared by the pass that ends, not by whichever of two ends first. The backend's analyze is a controllable stub;
// the rest (the editor, the media status) is the browser harness.

const real = { ...(await import('./api')) };
const notifications = { toast: Object.assign(() => {}, { success() {}, warning() {}, error() {}, info() {}, dismiss() {} }) };
mock.module('./notifications.svelte', () => notifications);

type Call = { assetId: string; steps: readonly AnalysisKind[] | undefined; end: (v: 'ok' | Error) => void };
const calls: Call[] = [];
const stub = {
	...real,
	analyzeAsset: (assetId: string, steps?: readonly AnalysisKind[]) =>
		new Promise((resolve, reject) => {
			calls.push({ assetId, steps, end: (v) => (v === 'ok' ? resolve({}) : reject(v)) });
		})
};
beforeAll(() => {
	mock.module('./api', () => stub);
});
afterAll(() => {
	mock.module('./api', () => real);
});

const { ui } = await import('./editor-ui.svelte');
const { editor } = await import('./state.svelte');
const tick = () => new Promise((r) => setTimeout(r, 0));
/** Let everything queued run, as far as it can without the stub's calls ending. */
const settle = async () => {
	for (let i = 0; i < 5; i++) await tick();
};

beforeEach(async () => {
	calls.length = 0;
	await editor.load();
});

describe('joinsRun', () => {
	test('what is asked joins a pass that is running all of it', () => {
		expect(joinsRun(['transcript', 'silence'], ['transcript'])).toBe(true);
		expect(joinsRun(['transcript'], ['transcript'])).toBe(true);
		expect(joinsRun(['silence'], ['transcript'])).toBe(false);
		expect(joinsRun(['transcript'], ['transcript', 'silence'])).toBe(false);
	});

	test('the Settings default set is only known to answer the same default question', () => {
		expect(joinsRun(undefined, undefined)).toBe(true);
		expect(joinsRun(undefined, ['transcript'])).toBe(false);
		expect(joinsRun(['transcript'], undefined)).toBe(false);
	});
});

describe('runAnalysis', () => {
	test('a second press for the same footage joins the pass: one transcription, analyzing held until it ends', async () => {
		const a = ui.runAnalysis('asset-1', ['transcript']);
		const b = ui.runAnalysis('asset-1', ['transcript']);
		await settle();
		expect(calls).toHaveLength(1);
		expect(ui.analyzing).toBe(true);
		expect(ui.analyzingId).toBe('asset-1');
		calls[0].end('ok');
		expect(await Promise.all([a, b])).toEqual([true, true]);
		expect(calls).toHaveLength(1);
		expect(ui.analyzing).toBe(false);
		expect(ui.analyzingId).toBeNull();
	});

	test('what the pass in flight is already doing is joined too', async () => {
		const a = ui.runAnalysis('asset-1', ['transcript', 'silence']);
		const b = ui.runAnalysis('asset-1', ['silence']);
		await settle();
		expect(calls).toHaveLength(1);
		calls[0].end('ok');
		await Promise.all([a, b]);
		expect(calls).toHaveLength(1);
	});

	test('other kinds of the same footage wait their turn and run after it, never beside it', async () => {
		const a = ui.runAnalysis('asset-1', ['silence']);
		const b = ui.runAnalysis('asset-1', ['transcript']);
		await settle();
		expect(calls.map((c) => c.steps)).toEqual([['silence']]);
		calls[0].end('ok');
		await settle();
		expect(calls.map((c) => c.steps)).toEqual([['silence'], ['transcript']]);
		expect(ui.analyzing).toBe(true);
		calls[1].end('ok');
		expect(await Promise.all([a, b])).toEqual([true, true]);
		expect(ui.analyzing).toBe(false);
	});

	test('another asset waits too: the flags are one set, so two passes would clobber them', async () => {
		const a = ui.runAnalysis('asset-1', ['transcript']);
		const b = ui.runAnalysis('asset-2', ['transcript']);
		await settle();
		expect(calls.map((c) => c.assetId)).toEqual(['asset-1']);
		expect(ui.analyzingId).toBe('asset-1');
		calls[0].end('ok');
		await settle();
		expect(calls.map((c) => c.assetId)).toEqual(['asset-1', 'asset-2']);
		expect(ui.analyzingId).toBe('asset-2');
		calls[1].end('ok');
		await Promise.all([a, b]);
		expect(ui.analyzing).toBe(false);
	});

	test('a pass that was stopped is stopped for everyone who joined it', async () => {
		const a = ui.runAnalysis('asset-1', ['transcript']);
		const b = ui.runAnalysis('asset-1', ['transcript']);
		await settle();
		calls[0].end(new Error('analysis cancelled'));
		expect(await Promise.all([a, b])).toEqual([false, false]);
		expect(ui.analyzing).toBe(false);
	});

	test('a failed pass rejects for the joiner too, and the next one starts clean', async () => {
		const a = ui.runAnalysis('asset-1', ['transcript']);
		const b = ui.runAnalysis('asset-1', ['transcript']);
		await settle();
		calls[0].end(new Error('ffmpeg exploded'));
		const seen = await Promise.allSettled([a, b]);
		expect(seen.map((s) => s.status)).toEqual(['rejected', 'rejected']);
		expect(ui.analyzing).toBe(false);
		const c = ui.runAnalysis('asset-1', ['transcript']);
		await settle();
		expect(calls).toHaveLength(2);
		calls[1].end('ok');
		expect(await c).toBe(true);
	});

	test('a quick edit asks what is still missing only once the pass in flight has ended', async () => {
		// The harness's interview is fully analysed: after the pass there is nothing left to run.
		const id = editor.assets[0].id;
		const pass = ui.runAnalysis(id, ['silence']);
		const edit = ui.ensureAnalysis(id, ['silence']);
		await settle();
		expect(calls).toHaveLength(1);
		calls[0].end('ok');
		await Promise.all([pass, edit]);
		expect(calls).toHaveLength(1);
		expect(ui.analyzing).toBe(false);
	});
});
