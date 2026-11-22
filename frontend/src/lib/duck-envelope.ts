/* What the lane overlay draws for a ducked track: the gain the export puts on it over the cut, predicted from the
 * waveform's peaks by the same machine the preview plays (`duck.ts`), so a user sees where the music dips, and why — it
 * is where the rest of the mix speaks. Pure: the component fetches the peaks and draws; this decides.
 *
 * The preview has the key's samples and runs the machine on them. The overlay has only a window's peaks — the largest and
 * smallest sample of every 10 ms — so it reads each hop's *peak* as the key's level, which is what the machine's detector
 * follows anyway: both detect on a squared sample that rises at the attack's rate and falls at the release's, which sits near
 * the peak and well above the mean (`PEAK_POWER_SCALE`). It is a prediction: within about a dB where the key is steady, with the edges a hop or two out where it is
 * not, and it says so where it is shown. The sound is the preview's. */

import { COMPRESSOR, COMPRESSOR_ENDS_WITH_KEY, duckEnvelope, GATE, MONO_FOLD, reductionDb, type DuckMode } from './duck';
import type { DuckPlan } from './duck-plan';
import { clipGainAt } from './audio-mix';
import { panGains } from './mixer';
import { timelineToSource } from './speed-ramp';
import type { Clip, Timeline } from './types';
import { clipDuration } from './types';

/** Seconds a step of the overlay stands for: the waveform pyramid's 100 peaks per second. */
export const HOP = 0.01;

/** The detector's equivalent power over the squared peak of a hop (-2.2 dB): a steady voice, noise or sine sits 0.6 to 1.6 dB under its peaks,
 *  and speech, which swells and falls by the syllable, a little more. */
export const PEAK_POWER_SCALE = 0.6;

/** The gain reduction at the bottom of the lane's dip, dB: a deeper one is drawn as this deep. */
export const DIP_FLOOR_DB = 24;

/** One window of an asset's peaks, as `get_waveform_range` answers: `rate` buckets a second from source time `t0`, each the
 *  largest and the smallest sample of its stretch, per channel. */
export interface Peaks {
	t0: number;
	rate: number;
	max: ArrayLike<number>[];
	min: ArrayLike<number>[];
}

/** The largest magnitude of bucket `i` over the channels' peaks — their mean, which is the mono the preview plays for two
 *  channels that agree. A bucket outside the window is silence. */
export function bucketPeak(p: Peaks, i: number): number {
	if (i < 0) return 0;
	let sum = 0;
	let n = 0;
	for (let c = 0; c < p.max.length; c++) {
		const hi = p.max[c]?.[i];
		const lo = p.min[c]?.[i];
		if (hi === undefined || lo === undefined) continue;
		sum += Math.max(Math.abs(hi), Math.abs(lo));
		n++;
	}
	return n === 0 ? 0 : sum / n;
}

/** The key's detection power per hop, `0..end`: every clip of the tracks that key the duck, read through its peaks, its
 *  own gain (volume, fades), its track's fader and pan, as the key the export builds is. Clips add in power — they are
 *  not in phase. A clip without peaks (not fetched yet, or none to fetch) says nothing. */
export function keyPower(
	timeline: Pick<Timeline, 'tracks'>,
	plan: Pick<DuckPlan, 'mode' | 'keyed'>,
	peaksOf: (clip: Clip) => Peaks | null,
	end: number,
	hop = HOP
): Float32Array {
	const power = new Float32Array(Math.max(0, Math.ceil(end / hop)));
	for (const track of timeline.tracks) {
		if (!plan.keyed.includes(track.id)) continue;
		const [gl, gr] = panGains(track.pan ?? 0);
		const fader = Math.max(0, track.volume ?? 1);
		for (const clip of track.clips) {
			if (clip.enabled === false) continue;
			const peaks = peaksOf(clip);
			if (!peaks) continue;
			const start = clip.timeline_start;
			const stop = start + clipDuration(clip);
			const from = Math.max(0, Math.ceil(start / hop));
			const to = Math.min(power.length, Math.ceil(stop / hop));
			for (let h = from; h < to; h++) {
				// The middle of the hop, which is where the bucket the peaks give for it is read.
				const tl = (h + 0.5) * hop;
				const a = bucketPeak(peaks, Math.floor((timelineToSource(clip, tl) - peaks.t0) * peaks.rate));
				if (a === 0) continue;
				const level = a * clipGainAt(clip, tl, start, stop) * fader;
				const l = level * gl;
				const r = level * gr;
				const detect = plan.mode.kind === 'compressor' ? (l + r) * 0.5 : (l + r) * MONO_FOLD;
				power[h] += detect * detect * PEAK_POWER_SCALE;
			}
		}
	}
	return power;
}

