// Band balance in the browser harness: windows are chosen exactly as `project/music.rs` chooses
// them (explicit sections, a composed asset's score sections, the analysed bar grid in phrases,
// eight equal parts) and the notes are `balance_notes` word for word; only the *measurement* is
// made up, because the harness has no audio to decode. Each window gets a plausible spread of
// energy over the six bands for the kind of section it is, steady from run to run.

import type { BandBalance, BandLevel, BandName, BandWindow, SectionKind, WindowBalance } from './types';

/** The six bands: name and edges in Hz (`engine::bands::BANDS`). */
export const BANDS: { name: BandName; low_hz: number; high_hz: number }[] = [
	{ name: 'sub', low_hz: 20, high_hz: 60 },
	{ name: 'bass', low_hz: 60, high_hz: 250 },
	{ name: 'low_mid', low_hz: 250, high_hz: 500 },
	{ name: 'mid', low_hz: 500, high_hz: 2000 },
	{ name: 'high_mid', low_hz: 2000, high_hz: 6000 },
	{ name: 'air', low_hz: 6000, high_hz: 20000 }
];

/** A window shorter than this has no frame to measure. */
export const MIN_WINDOW_S = 0.25;

/** `clip_windows`: cut `windows` to `lo..hi`, dropping what is left under `MIN_WINDOW_S`. */
export function clipWindows(windows: readonly BandWindow[], lo: number, hi: number): BandWindow[] {
	const out: BandWindow[] = [];
	for (const w of windows) {
		const start = Math.max(w.start, lo);
		const end = Math.min(w.end, hi);
		if (end - start >= MIN_WINDOW_S) out.push({ start, end, ...(w.label !== undefined ? { label: w.label } : {}) });
	}
	return out;
}

/** `equal_windows`: eight parts of `lo..hi`, or one when it is too short to split. */
export function equalWindows(lo: number, hi: number): BandWindow[] {
	const n = 8;
	const step = (hi - lo) / n;
	if (step < MIN_WINDOW_S) return [{ start: lo, end: hi }];
	return Array.from({ length: n }, (_, i) => ({ start: lo + i * step, end: lo + (i + 1) * step, label: `${i + 1}/${n}` }));
}

/** `phrase_windows`: groups of four bars (then eight, sixteen … while there would be more than 16)
 *  along a bar grid, up to `hi`. */
export function phraseWindows(firstDownbeat: number, barS: number, lo: number, hi: number): BandWindow[] {
	let bars = 4;
	const total = Math.max(Math.floor((hi - firstDownbeat) / barS), 0);
	while (Math.ceil(total / bars) > 16) bars *= 2;
	const out: BandWindow[] = [];
	for (let k = 0; k < total; ) {
		const endBar = Math.min(k + bars, total);
		out.push({
			start: firstDownbeat + k * barS,
			end: firstDownbeat + endBar * barS,
			label: endBar - k === 1 ? `bar ${k + 1}` : `bars ${k + 1}–${endBar}`
		});
		k = endBar;
	}
	return clipWindows(out, lo, hi);
}

/** `check_sections`: explicit sections, finite, ordered, inside the stretch and cut to it; the ones
 *  left with nothing are an error that names them. Returns the windows, or the message. */
export function checkSections(given: readonly BandWindow[], lo: number, hi: number): BandWindow[] | string {
	if (given.length === 0) return 'sections is empty: give at least one {start, end}';
	if (given.length > 100) return 'at most 100 sections';
	const out: BandWindow[] = [];
	for (const [i, w] of given.entries()) {
		if (!Number.isFinite(w.start) || !Number.isFinite(w.end) || w.end <= w.start) {
			return `sections[${i}]: needs start < end, in seconds`;
		}
		const cut = clipWindows([w], lo, hi);
		if (cut.length === 0) {
			return `sections[${i}] (${w.start.toFixed(1)}..${w.end.toFixed(1)}s) has less than ${MIN_WINDOW_S}s inside the measured sound (${lo.toFixed(1)}..${hi.toFixed(1)}s)`;
		}
		out.push(cut[0]);
	}
	return out;
}

// ---- the made-up measurement ---------------------------------------------------------------

/** A stable 0..1 from two integers, so the harness never flickers. */
function wobble(a: number, b: number): number {
	let h = Math.imul(a + 1, 0x9e3779b1) ^ Math.imul(b + 1, 0x85ebca6b);
	h ^= h >>> 15;
	h = Math.imul(h, 0x2c1b3c6d);
	h ^= h >>> 12;
	return (h >>> 0) / 0xffffffff;
}

/** Percent of the energy in each band (sub … air) for a kind of section; a drop is full, an intro a pad. */
const SHARES: Record<SectionKind | 'other', number[]> = {
	intro: [1, 9, 26, 36, 20, 8],
	verse: [3, 21, 22, 28, 18, 8],
	build: [4, 24, 20, 26, 18, 8],
	drop: [7, 30, 17, 21, 16, 9],
	breakdown: [1, 8, 25, 36, 22, 8],
	outro: [1, 9, 26, 36, 20, 8],
	other: [3, 22, 20, 30, 18, 7]
};

