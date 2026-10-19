import { describe, expect, test } from 'bun:test';
import { drawnMoveTracks, planMove } from './multi-move';
import { trimEdit } from './frames';
import {
	colorDraft,
	fadeDraft,
	maskDraft,
	moveDraft,
	titleMoveDraft,
	titleSizeDraft,
	transformDraft,
	trimDraft
} from './preview-draft';
import type { Clip, StreamKind, TextOverlay, Timeline, Track } from './types';
import { DEFAULT_COLOR, DEFAULT_MASK, DEFAULT_TRANSFORM } from './types';

function clip(id: string, start: number, dur: number, extra: Partial<Clip> = {}): Clip {
	return {
		id,
		asset_id: 'a',
		source_in: 0,
		source_out: dur,
		timeline_start: start,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		transform: { ...DEFAULT_TRANSFORM },
		color: { ...DEFAULT_COLOR },
		...extra
	};
}
function track(id: string, kind: StreamKind, clips: Clip[]): Track {
	return { id, kind, name: id.toUpperCase(), clips };
}
function title(extra: Partial<TextOverlay> = {}): TextOverlay {
	return { id: 't1', text: 'Hello', start: 0, end: 4, pos_x: 0.5, pos_y: 0.8, size: 0.06, color: 'white', bold: false, ...extra };
}

// V1: c1 0-10, c2 12-20   V2: c3 5-15   A1: c4 0-20
function sample(c1: Partial<Clip> = {}): Timeline {
	return {
		tracks: [
			track('v1', 'video', [clip('c1', 0, 10, c1), clip('c2', 12, 8)]),
			track('v2', 'video', [clip('c3', 5, 10)]),
			track('a1', 'audio', [clip('c4', 0, 20)])
		],
		overlays: [title()]
	};
}

describe('a slider drag is drafted as the edit it writes', () => {
	test('a static transform, colour, mask and fade patch the clip where it is', () => {
		const tl = sample();
		const tf = transformDraft(tl, 'c1', { scale: 0.5 });
		expect(tf?.overlays).toEqual([]);
		expect(tf?.clips).toHaveLength(1);
		expect(tf?.clips[0].track_id).toBe('v1');
		expect(tf?.clips[0].clip.transform).toEqual({ ...DEFAULT_TRANSFORM, scale: 0.5 });
		expect(tf?.clips[0].clip.timeline_start).toBe(0);

		expect(colorDraft(tl, 'c3', { gamma: 1.4 })?.clips[0]).toEqual({
			track_id: 'v2',
			clip: { ...clip('c3', 5, 10), color: { ...DEFAULT_COLOR, gamma: 1.4 } }
		});
		expect(maskDraft(tl, 'c1', { feather: 0.4 })?.clips[0].clip.mask).toEqual({ ...DEFAULT_MASK, feather: 0.4 });
		const masked = sample({ mask: { ...DEFAULT_MASK, shape: 'ellipse' } });
		expect(maskDraft(masked, 'c1', { x: 0.2 })?.clips[0].clip.mask).toEqual({ ...DEFAULT_MASK, shape: 'ellipse', x: 0.2 });
		// A fade is held to the clip's length, as `setFade` writes it.
		expect(fadeDraft(tl, 'c2', { fade_in: 30 })?.clips[0].clip).toMatchObject({ fade_in: 8, fade_out: 0 });
	});

	test('a clip stored without a transform or colour is drafted over the defaults the engine reads', () => {
		const bare = { ...clip('c1', 0, 10) } as Partial<Clip>;
		delete bare.transform;
		delete bare.color;
		const tl: Timeline = { tracks: [track('v1', 'video', [bare as Clip])] };
		expect(transformDraft(tl, 'c1', { rotation: 10 })?.clips[0].clip.transform).toEqual({ ...DEFAULT_TRANSFORM, rotation: 10 });
		expect(colorDraft(tl, 'c1', { contrast: 2 })?.clips[0].clip.color).toEqual({ ...DEFAULT_COLOR, contrast: 2 });
	});

	test('a keyed number is written as a key at the playhead, which is not drafted', () => {
		const bundle = sample({ keyframes: [{ time: 0, scale: 1, pos_x: 0, pos_y: 0, rotation: 0, opacity: 1 }] });
		expect(transformDraft(bundle, 'c1', { scale: 0.5 })).toBeNull();
		// Crop is never keyed: it is the static transform whatever the keys.
		expect(transformDraft(bundle, 'c1', { crop_left: 0.1 })?.clips[0].clip.transform?.crop_left).toBe(0.1);
		const channel = sample({ channels: [{ prop: 'rotation', keys: [{ time: 0, value: 0 }, { time: 1, value: 90 }] }] });
		expect(transformDraft(channel, 'c1', { rotation: 45 })).toBeNull();
		expect(transformDraft(channel, 'c1', { scale: 2 })?.clips[0].clip.transform?.scale).toBe(2);
		const graded = sample({ channels: [{ prop: 'brightness', keys: [{ time: 0, value: 0 }] }] });
		expect(colorDraft(graded, 'c1', { brightness: 0.3 })).toBeNull();
		expect(colorDraft(graded, 'c1', { saturation: 2 })).not.toBeNull();
	});

	test('a clip the cut does not hold is no draft', () => {
		expect(transformDraft(sample(), 'nope', { scale: 2 })).toBeNull();
		expect(titleSizeDraft([title()], 'nope', 0.1)).toBeNull();
	});
});

