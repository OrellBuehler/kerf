import { describe, expect, test } from 'bun:test';
import { MUSIC_CANCELLED } from './api';
import {
	addSection,
	alignAvailable,
	alignChoices,
	alignProblem,
	alignRows,
	alignTrackId,
	chordsText,
	composeOffer,
	composeRequest,
	cutCount,
	defaultBpm,
	defaultForm,
	describeComposed,
	firstIssue,
	fmtBpm,
	fmtClock,
	fmtLength,
	formFromScore,
	formIssues,
	formLength,
	formToScore,
	hitsLine,
	isMusicCancelled,
	moveSection,
	musicBed,
	musicStageLabel,
	musicTrackEnd,
	parseChords,
	removeSection,
	replaceNote,
	rerollSeed,
	revealAt,
	sectionContour,
	sectionStarts,
	setStyle,
	soundfontNote,
	wasReplaced,
	type ComposeForm
} from './compose-form';
import { MAX_SEED, MUSIC_STYLES, scoreDuration, validateScore } from './music-score';
import type { AlignReport, Clip, MusicScore, Timeline, Track } from './types';

const clip = (start: number, length: number, extra: Partial<Clip> = {}): Clip =>
	({ id: `c${start}`, asset_id: 'a', source_in: 0, source_out: length, timeline_start: start, volume: 1, fade_in: 0, fade_out: 0, ...extra }) as Clip;
const track = (kind: 'video' | 'audio', name: string, clips: Clip[], extra: Partial<Track> = {}): Track =>
	({ id: `t-${name}`, kind, name, clips, ...extra }) as Track;
const timeline = (tracks: Track[], markers: number[] = []): Pick<Timeline, 'tracks' | 'markers'> => ({
	tracks,
	markers: markers.map((time, i) => ({ id: `m${i}`, time, name: `m${i}` }))
});

/** The form of the dialog's first piece, typed into by a person: valid until a test breaks it. */
const form = (): ComposeForm => defaultForm();

describe('chords as text', () => {
	test('a progression is split on spaces, commas and bar lines', () => {
		expect(parseChords('I V vi IV')).toEqual(['I', 'V', 'vi', 'IV']);
		expect(parseChords('C G Am F')).toEqual(['C', 'G', 'Am', 'F']);
		expect(parseChords('C | G | Am | F')).toEqual(['C', 'G', 'Am', 'F']);
		expect(parseChords('  C,G ;  Am\tF\n')).toEqual(['C', 'G', 'Am', 'F']);
		expect(parseChords('F#m7 D/F# Gsus4 bVII')).toEqual(['F#m7', 'D/F#', 'Gsus4', 'bVII']);
	});

	test('arrows and dashes written between chords are not chords', () => {
		expect(parseChords('I - V - vi - IV')).toEqual(['I', 'V', 'vi', 'IV']);
		expect(parseChords('C → G → Am')).toEqual(['C', 'G', 'Am']);
		expect(parseChords('Am7 - Dm')).toEqual(['Am7', 'Dm']);
	});

	test('nothing is nothing', () => {
		expect(parseChords('')).toEqual([]);
		expect(parseChords('  | , ')).toEqual([]);
	});

	test('what the engine stores reads back to the same chords', () => {
		for (const chords of [['I', 'V', 'vi', 'IV'], ['Am'], ['F#m7', 'D/F#'], []]) {
			expect(parseChords(chordsText(chords))).toEqual(chords);
		}
	});
});

describe('the default form', () => {
	test('is a piece the engine accepts', () => {
		expect(formIssues(form()).any).toBe(false);
		expect(validateScore(formToScore(form()))).toBeNull();
	});

	test('opens on the tempo of its style', () => {
		expect(form().bpm).toBe(defaultBpm('tropical_house'));
		expect(defaultForm('lofi').bpm).toBe(80);
	});

	test('does not align until asked', () => {
		const f = form();
		expect(f.align).toBe(false);
		expect(composeRequest(f, alignChoices(timeline([]))).align).toBeUndefined();
	});
});

