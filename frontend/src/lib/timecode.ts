import { MAX_DELIVERY_FPS, MIN_DELIVERY_FPS } from './delivery-formats';
import { trackRenders } from './levels';
import { clipDuration, type Asset, type Timeline } from './types';

const validFps = (fps: number | undefined): number =>
	fps !== undefined && Number.isFinite(fps) && fps > 0 ? fps : 30;

/** The rate the project's delivery frame states, when it is one a delivery may name (`Delivery::rate`: a stored
	* value out of range is ignored, as the engine ignores it). */
function deliveryRate(timeline: Timeline): number | null {
	const fps = timeline.format?.fps;
	return fps !== undefined && fps !== null && Number.isFinite(fps) && fps >= MIN_DELIVERY_FPS && fps <= MAX_DELIVERY_FPS ? fps : null;
}

/** Match the engine's default export timebase: the delivery frame's rate when the project states one, else the
	* first clip carrying video, across all tracks, with a 30 fps fallback. Media selection is irrelevant. */
export function timelineFps(timeline: Timeline, assets: Asset[]): number {
	const delivered = deliveryRate(timeline);
	if (delivered !== null) return delivered;
	for (const track of timeline.tracks) {
		for (const clip of track.clips) {
			const stream = assets.find(a => a.id === clip.asset_id)?.streams.find(s => s.kind === 'video');
			if (stream) return validFps(stream.fps);
		}
	}
	return 30;
}

/** The rate the cut plays and exports at: [`timelineFps`] read from the cut **as it renders**, as the
	* engine reads it (`export_format` of `Timeline::for_render` — the muted and solo-shadowed tracks
	* (`trackRenders`) and the disabled clips are not in it, so their footage does not set the rate).
	* What playback's rate, ladder and gate follow; timecode keeps [`timelineFps`], which is a frame
	* count of the cut as it is edited. */
export function renderedFps(timeline: Timeline, assets: Asset[]): number {
	const delivered = deliveryRate(timeline);
	if (delivered !== null) return delivered;
	for (const track of timeline.tracks) {
		if (!trackRenders(timeline, track)) continue;
		for (const clip of track.clips) {
			if (clip.enabled === false) continue;
			const stream = assets.find(a => a.id === clip.asset_id)?.streams.find(s => s.kind === 'video');
			if (stream) return validFps(stream.fps);
		}
	}
	return 30;
}

/** How long the cut is **as it renders** (`Timeline::for_render().duration()`): the end of the last
	* enabled clip on a track that renders. What a stream may start before — the engine refuses a start at
	* or past it ("nothing to play past the playhead") — where the timeline's own length also counts the
	* clips that are muted, shadowed by a solo or disabled. */
export function renderedDuration(timeline: Timeline): number {
	let end = 0;
	for (const track of timeline.tracks) {
		if (!trackRenders(timeline, track)) continue;
		for (const clip of track.clips) {
			if (clip.enabled === false) continue;
			end = Math.max(end, clip.timeline_start + clipDuration(clip));
		}
	}
	return end;
}

/** Frames are held to this on the way in and out (kerf-core `timecode.rs`): every intermediate of the
	* count stays an exact integer in a double. */
export const MAX_TIMECODE_FRAME = 999_999_999_999;

const MAX_FIELD_DIGITS = 9;

/** How frames are counted into a timecode: frames per timecode second and frames dropped per minute
	* (0 for non-drop). The mirror of kerf-core's `TimecodeRate`. */
export interface TimecodeRate {
	base: number;
	drop: number;
}

/** The frames a timecode second holds at `fps`: 30 at 29.97, 30 for an unusable rate. */
export const nominalTimecodeRate = (fps: number): number =>
	Number.isFinite(fps) && fps > 0 ? Math.min(1000, Math.max(1, Math.round(fps))) : 30;

/** Whether `fps` is an NTSC rate that counts drop-frame — 29.97 or 59.94, however it is written (a
	* probed `30000/1001` or a preset's `29.97`). */
export const isDropFrameRate = (fps: number): boolean => [30, 60].some((base) => Math.abs(fps - (base * 1000) / 1001) < 0.01);

/** The count at `fps`; `dropFrame` asks for drop-frame where the rate has it. */
export function timecodeRate(fps: number, dropFrame = true): TimecodeRate {
	const base = nominalTimecodeRate(fps);
	return { base, drop: dropFrame && isDropFrameRate(fps) ? Math.trunc(base / 15) : 0 };
}

const clampFrame = (n: number) => (Number.isNaN(n) ? 0 : Math.min(MAX_TIMECODE_FRAME, Math.max(0, n)));

