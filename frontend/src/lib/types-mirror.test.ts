import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, posix } from 'node:path';
import ts from 'typescript';
import { SRC } from './test-sources';

// `types.ts` is a hand-written mirror of the serde structs and enums of kerf-core and kerf-app, and nothing else
// checks that a field added in Rust reaches it. This reads both and compares names:
//
//  - a serde struct's JSON field names against the properties of the exported interface of the same name
//    (`extends`, `&`, `Partial`, `Pick`, `Omit` followed);
//  - an enum's JSON variant names against the string-literal union (unit variants), the `tag` values and each
//    variant's own fields (internally tagged), or the keys (externally tagged).
//
// It does not compare field types or optionality, and a Rust type whose wire shape the source does not decide
// (a hand-written impl, `flatten` of a map or an enum, `untagged`, `transparent`, `from` / `into`, a tuple struct)
// is left unjudged rather than guessed at. A name on one side only is no failure: much of `types.ts` is UI-only
// and many Rust types never cross the boundary. `EXCEPTIONS` lists the differences that are on purpose and
// `MIRRORS` the types.ts types that mirror a Rust type of another name (see `.claude/docs/frontend.md`).
//
// Two Rust types may share a name (`Easing` is in the model and in the SVG text animation): a types.ts type is
// compared with each and passes when it agrees with ANY of them; `rust` pins an exception to one file.

const REPO = join(SRC, '..', '..');
const RUST_ROOTS = ['crates/kerf-core/src', 'crates/kerf-app/src'];
const TYPES_TS = join(SRC, 'lib/types.ts');

// ---- 1. Rust: tokens ----------------------------------------------------------------------------------

interface Tok {
	k: 'id' | 'life' | 'str' | 'lit' | 'p';
	v: string;
	line: number;
}