describe('picking a style', () => {
	test('moves the tempo to the style’s own', () => {
		const f = setStyle(form(), 'ambient');
		expect(f.style).toBe('ambient');
		expect(f.bpm).toBe(68);
	});

	test('every style on offer has a default tempo the engine takes', () => {
		for (const s of MUSIC_STYLES) {
			const f = setStyle(form(), s.name);
			expect(f.bpm).toBe(s.default_bpm);
			expect(formIssues(f).bpm).toBeNull();
		}
	});

	test('leaves a tempo that was set by hand alone', () => {
		const f = setStyle({ ...form(), bpm: 93 }, 'rock');
		expect(f.style).toBe('rock');
		expect(f.bpm).toBe(93);
	});

	test('follows again while the tempo is still the old style’s default, and when it is empty', () => {
		const twice = setStyle(setStyle(form(), 'pop'), 'lofi');
		expect(twice.bpm).toBe(80);
		expect(setStyle({ ...form(), bpm: null }, 'rock').bpm).toBe(124);
	});

	test('picking the same style changes nothing', () => {
		const f = { ...form(), bpm: 99 };
		expect(setStyle(f, f.style)).toBe(f);
	});
});

describe('the section list', () => {
	test('adding follows the last section, with its chords', () => {
		const f = addSection(form());
		const last = f.sections.at(-1)!;
		expect(f.sections.length).toBe(form().sections.length + 1);
		expect(last.kind).toBe('outro');
		expect(last.bars).toBe(4);
		expect(last.chords).toBe(form().sections.at(-1)!.chords);
		const verse = addSection({ ...form(), sections: [{ ...form().sections[0], kind: 'intro' }] });
		expect(verse.sections[1].kind).toBe('verse');
		expect(addSection({ ...form(), sections: [] }).sections[0].kind).toBe('verse');
	});

	test('a new section has an id of its own', () => {
		const f = addSection(form());
		expect(new Set(f.sections.map((s) => s.id)).size).toBe(f.sections.length);
	});

	test('there is no adding past the engine’s limit', () => {
		let f = form();
		for (let i = 0; i < 120; i++) f = addSection(f);
		expect(f.sections.length).toBe(100);
	});

	test('removing and moving go by id and keep the rest in order', () => {
		const f = form();
		const [a, b, c] = f.sections;
		expect(removeSection(f, b.id).sections.map((s) => s.id)).toEqual([a.id, c.id, ...f.sections.slice(3).map((s) => s.id)]);
		expect(moveSection(f, b.id, -1).sections.slice(0, 3).map((s) => s.id)).toEqual([b.id, a.id, c.id]);
		expect(moveSection(f, b.id, 1).sections.slice(0, 3).map((s) => s.id)).toEqual([a.id, c.id, b.id]);
	});

	test('moving past either end stays put', () => {
		const f = form();
		expect(moveSection(f, f.sections[0].id, -1)).toBe(f);
		expect(moveSection(f, f.sections.at(-1)!.id, 1)).toBe(f);
		expect(moveSection(f, -5, 1)).toBe(f);
	});

	test('the originals are not touched', () => {
		const f = form();
		const ids = f.sections.map((s) => s.id);
		moveSection(f, f.sections[1].id, 1);
		removeSection(f, f.sections[0].id);
		addSection(f);
		expect(f.sections.map((s) => s.id)).toEqual(ids);
	});
});

describe('another take', () => {
	test('picks a whole number the engine takes', () => {
		for (const r of [0, 0.5, 0.99999]) {
			const seed = rerollSeed(form(), () => r).seed!;
			expect(Number.isInteger(seed) && seed >= 0).toBe(true);
			expect(formIssues(rerollSeed(form(), () => r)).seed).toBeNull();
		}
	});

	test('is never the number it had', () => {
		expect(rerollSeed({ ...form(), seed: 0 }, () => 0).seed).not.toBe(0);
		expect(rerollSeed({ ...form(), seed: 4242 }, () => 0.04242).seed).not.toBe(4242);
	});
});

