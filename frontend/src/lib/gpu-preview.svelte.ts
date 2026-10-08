// The GPU preview as the webview sees it: what the backend says about this machine, and which
// renderer made the frame on screen (the status bar and the Settings dialog read both).
//
// The decision whether a frame is drawn by the GPU is the backend's — the render plan's — and
// the page only asks and is told (`getPreviewFrame`); this singleton holds the answers.

import { gpuPreviewStatus } from './api';
import type { GpuPreviewStatus, GpuTimings, PreviewFrameResult } from './types';

/** How often a refused frame may re-read the status (a failure that changed `ready` / `reason`). */
const REFRESH_EVERY_MS = 2000;

class GpuPreviewStore {
	status = $state<GpuPreviewStatus | null>(null);
	/** Which renderer made the frame now on screen; null before the first one, and after the
	 *  setting is turned off. */
	renderer = $state<'gpu' | 'ffmpeg' | null>(null);
	/** Why the last frame was not the GPU's (empty when it was). */
	reasons = $state<string[]>([]);
	timings = $state<GpuTimings | null>(null);
	private refreshedAt = 0;

	async refresh() {
		this.refreshedAt = Date.now();
		try {
			this.status = await gpuPreviewStatus();
		} catch (e) {
			console.error('could not read the GPU preview status', e);
		}
	}

	/** Record what `getPreviewFrame` answered. */
	note(result: PreviewFrameResult) {
		const before = this.renderer;
		this.renderer = result.renderer;
		this.reasons = result.reasons;
		this.timings = result.timings;
		// A frame the GPU did not draw may be a failure that changed the device's state; the
		// dialog's "why not" line should follow, without a command per frame.
		if ((result.renderer === 'ffmpeg' && Date.now() - this.refreshedAt > REFRESH_EVERY_MS) || before === null) {
			void this.refresh();
		}
	}

	/** The route is the JPEG (playback, a dialog over the frame, …): the frame on screen is FFmpeg's. */
	noteJpeg(why: string) {
		if (this.renderer === 'ffmpeg' && this.reasons[0] === why) return;
		this.renderer = 'ffmpeg';
		this.reasons = [why];
		this.timings = null;
	}

	/** The setting went off (or the preview unmounted): there is no renderer to name. */
	clear() {
		this.renderer = null;
		this.reasons = [];
		this.timings = null;
	}

	/** One line for the status bar, or null when there is nothing to say. */
	get label(): string | null {
		if (this.renderer === 'gpu') {
			const t = this.timings;
			const where = this.status?.adapter ? ` · ${this.status.adapter}` : '';
			return t
				? `GPU ${t.width}×${t.height} · ${Math.round(t.decode_ms + t.composite_ms + t.present_ms)} ms${where}`
				: `GPU${where}`;
		}
		if (this.renderer === 'ffmpeg') {
			const why = this.reasons[0];
			return why ? `FFmpeg · ${why}` : 'FFmpeg';
		}
		return null;
	}
}

export const gpuPreview = new GpuPreviewStore();
