import { describe, expect, test } from 'bun:test';
import { readFileSync } from 'node:fs';
import corpus from './fixtures/motion-presets-corpus.json';
import {
	MOTION_KINDS,
	MOTION_PRESET_IDS,
	MOTION_SPECS,
	applyMotion,
	applyMotionPreset,
	motionLabel,
	motionPresets,
	motionWindow,
	motionSpec,
	wasClamped
} from './motion-presets';
import { propertyKeys } from './channels';
import { DEFAULT_TRANSFORM } from './types';
import type { Clip, MotionMode, MotionPreset, PropertyTrack, Timeline } from './types';

// The differential test for the browser harness's motion presets. `motion-presets.ts` is a hand port of kerf-core's
// `model/motion.rs` (the `SPECS` table, the window rules, `Clip::apply_motion`) and `project/motion.rs`; kerf-core
// writes the table down and 89 input -> channels cases (`fixtures/motion-presets-corpus.json`, regenerated with
// `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features -- motion_corpus`) and every one is replayed
// here: the table key by key, every case's window and keys, and the seven refusals' exact words. A rule changed in
// Rust without the port following fails this test, naming the case.

interface Op {
	preset: MotionPreset;
	mode: MotionMode;
	at: number | null;
	duration: number | null;
	intensity: number | null;
}

interface Case {
	name: string;
	clip: {
		duration: number;
		transform: Omit<typeof DEFAULT_TRANSFORM, 'crop_left' | 'crop_right' | 'crop_top' | 'crop_bottom'>;
		keyframes: unknown[];
		channels: PropertyTrack[];
	};
	ops: Op[];
	expect: { ok: { window: [number, number]; keyframes: unknown[]; channels: PropertyTrack[] } } | { err: string };
}

interface TableRow {
	id: MotionPreset;
	kind: string;
	span: number | null;
	base: string;
	tracks: { prop: string; keys: { at: number; x: number; easing: unknown }[] }[];
}

const { table, cases } = corpus as unknown as { table: TableRow[]; cases: Case[] };

const clipOf = (c: Case['clip']) =>
	({
		id: 'c',
		asset_id: 'a',
		source_in: 0,
		source_out: c.duration,
		timeline_start: 0,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		transform: { ...DEFAULT_TRANSFORM, ...c.transform },
		keyframes: structuredClone(c.keyframes),
		channels: structuredClone(c.channels)
	}) as unknown as Clip;

