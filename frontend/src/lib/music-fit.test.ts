import { describe, expect, test } from 'bun:test';
import { barGrid, BEAT_MIN_CONFIDENCE } from './beats';
import {
	barStart,
	firstDownbeat,
	fitMusicOnto,
	FIT_FADE_S,
	gridBarSeconds,
	gridBeats,
	gridBpm,
	gridDownbeats,
	musicFitClips,
	jumpCost,
	MAX_FIT_BARS,
	musicFitInputs,
	phraseMatches,
	planMusicFit,
	planWalk,
	SPLICE_SIMILARITY,
	tempoFromGrid,
	toSamples,
	wholeBars
} from './music-fit';
import type { Asset, BeatGrid, Clip, MusicAnalysis, MusicFit, PhraseMatch, Timeline } from './types';
import { clipDuration } from './types';

// These replay the Rust tests of `model/music_fit.rs` (and the grid / phrase ones in
// `engine/music.rs`, and the two `fit_music` tests in `project.rs`) case for case: the same song,
// the same numbers. A rule changed in kerf-core has to change in `music-fit.ts`, or one of these
// names it.

const SR = 48_000;

const chord = (pc: number): number[] => {
	const c = new Array<number>(12).fill(0);
	c[pc] = 1;
	return c;
};

/** 16 bars of 2 s after a 1 s intro, then a 1.5 s ending: an 8-bar progression played twice,
 *  whose two halves differ (so only 8-bar repeats and the 4-bar halves of them match). */
function song(): MusicAnalysis {
	const prog = [0, 9, 5, 7, 0, 9, 2, 4];
	const bar_chroma = Array.from({ length: 16 }, (_, k) => chord(prog[k % 8]));
	return {
		grid: { period_s: 0.5, phase_s: 0, downbeat_offset: 2, beats_per_bar: 4 },
		duration: 1 + 32 + 1.5,
		bar_chroma,
		phrases: phraseMatches(bar_chroma, SPLICE_SIMILARITY)
	};
}

function assertContiguous(fit: MusicFit) {
	let at = 0;
	for (const s of fit.segments) {
		expect(Math.abs(s.output_start - at)).toBeLessThan(1e-9);
		expect(s.source_end - s.source_start).toBeGreaterThan(0);
		at += s.source_end - s.source_start;
	}
	expect(Math.abs(fit.duration - at)).toBeLessThan(1e-9);
	expect(Math.abs(fit.remainder - (fit.target - fit.duration))).toBeLessThan(1e-9);
}

describe('the bar grid', () => {
	const g: BeatGrid = { period_s: 0.5, phase_s: 0.25, downbeat_offset: 1, beats_per_bar: 4 };

	test('lists beats, bars and whole bars (beat_grid_lists_beats_bars_and_whole_bars)', () => {
		expect(firstDownbeat(g)).toBe(0.75);
		expect(gridBarSeconds(g)).toBe(2);
		expect(gridBeats(g, 1.5)).toEqual([0.25, 0.75, 1.25]);
		expect(gridDownbeats(g, 5)).toEqual([0.75, 2.75, 4.75]);
		expect(wholeBars(g, 4.75)).toBe(2);
		expect(wholeBars(g, 4.74)).toBe(1);
		expect(wholeBars(g, 0.5)).toBe(0);
		expect(gridBpm(g)).toBe(120);
		expect(barStart(g, 2)).toBe(4.75);
	});

	test('a fitted grid becomes the tempo and its bars land on the timeline', () => {
		const grid: BeatGrid = { period_s: 0.5, phase_s: -0.004, downbeat_offset: 1, beats_per_bar: 4 };
		const t = tempoFromGrid(grid, 5, 0.1);
		expect(t.bpm).toBe(120);
		expect(t.confidence).toBe(BEAT_MIN_CONFIDENCE);
		expect(t.beats.length).toBe(10); // the beat a hair before 0 is dropped
		expect(t.downbeats).toEqual([0.496, 2.496, 4.496]);
		const clip: Clip = { ...audioClip('music', 1, 5, 10), speed: 1 };
		const tl = timelineOf([], [clip]);
		const bars = barGrid(tl, () => t);
		expect(bars.length).toBe(2);
		expect(Math.abs(bars[0] - 11.496)).toBeLessThan(1e-9);
		expect(Math.abs(bars[1] - 13.496)).toBeLessThan(1e-9);
	});
});

