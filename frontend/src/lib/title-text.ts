/* A title's text as the user shapes it — pure. kerf-core may store a title's text as glyph outlines: the `<text>`
 * renamed `<g data-kerf-text="">`, everything it had still inside, and one generated `<g data-kerf-glyphs="">` of
 * paths (`svg/outline.rs`). `textForm` is a port of its `restore_text`, so the source box shows the text a user can
 * read and edit; `outline-corpus.test.ts` replays what kerf-core wrote down. A text's box (`inline-size`, and
 * `data-kerf-fit="shrink"`) is read off the start tag, which an outlined text keeps. The browser harness cannot set
 * text as outlines: it writes the box and says so. */
import { findTitle, titleOf } from './title-clips';
import type { Clip, SvgOverlay, TextBox, TextFit, Timeline, TitleTextForm } from './types';

export const TEXT_MARK = 'data-kerf-text';
export const GLYPHS_MARK = 'data-kerf-glyphs';
export const FIT_MARK = 'data-kerf-fit';
/** The widest box a text may have, in the title's units (`MAX_BOX_WIDTH`). */
export const MAX_BOX_WIDTH = 100_000;

const invalid = (why: string) => new Error(`invalid argument: ${why}`);

// Comments, CDATA, processing instructions and declarations are skipped whole; a start tag's quoted values may
// hold `>`.
const TOKEN =
	/<!--[\s\S]*?-->|<!\[CDATA\[[\s\S]*?\]\]>|<\?[\s\S]*?\?>|<![^>]*>|<\/([A-Za-z_][\w:.-]*)\s*>|<([A-Za-z_][\w:.-]*)((?:"[^"]*"|'[^']*'|[^'">])*?)(\/?)>/g;
const ATTRIBUTE = /([^\s=>/]+)\s*=\s*(?:"([^"]*)"|'([^']*)')/g;

function attributes(raw: string): Map<string, string> {
	const attrs = new Map<string, string>();
	for (const a of raw.matchAll(ATTRIBUTE)) if (!attrs.has(a[1])) attrs.set(a[1], a[2] ?? a[3] ?? '');
	return attrs;
}

/** Whether any of the title's text is drawn as outlines. */
export const isOutlined = (svg: string): boolean => svg.includes(`${TEXT_MARK}=""`);

/** `svg` with every outlined text back as the `<text>` it was, byte for byte (`restore_text`); as it is with none. */
export function textForm(svg: string): string {
	if (!svg.includes(TEXT_MARK)) return svg;
	const head = `<g ${TEXT_MARK}=""`;
	interface Open {
		name: string;
		start: number;
		marked: boolean;
		/** Inside a marked element, directly: a generated group is cut whole. */
		glyphs: boolean;
	}
	const stack: Open[] = [];
	const cuts: { from: number; to: number; with: string }[] = [];
	for (const m of svg.matchAll(TOKEN)) {
		const at = m.index ?? 0;
		const end = at + m[0].length;
		if (m[1] !== undefined) {
			const open = stack.pop();
			if (!open) continue;
			if (open.glyphs) cuts.push({ from: open.start, to: end, with: '' });
			else if (open.marked && m[1] === 'g') {
				cuts.push({ from: open.start, to: open.start + head.length, with: '<text' });
				cuts.push({ from: at + 2, to: at + 3, with: 'text' });
			}
			continue;
		}
		if (m[2] === undefined) continue;
		const attrs = attributes(m[3]);
		const parent = stack[stack.length - 1];
		const glyphs = !!parent?.marked && attrs.has(GLYPHS_MARK);
		if (m[4] === '/') {
			if (glyphs) cuts.push({ from: at, to: end, with: '' });
			continue;
		}
		const marked = m[2] === 'g' && attrs.has(TEXT_MARK) && svg.startsWith(head, at);
		stack.push({ name: m[2], start: at, marked, glyphs });
	}
	cuts.sort((a, b) => a.from - b.from || a.to - b.to);
	let out = '';
	let at = 0;
	for (const c of cuts) {
		if (c.from < at) continue;
		out += svg.slice(at, c.from) + c.with;
		at = c.to;
	}
	return out + svg.slice(at);
}

/** A text of a title that has an `id`: what can be given a box. */
export interface TitleText {
	id: string;
	/** Drawn as outlines now. */
	outlined: boolean;
	box: TextBox;
}

/** The box written on a start tag's attributes (`inline-size`, or one in its `style`; the fit mark). */
function boxOf(attrs: Map<string, string>): TextBox {
	const fromStyle = (attrs.get('style') ?? '')
		.split(';')
		.map((d) => d.split(':'))
		.find(([p]) => p?.trim().toLowerCase() === 'inline-size')?.[1];
	const raw = (attrs.get('inline-size') ?? fromStyle ?? '').trim().replace(/px$/, '').trim();
	const n = raw === '' ? NaN : Number(raw);
	const fit: TextFit = attrs.get(FIT_MARK)?.trim() === 'shrink' ? 'shrink' : 'wrap';
	return { width: Number.isFinite(n) && n > 0 ? n : null, fit };
}

