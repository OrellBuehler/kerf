// The browser harness's stand-in for a decoded frame. There is no decoder
// outside the desktop app, so playback and the settled frame are drawn here — an
// image, not interface: its colors are picture content (like the filmstrip
// sample next to it), which is why the theme guard lets this file keep literals.

/** A frame that visibly moves, so a frozen preview looks frozen. */
export function sampleFrameUrl(time: number): string {
	const x = (((time * 90) % 700) - 60).toFixed(1);
	const svg =
		`<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360">` +
		`<rect width="640" height="360" fill="#0d1116"/>` +
		`<rect x="${x}" y="150" width="60" height="60" fill="#e29d2e"/>` +
		`<text x="20" y="336" fill="#8fa3b8" font-family="monospace" font-size="22">${time.toFixed(2)}s</text>` +
		`</svg>`;
	return `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`;
}

let scratch: HTMLCanvasElement | null = null;

/** [`sampleFrameUrl`]'s picture as the JPEG bytes the desktop app's playback stream sends, so the
 *  harness drives the same decode-and-draw path (`null` where there is no canvas to draw it on). */
export function sampleFrameJpeg(time: number): Promise<Blob | null> {
	if (typeof document === 'undefined') return Promise.resolve(null);
	const canvas = (scratch ??= document.createElement('canvas'));
	canvas.width = 640;
	canvas.height = 360;
	const ctx = canvas.getContext('2d');
	if (!ctx) return Promise.resolve(null);
	ctx.fillStyle = '#0d1116';
	ctx.fillRect(0, 0, 640, 360);
	ctx.fillStyle = '#e29d2e';
	ctx.fillRect((time * 90) % 700 - 60, 150, 60, 60);
	ctx.fillStyle = '#8fa3b8';
	ctx.font = '22px monospace';
	ctx.fillText(`${time.toFixed(2)}s`, 20, 336);
	return new Promise((resolve) => canvas.toBlob(resolve, 'image/jpeg', 0.7));
}
