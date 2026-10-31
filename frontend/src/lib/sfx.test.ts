import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
	MAX_SEED,
	MIN_SFX_CLIP_S,
	SFX_CATALOGUE,
	SFX_CATEGORIES,
	SFX_KINDS,
	checkSfxPlacements,
	checkSfxSpec,
	checkSfxSuggestRequest,
	defaultSfxSpec,
	planSoundEffectUpdate,
	planSoundEffects,
	putSoundEffects,
	resolveSfxSpec,
	resolvedDuration,
	sfxAnchor,
	sfxAsset,
	sfxHash,
	sfxLaneNumber,
	sfxPreset,
	sfxSpan,
	sfxSpecProblem,
	suggestSoundEffects,
	type SfxSuggestInputs
} from './sfx';
import { clipDuration } from './types';
import type { Asset, AssetAnalysis, Clip, SfxKind, SfxPlacement, Timeline, Track } from './types';

// The catalogue is a copy of `CATALOGUE` in `crates/kerf-core/src/model/sfx.rs`: these tests read that file and fail when
// the two drift. The rest replay the Rust cases of `model/sfx.rs`, `project/sfx_tests.rs` and `model/sfx_suggest.rs` that the
// browser harness mirrors.

const RUST = readFileSync(join(import.meta.dir, '../../../crates/kerf-core/src/model/sfx.rs'), 'utf8');

const snake = (name: string) => name.replace(/(?!^)([A-Z])/g, '_$1').toLowerCase();

interface RustRow {
	kind: string;
	label: string;
	category: string;
	description: string;
	durations: number[];
	anchor: unknown;
	level_db: number;
}

