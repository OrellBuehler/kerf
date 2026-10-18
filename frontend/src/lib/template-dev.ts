// The browser harness's stand-in for the motion-template commands (`list_templates`, `template_poster`,
// `save_template`, `apply_template`, `set_template_values`, `import_template`, `export_template`,
// `delete_template`, `svg_element_ids`, `template_image_value`): the same refusals kerf-core makes, over an
// in-memory library seeded with one sample template. Values are spliced into the artwork by element id
// (a regular-expression walk of the start tags, not an XML parser: the harness has no sanitiser either),
// escaped, and read back into `TemplateInstance.values` the way the backend reads them from the `svg`.
// There is no file system here: an exported template is kept under its path in memory so an import of that
// path round-trips, and a picture for an `image` control is a fixed one.
import { checkRect, MAX_SVG_OVERLAYS } from './svg-dev';
import { svgDisplayName } from './svg-titles';
import { plainDecimal, validSvgColor } from './template-ui';
import type {
	ImportOutcome,
	SvgElementId,
	SvgLayout,
	SvgOverlay,
	TemplateControl,
	TemplateInfo,
	TemplateInstance,
	TemplateKind,
	TemplateList,
	TemplateSaved,
	Timeline
} from './types';

export const TEMPLATE_EXTENSION = 'kerftemplate';
const MAX_CONTROLS = 64;
const MAX_TARGETS = 16;
const MAX_TEXT_CHARS = 2000;
const MAX_FAMILY_CHARS = 64;
const MAX_NAME_CHARS = 120;
const MAX_LABEL_CHARS = 80;
const MAX_DESCRIPTION_CHARS = 2000;
const MAX_SVG_BYTES = 128 * 1024;
const MAX_SVG_SECONDS = 24 * 3600;
const COLOR_ATTRIBUTES = ['fill', 'stroke', 'stop-color', 'flood-color', 'lighting-color', 'color'];

/** A 4 x 4 red PNG: the one picture the harness can offer for an image control. */
const SAMPLE_PICTURE =
	'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAAEklEQVR4nGP4z8DwHxkzkC4AADxAH+HggXe0AAAAAElFTkSuQmCC';

export const SAMPLE_TEMPLATE_ID = '5b6f1c1e-0c64-4f6e-9a54-3f3a8c1d7a01';

