// The master strip's FX — the limiter, its ceiling and the duck mode. They sat above the fader and left it the
// shortest in the mixer (about 70 px of travel against 190 px for a track's), so they are a popover now
// (`MasterFx.svelte`), and one line on the strip says what is switched on while it is closed.

import { ceilingLabel } from './levels';
import type { MasterBus } from './types';

export interface FxSummary {
	/** What to say beside "FX": `Limiter −1.5 dBFS`, `Limiter · Gate`, `Off`. */
	text: string;
	/** Whether anything on the master is doing something, so a folded block does not hide a limiter that is on. */
	active: boolean;
}

/** One line for the folded block. The compressor is the duck mode every project starts in, so only the speech gate is news. */
export function fxSummary(master: Pick<MasterBus, 'limiter' | 'ceiling_db' | 'duck_depth_db'>): FxSummary {
	const gate = master.duck_depth_db != null;
	if (master.limiter && gate) return { text: 'Limiter · Gate', active: true };
	if (master.limiter) return { text: `Limiter ${ceilingLabel(master.ceiling_db)}`, active: true };
	if (gate) return { text: 'Speech gate', active: true };
	return { text: 'Off', active: false };
}

/** Which limiter the preview plays when the master's is on (`AudioEngine.limiterKind`): `pending` until
 *  playback has loaded the export's own in a worklet, `exact` once it has, `approximate` where it cannot. */
export type LimiterKind = 'pending' | 'exact' | 'approximate';

/** What the Limiter and Ceiling tooltips say about the preview's limiter. Even the exact one limits the
 *  preview's mix, which is not the export's: its sound is a mono 32 kHz decode. */
export function limiterNote(kind: LimiterKind): string {
	const measure = 'Measure reads the exported mix.';
	if (kind === 'exact')
		return `The preview runs the export’s limiter (FFmpeg’s alimiter, sample for sample) on the preview’s mix — a mono 32 kHz decode, ducked as the export ducks it — so it can limit a little differently from the export. True peak’s oversampling is export-only. ${measure}`;
	if (kind === 'approximate')
		return `This preview cannot run the export’s limiter, so it plays an approximation — a compressor with a hard knee at the ceiling — and a very hot mix can read a little over it. ${measure}`;
	return `The preview runs the export’s limiter (FFmpeg’s alimiter) on its own mix once playback has loaded it, and an approximation until then or where it cannot. ${measure}`;
}

/** Whether the preview ducks (`AudioEngine.duckKind`): `pending` until a ducked mix has loaded the worklet, `live` once
 *  it has — the export's own compressor / speech gate on the preview's mix — `unavailable` where it cannot run. */
export type DuckKind = 'pending' | 'live' | 'unavailable';

/** What the Duck tooltips say about the preview's ducking: it is the export's arithmetic on the preview's mix, which is
 *  not the export's (a mono 32 kHz decode, a start from nothing at the playhead), so the dip can differ a little, and
 *  the compressor ends a ducked track where the rest of the mix does — as the export's does today. */
export function duckNote(kind: DuckKind): string {
	if (kind === 'unavailable')
		return 'This preview cannot run the duck (no AudioWorklet), so a ducked track plays at its fader here; the export still dips it.';
	return 'The preview ducks as the export does — the same sidechain compressor or speech gate, keyed by the rest of the mix — but on the preview’s mix (a mono 32 kHz decode, starting from nothing at the playhead), so the dip can differ a little. Measure and Export read the real one.';
}

/** The Duck toggle's tooltip, on a strip and in the track header: what it does, and where the preview is the export's. */
export function duckToggleTip(on: boolean, kind: DuckKind): string {
	const head = on ? 'Ducking on — this track dips under the rest of the mix.' : 'Duck this track under the rest of the mix.';
	return `${head} ${duckNote(kind)}`;
}

/** The line under a strip's toggles while Duck is on: how far the preview holds it down while it plays (`down`, dB, positive), or
 *  that the preview cannot duck. */
export function duckHint(kind: DuckKind, down: number | null): string {
	if (kind === 'unavailable') return 'export only';
	if (down === null) return 'ducks';
	return down < 0.05 ? '0.0 dB' : `−${down.toFixed(1)} dB`;
}
