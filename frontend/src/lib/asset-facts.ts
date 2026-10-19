// What the Inspector says about a library asset when one is selected and no clip is: the facts the bin's
// right-click menu used to list as twenty rows (ten about the file, ten about its analysis, a few about its
// proxy), which made the menu 960 px tall and left the Inspector saying "Select a clip" with an asset
// selected. Pure, so the lines are tested and the menu can be the actions only.

import { doneCount, kindInfo } from './analysis-steps';
import { analysisFacts, shortPath, type MediaInfo } from './media-info';
import { proxyFacts } from './proxy-info';
import type { Asset, AnalysisStatus, AssetAnalysis, ProxyStatus } from './types';

export interface Fact {
	label: string;
	value: string;
	/** The whole text, when the value is a shortened one. */
	title?: string;
}

export interface AssetFacts {
	/** The file: its length, frame, codec, projection, where it is used. */
	media: Fact[];
	/** How much analysis has run, for the section's summary: `2 of 5`, `running…`. The chips say which. */
	analyzed: string;
	/** The kinds that failed, with their reasons, then what analysis has found. */
	analysis: Fact[];
	/** The preview proxy. Empty for a file that never gets one. */
	proxy: Fact[];
}

export function assetFacts(o: {
	asset: Pick<Asset, 'duration' | 'imported_at' | 'path' | 'source_paths'>;
	info: MediaInfo;
	analysis: AssetAnalysis | null | undefined;
	status: AnalysisStatus | null | undefined;
	/** A pass is running on this asset now. */
	analyzing: boolean;
	proxy: ProxyStatus | null | undefined;
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
	const imported = new Date(asset.imported_at);
	if (!Number.isNaN(imported.getTime())) media.push({ label: 'Imported', value: imported.toLocaleDateString() });
	media.push({ label: 'Path', value: shortPath(asset.path), title: asset.path });

	const analysis: Fact[] = [];
	for (const k of o.status?.kinds ?? []) {
		if (k.state === 'failed') analysis.push({ label: kindInfo(k.kind).label, value: 'failed', title: k.reason ?? undefined });
	}
	for (const f of analysisFacts(o.analysis, asset.duration)) analysis.push(f);

	return { media, analyzed: o.analyzing ? 'running…' : doneCount(o.status), analysis, proxy: proxyFacts(o.proxy) };
}
