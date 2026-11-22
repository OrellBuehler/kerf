// `applyEdits` in the browser harness: several edits as one revision, all or nothing — the page-side half of the
// backend's `apply_edits` command (`commands/editing.rs`, over `Project::batch` and the MCP tool's ops in
// `mcp/batch.rs`). Outside Tauri there is no backend to ask, so `api.ts` runs each op through the harness's own
// single-op function (`trimClip`, `setKeyframes`, …), which is what makes the edit itself — its refusals, ripple,
// links — exactly the single call's, and then folds the revisions those calls recorded into one.
//
// This file is the pure part: the checks the backend makes before the lock (`check`), the `"$K.path"` references
// between ops (`resolve` / `reference`), the answers cut down to their place (`compact`), the revision's name
// (`batch_label`), and the table that maps an op `{tool, args}` — the MCP tool's own snake_case arguments — onto the
// harness function. The table is injected with the api functions (`import type` only: api.ts imports this file).

import type * as Api from './api';
import { BLEND_MODES } from './blend-modes';
import type {
	AudioEffect,
	Behavior,
	BlendMode,
	Clip,
	ClipCut,
	ClipMove,
	Color,
	Curves,
	EditOp,
	Easing,
	Grade,
	Keyframe,
	Mask,
	MotionMode,
	MotionPreset,
	Property,
	PropertyKey,
	PropertyTrack,
	SpeedKey,
	SplitSide,
	Transform,
	TransitionKind,
	VideoEffect,
	Wheel,
	Wheels
} from './types';
import { DEFAULT_MASK, clipDuration } from './types';
import { clipNotFound } from './link-groups';

/** The most ops one call runs (`MAX_USER_OPS` in `mcp/batch.rs`). */
export const MAX_EDITS = 300;

/** The most bytes of earlier answers one batch may copy into later ops through `"$K.path"` (`MAX_REFERENCE_BYTES`). */
export const MAX_REFERENCE_BYTES = 1 << 20;

/** The longest name a batch's revision keeps, in characters (`MAX_LABEL_CHARS`). */
export const MAX_LABEL_CHARS = 200;

/** Text a user typed, as an argument that is to be taken **literally**. A string that starts with `$` in an op's
 *  arguments is a reference to an earlier op's answer (`"$0.right.id"`), or is refused when it names none, so a marker
 *  called `$100`, a title reading `$1.5M` or `$0` would otherwise be read as one: `literal('$100')` is `'$$100'`, which
 *  the batch reads as `$100`. Every string that came from a user and goes into `args` goes through it. */
export function literal(text: string): string {
	return text.startsWith('$') ? '$' + text : text;
}

/** A batch's revision name (`cap_label`): trimmed, and past `MAX_LABEL_CHARS` characters cut short with `…`. `''` is
 *  none, and the revision is named after its edits. */
export function capLabel(label: string | null | undefined): string {
	const named = (label ?? '').trim();
	const chars = Array.from(named);
	return chars.length > MAX_LABEL_CHARS ? chars.slice(0, MAX_LABEL_CHARS - 1).join('') + '…' : named;
}

/** Every tool the backend's `apply_edits` runs (`BATCHABLE` in `mcp/batch.rs`; `batch-edits.test.ts` fails when the two
 *  differ). The harness runs the clip-level ones (`harnessEdits`). */
export const BATCHABLE_TOOLS: readonly string[] = [
	'cut_clip',
	'add_clip_to_timeline',
	'split_at',
	'trim',
	'reorder',
	'move_clip',
	'move_clips',
	'ripple_delete',
	'cut_clip_range',
	'roll_edit',
	'slip_clip',
	'slide_clip',
	'split_remove',
	'split_remove_clips',
	'split_all_tracks',
	'lift_range',
	'extract_range',
	'ripple_delete_gap',
	'remove',
	'remove_clips',
	'duplicate_clips',
	'concatenate',
	'remove_silence',
	'snap_to_beats',
	'add_track',
	'remove_track',
	'extract_audio',
	'add_asset_audio',
	'detach_audio',
	'detach_audio_clips',
	'reattach_audio',
	'reattach_audio_clips',
	'link_clips',
	'unlink_clips',
	'set_volume',
	'set_fade',
	'set_speed',
	'set_speed_ramp',
	'set_transform',
	'set_transform_at',
	'set_color',
	'set_grade',
	'set_transition',
	'set_video_effects',
	'set_audio_effects',
	'set_mask',
	'set_blend_mode',
	'set_clip_enabled',
	'set_keyframes',
	'add_keyframe',
	'set_keyframe_easing',
	'set_property_keyframes',
	'set_property_tracks',
	'copy_keyframes',
	'apply_motion_preset',
	'set_behaviors',
	'bake_behaviors',
	'clear_keyframes',
	'set_reframe',
	'clear_reframe',
	'set_reframe_keyframes',
	'add_reframe_keyframe',
	'set_track_volume',
	'set_track_pan',
	'set_track_duck',
	'set_track_muted',
	'set_track_solo',
	'set_track_locked',
	'set_master_volume',
	'set_master_limiter',
	'set_master_true_peak',
	'set_master_duck',
	'fit_music',
	'add_overlay',
	'update_overlay',
	'remove_overlay',
	'set_overlay_keyframes',
	'remove_svg_overlay',
	'clear_captions',
	'add_marker',
	'update_marker',
	'remove_marker',
	'set_delivery_format'
];

