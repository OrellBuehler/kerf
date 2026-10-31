import { describe, expect, test } from 'bun:test';
import { icons } from './components/editor/icons';
import { SFX_CANCELLED } from './api';
import {
	CATEGORY_HUES,
	CATEGORY_OPTIONS,
	DENSITY_OPTIONS,
	dragOf,
	dropGhostFor,
	GAIN_SPEC,
	INTENSITY_SPEC,
	PITCH_SPEC,
	SFX_GLYPHS,
	TAIL_SPEC,
	addLabel,
	addedLine,
	anchorNote,
	dbOf,
	envelopeOf,
	filterPresets,
	gridStep,
	hitFraction,
	isSfxCancelled,
	lengthSpec,
	nextSeed,
	noMatchLine,
	nothingFoundLine,
	patchSpec,
	pickAll,
	pickLine,
	pickNone,
	pickedPlacements,
	placementOf,
	shapeChanges,
	shapeLine,
	shapeSummary,
	sketchOf,
	sourceLabel,
	stampOf,
	suggestRequest,
	suggestionRows,
	togglePick,
	undoLine
} from './sfx-panel';
import { MAX_SEED, SFX_CATALOGUE, SFX_CATEGORIES, SFX_KINDS, defaultSfxSpec, resolvedDuration, sfxPreset } from './sfx';
import { fromPos, readout, toPos } from './slider-row';
import type { SfxKind, SfxSuggestion } from './types';

const preset = (kind: SfxKind) => sfxPreset(kind);

describe('the glyphs and hues', () => {
	test('every kind has an icon the icon set knows, and no two share one', () => {
		expect(Object.keys(SFX_GLYPHS).sort()).toEqual([...SFX_KINDS].sort());
		for (const kind of SFX_KINDS) expect(icons[SFX_GLYPHS[kind]], kind).toBeDefined();
		expect(new Set(Object.values(SFX_GLYPHS)).size).toBe(SFX_KINDS.length);
	});

	test('every category has a theme token for a hue, never a colour literal', () => {
		for (const c of SFX_CATEGORIES) expect(CATEGORY_HUES[c.name]).toMatch(/^var\(--[a-z0-9-]+\)$/);
	});

	test('the chips are All, then the categories in the engine’s order and words', () => {
		expect(CATEGORY_OPTIONS.map((o) => o.label)).toEqual(['All', 'Transitions', 'Impacts', 'Builds', 'UI', 'Textures']);
		expect(CATEGORY_OPTIONS.map((o) => o.value)).toEqual(['all', 'transition', 'impact', 'build', 'ui', 'texture']);
	});
});

