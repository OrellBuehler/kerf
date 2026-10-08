/**
 * "Fit music to length": the *faithful* TS mirror of `kerf_core::model::music_fit`
 * (`plan_music_fit`, `music_fit_clips`) plus the bar-grid helpers of `BeatGrid`, the
 * phrase matcher of `engine/music.rs` and the checks of `Project::music_fit_inputs` /
 * `Project::fit_music` (messages included). The desktop app plans in Rust; this is what
 * the browser harness runs, so a rule changed in kerf-core changes here too — and
 * `music-fit.test.ts` replays the Rust tests case for case.
 *
 * One deliberate difference: the Rust walk uses `usize`/`f64`/`f32` where this uses numbers.
 * The chroma similarity is rounded through `Math.fround` at each step so a pair within a
 * float-epsilon of the threshold lands on the same side.
 */
import { BEAT_MIN_CONFIDENCE } from './beats';
import { formatTime } from './diff';
import { clipDuration } from './types';
import type {
	Asset,
	BeatGrid,
	Clip,
	MusicAnalysis,
	MusicFit,
	MusicFitReport,
	MusicSegment,
	PhraseMatch,
	Tempo,
	Timeline
} from './types';

/** A splice is crossfaded over this window, centred on the splice point. */
export const SPLICE_CROSSFADE_S = 0.01;

/** How long the fade-out is when a fit that runs over is cut to its target. */
export const FIT_FADE_S = 2;

/** The longest walk the planner will search, in bars: a few hours at any tempo. */
export const MAX_FIT_BARS = 4096;

/** Two bars match above this cosine similarity (`engine::music::SPLICE_SIMILARITY`). */
export const SPLICE_SIMILARITY = 0.95;

// ---- the bar grid (`BeatGrid`) -------------------------------------------------------

export const gridBpm = (g: BeatGrid): number => 60 / g.period_s;

export const gridBarSeconds = (g: BeatGrid): number => g.period_s * g.beats_per_bar;

/** The first downbeat (at most a few ms before 0). */
export const firstDownbeat = (g: BeatGrid): number => g.phase_s + g.downbeat_offset * g.period_s;

/** Where bar `k` starts (counted from the first downbeat). */
export const barStart = (g: BeatGrid, k: number): number => firstDownbeat(g) + k * gridBarSeconds(g);

/** How many whole bars fit between the first downbeat and `duration`. */
export function wholeBars(g: BeatGrid, duration: number): number {
	const span = duration - firstDownbeat(g);
	if (span <= 0 || gridBarSeconds(g) <= 0) return 0;
	// A bar that ends within a microsecond of the end still counts as whole.
	return Math.floor((span + 1e-6) / gridBarSeconds(g));
}

/** Every beat in `[0, duration)`. */
export function gridBeats(g: BeatGrid, duration: number): number[] {
	if (g.period_s <= 0) return [];
	const out: number[] = [];
	for (let k = 0; ; k++) {
		const t = g.phase_s + k * g.period_s;
		if (!(t < duration)) break;
		out.push(t);
	}
	return out;
}

/** Every downbeat in `[0, duration)`. */
export function gridDownbeats(g: BeatGrid, duration: number): number[] {
	if (gridBarSeconds(g) <= 0) return [];
	const out: number[] = [];
	for (let k = 0; ; k++) {
		const t = barStart(g, k);
		if (!(t < duration)) break;
		out.push(t);
	}
	return out;
}

/** `Tempo::from_grid`: the tempo read off a fitted bar grid — its beats and downbeats across
 *  `duration` (the few ms a first beat may sit before 0 dropped), and at least
 *  `BEAT_MIN_CONFIDENCE`, since the fit already refused anything without a steady pulse. */
export function tempoFromGrid(g: BeatGrid, duration: number, confidence: number): Tempo {
	return {
		bpm: gridBpm(g),
		beats: gridBeats(g, duration).filter((t) => t >= 0),
		confidence: Math.max(confidence, BEAT_MIN_CONFIDENCE),
		downbeats: gridDownbeats(g, duration).filter((t) => t >= 0)
	};
}

// ---- repeating phrases (`engine::music`) -----------------------------------------------

/** Cosine similarity of two L2-normalized chroma vectors, in f32 arithmetic like the engine's. */
export function chromaSimilarity(a: readonly number[], b: readonly number[]): number {
	let sum = 0;
	for (let i = 0; i < a.length && i < b.length; i++) sum = Math.fround(sum + Math.fround(a[i] * b[i]));
	return sum;
}

/** Every pair of phrases — 8 bars, then 4 — that start on a 4-bar boundary and whose bars all match
 *  above `threshold`. A 4-bar match inside an 8-bar one is still listed: the planner prefers the
 *  longer one but may need the shorter. */
