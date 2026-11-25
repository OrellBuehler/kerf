// What the Inspector says about a library asset when one is selected and no clip is: the facts the bin's
// right-click menu used to list as twenty rows (ten about the file, ten about its analysis, a few about its
// proxy), which made the menu 960 px tall and left the Inspector saying "Select a clip" with an asset
// selected. Pure, so the lines are tested and the menu can be the actions only.

import { analysisChips, analysisMeter, chipsFor, kindInfo, mediaTraits, type AnalysisMeter } from './analysis-steps';
import { analysisFacts, shortPath, type MediaInfo } from './media-info';
import { proxyFacts } from './proxy-info';
import { fmtSize } from './bin-view';
import { tidyReason } from './failure-copy';
import type { Asset, AnalysisKind, AnalysisState, AnalysisStatus, AssetAnalysis, AssetFile, ProxyStatus } from './types';

export interface Fact {
	label: string;
	value: string;
	/** The whole text, when the value is a shortened one. */
	title?: string;
}

/** One step of the analysis as the Inspector lists it: what it is, where it stands, what it found — or, while it has not
 *  run, what it will give. */
export interface AnalysisRow {
	kind: AnalysisKind;
	label: string;
	state: AnalysisState;
	/** What it found, once it has run (`3 gaps · 4.2 s`); empty before. */
	found: string;
	/** What it gives once it has run, said while it has not. */
	gives: string;
	/** Why it failed or is off, in the backend's words. */
	reason: string | null;
	/** What the row offers: run the step, run it again, or nothing (a run is on, or it cannot be run). */
	action: 'run' | 'retry' | null;
}

export interface AssetFacts {
	/** The file: its length, frame, codec, projection, where it is used. */
	media: Fact[];
	/** How much analysis has run, for the section's summary, in the bin pill's own words: `2 of 3 analyzed`,
	 *  `analyzing…`, `analysis failed`. */
	analyzed: string;
	/** The analysis as one row per step the file can give — the chips and what they found, in one list. */
	rows: AnalysisRow[];
	/** The pill's whole answer, for its tooltip and the lines under the section. */
	meter: AnalysisMeter | null;
	/** The one reason several steps are off for (`Analysis on import is off in Settings`), said once above the rows rather
	 *  than under each; the rows it covered carry none. */
	offReason: string | null;
	/** What analysis found that is not a step's own result (what kind of sound it is). */
	analysis: Fact[];
	/** The preview proxy. Empty for a file that never gets one. */
	proxy: Fact[];
}

/** What a step that ran and found nothing says, where the row would otherwise just say `done`. */
const NOTHING_FOUND: Record<AnalysisKind, string> = {
	silence: 'no gaps found',
	scenes: 'no scene changes',
	loudness: 'measured',
	rhythm: 'no beat found',
	transcript: 'no speech'
};

/** The line under a row that has not run (or failed, or is off): why, in the backend's words, then what the step will give. */
export function rowNote(r: Pick<AnalysisRow, 'state' | 'reason' | 'gives'>): string {
	if (r.state === 'done' || r.state === 'running') return '';
	const lead = r.state === 'failed' ? (r.reason ?? 'It failed') : r.state === 'off' ? r.reason : null;
	return [lead ? `${lead.replace(/[.\s]+$/, '')}.` : '', r.gives].filter(Boolean).join(' ');
}

const FOUND_LABELS: Record<AnalysisKind, readonly string[]> = {
	silence: ['Silence'],
	scenes: ['Scenes'],
	loudness: ['Loudness'],
	rhythm: ['Tempo'],
	transcript: ['Transcript']
};

