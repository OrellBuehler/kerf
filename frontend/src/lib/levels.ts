/* The master bus and the cut's measured levels — the TS side of
 * `kerf_core::MasterBus` and `kerf_core::Levels`.
 *
 * The real measurement is an `ebur128` pass over the export's audio graph and
 * only the backend can make it. What lives here is what the browser harness (and
 * the mixer's own labels) need without one:
 *
 *  - the master bus's limits and defaults — the same numbers as the Rust
 *    constants, so a control clamps where the engine does;
 *  - `levelNotes`, the **faithful** mirror of the advice `Levels::new` writes, so
 *    the harness says what the backend would about the same numbers;
 *  - `estimateLevels`, an **approximation** for the harness only: it combines the
 *    per-asset loudness the sample analysis carries through the faders, the pan,
 *    the master and the limiter. It is flagged `estimated`, because it knows
 *    nothing about what is actually in the audio.
 */

import { toFixedEven } from './format-fixed';
import { effectiveGain, gainToDb, panGains } from './mixer';
import type { Asset, Levels, LevelReading, Loudness, MasterBus, Timeline, Track, TrackLevels } from './types';
import { clipDuration } from './types';

/** The top of the master fader: +12 dB (`MASTER_MAX_VOLUME`). */
export const MASTER_MAX_VOLUME = 4;
/** The lowest limiter ceiling (`MASTER_MIN_CEILING_DB`): the filter's floor of 0.0625. */
export const MASTER_MIN_CEILING_DB = -24;
/** The default limiter ceiling, dBFS (`MASTER_DEFAULT_CEILING_DB`): the limiter holds the
 *  *sample* peak and the peak between samples runs above it, so it sits half a dB under
 *  the -1 dBTP that platforms ask for. */
export const MASTER_DEFAULT_CEILING_DB = -1.5;
/** The streaming loudness target the notes judge against (`LEVELS_TARGET_LUFS`). */
export const LEVELS_TARGET_LUFS = -14;
/** The true-peak ceiling platforms ask of a delivery (`LEVELS_TRUE_PEAK_CEILING_DBTP`). */
export const LEVELS_TRUE_PEAK_CEILING_DBTP = -1;
/** How much further than the overshoot the notes advise lowering a ceiling (`LEVELS_CEILING_MARGIN_DB`). */
const LEVELS_CEILING_MARGIN_DB = 0.5;

/** A master that does nothing — what a project that never touched it holds. */
export const DEFAULT_MASTER: MasterBus = { volume: 1, limiter: false, ceiling_db: MASTER_DEFAULT_CEILING_DB };

/** The timeline's master bus with every field filled (an absent or partial one is the default). */
export function masterOf(timeline: Pick<Timeline, 'master'>): MasterBus {
	return { ...DEFAULT_MASTER, ...(timeline.master ?? {}) };
}

/** A fader value as the engine clamps it: `0..=4`. */
export const clampMasterVolume = (v: number): number => Math.min(MASTER_MAX_VOLUME, Math.max(0, v));

/** A ceiling as the engine clamps it: `-24..=0` dBFS. */
export const clampCeiling = (db: number): number => Math.min(0, Math.max(MASTER_MIN_CEILING_DB, db));

/** How far one keypress moves the limiter's ceiling, in dB. */
export const CEILING_STEP = { fine: 0.1, normal: 0.5, coarse: 3, page: 6 } as const;

/** The ceiling slider's travel: a ceiling's place on it, `0..1` (left is `-24`, right is `0`). */
export const ceilingToPos = (db: number): number => (clampCeiling(db) - MASTER_MIN_CEILING_DB) / -MASTER_MIN_CEILING_DB;

/** The ceiling a place on the travel means, to the half dB — what the label can say. */
export const posToCeiling = (pos: number): number =>
	clampCeiling(Math.round((MASTER_MIN_CEILING_DB + Math.min(1, Math.max(0, pos)) * -MASTER_MIN_CEILING_DB) * 2) / 2);

/** The ceiling nudged by `steps` marks of `size`, on that size's own dB grid, within `-24..0`. */
export function nudgeCeiling(db: number, steps: number, size: keyof typeof CEILING_STEP = 'normal'): number {
	const grid = CEILING_STEP[size];
	const eps = 1e-9;
	const from = clampCeiling(db);
	const cell = steps >= 0 ? Math.floor(from / grid + eps) : Math.ceil(from / grid - eps);
	const next = clampCeiling(Math.round((cell + steps) * grid * 1000) / 1000);
	return next === 0 ? 0 : next;
}

/** `-1.0 dBFS`: a ceiling as the limiter's label writes it. */
export const ceilingLabel = (db: number): string => `${(Math.abs(db) < 0.05 ? 0 : db).toFixed(1)} dBFS`;

