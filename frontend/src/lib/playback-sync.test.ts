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
	takeoverStart,
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

	test('what a restart costs after a head start is the floor and the head start, not the floor alone', () => {
		// 60 fps, floor 0.1 s, then a run that starts 0.45 s behind and heals at 0.4 s per second: about 0.7 s
		// of frozen picture still to come when it has been watched. A restart costs the 0.5 s minimum, so
		// it is worth it; but if the stream was started 0.8 s ahead of the playhead its floor of 0.1 is
		// only what is left of a 0.9 s start-up, and a restart would cost that.
		const run = (lead: number) => {
			const verdict = createFrameGate({ fps: 60, lead });
			let at = 0;
			verdict(0.1, at);
			for (let i = 0; i < 30; i++) verdict(0.1, (at += 1 / 60));
			const t0 = at;
			const seen: FrameVerdict[] = [];
			for (let i = 0; i < 60; i++) {
				at += 1 / 60;
				seen.push(verdict(0.1 + 0.45 - 0.4 * (at - t0), at));
			}
			return seen;
		};
		expect(run(0).includes('resync')).toBe(true);
		expect(run(0.8).includes('resync')).toBe(false);
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
	test('is the playhead, or the start-up this cut has been seen to take ahead of it', () => {
		expect(startAt(10, null, 100, 30)).toBe(10);
		expect(startAt(10, 0.2, 100, 30)).toBeCloseTo(10.2, 10);
		expect(startAt(10, 0, 100, 30)).toBe(10);
	});

	test('a start-up that is not one to make up for is not taken: negative, not a number, or very long', () => {
		expect(startAt(10, -0.2, 100, 30)).toBe(10);
		expect(startAt(10, NaN, 100, 30)).toBe(10);
		expect(startAt(10, Infinity, 100, 30)).toBe(10);
		expect(startAt(10, 3, 100, 30)).toBeCloseTo(10 + MAX_LEAD, 10);
		expect(MAX_LEAD).toBeLessThanOrEqual(0.25);
	});

	test('never past the last frame of the cut, which the engine would refuse', () => {
		expect(startAt(99.9, 0.2, 100, 30)).toBeCloseTo(100 - 1 / 30, 10);
		expect(startAt(100, 0.2, 100, 30)).toBe(100);
		expect(startAt(99, 0.2, 100, 60)).toBeCloseTo(99.2, 10);
	});

	test('puts the first frame in step with the sound rather than a start-up behind it', () => {
		// The spawn takes what the last one's took: the sound is that far on when the first frame
		// arrives, and so is the frame.
		const startup = 0.2;
		const playhead = 12;
		const from = startAt(playhead, startup, 100, 30);
		const arrives = playhead + startup;
		expect(arrives - from).toBeCloseTo(0, 10);
		expect(arrives - startAt(playhead, null, 100, 30)).toBeCloseTo(startup, 10);
	});
});

describe('where a stream that takes a GPU session over starts', () => {
	const fps = 30;

	test('is the playhead and the start-up ahead of it, as any stream, with no hand-over to respect', () => {
		expect(takeoverStart(10, 0.15, 60, fps, null)).toBeCloseTo(10.15, 9);
		expect(takeoverStart(10, null, 60, fps, null)).toBe(10);
	});

	test('is never after the hand-over the engine named: a slice that began inside a window would lose the fade', () => {
		// The page starts the stream a start-up before the hand-over, one animation frame late at worst.
		const handover = 20;
		for (const late of [0, 1 / 120, 1 / 60, 0.05]) {
			const heard = handover - 0.15 + late;
			expect(takeoverStart(heard, 0.15, 60, fps, handover)).toBeLessThanOrEqual(handover);
		}
		// A hand-over still ahead of where the start-up lands is not moved.
		expect(takeoverStart(10, 0.15, 60, fps, 30)).toBeCloseTo(10.15, 9);
		// One in the past (a frame failed inside a dissolve): the slice starts where the dissolve does.
		expect(takeoverStart(21, 0.15, 60, fps, 20.4)).toBe(20.4);
		expect(takeoverStart(1, 0.15, 60, fps, -3)).toBe(0);
		expect(takeoverStart(21, 0.15, 60, fps, Number.NaN)).toBeCloseTo(21.15, 9);
	});
});