describe('the form as a score', () => {
	test('writes the chords as a list and leaves energy out when it is by kind', () => {
		const score = formToScore(form());
		expect(score.sections[0]).toEqual({ kind: 'intro', bars: 4, chords: ['I', 'V', 'vi', 'IV'] });
		expect(score.key).toBe('C');
		expect(score.seed).toBe(0);
		expect('hits' in score).toBe(false);
		expect('instruments' in score).toBe(false);
	});

	test('keeps a stated energy, and trims the key', () => {
		const f = form();
		f.sections[2].energy = 0.8;
		f.key = '  G ';
		const score = formToScore(f);
		expect(score.sections[2].energy).toBe(0.8);
		expect(score.sections[1].energy).toBeUndefined();
		expect(score.key).toBe('G');
	});

	test('an empty number box is a number the engine refuses, not a zero', () => {
		const f = form();
		f.bpm = null;
		f.sections[0].bars = null;
		const score = formToScore(f);
		expect(Number.isNaN(score.bpm)).toBe(true);
		expect(Number.isNaN(score.sections[0].bars)).toBe(true);
		expect(validateScore(score)).not.toBeNull();
	});

	test('editing a score again gives the same score back, hits and instruments kept', () => {
		const score: MusicScore = {
			bpm: 82,
			key: 'Am',
			style: 'lofi',
			sections: [
				{ kind: 'intro', bars: 4, chords: ['i', 'VI', 'III', 'VII'], energy: 0.2 },
				{ kind: 'drop', bars: 8, chords: ['Am', 'F', 'C', 'G'] }
			],
			hits: [
				{ at_s: 11.7, kind: 'impact' },
				{ at_s: 23.4, kind: 'riser' }
			],
			seed: 17,
			instruments: { bass: 38, lead: 80 }
		};
		expect(formToScore(formFromScore(score))).toEqual(score);
	});

	test('the copy is a copy', () => {
		const score: MusicScore = {
			bpm: 100,
			key: 'C',
			style: 'pop',
			sections: [{ kind: 'verse', bars: 4, chords: ['C'] }],
			hits: [{ at_s: 1, kind: 'impact' }],
			seed: 1,
			instruments: { bass: 33 }
		};
		const f = formFromScore(score);
		f.hits[0].at_s = 9;
		f.instruments!.bass = 1;
		expect(score.hits![0].at_s).toBe(1);
		expect(score.instruments!.bass).toBe(33);
	});

	test('opens with no alignment, whatever the score was fitted to', () => {
		const f = formFromScore({ bpm: 100, key: 'C', style: 'pop', sections: [{ kind: 'verse', bars: 4, chords: ['C'] }], seed: 0 });
		expect(f.align).toBe(false);
	});
});

