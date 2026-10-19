import { describe, expect, test } from 'bun:test';
import {
	chordError,
	keyError,
	MAX_HITS,
	MAX_SEED,
	planScoreToPicture,
	resolvedChords,
	scoreDuration,
	scoreTitle,
	sectionLabels,
	sectionSpans,
	tonicChord,
	validateScore
} from './music-score';
import type { MusicScore, ScoreSection, SectionKind } from './types';

// The cases are the Rust tests of `model/score.rs`, `score/chord.rs` and `score/align.rs`, replayed
// against the TypeScript mirror.

const section = (kind: SectionKind, bars: number, chords: string[]): ScoreSection => ({ kind, bars, chords });

/** `tests::song`: a small song that uses every kind of section, 120 BPM in A minor. */
function song(): MusicScore {
	return {
		bpm: 120,
		key: 'Am',
		style: 'pop',
		sections: [
			section('intro', 4, ['i', 'VI']),
			section('verse', 8, ['i', 'VI', 'III', 'VII']),
			section('build', 4, ['iv', 'iv', 'V', 'V']),
			section('drop', 8, ['i', 'VI', 'III', 'VII']),
			section('breakdown', 4, ['VI', 'VII']),
			section('drop', 8, ['i', 'VI', 'III', 'VII']),
			section('outro', 4, ['i', 'VI', 'iv', 'V'])
		],
		seed: 7
	};
}

describe('the score', () => {
	test('lasts its bars plus a release tail, never a whole extra bar', () => {
		expect(scoreDuration(song())).toBeCloseTo(40 * 2 + 1.5, 9);
		const fast = { ...song(), bpm: 240 };
		expect(scoreDuration(fast)).toBeCloseTo(40 * 1 + 0.9, 9);
	});

	test('puts its sections in time with their energies', () => {
		const spans = sectionSpans(song());
		expect(spans[1].start_bar).toBe(4);
		expect(spans[1].start_s).toBeCloseTo(8, 9);
		expect(spans[3].energy).toBe(1);
		expect(spans[0].energy).toBe(0.3);
		expect(spans.at(-1)!.end_s).toBeCloseTo(80, 9);
	});

	test('names its sections, numbered when a kind repeats', () => {
		expect(sectionLabels(['intro', 'drop', 'verse', 'drop'])).toEqual(['intro', 'drop 1', 'verse', 'drop 2']);
		expect(scoreTitle({ ...song(), style: 'lofi', bpm: 81.6 })).toBe('Lo-fi · Am · 82 BPM');
	});

	test('a valid one is accepted', () => {
		expect(validateScore(song())).toBeNull();
	});

	test('a bad one is refused naming the section or chord at fault', () => {
		let s = song();
		s.sections[1].chords[2] = 'Hm';
		expect(validateScore(s)).toContain('sections[1] (verse)');
		expect(validateScore(s)).toContain('"Hm"');

		s = song();
		s.sections[2].bars = 0;
		expect(validateScore(s)).toContain('sections[2] (build): bars');

		s = song();
		s.sections[0].chords = [];
		expect(validateScore(s)).toContain('sections[0] (intro): needs at least one chord');

		s = song();
		s.sections[3].energy = 1.5;
		expect(validateScore(s)).toContain('energy');

		s = song();
		s.bpm = 300;
		expect(validateScore(s)).toContain('bpm');
		s.bpm = NaN;
		expect(validateScore(s)).not.toBeNull();

		s = song();
		s.key = 'Z';
		expect(validateScore(s)).toContain('not a key');

		s = song();
		s.sections = [];
		expect(validateScore(s)).toContain('at least one section');

		// A seed must survive a trip through JavaScript: 2^53 - 1 at most, checked after the key and before the sections.
		s = song();
		s.seed = MAX_SEED;
		expect(validateScore(s)).toBeNull();
		s.seed = MAX_SEED + 1;
		expect(validateScore(s)).toBe(
			'seed must be 0..=9007199254740991 (2^53 - 1, so it survives JSON in JavaScript; got 9007199254740992)'
		);
		s.seed = -1;
		expect(validateScore(s)).toContain('seed must be 0..=9007199254740991');
		s.seed = 1.5;
		expect(validateScore(s)).toContain('seed must be 0..=');
		s = { ...song(), sections: [], seed: MAX_SEED + 1 };
		expect(validateScore(s)).toContain('seed must be');
		s = { ...song(), key: 'Z', seed: -1 };
		expect(validateScore(s)).toContain('not a key');

		s = song();
		s.hits = [{ at_s: 9999, kind: 'impact' }];
		expect(validateScore(s)).toContain('hits[0] (impact)');

		s = song();
		s.instruments = { bass: 200 };
		expect(validateScore(s)).toContain('instruments.bass');

		s = song();
		s.sections = [section('drop', 128, ['C']), section('drop', 128, ['C']), section('drop', 128, ['C']), section('drop', 128, ['C'])];
		expect(validateScore(s)).toContain('at most 400 bars');
	});
});