describe('phrase matches', () => {
	test('need every bar to match (phrase_matches_need_every_bar_to_match)', () => {
		const a = chord(0);
		const b = chord(7);
		const chroma = [a, a, a, a, a, a, a, b, a, a, a, a];
		const m = phraseMatches(chroma, SPLICE_SIMILARITY);
		expect(m).toContainEqual({ a: 0, b: 8, bars: 4 });
		expect(m.some((p) => p.b === 4)).toBe(false); // bar 7 breaks 4..8
		expect(m.some((p) => p.bars === 8)).toBe(false);
		expect(phraseMatches([a, a], SPLICE_SIMILARITY)).toEqual([]);
	});

	test('the eight-bar song repeats itself at 8 bars and in its 4-bar halves', () => {
		const m = song().phrases;
		expect(m).toContainEqual({ a: 0, b: 8, bars: 8 });
		expect(m).toContainEqual({ a: 0, b: 8, bars: 4 });
		expect(m).toContainEqual({ a: 4, b: 12, bars: 4 });
		expect(m.some((p) => p.a === 0 && p.b === 4)).toBe(false);
	});
});

describe('plan_music_fit', () => {
	test('the natural length is the whole file', () => {
		const m = song();
		const fit = planMusicFit(m, m.duration, SR);
		expect(fit.segments.length).toBe(1);
		expect(fit.segments[0].source_start).toBe(0);
		expect(fit.segments[0].source_end).toBe(m.duration);
		expect([fit.splices, fit.bars]).toEqual([0, 16]);
		expect(fit.remainder).toBe(0);
	});

	test('a longer target repeats a whole phrase and keeps intro and ending', () => {
		const m = song();
		const fit = planMusicFit(m, m.duration + 16, SR);
		assertContiguous(fit);
		expect(fit.bars).toBe(24);
		expect(fit.splices).toBe(1);
		expect(Math.abs(fit.remainder)).toBeLessThan(1e-9);
		expect(fit.segments[0].source_start).toBe(0); // the intro plays
		expect(fit.segments[fit.segments.length - 1].source_end).toBe(m.duration); // the ending plays
		expect(fit.segments[0].source_end).toBe(17); // the splice is on a phrase boundary
		expect(fit.segments[1].source_start).toBe(1); // back to the start of the repeated phrase
	});

	test('a shorter target drops a whole phrase', () => {
		const m = song();
		const fit = planMusicFit(m, m.duration - 16, SR);
		assertContiguous(fit);
		expect([fit.bars, fit.splices]).toEqual([8, 1]);
		expect(Math.abs(fit.remainder)).toBeLessThan(1e-9);
		expect(fit.segments[0].source_start).toBe(0);
		expect(fit.segments[fit.segments.length - 1].source_end).toBe(m.duration);
	});

	test('an off-grid target takes the nearest arrangement and reports the rest', () => {
		const m = song();
		// The repeats are 8 bars apart, so the reachable lengths step by 8 bars (16 s).
		const short = planMusicFit(m, m.duration + 6, SR);
		assertContiguous(short);
		expect(short.bars).toBe(16);
		expect(Math.abs(short.remainder - 6)).toBeLessThan(1e-9);
		const over = planMusicFit(m, m.duration + 10, SR);
		expect(over.bars).toBe(24);
		expect(Math.abs(over.remainder - -6)).toBeLessThan(1e-9);
		// Halfway between two arrangements: the longer one, which a fade can shorten.
		const tie = planMusicFit(m, m.duration + 8, SR);
		expect(tie.bars).toBe(24);
	});

	test('a long target loops until it fits', () => {
		const m = song();
		const fit = planMusicFit(m, 150, SR);
		assertContiguous(fit);
		expect(Math.abs(fit.remainder)).toBeLessThanOrEqual(4 + 1e-9);
		expect(fit.segments[fit.segments.length - 1].source_end).toBe(m.duration);
	});

	test('music without repeats plays through and reports the difference', () => {
		const m = song();
		m.phrases = [];
		const fit = planMusicFit(m, 60, SR);
		expect(fit.segments.length).toBe(1);
		expect(Math.abs(fit.remainder - (60 - m.duration))).toBeLessThan(1e-9);
	});

	test('splices land on the sample grid without drift', () => {
		const m = song();
		m.grid.period_s = 0.4987;
		m.grid.phase_s = 0.0113;
		m.duration = barStart(m.grid, 16) + 1.234567;
		const fit = planMusicFit(m, 120, 44_100);
		assertContiguous(fit);
		expect(fit.splices).toBeGreaterThanOrEqual(2);
		for (const s of fit.segments) {
			for (const t of [s.source_start, s.source_end, s.output_start]) {
				const n = t * 44_100;
				expect(Math.abs(n - Math.round(n))).toBeLessThan(1e-6);
			}
		}
	});

	test('no bars is the whole file', () => {
		const m = song();
		m.bar_chroma = [];
		m.phrases = [];
		m.duration = 3;
		const fit = planMusicFit(m, 10, SR);
		expect(fit.segments.length).toBe(1);
		expect(fit.duration).toBe(3);
	});

	test('rounds to the sample grid the way Rust does, halves away from zero', () => {
		expect(toSamples(2.5 / 10, 10)).toBe(0.3);
		expect(toSamples(-2.5 / 10, 10)).toBe(-0.3);
		expect(toSamples(1.234, 0)).toBe(1.234);
	});
});