describe('the envelope sketch', () => {
	const peakAt = (kind: SfxKind) => {
		const env = envelopeOf(kind, 200);
		return env.indexOf(Math.max(...env)) / 199;
	};

	test('every kind has an envelope of finite values from 0 to 1 that reaches 1', () => {
		for (const kind of SFX_KINDS) {
			const env = envelopeOf(kind);
			expect(env).toHaveLength(64);
			for (const v of env) expect(Number.isFinite(v) && v >= 0 && v <= 1, kind).toBe(true);
			expect(Math.max(...env), kind).toBeCloseTo(1, 9);
		}
	});

	test('it is the same every time, and each kind draws its own', () => {
		const paths = SFX_CATALOGUE.map((p) => sketchOf(p, 100, 20).d);
		expect(paths).toEqual(SFX_CATALOGUE.map((p) => sketchOf(p, 100, 20).d));
		expect(new Set(paths).size).toBe(SFX_CATALOGUE.length);
	});

	test('a swell peaks where the catalogue says its hit point is', () => {
		for (const p of SFX_CATALOGUE) {
			if (p.anchor.type !== 'peak') continue;
			expect(Math.abs(peakAt(p.kind) - p.anchor.fraction), p.kind).toBeLessThan(0.06);
		}
	});

	test('a riser builds to its end and a reverse swell does too, while an impact lands first and falls away', () => {
		expect(peakAt('riser')).toBeGreaterThan(0.85);
		expect(peakAt('reverse_swell')).toBeGreaterThan(0.85);
		for (const kind of ['impact', 'boom', 'thud', 'punch', 'pop', 'click', 'ding', 'downlifter'] as const) {
			expect(peakAt(kind), kind).toBeLessThan(0.12);
		}
		const impact = envelopeOf('impact');
		expect(impact[0]).toBeGreaterThan(impact[20]);
		expect(impact[20]).toBeGreaterThan(impact[63]);
	});

	test('a riser ends sharply, the way it sounds', () => {
		const env = envelopeOf('riser', 100);
		expect(env[94]).toBeGreaterThan(0.8);
		expect(env[99]).toBeLessThan(0.1);
	});

	test('the repeating sounds repeat', () => {
		const peaks = (kind: SfxKind) => {
			const env = envelopeOf(kind, 200);
			return env.filter((v, i) => v > 0.5 && v >= (env[i - 1] ?? 0) && v > (env[i + 1] ?? 0)).length;
		};
		expect(peaks('typewriter')).toBeGreaterThanOrEqual(6);
		expect(peaks('heartbeat')).toBeGreaterThanOrEqual(2);
		expect(peaks('shutter')).toBeGreaterThanOrEqual(1);
	});

	test('reverse plays the shape backwards, and the hit point with it', () => {
		const fwd = envelopeOf('impact', 64);
		const back = envelopeOf('impact', 64, true);
		expect(back).toEqual([...fwd].reverse());
		expect(hitFraction(preset('whoosh'), true)).toBeCloseTo(0.45, 9);
		expect(hitFraction(preset('riser'), true)).toBe(0);
		expect(hitFraction(preset('riser'))).toBe(1);
	});

	test('a hit near the start is at the start, and never past the end', () => {
		for (const p of SFX_CATALOGUE) {
			const x = hitFraction(p);
			expect(x).toBeGreaterThanOrEqual(0);
			expect(x).toBeLessThanOrEqual(1);
		}
		expect(hitFraction(preset('impact'))).toBeLessThan(0.01);
		expect(hitFraction({ anchor: { type: 'hit', seconds: 9 }, default_duration_s: 1 })).toBe(1);
	});

	test('the path is a closed shape inside its box, drawn both ways from the middle', () => {
		for (const p of SFX_CATALOGUE) {
			const { d, hit } = sketchOf(p, 120, 24);
			expect(d.startsWith('M')).toBe(true);
			expect(d.endsWith('Z')).toBe(true);
			expect(hit).toBeGreaterThanOrEqual(0);
			const nums = d.match(/-?\d+(?:\.\d+)?/g)!.map(Number);
			expect(nums.length).toBe(64 * 4);
			for (let i = 0; i < nums.length; i += 2) {
				expect(nums[i]).toBeGreaterThanOrEqual(0);
				expect(nums[i]).toBeLessThanOrEqual(120);
				expect(nums[i + 1]).toBeGreaterThanOrEqual(0);
				expect(nums[i + 1]).toBeLessThanOrEqual(24);
			}
		}
	});

	test('the quietest stretch keeps a hairline', () => {
		const { d } = sketchOf(preset('click'), 100, 20);
		const ys = [...d.matchAll(/,(-?\d+(?:\.\d+)?)/g)].map((m) => Number(m[1]));
		expect(Math.max(...ys) - Math.min(...ys)).toBeGreaterThan(0);
		const top = ys.slice(0, 64);
		expect(Math.max(...top)).toBeLessThan(10);
	});
});

