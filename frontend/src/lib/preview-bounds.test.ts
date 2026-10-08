import { describe, expect, test } from 'bun:test';
import {
	boundsReport,
	describeWhy,
	deviceRect,
	anyCovered,
	holePolygon,
	parseCssColor,
	routePreview,
	safeRatio,
	samplePoints,
	sameReport,
	surfaceShowing,
	type RouteInputs
} from './preview-bounds';

const view = { width: 1440, height: 900 };

describe('deviceRect', () => {
	test('is the css rectangle at a ratio of one', () => {
		expect(deviceRect({ left: 10, top: 20, width: 800, height: 450 }, 1, view)).toEqual({ x: 10, y: 20, width: 800, height: 450 });
	});

	test('scales by the device pixel ratio, 150 % and 200 % included', () => {
		expect(deviceRect({ left: 10, top: 20, width: 800, height: 450 }, 2, view)).toEqual({ x: 20, y: 40, width: 1600, height: 900 });
		expect(deviceRect({ left: 10, top: 20, width: 800, height: 450 }, 1.5, view)).toEqual({ x: 15, y: 30, width: 1200, height: 675 });
	});

	test('rounds the edges, so rectangles that touch still touch at a fractional ratio', () => {
		const a = deviceRect({ left: 0, top: 0, width: 333.4, height: 10 }, 1.25, view);
		const b = deviceRect({ left: 333.4, top: 0, width: 666.6, height: 10 }, 1.25, view);
		expect(a.x + a.width).toBe(b.x);
		// The size is the difference of the rounded edges, not a rounded size.
		const c = deviceRect({ left: 0.5, top: 0, width: 100.5, height: 10 }, 1, view);
		expect(c).toEqual({ x: 1, y: 0, width: 100, height: 10 });
	});

	test('is clamped to the viewport and empty when nothing of it is on screen', () => {
		const over = deviceRect({ left: 1400, top: 0, width: 200, height: 100 }, 1, view);
		expect(over).toEqual({ x: 1400, y: 0, width: 40, height: 100 });
		expect(deviceRect({ left: 1500, top: 0, width: 200, height: 100 }, 1, view).width).toBe(0);
		expect(deviceRect({ left: -50, top: -10, width: 100, height: 100 }, 1, view)).toEqual({ x: 0, y: 0, width: 50, height: 90 });
	});

	test('survives numbers a layout engine should never give', () => {
		expect(deviceRect({ left: NaN, top: 0, width: 10, height: 10 }, 1, view).width).toBe(0);
		expect(deviceRect({ left: 0, top: 0, width: Infinity, height: 10 }, 1, view)).toEqual({ x: 0, y: 0, width: 1440, height: 10 });
		expect(safeRatio(0)).toBe(1);
		expect(safeRatio(NaN)).toBe(1);
		expect(safeRatio(-2)).toBe(1);
		expect(safeRatio(2)).toBe(2);
	});
});

describe('boundsReport', () => {
	const rect = { left: 10, top: 20, width: 800, height: 450 };

	test('carries the rectangle, the webview size and the matte in device pixels', () => {
		expect(boundsReport({ rect, dpr: 1.5, viewport: view, visible: true, matte: '#000000' })).toEqual({
			x: 15,
			y: 30,
			width: 1200,
			height: 675,
			viewport_width: 2160,
			viewport_height: 1350,
			visible: true,
			matte: '#000000'
		});
	});

	test('a frame that is not laid out, or empty, is a surface that is not showing', () => {
		expect(boundsReport({ rect: null, dpr: 1, viewport: view, visible: true, matte: null }).visible).toBe(false);
		expect(boundsReport({ rect: { ...rect, width: 0 }, dpr: 1, viewport: view, visible: true, matte: null }).visible).toBe(false);
		expect(boundsReport({ rect, dpr: 1, viewport: view, visible: false, matte: null }).visible).toBe(false);
	});

	test('an unchanged report is recognised, a changed one is not', () => {
		const a = boundsReport({ rect, dpr: 1, viewport: view, visible: true, matte: '#000000' });
		expect(sameReport(null, a)).toBe(false);
		expect(sameReport({ ...a }, a)).toBe(true);
		for (const key of ['x', 'y', 'width', 'height', 'viewport_width', 'viewport_height'] as const) {
			expect(sameReport({ ...a, [key]: a[key] + 1 }, a)).toBe(false);
		}
		expect(sameReport({ ...a, visible: false }, a)).toBe(false);
		expect(sameReport({ ...a, matte: '#111111' }, a)).toBe(false);
	});
});

describe('parseCssColor', () => {
	test('reads what engines print for a resolved colour', () => {
		expect(parseCssColor('rgb(0, 0, 0)')).toBe('#000000');
		expect(parseCssColor('rgb(15 19 24)')).toBe('#0f1318');
		expect(parseCssColor('rgba(15, 19, 24, 1)')).toBe('#0f1318');
		expect(parseCssColor('rgb(15 19 24 / 100%)')).toBe('#0f1318');
		expect(parseCssColor('color(srgb 0.0588 0.0745 0.0941)')).toBe('#0f1318');
		expect(parseCssColor('color(srgb 1 1 1 / 1)')).toBe('#ffffff');
		expect(parseCssColor('#0F1318')).toBe('#0f1318');
		expect(parseCssColor('#fff')).toBe('#ffffff');
		expect(parseCssColor('rgb(100%, 0%, 50%)')).toBe('#ff0080');
	});

	test('refuses anything translucent or not a colour', () => {
		for (const bad of ['', 'transparent', 'rgba(0, 0, 0, 0)', 'rgba(0, 0, 0, 0.5)', 'color(srgb 0 0 0 / 0.2)', 'red', 'var(--x)', 'hsl(0 0% 0%)']) {
			expect(parseCssColor(bad)).toBeNull();
		}
	});
});