/** `[hours, minutes, seconds, frames]` of a frame count. */
export function timecodeFields(r: TimecodeRate, frame: number): [number, number, number, number] {
	let n = clampFrame(Math.floor(frame));
	const { base, drop } = r;
	if (drop > 0) {
		const perMin = base * 60 - drop;
		const perTen = perMin * 10 + drop;
		const tens = Math.floor(n / perTen);
		const rest = n % perTen;
		n += drop * 9 * tens;
		if (rest > drop) n += drop * Math.floor((rest - drop) / perMin);
	}
	return [Math.floor(n / (base * 3600)), Math.floor(n / (base * 60)) % 60, Math.floor(n / base) % 60, n % base];
}

/** The frame count of a label; a drop-frame label that does not exist (`00;01;00;00`) is the first one
	* of its minute that does. */
export function timecodeFrames(r: TimecodeRate, h: number, m: number, s: number, f: number): number {
	if (r.drop > 0 && m % 10 !== 0 && s === 0 && f < r.drop) f = r.drop;
	let n = (h * 3600 + m * 60 + s) * r.base + f;
	if (r.drop > 0) {
		const minutes = h * 60 + m;
		n -= r.drop * (minutes - Math.floor(minutes / 10));
	}
	return n;
}

/** `HH:MM:SS:FF`, or `HH;MM;SS;FF` counting drop-frame; `frames: false` cuts it at the second. */
export function formatFrames(r: TimecodeRate, frame: number, o: { frames?: boolean } = {}): string {
	const fields = timecodeFields(r, frame);
	return (o.frames === false ? fields.slice(0, 3) : fields).map((n) => String(n).padStart(2, '0')).join(r.drop > 0 ? ';' : ':');
}

/** The frame shown at `seconds`: the one it falls inside (30 fps for an unusable rate). */
export function timecodeFrameAt(seconds: number, fps: number): number {
	const t = Number.isFinite(seconds) ? Math.max(0, seconds) : 0;
	return Math.min(MAX_TIMECODE_FRAME, Math.floor(t * validFps(fps) + 1e-7));
}

const isDigits = (s: string) => /^[0-9]*$/.test(s);

/** A typed timecode as a frame count, as an NLE's timecode field reads it — the mirror of kerf-core's
	* `TimecodeRate::parse`: `01:02:03:04` with any of `:;.,` between the fields (fewer fill from the
	* right, so `1.00` is a second); bare digits as right-aligned pairs (`1000` is `00:00:10:00`); a
	* leading `+` / `-` relative to `current` (`+15` frames on, `-1.00` a second back). A field past its
	* range carries into the next before a drop-frame label is read. `null` for what is not a timecode
	* (fields that are all empty, `:`, too); the frame is held to `0..MAX_TIMECODE_FRAME`. */
export function parseTimecode(input: string, r: TimecodeRate, current: number): number | null {
	const text = input.trim();
	if (text === '') return null;
	const sign = text[0] === '+' ? 1 : text[0] === '-' ? -1 : null;
	const parts = (sign === null ? text : text.slice(1)).split(/[:;.,]/);
	let fields: number[];
	if (parts.length === 1) {
		const bare = parts[0];
		if (bare === '' || !isDigits(bare)) return null;
		if (sign !== null) {
			if (bare.length > MAX_FIELD_DIGITS) return null;
			fields = [Number(bare)];
		} else {
			if (bare.length > 8) return null;
			fields = [];
			for (let end = bare.length; end > 0; end -= 2) fields.unshift(Number(bare.slice(Math.max(0, end - 2), end)));
		}
	} else {
		if (parts.length > 4 || !parts.every((p) => p.length <= MAX_FIELD_DIGITS && isDigits(p)) || parts.every((p) => p === '')) return null;
		fields = parts.map((p) => (p === '' ? 0 : Number(p)));
	}
	let frames: number;
	if (sign !== null && parts.length === 1) {
		frames = fields[0];
	} else {
		// A field past its range carries into the next, then the label is read as a label.
		let [h, m, s, f] = [...new Array<number>(4 - fields.length).fill(0), ...fields];
		[s, f] = [s + Math.floor(f / r.base), f % r.base];
		[m, s] = [m + Math.floor(s / 60), s % 60];
		[h, m] = [h + Math.floor(m / 60), m % 60];
		frames = timecodeFrames(r, h, m, s, f);
	}
	return clampFrame(sign === null ? frames : clampFrame(current) + sign * Math.min(frames, MAX_TIMECODE_FRAME));
}

/** The project's one timecode: `HH:MM:SS:FF`, drop-frame (`HH;MM;SS;FF`) at 29.97 and 59.94 unless
	* `dropFrame: false`. Hours are always shown, so a cut under a minute reads `00:00:20:15` and cannot be
	* taken for minutes:seconds:frames. Count frames before dividing, so fractional rates and floating-point
	* frame stepping never skip a frame. `frames: false` is the same count cut at the second (`HH:MM:SS`),
	* for a ruler label that a whole-second step does not resolve below. */
export function formatTimecode(seconds: number, fps: number, o: { frames?: boolean; dropFrame?: boolean } = {}): string {
	return formatFrames(timecodeRate(fps, o.dropFrame !== false), timecodeFrameAt(seconds, fps), o);
}