describe('the chords as played', () => {
	test('cycle in each section, and the last bar of an outro resolves on the tonic', () => {
		// song(): the outro is `i VI iv V` in Am — its last bar is written as V and plays the tonic.
		const played = resolvedChords(song());
		expect(played.map((c) => c.length)).toEqual([4, 8, 4, 8, 4, 8, 4]);
		expect(played[0]).toEqual(['i', 'VI', 'i', 'VI']);
		expect(played[1]).toEqual(['i', 'VI', 'III', 'VII', 'i', 'VI', 'III', 'VII']);
		expect(played[6]).toEqual(['i', 'VI', 'iv', 'Am']);
	});

	test('only an outro that ends the piece resolves', () => {
		const middle = { ...song(), sections: [section('outro', 2, ['C', 'G']), section('verse', 2, ['C', 'G'])] };
		expect(resolvedChords(middle).flat()).toEqual(['C', 'G', 'C', 'G']);
		const ends = { ...song(), key: 'G', sections: [section('verse', 2, ['C', 'D']), section('outro', 2, ['C', 'D'])] };
		expect(resolvedChords(ends)[1]).toEqual(['C', 'G']);
	});

	test('the tonic chord is the key as a chord', () => {
		expect(tonicChord('Am')).toBe('Am');
		expect(tonicChord(' F#m ')).toBe('F#m');
		expect(tonicChord('Bb')).toBe('Bb');
		expect(tonicChord('ebm')).toBe('Ebm');
		expect(tonicChord('G')).toBe('G');
	});

	test('a section with no chords or no bars plays nothing', () => {
		const odd = { ...song(), sections: [section('verse', 0, ['C']), section('verse', 2, [])] };
		expect(resolvedChords(odd)).toEqual([[], ['', '']]);
	});
});

describe('keys and chords', () => {
	test('a key is a tonic and an optional m', () => {
		for (const key of ['G', 'F#m', 'Bb', 'Ebm', 'C', 'Am', ' D ']) expect(keyError(key)).toBeNull();
		for (const key of ['Z', '', 'H', 'Gx', '12']) expect(keyError(key)).toContain('not a key');
	});

	test('names and roman numerals are chords', () => {
		const good = [
			'C', 'Em', 'F#m7', 'Gsus4', 'D/F#', 'Bbmaj7', 'E5', 'Cadd9', 'Am7b5', 'G7', 'Dm9',
			'I', 'V', 'vi', 'IV', 'ii7', 'bVII', '#iv', 'V7', 'viidim', 'IVmaj7', 'V/F#'
		];
		for (const chord of good) expect(chordError(chord)).toBeNull();
	});

	test('anything else is refused with a sentence naming it', () => {
		for (const chord of ['Hm', '', 'H', 'Cxyz', 'D/H', 'iV', 'VIII', 'Cmaj13', 'am', 'I/III']) expect(chordError(chord)).not.toBeNull();
		expect(chordError('Cxyz')).toBe('"Cxyz" is not a chord: unknown chord type "xyz"');
		expect(chordError('D/H')).toContain('after the / goes a note name');
		expect(chordError('iV')).toContain('all upper case');
	});
});

