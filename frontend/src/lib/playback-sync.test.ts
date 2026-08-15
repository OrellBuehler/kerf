import { describe, expect, test } from 'bun:test';
import { createFrameGate, RESYNC_AFTER, STALE_AFTER } from './playback-sync';

describe('createFrameGate', () => {
	test('shows every frame of a stream with a constant transport cost', () => {
		// The regression: ffmpeg startup + base64 + IPC put ~90 ms on every frame,
		// which is more than the whole two-frame budget. Judged against zero this
		// dropped all of them and the preview stayed on its last still.
		const verdict = createFrameGate();
		const seen = Array.from({ length: 240 }, () => verdict(0.09));
		expect(seen.every((v) => v === 'show')).toBe(true);
	});

	test('adapts when the first frame is slower than the rest', () => {
		// ffmpeg's spawn lands on frame one only, so the floor has to come down to
		// the steady-state cost instead of granting that head start forever.
		const verdict = createFrameGate();
		expect(verdict(0.3)).toBe('show');
		expect(verdict(0.09)).toBe('show');
		// Drift is now measured from 0.09, so 0.09 + a bit over two frames is stale.
		expect(verdict(0.09 + STALE_AFTER + 0.01)).toBe('skip');
		expect(verdict(0.1)).toBe('show');
	});

	test('skips frames that drift past the budget and shows them again on recovery', () => {
		const verdict = createFrameGate();
		verdict(0.05);
		expect(verdict(0.05 + STALE_AFTER / 2)).toBe('show');
		expect(verdict(0.05 + STALE_AFTER * 2)).toBe('skip');
		expect(verdict(0.06)).toBe('show');
	});

	test('resyncs once compositing falls a second behind', () => {
		const verdict = createFrameGate();
		verdict(0.09);
		expect(verdict(0.09 + RESYNC_AFTER + 0.01)).toBe('resync');
	});

	test('shows a frame that arrives ahead of the clock', () => {
		// A frame the clock has not reached yet is early, not stale; it also lowers
		// the floor, since it is a cheaper transport than anything seen so far.
		const verdict = createFrameGate();
		verdict(0.09);
		expect(verdict(-0.01)).toBe('show');
		expect(verdict(0.09)).toBe('skip');
	});

	test('does not carry a floor across streams', () => {
		// Each play/seek spawns its own ffmpeg with its own startup cost.
		const first = createFrameGate();
		first(0.02);
		const second = createFrameGate();
		expect(second(0.5)).toBe('show');
	});
});
