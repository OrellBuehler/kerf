// Bridge to the Tauri backend (kerf-core via kerf-app commands).
//
// When running outside Tauri (e.g. `bun run dev` in a browser for design work)
// the calls fall back to a seeded sample so the UI is fully explorable.

import type { Asset, AssetMetadata, Timeline } from './types';

export function inTauri(): boolean {
	return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
	const { invoke } = await import('@tauri-apps/api/core');
	return invoke<T>(cmd, args);
}

// ---- sample fallback (browser dev) ----------------------------------------

const sampleAssets: Asset[] = [
	{
		id: '11111111-1111-1111-1111-111111111111',
		path: '/samples/interview.mp4',
		name: 'interview.mp4',
		duration: 120,
		streams: [
			{ index: 0, kind: 'video', codec: 'h264', width: 1920, height: 1080, fps: 30 },
			{ index: 1, kind: 'audio', codec: 'aac', sample_rate: 48000, channels: 2 }
		],
		imported_at: new Date().toISOString()
	},
	{
		id: '22222222-2222-2222-2222-222222222222',
		path: '/samples/broll.mp4',
		name: 'broll.mp4',
		duration: 45,
		streams: [{ index: 0, kind: 'video', codec: 'h264', width: 3840, height: 2160, fps: 24 }],
		imported_at: new Date().toISOString()
	}
];

const sampleTimeline: Timeline = {
	tracks: [
		{
			id: 'v1',
			kind: 'video',
			name: 'V1',
			clips: [
				{
					id: 'c1',
					asset_id: sampleAssets[0].id,
					source_in: 0,
					source_out: 12.5,
					timeline_start: 0,
					volume: 1
				},
				{
					id: 'c2',
					asset_id: sampleAssets[1].id,
					source_in: 0,
					source_out: 8,
					timeline_start: 12.5,
					volume: 1
				}
			]
		},
		{
			id: 'a1',
			kind: 'audio',
			name: 'A1',
			clips: [
				{
					id: 'c3',
					asset_id: sampleAssets[0].id,
					source_in: 0,
					source_out: 120,
					timeline_start: 0,
					volume: 1
				}
			]
		}
	]
};

const sampleAnalysis: AssetMetadata = {
	asset: sampleAssets[0],
	analysis: {
		asset_id: sampleAssets[0].id,
		silence_segments: [
			{ start: 12.5, end: 14 },
			{ start: 60, end: 63.2 }
		],
		scene_changes: [0, 30, 75, 110],
		transcript: [
			{ start: 0, end: 5.5, text: 'Welcome back to the channel.' },
			{ start: 5.5, end: 12.5, text: 'Today we are talking about non-destructive editing.' }
		]
	}
};

// ---- public API ------------------------------------------------------------

export async function listAssets(): Promise<Asset[]> {
	if (!inTauri()) return structuredClone(sampleAssets);
	return invoke<Asset[]>('list_assets');
}

export async function getTimeline(): Promise<Timeline> {
	if (!inTauri()) return structuredClone(sampleTimeline);
	return invoke<Timeline>('get_timeline');
}

export async function getAssetMetadata(assetId: string): Promise<AssetMetadata> {
	if (!inTauri()) return structuredClone(sampleAnalysis);
	return invoke<AssetMetadata>('get_asset_metadata', { assetId });
}

export async function importAsset(path: string): Promise<Asset> {
	if (!inTauri()) throw new Error('import is only available in the desktop app');
	return invoke<Asset>('import_asset', { path });
}

/** Open a native file picker and import the chosen media file. */
export async function pickAndImport(): Promise<Asset | null> {
	if (!inTauri()) return null;
	const { open } = await import('@tauri-apps/plugin-dialog');
	const selected = await open({
		multiple: false,
		filters: [{ name: 'Media', extensions: ['mp4', 'mov', 'mkv', 'webm', 'wav', 'mp3', 'm4a', 'aac'] }]
	});
	if (typeof selected !== 'string') return null;
	return importAsset(selected);
}
