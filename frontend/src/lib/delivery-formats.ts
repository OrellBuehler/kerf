/* The delivery frames a project can be cut for.
 *
 * The frame is a property of the *project*, not of an export: it decides the
 * shape of every rendered picture, so a 9:16 crop is visible while cutting
 * rather than discovered in the rendered file. `null` follows the footage.
 *
 * Sizes match the corresponding export presets so picking "Reels / Shorts"
 * here and exporting with that preset render the identical frame. */

import type { Delivery, Fit, Timeline } from './types';

export interface DeliveryPreset {
	id: string;
	label: string;
	hint: string;
	format: Delivery | null;
}

export const DELIVERY_PRESETS: DeliveryPreset[] = [
	{ id: 'source', label: 'Source', hint: 'Follow the footage', format: null },
	{ id: 'landscape', label: '16:9', hint: '1920×1080 — YouTube, web', format: { width: 1920, height: 1080, fit: 'contain' } },
	{ id: 'vertical', label: '9:16', hint: '1080×1920 — Reels, Shorts, TikTok', format: { width: 1080, height: 1920, fit: 'cover' } },
	{ id: 'square', label: '1:1', hint: '1080×1080 — feed post', format: { width: 1080, height: 1080, fit: 'cover' } },
	{ id: 'portrait', label: '4:5', hint: '1080×1350 — Instagram portrait', format: { width: 1080, height: 1350, fit: 'cover' } }
];

/** Whether the cut has a picture: a clip on a video track. Delivery shapes only
 *  mean something for one — an audio-only cut is a single file whatever was
 *  ticked. The export dialog and the Deliver panel gate on this together. */
export function hasPicture(timeline: Timeline): boolean {
	return timeline.tracks.some((t) => t.kind === 'video' && t.clips.length > 0);
}

/** The shapes a multi-format export can write a file for — every preset that
 *  is a frame (so not "Source"), in picker order. */
export const VARIANT_PRESETS: DeliveryPreset[] = DELIVERY_PRESETS.filter((p) => p.format !== null);

/** The frames for the ticked shape ids, in picker order whatever order they
 *  were ticked in; an id that is not a shape is ignored. */
export function formatsFor(ids: readonly string[]): Delivery[] {
	return VARIANT_PRESETS.filter((p) => ids.includes(p.id)).map((p) => p.format as Delivery);
}

/** The preset matching a timeline's format, falling back to a custom entry. */
export function presetFor(format: Delivery | null | undefined): DeliveryPreset {
	if (!format) return DELIVERY_PRESETS[0];
	const hit = DELIVERY_PRESETS.find(
		(p) => p.format && p.format.width === format.width && p.format.height === format.height
	);
	if (hit) return hit;
	return {
		id: 'custom',
		label: ratioLabel(format.width, format.height),
		hint: `${format.width}×${format.height}`,
		format
	};
}

/** A reduced "w:h" label, e.g. 1080×1920 -> "9:16". */
export function ratioLabel(width: number, height: number): string {
	const gcd = (a: number, b: number): number => (b ? gcd(b, a % b) : a);
	const d = gcd(width, height) || 1;
	return `${width / d}:${height / d}`;
}

/** Where a multi-format export puts one delivery: beside `base`, its shape in
 *  the name — `cut.mp4` at 9:16 is `cut-9x16.mp4`. Mirrors
 *  `ExportVariant::beside`; an `x` because `:` is not a filename character on
 *  Windows. */
export function variantPath(base: string, format: Delivery): string {
	const shape = ratioLabel(format.width, format.height).replace(':', 'x');
	const m = base.match(/^(.*?)(\.[^./\\]+)?$/);
	const stem = m?.[1] ?? base;
	const ext = m?.[2] ?? '';
	return `${stem}-${shape}${ext}`;
}

export function fitLabel(fit: Fit): string {
	return fit === 'cover' ? 'fill & crop' : fit === 'blur' ? 'fit & blurred fill' : 'fit & letterbox';
}

/** The fits a delivery frame can take, in picker order. */
export const DELIVERY_FITS: Fit[] = ['cover', 'contain', 'blur'];

/** What the delivery-frame button says of a project cut for `format`: its size and the project's
 *  own fit — not the matching preset's, which only shares the size. */
export function deliveryTip(format: Delivery | null | undefined): string {
	const rate = format?.fps != null ? ` at ${fpsLabel(format.fps)} fps` : '';
	return format
		? `Delivering ${format.width}\u00d7${format.height}${rate} — ${fitLabel(format.fit)}. Click to change.`
		: 'The frame follows the footage. Click to cut for a delivery shape.';
}

/** The fits the delivery-frame menu offers for `format`, the project's own one ticked; none
 *  without a frame. */
export function fitChoices(format: Delivery | null | undefined): { fit: Fit; label: string; checked: boolean }[] {
	if (!format) return [];
	return DELIVERY_FITS.map((fit) => ({ fit, label: `Footage: ${fitLabel(fit)}`, checked: fit === format.fit }));
}

// ---- the delivery frame rate ------------------------------------------------
//
// The rate belongs to the frame (`Delivery.fps`): an export, the playback and the stills default to it when
// the export names none, instead of the rate of whichever clip is first. Absent follows the footage.

