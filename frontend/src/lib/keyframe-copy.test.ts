import { describe, expect, test } from 'bun:test';
import { animatedProps, keyframeOffer, propsPhrase } from './keyframe-copy';
import type { Clip, Timeline } from './types';

const clip = (id: string, extra: Partial<Clip> = {}): Clip => ({
	id,
	asset_id: 'a',
	source_in: 0,
	source_out: 5,
	timeline_start: 0,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const animated = (id: string): Clip =>
	clip(id, {
		channels: [
			{
				prop: 'scale',
				keys: [
					{ time: 0, value: 1 },
					{ time: 2, value: 1.5 }
				]
			}
		]
	} as Partial<Clip>);
const cut = (...clips: Clip[]): Timeline => ({ tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips }] });

describe('what is animated', () => {
	test('lists the keyed numbers, and nothing for a still clip', () => {
		expect(animatedProps(animated('a'))).toEqual(['scale']);
		expect(animatedProps(clip('b'))).toEqual([]);
	});

	test('reads them out as words', () => {
		expect(propsPhrase([])).toBe('');
		expect(propsPhrase(['scale'])).toBe('scale');
		expect(propsPhrase(['scale', 'opacity'])).toBe('scale and opacity');
		expect(propsPhrase(['scale', 'opacity', 'rotation'])).toBe('scale, opacity and rotation');
	});
});

describe('Copy and Paste keyframes', () => {
	const tl = cut(animated('a'), clip('b'), animated('c'));

	test('copy needs a clip that is animated, and says why not', () => {
		expect(keyframeOffer(tl, null, 'a').copy).toEqual({ disabled: false });
		expect(keyframeOffer(tl, null, 'b').copy).toEqual({ disabled: true, reason: 'This clip has no keyframes to copy' });
		expect(keyframeOffer(tl, null, null).copy).toEqual({ disabled: true, reason: 'Select a clip first' });
	});

	test('paste needs a copy, a source that still exists, and another clip', () => {
		expect(keyframeOffer(tl, null, 'b').paste.reason).toBe('Copy keyframes from an animated clip first');
		expect(keyframeOffer(tl, 'gone', 'b').paste.reason).toBe('The clip they were copied from is gone — copy again');
		expect(keyframeOffer(tl, 'a', 'a').paste.reason).toBe('Select another clip to paste them onto');
		expect(keyframeOffer(tl, 'a', 'b').paste).toEqual({ disabled: false });
		expect(keyframeOffer(tl, 'a', null).paste.reason).toBe('Select a clip first');
	});

	test('a source whose animation was cleared since is told so', () => {
		expect(keyframeOffer(cut(clip('a'), clip('b')), 'a', 'b').paste.reason).toBe('The clip they were copied from has no keyframes any more');
	});
});
