// The pure side of the Compose music dialog: the form a score is written in (a chord progression is
// one line of text, a number box can be empty), how it becomes a `ComposeRequest`, what is wrong with
// it as it is typed (the engine's own refusals, from `music-score.ts`), and the words around a run —
// stages, what fitting to the picture did, the toast after. Pure, so the dialog and the Inspector say
// the same things and the rules are bun-tested (`compose-form.test.ts`).

import { fmtDuration } from './media-info';
import { approxMB } from './voiceover';
import {
	DEFAULT_ENERGY,
	DEFAULT_MAX_TEMPO_CHANGE,
	MAX_BPM,
	MAX_CHORDS_PER_SECTION,
	MAX_SECTIONS,
	MAX_SEED,
	MAX_SECTION_BARS,
	MIN_BPM,
	MUSIC_STYLES,
	RELEASE_TAIL_S,
	barSeconds,
	chordError,
	keyError,
	scoreDuration,
	scoreTitle,
	sectionLabels,
	sectionSpans,
	totalBars,
	validateScore
} from './music-score';
import { clipDuration } from './types';
import type {
	AlignReport,
	Asset,
	Clip,
	ComposeRequest,
	HitKind,
	InstrumentOverrides,
	MusicProgress,
	MusicScore,
	MusicStatus,
	MusicStyle,
	ScoreHit,
	SectionKind,
	Timeline,
	Track
} from './types';

// ---- chords as text ---------------------------------------------------------------------

/** Bar lines, commas and semicolons separate chords as well as spaces do (`C | G | Am | F`). */
const SEPARATORS = /[\s,;|]+/;
/** Written between chords as an arrow or a dash (`I - V - vi - IV`): not chords. */
const ARROWS = new Set(['-', '–', '—', '→', '>', '->', '·']);

/** `I V vi IV` or `C | G | Am | F` → one chord per bar. */
export function parseChords(text: string): string[] {
	return text.split(SEPARATORS).filter((t) => t !== '' && !ARROWS.has(t));
}

/** The text a section's chords are edited as. */
export function chordsText(chords: readonly string[]): string {
	return chords.join(' ');
}

// ---- the form ---------------------------------------------------------------------------

/** One section as it is edited: a number box can be empty, the chords are one line of text. */
export interface FormSection {
	/** Identity for the list (reordering keeps a row's focus), not part of the score. */
	id: number;
	kind: SectionKind;
	bars: number | null;
	chords: string;
	/** `null`: by kind. */
	energy: number | null;
}

export interface ComposeForm {
	style: MusicStyle;
	bpm: number | null;
	key: string;
	sections: FormSection[];
	/** `null`: 0. */
	seed: number | null;
	/** Kept from the score being edited (or from an earlier alignment): the form shows how many and can clear them. */
	hits: ScoreHit[];
	/** Kept from the score being edited; the dialog does not edit them. */
	instruments?: InstrumentOverrides;
	align: boolean;
	alignTo: 'cuts' | 'markers';
	/** The video track whose cuts it is fitted to; `null`: the first with any. */
	alignTrack: string | null;
	/** A fraction: 0.06 is 6 %. */
	maxTempo: number;
}

/** The most the engine lets the tempo move to fit a picture. */
export const MAX_TEMPO_LIMIT = 0.25;

let sectionIds = 0;
const nextId = () => ++sectionIds;

export function defaultBpm(style: MusicStyle): number {
	return MUSIC_STYLES.find((s) => s.name === style)?.default_bpm ?? 110;
}

export function styleLabel(style: MusicStyle): string {
	return MUSIC_STYLES.find((s) => s.name === style)?.label ?? style;
}

const section = (kind: SectionKind, bars: number, chords: string, energy: number | null = null): FormSection => ({
	id: nextId(),
	kind,
	bars,
	chords,
	energy
});