/** The slowest and the fastest rate a delivery may name (`MIN_DELIVERY_FPS` / `MAX_DELIVERY_FPS`). */
export const MIN_DELIVERY_FPS = 1;
export const MAX_DELIVERY_FPS = 240;

/** How near a rate must be to an NTSC one (`n * 1000 / 1001`) to be taken for it (`NTSC_SNAP`). */
const NTSC_SNAP = 0.005;
const NTSC_BASES = [24, 30, 48, 60, 120, 240];

/** A delivery frame rate, checked — the mirror of kerf-core's `check_delivery_fps`: finite and within
 *  1..240, with an NTSC rate written short (`23.976`, `29.97`, `59.94`) made the exact `n * 1000 / 1001`.
 *  Throws what the backend refuses with. */
export function checkDeliveryFps(fps: number): number {
	if (!Number.isFinite(fps) || fps < MIN_DELIVERY_FPS || fps > MAX_DELIVERY_FPS) {
		throw new Error(`a delivery frame rate must be between ${MIN_DELIVERY_FPS} and ${MAX_DELIVERY_FPS} fps, got ${fps}`);
	}
	const ntsc = NTSC_BASES.map((b) => (b * 1000) / 1001).find((n) => Math.abs(fps - n) < NTSC_SNAP);
	return ntsc ?? fps;
}

/** A frame rate as people write it: `50`, `29.97`, `23.976`. */
export function fpsLabel(fps: number): string {
	return String(Number(fps.toFixed(3)));
}

/** Whether two rates are the same one — `29.97` and the exact `30000/1001` are. `null` / `undefined` is
 *  "follows the footage", the same as itself only. */
export function sameRate(a: number | null | undefined, b: number | null | undefined): boolean {
	if (a == null || b == null) return a == null && b == null;
	return Math.abs(a - b) < NTSC_SNAP;
}

export interface DeliveryRate {
	/** `null` follows the footage. */
	fps: number | null;
	label: string;
}

/** The rates the delivery-frame menus offer: the footage's own, the usual whole rates, and the NTSC (drop-frame)
 *  ones beside the whole rate they sit under. */
export const DELIVERY_RATES: DeliveryRate[] = [
	{ fps: null, label: 'Source' },
	{ fps: 23.976, label: '23.976' },
	{ fps: 24, label: '24' },
	{ fps: 25, label: '25' },
	{ fps: 29.97, label: '29.97' },
	{ fps: 30, label: '30' },
	{ fps: 50, label: '50' },
	{ fps: 59.94, label: '59.94' },
	{ fps: 60, label: '60' }
];

/** The rates the delivery-frame menu offers for `format`, the project's own one ticked — with it among them
 *  when the list lacks it (a rate an agent set), so something is always ticked; none without a frame. */
export function rateChoices(format: Delivery | null | undefined): (DeliveryRate & { checked: boolean })[] {
	if (!format) return [];
	const fps = format.fps ?? null;
	const rates = DELIVERY_RATES.some((r) => sameRate(r.fps, fps)) ? DELIVERY_RATES : [...DELIVERY_RATES, { fps, label: fpsLabel(fps ?? 0) }];
	return rates.map((r) => ({ ...r, checked: sameRate(r.fps, fps) }));
}

/** `format` delivered at `fps` (`null` follows the footage). */
export function withRate(format: Delivery, fps: number | null): Delivery {
	const { fps: _old, ...rest } = format;
	return fps == null ? rest : { ...rest, fps };
}

/** `format` — a new shape, a preset — delivered at the rate `from` (the frame the project is cut for now) has: the rate
 *  is the project's, and cutting for another shape does not take it away. */
export function keepRate(format: Delivery | null, from: Delivery | null | undefined): Delivery | null {
	return format && from?.fps != null ? withRate(format, from.fps) : format;
}

/** What the delivery-frame button says: the shape, and the rate after it when the frame states one. */
export function deliveryLabel(format: Delivery | null | undefined): string {
	const shape = presetFor(format).label;
	return format?.fps != null ? `${shape} · ${fpsLabel(format.fps)} fps` : shape;
}

/** The frame rate the Deliver panel states for the cut — `fps` is the rate it plays and exports at — and where it comes
 *  from: the delivery frame's own, or (a frame that states none) the first clip's, which is the surprise to avoid. */
export function rateLine(format: Delivery | null | undefined, fps: number): { text: string; tip: string } {
	if (format?.fps != null) {
		return {
			text: `${fpsLabel(format.fps)} fps`,
			tip: 'The delivery frame’s rate: every export, the preview and the stills use it unless an export picks another.'
		};
	}
	return {
		text: `${fpsLabel(fps)} fps · first clip`,
		tip: format
			? 'No frame rate is set, so an export takes the first clip’s. Pick one in the timeline’s delivery frame menu.'
			: 'No frame rate is set, so an export takes the first clip’s. Pick a delivery shape in the timeline to set one.'
	};
}

/** The history label the backend writes for a delivery frame (`Project::set_delivery_format`). */
export function deliverLabel(format: Delivery | null | undefined): string {
	if (!format) return 'Deliver at source shape';
	const size = `${format.width}x${format.height}`;
	return format.fps != null ? `Deliver ${size} at ${fpsLabel(format.fps)} fps` : `Deliver ${size}`;
}
