import { describe, expect, test } from 'bun:test';
import {
	ALL_KINDS,
	QUICK_EDIT_STEPS,
	analysisChips,
	analysisKindDone,
	analyzeChoices,
	autoSteps,
	doneCount,
	doneSummary,
	kindOfStage,
	missingSteps,
	stateWord
} from './analysis-steps';
import type { AnalysisKind, AnalysisState, AnalysisStatus, AssetAnalysis, AutoAnalysis } from './types';

const status = (states: Partial<Record<AnalysisKind, AnalysisState | [AnalysisState, string]>>): AnalysisStatus => ({
	asset_id: 'a',
	kinds: ALL_KINDS.map((kind) => {
		const s = states[kind] ?? 'not_run';
		return Array.isArray(s) ? { kind, state: s[0], reason: s[1] } : { kind, state: s };
	})
});

const ALL_ON: AutoAnalysis = { enabled: true, silence: true, scenes: true, loudness: true, rhythm: true, transcript: true };

describe('the chips of an asset', () => {
	test('one per kind, in the order a pass runs them, with the state in the tooltip', () => {
		const chips = analysisChips(
			status({ silence: 'done', loudness: ['failed', 'ffmpeg exited with 1'], transcript: ['off', 'switched off in Settings'] })
		);
		expect(chips.map((c) => c.kind)).toEqual(['silence', 'scenes', 'loudness', 'rhythm', 'transcript']);
		expect(chips.map((c) => c.state)).toEqual(['done', 'not_run', 'failed', 'not_run', 'off']);
		expect(chips[0].title).toBe('Silence — done');
		expect(chips[1].title).toBe('Scenes — not run');
		expect(chips[2].title).toBe('Loudness — failed (ffmpeg exited with 1)');
		expect(chips[4].title).toBe('Transcript — off (switched off in Settings)');
		expect(chips.map((c) => c.short)).toEqual(['SIL', 'SCN', 'LUFS', 'BPM', 'TXT']);
	});

	test('with no status yet every chip reads not run', () => {
		expect(analysisChips(undefined).every((c) => c.state === 'not_run')).toBe(true);
		expect(analysisChips(null)).toHaveLength(5);
	});

	test('the kind the page is waiting on shows running, unless the backend already has it done', () => {
		const chips = analysisChips(status({ silence: 'done' }), 'transcript');
		expect(chips.find((c) => c.kind === 'transcript')?.state).toBe('running');
		expect(chips.find((c) => c.kind === 'transcript')?.title).toBe('Transcript — running');
		expect(analysisChips(status({ silence: 'done' }), 'silence')[0].state).toBe('done');
	});

	test('the one-line summary lists what has run', () => {
		expect(doneSummary(status({}))).toBe('none');
		expect(doneSummary(undefined)).toBe('none');
		expect(doneSummary(status({ silence: 'done', rhythm: 'done', scenes: 'failed' }))).toBe('silence · rhythm');
		expect(doneCount(status({}))).toBe('none');
		expect(doneCount(status({ silence: 'done', rhythm: 'done', scenes: 'failed' }))).toBe('2 of 5');
		expect(doneCount(status(Object.fromEntries(ALL_KINDS.map((k) => [k, 'done']))))).toBe('all 5');
		expect(stateWord('not_run')).toBe('not run');
	});
});

describe('what an import runs', () => {
	test('the kinds the toggles allow that are not already done', () => {
		expect(autoSteps(ALL_ON, undefined, true)).toEqual([...ALL_KINDS]);
		expect(autoSteps({ ...ALL_ON, transcript: false, scenes: false }, undefined, true)).toEqual([
			'silence',
			'loudness',
			'rhythm'
		]);
		// Re-importing a file the project holds redoes nothing that finished…
		expect(autoSteps(ALL_ON, status({ silence: 'done', scenes: 'done', loudness: 'done', rhythm: 'done' }), true)).toEqual([
			'transcript'
		]);
		// …but retries what failed.
		expect(autoSteps(ALL_ON, status({ silence: 'done', scenes: 'done', loudness: 'done', rhythm: 'done', transcript: 'failed' }), true)).toEqual([
			'transcript'
		]);
	});

	test('the master switch off runs nothing, and transcription needs a backend', () => {
		expect(autoSteps({ ...ALL_ON, enabled: false }, undefined, true)).toEqual([]);
		expect(autoSteps(ALL_ON, undefined, false)).toEqual(['silence', 'scenes', 'loudness', 'rhythm']);
	});
});

