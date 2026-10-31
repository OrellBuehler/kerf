// Sound effects for the browser harness: the catalogue, the spec rules, where an effect lands on the timeline and where
// a cut would take one. The desktop app asks the backend; this is the faithful TS mirror of `model/sfx.rs` (the table and
// the anchor), `project/sfx.rs` (the `FX` lanes, `span_for`, `put_sound_effects`, `replace_effect`) and the wire's
// refusals, plus a lossy one of `model/sfx_suggest.rs` — it reads the same sources (cuts, transitions, titles, punch-ins,
// composed and analysed music, markers, the opening), the same density and the same thinning, but not the scene changes
// inside a clip or the cover of a higher track. The harness synthesizes nothing: the audio of an effect is a
// stand-in asset of the right length. Pure and bun-tested (`sfx.test.ts`, which also pins the table to the Rust one).

import { formatTime } from './diff';
import { toFixedEven } from './format-fixed';
import { barSeconds, sectionSpans } from './music-score';
import { checkOwnLanes, rippleLanes } from './ripple';
import { clipDuration, sourceToTimeline } from './speed-ramp';
import { titleName, titleOf } from './title-clips';
import type {
	Asset,
	AssetAnalysis,
	Clip,
	SfxAnchor,
	SfxCategory,
	SfxDensity,
	SfxKind,
	SfxPlacement,
	SfxPreset,
	SfxSource,
	SfxSpec,
	SfxSpecInput,
	SfxSuggestRequest,
	SfxSuggestion,
	Timeline,
	Track
} from './types';

/** The audio track an effect goes on unless told another (`SFX_TRACK`); more lanes are `FX 2`, `FX 3`, …. */
export const SFX_TRACK = 'FX';
/** The most effects one call places (`MAX_SFX_PER_CALL`). */
export const MAX_SFX_PER_CALL = 256;
/** The quietest and the loudest a placement's gain may be, dB. */
export const MIN_SFX_GAIN_DB = -60;
export const MAX_SFX_GAIN_DB = 12;
/** The fade a head cut off at the start of the timeline gets, seconds. */
export const TRIM_FADE_S = 0.004;
/** The least of an effect that may be left on the timeline when its start is cut off at 0, seconds. */
export const MIN_SFX_CLIP_S = 0.02;
/** How far before 0 an effect's start may fall and still be put *at* 0, its hit landing that late, instead of having its
 *  head cut off, seconds (`SFX_HEAD_SNAP_S`). */
export const SFX_HEAD_SNAP_S = 0.01;
/** The most a pitch may move an effect, semitones either way. */
export const MAX_SFX_PITCH = 12;
/** The largest seed (2^53 - 1): the largest integer JSON in JavaScript keeps exactly. */
export const MAX_SEED = 9007199254740991;
export const DEFAULT_SFX_INTENSITY = 0.7;
export const DEFAULT_SFX_TAIL = 0.3;

/** Every effect Kerf can make, one row per kind in the order of `SfxKind::ALL` — `CATALOGUE` in
 *  `crates/kerf-core/src/model/sfx.rs`, copied. `sfx.test.ts` parses the Rust table and fails when they differ. */