describe('what is wrong with the form', () => {
	test('a field is named where it is at fault', () => {
		const f = form();
		f.bpm = 300;
		f.key = 'H';
		f.sections[1].bars = 0;
		f.sections[2].chords = 'I V Cwtf IV';
		f.sections[3].energy = 1.5;
		const issues = formIssues(f);
		expect(issues.any).toBe(true);
		expect(issues.bpm).toContain('40 to 240');
		expect(issues.key).toContain('is not a key');
		expect(issues.sections[1].bars).toContain('1 to 128');
		expect(issues.sections[2].chords).toContain('"Cwtf" is not a chord');
		expect(issues.sections[3].energy).toBe('Energy is 0 to 1');
		expect(issues.sections[0]).toEqual({ bars: null, chords: null, energy: null });
	});

	test('an empty chord line, a half-typed number and a fractional bar count are refused', () => {
		const f = form();
		f.sections[0].chords = '   ';
		f.sections[1].bars = null;
		f.sections[2].bars = 2.5;
		f.bpm = null;
		const issues = formIssues(f);
		expect(issues.sections[0].chords).toBe('Add at least one chord');
		expect(issues.sections[1].bars).not.toBeNull();
		expect(issues.sections[2].bars).not.toBeNull();
		expect(issues.bpm).not.toBeNull();
	});

	test('an empty key asks for one instead of quoting nothing', () => {
		expect(formIssues({ ...form(), key: '  ' }).key).toBe('Enter a key: a tonic and an optional m, like G, F#m, Bb or Ebm');
		expect(formIssues({ ...form(), key: 'Q' }).key).toContain('"Q" is not a key');
	});

	test('too many chords in one section', () => {
		const f = form();
		f.sections[0].chords = Array.from({ length: 65 }, () => 'C').join(' ');
		expect(formIssues(f).sections[0].chords).toBe('At most 64 chords');
	});

	test('the variation is a whole number from 0 to 2^53 - 1', () => {
		for (const seed of [-1, 1.5, Number.NaN, MAX_SEED + 2]) expect(formIssues({ ...form(), seed }).seed).toBe('Variation is a whole number from 0 to 9007199254740991');
		for (const seed of [0, 7, null, MAX_SEED]) expect(formIssues({ ...form(), seed }).seed).toBeNull();
	});

	test('what only the whole score can say is `general`', () => {
		expect(formIssues({ ...form(), sections: [] }).general).toBe('the score needs at least one section');
		const long = form();
		long.sections[0].bars = 128;
		long.sections[1].bars = 128;
		long.sections[2].bars = 128;
		long.sections[3].bars = 128;
		expect(formIssues(long).general).toBe('a score has at most 400 bars');
		const slow = form();
		slow.bpm = 40;
		slow.sections[0].bars = 100;
		slow.sections[1].bars = 100;
		expect(formIssues(slow).general).toContain('the longest is 600 s');
	});

	test('a hit kept from before that the piece no longer reaches is refused', () => {
		const f = { ...form(), hits: [{ at_s: 500, kind: 'impact' as const }] };
		const issues = formIssues(f);
		expect(issues.general).toContain('hits[0] (impact)');
		expect(issues.any).toBe(true);
	});

	test('a field’s fault is not said twice: `general` waits for the fields', () => {
		const f = form();
		f.sections[0].chords = 'Q';
		const issues = formIssues(f);
		expect(issues.sections[0].chords).not.toBeNull();
		expect(issues.general).toBeNull();
	});

	test('agrees with the engine’s own validation for what it can say', () => {
		const cases: ComposeForm[] = [];
		const base = form();
		cases.push(base);
		cases.push({ ...base, bpm: 39 }, { ...base, bpm: 241 }, { ...base, bpm: 40 }, { ...base, bpm: 240 }, { ...base, bpm: null });
		cases.push({ ...base, key: 'F#m' }, { ...base, key: 'Bb' }, { ...base, key: 'Ebm' }, { ...base, key: '' }, { ...base, key: 'H' });
		cases.push({ ...base, sections: [] });
		for (const chords of ['', 'C', 'Em', 'F#m7', 'Gsus4', 'D/F#', 'ii7', 'bVII', 'Xyz', 'Cm9', 'vI', 'I V Foo']) {
			const f = form();
			f.sections[1].chords = chords;
			cases.push(f);
		}
		for (const bars of [0, 1, 128, 129, 2.5, null]) {
			const f = form();
			f.sections[0].bars = bars;
			cases.push(f);
		}
		for (const seed of [0, 12, MAX_SEED, MAX_SEED + 2, -1, 1.5]) cases.push({ ...base, seed });
		for (const energy of [0, 1, -0.1, 1.1, null]) {
			const f = form();
			f.sections[0].energy = energy;
			cases.push(f);
		}
		for (const [i, c] of cases.entries()) {
			const engine = validateScore(formToScore(c));
			expect(formIssues(c).any, `case ${i}: ${engine}`).toBe(engine !== null);
		}
	});

	test('a message for a disabled button: fields first, in the order they are drawn', () => {
		const f = form();
		expect(firstIssue(formIssues(f))).toBeNull();
		f.sections[3].chords = 'zz';
		f.bpm = 5;
		expect(firstIssue(formIssues(f))).toContain('40 to 240');
		f.bpm = 100;
		expect(firstIssue(formIssues(f))).toMatch(/^Section 4: "zz" is not a chord/);
		expect(firstIssue(formIssues({ ...form(), sections: [] }))).toBe('the score needs at least one section');
	});
});

