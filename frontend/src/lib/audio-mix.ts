/* The arithmetic behind the preview's audio graph, kept out of `audio.ts` so it can
 * be tested without an `AudioContext`:
 *
 *  - a clip's own gain over time (`clipGainAt`) — the same envelope the preview has
 *    always played, now *without* the track fader, which moved onto the track's bus;
 *  - the master limiter's approximation on a `DynamicsCompressorNode`
 *    (`limiterParams`), including the trim that cancels the node's automatic makeup
 *    gain. */

import { propertyCurve, volumeAnimated, volumeAt } from './channels';
import type { Clip } from './types';

/** What decides a clip's gain: its volume, fades and a transition into it. */
export type GainClip = Pick<Clip, 'volume' | 'fade_in' | 'fade_out' | 'transition_in' | 'channels'>;

/** A clip's fade-in length, seconds. A transition approximates as an extra fade-in
 *  (the export folds it in the same way). */
export const fadeInOf = (clip: GainClip): number => (clip.fade_in ?? 0) + (clip.transition_in?.duration ?? 0);

/** A clip's fade-out length, seconds. */
export const fadeOutOf = (clip: GainClip): number => clip.fade_out ?? 0;

/**
 * The gain of a clip at timeline time `t`, for a clip that spans `[start, end]`:
 * its volume, shaped by the fade in and out. `level` is whatever else rides it —
 * since the track fader moved onto the track's bus it is 1, and `level × clipGainAt(…, 1)`
 * is exactly what the same call with `level` used to give (the factors are one product).
 */
export function clipGainAt(clip: GainClip, t: number, start: number, end: number, level = 1): number {
	// A keyed volume is the gain over the clip's own time (`volumeAt`), else its static one.
	let v = volumeAt(clip, t - start) * level;
	const fi = fadeInOf(clip);
	const fo = fadeOutOf(clip);
	if (fi > 0 && t < start + fi) v *= Math.max(0, (t - start) / fi);
	if (fo > 0 && t > end - fo) v *= Math.max(0, (end - t) / fo);
	return v;
}

/**
 * The timeline times the preview ramps a **keyed** clip's gain through, after `from`: every point
 * of the volume curve (the polyline the export draws) and the fade edges, up to `end`. Between two
 * of them the preview ramps linearly, which is the curve exactly where no fade overlaps it and
 * its product with the fade's ramp (a little rounder) where one does. Empty for a clip whose
 * volume is not keyed.
 */
export function gainBreakpoints(clip: GainClip, start: number, end: number, from: number): number[] {
	if (!volumeAnimated(clip)) return [];
	const times = new Set<number>(propertyCurve(clip, 'volume').map(([t]) => start + t));
	const fi = fadeInOf(clip);
	const fo = fadeOutOf(clip);
	if (fi > 0) times.add(start + fi);
	if (fo > 0) times.add(end - fo);
	times.add(end);
	return [...times].filter((t) => t > from && t <= end).sort((a, b) => a - b);
}

// ---- the master limiter ------------------------------------------------------

/** The most a `DynamicsCompressorNode` takes off a signal: its ratio is capped at 20. */
export const LIMITER_RATIO = 20;
/** A hard knee: the gain reduction starts at the ceiling and not before it. */
export const LIMITER_KNEE = 0;
/** Seconds. The node already looks 6 ms ahead of what it is compressing. */
export const LIMITER_ATTACK_S = 0.001;
export const LIMITER_RELEASE_S = 0.1;
/** The exponent a browser's compressor raises its "full range" gain to for its
 *  automatic makeup (WebKit's `DynamicsCompressorKernel`, which Blink and Gecko
 *  share): `makeup = (1 / gain at 0 dBFS) ** 0.6`. */
const MAKEUP_EXPONENT = 0.6;

/** What a `DynamicsCompressorNode` is set to for the preview's limiter, and the
 *  linear gain that goes after it. */
export interface LimiterParams {
	/** dBFS where reduction starts: the ceiling. */
	threshold: number;
	knee: number;
	ratio: number;
	attack: number;
	release: number;
	/** Linear gain after the compressor, cancelling its automatic makeup so the
	 *  ceiling is a ceiling and not a level the signal is lifted back up to. */
	trim: number;
}

/** The gain a hard-knee compressor leaves at full scale (0 dBFS), linear. */
export function gainAtFullScale(thresholdDb: number, ratio: number = LIMITER_RATIO): number {
	return 10 ** ((thresholdDb + (0 - thresholdDb) / ratio) / 20);
}

/**
 * The preview's stand-in for the master limiter: a compressor with a hard knee at
 * the ceiling and the steepest ratio the node allows, followed by a trim.
 *
 * It is an **approximation** and the Mixer says so: at ratio 20 the output rises
 * 1 dB for every 20 dB the input is over the ceiling (a brick-wall limiter would
 * not rise at all), so a very hot mix reads a little over the ceiling; and the
 * export's `alimiter` is a different algorithm with a different release. What it
 * gets right is what a monitor has to — the mix is held at about the ceiling, so
 * a master pushed into the limiter sounds like it is.
 *
 * `trim` exists because the node adds makeup gain on its own: at a −12 dB threshold
 * a full-scale input would otherwise come out ~6 dB over the ceiling.
 */
export function limiterParams(ceilingDb: number): LimiterParams {
	const threshold = Math.min(0, Math.max(-100, Number.isFinite(ceilingDb) ? ceilingDb : 0));
	return {
		threshold,
		knee: LIMITER_KNEE,
		ratio: LIMITER_RATIO,
		attack: LIMITER_ATTACK_S,
		release: LIMITER_RELEASE_S,
		trim: gainAtFullScale(threshold) ** MAKEUP_EXPONENT
	};
}

/** The level a limiter set to `ceilingDb` leaves a full-scale signal at: a hair
 *  over the ceiling (`ratio` keeps it from being an exact stop). */
export const limitedFullScaleDb = (ceilingDb: number): number => {
	const t = Math.min(0, Math.max(-100, ceilingDb));
	return t + (0 - t) / LIMITER_RATIO;
};