export const SFX_CATALOGUE: readonly SfxPreset[] = [
	{
		kind: 'whoosh',
		label: 'Whoosh',
		category: 'transition',
		description:
			'Wind-like pass of filtered noise that swells to a peak and falls away while it travels from left to right. The classic sound of a cut, a fast pan or an object flying past; put its peak on the cut.',
		default_duration_s: 1,
		min_duration_s: 0.3,
		max_duration_s: 3,
		anchor: { type: 'peak', fraction: 0.55 },
		level_db: -3
	},
	{
		kind: 'swoosh',
		label: 'Swoosh',
		category: 'transition',
		description:
			'A lighter, airier, quicker whoosh with a rising whistle in it, like a sword or a hand cutting the air. Good on a snappy cut or a title sliding in.',
		default_duration_s: 0.7,
		min_duration_s: 0.2,
		max_duration_s: 2,
		anchor: { type: 'peak', fraction: 0.45 },
		level_db: -3
	},
	{
		kind: 'swipe',
		label: 'Swipe',
		category: 'transition',
		description:
			'A short, bright swish for an on-screen swipe: a lower third sliding in, a menu, a page turning.',
		default_duration_s: 0.35,
		min_duration_s: 0.12,
		max_duration_s: 1,
		anchor: { type: 'peak', fraction: 0.4 },
		level_db: -2.5
	},
	{
		kind: 'impact',
		label: 'Impact',
		category: 'impact',
		description:
			'A cinematic hit: a deep sub drop with a noise burst and a room around it. Lands on a big reveal, a title card, a drop in the music.',
		default_duration_s: 1.6,
		min_duration_s: 0.5,
		max_duration_s: 4,
		anchor: { type: 'hit', seconds: 0.004 },
		level_db: 0
	},
	{
		kind: 'boom',
		label: 'Boom',
		category: 'impact',
		description:
			'A long, low explosion-style boom: a sub that falls in pitch and rumbles for seconds. For the biggest moments.',
		default_duration_s: 3,
		min_duration_s: 1,
		max_duration_s: 8,
		anchor: { type: 'hit', seconds: 0.006 },
		level_db: 0
	},
	{
		kind: 'thud',
		label: 'Thud',
		category: 'impact',
		description:
			'A short, dry, low thump, like a soft hit on a chest or a heavy object put down. Punctuates a beat without a tail.',
		default_duration_s: 0.5,
		min_duration_s: 0.15,
		max_duration_s: 1.5,
		anchor: { type: 'hit', seconds: 0.003 },
		level_db: -4
	},
	{
		kind: 'punch',
		label: 'Punch',
		category: 'impact',
		description:
			'A tight whack with a crack on top: for a punch-in zoom, a jump-cut emphasis, a hard text hit.',
		default_duration_s: 0.45,
		min_duration_s: 0.12,
		max_duration_s: 1.2,
		anchor: { type: 'hit', seconds: 0.002 },
		level_db: -1
	},
	{
		kind: 'riser',
		label: 'Riser',
		category: 'build',
		description:
			'Tension that climbs in pitch and brightness for its whole length and ends on its peak. Put its end (the anchor) on the moment it builds to: a drop, a reveal, a cut to the punchline.',
		default_duration_s: 3,
		min_duration_s: 0.8,
		max_duration_s: 12,
		anchor: { type: 'end' },
		level_db: -3
	},
	{
		kind: 'downlifter',
		label: 'Downlifter',
		category: 'build',
		description:
			'The opposite of a riser: it starts on a hit and the sound falls away in pitch and brightness. Releases tension after a drop or a big moment.',
		default_duration_s: 2,
		min_duration_s: 0.6,
		max_duration_s: 8,
		anchor: { type: 'hit', seconds: 0.01 },
		level_db: -4
	},
	{
		kind: 'reverse_swell',
		label: 'Reverse swell',
		category: 'build',
		description:
			'A reversed crash and reverb that swells from silence and ends sharply on its peak. A softer, more musical riser for a transition or the second before a hit.',
		default_duration_s: 2,
		min_duration_s: 0.6,
		max_duration_s: 6,
		anchor: { type: 'end' },
		level_db: -3
	},
	{
		kind: 'pop',
		label: 'Pop',
		category: 'ui',
		description:
			'A short, round bubble pop. For a title, a sticker or a caption appearing.',
		default_duration_s: 0.18,
		min_duration_s: 0.06,
		max_duration_s: 0.6,
		anchor: { type: 'hit', seconds: 0.003 },
		level_db: -6
	},
	{
		kind: 'click',
		label: 'Click',
		category: 'ui',
		description:
			'A crisp interface click. For a button press, a cursor, a list item landing.',
		default_duration_s: 0.08,
		min_duration_s: 0.03,
		max_duration_s: 0.3,
		anchor: { type: 'hit', seconds: 0.002 },
		level_db: -6
	},
	{
		kind: 'ding',
		label: 'Ding',
		category: 'ui',
		description:
			'A clear bell ding with a short ring. For a correct answer, a milestone, a price or number appearing.',
		default_duration_s: 1.2,
		min_duration_s: 0.3,
		max_duration_s: 4,
		anchor: { type: 'hit', seconds: 0.002 },
		level_db: -7
	},
	{
		kind: 'notify',
		label: 'Notify',
		category: 'ui',
		description:
			'A friendly two-note chime, like a message arriving.',
		default_duration_s: 0.9,
		min_duration_s: 0.3,
		max_duration_s: 2.5,
		anchor: { type: 'hit', seconds: 0.002 },
		level_db: -7
	},
	{
		kind: 'bubble',
		label: 'Bubble',
		category: 'ui',
		description:
			'A few rising water-bubble blips. Playful; for an emoji, a sticker or something floating up.',
		default_duration_s: 0.35,
		min_duration_s: 0.1,
		max_duration_s: 1.2,
		anchor: { type: 'hit', seconds: 0.006 },
		level_db: -6
	},
	{
		kind: 'typewriter',
		label: 'Typewriter',
		category: 'ui',
		description:
			'A run of keystrokes. Lay it under a title or a line of text that types itself on.',
		default_duration_s: 1,
		min_duration_s: 0.2,
		max_duration_s: 4,
		anchor: { type: 'hit', seconds: 0.003 },
		level_db: -6
	},
	{
		kind: 'glitch',
		label: 'Glitch',
		category: 'texture',
		description:
			'A burst of digital stutter: bit-crushed bleeps and noise chopped into repeating slices. For a distortion, a rewind, a tech or error moment.',
		default_duration_s: 0.5,
		min_duration_s: 0.15,
		max_duration_s: 2,
		anchor: { type: 'hit', seconds: 0.002 },
		level_db: -5
	},
	{
		kind: 'tape_stop',
		label: 'Tape stop',
		category: 'texture',
		description:
			'A tape machine losing power: the sound slows and drops in pitch to a stop. For a freeze frame or the moment the fun stops.',
		default_duration_s: 1.2,
		min_duration_s: 0.4,
		max_duration_s: 3,
		anchor: { type: 'hit', seconds: 0.0 },
		level_db: -2.5
	},
	{
		kind: 'record_scratch',
		label: 'Record scratch',
		category: 'texture',
		description:
			'A vinyl record scratched back and forth. The comedic "wait, what?" before an unexpected cut.',
		default_duration_s: 0.8,
		min_duration_s: 0.3,
		max_duration_s: 2,
		anchor: { type: 'hit', seconds: 0.0 },
		level_db: -3
	},
	{
		kind: 'shutter',
		label: 'Shutter',
		category: 'texture',
		description:
			'A camera shutter: two mechanical clicks. For a photo being taken or a freeze frame.',
		default_duration_s: 0.35,
		min_duration_s: 0.12,
		max_duration_s: 1,
		anchor: { type: 'hit', seconds: 0.002 },
		level_db: -4
	},
	{
		kind: 'heartbeat',
		label: 'Heartbeat',
		category: 'texture',
		description:
			'A low lub-dub heartbeat, repeating when it is long. For tension or a close call.',
		default_duration_s: 1.2,
		min_duration_s: 0.4,
		max_duration_s: 4,
		anchor: { type: 'hit', seconds: 0.006 },
		level_db: -6
	}
];

/** The categories in the order the panel lists them, with a name for a person (`SfxCategory::label`). */
export const SFX_CATEGORIES: readonly { name: SfxCategory; label: string }[] = [
	{ name: 'transition', label: 'Transitions' },
	{ name: 'impact', label: 'Impacts' },
	{ name: 'build', label: 'Builds' },
	{ name: 'ui', label: 'UI' },
	{ name: 'texture', label: 'Textures' }
];

export const SFX_KINDS: readonly SfxKind[] = SFX_CATALOGUE.map((p) => p.kind);

// ---- the spec ------------------------------------------------------------------------------

/** The catalogue row of a kind. */
export const sfxPreset = (kind: SfxKind): SfxPreset => SFX_CATALOGUE.find((p) => p.kind === kind)!;

/** The kind at its defaults (`SfxSpec::new`). */
export function defaultSfxSpec(kind: SfxKind): SfxSpec {
	return { kind, pitch: 0, intensity: DEFAULT_SFX_INTENSITY, tail: DEFAULT_SFX_TAIL, reverse: false, seed: 0 };
}

/** The render's length, seconds (`SfxSpec::resolved_duration_s`): `duration_s` or the kind's default, held to whole
 *  milliseconds and clamped to the kind's range. */
export function resolvedDuration(spec: SfxSpecInput): number {
	const preset = sfxPreset(spec.kind);
	const wanted = spec.duration_s ?? preset.default_duration_s;
	const length = Number.isFinite(wanted) ? wanted : preset.default_duration_s;
	return Math.min(Math.max(Math.round(length * 1000) / 1000, preset.min_duration_s), preset.max_duration_s);
}

/** The same spec with every field spelled out, the length too (`SfxSpec::resolved`): what is rendered and stored. */
export function resolveSfxSpec(spec: SfxSpecInput): SfxSpec {
	return {
		kind: spec.kind,
		duration_s: resolvedDuration(spec),
		pitch: spec.pitch ?? 0,
		intensity: spec.intensity ?? DEFAULT_SFX_INTENSITY,
		tail: spec.tail ?? DEFAULT_SFX_TAIL,
		reverse: spec.reverse ?? false,
		seed: spec.seed ?? 0
	};
}

