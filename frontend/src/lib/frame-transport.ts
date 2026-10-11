/* How a playback frame gets from the backend's channel onto the preview's canvas — pure, so it can be
 * tested without a webview.
 *
 * The backend sends each composited frame as raw bytes (`playback_message` in kerf-app): the frame's
 * timeline time as a little-endian f64, then the JPEG undecoded. The page used to receive a base64
 * `data:` URL in JSON and hand it to an `<img>`; the cost of that was never the base64 encode but
 * what the webview did with the string — take a third more bytes, parse them as JSON, decode the
 * URL on the main thread, then decode the picture. Here the bytes become a `Blob`, `createImageBitmap`
 * decodes them off the main thread, and a canvas draws the bitmap (a canvas, not an `<img>` of a
 * `blob:` URL: the CSP admits `data:` images and no `blob:`). */

/** Bytes in front of the JPEG in a playback message. Mirrors `PLAYBACK_HEADER_BYTES` in kerf-app. */
export const PLAYBACK_HEADER_BYTES = 8;

export interface PlaybackMessage {
	/** The timeline time the frame shows, seconds. */
	time: number;
	/** The JPEG, undecoded: a view into the message, not a copy. */
	jpeg: Uint8Array<ArrayBuffer>;
}

const isBuffer = (value: unknown): value is ArrayBuffer => Object.prototype.toString.call(value) === '[object ArrayBuffer]';

/**
 * Split a channel message into the frame's time and its JPEG, or `null` for a message that is not a
 * frame (a header with nothing after it, a time that is not a number, anything that is not bytes). The
 * channel hands raw payloads over as an `ArrayBuffer`; a view of one is taken as well.
 */
export function parsePlaybackMessage(message: unknown): PlaybackMessage | null {
	let bytes: Uint8Array<ArrayBuffer>;
	if (isBuffer(message)) bytes = new Uint8Array(message);
	else if (ArrayBuffer.isView(message)) bytes = new Uint8Array(message.buffer as ArrayBuffer, message.byteOffset, message.byteLength);
	else return null;
	if (bytes.byteLength <= PLAYBACK_HEADER_BYTES) return null;
	const time = new DataView(bytes.buffer, bytes.byteOffset, PLAYBACK_HEADER_BYTES).getFloat64(0, true);
	if (!Number.isFinite(time)) return null;
	return { time, jpeg: bytes.subarray(PLAYBACK_HEADER_BYTES) };
}

/** The frame as the `Blob` `createImageBitmap` takes (the bytes are copied into it). */
export function jpegBlob(jpeg: Uint8Array<ArrayBuffer>): Blob {
	return new Blob([jpeg], { type: 'image/jpeg' });
}

/** What the painter draws: a decoded picture and the way to let go of it. */
export interface Picture {
	source: CanvasImageSource;
	width: number;
	height: number;
	close(): void;
}

export interface PainterDeps {
	/** The JPEG as a picture; rejects for a frame that cannot be decoded. */
	decode(jpeg: Blob): Promise<Picture>;
	/** Draw a picture. Called in order, never with one older than the last drawn. */
	paint(picture: Picture): void;
}

export interface PainterStats {
	/** Frames drawn. */
	painted: number;
	/** Frames that waited for a decode slot and were replaced by a newer one. */
	replaced: number;
	/** Frames decoded after a newer one was already drawn, or after the stream ended. */
	discarded: number;
	/** Frames that could not be decoded. */
	failed: number;
}

/** Decodes in flight at once. Two lets a decode that is slower than a frame interval overlap the
 *  next without the frames piling up behind it. */
export const MAX_DECODES = 2;

/**
 * Decode and draw the frames of one stream, newest first. A decode is asynchronous, so a frame can
 * finish after a later one: it is dropped rather than drawn over it. While every slot is busy only the
 * newest waiting frame is kept — a picture that is already late is not worth a decode when a fresher
 * one is behind it. `stop` ends the stream: nothing still decoding is drawn, which matters because the
 * settled still takes the pane over the moment playback ends.
 */
export function createStreamPainter(deps: PainterDeps) {
	let generation = 0;
	let seq = 0;
	let lastPainted = -1;
	let inFlight = 0;
	let waiting: { seq: number; jpeg: Blob } | null = null;
	const stats: PainterStats = { painted: 0, replaced: 0, discarded: 0, failed: 0 };

	function begin(job: { seq: number; jpeg: Blob }) {
		inFlight++;
		const mine = generation;
		deps.decode(job.jpeg).then(
			(picture) => {
				inFlight--;
				try {
					if (mine === generation && job.seq > lastPainted) {
						lastPainted = job.seq;
						deps.paint(picture);
						stats.painted++;
					} else stats.discarded++;
				} catch {
					// A canvas that cannot be drawn on costs this frame, not the stream.
					stats.failed++;
				} finally {
					picture.close();
				}
				next();
			},
			() => {
				inFlight--;
				stats.failed++;
				next();
			}
		);
	}

	function next() {
		if (waiting && inFlight < MAX_DECODES) {
			const job = waiting;
			waiting = null;
			begin(job);
		}
	}

	return {
		push(jpeg: Blob) {
			const job = { seq: seq++, jpeg };
			if (inFlight < MAX_DECODES) begin(job);
			else {
				if (waiting) stats.replaced++;
				waiting = job;
			}
		},
		stop() {
			generation++;
			waiting = null;
		},
		stats: (): PainterStats => ({ ...stats })
	};
}