describe('a title drag is drafted as the edit it writes', () => {
	test('a still title takes the position, an animated one a key at the playhead', () => {
		expect(titleMoveDraft([title()], 't1', 0.25, 0.4, 1)).toEqual({ clips: [], overlays: [title({ pos_x: 0.25, pos_y: 0.4 })] });
		const animated = title({ keyframes: [{ time: 0, pos_x: 0.5, pos_y: 0.8, opacity: 1 }] });
		const keyed = titleMoveDraft([animated], 't1', 0.25, 0.4, 2)?.overlays[0];
		expect(keyed?.pos_x).toBe(0.5);
		expect(keyed?.keyframes).toEqual([
			{ time: 0, pos_x: 0.5, pos_y: 0.8, opacity: 1 },
			{ time: 2, pos_x: 0.25, pos_y: 0.4, opacity: 1 }
		]);
		expect(titleSizeDraft([title()], 't1', 0.1)?.overlays[0].size).toBe(0.1);
	});
});

describe('a timeline gesture is drafted where its ghosts are', () => {
	test('a move puts every clip of the group on the track and at the start of its ghost', () => {
		const tl = sample();
		const plan = planMove(drawnMoveTracks(tl), ['c2'], 'c2', 20, 'v2');
		expect(plan.ok).toBe(true);
		expect(moveDraft(tl, plan)).toEqual({ clips: [{ track_id: 'v2', clip: { ...clip('c2', 20, 8) } }], overlays: [] });
	});

	test('a refused drop, a drop that changes nothing and no drag are no draft', () => {
		const tl = sample();
		expect(moveDraft(tl, planMove(drawnMoveTracks(tl), ['c2'], 'c2', 8, 'v1'))).toBeNull();
		expect(moveDraft(tl, planMove(drawnMoveTracks(tl), ['c2'], 'c2', 12, 'v1'))).toBeNull();
		expect(moveDraft(tl, null)).toBeNull();
	});

	test('a trim of a clip alone is the fields the trim writes', () => {
		const tl = sample();
		const c2 = tl.tracks[0].clips[1];
		expect(trimDraft(tl, 'c2', trimEdit(c2, 'l', 14))?.clips[0].clip).toMatchObject({ source_in: 2, source_out: 8, timeline_start: 14 });
		expect(trimDraft(tl, 'c2', trimEdit(c2, 'r', 18))?.clips[0].clip).toMatchObject({ source_in: 0, source_out: 6, timeline_start: 12 });
	});
});
