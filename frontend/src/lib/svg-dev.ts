// The browser harness's stand-in for the animated-title commands (`add_svg_overlay`,
// `update_svg_overlay`, `remove_svg_overlay`, `validate_svg`, `list_svg_presets`,
// `svg_preset_preview`): the same rules kerf-core applies to what it is given, over a plain
// timeline object. The artwork of a preset comes from `fixtures/svg-presets.json`, which
// kerf-core writes down (`svg/presets.rs`, `KERF_BLESS_SVG_PRESETS=1`): each preset drawn once
// for the 1920x1080 frame and four seconds, with placeholder text. The harness swaps the text,
// colours and font into those documents; it does not re-time them to another length or frame, and
// custom SVG is neither sanitised nor evaluated (the page never inlines it either way).
import fixture from './fixtures/svg-presets.json';
import type {
	SvgCheck,
	SvgLayout,
	SvgOverlay,
	SvgOverlayPatch,
	SvgPresetInfo,
	SvgPresetKind,
	SvgPresetParams,
	SvgPresetParamsInput,
	SvgSource,
	Timeline
} from './types';

/** How many animated titles one timeline may hold. */
export const MAX_SVG_OVERLAYS = 50;

const MAX_BYTES = 128 * 1024;
const MAX_TEXT_CHARS = 200;
const MAX_FONT_CHARS = 100;

type Art = { plain: string; subtitled?: string };
const ART = fixture.presets as Record<SvgPresetKind, Art>;

/** The presets and what each takes (`list_svg_presets`). */
export function svgPresetInfos(): SvgPresetInfo[] {
	return structuredClone(fixture.infos) as SvgPresetInfo[];
}

function cleanText(s: string): string {
	return [...s.replace(/[\p{Cc}\s]+/gu, ' ').trim()].slice(0, MAX_TEXT_CHARS).join('');
}

function esc(s: string): string {
	return s
		.replaceAll('&', '&amp;')
		.replaceAll('<', '&lt;')
		.replaceAll('>', '&gt;')
		.replaceAll('"', '&quot;')
		.replaceAll("'", '&apos;');
}