describe('music_fit_clips', () => {
	const template = (): Clip => ({
		...audioClip('music', 0, 34.5, 10),
		id: 'template',
		volume: 0.5,
		fade_in: 0.25
	});
	let n = 0;
	const ids = () => `fit-${n++}`;

	test('crossfade over one window centred on each splice', () => {
		const m = song();
		const fit = planMusicFit(m, m.duration + 16, SR);
		const clips = musicFitClips(template(), fit, SR, false, ids);
		expect(clips.length).toBe(2);
		const [a, b] = clips;
		const splice = 10 + fit.segments[1].output_start;
		// Adjacent, so the transition pairs them.
		expect(a.timeline_start + clipDuration(a)).toBe(b.timeline_start);
		expect(Math.abs(b.timeline_start - (splice - 0.005))).toBeLessThan(1e-9);
		expect(Math.abs(a.source_out - (fit.segments[0].source_end - 0.005))).toBeLessThan(1e-9);
		expect(Math.abs(b.source_in - (fit.segments[1].source_start - 0.005))).toBeLessThan(1e-9);
		expect(b.transition_in?.kind).toBe('crossfade');
		expect(Math.abs((b.transition_in?.duration ?? 0) - 0.01)).toBeLessThan(1e-12);
		expect([a.fade_in, b.fade_out]).toEqual([0.25, 0]);
		expect(clips.every((c) => c.volume === 0.5 && c.id !== 'template')).toBe(true);
		expect(a.transition_in ?? null).toBeNull();
	});

	test('an overrun is cut at the target and faded', () => {
		const m = song();
		const fit = planMusicFit(m, m.duration + 10, SR);
		expect(fit.remainder).toBeLessThan(0);
		const clips = musicFitClips(template(), fit, SR, true, ids);
		const last = clips[clips.length - 1];
		expect(Math.abs(last.timeline_start + clipDuration(last) - (10 + fit.target))).toBeLessThan(1e-6);
		expect(last.fade_out).toBe(FIT_FADE_S);
		const kept = musicFitClips(template(), fit, SR, false, ids);
		const tail = kept[kept.length - 1];
		expect(tail.timeline_start + clipDuration(tail)).toBeGreaterThan(10 + fit.target);
		expect(tail.fade_out).toBe(0);
	});

	test('a copy keeps the clip but not its link, keyframes or transition', () => {
		const t: Clip = {
			...template(),
			link_id: 'pair',
			keyframes: [{ time: 0, scale: 1, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 }],
			transition_in: { kind: 'dip_to_black', duration: 1 },
			fade_out: 0.75
		};
		const fit = planMusicFit(song(), 34.5 + 16, SR);
		const clips = musicFitClips(t, fit, SR, false, ids);
		expect(clips.every((c) => !c.link_id && (c.keyframes ?? []).length === 0)).toBe(true);
		expect(clips[0].transition_in ?? null).toBeNull();
		// The first keeps the clip's fade-in, the last its fade-out.
		expect([clips[0].fade_in, clips[clips.length - 1].fade_out]).toEqual([0.25, 0.75]);
	});
});