describe('routePreview', () => {
	const on: RouteInputs = {
		enabled: true,
		supported: true,
		overlaysCapable: false,
		streaming: false,
		empty: false,
		titlesShown: false,
		trimMonitor: false,
		guides: false,
		covered: false
	};

	test('with the setting off the route is the JPEG whatever else is true', () => {
		for (const patch of [{}, { overlaysCapable: true }, { streaming: true }, { empty: true }, { guides: true }]) {
			expect(routePreview({ ...on, ...patch, enabled: false }).via).toBe('jpeg');
		}
		expect(routePreview({ ...on, enabled: false }).why).toBe('off');
	});

	test('a platform without a surface technique is the JPEG', () => {
		expect(routePreview({ ...on, supported: false })).toMatchObject({ via: 'jpeg', why: 'unsupported' });
	});

	test('a plain frame goes to the GPU', () => {
		expect(routePreview(on)).toEqual({ via: 'gpu', overlays: false });
	});

	test('playback and an empty timeline keep the surface out of it', () => {
		expect(routePreview({ ...on, streaming: true })).toMatchObject({ via: 'jpeg', why: 'streaming' });
		expect(routePreview({ ...on, empty: true })).toMatchObject({ via: 'jpeg', why: 'empty' });
	});

	test('anything the page draws over the picture needs a surface under it', () => {
		for (const patch of [{ titlesShown: true }, { trimMonitor: true }, { guides: true }]) {
			expect(routePreview({ ...on, ...patch })).toMatchObject({ via: 'jpeg', overlays: true, why: 'overlays' });
			expect(routePreview({ ...on, ...patch, overlaysCapable: true })).toEqual({ via: 'gpu', overlays: true });
		}
	});

	test('a dialog or menu over the frame keeps a surface that sits above the page out of it', () => {
		expect(routePreview({ ...on, covered: true })).toMatchObject({ via: 'jpeg', why: 'covered' });
		// Under a transparent webview the page draws over the surface, so nothing is covered.
		expect(routePreview({ ...on, covered: true, overlaysCapable: true })).toEqual({ via: 'gpu', overlays: false });
	});

	test('every reason the route is the JPEG has a sentence for the status bar', () => {
		for (const why of ['off', 'unsupported', 'streaming', 'empty', 'overlays', 'covered'] as const) {
			expect(describeWhy(why).length).toBeGreaterThan(10);
		}
	});

	test('the surface is on show only for a frame the GPU drew on a GPU route', () => {
		const gpu = routePreview(on);
		const jpeg = routePreview({ ...on, enabled: false });
		expect(surfaceShowing(gpu, 'gpu')).toBe(true);
		expect(surfaceShowing(gpu, 'ffmpeg')).toBe(false);
		expect(surfaceShowing(gpu, null)).toBe(false);
		expect(surfaceShowing(jpeg, 'gpu')).toBe(false);
	});
});

describe('holePolygon', () => {
	test('is the outer rectangle with an even-odd hole', () => {
		expect(holePolygon({ width: 100, height: 80 }, { x: 10, y: 20, width: 50, height: 30 })).toBe(
			'polygon(evenodd, 0px 0px, 100px 0px, 100px 80px, 0px 80px, 0px 0px, 10px 20px, 10px 50px, 60px 50px, 60px 20px, 10px 20px)'
		);
	});

	test('a hole past the edge is held inside it', () => {
		expect(holePolygon({ width: 100, height: 80 }, { x: 90, y: -5, width: 50, height: 30 })).toContain('100px 25px');
	});
});

describe('covering', () => {
	const rect = { left: 100, top: 50, width: 400, height: 225 };

	test('a grid of points inset from the edges, corners and middle included', () => {
		const pts = samplePoints(rect);
		expect(pts).toHaveLength(9);
		expect(pts[0]).toEqual([103, 53]);
		expect(pts[4]).toEqual([300, 162.5]);
		expect(pts[8]).toEqual([497, 272]);
		for (const [x, y] of pts) {
			expect(x).toBeGreaterThan(rect.left);
			expect(x).toBeLessThan(rect.left + rect.width);
			expect(y).toBeGreaterThan(rect.top);
			expect(y).toBeLessThan(rect.top + rect.height);
		}
	});

	test('a frame too small for an inset is sampled at its centre', () => {
		expect(samplePoints({ left: 10, top: 10, width: 6, height: 40 })).toEqual([[13, 30]]);
	});

	test('one point on something else is a covered frame; off-screen points say nothing', () => {
		const pts = samplePoints(rect);
		expect(anyCovered(pts, () => true)).toBe(false);
		expect(anyCovered(pts, () => null)).toBe(false);
		// A dialog over the lower-right corner only.
		expect(anyCovered(pts, (x, y) => !(x > 450 && y > 250))).toBe(true);
	});
});