describe('filtering the grid', () => {
	test('nothing typed and All is the whole catalogue, in order', () => {
		expect(filterPresets(SFX_CATALOGUE, '', 'all').map((p) => p.kind)).toEqual([...SFX_KINDS]);
		expect(filterPresets(SFX_CATALOGUE, '   ', 'all')).toHaveLength(SFX_CATALOGUE.length);
	});

	test('a category keeps its own rows', () => {
		const builds = filterPresets(SFX_CATALOGUE, '', 'build').map((p) => p.kind);
		expect(builds).toEqual(['riser', 'downlifter', 'reverse_swell']);
		for (const c of SFX_CATEGORIES) {
			for (const p of filterPresets(SFX_CATALOGUE, '', c.name)) expect(p.category).toBe(c.name);
		}
		const counts = SFX_CATEGORIES.map((c) => filterPresets(SFX_CATALOGUE, '', c.name).length);
		expect(counts.reduce((a, b) => a + b, 0)).toBe(SFX_CATALOGUE.length);
	});

	test('a search is case-insensitive and finds a name, a kind written with a space, a category or a word of the description', () => {
		expect(filterPresets(SFX_CATALOGUE, 'WHOOSH', 'all').map((p) => p.kind)).toContain('whoosh');
		expect(filterPresets(SFX_CATALOGUE, 'reverse swell', 'all').map((p) => p.kind)).toEqual(['reverse_swell']);
		expect(filterPresets(SFX_CATALOGUE, 'record', 'all').map((p) => p.kind)).toContain('record_scratch');
		expect(filterPresets(SFX_CATALOGUE, 'textures', 'all').map((p) => p.kind)).toContain('heartbeat');
		expect(filterPresets(SFX_CATALOGUE, 'camera', 'all').map((p) => p.kind)).toContain('shutter');
	});

	test('every word typed has to match, and the category narrows the search', () => {
		expect(filterPresets(SFX_CATALOGUE, 'riser swell', 'all').map((p) => p.kind)).toEqual(['reverse_swell']);
		expect(filterPresets(SFX_CATALOGUE, 'riser build', 'all').map((p) => p.kind)).toEqual(['riser', 'downlifter', 'reverse_swell']);
		expect(filterPresets(SFX_CATALOGUE, 'riser banana', 'all')).toEqual([]);
		expect(filterPresets(SFX_CATALOGUE, 'whoosh', 'impact')).toEqual([]);
	});

	test('what it says when nothing matches', () => {
		expect(noMatchLine('zzz', 'all')).toBe('No sound matches “zzz”.');
		expect(noMatchLine('zzz', 'ui')).toBe('No UI sound matches “zzz”. Try All.');
		expect(noMatchLine('', 'ui')).toBe('Nothing in this category.');
	});
});

describe('the arrow keys across the grid', () => {
	test('left and right step by one, up and down by a row, and stay at an edge', () => {
		expect(gridStep(4, 21, 3, 'ArrowRight')).toBe(5);
		expect(gridStep(4, 21, 3, 'ArrowLeft')).toBe(3);
		expect(gridStep(4, 21, 3, 'ArrowDown')).toBe(7);
		expect(gridStep(4, 21, 3, 'ArrowUp')).toBe(1);
		expect(gridStep(1, 21, 3, 'ArrowUp')).toBe(0);
		expect(gridStep(0, 21, 3, 'ArrowLeft')).toBe(0);
		expect(gridStep(19, 21, 3, 'ArrowDown')).toBe(20);
		expect(gridStep(20, 21, 3, 'ArrowRight')).toBe(20);
	});

	test('Home and End are the ends, and an empty grid has nowhere to go', () => {
		expect(gridStep(9, 21, 3, 'Home')).toBe(0);
		expect(gridStep(9, 21, 3, 'End')).toBe(20);
		expect(gridStep(0, 0, 3, 'ArrowRight')).toBe(-1);
		expect(gridStep(2, 5, 0, 'ArrowDown')).toBe(3);
	});
});

describe('the Shape sliders', () => {
	test('Length covers the kind’s own range and rests at its default', () => {
		for (const p of SFX_CATALOGUE) {
			const s = lengthSpec(p);
			expect(s.min).toBe(p.min_duration_s);
			expect(s.max).toBe(p.max_duration_s);
			expect(s.neutral).toBe(p.default_duration_s);
			expect(fromPos(toPos(p.default_duration_s, s), s)).toBeCloseTo(p.default_duration_s, 2);
		}
		expect(readout(1, lengthSpec(preset('whoosh')))).toBe('1.00s');
	});

	test('Pitch is whole semitones either way, Intensity and Tail are percentages that rest at the engine’s defaults', () => {
		expect([PITCH_SPEC.min, PITCH_SPEC.max, PITCH_SPEC.step]).toEqual([-12, 12, 1]);
		expect(readout(3, PITCH_SPEC)).toBe('3 st');
		expect(INTENSITY_SPEC.neutral).toBe(defaultSfxSpec('whoosh').intensity);
		expect(TAIL_SPEC.neutral).toBe(defaultSfxSpec('whoosh').tail);
		expect(readout(0.7, INTENSITY_SPEC)).toBe('70%');
		expect(readout(0.3, TAIL_SPEC)).toBe('30%');
	});

	test('Gain fits the number column in dB and may be typed as far as a clip’s gain goes', () => {
		expect(readout(-24, GAIN_SPEC)).toBe('-24.0dB');
		expect(readout(-24, GAIN_SPEC).length).toBeLessThanOrEqual(8);
		expect(GAIN_SPEC.limit).toEqual({ min: -60, max: 12 });
	});
});