export function assetFacts(o: {
	asset: Pick<Asset, 'duration' | 'imported_at' | 'path' | 'source_paths' | 'streams'> & Partial<Pick<Asset, 'voiceover'>>;
	info: MediaInfo;
	analysis: AssetAnalysis | null | undefined;
	status: AnalysisStatus | null | undefined;
	/** A pass is running on this asset now. */
	analyzing: boolean;
	/** A pass is running on *any* asset (one at a time: nothing else is offered to run). Defaults to `analyzing`. */
	busy?: boolean;
	/** The step that pass is on, once it has said. */
	running?: AnalysisKind | null;
	/** The asset waits its turn behind the one being analyzed. */
	queued?: boolean;
	/** Whether a speech-to-text backend exists (a transcript row can be run only then). */
	transcription?: boolean;
	proxy: ProxyStatus | null | undefined;
	/** The file on disk: its size and when it was written. */
	file?: AssetFile | null;
}): AssetFacts {
	const { asset, info } = o;
	const media: Fact[] = [{ label: 'Duration', value: info.duration }];
	if (info.resolution)
		media.push({ label: 'Frame', value: info.aspect ? `${info.resolution} · ${info.aspect}` : info.resolution });
	if (info.fps) media.push({ label: 'Rate', value: info.fps });
	if (info.codec) media.push({ label: 'Codec', value: info.codec });
	if (info.audio) media.push({ label: 'Audio', value: info.audio });
	if (info.projection) media.push({ label: 'Projection', value: info.projection });
	if (info.voiceover) media.push({ label: 'Voiceover', value: info.voiceover });
	if (info.stitched)
		media.push({
			label: 'Stitched',
			value: `${asset.source_paths?.length ?? 0} lens files`,
			title: asset.source_paths?.join('\n')
		});
	media.push({ label: 'Used in', value: info.uses === 1 ? '1 clip' : `${info.uses} clips` });
	if (o.file?.size_bytes !== undefined) media.push({ label: 'Size', value: fmtSize(o.file.size_bytes) });
	const modified = o.file?.modified ? new Date(o.file.modified) : null;
	if (modified && !Number.isNaN(modified.getTime()))
		media.push({ label: 'Modified', value: modified.toLocaleDateString(), title: modified.toLocaleString() });
	const imported = new Date(asset.imported_at);
	if (!Number.isNaN(imported.getTime())) media.push({ label: 'Imported', value: imported.toLocaleDateString() });
	media.push({ label: 'Path', value: shortPath(asset.path), title: asset.path });

	const chips = chipsFor(analysisChips(o.status, o.analyzing ? (o.running ?? null) : null), mediaTraits(asset.streams));
	const found = analysisFacts(o.analysis, asset.duration);
	const foundBy = (kind: AnalysisKind) => found.find((f) => FOUND_LABELS[kind].includes(f.label));
	const canRun = !(o.busy ?? o.analyzing) && !asset.voiceover;
	const rows: AnalysisRow[] = chips.map((c) => {
		const info = kindInfo(c.kind);
		const raw = o.status?.kinds.find((k) => k.kind === c.kind)?.reason;
		const reason = raw ? tidyReason(raw) : null;
		const result = foundBy(c.kind);
		const speech = c.kind === 'transcript' && o.transcription === false;
		return {
			kind: c.kind,
			label: info.label,
			state: c.state,
			found: c.state === 'done' ? (result?.value ?? NOTHING_FOUND[c.kind]) : '',
			gives: info.gives,
			reason,
			action: !canRun || c.state === 'running' || speech ? null : c.state === 'failed' ? 'retry' : c.state === 'done' ? null : 'run'
		};
	});
	// Steps that are off for one reason say it once.
	const offReasons = rows.filter((r) => r.state === 'off' && r.reason).map((r) => r.reason!);
	const offReason = offReasons.find((x) => offReasons.filter((y) => y === x).length >= 2) ?? null;
	if (offReason) for (const r of rows) if (r.state === 'off' && r.reason === offReason) r.reason = null;
	// What was found that no row owns: the kind of sound (speech, music) comes with the rhythm step.
	const analysis: Fact[] = found.filter((f) => f.label === 'Audio');
	const meter = analysisMeter(chips, { queued: o.queued, analyzing: o.analyzing });
	const analyzed = o.analyzing ? 'analyzing…' : (meter?.text ?? 'nothing to analyze');

	return { media, analyzed, rows, meter, offReason, analysis, proxy: proxyFacts(o.proxy) };
}
