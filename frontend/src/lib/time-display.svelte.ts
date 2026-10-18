// How times read in the interface right now: the one reactive singleton over `time-format.ts`. Everything that
// prints a position calls `timeDisplay.fmt` (or `range` / `field`), so the mode — timecode by default, seconds
// when it is asked for — changes every readout at once. The setting that picks it is Settings' to wire
// (`timeDisplay.mode = …`); nothing else sets it.

import { editor } from './state.svelte';
import { DEFAULT_TIME_MODE, formatField, formatRange, formatTime, parseFrameTime, parseTime, timeHint, type TimeMode } from './time-format';

class TimeDisplay {
	mode = $state<TimeMode>(DEFAULT_TIME_MODE);

	/** A position as a readout, at the cut's rate unless `fps` names another (a source's own). */
	fmt(seconds: number, fps: number = editor.fps, o: { frames?: boolean } = {}): string {
		return formatTime(seconds, fps, this.mode, o);
	}

	/** A start and an end as one range. */
	range(start: number, end: number, fps: number = editor.fps): string {
		return formatRange(start, end, fps, this.mode);
	}

	/** A position as the text of an editable field (`parse` reads it back). */
	field(seconds: number, fps: number = editor.fps): string {
		return formatField(seconds, fps, this.mode);
	}

	/** What typed text means in seconds, as typed (not on a frame), or null. A bare colon time (`1:05`) is read the
	 *  way the mode prints one. */
	parse(text: string, fps: number = editor.fps): number | null {
		return parseTime(text, fps, this.mode);
	}

	/** What typed text means as a frame position, or null. */
	parseFrame(text: string, fps: number = editor.fps): number | null {
		return parseFrameTime(text, fps, this.mode);
	}

	/** The spellings a field takes, for the message after a refusal. */
	hint(): string {
		return timeHint(this.mode);
	}
}

export const timeDisplay = new TimeDisplay();
