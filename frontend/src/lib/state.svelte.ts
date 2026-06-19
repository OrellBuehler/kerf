// Central editor state (Svelte 5 runes).

import { getAssetMetadata, listAssets, getTimeline, pickAndImport } from './api';
import type { Asset, AssetMetadata, Timeline } from './types';

class EditorState {
	assets = $state<Asset[]>([]);
	timeline = $state<Timeline>({ tracks: [] });
	selectedAssetId = $state<string | null>(null);
	selectedMetadata = $state<AssetMetadata | null>(null);
	loading = $state(false);
	error = $state<string | null>(null);

	get selectedAsset(): Asset | undefined {
		return this.assets.find((a) => a.id === this.selectedAssetId);
	}

	assetName(assetId: string): string {
		return this.assets.find((a) => a.id === assetId)?.name ?? 'unknown';
	}

	async load() {
		this.loading = true;
		this.error = null;
		try {
			[this.assets, this.timeline] = await Promise.all([listAssets(), getTimeline()]);
			if (!this.selectedAssetId && this.assets.length > 0) {
				await this.select(this.assets[0].id);
			}
		} catch (e) {
			this.error = e instanceof Error ? e.message : String(e);
		} finally {
			this.loading = false;
		}
	}

	async select(assetId: string) {
		this.selectedAssetId = assetId;
		try {
			this.selectedMetadata = await getAssetMetadata(assetId);
		} catch {
			this.selectedMetadata = null;
		}
	}

	async refreshTimeline() {
		this.timeline = await getTimeline();
	}

	async importMedia(): Promise<Asset | null> {
		const asset = await pickAndImport();
		if (asset) {
			this.assets = [...this.assets, asset];
			await this.select(asset.id);
		}
		return asset;
	}
}

export const editor = new EditorState();
