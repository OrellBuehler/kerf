// How a time reads, and how a typed one is read back — pure. There were three spellings of a position in the
// editor: the transport's frame timecode, the side panels' `m:ss.d`, and a bare `4.633333` in a number field,
// so the same clip's start was `00:00:12:15` in one panel and `0:12.5` in the next. This is the one place that
// decides, over a *mode* — `timecode` (the default: non-drop `HH:MM:SS:FF`, a count of frames of the rate it is
// asked in) or `seconds` (`m:ss.d`, what the review card prints). Which one is in force is `timeDisplay`
// (`time-display.svelte.ts`); everything that prints a position calls through it.
//
// A *field* is different from a readout: it is edited, so it shows a value that can be typed back exactly —
// timecode in timecode mode, decimal seconds (to the frame, never `4.633333`) in seconds mode — and
// `parseTime` takes either spelling in either mode, so a time can be pasted from wherever it was read.
//
// What stays as it is: the review card's `m:ss.d` (`diff.ts`) and the platform check's `m:ss` are mirrors of
// the backend's own phrasing, and a sentence the backend wrote cannot be re-spelled here.

import { clock } from './clock';
import { formatTimecode } from './timecode';

export type TimeMode = 'timecode' | 'seconds';

export const TIME_MODES: readonly { id: TimeMode; label: string; hint: string }[] = [
	{ id: 'timecode', label: 'Timecode', hint: 'HH:MM:SS:FF — hours, minutes, seconds and frames of the cut' },
	{ id: 'seconds', label: 'Seconds', hint: 'm:ss.d — minutes and seconds with a tenth' }
];

export const DEFAULT_TIME_MODE: TimeMode = 'timecode';

/** Whether `v` names a mode (a stored setting may say anything). */
export const isTimeMode = (v: unknown): v is TimeMode => v === 'timecode' || v === 'seconds';

const usableFps = (fps: number | undefined): number => (fps !== undefined && Number.isFinite(fps) && fps > 0 ? fps : 30);

/** A time as a readout: `00:00:12:15` (timecode) or `0:12.5` (seconds). `frames: false` cuts the timecode at
 *  the second, for a ruler label. A negative, NaN or infinite time is zero. */
export function formatTime(seconds: number, fps: number, mode: TimeMode = DEFAULT_TIME_MODE, o: { frames?: boolean } = {}): string {
	return mode === 'seconds' ? clock(seconds) : formatTimecode(seconds, fps, o);
}

/** A start and an end as one range: `00:00:12:15 – 00:00:20:15`. */
export function formatRange(start: number, end: number, fps: number, mode: TimeMode = DEFAULT_TIME_MODE): string {
	return `${formatTime(start, fps, mode)} – ${formatTime(end, fps, mode)}`;
}

/** The nearest frame boundary of a time at `fps`. */
export function roundToFrame(seconds: number, fps: number): number {
	const rate = usableFps(fps);
	const t = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
	return Math.round(t * rate + 1e-9) / rate;
}

/** Decimal seconds, to the frame and to no more than three places: `12.5`, `4.633`, `0`. Never the long
 *  tail of a division (`4.633333`) a number field used to show. */
export function formatSecondsField(seconds: number, fps: number): string {
	const t = roundToFrame(seconds, fps);
	return String(Number(t.toFixed(3)));
}

/** A time as the text of an editable field: timecode, or decimal seconds, per the mode. */
export function formatField(seconds: number, fps: number, mode: TimeMode = DEFAULT_TIME_MODE): string {
	return mode === 'seconds' ? formatSecondsField(seconds, fps) : formatTimecode(roundToFrame(seconds, fps), fps);
}

const NUMBER = /^\d+(?:\.\d+)?$|^\.\d+$/;
const WHOLE = /^\d+$/;

/**
 * What a typed time means, in seconds — or `null` when it is not one. Accepted, in any mode:
 *  - seconds: `12.5`, `12.5s`, `.5`;
 *  - frames: `375f` (a count of frames of the cut's rate);
 *  - timecode: `HH:MM:SS:FF`, `MM:SS:FF`, `SS:FF` — the last field is frames (a `;` for drop-frame reads as `:`);
 *  - clock: `1:02.5`, `1:02:03.25` — a colon time whose last field has a decimal point is `[h:]m:s.fraction`.
 * Whitespace is ignored and a negative time is refused. The result is as typed, not yet on a frame:
 * `roundToFrame` is the caller's decision (a field that is a frame count rounds; a title's length need not).
 */
export function parseTime(text: string, fps: number): number | null {
	const t = text.trim().toLowerCase().replace(/\s+/g, '').replace(/;/g, ':');
	if (t === '' || t.startsWith('-')) return null;
	const rate = usableFps(fps);
	if (t.endsWith('f') && WHOLE.test(t.slice(0, -1))) return Number(t.slice(0, -1)) / rate;
	if (!t.includes(':')) {
		const plain = t.endsWith('s') ? t.slice(0, -1) : t;
		return NUMBER.test(plain) ? Number(plain) : null;
	}
	const fields = t.split(':');
	if (fields.length > 4 || fields.some((f) => f === '')) return null;
	const last = fields[fields.length - 1];
	if (last.includes('.')) {
		// `m:ss.d` / `h:mm:ss.d`: the fraction belongs to the seconds, and the fields before it are whole.
		const head = fields.slice(0, -1);
		if (head.length > 2 || !head.every((f) => WHOLE.test(f)) || !NUMBER.test(last)) return null;
		const [h, m] = head.length === 2 ? head : [0, ...head];
		return Number(h) * 3600 + Number(m) * 60 + Number(last);
	}
	if (!fields.every((f) => WHOLE.test(f))) return null;
	const n = fields.map(Number);
	const frames = n[n.length - 1];
	const seconds = n.length >= 2 ? n[n.length - 2] : 0;
	const minutes = n.length >= 3 ? n[n.length - 3] : 0;
	const hours = n.length >= 4 ? n[0] : 0;
	// A timecode field past the rate is a typo, not a carry: `00:00:01:45` at 30 fps is no frame at all.
	const nominal = Math.max(1, Math.round(rate));
	if (frames >= nominal) return null;
	if (seconds >= 60 || (n.length >= 3 && minutes >= 60)) return null;
	// Timecode counts frames in whole nominal seconds (`formatTimecode`), so at 29.97 the second that is
	// `:00` is the 30th frame, not exactly one second in: divide the count, not the fields.
	return (((hours * 60 + minutes) * 60 + seconds) * nominal + frames) / rate;
}

/** A typed time for a field that is a frame position: parsed, then put on a frame. `null` for text that is not
 *  a time. */
export function parseFrameTime(text: string, fps: number): number | null {
	const t = parseTime(text, fps);
	return t === null ? null : roundToFrame(t, fps);
}
