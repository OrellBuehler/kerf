// Designed thumbnails: pictures made beside the cut and never drawn in it (`kerf_core::Thumbnail`). The sizes,
// the default shape, the headline placements the panel offers, and the browser harness's mirror of
// `prepare_thumbnail` (it draws nothing: a layer's artwork is the engine's to make).
import type { Delivery, SuggestedFrame, Thumbnail, ThumbnailLayer, ThumbnailLayerSpec, ThumbnailSpec, Timeline } from './types';

export const THUMBNAIL_SIZES = [
	{ id: 'youtube', label: 'YouTube', width: 1280, height: 720 },
	{ id: 'vertical', label: 'Vertical', width: 1080, height: 1920 },
	{ id: 'square', label: 'Square', width: 1080, height: 1080 },
	{ id: '4:5', label: '4:5', width: 1080, height: 1350 }
] as const;

export const MIN_THUMBNAIL_SIDE = 16;
export const MAX_THUMBNAIL_SIDE = 4096;
export const MAX_THUMBNAILS = 20;
export const MAX_THUMBNAIL_LAYERS = 12;

/** The size a shape name or `WxH` stands for — mirrors `parse_thumbnail_size` (`16:9` is 1920x1080). */
export function parseThumbnailSize(name: string): [number, number] {
	const n = name.trim().toLowerCase();
	const named = THUMBNAIL_SIZES.find((s) => s.id === n);
	let size: [number, number] | null = named ? [named.width, named.height] : null;
	if (!size && (n === '16:9' || n === 'landscape')) size = [1920, 1080];
	if (!size && (n === '9:16' || n === 'portrait')) size = [1080, 1920];
	if (!size && n === '1:1') size = [1080, 1080];
	if (!size) {
		const m = /^(\d+)\s*[x×]\s*(\d+)$/.exec(n);
		if (m) size = [Math.max(2, Number(m[1])) & ~1, Math.max(2, Number(m[2])) & ~1];
	}
	if (!size) throw new Error(`“${name}” is not a thumbnail size: youtube, vertical, square, 4:5, 16:9 or WxH like 1280x720`);
	const ok = (v: number) => v >= MIN_THUMBNAIL_SIDE && v <= MAX_THUMBNAIL_SIDE;
	if (!ok(size[0]) || !ok(size[1])) {
		throw new Error(`a thumbnail is ${MIN_THUMBNAIL_SIDE} to ${MAX_THUMBNAIL_SIDE} pixels a side, not ${size[0]}x${size[1]}`);
	}
	return size;
}

/** The shape name of a size, else `WxH`: what the size select shows. */
export function sizeName(width: number, height: number): string {
	return THUMBNAIL_SIZES.find((s) => s.width === width && s.height === height)?.id ?? `${width}x${height}`;
}

function gcd(a: number, b: number): number {
	return b === 0 ? a : gcd(b, a % b);
}

/** The size a new thumbnail takes from the delivery frame — mirrors `default_size`. */
export function defaultThumbnailSize(format: Delivery | null | undefined): [number, number] {
	if (!format) return [1280, 720];
	const d = gcd(format.width, format.height) || 1;
	const [w, h] = [format.width / d, format.height / d];
	if (w === 4 && h === 5) return [1080, 1350];
	if (w < h) return [1080, 1920];
	if (w === h) return [1080, 1080];
	return [1280, 720];
}

/** Where the panel can put a headline: a band across the top, the middle or the bottom (the default). */
export const HEADLINE_PLACEMENTS = {
	top: { x: 0.05, y: 0.06, width: 0.9, height: 0.3 },
	middle: { x: 0.05, y: 0.35, width: 0.9, height: 0.3 },
	bottom: { x: 0.05, y: 0.62, width: 0.9, height: 0.3 }
} as const;
export type HeadlinePlacement = keyof typeof HEADLINE_PLACEMENTS;

/** The band a layer sits in, or `null` when it was placed some other way. */
export function placementOf(layer: Pick<ThumbnailLayer, 'x' | 'y' | 'width' | 'height'>): HeadlinePlacement | null {
	const near = (a: number, b: number) => Math.abs(a - b) < 1e-6;
	for (const [name, p] of Object.entries(HEADLINE_PLACEMENTS)) {
		if (near(layer.x, p.x) && near(layer.y, p.y) && near(layer.width, p.width) && near(layer.height, p.height)) {
			return name as HeadlinePlacement;
		}
	}
	return null;
}

/** A spec for the stack as it is, every layer kept: the base an edit of one layer starts from. */
export function keptLayers(thumbnail: Thumbnail): ThumbnailLayerSpec[] {
	return (thumbnail.layers ?? []).map((l) => ({ layer_id: l.id }));
}

/** The kind of artwork a layer holds — mirrors `ThumbnailLayer::kind`. */
export function layerKind(layer: ThumbnailLayer): 'text' | 'template' | 'svg' {
	return layer.text ? 'text' : layer.template ? 'template' : 'svg';
}