export function phraseMatches(chroma: readonly (readonly number[])[], threshold = SPLICE_SIMILARITY): PhraseMatch[] {
	const limit = Math.fround(threshold);
	const out: PhraseMatch[] = [];
	for (const bars of [8, 4]) {
		if (chroma.length < bars) continue;
		const starts: number[] = [];
		for (let s = 0; s <= chroma.length - bars; s += 4) starts.push(s);
		for (let i = 0; i < starts.length; i++) {
			for (const b of starts.slice(i + 1)) {
				const a = starts[i];
				let all = true;
				for (let k = 0; k < bars && all; k++) all = chromaSimilarity(chroma[a + k], chroma[b + k]) > limit;
				if (all) out.push({ a, b, bars });
			}
		}
	}
	return out;
}

// ---- the planner (`model/music_fit.rs`) -------------------------------------------------

/** Rust's `f64::round`: halves away from zero (`Math.round` takes them towards +infinity). */
const roundAway = (x: number): number => (x < 0 ? -Math.round(-x) : Math.round(x));

/** Round `t` to the sample grid of `rate` (unchanged for rate 0). */
export function toSamples(t: number, rate: number): number {
	return rate === 0 ? t : roundAway(t * rate) / rate;
}

/** The source spans a walk plays, merged wherever it runs on in the source: the intro, each run of
 *  consecutive bars, and the ending after the last bar. */
function sourceRuns(m: MusicAnalysis, played: readonly number[]): [number, number][] {
	const startOf = (k: number) => Math.max(barStart(m.grid, k), 0);
	const runs: [number, number][] = [];
	let open = 0;
	let next = 0;
	for (const b of played) {
		if (b !== next) {
			// An intro too short to crossfade out of is dropped rather than spliced.
			if (startOf(next) - open >= SPLICE_CROSSFADE_S) runs.push([open, startOf(next)]);
			open = startOf(b);
		}
		next = b + 1;
	}
	runs.push([open, m.duration]);
	return runs;
}

/** Splice cost: an 8-bar match is the safer jump, so it is the cheaper one. */
export const jumpCost = (bars: number): number => (bars >= 8 ? 2 : 3);

const UNREACHED = 0xffffffff;

/**
 * The bars to play, in order, from bar 0 to the ending: the walk whose length is nearest `wanted`
 * bars (a tie goes to the longer one: a fade-out can shorten it, where the shorter one leaves the
 * picture's last seconds without music), and among those the cheapest in jumps. `null` when there
 * is nothing to walk. (Exported for the tests, which check it against a brute force.)
 *
 * A dynamic programme over (bar position, bars played, just jumped): playing a bar moves one bar
 * on, a jump moves to the matching phrase without playing anything, and two jumps in a row are not
 * allowed (they would be one jump).
 */
export function planWalk(bars: number, phrases: readonly PhraseMatch[], wanted: number): number[] | null {
	// A jump moves at most `bars` bars, so the nearest walk above `wanted` is within one more pass
	// over the song.
	const ceil = Number.isNaN(wanted) ? 0 : Math.min(Math.ceil(Math.max(wanted, 0)), 1e9);
	const maxCount = Math.min(Math.max(ceil, bars) + bars, Math.max(MAX_FIT_BARS, 2 * bars));
	const jumps: [number, number][][] = Array.from({ length: bars + 1 }, () => []);
	for (const p of phrases) {
		if (p.a + p.bars > bars || p.b + p.bars > bars || p.a === p.b) continue;
		const cost = jumpCost(p.bars);
		for (const [from, to] of [
			[p.a, p.b],
			[p.b, p.a]
		]) {
			const j = jumps[from].find(([q]) => q === to);
			if (j) j[1] = Math.min(j[1], cost);
			else jumps[from].push([to, cost]);
		}
	}
	const width = bars + 1;
	const idx = (count: number, pos: number, jumped: number) => (count * width + pos) * 2 + jumped;
	const states = (maxCount + 1) * width * 2;
	const cost = new Uint32Array(states).fill(UNREACHED);
	const parent = new Int32Array(states).fill(-1);
	cost[idx(0, 0, 0)] = 0;
	for (let count = 0; count <= maxCount; count++) {
		for (let pos = 0; pos < bars; pos++) {
			const from = idx(count, pos, 0);
			if (cost[from] === UNREACHED) continue;
			for (const [to, w] of jumps[pos]) {
				const s = idx(count, to, 1);
				if (cost[from] + w < cost[s]) {
					cost[s] = cost[from] + w;
					parent[s] = from;
				}
			}
		}
		if (count === maxCount) break;
		for (let pos = 0; pos < bars; pos++) {
			for (let jumped = 0; jumped < 2; jumped++) {
				const from = idx(count, pos, jumped);
				if (cost[from] === UNREACHED) continue;
				const s = idx(count + 1, pos + 1, 0);
				if (cost[from] < cost[s]) {
					cost[s] = cost[from];
					parent[s] = from;
				}
			}
		}
	}
	let best: [number, number] | null = null;
	for (let count = 0; count <= maxCount; count++) {
		const c = cost[idx(count, bars, 0)];
		if (c === UNREACHED) continue;
		let better = best === null;
		if (best !== null) {
			const [bc, bcost] = best;
			const d = Math.abs(count - wanted);
			const bd = Math.abs(bc - wanted);
			better = d < bd - 1e-9 || (Math.abs(d - bd) <= 1e-9 && (count > bc || (count === bc && c < bcost)));
		}
		if (better) best = [count, c];
	}
	if (best === null) return null;
	const played: number[] = [];
	let s = idx(best[0], bars, 0);
	const start = idx(0, 0, 0);
	while (s !== start) {
		const p = parent[s];
		const pCount = Math.floor((p >> 1) / width);
		const pPos = (p >> 1) % width;
		if (pCount + 1 === Math.floor((s >> 1) / width)) played.push(pPos);
		s = p;
	}
	return played.reverse();
}