/** The rows of the Rust table, read out of its source. */
function rustTable(): RustRow[] {
	const table = RUST.slice(RUST.indexOf('static CATALOGUE'), RUST.indexOf('/// Every effect Kerf can make, one row per kind.\npub fn catalogue'));
	const str = String.raw`"((?:[^"\\]|\\.)*)"`;
	const row = new RegExp(
		String.raw`preset\(\s*K::(\w+),\s*${str},\s*(\w+),\s*${str},\s*\[([^\]]+)\],\s*(peak\([\d.]+\)|hit\([\d.]+\)|SfxAnchor::End),\s*(-?[\d.]+),\s*\)`,
		'g'
	);
	return [...table.matchAll(row)].map((m) => {
		const anchor = m[6];
		const n = Number(/[\d.]+/.exec(anchor)?.[0]);
		return {
			kind: snake(m[1]),
			label: m[2],
			category: snake(m[3]),
			description: m[4].replace(/\\"/g, '"'),
			durations: m[5].split(',').map(Number),
			anchor: anchor.startsWith('peak')
				? { type: 'peak', fraction: n }
				: anchor.startsWith('hit')
					? { type: 'hit', seconds: n }
					: { type: 'end' },
			level_db: Number(m[7])
		};
	});
}

describe('the catalogue', () => {
	test('is the Rust table, row for row', () => {
		const rust = rustTable();
		expect(rust.length).toBe(21);
		expect(SFX_CATALOGUE.length).toBe(rust.length);
		rust.forEach((r, i) => {
			const ts = SFX_CATALOGUE[i];
			expect(ts.kind).toBe(r.kind as SfxKind);
			expect(ts.label).toBe(r.label);
			expect(ts.category).toBe(r.category as typeof ts.category);
			expect(ts.description).toBe(r.description);
			expect([ts.default_duration_s, ts.min_duration_s, ts.max_duration_s]).toEqual(r.durations);
			expect(ts.anchor).toEqual(r.anchor as typeof ts.anchor);
			expect(ts.level_db).toBe(r.level_db);
		});
	});

	test('lists the kinds and the categories as the Rust enums do', () => {
		const enumBody = (name: string) => RUST.slice(RUST.indexOf(`pub enum ${name} {`)).split('\n}\n')[0];
		const variants = (name: string) =>
			[...enumBody(name).matchAll(/^    ([A-Z]\w+),?$/gm)].map((m) => snake(m[1]));
		expect(variants('SfxKind')).toEqual([...SFX_KINDS]);
		expect(variants('SfxCategory')).toEqual(SFX_CATEGORIES.map((c) => c.name));
		const labels = [...RUST.matchAll(/SfxCategory::\w+ => "(\w+)",\n/g)].map((m) => m[1]);
		// `name()` then `label()`, five of each.
		expect(labels.slice(5)).toEqual(SFX_CATEGORIES.map((c) => c.label));
	});

	test('every row is sane and the lookup finds it', () => {
		for (const row of SFX_CATALOGUE) {
			expect(sfxPreset(row.kind)).toBe(row);
			expect(row.min_duration_s).toBeLessThanOrEqual(row.default_duration_s);
			expect(row.default_duration_s).toBeLessThanOrEqual(row.max_duration_s);
			expect(row.level_db).toBeLessThanOrEqual(0);
		}
		expect(SFX_CATEGORIES.every((c) => SFX_CATALOGUE.some((p) => p.category === c.name))).toBe(true);
	});
});

describe('the spec', () => {
	test('the length is the default clamped to the kind’s range in whole milliseconds', () => {
		expect(resolvedDuration({ kind: 'whoosh' })).toBe(1);
		expect(resolvedDuration({ kind: 'whoosh', duration_s: 10 })).toBe(3);
		expect(resolvedDuration({ kind: 'whoosh', duration_s: 0.01 })).toBe(0.3);
		expect(resolvedDuration({ kind: 'whoosh', duration_s: 0.4567891 })).toBe(0.457);
		expect(resolveSfxSpec({ kind: 'whoosh', duration_s: 1 })).toEqual(resolveSfxSpec({ kind: 'whoosh' }));
		expect(resolveSfxSpec(resolveSfxSpec({ kind: 'pop', pitch: 3 }))).toEqual(resolveSfxSpec({ kind: 'pop', pitch: 3 }));
		expect(defaultSfxSpec('pop')).toEqual({ kind: 'pop', pitch: 0, intensity: 0.7, tail: 0.3, reverse: false, seed: 0 });
	});

	test('the hit point is where the kind says and reverse mirrors it', () => {
		const at = (kind: SfxKind, len: number, reverse = false) => sfxAnchor({ kind, reverse }, len);
		expect(at('whoosh', 2)).toBeCloseTo(1.1, 9);
		expect(at('whoosh', 2, true)).toBeCloseTo(0.9, 9);
		expect(at('impact', 1.6)).toBeCloseTo(0.004, 9);
		expect(at('impact', 1.6, true)).toBeCloseTo(1.596, 9);
		expect(at('riser', 3)).toBe(3);
		expect(at('riser', 3, true)).toBe(0);
		expect(at('tape_stop', 1)).toBe(0);
		expect(at('click', 0.001)).toBeLessThanOrEqual(0.001);
		for (const { kind, default_duration_s: len } of SFX_CATALOGUE) {
			expect(at(kind, len) + at(kind, len, true)).toBeCloseTo(len, 9);
		}
	});

	test('validation names the bad field in the backend’s words', () => {
		expect(sfxSpecProblem({ kind: 'impact' })).toBeNull();
		expect(sfxSpecProblem({ kind: 'impact', duration_s: 0 })).toContain('duration_s must be a positive number');
		expect(sfxSpecProblem({ kind: 'impact', duration_s: Number.NaN })).toContain('duration_s');
		expect(sfxSpecProblem({ kind: 'impact', pitch: 12.5 })).toBe('pitch must be -12..=12 semitones (got 12.5)');
		expect(sfxSpecProblem({ kind: 'impact', intensity: 1.5 })).toBe('intensity must be 0..=1 (got 1.5)');
		expect(sfxSpecProblem({ kind: 'impact', tail: -0.1 })).toBe('tail must be 0..=1 (got -0.1)');
		expect(sfxSpecProblem({ kind: 'impact', seed: MAX_SEED + 2 })).toContain('seed must be 0..=9007199254740991');
		expect(sfxSpecProblem({ kind: 'impact', pitch: -12, intensity: 0, tail: 1, seed: MAX_SEED })).toBeNull();
	});

	test('the same spec is the same file, another seed another one', () => {
		expect(sfxHash({ kind: 'pop' })).toBe(sfxHash({ kind: 'pop', duration_s: 0.18 }));
		expect(sfxHash({ kind: 'pop' })).not.toBe(sfxHash({ kind: 'pop', seed: 1 }));
		const ids = (function* () {
			for (let i = 0; ; i++) yield `id${i}`;
		})();
		const asset = sfxAsset({ kind: 'whoosh', pitch: 2 }, () => ids.next().value);
		expect(asset.name).toBe('SFX: Whoosh');
		expect(asset.duration).toBe(1);
		expect(asset.sound_effect).toEqual({ kind: 'whoosh', duration_s: 1, pitch: 2, intensity: 0.7, tail: 0.3, reverse: false, seed: 0 });
		expect(asset.streams.map((s) => s.kind)).toEqual(['audio']);
	});
});

describe('what the wire refuses', () => {
	test('a misspelled field names itself and the ones that exist', () => {
		expect(() => checkSfxPlacements([{ kind: 'whoosh', at: 1 }, { kind: 'pop', at: 2, pitch: 1, volume_db: -3 }])).not.toThrow();
		expect(() => checkSfxPlacements([{ kind: 'whoosh', at: 1, pitc: 2 }])).toThrow(
			"invalid args `placements` for command `add_sound_effects`: unknown field `pitc`, expected one of `kind`, `duration_s`, `pitch`, `intensity`, `tail`, `reverse`, `seed`"
		);
		expect(() => checkSfxPlacements([{ kind: 'zap', at: 1 }])).toThrow('unknown variant `zap`');
		expect(() => checkSfxPlacements([{ kind: 'pop' }])).toThrow('missing field `at`');
		expect(() => checkSfxPlacements([{ at: 1 }])).toThrow('missing field `kind`');
		expect(() => checkSfxSpec({ kind: 'pop', at: 1 })).toThrow('unknown field `at`');
		expect(() => checkSfxSpec({ kind: 'pop', seed: 3 })).not.toThrow();
		expect(() => checkSfxSuggestRequest({ density: 'loud' })).toThrow('unknown variant `loud`');
		expect(() => checkSfxSuggestRequest({ dense: 'punchy' })).toThrow('unknown field `dense`, expected one of `range`, `density`, `categories`, `kinds`');
		expect(() => checkSfxSuggestRequest({ categories: ['ui'], kinds: ['pop'], density: 'subtle', range: { start: 0, end: 4 } })).not.toThrow();
	});
});

// ---- placing ----------------------------------------------------------------------------------

let n = 0;
const newId = () => `gen-${++n}`;

const clip = (id: string, asset: string, start: number, len: number, source_in = 0): Clip => ({
	id,
	asset_id: asset,
	source_in,
	source_out: source_in + len,
	timeline_start: start,
	volume: 1,
	fade_in: 0,
	fade_out: 0
});
const video = (name: string, ...clips: Clip[]): Track => ({ id: `t-${name}`, kind: 'video', name, clips });
const audio = (name: string, ...clips: Clip[]): Track => ({ id: `t-${name}`, kind: 'audio', name, clips });
const lanes = (tl: Timeline) => tl.tracks.filter((t) => sfxLaneNumber(t.name) !== null).map((t) => [t.name, t.clips.length]);
const at = (kind: SfxKind, when: number, extra: Partial<SfxPlacement> = {}): SfxPlacement => ({ kind, at: when, ...extra });

/** Check, "render" and place a batch the way the three steps do around a real render. */
function add(tl: Timeline, placements: SfxPlacement[], ripple = false) {
	const plan = planSoundEffects(tl, placements, ripple, newId);
	const assets = plan.map((e) => ({ id: `render-${e.spec.kind}`, duration: e.spec.duration_s! }));
	const ids = plan.map(() => newId());
	return putSoundEffects(tl, assets, plan, ids, newId);
}

const emptyCut = (): Timeline => ({ tracks: [video('V1'), audio('A1')] });

describe('placing effects', () => {
	test('each lands with its hit on the requested time, on the first FX lane with room', () => {
		const tl = emptyCut();
		const clips = add(tl, [
			at('whoosh', 10),
			at('impact', 4),
			at('riser', 20, { duration_s: 3 }),
			at('impact', 30, { reverse: true })
		]);
		expect(lanes(tl)).toEqual([['FX', 4]]);
		const [whoosh, impact, riser, reversed] = clips;
		expect(whoosh.timeline_start).toBeCloseTo(10 - 0.55, 9);
		expect(impact.timeline_start).toBeCloseTo(4 - 0.004, 9);
		expect(riser.timeline_start + clipDuration(riser)).toBeCloseTo(20, 9);
		expect(reversed.timeline_start + clipDuration(reversed)).toBeCloseTo(30 + 0.004, 9);
		const track = tl.tracks.find((t) => t.name === 'FX')!;
		expect(track.clips.map((c) => c.timeline_start)).toEqual([...track.clips.map((c) => c.timeline_start)].sort((a, b) => a - b));
	});

	test('effects that overlap take further lanes, in order, and a locked lane is skipped', () => {
		const tl = emptyCut();
		add(tl, [at('riser', 6, { duration_s: 2 }), at('impact', 6), at('boom', 6.5), at('whoosh', 6)]);
		// The riser takes FX; the impact on its end overlaps it (FX 2); the boom after it fits FX again; the whoosh
		// overlaps all of them (FX 3).
		expect(lanes(tl)).toEqual([
			['FX', 2],
			['FX 2', 1],
			['FX 3', 1]
		]);
		const locked = emptyCut();
		locked.tracks.push({ ...audio('FX', clip('x', 'a', 0, 100)), locked: true });
		add(locked, [at('pop', 5)]);
		expect(lanes(locked)).toEqual([
			['FX', 1],
			['FX 2', 1]
		]);
		expect(sfxLaneNumber('FX')).toBe(1);
		expect(sfxLaneNumber('FX 3')).toBe(3);
		expect(sfxLaneNumber('FX 1')).toBeNull();
		expect(sfxLaneNumber('FX 03')).toBeNull();
		expect(sfxLaneNumber('FX3')).toBeNull();
	});

	test('a start before 0 is snapped when it is a few ms, else the head is cut off with a short fade, else refused', () => {
		expect(sfxSpan({ kind: 'whoosh' }, 1, 0.5)![1]).toBeCloseTo(0.05, 9);
		expect(sfxSpan({ kind: 'impact' }, 1.6, 0)).toEqual([0, 0]);
		expect(sfxSpan({ kind: 'whoosh' }, 1, 0.1)![1]).toBeCloseTo(0.45, 9);
		expect(sfxSpan({ kind: 'riser' }, 3, 0.01)).toBeNull();
		const tl = emptyCut();
		const [cut] = add(tl, [at('whoosh', 0.2)]);
		expect(cut.timeline_start).toBe(0);
		expect(cut.source_in).toBeCloseTo(0.35, 9);
		expect(cut.fade_in).toBeCloseTo(0.004, 9);
		const [snapped] = add(emptyCut(), [at('impact', 0)]);
		expect(snapped.source_in).toBe(0);
		expect(snapped.fade_in).toBe(0);
		expect(() => add(emptyCut(), [at('riser', 0.01, { duration_s: 3 })])).toThrow(/effects\[0\] \(riser\): its hit is 3\.00 s into its 3\.00 s.*place it at 0\.02 s or later/);
		expect(MIN_SFX_CLIP_S).toBe(0.02);
	});

	test('the gain is the clip’s volume', () => {
		const [loud, quiet] = add(emptyCut(), [at('pop', 1), at('pop', 3, { volume_db: -6 })]);
		expect(loud.volume).toBe(1);
		expect(quiet.volume).toBeCloseTo(0.501187, 5);
	});

	test('a refusal names the effect and changes nothing', () => {
		const tl = emptyCut();
		tl.tracks.push(audio('A2', clip('a2', 'x', 0, 10)));
		const before = structuredClone(tl);
		const refused = (placements: SfxPlacement[], why: string | RegExp) => {
			expect(() => add(tl, placements)).toThrow(why);
			expect(tl).toEqual(before);
		};
		refused([], 'there are no sound effects to add');
		refused([at('pop', 1), at('riser', -2)], 'effects[1] (riser): at must be a time in seconds, 0 or later (got -2)');
		refused([at('pop', 1, { pitch: 99 })], 'effects[0] (pop): pitch must be -12..=12 semitones (got 99)');
		refused([at('pop', 1, { volume_db: 40 })], 'effects[0] (pop): volume_db must be -60..=12 (got 40)');
		refused([at('pop', 1, { track_id: 't-V1' })], 'effects[0] (pop): a sound effect goes on an audio track; V1 is not one');
		refused([at('pop', 1, { track_id: 'nope' })], 'track not found: nope');
		refused([at('pop', 1, { track_id: 't-A2' })], /would overlap another clip on A2/);
		tl.tracks[2].locked = true;
		before.tracks[2].locked = true;
		refused([at('pop', 20, { track_id: 't-A2' })], 'effects[0] (pop): track A2 is locked');
		expect(() => add(tl, Array.from({ length: 257 }, (_, i) => at('click', i)))).toThrow('add at most 256 sound effects at a time');
	});

	test('with ripple on, an effect that only pushes what is there passes', () => {
		const tl = emptyCut();
		tl.tracks.push(audio('A2', clip('a2', 'x', 5, 4)));
		expect(() => planSoundEffects(tl, [at('boom', 4.5, { track_id: 't-A2' })], false, newId)).toThrow(/overlap/);
		expect(() => planSoundEffects(tl, [at('boom', 4.5, { track_id: 't-A2' })], true, newId)).not.toThrow();
	});
});

describe('reshaping an effect', () => {
	function placed() {
		const tl = emptyCut();
		const [riser] = add(tl, [at('riser', 8, { duration_s: 2 }), at('whoosh', 12)]);
		const asset = sfxAsset({ kind: 'riser', duration_s: 2 }, newId);
		riser.asset_id = asset.id;
		tl.tracks.find((t) => t.name === 'FX')!.clips[0].asset_id = asset.id;
		const assetOf = (id: string) => (id === asset.id ? asset : undefined);
		return { tl, riser, asset, assetOf };
	}

	test('keeps the hit where it was when the length changes', () => {
		const { tl, riser, assetOf } = placed();
		const plan = planSoundEffectUpdate(tl, riser.id, { kind: 'riser', duration_s: 3, pitch: -3 }, -9, assetOf, false);
		expect(plan.anchor_at).toBeCloseTo(8, 9);
		expect(plan.spec.duration_s).toBe(3);
		expect(plan.volume_db).toBe(-9);
	});

	test('refuses what is not an effect, a locked lane, a bad spec and a render that runs into the next clip', () => {
		const { tl, riser, assetOf } = placed();
		const other = clip('other', 'plain', 20, 4);
		tl.tracks.find((t) => t.name === 'A1')!.clips.push(other);
		const plain = { id: 'plain', path: '/a.wav', name: 'a.wav', duration: 4, streams: [], imported_at: '' } as Asset;
		const lookup = (id: string) => (id === 'plain' ? plain : assetOf(id));
		expect(() => planSoundEffectUpdate(tl, other.id, { kind: 'pop' }, undefined, lookup, false)).toThrow('that clip is not a sound effect');
		expect(() => planSoundEffectUpdate(tl, 'nope', { kind: 'pop' }, undefined, lookup, false)).toThrow('clip not found: nope');
		expect(() => planSoundEffectUpdate(tl, riser.id, { kind: 'riser', tail: 2 }, undefined, lookup, false)).toThrow('tail must be 0..=1');
		expect(() => planSoundEffectUpdate(tl, riser.id, { kind: 'riser' }, 99, lookup, false)).toThrow('volume_db must be -60..=12');
		const next = clip('next', 'x', 8.2, 3);
		tl.tracks.find((t) => t.name === 'FX')!.clips.push(next);
		// A riser's end is its hit: the longer one starts earlier, so it does not run into what follows.
		expect(() => planSoundEffectUpdate(tl, riser.id, { kind: 'riser', duration_s: 4 }, undefined, lookup, false)).not.toThrow();
		tl.tracks.find((t) => t.name === 'FX')!.locked = true;
		expect(() => planSoundEffectUpdate(tl, riser.id, { kind: 'riser' }, undefined, lookup, false)).toThrow('track FX is locked');
		tl.tracks.find((t) => t.name === 'V1')!.clips.push(clip('v', 'x', 0, 2));
		expect(() => planSoundEffectUpdate(tl, 'v', { kind: 'pop' }, undefined, lookup, false)).toThrow('only a sound effect on an audio track');
	});
});

// ---- suggesting -------------------------------------------------------------------------------

const stream = (kind: 'video' | 'audio') => ({ index: 0, kind, codec: 'x' });
function asset(id: string, name: string, extra: Partial<Asset> = {}): Asset {
	return { id, path: `/${name}`, name, duration: 60, streams: [stream('video'), stream('audio')], imported_at: '', ...extra };
}
const A = asset('a', 'interview.mp4');
const B = asset('b', 'broll.mp4');

/** `n` back-to-back shots of `len` seconds on V1, alternating between the two assets. */
function shots(count: number, len: number): Timeline {
	return {
		tracks: [video('V1', ...Array.from({ length: count }, (_, i) => clip(`c${i}`, i % 2 ? 'b' : 'a', i * len, len)))]
	};
}

function inputs(timeline: Timeline, assets: Asset[] = [A, B], analyses: Record<string, AssetAnalysis> = {}): SfxSuggestInputs {
	return { timeline, assets, analyses };
}

const times = (out: { placement: { at: number } }[]) => out.map((s) => s.placement.at);

describe('suggesting effects', () => {
	test('a cut gets a whoosh or swoosh on it, alternating down a run, with a reason and a seed each', () => {
		const out = suggestSoundEffects(inputs(shots(5, 4)), {});
		expect(times(out)).toEqual([4, 8, 12, 16]);
		expect(out.map((s) => s.placement.kind)).toEqual(['whoosh', 'swoosh', 'whoosh', 'swoosh']);
		expect(out.map((s) => s.placement.seed)).toEqual([0, 1, 2, 3]);
		expect(out.every((s) => s.source === 'cut' && s.placement.volume_db === -3)).toBe(true);
		expect(out[0].reason).toBe('Cut from interview.mp4 to broll.mp4 at 0:04.0');
		expect(out[0].score).toBeGreaterThan(0.35);
	});

	test('the density sets the spacing and which sources count', () => {
		const tl = shots(9, 1.6);
		expect(suggestSoundEffects(inputs(tl), { density: 'subtle' })).toEqual([]);
		expect(times(suggestSoundEffects(inputs(tl), { density: 'balanced' }))).toEqual([1.6, 4.8, 8, 11.2]);
		const punchy = times(suggestSoundEffects(inputs(tl), { density: 'punchy' }));
		expect(punchy[0]).toBe(0);
		expect(punchy.length).toBeGreaterThan(8);
		const brisk = suggestSoundEffects(inputs(shots(6, 0.5)), { density: 'punchy' });
		expect(times(brisk).slice(1).every((t, i) => t - times(brisk)[i] >= 1 - 1e-9)).toBe(true);
		expect(suggestSoundEffects(inputs(shots(6, 0.5)), {})).toEqual([]);
	});

	test('a shot that carries on is no cut, but a punch-in on it is a punch', () => {
		const same: Timeline = { tracks: [video('V1', clip('a1', 'a', 0, 5), clip('a2', 'a', 5, 5, 5))] };
		expect(suggestSoundEffects(inputs(same), {})).toEqual([]);
		const jump: Timeline = { tracks: [video('V1', clip('a1', 'a', 0, 5), clip('a2', 'a', 5, 5, 5))] };
		jump.tracks[0].clips[1].transform = { scale: 1.3, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1, crop_left: 0, crop_right: 0, crop_top: 0, crop_bottom: 0 };
		const out = suggestSoundEffects(inputs(jump), {});
		expect(out.map((s) => [s.source, s.placement.kind, s.placement.at])).toEqual([['punch_in', 'punch', 5]]);
		expect(out[0].reason).toContain('Punch-in on interview.mp4 at 0:05.0 (100% to 130%)');
	});

	test('a transition is a swoosh as long as it, centred on it', () => {
		const tl = shots(2, 6);
		tl.tracks[0].clips[1].transition_in = { kind: 'crossfade', duration: 1 };
		const [s] = suggestSoundEffects(inputs(tl), {});
		expect([s.source, s.placement.kind, s.placement.at, s.placement.duration_s]).toEqual(['transition', 'swoosh', 6.5, 1]);
		expect(s.reason).toBe('Crossfade from interview.mp4 to broll.mp4 at 0:06.0');
	});

	test('titles pop or swipe at their in-point, captions are left alone', () => {
		const tl: Timeline = {
			tracks: [video('V1', clip('c', 'a', 0, 30))],
			overlays: [
				{ id: 'o1', text: 'Hello', start: 3, end: 6, pos_x: 0.5, pos_y: 0.4, size: 0.1, color: '#fff', bold: false },
				{ id: 'o2', text: 'Jane Doe, producer', start: 9, end: 12, pos_x: 0.5, pos_y: 0.8, size: 0.05, color: '#fff', bold: false },
				{ id: 'o3', text: 'a caption', start: 15, end: 16, pos_x: 0.5, pos_y: 0.9, size: 0.05, color: '#fff', bold: false, generated: true },
				{ id: 'o4', text: 'flicker', start: 18, end: 18.1, pos_x: 0.5, pos_y: 0.4, size: 0.1, color: '#fff', bold: false }
			]
		};
		const out = suggestSoundEffects(inputs(tl), {});
		expect(out.map((s) => [s.source, s.placement.kind, s.placement.at])).toEqual([
			['title', 'pop', 3],
			['title', 'swipe', 9]
		]);
		expect(out[0].reason).toBe('Title "Hello" appears at 0:03.0');
		expect(out[1].reason).toBe('Lower third "Jane Doe, producer" slides in at 0:09.0');
	});

	test('a marker named sfx asks for its sound and is never thinned away', () => {
		const tl = shots(2, 6);
		tl.markers = [
			{ id: 'm1', time: 6.4, name: 'sfx:Record Scratch' },
			{ id: 'm2', time: 9, name: 'sfx' },
			{ id: 'm3', time: 10, name: 'sfx:zap' },
			{ id: 'm4', time: 11, name: 'chapter one' }
		];
		const out = suggestSoundEffects(inputs(tl), { density: 'subtle' });
		expect(out.map((s) => [s.source, s.placement.kind, s.placement.at])).toEqual([
			['marker', 'record_scratch', 6.4],
			['marker', 'impact', 9]
		]);
		expect(out[0].score).toBe(1);
		// A marker thins the cut at 6 s, 0.4 s away, instead of the other way round.
		expect(times(suggestSoundEffects(inputs(tl), {}))).toEqual([6.4, 9]);
	});

	test('a drop in composed music gets a riser into it and an impact on it', () => {
		const song = asset('s', 'song.wav', {
			streams: [stream('audio')],
			music_score: {
				bpm: 120,
				key: 'Am',
				style: 'pop',
				seed: 0,
				sections: [
					{ kind: 'verse', bars: 4, chords: ['Am'] },
					{ kind: 'drop', bars: 4, chords: ['Am'] }
				]
			}
		});
		const tl: Timeline = { tracks: [video('V1', clip('c', 'a', 0, 30)), audio('M', clip('m', 's', 0, 16))] };
		const out = suggestSoundEffects(inputs(tl, [A, song]), {});
		// 4 bars of 2 s: the drop is at 8 s, the riser two bars (4 s) long.
		expect(out.map((s) => [s.source, s.placement.kind, s.placement.at, s.placement.duration_s])).toEqual([
			['music_drop', 'riser', 8, 4],
			['music_drop', 'impact', 8, undefined]
		]);
		expect(out[0].reason).toBe('Riser into the drop in song.wav at 0:08.0');
		// Both are valid placements for the harness's add.
		expect(() => planSoundEffects(tl, out.map((s) => s.placement), false, newId)).not.toThrow();
	});

	test('what an effect already covers is left out, and running it again suggests only what is left', () => {
		const tl = shots(4, 4);
		const first = suggestSoundEffects(inputs(tl), {});
		expect(times(first)).toEqual([4, 8, 12]);
		const effect = sfxAsset({ kind: 'whoosh' }, newId);
		const withEffect: Timeline = { ...tl, tracks: [...tl.tracks, audio('FX', { ...clip('fx', effect.id, 3.45, 1), source_out: 1 })] };
		const again = suggestSoundEffects(inputs(withEffect, [A, B, effect]), {});
		expect(times(again)).toEqual([8, 12]);
	});

	test('over speech the gain is lower', () => {
		const tl = shots(3, 6);
		const analysis = { asset_id: 'a', transcript: [{ start: 4, end: 8, text: 'hello there', speaker: 'S1' }] } as unknown as AssetAnalysis;
		const out = suggestSoundEffects(inputs(tl, [A, B], { a: analysis }), {});
		expect(out.map((s) => [s.placement.at, s.placement.volume_db])).toEqual([
			[6, -8],
			[12, -3]
		]);
	});

	test('categories and kinds limit the sounds, and a moment with none left is dropped', () => {
		const tl = shots(3, 6);
		expect(suggestSoundEffects(inputs(tl), { kinds: ['swipe'] }).map((s) => s.placement.kind)).toEqual(['swipe', 'swipe']);
		expect(suggestSoundEffects(inputs(tl), { categories: ['impact'] })).toEqual([]);
		expect(suggestSoundEffects(inputs(tl), { categories: ['impact'], kinds: ['whoosh'] }).map((s) => s.placement.kind)).toEqual(['whoosh', 'whoosh']);
	});

	test('a range keeps what the whole cut puts there, and a bad one is refused', () => {
		const tl = shots(5, 4);
		const whole = suggestSoundEffects(inputs(tl), {});
		expect(suggestSoundEffects(inputs(tl), { range: { start: 7, end: 13 } })).toEqual(whole.filter((s) => s.placement.at >= 7 && s.placement.at < 13));
		expect(() => suggestSoundEffects(inputs(tl), { range: { start: 5, end: 5 } })).toThrow('range needs 0 <= start < end, in seconds');
	});

	test('is deterministic, sorted, and every suggestion is a placement the harness takes', () => {
		const tl = shots(12, 2.5);
		tl.markers = [{ id: 'm', time: 7.7, name: 'sfx:boom' }];
		for (const density of ['subtle', 'balanced', 'punchy'] as const) {
			const out = suggestSoundEffects(inputs(tl), { density });
			expect(suggestSoundEffects(inputs(tl), { density })).toEqual(out);
			expect(times(out)).toEqual([...times(out)].sort((a, b) => a - b));
			expect(() => planSoundEffects(tl, out.map((s) => s.placement), false, newId)).not.toThrow();
			expect(new Set(out.map((s) => s.placement.seed)).size).toBe(out.length);
		}
	});
});
