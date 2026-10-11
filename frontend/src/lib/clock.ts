// How a time reads in the side panels. There are three, each for one job: the transport's frame
// timecode (`timecode.ts`, what the playhead is at), a bin row's `m:ss` duration (`media-info.ts`), and
// this — a *position* or a length in the cut or a source, `m:ss.d`, which the Inspector, the titles list,
// the status bar's "Cut" and the agent's review card (`diff.ts`'s `formatTime`, the mirror of what the
// backend prints) all use. The Inspector used to say `00:12.50` above a field that said `12.5`; a field holds
// seconds, and says so.

/** `m:ss.d` (`h:mm:ss.d` from an hour). The tenth is rounded first, so 59.96 is `1:00.0`, never `0:60.0`. */
export function clock(seconds: number): string {
	const tenths = Math.round((Number.isFinite(seconds) ? Math.max(0, seconds) : 0) * 10);
	const whole = Math.floor(tenths / 10);
	const m = Math.floor(whole / 60);
	const ss = `${String(whole % 60).padStart(2, '0')}.${tenths % 10}`;
	return m >= 60 ? `${Math.floor(m / 60)}:${String(m % 60).padStart(2, '0')}:${ss}` : `${m}:${ss}`;
}

/** A start and an end as one range: `0:12.5 – 0:20.0`. */
export function clockRange(start: number, end: number): string {
	return `${clock(start)} – ${clock(end)}`;
}
