import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';
import { FRAME_STEPS, MIN_FRAME_PX } from './preview-size';
import {
	createFrameGate,
	DEGRADE_MEMORY_MS,
	MAX_PLAYBACK_FPS,
	MIN_PLAYBACK_FPS,
	MIN_STALE_AFTER,
	OVERLOAD_AFTER,
	playbackFps,
	REPEAT_RESYNC_WINDOW_MS,
	RESYNC_AFTER,
	rungAfter,
	staleAfter,
	startRung,
	stepDown,
	stepsAfter,
	stepsToStart,
	type FrameVerdict,
	type Rung
} from './playback-sync';

const STALE_24 = staleAfter(24);

// The bounds the engine clamps a request to (`playback_params` in kerf-core): the page's own limits
// are mirrors of them, and a request past them is silently clamped there.
describe('the engine\u2019s bounds', () => {
	const rust = readFileSync(join(SRC, '..', '..', 'crates/kerf-core/src/engine/cli.rs'), 'utf8');
	const constant = (name: string) => Number(new RegExp(`pub const ${name}: [a-z0-9]+ = ([0-9.]+);`).exec(rust)?.[1]);

	test('the rate range is the one the page plays within', () => {
		expect(constant('MAX_STREAM_FPS')).toBe(MAX_PLAYBACK_FPS);
		expect(constant('MIN_STREAM_FPS')).toBe(MIN_PLAYBACK_FPS);
	});

	test('the width range holds every step the page asks at', () => {
		expect(constant('MAX_STREAM_WIDTH')).toBe(FRAME_STEPS[FRAME_STEPS.length - 1]);
		expect(constant('MIN_STREAM_WIDTH')).toBe(MIN_FRAME_PX);
	});
});

describe('playbackFps', () => {
	test('a cut plays at its own rate', () => {
		for (const fps of [12, 23.976, 24, 25, 29.97, 30, 50, 59.94, 60]) expect(playbackFps(fps)).toBe(fps);
	});

	test('but not past what a display paints, and not at nothing', () => {
		expect(playbackFps(120)).toBe(MAX_PLAYBACK_FPS);
		expect(playbackFps(240)).toBe(MAX_PLAYBACK_FPS);
		expect(playbackFps(0.2)).toBe(1);
		for (const bad of [NaN, 0, -30, Infinity]) expect(playbackFps(bad)).toBe(30);
	});
});

describe('staleAfter', () => {
	test('is two frames at the low rates, as it was at 24 fps', () => {
		expect(staleAfter(24)).toBeCloseTo(2 / 24, 10);
		expect(staleAfter(30)).toBeCloseTo(2 / 30, 10);
	});

	test('does not shrink below what one main-thread hiccup costs at 50 and 60 fps', () => {
		expect(staleAfter(60)).toBe(MIN_STALE_AFTER);
		expect(staleAfter(50)).toBe(MIN_STALE_AFTER);
		expect(staleAfter(60)).toBeGreaterThan(2 / 60);
	});
});