/** The RMS level of each kind, dBFS. */
const LEVEL: Record<SectionKind | 'other', number> = {
	intro: -25,
	verse: -21,
	build: -19,
	drop: -16,
	breakdown: -24,
	outro: -25,
	other: -20
};

const round = (x: number, places = 2) => Math.round(x * 10 ** places) / 10 ** places;

function windowOf(w: BandWindow, shares: number[], rms: number): WindowBalance {
	const total = shares.reduce((a, b) => a + b, 0);
	const bands: BandLevel[] = BANDS.map((b, i) => {
		const share = (shares[i] / total) * 100;
		return {
			name: b.name,
			low_hz: b.low_hz,
			high_hz: b.high_hz,
			energy_dbfs: round(rms + 10 * Math.log10(share / 100)),
			share_pct: round(share)
		};
	});
	const top = bands.reduce((m, b) => (b.share_pct > m.share_pct ? b : m), bands[0]);
	return {
		start: w.start,
		end: w.end,
		label: w.label ?? null,
		rms_dbfs: round(rms),
		loudness_lufs: round(rms + 1.8),
		bands,
		dominant: top.name
	};
}

/** The made-up balance of `windows`, a kind of section for each when known (`kinds[i]`), and the
 *  whole stretch as one window — the shape `Project::measure_band_balance` answers. */
export function synthBalance(
	windows: readonly BandWindow[],
	kinds: readonly (SectionKind | null)[] = []
): Pick<BandBalance, 'windows' | 'overall'> {
	const measured = windows.map((w, i) => {
		const kind = kinds[i] ?? 'other';
		const shares = SHARES[kind].map((s, k) => s * (0.85 + 0.3 * wobble(i, k)));
		return windowOf(w, shares, LEVEL[kind] + (wobble(i, 99) - 0.5) * 2);
	});
	const lo = windows.length ? Math.min(...windows.map((w) => w.start)) : 0;
	const hi = windows.length ? Math.max(...windows.map((w) => w.end)) : 0;
	const weights = windows.map((w) => w.end - w.start);
	const sum = weights.reduce((a, b) => a + b, 0) || 1;
	const shares = BANDS.map((_, k) => measured.reduce((a, m, i) => a + m.bands[k].share_pct * weights[i], 0) / sum);
	const ms = measured.reduce((a, m, i) => a + 10 ** ((m.rms_dbfs ?? -120) / 10) * weights[i], 0) / sum;
	return { windows: measured, overall: windowOf({ start: lo, end: hi }, shares, 10 * Math.log10(ms)) };
}

/** `balance_notes`: what stands out in a balance, in words. */
export function balanceNotes(overall: WindowBalance, windows: readonly WindowBalance[]): string[] {
	const notes: string[] = [];
	if (overall.bands.length === 0 || overall.rms_dbfs === null) return ['The sound is silent.'];
	const share = (w: WindowBalance, name: BandName) => w.bands.find((b) => b.name === name)?.share_pct ?? 0;
	const low = share(overall, 'sub') + share(overall, 'bass');
	if (low > 60) {
		notes.push(
			`The low end (sub + bass, under 250 Hz) holds ${low.toFixed(0)}% of the energy: heavy. A voice over it needs the music's low end cut back (an EQ high-pass on the music) or the music turned down.`
		);
	}
	if (share(overall, 'sub') > 20) {
		notes.push(
			`The sub band (20–60 Hz) holds ${share(overall, 'sub').toFixed(0)}%: more than small speakers and phones play, and it eats headroom.`
		);
	}
	const mids = share(overall, 'mid') + share(overall, 'high_mid');
	if (mids < 10) {
		notes.push(`The mid range (500 Hz–6 kHz) holds only ${mids.toFixed(0)}%: thin or boomy, and a voice will have little room to be heard.`);
	}
	if (share(overall, 'air') < 1 && share(overall, 'high_mid') < 6) notes.push('Almost nothing above 2 kHz: the sound is dull.');
	const name = (w: WindowBalance) => w.label ?? `${w.start.toFixed(0)}–${w.end.toFixed(0)}s`;
	const loud = windows.filter((w) => w.loudness_lufs !== null);
	if (loud.length >= 2) {
		const hi = loud.reduce((m, w) => (w.loudness_lufs! > m.loudness_lufs! ? w : m), loud[0]);
		const lo = loud.reduce((m, w) => (w.loudness_lufs! < m.loudness_lufs! ? w : m), loud[0]);
		const swing = hi.loudness_lufs! - lo.loudness_lufs!;
		if (swing > 6) {
			notes.push(
				`Loudness swings ${swing.toFixed(0)} LU between "${name(hi)}" (${hi.loudness_lufs!.toFixed(1)} LUFS) and "${name(lo)}" (${lo.loudness_lufs!.toFixed(1)} LUFS).`
			);
		}
	}
	for (const w of windows) {
		const d = share(w, 'sub') + share(w, 'bass') - low;
		if (d > 15) notes.push(`"${name(w)}" is ${d.toFixed(0)} points bassier than the whole.`);
	}
	return notes;
}