/** A starting point worth hearing: a short arc in C with the usual pop progression. */
export function defaultForm(style: MusicStyle = 'tropical_house'): ComposeForm {
	return {
		style,
		bpm: defaultBpm(style),
		key: 'C',
		sections: [
			section('intro', 4, 'I V vi IV'),
			section('build', 4, 'vi IV I V'),
			section('drop', 8, 'I V vi IV'),
			section('breakdown', 4, 'vi IV I V'),
			section('outro', 4, 'I')
		],
		seed: 0,
		hits: [],
		align: false,
		alignTo: 'cuts',
		alignTrack: null,
		maxTempo: DEFAULT_MAX_TEMPO_CHANGE
	};
}

/** The form for editing `score` again. */
export function formFromScore(score: MusicScore): ComposeForm {
	return {
		style: score.style,
		bpm: score.bpm,
		key: score.key,
		sections: score.sections.map((s) => ({
			id: nextId(),
			kind: s.kind,
			bars: s.bars,
			chords: chordsText(s.chords),
			energy: s.energy ?? null
		})),
		seed: score.seed,
		hits: (score.hits ?? []).map((h) => ({ ...h })),
		...(score.instruments ? { instruments: { ...score.instruments } } : {}),
		align: false,
		alignTo: 'cuts',
		alignTrack: null,
		maxTempo: DEFAULT_MAX_TEMPO_CHANGE
	};
}

/** The score the form says. Numbers left empty or half-typed come out as NaN, which `validateScore` refuses. */
export function formToScore(form: ComposeForm): MusicScore {
	return {
		bpm: form.bpm ?? NaN,
		key: form.key.trim(),
		style: form.style,
		sections: form.sections.map((s) => ({
			kind: s.kind,
			bars: s.bars ?? NaN,
			chords: parseChords(s.chords),
			...(s.energy !== null ? { energy: s.energy } : {})
		})),
		...(form.hits.length > 0 ? { hits: form.hits.map((h) => ({ ...h })) } : {}),
		seed: form.seed ?? 0,
		...(form.instruments ? { instruments: { ...form.instruments } } : {})
	};
}

/** Pick a style. The tempo follows it unless it was set by hand: only a tempo still at the old style's default moves. */
export function setStyle(form: ComposeForm, style: MusicStyle): ComposeForm {
	if (style === form.style) return form;
	const untouched = form.bpm === null || form.bpm === defaultBpm(form.style);
	return { ...form, style, bpm: untouched ? defaultBpm(style) : form.bpm };
}

/** What follows a kind of section when one is added after it. */
const NEXT_KIND: Record<SectionKind, SectionKind> = {
	intro: 'verse',
	verse: 'build',
	build: 'drop',
	drop: 'breakdown',
	breakdown: 'drop',
	outro: 'outro'
};

/** A new section after the last: the kind that usually follows it, four bars, the same chords. */
export function addSection(form: ComposeForm): ComposeForm {
	if (form.sections.length >= MAX_SECTIONS) return form;
	const last = form.sections[form.sections.length - 1];
	const added = last ? section(NEXT_KIND[last.kind], 4, last.chords) : section('verse', 4, 'I V vi IV');
	return { ...form, sections: [...form.sections, added] };
}

export function removeSection(form: ComposeForm, id: number): ComposeForm {
	return { ...form, sections: form.sections.filter((s) => s.id !== id) };
}

/** Move a section one place up (`-1`) or down (`1`); at an end it stays. */
export function moveSection(form: ComposeForm, id: number, dir: -1 | 1): ComposeForm {
	const i = form.sections.findIndex((s) => s.id === id);
	const j = i + dir;
	if (i < 0 || j < 0 || j >= form.sections.length) return form;
	const sections = [...form.sections];
	[sections[i], sections[j]] = [sections[j], sections[i]];
	return { ...form, sections };
}

/** Another take: the variation number picks the humanisation and the hook, so the same score renders differently. */
export function rerollSeed(form: ComposeForm, rand: () => number = Math.random): ComposeForm {
	const pick = Math.floor(rand() * 100000);
	const current = form.seed ?? 0;
	return { ...form, seed: pick === current ? (pick + 1) % 100000 : pick };
}

