// The browser harness's music: a stand-in for what the analysis pass finds in a song, so the
// bar grid, the ruler's bar ticks and "Fit to video" can be explored under `bun run dev`.
//
// The desktop app derives all of this from the decoded audio (`engine/music.rs`); here it is
// *written down* in the shape the engine produces: a fitted `BeatGrid`, one L2-normalized
// 12-bin chroma per whole bar (a chord progression with a little deterministic wobble, so
// repeated bars are alike rather than identical), and the repeating phrases found in those
// chromas by the same matcher (`phraseMatches`, the faithful mirror of `phrase_matches`).
//
// 60.5 s at 120 BPM in 4/4, the first downbeat 0.52 s in, so 29 whole bars (2 s each) and a
// 1.98 s ending: a four-bar loop, a four-bar bridge nothing else repeats at bars 16-19, then the
// loop again. Any whole loop can be dropped or repeated, so the lengths a fit can reach step by
// four bars (8 s) from 2.5 s + 2 s per bar — 20.5 s, the harness picture, is nine bars: exact —
// and a custom length shows the short and the over cases.

import { phraseMatches, tempoFromGrid } from './music-fit';
import type { AssetAnalysis, BeatGrid, MusicAnalysis } from './types';

export const SAMPLE_MUSIC_DURATION = 60.5;

export const SAMPLE_MUSIC_GRID: BeatGrid = { period_s: 0.5, phase_s: 0.02, downbeat_offset: 1, beats_per_bar: 4 };

/** Pitch classes of the chords, C = 0 … B = 11: root, third, fifth. */
const CHORDS: Record<string, [number, number, number]> = {
	Am: [9, 0, 4],
	F: [5, 9, 0],
	C: [0, 4, 7],
	G: [7, 11, 2]
};

const LOOP = ['Am', 'F', 'C', 'G'];
const BRIDGE = ['F', 'G', 'Am', 'Am'];

/** The chord played in bar `k`: the loop, with the bridge in bars 16-19. */
const chordOfBar = (k: number): string => (k >= 16 && k < 20 ? BRIDGE : LOOP)[k % 4];

/** A stable 0..1 from two integers (a small integer hash), so the harness never flickers. */
function wobble(a: number, b: number): number {
	let h = Math.imul(a + 1, 0x9e3779b1) ^ Math.imul(b + 1, 0x85ebca6b);
	h ^= h >>> 15;
	h = Math.imul(h, 0x2c1b3c6d);
	h ^= h >>> 12;
	return (h >>> 0) / 0xffffffff;
}

/** The chroma of bar `k`: its triad (root strongest) over a faint noise floor, normalized. */
function chromaOfBar(k: number): number[] {
	const [root, third, fifth] = CHORDS[chordOfBar(k)];
	const v = Array.from({ length: 12 }, (_, pc) => 0.04 * wobble(k, pc));
	v[root] += 1;
	v[third] += 0.7;
	v[fifth] += 0.8;
	const norm = Math.sqrt(v.reduce((s, x) => s + x * x, 0));
	return v.map((x) => Math.fround(x / norm));
}

export function sampleMusic(): MusicAnalysis {
	const grid = SAMPLE_MUSIC_GRID;
	const first = grid.phase_s + grid.downbeat_offset * grid.period_s;
	const bars = Math.floor((SAMPLE_MUSIC_DURATION - first + 1e-6) / (grid.period_s * grid.beats_per_bar));
	const bar_chroma = Array.from({ length: bars }, (_, k) => chromaOfBar(k));
	return { grid, duration: SAMPLE_MUSIC_DURATION, bar_chroma, phrases: phraseMatches(bar_chroma) };
}

/** The whole analysis of the harness's song: the tempo is the grid's, as `Tempo::from_grid` makes it. */
export function sampleMusicAnalysis(assetId: string): AssetAnalysis {
	const music = sampleMusic();
	const tempo = tempoFromGrid(music.grid, music.duration, 0.86);
	return {
		asset_id: assetId,
		silence_segments: [],
		scene_changes: [],
		transcript: [],
		loudness: { integrated_lufs: -9.6, loudness_range: 5.2, true_peak_dbtp: -0.4, threshold_lufs: -19.6 },
		onsets: tempo.beats.slice(0, 32),
		tempo,
		audio_class: { class: 'music', confidence: 0.94 },
		music
	};
}
