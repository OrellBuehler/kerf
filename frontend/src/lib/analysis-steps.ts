// What the bin, the inspector and the agent panel know about an asset's analysis, kind by
// kind. Pure, so the phrasing and the rules (what an import runs, what a quick edit still
// needs) are testable and the surfaces cannot drift apart. The backend owns the truth —
// `kerf_core::analysis_status` — and these read what it reports.

import type { AnalysisKind, AnalysisState, AnalysisStatus, AssetAnalysis, AutoAnalysis } from './types';

export interface KindInfo {
	kind: AnalysisKind;
	/** The name in menus and tooltips. */
	label: string;
	/** The two-to-four letter tag on a chip. */
	short: string;
	/** The `AnalysisProgress.stage` the backend reports while it runs. */
	stage: string;
	/** The progress line while it runs. */
	doing: string;
	/** What a menu item says it does. */
	action: string;
}

/** Every kind, in the order a pass runs them. */
export const ANALYSIS_KINDS: readonly KindInfo[] = [
	{ kind: 'silence', label: 'Silence', short: 'SIL', stage: 'silence', doing: 'detecting silence', action: 'Detect silence' },
	{ kind: 'scenes', label: 'Scenes', short: 'SCN', stage: 'scenes', doing: 'detecting scenes', action: 'Find scene changes' },
	{ kind: 'loudness', label: 'Loudness', short: 'LUFS', stage: 'loudness', doing: 'measuring loudness', action: 'Measure loudness' },
	{ kind: 'rhythm', label: 'Rhythm', short: 'BPM', stage: 'rhythm', doing: 'finding the beat', action: 'Find the beat and tempo' },
	{ kind: 'transcript', label: 'Transcript', short: 'TXT', stage: 'transcribe', doing: 'transcribing', action: 'Transcribe speech' }
];

export const ALL_KINDS: readonly AnalysisKind[] = ANALYSIS_KINDS.map((k) => k.kind);

export function kindInfo(kind: AnalysisKind): KindInfo {
	return ANALYSIS_KINDS.find((k) => k.kind === kind)!;
}

/** The kind a progress stage belongs to (`transcribe` is the transcript's), if any. */
export function kindOfStage(stage: string | null | undefined): AnalysisKind | null {
	return ANALYSIS_KINDS.find((k) => k.stage === stage)?.kind ?? null;
}

/** Whether `kind` is done in a cached analysis — `AssetAnalysis::done` in kerf-core: its step
 *  ran (`ran`), or, for an analysis cached before that was recorded, it holds data of that
 *  kind. The browser harness has no backend to ask. */
export function analysisKindDone(analysis: AssetAnalysis | null | undefined, kind: AnalysisKind): boolean {
	if (!analysis) return false;
	if (analysis.ran?.includes(kind)) return true;
	switch (kind) {
		case 'silence':
			return analysis.silence_segments.length > 0;
		case 'scenes':
			return analysis.scene_changes.length > 0;
		case 'loudness':
			return analysis.loudness != null;
		case 'rhythm':
			return analysis.onsets.length > 0 || analysis.tempo != null || analysis.audio_class != null;
		case 'transcript':
			return analysis.transcript.length > 0;
	}
}

const STATE_WORD: Record<AnalysisState, string> = {
	done: 'done',
	not_run: 'not run',
	running: 'running',
	failed: 'failed',
	off: 'off'
};

export function stateWord(state: AnalysisState): string {
	return STATE_WORD[state];
}

/** One chip: a kind and the state to draw it in. */
export interface Chip {
	kind: AnalysisKind;
	label: string;
	short: string;
	state: AnalysisState;
	/** The tooltip: `Silence — done`, `Loudness — failed: ffmpeg exited with 1`. */
	title: string;
}

/** The chips of an asset: one per kind, in pass order. `running` is the kind the page itself
 *  is waiting on (it knows before the backend's next status event does). With no status yet
 *  every kind reads `not run`. */
export function analysisChips(status: AnalysisStatus | null | undefined, running: AnalysisKind | null = null): Chip[] {
	return ANALYSIS_KINDS.map((info) => {
		const found = status?.kinds.find((k) => k.kind === info.kind);
		const state: AnalysisState = running === info.kind && found?.state !== 'done' ? 'running' : (found?.state ?? 'not_run');
		const reason = found?.reason ?? null;
		return {
			kind: info.kind,
			label: info.label,
			short: info.short,
			state,
			title: `${info.label} — ${stateWord(state)}${reason && state !== 'running' ? ` (${reason})` : ''}`
		};
	});
}

