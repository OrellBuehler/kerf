/* The track mixer's arithmetic, shared by the timeline's mixer strip and the
 * Web Audio preview.
 *
 * `panGains` is the TS mirror of `Track::pan_gains` in
 * crates/kerf-core/src/model.rs — faithful, not approximate, because preview
 * playback is meant to be what the export sounds like. It is a **balance**: the
 * side you turn towards stays at unity and the other is attenuated away, so
 * leaning a track never makes it louder. */

/** Left / right gains for a pan position, -1 (hard left) to 1 (hard right). */
export function panGains(pan: number): [number, number] {
	const p = Math.min(1, Math.max(-1, pan || 0));
	return p < 0 ? [1, 1 + p] : [1 - p, 1];
}

/** A linear gain as dB; silence is `-Infinity`. */
export const gainToDb = (v: number): number => (v > 0 ? 20 * Math.log10(v) : -Infinity);

/** dB as a linear gain (`-Infinity` is silence). */
export const dbToGain = (db: number): number => (db === -Infinity ? 0 : 10 ** (db / 20));

/** What a clip's drawn (and heard) level is: its own gain through the track's
 *  fader, multiplied — the fader rides each clip *after* the clip's gain, as the
 *  export's graph does, so the two are one linear factor. Both default to unity. */
export const effectiveGain = (clipVolume: number | undefined, trackVolume: number | undefined): number =>
	Math.max(0, clipVolume ?? 1) * Math.max(0, trackVolume ?? 1);

/** A track fader as dB — the unit a level is actually judged in. */
export function gainLabel(v: number): string {
	if (v <= 0.0001) return '−∞ dB';
	const db = 20 * Math.log10(v);
	return `${db > 0 ? '+' : ''}${db.toFixed(1)} dB`;
}

/** A pan position as the L/R reading a mixer shows. */
export function panLabel(p: number): string {
	if (Math.abs(p) < 0.005) return 'centre';
	return `${p < 0 ? 'L' : 'R'}${Math.round(Math.abs(p) * 100)}`;
}

/** True when a track's mix is untouched, and so contributes nothing to render. */
export function isUnityMix(volume: number | undefined, pan: number | undefined): boolean {
	return (volume ?? 1) === 1 && (pan ?? 0) === 0;
}
