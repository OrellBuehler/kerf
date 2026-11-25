import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { AnalysisKind, AnalysisStatus } from './types';

// A notice's "Try again" cannot be disabled like the menu's entries and the rows' buttons are, so it is gated where it
// acts: one analysis at a time, whoever started it. A press during a run starts nothing and says why; two failed
// steps of one run are one notice, so one button.

const g = globalThis as unknown as { document?: unknown };
const originalDocument = g.document;
g.document = { documentElement: { style: { setProperty() {} }, classList: { toggle() {} } } };
afterAll(() => {
	if (originalDocument === undefined) delete g.document;
	else g.document = originalDocument;
});

const shown: { kind: string; text: string; opts?: { description?: string; action?: { label: string; onClick: () => void } } }[] = [];
const note = (kind: string) => (text: string, opts?: never) => void shown.push({ kind, text, opts });
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), { success: note('success'), warning: note('warning'), error: note('error'), info: note('info'), dismiss: () => {} })
}));

const { ui, RETRY_BUSY } = await import('./editor-ui.svelte');
const { editor } = await import('./state.svelte');
const { mediaStatus } = await import('./media-status.svelte');

const runs: { id: string; steps: readonly AnalysisKind[] | undefined }[] = [];
let release: (() => void) | null = null;
(editor as unknown as { analyze: unknown }).analyze = (id: string, steps?: readonly AnalysisKind[]) => {
	runs.push({ id, steps });
	return new Promise((resolve) => {
		release = () => resolve({});
	});
};

const idle = () => {
	ui.analyzing = false;
	ui.analyzingId = null;
	mediaStatus.analyses = {};
};

beforeEach(() => {
	shown.length = 0;
	runs.length = 0;
	release = null;
	idle();
});

describe('"Try again" on an analysis notice', () => {
	test('starts one run of the steps it names when nothing is going', async () => {
		const done = ui.retryAnalysis('a', ['loudness']);
		expect(runs).toEqual([{ id: 'a', steps: ['loudness'] }]);
		expect(ui.analyzing).toBe(true);
		release?.();
		expect(await done).toBe(true);
		expect(ui.analyzing).toBe(false);
	});

	test('while a run of the page’s own is going it starts nothing and says why', async () => {
		ui.analyzing = true;
		ui.analyzingId = 'b';
		expect(await ui.retryAnalysis('a', ['silence'])).toBe(false);
		expect(runs).toEqual([]);
		expect(shown).toEqual([{ kind: 'info', text: RETRY_BUSY, opts: undefined }]);
	});

	test('a second press right after the first is the same case: one run, not two', async () => {
		const first = ui.retryAnalysis('a', ['loudness']);
		const second = await ui.retryAnalysis('a', ['rhythm']);
		expect(second).toBe(false);
		expect(runs).toHaveLength(1);
		release?.();
		await first;
		// and once it has ended a press is allowed again
		const third = ui.retryAnalysis('a', ['rhythm']);
		expect(runs).toHaveLength(2);
		release?.();
		await third;
	});

	test('a run an agent started — the backend says a step is running — counts too', async () => {
		const running: AnalysisStatus = { asset_id: 'x', kinds: [{ kind: 'silence', state: 'running' }] };
		mediaStatus.analyses = { x: running };
		expect(ui.analysisBusy).toBe(true);
		expect(ui.activeRun).toMatchObject({ id: 'x', own: false });
		expect(await ui.retryAnalysis('a', ['silence'])).toBe(false);
		expect(runs).toEqual([]);
	});
});

describe('the notice for steps that failed', () => {
	const failedTwo: AnalysisStatus = {
		asset_id: 'a',
		kinds: [
			{ kind: 'silence', state: 'failed', reason: 'ffmpeg exited with 1' },
			{ kind: 'scenes', state: 'done' },
			{ kind: 'loudness', state: 'failed', reason: 'no audio stream' },
			{ kind: 'rhythm', state: 'not_run' },
			{ kind: 'transcript', state: 'off' }
		]
	};

	test('two failed steps of one run are one notice, with one Try again that runs both together', async () => {
		mediaStatus.analyses = { a: failedTwo };
		(ui as unknown as { reportFailedSteps: (id: string, steps?: readonly AnalysisKind[]) => void }).reportFailedSteps('a');
		expect(shown).toHaveLength(1);
		expect(shown[0].kind).toBe('error');
		expect(shown[0].text).toBe("Couldn't run 2 analysis steps for that clip");
		expect(shown[0].opts?.description).toBe('Silence: FFmpeg exited with 1 · Loudness: No audio stream');
		// pressing it twice starts one run of both steps, not two runs
		const press = shown[0].opts!.action!.onClick;
		press();
		press();
		expect(runs).toEqual([{ id: 'a', steps: ['silence', 'loudness'] }]);
		expect(shown.at(-1)).toMatchObject({ kind: 'info', text: RETRY_BUSY });
		release?.();
	});

	test('one failed step says which, and only a step this run asked for is news', () => {
		mediaStatus.analyses = { a: failedTwo };
		(ui as unknown as { reportFailedSteps: (id: string, steps?: readonly AnalysisKind[]) => void }).reportFailedSteps('a', ['loudness']);
		expect(shown).toHaveLength(1);
		expect(shown[0].text).toBe("Couldn't measure loudness for that clip");
		expect(shown[0].opts?.description).toBe('No audio stream');
	});
});