const SAMPLE_SVG = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1920 1080"><defs><linearGradient id="bg-gradient" x1="0" y1="0" x2="0" y2="1"><stop id="bg-top" offset="0" stop-color="darkcyan"/><stop offset="1" stop-color="midnightblue"/></linearGradient></defs><rect width="1920" height="1080" fill="url(#bg-gradient)"/><text id="headline" x="960" y="500" font-family="Geist" font-size="120" font-weight="700" fill="white" text-anchor="middle" opacity="0"><animate attributeName="opacity" from="0" to="1" begin="0.4s" dur="0.8s" fill="freeze"/>Thanks for watching</text><text id="tagline" x="960" y="620" font-family="Geist" font-size="56" fill="lightcyan" text-anchor="middle" opacity="0"><animate attributeName="opacity" from="0" to="1" begin="1s" dur="0.8s" fill="freeze"/>See you next time</text></svg>`;

const SAMPLE_CONTROLS: TemplateControl[] = [
	{ id: 'headline', label: 'Headline', kind: 'text', targets: [{ element: 'headline' }] },
	{ id: 'tagline', label: 'Tagline', kind: 'text', targets: [{ element: 'tagline' }] },
	{ id: 'brand', label: 'Brand colour', kind: 'color', targets: [{ element: 'bg-top', attribute: 'stop-color' }] },
	{
		id: 'headline-size',
		label: 'Headline size',
		kind: 'number',
		targets: [{ element: 'headline', attribute: 'font-size' }],
		min: 40,
		max: 200
	}
];

type Entry = { info: TemplateInfo; svg: string };

let library = new Map<string, Entry>();
let files = new Map<string, Entry>();

const uid = () => (crypto.randomUUID ? crypto.randomUUID() : `00000000-0000-4000-8000-${Math.random().toString(16).slice(2, 14).padEnd(12, '0')}`);
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function shapeOf(svg: string): { width: number; height: number } {
	const root = /<svg\b[^>]*>/.exec(svg)?.[0] ?? '';
	const box = /viewBox\s*=\s*["']\s*[-\d.eE+]+[\s,]+[-\d.eE+]+[\s,]+([\d.eE+]+)[\s,]+([\d.eE+]+)\s*["']/.exec(root);
	const attr = (name: string) => Number.parseFloat(new RegExp(`\\b${name}\\s*=\\s*["']([\\d.]+)`).exec(root)?.[1] ?? '');
	const width = box ? Number(box[1]) : attr('width');
	const height = box ? Number(box[2]) : attr('height');
	return { width: Math.round(width) || 1920, height: Math.round(height) || 1080 };
}

function seed(): Map<string, Entry> {
	const { width, height } = shapeOf(SAMPLE_SVG);
	const info: TemplateInfo = {
		id: SAMPLE_TEMPLATE_ID,
		name: 'Sample end card',
		description: 'A headline and a tagline over a brand-coloured gradient.',
		duration: 4,
		width,
		height,
		controls: structuredClone(SAMPLE_CONTROLS),
		fonts: [],
		has_poster: true,
		size_bytes: SAMPLE_SVG.length + 900
	};
	return new Map([[info.id, { info, svg: SAMPLE_SVG }]]);
}

/** `?empty=1` is a first run: no templates. `?templateproblems=1` adds a file in the library that would not read. */
const wants = (flag: string) => typeof window !== 'undefined' && new URLSearchParams(window.location?.search ?? '').has(flag);

library = wants('empty') ? new Map() : seed();

/** Put the harness library back to its one sample template and forget the exported files (tests, `?empty`). */
export function resetDevTemplates(empty = false): void {
	library = empty ? new Map() : seed();
	files = new Map();
}

const invalid = (message: string) => new Error(message);
const missing = (id: string) => invalid(`there is no template with the id ${id} in the library`);

function checkId(id: string): void {
	if (!UUID.test(id)) throw invalid("a template's id must be a lower-case UUID");
}

function posterOf(svg: string): string {
	return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

// ---- reading and splicing the artwork -------------------------------------------------------

const decode = (s: string) =>
	s
		.replace(/&#x([0-9a-f]+);/gi, (_, h: string) => String.fromCodePoint(Number.parseInt(h, 16)))
		.replace(/&#(\d+);/g, (_, d: string) => String.fromCodePoint(Number(d)))
		.replaceAll('&lt;', '<')
		.replaceAll('&gt;', '>')
		.replaceAll('&quot;', '"')
		.replaceAll('&apos;', "'")
		.replaceAll('&amp;', '&');

const escapeText = (s: string) => s.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
const escapeAttr = (s: string, quote: string) =>
	escapeText(s).replaceAll(quote, quote === '"' ? '&quot;' : '&apos;');

type Tag = { name: string; start: number; end: number; selfClosing: boolean; attrs: Attr[] };
type Attr = { name: string; value: string; valueStart: number; valueEnd: number; quote: string };

const START_TAG = /<([A-Za-z_][\w:.-]*)((?:\s+[^\s=>/]+\s*=\s*(?:"[^"]*"|'[^']*'))*)\s*(\/?)>/g;

function tags(svg: string): Tag[] {
	const found: Tag[] = [];
	for (const m of svg.matchAll(START_TAG)) {
		const start = m.index!;
		const attrs: Attr[] = [];
		const open = start + 1 + m[1].length;
		for (const a of m[2].matchAll(/([^\s=>/]+)\s*=\s*("([^"]*)"|'([^']*)')/g)) {
			const quote = a[2][0];
			const valueStart = open + a.index! + a[0].length - a[2].length + 1;
			attrs.push({ name: a[1], value: decode(a[3] ?? a[4] ?? ''), valueStart, valueEnd: valueStart + (a[3] ?? a[4] ?? '').length, quote });
		}
		found.push({ name: m[1], start, end: start + m[0].length, selfClosing: m[3] === '/', attrs });
	}
	return found;
}

function elementWith(svg: string, id: string): Tag {
	const matching = tags(svg).filter((t) => t.attrs.some((a) => a.name === 'id' && a.value === id));
	if (matching.length === 0) throw invalid(`the title has no element with the id “${id}”`);
	if (matching.length > 1) throw invalid(`${matching.length} elements of the title have the id “${id}”; it has to be one`);
	return matching[0];
}

/** Where the content of `tag` ends: the start of its matching end tag, counting nested elements of the same name. */
function closeOf(svg: string, tag: Tag): number {
	const re = new RegExp(`<(/?)${tag.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?=[\\s/>])[^>]*?(/?)>`, 'g');
	re.lastIndex = tag.end;
	let depth = 1;
	for (let m = re.exec(svg); m; m = re.exec(svg)) {
		if (m[1]) depth -= 1;
		else if (!m[2]) depth += 1;
		if (depth === 0) return m.index;
	}
	throw invalid(`the element “${tag.attrs.find((a) => a.name === 'id')?.value}” has no end tag`);
}

/** The ranges of `tag`'s own text, as kerf-core's `text_runs` has them: the gaps between its child elements,
 *  comments and processing instructions. A CDATA section is part of the run it sits in. */
function textRuns(svg: string, tag: Tag): { from: number; to: number }[] {
	const close = closeOf(svg, tag);
	const token = /<!\[CDATA\[[\s\S]*?\]\]>|<!--[\s\S]*?-->|<\?[\s\S]*?\?>|<(\/?)[A-Za-z_][\w:.-]*(?:\s+[^\s=>/]+\s*=\s*(?:"[^"]*"|'[^']*'))*\s*(\/?)>/g;
	token.lastIndex = tag.end;
	const runs: { from: number; to: number }[] = [];
	let at = tag.end;
	let depth = 0;
	for (let m = token.exec(svg); m && m.index < close; m = token.exec(svg)) {
		if (m[0].startsWith('<![CDATA[')) continue;
		if (depth === 0 && m.index > at) runs.push({ from: at, to: m.index });
		if (m[0].startsWith('<!--') || m[0].startsWith('<?')) {
			if (depth === 0) at = m.index + m[0].length;
			continue;
		}
		if (m[1]) depth -= 1;
		else if (!m[2]) depth += 1;
		if (depth === 0) at = m.index + m[0].length;
	}
	if (close > at) runs.push({ from: at, to: close });
	return runs;
}

/** A run's text as the XML parser reads it: entities decoded, a CDATA section's content as it is. */
function runText(run: string): string {
	return run
		.split(/(<!\[CDATA\[[\s\S]*?\]\]>)/)
		.map((part) => (part.startsWith('<![CDATA[') ? part.slice(9, -3) : decode(part)))
		.join('');
}

function readTarget(svg: string, element: string, attribute: string | undefined): string | null {
	const tag = elementWith(svg, element);
	if (attribute === undefined) {
		if (tag.selfClosing) return '';
		return textRuns(svg, tag)
			.map((run) => runText(svg.slice(run.from, run.to)))
			.join('');
	}
	return tag.attrs.find((a) => a.name === attribute)?.value ?? null;
}

/** The value each control holds in `svg` now; an `image` control, and one whose element is gone, is left out. */
export function readValues(svg: string, controls: TemplateControl[]): Record<string, string> {
	const values: Record<string, string> = {};
	for (const c of controls) {
		if (c.kind === 'image' || !c.targets[0]) continue;
		try {
			const value = readTarget(svg, c.targets[0].element, c.targets[0].attribute);
			if (value !== null) values[c.id] = value;
		} catch {
			// the element is gone
		}
	}
	return values;
}

/** The words Rust's `f64::from_str` takes, after a trim: a sign, decimal digits with an optional point and exponent
 *  (`5`, `5.`, `.5`, `1e3`, `+2.5E-3`), or `inf` / `infinity` / `nan` in any case. Nothing `Number()` adds: `0x10`, `0b11`,
 *  `0o7`, `1_000`, a bare `.`. `null` is a string Rust refuses; a word that parses but is not finite comes back as `NaN`. */
function parseFloatLikeRust(raw: string): number | null {
	const t = raw.trim();
	if (/^[+-]?(?:inf|infinity|nan)$/i.test(t)) return Number.NaN;
	if (!/^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/.test(t)) return null;
	return Number(t);
}

function checkValue(control: TemplateControl, raw: string): string {
	const { id } = control;
	switch (control.kind) {
		case 'text':
			if ([...raw].length > MAX_TEXT_CHARS) throw invalid(`the text of “${id}” is longer than ${MAX_TEXT_CHARS} characters`);
			if (/[\p{Cc}]/u.test(raw.replace(/[\t\n]/g, ''))) throw invalid(`the text of “${id}” has a control character in it`);
			return raw;
		case 'color': {
			const color = raw.trim();
			if (!validSvgColor(color)) throw invalid(`“${raw}” is not a colour for “${id}” (use #rrggbb, #rrggbbaa or a colour name)`);
			return color;
		}
		case 'number': {
			const n = parseFloatLikeRust(raw);
			if (n === null) throw invalid(`“${raw}” is not a number for “${id}”`);
			if (!Number.isFinite(n)) throw invalid(`“${raw}” is not a finite number for “${id}”`);
			if ((control.min !== undefined && n < control.min) || (control.max !== undefined && n > control.max)) {
				throw invalid(`${n} is outside what “${id}” takes (${control.min ?? 'any'} to ${control.max ?? 'any'})`);
			}
			return plainDecimal(n);
		}
		case 'font': {
			const family = raw.trim();
			if (!family || [...family].length > MAX_FAMILY_CHARS || !/^[\p{L}\p{N} _.-]+$/u.test(family)) {
				throw invalid(
					`“${raw}” is not a font family for “${id}” (letters, digits, spaces, - _ . — up to ${MAX_FAMILY_CHARS})`
				);
			}
			return family;
		}
		case 'image':
			if (!raw.startsWith('data:')) throw invalid(`the image for “${id}” must be a data: URL`);
			if (!/^data:image\/(?:png|jpeg|gif|webp);base64,/i.test(raw)) {
				throw invalid(`the image for “${id}”: only a data:image/png, jpeg, gif or webp URI is kept`);
			}
			return raw;
	}
}

/** `svg` with each of `values` written where its control points; only the named controls change. All of it or nothing. */
export function applyValues(svg: string, controls: TemplateControl[], values: Record<string, string>): string {
	const entries = Object.entries(values);
	if (entries.length === 0) return svg;
	const unknown = entries.map(([id]) => id).filter((id) => !controls.some((c) => c.id === id));
	if (unknown.length > 0) {
		const known = controls.map((c) => `“${c.id}”`).join(', ') || 'none';
		throw invalid(`no control called ${unknown.map((u) => `“${u}”`).join(', ')}; this template has ${known}`);
	}
	const edits: { from: number; to: number; with: string }[] = [];
	for (const control of controls) {
		const raw = values[control.id];
		if (raw === undefined) continue;
		const value = checkValue(control, raw);
		for (const target of control.targets) {
			const tag = elementWith(svg, target.element);
			if (target.attribute === undefined) {
				if (tag.selfClosing) {
					edits.push({ from: tag.end - 2, to: tag.end, with: `>${escapeText(value)}</${tag.name}>` });
				} else {
					const [first, ...rest] = textRuns(svg, tag);
					if (!first) {
						const close = closeOf(svg, tag);
						edits.push({ from: close, to: close, with: escapeText(value) });
					} else {
						edits.push({ from: first.from, to: first.to, with: escapeText(value) });
						for (const run of rest) edits.push({ from: run.from, to: run.to, with: '' });
					}
				}
			} else {
				const attr = tag.attrs.find((a) => a.name === target.attribute);
				if (!attr) throw invalid(`the element “${target.element}” has no attribute “${target.attribute}” to set`);
				edits.push({ from: attr.valueStart, to: attr.valueEnd, with: escapeAttr(value, attr.quote) });
			}
		}
	}
	edits.sort((a, b) => a.from - b.from);
	for (let i = 1; i < edits.length; i++) {
		if (edits[i].from < edits[i - 1].to) throw invalid('two values would be written to the same place in the title');
	}
	let out = svg;
	for (const e of [...edits].reverse()) out = out.slice(0, e.from) + e.with + out.slice(e.to);
	// The result goes through the sanitiser, which holds a title to 128 KiB.
	if (new TextEncoder().encode(out).length > MAX_SVG_BYTES) throw invalid(`the SVG is larger than ${MAX_SVG_BYTES / 1024} KiB`);
	return out;
}

// ---- checking a template's controls (save) -------------------------------------------------
// Two passes, as kerf-core runs them (`validate_controls_shape`, then `validate_controls` against the title).

const hasControlChars = (s: string) => [...s].some((c) => /\p{Cc}/u.test(c) && c !== '\t' && c !== '\n');

/** `check_text`: required means not blank; at most `max` characters; no control characters but tab and line break. */
function checkText(what: string, text: string, max: number, required: boolean): void {
	if (required && text.trim() === '') throw invalid(`${what} is empty`);
	if ([...text].length > max) throw invalid(`${what} is longer than ${max} characters`);
	if (hasControlChars(text)) throw invalid(`${what} has a control character in it`);
}

/** An attribute name as the source spells it: an XML name with at most one prefix, and that prefix `xlink`. */
function attributeNameOk(name: string): boolean {
	const ncname = (s: string) => s.length <= 64 && /^[A-Za-z_][A-Za-z0-9_.-]*$/.test(s);
	const colon = name.indexOf(':');
	return colon >= 0 ? name.slice(0, colon) === 'xlink' && ncname(name.slice(colon + 1)) : ncname(name);
}

function checkControlsShape(controls: TemplateControl[]): void {
	if (controls.length > MAX_CONTROLS) throw invalid(`a template has at most ${MAX_CONTROLS} controls`);
	const ids = new Set<string>();
	for (const c of controls) {
		if (!/^[A-Za-z][A-Za-z0-9_-]{0,63}$/.test(c.id)) {
			throw invalid(`“${c.id}” is not a control id: start with a letter, then letters, digits, - or _ (64 at most)`);
		}
		if (ids.has(c.id)) throw invalid(`two controls are called “${c.id}”`);
		ids.add(c.id);
		checkText(`the label of “${c.id}”`, c.label, MAX_LABEL_CHARS, true);
		if (c.targets.length === 0 || c.targets.length > MAX_TARGETS) throw invalid(`“${c.id}” needs 1 to ${MAX_TARGETS} targets`);
		for (const t of c.targets) {
			checkText(`a target of “${c.id}”`, t.element, 200, true);
			checkAttribute(c, t.attribute);
		}
		if (c.kind === 'number') {
			for (const bound of [c.min, c.max]) {
				if (bound !== undefined && !Number.isFinite(bound)) throw invalid(`the bounds of “${c.id}” must be numbers`);
			}
			if (c.min !== undefined && c.max !== undefined && c.min > c.max) {
				throw invalid(`the minimum of “${c.id}” is above its maximum`);
			}
		} else if (c.min !== undefined || c.max !== undefined) {
			throw invalid(`only a number control has a minimum or maximum (“${c.id}”)`);
		}
	}
}

/** Whether `inner` is inside `outer` (or the same element), by the start tags' places in the source. */
function within(svg: string, outer: string, inner: string): boolean {
	const o = elementWith(svg, outer);
	const i = elementWith(svg, inner);
	if (o.start === i.start) return true;
	return !o.selfClosing && i.start >= o.end && i.start < closeOf(svg, o);
}

function checkControls(controls: TemplateControl[], svg: string): void {
	checkControlsShape(controls);
	if (controls.length === 0) return;
	const places: { element: string; attribute: string | undefined }[] = [];
	for (const c of controls) {
		for (const t of c.targets) {
			const matching = tags(svg).filter((tag) => tag.attrs.some((a) => a.name === 'id' && a.value === t.element));
			if (matching.length === 0) throw invalid(`“${c.id}” targets the element “${t.element}”, which the title does not have`);
			if (matching.length > 1) throw invalid(`“${c.id}” targets “${t.element}”, which ${matching.length} elements are called`);
			const tag = matching[0];
			if (c.kind === 'text' && !['text', 'tspan', 'textPath'].includes(tag.name)) {
				throw invalid(`“${c.id}” is a text control but “${t.element}” is a <${tag.name}>: aim it at a <text> or <tspan>`);
			}
			if (c.kind === 'image' && tag.name !== 'image') {
				throw invalid(`“${c.id}” is an image control but “${t.element}” is a <${tag.name}>, not an <image>`);
			}
			if (t.attribute !== undefined && !tag.attrs.some((a) => a.name === t.attribute)) {
				throw invalid(`“${c.id}” sets “${t.attribute}” on “${t.element}”, which has no such attribute`);
			}
			if (places.some((p) => p.element === t.element && p.attribute === t.attribute)) {
				throw invalid(`more than one control writes ${t.attribute ?? 'the text'} of “${t.element}”`);
			}
			places.push({ element: t.element, attribute: t.attribute });
		}
	}
	// A control that replaces an element's text replaces what is inside it, including another control's target.
	for (const outer of places) {
		if (outer.attribute !== undefined) continue;
		for (const inner of places) {
			if (inner.element !== outer.element && within(svg, outer.element, inner.element)) {
				throw invalid(`the text of “${outer.element}” is a control, and it holds “${inner.element}”, which another control sets`);
			}
		}
	}
}

function checkAttribute(c: TemplateControl, attribute: string | undefined): void {
	const id = c.id;
	if (c.kind === 'text') {
		if (attribute !== undefined) throw invalid(`“${id}” is a text control: its targets take no attribute`);
		return;
	}
	if (attribute === undefined) throw invalid(`“${id}” is a ${c.kind} control: each target needs an attribute`);
	const forbidden =
		['id', 'class', 'style', 'href', 'xlink:href'].includes(attribute) || attribute.startsWith('xmlns') || attribute.startsWith('on');
	const imageLink = c.kind === 'image' && (attribute === 'href' || attribute === 'xlink:href');
	if (!attributeNameOk(attribute) || (forbidden && !imageLink)) throw invalid(`“${attribute}” cannot be set by a control (“${id}”)`);
	const allowed: Record<TemplateKind, (a: string) => boolean> = {
		text: () => false,
		color: (a) => COLOR_ATTRIBUTES.includes(a),
		font: (a) => a === 'font-family',
		image: (a) => a === 'href' || a === 'xlink:href',
		number: () => true
	};
	if (!allowed[c.kind](attribute)) throw invalid(`a ${c.kind} control cannot write “${attribute}” (“${id}”)`);
}

// ---- the commands -------------------------------------------------------------------------

const byName = (a: Entry, b: Entry) =>
	a.info.name.toLowerCase().localeCompare(b.info.name.toLowerCase()) || a.info.id.localeCompare(b.info.id);

/** `list_templates`: the library by name; a harness library has no unreadable files unless `?templateproblems=1` says so. */
export function listTemplatesDev(): TemplateList {
	const problems = wants('templateproblems')
		? [{ file: '9d1c0f52-4a47-4b8e-8e0c-6b0c1d3f7a11.kerftemplate', error: 'invalid argument: not a Kerf template (format “other”)' }]
		: [];
	return { templates: [...library.values()].sort(byName).map((e) => structuredClone(e.info)), problems };
}

/** `template_poster`: the artwork as an SVG data URL (the backend sends a PNG; both go through an `<img>`). */
export function templatePosterDev(id: string): string | null {
	checkId(id);
	const entry = library.get(id);
	if (!entry) throw missing(id);
	return entry.info.has_poster ? posterOf(entry.svg) : null;
}

/** `svg_element_ids`: every element with an `id` and its tag name. */
export function svgElementIdsOf(svg: string): SvgElementId[] {
	if (!/<svg[\s>]/.test(svg)) throw invalid('not valid XML: there is no <svg> element');
	return tags(svg).flatMap((t) => {
		const id = t.attrs.find((a) => a.name === 'id')?.value;
		return id === undefined ? [] : [{ id, tag: t.name }];
	});
}

/** `template_image_value`: the harness cannot read a file, so any path to a picture gets the same small PNG. */
export function templateImageValueOf(path: string): string {
	if (!/^(?:\/|[A-Za-z]:[\\/])/.test(path)) throw invalid(`${path} must be an absolute file path`);
	if (!/\.(?:png|jpe?g|gif|webp)$/i.test(path)) throw invalid('not a PNG, JPEG, GIF or WebP image');
	return SAMPLE_PICTURE;
}

/** `save_template` over a timeline object: keep the title as a template with these controls. */
export function saveTemplateFrom(
	timeline: Timeline,
	overlayId: string,
	name: string,
	description: string | null | undefined,
	controls: TemplateControl[]
): TemplateSaved {
	const o = timeline.svg_overlays?.find((t) => t.id === overlayId);
	if (!o) throw invalid(`animated title not found: ${overlayId}`);
	const duration = Math.max(o.end - o.start, 0) + (o.offset ?? 0);
	if (!Number.isFinite(duration) || duration <= 0 || duration > MAX_SVG_SECONDS) {
		throw invalid('the title has no length to make a template of');
	}
	// A blank name is the title's own display name, as `build_template` has it.
	const title = name.trim() || svgDisplayName(o);
	const about = (description ?? '').trim();
	checkText('the name', title, MAX_NAME_CHARS, true);
	checkText('the description', about, MAX_DESCRIPTION_CHARS, false);
	checkControls(controls, o.svg);
	const { width, height } = shapeOf(o.svg);
	const info: TemplateInfo = {
		id: uid(),
		name: title,
		description: about,
		duration,
		width,
		height,
		controls: structuredClone(controls),
		fonts: [],
		has_poster: true,
		size_bytes: o.svg.length + 900
	};
	library.set(info.id, { info, svg: o.svg });
	return { info: structuredClone(info), warnings: [] };
}

/** `apply_template` over a timeline object: add the template's artwork as an animated title with `values` spliced in. */
export function applyTemplateTo(
	timeline: Timeline,
	templateId: string,
	values: Record<string, string>,
	start: number,
	end: number | null | undefined,
	layout: SvgLayout = {}
): { overlay: SvgOverlay; warnings: string[] } {
	checkId(templateId);
	const entry = library.get(templateId);
	if (!entry) throw missing(templateId);
	const { info } = entry;
	const svg = applyValues(entry.svg, info.controls, values);
	if (!Number.isFinite(start)) throw invalid('start must be a number of seconds');
	const from = Math.max(0, start);
	if (end != null && (!Number.isFinite(end) || end <= from)) throw invalid('overlay end must be after start');
	const to = end ?? from + info.duration;
	if (to > MAX_SVG_SECONDS) throw invalid(`an animated title ends within the first ${MAX_SVG_SECONDS / 3600} hours`);
	const instance: TemplateInstance = {
		template_id: info.id,
		name: info.name,
		controls: structuredClone(info.controls),
		values: readValues(svg, info.controls)
	};
	const overlay: SvgOverlay = {
		id: uid(),
		name: layout.name?.trim() || info.name,
		svg,
		start: from,
		end: to,
		x: layout.x ?? 0,
		y: layout.y ?? 0,
		width: layout.width ?? 1,
		height: layout.height ?? 1,
		opacity: layout.opacity ?? 1,
		template: instance
	};
	checkRect(overlay);
	if ((timeline.svg_overlays?.length ?? 0) >= MAX_SVG_OVERLAYS) {
		throw invalid(`a timeline holds at most ${MAX_SVG_OVERLAYS} animated titles`);
	}
	(timeline.svg_overlays ??= []).push(overlay);
	return { overlay: structuredClone(overlay), warnings: [] };
}

/** `set_template_values` over a timeline object: re-splice the title's own artwork; the id, timing and placement stay. */
export function setTemplateValuesIn(
	timeline: Timeline,
	overlayId: string,
	values: Record<string, string>
): { overlay: SvgOverlay; warnings: string[] } {
	const at = timeline.svg_overlays?.findIndex((o) => o.id === overlayId) ?? -1;
	if (at < 0) throw invalid(`animated title not found: ${overlayId}`);
	const o = timeline.svg_overlays![at];
	if (!o.template) {
		throw invalid(`“${svgDisplayName(o)}” is not a template title, so it has no values to set`);
	}
	const svg = applyValues(o.svg, o.template.controls, values);
	const next: SvgOverlay = {
		...structuredClone(o),
		svg,
		template: { ...structuredClone(o.template), values: readValues(svg, o.template.controls) }
	};
	timeline.svg_overlays![at] = next;
	return { overlay: structuredClone(next), warnings: [] };
}

const absolute = (path: string) => /^(?:\/|[A-Za-z]:[\\/])/.test(path);
const hasExtension = (path: string) => /\.[^./\\]+$/.test(path.replace(/[\\/]+$/, '').split(/[\\/]/).pop() ?? '');

/** `export_template`: the harness keeps the "file" under its path so `importTemplateDev` can read it back. */
export function exportTemplateDev(id: string, path: string): string {
	checkId(id);
	if (!absolute(path)) throw invalid(`output path ${JSON.stringify(path)} must be an absolute local file path`);
	const entry = library.get(id);
	if (!entry) throw missing(id);
	let target = path;
	if (/[\\/]$/.test(path)) {
		target = `${path}${entry.info.name.replace(/[^\p{L}\p{N}]+/gu, '-').replace(/^-|-$/g, '') || 'template'}.${TEMPLATE_EXTENSION}`;
	} else if (!hasExtension(path)) {
		target = `${path}.${TEMPLATE_EXTENSION}`;
	}
	files.delete(target);
	files.set(target, structuredClone(entry));
	return target;
}

/** The path the most recent `export_template` wrote in this session, for the harness's import picker; `null` if none. */
export function latestExportedTemplate(): string | null {
	const paths = [...files.keys()];
	return paths.length ? paths[paths.length - 1] : null;
}

/** `import_template`: never overwrites. The same template is `already_present`, another one with its id is kept under a new id. */
export function importTemplateDev(path: string): ImportOutcome {
	if (!path.toLowerCase().endsWith(`.${TEMPLATE_EXTENSION}`)) throw invalid(`${path} is not a .${TEMPLATE_EXTENSION} file`);
	if (!absolute(path)) throw invalid(`${path} is a relative path — give the full path of the file`);
	const file = files.get(path);
	if (!file) throw invalid(`${path} does not exist`);
	const have = library.get(file.info.id);
	const same = (a: Entry, b: Entry) =>
		a.svg === b.svg &&
		JSON.stringify({ ...a.info, size_bytes: 0 }) === JSON.stringify({ ...b.info, size_bytes: 0 });
	if (have && same(have, file)) {
		return { info: structuredClone(have.info), status: 'already_present', warnings: [] };
	}
	const entry = structuredClone(file);
	let status: ImportOutcome['status'] = 'added';
	if (have) {
		entry.info.id = uid();
		status = 'added_as_copy';
	}
	library.set(entry.info.id, entry);
	return { info: structuredClone(entry.info), status, warnings: [] };
}

/** `delete_template`: titles already made from it keep working (they carry their own artwork). */
export function deleteTemplateDev(id: string): void {
	checkId(id);
	if (!library.delete(id)) throw missing(id);
}