describe('the walk planner against a brute force', () => {
	/** A small deterministic generator, so a failing case can be named. */
	function rng(seed: number): () => number {
		let s = seed >>> 0;
		return () => (s = (Math.imul(s, 1664525) + 1013904223) >>> 0) / 2 ** 32;
	}

	/** What the spec says, worked out the long way: relax every (count, bar, just-jumped) state
	 *  until nothing improves, then take the length nearest `wanted` (a tie to the longer) at the
	 *  least cost. Nothing here is the planner's table. */
	function reference(bars: number, phrases: readonly PhraseMatch[], wanted: number) {
		const edges = new Map<string, number>();
		for (const p of phrases) {
			if (p.a + p.bars > bars || p.b + p.bars > bars || p.a === p.b) continue;
			for (const [from, to] of [
				[p.a, p.b],
				[p.b, p.a]
			]) edges.set(`${from}>${to}`, Math.min(edges.get(`${from}>${to}`) ?? Infinity, jumpCost(p.bars)));
		}
		const maxCount = Math.min(Math.max(Math.ceil(Math.max(wanted, 0)), bars) + bars, Math.max(MAX_FIT_BARS, 2 * bars));
		const best = new Map<string, number>([['0,0,0', 0]]);
		const queue = ['0,0,0'];
		while (queue.length > 0) {
			const key = queue.shift()!;
			const cost = best.get(key)!;
			const [count, pos, jumped] = key.split(',').map(Number);
			const go = (next: string, c: number) => {
				if (c < (best.get(next) ?? Infinity)) {
					best.set(next, c);
					queue.push(next);
				}
			};
			if (pos < bars && count < maxCount) go(`${count + 1},${pos + 1},0`, cost);
			if (jumped === 0 && pos < bars) {
				for (const [edge, w] of edges) {
					const [from, to] = edge.split('>').map(Number);
					if (from === pos) go(`${count},${to},1`, cost + w);
				}
			}
		}
		let pick: { count: number; cost: number } | null = null;
		for (let count = 0; count <= maxCount; count++) {
			const cost = best.get(`${count},${bars},0`);
			if (cost === undefined) continue;
			const d = Math.abs(count - wanted);
			const bd = pick ? Math.abs(pick.count - wanted) : Infinity;
			if (!pick || d < bd - 1e-9 || (Math.abs(d - bd) <= 1e-9 && count > pick.count)) pick = { count, cost };
			else if (Math.abs(d - bd) <= 1e-9 && count === pick.count && cost < pick.cost) pick = { count, cost };
		}
		return { pick, edges };
	}

	/** What a walk costs in jumps, or `null` when it is not a legal walk: it has to start at bar 0,
	 *  jump only along a listed edge, and finish by playing the last bar. */
	function costOf(played: readonly number[], bars: number, edges: Map<string, number>): number | null {
		let pos = 0;
		let cost = 0;
		for (const b of played) {
			if (b !== pos) {
				const w = edges.get(`${pos}>${b}`);
				if (w === undefined) return null;
				cost += w;
			}
			if (b < 0 || b >= bars) return null;
			pos = b + 1;
		}
		return pos === bars ? cost : null;
	}

	test('finds the nearest length at the least cost for 400 random songs', () => {
		const next = rng(20261009);
		for (let trial = 0; trial < 400; trial++) {
			const bars = 4 + Math.floor(next() * 11);
			const classes = 2 + Math.floor(next() * 2);
			const chroma = Array.from({ length: bars }, () => chord(Math.floor(next() * classes)));
			const phrases = phraseMatches(chroma, SPLICE_SIMILARITY);
			const wanted = Math.round(next() * bars * 3 * 4) / 4 - 1;
			const { pick, edges } = reference(bars, phrases, wanted);
			const walk = planWalk(bars, phrases, wanted);
			const label = `trial ${trial}: ${bars} bars, wanted ${wanted}, phrases ${JSON.stringify(phrases)}`;
			expect(pick, label).not.toBeNull();
			expect(walk, label).not.toBeNull();
			expect(walk!.length, label).toBe(pick!.count);
			expect(costOf(walk!, bars, edges), label).toBe(pick!.cost);
		}
	});

	test('a song without repeats has exactly one walk: all of it', () => {
		for (const bars of [1, 2, 7]) {
			expect(planWalk(bars, [], 100)).toEqual(Array.from({ length: bars }, (_, k) => k));
		}
	});
});

// ---- the project's half ----------------------------------------------------------------

function audioClip(assetId: string, sourceIn: number, sourceOut: number, start: number): Clip {
	return {
		id: `${assetId}-${start}`,
		asset_id: assetId,
		source_in: sourceIn,
		source_out: sourceOut,
		timeline_start: start,
		volume: 1,
		fade_in: 0,
		fade_out: 0
	};
}

function timelineOf(video: Clip[], audio: Clip[]): Timeline {
	return {
		tracks: [
			{ id: 'v1', kind: 'video', name: 'V1', clips: video },
			{ id: 'a1', kind: 'audio', name: 'A1', clips: audio }
		]
	} as unknown as Timeline;
}

const musicAsset: Asset = {
	id: 'music',
	path: '/fit-music.wav',
	name: 'fit-music.wav',
	duration: 34.5,
	streams: [{ index: 0, kind: 'audio', codec: 'pcm_s16le', sample_rate: 48000, channels: 2 }],
	imported_at: ''
};
const videoAsset: Asset = {
	id: 'video',
	path: '/fit-video.mp4',
	name: 'fit-video.mp4',
	duration: 60,
	streams: [{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps: 30 }],
	imported_at: ''
};