describe('score to picture', () => {
	test('a picture already on the grid leaves the tempo alone', () => {
		const { score, report } = planScoreToPicture(song(), [4, 8, 12], 0);
		expect(score.bpm).toBe(120);
		expect(report.cuts).toBe(3);
		expect(report.on_downbeat_before).toBe(3);
		expect(report.on_downbeat_after).toBe(3);
		expect(report.tempo_change).toBe(0);
		expect(report.notes.some((n) => n.includes('already fits'))).toBe(true);
	});

	test('a nudged tempo puts more cuts on beats, and an impact goes on each downbeat cut', () => {
		const cuts = Array.from({ length: 8 }, (_, k) => 2.08 * (k + 1));
		const { score, report } = planScoreToPicture(song(), cuts, 0);
		expect(report.on_beat_before).toBeLessThan(report.on_beat_after);
		expect(report.on_downbeat_after).toBe(8);
		expect(Math.abs(score.bpm - 240 / 2.08)).toBeLessThan(0.2);
		expect(report.tempo_change).toBeLessThan(0);
		expect(report.tempo_change).toBeGreaterThan(-0.06);
		for (const d of report.details) expect(Math.abs(d.after.offset_s)).toBeLessThan(0.012);
		const impacts = score.hits!.filter((h) => h.kind === 'impact').map((h) => h.at_s);
		expect(impacts.length).toBe(8);
		const bar = 240 / score.bpm;
		impacts.forEach((t, k) => expect(Math.abs(t - bar * (k + 1))).toBeLessThan(0.03));
	});

	test('the tempo never moves further than allowed or out of range', () => {
		const cuts = Array.from({ length: 6 }, (_, k) => 2.4 * (k + 1));
		const { score } = planScoreToPicture(song(), cuts, 0);
		expect(Math.abs(score.bpm / 120 - 1)).toBeLessThanOrEqual(0.06 + 1e-9);
		const same = planScoreToPicture(song(), cuts, 0, { max_tempo_change: 0, tolerance_s: 0.04, add_hits: true });
		expect(same.score.bpm).toBe(120);
		const slow = planScoreToPicture({ ...song(), bpm: 41 }, [5, 10, 15], 0);
		expect(slow.score.bpm).toBeGreaterThanOrEqual(40);
	});

	test('the music start shifts the cuts into music time', () => {
		const { report } = planScoreToPicture(song(), [2, 14, 18, 9999], 10);
		expect(report.cuts).toBe(2);
		expect(report.details[0].music_s).toBe(4);
		expect(report.details[0].cut_s).toBe(14);
		expect(report.on_downbeat_after).toBe(2);
	});

	test('a cut on a drop gets a dropout and a riser before it, unless a build leads in', () => {
		let score: MusicScore = { ...song(), sections: [section('intro', 4, ['Am']), section('drop', 4, ['Am'])] };
		let planned = planScoreToPicture(score, [8], 0);
		const kinds = planned.score.hits!.map((h) => `${h.kind}@${h.at_s}`);
		expect(kinds).toContain('impact@8');
		expect(kinds).toContain('dropout@7.5');
		expect(kinds).toContain('riser@8');
		expect(planned.report.hits_added.length).toBe(3);

		score = { ...song(), sections: [section('intro', 2, ['Am']), section('build', 2, ['Am']), section('drop', 4, ['Am'])] };
		planned = planScoreToPicture(score, [8], 0);
		expect(planned.score.hits!.some((h) => h.kind === 'dropout')).toBe(true);
		expect(planned.score.hits!.some((h) => h.kind === 'riser')).toBe(false);

		planned = planScoreToPicture(score, [2], 0);
		expect(planned.score.hits!.every((h) => h.kind === 'impact')).toBe(true);
	});

	test('hits are not added twice, and can be switched off', () => {
		const once = planScoreToPicture(song(), [4], 0);
		const twice = planScoreToPicture(once.score, [4], 0);
		expect(twice.score.hits).toEqual(once.score.hits);
		expect(twice.report.hits_added).toEqual([]);
		const bare = planScoreToPicture(song(), [4], 0, { max_tempo_change: 0.06, tolerance_s: 0.04, add_hits: false });
		expect(bare.score.hits ?? []).toEqual([]);
		expect(bare.report.hits_added).toEqual([]);
	});

	test('a tempo change never ends the piece before a hit it already has', () => {
		const score = song();
		// Cuts a bar apart at 127 BPM: the tempo that fits them is inside the 6 % limit of 120.
		const cuts = Array.from({ length: 12 }, (_, k) => (240 / 127) * (k + 1));
		const free = planScoreToPicture(score, cuts, 0).score;
		expect(free.bpm).toBeGreaterThan(126);
		expect(scoreDuration(free)).toBeLessThan(80);

		// An impact at 80 s: 127 BPM would end the piece (77 s) before it, so a slower fit is chosen
		// and the hit stays where it was, inside the piece.
		score.hits = [{ at_s: 80, kind: 'impact' }];
		const { score: aligned, report } = planScoreToPicture(score, cuts, 0);
		expect(aligned.bpm).toBeLessThanOrEqual(122.4);
		expect(scoreDuration(aligned)).toBeGreaterThanOrEqual(80);
		expect(aligned.hits!.some((h) => h.at_s === 80 && h.kind === 'impact')).toBe(true);
		expect(validateScore(aligned)).toBeNull();
		expect(report.bpm_after).toBe(aligned.bpm);
	});

	test('added hits stop at the limit and the report says so', () => {
		// 254 hits already, none near the downbeats the cuts land on.
		const score: MusicScore = { ...song(), hits: Array.from({ length: 254 }, (_, k) => ({ at_s: 0.3 * (k + 1), kind: 'whoosh' as const })) };
		const opts = { max_tempo_change: 0, tolerance_s: 0.04, add_hits: true };
		const { score: aligned, report } = planScoreToPicture(score, [4, 8, 12, 16], 0, opts);
		expect(aligned.hits!.length).toBe(MAX_HITS);
		expect(report.hits_added.length).toBe(2);
		expect(report.notes.some((n) => n.includes('2 hit(s) were not added'))).toBe(true);
		expect(report.notes).toContain(`the score is at its limit of ${MAX_HITS} hits: 2 hit(s) were not added`);
		expect(validateScore(aligned)).toBeNull();
	});

	test('a score the engine would refuse is refused, not fitted', () => {
		const bad = song();
		bad.sections[1].chords[0] = 'Hm';
		expect(() => planScoreToPicture(bad, [4], 0)).toThrow(/^invalid argument: sections\[1\] \(verse\).*"Hm"/);
		expect(() => planScoreToPicture({ ...song(), bpm: 20 }, [4], 0)).toThrow('invalid argument: bpm must be');
	});

	test('no cuts is a quiet no-op, and the search is deterministic', () => {
		const none = planScoreToPicture(song(), [], 0);
		expect(none.score).toEqual(song());
		expect(none.report.cuts).toBe(0);
		expect(none.report.notes.length).toBeGreaterThan(0);
		const cuts = [1.93, 4.07, 6.2, 8.31, 10.5, 12.44];
		expect(planScoreToPicture(song(), cuts, 0)).toEqual(planScoreToPicture(song(), cuts, 0));
	});
});