/**
 * Plan a fit of the analysed music to `target` seconds. Segment boundaries land on the source's
 * `sampleRate` grid, and each segment's output position is the exact sum of the lengths before it,
 * rounded to that grid once — so no splice drifts.
 */
export function planMusicFit(m: MusicAnalysis, target: number, sampleRate: number): MusicFit {
	const bars = Math.min(m.bar_chroma.length, wholeBars(m.grid, m.duration));
	const barS = gridBarSeconds(m.grid);
	const intro = Math.max(firstDownbeat(m.grid), 0);
	const ending = bars > 0 ? Math.max(m.duration - barStart(m.grid, bars), 0) : 0;
	const walk = bars === 0 || barS <= 0 ? null : planWalk(bars, m.phrases, (target - intro - ending) / barS);
	const runs: [number, number][] = walk ? sourceRuns(m, walk) : [[0, m.duration]];
	const segments: MusicSegment[] = [];
	let at = 0;
	for (const [from, to] of runs) {
		const start = toSamples(from, sampleRate);
		const end = toSamples(to, sampleRate);
		if (end - start <= 0) continue;
		segments.push({ source_start: start, source_end: end, output_start: toSamples(at, sampleRate) });
		at += end - start;
	}
	const last = segments[segments.length - 1];
	const duration = last ? last.output_start + (last.source_end - last.source_start) : 0;
	return {
		splices: Math.max(segments.length - 1, 0),
		bars: walk ? walk.length : bars,
		target,
		duration,
		remainder: target - duration,
		segments
	};
}

/**
 * The clips a fit puts on the timeline in place of `template` (the music clip it was planned for),
 * in order: each segment a copy of the clip's sound (gain and audio effects) on its source span,
 * placed at `template.timeline_start` plus its output position. Every splice is crossfaded over one
 * window centred on it — the outgoing clip ends half a window early and plays on under the incoming
 * one, which starts half a window early — so both fades cover the same samples and the copies never
 * play at full gain together. With `fadeOut` a fit that runs over is cut at its target and faded
 * out over the last `FIT_FADE_S`.
 */
export function musicFitClips(
	template: Clip,
	fit: MusicFit,
	sampleRate: number,
	fadeOut: boolean,
	newId: () => string = defaultId
): Clip[] {
	const half = toSamples(SPLICE_CROSSFADE_S / 2, sampleRate);
	const segs = fit.segments;
	let clips = segs.map((s) => {
		const c: Clip = structuredClone(template);
		c.id = newId();
		c.source_in = s.source_start;
		c.source_out = s.source_end;
		c.timeline_start = template.timeline_start + s.output_start;
		c.fade_in = 0;
		c.fade_out = 0;
		delete c.transition_in;
		if (c.keyframes) c.keyframes = [];
		delete c.link_id;
		return c;
	});
	for (let i = 1; i < clips.length; i++) {
		const len = (s: MusicSegment) => s.source_end - s.source_start;
		const h0 = Math.min(half, segs[i].source_start, len(segs[i]) / 2, len(segs[i - 1]) / 2);
		const h = toSamples(Math.max(h0, 0), sampleRate);
		if (h <= 0) continue;
		clips[i - 1].source_out -= h;
		clips[i].source_in -= h;
		clips[i].timeline_start -= h;
		clips[i].transition_in = { kind: 'crossfade', duration: 2 * h };
	}
	if (clips.length > 0) clips[0].fade_in = template.fade_in;
	const end = template.timeline_start + fit.target;
	if (fadeOut && fit.remainder < 0) {
		clips = clips.filter((c) => c.timeline_start < end - 1e-9);
		const last = clips[clips.length - 1];
		if (last) {
			last.source_out = last.source_in + toSamples(end - last.timeline_start, sampleRate);
			last.fade_out = Math.min(FIT_FADE_S, clipDuration(last));
		}
	} else if (clips.length > 0) {
		clips[clips.length - 1].fade_out = template.fade_out;
	}
	return clips;
}