// ---- what is wrong with it ---------------------------------------------------------------

export interface SectionIssues {
	bars: string | null;
	chords: string | null;
	energy: string | null;
}

export interface FormIssues {
	bpm: string | null;
	key: string | null;
	seed: string | null;
	sections: SectionIssues[];
	/** What the engine would refuse that no one field explains: no sections, too long, a hit past the end. */
	general: string | null;
	/** Anything at all. */
	any: boolean;
}

const wholeNumber = (n: number | null): n is number => n !== null && Number.isInteger(n);

/** The problems with the form as it stands: per field where one field is at fault (the chord and key texts are the
 *  engine's own), and `general` for what only the whole score can say. */
export function formIssues(form: ComposeForm): FormIssues {
	const bpm =
		form.bpm !== null && Number.isFinite(form.bpm) && form.bpm >= MIN_BPM && form.bpm <= MAX_BPM
			? null
			: `Tempo is ${MIN_BPM} to ${MAX_BPM} BPM`;
	const key = form.key.trim() === '' ? 'Enter a key: a tonic and an optional m, like G, F#m, Bb or Ebm' : keyError(form.key);
	const seed =
		form.seed === null || (Number.isInteger(form.seed) && form.seed >= 0 && form.seed <= MAX_SEED)
			? null
			: `Variation is a whole number from 0 to ${MAX_SEED}`;
	const sections = form.sections.map((s): SectionIssues => {
		const chords = parseChords(s.chords);
		let chordsIssue: string | null = null;
		if (chords.length === 0) chordsIssue = 'Add at least one chord';
		else if (chords.length > MAX_CHORDS_PER_SECTION) chordsIssue = `At most ${MAX_CHORDS_PER_SECTION} chords`;
		else chordsIssue = chords.map(chordError).find((e) => e !== null) ?? null;
		return {
			bars:
				wholeNumber(s.bars) && s.bars >= 1 && s.bars <= MAX_SECTION_BARS
					? null
					: `Bars is a whole number from 1 to ${MAX_SECTION_BARS}`,
			chords: chordsIssue,
			energy: s.energy === null || (Number.isFinite(s.energy) && s.energy >= 0 && s.energy <= 1) ? null : 'Energy is 0 to 1'
		};
	});
	const fieldWrong = !!(bpm || key || seed) || sections.some((s) => s.bars || s.chords || s.energy);
	const general = fieldWrong ? null : validateScore(formToScore(form));
	return { bpm, key, seed, sections, general, any: fieldWrong || general !== null };
}

/** The first thing wrong, as one line for a disabled button's reason (fields first, in the order they are drawn). */
export function firstIssue(issues: FormIssues): string | null {
	const sections = issues.sections.flatMap((s, i) =>
		[s.bars, s.chords, s.energy].filter((m): m is string => m !== null).map((m) => `Section ${i + 1}: ${m}`)
	);
	return issues.bpm ?? issues.key ?? issues.seed ?? sections[0] ?? issues.general;
}

/** What the piece will run, from the form: the whole bars that are filled in and the tempo, tolerant of a number
 *  half-typed (it counts what it can). `seconds` is `null` until the tempo is one. */
export function formLength(form: ComposeForm): { bars: number; seconds: number | null } {
	const bars = form.sections.reduce((n, s) => n + (wholeNumber(s.bars) && s.bars > 0 ? s.bars : 0), 0);
	const bpm = form.bpm;
	if (bpm === null || !Number.isFinite(bpm) || bpm <= 0) return { bars, seconds: null };
	const bar = barSeconds(bpm);
	return { bars, seconds: bars * bar + Math.min(RELEASE_TAIL_S, 0.9 * bar) };
}

