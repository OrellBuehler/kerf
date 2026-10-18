// What the wire refuses before the engine looks at a request: the score and request structs of kerf-core carry
// `#[serde(deny_unknown_fields)]`, so a misspelled key (`bmp`, `replace_clip`) is an error that names it and lists
// the ones that exist, rather than a request that quietly does something else. The browser harness has no serde,
// so this is it, in serde's wording, in the order serde meets the keys (the input's own), nested structs included.
// Pure, bun-tested (`music-wire.test.ts`); the desktop app's refusals come from the backend.

/** The fields of each struct that denies unknown ones, in declaration order (`model/score.rs`, `project/music.rs`,
 *  `engine/bands.rs`). */
const FIELDS = {
	request: ['score', 'track_id', 'at', 'align', 'replace_clip_id'],
	score: ['bpm', 'key', 'style', 'sections', 'hits', 'seed', 'instruments'],
	section: ['kind', 'bars', 'chords', 'energy'],
	hit: ['at_s', 'kind'],
	instruments: ['drums', 'bass', 'pad', 'keys', 'lead'],
	align: ['to', 'track_id', 'max_tempo_change'],
	band: ['asset_id', 'clip_id', 'range', 'sections'],
	window: ['start', 'end', 'label']
} as const;

type Struct = keyof typeof FIELDS;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/** serde's `expected …` clause: ``expected `a` ``, ``expected `a` or `b` ``, ``expected one of `a`, `b`, `c` ``. */
function expected(fields: readonly string[]): string {
	const q = fields.map((f) => `\`${f}\``);
	if (q.length === 1) return `expected ${q[0]}`;
	if (q.length === 2) return `expected ${q[0]} or ${q[1]}`;
	return `expected one of ${q.join(', ')}`;
}

/** The nested structs a field holds, by struct and field. */
const NESTED: Partial<Record<Struct, Record<string, { to: Struct; list?: boolean }>>> = {
	request: { score: { to: 'score' }, align: { to: 'align' } },
	score: { sections: { to: 'section', list: true }, hits: { to: 'hit', list: true }, instruments: { to: 'instruments' } },
	band: { sections: { to: 'window', list: true } }
};

/** The first unknown key of `value` read as `struct`, or of anything nested in it, as serde words it; null when there is none. */
function unknownIn(value: unknown, struct: Struct): string | null {
	if (!isObject(value)) return null;
	const fields: readonly string[] = FIELDS[struct];
	for (const [key, inner] of Object.entries(value)) {
		if (!fields.includes(key)) return `unknown field \`${key}\`, ${expected(fields)}`;
		const nested = NESTED[struct]?.[key];
		if (!nested) continue;
		const items = nested.list ? (Array.isArray(inner) ? inner : []) : [inner];
		for (const item of items) {
			const found = unknownIn(item, nested.to);
			if (found) return found;
		}
	}
	return null;
}

/** How Tauri words a command argument that does not deserialize. */
const badArgs = (command: string, why: string) => `invalid args \`request\` for command \`${command}\`: ${why}`;

/** Throw what `compose_music` answers to a request with a key the structs do not have. */
export function checkComposeRequest(request: unknown): void {
	const why = unknownIn(request, 'request');
	if (why) throw new Error(badArgs('compose_music', why));
}

/** The same for `get_band_balance`. */
export function checkBandRequest(request: unknown): void {
	const why = unknownIn(request, 'band');
	if (why) throw new Error(badArgs('get_band_balance', why));
}
