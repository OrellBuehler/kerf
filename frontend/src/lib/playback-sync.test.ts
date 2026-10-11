import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { SRC } from './test-sources';
import { FRAME_STEPS, MIN_FRAME_PX } from './preview-size';
import {
	createFrameGate,
	createLadder,
	DEGRADE_MEMORY_MS,
	HEAL_WATCH,
	MAX_LEAD,
	MAX_PLAYBACK_FPS,
	MIN_PLAYBACK_FPS,
	MIN_STALE_AFTER,
	OVERLOAD_AFTER,
	playbackFps,
	REPEAT_RESYNC_WINDOW_MS,
	RESYNC_AFTER,
	rungAfter,
	staleAfter,
	startAt,
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
		expect(staleAfter(12)).toBeCloseTo(2 / 12, 10);
	});

	test('does not shrink below what a main-thread hiccup and ui.time\u2019s refresh cost at 30 to 60 fps', () => {
		for (const fps of [30, 50, 60]) expect(staleAfter(fps)).toBe(MIN_STALE_AFTER);
		expect(staleAfter(60)).toBeGreaterThan(2 / 60 + 1 / 60);
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

	test('the budget follows the rate it is told: a 60 fps stream keeps 75 ms, not 33', () => {
		const verdict = createFrameGate({ fps: 60 });
		verdict(0.09);
		expect(verdict(0.09 + 0.06)).toBe('show');
		expect(verdict(0.09 + MIN_STALE_AFTER + 0.005)).toBe('skip');
	});

	test('says what the stream\u2019s start-up cost and what it dropped', () => {
		const verdict = createFrameGate({ fps: 24 });
		expect(verdict.floor()).toBeNull();
		verdict(0.3);
		verdict(0.12);
		verdict(0.5);
		verdict(0.13);
		expect(verdict.floor()).toBe(0.12);
		expect(verdict.stats()).toEqual({ shown: 3, skipped: 1 });
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

/**
 * A stream that was in step with the clock, stalls for `stallFor` seconds at its 2 s mark, then delivers
 * what it owes at `catchUp` times real time (`Infinity`: all at once) until it is back on its schedule.
 * The clock runs at real time and frame i of the stream is `i / fps` of media time, so a frame's lag is
 * its arrival minus that. Returns what the gate said and how long the pane showed nothing after the
 * stall ended: until the first frame shown, or until the restart a `resync` / `degrade` costs
 * (`spawn` of ffmpeg and its first frame). `heal: false` is the gate without the projection of how long
 * the catching up takes.
 */
function stall(fps: number, stallFor: number, catchUp: number, o: { heal?: boolean } = {}) {
	const verdict = createFrameGate({ fps, healWatch: o.heal === false ? Infinity : undefined });
	const spawn = 0.15;
	const resumes = spawn + 2 + stallFor;
	const owed = Math.round(2 * fps);
	const verdicts: FrameVerdict[] = [];
	let shownAfter: number | null = null;
	let prev = 0;
	for (let i = 0; ; i++) {
		const due = spawn + i / fps;
		const arrival = i < owed ? due : Math.max(due, i === owed ? resumes : prev + 1 / (catchUp * fps));
		if (arrival > spawn + 14) break;
		prev = arrival;
		const v = verdict(arrival - i / fps, arrival);
		verdicts.push(v);
		if (i >= owed && shownAfter === null && v === 'show') shownAfter = arrival;
		if (v === 'resync' || v === 'degrade') {
			shownAfter = arrival + spawn;
			break;
		}
	}
	return { verdicts, frozen: (shownAfter ?? Infinity) - resumes };
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

	test('a stall that is caught up slowly is restarted, not skipped through', () => {
		// The case the one-second resync does not see: 60 fps, a 600 ms stall, then frames at 1.1x real
		// time. The drift heals by a ninth of a second per second, so the pane would show nothing for
		// the six seconds until it is back inside the budget.
		const slow = stall(60, 0.6, 1.1);
		expect(slow.verdicts.at(-1)).toBe('resync');
		expect(slow.frozen).toBeLessThan(0.6);
		const without = stall(60, 0.6, 1.1, { heal: false });
		expect(without.verdicts.includes('resync')).toBe(false);
		expect(without.frozen).toBeGreaterThan(5);
	});

	test('whatever the rate, and however long the stall, when the catch-up is slow', () => {
		for (const fps of [24, 30, 60]) {
			for (const [stallFor, catchUp] of [
				[0.3, 1.2],
				[0.6, 1.1],
				[0.9, 1.3]
			]) {
				const r = stall(fps, stallFor, catchUp);
				expect(r.verdicts.at(-1), `${fps} fps, ${stallFor} s at ${catchUp}x`).toBe('resync');
				expect(r.frozen, `${fps} fps, ${stallFor} s at ${catchUp}x`).toBeLessThan(1);
			}
		}
	});

	test('but a stall that is caught up quickly is waited out: the burst is the picture catching up', () => {
		for (const fps of [24, 60]) {
			const r = stall(fps, 0.45, Infinity);
			expect(r.verdicts.includes('resync') || r.verdicts.includes('degrade')).toBe(false);
			expect(r.frozen).toBeLessThan(0.1);
		}
		// Four times real time: back inside the budget in half a second, which is what a restart costs.
		const fast = stall(60, 0.4, 4);
		expect(fast.verdicts.includes('resync')).toBe(false);
		expect(fast.frozen).toBeLessThan(0.5);
	});

	test('a drift a hair over the budget heals before it matters and is not restarted over', () => {
		const verdict = createFrameGate({ fps: 60 });
		let at = 0;
		for (let i = 0; i < 60; i++) verdict(0.1, (at += 1 / 60));
		const seen = new Set<FrameVerdict>();
		// 85 ms over the floor, closing slowly: skipped for a moment, never worth a restart.
		for (let i = 0; i < 30; i++) seen.add(verdict(0.1 + 0.085 - i * 0.0004, (at += 1 / 60)));
		expect(seen.has('resync')).toBe(false);
	});

	test('watches a run for as long as it takes to read a rate, no less', () => {
		const verdict = createFrameGate({ fps: 60 });
		let at = 0;
		for (let i = 0; i < 60; i++) verdict(0.1, (at += 1 / 60));
		// 600 ms behind and healing at a ninth per second, but only a few frames in.
		expect(verdict(0.7, (at += 1 / 60))).toBe('skip');
		expect(verdict(0.7 - 0.0015, (at += 1 / 60))).toBe('skip');
		expect(HEAL_WATCH).toBeGreaterThan(2 / 60);
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

describe('startAt', () => {
	test('is the playhead, or a head start of the stream\u2019s own floor after a restart for lag', () => {
		expect(startAt(10, null, 100, 30)).toBe(10);
		expect(startAt(10, 0.3, 100, 30)).toBeCloseTo(10.3, 10);
		expect(startAt(10, 0, 100, 30)).toBe(10);
	});

	test('a floor that is not a start-up to make up for is not taken: negative, not a number, or very long', () => {
		expect(startAt(10, -0.2, 100, 30)).toBe(10);
		expect(startAt(10, NaN, 100, 30)).toBe(10);
		expect(startAt(10, Infinity, 100, 30)).toBe(10);
		expect(startAt(10, 3, 100, 30)).toBeCloseTo(10 + MAX_LEAD, 10);
	});

	test('never past the last frame of the cut, which the engine would refuse', () => {
		expect(startAt(99.9, 0.4, 100, 30)).toBeCloseTo(100 - 1 / 30, 10);
		expect(startAt(100, 0.4, 100, 30)).toBe(100);
		expect(startAt(99, 0.4, 100, 60)).toBeCloseTo(99.4, 10);
	});

	test('puts the first frame in step with the sound rather than a start-up behind it', () => {
		// The restart's own spawn takes what the last stream's took (its floor): the sound is that far on
		// when the first frame arrives, and so is the frame.
		const floor = 0.3;
		const playhead = 12;
		const from = startAt(playhead, floor, 100, 30);
		const arrives = playhead + floor;
		expect(arrives - from).toBeCloseTo(0, 10);
		expect(arrives - startAt(playhead, null, 100, 30)).toBeCloseTo(floor, 10);
	});
});

describe('createLadder', () => {
	const cut = { fps: 60, width: 1920 as number | null, playhead: 5, duration: 60 };
	const key = (timeline: unknown = {}, epoch = 0, width: number | null = 1920) => ({ timeline, epoch, width });
	let now = 0;
	const fresh = () => {
		now = 0;
		return createLadder(() => now);
	};

	test('starts at the top: the cut\u2019s own rate left to the engine, the pane\u2019s width, the playhead', () => {
		const ladder = fresh();
		const start = ladder.start(key(), cut);
		expect(start.rung).toEqual({ fps: 60, width: 1920 });
		expect(start.request).toEqual({ fps: null, width: 1920 });
		expect(start.from).toBe(5);
		expect(start.cheaper).toBe(true);
		expect(ladder.steps).toBe(0);
	});

	test('an unmeasured pane asks for nothing and plays at the default width', () => {
		const start = fresh().start(key(undefined, 0, null), { ...cut, width: null });
		expect(start.request).toEqual({ fps: null, width: null });
		expect(start.rung).toEqual({ fps: 60, width: 960 });
	});

	test('a degrade steps down, states what it asks for, and starts a head start ahead', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		now = 3000;
		ladder.restarted('degrade', 0.3);
		const start = ladder.start(k, cut);
		expect(start.rung).toEqual({ fps: 30, width: 1920 });
		expect(start.request).toEqual({ fps: 30, width: 1920 });
		expect(start.from).toBeCloseTo(5.3, 10);
		expect(ladder.steps).toBe(1);
	});

	test('the head start is for the restart that asked for it: a seek or an edit starts at the playhead', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.restarted('degrade', 0.3);
		expect(ladder.start(k, cut).from).toBeCloseTo(5.3, 10);
		expect(ladder.start(k, { ...cut, playhead: 9 }).from).toBe(9);
	});

	test('a stream that was started ahead hands its start-up on whole, not what is left of it', () => {
		// The first restart's stream was given 0.3 s and was in step to the millisecond (floor 0): the
		// next restart still has a 0.3 s spawn to make up for.
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.restarted('degrade', 0.3);
		expect(ladder.start(k, cut).from).toBeCloseTo(5.3, 10);
		ladder.restarted('degrade', 0);
		expect(ladder.start(k, cut).from).toBeCloseTo(5.3, 10);
		// ...and one that was given too much (floor -0.1: the picture ran ahead) hands on less.
		ladder.restarted('degrade', -0.1);
		expect(ladder.start(k, cut).from).toBeCloseTo(5.2, 10);
	});

	test('a resync is a step only when it follows another within ten seconds', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		now = 1000;
		ladder.restarted('resync', 0.2);
		expect(ladder.start(k, cut).rung.fps).toBe(60);
		now = 4000;
		ladder.restarted('resync', 0.2);
		expect(ladder.start(k, cut).rung.fps).toBe(30);
	});

	test('a new cut, a proxy that landed or another pane starts at the top again', () => {
		const ladder = fresh();
		const timeline = {};
		ladder.start(key(timeline), cut);
		ladder.restarted('degrade', 0.3);
		expect(ladder.start(key(timeline), cut).rung.fps).toBe(30);
		ladder.restarted('degrade', 0.3);
		expect(ladder.start(key({}), cut).rung.fps).toBe(60);
		ladder.restarted('degrade', 0.3);
		expect(ladder.start(key(timeline, 1), cut).rung.fps).toBe(60);
		const again = key(timeline, 1);
		ladder.restarted('degrade', 0.3);
		expect(ladder.start(again, cut).rung.fps).toBe(30);
		ladder.restarted('degrade', 0.3);
		expect(ladder.start(key(timeline, 1, 960), cut).rung.fps).toBe(60);
		// ...and the head start of the restart before it is gone with it.
		expect(ladder.start(key(timeline, 1, 960), cut).from).toBe(5);
	});

	test('forgets a step after a minute, and not before', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.restarted('degrade', 0.3);
		ladder.start(k, cut);
		now = DEGRADE_MEMORY_MS - 1;
		expect(ladder.start(k, cut).rung.fps).toBe(30);
		now = DEGRADE_MEMORY_MS + 1;
		expect(ladder.start(k, cut).rung.fps).toBe(60);
		expect(ladder.steps).toBe(0);
	});

	test('says when a restart has nowhere cheaper to go', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, { ...cut, fps: 15, width: 480 });
		expect(ladder.start(k, { ...cut, fps: 15, width: 480 }).cheaper).toBe(false);
		expect(ladder.start(k, { ...cut, fps: 24, width: 480 }).cheaper).toBe(true);
	});

	test('never starts past the end of the cut', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.restarted('degrade', 0.4);
		expect(ladder.start(k, { ...cut, playhead: 59.9 }).from).toBeCloseTo(60 - 1 / 30, 10);
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