/** When each section starts, seconds (`null` while the tempo is not a number). */
export function sectionStarts(form: ComposeForm): (number | null)[] {
	const bpm = form.bpm;
	const bar = bpm !== null && Number.isFinite(bpm) && bpm > 0 ? barSeconds(bpm) : null;
	let at = 0;
	return form.sections.map((s) => {
		const start = bar === null ? null : at * bar;
		at += wholeNumber(s.bars) && s.bars > 0 ? s.bars : 0;
		return start;
	});
}

/** `1:05`: where a section starts, in the gutter of the section list. */
export function fmtClock(seconds: number): string {
	const s = Math.max(0, Math.floor(seconds + 1e-6));
	return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
}

/** `52 s` under a minute and a half, else `1:52`: how long a piece is, in the dialog's summary. */
export function fmtLength(seconds: number): string {
	return seconds < 90 ? `${Math.round(seconds)} s` : fmtDuration(seconds);
}

// ---- aligning to the picture --------------------------------------------------------------

export interface AlignChoices {
	/** Every video track and the cuts in it (the boundaries between its clips). */
	tracks: { id: string; name: string; cuts: number }[];
	markers: number;
}

/** The cuts of a video track: the boundaries *between* its clips, the picture's first start and last end not counted
 *  (`cut_times` in `project/music.rs`). */
export function cutCount(track: Pick<Track, 'clips'>): number {
	const edges = track.clips
		.flatMap((c) => [c.timeline_start, c.timeline_start + clipDuration(c)])
		.sort((a, b) => a - b)
		.filter((t, i, all) => i === 0 || Math.abs(t - all[i - 1]) >= 1e-6);
	return Math.max(edges.length - 2, 0);
}

export function alignChoices(timeline: Pick<Timeline, 'tracks' | 'markers'>): AlignChoices {
	return {
		tracks: timeline.tracks.filter((t) => t.kind === 'video').map((t) => ({ id: t.id, name: t.name, cuts: cutCount(t) })),
		markers: timeline.markers?.length ?? 0
	};
}

/** The video track the cuts are read from: the one chosen if it is still there, else the first with a cut. */
export function alignTrackId(choices: AlignChoices, form: Pick<ComposeForm, 'alignTrack'>): string | null {
	const chosen = choices.tracks.find((t) => t.id === form.alignTrack);
	return (chosen ?? choices.tracks.find((t) => t.cuts > 0))?.id ?? null;
}

/** Why aligning cannot be done right now, or null. */
export function alignProblem(choices: AlignChoices, form: Pick<ComposeForm, 'align' | 'alignTo' | 'alignTrack'>): string | null {
	if (!form.align) return null;
	if (form.alignTo === 'markers') return choices.markers > 0 ? null : 'The cut has no markers to land on';
	if (choices.tracks.length === 0) return 'There is no video track to take cuts from';
	const id = alignTrackId(choices, form);
	const track = choices.tracks.find((t) => t.id === id);
	return track && track.cuts > 0 ? null : 'The video track has no cuts: it holds a single clip';
}

/** Whether aligning is on offer at all: there is a cut or a marker to land on. */
export function alignAvailable(choices: AlignChoices): boolean {
	return choices.markers > 0 || choices.tracks.some((t) => t.cuts > 0);
}

// ---- the request --------------------------------------------------------------------------

/** What the dialog is opened with to edit a piece again. */
export interface ComposePrefill {
	score: MusicScore;
	/** The clip the new render replaces, else a clip is added. */
	replaceClipId?: string;
}

export interface ComposePlace {
	/** Replace this composed clip (it keeps its place and volume). */
	replaceClipId?: string | null;
	/** Where a new clip starts, timeline seconds; absent, after what the M track holds. */
	at?: number | null;
}