/** What is wrong with the numbers of a spec, in the backend's words (`SfxSpec::validate`); null when none is. */
export function sfxSpecProblem(spec: SfxSpecInput): string | null {
	const preset = sfxPreset(spec.kind);
	if (spec.duration_s !== undefined && (!Number.isFinite(spec.duration_s) || spec.duration_s <= 0)) {
		return `duration_s must be a positive number of seconds (got ${spec.duration_s}); it is clamped to ${toFixedEven(preset.min_duration_s, 2)}..=${toFixedEven(preset.max_duration_s, 2)} s for ${spec.kind}`;
	}
	const pitch = spec.pitch ?? 0;
	if (!Number.isFinite(pitch) || Math.abs(pitch) > MAX_SFX_PITCH) {
		return `pitch must be -${MAX_SFX_PITCH}..=${MAX_SFX_PITCH} semitones (got ${pitch})`;
	}
	const intensity = spec.intensity ?? DEFAULT_SFX_INTENSITY;
	if (!Number.isFinite(intensity) || intensity < 0 || intensity > 1) return `intensity must be 0..=1 (got ${intensity})`;
	const tail = spec.tail ?? DEFAULT_SFX_TAIL;
	if (!Number.isFinite(tail) || tail < 0 || tail > 1) return `tail must be 0..=1 (got ${tail})`;
	const seed = spec.seed ?? 0;
	if (!Number.isInteger(seed) || seed < 0 || seed > MAX_SEED) {
		return `seed must be 0..=${MAX_SEED} (2^53 - 1, so it survives JSON in JavaScript; got ${seed})`;
	}
	return null;
}

/** Where the hit is in a render of `len` seconds, seconds from its start (`anchor_s`): the moment that should land on the
 *  picture event. `reverse` mirrors it. */
export function sfxAnchor(spec: SfxSpecInput, len: number): number {
	const length = Math.max(len, 0);
	const anchor: SfxAnchor = sfxPreset(spec.kind).anchor;
	const at = anchor.type === 'hit' ? Math.min(anchor.seconds, length) : anchor.type === 'peak' ? anchor.fraction * length : length;
	return spec.reverse ? length - at : at;
}

/** A short title for the asset: `SFX: Whoosh`. */
export const sfxTitle = (spec: Pick<SfxSpec, 'kind'>): string => `SFX: ${sfxPreset(spec.kind).label}`;

/** A gain in dB as the linear factor a clip carries. */
export const gainOf = (db: number): number => Math.pow(10, db / 20);

/** A stable short hex name for a spec, so the same spec is the same file and the same asset. */
export function sfxHash(spec: SfxSpecInput): string {
	const text = `v1\0${JSON.stringify(resolveSfxSpec(spec))}`;
	let h = 0xcbf29ce484222325n;
	for (const c of new TextEncoder().encode(text)) h = ((h ^ BigInt(c)) * 0x100000001b3n) & 0xffffffffffffffffn;
	return h.toString(16).padStart(16, '0').slice(0, 16);
}

/** The asset a render of `spec` is: its path is the hash of the spec, so the same spec is the same asset. */
export function sfxAsset(spec: SfxSpecInput, newId: () => string): Asset {
	const resolved = resolveSfxSpec(spec);
	return {
		id: newId(),
		path: `/sfx/sfx-${sfxHash(resolved)}.wav`,
		name: sfxTitle(resolved),
		duration: resolved.duration_s!,
		streams: [{ index: 0, kind: 'audio', codec: 'pcm_s16le', sample_rate: 48000, channels: 2 }],
		imported_at: new Date().toISOString(),
		sound_effect: resolved
	};
}

// ---- what the wire refuses -----------------------------------------------------------------

const SPEC_FIELDS = ['kind', 'duration_s', 'pitch', 'intensity', 'tail', 'reverse', 'seed'] as const;
const PLACEMENT_EXTRA = ['at', 'volume_db', 'track_id'] as const;
const SUGGEST_FIELDS = ['range', 'density', 'categories', 'kinds'] as const;
const DENSITIES: readonly SfxDensity[] = ['subtle', 'balanced', 'punchy'];

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);

/** serde's `expected …` clause. */
function expected(names: readonly string[]): string {
	const q = names.map((n) => `\`${n}\``);
	if (q.length === 1) return `expected ${q[0]}`;
	if (q.length === 2) return `expected ${q[0]} or ${q[1]}`;
	return `expected one of ${q.join(', ')}`;
}

const unknownVariant = (got: unknown, names: readonly string[]) => `unknown variant \`${String(got)}\`, ${expected(names)}`;

/** The first thing serde would refuse in a spec's fields (an unknown field, a kind that is none), keys other than the
 *  spec's being `others`. */
function specWireProblem(value: Record<string, unknown>, others: readonly string[]): string | null {
	for (const key of Object.keys(value)) {
		if (!others.includes(key) && !(SPEC_FIELDS as readonly string[]).includes(key)) {
			return `unknown field \`${key}\`, ${expected(SPEC_FIELDS)}`;
		}
	}
	if (!('kind' in value)) return 'missing field `kind`';
	if (!SFX_KINDS.includes(value.kind as SfxKind)) return unknownVariant(value.kind, SFX_KINDS);
	return null;
}

const badArgs = (command: string, arg: string, why: string) => new Error(`invalid args \`${arg}\` for command \`${command}\`: ${why}`);

/** Throw what `add_sound_effects` answers to placements the wire refuses. */
export function checkSfxPlacements(placements: unknown): void {
	if (!Array.isArray(placements)) throw badArgs('add_sound_effects', 'placements', 'invalid type, expected a sequence');
	for (const p of placements) {
		if (!isObject(p)) throw badArgs('add_sound_effects', 'placements', 'invalid type, expected a map');
		if (!('at' in p)) throw badArgs('add_sound_effects', 'placements', 'missing field `at`');
		const why = specWireProblem(p, PLACEMENT_EXTRA);
		if (why) throw badArgs('add_sound_effects', 'placements', why);
	}
}

/** The same for a spec on its own (`update_sound_effect`). */
export function checkSfxSpec(spec: unknown): void {
	if (!isObject(spec)) throw badArgs('update_sound_effect', 'spec', 'invalid type, expected a map');
	const why = specWireProblem(spec, []);
	if (why) throw badArgs('update_sound_effect', 'spec', why);
}

/** The same for `suggest_sound_effects`' request. */
export function checkSfxSuggestRequest(request: unknown): void {
	if (!isObject(request)) throw badArgs('suggest_sound_effects', 'request', 'invalid type, expected a map');
	for (const key of Object.keys(request)) {
		if (!(SUGGEST_FIELDS as readonly string[]).includes(key)) {
			throw badArgs('suggest_sound_effects', 'request', `unknown field \`${key}\`, ${expected(SUGGEST_FIELDS)}`);
		}
	}
	if (request.density !== undefined && !DENSITIES.includes(request.density as SfxDensity)) {
		throw badArgs('suggest_sound_effects', 'request', unknownVariant(request.density, DENSITIES));
	}
	const categories = SFX_CATEGORIES.map((c) => c.name);
	for (const c of (request.categories as unknown[] | undefined) ?? []) {
		if (!categories.includes(c as SfxCategory)) throw badArgs('suggest_sound_effects', 'request', unknownVariant(c, categories));
	}
	for (const k of (request.kinds as unknown[] | undefined) ?? []) {
		if (!SFX_KINDS.includes(k as SfxKind)) throw badArgs('suggest_sound_effects', 'request', unknownVariant(k, SFX_KINDS));
	}
}

