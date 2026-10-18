import { describe, expect, test } from 'bun:test';
import { createContextReporter, uiContext, type Timers, type UiContextInput } from './editor-context';
import type { UiContext } from './types';

/** Timers the test fires by hand. */
function fakeTimers() {
	let next = 1;
	const timeouts = new Map<number, () => void>();
	const intervals = new Map<number, () => void>();
	const timers: Timers = {
		setTimeout: (fn) => (timeouts.set(next, fn), next++),
		clearTimeout: (id) => void timeouts.delete(id as number),
		setInterval: (fn) => (intervals.set(next, fn), next++),
		clearInterval: (id) => void intervals.delete(id as number)
	};
	return {
		timers,
		pending: () => timeouts.size,
		fire() {
			const due = [...timeouts.values()];
			timeouts.clear();
			for (const fn of due) fn();
		},
		beat: () => [...intervals.values()].forEach((fn) => fn()),
		intervals: () => intervals.size
	};
}

const base: UiContextInput = {
	time: 0,
	selectedClipIds: [],
	markIn: null,
	markOut: null,
	view: null,
	panel: null,
	assetId: null
};

describe('uiContext', () => {
	test('maps the editor state onto the backend shape, in milliseconds', () => {
		expect(
			uiContext({
				time: 12.34567,
				selectedClipIds: ['a', 'b'],
				markIn: 1.00001,
				markOut: null,
				view: { start: -0.2, end: 30.12345 },
				panel: 'timeline',
				assetId: 'x'
			})
		).toEqual({
			playhead: 12.346,
			selected_clip_ids: ['a', 'b'],
			mark_in: 1,
			mark_out: null,
			visible_range: { start: 0, end: 30.123 },
			focused_panel: 'timeline',
			selected_asset_id: 'x'
		});
	});

	test('a number the backend could not parse is never sent', () => {
		const c = uiContext({ ...base, time: NaN, markIn: Infinity, view: { start: 0, end: Infinity } });
		expect(c.playhead).toBe(0);
		expect(c.mark_in).toBeNull();
		expect(c.visible_range).toBeNull();
	});
});

describe('createContextReporter', () => {
	const at = (t: number): UiContext => uiContext({ ...base, time: t });

	test('a burst of updates sends once, the latest', () => {
		const t = fakeTimers();
		const sent: UiContext[] = [];
		const r = createContextReporter((c) => sent.push(c), t.timers);
		r.update(at(1));
		r.update(at(2));
		r.update(at(3));
		expect(sent).toEqual([]);
		expect(t.pending()).toBe(1);
		t.fire();
		expect(sent.map((c) => c.playhead)).toEqual([3]);
	});

	test('an unchanged context is not sent again, but the heartbeat re-sends it', () => {
		const t = fakeTimers();
		const sent: UiContext[] = [];
		const r = createContextReporter((c) => sent.push(c), t.timers);
		t.beat();
		expect(sent).toEqual([]);
		r.update(at(1));
		t.fire();
		r.update(at(1));
		t.fire();
		expect(sent.length).toBe(1);
		t.beat();
		expect(sent.length).toBe(2);
	});

	test('stop cancels what is pending and the heartbeat', () => {
		const t = fakeTimers();
		const sent: UiContext[] = [];
		const r = createContextReporter((c) => sent.push(c), t.timers);
		r.update(at(1));
		r.stop();
		expect(t.pending()).toBe(0);
		expect(t.intervals()).toBe(0);
		expect(sent).toEqual([]);
	});
});