export function composeRequest(form: ComposeForm, choices: AlignChoices, place: ComposePlace = {}): ComposeRequest {
	const request: ComposeRequest = { score: formToScore(form) };
	if (place.replaceClipId) request.replace_clip_id = place.replaceClipId;
	else if (place.at !== undefined && place.at !== null) request.at = place.at;
	if (form.align) {
		const track = form.alignTo === 'cuts' ? alignTrackId(choices, form) : null;
		request.align = {
			to: form.alignTo,
			...(track ? { track_id: track } : {}),
			max_tempo_change: Math.min(Math.max(form.maxTempo, 0), MAX_TEMPO_LIMIT)
		};
	}
	return request;
}

// ---- words --------------------------------------------------------------------------------

const STAGES: Record<MusicProgress['stage'], string> = {
	download_soundfont: 'Downloading the SoundFont…',
	waiting: 'Waiting for the machine…',
	render: 'Rendering the music…'
};

export function musicStageLabel(stage: MusicProgress['stage']): string {
	return STAGES[stage] ?? 'Working…';
}

/** What the first composition downloads, said before it is asked; `null` once the SoundFont is on disk. */
export function soundfontNote(status: Pick<MusicStatus, 'soundfont_ready' | 'approx_download_bytes'>): string | null {
	if (status.soundfont_ready) return null;
	return `The first piece downloads the SoundFont it is played on (${approxMB(status.approx_download_bytes)}). After that it renders on this machine, offline.`;
}

/** What a rejection says when the user stopped the run (`MUSIC_CANCELLED` in `api.ts`). */
export function isMusicCancelled(e: unknown): boolean {
	return (e instanceof Error ? e.message : String(e)) === 'music cancelled';
}

/** Whether the piece replaced any clip: `Composed.replaced` is the ids of the clips it replaced, empty when one was added. */
export function wasReplaced(replaced: readonly string[]): boolean {
	return replaced.length > 0;
}

/** `110` or `113.2`: a tempo to a tenth, without a trailing `.0`. */
export function fmtBpm(bpm: number): string {
	return String(Math.round(bpm * 10) / 10);
}

const HIT_NAMES: Record<HitKind, [string, string]> = {
	impact: ['impact', 'impacts'],
	riser: ['riser', 'risers'],
	dropout: ['dropout', 'dropouts'],
	whoosh: ['whoosh', 'whooshes']
};

/** `2 impacts, 1 dropout`, in a fixed order; `none` for no hits. */
export function hitsLine(hits: readonly Pick<ScoreHit, 'kind'>[]): string {
	const parts = (Object.keys(HIT_NAMES) as HitKind[])
		.map((k) => [k, hits.filter((h) => h.kind === k).length] as const)
		.filter(([, n]) => n > 0)
		.map(([k, n]) => `${n} ${HIT_NAMES[k][n === 1 ? 0 : 1]}`);
	return parts.length > 0 ? parts.join(', ') : 'none';
}

export interface ReportRow {
	label: string;
	value: string;
}

/** What fitting to the picture did, as rows for the dialog's result: cuts on a beat and on a downbeat before → after,
 *  the tempo and the hits written in. */
export function alignRows(r: AlignReport): ReportRow[] {
	const of = ` of ${r.cuts}`;
	const change = r.tempo_change * 100;
	const tempo =
		Math.abs(r.bpm_after - r.bpm_before) < 0.05
			? `${fmtBpm(r.bpm_before)} BPM, unchanged`
			: `${fmtBpm(r.bpm_before)} → ${fmtBpm(r.bpm_after)} BPM (${change > 0 ? '+' : '−'}${Math.abs(change).toFixed(1)} %)`;
	return [
		{ label: 'Cuts on a beat', value: `${r.on_beat_before} → ${r.on_beat_after}${of}` },
		{ label: 'Cuts on a downbeat', value: `${r.on_downbeat_before} → ${r.on_downbeat_after}${of}` },
		{ label: 'Tempo', value: tempo },
		{ label: 'Hits added', value: hitsLine(r.hits_added) }
	];
}