// ---- the checks made before anything runs ------------------------------------------------------------------------

type Args = Record<string, unknown>;

/** Refuse what can be told without running anything (`check`): no ops, more than `MAX_EDITS`, a tool that is no
 *  edit, a key the tool does not take, a LUT file. `table` is what the harness can run; a tool the backend runs that
 *  it cannot is refused as that. */
export function checkEdits(edits: readonly EditOp[], table: Record<string, HarnessEdit>, max = MAX_EDITS): void {
	if (edits.length === 0) throw new Error('`ops` must list at least one edit');
	if (edits.length > max) throw new Error(`\`ops\` lists ${edits.length} edits; one call runs at most ${max}`);
	edits.forEach((edit, i) => {
		const fail = (why: string) => new Error(`op ${i} (${edit.tool}): ${why}`);
		// What the backend's deserializer refuses (`EditOp` is `deny_unknown_fields`, `args` a map: `null` is not one).
		const extra = Object.keys(edit).find((k) => k !== 'tool' && k !== 'args');
		if (extra !== undefined) throw fail('unknown field `' + extra + '`, expected `tool` or `args`');
		if (edit.args !== undefined && !isObj(edit.args)) throw fail('invalid type for `args`: expected a map');
		if (!BATCHABLE_TOOLS.includes(edit.tool)) {
			throw fail(
				'not an edit apply_edits runs (long jobs, files, imports, captions, titles from SVG, staging, history and ' +
					'settings run on their own), or no such tool'
			);
		}
		const run: HarnessEdit | undefined = table[edit.tool];
		if (!run) throw fail('not available in the browser harness, which runs the clip edits (cut, trim, look, keyframes, motion)');
		const accepted = [...run.keys].sort();
		const bad = Object.keys(edit.args ?? {}).find((k) => !accepted.includes(k));
		if (bad !== undefined) {
			const expected = accepted.length === 0 ? 'it takes no arguments' : `expected: ${accepted.join(', ')}`;
			throw fail(`unknown argument "${bad}" for ${edit.tool}; ${expected}`);
		}
		const lut = edit.args?.lut_path;
		if (edit.tool === 'set_grade' && lut !== undefined && lut !== '') {
			throw fail('a lut_path is a file to read; call set_grade on its own for that ("" to remove a LUT is fine)');
		}
	});
}

// ---- "$K.path": an earlier op's answer inside a later op's arguments ----------------------------------------------

/** Replace every `"$K.path"` string in `value` by that value of answer K (`resolve`): `"$$…"` is a literal `$…` (see
 *  `literal`). A reference to an op that has not run, or to a path its answer does not have, throws, and so does one
 *  that takes the batch past `MAX_REFERENCE_BYTES` of copied answers in all (`spent` counts them). */
export function resolve(value: unknown, answers: readonly unknown[], spent = { bytes: 0 }): unknown {
	if (typeof value === 'string') {
		if (value.startsWith('$$')) return value.slice(1);
		const found = reference(value, answers, spent);
		return found === NOT_A_REFERENCE ? value : found;
	}
	if (Array.isArray(value)) return value.map((v) => resolve(v, answers, spent));
	if (value !== null && typeof value === 'object') {
		return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, resolve(v, answers, spent)]));
	}
	return value;
}

const NOT_A_REFERENCE = Symbol('not a reference');