/** The gain on the ducked bus per hop, as the export would put it there (1: not dipped). */
export function duckCurve(
	timeline: Pick<Timeline, 'tracks'>,
	plan: Pick<DuckPlan, 'mode' | 'keyed'>,
	peaksOf: (clip: Clip) => Peaks | null,
	end: number,
	hop = HOP
): Float32Array {
	return duckEnvelope(keyPower(timeline, plan, peaksOf, end, hop), hop, plan.mode);
}

/** What a lane draws: the gain curve, where the key speaks, and how deep the deepest dip goes. One pass over the key. */
export interface DuckView {
	curve: Float32Array;
	spans: [number, number][];
	deepest: number;
}

export function duckView(
	timeline: Pick<Timeline, 'tracks'>,
	plan: Pick<DuckPlan, 'mode' | 'keyed'>,
	peaksOf: (clip: Clip) => Peaks | null,
	end: number,
	hop = HOP
): DuckView {
	const power = keyPower(timeline, plan, peaksOf, end, hop);
	const curve = duckEnvelope(power, hop, plan.mode);
	return { curve, spans: keySpans(power, plan.mode, hop), deepest: deepest(curve, 0, Infinity, hop) };
}

/** The deepest dip of a curve, in dB (positive), over `from..to` seconds. */
export function deepest(curve: Float32Array, from = 0, to = Infinity, hop = HOP): number {
	let worst = 0;
	for (let h = Math.max(0, Math.floor(from / hop)); h < Math.min(curve.length, Math.ceil(to / hop)); h++) worst = Math.max(worst, reductionDb(curve[h]));
	return worst;
}

/** The dip as points in `(seconds, fraction of the lane's depth)`, from the top edge down: the curve's reduction over `DIP_FLOOR_DB`, held
 *  to `0..1`. Runs that do not move are one point, so a long cut is a few hundred points. */
function dipPoints(curve: Float32Array, from: number, to: number, hop: number): [number, number][] {
	const lo = Math.max(0, Math.floor(from / hop));
	const hi = Math.min(curve.length, Math.ceil(to / hop));
	const pts: [number, number][] = [];
	let lastY = Number.NaN;
	let pending: [number, number] | null = null;
	for (let h = lo; h < hi; h++) {
		const y = Math.round(Math.min(1, reductionDb(curve[h]) / DIP_FLOOR_DB) * 200) / 200;
		const at: [number, number] = [Math.round(h * hop * 1000) / 1000, y];
		if (y === lastY) {
			pending = at;
			continue;
		}
		if (pending) pts.push(pending);
		pending = null;
		pts.push(at);
		lastY = y;
	}
	if (pending) pts.push(pending);
	return pts;
}

const fmt = (pts: [number, number][]) => pts.map(([x, y]) => `${x.toFixed(3)},${y}`).join(' L');

/** The dip as the path of a filled shape, closed along the lane's top edge (`''` for nothing to draw). */
export function dipPath(curve: Float32Array, from: number, to: number, hop = HOP): string {
	const pts = dipPoints(curve, from, to, hop);
	if (pts.length === 0) return '';
	return `M${pts[0][0].toFixed(3)},0 L${fmt(pts)} L${pts[pts.length - 1][0].toFixed(3)},0 Z`;
}

/** The dip's edge alone — the line to stroke, without the top edge a closed shape would draw. */
export function dipLine(curve: Float32Array, from: number, to: number, hop = HOP): string {
	const pts = dipPoints(curve, from, to, hop);
	return pts.length === 0 ? '' : `M${fmt(pts)}`;
}

