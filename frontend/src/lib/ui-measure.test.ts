import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

// The Mixer's Measure button is `ui.measureLevels()`: it runs `get_levels` over the
// whole cut or the in / out range, keeps the answer (and what it was an answer *about*)
// where a rebuilt panel can find it, and refuses to start a second pass while one runs.
// Here it runs over the browser harness's stand-in for the backend, with the
// toasts standing in for what only a window shows.

const real = { ...(await import('./api')) };
const calls: unknown[][] = [];
let fail: Error | null = null;
let gate: Promise<void> | null = null;

mock.module('./api', () => ({
	...real,
	getLevels: async (...args: Parameters<typeof real.getLevels>) => {
		calls.push(args);
		if (gate) await gate;
		if (fail) throw fail;
		return real.getLevels(...args);
	}
}));
const shown: { kind: string; text: string }[] = [];
const note = (kind: string) => (text: string) => void shown.push({ kind, text });
// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		warning: note('warning'),
		error: note('error'),
		info: note('info'),
		dismiss: () => {}
	})
}));

afterAll(() => {
	mock.module('./api', () => real);
});

const { editor } = await import('./state.svelte');
const { ui } = await import('./editor-ui.svelte');
const { isStale } = await import('./levels-view');
const { revertTo } = real;

beforeEach(async () => {
	await revertTo(0);
	await editor.load();
	calls.length = 0;
	shown.length = 0;
	fail = null;
	gate = null;
	ui.markIn = null;
	ui.markOut = null;
	ui.measure = { running: false, result: null, error: null };
});

const now = () => ({ seq: editor.history.find((r) => r.current)?.seq ?? null, path: editor.currentPath });

describe('ui.measureLevels', () => {
	test('measures the whole cut when there are no marks, and keeps the answer', async () => {
		await ui.measureLevels();
		expect(calls).toEqual([[null]]);
		const r = ui.measure.result!;
		expect(r.range).toBeNull();
		expect(r.levels.estimated).toBe(true); // the harness says it is an estimate
		expect(r.levels.master?.integrated_lufs).not.toBeNull();
		expect(ui.measure.running).toBe(false);
		expect(ui.measure.error).toBeNull();
	});

	test('measures the in / out range when both marks are set — the export dialog’s rule', async () => {
		ui.markIn = 2;
		ui.markOut = 6;
		await ui.measureLevels();
		expect(calls).toEqual([[{ start: 2, end: 6 }]]);
		expect(ui.measure.result!.range).toEqual({ start: 2, end: 6 });
		expect(ui.measure.result!.levels.duration).toBe(4);
		// One mark alone is not a range.
		ui.markOut = null;
		await ui.measureLevels();
		expect(calls.at(-1)).toEqual([null]);
		expect(ui.measure.result!.range).toBeNull();
	});

	test('is running while it waits, and a second press does nothing', async () => {
		let release!: () => void;
		gate = new Promise<void>((r) => (release = r));
		const first = ui.measureLevels();
		expect(ui.measure.running).toBe(true);
		await ui.measureLevels();
		expect(calls).toHaveLength(1);
		release();
		await first;
		expect(ui.measure.running).toBe(false);
		expect(ui.measure.result).not.toBeNull();
	});

	test('a failure is kept, told to the user, and leaves the last answer in place', async () => {
		await ui.measureLevels();
		const kept = ui.measure.result;
		fail = new Error('ffmpeg was not found');
		await ui.measureLevels();
		expect(ui.measure.error).toBe('ffmpeg was not found');
		expect(ui.measure.running).toBe(false);
		expect(ui.measure.result).toBe(kept);
		expect(shown).toEqual([{ kind: 'error', text: "Couldn't measure the loudness — ffmpeg was not found" }]);
		// The next try starts clean.
		fail = null;
		await ui.measureLevels();
		expect(ui.measure.error).toBeNull();
	});

	test('an answer goes out of date with the next edit, and not before', async () => {
		await ui.measureLevels();
		const stamp = ui.measure.result!.stamp;
		expect(isStale(stamp, now())).toBe(false);
		await editor.setTrackVolume('a1', 0.5);
		expect(isStale(stamp, now())).toBe(true);
		// An undo is a different revision too: the measurement is of neither state it can be in.
		await editor.undo();
		expect(isStale(stamp, now())).toBe(false); // back at the revision it measured
	});
});