describe('how long the piece runs', () => {
	test('is the score’s own length for a valid form', () => {
		const f = form();
		const length = formLength(f);
		expect(length.bars).toBe(24);
		expect(length.seconds).toBeCloseTo(scoreDuration(formToScore(f)), 9);
	});

	test('counts what is filled in while a number is half-typed', () => {
		const f = form();
		f.sections[0].bars = null;
		f.sections[1].bars = 2.5;
		expect(formLength(f).bars).toBe(16);
		f.bpm = null;
		expect(formLength(f)).toEqual({ bars: 16, seconds: null });
	});

	test('says where each section starts', () => {
		const f = { ...form(), bpm: 120 };
		expect(sectionStarts(f)).toEqual([0, 8, 16, 32, 40]);
		expect(sectionStarts({ ...f, bpm: null })).toEqual([null, null, null, null, null]);
	});

	test('reads as seconds for a short piece and as a clock for a long one', () => {
		expect(fmtLength(52.4)).toBe('52 s');
		expect(fmtLength(89.4)).toBe('89 s');
		expect(fmtLength(128)).toBe('2:08');
		expect(fmtClock(0)).toBe('0:00');
		expect(fmtClock(65.9)).toBe('1:05');
		expect(fmtClock(599.99999999)).toBe('10:00');
	});
});

describe('fitting to the picture', () => {
	const video = track('video', 'V1', [clip(0, 4), clip(4, 6), clip(10, 5)]);
	const tl = timeline([video, track('audio', 'A1', [clip(0, 15)])], [3, 8]);

	test('a video track’s cuts are the boundaries between its clips', () => {
		expect(cutCount(video)).toBe(2);
		expect(cutCount(track('video', 'V', [clip(0, 10)]))).toBe(0);
		expect(cutCount(track('video', 'V', []))).toBe(0);
		// a gap leaves two edges at the gap, both of them cuts
		expect(cutCount(track('video', 'V', [clip(0, 4), clip(6, 4)]))).toBe(2);
		// two clips meeting at one edge count once; a doubled-up edge does not make a cut of its own
		expect(cutCount(track('video', 'V', [clip(0, 4), clip(4, 4), clip(4, 4)]))).toBe(1);
	});

	test('a clip’s speed is counted in where its boundary falls', () => {
		const fast = track('video', 'V', [clip(0, 8, { speed: 2 }), clip(4, 4)]);
		expect(cutCount(fast)).toBe(1);
	});

	test('the choices are the video tracks and the markers', () => {
		expect(alignChoices(tl)).toEqual({ tracks: [{ id: 't-V1', name: 'V1', cuts: 2 }], markers: 2 });
		expect(alignChoices(timeline([]))).toEqual({ tracks: [], markers: 0 });
		expect(alignAvailable(alignChoices(tl))).toBe(true);
		expect(alignAvailable(alignChoices(timeline([track('video', 'V1', [clip(0, 4)])])))).toBe(false);
		expect(alignAvailable(alignChoices(timeline([], [2])))).toBe(true);
	});

	test('the track is the one chosen if it is still there, else the first with a cut', () => {
		const two = alignChoices(timeline([track('video', 'V1', [clip(0, 4)]), track('video', 'V2', [clip(0, 4), clip(4, 4)])]));
		expect(alignTrackId(two, { alignTrack: null })).toBe('t-V2');
		expect(alignTrackId(two, { alignTrack: 't-V1' })).toBe('t-V1');
		expect(alignTrackId(two, { alignTrack: 'gone' })).toBe('t-V2');
		expect(alignTrackId(alignChoices(timeline([])), { alignTrack: null })).toBeNull();
	});

	test('says why it cannot be done, only when it is on', () => {
		const none = alignChoices(timeline([track('video', 'V1', [clip(0, 4)])]));
		expect(alignProblem(none, { align: false, alignTo: 'cuts', alignTrack: null })).toBeNull();
		expect(alignProblem(none, { align: true, alignTo: 'cuts', alignTrack: null })).toBe('The video track has no cuts: it holds a single clip');
		expect(alignProblem(none, { align: true, alignTo: 'markers', alignTrack: null })).toBe('The cut has no markers to land on');
		expect(alignProblem(alignChoices(timeline([])), { align: true, alignTo: 'cuts', alignTrack: null })).toBe('There is no video track to take cuts from');
		expect(alignProblem(alignChoices(tl), { align: true, alignTo: 'cuts', alignTrack: null })).toBeNull();
		expect(alignProblem(alignChoices(tl), { align: true, alignTo: 'markers', alignTrack: null })).toBeNull();
	});
});