describe('a spec patched', () => {
	const base = defaultSfxSpec('whoosh');

	test('a patch is written over it and nothing else moves', () => {
		const next = patchSpec(base, { pitch: 3, reverse: true });
		expect(next).toEqual({ ...base, pitch: 3, reverse: true });
		expect(base.pitch).toBe(0);
	});

	test('every number is held to what the engine accepts', () => {
		const wild = patchSpec(base, { pitch: 40.4, intensity: 9, tail: -3, seed: -5 });
		expect(wild.pitch).toBe(12);
		expect(wild.intensity).toBe(1);
		expect(wild.tail).toBe(0);
		expect(wild.seed).toBe(0);
		expect(patchSpec(base, { seed: 1e20 }).seed).toBe(MAX_SEED);
		expect(patchSpec(base, { pitch: -2.6 }).pitch).toBe(-3);
	});

	test('a length is held to the kind’s range, whole milliseconds', () => {
		expect(patchSpec(base, { duration_s: 99 }).duration_s).toBe(3);
		expect(patchSpec(base, { duration_s: 0.001 }).duration_s).toBe(0.3);
		expect(patchSpec(base, { duration_s: 1.23456 }).duration_s).toBe(1.235);
		expect(patchSpec(base, { pitch: 1 }).duration_s).toBeUndefined();
	});

	test('changing the kind forgets a length that was the old kind’s', () => {
		const long = patchSpec(base, { duration_s: 2.5 });
		const swapped = patchSpec(long, { kind: 'click' });
		expect(swapped.duration_s).toBeUndefined();
		expect(resolvedDuration(swapped)).toBe(sfxPreset('click').default_duration_s);
	});

	test('a new seed is never the old one and stays readable', () => {
		expect(nextSeed(0, () => 0)).toBe(1);
		expect(nextSeed(1, () => 0)).toBe(2);
		expect(nextSeed(9999, () => 0.99999)).toBe(1);
		for (const r of [0, 0.25, 0.5, 0.999999]) {
			const n = nextSeed(4821, () => r);
			expect(n).not.toBe(4821);
			expect(Number.isInteger(n) && n >= 1 && n <= 9999).toBe(true);
		}
	});
});

describe('what a spec says about itself', () => {
	test('the summary lists only what moved off the defaults', () => {
		const spec = defaultSfxSpec('whoosh');
		expect(shapeSummary(spec)).toBe('Defaults');
		expect(shapeSummary(patchSpec(spec, { pitch: 3, tail: 0.6, reverse: true, seed: 12 }))).toBe('+3 st · Tail 60% · Reversed · #12');
		expect(shapeSummary(patchSpec(spec, { duration_s: 1.5, intensity: 0.2, pitch: -4 }))).toBe('1.50 s · -4 st · Intensity 20%');
	});

	test('the footer line is the length, then what moved', () => {
		const spec = defaultSfxSpec('whoosh');
		expect(shapeLine(spec)).toBe('1.00 s');
		expect(shapeLine(patchSpec(spec, { duration_s: 1.5, pitch: -4, reverse: true }))).toBe('1.50 s · -4 st · Reversed');
		expect(shapeChanges(patchSpec(spec, { duration_s: 1.5 }))).toEqual([]);
	});

	test('the anchor note says which part lands on the playhead, and how far before it the clip starts', () => {
		expect(anchorNote(defaultSfxSpec('impact'))).toBe('Its hit lands on the playhead.');
		expect(anchorNote(defaultSfxSpec('whoosh'))).toBe('Its peak lands on the playhead; the clip starts 0.55 s before it.');
		expect(anchorNote(defaultSfxSpec('riser'))).toBe('Its end lands on the playhead; the clip starts 3.00 s before it.');
		expect(anchorNote(patchSpec(defaultSfxSpec('riser'), { reverse: true }))).toBe('Its hit lands on the playhead.');
		expect(anchorNote(patchSpec(defaultSfxSpec('whoosh'), { duration_s: 2 }))).toBe('Its peak lands on the playhead; the clip starts 1.10 s before it.');
	});

	test('a clip’s gain is read back in dB', () => {
		expect(dbOf(1)).toBe(0);
		expect(dbOf(0.5)).toBe(-6);
		expect(dbOf(10 ** (-3 / 20))).toBe(-3);
		expect(dbOf(0)).toBe(-60);
	});

	test('the backend’s “cancelled” answer is told from a failure', () => {
		expect(isSfxCancelled(new Error('sound effect cancelled'))).toBe(true);
		expect(isSfxCancelled('sound effect cancelled')).toBe(true);
		expect(isSfxCancelled(new Error(SFX_CANCELLED))).toBe(true);
		expect(isSfxCancelled(new Error('invalid argument: pitch'))).toBe(false);
	});
});