/** The value `s` refers to, `NOT_A_REFERENCE` when it is no reference (`$` then digits, then `.segment`s). */
function reference(s: string, answers: readonly unknown[], spent: { bytes: number }): unknown {
	if (!s.startsWith('$')) return NOT_A_REFERENCE;
	const [head, ...path] = s.slice(1).split('.');
	if (!/^\d+$/.test(head)) return NOT_A_REFERENCE;
	if (path.some((seg) => seg === '' || /\s/.test(seg))) return NOT_A_REFERENCE;
	const index = Number(head);
	if (index >= answers.length) {
		throw new Error(`"${s}" refers to op ${index}, which has not run yet (an op can only use earlier ops' answers)`);
	}
	let node: unknown = answers[index];
	for (const seg of path) {
		// Own keys only: `constructor` and `__proto__` are no part of an answer.
		node = Array.isArray(node)
			? /^\d+$/.test(seg)
				? node[Number(seg)]
				: undefined
			: node !== null && typeof node === 'object' && Object.prototype.hasOwnProperty.call(node, seg)
				? (node as Record<string, unknown>)[seg]
				: undefined;
		if (node === undefined) throw new Error(`"${s}": op ${index}'s answer has nothing at \`${path.join('.')}\``);
	}
	spent.bytes += new TextEncoder().encode(JSON.stringify(node)).length;
	if (spent.bytes > MAX_REFERENCE_BYTES) {
		throw new Error(
			`the references in this batch copy more than ${MAX_REFERENCE_BYTES} bytes of earlier answers ("${s}" is one): refer to less, or send fewer edits`
		);
	}
	return structuredClone(node);
}

// ---- answers --------------------------------------------------------------------------------------------------------

const isClip = (v: unknown): v is Clip =>
	v !== null && typeof v === 'object' && 'asset_id' in v && 'timeline_start' in v;

/** An op's answer as the tool gives it when it is the one clip the op edited, else with every clip in it cut down to
 *  its place (`compact_answer`). */
export function compactAnswer(value: unknown): unknown {
	return isClip(value) ? value : compact(value);
}

/** `value` with every clip in it cut to where it is: its id, asset, place and span (`compact`). */
function compact(value: unknown): unknown {
	if (isClip(value)) {
		const out: Record<string, unknown> = {
			id: value.id,
			asset_id: value.asset_id,
			timeline_start: value.timeline_start,
			timeline_end: value.timeline_start + clipDuration(value),
			source_in: value.source_in,
			source_out: value.source_out
		};
		if (value.link_id) out.link_id = value.link_id;
		return out;
	}
	if (Array.isArray(value)) return value.map(compact);
	if (value !== null && typeof value === 'object') {
		return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, compact(v)]));
	}
	return value;
}

/** The name of a batch that was given none (`batch_label`): its one edit's label, else the count and the kinds of
 *  edit — the first three, then `…`. */
export function batchLabel(labels: readonly string[]): string {
	if (labels.length === 1) return labels[0];
	const kinds = [...new Set(labels)];
	return `${labels.length} edits: ${kinds.slice(0, 3).join(', ')}${kinds.length > 3 ? ', …' : ''}`;
}

// ---- reading an op's arguments --------------------------------------------------------------------------------------

const missing = (k: string) => new Error(`failed to deserialize parameters: missing field \`${k}\``);
const invalidType = (k: string, want: string) =>
	new Error(`failed to deserialize parameters: invalid type for \`${k}\`, expected ${want}`);

const isStr = (v: unknown): v is string => typeof v === 'string';
const isNum = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const isBool = (v: unknown): v is boolean => typeof v === 'boolean';
const isArr = (v: unknown): v is unknown[] => Array.isArray(v);
const isObj = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === 'object' && !Array.isArray(v);

function opt<T>(a: Args, k: string, ok: (v: unknown) => v is T, want: string): T | undefined {
	const v = a[k];
	if (v === undefined || v === null) return undefined;
	if (!ok(v)) throw invalidType(k, want);
	return v;
}

function need<T>(a: Args, k: string, ok: (v: unknown) => v is T, want: string): T {
	const v = opt(a, k, ok, want);
	if (v === undefined) throw missing(k);
	return v;
}

const str = (a: Args, k: string) => need(a, k, isStr, 'a string');
const num = (a: Args, k: string) => need(a, k, isNum, 'a number');
const bool = (a: Args, k: string) => need(a, k, isBool, 'a boolean');
const optStr = (a: Args, k: string) => opt(a, k, isStr, 'a string');
const optNum = (a: Args, k: string) => opt(a, k, isNum, 'a number');
const optBool = (a: Args, k: string) => opt(a, k, isBool, 'a boolean');
const list = (a: Args, k: string) => need(a, k, isArr, 'a list');
const optList = (a: Args, k: string) => opt(a, k, isArr, 'a list');
const strs = (a: Args, k: string): string[] => {
	const items = list(a, k);
	if (!items.every(isStr)) throw invalidType(k, 'a list of strings');
	return items;
};
const optStrs = (a: Args, k: string): string[] | undefined => (a[k] === undefined || a[k] === null ? undefined : strs(a, k));

/** Every number in `keys` that `a` names, as the patch the single-op function takes. */
function patchOf<T>(a: Args, keys: readonly (keyof T & string)[]): Partial<T> {
	const patch: Record<string, number> = {};
	for (const k of keys) {
		const v = optNum(a, k);
		if (v !== undefined) patch[k] = v;
	}
	return patch as Partial<T>;
}