const regexSafe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** A colour that can sit in an attribute: the engine reads the real syntax. */
function colourProblem(what: string, value: string): string | null {
	return /^[#\w(),.%\s-]{1,40}$/.test(value.trim()) ? null : `${what} "${value}" is not an SVG colour like ${fixture.defaults.color}`;
}

/** What `check_params` refuses: no text, a text or font too long, a colour that is not one. */
export function checkPresetParams(params: SvgPresetParamsInput): void {
	if (!cleanText(params.text ?? '')) throw new Error('a preset needs some text');
	for (const [what, text] of [
		['text', params.text],
		['subtitle', params.subtitle]
	] as const) {
		if (text && [...text].length > MAX_TEXT_CHARS) throw new Error(`${what} is limited to ${MAX_TEXT_CHARS} characters`);
	}
	for (const [what, color] of [
		['color', params.color ?? fixture.defaults.color],
		['accent', params.accent ?? fixture.defaults.accent]
	] as const) {
		const problem = colourProblem(what, color);
		if (problem) throw new Error(problem);
	}
	if (params.font && [...params.font].length > MAX_FONT_CHARS) {
		throw new Error(`font is limited to ${MAX_FONT_CHARS} characters`);
	}
}

/** The same params with what a caller left out filled in, as the backend does. */
export function fillPresetParams(params: SvgPresetParamsInput): SvgPresetParams {
	return {
		text: params.text,
		subtitle: params.subtitle ?? null,
		color: params.color ?? fixture.defaults.color,
		accent: params.accent ?? fixture.defaults.accent,
		font: params.font ?? null
	};
}

const TYPED = /<tspan visibility="hidden"><set attributeName="visibility" to="visible" begin="([\d.]+)s"\/>[^<]*<\/tspan>/g;

/** One `<tspan>` per character, appearing at the pace the fixture's own characters do. */
function retype(svg: string, text: string): string {
	const spans = [...svg.matchAll(TYPED)];
	if (spans.length < 2) return svg;
	const first = Number(spans[0][1]);
	const step = Number(spans[1][1]) - first;
	const last = spans[spans.length - 1];
	const typed = [...text]
		.map(
			(c, i) =>
				`<tspan visibility="hidden"><set attributeName="visibility" to="visible" begin="${+(first + step * i).toFixed(3)}s"/>${esc(c)}</tspan>`
		)
		.join('');
	return svg.slice(0, spans[0].index) + typed + svg.slice(last.index! + last[0].length);
}

/** The artwork of a preset (`svg_preset_preview`), as SVG text. */
export function svgPresetArt(kind: SvgPresetKind, params: SvgPresetParamsInput): string {
	checkPresetParams(params);
	const text = cleanText(params.text);
	const subtitle = params.subtitle ? cleanText(params.subtitle) : '';
	const art = ART[kind];
	let svg = subtitle && art.subtitled ? art.subtitled : art.plain;
	if (kind === 'typewriter') {
		svg = retype(svg, text);
	} else if (kind === 'word_pop') {
		const [a, b] = fixture.text.split(' ');
		const [head, ...rest] = text.split(' ');
		svg = svg.replace(new RegExp(`>(${regexSafe(a)}|${regexSafe(b)})<`, 'g'), (_, w: string) => `>${esc(w === a ? head : rest.join(' '))}<`);
	}
	const sentinel = new RegExp(`${regexSafe(fixture.subtitle)}${kind === 'typewriter' || kind === 'word_pop' ? '' : `|${regexSafe(fixture.text)}`}`, 'g');
	svg = svg.replace(sentinel, (m) => esc(m === fixture.subtitle ? subtitle : text));
	const { color, accent, font } = fixture.defaults;
	const paints = new RegExp(`fill="(${regexSafe(color)}|${regexSafe(accent)})"`, 'g');
	svg = svg.replace(paints, (_, c: string) => `fill="${esc((c === color ? params.color : params.accent)?.trim() || c)}"`);
	const family = [...(params.font ?? '')]
		.filter((c) => /[\p{L}\p{N} _.+()-]/u.test(c))
		.slice(0, MAX_FONT_CHARS)
		.join('')
		.trim();
	return family ? svg.replaceAll(`font-family="${font}"`, `font-family="${esc(family)}"`) : svg;
}

/** `validate_svg`: what the engine would store, and what it could not honour. The harness has no
 *  sanitiser: it drops scripts, foreign objects and animateMotion and says so, and leaves the rest. */
export function validateSvgSource(src: string): SvgCheck {
	if (new TextEncoder().encode(src).length > MAX_BYTES) throw new Error(`the SVG is larger than ${MAX_BYTES / 1024} KiB`);
	if (!/^\s*(?:<\?xml[^>]*\?>\s*)?(?:<!--[\s\S]*?-->\s*)*<svg[\s>]/.test(src)) {
		throw new Error('the root element must be <svg xmlns="http://www.w3.org/2000/svg">');
	}
	const warnings: string[] = [];
	let svg = src;
	const strip = (tag: string, message: string) => {
		const next = svg.replace(new RegExp(`<${tag}\\b[^>]*/>|<${tag}\\b[\\s\\S]*?</${tag}>`, 'gi'), '');
		if (next !== svg) warnings.push(message);
		svg = next;
	};
	strip('script', '<script> was removed');
	strip('foreignObject', '<foreignObject> was removed');
	strip('animateMotion', '<animateMotion> is not rendered');
	if (/@keyframes|animation\s*:/i.test(svg)) warnings.push('CSS animations are not rendered — use SMIL');
	return { svg, warnings, natural_duration: null };
}

/** A placement the compositor can draw: finite numbers, a real size, an opacity. */
function checkRect(o: Pick<SvgOverlay, 'x' | 'y' | 'width' | 'height' | 'opacity'>): void {
	const { x, y, width, height, opacity } = o;
	if (![x, y, width, height, opacity].every(Number.isFinite)) {
		throw new Error('x, y, width, height and opacity must be numbers');
	}
	if (x < -5 || x > 5 || y < -5 || y > 5) throw new Error('x and y are fractions of the frame, within -5..=5');
	if (width <= 0 || height <= 0 || width > 10 || height > 10) {
		throw new Error('width and height are fractions of the frame, > 0 and <= 10');
	}
	if (opacity < 0 || opacity > 1) throw new Error('opacity must be within 0.0..=1.0');
}

/** A title's name to show: its own, else a preset's text, else a placeholder. */
export function svgDisplayName(o: Pick<SvgOverlay, 'name' | 'preset'>): string {
	return o.name.trim() || o.preset?.params.text || 'SVG';
}

/** `add_svg_overlay` over a timeline object. A title with no `end` runs four seconds. */
export function addSvgOverlayTo(
	timeline: Timeline,
	source: SvgSource,
	start: number,
	end: number | null | undefined,
	layout: SvgLayout = {}
): { overlay: SvgOverlay; warnings: string[] } {
	if (!Number.isFinite(start)) throw new Error('start must be a number of seconds');
	const from = Math.max(0, start);
	if (end != null && (!Number.isFinite(end) || end <= from)) throw new Error('overlay end must be after start');
	let svg = '';
	let preset: SvgOverlay['preset'];
	let warnings: string[] = [];
	if ('custom' in source) {
		({ svg, warnings } = validateSvgSource(source.custom));
	} else {
		checkPresetParams(source.preset.params);
		preset = { kind: source.preset.kind, params: fillPresetParams(source.preset.params) };
		svg = svgPresetArt(preset.kind, preset.params);
	}
	const overlay: SvgOverlay = {
		id: crypto.randomUUID(),
		name: layout.name?.trim() ?? '',
		svg,
		start: from,
		end: end ?? from + 4,
		x: layout.x ?? 0,
		y: layout.y ?? 0,
		width: layout.width ?? 1,
		height: layout.height ?? 1,
		opacity: layout.opacity ?? 1,
		...(preset ? { preset } : {})
	};
	checkRect(overlay);
	if ((timeline.svg_overlays?.length ?? 0) >= MAX_SVG_OVERLAYS) {
		throw new Error(`a timeline holds at most ${MAX_SVG_OVERLAYS} animated titles`);
	}
	(timeline.svg_overlays ??= []).push(overlay);
	return { overlay: structuredClone(overlay), warnings };
}

/** `update_svg_overlay` over a timeline object: all of it or nothing. */
export function updateSvgOverlayIn(timeline: Timeline, overlayId: string, patch: SvgOverlayPatch): string[] {
	if (patch.svg !== undefined && patch.preset_params !== undefined) {
		throw new Error('pass either svg or preset_params, not both');
	}
	let warnings: string[] = [];
	let svg: string | undefined;
	if (patch.svg !== undefined) ({ svg, warnings } = validateSvgSource(patch.svg));
	if (patch.preset_params) checkPresetParams(patch.preset_params);
	if ([patch.start, patch.end].some((v) => v !== undefined && !Number.isFinite(v))) {
		throw new Error('start and end must be numbers of seconds');
	}
	const at = timeline.svg_overlays?.findIndex((o) => o.id === overlayId) ?? -1;
	if (at < 0) throw new Error(`animated title not found: ${overlayId}`);
	const o = structuredClone(timeline.svg_overlays![at]);
	const length = o.end - o.start + (o.offset ?? 0);
	if (patch.name !== undefined) o.name = patch.name.trim();
	if (patch.start !== undefined) o.start = Math.max(0, patch.start);
	if (patch.end !== undefined) o.end = patch.end;
	o.x = patch.x ?? o.x;
	o.y = patch.y ?? o.y;
	o.width = patch.width ?? o.width;
	o.height = patch.height ?? o.height;
	o.opacity = patch.opacity ?? o.opacity;
	if (o.end <= o.start) throw new Error('overlay end must be after start');
	checkRect(o);
	let regenerate = !!o.preset && Math.abs(o.end - o.start + (o.offset ?? 0) - length) > 1e-6;
	if (svg !== undefined) {
		o.svg = svg;
		delete o.preset;
		regenerate = false;
	} else if (patch.preset_params) {
		if (!o.preset) throw new Error('this title is custom SVG, not a preset; pass svg to change it');
		o.preset = { kind: o.preset.kind, params: fillPresetParams(patch.preset_params) };
		regenerate = true;
	}
	if (regenerate && o.preset) o.svg = svgPresetArt(o.preset.kind, o.preset.params);
	timeline.svg_overlays![at] = o;
	return warnings;
}

/** `remove_svg_overlay` over a timeline object. The list goes when it empties, as the saved file has it. */
export function removeSvgOverlayFrom(timeline: Timeline, overlayId: string): void {
	const kept = (timeline.svg_overlays ?? []).filter((o) => o.id !== overlayId);
	if (kept.length === (timeline.svg_overlays ?? []).length) throw new Error(`animated title not found: ${overlayId}`);
	if (kept.length) timeline.svg_overlays = kept;
	else delete timeline.svg_overlays;
}