/** Where the key speaks — the stretches over the detector's threshold, runs closer than `bridge` seconds one — which is *why* the bed dips. The
 *  lane marks them along its bottom edge. */
export function keySpans(power: ArrayLike<number>, mode: DuckMode, hop = HOP, bridge = 0.15): [number, number][] {
	// A hop's power already carries `PEAK_POWER_SCALE`; the gate opens at 0.03, the compressor starts to bite at the foot of its knee.
	const level = mode.kind === 'gate' ? GATE.threshold : COMPRESSOR.threshold / Math.sqrt(COMPRESSOR.knee);
	const over = level * level * PEAK_POWER_SCALE;
	const spans: [number, number][] = [];
	for (let h = 0; h < power.length; h++) {
		if (!(power[h] >= over)) continue;
		const start = h * hop;
		const last = spans[spans.length - 1];
		if (last && start - last[1] <= bridge) last[1] = (h + 1) * hop;
		else spans.push([start, (h + 1) * hop]);
	}
	return spans;
}

/** Where the export stops a ducked track — the compressor path ends with its key — if that is before the track's own end, or `null`. */
export function cutAt(plan: Pick<DuckPlan, 'keyEnd'>, trackEnd: number): number | null {
	if (!COMPRESSOR_ENDS_WITH_KEY || plan.keyEnd === null) return null;
	return plan.keyEnd < trackEnd - 1e-6 ? plan.keyEnd : null;
}

/** What the peaks a clip needs are: its source window, which is what `get_waveform_range` is asked for. */
export function peakWindow(clip: Pick<Clip, 'source_in' | 'source_out'>): [number, number] {
	return [clip.source_in, clip.source_out];
}

/** How a mode is worded where the overlay says what it is showing. */
export function dipCaption(mode: DuckMode): string {
	return mode.kind === 'gate' ? `Speech gate ${mode.depthDb} dB` : 'Compressor';
}

// ---- fetching the peaks a window needs -----------------------------------------------------------------------

/** A key clip's window in the peaks cache: the asset and the part of it the clip plays. */
export const peakKey = (clip: Pick<Clip, 'asset_id' | 'source_in' | 'source_out'>): string => `${clip.asset_id}:${clip.source_in}:${clip.source_out}`;

/** The seconds of source one request asks for: 4000 buckets at 100 a second, under the 4096 the backend returns at most. */
export const PEAK_CHUNK_S = 40;

/** What `get_waveform_range` answers for one request, as far as the overlay reads it. */
export interface PeakChunk {
	buckets: number;
	min: number[][];
	max: number[][];
}

/** The requests that cover `[from, to)` of an asset at `PEAKS_PER_S` a second: `[start, end, buckets]` each, `PEAK_CHUNK_S` long. */
export function peakRequests(from: number, to: number): [number, number, number][] {
	const out: [number, number, number][] = [];
	for (let a = from; a < to - 1e-9; a += PEAK_CHUNK_S) {
		const b = Math.min(to, a + PEAK_CHUNK_S);
		out.push([a, b, Math.max(1, Math.round((b - a) * (1 / HOP)))]);
	}
	return out;
}

/** The chunks of one window joined into the one `Peaks` the key is read from: the buckets in order, the rate the requests asked for. */
export function joinChunks(from: number, chunks: PeakChunk[]): Peaks {
	const channels = Math.max(1, ...chunks.map((c) => c.max.length));
	const max: number[][] = Array.from({ length: channels }, () => []);
	const min: number[][] = Array.from({ length: channels }, () => []);
	for (const chunk of chunks) {
		for (let c = 0; c < channels; c++) {
			// A chunk of a mono file has one channel: it is every channel (a stereo window never mixes).
			const hi = chunk.max[c] ?? chunk.max[0] ?? [];
			const lo = chunk.min[c] ?? chunk.min[0] ?? [];
			for (let i = 0; i < chunk.buckets; i++) {
				max[c].push(hi[i] ?? 0);
				min[c].push(lo[i] ?? 0);
			}
		}
	}
	return { t0: from, rate: 1 / HOP, max, min };
}