const TRANSFORM_KEYS = ['scale', 'pos_x', 'pos_y', 'rotation', 'opacity', 'crop_left', 'crop_right', 'crop_top', 'crop_bottom'] as const;
const COLOR_KEYS = ['brightness', 'contrast', 'saturation', 'gamma', 'temperature'] as const;
const KEYFRAME_KEYS = ['scale', 'pos_x', 'pos_y', 'rotation', 'opacity'] as const;

/** A blend mode as the backend reads one (`BlendMode::parse`): the wire name, or a common spelling of it. */
function blendMode(s: string): BlendMode {
	const wanted = s.trim().toLowerCase().replace(/[\s-]+/g, '_');
	const found = BLEND_MODES.find((m) => m.id === wanted);
	if (!found) throw new Error(`invalid blend mode '${s}'; expected one of ${BLEND_MODES.map((m) => m.id).join(', ')}`);
	return found.id;
}

const side = (a: Args): SplitSide => {
	const s = need(a, 'side', isStr, '"left" or "right"');
	if (s !== 'left' && s !== 'right') throw invalidType('side', '"left" or "right"');
	return s;
};

/** `set_grade`'s curves (`CurvesArg`): the model's `Curves`, each channel's list of `{x, y}`; `{}` clears. */
function curvesOf(v: unknown): Curves {
	if (!isObj(v)) throw invalidType('curves', 'an object');
	const out: Curves = {};
	for (const [channel, points] of Object.entries(v)) {
		if (!['master', 'red', 'green', 'blue'].includes(channel)) {
			throw new Error(`failed to deserialize parameters: unknown field \`${channel}\` in \`curves\``);
		}
		if (!isArr(points)) throw invalidType(`curves.${channel}`, 'a list of {x, y}');
		out[channel as keyof Curves] = points.map((p) => {
			if (!isObj(p) || !isNum(p.x) || !isNum(p.y)) throw invalidType(`curves.${channel}`, 'a list of {x, y}');
			return { x: p.x, y: p.y };
		});
	}
	return out;
}

/** `set_grade`'s wheels (`WheelsArg`): the three ranges, each `{x, y, lightness}` with what is left out at 0. */
function wheelsOf(v: unknown): Wheels {
	if (!isObj(v)) throw invalidType('wheels', 'an object');
	const wheel = (name: string): Wheel => {
		const w = v[name] ?? {};
		if (!isObj(w)) throw invalidType(`wheels.${name}`, 'an object');
		const read = (k: keyof Wheel) => {
			const n = w[k] ?? 0;
			if (!isNum(n)) throw invalidType(`wheels.${name}.${k}`, 'a number');
			return n;
		};
		return { x: read('x'), y: read('y'), lightness: read('lightness') };
	};
	return { shadows: wheel('shadows'), midtones: wheel('midtones'), highlights: wheel('highlights') };
}

// ---- the harness's ops -----------------------------------------------------------------------------------------------

/** What the harness lends an op: the answer of the last link-aware edit it ran, the clip as the cut has it now, and
 *  ripple forced on or off for one call (`Project::with_ripple`). */
export interface HarnessHost {
	/** A copy of what the last link-aware harness edit (`devRun`) returned: the pieces of a cut, the halves of a split. */
	last(): unknown;
	/** A copy of the clip as the harness cut has it now. */
	clip(id: string): Clip | undefined;
	withRipple<R>(ripple: boolean | undefined, run: () => Promise<R>): Promise<R>;
}

/** One op the harness can run: the keys its arguments may hold (the tool's own, as the MCP schema lists them) and a
 *  function that runs it through the harness's single-op function and answers as the tool does. */
export interface HarnessEdit {
	keys: readonly string[];
	run(a: Args, host: HarnessHost): Promise<unknown>;
}

/** The functions of `api.ts` the table calls. */
export type BatchApi = Pick<
	typeof Api,
	| 'cutClip'
	| 'splitClip'
	| 'trimClip'
	| 'reorderClip'
	| 'moveClip'
	| 'moveClips'
	| 'rippleDelete'
	| 'cutClipRange'
	| 'rollEdit'
	| 'slipClip'
	| 'slideClip'
	| 'splitRemove'
	| 'splitRemoveClips'
	| 'splitAllTracks'
	| 'liftRange'
	| 'extractRange'
	| 'rippleDeleteGap'
	| 'removeClip'
	| 'removeClips'
	| 'duplicateClips'
	| 'setVolume'
	| 'setFade'
	| 'setSpeed'
	| 'setSpeedRamp'
	| 'setTransform'
	| 'setTransformAt'
	| 'setColor'
	| 'setGrade'
	| 'setTransition'
	| 'setVideoEffects'
	| 'setAudioEffects'
	| 'setMask'
	| 'setBlendMode'
	| 'setClipEnabled'
	| 'setKeyframes'
	| 'addKeyframe'
	| 'setKeyframeEasing'
	| 'setPropertyKeyframes'
	| 'setPropertyTracks'
	| 'copyKeyframes'
	| 'applyMotionPreset'
	| 'setBehaviors'
	| 'bakeBehaviors'
	| 'clearKeyframes'
