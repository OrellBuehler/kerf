import { describe, expect, test } from 'bun:test';
import corpus from './fixtures/links-corpus.json';
import * as ops from './link-ops';
import type { Clip, Timeline } from './types';

// The differential test for the browser harness's linked edits. `links.ts` / `ripple.ts` /
// `link-groups.ts` / `link-ops.ts` are a hand port of kerf-core's link-aware ops, and the
// sync lock is too much arithmetic to trust a port of by reading — so kerf-core *writes the
// answers down* (`project/linked_corpus.rs` → `fixtures/links-corpus.json`: a timeline, the
// call it is made under, one op, and what `Project` did: the timeline it left, canonicalized,
// and its revision label — or its exact refusal) and every case is replayed here through the
// port. A rule changed in Rust without the port following fails *this* test, naming the case;
// regenerate the file with `KERF_BLESS_CORPUS=1 cargo test -p kerf-core --no-default-features
// -- links_corpus` after an intended change, and this test says what the port has to follow.

interface Asset {
	id: string;
	duration: number;
	audio: boolean;
}

interface Case {
	name: string;
	ripple: boolean;
	links: boolean;
	before: unknown;
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	op: Record<string, any>;
	expect:
		| { ok: true; label: string; report: Record<string, unknown> | null; timeline: unknown }
		| { ok: false; error: string };
}

const assets = corpus.assets as Asset[];
const cases = corpus.cases as unknown as Case[];

const env = (c: Case): ops.EditEnv => ({
	ripple: c.ripple,
	links: c.links,
	footage: new Map(assets.map((a) => [a.id, a.duration])),
	hasAudio: (id) => assets.find((a) => a.id === id)?.audio ?? false
});

/** What the op returned beyond the timeline, in the shape the corpus records it. */
type Outcome = { timeline: Timeline; label: string; report: Record<string, unknown> | null };

function replay(c: Case): Outcome {
	const tl = structuredClone(c.before) as Timeline;
	const e = env(c);
	const op = c.op;
	const many = (done: { result: { detached: unknown[]; skipped: { reason: string }[] } }) => ({
		detached: done.result.detached.length,
		skipped: done.result.skipped.map((s) => s.reason)
	});
	switch (op.kind) {
		case 'split_at':
			return { ...ops.splitAt(tl, e, op.clip_id, op.at), report: null };
		case 'trim':
			return { ...ops.trim(tl, e, op.clip_id, op.source_in, op.source_out, op.timeline_start), report: null };
		case 'cut_clip_range':
			return { ...ops.cutRange(tl, e, op.clip_id, op.from, op.to), report: null };
		case 'ripple_delete':
			return { ...ops.rippleDelete(tl, e, op.clip_id), report: null };
		case 'remove': {
			const done = ops.remove(tl, e, op.clip_id);
			return { ...done, report: null };
		}
		case 'remove_clips': {
			const done = ops.removeClips(tl, e, op.clip_ids);
			return { ...done, report: { removed: done.result } };
		}
		case 'set_speed':
			return { ...ops.setSpeed(tl, e, op.clip_id, op.speed), report: null };
		case 'split_remove':
			return { ...ops.splitRemoveClips(tl, e, [{ clip_id: op.clip_id, at: op.at }], op.side), report: null };
		case 'split_remove_clips':
			return { ...ops.splitRemoveClips(tl, e, op.cuts, op.side), report: null };
		case 'move_clips':
			return { ...ops.moveClips(tl, e, op.moves), report: null };
		case 'reorder':
			return { ...ops.reorder(tl, e, op.track_id, op.clip_id, op.new_index), report: null };
		case 'detach_audio':
			return { ...ops.detach(tl, e, op.clip_id), report: null };
		case 'detach_audio_clips': {
			const done = ops.detachClips(tl, e, op.clip_ids);
			return { ...done, report: many(done) };
		}
		case 'reattach_audio':
			return { ...ops.reattach(tl, e, op.clip_id), report: null };
		case 'extract_audio': {
			const done = ops.extractAudio(tl, e, op.asset);
			return { ...done, report: many(done) };
		}
		case 'add_asset_audio': {
			const asset = assets.find((a) => a.id === op.asset)!;
			return { ...ops.addAssetAudio(tl, e, asset), report: null };
		}
		case 'link_clips':
			return { ...ops.link(tl, e, op.clip_ids), report: null };
		case 'unlink_clips': {
			const done = ops.unlink(tl, e, op.clip_ids);
			return { ...done, report: { unlinked: done.result } };
		}
		case 'duplicate_clips':
			return { ...ops.duplicateClips(tl, e, op.clip_ids, op.at), report: null };
		default:
			throw new Error(`the corpus has an op this test does not know: ${op.kind}`);
	}
}