/** Whether the master leaves the mix alone (unity gain, no limiter) — `MasterBus::is_neutral`. */
export function isNeutralMaster(master: MasterBus): boolean {
	return !master.limiter && Math.abs(master.volume - 1) <= Number.EPSILON;
}

const f1 = (x: number): string => toFixedEven(x, 1);
const f0 = (x: number): string => String(Math.round(x));
const signed1 = (x: number): string => `${x >= 0 ? '+' : ''}${f1(x)}`;

/**
 * The advice behind `Levels.notes` — the mirror of `level_notes` in
 * crates/kerf-core/src/model.rs, word for word, so the harness reads like the
 * backend about the same numbers. `bus` is the master the mix went through: a true
 * peak over the line with the limiter already on is a ceiling to lower, not a limiter
 * to turn on.
 */
export function levelNotes(master: LevelReading | null, tracks: TrackLevels[], bus: MasterBus = DEFAULT_MASTER): string[] {
	if (!master) return ['The cut has no audio to measure.'];
	const notes: string[] = [];
	const i = master.integrated_lufs;
	const target = LEVELS_TARGET_LUFS;
	if (i === null) {
		notes.push('The mix is silent.');
	} else if (i - target > 1) {
		notes.push(
			`Integrated loudness ${f1(i)} LUFS is ${f1(i - target)} LU over the ${f0(target)} LUFS streaming target — platforms will turn it down. Lower the master or the loudest track, or export with loudnorm.`
		);
	} else if (target - i > 3) {
		notes.push(
			`Integrated loudness ${f1(i)} LUFS is ${f1(target - i)} LU under the ${f0(target)} LUFS streaming target — it will sound quiet beside other posts. Raise the master or the tracks, or export with loudnorm.`
		);
	} else {
		notes.push(`Integrated loudness ${f1(i)} LUFS is close to the ${f0(target)} LUFS streaming target.`);
	}
	const tp = master.true_peak_dbtp;
	if (tp !== null && tp > LEVELS_TRUE_PEAK_CEILING_DBTP) {
		const over = `True peak ${f1(tp)} dBTP is over the ${f0(LEVELS_TRUE_PEAK_CEILING_DBTP)} dBTP platforms ask for and can clip when re-encoded.`;
		if (bus.limiter) {
			// The limiter holds the sample peak at its ceiling, so what is left over is the
			// peak between samples: take the ceiling down by that, and a margin.
			const ceiling = Number.isFinite(bus.ceiling_db) ? clampCeiling(bus.ceiling_db) : MASTER_DEFAULT_CEILING_DB;
			const lower = Math.max(
				ceiling - (tp - LEVELS_TRUE_PEAK_CEILING_DBTP) - LEVELS_CEILING_MARGIN_DB,
				MASTER_MIN_CEILING_DB
			);
			notes.push(
				lower < ceiling
					? `${over} The master limiter is already on, but it holds the sample peak at ${f1(ceiling)} dBFS and the peak between samples runs above that. Lower its ceiling to about ${f1(lower)} dBFS (set_master_limiter).`
					: `${over} The master limiter is already on at its lowest ceiling. Lower the master or the loudest track.`
			);
		} else {
			notes.push(`${over} Turn on the master limiter (set_master_limiter) or lower the master.`);
		}
	}
	for (const t of tracks) {
		const peak = t.level?.peak_dbfs;
		if (peak !== null && peak !== undefined && peak > 0) {
			notes.push(
				`Track ${t.name} peaks at ${signed1(peak)} dBFS before the master, over full scale. Nothing clips until the mix is written, so the master can still bring it under (its fader or limiter); otherwise lower this track's fader.`
			);
		}
	}
	return notes;
}

/** Whether a track reaches the render: muted never does, and while a track of its kind is soloed, only soloed ones. */
export function trackRenders(timeline: Timeline, track: Track): boolean {
	if (track.muted) return false;
	const soloed = timeline.tracks.some((t) => t.kind === track.kind && t.solo);
	return !soloed || !!track.solo;
}

/** What the sample analysis does not say: a typical, unremarkable mix. */
const FALLBACK: Pick<Loudness, 'integrated_lufs' | 'true_peak_dbtp'> = { integrated_lufs: -23, true_peak_dbtp: -3 };

const powerSum = (dbs: number[]): number => 10 * Math.log10(dbs.reduce((s, d) => s + 10 ** (d / 10), 0));

/**
 * A stand-in for `get_levels` where there is no ffmpeg to run: the loudness the
 * sample analysis carries per asset (`loudnessOf`), carried through each clip's
 * volume, its track's fader and pan, the master fader and the limiter. Flagged
 * `estimated`. Deliberately simple — clips are taken to play whenever the span
 * covers them, tracks to be concurrent and to add in power (uncorrelated
 * sources: their peaks do not line up), which errs loud for a duplicated source
 * and is the usual case otherwise.
 */
