// The app's one filmstrip cache: `FilmstripCache` wired to the backend's
// `get_filmstrip`, to a browser decoder for its JPEG sheets, and to the
// notification log — so an asset whose thumbnails cannot be made says so once
// rather than silently drawing nothing.

import { getFilmstrip } from './api';
import { FilmstripCache, type SheetDecoder } from './filmstrip-cache';
import { toast } from './notifications.svelte';
import { editor } from './state.svelte';

/**
 * Decode one sheet's `data:` URL into something a canvas blits from without
 * decoding it again. An `HTMLImageElement` is decoded first (`decode()` resolves
 * once the pixels exist, and an `<img>` is the one thing every webview can read a
 * `data:` URL into — `fetch` on one is a `connect-src` question the CSP answers no
 * to). It is then handed to `createImageBitmap` where the engine has it: a bitmap
 * *is* the decoded pixels, held until closed, while an element's decoded copy is
 * the browser's to drop and re-make on a later draw. An engine that cannot (or
 * will not, for this source) keeps the element.
 */
export const decodeSheet: SheetDecoder = async (sheet) => {
	const img = new Image();
	img.decoding = 'async';
	img.src = sheet.data_url;
	await img.decode();
	const w = img.naturalWidth || sheet.width;
	const h = img.naturalHeight || sheet.height;
	const bytes = w * h * 4;
	if (typeof createImageBitmap === 'function') {
		try {
			const bitmap = await createImageBitmap(img);
			img.src = '';
			return { image: bitmap, bytes, close: () => bitmap.close() };
		} catch {
			// keep the element
		}
	}
	return { image: img, bytes };
};

export const filmstrips = new FilmstripCache(getFilmstrip, decodeSheet, {
	onFail: (assetId, message) => toast.warning(`Couldn't make thumbnails for ${editor.assetName(assetId)} — ${message}`)
});
