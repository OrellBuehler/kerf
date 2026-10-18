/* The render bar over the timeline ruler: how each span of the cut will play in the Preview
 * (`kerf_core::render_bar`, `get_render_bar`). Pure, so the colours, the words and the browser
 * harness's stand-in are tested without a timeline. */

import type { BarSpan, BarState, RenderBar, Timeline } from './types';
import { clipDuration } from './types';

/** The colour a span is drawn in: green where the GPU preview draws it, yellow where it hands
 *  over to the FFmpeg stream, red where playback stalls. */
export function barColor(state: BarState): string {
	switch (state) {
		case 'gpu':
			return 'var(--success)';
		case 'stream':
			return 'var(--warning)';
		case 'stall':
			return 'var(--danger)';
	}
}

const HEADINGS: Record<BarState, string> = {
	gpu: 'GPU preview',
	stream: 'FFmpeg stream',
	stall: 'May stall'
};

/** The tooltip of a span: what plays it, where, and why. */
export function barTip(span: BarSpan, tc: (t: number) => string): string {
	return `${HEADINGS[span.state]} ${tc(span.start)}–${tc(span.end)}: ${span.reason}`;
}

/** The spans that reach into `[from, to]` seconds, so a long cut draws only what is in view. */
export function spansIn(spans: BarSpan[], from: number, to: number): BarSpan[] {
	return spans.filter((s) => s.end > from && s.start < to);
}

/** What `get_render_bar` answers in the browser harness, where there is no GPU preview: the
 *  reversed clips on video tracks, which stall whoever plays them. */
export function harnessRenderBar(timeline: Timeline): RenderBar {
	const spans: BarSpan[] = [];
	const reversed = timeline.tracks
		.filter((t) => t.kind === 'video' && !t.muted)
		.flatMap((t) => t.clips.filter((c) => (c.speed ?? 1) < 0))
		.map((c) => [c.timeline_start, c.timeline_start + clipDuration(c)] as const)
		.sort((a, b) => a[0] - b[0]);
	for (const [start, end] of reversed) {
		const last = spans[spans.length - 1];
		if (last && start <= last.end) last.end = Math.max(last.end, end);
		else
			spans.push({
				start,
				end,
				state: 'stall',
				kind: 'reversed',
				reason: 'a clip plays backwards, which needs its whole source window read first'
			});
	}
	return { gpu_preview: false, spans };
}