/** Every text of the title with an `id`, in document order, with its box (`text_box_of`). */
export function titleTexts(svg: string): TitleText[] {
	const out: TitleText[] = [];
	for (const m of svg.matchAll(TOKEN)) {
		if (m[2] === undefined) continue;
		const local = m[2].split(':').pop();
		const attrs = attributes(m[3]);
		const outlined = local === 'g' && attrs.has(TEXT_MARK);
		if (local !== 'text' && !outlined) continue;
		const id = attrs.get('id');
		if (id) out.push({ id, outlined, box: boxOf(attrs) });
	}
	return out;
}

/** Why a typed box width cannot be one, or `null` (empty is no box). */
export function boxWidthRefusal(raw: string): string | null {
	if (raw.trim() === '') return null;
	const n = Number(raw);
	if (!Number.isFinite(n) || n <= 0) return 'a box is wider than nothing';
	if (n > MAX_BOX_WIDTH) return `a box is at most ${MAX_BOX_WIDTH.toLocaleString('en-US')} units wide`;
	return null;
}

/** A width to a thousandth, without trailing zeros (as kerf-core writes it). */
const number = (v: number): string => v.toFixed(3).replace(/\.?0+$/, '');

/** `svg` with the `<text>` `element` given `box` (`set_text_box`): its old `inline-size` and fit mark (and an
 *  `inline-size` in its `style`) taken out, the new ones written at the end of its start tag. The harness's. */
export function withTextBox(svg: string, element: string, box: TextBox): string {
	if (box.width !== null && boxWidthRefusal(String(box.width))) throw invalid(boxWidthRefusal(String(box.width))!);
	const tags = [...svg.matchAll(TOKEN)].filter((m) => m[2] !== undefined && attributes(m[3]).get('id') === element);
	if (tags.length === 0) throw invalid(`the title has no element with the id “${element}”`);
	if (tags.length > 1) throw invalid(`more than one element has the id “${element}”`);
	const tag = tags[0];
	if (tag[2].split(':').pop() !== 'text') throw invalid(`“${element}” is a <${tag[2]}>; a box is given to a <text>`);
	let raw = tag[3]
		.replace(/\s+(?:inline-size|data-kerf-fit)\s*=\s*(?:"[^"]*"|'[^']*')/g, '')
		.replace(/(\sstyle\s*=\s*)(["'])([^"']*)\2/, (_, pre: string, q: string, body: string) => {
			const kept = body.split(';').filter((d) => d.split(':')[0]?.trim().toLowerCase() !== 'inline-size' && d.trim() !== '');
			return `${pre}${q}${kept.join(';')}${q}`;
		});
	if (box.width !== null) {
		raw += ` inline-size="${number(box.width)}"`;
		if (box.fit === 'shrink') raw += ` ${FIT_MARK}="shrink"`;
	}
	const at = tag.index ?? 0;
	const rebuilt = `<${tag[2]}${raw}${tag[4]}>`;
	return svg.slice(0, at) + rebuilt + svg.slice(at + tag[0].length);
}

/** The harness's note: it has no text engine. */
export const HARNESS_NOTE = 'The browser harness draws text with its fonts: the app sets it as outlines, where a box wraps or shrinks.';

/** A floating title or a title clip by id, for an edit of its artwork; a preset title is refused. */
function editTitle(
	timeline: Timeline,
	id: string,
	change: (svg: string) => string
): { overlay?: SvgOverlay; clip?: Clip } {
	const at = timeline.svg_overlays?.findIndex((o) => o.id === id) ?? -1;
	if (at >= 0) {
		const o = timeline.svg_overlays![at];
		if (o.preset) throw invalid(`“${o.name || o.preset.params.text}” is drawn from a preset's parameters: convert it to custom artwork first`);
		const next = { ...structuredClone(o), svg: change(o.svg) };
		timeline.svg_overlays![at] = next;
		return { overlay: structuredClone(next) };
	}
	for (const track of timeline.tracks) {
		const i = track.clips.findIndex((c) => c.id === id);
		if (i < 0) continue;
		const title = titleOf(track.clips[i]);
		if (!title) throw invalid(`the clip ${id} is not a title`);
		const next = structuredClone(track.clips[i]);
		next.generator = { kind: 'title', ...title, svg: change(title.svg) };
		track.clips[i] = next;
		return { clip: structuredClone(next) };
	}
	throw invalid(`animated title not found: ${id}`);
}

/** `set_title_text_box` in the harness: the box is written; nothing is outlined. */
export function setTitleTextBoxIn(
	timeline: Timeline,
	id: string,
	element: string,
	box: TextBox
): { overlay?: SvgOverlay; clip?: Clip; warnings: string[] } {
	return { ...editTitle(timeline, id, (svg) => withTextBox(textForm(svg), element, box)), warnings: [HARNESS_NOTE] };
}

/** `convert_title_text` in the harness: back to text works; outlines it cannot make, and says so. */
export function convertTitleTextIn(
	timeline: Timeline,
	id: string,
	form: TitleTextForm
): { overlay?: SvgOverlay; clip?: Clip; warnings: string[] } {
	if (form === 'outlines') return { ...editTitle(timeline, id, (svg) => svg), warnings: [HARNESS_NOTE] };
	if (findTitle(timeline, id)?.template) {
		throw invalid(
			"a template title's text is set as outlines whenever its values change; save it as custom artwork to draw it with its fonts"
		);
	}
	return {
		...editTitle(timeline, id, (svg) => {
			const text = textForm(svg);
			if (text === svg) throw invalid("the title's text is drawn with its fonts already");
			return text;
		}),
		warnings: []
	};
}