// ---- placing -------------------------------------------------------------------------------

const invalid = (why: string) => new Error(`invalid argument: ${why}`);
const PREFIX = 'invalid argument: ';

/** `within`: an invalid-argument refusal with `at` (`effects[2] (riser)`) in front of it; anything else as it is. */
function within<T>(at: string, run: () => T): T {
	try {
		return run();
	} catch (e) {
		if (e instanceof Error && e.message.startsWith(PREFIX)) throw invalid(`${at}: ${e.message.slice(PREFIX.length)}`);
		throw e;
	}
}

/** One checked effect: the spec spelled out and the gain defaulted (`PlannedSfx`). */
export interface PlannedSfx {
	spec: SfxSpec;
	at: number;
	volume_db: number;
	track_id?: string;
}

/** What placing needs to know of a render: its id and length. */
export interface PlacedSfxAsset {
	id: string;
	duration: number;
}

function checkGain(db: number) {
	if (!Number.isFinite(db) || db < MIN_SFX_GAIN_DB || db > MAX_SFX_GAIN_DB) {
		throw invalid(`volume_db must be ${MIN_SFX_GAIN_DB}..=${MAX_SFX_GAIN_DB} (got ${db})`);
	}
}

/** Where a render of `len` seconds starts so that its hit lands on `at`, and how much of its head is cut off to keep the
 *  clip from starting before 0: `[timeline_start, source_in]`. A start less than `SFX_HEAD_SNAP_S` before 0 is moved to 0
 *  untouched (the hit lands that late); a deeper one cuts the head, which must leave `MIN_SFX_CLIP_S` of sound. Null is a
 *  hit that cannot land (`model::sfx::span`). */
export function sfxSpan(spec: SfxSpecInput, len: number, at: number): [number, number] | null {
	const start = at - sfxAnchor(spec, len);
	if (start >= 0) return [start, 0];
	if (-start < SFX_HEAD_SNAP_S) return [0, 0];
	return len + start >= MIN_SFX_CLIP_S - 1e-9 ? [0, -start] : null;
}

/** `span_for`: `sfxSpan`, or the refusal naming the time the effect can land. */
function spanFor(spec: SfxSpecInput, len: number, at: number): [number, number] {
	const span = sfxSpan(spec, len, at);
	if (span) return span;
	const anchor = sfxAnchor(spec, len);
	// Rounded up to the hundredth the message shows, so the time it names is one that lands.
	const earliest = Math.max(Math.ceil((anchor - len + MIN_SFX_CLIP_S) * 100 - 1e-6) / 100, 0);
	throw invalid(
		`its hit is ${toFixedEven(anchor, 2)} s into its ${toFixedEven(len, 2)} s, so it cannot land at ${toFixedEven(at, 2)} s with the start of the timeline in the way; place it at ${toFixedEven(earliest, 2)} s or later`
	);
}

/** A clip for `asset` with its hit at `at`, cut at 0 when it would start before it, with a short fade-in over a cut head. */
function effectClip(asset: PlacedSfxAsset, spec: SfxSpec, at: number, volumeDb: number, id: string): Clip {
	const [start, sourceIn] = spanFor(spec, asset.duration, at);
	const clip: Clip = {
		id,
		asset_id: asset.id,
		source_in: sourceIn,
		source_out: asset.duration,
		timeline_start: start,
		volume: gainOf(volumeDb),
		fade_in: 0,
		fade_out: 0
	};
	if (sourceIn > 0) clip.fade_in = Math.min(TRIM_FADE_S, clipDuration(clip) / 2);
	return clip;
}

/** The lane number `FX` (1), `FX 2` (2), … a track name stands for; null for any other name. */
export function sfxLaneNumber(name: string): number | null {
	if (name === SFX_TRACK) return 1;
	const m = /^FX (\d+)$/.exec(name);
	if (!m) return null;
	const n = Number(m[1]);
	return n >= 2 && sfxLaneName(n) === name ? n : null;
}

export const sfxLaneName = (n: number): string => (n <= 1 ? SFX_TRACK : `${SFX_TRACK} ${n}`);

const clipEnd = (c: Clip): number => c.timeline_start + clipDuration(c);

/** Whether `[start, end)` is free on `track` (touching a neighbour is not an overlap). */
const fits = (track: Track, start: number, end: number): boolean =>
	track.clips.every((c) => !(start < clipEnd(c) - 1e-6 && c.timeline_start < end - 1e-6));

/** The id of the effects lane an effect spanning `[start, end)` goes on: the first unlocked audio track named `FX`,
 *  `FX 2`, … with room, or a new one pushed on the end (`fx_lane`). */
function fxLane(timeline: Timeline, start: number, end: number, newId: () => string): string {
	const lanes = timeline.tracks
		.filter((t) => t.kind === 'audio')
		.flatMap((t) => {
			const n = sfxLaneNumber(t.name);
			return n === null ? [] : [{ n, id: t.id, ok: !t.locked && fits(t, start, end) }];
		})
		.sort((a, b) => a.n - b.n);
	const found = lanes.find((l) => l.ok);
	if (found) return found.id;
	let n = 1;
	while (lanes.some((l) => l.n === n)) n++;
	const track: Track = { id: newId(), kind: 'audio', name: sfxLaneName(n), clips: [] };
	timeline.tracks.push(track);
	return track.id;
}

/** `Timeline::ensure_track_editable`. */
function ensureEditable(timeline: Timeline, trackId: string) {
	const track = timeline.tracks.find((t) => t.id === trackId);
	if (!track) throw new Error(`track not found: ${trackId}`);
	if (track.locked) throw invalid(`track ${track.name} is locked`);
}

/** The audio track `trackId` names, refused unless it is an unlocked audio track. */
function namedLane(timeline: Timeline, trackId: string, at: string): Track {
	within(at, () => ensureEditable(timeline, trackId));
	const track = timeline.tracks.find((t) => t.id === trackId)!;
	if (track.kind !== 'audio') throw invalid(`${at}: a sound effect goes on an audio track; ${track.name} is not one`);
	return track;
}

/** Put every effect of `plan` on `timeline` (mutated), in order, as the clips `ids` of `assets` (one asset per effect): the
 *  one placement rule, shared by the rehearsal before the render and the placing after it (`put_sound_effects`). */
export function putSoundEffects(
	timeline: Timeline,
	assets: readonly PlacedSfxAsset[],
	plan: readonly PlannedSfx[],
	ids: readonly string[],
	newId: () => string
): Clip[] {
	const placed: Clip[] = [];
	plan.forEach((effect, i) => {
		const at = `effects[${i}] (${effect.spec.kind})`;
		const clip = within(at, () => effectClip(assets[i], effect.spec, effect.at, effect.volume_db, ids[i]));
		const laneId = effect.track_id === undefined ? fxLane(timeline, clip.timeline_start, clipEnd(clip), newId) : null;
		const track = laneId === null ? namedLane(timeline, effect.track_id!, at) : timeline.tracks.find((t) => t.id === laneId)!;
		track.clips.push(clip);
		track.clips.sort((a, b) => a.timeline_start - b.timeline_start);
		placed.push(structuredClone(clip));
	});
	return placed;
}