>;

/** The clip an op names, as it ended up. `null` for a clip the edit removed. */
const clipAfter = (a: Args, h: HarnessHost, key = 'clip_id'): Clip | null => h.clip(str(a, key)) ?? null;

/** The ops the harness runs, keyed by the MCP tool's name: the clip-level edits — cut and arrange, look, keyframes,
 *  motion. A tool the backend's `apply_edits` runs that is not here (tracks, the master, markers, titles, captions,
 *  reframe, library audio, linking) is refused in the harness by name. Each answers as the tool does where the
 *  harness knows it — the clip it edited, the pieces a cut left, `"ok"` — and with `null` where it does not (a roll,
 *  slip or slide's outcome, a group move's partners). */
export function harnessEdits(api: BatchApi): Record<string, HarnessEdit> {
	const ripple = ['ripple'] as const;
	const link = ['link'] as const;
	const clipId = ['clip_id'] as const;
	return {
		// ---- cut and arrange
		cut_clip: {
			keys: ['asset_id', 'start', 'end'],
			async run(a, h) {
				await api.cutClip(str(a, 'asset_id'), num(a, 'start'), num(a, 'end'));
				return h.last();
			}
		},
		split_at: {
			keys: [...clipId, 'at', ...ripple, ...link],
			async run(a, h) {
				await h.withRipple(optBool(a, 'ripple'), () => api.splitClip(str(a, 'clip_id'), num(a, 'at'), optBool(a, 'link')));
				const [left, right] = h.last() as [Clip, Clip];
				return { left, right };
			}
		},
		trim: {
			keys: [...clipId, 'source_in', 'source_out', 'timeline_start', ...ripple, ...link],
			async run(a, h) {
				await h.withRipple(optBool(a, 'ripple'), () =>
					api.trimClip(str(a, 'clip_id'), optNum(a, 'source_in'), optNum(a, 'source_out'), optNum(a, 'timeline_start'), optBool(a, 'link'))
				);
				return clipAfter(a, h);
			}
		},
		reorder: {
			keys: ['track_id', 'clip_id', 'new_index'],
			async run(a) {
				await api.reorderClip(str(a, 'track_id'), str(a, 'clip_id'), num(a, 'new_index'));
				return 'ok';
			}
		},
		move_clip: {
			keys: [...clipId, 'timeline_start', 'track_id', ...link],
			async run(a, h) {
				await api.moveClip(str(a, 'clip_id'), num(a, 'timeline_start'), optStr(a, 'track_id'), optBool(a, 'link'));
				return clipAfter(a, h);
			}
		},
		move_clips: {
			keys: ['moves', ...link],
			async run(a) {
				const moves = list(a, 'moves').map((m): ClipMove => {
					if (!isObj(m)) throw invalidType('moves', 'a list of {clip_id, timeline_start, track_id?}');
					const move: ClipMove = { clip_id: str(m, 'clip_id'), timeline_start: num(m, 'timeline_start') };
					const track = optStr(m, 'track_id');
					if (track !== undefined) move.track_id = track;
					return move;
				});
				await api.moveClips(moves, optBool(a, 'link'));
				return null;
			}
		},
		ripple_delete: {
			keys: [...clipId, ...link],
			async run(a) {
				await api.rippleDelete(str(a, 'clip_id'), optBool(a, 'link'));
				return 'ok';
			}
		},
		cut_clip_range: {
			keys: [...clipId, 'from', 'to', ...link],
			async run(a, h) {
				await api.cutClipRange(str(a, 'clip_id'), num(a, 'from'), num(a, 'to'), optBool(a, 'link'));
				return h.last();
			}
		},
		roll_edit: {
			keys: ['clip_a', 'clip_b', 'delta', ...link],
			async run(a) {
				await api.rollEdit(str(a, 'clip_a'), str(a, 'clip_b'), num(a, 'delta'), optBool(a, 'link'));
				return null;
			}
		},
		slip_clip: {
			keys: [...clipId, 'delta', ...link],
			async run(a) {
				await api.slipClip(str(a, 'clip_id'), num(a, 'delta'), optBool(a, 'link'));
				return null;
			}
		},
		slide_clip: {
			keys: [...clipId, 'delta', ...link],
			async run(a) {
				await api.slideClip(str(a, 'clip_id'), num(a, 'delta'), optBool(a, 'link'));
				return null;
			}
		},
		split_remove: {
			keys: [...clipId, 'at', 'side', ...ripple, ...link],
			async run(a, h) {
				await h.withRipple(optBool(a, 'ripple'), () =>
					api.splitRemove(str(a, 'clip_id'), num(a, 'at'), side(a), optBool(a, 'link'))
				);
				return clipAfter(a, h);
			}
		},
		split_remove_clips: {
			keys: ['cuts', 'side', ...ripple, ...link],
			async run(a, h) {
				const cuts = list(a, 'cuts').map((c): ClipCut => {
					if (!isObj(c)) throw invalidType('cuts', 'a list of {clip_id, at}');
					return { clip_id: str(c, 'clip_id'), at: num(c, 'at') };
				});
				await api.splitRemoveClips(cuts, side(a), optBool(a, 'link'), optBool(a, 'ripple'));
				return h.last();
			}
		},
		// The range edits answer what they did (`CutAll`, `RangeEdit`, `GapClosed`), as the tools do.
		split_all_tracks: {
			keys: ['at', 'track_ids', ...link],
			async run(a) {
				return (await api.splitAllTracks(num(a, 'at'), optStrs(a, 'track_ids'), optBool(a, 'link'))).result;
			}
		},
		lift_range: {
			keys: ['from', 'to', 'track_ids', ...link],
			async run(a) {
				return (await api.liftRange(num(a, 'from'), num(a, 'to'), optStrs(a, 'track_ids'), optBool(a, 'link'))).result;
			}
		},
		extract_range: {
			keys: ['from', 'to', 'track_ids', ...link],
			async run(a) {
				return (await api.extractRange(num(a, 'from'), num(a, 'to'), optStrs(a, 'track_ids'), optBool(a, 'link'))).result;
			}
		},
		ripple_delete_gap: {
			keys: ['track_id', 'at', ...link],
			async run(a) {
				return (await api.rippleDeleteGap(str(a, 'track_id'), num(a, 'at'), optBool(a, 'link'))).result;
			}
		},
		remove: {
			keys: [...clipId, ...ripple, ...link],
			async run(a, h) {
				await h.withRipple(optBool(a, 'ripple'), () => api.removeClip(str(a, 'clip_id'), optBool(a, 'link')));
				return 'ok';
			}
		},
		remove_clips: {
			keys: ['clip_ids', ...ripple, ...link],
			async run(a) {
				await api.removeClips(strs(a, 'clip_ids'), optBool(a, 'ripple'), optBool(a, 'link'));
				return null;
			}
		},
		duplicate_clips: {
			keys: ['clip_ids', 'at'],
			async run(a, h) {
				await api.duplicateClips(strs(a, 'clip_ids'), num(a, 'at'));
				return h.last();
			}
		},

		// ---- sound and timing
		set_volume: {
			keys: [...clipId, 'volume'],
			async run(a, h) {
				await api.setVolume(str(a, 'clip_id'), num(a, 'volume'));
				return clipAfter(a, h);
			}
		},
		set_fade: {
			keys: [...clipId, 'fade_in', 'fade_out'],
			async run(a, h) {
				await api.setFade(str(a, 'clip_id'), optNum(a, 'fade_in'), optNum(a, 'fade_out'));
				return clipAfter(a, h);
			}
		},
		set_speed: {
			keys: [...clipId, 'speed', ...ripple, ...link],
			async run(a, h) {
				await h.withRipple(optBool(a, 'ripple'), () => api.setSpeed(str(a, 'clip_id'), num(a, 'speed'), optBool(a, 'link')));
				return clipAfter(a, h);
			}
		},
		set_speed_ramp: {
			keys: [...clipId, 'keys', ...ripple, ...link],
			async run(a, h) {
				const keys = list(a, 'keys') as SpeedKey[];
				await h.withRipple(optBool(a, 'ripple'), () => api.setSpeedRamp(str(a, 'clip_id'), keys, optBool(a, 'link')));
				return clipAfter(a, h);
			}
		},
		set_clip_enabled: {
			keys: [...clipId, 'enabled'],
			async run(a, h) {
				await api.setClipEnabled(str(a, 'clip_id'), bool(a, 'enabled'));
				return clipAfter(a, h);
			}
		},

		// ---- look
		set_transform: {
			keys: [...clipId, ...TRANSFORM_KEYS],
			async run(a, h) {
				await api.setTransform(str(a, 'clip_id'), patchOf<Transform>(a, TRANSFORM_KEYS));
				return clipAfter(a, h);
			}
		},
		set_transform_at: {
			keys: [...clipId, 'time', ...TRANSFORM_KEYS],
			async run(a, h) {
				await api.setTransformAt(str(a, 'clip_id'), num(a, 'time'), patchOf<Transform>(a, TRANSFORM_KEYS));
				return clipAfter(a, h);
			}
		},
		set_color: {
			keys: [...clipId, ...COLOR_KEYS],
			async run(a, h) {
				await api.setColor(str(a, 'clip_id'), patchOf<Color>(a, COLOR_KEYS));
				return clipAfter(a, h);
			}
		},
		set_grade: {
			keys: [...clipId, 'curves', 'wheels', 'lut_path', 'lut_intensity'],
			async run(a, h) {
				const id = str(a, 'clip_id');
				const clip = h.clip(id);
				if (!clip) throw new Error(`clip not found: ${id}`);
				// A patch, as the tool is: the parts left out stay as the clip has them.
				const grade: Grade = { ...(clip.grade ?? {}) };
				if (a.curves !== undefined && a.curves !== null) grade.curves = curvesOf(a.curves);
				if (a.wheels !== undefined && a.wheels !== null) grade.wheels = wheelsOf(a.wheels);
				const intensity = optNum(a, 'lut_intensity');
				if (optStr(a, 'lut_path') === '') {
					grade.lut = null;
				} else if (intensity !== undefined) {
					if (!grade.lut) throw new Error('invalid argument: the clip has no LUT whose intensity to change');
					grade.lut = { ...grade.lut, intensity };
				}
				await api.setGrade(id, grade);
				return clipAfter(a, h);
			}
		},
		set_transition: {
			keys: [...clipId, 'kind', 'duration'],
			async run(a, h) {
				const kind = optStr(a, 'kind');
				const duration = optNum(a, 'duration');
				if (kind !== undefined && duration === undefined) throw new Error('transition duration is required');
				await api.setTransition(
					str(a, 'clip_id'),
					kind === undefined ? null : { kind: kind as TransitionKind, duration: duration as number }
				);
				return clipAfter(a, h);
			}
		},
		set_video_effects: {
			keys: [...clipId, 'effects'],
			async run(a, h) {
				await api.setVideoEffects(str(a, 'clip_id'), list(a, 'effects') as VideoEffect[]);
				return clipAfter(a, h);
			}
		},
		set_audio_effects: {
			keys: [...clipId, 'effects'],
			async run(a, h) {
				await api.setAudioEffects(str(a, 'clip_id'), list(a, 'effects') as AudioEffect[]);
				return clipAfter(a, h);
			}
		},
		set_mask: {
			keys: [...clipId, 'shape', 'x', 'y', 'width', 'height', 'feather', 'inverted'],
			async run(a, h) {
				const id = str(a, 'clip_id');
				const shape = optStr(a, 'shape');
				let mask: Mask | null = null;
				if (shape !== undefined) {
					if (shape !== 'rect' && shape !== 'ellipse') throw new Error(`invalid mask shape '${shape}'; expected "rect" or "ellipse"`);
					// What is left out keeps the clip's mask, so nudging one number does not reset the rest.
					const was = h.clip(id)?.mask ?? DEFAULT_MASK;
					mask = {
						shape,
						x: optNum(a, 'x') ?? was.x,
						y: optNum(a, 'y') ?? was.y,
						width: optNum(a, 'width') ?? was.width,
						height: optNum(a, 'height') ?? was.height,
						feather: optNum(a, 'feather') ?? was.feather,
						inverted: optBool(a, 'inverted') ?? was.inverted ?? false
					};
				}
				await api.setMask(id, mask);
				return clipAfter(a, h);
			}
		},
		set_blend_mode: {
			keys: [...clipId, 'mode'],
			async run(a, h) {
				await api.setBlendMode(str(a, 'clip_id'), blendMode(str(a, 'mode')));
				return clipAfter(a, h);
			}
		},

		// ---- keyframes and motion
		set_keyframes: {
			keys: [...clipId, 'keyframes'],
			async run(a, h) {
				await api.setKeyframes(str(a, 'clip_id'), list(a, 'keyframes') as Keyframe[]);
				return clipAfter(a, h);
			}
		},
		add_keyframe: {
			keys: [...clipId, 'time', ...KEYFRAME_KEYS],
			async run(a, h) {
				await api.addKeyframe(str(a, 'clip_id'), num(a, 'time'), patchOf<Omit<Keyframe, 'time'>>(a, KEYFRAME_KEYS));
				return clipAfter(a, h);
			}
		},
		set_keyframe_easing: {
			keys: [...clipId, 'time', 'prop', 'easing'],
			async run(a, h) {
				const easing = a.easing;
				if (easing === undefined || easing === null) throw missing('easing');
				await api.setKeyframeEasing(str(a, 'clip_id'), num(a, 'time'), easing as Easing, optStr(a, 'prop') as Property | undefined);
				return clipAfter(a, h);
			}
		},
		set_property_keyframes: {
			keys: [...clipId, 'prop', 'keys'],
			async run(a, h) {
				await api.setPropertyKeyframes(str(a, 'clip_id'), str(a, 'prop') as Property, list(a, 'keys') as PropertyKey[]);
				return clipAfter(a, h);
			}
		},
		set_property_tracks: {
			keys: [...clipId, 'tracks'],
			async run(a, h) {
				await api.setPropertyTracks(str(a, 'clip_id'), list(a, 'tracks') as PropertyTrack[]);
				return clipAfter(a, h);
			}
		},
		copy_keyframes: {
			keys: ['from_clip_id', 'to_clip_id', 'props', 'offset'],
			async run(a, h) {
				const props = optList(a, 'props') ?? [];
				if (!props.every(isStr)) throw invalidType('props', 'a list of numbers to copy');
				await api.copyKeyframes(str(a, 'from_clip_id'), str(a, 'to_clip_id'), props as Property[], optNum(a, 'offset') ?? 0);
				return clipAfter(a, h, 'to_clip_id');
			}
		},
		apply_motion_preset: {
			keys: ['clip_ids', 'preset', 'mode', 'at', 'duration', 'intensity'],
			async run(a) {
				const mode = optStr(a, 'mode') as MotionMode | undefined;
				const done = await api.applyMotionPreset(strs(a, 'clip_ids'), str(a, 'preset') as MotionPreset, {
					mode,
					at: optNum(a, 'at'),
					duration: optNum(a, 'duration'),
					intensity: optNum(a, 'intensity')
				});
				return { preset: a.preset, mode: mode ?? 'add', applied: done.applied };
			}
		},
		set_behaviors: {
			keys: ['clip_ids', 'behaviors'],
			async run(a, h) {
				const ids = strs(a, 'clip_ids');
				await api.setBehaviors(ids, list(a, 'behaviors') as Behavior[]);
				return { clips: ids.map((id) => ({ clip_id: id, behaviors: h.clip(id)?.behaviors ?? [] })) };
			}
		},
		bake_behaviors: {
			keys: ['clip_ids', 'fps'],
			async run(a) {
				const done = await api.bakeBehaviors(strs(a, 'clip_ids'), optNum(a, 'fps'));
				return { baked: done.baked };
			}
		},
		clear_keyframes: {
			keys: clipId,
			async run(a, h) {
				await api.clearKeyframes(str(a, 'clip_id'));
				return clipAfter(a, h);
			}
		}
	};
}

