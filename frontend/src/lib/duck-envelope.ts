/* What the lane overlay draws for a ducked track: the gain the export puts on it over the cut, predicted from the
 * waveform's peaks by the same machine the preview plays (`duck.ts`), so a user sees where the music dips, and why — it
 * is where the rest of the mix speaks. Pure: the component fetches the peaks and draws; this decides.
 *
 * The preview has the key's samples and runs the machine on them. The overlay has only a window's peaks — the largest and
 * smallest sample of every 10 ms — so it reads each hop's *peak* as the key's level, which is what the machine's detector
 * follows anyway: both detect on a squared sample that rises at the attack's rate and falls at the release's, which sits near
 * the peak and well above the mean (`PEAK_POWER_SCALE`). It is a prediction: within about a dB where the key is steady, with the edges a hop or two out where it is
 * not, and it says so where it is shown. The sound is the preview's. */

import { COMPRESSOR_ENDS_WITH_KEY, duckEnvelope, MONO_FOLD, reductionDb, type DuckMode } from './duck';
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

/** The deepest dip of a curve, in dB (positive), over `from..to` seconds. */
export function deepest(curve: Float32Array, from = 0, to = Infinity, hop = HOP): number {
	let worst = 0;
	for (let h = Math.max(0, Math.floor(from / hop)); h < Math.min(curve.length, Math.ceil(to / hop)); h++) worst = Math.max(worst, reductionDb(curve[h]));
	return worst;
}

/** The dip as the path of a filled shape in `(seconds, fraction of the lane's depth)`, from the top edge down: the curve's
 *  reduction over `DIP_FLOOR_DB`, held to `0..1`. Runs that do not move are one point, so a long cut is a few hundred points. */
export function dipPath(curve: Float32Array, from: number, to: number, hop = HOP): string {
	const lo = Math.max(0, Math.floor(from / hop));
	const hi = Math.min(curve.length, Math.ceil(to / hop));
	if (hi <= lo) return '';
	const depth = (h: number) => Math.min(1, reductionDb(curve[h]) / DIP_FLOOR_DB);
	const pts: string[] = [];
	let lastY = Number.NaN;
	let pending: string | null = null;
	for (let h = lo; h < hi; h++) {
		const y = Math.round(depth(h) * 200) / 200;
		const x = (h * hop).toFixed(3);
		if (y === lastY) {
			pending = `${x},${y}`;
			continue;
		}
		if (pending) pts.push(pending);
		pending = null;
		pts.push(`${x},${y}`);
		lastY = y;
	}
	if (pending) pts.push(pending);
	const first = pts[0].split(',')[0];
	const last = pts[pts.length - 1].split(',')[0];
	return `M${first},0 L${pts.join(' L')} L${last},0 Z`;
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