describe('the request', () => {
	const choices = alignChoices(timeline([track('video', 'V1', [clip(0, 4), clip(4, 4)])], [1]));

	test('a plain composition is the score and nothing else', () => {
		const request = composeRequest(form(), choices);
		expect(Object.keys(request)).toEqual(['score']);
		expect(request.score).toEqual(formToScore(form()));
	});

	test('a start on the timeline is sent as `at`', () => {
		expect(composeRequest(form(), choices, { at: 12.5 }).at).toBe(12.5);
		expect(composeRequest(form(), choices, { at: 0 }).at).toBe(0);
		expect('at' in composeRequest(form(), choices, { at: null })).toBe(false);
	});

	test('re-composing names the clip and sends no `at` of its own', () => {
		const request = composeRequest(form(), choices, { replaceClipId: 'clip-1', at: 12.5 });
		expect(request.replace_clip_id).toBe('clip-1');
		expect('at' in request).toBe(false);
		expect('replace_clip_id' in composeRequest(form(), choices, { replaceClipId: null })).toBe(false);
	});

	test('fitting to cuts names the track and the most the tempo may change', () => {
		const f = { ...form(), align: true, maxTempo: 0.1 };
		expect(composeRequest(f, choices).align).toEqual({ to: 'cuts', track_id: 't-V1', max_tempo_change: 0.1 });
	});

	test('fitting to markers names no track', () => {
		const f = { ...form(), align: true, alignTo: 'markers' as const };
		expect(composeRequest(f, choices).align).toEqual({ to: 'markers', max_tempo_change: 0.06 });
	});

	test('the tempo change is held to what the engine accepts', () => {
		expect(composeRequest({ ...form(), align: true, maxTempo: 0.9 }, choices).align?.max_tempo_change).toBe(0.25);
		expect(composeRequest({ ...form(), align: true, maxTempo: -1 }, choices).align?.max_tempo_change).toBe(0);
		expect(composeRequest({ ...form(), align: true, maxTempo: 0 }, choices).align?.max_tempo_change).toBe(0);
	});

	test('the variation travels with the score', () => {
		expect(composeRequest({ ...form(), seed: 31 }, choices).score.seed).toBe(31);
		expect(composeRequest({ ...form(), seed: null }, choices).score.seed).toBe(0);
	});
});

