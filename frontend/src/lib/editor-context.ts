/* What the editor shows, reported to the backend so an agent's `get_editor_context`
   (MCP) can tell which clip "this clip" is. The playhead moves every frame while
   playing, so reports are throttled: at most one per `delay`, always the latest,
   and only when something changed. A heartbeat re-sends the last one so the backend
   can tell a quiet window from a closed one (`editor_context.rs` calls a report
   older than 90 s stale). */
import type { UiContext } from './types';

export const REPORT_DELAY_MS = 300;
export const HEARTBEAT_MS = 30_000;

export interface UiContextInput {
	time: number;
	selectedClipIds: readonly string[];
	markIn: number | null;
	markOut: number | null;
	view: { start: number; end: number } | null;
	panel: string | null;
	assetId: string | null;
}

/** Milliseconds are as fine as an agent can act on, and coarser keeps a parked playhead's
 *  float noise from counting as a change. */
function ms(t: number): number {
	return Number.isFinite(t) ? Math.round(t * 1000) / 1000 : 0;
}

function msOrNull(t: number | null): number | null {
	return t === null || !Number.isFinite(t) ? null : ms(t);
}

export function uiContext(i: UiContextInput): UiContext {
	const view = i.view && Number.isFinite(i.view.start) && Number.isFinite(i.view.end) ? i.view : null;
	return {
		playhead: ms(i.time),
		selected_clip_ids: [...i.selectedClipIds],
		mark_in: msOrNull(i.markIn),
		mark_out: msOrNull(i.markOut),
		visible_range: view ? { start: ms(Math.max(0, view.start)), end: ms(Math.max(0, view.end)) } : null,
		focused_panel: i.panel,
		selected_asset_id: i.assetId
	};
}

export interface Timers {
	setTimeout(fn: () => void, ms: number): unknown;
	clearTimeout(id: unknown): void;
	setInterval(fn: () => void, ms: number): unknown;
	clearInterval(id: unknown): void;
}

const realTimers: Timers = {
	setTimeout: (fn, ms) => setTimeout(fn, ms),
	clearTimeout: (id) => clearTimeout(id as ReturnType<typeof setTimeout>),
	setInterval: (fn, ms) => setInterval(fn, ms),
	clearInterval: (id) => clearInterval(id as ReturnType<typeof setInterval>)
};

export interface ContextReporter {
	update(context: UiContext): void;
	stop(): void;
}

export function createContextReporter(
	send: (context: UiContext) => void,
	timers: Timers = realTimers,
	delay = REPORT_DELAY_MS,
	heartbeat = HEARTBEAT_MS
): ContextReporter {
	let latest: UiContext | null = null;
	let sent: string | null = null;
	let pending: unknown = null;
	const flush = () => {
		pending = null;
		if (!latest) return;
		const key = JSON.stringify(latest);
		if (key === sent) return;
		sent = key;
		send(latest);
	};
	const beat = timers.setInterval(() => {
		if (latest) send(latest);
	}, heartbeat);
	return {
		update(context) {
			latest = context;
			if (pending === null) pending = timers.setTimeout(flush, delay);
		},
		stop() {
			if (pending !== null) timers.clearTimeout(pending);
			pending = null;
			timers.clearInterval(beat);
		}
	};
}