/** Rehearse putting `plan`'s renders on a copy of `timeline` the way placing will, under the ripple the edit will have:
 *  stand-ins of the renders' lengths go through the same placement and the same overlap check, so what is refused is
 *  refused with the same words before anything renders (`check_sfx_fits`). */
export function rehearseSoundEffects(timeline: Timeline, plan: readonly PlannedSfx[], ripple: boolean, newId: () => string) {
	const assets = plan.map((e) => ({ id: newId(), duration: resolvedDuration(e.spec) }));
	const ids = plan.map(() => newId());
	let after = structuredClone(timeline);
	putSoundEffects(after, assets, plan, ids, newId);
	if (ripple) after = rippleLanes(after, timeline);
	checkOwnLanes(after, timeline, ids);
}

/** Check a batch of placements against `timeline` and resolve it (`Project::sound_effects_inputs`): every spec is valid,
 *  every `at` and gain a sane number, a named track exists, is an audio track and is not locked, and the placement
 *  rehearses (`ripple` is the ripple the edit will have). */
export function planSoundEffects(timeline: Timeline, placements: readonly SfxPlacement[], ripple: boolean, newId: () => string): PlannedSfx[] {
	if (placements.length === 0) throw invalid('there are no sound effects to add');
	if (placements.length > MAX_SFX_PER_CALL) throw invalid(`add at most ${MAX_SFX_PER_CALL} sound effects at a time`);
	const plan = placements.map((p, i): PlannedSfx => {
		const at = `effects[${i}] (${p.kind})`;
		const bad = sfxSpecProblem(p);
		if (bad) throw invalid(`${at}: ${bad}`);
		if (!Number.isFinite(p.at) || p.at < 0) throw invalid(`${at}: at must be a time in seconds, 0 or later (got ${p.at})`);
		if (p.volume_db !== undefined) within(at, () => checkGain(p.volume_db!));
		if (p.track_id !== undefined) namedLane(timeline, p.track_id, at);
		return { spec: resolveSfxSpec(p), at: p.at, volume_db: p.volume_db ?? 0, track_id: p.track_id };
	});
	rehearseSoundEffects(timeline, plan, ripple, newId);
	return plan;
}

/** Where on the timeline the hit point of effect clip `clip` is (`effect_anchor_at`): the clip's own mapping of the render's
 *  hit, trim and speed included, not its start. */
export const sfxHitAt = (clip: Clip, asset: Pick<Asset, 'duration'> & { sound_effect: SfxSpecInput }): number =>
	sourceToTimeline(clip, sfxAnchor(asset.sound_effect, asset.duration));

/** A checked reshape (`SfxUpdate`). */
export interface PlannedSfxUpdate {
	clip_id: string;
	spec: SfxSpec;
	volume_db?: number;
	/** Where on the timeline the effect's hit point was when the plan was made, seconds. */
	anchor_at: number;
}

/** Swap the effect clip `clipId` of `timeline` (mutated) for a render of `spec`, its hit at `anchorAt`: the new clip keeps the
 *  old one's id, gain (unless `volumeDb` says another), fades, effects and keys, and plays the whole render at normal speed
 *  (`replace_effect`). */
export function replaceEffect(timeline: Timeline, clipId: string, asset: PlacedSfxAsset, spec: SfxSpec, volumeDb: number | undefined, anchorAt: number): Clip {
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
	if (!track) throw new Error(`clip not found: ${clipId}`);
	ensureEditable(timeline, track.id);
	const [start, sourceIn] = spanFor(spec, asset.duration, anchorAt);
	const clip = track.clips.find((c) => c.id === clipId)!;
	clip.asset_id = asset.id;
	clip.source_in = sourceIn;
	clip.source_out = asset.duration;
	clip.timeline_start = start;
	clip.speed = 1;
	delete clip.speed_ramp;
	if (volumeDb !== undefined) clip.volume = gainOf(volumeDb);
	const duration = clipDuration(clip);
	clip.fade_in = Math.min(Math.max(clip.fade_in, sourceIn > 0 ? TRIM_FADE_S : 0), duration);
	clip.fade_out = Math.min(clip.fade_out, duration);
	track.clips.sort((a, b) => a.timeline_start - b.timeline_start);
	return structuredClone(clip);
}

/** Check a reshape under the lock (`Project::update_sound_effect_inputs`): the clip is on an unlocked audio track and its asset
 *  was synthesized as a sound effect (`assetOf` finds it), the spec and gain are valid, and the swap rehearses. */
export function planSoundEffectUpdate(
	timeline: Timeline,
	clipId: string,
	input: SfxSpecInput,
	volumeDb: number | undefined,
	assetOf: (id: string) => Asset | undefined,
	ripple: boolean
): PlannedSfxUpdate {
	const bad = sfxSpecProblem(input);
	if (bad) throw invalid(bad);
	if (volumeDb !== undefined) checkGain(volumeDb);
	const track = timeline.tracks.find((t) => t.clips.some((c) => c.id === clipId));
	if (!track) throw new Error(`clip not found: ${clipId}`);
	if (track.kind !== 'audio') throw invalid('only a sound effect on an audio track can be reshaped');
	ensureEditable(timeline, track.id);
	const clip = track.clips.find((c) => c.id === clipId)!;
	const asset = assetOf(clip.asset_id);
	if (!asset) throw new Error(`asset not found: ${clip.asset_id}`);
	if (!asset.sound_effect) {
		throw invalid('that clip is not a sound effect: only a clip of a synthesized sound effect can be reshaped');
	}
	const anchorAt = sfxHitAt(clip, { duration: asset.duration, sound_effect: asset.sound_effect });
	const spec = resolveSfxSpec(input);
	let after = structuredClone(timeline);
	replaceEffect(after, clipId, { id: clip.asset_id, duration: spec.duration_s! }, spec, volumeDb, anchorAt);
	if (ripple) after = rippleLanes(after, timeline);
	checkOwnLanes(after, timeline, [clipId]);
	return { clip_id: clipId, spec, volume_db: volumeDb, anchor_at: anchorAt };
}

// ---- suggesting ----------------------------------------------------------------------------

/** How far a scale may change across a cut before it is a punch-in, as a fraction of the outgoing clip's (`PUNCH_IN_SCALE`). */
export const PUNCH_IN_SCALE = 0.08;
const COVER_S = 0.25;
const CLEAR_GAIN_DB = -3;
const SPEECH_GAIN_DB = -8;
const SPEECH_GAP_S = 0.35;
const SPEECH_SLACK_S = 0.1;
const MIN_TITLE_S = 0.3;
const SHORT_TITLE_CHARS = 32;
const LOWER_THIRD_Y = 0.6;
const MIN_LEAD_IN_S = 1;
const HIT_NEAR_S = 0.35;
const SECTION_BARS = 4;
const SECTION_NOVELTY = 0.25;
const MUSIC_CONFIDENCE = 0.5;
const MUSIC_EDGE_S = 0.5;

