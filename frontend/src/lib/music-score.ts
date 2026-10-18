// The pure half of a music score, in TypeScript: what the Compose dialog shows while a score is
// being written (its length, where its sections sit, whether it would be refused) and what the
// browser harness needs to fake a composition. A faithful mirror of `model/score.rs`
// (`duration_s`, `resolve`, `section_spans`), `score/chord.rs` (the chord and key grammar, syntax
// only: the harness has no synthesizer, so which notes a chord is does not matter here) and
// `score/align.rs` (`plan_score_to_picture`); the desktop app renders and aligns in kerf-core, and
// the tests replay its cases.

import type { AlignReport, BeatReading, CutAlignment, HitKind, MusicScore, MusicStyle, ScoreHit, SectionKind } from './types';

export const MIN_BPM = 40;
export const MAX_BPM = 240;
export const MAX_SECTIONS = 100;
export const MAX_SECTION_BARS = 128;
export const MAX_SCORE_BARS = 400;
/** The longest piece, seconds. */
export const MAX_SCORE_SECONDS = 600;
export const MAX_HITS = 256;
/** The largest seed: 2^53 - 1, the largest integer a JavaScript number (and so JSON through the webview or an
 *  agent) holds exactly. A bigger one would come back as another seed and render another piece. */
export const MAX_SEED = Number.MAX_SAFE_INTEGER;
export const MAX_CHORDS_PER_SECTION = 64;
/** How long the sound rings on after the last bar (at most 0.9 of a bar). */
export const RELEASE_TAIL_S = 1.5;
export const DEFAULT_MAX_TEMPO_CHANGE = 0.06;
/** A cut counts as on the beat within this many seconds of it. */
export const ON_BEAT_TOLERANCE_S = 0.04;

export const MUSIC_STYLES: { name: MusicStyle; label: string; default_bpm: number }[] = [
	{ name: 'tropical_house', label: 'Tropical house', default_bpm: 110 },
	{ name: 'pop', label: 'Pop', default_bpm: 112 },
	{ name: 'lofi', label: 'Lo-fi', default_bpm: 80 },
	{ name: 'cinematic', label: 'Cinematic', default_bpm: 76 },
	{ name: 'ambient', label: 'Ambient', default_bpm: 68 },
	{ name: 'rock', label: 'Rock', default_bpm: 124 }
];

export const SECTION_KINDS: SectionKind[] = ['intro', 'verse', 'build', 'drop', 'breakdown', 'outro'];

export const HIT_KINDS: HitKind[] = ['impact', 'riser', 'dropout', 'whoosh'];

/** How loud and busy a section of each kind is when it states no `energy`. */
export const DEFAULT_ENERGY: Record<SectionKind, number> = {
	intro: 0.3,
	verse: 0.5,
	build: 0.7,
	drop: 1,
	breakdown: 0.3,
	outro: 0.3
};

/** Seconds in one bar at `bpm`. */
export const barSeconds = (bpm: number): number => (4 * 60) / bpm;

export const totalBars = (score: MusicScore): number => score.sections.reduce((n, s) => n + s.bars, 0);

/** The render's length: every bar plus the release tail. Valid for any bpm. */
export function scoreDuration(score: MusicScore): number {
	const bar = barSeconds(score.bpm);
	return totalBars(score) * bar + Math.min(RELEASE_TAIL_S, 0.9 * bar);
}

/** A short title for the asset: `Lo-fi · Am · 82 BPM`. */
export function scoreTitle(score: MusicScore): string {
	const label = MUSIC_STYLES.find((s) => s.name === score.style)?.label ?? score.style;
	return `${label} · ${score.key.trim()} · ${Math.round(score.bpm)} BPM`;
}

/** Where a section sits in the piece. */
export interface SectionSpan {
	index: number;
	kind: SectionKind;
	/** The bar the section starts at (0-based). */
	start_bar: number;
	bars: number;
	start_s: number;
	end_s: number;
	/** The section's energy, stated or by kind. */
	energy: number;
}

/** The sections in time (they are not checked: use `validateScore`). */
export function sectionSpans(score: MusicScore): SectionSpan[] {
	const bar = barSeconds(score.bpm);
	let at = 0;
	return score.sections.map((s, index) => {
		const span: SectionSpan = {
			index,
			kind: s.kind,
			start_bar: at,
			bars: s.bars,
			start_s: at * bar,
			end_s: (at + s.bars) * bar,
			energy: s.energy ?? DEFAULT_ENERGY[s.kind]
		};
		at += s.bars;
		return span;
	});
}

