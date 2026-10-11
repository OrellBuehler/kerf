import { describe, expect, test } from 'bun:test';
import {
	DEFAULT_STARTUP,
	GpuPlayback,
	MAX_ROUND_TRIP,
	Median,
	OVERLOAD_AFTER,
	frameIndex,
	pictureTime
} from './gpu-playback';
import { MAX_OUTPUT_LATENCY } from './output-latency';
import type { PlayAhead, PlayInfo, PreviewFrameResult } from './types';

const timings = { width: 960, height: 540, decode_ms: 4, composite_ms: 2, present_ms: 1 };

function play(over: Partial<PlayInfo> = {}): PlayInfo {
	return { session: 1, start: 0, frame: 0, repeat: false, ahead: null, ...over };
}

function drew(over: Partial<PlayInfo> = {}): PreviewFrameResult {
	return { renderer: 'gpu', frame: null, reasons: [], timings, play: play(over) };
}

const stream = (ahead: PlayAhead | null = null): PreviewFrameResult => ({
	renderer: 'stream',
	frame: null,
	reasons: ['a mask'],
	timings: null,
	play: ahead ? play({ ahead }) : null
});

const superseded: PreviewFrameResult = { renderer: 'superseded', frame: null, reasons: [], timings: null };

const ahead = (at: number, handover = at): PlayAhead => ({ at, handover, kind: 'mask', why: 'a mask' });

describe('the time a picture is asked for', () => {
	test('is the clock less what the sound has not got through, plus the request its own cost', () => {
		expect(pictureTime(10, 0.04, 0.02)).toBeCloseTo(9.98, 9);
		expect(pictureTime(10, 0, 0)).toBe(10);
		// More round trip than latency: the picture is asked for a little ahead of the clock.
		expect(pictureTime(10, 0.01, 0.03)).toBeCloseTo(10.02, 9);
	});

	test('holds both to what is believable, and never asks for before the start', () => {
		expect(pictureTime(10, 9, 0)).toBeCloseTo(10 - MAX_OUTPUT_LATENCY, 9);
		expect(pictureTime(10, 0, 9)).toBeCloseTo(10 + MAX_ROUND_TRIP, 9);
		expect(pictureTime(10, Number.NaN, Number.NaN)).toBe(10);
		expect(pictureTime(0.1, 0.3, 0)).toBe(0);
		expect(pictureTime(-1, 0, 0)).toBe(0);
	});

	test('is a frame of the cut by where it falls, a hair short of a boundary included', () => {
		expect(frameIndex(1, 30)).toBe(30);
		expect(frameIndex(30 / 30 - 1e-9, 30)).toBe(30);
		expect(frameIndex(0.0333, 30)).toBe(0);
		expect(frameIndex(-2, 30)).toBe(0);
		expect(frameIndex(1.0, 29.97)).toBe(29);
	});
});

describe('Median', () => {
	test('is the middle of the last samples, and ignores what is not a number', () => {
		const m = new Median(3);
		expect(m.value).toBeNull();
		for (const v of [5, 1, 3, Number.NaN]) m.add(v);
		expect(m.value).toBe(3);
		m.add(9);
		m.add(7);
		// 3, 9, 7 -> 7
		expect(m.value).toBe(7);
	});
});

describe('the requests of a session', () => {
	test('one is made for each frame of the cut, not for each animation frame', () => {
		const p = new GpuPlayback(30);
		expect(p.next(0, 0)).toEqual({ time: 0, frame: 0 });
		p.answer(drew(), 0.02, 0);
		const asked: number[] = [];
		// A 60 Hz display for a third of a second.
		for (let i = 1; i <= 20; i++) {
			const step = p.next(i / 60, 0);
			if (step) asked.push(step.frame);
		}
		expect(asked).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
	});

	test('never asks for a frame twice, or for an earlier one when the estimate moves', () => {
		const p = new GpuPlayback(30);
		p.next(1, 0);
		p.answer(drew(), 0, 0);
		expect(p.next(1.0, 0)).toBeNull();
		// The sound's latency estimate jumps: the picture would be asked for earlier.
		expect(p.next(1.02, 0.1)).toBeNull();
		expect(p.next(1.2, 0.1)?.frame).toBe(33);
	});

	test('the first request decides, and only it is in flight until it is answered', () => {
		const p = new GpuPlayback(30);
		expect(p.next(0, 0)).not.toBeNull();
		expect(p.next(0.1, 0)).toBeNull();
		expect(p.next(0.2, 0)).toBeNull();
		expect(p.decided).toBe('pending');
		// Replaced before it was decided (another pump's request took its place): ask again.
		expect(p.answer(superseded, 0, 0)).toBe('ignore');
		expect(p.next(0.3, 0)?.frame).toBe(9);
		expect(p.answer(drew(), 0.03, 0)).toBe('show');
		expect(p.decided).toBe('gpu');
		expect(p.next(0.4, 0)?.frame).toBe(12);
		expect(p.next(0.5, 0)?.frame).toBe(15);
	});

	test('the picture is asked for ahead by the median round trip of the frames that were drawn', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		p.answer(drew(), 0.03, 0);
		p.answer(drew(), 0.04, 0);
		p.answer(drew(), 0.05, 0);
		// A repeat is no cost of drawing: it must not pull the estimate down.
		p.answer(drew({ repeat: true }), 0.001, 0);
		expect(p.roundTrip).toBeCloseTo(0.04, 9);
		const step = p.next(2, 0.06);
		expect(step?.time).toBeCloseTo(2 - 0.06 + 0.04, 9);
	});

	test('smooths the output latency it is given, so one odd reading moves nothing', () => {
		const p = new GpuPlayback(60);
		p.next(0, 0.04);
		p.answer(drew(), 0, 0);
		for (let i = 1; i < 6; i++) p.next(i * 0.1, 0.04);
		const step = p.next(1, 0.5);
		expect(step?.time).toBeCloseTo(1 - 0.04, 9);
	});
});

