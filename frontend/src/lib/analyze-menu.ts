// The "Analyze" entries of a context menu (the bin's, a clip's) and the action behind them.
// The choices come from `analyze-steps.ts` (pure, tested); this is the glue that runs them.

import type { MenuItem } from './context-menu.svelte';
import { contextMenu } from './context-menu.svelte';
import { ui } from './editor-ui.svelte';
import { analyzeChoices } from './analysis-steps';
import { mediaStatus } from './media-status.svelte';
import { toast } from './notifications.svelte';
import type { Asset, AnalysisKind } from './types';

function fail(e: unknown) {
	toast.error(e instanceof Error ? e.message : String(e));
}

/** Run `steps` on an asset, saying so when it fails outright (a partial failure is reported by
 *  the run itself). */
export function analyzeAsset(assetId: string, steps: AnalysisKind[]) {
	return ui.runAnalysis(assetId, steps).catch(fail);
}

/** The menu items for analysing `asset`: a header, one entry per kind, then everything, then
 *  Stop while a pass is running on it. */
export function analyzeMenuItems(asset: Asset): MenuItem[] {
	const busy = ui.analyzingId === asset.id;
	const choices = analyzeChoices(mediaStatus.analysis(asset.id), {
		voiceover: !!asset.voiceover,
		busy: ui.analyzing,
		audio: asset.streams.some((s) => s.kind === 'audio'),
		image: asset.streams.some((s) => s.image),
		transcriptionAvailable: ui.transcription?.available ?? true
	});
	const items: MenuItem[] = [{ type: 'header', label: 'Analyze' }];
	for (const choice of choices) {
		items.push({
			label: choice.hint ? `${choice.label} · ${choice.hint}` : choice.label,
			icon: choice.id === 'all' ? 'scan-line' : undefined,
			disabled: choice.disabled,
			reason: choice.reason,
			action: () => void analyzeAsset(asset.id, choice.steps)
		});
	}
	if (busy) items.push({ label: 'Stop analysis', icon: 'x', action: () => ui.stopAnalysis() });
	return items;
}

/** A single menu entry that opens the Analyze menu where the pointer was — for menus too long
 *  to list six more items. */
export function analyzeSubmenuItem(asset: Asset, at: { x: number; y: number }): MenuItem {
	return {
		label: 'Analyze…',
		icon: 'scan-line',
		action: () => contextMenu.show(new MouseEvent('contextmenu', { clientX: at.x, clientY: at.y }), analyzeMenuItems(asset))
	};
}