describe('a placement', () => {
	const spec = defaultSfxSpec('whoosh');

	test('is the spec flat with where its hit lands', () => {
		expect(placementOf(spec, 12.5)).toEqual({ kind: 'whoosh', pitch: 0, intensity: 0.7, tail: 0.3, reverse: false, seed: 0, at: 12.5 });
	});

	test('names the length only when it was set, the gain only when it is not 0, and the track only when given', () => {
		const p = placementOf(patchSpec(spec, { duration_s: 1.2 }), 3, -4, 'track-1');
		expect(p.duration_s).toBe(1.2);
		expect(p.volume_db).toBe(-4);
		expect(p.track_id).toBe('track-1');
		const plain = placementOf(spec, 3, 0);
		expect('duration_s' in plain).toBe(false);
		expect('volume_db' in plain).toBe(false);
		expect('track_id' in plain).toBe(false);
	});
});

describe('a tile dragged over a lane', () => {
	const whoosh = dragOf(defaultSfxSpec('whoosh'), 0, 'Whoosh');
	const riser = dragOf(defaultSfxSpec('riser'), -3, 'Riser');

	test('carries the spec, its gain and its length', () => {
		expect(whoosh).toEqual({ spec: defaultSfxSpec('whoosh'), gainDb: 0, duration: 1, label: 'Whoosh' });
		expect(riser.duration).toBe(3);
	});

	test('starts before the drop by how far in its hit is, so the hit lands on it', () => {
		const g = dropGhostFor(whoosh, 5, []);
		expect(g.start).toBeCloseTo(5 - 0.55, 9);
		expect(g.dur).toBe(1);
		expect(g.ok).toBe(true);
		expect(dropGhostFor(riser, 10, [])).toMatchObject({ start: 7, dur: 3, ok: true });
	});

	test('a hit close to the start of the timeline has its head cut off, and one that cannot land is red', () => {
		const cut = dropGhostFor(riser, 1, []);
		expect(cut.start).toBe(0);
		expect(cut.dur).toBeCloseTo(1, 9);
		expect(cut.ok).toBe(true);
		const none = dropGhostFor(riser, 0.005, []);
		expect(none.ok).toBe(false);
		expect(none.start).toBe(0);
		expect(dropGhostFor(whoosh, 0.55 - 0.005, []).ok).toBe(true);
	});

	test('is red where another clip is in the way, and only there', () => {
		const lane: [number, number][] = [[2, 4]];
		expect(dropGhostFor(whoosh, 5, lane).ok).toBe(true);
		expect(dropGhostFor(whoosh, 3, lane).ok).toBe(false);
		expect(dropGhostFor(whoosh, 4.5, lane).ok).toBe(false);
		expect(dropGhostFor(whoosh, 4.55, lane).ok).toBe(true);
		expect(dropGhostFor(whoosh, 2.5, lane).ok).toBe(false);
	});
});