const defaultId = (): string => (crypto.randomUUID ? crypto.randomUUID() : `id-${Math.random().toString(36).slice(2)}`);

// ---- the project's half (`Project::music_fit_inputs` / `fit_music`) ------------------------

/** What a fit needs of the project: the asset, its analysis, the audio sample rate it plays at. */
export interface FitSources {
	assets: readonly Asset[];
	analysisOf: (assetId: string) => { music?: MusicAnalysis | null } | null | undefined;
}

export interface FitInputs {
	clip: Clip;
	rate: number;
	fit: MusicFit;
}

/** What a music clip is fitted to by default: the picture's length from the clip's start. */
function pictureTarget(timeline: Timeline, clip: Clip): number {
	const end = timeline.tracks
		.filter((t) => t.kind === 'video')
		.reduce((m, t) => Math.max(m, t.clips.reduce((e, c) => Math.max(e, c.timeline_start + clipDuration(c)), 0)), 0);
	const target = end - clip.timeline_start;
	if (target <= 0) throw new Error('invalid argument: there is no picture after this clip\'s start to fit it to');
	return target;
}

/**
 * `Project::plan_music_fit` / `music_fit_inputs`: validate the clip and plan its fit. Throws the
 * backend's own refusals (`invalid argument: …`) — a clip off an audio track, one not at normal
 * speed, music without a bar grid, a target that is not a positive number or is too long.
 */
export function musicFitInputs(timeline: Timeline, src: FitSources, clipId: string, target?: number | null): FitInputs {
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
	const clip = track?.clips.find((c) => c.id === clipId);
	if (!track || !clip) throw new Error(`clip not found: ${clipId}`);
	if (track.kind !== 'audio') throw new Error('invalid argument: fit music to length works on a clip on an audio track');
	if (Math.abs((clip.speed ?? 1) - 1) > 1e-9) throw new Error('invalid argument: fit music to length needs the clip at normal speed');
	const asset = src.assets.find((a) => a.id === clip.asset_id);
	if (!asset) throw new Error(`asset not found: ${clip.asset_id}`);
	const music = src.analysisOf(asset.id)?.music;
	if (!music) {
		throw new Error('invalid argument: this music has no bar grid — analyze the asset first (it needs a steady beat)');
	}
	let want: number;
	if (target === undefined || target === null) want = pictureTarget(timeline, clip);
	else if (Number.isFinite(target) && target > 0) want = target;
	else throw new Error('invalid argument: target must be a positive number of seconds');
	if (want / gridBarSeconds(music.grid) > MAX_FIT_BARS) throw new Error('invalid argument: that target is too long to fit music to');
	const rate = asset.streams.find((s) => s.kind === 'audio')?.sample_rate ?? 48000;
	return { clip, rate, fit: planMusicFit(music, want, rate) };
}

/**
 * `Project::fit_music`: replace a music clip, **in place** on `timeline`, with the edit list
 * `musicFitInputs` plans. Throws before touching anything when the clip is linked, its track is
 * locked or the result would run into the next clip on the track.
 */
export function fitMusicOnto(
	timeline: Timeline,
	src: FitSources,
	clipId: string,
	target: number | null | undefined,
	fadeOut: boolean,
	newId?: () => string
): MusicFitReport {
	const { clip, rate, fit } = musicFitInputs(timeline, src, clipId, target);
	if (clip.link_id) throw new Error('invalid argument: unlink the music from its picture before fitting it');
	const clips = musicFitClips(clip, fit, rate, fadeOut, newId);
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId))!;
	if (track.locked) throw new Error(`invalid argument: ${track.name} is locked`);
	const at = track.clips.findIndex((c) => c.id === clipId);
	const rest = track.clips.filter((c) => c.id !== clipId);
	const start = clips.length > 0 ? clips[0].timeline_start : clip.timeline_start;
	const last = clips[clips.length - 1];
	const end = last ? last.timeline_start + clipDuration(last) : start;
	if (rest.some((c) => c.timeline_start < end - 1e-9 && c.timeline_start + clipDuration(c) > start + 1e-9)) {
		throw new Error(
			`invalid argument: the fitted music (${formatTime(end - start)}) would run into the next clip on ${track.name} — make room first`
		);
	}
	rest.splice(at, 0, ...clips);
	track.clips = rest;
	return { clips: clips.map((c) => c.id), faded: fadeOut && fit.remainder < 0, duration: end - start, fit };
}