describe('the words', () => {
	test('a stage is named', () => {
		expect(musicStageLabel('download_soundfont')).toBe('Downloading the SoundFont…');
		expect(musicStageLabel('waiting')).toBe('Waiting for the machine…');
		expect(musicStageLabel('render')).toBe('Rendering the music…');
	});

	test('the first piece says what it downloads, a later one says nothing', () => {
		expect(soundfontNote({ soundfont_ready: false, approx_download_bytes: 32319396 })).toContain('~31 MB');
		expect(soundfontNote({ soundfont_ready: true, approx_download_bytes: 0 })).toBeNull();
	});

	test('a stop is the user’s, in either spelling of a rejection, and is what the bridge rejects with', () => {
		expect(isMusicCancelled(new Error('music cancelled'))).toBe(true);
		expect(isMusicCancelled('music cancelled')).toBe(true);
		expect(isMusicCancelled(new Error(MUSIC_CANCELLED))).toBe(true);
		expect(isMusicCancelled(new Error('invalid argument: bpm must be 40..=240'))).toBe(false);
	});

	test('a replaced clip is a list of ids: none is a clip added', () => {
		expect(wasReplaced(['a'])).toBe(true);
		expect(wasReplaced(['a', 'b'])).toBe(true);
		expect(wasReplaced([])).toBe(false);
	});

	test('the toast says whether the piece was added or the clip re-composed', () => {
		const score = formToScore({ ...form(), style: 'lofi', bpm: 82, key: 'Am' });
		expect(describeComposed({ score, replaced: [] })).toMatch(/^Music added: Lo-fi · Am · 82 BPM · \d+ s$/);
		expect(describeComposed({ score, replaced: ['c1'] })).toMatch(/^Music re-composed: Lo-fi · Am · 82 BPM/);
		expect(describeComposed({ score, replaced: ['c1', 'c2'] })).toMatch(/^Music re-composed/);
	});

	test('a tempo is a tenth, without a trailing .0', () => {
		expect(fmtBpm(110)).toBe('110');
		expect(fmtBpm(113.24)).toBe('113.2');
		expect(fmtBpm(113.25)).toBe('113.3');
	});

	test('hits are counted by kind, in a fixed order', () => {
		expect(hitsLine([])).toBe('none');
		expect(hitsLine([{ kind: 'riser' }, { kind: 'impact' }, { kind: 'impact' }, { kind: 'dropout' }])).toBe('2 impacts, 1 riser, 1 dropout');
		expect(hitsLine([{ kind: 'whoosh' }])).toBe('1 whoosh');
		expect(hitsLine([{ kind: 'whoosh' }, { kind: 'whoosh' }])).toBe('2 whooshes');
	});
});

describe('what fitting to the picture did', () => {
	const report = (over: Partial<AlignReport> = {}): AlignReport => ({
		cuts: 12,
		bpm_before: 110,
		bpm_after: 113.2,
		tempo_change: 113.2 / 110 - 1,
		on_beat_before: 3,
		on_downbeat_before: 1,
		on_beat_after: 9,
		on_downbeat_after: 5,
		hits_added: [{ at_s: 8, kind: 'impact' }, { at_s: 16, kind: 'impact' }, { at_s: 23, kind: 'dropout' }],
		details: [],
		notes: [],
		...over
	});

	test('reads the cuts on a beat and on a downbeat, before and after, the tempo and the hits', () => {
		expect(alignRows(report())).toEqual([
			{ label: 'Cuts on a beat', value: '3 → 9 of 12' },
			{ label: 'Cuts on a downbeat', value: '1 → 5 of 12' },
			{ label: 'Tempo', value: '110 → 113.2 BPM (+2.9 %)' },
			{ label: 'Hits added', value: '2 impacts, 1 dropout' }
		]);
	});

	test('a slower tempo has a true minus, an unchanged one says so', () => {
		const slower = alignRows(report({ bpm_after: 105, tempo_change: 105 / 110 - 1 }));
		expect(slower[2].value).toBe('110 → 105 BPM (−4.5 %)');
		const same = alignRows(report({ bpm_after: 110, tempo_change: 0, hits_added: [] }));
		expect(same[2].value).toBe('110 BPM, unchanged');
		expect(same[3].value).toBe('none');
	});
});

