// How a band balance reads on screen: the six bands in order with short names, one row per window with
// a bar per band (its share of the window's energy), the dominant band and the loudness. Pure, so the
// Inspector's readout is bun-tested (`band-view.test.ts`); the measurement is `get_band_balance`'s.

import type { BandBalance, BandName, WindowBalance } from './types';

export const BAND_ORDER: readonly BandName[] = ['sub', 'bass', 'low_mid', 'mid', 'high_mid', 'air'];

export const BAND_LABEL: Record<BandName, string> = {
	sub: 'Sub',
	bass: 'Bass',
	low_mid: 'Low mid',
	mid: 'Mid',
	high_mid: 'High mid',
	air: 'Air'
};

/** What fits under a bar. */
export const BAND_SHORT: Record<BandName, string> = {
	sub: 'Sub',
	bass: 'Bass',
	low_mid: 'L-M',
	mid: 'Mid',
	high_mid: 'H-M',
	air: 'Air'
};

export const BAND_RANGE: Record<BandName, string> = {
	sub: '20–60 Hz',
	bass: '60–250 Hz',
	low_mid: '250–500 Hz',
	mid: '500 Hz–2 kHz',
	high_mid: '2–6 kHz',
	air: '6–20 kHz'
};

/** A level with a true minus: `−16.4`. */
export const fmtDb = (db: number): string => db.toFixed(1).replace('-', '−');

export interface BarView {
	name: BandName;
	/** Percent of the window's energy. */
	share: number;
	/** 0 to 1 of the readout's scale. */
	height: number;
	dominant: boolean;
	tip: string;
}

export interface WindowView {
	label: string;
	/** `0:12–0:24`. */
	range: string;
	/** `−16.4 LUFS`, or `silent`. */
	loudness: string;
	dominant: BandName | null;
	bars: BarView[];
	tip: string;
}

export interface BalanceView {
	overall: WindowView;
	rows: WindowView[];
	/** The percent a full-height bar stands for, the same in every row so they compare. */
	scale: number;
	/** Where the windows came from, in words. */
	basis: string;
	/** `Mostly mid · −16.4 LUFS`. */
	headline: string;
}

const BASIS: Record<BandBalance['windows_from'], string> = {
	sections: 'the sections given',
	score: 'the score’s sections',
	phrases: 'the music’s phrases',
	equal: 'equal parts'
};

/** A full-height bar is 40 % of a window's energy, or more when a band holds more, in steps of ten. */
export function scaleFor(windows: readonly WindowBalance[]): number {
	const top = windows.reduce((m, w) => w.bands.reduce((n, b) => Math.max(n, b.share_pct), m), 0);
	return Math.max(40, Math.ceil(top / 10) * 10);
}

function windowView(w: WindowBalance, scale: number, fmt: (seconds: number) => string, label?: string): WindowView {
	const range = `${fmt(w.start)}–${fmt(w.end)}`;
	const name = label ?? w.label ?? range;
	const loudness = w.loudness_lufs === null ? 'silent' : `${fmtDb(w.loudness_lufs)} LUFS`;
	const bars = BAND_ORDER.map((band): BarView => {
		const b = w.bands.find((x) => x.name === band);
		const share = b?.share_pct ?? 0;
		const level = b?.energy_dbfs == null ? '' : ` · ${fmtDb(b.energy_dbfs)} dBFS`;
		return {
			name: band,
			share,
			height: Math.min(Math.max(share / scale, 0), 1),
			dominant: w.dominant === band,
			tip: `${BAND_LABEL[band]} ${BAND_RANGE[band]}: ${share.toFixed(0)} %${level}`
		};
	});
	const lead = w.dominant ? `Mostly ${BAND_LABEL[w.dominant].toLowerCase()}` : 'Nothing to measure';
	const rms = w.rms_dbfs === null ? '' : ` · RMS ${fmtDb(w.rms_dbfs)} dBFS`;
	return {
		label: name,
		range,
		loudness,
		dominant: w.dominant,
		bars,
		tip: `${name} (${range})\n${lead} · ${loudness}${rms}`
	};
}

/** The readout of a balance: one row per window and the whole stretch. `fmt` prints a time (`timeDisplay.fmt`). */
export function balanceView(balance: BandBalance, fmt: (seconds: number) => string): BalanceView {
	const scale = scaleFor([balance.overall, ...balance.windows]);
	const overall = windowView(balance.overall, scale, fmt, 'Overall');
	const lead = overall.dominant ? `Mostly ${BAND_LABEL[overall.dominant].toLowerCase()}` : 'Silent';
	return {
		overall,
		rows: balance.windows.map((w) => windowView(w, scale, fmt)),
		scale,
		basis: BASIS[balance.windows_from] ?? 'windows',
		headline: overall.loudness === 'silent' ? lead : `${lead} · ${overall.loudness}`
	};
}
