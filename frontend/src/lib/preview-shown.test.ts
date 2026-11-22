import { describe, expect, test } from 'bun:test';
import { picturePlays, pumpWidth, rendererNote } from './preview-shown';

// The Preview is a panel like the others and may be closed or sit behind another tab (`preview-shown.ts`). What it then asks of
// the backend and says in the status bar is decided by these three functions; that the pump really asks nothing for a
// panel that is not on show is `frame-pump.test.ts`, and what was seen in the browser harness with `?gpusurface=1` is in the
// doc's account of the dock.

describe('the width the pump may ask for', () => {
	test('is the frame’s while the panel is on show, and none while it is not', () => {
		expect(pumpWidth(true, 1440)).toBe(1440);
		expect(pumpWidth(false, 1440)).toBeNull();
	});

	test('an unmeasured frame is unmeasured either way', () => {
		expect(pumpWidth(true, null)).toBeNull();
		expect(pumpWidth(false, null)).toBeNull();
	});
});

describe('whether the panel plays the picture', () => {
	const on = { playing: true, rate: 1, hasClips: true, shown: true };

	test('a forward 1x play of a cut with clips, in a panel on show', () => {
		expect(picturePlays(on)).toBe(true);
	});

	test('not behind another tab: the stream and the GPU session it had end, and the sound is not this’s to stop', () => {
		expect(picturePlays({ ...on, shown: false })).toBe(false);
	});

	test('not when paused, at another rate, or with nothing to play', () => {
		expect(picturePlays({ ...on, playing: false })).toBe(false);
		for (const rate of [2, 4, 8, -1, -2]) expect(picturePlays({ ...on, rate })).toBe(false);
		expect(picturePlays({ ...on, hasClips: false })).toBe(false);
	});

	test('every one of the four has to hold', () => {
		for (const off of ['playing', 'hasClips', 'shown'] as const) expect(picturePlays({ ...on, [off]: false })).toBe(false);
	});
});

describe('what the status bar says about the renderer', () => {
	const shown = { enabled: true, shown: true, via: 'jpeg' as const, why: 'this platform has no preview surface' };

	test('the JPEG route’s reason while the panel is on show', () => {
		expect(rendererNote(shown)).toEqual({ kind: 'jpeg', why: 'this platform has no preview surface' });
	});

	test('nothing when the panel mounts hidden: a hidden Preview has no frame to name, whatever the route says for it', () => {
		expect(rendererNote({ ...shown, shown: false })).toEqual({ kind: 'clear' });
		expect(rendererNote({ ...shown, shown: false, via: 'gpu', why: null })).toEqual({ kind: 'clear' });
	});

	test('nothing with the setting off', () => {
		expect(rendererNote({ ...shown, enabled: false })).toEqual({ kind: 'clear' });
	});

	test('the last answer stands on the GPU route, and for a JPEG with no reason to give', () => {
		expect(rendererNote({ ...shown, via: 'gpu', why: null })).toEqual({ kind: 'leave' });
		expect(rendererNote({ ...shown, why: null })).toEqual({ kind: 'leave' });
	});
});