describe('the Preview\u2019s hand-over to the stream', () => {
	const source = readFileSync(join(SRC, 'lib/components/editor/Preview.svelte'), 'utf8');

	test('starts the stream no later than the engine\u2019s hand-over, in the time that is heard', () => {
		// The pieces `createLadder`'s tests above hold: where the stream starts and what the clock is read less.
		expect(source).toContain('earliest: takeover ? (handover?.handover ?? null) : null');
		expect(source).toContain('latency: takeover ? (gpuPlay?.latency ?? 0) : takeoverLatency');
		expect(source).toContain('verdict(ui.time - offset - f.time');
	});

	test('starts the GPU\u2019s session at the playhead it was started from', () => {
		expect(source).toContain('new GpuPlayback(playFps, untrack(() => ui.time))');
	});
});

describe('createLadder for a stream that takes a GPU session over', () => {
	const cut = { fps: 30, width: 960 as number | null, playhead: 20, duration: 60 };
	const key = { timeline: {}, epoch: 0, width: 960 };

	test('starts at or before ahead.handover, in the time that is heard, and tells the gate what it is behind', () => {
		let now = 0;
		const ladder = createLadder(() => now);
		// A first stream that took 0.15 s to start teaches the ladder its start-up.
		ladder.start(key, cut);
		ladder.ended(0.15);
		const handover = 20.1;
		// The page triggers at heard >= handover - startup, so the clock is 0.2 s of latency past that, plus a frame.
		const playhead = handover - 0.15 + 0.2 + 1 / 60;
		const start = ladder.start(key, { ...cut, playhead, latency: 0.2, earliest: handover });
		expect(start.offset).toBeCloseTo(0.2, 9);
		expect(start.from).toBeLessThanOrEqual(handover);
		expect(start.from).toBeGreaterThanOrEqual(handover - 1 / 30);
		// It starts behind where a plain start-up would put it by at most a frame, never ahead of the hand-over.
		expect(start.lead).toBeLessThan(0.15);
	});

	test('a hand-over found late starts the slice where its window starts, and the lead the gate adds is not negative', () => {
		const ladder = createLadder(() => 0);
		const start = ladder.start(key, { ...cut, playhead: 20.6, latency: 0, earliest: 20 });
		expect(start.from).toBe(20);
		expect(start.lead).toBe(0);
		// Its floor was 0.7 s, 0.6 of which was the time it was started behind the clock: the start-up it took
		// was 0.1 s, and the next stream is started that far ahead.
		ladder.ended(0.7);
		const next = ladder.start(key, { ...cut, playhead: 30 });
		expect(next.from).toBeCloseTo(30.1, 9);
	});

	test('a stream that is not a takeover is unchanged: the clock itself, no limit', () => {
		const start = createLadder(() => 0).start(key, cut);
		expect(start.offset).toBe(0);
		expect(start.from).toBe(20);
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
		expect(start.lead).toBe(0);
		expect(start.cheaper).toBe(true);
		expect(start.heal).toBe(true);
		expect(ladder.steps).toBe(0);
	});

	test('an unmeasured pane asks for nothing and plays at the default width', () => {
		const start = fresh().start(key(undefined, 0, null), { ...cut, width: null });
		expect(start.request).toEqual({ fps: null, width: null });
		expect(start.rung).toEqual({ fps: 60, width: 960 });
	});

	test('a degrade steps down, states what it asks for, and starts the start-up it took ahead', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		now = 3000;
		ladder.restarted('degrade', 0.2);
		const start = ladder.start(k, cut);
		expect(start.rung).toEqual({ fps: 30, width: 1920 });
		expect(start.request).toEqual({ fps: 30, width: 1920 });
		expect(start.from).toBeCloseTo(5.2, 10);
		expect(start.lead).toBeCloseTo(0.2, 10);
		expect(ladder.steps).toBe(1);
	});

	test('the first Play of a cut has nothing to lead by; an ordinary start after a stream has run is seeded with its start-up', () => {
		const ladder = fresh();
		const k = key();
		expect(ladder.start(k, cut).from).toBe(5);
		// The stream ran, was paused, and its gate had seen a floor of 0.18 s.
		ladder.ended(0.18);
		const play = ladder.start(k, { ...cut, playhead: 9 });
		expect(play.from).toBeCloseTo(9.18, 10);
		expect(play.heal).toBe(true);
		expect(ladder.steps).toBe(0);
		// A seek during play, the same.
		ladder.ended(0);
		expect(ladder.start(k, { ...cut, playhead: 20 }).from).toBeCloseTo(20.18, 10);
	});

	test('a stream that never showed a frame says nothing about start-up', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.ended(null);
		expect(ladder.start(k, cut).from).toBe(5);
		expect(ladder.startup).toBeNull();
	});

	test('says what a start-up costs, held to the most a stream is led by (the hand-over from the GPU reads it)', () => {
		const ladder = fresh();
		const k = key();
		expect(ladder.startup).toBeNull();
		ladder.start(k, cut);
		ladder.ended(0.18);
		expect(ladder.startup).toBeCloseTo(0.18, 10);
		// A stall that landed in a start-up is not one to start ahead by.
		const slow = fresh();
		slow.start(k, cut);
		slow.ended(2);
		expect(slow.startup).toBe(MAX_LEAD);
	});

	test('the least start-up seen is what the next one is led by: nothing that comes in under it puts the picture ahead', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.ended(0.3);
		expect(ladder.start(k, cut).lead).toBeCloseTo(MAX_LEAD, 10);
		// That stream was in step by its floor and no more: it took 0.1 + the lead it was given (0.25).
		ladder.ended(0.1);
		expect(ladder.start(k, cut).lead).toBeCloseTo(0.25, 10);
		// A quicker one comes in at 0.15 in all (floor -0.1 with the 0.25 it was given): the minimum drops.
		ladder.ended(-0.1);
		expect(ladder.start(k, cut).lead).toBeCloseTo(0.15, 10);
		// A slower one afterwards does not raise it again.
		ladder.ended(0.5);
		expect(ladder.start(k, cut).lead).toBeCloseTo(0.15, 10);
	});

	test('a stream that was started ahead hands its start-up on whole, not what is left of it', () => {
		// The first restart's stream was given 0.2 s and was in step to the millisecond (floor 0): the
		// next restart still has a 0.2 s spawn to make up for.
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.restarted('degrade', 0.2);
		expect(ladder.start(k, cut).from).toBeCloseTo(5.2, 10);
		ladder.restarted('degrade', 0);
		expect(ladder.start(k, cut).from).toBeCloseTo(5.2, 10);
		// ...and one that was given too much (floor -0.1: the picture ran ahead) hands on less.
		ladder.restarted('degrade', -0.1);
		expect(ladder.start(k, cut).from).toBeCloseTo(5.1, 10);
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

	test('a stream a lag restart started is not offered a restart for healing, until the window has passed', () => {
		const ladder = fresh();
		const k = key();
		expect(ladder.start(k, cut).heal).toBe(true);
		now = 5000;
		ladder.restarted('resync', 0.2);
		expect(ladder.start(k, cut).heal).toBe(false);
		// Whatever starts it, for as long as the restart is that recent: a seek, a play.
		now = 5000 + REPEAT_RESYNC_WINDOW_MS - 1;
		expect(ladder.start(k, cut).heal).toBe(false);
		now = 5000 + REPEAT_RESYNC_WINDOW_MS;
		expect(ladder.start(k, cut).heal).toBe(true);
		// A degrade is a lag restart too.
		ladder.restarted('degrade', 0.2);
		expect(ladder.start(k, cut).heal).toBe(false);
	});

	test('a new cut, a proxy that landed or another pane starts at the top again', () => {
		const ladder = fresh();
		const timeline = {};
		ladder.start(key(timeline), cut);
		ladder.restarted('degrade', 0.2);
		expect(ladder.start(key(timeline), cut).rung.fps).toBe(30);
		ladder.restarted('degrade', 0.2);
		const fresher = ladder.start(key({}), cut);
		expect(fresher.rung.fps).toBe(60);
		expect(fresher.heal).toBe(true);
		ladder.restarted('degrade', 0.2);
		expect(ladder.start(key(timeline, 1), cut).rung.fps).toBe(60);
		const again = key(timeline, 1);
		ladder.restarted('degrade', 0.2);
		expect(ladder.start(again, cut).rung.fps).toBe(30);
		ladder.restarted('degrade', 0.2);
		expect(ladder.start(key(timeline, 1, 960), cut).rung.fps).toBe(60);
		// ...and what was learnt about start-up is gone with it: nothing leads the first start of a new key.
		const reset = ladder.start(key(timeline, 1, 480), cut);
		expect(reset.from).toBe(5);
		expect(reset.lead).toBe(0);
	});

	test('a stream that ends under a key that has since changed does not teach the new one', () => {
		const ladder = fresh();
		ladder.start(key({}), cut);
		const next = key({});
		// The effect's cleanup runs before the new start: the old stream is observed, then the key resets.
		ladder.ended(0.2);
		expect(ladder.start(next, cut).from).toBe(5);
	});

	test('forgets a step after a minute, and not before', () => {
		const ladder = fresh();
		const k = key();
		ladder.start(k, cut);
		ladder.restarted('degrade', 0.2);
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
		// The end that counts is the cut as it renders: what the page gives as `duration` is the page's to get right.
		expect(ladder.start(k, { ...cut, playhead: 29.9, duration: 30 }).from).toBeCloseTo(30 - 1 / 30, 10);
	});
});