describe('the answers', () => {
	test('a drawn frame shows, a repeat repeats, and where the GPU stops is remembered', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		expect(p.answer(drew({ ahead: ahead(7, 6.5) }), 0.02, 0)).toBe('show');
		expect(p.ahead).toEqual(ahead(7, 6.5));
		expect(p.answer(drew({ repeat: true }), 0.001, 0)).toBe('repeat');
		expect(p.answer(drew(), 0.02, 0)).toBe('show');
		expect(p.ahead).toBeNull();
		expect([p.drawn, p.repeated]).toEqual([2, 1]);
	});

	test('a superseded answer says nothing', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		p.answer(drew(), 0.02, 0);
		expect(p.answer(superseded, 0, 0)).toBe('ignore');
		expect(p.ended).toBe(false);
	});

	test('the stream takes the session when the engine says so, with where from, and nothing is asked after', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		expect(p.answer(stream(ahead(4, 3.5)), 0.01, 0)).toBe('stream');
		expect(p.ahead).toEqual(ahead(4, 3.5));
		expect(p.ended).toBe(true);
		expect(p.next(1, 0)).toBeNull();
		expect(p.answer(drew(), 0, 0)).toBe('ignore');
	});

	test('an FFmpeg frame is not what a forward request is meant to get: the stream plays on', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		const jpeg: PreviewFrameResult = { renderer: 'ffmpeg', frame: 'data:', reasons: ['x'], timings: null };
		expect(p.answer(jpeg, 0.01, 0)).toBe('stream');
		expect(p.ended).toBe(true);
	});

	test('a GPU that takes more than three frame periods a frame for a second gives the session up', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		let verdict = p.answer(drew(), 0.15, 0);
		for (let at = 0.1; at < OVERLOAD_AFTER && verdict === 'show'; at += 0.1) {
			verdict = p.answer(drew(), 0.15, at);
		}
		expect(verdict).toBe('show');
		expect(p.answer(drew(), 0.15, OVERLOAD_AFTER + 0.01)).toBe('stream');
		expect(p.overloaded).toBe(true);
		expect(p.ended).toBe(true);
	});

	test('a slow moment is not an overload: the median recovers', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		p.answer(drew(), 0.03, 0);
		for (let at = 0.1; at < 3; at += 0.1) {
			// Every fifth frame is slow.
			const slow = Math.round(at * 10) % 5 === 0;
			expect(p.answer(drew(), slow ? 0.3 : 0.03, at)).toBe('show');
		}
		expect(p.overloaded).toBe(false);
	});
});

describe('the hand-over', () => {
	test('is due when the playhead is a start-up short of where the stream has to start', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		p.answer(drew({ ahead: ahead(6, 5.5) }), 0.02, 0);
		expect(p.handoverDue(1, 0.2)).toBeNull();
		expect(p.handoverDue(5.29, 0.2)).toBeNull();
		expect(p.handoverDue(5.31, 0.2)?.handover).toBe(5.5);
		// With no start-up measured, the default is used.
		expect(p.handoverDue(5.5 - DEFAULT_STARTUP + 0.001)?.at).toBe(6);
		expect(p.handoverDue(5.5 - DEFAULT_STARTUP - 0.01)).toBeNull();
	});

	test('is never due with nowhere in sight, or once the session is over', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		p.answer(drew(), 0.02, 0);
		expect(p.handoverDue(100)).toBeNull();
		p.answer(drew({ ahead: ahead(1) }), 0.02, 0);
		expect(p.handoverDue(100)).not.toBeNull();
		p.end();
		expect(p.handoverDue(100)).toBeNull();
	});

	test('a handover in the past (the engine found it late) is due at once', () => {
		const p = new GpuPlayback(30);
		p.next(0, 0);
		p.answer(drew({ ahead: ahead(2, 1.5) }), 0.02, 0);
		expect(p.handoverDue(2.5)?.at).toBe(2);
	});
});

describe('sessions', () => {
	test('are numbered apart', () => {
		const a = new GpuPlayback(30);
		const b = new GpuPlayback(30);
		expect(b.session).toBeGreaterThan(a.session);
	});
});
