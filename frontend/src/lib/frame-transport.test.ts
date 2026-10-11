import { describe, expect, test } from 'bun:test';
import {
	CANVAS_PAINTER,
	createStreamPainter,
	jpegBlob,
	jpegDataUrl,
	MAX_DECODES,
	parsePlaybackMessage,
	PLAYBACK_HEADER_BYTES,
	toBase64,
	toBase64Chunked,
	type Picture
} from './frame-transport';

// The bytes `playback_message(1.5, [FF D8 01 02 FF D9])` makes in kerf-app
// (`a_playback_message_is_the_time_then_the_jpeg`): 1.5 as a little-endian f64, then the frame.
const SENT = [0, 0, 0, 0, 0, 0, 0xf8, 0x3f, 0xff, 0xd8, 1, 2, 0xff, 0xd9];

describe('parsePlaybackMessage', () => {
	test('reads the time and leaves the JPEG alone', () => {
		const message = parsePlaybackMessage(new Uint8Array(SENT).buffer);
		expect(message?.time).toBe(1.5);
		expect([...message!.jpeg]).toEqual([0xff, 0xd8, 1, 2, 0xff, 0xd9]);
	});

	test('the JPEG is a view into the message, not a copy of it', () => {
		const buffer = new Uint8Array(SENT).buffer;
		const message = parsePlaybackMessage(buffer)!;
		expect(message.jpeg.buffer).toBe(buffer);
		expect(message.jpeg.byteOffset).toBe(PLAYBACK_HEADER_BYTES);
	});

	test('takes a view of a larger buffer at an offset, as a channel may hand over', () => {
		const padded = new Uint8Array([9, 9, 9, ...SENT, 7, 7]);
		const view = new Uint8Array(padded.buffer, 3, SENT.length);
		const message = parsePlaybackMessage(view);
		expect(message?.time).toBe(1.5);
		expect(message?.jpeg.byteLength).toBe(6);
	});

	test('reads any time back exactly', () => {
		for (const time of [0, 0.041666666666666664, 12.5, 3599.9999, 1e6]) {
			const bytes = new Uint8Array(PLAYBACK_HEADER_BYTES + 3);
			new DataView(bytes.buffer).setFloat64(0, time, true);
			bytes.set([0xff, 0xd8, 0xd9], PLAYBACK_HEADER_BYTES);
			expect(parsePlaybackMessage(bytes.buffer)?.time).toBe(time);
		}
	});

	test('is not a frame: a bare header, nothing, text, a time that is not a number', () => {
		expect(parsePlaybackMessage(new Uint8Array(PLAYBACK_HEADER_BYTES).buffer)).toBeNull();
		expect(parsePlaybackMessage(new ArrayBuffer(0))).toBeNull();
		expect(parsePlaybackMessage(new ArrayBuffer(3))).toBeNull();
		expect(parsePlaybackMessage('data:image/jpeg;base64,/9j/')).toBeNull();
		expect(parsePlaybackMessage({ time: 1, jpeg: 'x' })).toBeNull();
		expect(parsePlaybackMessage(null)).toBeNull();
		const nan = new Uint8Array(PLAYBACK_HEADER_BYTES + 2);
		new DataView(nan.buffer).setFloat64(0, NaN, true);
		expect(parsePlaybackMessage(nan.buffer)).toBeNull();
	});
});

describe('the data URL the pane shows', () => {
	const sizes = [0, 1, 2, 3, 4, 100, 0x7fff, 0x8000, 0x8001, 0x10000, 123_457];
	const bytes = (n: number) => Uint8Array.from({ length: n }, (_, i) => (i * 31 + 7) & 0xff);

	test('is the frame in base64 behind the image/jpeg prefix, whatever its length', () => {
		for (const n of sizes) {
			const b = bytes(n);
			expect(toBase64Chunked(b)).toBe(Buffer.from(b).toString('base64'));
			expect(toBase64(b)).toBe(Buffer.from(b).toString('base64'));
			expect(jpegDataUrl(b)).toBe(`data:image/jpeg;base64,${Buffer.from(b).toString('base64')}`);
		}
	});

	test('reads a view into a larger message, as the channel hands the JPEG over', () => {
		const message = new Uint8Array(SENT).buffer;
		const view = parsePlaybackMessage(message)!.jpeg;
		expect(jpegDataUrl(view)).toBe(`data:image/jpeg;base64,${Buffer.from([0xff, 0xd8, 1, 2, 0xff, 0xd9]).toString('base64')}`);
	});

	test('falls back to the chunked btoa where the webview has no toBase64', () => {
		const b = bytes(70_000);
		const own = Object.getOwnPropertyDescriptor(Uint8Array.prototype, 'toBase64');
		try {
			Object.defineProperty(Uint8Array.prototype, 'toBase64', { value: undefined, configurable: true, writable: true });
			expect(toBase64(b)).toBe(Buffer.from(b).toString('base64'));
		} finally {
			if (own) Object.defineProperty(Uint8Array.prototype, 'toBase64', own);
		}
	});

	test('the canvas painter is off until a webview is measured where it wins', () => {
		expect(CANVAS_PAINTER).toBe(false);
	});
});

