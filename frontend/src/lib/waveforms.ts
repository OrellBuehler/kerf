// The app's one waveform tile cache: `WaveformCache` wired to the backend's
// `get_waveform_range` and to the notification log, so a file whose peaks cannot
// be read says so once rather than silently drawing nothing.

import { getWaveformRange } from './api';
import { toast } from './notifications.svelte';
import { editor } from './state.svelte';
import { WaveformCache } from './waveform-cache';

export const waveforms = new WaveformCache(getWaveformRange, {
	onFail: (assetId, message) => toast.error(`Couldn't read the waveform of ${editor.assetName(assetId)} — ${message}`)
});