/** `3 of 5`, `all 5` or `none`: how much of the analysis has run, short enough for a menu row. */
export function doneCount(status: AnalysisStatus | null | undefined): string {
	const n = (status?.kinds ?? []).filter((k) => k.state === 'done').length;
	return n === 0 ? 'none' : n === ANALYSIS_KINDS.length ? `all ${n}` : `${n} of ${ANALYSIS_KINDS.length}`;
}

/** `silence · scenes · loudness` — what has run, for a tooltip; `none` when nothing has. */
export function doneSummary(status: AnalysisStatus | null | undefined): string {
	const done = (status?.kinds ?? []).filter((k) => k.state === 'done').map((k) => kindInfo(k.kind).label.toLowerCase());
	return done.length === 0 ? 'none' : done.join(' · ');
}

function stateOf(status: AnalysisStatus | null | undefined, kind: AnalysisKind): AnalysisState {
	return status?.kinds.find((k) => k.kind === kind)?.state ?? 'not_run';
}

/**
 * What an import runs on a new asset: the kinds the toggles allow that are not already done
 * (re-importing a file the project holds must not redo its transcription), without the
 * transcript when there is no speech backend. Empty when the master switch is off.
 */
export function autoSteps(
	auto: AutoAnalysis,
	status: AnalysisStatus | null | undefined,
	transcriptionAvailable: boolean
): AnalysisKind[] {
	if (!auto.enabled) return [];
	return ALL_KINDS.filter(
		(kind) =>
			auto[kind] && stateOf(status, kind) !== 'done' && (kind !== 'transcript' || transcriptionAvailable)
	);
}

/** Of the `needed` kinds, those an edit still has to run first: not done (a kind that failed
 *  last time is tried again). A quick edit runs only these, not the whole pass. */
export function missingSteps(status: AnalysisStatus | null | undefined, needed: readonly AnalysisKind[]): AnalysisKind[] {
	return needed.filter((kind) => stateOf(status, kind) !== 'done');
}

/** The kinds the quick edits in the agent panel need analysed first. */
export const QUICK_EDIT_STEPS = {
	/** Remove silences / assemble a rough cut read the silent spans. */
	silences: ['silence'],
	/** Cut to the beat reads the music's beat grid. */
	beat: ['rhythm'],
	/** Caption the cut reads every clip's transcript. */
	captions: ['transcript']
} as const satisfies Record<string, readonly AnalysisKind[]>;

/** One entry of the "Analyze" menu. */
export interface AnalyzeChoice {
	/** `silence` … or `all`. */
	id: AnalysisKind | 'all';
	label: string;
	/** What to run. */
	steps: AnalysisKind[];
	/** The state of the kind it runs (for `all`: `done` when everything is). */
	state: AnalysisState;
	/** Said beside the label: `done`, `failed`, … */
	hint: string;
	disabled: boolean;
	/** Why it is disabled. */
	reason?: string;
}

/**
 * The entries of the Analyze menu for an asset: one per kind, then everything. A done kind
 * is offered again (it re-runs); nothing is offered while a pass is running on the asset, and
 * a voiceover — whose transcript is its script — has no use for any of it.
 */
export function analyzeChoices(
	status: AnalysisStatus | null | undefined,
	opts: { voiceover?: boolean; busy?: boolean; audio?: boolean; transcriptionAvailable?: boolean } = {}
): AnalyzeChoice[] {
	const { voiceover = false, busy = false, audio = true, transcriptionAvailable = true } = opts;
	const why = (kind: AnalysisKind | 'all'): string | undefined => {
		if (voiceover) return 'its transcript is the script it was read from';
		if (busy) return 'an analysis is already running';
		if (!audio && kind !== 'scenes' && kind !== 'all') return 'this file has no audio';
		if (kind === 'transcript' && !transcriptionAvailable) return 'no speech-to-text backend is available';
		return undefined;
	};
	const choices: AnalyzeChoice[] = ANALYSIS_KINDS.map((info) => {
		const state = stateOf(status, info.kind);
		const reason = why(info.kind);
		return {
			id: info.kind,
			label: info.action,
			steps: [info.kind],
			state,
			hint: state === 'done' ? 'done · run again' : state === 'failed' ? 'failed · retry' : '',
			disabled: reason !== undefined,
			reason
		};
	});
	const everything = ANALYSIS_KINDS.filter((k) => transcriptionAvailable || k.kind !== 'transcript').map((k) => k.kind);
	const reason = why('all');
	const allDone = everything.every((k) => stateOf(status, k) === 'done');
	choices.push({
		id: 'all',
		label: 'Analyze everything',
		steps: everything,
		state: allDone ? 'done' : 'not_run',
		hint: allDone ? 'done · run again' : '',
		disabled: reason !== undefined,
		reason
	});
	return choices;
}
