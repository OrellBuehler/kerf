/**
 * The words and gates around "Fit to video": when a clip offers it, what a plan reads like next to
 * its target, and the toast after. Pure, so the clip menu, the Inspector and the dialog agree
 * (`music-ui.test.ts`). The arithmetic is `music-fit.ts`'s.
 */
import { formatTime } from './diff';
import { FIT_FADE_S, gridBarSeconds, gridBpm } from './music-fit';
import type { AssetAnalysis, Clip, MusicAnalysis, MusicFit, MusicFitReport, Track } from './types';

/** Whether a clip offers *Fit to video*, and if it cannot be used right now, why. */
export interface FitOffer {
	/** A clip on an audio track whose asset has a music analysis (a fitted bar grid). */
	show: boolean;
	/** Why the action is greyed out — the backend's own refusals, said before the click. */
	reason: string | null;
}

/**
 * `show` is the analysis: nothing else says a clip is music a fit could be planned for (speech on an
 * audio track has none). The reasons mirror what `Project::fit_music` refuses: a locked track, a clip
 * still linked to a picture, a clip not at normal speed.
 */
export function fitOffer(track: Track | undefined, clip: Clip, analysis: Pick<AssetAnalysis, 'music'> | null | undefined): FitOffer {
	if (track?.kind !== 'audio' || !analysis?.music) return { show: false, reason: null };
	if (track.locked) return { show: true, reason: `Track ${track.name} is locked` };
	if (clip.link_id) return { show: true, reason: 'Unlink the music from its picture first' };
	if (Math.abs((clip.speed ?? 1) - 1) > 1e-9) return { show: true, reason: 'Needs the clip at normal speed' };
	return { show: true, reason: null };
}

/** `120 BPM · 4/4 · 31 bars` — what the analysis found, for the Inspector's section and the dialog. */
export function musicSummary(music: MusicAnalysis): string {
	const bpm = Math.round(gridBpm(music.grid) * 10) / 10;
	const bars = music.bar_chroma.length;
	return `${bpm} BPM · ${music.grid.beats_per_bar}/4 · ${bars} bar${bars === 1 ? '' : 's'}`.replace('.0 BPM', ' BPM');
}

/** How long a bar lasts, `2.0 s`. */
export const barLength = (music: MusicAnalysis): string => `${gridBarSeconds(music.grid).toFixed(1)} s`;

/** How a plan reads next to its target: `exact`, `short` of it or `over` it. */
export type FitVerdict = 'exact' | 'short' | 'over';

/** Below this the remainder is rounding, not a miss (a sample at 8 kHz is 125 µs). */
const EXACT_S = 0.0005;

export function fitVerdict(fit: Pick<MusicFit, 'remainder'>): FitVerdict {
	if (Math.abs(fit.remainder) <= EXACT_S) return 'exact';
	return fit.remainder > 0 ? 'short' : 'over';
}

const seconds = (s: number): string => `${(Math.round(s * 10) / 10).toFixed(1)} s`;

/** One line for the remainder, aware of whether an overrun will be faded. */
export function describeRemainder(fit: Pick<MusicFit, 'remainder'>, fadeOut: boolean): string {
	const verdict = fitVerdict(fit);
	if (verdict === 'exact') return 'Lands exactly on the target';
	const by = seconds(Math.abs(fit.remainder));
	if (verdict === 'short') return `${by} short of the target — the closest arrangement that splices on a phrase`;
	return fadeOut
		? `${by} over — cut at the target and faded out over ${seconds(FIT_FADE_S)}`
		: `${by} over — the whole arrangement plays, ending included`;
}

/** The toast after a fit: `Music fitted to 0:20.5 · 1 splice`, with what was done to an overrun. */
export function describeFit(report: Pick<MusicFitReport, 'duration' | 'faded' | 'fit'>): string {
	const n = report.fit.splices;
	const splices = n === 0 ? 'no splices' : `${n} splice${n === 1 ? '' : 's'}`;
	return `Music fitted to ${formatTime(report.duration)} · ${splices}${report.faded ? ' · faded out' : ''}`;
}

/** Parse the dialog's custom-length box: a positive number of seconds (a comma decimal is
 *  accepted), else `null`. */
export function parseTarget(text: string): number | null {
	const t = text.trim().replace(',', '.');
	if (t === '') return null;
	const v = Number(t);
	return Number.isFinite(v) && v > 0 ? v : null;
}