function project() {
	const analysis = { music: song() };
	const timeline = timelineOf([audioClip('video', 0, 50, 0)], [audioClip('music', 0, 34.5, 0)]);
	const src = {
		assets: [musicAsset, videoAsset],
		analysisOf: (id: string) => (id === 'music' ? analysis : null)
	};
	return { timeline, src, clip: timeline.tracks[1].clips[0].id };
}

describe('fitting a clip on the timeline', () => {
	test('fills the picture with whole phrases (fit_music_fills_the_picture_with_whole_phrases)', () => {
		const { timeline, src, clip } = project();
		const plan = musicFitInputs(timeline, src, clip).fit;
		expect(plan.target).toBe(50);
		expect(plan.bars).toBe(24);
		expect(plan.remainder).toBeLessThan(0); // 24 bars run half a second over
		expect(timeline.tracks[1].clips.some((c) => c.id === clip)).toBe(true); // planning changes nothing

		let n = 0;
		const report = fitMusicOnto(timeline, src, clip, null, true, () => `fitted-${n++}`);
		expect(report.faded).toBe(true);
		const audio = timeline.tracks[1];
		expect(audio.clips.length).toBe(report.clips.length);
		expect(audio.clips.length).toBe(2);
		const end = Math.max(...audio.clips.map((c) => c.timeline_start + clipDuration(c)));
		expect(Math.abs(end - 50)).toBeLessThan(1e-6);
		expect(audio.clips[1].transition_in).toBeTruthy();
		expect(audio.clips[1].fade_out).toBe(FIT_FADE_S);
		expect(audio.clips.some((c) => c.id === clip)).toBe(false); // the original clip is replaced
		expect(Math.abs(report.duration - 50)).toBeLessThan(1e-6);

		expect(() => fitMusicOnto(timeline, src, report.clips[0], -1, false)).toThrow('positive');
	});

	test('without the fade the whole arrangement stays, ending included', () => {
		const { timeline, src, clip } = project();
		const report = fitMusicOnto(timeline, src, clip, null, false);
		expect(report.faded).toBe(false);
		expect(report.duration).toBeGreaterThan(50);
	});

	test('needs a bar grid (fit_music_needs_a_bar_grid)', () => {
		const { timeline, clip } = project();
		const src = { assets: [musicAsset, videoAsset], analysisOf: () => null };
		expect(() => musicFitInputs(timeline, src, clip, 20)).toThrow('no bar grid');
	});

	test('refuses what the backend refuses, and leaves the timeline as it was', () => {
		const { timeline, src, clip } = project();
		const before = structuredClone(timeline);
		const refuse = (f: () => unknown, text: string) => expect(f).toThrow(text);
		refuse(() => musicFitInputs(timeline, src, 'nope'), 'clip not found');
		refuse(() => musicFitInputs(timeline, src, timeline.tracks[0].clips[0].id), 'audio track');
		refuse(() => musicFitInputs(timeline, src, clip, 0), 'positive');
		refuse(() => musicFitInputs(timeline, src, clip, Number.NaN), 'positive');
		refuse(() => musicFitInputs(timeline, src, clip, 1e9), 'too long');
		timeline.tracks[1].clips[0].speed = 2;
		refuse(() => musicFitInputs(timeline, src, clip), 'normal speed');
		delete timeline.tracks[1].clips[0].speed;
		timeline.tracks[1].clips[0].link_id = 'pair';
		refuse(() => fitMusicOnto(timeline, src, clip, 30, false), 'unlink the music');
		delete timeline.tracks[1].clips[0].link_id;
		timeline.tracks[1].locked = true;
		refuse(() => fitMusicOnto(timeline, src, clip, 30, false), 'A1 is locked');
		delete timeline.tracks[1].locked;
		timeline.tracks[0].clips = [];
		refuse(() => musicFitInputs(timeline, src, clip), 'no picture');
		timeline.tracks[0].clips = before.tracks[0].clips;
		expect(timeline).toEqual(before);
	});

	test('refuses a fit that would run into the next clip, changing nothing', () => {
		const { timeline, src, clip } = project();
		timeline.tracks[1].clips.push(audioClip('music', 0, 5, 40));
		const before = structuredClone(timeline);
		expect(() => fitMusicOnto(timeline, src, clip, null, false)).toThrow('would run into the next clip on A1');
		expect(timeline).toEqual(before);
	});

	test('a custom target replaces the picture length', () => {
		const { timeline, src, clip } = project();
		expect(musicFitInputs(timeline, src, clip, 30).fit.target).toBe(30);
	});
});