function check(t: Thumbnail): void {
	if (!Number.isFinite(t.time) || t.time < 0) throw new Error("a thumbnail's time is a number of timeline seconds, at least 0");
	for (const v of [t.focus_x, t.focus_y]) {
		if (!Number.isFinite(v) || v < 0 || v > 1) throw new Error('focus_x and focus_y are fractions of the frame, within 0..=1');
	}
	for (const l of t.layers ?? []) {
		if (!(l.width > 0 && l.height > 0 && l.width <= 3 && l.height <= 3)) {
			throw new Error('width and height are fractions of the thumbnail, > 0 and <= 3');
		}
		if (!(l.opacity >= 0 && l.opacity <= 1)) throw new Error('opacity must be within 0.0..=1.0');
	}
}

function newName(spec: ThumbnailLayerSpec): string {
	if (spec.text) return spec.text.text.split('\n')[0].trim().slice(0, 40);
	if (spec.template_id) return spec.template_id;
	return spec.image ? 'Picture' : 'Artwork';
}

function layerOf(spec: ThumbnailLayerSpec, existing: ThumbnailLayer[], uid: () => string): ThumbnailLayer {
	const sources = [spec.text, spec.template_id, spec.svg, spec.image].filter((s) => s != null).length;
	if (sources > 1) throw new Error('a layer is one of `text`, `template_id`, `svg` or `image`, not several');
	let out: ThumbnailLayer;
	if (spec.layer_id != null) {
		const kept = existing.find((l) => l.id === spec.layer_id);
		if (!kept) throw new Error(`the thumbnail has no layer ${spec.layer_id}`);
		out = structuredClone(kept);
		if (sources === 0 && spec.values) {
			if (!out.template) throw new Error(`layer “${out.name}” is not a template, so it has no values to set`);
			out.template.values = { ...out.template.values, ...spec.values };
		}
	} else {
		if (sources === 0) throw new Error('a new layer needs `text`, `template_id`, `svg` or `image`');
		const p = spec.text ? HEADLINE_PLACEMENTS.bottom : { x: 0, y: 0, width: 1, height: 1 };
		out = { id: uid(), name: newName(spec), svg: '', ...p, opacity: 1 };
	}
	if (sources === 1) {
		out.svg = spec.svg ?? '';
		out.text = spec.text ?? null;
		out.template = spec.template_id
			? { template_id: spec.template_id, name: spec.template_id, controls: [], values: spec.values ?? {} }
			: null;
	}
	const name = spec.name?.trim();
	if (name) out.name = name.slice(0, 80);
	if (spec.at != null) out.at = spec.at < 0 ? null : spec.at;
	out.x = spec.x ?? out.x;
	out.y = spec.y ?? out.y;
	out.width = spec.width ?? out.width;
	out.height = spec.height ?? out.height;
	out.opacity = spec.opacity ?? out.opacity;
	return out;
}

/** The browser harness's add / change of a thumbnail (`id` null adds one) — mirrors `prepare_thumbnail` and
 *  `apply_thumbnail_parsed`, the artwork left to the engine. Writes into `timeline`; returns the thumbnail. */
export function putThumbnail(timeline: Timeline, id: string | null, spec: ThumbnailSpec, uid: () => string): Thumbnail {
	const list = (timeline.thumbnails ??= []);
	const at = id == null ? -1 : list.findIndex((t) => t.id === id);
	if (id != null && at < 0) throw new Error(`no thumbnail ${id}`);
	if (id == null && list.length >= MAX_THUMBNAILS) throw new Error(`a project keeps at most ${MAX_THUMBNAILS} thumbnails`);
	const [width, height] = defaultThumbnailSize(timeline.format);
	const t: Thumbnail =
		at >= 0
			? structuredClone(list[at])
			: { id: uid(), name: `Thumbnail ${list.length + 1}`, time: 0, width, height, focus_x: 0.5, focus_y: 0.5, graphics: 'titles' };
	const name = spec.name?.trim();
	if (name) t.name = name.slice(0, 80);
	t.time = spec.time ?? t.time;
	if (spec.size != null) [t.width, t.height] = parseThumbnailSize(spec.size);
	t.focus_x = spec.focus_x ?? t.focus_x;
	t.focus_y = spec.focus_y ?? t.focus_y;
	t.graphics = spec.graphics ?? t.graphics;
	if (spec.layers != null) {
		if (spec.layers.length > MAX_THUMBNAIL_LAYERS) throw new Error(`a thumbnail stacks at most ${MAX_THUMBNAIL_LAYERS} layers`);
		const layers = spec.layers.map((s) => layerOf(s, t.layers ?? [], uid));
		if (new Set(layers.map((l) => l.id)).size !== layers.length) {
			throw new Error('a layer is in the stack once: a `layer_id` is named twice');
		}
		t.layers = layers;
	}
	check(t);
	if (at >= 0) list[at] = t;
	else list.push(t);
	return t;
}

/** Frames spread through the cut, for the harness's suggestions (the engine scores real ones). */
export function evenFrames(duration: number, count: number): SuggestedFrame[] {
	if (!(duration > 0)) return [];
	const n = Math.max(1, Math.min(12, Math.floor(count)));
	return Array.from({ length: n }, (_, i) => ({
		time: Math.round(((duration * (i + 0.5)) / n) * 1000) / 1000,
		score: 0,
		sharpness: 0,
		exposure: 0,
		contrast: 0
	}));
}
