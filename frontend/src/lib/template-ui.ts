// Motion templates, the page's side of them — pure. The library, the saving, the splicing of a value into the
// artwork and the sanitiser are the engine's (`kerf_core::template`); the page never draws or rewrites the
// SVG. What it keeps here is what the dialog and the Inspector need to *ask well*: a control's id made from its
// label, the attribute a kind of control most likely sets, the refusals the engine will make said before the
// round trip, and the words around an outcome. The engine stays the authority: nothing here is the only check.

import type { ImportOutcome, SvgElementId, TemplateControl, TemplateInfo, TemplateKind, TemplateSaved } from './types';

// ---- limits (kerf-core `template/validate.rs`) ----------------------------------------------------

export const MAX_CONTROLS = 64;
export const MAX_TARGETS = 16;
export const MAX_NAME_CHARS = 120;
export const MAX_LABEL_CHARS = 80;
export const MAX_DESCRIPTION_CHARS = 2000;
export const MAX_ID_CHARS = 64;
export const MAX_TEXT_CHARS = 2000;
export const MAX_FAMILY_CHARS = 64;

// ---- the kinds ------------------------------------------------------------------------------------

export interface KindChoice {
	id: TemplateKind;
	label: string;
	hint: string;
}

/** In the order they are offered. */
export const KIND_CHOICES: readonly KindChoice[] = [
	{ id: 'text', label: 'Text', hint: 'The words of a text element' },
	{ id: 'color', label: 'Color', hint: 'A color: a fill, a stroke, a gradient stop' },
	{ id: 'number', label: 'Number', hint: 'A plain number in an attribute: a size, a width, an opacity' },
	{ id: 'font', label: 'Font', hint: 'The font family of a text element' },
	{ id: 'image', label: 'Image', hint: 'The picture an image element shows' }
];

export const kindLabel = (kind: TemplateKind): string => KIND_CHOICES.find((k) => k.id === kind)?.label ?? kind;

/** The attributes a `color` control may write. */
export const COLOR_ATTRIBUTES: readonly string[] = ['fill', 'stroke', 'stop-color', 'flood-color', 'lighting-color', 'color'];
/** The attributes an `image` control may write: the link of an `<image>`. */
export const IMAGE_ATTRIBUTES: readonly string[] = ['href', 'xlink:href'];
export const FONT_ATTRIBUTE = 'font-family';
/** The elements whose text a `text` control may replace. */
export const TEXT_TAGS: readonly string[] = ['text', 'tspan', 'textPath'];

/** The attributes a kind may write when that is a closed set, else `null` (a number takes any plain attribute). */
export function attributeChoices(kind: TemplateKind): readonly string[] | null {
	switch (kind) {
		case 'color':
			return COLOR_ATTRIBUTES;
		case 'image':
			return IMAGE_ATTRIBUTES;
		case 'font':
			return [FONT_ATTRIBUTE];
		case 'text':
			return [];
		case 'number':
			return null;
	}
}

/** Whether a kind writes an attribute (every kind but `text`, which replaces the element's words). */
export const takesAttribute = (kind: TemplateKind): boolean => kind !== 'text';

/** Names no control may write: the element's identity, its styling hooks, its links, script and namespace plumbing. */
export function forbiddenAttribute(name: string): boolean {
	return ['id', 'class', 'style', 'href', 'xlink:href'].includes(name) || name.startsWith('xmlns') || name.startsWith('on');
}

const ATTRIBUTE_NAME = /^(?:xlink:)?[A-Za-z_][A-Za-z0-9_.-]{0,63}$/;

// ---- ids ------------------------------------------------------------------------------------------

/** `[A-Za-z][A-Za-z0-9_-]{0,63}`: a control's id is a key in JSON and a name in a tool call. */
export const isControlId = (id: string): boolean => /^[A-Za-z][A-Za-z0-9_-]{0,63}$/.test(id);

/** A control id made from its label: lower case, accents dropped, anything else a hyphen, a letter first, at most
 *  64 characters, and `-2`, `-3`… until it is not in `taken`. A label with nothing to keep is `control`. */