describe('what a quick edit still has to run', () => {
	test('only the needed kinds that are not done', () => {
		expect(missingSteps(undefined, QUICK_EDIT_STEPS.silences)).toEqual(['silence']);
		expect(missingSteps(status({ silence: 'done' }), QUICK_EDIT_STEPS.silences)).toEqual([]);
		// Removing silences never asks for a transcript, however much else is missing.
		expect(missingSteps(status({}), QUICK_EDIT_STEPS.silences)).not.toContain('transcript');
		expect(missingSteps(status({ silence: 'done', rhythm: 'failed' }), QUICK_EDIT_STEPS.beat)).toEqual(['rhythm']);
		expect(missingSteps(status({ transcript: 'off' }), QUICK_EDIT_STEPS.captions)).toEqual(['transcript']);
		expect(QUICK_EDIT_STEPS.captions).toEqual(['transcript']);
	});
});

describe('the analyze menu', () => {
	test('one choice per kind, then everything', () => {
		const choices = analyzeChoices(status({ silence: 'done', loudness: 'failed' }));
		expect(choices.map((c) => c.id)).toEqual(['silence', 'scenes', 'loudness', 'rhythm', 'transcript', 'all']);
		expect(choices[0].steps).toEqual(['silence']);
		expect(choices[0].hint).toBe('done · run again');
		expect(choices[2].hint).toBe('failed · retry');
		expect(choices[1].hint).toBe('');
		expect(choices[5].steps).toEqual([...ALL_KINDS]);
		expect(choices.every((c) => !c.disabled)).toBe(true);
	});

	test('a voiceover, a running pass, a file with no audio and a missing backend each say why not', () => {
		expect(analyzeChoices(undefined, { voiceover: true }).every((c) => c.disabled && /script/.test(c.reason ?? ''))).toBe(true);
		expect(analyzeChoices(undefined, { busy: true }).every((c) => c.disabled && /already running/.test(c.reason ?? ''))).toBe(true);
		const silent = analyzeChoices(undefined, { audio: false });
		expect(silent.find((c) => c.id === 'scenes')?.disabled).toBe(false);
		expect(silent.find((c) => c.id === 'silence')?.reason).toMatch(/no audio/);
		const noBackend = analyzeChoices(undefined, { transcriptionAvailable: false });
		expect(noBackend.find((c) => c.id === 'transcript')?.disabled).toBe(true);
		// "Everything" leaves out what cannot run rather than failing on it.
		expect(noBackend.find((c) => c.id === 'all')?.steps).not.toContain('transcript');
	});
});

describe('stages and cached data', () => {
	test('a progress stage belongs to a kind; transcription is spelled transcribe', () => {
		expect(kindOfStage('silence')).toBe('silence');
		expect(kindOfStage('transcribe')).toBe('transcript');
		expect(kindOfStage('download_model')).toBeNull();
		expect(kindOfStage(undefined)).toBeNull();
	});

	// Mirrors `AssetAnalysis::done` in kerf-core (the harness has no backend to ask).
	const empty: AssetAnalysis = {
		asset_id: 'a',
		silence_segments: [],
		scene_changes: [],
		transcript: [],
		loudness: null,
		onsets: [],
		tempo: null,
		audio_class: null
	};

	test('a kind is done when its step ran, or — for an older cache — when it has data', () => {
		expect(ALL_KINDS.some((k) => analysisKindDone(empty, k))).toBe(false);
		expect(analysisKindDone(null, 'silence')).toBe(false);
		const legacy = { ...empty, silence_segments: [{ start: 1, end: 2 }] };
		expect(ALL_KINDS.filter((k) => analysisKindDone(legacy, k))).toEqual(['silence']);
		// Ran and found nothing is still done once it is recorded.
		expect(analysisKindDone({ ...empty, ran: ['transcript'] }, 'transcript')).toBe(true);
		expect(analysisKindDone({ ...empty, ran: ['transcript'] }, 'silence')).toBe(false);
		expect(analysisKindDone({ ...empty, audio_class: { class: 'music', confidence: 0.9 } }, 'rhythm')).toBe(true);
	});
});
