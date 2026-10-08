// Where the Preview frame is in the window, and whether the native surface should be showing.
//
// The GPU preview draws into a native surface behind or over the Preview panel's frame; the page
// tells the backend where that frame is (device pixels, relative to the webview) and the backend
// draws into exactly that rectangle. Everything that can be decided without a DOM is here:
// the rounding, the policy of when the surface shows, the colour the backend clears with and the
// polygon that cuts a hole in the page.

import type { PreviewBoundsReport } from './types';

/** A rectangle in CSS pixels. */
export interface CssRect {
	left: number;
	top: number;
	width: number;
	height: number;
}

/** A rectangle in device pixels. */
export interface DeviceRect {
	x: number;
	y: number;
	width: number;
	height: number;
}

const finite = (v: number) => Number.isFinite(v);

/** `devicePixelRatio`, or 1 when the engine reports something unusable. */
export function safeRatio(dpr: number): number {
	return finite(dpr) && dpr > 0 ? dpr : 1;
}

/**
 * `rect` (CSS px, relative to the viewport — which is the webview) in device pixels, clamped to
 * the viewport. Both edges are rounded, not the size, so neighbouring rectangles never gap or
 * overlap at a fractional ratio (125 %, 150 %). Empty when nothing of it is on screen.
 */
export function deviceRect(rect: CssRect, dpr: number, viewport: { width: number; height: number }): DeviceRect {
	const r = safeRatio(dpr);
	const vw = Math.max(0, Math.round(viewport.width * r));
	const vh = Math.max(0, Math.round(viewport.height * r));
	const clamp = (v: number, hi: number) => Math.min(hi, Math.max(0, v));
	const x0 = clamp(Math.round(rect.left * r), vw);
	const y0 = clamp(Math.round(rect.top * r), vh);
	const x1 = clamp(Math.round((rect.left + rect.width) * r), vw);
	const y1 = clamp(Math.round((rect.top + rect.height) * r), vh);
	if (![x0, y0, x1, y1].every(finite)) return { x: 0, y: 0, width: 0, height: 0 };
	return { x: x0, y: y0, width: Math.max(0, x1 - x0), height: Math.max(0, y1 - y0) };
}

/** A `getComputedStyle` colour as `#rrggbb`, or null. Takes what engines print for a resolved
 *  colour: `rgb()` / `rgba()` (comma or space syntax), `color(srgb r g b)` — what a `color-mix`
 *  resolves to — and hex. An alpha below 1 is not a matte: null. */