const SPACING_S: Record<SfxDensity, number> = { subtle: 4, balanced: 2, punchy: 1 };
/** The shortest shot a cut is suggested on, or null when cuts are not suggested. */
const MIN_SHOT_S: Record<SfxDensity, number | null> = { subtle: null, balanced: 1.5, punchy: 0 };
const SOURCES: readonly SfxSource[] = ['cut', 'transition', 'title', 'punch_in', 'music_drop', 'marker', 'opening'];

/** The kinds that can serve a part: the preferred ones, else the fallbacks. */
interface Choices {
	preferred: readonly SfxKind[];
	fallback: readonly SfxKind[];
}
const CUT: Choices = { preferred: ['whoosh', 'swoosh'], fallback: ['swipe'] };
const TRANSITION: Choices = { preferred: ['swoosh'], fallback: ['whoosh', 'swipe'] };
const POP: Choices = { preferred: ['pop'], fallback: ['bubble', 'click', 'ding'] };
const SWIPE: Choices = { preferred: ['swipe'], fallback: ['swoosh'] };
const TYPEWRITER: Choices = { preferred: ['typewriter'], fallback: ['click', 'pop'] };
const PUNCH: Choices = { preferred: ['punch'], fallback: ['thud', 'impact'] };
const RISER: Choices = { preferred: ['riser'], fallback: ['reverse_swell'] };
const HIT: Choices = { preferred: ['impact'], fallback: ['boom', 'thud', 'punch'] };

interface Part {
	pool: Choices;
	/** Which part of its moment it is, so alternating kinds counts like with like. */
	slot: number;
	length_s?: number;
	reason: string;
	usable: SfxKind[];
}

interface Moment {
	at: number;
	source: SfxSource;
	score: number;
	/** A marker is the user's own word: it is never thinned away, and it thins what is near it. */
	pinned: boolean;
	parts: Part[];
}

const round3 = (x: number): number => Math.round(x * 1000) / 1000;
const part = (pool: Choices, reason: string, slot = 0, length_s?: number): Part => ({ pool, slot, length_s, reason, usable: [] });
const moment = (at: number, source: SfxSource, score: number, parts: Part[]): Moment => ({
	at: round3(at),
	source,
	score: round3(Math.min(Math.max(score, 0), 1)),
	pinned: source === 'marker',
	parts
});

/** A name for a person from an asset or title name: whitespace collapsed, control characters gone, cut to a sentence-sized length. */
function short(text: string): string {
	const flat = text
		.replace(/[\u0000-\u0008\u000e-\u001f\u007f]/g, '')
		.split(/\s+/)
		.filter(Boolean)
		.join(' ');
	return [...flat].length <= 28 ? flat : `${[...flat].slice(0, 27).join('')}…`;
}

/** What a marker called `name` asks for: null when it is no sound-effect marker, `'any'` for a bare `sfx`, else its kind. */
function markerKind(name: string): SfxKind | 'any' | null {
	const n = name.trim().toLowerCase();
	if (n === 'sfx') return 'any';
	if (!n.startsWith('sfx:')) return null;
	const kind = n.slice(4).trim().replace(/[ -]/g, '_') as SfxKind;
	return SFX_KINDS.includes(kind) ? kind : null;
}

/** What suggesting reads: the working timeline, the assets and the cached analysis of the assets it uses. */
export interface SfxSuggestInputs {
	timeline: Timeline;
	assets: readonly Asset[];
	analyses: Readonly<Record<string, AssetAnalysis | undefined>>;
}

const dipOrSlide = (kind: string): string => kind.charAt(0).toUpperCase() + kind.slice(1).replace(/_/g, ' ');

/** The harmony changes of analysed music: `[source seconds, novelty]` at each 4-bar boundary whose preceding and following
 *  four bars' mean chroma are `SECTION_NOVELTY` or more apart (`section_changes`). */
function sectionChanges(music: NonNullable<AssetAnalysis['music']>): [number, number][] {
	const mean = (bars: number[][]): number[] | null => {
		const sum = new Array<number>(12).fill(0);
		for (const bar of bars) bar.forEach((v, i) => (sum[i] += v));
		const norm = Math.sqrt(sum.reduce((a, v) => a + v * v, 0));
		return norm > 1e-9 && Number.isFinite(norm) ? sum.map((v) => v / norm) : null;
	};
	const { bar_chroma: chroma, grid } = music;
	const barS = grid.period_s * grid.beats_per_bar;
	const first = grid.phase_s + grid.downbeat_offset * grid.period_s;
	const out: [number, number][] = [];
	for (let k = SECTION_BARS; k + SECTION_BARS <= chroma.length; k += SECTION_BARS) {
		const before = mean(chroma.slice(k - SECTION_BARS, k));
		const after = mean(chroma.slice(k, k + SECTION_BARS));
		if (!before || !after) continue;
		const novelty = 1 - before.reduce((a, v, i) => a + v * after[i], 0);
		if (novelty >= SECTION_NOVELTY) out.push([first + k * barS, Math.min(novelty, 1)]);
	}
	return out;
}

/** Where the working cut would take a sound effect (`model::suggest_sound_effects`, loosely): placements `addSoundEffects`
 *  takes as they are, sorted by time. */
