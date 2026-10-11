/* The browser half of the playback transport (`frame-transport.ts` is the part a test can run): decode a
 * JPEG off the main thread and draw it on the preview's canvas. */

import type { Picture } from './frame-transport';

/** `createImageBitmap` is what decodes a frame. A webview that lacks it, or whose copy cannot read a
 *  frame the `<img>` route can, falls back to the data URL the page used before this transport existed
 *  (the CSP admits `data:` images and no `blob:`), and stays there: a decode that throws on every frame
 *  would otherwise cost an exception per frame as well. */
let bitmaps = typeof createImageBitmap === 'function';

async function viaImage(jpeg: Blob): Promise<Picture> {
	const url = await new Promise<string>((resolve, reject) => {
		const reader = new FileReader();
		reader.onload = () => resolve(String(reader.result));
		reader.onerror = () => reject(reader.error);
		reader.readAsDataURL(jpeg);
	});
	const image = new Image();
	image.src = url;
	await image.decode();
	return { source: image, width: image.naturalWidth, height: image.naturalHeight, close() {} };
}

/** A JPEG as a picture the canvas can draw. */
export async function decodeJpeg(jpeg: Blob): Promise<Picture> {
	if (!bitmaps) return viaImage(jpeg);
	try {
		const bitmap = await createImageBitmap(jpeg);
		return { source: bitmap, width: bitmap.width, height: bitmap.height, close: () => bitmap.close() };
	} catch {
		// A damaged frame fails the same way and fails the image route too, which rethrows and drops
		// just that frame; only a frame the image route reads proves the bitmap route is the broken one.
		const picture = await viaImage(jpeg);
		bitmaps = false;
		console.warn('createImageBitmap could not decode a playback frame; decoding through an image instead');
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