describe('jpegBlob', () => {
	test('holds the frame’s bytes as an image/jpeg', async () => {
		const message = parsePlaybackMessage(new Uint8Array(SENT).buffer)!;
		const blob = jpegBlob(message.jpeg);
		expect(blob.type).toBe('image/jpeg');
		expect([...new Uint8Array(await blob.arrayBuffer())]).toEqual([0xff, 0xd8, 1, 2, 0xff, 0xd9]);
	});
});

/** A decode the test settles by hand, in whatever order it likes. */
function decoder() {
	const pending: { name: string; resolve: (p: Picture) => void; reject: () => void }[] = [];
	const closed: string[] = [];
	return {
		pending,
		closed,
		decode: (jpeg: Uint8Array<ArrayBuffer>) =>
			new Promise<Picture>((resolve, reject) => {
				const name = String.fromCharCode(...jpeg);
				pending.push({
					name,
					resolve: () => resolve({ source: {} as CanvasImageSource, width: 640, height: 360, close: () => closed.push(name) }),
					reject: () => reject(new Error('bad frame'))
				});
			})
	};
}

const frame = (name: string) => new Uint8Array([...name].map((c) => c.charCodeAt(0)));
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

describe('createStreamPainter', () => {
	test('draws a frame once it is decoded, then lets go of it', async () => {
		const d = decoder();
		const drawn: string[] = [];
		const painter = createStreamPainter({ decode: d.decode, paint: () => drawn.push(d.closed.length ? 'after' : 'during') });
		painter.push(frame('a'));
		d.pending[0].resolve({} as Picture);
		await tick();
		expect(drawn).toEqual(['during']);
		expect(d.closed).toEqual(['a']);
		expect(painter.stats()).toEqual({ painted: 1, replaced: 0, discarded: 0, failed: 0 });
	});

	test('keeps at most MAX_DECODES in flight and only the newest of those waiting', async () => {
		const d = decoder();
		const drawn: string[] = [];
		const painter = createStreamPainter({ decode: d.decode, paint: () => drawn.push(d.pending[0]?.name ?? '') });
		for (const name of ['a', 'b', 'c', 'd', 'e']) painter.push(frame(name));
		expect(d.pending.map((p) => p.name)).toEqual(['a', 'b'].slice(0, MAX_DECODES));
		expect(painter.stats().replaced).toBe(2);
		// A slot frees: the newest waiting frame ('e') goes in, not 'c' or 'd'.
		d.pending[0].resolve({} as Picture);
		await tick();
		expect(d.pending.map((p) => p.name)).toEqual(['a', 'b', 'e']);
	});

	test('a frame that finishes after a newer one is dropped, not drawn over it', async () => {
		const d = decoder();
		const painted: Picture[] = [];
		const painter = createStreamPainter({ decode: d.decode, paint: (p) => painted.push(p) });
		painter.push(frame('old'));
		painter.push(frame('new'));
		d.pending[1].resolve({} as Picture);
		await tick();
		d.pending[0].resolve({} as Picture);
		await tick();
		expect(painted).toHaveLength(1);
		expect(d.closed).toEqual(['new', 'old']);
		expect(painter.stats()).toEqual({ painted: 1, replaced: 0, discarded: 1, failed: 0 });
	});

	test('nothing is drawn after stop: the settled still owns the pane by then', async () => {
		const d = decoder();
		const painted: Picture[] = [];
		const painter = createStreamPainter({ decode: d.decode, paint: (p) => painted.push(p) });
		painter.push(frame('a'));
		painter.push(frame('b'));
		painter.push(frame('c'));
		painter.stop();
		for (const p of [...d.pending]) p.resolve({} as Picture);
		await tick();
		expect(painted).toHaveLength(0);
		expect(d.closed.sort()).toEqual(['a', 'b']);
		// ...and the frame that was waiting was never decoded.
		expect(d.pending.map((p) => p.name)).toEqual(['a', 'b']);
	});

	test('a frame that cannot be decoded is dropped and the next one is drawn', async () => {
		const d = decoder();
		const painted: Picture[] = [];
		const painter = createStreamPainter({ decode: d.decode, paint: (p) => painted.push(p) });
		painter.push(frame('bad'));
		painter.push(frame('good'));
		d.pending[0].reject();
		await tick();
		d.pending[1].resolve({} as Picture);
		await tick();
		expect(painted).toHaveLength(1);
		expect(painter.stats()).toEqual({ painted: 1, replaced: 0, discarded: 0, failed: 1 });
	});

	test('a painter that throws still lets go of the picture and keeps going', async () => {
		const d = decoder();
		let calls = 0;
		const painter = createStreamPainter({
			decode: d.decode,
			paint: () => {
				if (calls++ === 0) throw new Error('canvas gone');
			}
		});
		painter.push(frame('a'));
		painter.push(frame('b'));
		painter.push(frame('c'));
		d.pending[0].resolve({} as Picture);
		await tick();
		d.pending[1].resolve({} as Picture);
		d.pending[2].resolve({} as Picture);
		await tick();
		expect(d.closed.sort()).toEqual(['a', 'b', 'c']);
		expect(painter.stats()).toEqual({ painted: 2, replaced: 0, discarded: 0, failed: 1 });
	});
});
