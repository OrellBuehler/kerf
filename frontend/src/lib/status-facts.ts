// The facts the status bar's left side states — pure. It used to open with "24.000 fps · 3840×2160 · h264" and no
// label: the *selected library asset's* format, which reads as the project's (and then disagrees with the
// timecode beside the playhead, which counts the cut's 30). Now the two are named for what they are: the
// project — the rate and frame the cut plays and exports at — and, when one is picked, the asset.

import type { Asset, Timeline } from './types';

const rate = (fps: number) => `${Number(fps.toFixed(3))} fps`;

export interface Frame {
	width: number;
	height: number;
}

/** The frame the cut is made at: the delivery frame when the project has one, else the first picture on the cut
 *  (what the export follows), else nothing. */
export function projectFrame(timeline: Timeline, assets: readonly Pick<Asset, 'id' | 'streams'>[]): Frame | null {
	const f = timeline.format;
	if (f?.width && f?.height) return { width: f.width, height: f.height };
	for (const track of timeline.tracks) {
		for (const clip of track.clips) {
			const v = assets.find((a) => a.id === clip.asset_id)?.streams.find((s) => s.kind === 'video');
			if (v?.width && v?.height) return { width: v.width, height: v.height };
		}
	}
	return null;
}

/** `30 fps · 1920×1080` — the project. With nothing to say a frame of (an empty cut) it is the rate alone. */
export function projectLine(fps: number, frame: Frame | null): string {
	return [rate(fps), frame ? `${frame.width}×${frame.height}` : ''].filter(Boolean).join(' · ');
}

/** `24 fps · 3840×2160 · h264` for a library asset, or null when it says none of the three (an audio file with no codec). */
export function assetLine(asset: Pick<Asset, 'streams'>): string | null {
	const v = asset.streams.find((s) => s.kind === 'video');
	const parts: string[] = [];
	if (v?.fps) parts.push(rate(v.fps));
	if (v?.width && v?.height) parts.push(`${v.width}×${v.height}`);
	const codec = (v ?? asset.streams[0])?.codec;
	if (codec) parts.push(codec);
	return parts.join(' · ') || null;
}