/**
 * A session of `seconds` on the real ladder and gate, against a renderer that stalls for `stall` seconds
 * a fixed `after` seconds into every stream it starts (a decoder's first second, a cold cache) and then
 * catches up at `catchUp` times real time: the case a restart for healing cannot cure, since the
 * restarted stream stalls the same way (`once`: only the first stream does). Returns how often the stream was restarted and how long the
 * pane showed nothing (gaps over 100 ms between shown frames, the stall itself included).
 */
function session(o: { seconds: number; stall: number; after: number; catchUp: number; breaker: boolean; once?: boolean }) {
	const fps = 60;
	const spawn = 0.15;
	let now = 0;
	const ladder = createLadder(() => now * 1000);
	const k = { timeline: {}, epoch: 0, width: 1920 };
	const shown: number[] = [];
	let restarts = 0;
	let t = 0;
	while (t < o.seconds) {
		const start = ladder.start(k, { fps, width: 1920, playhead: t, duration: 1e6 });
		const gate = createFrameGate({
			fps: start.rung.fps,
			overloadAfter: start.cheaper ? undefined : Infinity,
			healWatch: o.breaker && !start.heal ? Infinity : undefined,
			lead: start.lead
		});
		const rate = start.rung.fps;
		// `once`: only the first stream meets the stall.
		const stalledAt = o.once && restarts > 0 ? Infinity : Math.round(o.after * rate);
		let prev = 0;
		let ended: number | null = null;
		for (let i = 0; ; i++) {
			const due = t + spawn + i / rate;
			const arrival = i < stalledAt ? due : Math.max(due, i === stalledAt ? due + o.stall : prev + 1 / (o.catchUp * rate));
			if (arrival > o.seconds) break;
			prev = arrival;
			now = arrival;
			const verdict = gate(arrival - (start.from + i / rate), arrival);
			if (verdict === 'show') shown.push(arrival);
			if (verdict === 'resync' || verdict === 'degrade') {
				ladder.restarted(verdict, gate.floor());
				ended = arrival;
				break;
			}
		}
		ladder.ended(gate.floor());
		if (ended === null) break;
		restarts++;
		t = ended;
	}
	let frozen = 0;
	for (let i = 1; i < shown.length; i++) if (shown[i] - shown[i - 1] > 0.1) frozen += shown[i] - shown[i - 1];
	return { restarts, frozen, steps: ladder.steps };
}

describe('a stall that comes with every start', () => {
	const stall = { seconds: 40, stall: 0.6, after: 1.0, catchUp: 1.1 };

	test('without the breaker a restart for healing restarts the stream into the same stall, over and over', () => {
		const loop = session({ ...stall, breaker: false });
		expect(loop.restarts).toBeGreaterThan(15);
		expect(loop.frozen).toBeGreaterThan(15);
		expect(loop.steps).toBeGreaterThan(3);
	});

	test('with it there is one restart, and the stream is left to heal', () => {
		const once = session({ ...stall, breaker: true });
		expect(once.restarts).toBe(1);
		expect(once.frozen).toBeLessThan(8);
	});

	test('a one-off stall is still healed by a restart, breaker or not', () => {
		// The same renderer, stalling once, ten seconds in, rather than at every start.
		const lone = session({ ...stall, after: 10, once: true, breaker: true });
		expect(lone.restarts).toBe(1);
		expect(lone.frozen).toBeLessThan(2);
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