// ---- running them ---------------------------------------------------------------------------------------------------

/** The clips an op's arguments name: `clip_id` and its kin, a list of `clip_ids`, the `clip_id` of each of its `cuts`
 *  and `moves`. */
function namedClips(a: Args): string[] {
	const ids = ['clip_id', 'from_clip_id', 'to_clip_id', 'clip_a', 'clip_b'].flatMap((k) => (isStr(a[k]) ? [a[k]] : []));
	if (isArr(a.clip_ids)) ids.push(...a.clip_ids.filter(isStr));
	for (const k of ['cuts', 'moves']) {
		const entries = a[k];
		if (isArr(entries)) ids.push(...entries.flatMap((e) => (isObj(e) && isStr(e.clip_id) ? [e.clip_id] : [])));
	}
	return ids;
}

/** Run `edits` (already `checkEdits`-ed) in order through the harness, each seeing the answers of the ones before it
 *  (`run_ops`). A clip an op names that is not in the cut is refused, as the backend does: several of the harness's
 *  single-op functions let it pass. A failing op throws the tool's own error with `op K (tool): ` in front; what the
 *  ops before it did is for the caller to take back. Resolves to each op's answer, whole. */
export async function runEdits(
	edits: readonly EditOp[],
	table: Record<string, HarnessEdit>,
	host: HarnessHost
): Promise<unknown[]> {
	const answers: unknown[] = [];
	const spent = { bytes: 0 };
	for (const [i, edit] of edits.entries()) {
		try {
			const args = resolve(edit.args ?? {}, answers, spent) as Args;
			for (const id of namedClips(args)) if (!host.clip(id)) throw clipNotFound(id);
			answers.push(structuredClone((await table[edit.tool].run(args, host)) ?? null));
		} catch (e) {
			throw new Error(`op ${i} (${edit.tool}): ${e instanceof Error ? e.message : String(e)}`, { cause: e });
		}
	}
	return answers;
}
