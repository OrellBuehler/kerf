import { describe, expect, test } from 'bun:test';
import { rollEdit, rollRange, slideClip, slideRange, slipClip, slipRange, type SourceLimits } from './edit-modes';
import { frameTime, onFrame, snapToFrame } from './frames';
import {
	CUT_REACH_PX,
	TOOL_HINT,
	TOOL_VERB,
	cutsOf,
	deltaLabel,
	edgeFrameTime,
	holdToRange,
	limitNotice,
	monitorFor,
	nearestCut,
	planPlayheadTrim,
	playheadCut,
	previewEdit,
	readoutFor,
	slideMembers,
	slideNeighbours,
	slipDelta,
	sourceLimits,
	subjectsPresent,
	trimNotice,
	type GestureEdit
} from './trim-tools';
import type { Asset, Clip, StreamKind, Timeline, Track } from './types';
import { clipDuration } from './types';

let seq = 0;
const sclip = (si: number, so: number, start: number, extra: Partial<Clip> = {}): Clip => ({
	id: `clip-${++seq}`,
	asset_id: 'asset',
	source_in: si,
	source_out: so,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const track = (name: string, clips: Clip[], extra: Partial<Track> = {}, kind: StreamKind = 'video'): Track => ({
	id: `track-${name}`,
	kind,
	name,
	clips,
	...extra
});
const lane = (clips: Clip[], extra: Partial<Track> = {}): Timeline => ({ tracks: [track('V1', clips, extra)] });
const footage = (secs: number): SourceLimits => new Map([['asset', secs]]);
const near = (a: number, b: number, eps = 1e-9) => expect(Math.abs(a - b)).toBeLessThan(eps);
const span = (c: Clip): [number, number] => [c.timeline_start, c.timeline_start + clipDuration(c)];

/** `a [0,4)` src 10..14 abutting `b [4,8)` src 20..24 in 60 s of footage. */
function cutPair() {
	const [a, b] = [sclip(10, 14, 0), sclip(20, 24, 4)];
	return { t: lane([a, b]), a, b, edit: { tool: 'roll', a: a.id, b: b.id } satisfies GestureEdit };
}

describe('sourceLimits', () => {
	const asset = (id: string, duration: number, image = false): Pick<Asset, 'id' | 'duration' | 'streams'> => ({
		id,
		duration,
		streams: [{ index: 0, kind: 'video', codec: image ? 'png' : 'h264', ...(image ? { image: true } : {}) }]
	});

	test('a clip runs out of its footage, a still never does', () => {
		const m = sourceLimits([asset('v', 45), asset('s', 5, true)]);
		expect(m.get('v')).toBe(45);
		expect(m.get('s')).toBe(Infinity);
		expect(m.has('missing')).toBe(false);
	});
});

describe('cuts', () => {
	test('two touching clips are a cut at the second one’s start', () => {
		const [a, b, c] = [sclip(0, 4, 0), sclip(0, 4, 4), sclip(0, 2, 8)];
		expect(cutsOf(track('V1', [a, b, c]))).toEqual([
			{ trackId: 'track-V1', a: a.id, b: b.id, time: 4 },
			{ trackId: 'track-V1', a: b.id, b: c.id, time: 8 }
		]);
	});

	test('a gap, an overlap and an unsorted lane', () => {
		const [a, b, c] = [sclip(0, 4, 0), sclip(0, 4, 5), sclip(0, 4, 8.5)];
		expect(cutsOf(track('V1', [a, b, c]))).toEqual([]); // 1 s and 0.5 s gaps
		const [x, y] = [sclip(0, 4, 0), sclip(0, 4, 3.5)];
		expect(cutsOf(track('V1', [x, y]))).toEqual([]); // overlapping by 0.5 s
		// Listed out of order: still found, still earlier first.
		const [p, q] = [sclip(0, 4, 4), sclip(0, 4, 0)];
		expect(cutsOf(track('V1', [p, q]))).toEqual([{ trackId: 'track-V1', a: q.id, b: p.id, time: 4 }]);
	});

	test('a hair of float noise is still touching, a millisecond is not', () => {
		const [a, b] = [sclip(0, 4, 0), sclip(0, 4, 4 + 1e-12)];
		expect(cutsOf(track('V1', [a, b]))).toHaveLength(1);
		const c = sclip(0, 4, 4 + 2e-3);
		expect(cutsOf(track('V1', [a, c]))).toHaveLength(0);
	});

	test('a lone clip and an empty lane have none', () => {
		expect(cutsOf(track('V1', []))).toEqual([]);
		expect(cutsOf(track('V1', [sclip(0, 4, 0)]))).toEqual([]);
	});

	test('the nearest cut within reach, else none', () => {
		const [a, b, c] = [sclip(0, 4, 0), sclip(0, 4, 4), sclip(0, 4, 8)];
		const cuts = cutsOf(track('V1', [a, b, c]));
		expect(nearestCut(cuts, 4.05, 0.1)?.time).toBe(4);
		expect(nearestCut(cuts, 7.95, 0.1)?.time).toBe(8);
		expect(nearestCut(cuts, 6, 0.1)).toBeNull();
		expect(nearestCut(cuts, 4.1, 0.1)?.time).toBe(4); // exactly at the reach counts
		expect(nearestCut(cuts, 6, 2)?.time).toBe(4); // equidistant: the earlier cut
		expect(nearestCut([], 4, 10)).toBeNull();
		expect(CUT_REACH_PX).toBeGreaterThan(2);
	});
});

describe('slide neighbours', () => {
	test('a touching neighbour gives way, one across a gap does not', () => {
		const [a, b, c] = [sclip(0, 4, 0), sclip(0, 4, 4), sclip(0, 4, 9)];
		const t = track('V1', [a, b, c]);
		const n = slideNeighbours(t, b.id)!;
		expect([n.prev?.id, n.next?.id, n.prevTouches, n.nextTouches]).toEqual([a.id, c.id, true, false]);
		expect([...slideMembers(t, b.id)].sort()).toEqual([a.id, b.id].sort());
		expect([...slideMembers(t, c.id)]).toEqual([c.id]);
		expect(slideNeighbours(t, 'nope')).toBeNull();
	});

	test('the first and last clips have one side', () => {
		const [a, b] = [sclip(0, 4, 0), sclip(0, 4, 4)];
		const t = track('V1', [b, a]);
		expect(slideNeighbours(t, a.id)).toMatchObject({ prev: null, prevTouches: false, nextTouches: true });
		expect(slideNeighbours(t, b.id)).toMatchObject({ next: null, nextTouches: false, prevTouches: true });
	});
});

describe('holdToRange', () => {
	const range = { min: -1, max: 2, whyMin: 'no head', whyMax: 'no tail' };

	test('inside the range nothing is held', () => {
		expect(holdToRange(range, 1.5)).toEqual({ applied: 1.5, clamped: false, why: '' });
		expect(holdToRange(range, -1)).toEqual({ applied: -1, clamped: false, why: '' });
	});

	test('past a limit it stops there and says which', () => {
		expect(holdToRange(range, 5)).toEqual({ applied: 2, clamped: true, why: 'no tail' });
		expect(holdToRange(range, -3)).toEqual({ applied: -1, clamped: true, why: 'no head' });
	});

	test('float noise past a limit is not a clamp', () => {
		expect(holdToRange(range, 2 + 1e-9).clamped).toBe(false);
	});

	test('a range of nothing pins the pointer at zero', () => {
		const pinned = { min: 0, max: 0, whyMin: 'a', whyMax: 'b' };
		expect(holdToRange(pinned, 3)).toEqual({ applied: 0, clamped: true, why: 'b' });
		expect(holdToRange(pinned, -3)).toEqual({ applied: 0, clamped: true, why: 'a' });
		expect(holdToRange(pinned, 0).clamped).toBe(false);
	});

	test('a request that is not a number asks for nothing', () => {
		expect(holdToRange(range, NaN)).toEqual({ applied: 0, clamped: false, why: '' });
	});
});

describe('previewEdit — roll', () => {
	test('the ghosts are both clips with the cut moved', () => {
		const { t, a, b, edit } = cutPair();
		const p = previewEdit(t, edit, 1, footage(60));
		expect([p.ok, p.applied, p.clamped, p.why]).toEqual([true, 1, false, '']);
		expect(p.ghosts.map((g) => [g.id, g.role, g.start, g.dur])).toEqual([
			[a.id, 'a', 0, 5],
			[b.id, 'b', 5, 3]
		]);
		expect(p.ghosts[0].clip.source_out).toBe(15);
		expect(p.ghosts[1].clip.source_in).toBe(21);
	});

	test('it works on a reactive timeline, which structuredClone refuses', () => {
		const { t, a, b, edit } = cutPair();
		// The editor's timeline is `$state`: a proxy all the way down, and it is what the drag is handed.
		const reactive = (v: unknown): unknown =>
			v && typeof v === 'object'
				? new Proxy(v as object, { get: (target, key, recv) => reactive(Reflect.get(target, key, recv)) })
				: v;
		const proxied = reactive(t) as Timeline;
		const before = structuredClone(t);
		expect(() => structuredClone(proxied.tracks[0])).toThrow();
		const p = previewEdit(proxied, edit, 1, footage(60));
		expect([p.ok, p.applied, p.ghosts.map((g) => g.id)]).toEqual([true, 1, [a.id, b.id]]);
		expect(t).toEqual(before);
		const slide = previewEdit(proxied, { tool: 'slide', clipId: b.id }, 1, footage(60));
		expect(slide.ok).toBe(true);
	});

	test('it never touches the timeline it was shown', () => {
		const { t, edit } = cutPair();
		const before = structuredClone(t);
		previewEdit(t, edit, 1.5, footage(60));
		previewEdit(t, edit, 100, footage(60));
		previewEdit(t, edit, -100, footage(60));
		expect(t).toEqual(before);
	});

	test('past the footage it clamps, and the reason is the backend’s', () => {
		const [ca, cb] = [sclip(0, 4, 0), sclip(1, 5, 4)];
		const t = lane([ca, cb]);
		const p = previewEdit(t, { tool: 'roll', a: ca.id, b: cb.id }, 5, footage(6));
		expect([p.applied, p.clamped, p.why]).toEqual([2, true, 'the outgoing clip has no footage left to extend into']);
		const back = previewEdit(t, { tool: 'roll', a: ca.id, b: cb.id }, -5, footage(6));
		expect([back.applied, back.clamped]).toEqual([-1, true]);
		expect(back.why).toBe('the incoming clip has no footage left to extend into');
	});

	test('a cut with nowhere to go has nothing to draw and still says why', () => {
		const [ca, cb] = [sclip(0, 4, 0), sclip(0, 4, 4)]; // b opens on its first frame
		const p = previewEdit(lane([ca, cb]), { tool: 'roll', a: ca.id, b: cb.id }, -1, footage(60));
		expect([p.ok, p.applied, p.clamped, p.ghosts]).toEqual([true, 0, true, []]);
		expect(p.why).toContain('incoming clip has no footage left');
	});

	test('no movement is no preview, and not a limit', () => {
		const { t, edit } = cutPair();
		const p = previewEdit(t, edit, 0, footage(60));
		expect([p.ok, p.applied, p.clamped, p.why, p.ghosts]).toEqual([true, 0, false, '', []]);
	});

	test('what the backend would refuse comes back as a refusal, not a throw', () => {
		const [a, b] = [sclip(0, 4, 0), sclip(0, 4, 6)];
		const gap = previewEdit(lane([a, b]), { tool: 'roll', a: a.id, b: b.id }, 1, footage(60));
		expect(gap.ok).toBe(false);
		expect(gap.why).toContain('not adjacent');
		expect(gap.why.startsWith('invalid argument')).toBe(false);
		const { t, edit } = cutPair();
		const locked = { ...t, tracks: [{ ...t.tracks[0], locked: true }] };
		expect(previewEdit(locked, edit, 1, footage(60))).toMatchObject({ ok: false, why: 'track V1 is locked' });
		expect(previewEdit(t, { tool: 'roll', a: 'gone', b: 'gone2' }, 1, footage(60))).toMatchObject({ ok: false, why: 'clip not found: gone' });
		expect(previewEdit(t, edit, 1, new Map())).toMatchObject({ ok: false });
	});
});

describe('a landing on a frame is that frame', () => {
	test('the delta from the pointer’s rounded position puts the clip on k / fps', () => {
		// The gesture rounds once (frames.ts) and asks for `pos - cut`. The backend adds that
		// delta to the clip's start, so a landing is the frame to float noise — and to the
		// bit whenever the move is under 2x the start (the subtraction is then exact).
		const fps = 30;
		for (const k of [100, 200, 300, 380, 419, 500, 613]) {
			const [a, b] = [sclip(0, 12.5, 0), sclip(100, 108, 12.5)]; // b has 100 s of head to give
			const t = lane([a, b]);
			const pos = frameTime(k, fps);
			const roll = previewEdit(t, { tool: 'roll', a: a.id, b: b.id }, pos - b.timeline_start, footage(1e6));
			expect([roll.clamped, roll.ghosts.length]).toEqual([false, 2]);
			const slide = previewEdit(t, { tool: 'slide', clipId: b.id }, pos - b.timeline_start, footage(1e6));
			expect([slide.clamped, slide.ghosts.length]).toEqual([false, 2]);
			const landed = [roll.ghosts[1].start, slide.ghosts.find((g) => g.role === 'clip')!.start];
			for (const at of landed) {
				near(at, pos, 1e-12);
				if (pos / b.timeline_start <= 2 && pos / b.timeline_start >= 0.5) expect(at).toBe(pos);
			}
		}
	});
});

describe('previewEdit — slip', () => {
	test('the clip stays where it is and its window moves', () => {
		const c = sclip(10, 14, 3);
		const p = previewEdit(lane([c]), { tool: 'slip', clipId: c.id }, 2, footage(60));
		expect([p.applied, p.ghosts.length]).toEqual([1 * 2, 1]);
		expect(p.ghosts[0]).toMatchObject({ role: 'clip', start: 3, dur: 4 });
		expect([p.ghosts[0].clip.source_in, p.ghosts[0].clip.source_out]).toEqual([12, 16]);
	});

	test('it clamps to the footage either way', () => {
		const c = sclip(1, 5, 0);
		const edit: GestureEdit = { tool: 'slip', clipId: c.id };
		const later = previewEdit(lane([c]), edit, 10, footage(8));
		expect([later.applied, later.clamped, later.why]).toEqual([3, true, "there is no footage left after the clip's out-point"]);
		const earlier = previewEdit(lane([c]), edit, -10, footage(8));
		expect([earlier.applied, earlier.clamped, earlier.why]).toEqual([-1, true, "there is no footage left before the clip's in-point"]);
	});

	test('a still has nothing to slip', () => {
		const c = sclip(0, 5, 0);
		const p = previewEdit(lane([c]), { tool: 'slip', clipId: c.id }, 1, new Map([['asset', Infinity]]));
		expect(p.ok).toBe(false);
		expect(p.why).toContain('still image');
	});

	test('a reversed clip’s window moves the mirrored way, so the sign means the same on screen', () => {
		const c = sclip(10, 14, 0, { speed: -1 });
		const p = previewEdit(lane([c]), { tool: 'slip', clipId: c.id }, 2, footage(60));
		expect([p.ghosts[0].clip.source_in, p.ghosts[0].clip.source_out]).toEqual([8, 12]);
	});
});

describe('slipDelta — the pointer into a slip', () => {
	test('the content follows the pointer: dragging right shows earlier footage', () => {
		near(slipDelta(1, {}, 30), -1);
		near(slipDelta(-1, {}, 30), 1);
	});

	test('a reversed clip needs no special case', () => {
		near(slipDelta(1, { speed: -1 }, 30), -1);
	});

	test('it is rounded to a frame of the cut once, then scaled by speed', () => {
		const dt = 0.5 + 0.004; // half a second and a bit, 30 fps → 15 frames
		near(slipDelta(dt, {}, 30), -0.5);
		near(slipDelta(dt, { speed: 2 }, 30), -1);
		near(slipDelta(dt, { speed: 0.5 }, 30), -0.25);
		// …and what the pointer asked for is on a frame, so is what the content moved.
		expect(onFrame(-slipDelta(0.3333, {}, 24), 24)).toBe(true);
	});

	test('no travel is zero (and not -0)', () => {
		expect(Object.is(slipDelta(0, {}, 30), 0)).toBe(true);
		expect(Object.is(slipDelta(0.004, {}, 30), 0)).toBe(true);
	});
});

describe('previewEdit — slide', () => {
	// a [0,4) src 0..4, b [4,8) src 20..24, c [8,12) src 30..34, all in 60 s of footage.
	function trio() {
		const [a, b, c] = [sclip(0, 4, 0), sclip(20, 24, 4), sclip(30, 34, 8)];
		return { t: lane([a, b, c]), a, b, c };
	}

	test('the clip moves and the neighbours that touch it give way', () => {
		const { t, a, b, c } = trio();
		const p = previewEdit(t, { tool: 'slide', clipId: b.id }, 1, footage(60));
		expect([p.ok, p.applied, p.clamped]).toEqual([true, 1, false]);
		expect(p.ghosts.map((g) => [g.id, g.role, ...span(g.clip)])).toEqual([
			[a.id, 'prev', 0, 5],
			[b.id, 'clip', 5, 9],
			[c.id, 'next', 9, 12]
		]);
	});

	test('the first clip has no previous to give way, the last no next', () => {
		const { t, a, c } = trio();
		const first = previewEdit(t, { tool: 'slide', clipId: a.id }, 1, footage(60));
		expect(first.ghosts.map((g) => g.role)).toEqual(['clip', 'next']);
		const last = previewEdit(t, { tool: 'slide', clipId: c.id }, 1, footage(60));
		expect(last.ghosts.map((g) => g.role)).toEqual(['prev', 'clip']);
		expect(last.clamped).toBe(false); // nothing after it: the track just gets longer
	});

	test('a neighbour across a gap is left alone and stops the slide', () => {
		const [a, b] = [sclip(0, 4, 0), sclip(0, 4, 5)];
		const p = previewEdit(lane([a, b]), { tool: 'slide', clipId: a.id }, 3, footage(60));
		expect([p.applied, p.clamped]).toEqual([1, true]);
		expect(p.why).toContain('not touching');
		expect(p.ghosts.map((g) => g.role)).toEqual(['clip']);
	});

	test('it cannot take a neighbour below the floor, and says so', () => {
		const { t, b } = trio();
		const p = previewEdit(t, { tool: 'slide', clipId: b.id }, 99, footage(60));
		near(p.applied, 4 - 0.05);
		expect(p.clamped).toBe(true);
		expect(p.why).toBe('the next clip would be shorter than 0.05s');
	});
});

describe('the preview is the edit', () => {
	// A small deterministic generator: the property is "whatever the pointer asks, the
	// ghost is exactly what the real edit leaves, and it never asks more than the range".
	let state = 20240607;
	const rand = () => {
		state = (state * 1664525 + 1013904223) >>> 0;
		return state / 2 ** 32;
	};

	function randomLane(): Timeline {
		const clips: Clip[] = [];
		let at = rand() < 0.3 ? 0.5 : 0;
		for (let i = 0; i < 4; i++) {
			const len = 0.4 + rand() * 4;
			const si = rand() * 30;
			const speed = [1, 1, 2, 0.5, -1][Math.floor(rand() * 5)];
			clips.push(sclip(si, si + len * Math.abs(speed), at, { speed }));
			at += len + (rand() < 0.25 ? 0.2 + rand() : 0); // mostly touching
		}
		return lane(clips);
	}

	test('roll, slip and slide agree with the edits they preview', () => {
		for (let round = 0; round < 250; round++) {
			const t = randomLane();
			const before = structuredClone(t);
			const lim = footage(45);
			const requested = (rand() - 0.5) * 12;
			const clips = t.tracks[0].clips;
			const cuts = cutsOf(t.tracks[0]);
			const edits: GestureEdit[] = [
				{ tool: 'slip', clipId: clips[Math.floor(rand() * clips.length)].id },
				{ tool: 'slide', clipId: clips[Math.floor(rand() * clips.length)].id },
				...(cuts.length ? [{ tool: 'roll', a: cuts[0].a, b: cuts[0].b } satisfies GestureEdit] : [])
			];
			for (const edit of edits) {
				const p = previewEdit(t, edit, requested, lim);
				expect(t).toEqual(before);
				const range =
					edit.tool === 'roll' ? rollRange(t, edit.a, edit.b, lim) : edit.tool === 'slip' ? slipRange(t, edit.clipId, lim) : slideRange(t, edit.clipId, lim);
				expect(p.applied).toBeGreaterThanOrEqual(range.min - 1e-12);
				expect(p.applied).toBeLessThanOrEqual(range.max + 1e-12);
				expect(p.clamped).toBe(Math.abs(p.applied - requested) > 1e-6);
				if (p.ghosts.length === 0) continue;
				// The real thing, on a copy.
				const real = structuredClone(t);
				const out =
					edit.tool === 'roll'
						? rollEdit(real, edit.a, edit.b, p.applied, lim)
						: edit.tool === 'slip'
							? slipClip(real, edit.clipId, p.applied, lim)
							: slideClip(real, edit.clipId, p.applied, lim);
				expect(p.ghosts.map((g) => g.clip)).toEqual(out.clips);
				for (const g of p.ghosts) {
					const landed = real.tracks[0].clips.find((c) => c.id === g.id)!;
					expect([g.start, g.dur]).toEqual([landed.timeline_start, clipDuration(landed)]);
				}
			}
		}
	});

	test('a roll leaves the span of the pair alone, a slide the span of the three', () => {
		for (let round = 0; round < 100; round++) {
			const t = randomLane();
			const cuts = cutsOf(t.tracks[0]);
			if (!cuts.length) continue;
			const cut = cuts[Math.floor(rand() * cuts.length)];
			const p = previewEdit(t, { tool: 'roll', a: cut.a, b: cut.b }, (rand() - 0.5) * 4, footage(45));
			if (p.ghosts.length !== 2) continue;
			const [ga, gb] = p.ghosts;
			const [a, b] = [t.tracks[0].clips.find((c) => c.id === cut.a)!, t.tracks[0].clips.find((c) => c.id === cut.b)!];
			near(ga.start, a.timeline_start);
			near(gb.start + gb.dur, b.timeline_start + clipDuration(b), 1e-9);
			near(ga.start + ga.dur, gb.start, 1e-9); // still touching
		}
	});
});

describe('words', () => {
	test('a shift in frames and seconds', () => {
		expect(deltaLabel(0.4, 30)).toBe('+12 f · +0.40 s');
		expect(deltaLabel(-0.5, 24)).toBe('−12 f · −0.50 s');
		expect(deltaLabel(0, 30)).toBe('0 f');
		expect(deltaLabel(0.4, 0)).toBe('+12 f · +0.40 s'); // no usable rate: 30
	});

	test('each tool has a verb and a hint that says it ignores ripple', () => {
		for (const tool of ['roll', 'slip', 'slide'] as const) {
			expect(TOOL_VERB[tool].length).toBeGreaterThan(2);
			expect(TOOL_HINT[tool]).toContain('Ignores ripple mode');
		}
	});

	test('the readout names the new cut, the new window, the new position', () => {
		const { t, edit } = cutPair();
		const roll = readoutFor(previewEdit(t, edit, 1, footage(60)), 30);
		expect(roll).toEqual({ title: 'Roll +30 f · +1.00 s · cut 00:05:00', detail: null, tone: 'ok' });

		const c = sclip(10, 14, 3);
		const slip = readoutFor(previewEdit(lane([c]), { tool: 'slip', clipId: c.id }, 2, footage(60)), 30);
		expect(slip.title).toBe('Slip +60 f · +2.00 s · in 00:12:00 → out 00:16:00');

		// At 2× a 2 s source slip is one second of screen.
		const fast = sclip(10, 18, 0, { speed: 2 });
		const fastSlip = readoutFor(previewEdit(lane([fast]), { tool: 'slip', clipId: fast.id }, 2, footage(60)), 30);
		expect(fastSlip.title.startsWith('Slip +30 f · +1.00 s')).toBe(true);

		const [a, b] = [sclip(0, 4, 0), sclip(0, 4, 4)];
		const slide = readoutFor(previewEdit(lane([a, b]), { tool: 'slide', clipId: b.id }, 1, footage(60)), 30);
		expect(slide.title).toBe('Slide +30 f · +1.00 s · now at 00:05:00');
	});

	test('a limit is a second line in the limit tone; a refusal is red', () => {
		const [ca, cb] = [sclip(0, 4, 0), sclip(1, 5, 4)];
		const clamped = readoutFor(previewEdit(lane([ca, cb]), { tool: 'roll', a: ca.id, b: cb.id }, 5, footage(6)), 30);
		expect([clamped.tone, clamped.detail]).toEqual(['limit', 'the outgoing clip has no footage left to extend into']);
		const [x, y] = [sclip(0, 4, 0), sclip(0, 4, 6)];
		const refused = readoutFor(previewEdit(lane([x, y]), { tool: 'roll', a: x.id, b: y.id }, 1, footage(60)), 30);
		expect(refused.tone).toBe('refused');
		expect(refused.detail).toContain('not adjacent');
		// Pinned at a limit it cannot leave: nothing to draw, but the reason is still on offer.
		const [pa, pb] = [sclip(0, 4, 0), sclip(0, 4, 4)];
		const pinned = readoutFor(previewEdit(lane([pa, pb]), { tool: 'roll', a: pa.id, b: pb.id }, -1, footage(60)), 30);
		expect(pinned.title).toBe('Roll — at the limit');
		expect(pinned.tone).toBe('limit');
		expect(pinned.detail).toContain('incoming clip has no footage left');
		// Not moved yet: plain, no detail.
		expect(readoutFor(previewEdit(lane([pa, pb]), { tool: 'roll', a: pa.id, b: pb.id }, 0, footage(60)), 30)).toEqual({
			title: 'Roll',
			detail: null,
			tone: 'ok'
		});
	});

	test('the release notice says how far it got, or that it could not move', () => {
		const [ca, cb] = [sclip(0, 4, 0), sclip(1, 5, 4)];
		const edit: GestureEdit = { tool: 'roll', a: ca.id, b: cb.id };
		const t = lane([ca, cb]);
		expect(limitNotice(previewEdit(t, edit, 5, footage(6)), 30)).toBe(
			'Roll stopped at +60 f · +2.00 s — the outgoing clip has no footage left to extend into'
		);
		const [pa, pb] = [sclip(0, 4, 0), sclip(0, 4, 4)];
		expect(limitNotice(previewEdit(lane([pa, pb]), { tool: 'roll', a: pa.id, b: pb.id }, -1, footage(60)), 30)).toBe(
			"Can't roll the cut earlier — the incoming clip has no footage left to extend into"
		);
		const s = sclip(1, 5, 0);
		expect(limitNotice(previewEdit(lane([s]), { tool: 'slip', clipId: s.id }, 10, footage(8)), 30)).toBe(
			"Slip stopped at +90 f · +3.00 s — there is no footage left after the clip's out-point"
		);
		expect(limitNotice(previewEdit(lane([s]), { tool: 'slip', clipId: s.id }, -10, footage(8)), 30)).toContain('−30 f');
	});
});

describe('the monitor', () => {
	test('a forward clip opens on its in-point and closes one frame short of its out', () => {
		const c = sclip(10, 14, 0);
		near(edgeFrameTime(c, 'first', 30), 10);
		near(edgeFrameTime(c, 'last', 30), 14 - 1 / 30);
	});

	test('a reversed clip plays its window backwards, so first and last swap', () => {
		const c = sclip(10, 14, 0, { speed: -1 });
		near(edgeFrameTime(c, 'first', 30), 14 - 1 / 30);
		near(edgeFrameTime(c, 'last', 30), 10);
	});

	test('a frame is the cut’s, scaled by speed; never before the footage or past the window', () => {
		near(edgeFrameTime(sclip(10, 14, 0, { speed: 2 }), 'last', 30), 14 - 2 / 30);
		near(edgeFrameTime(sclip(0, 0.01, 0), 'last', 30), 0); // shorter than a frame: its one frame
		expect(edgeFrameTime(sclip(0, 4, 0), 'first', 30)).toBe(0);
		near(edgeFrameTime(sclip(10, 14, 0), 'last', 0), 14); // no usable rate: the window's end
	});

	test('a roll shows the outgoing last frame beside the incoming first', () => {
		const { t, a, b, edit } = cutPair();
		const m = monitorFor(previewEdit(t, edit, 1, footage(60)), 30, 'video')!;
		expect(m.cells.map((c) => [c.label, c.clipId, c.assetId])).toEqual([
			['Out', a.id, 'asset'],
			['In', b.id, 'asset']
		]);
		near(m.cells[0].time, 15 - 1 / 30); // a's window now ends at 15
		near(m.cells[1].time, 21); // b opens on 21
		expect(m.cells[1].timecode).toBe('00:21:00');
		expect(m.title).toBe('Roll +30 f · +1.00 s · cut 00:05:00');
	});

	test('a slip shows the new first and last frame of the same clip', () => {
		const c = sclip(10, 14, 3);
		const m = monitorFor(previewEdit(lane([c]), { tool: 'slip', clipId: c.id }, 2, footage(60)), 30, 'video')!;
		expect(m.cells.map((x) => [x.label, x.clipId])).toEqual([
			['In', c.id],
			['Out', c.id]
		]);
		near(m.cells[0].time, 12);
		near(m.cells[1].time, 16 - 1 / 30);
	});

	test('a slide shows the edges of the neighbours that gave way', () => {
		const [a, b, c] = [sclip(0, 4, 0), sclip(20, 24, 4), sclip(30, 34, 8)];
		const t = lane([a, b, c]);
		const both = monitorFor(previewEdit(t, { tool: 'slide', clipId: b.id }, 1, footage(60)), 30, 'video')!;
		expect(both.cells.map((x) => [x.label, x.clipId])).toEqual([
			['Out', a.id],
			['In', c.id]
		]);
		const first = monitorFor(previewEdit(t, { tool: 'slide', clipId: a.id }, 1, footage(60)), 30, 'video')!;
		expect(first.cells.map((x) => x.label)).toEqual(['In']);
	});

	test('no picture, no monitor: audio, a slide with nothing touching, a drag that has not moved', () => {
		const { t, edit } = cutPair();
		expect(monitorFor(previewEdit(t, edit, 1, footage(60)), 30, 'audio')).toBeNull();
		expect(monitorFor(previewEdit(t, edit, 0, footage(60)), 30, 'video')).toBeNull();
		const [x, y] = [sclip(0, 4, 0), sclip(0, 4, 6)];
		expect(monitorFor(previewEdit(lane([x, y]), { tool: 'slide', clipId: x.id }, 1, footage(60)), 30, 'video')).toBeNull();
	});
});

describe('trim to the playhead', () => {
	const c = sclip(10, 18, 4); // [4, 12)

	test('the playhead is put on a frame', () => {
		const left = playheadCut(c, 6.012, 30, 'left');
		expect('at' in left && onFrame(left.at, 30)).toBe(true);
		expect('at' in left && left.at).toBe(snapToFrame(6.012, 30));
		const right = playheadCut(c, 9.99, 30, 'right');
		expect('at' in right && right.at).toBe(snapToFrame(9.99, 30));
	});

	test('a playhead outside the clip, or on its edge, is not a cut', () => {
		for (const time of [3, 4, 12, 13]) expect(playheadCut(c, time, 30, 'left')).toEqual({ why: 'the playhead is not inside the clip' });
	});

	test('it holds half a frame inside, like the razor', () => {
		const r = playheadCut(c, 4.001, 30, 'left');
		expect('at' in r && r.at).toBeGreaterThanOrEqual(4 + 0.5 / 30 - 1e-9);
	});

	test('the backend’s floor is a sentence, not an error code', () => {
		const left = playheadCut(c, 11.97, 30, 'left'); // would keep 0.03 s
		expect(left).toEqual({ why: 'that would leave only 0.03s of the clip — remove the clip instead' });
		const right = playheadCut(c, 4.03, 30, 'right');
		expect('why' in right && right.why).toContain('only 0.03s');
		expect('at' in playheadCut(c, 11.9, 30, 'left')).toBe(true); // 0.1 s is fine
		const tiny = sclip(0, 0.04, 0);
		expect('why' in playheadCut(tiny, 0.02, 30, 'left')).toBe(true);
	});

	test('a lone clip shorter than a frame cannot be cut on one', () => {
		const sliver = sclip(0, 0.02, 0);
		expect(playheadCut(sliver, 0.01, 30, 'left')).toEqual({ why: 'that clip is too short to cut on a frame' });
	});

	test('it acts on every selected clip the playhead is in', () => {
		const v = sclip(0, 10, 0);
		const a = sclip(0, 10, 0);
		const other = sclip(0, 10, 20);
		const t: Timeline = { tracks: [track('V1', [v, other]), track('A1', [a], {}, 'audio')] };
		const plan = planPlayheadTrim(t, [v.id, a.id, other.id], 5, 30, 'left');
		expect(plan.trims.map((x) => [x.clipId, x.trackName, x.at])).toEqual([
			[v.id, 'V1', 5],
			[a.id, 'A1', 5]
		]);
		expect([plan.under, plan.problems]).toEqual([2, []]);
		// Only the selection: the unselected audio clip is left alone.
		expect(planPlayheadTrim(t, [v.id], 5, 30, 'right').trims.map((x) => x.clipId)).toEqual([v.id]);
	});

	test('a locked track and a clip too short are reported, the rest still cut', () => {
		const [v, a] = [sclip(0, 10, 0), sclip(0, 10, 0)];
		const t: Timeline = { tracks: [track('V1', [v]), track('A1', [a], { locked: true }, 'audio')] };
		const plan = planPlayheadTrim(t, [v.id, a.id], 5, 30, 'left');
		expect(plan.trims.map((x) => x.clipId)).toEqual([v.id]);
		expect(plan.problems).toEqual(['A1 is locked']);
		const near_ = planPlayheadTrim(t, [v.id], 9.98, 30, 'left');
		expect(near_.trims).toEqual([]);
		expect(near_.problems[0]).toContain('V1: that would leave only');
	});

	test('one clip per track: a lane with two selected clips under the playhead cuts one and says so', () => {
		// An old project that overlaps itself. The backend's group trim takes one clip per
		// track, so the plan sends the first and reports the second rather than failing the lot.
		const [first, second] = [sclip(0, 10, 0), sclip(0, 10, 2)];
		const partner = sclip(0, 10, 0);
		const t: Timeline = { tracks: [track('V1', [first, second]), track('A1', [partner], {}, 'audio')] };
		const plan = planPlayheadTrim(t, [first.id, second.id, partner.id], 5, 30, 'left');
		expect(plan.trims.map((x) => x.clipId)).toEqual([first.id, partner.id]);
		expect(plan.problems).toEqual(['V1: two selected clips are under the playhead — trim them one at a time']);
		expect(plan.under).toBe(3);
	});

	test('a gesture is given up when a clip it edits is gone', () => {
		const { t, a, b } = cutPair();
		const roll: GestureEdit = { tool: 'roll', a: a.id, b: b.id };
		expect(subjectsPresent(t, roll)).toBe(true);
		expect(subjectsPresent(t, { tool: 'slide', clipId: b.id })).toBe(true);
		const without = lane([a]);
		expect(subjectsPresent(without, roll)).toBe(false); // one side of the cut removed
		expect(subjectsPresent(without, { tool: 'slip', clipId: b.id })).toBe(false);
		expect(subjectsPresent(without, { tool: 'slide', clipId: a.id })).toBe(true);
		expect(subjectsPresent({ tracks: [] }, roll)).toBe(false);
	});

	test('what to say when there is nothing to cut', () => {
		const v = sclip(0, 10, 0);
		const t = lane([v]);
		expect(trimNotice(planPlayheadTrim(t, [], 5, 30, 'left'), 0, 'left')).toContain('Select a clip first');
		expect(trimNotice(planPlayheadTrim(t, [v.id], 15, 30, 'left'), 1, 'left')).toBe('Move the playhead into the selected clip to trim the start');
		expect(trimNotice(planPlayheadTrim(t, [v.id, 'x'], 15, 30, 'right'), 2, 'right')).toBe('Move the playhead into the selected clips to trim the end');
		const locked = lane([v], { locked: true });
		expect(trimNotice(planPlayheadTrim(locked, [v.id], 5, 30, 'left'), 1, 'left')).toBe('V1 is locked');
	});
});