export function estimateLevels(
	timeline: Timeline,
	assets: Asset[],
	loudnessOf: (assetId: string) => Loudness | undefined,
	opts: { range?: { start: number; end: number } | null; loudnorm?: boolean } = {}
): Levels {
	const loudnorm = !!opts.loudnorm;
	const range = opts.range ?? null;
	const cutEnd = timeline.tracks.reduce(
		(m, t) => Math.max(m, ...t.clips.map((c) => c.timeline_start + clipDuration(c))),
		0
	);
	const duration = range ? Math.max(0, Math.min(range.end, cutEnd) - range.start) : cutEnd;
	const hasAudio = (assetId: string) =>
		assets.find((a) => a.id === assetId)?.streams.some((s) => s.kind === 'audio') ?? false;

	const tracks: TrackLevels[] = [];
	const heard: { integrated: number; peak: number }[] = [];
	for (const track of timeline.tracks) {
		const audible = track.clips.filter((c) => hasAudio(c.asset_id));
		if (audible.length === 0) continue;
		const [gl, gr] = panGains(track.pan ?? 0);
		// A balance leaves the louder side at unity, so it moves the loudness by one
		// channel's share of the power and the peak not at all.
		const panDb = 10 * Math.log10((gl * gl + gr * gr) / 2);
		const parts = audible
			.map((c) => {
				const start = range ? Math.max(c.timeline_start, range.start) : c.timeline_start;
				const end = range ? Math.min(c.timeline_start + clipDuration(c), range.end) : c.timeline_start + clipDuration(c);
				const loud = loudnessOf(c.asset_id) ?? FALLBACK;
				const gainDb = gainToDb(effectiveGain(c.volume, track.volume));
				return { seconds: Math.max(0, end - start), integrated: loud.integrated_lufs + gainDb + panDb, peak: loud.true_peak_dbtp + gainDb };
			})
			.filter((p) => p.seconds > 0);
		const rendered = trackRenders(timeline, track);
		let level: LevelReading | null = null;
		if (rendered && parts.length > 0 && parts.every((p) => Number.isFinite(p.integrated))) {
			const total = parts.reduce((s, p) => s + p.seconds, 0);
			// A power mean over the time each clip plays: gaps are gated out of a real
			// integrated loudness, so they do not pull the average down.
			const integrated = 10 * Math.log10(parts.reduce((s, p) => s + (p.seconds / total) * 10 ** (p.integrated / 10), 0));
			const peak = Math.max(...parts.map((p) => p.peak));
			level = {
				integrated_lufs: integrated,
				loudness_range_lu: 6,
				short_term_max_lufs: duration >= 3 ? integrated + 1.5 : null,
				peak_dbfs: peak - 0.2,
				true_peak_dbtp: peak
			};
			heard.push({ integrated, peak });
		}
		tracks.push({
			track_id: track.id,
			name: track.name,
			kind: track.kind,
			ducked: !!track.duck,
			heard: rendered,
			level
		});
	}

	let master: LevelReading | null = null;
	if (heard.length > 0) {
		const m = masterOf(timeline);
		let integrated = powerSum(heard.map((h) => h.integrated));
		let peak = powerSum(heard.map((h) => h.peak));
		const gain = gainToDb(clampMasterVolume(m.volume));
		if (Number.isFinite(gain)) {
			integrated += gain;
			peak += gain;
			if (m.limiter && peak > m.ceiling_db) {
				// A limiter taking X dB off the peak takes less than X off the loudness.
				integrated -= (peak - m.ceiling_db) * 0.5;
				peak = clampCeiling(m.ceiling_db);
			}
			if (loudnorm) {
				peak = Math.min(peak + (LEVELS_TARGET_LUFS - integrated), -1.5);
				integrated = LEVELS_TARGET_LUFS;
			}
			master = {
				integrated_lufs: integrated,
				loudness_range_lu: 6,
				short_term_max_lufs: duration >= 3 ? integrated + 1.5 : null,
				peak_dbfs: peak - 0.3,
				true_peak_dbtp: peak
			};
		} else {
			master = { integrated_lufs: null, loudness_range_lu: 0, short_term_max_lufs: null, peak_dbfs: null, true_peak_dbtp: null };
		}
	} else if (tracks.some((t) => t.heard)) {
		// Audio is in the render but nothing could be estimated: say silence, not "no audio".
		master = { integrated_lufs: null, loudness_range_lu: 0, short_term_max_lufs: null, peak_dbfs: null, true_peak_dbtp: null };
	}

	return {
		duration,
		master,
		tracks,
		loudnorm,
		target_lufs: LEVELS_TARGET_LUFS,
		notes: levelNotes(master, tracks, masterOf(timeline)),
		estimated: true
	};
}