export function slugId(label: string, taken: ReadonlySet<string> | readonly string[] = []): string {
	const used = taken instanceof Set ? taken : new Set(taken);
	let base = label
		.normalize('NFKD')
		.replace(/[̀-ͯ]/g, '')
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '');
	if (!/^[a-z]/.test(base)) base = base ? `c-${base}` : 'control';
	base = base.slice(0, MAX_ID_CHARS).replace(/-+$/, '');
	let id = base;
	for (let n = 2; used.has(id); n++) {
		const suffix = `-${n}`;
		id = `${base.slice(0, MAX_ID_CHARS - suffix.length).replace(/-+$/, '')}${suffix}`;
	}
	return id;
}

// ---- reading the title's own source for the attributes its elements have ---------------------------
// A control sets an attribute the element already has (the engine refuses one it does not). The title's
// source is data here, read only for names and values: a regular-expression walk of its start tags, enough
// to offer the right attribute — the engine's parser decides at save time.

const START_TAG = /<([A-Za-z_][\w:.-]*)((?:\s+[^\s=>/]+\s*=\s*(?:"[^"]*"|'[^']*'))*)\s*\/?>/g;
const ATTRIBUTE = /([^\s=>/]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;

/** The attributes of the element with this `id`, name to raw value, or `null` when no start tag carries it. */
export function elementAttributes(svg: string, elementId: string): Map<string, string> | null {
	for (const tag of svg.matchAll(START_TAG)) {
		const attrs = new Map<string, string>();
		for (const a of tag[2].matchAll(ATTRIBUTE)) attrs.set(a[1], a[2] ?? a[3] ?? '');
		if (attrs.get('id') === elementId) return attrs;
	}
	return null;
}

/** The attribute names a number control could set on this element: those that hold a plain decimal now. */
export function numericAttributes(svg: string, elementId: string): string[] {
	const attrs = elementAttributes(svg, elementId);
	if (!attrs) return [];
	return [...attrs].filter(([name, value]) => !forbiddenAttribute(name) && PLAIN_NUMBER.test(value.trim())).map(([name]) => name);
}

const PLAIN_NUMBER = /^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/;

const DATA_IMAGE = /^data:image\/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/=]+$/i;

/** The picture an `image` control currently shows, as a `data:` URL an `<img>` may take, or `null`: the value is in
 *  the title's own `svg` (an image control has no entry in `values`). Only the formats the engine keeps pass. */
export function imageHref(svg: string, control: Pick<TemplateControl, 'targets'>): string | null {
	const target = control.targets[0];
	if (!target?.attribute) return null;
	const value = elementAttributes(svg, target.element)?.get(target.attribute)?.trim();
	return value && DATA_IMAGE.test(value) ? value : null;
}

// ---- defaults for a new control -------------------------------------------------------------------

const COLOR_BY_TAG: Record<string, string> = {
	stop: 'stop-color',
	feFlood: 'flood-color',
	feDropShadow: 'flood-color',
	feDiffuseLighting: 'lighting-color',
	feSpecularLighting: 'lighting-color'
};
const NUMBER_BY_TAG: Record<string, string> = {
	text: 'font-size',
	tspan: 'font-size',
	circle: 'r',
	ellipse: 'rx',
	rect: 'width',
	image: 'width',
	line: 'stroke-width',
	path: 'stroke-width'
};
const NUMBER_COMMON = ['opacity', 'stroke-width', 'font-size', 'width', 'height', 'r', 'x', 'y'];

/** The attribute a new control of this kind most likely sets on an element of this tag: for a color the one that
 *  element draws with (`stop-color` on a gradient stop, else `fill`), for an image `href`, for a font `font-family`,
 *  for a number the usual size of that shape. When the element's attributes are known (`present`) it prefers one the
 *  element already has — the engine refuses to set one it lacks. A number with nothing likely is empty: free. */
export function defaultAttribute(kind: TemplateKind, tag: string, present: ReadonlySet<string> | null): string {
	switch (kind) {
		case 'text':
			return '';
		case 'font':
			return FONT_ATTRIBUTE;
		case 'image':
			return present?.has('xlink:href') && !present.has('href') ? 'xlink:href' : 'href';
		case 'color': {
			const order = [COLOR_BY_TAG[tag], 'fill', 'stroke', 'color', ...COLOR_ATTRIBUTES].filter((a): a is string => !!a);
			return (present && order.find((a) => present.has(a))) || order[0];
		}
		case 'number': {
			const order = [NUMBER_BY_TAG[tag], ...NUMBER_COMMON].filter((a): a is string => !!a);
			return (present && order.find((a) => present.has(a))) || (present ? '' : order[0]);
		}
	}
}

/** The elements a kind can aim at, in the title's order: text at a text element, an image at an `<image>`, anything
 *  else at any element with an id. */
export function elementsFor(kind: TemplateKind, elements: readonly SvgElementId[]): SvgElementId[] {
	const seen = new Set<string>();
	return elements.filter((e) => {
		if (seen.has(e.id)) return false;
		seen.add(e.id);
		if (kind === 'text') return TEXT_TAGS.includes(e.tag);
		if (kind === 'image') return e.tag === 'image';
		return true;
	});
}

// ---- the dialog's draft ---------------------------------------------------------------------------

export interface DraftTarget {
	element: string;
	attribute: string;
	/** The attribute was typed or picked: a new element does not overwrite it. */
	touched: boolean;
}

/** A control being edited: strings all the way down (a number field holds what was typed). */
export interface DraftControl {
	/** Stable for the life of the dialog, so a keyed list does not lose a row's focus. */
	key: number;
	id: string;
	/** The id was edited by hand: the label no longer rewrites it. */
	idTouched: boolean;
	label: string;
	kind: TemplateKind;
	targets: DraftTarget[];
	min: string;
	max: string;
}

/** What the draft is made against: the title's source and the ids of its elements. */
export interface DraftContext {
	svg: string;
	elements: readonly SvgElementId[];
}

const tagOf = (ctx: DraftContext, element: string) => ctx.elements.find((e) => e.id === element)?.tag ?? '';
const presentOf = (ctx: DraftContext, element: string): ReadonlySet<string> | null => {
	const attrs = elementAttributes(ctx.svg, element);
	return attrs ? new Set(attrs.keys()) : null;
};

/** An empty control, the first element it can aim at already chosen when there is exactly one to choose. */
export function newDraft(key: number, ctx: DraftContext, kind: TemplateKind = 'text'): DraftControl {
	return { key, id: '', idTouched: false, label: '', kind, targets: [blankTarget(kind, ctx)], min: '', max: '' };
}

function blankTarget(kind: TemplateKind, ctx: DraftContext): DraftTarget {
	const options = elementsFor(kind, ctx.elements);
	return options.length === 1 ? targetAt(kind, options[0].id, ctx) : { element: '', attribute: kind === 'font' ? FONT_ATTRIBUTE : '', touched: false };
}

function targetAt(kind: TemplateKind, element: string, ctx: DraftContext): DraftTarget {
	return { element, attribute: takesAttribute(kind) ? defaultAttribute(kind, tagOf(ctx, element), presentOf(ctx, element)) : '', touched: false };
}

/** A saved control as a draft (editing the controls a template title already has). */
export function draftFrom(control: TemplateControl, key: number): DraftControl {
	return {
		key,
		id: control.id,
		idTouched: true,
		label: control.label,
		kind: control.kind,
		targets: control.targets.map((t) => ({ element: t.element, attribute: t.attribute ?? '', touched: true })),
		min: control.min === undefined ? '' : String(control.min),
		max: control.max === undefined ? '' : String(control.max)
	};
}

/** The control a draft describes. Bounds only on a number; an empty or unreadable bound is none. */
export function toControl(d: DraftControl): TemplateControl {
	const out: TemplateControl = {
		id: d.id.trim(),
		label: d.label.trim(),
		kind: d.kind,
		targets: d.targets.map((t) => (takesAttribute(d.kind) ? { element: t.element, attribute: t.attribute.trim() } : { element: t.element }))
	};
	if (d.kind === 'number') {
		const min = bound(d.min);
		const max = bound(d.max);
		if (min !== undefined) out.min = min;
		if (max !== undefined) out.max = max;
	}
	return out;
}

function bound(text: string): number | undefined {
	const t = text.trim();
	if (!t || !PLAIN_NUMBER.test(t)) return undefined;
	const n = Number(t);
	return Number.isFinite(n) ? n : undefined;
}

/** The label changed: the id follows it until the id has been edited by hand, kept clear of the others' ids. */
export function relabel(d: DraftControl, label: string, others: readonly DraftControl[]): DraftControl {
	if (d.idTouched) return { ...d, label };
	return { ...d, label, id: label.trim() ? slugId(label, others.map((o) => o.id)) : '' };
}

/** The kind changed: a target the new kind cannot aim at is cleared, an attribute that was not chosen by hand is
 *  the new kind's likely one, and bounds belong to a number only. */
export function changeKind(d: DraftControl, kind: TemplateKind, ctx: DraftContext): DraftControl {
	const options = elementsFor(kind, ctx.elements);
	const targets = d.targets.map((t) => {
		const keep = options.some((e) => e.id === t.element);
		const element = keep ? t.element : options.length === 1 && d.targets.length === 1 ? options[0].id : '';
		if (!element) return { element: '', attribute: kind === 'font' ? FONT_ATTRIBUTE : '', touched: false };
		return targetAt(kind, element, ctx);
	});
	return { ...d, kind, targets, min: kind === 'number' ? d.min : '', max: kind === 'number' ? d.max : '' };
}

/** An element was picked for target `at`: its attribute becomes the likely one unless it was chosen by hand. */
export function pickElement(d: DraftControl, at: number, element: string, ctx: DraftContext): DraftControl {
	const targets = d.targets.map((t, i) => {
		if (i !== at) return t;
		const next = targetAt(d.kind, element, ctx);
		return t.touched && takesAttribute(d.kind) && t.attribute ? { ...next, attribute: t.attribute, touched: true } : next;
	});
	return { ...d, targets };
}

export function setAttribute(d: DraftControl, at: number, attribute: string): DraftControl {
	return { ...d, targets: d.targets.map((t, i) => (i === at ? { ...t, attribute, touched: true } : t)) };
}

export function addTarget(d: DraftControl): DraftControl {
	if (d.targets.length >= MAX_TARGETS) return d;
	const last = d.targets[d.targets.length - 1];
	return { ...d, targets: [...d.targets, { element: '', attribute: takesAttribute(d.kind) ? (last?.attribute ?? '') : '', touched: !!last?.touched }] };
}

export function removeTarget(d: DraftControl, at: number): DraftControl {
	return d.targets.length <= 1 ? d : { ...d, targets: d.targets.filter((_, i) => i !== at) };
}

// ---- refusing a draft before the round trip -------------------------------------------------------

/** What is wrong with one control, said to the person making it, or `null`. The same rules the engine applies at
 *  save time (`validate_controls`), in the order it is easiest to fix them. */
export function controlProblem(d: DraftControl, all: readonly DraftControl[], ctx: DraftContext): string | null {
	const label = d.label.trim();
	if (!label) return 'Give it a label: it is what you read next to the field.';
	if ([...label].length > MAX_LABEL_CHARS) return `A label is at most ${MAX_LABEL_CHARS} characters.`;
	const id = d.id.trim();
	if (!isControlId(id)) {
		return `The id starts with a letter, then letters, digits, - or _ (${MAX_ID_CHARS} at most).`;
	}
	if (all.some((o) => o.key !== d.key && o.id.trim() === id)) return `Another control already uses the id “${id}”.`;
	if (d.targets.length === 0) return 'Pick the element it changes.';
	for (const t of d.targets) {
		if (!t.element) return 'Pick the element it changes.';
		const tag = ctx.elements.find((e) => e.id === t.element)?.tag;
		if (tag === undefined) return `The title has no element with the id “${t.element}”.`;
		if (d.kind === 'text' && !TEXT_TAGS.includes(tag)) return `“${t.element}” is a <${tag}>, not text: aim a text control at a <text> or <tspan>.`;
		if (d.kind === 'image' && tag !== 'image') return `“${t.element}” is a <${tag}>, not an <image>.`;
		if (!takesAttribute(d.kind)) continue;
		const attribute = t.attribute.trim();
		if (!attribute) return 'Say which attribute it sets.';
		const closed = attributeChoices(d.kind);
		if (closed ? !closed.includes(attribute) : !ATTRIBUTE_NAME.test(attribute) || forbiddenAttribute(attribute)) {
			return `A ${d.kind} control cannot set “${attribute}”${closed ? `: use ${closed.join(', ')}` : ''}.`;
		}
		const has = elementAttributes(ctx.svg, t.element);
		if (has && !has.has(attribute)) {
			return `“${t.element}” has no “${attribute}” attribute to set. Add it to the element in the SVG source first.`;
		}
	}
	if (d.kind === 'number') {
		for (const [name, text] of [['Min', d.min], ['Max', d.max]] as const) {
			if (text.trim() && bound(text) === undefined) return `${name} is not a number.`;
		}
		const min = bound(d.min);
		const max = bound(d.max);
		if (min !== undefined && max !== undefined && min > max) return 'Min is above max.';
	}
	const placeOf = (o: DraftControl, t: DraftTarget) => `${t.element}\u0000${takesAttribute(o.kind) ? t.attribute.trim() : ''}`;
	const mine = d.targets.filter((t) => t.element).map((t) => placeOf(d, t));
	if (new Set(mine).size < mine.length) return 'It sets the same thing more than once.';
	for (const o of all.slice(0, all.findIndex((c) => c.key === d.key))) {
		const taken = new Set(o.targets.filter((t) => t.element).map((t) => placeOf(o, t)));
		const clash = d.targets.find((t) => t.element && taken.has(placeOf(d, t)));
		if (clash) {
			const what = takesAttribute(d.kind) ? `${clash.attribute.trim()} of “${clash.element}”` : `the text of “${clash.element}”`;
			return `“${o.label.trim() || o.id}” already sets ${what}.`;
		}
	}
	return null;
}

export interface DraftCheck {
	/** The name's problem, if any. */
	name: string | null;
	description: string | null;
	/** One entry per control, in order. */
	controls: (string | null)[];
	/** Nothing to fix. */
	ok: boolean;
}

export function checkDraft(name: string, description: string, drafts: readonly DraftControl[], ctx: DraftContext): DraftCheck {
	const n = name.trim();
	const nameProblem = !n
		? 'Give the template a name.'
		: [...n].length > MAX_NAME_CHARS
			? `A name is at most ${MAX_NAME_CHARS} characters.`
			: null;
	const descriptionProblem = [...description].length > MAX_DESCRIPTION_CHARS ? `A description is at most ${MAX_DESCRIPTION_CHARS} characters.` : null;
	const controls = drafts.map((d) => controlProblem(d, drafts, ctx));
	const tooMany = drafts.length > MAX_CONTROLS;
	return { name: nameProblem, description: descriptionProblem, controls, ok: !nameProblem && !descriptionProblem && !tooMany && controls.every((c) => c === null) };
}

/** Where a control writes, in a line: `Tagline · text of tagline`, `Brand · fill of bg-top`. */
export function targetsLine(control: Pick<TemplateControl, 'targets'>): string {
	const parts = control.targets.map((t) => (t.attribute ? `${t.attribute} of ${t.element}` : `text of ${t.element}`));
	return parts.length > 2 ? `${parts.slice(0, 2).join(', ')} +${parts.length - 2}` : parts.join(', ');
}

// ---- values ---------------------------------------------------------------------------------------

/** Why the engine would refuse `raw` for this control, in words for the field, or `null`. Text is taken as typed;
 *  the others are trimmed first (`normalizeValue`). */
export function valueRefusal(control: Pick<TemplateControl, 'kind' | 'min' | 'max'>, raw: string): string | null {
	switch (control.kind) {
		case 'text':
			if ([...raw].length > MAX_TEXT_CHARS) return `At most ${MAX_TEXT_CHARS} characters.`;
			if (/[\p{Cc}]/u.test(raw.replace(/[\t\n]/g, ''))) return 'Control characters cannot be in the text.';
			return null;
		case 'color': {
			const c = raw.trim();
			if (/^(?:rgba?|hsla?)\(/i.test(c)) return 'Write the color as #rrggbb (or #rrggbbaa), not as a function.';
			if (/^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(c) || /^[A-Za-z]+$/.test(c)) return null;
			return 'Use #rrggbb, #rgb or #rrggbbaa, a color name, none or currentColor.';
		}
		case 'number': {
			const t = raw.trim();
			if (!t) return 'Enter a number.';
			if (!PLAIN_NUMBER.test(t)) return /^[+-]?\d/.test(t) ? 'Just the number: no unit.' : 'Not a number.';
			const n = Number(t);
			if (!Number.isFinite(n)) return 'Not a finite number.';
			if (control.min !== undefined && control.max !== undefined && (n < control.min || n > control.max)) return `Between ${control.min} and ${control.max}.`;
			if (control.min !== undefined && n < control.min) return `At least ${control.min}.`;
			if (control.max !== undefined && n > control.max) return `At most ${control.max}.`;
			return null;
		}
		case 'font': {
			const f = raw.trim();
			if (!f) return 'Pick a font.';
			if ([...f].length > MAX_FAMILY_CHARS || !/^[\p{L}\p{N} _.-]+$/u.test(f)) return `One family name: letters, digits, spaces, - _ . (${MAX_FAMILY_CHARS} at most).`;
			return null;
		}
		case 'image':
			return raw.startsWith('data:image/') ? null : 'Pick a picture file.';
	}
}

/** A number as the engine writes it: plain decimal, never an exponent (`1e21` is a one and twenty-one zeros), `-0` as `0`.
 *  JavaScript's shortest round-trip digits are Rust's, so the same number is the same text. */
export function plainDecimal(n: number): string {
	if (n === 0) return '0';
	const s = String(n);
	const m = /^(-?)(\d+)(?:\.(\d+))?e([+-]\d+)$/.exec(s);
	if (!m) return s;
	const [, sign, int, frac = '', exp] = m;
	const digits = int + frac;
	const point = int.length + Number(exp);
	if (point <= 0) return `${sign}0.${'0'.repeat(-point)}${digits}`;
	if (point >= digits.length) return `${sign}${digits}${'0'.repeat(point - digits.length)}`;
	return `${sign}${digits.slice(0, point)}.${digits.slice(point)}`;
}

/** The value as it is sent: a number in plain decimal, a color or a font trimmed, text as typed. */
export function normalizeValue(control: Pick<TemplateControl, 'kind'>, raw: string): string {
	switch (control.kind) {
		case 'text':
		case 'image':
			return raw;
		case 'number':
			return plainDecimal(Number(raw.trim()));
		default:
			return raw.trim();
	}
}

/** The arrow-key step of a number field: fine when the range is small, whole numbers otherwise. */
export function numberStep(control: Pick<TemplateControl, 'min' | 'max'>): number {
	const span = control.min !== undefined && control.max !== undefined ? control.max - control.min : Infinity;
	return span <= 1 ? 0.01 : span <= 20 ? 0.1 : 1;
}

/** What the swatch beside a color field shows for `value`: `#rrggbb` for any hex spelling, else `null` (a name, `none`,
 *  `currentColor`: the component asks the browser to resolve a name). */
export function swatchOf(value: string | null | undefined): string | null {
	const v = (value ?? '').trim().toLowerCase();
	const short = /^#([0-9a-f])([0-9a-f])([0-9a-f])[0-9a-f]?$/.exec(v);
	if (short) return `#${short[1]}${short[1]}${short[2]}${short[2]}${short[3]}${short[3]}`;
	const long = /^#([0-9a-f]{6})(?:[0-9a-f]{2})?$/.exec(v);
	return long ? `#${long[1]}` : null;
}

/** The text after a pick: the picked `#rrggbb`, keeping the alpha a hex spelling had (`#rrggbbaa`, `#rgba`). */
export function pickColor(current: string | null | undefined, picked: string): string {
	const color = picked.toLowerCase();
	const v = (current ?? '').trim().toLowerCase();
	const eight = /^#[0-9a-f]{6}([0-9a-f]{2})$/.exec(v);
	if (eight) return `${color}${eight[1]}`;
	const four = /^#[0-9a-f]{3}([0-9a-f])$/.exec(v);
	return four ? `${color}${four[1]}${four[1]}` : color;
}

// ---- the library's words --------------------------------------------------------------------------

/** `Barlow Condensed`, `Barlow Condensed, Inter`, `Barlow Condensed, Inter +2`; empty when it carries none. */
export function fontsLine(fonts: readonly string[]): string {
	if (fonts.length <= 2) return fonts.join(', ');
	return `${fonts.slice(0, 2).join(', ')} +${fonts.length - 2}`;
}

export const controlCount = (n: number): string => `${n} ${n === 1 ? 'control' : 'controls'}`;

/** The card's hover text: the description, then the facts that do not fit under the name. */
export function templateTooltip(info: Pick<TemplateInfo, 'name' | 'description' | 'controls' | 'fonts'>): string {
	const lines = [info.description.trim() || info.name];
	lines.push(info.controls.length ? info.controls.map((c) => c.label).join(' · ') : 'No controls: it can be applied but not changed');
	if (info.fonts.length) lines.push(`Carries ${info.fonts.join(', ')}`);
	return lines.join('\n');
}

export interface Line {
	kind: 'success' | 'info' | 'warning';
	text: string;
	/** What the engine could not carry over, if anything. */
	detail?: string;
}

const quoted = (name: string) => `“${name}”`;

/** What saving a template says: a plain success, or a warning with the engine's notes under it. */
export function savedLine(saved: TemplateSaved): Line {
	const name = quoted(saved.info.name);
	if (saved.warnings.length === 0) return { kind: 'success', text: `Saved ${name} to your templates` };
	return { kind: 'warning', text: `Saved ${name}, with notes`, detail: saved.warnings.join(' ') };
}

/** What importing a file did: it never overwrites, and says when it kept the file as a copy. */
export function importLine(outcome: ImportOutcome): Line {
	const name = quoted(outcome.info.name);
	const detail = outcome.warnings.length ? outcome.warnings.join(' ') : undefined;
	switch (outcome.status) {
		case 'already_present':
			return { kind: 'info', text: `${name} is already in your templates`, detail };
		case 'added_as_copy':
			return {
				kind: 'warning',
				text: `Added ${name} as a copy`,
				detail: ['Your library has a different template with the same id, so it was kept as a new one.', detail].filter(Boolean).join(' ')
			};
		default:
			return { kind: detail ? 'warning' : 'success', text: `Added ${name} to your templates`, detail };
	}
}

/** What applying a template, or changing its values, said it could not honour (a font that is not installed). */
export function appliedLine(name: string, warnings: readonly string[]): Line | null {
	if (warnings.length === 0) return null;
	return { kind: 'warning', text: `${quoted(name)} has notes`, detail: warnings.join(' ') };
}

/** The question before a template leaves the library. */
export function deleteMessage(name: string): string {
	return `Delete ${quoted(name)} from your templates? Titles already made from it keep working. Export it first if you want a copy.`;
}

/** The file name a template is offered under in the save dialog. */
export function exportFileName(name: string): string {
	const base = name
		.normalize('NFKD')
		.replace(/[̀-ͯ]/g, '')
		.replace(/[^A-Za-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.slice(0, 60);
	return `${base || 'template'}.kerftemplate`;
}
