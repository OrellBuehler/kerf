/* The Mixer's meters: what a block of samples reads as, and how a meter moves
 * between blocks — pure, so the ballistics are pinned by tests and the component
 * only draws.
 *
 * A meter shows three things for each channel, the way a console's does:
 *
 *  - the **peak** — the loudest sample of each block, which rises at once and
 *    falls at a fixed rate, so a transient can be seen;
 *  - the **RMS** — the block's power, smoothed (quick to rise, slower to fall),
 *    which is closer to how loud it sounds;
 *  - a **hold** — the peak parked at its highest for a moment before it falls, with
 *    the highest peak since it was cleared (`max`) and whether the channel ever
 *    reached full scale (`clipped`, latched until cleared) beside it.
 *
 * Levels are dBFS of the *sample* peak — not the true peak the Measure button
 * reports, which needs oversampling the analyser nodes do not do. */

/** Quieter than this reads as nothing; the bottom of every meter. */
export const METER_FLOOR_DB = -90;

/** A sample at or above this is full scale: the channel is clipping (or about to). */
export const METER_CLIP_DB = -0.1;

/** The peak bar's fall, dB per second — near the 20 dB in 1.7 s of a digital PPM. */
export const PEAK_FALL_DB_PER_S = 15;
/** How long the hold marker stays at the highest peak before it starts to fall. */
export const HOLD_MS = 1400;
/** The hold marker's fall once it lets go, dB per second. */
export const HOLD_FALL_DB_PER_S = 24;
/** The RMS bar's time constant rising and falling, ms. */
export const RMS_RISE_MS = 80;
export const RMS_FALL_MS = 280;
/** The longest step a meter integrates: a tab that was in the background does not
 *  come back with every bar dropped to nothing. */
export const MAX_STEP_MS = 250;

/** One block of one channel, read in dBFS. */
export interface Reading {
	peak: number;
	rms: number;
}

/** A level as dBFS; nothing (or less than the floor) is the floor. */
export function toDb(linear: number): number {
	if (!(linear > 0)) return METER_FLOOR_DB;
	return Math.max(METER_FLOOR_DB, 20 * Math.log10(linear));
}

/** The peak and RMS of a block of samples, in dBFS. */
export function readBlock(samples: ArrayLike<number>): Reading {
	const n = samples.length;
	if (n === 0) return SILENCE;
	let peak = 0;
	let sum = 0;
	for (let i = 0; i < n; i++) {
		const x = samples[i];
		const a = x < 0 ? -x : x;
		if (a > peak) peak = a;
		sum += x * x;
	}
	return { peak: toDb(peak), rms: toDb(Math.sqrt(sum / n)) };
}

/** What a channel reads when nothing is playing through it. */
export const SILENCE: Reading = { peak: METER_FLOOR_DB, rms: METER_FLOOR_DB };

/** One channel's meter. */
export interface ChannelMeter {
	/** The peak bar, dBFS. */
	peak: number;
	/** The RMS bar, dBFS. */
	rms: number;
	/** The hold marker, dBFS. */
	hold: number;
	/** Milliseconds since the hold marker was last raised. */
	holdAge: number;
	/** The highest peak since the meter was cleared, dBFS. */
	max: number;
	/** A sample reached full scale since the meter was cleared. */
	clipped: boolean;
}

/** A meter that has read nothing. */
export const IDLE_METER: ChannelMeter = {
	peak: METER_FLOOR_DB,
	rms: METER_FLOOR_DB,
	hold: METER_FLOOR_DB,
	holdAge: 0,
	max: METER_FLOOR_DB,
	clipped: false
};

/** Both channels of a strip. */
export interface StereoMeter {
	l: ChannelMeter;
	r: ChannelMeter;
}

export const IDLE_STEREO: StereoMeter = { l: IDLE_METER, r: IDLE_METER };

/**
 * The meter `dtMs` later, given what its channel read in that time. The peak and
 * the hold take the reading at once when it is higher; otherwise the peak falls at
 * `PEAK_FALL_DB_PER_S`, the hold waits `HOLD_MS` then falls (never below the bar),
 * and the RMS eases toward the reading — quickly up, slowly down.
 */
export function stepMeter(m: ChannelMeter, r: Reading, dtMs: number): ChannelMeter {
	const dt = Math.min(MAX_STEP_MS, Math.max(0, Number.isFinite(dtMs) ? dtMs : 0));
	const secs = dt / 1000;
	const peak = Math.max(r.peak, m.peak - PEAK_FALL_DB_PER_S * secs, METER_FLOOR_DB);

	const tau = r.rms > m.rms ? RMS_RISE_MS : RMS_FALL_MS;
	const rms = Math.max(METER_FLOOR_DB, m.rms + (r.rms - m.rms) * (1 - Math.exp(-dt / tau)));

	let hold = m.hold;
	let holdAge = m.holdAge + dt;
	if (r.peak >= hold) {
		hold = r.peak;
		holdAge = 0;
	} else if (holdAge > HOLD_MS) {
		hold = Math.max(hold - HOLD_FALL_DB_PER_S * secs, METER_FLOOR_DB);
	}
	hold = Math.max(hold, peak);

	return {
		peak,
		rms,
		hold,
		holdAge,
		max: Math.max(m.max, r.peak),
		clipped: m.clipped || r.peak >= METER_CLIP_DB
	};
}

/** Playback stopped: the bars drop to nothing, but what the run reached — the hold
 *  marker, the highest peak, a clip — stays to be read, until it is cleared. */
export function settleMeter(m: ChannelMeter): ChannelMeter {
	return { ...m, peak: METER_FLOOR_DB, rms: METER_FLOOR_DB, holdAge: 0 };
}

/** Clear what a run reached — the hold marker, the highest peak, the clip lamp — keeping
 *  the bars where they are (a meter cleared mid-play does not blink). */
export function clearMeter(m: ChannelMeter): ChannelMeter {
	return { ...m, hold: m.peak, holdAge: 0, max: METER_FLOOR_DB, clipped: false };
}

export const clearStereo = (m: StereoMeter): StereoMeter => ({ l: clearMeter(m.l), r: clearMeter(m.r) });

/** Both channels, one step. */
export function stepStereo(m: StereoMeter, l: Reading, r: Reading, dtMs: number): StereoMeter {
	return { l: stepMeter(m.l, l, dtMs), r: stepMeter(m.r, r, dtMs) };
}

export function settleStereo(m: StereoMeter): StereoMeter {
	return { l: settleMeter(m.l), r: settleMeter(m.r) };
}

/** The highest peak either channel reached since it was cleared. */
export const strongest = (m: StereoMeter): number => Math.max(m.l.max, m.r.max);

/** Whether either channel clipped. */
export const clippedAny = (m: StereoMeter): boolean => m.l.clipped || m.r.clipped;

/** A peak as the strip's readout writes it: `-6.2`, or `-∞` when nothing came through. */
export function peakLabel(db: number): string {
	if (db <= METER_FLOOR_DB + 1e-6) return '−∞';
	return (Math.abs(db) < 0.05 ? 0 : db).toFixed(1);
}
