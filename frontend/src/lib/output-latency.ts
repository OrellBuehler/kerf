// How late the sound of the preview's Web Audio graph is: the picture of GPU playback is asked for
// the time that is *audible* now, and the audio clock (`AudioContext.currentTime`, what
// `audio.clock()` reads) is ahead of that by the buffering between the context and the speakers.
//
// Kept apart from `audio.ts` (which pulls in the toasts) so it is tested on its own.

/** What this needs of an `AudioContext`. */
export interface LatencyContext {
	currentTime: number;
	baseLatency?: number;
	outputLatency?: number;
	getOutputTimestamp?: () => { contextTime?: number; performanceTime?: number };
}

/** The most latency believed: a driver that reports more is lying, or the machine is in trouble,
 *  and a picture shown half a second early is worse than one shown a little late. */
export const MAX_OUTPUT_LATENCY = 0.5;

function finite(v: unknown): v is number {
	return typeof v === 'number' && Number.isFinite(v);
}

function clamp(seconds: number): number {
	return Math.min(MAX_OUTPUT_LATENCY, Math.max(0, seconds));
}

/**
 * Seconds between the context rendering a sample and the device playing it, or 0 when the context
 * cannot say. In order of trust:
 *
 * 1. `getOutputTimestamp()`: the context time of the sample the device is playing *now*, against
 *    `currentTime`, the next one it renders — the whole of the delay, measured, not reported;
 * 2. `outputLatency` (the device's) plus `baseLatency` (the context's own buffering), what the
 *    browser says it is — Chromium and Firefox have both, Safari has `baseLatency` alone;
 * 3. nothing: 0, the picture is asked for the audio clock itself.
 */
export function outputLatency(ctx: LatencyContext | null | undefined): number {
	if (!ctx) return 0;
	try {
		const heard = ctx.getOutputTimestamp?.().contextTime;
		// 0 is "no timestamp yet" (the device has not started), and a timestamp ahead of the render
		// head cannot be a delay.
		if (finite(heard) && heard > 0 && finite(ctx.currentTime) && ctx.currentTime >= heard) {
			return clamp(ctx.currentTime - heard);
		}
	} catch {
		// a context that throws for a timestamp is read the other way
	}
	const device = finite(ctx.outputLatency) && ctx.outputLatency > 0 ? ctx.outputLatency : 0;
	const base = finite(ctx.baseLatency) && ctx.baseLatency > 0 ? ctx.baseLatency : 0;
	return clamp(device + base);
}