describe('the clip’s gate', () => {
	const composed = { music_score: { bpm: 100, key: 'C', style: 'pop' as const, sections: [], seed: 0 } };
	const me = { asset_id: 'a' };

	test('is offered for composed music on an audio track', () => {
		expect(composeOffer(track('audio', 'M', []), composed, me)).toEqual({ show: true, reason: null });
	});

	test('says why a locked track cannot be edited', () => {
		expect(composeOffer(track('audio', 'M', [], { locked: true }), composed, me)).toEqual({ show: true, reason: 'Track M is locked' });
	});

	test('says why a piece still linked to a picture cannot, whichever segment of it is linked', () => {
		const m = track('audio', 'M', [clip(0, 10), clip(10, 10, { link_id: 'L' })]);
		expect(composeOffer(m, composed, me)).toEqual({ show: true, reason: 'Unlink the music from its picture first' });
		const other = track('audio', 'M', [clip(0, 10), clip(10, 10, { asset_id: 'b', link_id: 'L' })]);
		expect(composeOffer(other, composed, me).reason).toBeNull();
	});

	test('is not offered for anything else', () => {
		expect(composeOffer(track('audio', 'A1', []), {}, me).show).toBe(false);
		expect(composeOffer(track('video', 'V1', []), composed, me).show).toBe(false);
		expect(composeOffer(undefined, composed, me).show).toBe(false);
		expect(composeOffer(track('audio', 'M', []), undefined, me).show).toBe(false);
	});
});

describe('what re-composing replaces', () => {
	test('the whole bed: the clips of the track that play the same piece', () => {
		const m = track('audio', 'M', [clip(0, 10, { asset_id: 'x' }), clip(10, 10, { asset_id: 'x' }), clip(20, 10, { asset_id: 'y' })]);
		expect(musicBed(m, { asset_id: 'x' }).map((c) => c.timeline_start)).toEqual([0, 10]);
		expect(musicBed(m, { asset_id: 'y' }).length).toBe(1);
		expect(musicBed(m, { asset_id: 'nope' })).toEqual([]);
	});

	test('says so: one clip keeps its place, a bed becomes one clip', () => {
		expect(replaceNote(1, 'M', '0:12')).toBe('Replaces the clip on M at 0:12. It keeps its place and level; its length becomes the new piece’s.');
		expect(replaceNote(3, 'M', '0:12')).toContain('its 3 clips become one clip of the new piece, from 0:12');
	});
});

describe('where the piece lands', () => {
	test('the playhead goes to the clip unless it is already inside it', () => {
		expect(revealAt(0, 12, 60)).toBe(12);
		expect(revealAt(12, 12, 60)).toBeNull();
		expect(revealAt(40, 12, 60)).toBeNull();
		expect(revealAt(72, 12, 60)).toBe(12);
	});

	test('the M track ends where its last clip does', () => {
		expect(musicTrackEnd(timeline([]))).toBe(0);
		expect(musicTrackEnd(timeline([track('audio', 'A1', [clip(0, 30)])]))).toBe(0);
		expect(musicTrackEnd(timeline([track('audio', 'M', [clip(0, 30), clip(40, 20)])]))).toBe(60);
		expect(musicTrackEnd(timeline([track('video', 'M', [clip(0, 30)])]))).toBe(0);
	});
});

describe('the shape of a piece', () => {
	test('is each section by its length and energy, numbered when a kind repeats', () => {
		const score = formToScore({ ...form(), bpm: 120, sections: [...form().sections, ...form().sections.slice(2, 3)] });
		const blocks = sectionContour(score);
		expect(blocks.map((b) => b.label)).toEqual(['intro', 'build', 'drop 1', 'breakdown', 'outro', 'drop 2']);
		expect(blocks.map((b) => b.bars)).toEqual([4, 4, 8, 4, 4, 8]);
		expect(blocks[2].energy).toBe(1);
		expect(blocks[0].energy).toBe(0.3);
		expect(blocks.reduce((n, b) => n + b.width, 0)).toBeCloseTo(100, 9);
		expect(blocks[1].start_s).toBeCloseTo(8, 9);
		expect(blocks[1].end_s).toBeCloseTo(16, 9);
	});

	test('a stated energy wins over the kind’s', () => {
		const f = form();
		f.sections[0].energy = 0.9;
		expect(sectionContour(formToScore(f))[0].energy).toBe(0.9);
	});
});
