import { describe, expect, test } from 'bun:test';
import { diffHeadline, formatTime, groupEntries, polarity } from './diff';
import type { DiffEntry, TimelineDiff } from './types';

const entry = (kind: DiffEntry['kind']): DiffEntry => ({ kind, summary: kind });

function diff(entries: DiffEntry[], before = 10, after = 10, clipsBefore = 3, clipsAfter = 3): TimelineDiff {
	return {
		entries,
		duration_before: before,
		duration_after: after,
		clips_before: clipsBefore,
		clips_after: clipsAfter
	};
}

describe('formatTime', () => {
	test('reads as m:ss.d', () => {
		expect(formatTime(0)).toBe('0:00.0');
		expect(formatTime(4)).toBe('0:04.0');
		expect(formatTime(72)).toBe('1:12.0');
		expect(formatTime(-3)).toBe('0:00.0');
	});
});

describe('diffHeadline', () => {
	test('an empty diff says so', () => {
		expect(diffHeadline(diff([]))).toBe('No changes');
	});

	test('leads with what the edit did to the runtime', () => {
		const d = diff([entry('clip_removed'), entry('clip_moved'), entry('clip_retrimmed')], 9, 8, 3, 2);
		expect(diffHeadline(d)).toBe('3 changes · 0:09.0 → 0:08.0 (-1.0s) · 3 → 2 clips');
	});

	test('a same-length change reports the runtime once', () => {
		expect(diffHeadline(diff([entry('clip_changed')]))).toBe('1 change · 0:10.0');
	});
});

describe('polarity', () => {
	test('tints by what the entry does', () => {
		expect(polarity('clip_added')).toBe('added');
		expect(polarity('track_removed')).toBe('removed');
		expect(polarity('clip_retrimmed')).toBe('changed');
		expect(polarity('format_changed')).toBe('changed');
	});
});

describe('groupEntries', () => {
	test('buckets by what the entry touches and drops empty groups', () => {
		const groups = groupEntries([entry('clip_added'), entry('overlay_added'), entry('clip_moved')]);
		expect(groups.map((g) => g.label)).toEqual(['Clips', 'Text']);
		expect(groups[0].entries).toHaveLength(2);
	});
});