export function parseCssColor(css: string): string | null {
	const text = css.trim().toLowerCase();
	const hex = (n: number) => Math.min(255, Math.max(0, Math.round(n))).toString(16).padStart(2, '0');
	const m = text.match(/^#([0-9a-f]{6})$/);
	if (m) return `#${m[1]}`;
	const short = text.match(/^#([0-9a-f])([0-9a-f])([0-9a-f])$/);
	if (short) return `#${short[1]}${short[1]}${short[2]}${short[2]}${short[3]}${short[3]}`;
	const num = '(-?\\d*\\.?\\d+)(%?)';
	const rgb = text.match(new RegExp(`^rgba?\\(\\s*${num}[\\s,]+${num}[\\s,]+${num}(?:\\s*[,/]\\s*${num})?\\s*\\)$`));
	if (rgb) {
		const alpha = rgb[7] === undefined ? 1 : Number(rgb[7]) / (rgb[8] ? 100 : 1);
		if (alpha < 1) return null;
		const ch = (v: string, pct: string) => (pct ? (Number(v) / 100) * 255 : Number(v));
		return `#${hex(ch(rgb[1], rgb[2]))}${hex(ch(rgb[3], rgb[4]))}${hex(ch(rgb[5], rgb[6]))}`;
	}
	const srgb = text.match(new RegExp(`^color\\(\\s*srgb\\s+${num}\\s+${num}\\s+${num}(?:\\s*/\\s*${num})?\\s*\\)$`));
	if (srgb) {
		const alpha = srgb[7] === undefined ? 1 : Number(srgb[7]) / (srgb[8] ? 100 : 1);
		if (alpha < 1) return null;
		const ch = (v: string, pct: string) => (pct ? Number(v) / 100 : Number(v)) * 255;
		return `#${hex(ch(srgb[1], srgb[2]))}${hex(ch(srgb[3], srgb[4]))}${hex(ch(srgb[5], srgb[6]))}`;
	}
	return null;
}

/** The report for the backend. `rect` is null when the frame is not laid out (the panel is
 *  closed or in a background tab); that is a surface that is not showing. */
export function boundsReport(args: {
	rect: CssRect | null;
	dpr: number;
	viewport: { width: number; height: number };
	visible: boolean;
	matte: string | null;
}): PreviewBoundsReport {
	const r = safeRatio(args.dpr);
	const device = args.rect ? deviceRect(args.rect, r, args.viewport) : { x: 0, y: 0, width: 0, height: 0 };
	return {
		...device,
		viewport_width: Math.max(0, Math.round(args.viewport.width * r)),
		viewport_height: Math.max(0, Math.round(args.viewport.height * r)),
		visible: args.visible && device.width > 0 && device.height > 0,
		matte: args.matte
	};
}

/** Whether two reports say the same thing (so an unchanged one is not sent again). */
export function sameReport(a: PreviewBoundsReport | null, b: PreviewBoundsReport): boolean {
	return (
		!!a &&
		a.x === b.x &&
		a.y === b.y &&
		a.width === b.width &&
		a.height === b.height &&
		a.viewport_width === b.viewport_width &&
		a.viewport_height === b.viewport_height &&
		a.visible === b.visible &&
		a.matte === b.matte
	);
}

/** What decides how the Preview frame is produced right now. */
export interface RouteInputs {
	/** The setting. */
	enabled: boolean;
	/** The platform has a surface technique. */
	supported: boolean;
	/** The page can draw over the picture on this technique. */
	overlaysCapable: boolean;
	/** Forward 1x playback is streaming JPEGs into the pane. */
	streaming: boolean;
	/** Nothing on the timeline to show. */
	empty: boolean;
	/** A title box is over the picture. */
	titlesShown: boolean;
	/** The trim monitor stands in for the playhead's frame. */
	trimMonitor: boolean;
	/** Safe-area guides are drawn over the picture. */
	guides: boolean;
	/** Something of the page (a dialog, a menu, a drag ghost) is on top of the Preview frame. A
	 *  surface that sits above the page would paint over it. */
	covered: boolean;
}

export type RouteWhy = 'off' | 'unsupported' | 'streaming' | 'empty' | 'overlays' | 'covered';

export interface Route {
	/** `gpu`: ask the backend, which may still answer with the JPEG for this frame. `jpeg`: the
	 *  FFmpeg path as it always was, and the surface stays hidden. */
	via: 'gpu' | 'jpeg';
	/** The page has something to draw over the picture (passed to the backend, which refuses
	 *  it too on a surface that sits above the page). */
	overlays: boolean;
	why?: RouteWhy;
}

/**
 * How the frame under the playhead is produced. The GPU is asked only when the setting is on,
 * the platform has a surface, the pane is not being fed a stream and the page does not have to
 * draw over a surface that covers it, nor has something of its own over the frame (a dialog, a
 * menu). Everything else is the JPEG path, unchanged.
 */
export function routePreview(i: RouteInputs): Route {
	const overlays = i.titlesShown || i.trimMonitor || i.guides;
	if (!i.enabled) return { via: 'jpeg', overlays, why: 'off' };
	if (!i.supported) return { via: 'jpeg', overlays, why: 'unsupported' };
	if (i.empty) return { via: 'jpeg', overlays, why: 'empty' };
	if (i.streaming) return { via: 'jpeg', overlays, why: 'streaming' };
	if (overlays && !i.overlaysCapable) return { via: 'jpeg', overlays, why: 'overlays' };
	if (i.covered && !i.overlaysCapable) return { via: 'jpeg', overlays, why: 'covered' };
	return { via: 'gpu', overlays };
}

/** What the status bar says when the route is the JPEG (the backend's own refusals say theirs). */
export function describeWhy(why: RouteWhy): string {
	switch (why) {
		case 'off':
			return 'the GPU preview is off';
		case 'unsupported':
			return 'this platform has no preview surface';
		case 'streaming':
			return 'playback streams through FFmpeg';
		case 'empty':
			return 'nothing on the timeline';
		case 'overlays':
			return 'the page draws over the picture, and the surface sits above the page';
		case 'covered':
			return 'a dialog or menu is over the frame, and the surface sits above the page';
	}
}

/** Whether the native surface is the one on show: the route asked for it and the last frame
 *  was drawn by it. (The backend hides a child window itself on any frame it did not draw.) */
export function surfaceShowing(route: Route, renderer: 'gpu' | 'ffmpeg' | null): boolean {
	return route.via === 'gpu' && renderer === 'gpu';
}

/**
 * A CSS `clip-path` polygon (even-odd) that is `outer` with `hole` cut out of it, both in the
 * same space, in px. Used to let the surface behind the page show through the Preview frame
 * while the pane's surround stays opaque.
 */
export function holePolygon(outer: { width: number; height: number }, hole: { x: number; y: number; width: number; height: number }): string {
	const r = (v: number) => `${Math.round(v * 100) / 100}px`;
	const x0 = Math.min(Math.max(hole.x, 0), outer.width);
	const y0 = Math.min(Math.max(hole.y, 0), outer.height);
	const x1 = Math.min(Math.max(hole.x + hole.width, 0), outer.width);
	const y1 = Math.min(Math.max(hole.y + hole.height, 0), outer.height);
	const pts = [
		[0, 0],
		[outer.width, 0],
		[outer.width, outer.height],
		[0, outer.height],
		[0, 0],
		[x0, y0],
		[x0, y1],
		[x1, y1],
		[x1, y0],
		[x0, y0]
	];
	return `polygon(evenodd, ${pts.map(([x, y]) => `${r(x)} ${r(y)}`).join(', ')})`;
}

/**
 * Points to hit-test to find out whether anything of the page is on top of the frame: a 3x3 grid
 * (corners, edge midpoints, centre) inset from the edges, in CSS pixels. A frame too small for an
 * inset gets its centre alone.
 */
export function samplePoints(rect: CssRect, inset = 3): [number, number][] {
	if (rect.width <= inset * 2 + 1 || rect.height <= inset * 2 + 1) {
		return [[rect.left + rect.width / 2, rect.top + rect.height / 2]];
	}
	const xs = [rect.left + inset, rect.left + rect.width / 2, rect.left + rect.width - inset];
	const ys = [rect.top + inset, rect.top + rect.height / 2, rect.top + rect.height - inset];
	return ys.flatMap((y) => xs.map((x) => [x, y] as [number, number]));
}

/** Whether any of `points` hits something that is not part of the frame. `hit` is
 *  `document.elementFromPoint` with the frame's `contains` folded in: true when the point lands
 *  on the frame or one of its own layers, false when something else is over it; null (no
 *  element: off the viewport) says nothing. */
export function anyCovered(points: [number, number][], hit: (x: number, y: number) => boolean | null): boolean {
	return points.some(([x, y]) => hit(x, y) === false);
}