export function suggestSoundEffects(inputs: SfxSuggestInputs, request: SfxSuggestRequest): SfxSuggestion[] {
	const range = request.range;
	if (range && (!Number.isFinite(range.start) || !Number.isFinite(range.end) || range.start < 0 || range.end <= range.start)) {
		throw invalid('range needs 0 <= start < end, in seconds');
	}
	const density = request.density ?? 'balanced';
	const timeline = inputs.timeline;
	const assets = new Map(inputs.assets.map((a) => [a.id, a]));
	const soloed = new Set(timeline.tracks.filter((t) => t.solo).map((t) => t.kind));
	const rendered = timeline.tracks
		.filter((t) => !t.muted && !(soloed.has(t.kind) && !t.solo))
		.map((t): Track => ({ ...t, clips: t.clips.filter((c) => c.enabled !== false) }));
	const videoFps = inputs.assets.flatMap((a) => a.streams.map((s) => s.fps ?? 0)).find((f) => f > 0);
	const frame = 1 / Math.max(timeline.format?.fps ?? videoFps ?? 30, 1);

	const generated = (c: Clip) => !!c.generator;
	const hasStream = (c: Clip, kind: string) => !!assets.get(c.asset_id)?.streams.some((s) => s.kind === kind);
	const picture = (c: Clip) => generated(c) || hasStream(c, 'video');
	const footage = (c: Clip) => !generated(c) && hasStream(c, 'video') && !assets.get(c.asset_id)?.streams.some((s) => s.image);
	const nameOf = (c: Clip) => {
		const title = titleOf(c);
		return title ? short(titleName(title)) : assets.get(c.asset_id) ? short(assets.get(c.asset_id)!.name) : 'a clip';
	};
	const scaleJump = (a: Clip, b: Clip) => {
		const [x, y] = [a.transform?.scale ?? 1, b.transform?.scale ?? 1];
		return Math.abs(x - y) / Math.max(Math.abs(x), 1e-6);
	};
	const animated = (c: Clip) => (c.keyframes?.length ?? 0) > 0 || (c.channels?.length ?? 0) > 0;
	const punchesIn = (a: Clip, b: Clip) =>
		a.asset_id === b.asset_id && footage(a) && footage(b) && scaleJump(a, b) >= PUNCH_IN_SCALE && !animated(a) && !animated(b);
	const continues = (a: Clip, b: Clip) =>
		a.asset_id === b.asset_id && (a.speed ?? 1) === (b.speed ?? 1) && Math.abs(b.source_in - a.source_out) <= (frame / 2) * Math.abs(a.speed ?? 1);

	const moments: Moment[] = [];
	const minShot = MIN_SHOT_S[density];

	// The picture: cuts, transitions, punch-ins.
	for (const track of rendered.filter((t) => t.kind === 'video')) {
		const clips = track.clips.filter(picture).sort((a, b) => a.timeline_start - b.timeline_start);
		for (let i = 0; i + 1 < clips.length; i++) {
			const [a, b] = [clips[i], clips[i + 1]];
			if (Math.abs(b.timeline_start - clipEnd(a)) > frame / 2) continue;
			const at = b.timeline_start;
			const tr = b.transition_in;
			if (tr && Number.isFinite(tr.duration) && tr.duration > 0) {
				const dip = tr.kind === 'dip_to_black' || tr.kind === 'dip_to_white';
				const [start, end] = dip
					? [at - Math.min(tr.duration / 2, clipDuration(a)), at + Math.min(tr.duration / 2, clipDuration(b))]
					: [at, at + Math.min(tr.duration, clipDuration(a), clipDuration(b))];
				if (end - start > 0) {
					const reason = `${dipOrSlide(tr.kind)} from ${nameOf(a)} to ${nameOf(b)} at ${formatTime(at)}`;
					moments.push(moment(start + (end - start) / 2, 'transition', 0.7, [part(TRANSITION, reason, 0, end - start)]));
				}
				continue;
			}
			if (minShot === null || generated(a) || generated(b) || (continues(a, b) && !punchesIn(a, b))) continue;
			const shot = Math.min(clipDuration(a), clipDuration(b));
			if (shot < minShot) continue;
			if (punchesIn(a, b)) {
				const [from, to] = [a.transform?.scale ?? 1, b.transform?.scale ?? 1];
				const reason = `${to > from ? 'Punch-in' : 'Punch-out'} on ${nameOf(b)} at ${formatTime(at)} (${toFixedEven(from * 100, 0)}% to ${toFixedEven(to * 100, 0)}%)`;
				moments.push(moment(at, 'punch_in', 0.65, [part(PUNCH, reason)]));
				continue;
			}
			const same = a.asset_id === b.asset_id;
			const newShot =
				!same || (inputs.analyses[a.asset_id]?.scene_changes ?? []).some((s) => s >= a.source_out - frame && s <= b.source_in + frame);
			const reason = same
				? `Cut within ${nameOf(b)} at ${formatTime(at)}`
				: `Cut from ${nameOf(a)} to ${nameOf(b)} at ${formatTime(at)}`;
			const longer = Math.min(Math.max((shot - 1) / 4, 0), 1);
			moments.push(moment(at, 'cut', 0.35 + 0.15 * longer + (newShot ? 0.1 : 0), [part(CUT, reason)]));
		}
	}

	// Titles appearing.
	const title = (at: number, choices: Choices, noun: string, verb: string, text: string) =>
		moments.push(moment(at, 'title', 0.75, [part(choices, `${noun} "${short(text)}" ${verb} at ${formatTime(at)}`)]));
	for (const o of timeline.overlays ?? []) {
		if (o.generated || !(o.end - o.start >= MIN_TITLE_S)) continue;
		if (o.pos_y >= LOWER_THIRD_Y) title(o.start, SWIPE, 'Lower third', 'slides in', o.text);
		else if ([...o.text].length <= SHORT_TITLE_CHARS) title(o.start, POP, 'Title', 'appears', o.text);
		else title(o.start, SWIPE, 'Title', 'slides in', o.text);
	}
	for (const o of timeline.svg_overlays ?? []) {
		if (!(o.end - o.start >= MIN_TITLE_S)) continue;
		const kind = o.preset?.kind;
		if (kind === 'lower_third') title(o.start, SWIPE, 'Lower third', 'slides in', o.name);
		else if (kind === 'typewriter') title(o.start, TYPEWRITER, 'Title', 'types on', o.name);
		else if (kind === 'word_pop') title(o.start, POP, 'Title', 'pops up', o.name);
		else if (o.y + o.height / 2 >= LOWER_THIRD_Y && o.height <= 0.5) title(o.start, SWIPE, 'Lower third', 'slides in', o.name);
		else title(o.start, POP, 'Title', 'appears', o.name);
	}
	for (const track of rendered.filter((t) => t.kind === 'video')) {
		for (const clip of track.clips.filter((c) => generated(c) && clipDuration(c) >= MIN_TITLE_S)) {
			const name = nameOf(clip);
			const lowered = name.toLowerCase();
			if (lowered.includes('lower third') || lowered.includes('lower-third')) title(clip.timeline_start, SWIPE, 'Lower third', 'slides in', name);
			else if (lowered.includes('typewriter')) title(clip.timeline_start, TYPEWRITER, 'Title', 'types on', name);
			else title(clip.timeline_start, POP, 'Title', 'appears', name);
		}
	}

	// Drops in composed music, section changes in analysed music: a riser into it and an impact on it.
	const drop = (at: number, barS: number, leadIn: boolean, hit: boolean, score: number, what: string, name: string) => {
		const when = formatTime(at);
		const parts: Part[] = [];
		if (leadIn && at >= MIN_LEAD_IN_S) {
			const length = Math.min(Math.min(Math.max(2 * barS, 1.5), 4), at);
			parts.push(part(RISER, `Riser into the ${what} in ${name} at ${when}`, 0, length));
		}
		if (hit) parts.push(part(HIT, `Impact on the ${what} in ${name} at ${when}`, 1));
		if (parts.length > 0) moments.push(moment(at, 'music_drop', score, parts));
	};
	for (const clip of rendered.flatMap((t) => t.clips)) {
		const asset = assets.get(clip.asset_id);
		if (generated(clip) || clip.source_audio === false || (clip.speed ?? 1) < 0 || !asset || !hasStream(clip, 'audio')) continue;
		const sourceSpan = clip.source_out - clip.source_in;
		const pace = sourceSpan > 0 ? clipDuration(clip) / sourceSpan : 1;
		const score = asset.music_score;
		if (score) {
			const spans = sectionSpans(score);
			const covers = (t: number) => t >= clip.source_in - 1e-6 && t < clip.source_out;
			const hits = (score.hits ?? []).filter((h) => covers(h.at_s)).map((h) => ({ kind: h.kind, at: sourceToTimeline(clip, h.at_s) }));
			spans.forEach((span, i) => {
				if (span.kind !== 'drop' || !covers(span.start_s)) return;
				const at = sourceToTimeline(clip, span.start_s);
				const marked = (kinds: string[]) => hits.some((h) => kinds.includes(h.kind) && Math.abs(h.at - at) <= HIT_NEAR_S);
				const built = i > 0 && spans[i - 1].kind === 'build';
				drop(
					at,
					barSeconds(score.bpm) * pace,
					!built && !marked(['riser', 'whoosh']),
					!marked(['impact', 'dropout']),
					0.9 + 0.05 * Math.min(Math.max(span.energy, 0), 1),
					'drop',
					short(asset.name)
				);
			});
			continue;
		}
		const analysis = inputs.analyses[clip.asset_id];
		const music = analysis?.music;
		if (!music || !analysis?.audio_class || analysis.audio_class.class !== 'music' || analysis.audio_class.confidence < MUSIC_CONFIDENCE) continue;
		const barS = music.grid.period_s * music.grid.beats_per_bar;
		if (!Number.isFinite(barS) || barS <= 0) continue;
		for (const [t, novelty] of sectionChanges(music)) {
			if (t < clip.source_in + MUSIC_EDGE_S || t > clip.source_out - MUSIC_EDGE_S) continue;
			const strength = Math.min(Math.max((novelty - SECTION_NOVELTY) / (1 - SECTION_NOVELTY), 0), 1);
			drop(sourceToTimeline(clip, t), barS * pace, true, true, 0.55 + 0.3 * strength, 'section change', short(asset.name));
		}
	}

	// Markers: the user's own word.
	for (const m of timeline.markers ?? []) {
		const wanted = markerKind(m.name);
		if (wanted === null) continue;
		const what = wanted === 'any' ? 'a hit' : `a ${sfxPreset(wanted).label.toLowerCase()}`;
		const reason = `Marker "${short(m.name)}" at ${formatTime(m.time)} asks for ${what}`;
		moments.push(moment(m.time, 'marker', 1, [wanted === 'any' ? part(HIT, reason) : part({ preferred: [wanted], fallback: [] }, reason)]));
	}

	// Punchy only: the hook.
	if (density === 'punchy') {
		const first = rendered
			.filter((t) => t.kind === 'video')
			.flatMap((t) => t.clips)
			.filter(picture)
			.sort((a, b) => a.timeline_start - b.timeline_start)[0];
		if (first) {
			const reason = `Hook: a hit as ${nameOf(first)} opens at ${formatTime(first.timeline_start)}`;
			moments.push(moment(first.timeline_start, 'opening', 0.8, [part(HIT, reason)]));
		}
	}

	// What an effect already covers, and what the request has no sound for, goes before the thinning.
	const existing = timeline.tracks
		.flatMap((t) => t.clips)
		.filter((c) => assets.get(c.asset_id)?.sound_effect)
		.map((c): [number, number] => [c.timeline_start, clipEnd(c)]);
	const allowed = (kind: SfxKind) =>
		(!request.categories?.length && !request.kinds?.length) ||
		!!request.kinds?.includes(kind) ||
		!!request.categories?.includes(sfxPreset(kind).category);
	const lands = (kind: SfxKind, length: number | undefined, at: number) =>
		sfxSpan({ kind }, resolvedDuration({ kind, duration_s: length }), at) !== null;
	const candidates = moments.filter((m) => Number.isFinite(m.at) && m.at >= 0 && !existing.some(([s, e]) => m.at >= s - COVER_S && m.at <= e + COVER_S));
	for (const m of candidates) {
		for (const p of m.parts) {
			const ok = (k: SfxKind) => allowed(k) && lands(k, p.length_s, m.at);
			const preferred = p.pool.preferred.filter(ok);
			p.usable = preferred.length > 0 ? preferred : p.pool.fallback.filter(ok);
		}
		m.parts = m.parts.filter((p) => p.usable.length > 0);
	}
	const wanted = candidates.filter((m) => m.parts.length > 0);

	// Thin: markers first, then the best scores, each kept unless one already kept is too near.
	const order = [...wanted].sort(
		(a, b) => Number(b.pinned) - Number(a.pinned) || b.score - a.score || a.at - b.at || SOURCES.indexOf(a.source) - SOURCES.indexOf(b.source)
	);
	const spacing = SPACING_S[density];
	const kept: Moment[] = [];
	let count = 0;
	for (const m of order) {
		if (count + m.parts.length > MAX_SFX_PER_CALL) continue;
		if (!m.pinned && kept.some((k) => Math.abs(k.at - m.at) < spacing - 1e-9)) continue;
		count += m.parts.length;
		kept.push(m);
	}
	kept.sort((a, b) => a.at - b.at || SOURCES.indexOf(a.source) - SOURCES.indexOf(b.source));

	// The speech in the cut: every transcribed stretch of a clip that plays its own sound, where it plays.
	const spoken: [number, number][] = [];
	for (const clip of rendered.flatMap((t) => t.clips)) {
		const asset = assets.get(clip.asset_id);
		if (generated(clip) || clip.source_audio === false || !asset || !hasStream(clip, 'audio') || asset.sound_effect || asset.music_score) continue;
		for (const seg of inputs.analyses[clip.asset_id]?.transcript ?? []) {
			for (const w of seg.words?.length ? seg.words : [seg]) {
				const [s, e] = [Math.max(w.start, clip.source_in), Math.min(w.end, clip.source_out)];
				if (e <= s) continue;
				const [x, y] = [sourceToTimeline(clip, s), sourceToTimeline(clip, e)];
				spoken.push([Math.min(x, y), Math.max(x, y)]);
			}
		}
	}
	spoken.sort((a, b) => a[0] - b[0]);
	const speech: [number, number][] = [];
	for (const [s, e] of spoken) {
		const last = speech[speech.length - 1];
		if (last && s - last[1] <= SPEECH_GAP_S) last[1] = Math.max(last[1], e);
		else speech.push([s, e]);
	}
	const overSpeech = (t: number) => speech.some(([s, e]) => s <= t + SPEECH_SLACK_S && e >= t - SPEECH_SLACK_S);

	const seen = new Map<string, number>();
	const out: SfxSuggestion[] = [];
	let seed = 0;
	for (const m of kept) {
		for (const p of m.parts) {
			const key = `${m.source}/${p.slot}`;
			const n = seen.get(key) ?? 0;
			seen.set(key, n + 1);
			const kind = p.usable[n % p.usable.length];
			const placement: SfxPlacement = {
				...defaultSfxSpec(kind),
				...(p.length_s !== undefined ? { duration_s: p.length_s } : {}),
				seed,
				at: m.at,
				volume_db: overSpeech(m.at) ? SPEECH_GAIN_DB : CLEAR_GAIN_DB
			};
			out.push({ placement, source: m.source, reason: p.reason, score: m.score });
			seed++;
		}
	}
	return range ? out.filter((s) => s.placement.at >= range.start && s.placement.at < range.end) : out;
}