describe('the table', () => {
	test('has every row of the corpus, key by key', () => {
		expect(MOTION_SPECS.length).toBe(table.length);
		for (const row of table) {
			const s = motionSpec(row.id);
			expect({ id: s.id, kind: s.kind, span: s.span, base: s.base }, row.id).toEqual({
				id: row.id,
				kind: row.kind as never,
				span: row.span,
				base: row.base as never
			});
			expect(s.tracks.length, row.id).toBe(row.tracks.length);
			row.tracks.forEach((t, i) => {
				expect(s.tracks[i].prop, row.id).toBe(t.prop as never);
				expect(s.tracks[i].keys, `${row.id} ${t.prop}`).toEqual(t.keys as never);
			});
		}
		expect(MOTION_PRESET_IDS).toEqual(table.map((r) => r.id));
	});

	test('labels and hints are the ones kerf-core lists', () => {
		const rust = readFileSync(new URL('../../../crates/kerf-core/src/model/motion.rs', import.meta.url), 'utf8');
		const rows = [...rust.matchAll(/\n    spec\(\s*"([a-z_]+)",\s*"([^"]+)",\s*"((?:[^"\\]|\\.)+)",/g)];
		expect(rows.length).toBe(MOTION_SPECS.length);
		for (const [, id, label, hint] of rows) {
			const s = motionSpec(id as MotionPreset);
			expect({ label: s.label, hint: s.hint }, id).toEqual({ label, hint: hint.replace(/\\'/g, "'") });
		}
	});

	test('lists entrances, exits and emphases as a picker needs them', () => {
		const list = motionPresets();
		expect(list.length).toBe(23);
		expect(MOTION_KINDS.map((k) => list.filter((p) => p.kind === k).length)).toEqual([9, 9, 5]);
		expect(list[6]).toEqual({
			id: 'pop_in',
			label: 'Pop in',
			kind: 'entrance',
			default_duration: 0.5,
			hint: 'Springs up from nothing, overshoots a little and settles.',
			properties: ['scale', 'opacity']
		});
		expect(list[21].id).toBe('push_in');
		expect('default_duration' in list[21]).toBe(false);
		expect(list.map((p) => p.kind)).toEqual(
			[...list].sort((a, b) => MOTION_KINDS.indexOf(a.kind) - MOTION_KINDS.indexOf(b.kind)).map((p) => p.kind)
		);
	});
});

describe('the corpus', () => {
	test('is the size kerf-core wrote, refusals included', () => {
		expect(cases.length).toBe(89);
		expect(cases.filter((c) => 'err' in c.expect).length).toBe(7);
	});

	for (const c of cases) {
		test(c.name, () => {
			const clip = clipOf(c.clip);
			const before = structuredClone(clip);
			let window: [number, number] | undefined;
			let refusal: string | undefined;
			try {
				for (const o of c.ops) window = applyMotion(clip, c.clip.duration, o.preset, o.mode, o.at, o.duration, o.intensity);
			} catch (e) {
				refusal = (e as Error).message;
			}
			if ('err' in c.expect) {
				expect(refusal).toBe(c.expect.err);
				expect(clip, 'a refusal leaves the clip as it was').toEqual(before);
				return;
			}
			expect(refusal).toBeUndefined();
			const { ok } = c.expect;
			// Bit for bit: the split beziers' arithmetic is the same as Rust's.
			expect(window).toEqual(ok.window);
			expect(clip.keyframes).toEqual(ok.keyframes as never);
			expect(clip.channels ?? []).toEqual(ok.channels);
		});
	}
});

describe('the window', () => {
	const entrance = { kind: 'entrance', span: 0.5 } as const;
	const exit = { kind: 'exit', span: 0.6 } as const;
	const fill = { kind: 'emphasis', span: null } as const;

	test('an entrance starts at 0, an exit ends at the clip, a preset with no span fills the rest', () => {
		expect(motionWindow(entrance, 4)).toEqual([0, 0.5]);
		expect(motionWindow(exit, 4)).toEqual([3.4, 4]);
		expect(motionWindow(fill, 4)).toEqual([0, 4]);
		expect(motionWindow(fill, 4, 1)).toEqual([1, 4]);
		expect(motionWindow(exit, 4, 1)).toEqual([1, 1.6]);
	});

	test('a duration longer than the clip is clamped to what is left, not refused', () => {
		expect(motionWindow(entrance, 0.3)).toEqual([0, 0.3]);
		expect(motionWindow(exit, 0.3)).toEqual([0, 0.3]);
		expect(motionWindow(entrance, 2, 1.5, 9)).toEqual([1.5, 2]);
	});

	test('says when the clip was in the way', () => {
		expect(wasClamped('fade_in', { start: 0, end: 0.3 })).toBe(true);
		expect(wasClamped('fade_in', { start: 0, end: 0.5 })).toBe(false);
		expect(wasClamped('fade_in', { start: 0, end: 2 }, 2)).toBe(false);
		expect(wasClamped('fade_in', { start: 0, end: 1 }, 2)).toBe(true);
		expect(wasClamped('push_in', { start: 0, end: 1 })).toBe(false);
	});
});

describe('on a timeline (Project::apply_motion_preset)', () => {
	const clip = (id: string, start: number, seconds: number): Clip =>
		({
			id,
			asset_id: 'a',
			source_in: 0,
			source_out: seconds,
			timeline_start: start,
			volume: 1,
			fade_in: 0,
			fade_out: 0,
			transform: { ...DEFAULT_TRANSFORM }
		}) as Clip;

	const shots = (): Timeline => ({
		tracks: [
			{ id: 'v1', kind: 'video', name: 'V1', clips: [clip('c1', 0, 10), clip('c2', 10, 6)] },
			{ id: 'a1', kind: 'audio', name: 'A1', clips: [clip('s1', 0, 10)] }
		]
	});
	const opacity = (tl: Timeline, id: string) =>
		propertyKeys(tl.tracks.flatMap((t) => t.clips).find((c) => c.id === id)!, 'opacity').map((k) => [k.time, k.value]);

	test('is one edit for every clip, named by the preset and the count', () => {
		const tl = shots();
		const done = applyMotionPreset(tl, ['c1', 'c2'], 'fade_in');
		expect(done.label).toBe('Apply motion preset Fade in (2 clips)');
		expect(done.result.map((m) => [m.start, m.end])).toEqual([
			[0, 0.5],
			[0, 0.5]
		]);
		expect(opacity(done.timeline, 'c1')).toEqual([
			[0, 0],
			[0.5, 1]
		]);
		expect(opacity(tl, 'c1'), 'the timeline given is untouched').toEqual([]);
		expect(applyMotionPreset(tl, ['c2'], 'slide_out_to_left', 'add', null, 1).label).toBe(
			'Apply motion preset Slide out to left'
		);
		expect(motionLabel('pop_in', 3)).toBe('Apply motion preset Pop in (3 clips)');
	});

	test('names a clip twice once and clamps the window to each clip', () => {
		const tl = shots();
		const once = applyMotionPreset(tl, ['c1', 'c1'], 'fade_out', 'add', null, 8);
		expect(once.result.length).toBe(1);
		expect(once.label).toBe('Apply motion preset Fade out');
		expect([once.result[0].start, once.result[0].end]).toEqual([2, 10]);
		const both = applyMotionPreset(tl, ['c1', 'c2'], 'fade_in', 'add', null, 8);
		expect(both.result.map((m) => m.end)).toEqual([8, 6]);
	});

	test('refuses whole: an unknown clip, nothing named, a bad number, a sound', () => {
		const tl = shots();
		expect(() => applyMotionPreset(tl, ['c1', 'ghost'], 'fade_in')).toThrow('clip not found: ghost');
		expect(() => applyMotionPreset(tl, [], 'fade_in')).toThrow('name at least one clip');
		expect(() => applyMotionPreset(tl, ['c1'], 'pulse', 'add', null, 0)).toThrow('invalid argument: duration must be');
		expect(() => applyMotionPreset(tl, ['c1'], 'pulse', 'add', null, null, 5)).toThrow('intensity must be within');
		expect(() => applyMotionPreset(tl, ['c2', 's1'], 'fade_in')).toThrow(
			'invalid argument: clip s1 is on audio track A1: a sound has no picture to move'
		);
		expect(opacity(tl, 'c2')).toEqual([]);
	});

	test('a lock guards the layout, not the look', () => {
		const tl = shots();
		tl.tracks[0].locked = true;
		const done = applyMotionPreset(tl, ['c1'], 'fade_in');
		expect(done.timeline.tracks[0].locked).toBe(true);
		expect(opacity(done.timeline, 'c1')).toEqual([
			[0, 0],
			[0.5, 1]
		]);
	});

	test('an exit merges into the entrance before it, and applying a preset twice changes nothing', () => {
		const first = applyMotionPreset(shots(), ['c1'], 'fade_in').timeline;
		const both = applyMotionPreset(first, ['c1'], 'fade_out').timeline;
		expect(opacity(both, 'c1')).toEqual([
			[0, 0],
			[0.5, 1],
			[9.5, 1],
			[10, 0]
		]);
		const again = applyMotionPreset(both, ['c1'], 'fade_out').timeline;
		expect(again.tracks[0].clips[0].channels).toEqual(both.tracks[0].clips[0].channels);
	});

	test('refuses a track a preset would push past the key limit', () => {
		const tl = shots();
		tl.tracks[0].clips[0].channels = [
			{ prop: 'opacity', keys: Array.from({ length: 1000 }, (_, i) => ({ time: 5 + i * 0.004, value: 1 })) }
		];
		expect(() => applyMotionPreset(tl, ['c1'], 'fade_in')).toThrow('at most 1000 keyframes');
		const replaced = applyMotionPreset(tl, ['c1'], 'fade_in', 'replace').timeline;
		expect(opacity(replaced, 'c1')).toEqual([
			[0, 0],
			[0.5, 1]
		]);
	});
});