/** The windows a composed asset's band balance reports on: its sections, named and numbered when a kind repeats. */
export function sectionLabels(kinds: readonly SectionKind[]): string[] {
	return kinds.map((k, i) => {
		const total = kinds.filter((o) => o === k).length;
		if (total === 1) return k;
		return `${k} ${kinds.slice(0, i + 1).filter((o) => o === k).length}`;
	});
}

/** The tonic chord of a key, as text: `Am` for `Am` / `A minor`, `F#` for `F#`. */
export function tonicChord(key: string): string {
	const m = /^([A-Ga-g])([#♯b♭]*)/.exec(key.trim());
	if (!m) return key.trim();
	return `${m[1].toUpperCase()}${m[2].replace(/♯/g, '#').replace(/♭/g, 'b')}${isMinorKey(key) ? 'm' : ''}`;
}

/** `Resolved::chords`: `chords[section][bar]` **as played** — each section's list cycled to its length, and the
 *  last bar of an outro that ends the piece replaced by the tonic chord (it resolves), so the notes and the chroma
 *  the analysis is written from agree. Chords stay text here: the harness has no synthesizer. */
export function resolvedChords(score: MusicScore): string[][] {
	const chords = score.sections.map((s) => {
		const bars = Number.isInteger(s.bars) && s.bars > 0 ? s.bars : 0;
		return Array.from({ length: bars }, (_, b) => (s.chords.length > 0 ? s.chords[b % s.chords.length] : ''));
	});
	if (score.sections.at(-1)?.kind === 'outro') {
		const last = chords.at(-1);
		if (last && last.length > 0) last[last.length - 1] = tonicChord(score.key);
	}
	return chords;
}

// ---- keys and chords (score/chord.rs, syntax only) ---------------------------------------

const LETTERS = 'CDEFGAB';

/** A note name (`F#`, `Bb`, `E`) at the start of `s`: its pitch class and the rest of the text. */
function takeNote(s: string): [number, string] | null {
	const letter = LETTERS.indexOf(s.charAt(0).toUpperCase());
	if (s.length === 0 || letter < 0) return null;
	let pc = [0, 2, 4, 5, 7, 9, 11][letter];
	let rest = s.slice(1);
	for (;;) {
		const c = rest.charAt(0);
		if (c === '#' || c === '♯') pc += 1;
		else if (c === 'b' || c === '♭') pc -= 1;
		else break;
		rest = rest.slice(1);
	}
	return [((pc % 12) + 12) % 12, rest];
}

/** The engine's `invalid argument: …` (`Error::InvalidArgument`). */
const invalid = (why: string) => new Error(`invalid argument: ${why}`);

const notAKey = (text: string) => `"${text}" is not a key: write a tonic and an optional m, like G, F#m, Bb or Ebm`;

/** Why `text` is not a key (`G`, `F#m`, `Bb`, `Ebm`), or null when it is one. */
export function keyError(text: string): string | null {
	const note = takeNote(text.trim());
	if (!note) return notAKey(text);
	return ['', 'maj', 'major', 'M', 'm', 'min', 'minor', '-'].includes(note[1]) ? null : notAKey(text);
}

/** Whether the key is a minor one; false for anything `keyError` refuses. */
export function isMinorKey(text: string): boolean {
	const note = takeNote(text.trim());
	return !!note && ['m', 'min', 'minor', '-'].includes(note[1]);
}

/** The suffixes a chord name may carry (case-sensitive: `M7` is major, `m7` minor). */
const SUFFIXES = new Set([
	'', 'maj', 'M', 'm', 'min', '-', 'dim', '°', 'o', 'aug', '+', 'sus2', 'sus4', 'sus', '5', '7', 'maj7', 'M7', 'Δ', 'Δ7', 'm7',
	'min7', '-7', 'm7b5', 'ø', 'ø7', 'dim7', '°7', 'o7', 'mmaj7', 'mM7', 'm(maj7)', '7sus4', '7sus', '6', 'maj6', 'm6', 'add9',
	'add2', 'madd9', 'm(add9)', '9', 'maj9', 'M9', 'm9', 'min9'
]);

function knownQuality(suffix: string, minorNumeral: boolean): boolean {
	if (!minorNumeral) return SUFFIXES.has(suffix);
	if (SUFFIXES.has(`m${suffix}`)) return true;
	return ['dim', '°', 'o', 'ø', 'sus', 'aug', '+'].some((p) => suffix.startsWith(p)) && SUFFIXES.has(suffix);
}

const NUMERALS = ['I', 'II', 'III', 'IV', 'V', 'VI', 'VII'];

/** Why `text` is not a chord — a name (`C`, `Em`, `F#m7`, `Gsus4`, `D/F#`) or a roman numeral in
 *  the key (`I`, `V`, `vi`, `IV`, `ii7`, `bVII`) —, or null when it is one. */
export function chordError(text: string): string | null {
	const t = text.trim();
	if (t === '') return 'an empty chord';
	const bad = (why: string) => `"${text}" is not a chord: ${why}`;
	const slash = t.lastIndexOf('/');
	const main = slash >= 0 ? t.slice(0, slash) : t;
	if (slash >= 0) {
		const bass = takeNote(t.slice(slash + 1).trim());
		if (!bass || bass[1] !== '') return bad('after the / goes a note name, like D/F#');
	}
	const first = main.charAt(0);
	if (first >= 'A' && first <= 'G') {
		const note = takeNote(main);
		if (!note) return bad('it starts with no note name');
		return SUFFIXES.has(note[1]) ? null : bad(`unknown chord type "${note[1]}"`);
	}
	let body = main;
	if (first !== '' && 'b♭#♯'.includes(first)) body = main.slice(1);
	const len = [...body].findIndex((c) => !'IViv'.includes(c));
	const numeral = len < 0 ? body : body.slice(0, len);
	const suffix = len < 0 ? '' : body.slice(len);
	if (!NUMERALS.includes(numeral.toUpperCase())) {
		return bad('write a chord name (C, Em, F#m7, Gsus4, D/F#) or a roman numeral (I, V, vi, IV, ii7)');
	}
	const lower = numeral === numeral.toLowerCase();
	const upper = numeral === numeral.toUpperCase();
	if (!lower && !upper) return bad('a roman numeral is all upper case (major) or all lower case (minor)');
	return knownQuality(suffix, lower) ? null : bad(`unknown chord type "${suffix}"`);
}

/** Why the score would be refused, as the engine words it (naming the section or chord at fault), or
 *  null when it would render. */
export function validateScore(score: MusicScore): string | null {
	if (!Number.isFinite(score.bpm) || score.bpm < MIN_BPM || score.bpm > MAX_BPM) {
		return `bpm must be ${MIN_BPM}..=${MAX_BPM} (got ${score.bpm})`;
	}
	const key = keyError(score.key);
	if (key) return key;
	if (!Number.isInteger(score.seed) || score.seed < 0 || score.seed > MAX_SEED) {
		return `seed must be 0..=${MAX_SEED} (2^53 - 1, so it survives JSON in JavaScript; got ${score.seed})`;
	}
	if (score.sections.length === 0) return 'the score needs at least one section';
	if (score.sections.length > MAX_SECTIONS) return `a score has at most ${MAX_SECTIONS} sections`;
	let bars = 0;
	for (const [i, section] of score.sections.entries()) {
		const at = `sections[${i}] (${section.kind})`;
		if (!Number.isInteger(section.bars) || section.bars < 1 || section.bars > MAX_SECTION_BARS) {
			return `${at}: bars must be 1..=${MAX_SECTION_BARS} (got ${section.bars})`;
		}
		const e = section.energy;
		if (e !== undefined && e !== null && (!Number.isFinite(e) || e < 0 || e > 1)) {
			return `${at}: energy must be 0..=1 (got ${e})`;
		}
		if (section.chords.length === 0) return `${at}: needs at least one chord`;
		if (section.chords.length > MAX_CHORDS_PER_SECTION) return `${at}: at most ${MAX_CHORDS_PER_SECTION} chords`;
		for (const chord of section.chords) {
			const why = chordError(chord);
			if (why) return `${at}: ${why}`;
		}
		bars += section.bars;
		if (bars > MAX_SCORE_BARS) return `a score has at most ${MAX_SCORE_BARS} bars`;
	}
	const duration = scoreDuration(score);
	if (duration > MAX_SCORE_SECONDS) {
		return `the score lasts ${duration.toFixed(0)} s; the longest is ${MAX_SCORE_SECONDS.toFixed(0)} s`;
	}
	const hits = score.hits ?? [];
	if (hits.length > MAX_HITS) return `a score has at most ${MAX_HITS} hits`;
	for (const [i, hit] of hits.entries()) {
		if (!Number.isFinite(hit.at_s) || hit.at_s < 0 || hit.at_s > duration) {
			return `hits[${i}] (${hit.kind}): at_s must be 0..=${duration.toFixed(2)} seconds (got ${hit.at_s})`;
		}
	}
	for (const role of ['drums', 'bass', 'pad', 'keys', 'lead'] as const) {
		const program = score.instruments?.[role];
		if (program !== undefined && (!Number.isInteger(program) || program < 0 || program > 127)) {
			return `instruments.${role}: a General MIDI program is 0..=127`;
		}
	}
	return null;
}

// ---- score to picture (score/align.rs) ---------------------------------------------------

/** The tempo search steps this finely, as a fraction of the tempo (0.05 %). */
const SEARCH_STEP = 0.0005;
/** Hits closer than this to one already in the score are not added again. */
const DUPLICATE_S = 0.05;

export interface AlignOptions {
	/** The most the tempo may change, as a fraction; 0 leaves it alone. */
	max_tempo_change: number;
	/** A cut within this many seconds of a beat is on it. */
	tolerance_s: number;
	/** Add the impacts, dropouts and risers (otherwise only the tempo is tuned). */
	add_hits: boolean;
}

export const DEFAULT_ALIGN: AlignOptions = {
	max_tempo_change: DEFAULT_MAX_TEMPO_CHANGE,
	tolerance_s: ON_BEAT_TOLERANCE_S,
	add_hits: true
};

function read(musicS: number, bpm: number, tolerance: number): BeatReading {
	const period = 60 / bpm;
	const k = Math.round(musicS / period);
	const offset = musicS - k * period;
	const onBeat = Math.abs(offset) <= tolerance;
	return { on_beat: onBeat, on_downbeat: onBeat && (((k % 4) + 4) % 4) === 0, offset_s: offset };
}

/** How well `bpm` fits the cuts: the weighted count of cuts on beats (a downbeat counts 2) and the
 *  total distance of the cuts from their beats (the tie-break). */
function fit(cuts: readonly number[], bpm: number, tolerance: number): [number, number] {
	let weight = 0;
	let distance = 0;
	for (const c of cuts) {
		const r = read(c, bpm, tolerance);
		weight += r.on_downbeat ? 2 : r.on_beat ? 1 : 0;
		distance += Math.abs(r.offset_s);
	}
	return [weight, distance];
}

/** `plan_score_to_picture`: fit `score` to a picture. `cutTimes` are the cuts on the timeline and
 *  `musicStart` where the music starts there; cuts outside the music are ignored. */
export function planScoreToPicture(
	score: MusicScore,
	cutTimes: readonly number[],
	musicStart: number,
	opts: AlignOptions = DEFAULT_ALIGN
): { score: MusicScore; report: AlignReport } {
	const refused = validateScore(score);
	if (refused) throw invalid(refused);
	const duration = scoreDuration(score);
	const cuts = cutTimes
		.filter((t) => Number.isFinite(t))
		.map((t) => [t, t - musicStart] as const)
		.filter(([, m]) => m >= 0 && m <= duration)
		.sort((a, b) => a[1] - b[1]);
	const deduped: (readonly [number, number])[] = [];
	for (const c of cuts) {
		const last = deduped[deduped.length - 1];
		if (!last || Math.abs(c[1] - last[1]) >= 1e-6) deduped.push(c);
	}
	const music = deduped.map((c) => c[1]);
	const tol = Math.max(opts.tolerance_s, 0);
	const notes: string[] = [];

	const before = score.bpm;
	let after = before;
	const max = Math.min(Math.max(opts.max_tempo_change, 0), 0.5);
	if (max > 0 && music.length > 0) {
		const steps = Math.round(max / SEARCH_STEP);
		const candidates: { bpm: number; weight: number; distance: number }[] = [];
		for (let k = -steps; k <= steps; k++) {
			const bpm = before * (1 + k * SEARCH_STEP);
			const [weight, distance] = fit(music, bpm, tol);
			candidates.push({ bpm, weight, distance });
		}
		// The piece may not end before one of the hits it already has: they are absolute seconds.
		const lastHit = (score.hits ?? []).reduce((m, h) => Math.max(m, h.at_s), 0);
		const bars = totalBars(score);
		const durationAt = (bpm: number) => {
			const bar = barSeconds(bpm);
			return bars * bar + Math.min(RELEASE_TAIL_S, 0.9 * bar);
		};
		const valid = (bpm: number) => bpm >= MIN_BPM && bpm <= MAX_BPM && durationAt(bpm) >= lastHit;
		const baseline = fit(music, before, tol)[0];
		const weights = candidates.filter((c) => valid(c.bpm)).map((c) => c.weight);
		const best = weights.length > 0 ? Math.max(...weights) : baseline;
		if (best > baseline) {
			let nearest = -1;
			candidates.forEach((c, i) => {
				if (!valid(c.bpm) || c.weight !== best) return;
				if (nearest < 0) return void (nearest = i);
				const a = Math.abs(c.bpm - before);
				const b = Math.abs(candidates[nearest].bpm - before);
				if (a < b || (a === b && c.bpm < candidates[nearest].bpm)) nearest = i;
			});
			if (nearest >= 0) {
				let lo = nearest;
				let hi = nearest;
				while (lo > 0 && candidates[lo - 1].weight === best && valid(candidates[lo - 1].bpm)) lo--;
				while (hi + 1 < candidates.length && candidates[hi + 1].weight === best && valid(candidates[hi + 1].bpm)) hi++;
				let pick = candidates[lo];
				for (const c of candidates.slice(lo + 1, hi + 1)) {
					const better =
						c.distance < pick.distance ||
						(c.distance === pick.distance && Math.abs(c.bpm - before) < Math.abs(pick.bpm - before));
					if (better) pick = c;
				}
				after = pick.bpm;
			}
		}
		if (Math.abs(after - before) < 1e-12 && best <= baseline) {
			notes.push('the tempo already fits the cuts as well as any within the limit');
		}
	} else if (music.length === 0) {
		notes.push('no cut falls inside the music: nothing to align');
	}

	const aligned: MusicScore = { ...structuredClone(score), bpm: after };
	const period = 60 / after;
	const hitsAdded: ScoreHit[] = [];
	if (opts.add_hits) {
		const hits = [...(aligned.hits ?? [])];
		const existing = (kind: HitKind, at: number) => hits.some((h) => h.kind === kind && Math.abs(h.at_s - at) < DUPLICATE_S);
		let skipped = 0;
		// Add `hit` unless the score is full.
		const add = (hit: ScoreHit) => {
			if (hits.length >= MAX_HITS) {
				skipped += 1;
				return;
			}
			hits.push(hit);
			hitsAdded.push(hit);
		};
		const spans = sectionSpans(aligned);
		const total = scoreDuration(aligned);
		for (const m of music) {
			if (!read(m, after, tol).on_downbeat) continue;
			const at = Math.round(m / (4 * period)) * 4 * period;
			if (at <= total && !existing('impact', at)) add({ at_s: at, kind: 'impact' });
		}
		spans.forEach((span, i) => {
			if (span.kind !== 'drop' || span.start_s <= period) return;
			if (!music.some((m) => Math.abs(m - span.start_s) <= period)) return;
			const afterBuild = i > 0 && spans[i - 1].kind === 'build';
			const dropoutAt = span.start_s - period;
			if (!existing('dropout', dropoutAt)) add({ at_s: dropoutAt, kind: 'dropout' });
			if (!afterBuild && !existing('riser', span.start_s) && span.start_s >= barSeconds(after) * 0.5) {
				add({ at_s: span.start_s, kind: 'riser' });
			}
		});
		if (skipped > 0) notes.push(`the score is at its limit of ${MAX_HITS} hits: ${skipped} hit(s) were not added`);
		hits.sort((a, b) => a.at_s - b.at_s);
		if (hits.length > 0) aligned.hits = hits;
	}
	// Whatever was done to it, what is handed back is a score the renderer accepts.
	const left = validateScore(aligned);
	if (left) throw invalid(left);

	const details: CutAlignment[] = deduped.map(([cut, m]) => ({
		cut_s: cut,
		music_s: m,
		before: read(m, before, tol),
		after: read(m, after, tol)
	}));
	const count = (f: (d: CutAlignment) => boolean) => details.filter(f).length;
	return {
		score: aligned,
		report: {
			cuts: details.length,
			bpm_before: before,
			bpm_after: after,
			tempo_change: after / before - 1,
			on_beat_before: count((d) => d.before.on_beat),
			on_downbeat_before: count((d) => d.before.on_downbeat),
			on_beat_after: count((d) => d.after.on_beat),
			on_downbeat_after: count((d) => d.after.on_downbeat),
			hits_added: hitsAdded,
			details,
			notes
		}
	};
}
