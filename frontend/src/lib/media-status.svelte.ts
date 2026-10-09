// What the page knows about each asset's preview proxy and analysis, kept current by the
// backend's events (`proxy-progress`, `analysis-status`) and re-read when the assets change.
// The bin's badges and chips, the inspector, the status bar and the quick edits all read this,
// so they cannot disagree about whether a proxy is ready or a transcript has run.

import { getAnalysisStatuses, getProxyStatuses } from './api';
import type { AnalysisKind, AnalysisStatus, ProxyStatus } from './types';

class MediaStatus {
	proxies = $state<Record<string, ProxyStatus>>({});
	analyses = $state<Record<string, AnalysisStatus>>({});

	proxy(assetId: string): ProxyStatus | undefined {
		return this.proxies[assetId];
	}

	analysis(assetId: string): AnalysisStatus | undefined {
		return this.analyses[assetId];
	}

	/** Whether `kind` of an asset has run to completion. */
	done(assetId: string, kind: AnalysisKind): boolean {
		return this.analyses[assetId]?.kinds.find((k) => k.kind === kind)?.state === 'done';
	}

	noteProxy(status: ProxyStatus) {
		this.proxies[status.asset_id] = status;
	}

	noteAnalysis(status: AnalysisStatus) {
		this.analyses[status.asset_id] = status;
	}

	/** Re-read everything from the backend (the assets changed, or a setting that moves
	 *  what a status means). Whole maps are replaced, so a removed asset's status goes. */
	async refresh() {
		const [proxies, analyses] = await Promise.all([getProxyStatuses().catch(() => null), getAnalysisStatuses().catch(() => null)]);
		if (proxies) this.proxies = Object.fromEntries(proxies.map((p) => [p.asset_id, p]));
		if (analyses) this.analyses = Object.fromEntries(analyses.map((a) => [a.asset_id, a]));
	}
}

export const mediaStatus = new MediaStatus();
