import { describe, expect, test } from 'bun:test';
import { markerPatch, markerTip, MAX_MARKER_COMMENT } from './marker-edit';
import type { Marker } from './types';

const m: Marker = { id: 'm1', time: 4, name: 'Intro' };
const draft = { name: 'Intro', kind: 'comment' as const, durationText: '0', comment: '' };

describe('marker edits', () => {
	test('only what changed is sent', () => {
		expect(markerPatch(m, draft)).toEqual({ patch: {} });
		expect(markerPatch(m, { ...draft, kind: 'chapter', durationText: '2,5', comment: 'note\n' })).toEqual({
			patch: { kind: 'chapter', duration: 2.5, comment: 'note\n' }
		});
		const chapter: Marker = { ...m, kind: 'chapter', duration: 3, comment: 'x' };
		expect(markerPatch(chapter, { ...draft, kind: 'chapter', durationText: '3', comment: 'x' })).toEqual({ patch: {} });
		expect(markerPatch(chapter, { ...draft, durationText: '' })).toEqual({ patch: { kind: 'comment', duration: 0, comment: '' } });
	});

	test('a bad length, an empty name or an overlong comment is refused', () => {
		expect('error' in markerPatch(m, { ...draft, durationText: '-1' })).toBe(true);
		expect('error' in markerPatch(m, { ...draft, durationText: 'abc' })).toBe(true);
		expect('error' in markerPatch(m, { ...draft, name: '  ' })).toBe(true);
		expect('error' in markerPatch(m, { ...draft, comment: 'x'.repeat(MAX_MARKER_COMMENT + 1) })).toBe(true);
	});

	test('the tooltip names the kind, the span and the comment', () => {
		expect(markerTip(m, '0:04')).toBe('Intro — 0:04');
		expect(markerTip({ ...m, kind: 'chapter', duration: 2, comment: 'why' }, '0:04')).toBe('Intro (chapter) — 0:04 for 2s\nwhy');
	});
});