const isDigit = (c: string) => c >= '0' && c <= '9';
const isIdStart = (c: string) =>
	(c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c === '_' || (c > '\x7f' && /\p{L}/u.test(c));
const isIdPart = (c: string) => isIdStart(c) || isDigit(c) || (c > '\x7f' && /\p{N}/u.test(c));

/** Comments (line, nested block, doc) are dropped, so a `{` or `#[serde(skip)]` in prose is nothing; string,
 *  raw-string and char literals become single tokens, so a brace in one is nothing either. `->`, `=>` and
 *  `::` are one token each: a `>` the parser counts is always an angle bracket. */
function tokenize(src: string): Tok[] {
	const toks: Tok[] = [];
	const n = src.length;
	let i = 0;
	let line = 1;
	const emit = (k: Tok['k'], v: string, end: number) => {
		toks.push({ k, v, line });
		for (let j = i; j < end; j++) if (src[j] === '\n') line++;
		i = end;
	};
	/** `q` is the opening quote; a raw string ends at a quote followed by `hashes` hashes. */
	const string = (q: number, hashes: number, raw: boolean) => {
		let text = '';
		for (let j = q + 1; j < n; j++) {
			const c = src[j];
			if (!raw && c === '\\') {
				text += src[++j] ?? '';
			} else if (c === '"' && (!raw || src.startsWith('#'.repeat(hashes), j + 1))) {
				return { text, end: j + 1 + (raw ? hashes : 0) };
			} else {
				text += c;
			}
		}
		return { text, end: n };
	};
	while (i < n) {
		const c = src[i];
		if (c === '\n') {
			line++;
			i++;
		} else if (c === ' ' || c === '\t' || c === '\r') {
			i++;
		} else if (c === '/' && src[i + 1] === '/') {
			while (i < n && src[i] !== '\n') i++;
		} else if (c === '/' && src[i + 1] === '*') {
			let depth = 1;
			i += 2;
			while (i < n && depth > 0) {
				if (src[i] === '/' && src[i + 1] === '*') {
					depth++;
					i += 2;
				} else if (src[i] === '*' && src[i + 1] === '/') {
					depth--;
					i += 2;
				} else {
					if (src[i] === '\n') line++;
					i++;
				}
			}
		} else if (c === '"') {
			const s = string(i, 0, false);
			emit('str', s.text, s.end);
		} else if (c === "'") {
			if (src[i + 1] === '\\') {
				let j = i + 3;
				while (j < n && src[j] !== "'") j++;
				emit('lit', '', j + 1);
				continue;
			}
			const width = (src.codePointAt(i + 1) ?? 0) > 0xffff ? 2 : 1;
			if (src[i + 1 + width] === "'") {
				emit('lit', src.slice(i + 1, i + 1 + width), i + 2 + width);
				continue;
			}
			let j = i + 1;
			while (j < n && isIdPart(src[j])) j++;
			emit('life', src.slice(i, j), j);
		} else if (isDigit(c)) {
			let j = i + 1;
			while (j < n && isIdPart(src[j])) j++;
			if (src[j] === '.' && isDigit(src[j + 1] ?? '')) {
				j++;
				while (j < n && isIdPart(src[j])) j++;
			}
			emit('lit', src.slice(i, j), j);
		} else if (isIdStart(c)) {
			let j = i + 1;
			while (j < n && isIdPart(src[j])) j++;
			const word = src.slice(i, j);
			const raw = word === 'r' || word === 'br' || word === 'cr';
			if ((raw || word === 'b' || word === 'c') && src[j] === '"') {
				const s = string(j, 0, raw);
				emit('str', s.text, s.end);
				continue;
			}
			if (raw && src[j] === '#') {
				let h = j;
				while (src[h] === '#') h++;
				if (src[h] === '"') {
					const s = string(h, h - j, true);
					emit('str', s.text, s.end);
					continue;
				}
				if (word === 'r' && h === j + 1 && h < n && isIdStart(src[h])) {
					let e = h + 1;
					while (e < n && isIdPart(src[e])) e++;
					const name = src.slice(h, e);
					emit('id', name, e);
					continue;
				}
			}
			emit('id', word, j);
		} else {
			const two = src.slice(i, i + 2);
			if (two === '->' || two === '=>' || two === '::') emit('p', two, i + 2);
			else emit('p', c, i + 1);
		}
	}
	return toks;
}

/** `t[i]` opens a bracket: the index after the one that closes it. */
function skipGroup(t: Tok[], i: number): number {
	let depth = 0;
	for (let j = i; j < t.length; j++) {
		if (t[j].k !== 'p') continue;
		const v = t[j].v;
		if (v === '(' || v === '[' || v === '{') depth++;
		else if (v === ')' || v === ']' || v === '}') {
			depth--;
			if (depth === 0) return j + 1;
		}
	}
	return t.length;
}

/** `t[i]` is a `<`: the index after its `>`. */
function skipAngle(t: Tok[], i: number): number {
	let depth = 0;
	for (let j = i; j < t.length; j++) {
		if (t[j].k !== 'p') continue;
		const v = t[j].v;
		if (v === '<') depth++;
		else if (v === '>') {
			depth--;
			if (depth === 0) return j + 1;
		} else if (v === '(' || v === '[' || v === '{') j = skipGroup(t, j) - 1;
	}
	return t.length;
}

/** `[from, to)` cut at the commas that are not inside a bracket or a generic. */
function splitTop(t: Tok[], from: number, to: number): [number, number][] {
	const parts: [number, number][] = [];
	let start = from;
	let angle = 0;
	for (let j = from; j < to; j++) {
		if (t[j].k !== 'p') continue;
		const v = t[j].v;
		if (v === '(' || v === '[' || v === '{') j = skipGroup(t, j) - 1;
		else if (v === '<') angle++;
		else if (v === '>') angle = Math.max(0, angle - 1);
		else if (v === ',' && angle === 0) {
			parts.push([start, j]);
			start = j + 1;
		}
	}
	if (start < to) parts.push([start, to]);
	return parts;
}

function typeText(toks: Tok[]): string {
	let out = '';
	for (const [n, tok] of toks.entries()) {
		const word = (x: Tok) => x.k === 'id' || x.k === 'life' || x.k === 'lit';
		if (n > 0 && word(tok) && word(toks[n - 1])) out += ' ';
		out += tok.k === 'str' ? JSON.stringify(tok.v) : tok.v;
	}
	return out;
}

// ---- 2. Rust: attributes ------------------------------------------------------------------------------

interface Attr {
	path: string;
	args: Tok[];
}

function attrsOf(inner: Tok[]): Attr[] {
	let j = 0;
	let path = '';
	while (j < inner.length && (inner[j].k === 'id' || inner[j].v === '::')) path += inner[j++].v;
	if (inner[j]?.v === '(') {
		const args = inner.slice(j + 1, skipGroup(inner, j) - 1);
		if (path === 'cfg_attr') {
			return splitTop(args, 0, args.length)
				.slice(1)
				.flatMap(([a, b]) => attrsOf(args.slice(a, b)));
		}
		return [{ path, args }];
	}
	return [{ path, args: inner.slice(j) }];
}

/** The `#[...]` / `#![...]` attributes at `i`, and the index after them. */
function readAttrs(t: Tok[], i: number): { attrs: Attr[]; i: number } {
	const attrs: Attr[] = [];
	while (t[i]?.k === 'p' && t[i].v === '#') {
		let j = i + 1;
		if (t[j]?.v === '!') j++;
		if (t[j]?.v !== '[') break;
		const end = skipGroup(t, j);
		attrs.push(...attrsOf(t.slice(j + 1, end - 1)));
		i = end;
	}
	return { attrs, i };
}

/** Whether `cfg(...)` holds only under `cargo test`. */
function testOnly(args: Tok[]): boolean {
	if (args.length === 1) return args[0].v === 'test';
	const call = args[0]?.v;
	if ((call !== 'all' && call !== 'any') || args[1]?.v !== '(') return false;
	const inner = args.slice(2, skipGroup(args, 1) - 1);
	const parts = splitTop(inner, 0, inner.length).map(([a, b]) => testOnly(inner.slice(a, b)));
	return call === 'all' ? parts.some(Boolean) : parts.length > 0 && parts.every(Boolean);
}

const cfgTest = (attrs: Attr[]) => attrs.some((a) => a.path === 'cfg' && testOnly(a.args));

interface Meta {
	key: string;
	value?: string;
	list?: Meta[];
}

/** `skip_serializing_if = "f", rename(serialize = "a", deserialize = "b"), flatten` as a list. */
function metas(args: Tok[]): Meta[] {
	const out: Meta[] = [];
	for (const [a, b] of splitTop(args, 0, args.length)) {
		let j = a;
		let key = '';
		while (j < b && (args[j].k === 'id' || args[j].v === '::')) key += args[j++].v;
		if (!key) continue;
		if (args[j]?.v === '=') {
			const v = args[j + 1];
			out.push({ key, value: v?.k === 'str' ? v.v : args.slice(j + 1, b).map((x) => x.v).join('') });
		} else if (args[j]?.v === '(') {
			out.push({ key, list: metas(args.slice(j + 1, skipGroup(args, j) - 1)) });
		} else out.push({ key });
	}
	return out;
}

const serdeMetas = (attrs: Attr[]) => attrs.filter((a) => a.path === 'serde').flatMap((a) => metas(a.args));

/** A serde setting that may differ between writing and reading: `rename = "x"` or `rename(serialize, deserialize)`. */
interface Both {
	ser?: string;
	de?: string;
}

function both(m: Meta): Both {
	if (m.list) {
		return {
			ser: m.list.find((x) => x.key === 'serialize')?.value,
			de: m.list.find((x) => x.key === 'deserialize')?.value
		};
	}
	return { ser: m.value, de: m.value };
}

/** A field ident under `rename_all` (serde's `RenameRule::apply_to_field`). */
function fieldName(ident: string, rule?: string): string {
	const pascal = ident
		.split('_')
		.map((w) => w.charAt(0).toUpperCase() + w.slice(1))
		.join('');
	switch (rule) {
		case 'lowercase':
			return ident.toLowerCase();
		case 'UPPERCASE':
		case 'SCREAMING_SNAKE_CASE':
			return ident.toUpperCase();
		case 'PascalCase':
			return pascal;
		case 'camelCase':
			return pascal.charAt(0).toLowerCase() + pascal.slice(1);
		case 'kebab-case':
			return ident.replaceAll('_', '-');
		case 'SCREAMING-KEBAB-CASE':
			return ident.toUpperCase().replaceAll('_', '-');
		default:
			return ident;
	}
}

/** A variant ident under `rename_all` (serde's `RenameRule::apply_to_variant`). */
function variantName(ident: string, rule?: string): string {
	const snake = ident.replace(/(?<!^)([A-Z])/g, '_$1').toLowerCase();
	switch (rule) {
		case 'lowercase':
			return ident.toLowerCase();
		case 'UPPERCASE':
			return ident.toUpperCase();
		case 'camelCase':
			return ident.charAt(0).toLowerCase() + ident.slice(1);
		case 'snake_case':
			return snake;
		case 'SCREAMING_SNAKE_CASE':
			return snake.toUpperCase();
		case 'kebab-case':
			return snake.replaceAll('_', '-');
		case 'SCREAMING-KEBAB-CASE':
			return snake.toUpperCase().replaceAll('_', '-');
		default:
			return ident;
	}
}

// ---- 3. Rust: items -----------------------------------------------------------------------------------

interface RustField {
	ident: string;
	ser: string;
	de: string;
	aliases: string[];
	skipSer: boolean;
	skipDe: boolean;
	flatten: boolean;
	/** `with` / `serialize_with` / `deserialize_with`: the value is custom, the key is not. */
	custom: boolean;
	ty: Tok[];
	line: number;
}

interface RustVariant {
	ident: string;
	ser: string;
	de: string;
	aliases: string[];
	skipSer: boolean;
	skipDe: boolean;
	other: boolean;
	untagged: boolean;
	custom: boolean;
	shape: 'unit' | 'newtype' | 'tuple' | 'struct';
	/** A struct variant's fields, or a newtype variant's one type. */
	fields: RustField[];
	line: number;
}

interface Container {
	renameAll: Both;
	renameAllFields: Both;
	tag?: string;
	content?: string;
	untagged: boolean;
	transparent: boolean;
	/** `from` / `into` / `try_from` / `remote`: the wire is another type's. */
	opaque?: string;
}

interface RustType {
	name: string;
	kind: 'struct' | 'enum';
	file: string;
	line: number;
	ser: boolean;
	de: boolean;
	container: Container;
	body: 'named' | 'tuple' | 'unit';
	fields: RustField[];
	variants: RustVariant[];
	/** A hand-written `impl Serialize` / `impl Deserialize`: whatever it writes is not in the source's shape. */
	manual: string[];
}

interface ModDecl {
	name: string;
	testOnly: boolean;
	path?: string;
}

interface Scan {
	types: RustType[];
	mods: ModDecl[];
	impls: { trait: string; name: string }[];
}

function container(attrs: Attr[]): Container {
	const c: Container = { renameAll: {}, renameAllFields: {}, untagged: false, transparent: false };
	for (const m of serdeMetas(attrs)) {
		if (m.key === 'rename_all') c.renameAll = both(m);
		else if (m.key === 'rename_all_fields') c.renameAllFields = both(m);
		else if (m.key === 'tag') c.tag = m.value;
		else if (m.key === 'content') c.content = m.value;
		else if (m.key === 'untagged') c.untagged = true;
		else if (m.key === 'transparent') c.transparent = true;
		else if (['from', 'into', 'try_from', 'remote'].includes(m.key)) c.opaque = m.key;
	}
	return c;
}

function plainField(ident: string, ty: Tok[], line: number, rule: Both): RustField {
	return {
		ident,
		ser: fieldName(ident, rule.ser),
		de: fieldName(ident, rule.de),
		aliases: [],
		skipSer: false,
		skipDe: false,
		flatten: false,
		custom: false,
		ty,
		line
	};
}

function namedFields(t: Tok[], from: number, to: number, rule: Both): RustField[] {
	const out: RustField[] = [];
	for (const [a, b] of splitTop(t, from, to)) {
		const { attrs, i } = readAttrs(t, a);
		let j = i;
		if (t[j]?.v === 'pub') {
			j++;
			if (t[j]?.v === '(') j = skipGroup(t, j);
		}
		const name = t[j];
		if (name?.k !== 'id' || t[j + 1]?.v !== ':') continue;
		const f = plainField(name.v, t.slice(j + 2, b), name.line, rule);
		for (const m of serdeMetas(attrs)) {
			if (m.key === 'rename') {
				const r = both(m);
				f.ser = r.ser ?? f.ser;
				f.de = r.de ?? f.de;
			} else if (m.key === 'alias' && m.value) f.aliases.push(m.value);
			else if (m.key === 'skip') f.skipSer = f.skipDe = true;
			else if (m.key === 'skip_serializing') f.skipSer = true;
			else if (m.key === 'skip_deserializing') f.skipDe = true;
			else if (m.key === 'flatten') f.flatten = true;
			else if (['with', 'serialize_with', 'deserialize_with'].includes(m.key)) f.custom = true;
		}
		out.push(f);
	}
	return out;
}

/** The types of a tuple's `( ... )`, one range each; attributes and `pub` stripped. */
function tupleFields(t: Tok[], from: number, to: number): Tok[][] {
	return splitTop(t, from, to).map(([a, b]) => {
		let j = readAttrs(t, a).i;
		if (t[j]?.v === 'pub') {
			j++;
			if (t[j]?.v === '(') j = skipGroup(t, j);
		}
		return t.slice(j, b);
	});
}

function variants(t: Tok[], from: number, to: number, c: Container): RustVariant[] {
	const out: RustVariant[] = [];
	for (const [a] of splitTop(t, from, to)) {
		const { attrs, i } = readAttrs(t, a);
		const name = t[i];
		if (name?.k !== 'id') continue;
		const v: RustVariant = {
			ident: name.v,
			ser: variantName(name.v, c.renameAll.ser),
			de: variantName(name.v, c.renameAll.de),
			aliases: [],
			skipSer: false,
			skipDe: false,
			other: false,
			untagged: false,
			custom: false,
			shape: 'unit',
			fields: [],
			line: name.line
		};
		let fieldRule: Both = c.renameAllFields;
		for (const m of serdeMetas(attrs)) {
			if (m.key === 'rename') {
				const r = both(m);
				v.ser = r.ser ?? v.ser;
				v.de = r.de ?? v.de;
			} else if (m.key === 'rename_all') fieldRule = both(m);
			else if (m.key === 'alias' && m.value) v.aliases.push(m.value);
			else if (m.key === 'skip') v.skipSer = v.skipDe = true;
			else if (m.key === 'skip_serializing') v.skipSer = true;
			else if (m.key === 'skip_deserializing') v.skipDe = true;
			else if (m.key === 'other') v.other = true;
			else if (m.key === 'untagged') v.untagged = true;
			else if (['with', 'serialize_with', 'deserialize_with'].includes(m.key)) v.custom = true;
		}
		const open = t[i + 1];
		if (open?.k === 'p' && open.v === '{') {
			v.shape = 'struct';
			v.fields = namedFields(t, i + 2, skipGroup(t, i + 1) - 1, fieldRule);
		} else if (open?.k === 'p' && open.v === '(') {
			const types = tupleFields(t, i + 2, skipGroup(t, i + 1) - 1);
			v.shape = types.length === 1 ? 'newtype' : 'tuple';
			if (types.length === 1) v.fields = [plainField('0', types[0], name.line, {})];
		}
		out.push(v);
	}
	return out;
}

/** `struct` / `enum` / `union` at `t[i]`, and the index after it. */
function adt(t: Tok[], i: number, attrs: Attr[], file: string): { type: RustType; next: number } {
	const kind = t[i].v === 'enum' ? 'enum' : 'struct';
	const name = t[i + 1];
	let j = i + 2;
	if (t[j]?.v === '<') j = skipAngle(t, j);
	const derived = attrs
		.filter((a) => a.path === 'derive')
		.flatMap((a) => splitTop(a.args, 0, a.args.length).map(([, y]) => a.args[y - 1]?.v));
	const c = container(attrs);
	const r: RustType = {
		name: name?.v ?? '',
		kind,
		file,
		line: name?.line ?? t[i].line,
		ser: derived.includes('Serialize'),
		de: derived.includes('Deserialize'),
		container: c,
		body: 'unit',
		fields: [],
		variants: [],
		manual: []
	};
	let next: number;
	if (t[j]?.v === '(') {
		const end = skipGroup(t, j);
		r.body = 'tuple';
		next = end;
		while (next < t.length && !(t[next].k === 'p' && (t[next].v === ';' || t[next].v === '{'))) next++;
		if (t[next]?.v === ';') next++;
	} else {
		while (j < t.length && !(t[j].k === 'p' && (t[j].v === '{' || t[j].v === ';'))) {
			j = t[j].k === 'p' && (t[j].v === '(' || t[j].v === '[') ? skipGroup(t, j) : j + 1;
		}
		if (t[j]?.v === '{') {
			next = skipGroup(t, j);
			if (kind === 'enum') r.variants = variants(t, j + 1, next - 1, c);
			else {
				r.body = 'named';
				r.fields = namedFields(t, j + 1, next - 1, c.renameAll);
			}
		} else next = j + 1;
	}
	return { type: r, next };
}

/** One item that is not a type, skipped to its `;` or its `{ ... }`. */
function skipItem(t: Tok[], i: number, end: number): number {
	while (i < end) {
		const { k, v } = t[i];
		if (k === 'p' && v === ';') return i + 1;
		if (k === 'p' && v === '{') return skipGroup(t, i);
		i = k === 'p' && (v === '(' || v === '[') ? skipGroup(t, i) : i + 1;
	}
	return i;
}

/** `impl [<..>] Trait for Type { .. }`: the trait's and the type's last path segment, and the index after it. */
function implHeader(t: Tok[], i: number, end: number): { trait: string; name: string; next: number } {
	let j = i + 1;
	if (t[j]?.v === '<') j = skipAngle(t, j);
	let trait = '';
	let name = '';
	let afterFor = false;
	let angle = 0;
	for (; j < end; j++) {
		const { k, v } = t[j];
		if (k === 'p' && (v === '{' || v === ';')) break;
		if (k === 'p' && v === '<') angle++;
		else if (k === 'p' && v === '>') angle = Math.max(0, angle - 1);
		else if (k === 'p' && (v === '(' || v === '[')) j = skipGroup(t, j) - 1;
		else if (k !== 'id' || angle > 0) continue;
		else if (v === 'where') break;
		else if (v === 'for') afterFor = true;
		else if (v !== 'dyn' && v !== 'mut') {
			if (afterFor) name = v;
			else trait = v;
		}
	}
	return { trait, name: afterFor ? name : '', next: skipItem(t, j, end) };
}

function scanItems(t: Tok[], from: number, to: number, file: string, out: Scan): void {
	let i = from;
	while (i < to) {
		const read = readAttrs(t, i);
		i = read.i;
		if (i >= to) break;
		const test = cfgTest(read.attrs);
		if (t[i].v === 'pub') {
			i++;
			if (t[i]?.v === '(') i = skipGroup(t, i);
		}
		if (t[i]?.v === 'unsafe' && t[i + 1]?.v === 'impl') i++;
		const kw = t[i]?.v;
		if ((kw === 'struct' || kw === 'enum' || kw === 'union') && t[i + 1]?.k === 'id') {
			const r = adt(t, i, read.attrs, file);
			if (!test) out.types.push(r.type);
			i = r.next;
		} else if (kw === 'mod' && t[i + 1]?.k === 'id') {
			const name = t[i + 1].v;
			if (t[i + 2]?.v === '{') {
				const end = skipGroup(t, i + 2);
				if (!test) scanItems(t, i + 3, end - 1, file, out);
				i = end;
			} else {
				const path = read.attrs.find((a) => a.path === 'path')?.args.find((x) => x.k === 'str')?.v;
				out.mods.push({ name, testOnly: test, path });
				i += 3;
			}
		} else if (kw === 'impl') {
			const h = implHeader(t, i, to);
			if (!test && h.name && (h.trait === 'Serialize' || h.trait === 'Deserialize')) {
				out.impls.push({ trait: h.trait, name: h.name });
			}
			i = h.next;
		} else i = skipItem(t, i, to);
	}
}

function scanRust(src: string, file: string): Scan {
	const t = tokenize(src);
	const out: Scan = { types: [], mods: [], impls: [] };
	scanItems(t, 0, t.length, file, out);
	return out;
}

/** Every serde type in `sources` (file path to text), the ones behind `#[cfg(test)]` and the files only a
 *  `#[cfg(test)] mod x;` pulls in left out. A hand-written impl makes a type serde and marks it `manual`. */
function rustTypes(sources: Map<string, string>): RustType[] {
	const scans = new Map<string, Scan>([...sources].map(([file, src]) => [file, scanRust(src, file)]));
	const child = (file: string, m: ModDecl) => {
		const dir = posix.dirname(file);
		const stem = posix.basename(file, '.rs');
		const home = ['mod', 'lib', 'main'].includes(stem) ? dir : posix.join(dir, stem);
		const candidates = m.path ? [posix.join(dir, m.path)] : [posix.join(home, `${m.name}.rs`), posix.join(home, m.name, 'mod.rs')];
		return candidates.find((c) => scans.has(c));
	};
	const skipped = new Set<string>();
	const queue: string[] = [];
	for (const [file, s] of scans) {
		for (const m of s.mods) {
			const c = child(file, m);
			if (m.testOnly && c) queue.push(c);
		}
	}
	for (let f = queue.pop(); f !== undefined; f = queue.pop()) {
		if (skipped.has(f)) continue;
		skipped.add(f);
		for (const m of scans.get(f)?.mods ?? []) {
			const c = child(f, m);
			if (c) queue.push(c);
		}
	}
	const all = [...scans].filter(([file]) => !skipped.has(file));
	const types = all.flatMap(([, s]) => s.types);
	for (const [file, s] of all) {
		for (const impl of s.impls) {
			const named = types.filter((r) => r.name === impl.name);
			const here = named.filter((r) => r.file === file);
			for (const r of here.length ? here : named) {
				r.manual.push(impl.trait);
				if (impl.trait === 'Serialize') r.ser = true;
				else r.de = true;
			}
		}
	}
	return types.filter((r) => r.ser || r.de);
}

// ---- 4. Rust: the shape on the wire -------------------------------------------------------------------

type Dir = 'ser' | 'de';

interface Loc {
	file: string;
	line: number;
}

/** Names on the wire and where the source declares each. */
type Members = Map<string, Loc>;

interface VariantWire {
	loc: Loc;
	/** Present in one direction only (a `skip_serializing` variant, an alias, a `rename` that differs). */
	may: boolean;
	/** An internally tagged variant's own fields. */
	body: Wire;
}

/** What a type is on the wire. In an `object`, `must` is on the wire in every direction the type has (it derives
 *  `Serialize`, `Deserialize` or both) and `may` in one only (`skip_serializing`, an `alias`, a `rename(serialize,
 *  deserialize)` that differs). */
type Wire =
	| { kind: 'object'; must: Members; may: Members }
	| {
			kind: 'variants';
			style: 'strings' | 'internal' | 'adjacent' | 'external';
			tag?: string;
			variants: Map<string, VariantWire>;
	  }
	| { kind: 'unjudged'; reason: string };

const unjudged = (reason: string): Wire => ({ kind: 'unjudged', reason });

/** Types whose keys are anything: flattening one adds names the source does not list. */
const OPEN_MAPS = new Set(['HashMap', 'BTreeMap', 'IndexMap', 'Map', 'Value']);

const WRAPPERS = new Set(['Option', 'Box', 'Arc', 'Rc', 'Cow']);

/** The type a field's type ends up as: `Option<Box<Foo>>` is `Foo`, `crate::a::Foo<T>` is `Foo`. */
function headType(ty: Tok[]): string {
	let t = ty;
	for (;;) {
		let j = 0;
		while (t[j] && (t[j].v === '&' || t[j].k === 'life' || t[j].v === 'mut' || t[j].v === 'dyn')) j++;
		let head = '';
		while (t[j]?.k === 'id') {
			head = t[j].v;
			j++;
			if (t[j]?.v !== '::') break;
			j++;
		}
		if (WRAPPERS.has(head) && t[j]?.v === '<') {
			t = t.slice(j + 1, skipAngle(t, j) - 1);
			continue;
		}
		return head;
	}
}

function judgeTypes(types: RustType[]): (t: RustType) => Wire {
	const index = new Map<string, RustType[]>();
	for (const t of types) index.set(t.name, [...(index.get(t.name) ?? []), t]);
	const cache = new Map<RustType, Wire>();
	const active = new Set<RustType>();

	const dirsOf = (t: RustType): Dir[] => [...(t.ser ? (['ser'] as const) : []), ...(t.de ? (['de'] as const) : [])];

	/** The serde type `head` names, as seen from `from`'s file. */
	const target = (head: string, from: RustType): RustType | string => {
		const all = index.get(head) ?? [];
		const near = all.length === 1 ? all : all.filter((x) => x.file === from.file);
		if (near.length === 1) return near[0];
		return all.length ? `${head} names ${all.length} serde types` : `${head} is not a serde type in this tree`;
	};

	const fieldsWire = (fields: RustField[], dirs: Dir[], owner: RustType): Wire => {
		const must: Members = new Map();
		const may: Members = new Map();
		for (const f of fields) {
			const present = dirs.filter((d) => !(d === 'ser' ? f.skipSer : f.skipDe));
			if (!present.length) continue;
			const loc = { file: owner.file, line: f.line };
			const everywhere = present.length === dirs.length;
			if (f.flatten) {
				const head = headType(f.ty);
				if (f.custom) return unjudged(`flatten of ${head} with a custom (de)serializer`);
				if (OPEN_MAPS.has(head)) return unjudged(`flatten of ${head}, whose keys are open`);
				const to = target(head, owner);
				if (typeof to === 'string') return unjudged(`flatten of ${typeText(f.ty)}: ${to}`);
				const inner = wire(to);
				if (inner.kind !== 'object') {
					return unjudged(`flatten of ${head}: ${inner.kind === 'unjudged' ? inner.reason : 'an enum, whose fields depend on its variant'}`);
				}
				for (const [n, l] of inner.must) (everywhere ? must : may).set(n, l);
				for (const [n, l] of inner.may) may.set(n, l);
				continue;
			}
			const names = new Set(present.map((d) => (d === 'ser' ? f.ser : f.de)));
			for (const n of names) (everywhere && names.size === 1 ? must : may).set(n, loc);
			if (present.includes('de')) for (const a of f.aliases) may.set(a, loc);
		}
		for (const n of must.keys()) may.delete(n);
		return { kind: 'object', must, may };
	};

	const body = (t: RustType, v: RustVariant, dirs: Dir[]): Wire => {
		if (v.shape === 'unit') return { kind: 'object', must: new Map(), may: new Map() };
		if (v.shape === 'struct') return fieldsWire(v.fields, dirs, t);
		if (v.shape === 'tuple') return unjudged('a tuple variant');
		const head = headType(v.fields[0].ty);
		const inner = target(head, t);
		if (typeof inner === 'string') return unjudged(`newtype variant of ${typeText(v.fields[0].ty)}: ${inner}`);
		return wire(inner);
	};

	const enumWire = (t: RustType): Wire => {
		const c = t.container;
		if (c.untagged) return unjudged('#[serde(untagged)]: the wire is whichever variant matches');
		const dirs = dirsOf(t);
		const style =
			c.tag === undefined
				? t.variants.every((v) => v.shape === 'unit')
					? 'strings'
					: 'external'
				: c.content === undefined
					? 'internal'
					: 'adjacent';
		const variants = new Map<string, VariantWire>();
		for (const v of t.variants) {
			if (v.custom) return unjudged(`variant ${v.ident} has a custom (de)serializer`);
			if (v.untagged) return unjudged(`variant ${v.ident} is #[serde(untagged)]`);
			const present = dirs.filter((d) => !(d === 'ser' ? v.skipSer : v.skipDe));
			if (!present.length) continue;
			const loc = { file: t.file, line: v.line };
			const names = new Set(present.map((d) => (d === 'ser' ? v.ser : v.de)));
			const may = present.length < dirs.length || names.size > 1 || v.other;
			const content = style === 'internal' ? body(t, v, dirs) : unjudged('the payload of an externally tagged variant');
			for (const n of names) variants.set(n, { loc, may, body: content });
			if (present.includes('de')) {
				for (const a of v.aliases) if (!variants.has(a)) variants.set(a, { loc, may: true, body: content });
			}
		}
		return { kind: 'variants', style, tag: c.tag, variants };
	};

	const compute = (t: RustType): Wire => {
		const c = t.container;
		if (t.manual.length) return unjudged(`a hand-written impl of ${t.manual.join(' and ')}`);
		if (c.opaque) return unjudged(`#[serde(${c.opaque})]: the wire is another type's`);
		if (c.transparent) return unjudged('#[serde(transparent)]: the wire is the field\'s type');
		if (t.kind === 'enum') return enumWire(t);
		if (t.body !== 'named') return unjudged(t.body === 'tuple' ? 'a tuple or newtype struct' : 'a unit struct');
		if (c.tag !== undefined) return unjudged('#[serde(tag)] on a struct');
		return fieldsWire(t.fields, dirsOf(t), t);
	};

	const wire = (t: RustType): Wire => {
		const known = cache.get(t);
		if (known) return known;
		if (active.has(t)) return unjudged('a struct that flattens itself');
		active.add(t);
		const w = compute(t);
		active.delete(t);
		cache.set(t, w);
		return w;
	};
	return wire;
}

// ---- 5. types.ts --------------------------------------------------------------------------------------

interface TsProp {
	line: number;
	node?: ts.TypeNode;
}

type TsType =
	| { k: 'lit'; v: string }
	| { k: 'null' }
	| { k: 'obj'; props: Map<string, TsProp>; open: boolean }
	| { k: 'union'; of: TsType[] }
	| { k: 'other'; why: string };

interface TsDecl {
	name: string;
	line: number;
	type: TsType;
}

/** Every exported interface and type alias of `source`, resolved as far as names go: `extends`, `&`, `Partial`,
 *  `Pick`, `Omit`, `Record` and other declarations of the same file are followed; anything else is `other`. */
function readTypes(source: string): { decls: Map<string, TsDecl>; literals: (p: TsProp) => string[] | null } {
	const sf = ts.createSourceFile('types.ts', source, ts.ScriptTarget.Latest, true);
	const declared = new Map<string, (ts.InterfaceDeclaration | ts.TypeAliasDeclaration)[]>();
	const exported = new Set<string>();
	for (const s of sf.statements) {
		if (!ts.isInterfaceDeclaration(s) && !ts.isTypeAliasDeclaration(s)) continue;
		declared.set(s.name.text, [...(declared.get(s.name.text) ?? []), s]);
		if (s.modifiers?.some((m) => m.kind === ts.SyntaxKind.ExportKeyword)) exported.add(s.name.text);
	}
	const lineOf = (n: ts.Node) => sf.getLineAndCharacterOfPosition(n.getStart(sf)).line + 1;
	const other = (why: string): TsType => ({ k: 'other', why });

	const members = (ms: readonly ts.TypeElement[]): TsType => {
		const props = new Map<string, TsProp>();
		let open = false;
		for (const m of ms) {
			if (ts.isIndexSignatureDeclaration(m)) open = true;
			else if (ts.isPropertySignature(m) || ts.isMethodSignature(m)) {
				const name = ts.isIdentifier(m.name) || ts.isStringLiteral(m.name) || ts.isNumericLiteral(m.name) ? m.name.text : undefined;
				if (name === undefined) open = true;
				else props.set(name, { line: lineOf(m), node: ts.isPropertySignature(m) ? m.type : undefined });
			}
		}
		return { k: 'obj', props, open };
	};

	const intersect = (parts: TsType[]): TsType => {
		const props = new Map<string, TsProp>();
		let open = false;
		for (const p of parts) {
			if (p.k !== 'obj') return other(`an intersection with ${p.k === 'other' ? p.why : p.k}`);
			for (const [n, v] of p.props) props.set(n, v);
			open ||= p.open;
		}
		return { k: 'obj', props, open };
	};

	const keysOf = (n: ts.TypeNode | undefined, depth: number): Set<string> | null => {
		const t = n ? typeOf(n, depth + 1) : other('no key');
		const parts = t.k === 'union' ? t.of : [t];
		return parts.every((p) => p.k === 'lit') ? new Set(parts.map((p) => (p as { v: string }).v)) : null;
	};

	const named = (name: string, args: readonly ts.TypeNode[], depth: number): TsType => {
		if (depth > 30) return other('nested too deep');
		if (name === 'Partial' || name === 'Required' || name === 'Readonly') return args[0] ? typeOf(args[0], depth + 1) : other(name);
		if (name === 'Record') return { k: 'obj', props: new Map(), open: true };
		if (name === 'Pick' || name === 'Omit') {
			const base = args[0] ? typeOf(args[0], depth + 1) : other(name);
			const keys = keysOf(args[1], depth);
			if (base.k !== 'obj' || !keys) return other(`${name} of something this reader cannot follow`);
			const props = new Map([...base.props].filter(([n]) => keys.has(n) === (name === 'Pick')));
			return { k: 'obj', props, open: base.open };
		}
		const ds = declared.get(name);
		if (!ds) return other(`${name}, which types.ts does not declare`);
		const parts = ds.map((d) => (ts.isInterfaceDeclaration(d) ? iface(d, depth) : typeOf(d.type, depth + 1)));
		return parts.length === 1 ? parts[0] : intersect(parts);
	};

	const iface = (d: ts.InterfaceDeclaration, depth: number): TsType => {
		const bases = (d.heritageClauses ?? [])
			.filter((h) => h.token === ts.SyntaxKind.ExtendsKeyword)
			.flatMap((h) => h.types)
			.map((b) => (ts.isIdentifier(b.expression) ? named(b.expression.text, b.typeArguments ?? [], depth + 1) : other('a qualified base')));
		return bases.length ? intersect([...bases, members(d.members)]) : members(d.members);
	};

	const typeOf = (n: ts.TypeNode, depth: number): TsType => {
		if (ts.isParenthesizedTypeNode(n)) return typeOf(n.type, depth);
		if (ts.isLiteralTypeNode(n)) {
			if (ts.isStringLiteral(n.literal) || ts.isNoSubstitutionTemplateLiteral(n.literal)) return { k: 'lit', v: n.literal.text };
			return n.literal.kind === ts.SyntaxKind.NullKeyword ? { k: 'null' } : other('a non-string literal');
		}
		if (n.kind === ts.SyntaxKind.UndefinedKeyword) return { k: 'null' };
		if (ts.isTypeLiteralNode(n)) return members(n.members);
		if (ts.isTypeReferenceNode(n) && ts.isIdentifier(n.typeName)) return named(n.typeName.text, n.typeArguments ?? [], depth);
		if (ts.isIntersectionTypeNode(n)) return intersect(n.types.map((x) => typeOf(x, depth + 1)));
		if (ts.isUnionTypeNode(n)) return { k: 'union', of: n.types.map((x) => typeOf(x, depth + 1)) };
		return other(ts.SyntaxKind[n.kind]);
	};

	const decls = new Map<string, TsDecl>();
	for (const name of exported) {
		const first = declared.get(name)![0];
		decls.set(name, { name, line: lineOf(first), type: named(name, [], 0) });
	}
	const literals = (p: TsProp): string[] | null => {
		const t = p.node ? typeOf(p.node, 0) : other('no type');
		const parts = t.k === 'union' ? t.of : [t];
		return parts.every((x) => x.k === 'lit') ? parts.map((x) => (x as { v: string }).v) : null;
	};
	return { decls, literals };
}

// ---- 6. Comparing -------------------------------------------------------------------------------------

interface Problem {
	type: string;
	/** `rust-only`: the Rust side names it and `types.ts` does not. `ts-only`: the other way round. */
	side: 'rust-only' | 'ts-only';
	/** A field or a variant as it is on the wire; a tagged variant's field is `variant.field`. */
	member: string;
	/** Where Rust declares the member (`rust-only`), or the type when `types.ts` has the only mention. */
	rust: Loc;
	/** The `types.ts` line (`ts-only`). */
	ts?: number;
}

/** A deliberate mismatch, and why. */
interface Exception {
	type: string;
	side: Problem['side'];
	member: string;
	/** Part of the Rust file's path, when several Rust types share the name and only one is meant. */
	rust?: string;
	why: string;
}

type ObjType = Extract<TsType, { k: 'obj' }>;

const where = (l: Loc) => `${l.file}:${l.line}`;

function diffObject(type: string, owner: Loc, w: { must: Members; may: Members }, o: ObjType, prefix = '', tag?: string): Problem[] {
	const out: Problem[] = [];
	if (!o.open) {
		for (const [n, loc] of w.must) if (!o.props.has(n)) out.push({ type, side: 'rust-only', member: prefix + n, rust: loc });
	}
	for (const [n, p] of o.props) {
		if (n !== tag && !w.must.has(n) && !w.may.has(n)) out.push({ type, side: 'ts-only', member: prefix + n, rust: owner, ts: p.line });
	}
	return out;
}

/** `t` as a flat list of its alternatives, `null` and `undefined` dropped. */
function alternatives(t: TsType): TsType[] {
	if (t.k === 'union') return t.of.flatMap(alternatives);
	return t.k === 'null' ? [] : [t];
}

/** The problems between one judged Rust type and its `types.ts` namesake, or why the two cannot be compared. */
function diff(type: string, owner: Loc, w: Wire, decl: TsDecl, literals: (p: TsProp) => string[] | null): Problem[] | { skip: string } {
	if (w.kind === 'unjudged') return { skip: w.reason };
	const t = decl.type;
	if (w.kind === 'object') {
		return t.k === 'obj' ? diffObject(type, owner, w, t) : { skip: `a Rust struct, and types.ts has ${t.k === 'other' ? t.why : t.k}` };
	}
	const alts = alternatives(t);
	const ts = new Map<string, { line: number; obj?: ObjType }>();
	if (w.style === 'strings') {
		if (!alts.every((a) => a.k === 'lit')) return { skip: 'a Rust unit enum, and types.ts is not a string-literal union' };
		for (const a of alts) ts.set((a as { v: string }).v, { line: decl.line });
	} else if (w.style === 'external') {
		for (const a of alts) {
			if (a.k === 'lit') ts.set(a.v, { line: decl.line });
			else if (a.k === 'obj' && a.props.size === 1 && !a.open) ts.set([...a.props.keys()][0], { line: [...a.props.values()][0].line });
			else return { skip: 'a Rust externally tagged enum, and types.ts has a member that is neither a string nor a one-key object' };
		}
	} else {
		for (const a of alts) {
			const prop = a.k === 'obj' ? a.props.get(w.tag!) : undefined;
			const tags = prop && literals(prop);
			if (a.k !== 'obj' || !prop || !tags) {
				return { skip: `a Rust enum tagged by ${w.tag}, and types.ts has a member without that string-literal property` };
			}
			for (const v of tags) ts.set(v, { line: prop.line, obj: a });
		}
	}
	const out: Problem[] = [];
	for (const [n, v] of w.variants) {
		const has = ts.get(n);
		if (!has && !v.may) out.push({ type, side: 'rust-only', member: n, rust: v.loc });
		if (has?.obj && v.body.kind === 'object' && w.style === 'internal') out.push(...diffObject(type, v.loc, v.body, has.obj, `${n}.`, w.tag));
	}
	for (const [n, v] of ts) if (!w.variants.has(n)) out.push({ type, side: 'ts-only', member: n, rust: owner, ts: v.line });
	return out;
}

interface Report {
	failures: { problem: Problem; message: string }[];
	/** The `types.ts` names compared with at least one Rust type. */
	compared: string[];
	/** The Rust types behind a `types.ts` name that could not be judged, by reason. */
	unjudged: Map<string, string[]>;
	/** A `types.ts` name whose Rust types were judged but the two cannot be compared, and why. */
	skipped: [string, string][];
}

/** A mirror target as `[file part, Rust name]`. */
const mirrorSpec = (spec: string): [string, string] => (spec.includes('#') ? (spec.split('#') as [string, string]) : ['', spec]);

const FIX_RUST_ONLY = 'add it to the interface, or give EXCEPTIONS the reason it is not there';
const FIX_TS_ONLY = 'remove it, or give EXCEPTIONS the reason it is there';

/** `types.ts` against `rust`. A `types.ts` type is compared with every Rust serde type of its name, and with
 *  the ones `mirrors` adds (`Name`, or `path#Name` to pin a file); it passes when it agrees with any of them. */
function compare(
	rust: RustType[],
	tsTypes: ReturnType<typeof readTypes>,
	exceptions: Exception[],
	mirrors: Record<string, string[]> = {}
): Report {
	const wire = judgeTypes(rust);
	const byName = new Map<string, RustType[]>();
	for (const r of rust) byName.set(r.name, [...(byName.get(r.name) ?? []), r]);
	const named = (spec: string) => {
		const [path, name] = mirrorSpec(spec);
		return (byName.get(name) ?? []).filter((r) => r.file.includes(path));
	};
	const excused = (p: Problem, r: RustType) =>
		exceptions.some((e) => e.type === p.type && e.side === p.side && e.member === p.member && (!e.rust || r.file.includes(e.rust)));
	const report: Report = { failures: [], compared: [], unjudged: new Map(), skipped: [] };

	for (const [name, decl] of tsTypes.decls) {
		const judged: { r: RustType; problems: Problem[] }[] = [];
		const skipped: [string, string][] = [];
		for (const r of new Set([name, ...(mirrors[name] ?? [])].flatMap(named))) {
			const w = wire(r);
			if (w.kind === 'unjudged') {
				report.unjudged.set(w.reason, [...(report.unjudged.get(w.reason) ?? []), `${name} (${r.file}:${r.line})`]);
				continue;
			}
			const found = diff(name, { file: r.file, line: r.line }, w, decl, tsTypes.literals);
			if (Array.isArray(found)) judged.push({ r, problems: found });
			else skipped.push([name, found.skip]);
		}
		if (!judged.length) {
			report.skipped.push(...skipped);
			continue;
		}
		report.compared.push(name);
		const open = judged.map(({ r, problems }) => ({ r, rest: problems.filter((p) => !excused(p, r)) }));
		if (open.some((x) => x.rest.length === 0)) continue;
		const best = open.reduce((a, b) => (b.rest.length < a.rest.length ? b : a));
		const among = open.length > 1 ? ` [closest of ${open.map((x) => `${x.r.file}:${x.r.line}`).join(', ')}]` : '';
		for (const p of best.rest) {
			const message =
				p.side === 'rust-only'
					? `${p.type}.${p.member}: Rust has it (${where(p.rust)}), types.ts does not: ${FIX_RUST_ONLY}${among}`
					: `${p.type}.${p.member}: types.ts:${p.ts} has it, Rust ${best.r.name} (${where(p.rust)}) does not: ${FIX_TS_ONLY}${among}`;
			report.failures.push({ problem: p, message });
		}
	}
	return report;
}

// ---- 7. The sources on disk ---------------------------------------------------------------------------

/** Every `.rs` under `dir`, as a repo-relative path with forward slashes. */
function rustFiles(dir: string): string[] {
	return readdirSync(join(REPO, dir)).flatMap((name) => {
		const path = `${dir}/${name}`;
		if (statSync(join(REPO, path)).isDirectory()) return rustFiles(path);
		return name.endsWith('.rs') ? [path] : [];
	});
}

let loaded: { rust: RustType[]; types: ReturnType<typeof readTypes> } | undefined;

/** Read once: it is some 14 MB of Rust. */
function load() {
	loaded ??= {
		rust: rustTypes(new Map(RUST_ROOTS.flatMap(rustFiles).map((f) => [f, readFileSync(join(REPO, f), 'utf8')]))),
		types: readTypes(readFileSync(TYPES_TS, 'utf8'))
	};
	return loaded;
}

// ---- 8. Deliberate differences ------------------------------------------------------------------------

/** A `types.ts` name that also mirrors a Rust type of another name (it is compared with that one too). `path#Name`
 *  pins the Rust file when several share the name. */
const MIRRORS: Record<string, string[]> = {
	// The SVG text animation's types, prefixed with `Animation` in types.ts (`Easing` and `Curves` are already its
	// keyframe easing and its colour curves).
	AnimationEasing: ['svg/text_animation.rs#Easing'],
	AnimationShape: ['svg/text_animation.rs#Shape'],
	AnimationRange: ['RangeSelector'],
	AnimationWiggle: ['Wiggle'],
	AnimationProperties: ['svg/text_animation.rs#Properties'],
	AnimationCurve: ['svg/curve.rs#Curve'],
	AnimationCurves: ['svg/curve.rs#Curves'],
	AnimationCurvePreset: ['CurvePreset'],
	ReviewCheck: ['review.rs#Check'],
	// The scopes' types, prefixed with `Scope` in types.ts.
	ScopeGridSize: ['scopes.rs#GridSize'],
	ScopeChannel: ['ChannelStats'],
	ScopeChroma: ['ChromaStats'],
	ScopePeak: ['VectorPeak'],
	ScopeGrid: ['scopes.rs#Grid'],
	ScopeHistogram: ['scopes.rs#Histogram'],
	// Payloads that carry what the adapter adds to the core's type (`variant` / `total` / `together`, `asset_id`,
	// `data_url`), and `VoiceoverProgressEvent`, which is the payload of four progress events.
	ExportProgress: ['VariantProgress'],
	AnalysisProgress: ['AnalysisProgressEvent'],
	FilmstripSheet: ['FilmstripSheetPayload'],
	CaptionImportResult: ['CaptionImport'],
	CaptionImportSummary: ['ImportSummary'],
	ModelProgress: ['ModelProgressEvent'],
	VoiceoverProgress: ['VoiceoverProgressEvent'],
	MusicProgress: ['VoiceoverProgressEvent'],
	StemsProgress: ['VoiceoverProgressEvent'],
	AudioSyncProgress: ['VoiceoverProgressEvent'],
	// kerf-app's own types, under other names in types.ts.
	SpeechModelInfo: ['ModelInfo'],
	MusicStyleInfo: ['StyleInfo'],
	McpToolSet: ['ToolSetInfo'],
	GpuRefusalCount: ['RefusalCount'],
	PreviewBoundsReport: ['BoundsReport'],
	PlayAhead: ['AheadInfo'],
	LaunchRequest: ['LaunchProject']
};

/** Every mismatch that is on purpose. Keep it short: a field the page never reads is still a field the next
 *  reader of `types.ts` looks for, so add it to the interface first and list it here only when it cannot be there
 *  (or must not be). Each entry is checked to still be needed. */
const EXCEPTIONS: Exception[] = [
	{
		type: 'Levels',
		side: 'ts-only',
		member: 'estimated',
		why: 'the browser harness has no ffmpeg and estimates the levels (`levels.ts`), and says so; the backend never sends it'
	},
	{
		type: 'LaunchRequest',
		side: 'rust-only',
		member: 'nothing',
		why: '`LaunchProject::Nothing` (no `.kerf` argument) is never sent: `take_launch_project` answers null for it'
	}
];

// ---- tests --------------------------------------------------------------------------------------------

/** Reading all of `crates/` takes a second or two, more on a cold CI disk. */
const SLOW = 30_000;

const rustOf = (src: string, others: Record<string, string> = {}) =>
	rustTypes(new Map([['src/lib.rs', src], ...Object.entries(others)]));

function wireOf(src: string, name: string, others: Record<string, string> = {}): Wire {
	const types = rustOf(src, others);
	const t = types.find((x) => x.name === name);
	if (!t) throw new Error(`no serde type ${name} in ${types.map((x) => x.name)}`);
	return judgeTypes(types)(t);
}

function fieldsOf(w: Wire) {
	if (w.kind !== 'object') throw new Error(w.kind === 'unjudged' ? w.reason : w.kind);
	return { must: [...w.must.keys()], may: [...w.may.keys()] };
}

function variantsOf(w: Wire) {
	if (w.kind !== 'variants') throw new Error(w.kind === 'unjudged' ? w.reason : w.kind);
	const variants = [...w.variants];
	return {
		style: w.style,
		tag: w.tag,
		must: variants.filter(([, v]) => !v.may).map(([n]) => n),
		may: variants.filter(([, v]) => v.may).map(([n]) => n),
		fields: Object.fromEntries(variants.map(([n, v]) => [n, v.body.kind === 'object' ? [...v.body.must.keys()] : v.body.kind]))
	};
}

const reason = (w: Wire) => (w.kind === 'unjudged' ? w.reason : `judged (${w.kind})`);

describe('the Rust reader', () => {
	test('a brace, an attribute or a type inside a comment, a string or a char is not read', () => {
		const src = [
			'/// Closes early? } and opens { #[serde(skip)] pub ghost: u8,',
			'#[derive(Serialize)]',
			'pub struct A {',
			'    /** a block { with braces } */',
			'    pub a: u8, // trailing }',
			'    #[serde(rename = "}{")]',
			"    pub b: &'static str,",
			"    pub c: char, // '}'",
			'    pub d: [u8; 3],',
			'}',
			"const OPEN: char = '{'; const QUOTE: char = '\\''; const SLASH: char = '\\\\';",
			'const RAW: &str = r#"} struct Ghost { x: u8 } "#; const BYTES: &[u8] = b"{";',
			'#[derive(Serialize)] struct B { z: u8 }'
		].join('\n');
		const types = rustOf(src);
		expect(types.map((t) => t.name)).toEqual(['A', 'B']);
		expect(fieldsOf(wireOf(src, 'A'))).toEqual({ must: ['a', '}{', 'c', 'd'], may: [] });
		expect(types[0].fields.map((f) => f.line)).toEqual([5, 7, 8, 9]);
		expect(types[1].line).toBe(13);
	});

	test('nested generics, a where clause and a closure type do not end a field early', () => {
		const src = `
			#[derive(Serialize)]
			pub struct W<T: Into<Vec<u8>>, const N: usize> where T: Clone + Fn(u8) -> u8 {
				pub a: std::collections::HashMap<String, Vec<(f64, Option<Box<dyn Fn(u8) -> u8>>)>>,
				pub b: [T; N],
				pub(crate) c: Vec<Vec<Vec<u8>>>,
				d: for<'a> fn(&'a str) -> Result<(), ()>,
			}
			#[derive(Serialize)] struct After { z: u8 }`;
		expect(fieldsOf(wireOf(src, 'W'))).toEqual({ must: ['a', 'b', 'c', 'd'], may: [] });
		expect(fieldsOf(wireOf(src, 'After'))).toEqual({ must: ['z'], may: [] });
	});

	test('rename_all and rename name fields and variants the way serde does', () => {
		const src = `
			#[derive(Serialize)] #[serde(rename_all = "camelCase")]
			struct Cam { first_name: u8, #[serde(rename = "ID")] user_id: u8, r#type: u8 }
			#[derive(Serialize)] #[serde(rename_all = "SCREAMING-KEBAB-CASE")]
			struct Scream { first_name: u8 }
			#[derive(Serialize, Deserialize)] #[serde(rename_all(serialize = "PascalCase", deserialize = "snake_case"))]
			struct Split { first_name: u8 }
			#[derive(Serialize, Deserialize)] #[serde(rename_all = "kebab-case")]
			enum Kebab { FirstOne, Second, #[serde(rename = "x")] Third, #[serde(alias = "old")] Fourth }
			#[derive(Serialize)] #[serde(rename_all = "snake_case")]
			enum Plain { HDRPlus, Two }`;
		expect(fieldsOf(wireOf(src, 'Cam'))).toEqual({ must: ['firstName', 'ID', 'type'], may: [] });
		expect(fieldsOf(wireOf(src, 'Scream'))).toEqual({ must: ['FIRST-NAME'], may: [] });
		expect(fieldsOf(wireOf(src, 'Split'))).toEqual({ must: [], may: ['FirstName', 'first_name'] });
		expect(variantsOf(wireOf(src, 'Kebab'))).toMatchObject({
			style: 'strings',
			must: ['first-one', 'second', 'x', 'fourth'],
			may: ['old']
		});
		expect(variantsOf(wireOf(src, 'Plain')).must).toEqual(['h_d_r_plus', 'two']);
	});

	test('every rename_all rule matches serde on a field and on a variant', () => {
		const rules = [
			'lowercase',
			'UPPERCASE',
			'PascalCase',
			'camelCase',
			'snake_case',
			'SCREAMING_SNAKE_CASE',
			'kebab-case',
			'SCREAMING-KEBAB-CASE'
		];
		expect(rules.map((r) => fieldName('two_words', r))).toEqual([
			'two_words',
			'TWO_WORDS',
			'TwoWords',
			'twoWords',
			'two_words',
			'TWO_WORDS',
			'two-words',
			'TWO-WORDS'
		]);
		expect(rules.map((r) => variantName('TwoWords', r))).toEqual([
			'twowords',
			'TWOWORDS',
			'TwoWords',
			'twoWords',
			'two_words',
			'TWO_WORDS',
			'two-words',
			'TWO-WORDS'
		]);
	});

	test('skip leaves a field out, skip_serializing and skip_deserializing out of one direction', () => {
		const src = `
			#[derive(Serialize, Deserialize)]
			struct Both {
				a: u8,
				#[serde(skip)] b: u8,
				#[serde(skip_serializing)] c: u8,
				#[serde(skip_deserializing)] d: u8,
				#[serde(default, skip_serializing_if = "Option::is_none")] e: Option<u8>,
				#[serde(alias = "old", alias = "older")] f: u8,
				#[serde(serialize_with = "s", deserialize_with = "d")] g: u8,
			}
			#[derive(Serialize)]
			struct Out { a: u8, #[serde(skip_deserializing)] d: u8, #[serde(skip_serializing)] c: u8 }
			#[derive(Deserialize)]
			struct In { a: u8, #[serde(skip_deserializing)] d: u8, #[serde(skip_serializing)] c: u8 }`;
		expect(fieldsOf(wireOf(src, 'Both'))).toEqual({ must: ['a', 'e', 'f', 'g'], may: ['c', 'd', 'old', 'older'] });
		expect(fieldsOf(wireOf(src, 'Out'))).toEqual({ must: ['a', 'd'], may: [] });
		expect(fieldsOf(wireOf(src, 'In'))).toEqual({ must: ['a', 'c'], may: [] });
	});

	test('flatten brings in the fields of the type it names, wherever that is', () => {
		const src = `
			#[derive(Serialize, Deserialize)]
			struct Outer {
				id: u8,
				#[serde(flatten)] inner: Inner,
				#[serde(flatten)] maybe: Option<Box<other::Other>>,
			}
			#[derive(Serialize, Deserialize)]
			struct Inner { x: u8, #[serde(skip_serializing)] hidden: u8 }`;
		const others = { 'src/other.rs': '#[derive(Serialize, Deserialize)] pub struct Other { y: u8 }' };
		expect(fieldsOf(wireOf(src, 'Outer', others))).toEqual({ must: ['id', 'x', 'y'], may: ['hidden'] });
	});

	test('a flatten whose keys the source does not list leaves the type unjudged', () => {
		const flatten = (ty: string) =>
			`#[derive(Serialize)] struct S<T> { a: u8, #[serde(flatten)] rest: ${ty} } #[derive(Serialize)] enum E { A { x: u8 } }`;
		expect(reason(wireOf(flatten('HashMap<String, Value>'), 'S'))).toContain('open');
		expect(reason(wireOf(flatten('serde_json::Map<String, serde_json::Value>'), 'S'))).toContain('open');
		expect(reason(wireOf(flatten('E'), 'S'))).toContain('an enum');
		expect(reason(wireOf(flatten('T'), 'S'))).toContain('not a serde type');
		const twice = `#[derive(Serialize)] struct S { #[serde(flatten)] i: Inner }`;
		const inner = (n: string) => `#[derive(Serialize)] pub struct Inner { ${n}: u8 }`;
		expect(reason(wireOf(twice, 'S', { 'src/a.rs': inner('a'), 'src/b.rs': inner('b') }))).toContain('names 2 serde types');
		expect(fieldsOf(wireOf(twice + inner('mine'), 'S', { 'src/b.rs': inner('b') }))).toEqual({ must: ['mine'], may: [] });
	});

	test('a tagged enum is read by its tag, its variants and their fields', () => {
		const src = `
			#[derive(Serialize, Deserialize)] #[serde(tag = "type", rename_all = "snake_case")]
			enum Shape {
				Circle { radius: f64 },
				Rect { w: f64, #[serde(default)] h: f64 },
				#[serde(rename_all = "camelCase")] Pill { corner_radius: f64 },
				Named(Label),
				Empty,
				#[serde(skip_deserializing)] Internal,
			}
			#[derive(Serialize, Deserialize)] struct Label { text: String }
			#[derive(Serialize)] #[serde(tag = "t", content = "c")]
			enum Adjacent { One(u8), Two }
			#[derive(Serialize)]
			enum External { A, B(u8), C { x: u8 } }
			#[derive(Serialize)] #[serde(untagged)]
			enum Loose { A(u8), B(String) }
			#[derive(Serialize)] #[serde(tag = "k", rename_all_fields = "camelCase")]
			enum Fields { A { some_field: u8 } }`;
		expect(variantsOf(wireOf(src, 'Shape'))).toEqual({
			style: 'internal',
			tag: 'type',
			must: ['circle', 'rect', 'pill', 'named', 'empty'],
			may: ['internal'],
			fields: { circle: ['radius'], rect: ['w', 'h'], pill: ['cornerRadius'], named: ['text'], empty: [], internal: [] }
		});
		expect(variantsOf(wireOf(src, 'Adjacent'))).toMatchObject({ style: 'adjacent', tag: 't', must: ['One', 'Two'] });
		expect(variantsOf(wireOf(src, 'External'))).toMatchObject({ style: 'external', must: ['A', 'B', 'C'] });
		expect(reason(wireOf(src, 'Loose'))).toContain('untagged');
		expect(variantsOf(wireOf(src, 'Fields')).fields).toEqual({ A: ['someField'] });
	});

	test('what is behind #[cfg(test)], in a file only a test module pulls in, is not a type of the app', () => {
		const src = `
			#[derive(Serialize)] struct Kept { a: u8 }
			#[cfg(test)] #[derive(Serialize)] struct OnlyInTests { a: u8 }
			#[cfg(not(test))] #[derive(Serialize)] struct NotTests { a: u8 }
			#[cfg(all(test, unix))] #[derive(Serialize)] struct TestAndUnix { a: u8 }
			#[cfg(any(test, windows))] #[derive(Serialize)] struct Maybe { a: u8 }
			#[cfg(test)] mod tests { #[derive(Serialize)] struct InMod { a: u8 } }
			mod inline { #[derive(Serialize)] struct Nested { a: u8 } }
			#[cfg(test)] mod golden;
			mod real;
			#[cfg(test)] #[path = "elsewhere.rs"] mod moved;`;
		const others = {
			'src/golden.rs': '#[derive(Serialize)] struct Golden { a: u8 } mod more;',
			'src/golden/more.rs': '#[derive(Serialize)] struct More { a: u8 }',
			'src/real.rs': '#[derive(Serialize)] struct Real { a: u8 }',
			'src/elsewhere.rs': '#[derive(Serialize)] struct Elsewhere { a: u8 }'
		};
		expect(rustOf(src, others).map((t) => t.name)).toEqual(['Kept', 'NotTests', 'Maybe', 'Nested', 'Real']);
	});

	test('a hand-written impl makes a type serde and leaves it unjudged', () => {
		const src = `
			#[derive(Serialize)] struct A { a: u8 }
			impl<'de> serde::Deserialize<'de> for A { fn deserialize<D>(d: D) -> Result<Self, D::Error> { todo!() } }
			struct B { b: u8 }
			impl Serialize for B { fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> { todo!() } }
			struct C { c: u8 }
			impl Serializer for C {}
			impl<T: Clone> Serialize for crate::x::D<T> where T: Foo {}
			#[derive(Serialize)] enum D<T> { X(T) }`;
		expect(rustOf(src).map((t) => t.name)).toEqual(['A', 'B', 'D']);
		expect(reason(wireOf(src, 'A'))).toBe('a hand-written impl of Deserialize');
		expect(reason(wireOf(src, 'B'))).toBe('a hand-written impl of Serialize');
		expect(reason(wireOf(src, 'D'))).toBe('a hand-written impl of Serialize');
	});

	test('a type whose wire shape the source does not decide is unjudged, not guessed', () => {
		const src = `
			#[derive(Serialize)] #[serde(transparent)] struct Id(u64);
			#[derive(Serialize)] struct Pair(u8, u8);
			#[derive(Serialize)] struct Unit;
			#[derive(Serialize)] #[serde(into = "Other")] struct Wrapped { a: u8 }
			#[derive(Serialize)] enum Odd { #[serde(serialize_with = "f")] A, B }
			#[derive(Serialize)] struct Custom { #[serde(flatten, with = "m")] rest: Inner }
			#[derive(Serialize)] struct Inner { a: u8 }`;
		for (const name of ['Id', 'Pair', 'Unit', 'Wrapped', 'Odd', 'Custom']) {
			expect(wireOf(src, name).kind, name).toBe('unjudged');
		}
	});

	test('derive paths and attributes in any spelling count', () => {
		const src = `
			#[derive(Debug, serde::Serialize)] #[derive(serde::Deserialize)] struct A { a: u8 }
			#[cfg_attr(feature = "x", derive(Serialize))] struct B { b: u8 }
			#[derive(Debug, Clone)] struct NotSerde { c: u8 }
			#[derive(Serialize)] pub(crate) struct C<'a> { c: &'a str }
			#[derive(Serialize)] pub enum D where Self: Sized { X }`;
		expect(rustOf(src).map((t) => [t.name, t.ser, t.de])).toEqual([
			['A', true, true],
			['B', true, false],
			['C', true, false],
			['D', true, false]
		]);
	});
});

const TS_SOURCE = `
	export interface Base { id: string; 'quoted-name'?: number; }
	export interface Child extends Base { extra: boolean }
	interface Hidden { h: number }
	export interface UsesHidden extends Hidden { u: number }
	export type Kind = 'a' | 'b' | null;
	export type Both = { x: number } & Child;
	export type Slim = Pick<Child, 'id' | 'extra'>;
	export type Part = Partial<Omit<Child, 'extra'>>;
	export type Ev = { type: 'one'; n: number } | { type: 'two' | 'three' };
	export type Open = Record<string, number>;
	export interface Idx { [k: string]: number; a: number }
	export interface Gen<T> { items: T[] }
	export interface Ext extends Missing { z: number }
	export interface Merged { a: number }
	export interface Merged { b: number }
	export type Tuple = [number, number];
`;

describe('the types.ts reader', () => {
	const { decls, literals } = readTypes(TS_SOURCE);
	const props = (name: string) => {
		const t = decls.get(name)?.type;
		return t?.k === 'obj' ? [...t.props.keys()] : t?.k;
	};

	test('it reads interfaces, extends, intersections and the utility types by name', () => {
		expect(props('Base')).toEqual(['id', 'quoted-name']);
		expect(props('Child')).toEqual(['id', 'quoted-name', 'extra']);
		expect(props('UsesHidden')).toEqual(['h', 'u']);
		expect(props('Both')).toEqual(['x', 'id', 'quoted-name', 'extra']);
		expect(props('Slim')).toEqual(['id', 'extra']);
		expect(props('Part')).toEqual(['id', 'quoted-name']);
		expect(props('Gen')).toEqual(['items']);
		expect(props('Merged')).toEqual(['a', 'b']);
	});

	test('only what is exported is a type of the file, and what it cannot follow says so', () => {
		expect(decls.has('Hidden')).toBe(false);
		expect(props('Ext')).toBe('other');
		expect(props('Tuple')).toBe('other');
		const open = decls.get('Open')?.type;
		expect(open?.k === 'obj' && open.open).toBe(true);
		const idx = decls.get('Idx')?.type;
		expect(idx?.k === 'obj' && idx.open).toBe(true);
	});

	test('it reads a union of literals and the literals of a tag', () => {
		const kind = decls.get('Kind')?.type;
		expect(kind?.k === 'union' && kind.of.map((t) => (t.k === 'lit' ? t.v : t.k))).toEqual(['a', 'b', 'null']);
		const ev = decls.get('Ev')?.type;
		const tags = ev?.k === 'union' ? ev.of.map((m) => (m.k === 'obj' ? literals(m.props.get('type')!) : null)) : [];
		expect(tags).toEqual([['one'], ['two', 'three']]);
	});
});

/** `rust` read as one file against `tsSource`. */
function check(rust: string, tsSource: string, exceptions: Exception[] = [], mirrors: Record<string, string[]> = {}, others = {}) {
	return compare(rustOf(rust, others), readTypes(tsSource), exceptions, mirrors);
}

const messages = (r: Report) => r.failures.map((f) => f.message);

describe('the comparison', () => {
	const RUST = `
		#[derive(Serialize, Deserialize)]
		pub struct Clip {
			pub id: String,
			#[serde(default, skip_serializing_if = "Option::is_none")]
			pub link: Option<String>,
			#[serde(skip)] pub cache: u8,
		}`;

	test('a field Rust has and types.ts lacks, and one types.ts has and Rust lacks, are named with their lines', () => {
		const r = check(RUST, 'export interface Clip {\n\tid: string;\n\tstale: number;\n}');
		expect(messages(r)).toEqual([
			expect.stringContaining('Clip.link: Rust has it (src/lib.rs:6), types.ts does not'),
			expect.stringContaining('Clip.stale: types.ts:3 has it, Rust Clip (src/lib.rs:3) does not')
		]);
		expect(r.failures.map((f) => f.problem.side)).toEqual(['rust-only', 'ts-only']);
	});

	test('a skipped field is not expected, and an optional one is still expected', () => {
		expect(messages(check(RUST, 'export interface Clip { id: string; link?: string | null }'))).toEqual([]);
		expect(messages(check(RUST, 'export interface Clip { id: string; link?: string; cache: number }'))).toEqual([
			expect.stringContaining('Clip.cache')
		]);
	});

	test('a unit enum is compared by its variants, in either direction', () => {
		const rust = '#[derive(Serialize)] #[serde(rename_all = "snake_case")] enum Kind { One, TwoWords, Three }';
		expect(messages(check(rust, "export type Kind = 'one' | 'two_words' | 'three';"))).toEqual([]);
		expect(messages(check(rust, "export type Kind = 'one' | 'two_words' | 'four' | null;"))).toEqual([
			expect.stringContaining('Kind.three: Rust has it'),
			expect.stringContaining('Kind.four: types.ts:1 has it')
		]);
	});

	test('a tagged enum is compared by its tags and by the fields of each variant', () => {
		const rust = '#[derive(Serialize)] #[serde(tag = "kind", rename_all = "snake_case")] enum Fx { Blur { radius: f64 }, Off }';
		const ok = "export type Fx = { kind: 'blur'; radius: number } | { kind: 'off' };";
		expect(messages(check(rust, ok))).toEqual([]);
		const bad = "export type Fx = { kind: 'blur'; radius: number; extra: number } | { kind: 'on' };";
		expect(messages(check(rust, bad))).toEqual([
			expect.stringContaining('Fx.blur.extra: types.ts:1 has it'),
			expect.stringContaining('Fx.off: Rust has it'),
			expect.stringContaining('Fx.on: types.ts:1 has it')
		]);
		expect(check(rust, 'export type Fx = { radius: number };').skipped).toEqual([
			['Fx', expect.stringContaining('a Rust enum tagged by kind')]
		]);
	});

	test('an externally tagged enum is compared by its keys', () => {
		const rust = '#[derive(Serialize)] #[serde(rename_all = "snake_case")] enum Ease { Linear, Bezier { x: f64 } }';
		expect(messages(check(rust, "export type Ease = 'linear' | { bezier: { x: number } };"))).toEqual([]);
		expect(messages(check(rust, "export type Ease = 'linear';"))).toEqual([expect.stringContaining('Ease.bezier: Rust has it')]);
	});

	test('names on one side only, and types the source cannot judge, are not failures', () => {
		const rust = `${RUST} #[derive(Serialize)] struct RustOnly { a: u8 } #[derive(Serialize)] #[serde(transparent)] struct Id(String);
			#[derive(Serialize)] struct Odd { #[serde(flatten)] rest: HashMap<String, u8> }`;
		const r = check(
			rust,
			`export interface Clip { id: string; link?: string }
			export interface UiOnly { z: number }
			export interface Id { x: number }
			export interface Odd { q: number }`
		);
		expect(messages(r)).toEqual([]);
		expect(r.compared).toEqual(['Clip']);
		expect([...r.unjudged.keys()]).toEqual([expect.stringContaining('transparent'), expect.stringContaining('open')]);
	});

	test('two Rust types of one name: the types.ts type passes when it agrees with either', () => {
		const a = '#[derive(Serialize)] pub struct Dup { a: u8 }';
		const b = '#[derive(Serialize)] pub struct Dup { b: u8, c: u8 }';
		expect(messages(check(a, 'export interface Dup { b: number; c: number }', [], {}, { 'src/b.rs': b }))).toEqual([]);
		expect(messages(check(a, 'export interface Dup { a: number }', [], {}, { 'src/b.rs': b }))).toEqual([]);
		const none = messages(check(a, 'export interface Dup { a: number; z: number }', [], {}, { 'src/b.rs': b }));
		expect(none).toEqual([expect.stringContaining('Dup.z: types.ts:1 has it')]);
		expect(none[0]).toContain('closest of src/lib.rs:1, src/b.rs:1');
	});

	test('an exception excuses one member, and can be pinned to one of two Rust types', () => {
		const exception: Exception = { type: 'Clip', side: 'ts-only', member: 'stale', why: 'test' };
		const tsSource = 'export interface Clip { id: string; link?: string; stale: number }';
		expect(messages(check(RUST, tsSource, [exception]))).toEqual([]);
		expect(messages(check(RUST, tsSource, [{ ...exception, member: 'other' }]))).toHaveLength(1);
		expect(messages(check(RUST, tsSource, [{ ...exception, side: 'rust-only' }]))).toHaveLength(1);
		const other = { 'src/other.rs': '#[derive(Serialize)] pub struct Clip { zzz: u8 }' };
		expect(messages(check(RUST, tsSource, [{ ...exception, rust: 'src/lib.rs' }], {}, other))).toEqual([]);
		expect(messages(check(RUST, tsSource, [{ ...exception, rust: 'src/other.rs' }], {}, other))).toHaveLength(1);
	});

	test('a mirror compares a types.ts name with a Rust type of another name', () => {
		const rust = '#[derive(Serialize)] struct ClipEvent { id: String, at: f64 }';
		const tsSource = 'export interface ClipProgress { id: string }';
		expect(messages(check(rust, tsSource))).toEqual([]);
		expect(messages(check(rust, tsSource, [], { ClipProgress: ['ClipEvent'] }))).toEqual([
			expect.stringContaining('ClipProgress.at: Rust has it')
		]);
		expect(messages(check(rust, tsSource, [], { ClipProgress: ['src/gone.rs#ClipEvent'] }))).toEqual([]);
	});
});

describe('types.ts against the serde types of kerf-core and kerf-app', () => {
	test('the readers find the types they should, and none that only a test has', () => {
		const { rust, types } = load();
		expect(rust.length).toBeGreaterThan(400);
		expect(types.decls.size).toBeGreaterThan(250);
		expect(rust.filter((r) => /(_tests?|_rendered|_checker|golden|corpus|test_support)\.rs$/.test(r.file))).toEqual([]);
		expect(new Set(rust.map((r) => r.file.split('/')[1])).size).toBe(RUST_ROOTS.length);
		const projection = variantsOf(judgeTypes(rust)(rust.find((r) => r.name === 'Projection')!));
		expect(projection.style).toBe('strings');
		expect(projection.must).toContain('dual_fisheye');
	}, SLOW);

	test('a real Rust type is held to what types.ts says of it', () => {
		const { rust } = load();
		const tsSource = "export interface Tag { value: string; bogus: number } export type Projection = 'equirect' | 'flat' | 'cube';";
		expect(messages(compare(rust, readTypes(tsSource), [], {}))).toEqual([
			expect.stringContaining('Tag.key: Rust has it (crates/kerf-core/src/'),
			expect.stringContaining('Tag.bogus: types.ts:1 has it'),
			expect.stringContaining('Projection.dual_fisheye: Rust has it'),
			expect.stringContaining('Projection.fisheye: Rust has it'),
			expect.stringContaining('Projection.cube: types.ts:1 has it')
		]);
	}, SLOW);

	test('every type of types.ts with a serde counterpart has the field and variant names Rust writes', () => {
		const { rust, types } = load();
		const report = compare(rust, types, EXCEPTIONS, MIRRORS);
		expect(report.compared.length).toBeGreaterThan(250);
		expect(messages(report)).toEqual([]);
	}, SLOW);

	test('every exception and every mirror is still needed', () => {
		const { rust, types } = load();
		for (const e of EXCEPTIONS) {
			expect(e.why.length, `${e.type}.${e.member} needs a reason`).toBeGreaterThan(10);
			const without = compare(rust, types, EXCEPTIONS.filter((x) => x !== e), MIRRORS);
			const needed = without.failures.some((f) => f.problem.type === e.type && f.problem.side === e.side && f.problem.member === e.member);
			expect(needed, `${e.type}.${e.member} (${e.side}) no longer differs: drop it from EXCEPTIONS`).toBe(true);
		}
		const wire = judgeTypes(rust);
		for (const [name, targets] of Object.entries(MIRRORS)) {
			expect(types.decls.has(name), `${name} is no longer in types.ts: drop it from MIRRORS`).toBe(true);
			for (const spec of targets) {
				const [path, rustName] = mirrorSpec(spec);
				const found = rust.filter((r) => r.name === rustName && r.file.includes(path));
				expect(found.length, `${name} mirrors ${spec}, which is no longer a serde type`).toBeGreaterThan(0);
				expect(found.some((r) => wire(r).kind !== 'unjudged'), `${spec} cannot be judged: ${name} is not checked by it`).toBe(true);
			}
		}
	}, SLOW);
});