/** A line for a score: `Lo-fi · Am · 82 BPM · 5 sections · 24 bars · 0:52`. */
export function describeScore(score: MusicScore): string {
	const n = score.sections.length;
	return `${scoreTitle(score)} · ${n} section${n === 1 ? '' : 's'} · ${totalBars(score)} bars · ${fmtLength(scoreDuration(score))}`;
}

/** The toast after a piece lands. */
export function describeComposed(done: { score: MusicScore; replaced: readonly string[] }): string {
	return `${wasReplaced(done.replaced) ? 'Music re-composed' : 'Music added'}: ${scoreTitle(done.score)} · ${fmtLength(scoreDuration(done.score))}`;
}

/** Where the playhead goes to show a clip that has just landed: its start, unless the playhead is already inside it. */
export function revealAt(playhead: number, clipStart: number, clipLength: number): number | null {
	return playhead >= clipStart && playhead < clipStart + clipLength ? null : clipStart;
}

/** Where the M track ends (the audio track named `M` that composed music lands on), 0 when there is none. */
export function musicTrackEnd(timeline: Pick<Timeline, 'tracks'>): number {
	const track = timeline.tracks.find((t) => t.kind === 'audio' && t.name === 'M');
	return track ? Math.max(0, ...track.clips.map((c) => c.timeline_start + clipDuration(c))) : 0;
}

// ---- the clip's gate and the section contour -------------------------------------------------

export interface ComposeOffer {
	/** The clip is composed music: its asset carries a score. */
	show: boolean;
	/** Why editing it is off right now. */
	reason: string | null;
}

/** Whether a clip offers *Edit & re-compose*, and what the core refuses when it cannot be used right now (`music_bed`
 *  in `project/music.rs`): a locked track, or a piece still linked to a picture. */
export function composeOffer(track: Track | undefined, asset: Pick<Asset, 'music_score'> | undefined, clip: Pick<Clip, 'asset_id'>): ComposeOffer {
	if (track?.kind !== 'audio' || !asset?.music_score) return { show: false, reason: null };
	if (track.locked) return { show: true, reason: `Track ${track.name} is locked` };
	if (musicBed(track, clip).some((c) => c.link_id)) return { show: true, reason: 'Unlink the music from its picture first' };
	return { show: true, reason: null };
}

/** The clips of a track that play the same piece as `clip`: re-composing one replaces them all (the segments
 *  Fit to video made of one piece become one clip of the new render). */
export function musicBed(track: Pick<Track, 'clips'>, clip: Pick<Clip, 'asset_id'>): Clip[] {
	return track.clips.filter((c) => c.asset_id === clip.asset_id);
}

/** What the dialog says it will replace: the one clip, or the whole bed it is a segment of. `at` is where it starts. */
export function replaceNote(bed: number, trackName: string, at: string): string {
	if (bed <= 1) return `Replaces the clip on ${trackName} at ${at}. It keeps its place and level; its length becomes the new piece’s.`;
	return `Replaces this music on ${trackName}: its ${bed} clips become one clip of the new piece, from ${at}. The first clip’s level and fade-in are kept; undo puts them back.`;
}

export interface ContourBlock {
	kind: SectionKind;
	label: string;
	bars: number;
	/** 0 to 1. */
	energy: number;
	/** Share of the piece's bars, percent. */
	width: number;
	start_s: number;
	end_s: number;
}

/** The sections laid out by length and energy: what the Inspector draws as the shape of the piece. */
export function sectionContour(score: MusicScore): ContourBlock[] {
	const spans = sectionSpans(score);
	const total = spans.reduce((n, s) => n + s.bars, 0) || 1;
	const labels = sectionLabels(spans.map((s) => s.kind));
	return spans.map((s, i) => ({
		kind: s.kind,
		label: labels[i],
		bars: s.bars,
		energy: Math.min(Math.max(s.energy ?? DEFAULT_ENERGY[s.kind], 0), 1),
		width: (s.bars / total) * 100,
		start_s: s.start_s,
		end_s: s.end_s
	}));
}