describe('createFrameGate', () => {
	test('shows every frame of a stream with a constant transport cost', () => {
		// The regression: ffmpeg startup + IPC put ~90 ms on every frame,
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
		expect(verdict(0.09 + STALE_24 + 0.01)).toBe('skip');
		expect(verdict(0.1)).toBe('show');
	});

	test('skips frames that drift past the budget and shows them again on recovery', () => {
		const verdict = createFrameGate();
		verdict(0.05);
		expect(verdict(0.05 + STALE_24 / 2)).toBe('show');
		expect(verdict(0.05 + STALE_24 * 2)).toBe('skip');
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

	test('the budget follows the rate it is told: a 60 fps stream keeps 50 ms, not 33', () => {
		const verdict = createFrameGate({ fps: 60 });
		verdict(0.09);
		expect(verdict(0.09 + 0.04)).toBe('show');
		expect(verdict(0.09 + MIN_STALE_AFTER + 0.005)).toBe('skip');
	});
});

/** What the gate says to a stream that renders at `renders` fps while it is played at `plays`,
 *  against a clock that runs at real time: frame i of the stream is `i / plays` of media time
 *  and arrives `i / renders` seconds in, after `spawn` of ffmpeg start-up. */
function play(plays: number, renders: number, seconds: number, spawn = 0.15): { verdicts: FrameVerdict[]; at: number[] } {
	const verdict = createFrameGate({ fps: plays });
	const verdicts: FrameVerdict[] = [];
	const at: number[] = [];
	for (let i = 0; ; i++) {
		const arrives = spawn + i / renders;
		if (arrives > seconds) break;
		const v = verdict(arrives - i / plays);
		verdicts.push(v);
		at.push(arrives);
		if (v === 'resync' || v === 'degrade') break;
	}
	return { verdicts, at };
}

describe('a stream that cannot keep up', () => {
	test('is called overloaded within a second, not left frozen for the six it takes to drift a second behind', () => {
		// 60 fps asked of a machine that does 50: drift grows by a sixth of a second per second.
		const { verdicts, at } = play(60, 50, 10);
		expect(verdicts.at(-1)).toBe('degrade');
		expect(at.at(-1)!).toBeLessThan(2);
		// The frozen stretch: the skips between the last frame shown and the verdict.
		const skipped = verdicts.filter((v) => v === 'skip').length;
		expect(skipped).toBeLessThanOrEqual(Math.ceil(OVERLOAD_AFTER * 60));
	});

	test('whatever the rate', () => {
		for (const [plays, renders] of [
			[24, 20],
			[30, 25],
			[50, 40],
			[60, 45],
			[60, 20]
		]) {
			const { verdicts, at } = play(plays, renders, 20);
			expect(verdicts.at(-1)).toBe('degrade');
			expect(at.at(-1)!).toBeLessThan(3);
		}
	});

	test('a stream that keeps its rate is never called one', () => {
		for (const fps of [24, 30, 50, 60]) {
			const { verdicts } = play(fps, fps, 30);
			expect(verdicts.every((v) => v === 'show')).toBe(true);
		}
	});

	test('a hiccup is not: the burst after a stall is skipped while it catches up, not given up on', () => {
		// 60 fps, then the page stops for 450 ms and the frames queued behind it arrive together:
		// each is staler than the last was early, so the drift shrinks across the burst.
		const fps = 60;
		const verdict = createFrameGate({ fps });
		const out: FrameVerdict[] = [];
		for (let i = 0; i < 120; i++) out.push(verdict(0.1)); // a second of steady state
		const stall = 0.45;
		const queued = Math.round(stall * fps);
		for (let j = 0; j < queued; j++) out.push(verdict(0.1 + stall - j / fps));
		for (let j = 0; j < 60; j++) out.push(verdict(0.1));
		expect(out.includes('degrade')).toBe(false);
		expect(out.includes('resync')).toBe(false);
		expect(out.filter((v) => v === 'skip').length).toBeGreaterThan(10);
		expect(out.at(-1)).toBe('show');
	});

	test('with no cheaper rung to restart into it skips until the resync, as it did before', () => {
		const verdict = createFrameGate({ fps: 15, overloadAfter: Infinity });
		const seen = new Set<FrameVerdict>();
		for (let i = 0; i < 400; i++) {
			const v = verdict(0.15 + i / 40);
			seen.add(v);
			if (v === 'resync') break;
		}
		expect([...seen].sort()).toEqual(['resync', 'show', 'skip']);
	});

	test('a stall of a second is a resync, not a degrade', () => {
		const verdict = createFrameGate({ fps: 30 });
		for (let i = 0; i < 30; i++) verdict(0.1);
		expect(verdict(0.1 + RESYNC_AFTER + 0.2)).toBe('resync');
	});
});

describe('stepDown', () => {
	const walk = (from: Rung) => {
		const out = [from];
		for (let r = stepDown(from); r; r = stepDown(r)) out.push(r);
		return out.map((r) => `${r.fps}@${r.width}`);
	};

	test('halves a 60 fps stream first: smooth motion before a sharp picture', () => {
		expect(walk({ fps: 60, width: 1920 })).toEqual(['60@1920', '30@1920', '30@1440', '30@960', '15@960', '15@480']);
	});

	test('narrows a 30 fps stream a step at a time before it drops below 30', () => {
		expect(walk({ fps: 30, width: 1440 })).toEqual(['30@1440', '30@960', '15@960', '15@480']);
	});

	test('keeps the rate of a fractional cut in proportion', () => {
		expect(walk({ fps: 59.94, width: 960 })).toEqual(['59.94@960', '29.97@960', '14.985@960', '14.985@480']);
		expect(walk({ fps: 50, width: 960 })).toEqual(['50@960', '25@960', '12.5@960', '12.5@480']);
		expect(walk({ fps: 24, width: 960 })).toEqual(['24@960', '12@960', '12@480']);
	});

	test('a width that is not a step (a 1280 px proxy) goes to the next one down', () => {
		expect(walk({ fps: 30, width: 1280 })).toEqual(['30@1280', '30@960', '15@960', '15@480']);
	});

	test('ends: the cheapest rung has nothing below it', () => {
		expect(stepDown({ fps: 15, width: 480 })).toBeNull();
		expect(stepDown({ fps: 12, width: 480 })).toBeNull();
		expect(stepDown({ fps: 1, width: 64 })).toBeNull();
	});

	test('every rung is cheaper than the one above it', () => {
		for (const fps of [60, 59.94, 50, 30, 24]) {
			for (const width of [1920, 1440, 1280, 960, 480]) {
				let r: Rung = { fps, width };
				for (let next = stepDown(r); next; r = next, next = stepDown(r)) {
					expect(next.fps * next.width).toBeLessThan(r.fps * r.width);
				}
			}
		}
	});
});

describe('rungAfter / startRung / stepsAfter', () => {
	test('a cut starts at its own rate and the pane width, or the default width until measured', () => {
		expect(startRung(60, 1440)).toEqual({ fps: 60, width: 1440 });
		expect(startRung(120, null)).toEqual({ fps: 60, width: 960 });
		expect(startRung(NaN, 1920)).toEqual({ fps: 30, width: 1920 });
	});

	test('steps are applied in order and stop at the bottom', () => {
		const base = startRung(60, 1920);
		expect(rungAfter(base, 0)).toEqual(base);
		expect(rungAfter(base, 1)).toEqual({ fps: 30, width: 1920 });
		expect(rungAfter(base, 3)).toEqual({ fps: 30, width: 960 });
		expect(rungAfter(base, 50)).toEqual({ fps: 15, width: 480 });
	});

	test('an overload always costs a step; a stall only when it follows another', () => {
		expect(stepsAfter('degrade', 0, Infinity)).toBe(1);
		expect(stepsAfter('degrade', 2, 0)).toBe(3);
		expect(stepsAfter('resync', 0, Infinity)).toBe(0);
		expect(stepsAfter('resync', 1, REPEAT_RESYNC_WINDOW_MS + 1)).toBe(1);
		expect(stepsAfter('resync', 1, REPEAT_RESYNC_WINDOW_MS - 1)).toBe(2);
	});
});

describe('stepsToStart', () => {
	test('a step down is kept for a minute, then forgotten', () => {
		expect(stepsToStart(2, 0)).toBe(2);
		expect(stepsToStart(2, DEGRADE_MEMORY_MS)).toBe(2);
		expect(stepsToStart(2, DEGRADE_MEMORY_MS + 1)).toBe(0);
		expect(stepsToStart(0, Infinity)).toBe(0);
		expect(stepsToStart(3, Infinity)).toBe(0);
	});
});

describe('a machine that does 45 fps at the full width', () => {
	test('settles at a rung it can hold, in a handful of restarts, and then plays without a drop', () => {
		// The rate a rung can be rendered at on this machine, frames/s: the cost per frame scales
		// with the pixels, so 45 fps at 1920 is 90 at 1440 and 180 at 960.
		const throughput = (r: Rung) => 45 * (1920 / r.width) ** 2;
		let steps = 0;
		const base = startRung(60, 1920);
		let restarts = 0;
		let verdicts: FrameVerdict[] = [];
		for (; restarts < 10; restarts++) {
			const rung = rungAfter(base, steps);
			({ verdicts } = play(rung.fps, throughput(rung), 20));
			const last = verdicts.at(-1)!;
			if (last !== 'degrade' && last !== 'resync') break;
			steps = stepsAfter(last, steps, 0);
		}
		const settled = rungAfter(base, steps);
		expect(restarts).toBeLessThanOrEqual(2);
		expect(settled).toEqual({ fps: 30, width: 1920 });
		expect(verdicts.every((v) => v === 'show')).toBe(true);
	});
});
