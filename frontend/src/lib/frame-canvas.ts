/* The browser half of the canvas painter (`frame-transport.ts` is the part a test can run, and says
 * when this is used): decode a JPEG off the main thread and draw it on a canvas. */

import { jpegBlob, jpegDataUrl, type Picture } from './frame-transport';
import { logFrontend } from './log';

/** `createImageBitmap` is what decodes a frame. A webview that lacks it, or whose copy fails on frame
 *  after frame that the `<img>` route reads, falls back to the data URL through an `Image` (the CSP
 *  admits `data:` images and no `blob:`) and stays there. One failure is not that: a damaged frame fails
 *  both routes, and a bitmap that failed once may not fail again. */
let bitmaps = typeof createImageBitmap === 'function';
let failures = 0;

/** Consecutive frames only the image route could read before the bitmap route is given up for good. */
export const BITMAP_GIVE_UP = 5;

async function viaImage(jpeg: Uint8Array<ArrayBuffer>): Promise<Picture> {
	const image = new Image();
	await new Promise<void>((resolve, reject) => {
		image.onload = () => resolve();
		image.onerror = () => reject(new Error('the frame could not be loaded'));
		image.src = jpegDataUrl(jpeg);
	});
	return { source: image, width: image.naturalWidth, height: image.naturalHeight, close() {} };
}

/** A JPEG as a picture the canvas can draw. */
export async function decodeJpeg(jpeg: Uint8Array<ArrayBuffer>): Promise<Picture> {
	if (!bitmaps) return viaImage(jpeg);
	try {
		const bitmap = await createImageBitmap(jpegBlob(jpeg));
		failures = 0;
		return { source: bitmap, width: bitmap.width, height: bitmap.height, close: () => bitmap.close() };
	} catch {
		// A damaged frame fails the same way and fails the image route too, which rethrows and drops
		// just that frame; only frames the image route reads say the bitmap route is the broken one.
		const picture = await viaImage(jpeg);
		if (++failures >= BITMAP_GIVE_UP) {
			bitmaps = false;
			logFrontend('warn', 'createImageBitmap could not decode playback frames; decoding through an image instead', 'playback');
		}
		return picture;
	}
}

/** Draw a picture on the canvas, which takes the picture's size (a stream rendered at another
 *  width changes it; the page shows it `object-fit: contain`, so the box stays put). */
export function paintPicture(canvas: HTMLCanvasElement, picture: Picture): void {
	if (canvas.width !== picture.width || canvas.height !== picture.height) {
		canvas.width = picture.width;
		canvas.height = picture.height;
	}
	canvas.getContext('2d', { alpha: false })?.drawImage(picture.source, 0, 0);
}