describe('auto sound design', () => {
	const found = (kind: SfxKind, at: number, extra: Partial<SfxSuggestion> = {}): SfxSuggestion => ({
		placement: { ...defaultSfxSpec(kind), at },
		source: 'cut',
		reason: `Cut at ${at}`,
		score: 0.5,
		...extra
	});
	const list = [found('whoosh', 1), found('pop', 4, { source: 'title' }), found('impact', 9, { source: 'music_drop' })];

	test('the densities are the three the engine has, each saying what it counts', () => {
		expect(DENSITY_OPTIONS.map((d) => d.value)).toEqual(['subtle', 'balanced', 'punchy']);
		expect(DENSITY_OPTIONS.map((d) => d.label)).toEqual(['Subtle', 'Balanced', 'Punchy']);
		for (const d of DENSITY_OPTIONS) expect(d.hint.length).toBeGreaterThan(10);
	});

	test('the request carries the density and, for one category, only that category', () => {
		expect(suggestRequest('balanced', 'all')).toEqual({ density: 'balanced' });
		expect(suggestRequest('punchy', 'impact')).toEqual({ density: 'punchy', categories: ['impact'] });
	});

	test('a row is a moment: where, what, why and how loud', () => {
		const rows = suggestionRows([found('whoosh', 1.5, { placement: { ...defaultSfxSpec('whoosh'), at: 1.5, volume_db: -8 } }), list[1]]);
		expect(rows[0]).toMatchObject({ index: 0, kind: 'whoosh', label: 'Whoosh', category: 'transition', source: 'Cut', at: 1.5, gainDb: -8, reason: 'Cut at 1.5' });
		expect(rows[1]).toMatchObject({ index: 1, kind: 'pop', label: 'Pop', source: 'Title', gainDb: null });
	});

	test('every source has a word', () => {
		expect(
			(['cut', 'transition', 'title', 'punch_in', 'music_drop', 'marker', 'opening'] as const).map(sourceLabel)
		).toEqual(['Cut', 'Transition', 'Title', 'Punch-in', 'Music drop', 'Marker', 'Opening']);
	});

	test('all, none and a toggle', () => {
		expect([...pickAll(3)]).toEqual([0, 1, 2]);
		expect([...pickNone()]).toEqual([]);
		const one = togglePick(pickAll(3), 1);
		expect([...one].sort()).toEqual([0, 2]);
		expect([...togglePick(one, 1)].sort()).toEqual([0, 1, 2]);
		const all = pickAll(2);
		togglePick(all, 0);
		expect(all.size).toBe(2);
	});

	test('what is added is exactly what was suggested, for the ticked ones, in time order', () => {
		const placements = pickedPlacements(list, new Set([2, 0]));
		expect(placements).toEqual([list[0].placement, list[2].placement]);
		expect(pickedPlacements(list, pickNone())).toEqual([]);
		expect(pickedPlacements(list, pickAll(3))).toHaveLength(3);
	});

	test('the words around the list', () => {
		expect(addLabel(0)).toBe('Add effects');
		expect(addLabel(1)).toBe('Add 1 effect');
		expect(addLabel(5)).toBe('Add 5 effects');
		expect(pickLine(5, 12)).toBe('5 of 12 selected');
		expect(addedLine(1, 'Whoosh')).toBe('Added Whoosh');
		expect(addedLine(1)).toBe('Added 1 sound effect');
		expect(addedLine(4)).toBe('Added 4 sound effects');
		expect(undoLine(1)).toBe('Undo takes it out again.');
		expect(undoLine(4)).toBe('Undo takes them out again.');
		expect(nothingFoundLine('balanced', 'all')).toBe('Nothing in the cut calls for a sound. Try a punchier density.');
		expect(nothingFoundLine('punchy', 'ui')).toBe('Nothing in the cut calls for a sound among UI.');
	});

	test('a list is stale once the cut it was found on has moved', () => {
		expect(stampOf(3, '/a.kerf')).toBe(stampOf(3, '/a.kerf'));
		expect(stampOf(3, '/a.kerf')).not.toBe(stampOf(4, '/a.kerf'));
		expect(stampOf(3, '/a.kerf')).not.toBe(stampOf(3, '/b.kerf'));
		expect(stampOf(null, null)).toBe('#');
	});
});