/** A timeline as the corpus compares it (`canonical` in `linked_corpus.rs`): tracks in order, clips in
 *  start order, assets by index, link groups by first appearance — no ids. */
function canonical(t: Timeline) {
	const groups = new Map<string, number>();
	return {
		tracks: t.tracks.map((track) => {
			const clips = [...track.clips].sort(
				(a: Clip, b: Clip) => a.timeline_start - b.timeline_start || a.source_in - b.source_in || a.source_out - b.source_out
			);
			return {
				name: track.name,
				kind: track.kind,
				volume: track.volume ?? 1,
				locked: !!track.locked,
				clips: clips.map((c) => {
					let link: string | null = null;
					if (c.link_id) {
						if (!groups.has(c.link_id)) groups.set(c.link_id, groups.size);
						link = `g${groups.get(c.link_id)}`;
					}
					const index = assets.findIndex((a) => a.id === c.asset_id);
					return {
						asset: index < 0 ? null : index,
						start: c.timeline_start,
						in: c.source_in,
						out: c.source_out,
						speed: c.speed ?? 1,
						volume: c.volume,
						fade_in: c.fade_in,
						fade_out: c.fade_out,
						own_sound: c.source_audio !== false,
						link
					};
				})
			};
		})
	};
}

/** Whether two JSON values agree, numbers to a microsecond (the Rust side keeps volumes as f32). */
function agree(a: unknown, b: unknown, path = ''): string | null {
	if (typeof a === 'number' && typeof b === 'number') return Math.abs(a - b) <= 1e-6 ? null : `${path}: ${a} vs ${b}`;
	if (Array.isArray(a) && Array.isArray(b)) {
		if (a.length !== b.length) return `${path}: ${a.length} items vs ${b.length}`;
		for (let i = 0; i < a.length; i++) {
			const bad = agree(a[i], b[i], `${path}[${i}]`);
			if (bad) return bad;
		}
		return null;
	}
	if (a && b && typeof a === 'object' && typeof b === 'object') {
		const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
		for (const k of keys) {
			const bad = agree((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k], `${path}.${k}`);
			if (bad) return bad;
		}
		return null;
	}
	return a === b ? null : `${path}: ${JSON.stringify(a)} vs ${JSON.stringify(b)}`;
}

describe('the harness replays the engine’s own answers (kerf-core → links-corpus.json)', () => {
	test('the corpus is not empty and says something about both outcomes', () => {
		expect(cases.length).toBeGreaterThan(60);
		expect(cases.filter((c) => c.expect.ok).length).toBeGreaterThan(40);
		expect(cases.filter((c) => !c.expect.ok).length).toBeGreaterThan(6);
	});

	for (const c of cases) {
		test(c.name, () => {
			let outcome: Outcome | null = null;
			let error: string | null = null;
			try {
				outcome = replay(c);
			} catch (e) {
				error = (e as Error).message;
			}
			if (!c.expect.ok) {
				expect({ refused: error !== null, error }).toEqual({ refused: true, error: c.expect.error });
				return;
			}
			expect(error).toBeNull();
			const got = outcome!;
			expect(got.label).toBe(c.expect.label);
			if (c.expect.report) expect(agree(got.report, c.expect.report)).toBeNull();
			expect(agree(canonical(got.timeline), c.expect.timeline)).toBeNull();
		});
	}
});
