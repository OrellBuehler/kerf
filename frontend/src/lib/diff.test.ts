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

	// Rust's `{:04.1}` rounds the exact binary value, and a value that is exactly a tie
	// goes to the even digit; `toFixed` takes the larger one. The same cases are pinned
	// on the Rust side (`times_and_deltas_round_half_to_even_on_the_exact_value`).
	test('rounds an exact tie to the even tenth, as the backend prints it', () => {
		expect(formatTime(4.25)).toBe('0:04.2');
		expect(formatTime(4.75)).toBe('0:04.8');
		expect(formatTime(0.25)).toBe('0:00.2');
		expect(formatTime(72.25)).toBe('1:12.2');
		expect(formatTime(0.75)).toBe('0:00.8');
		expect(formatTime(2.5)).toBe('0:02.5'); // not a tie at one decimal
	});

	test('anything that is not an exact tie rounds by its exact value', () => {
		expect(formatTime(0.35)).toBe('0:00.3'); // 0.35 is a hair under a tie
		expect(formatTime(0.45)).toBe('0:00.5'); // …and 0.45 a hair over one
		expect(formatTime(0.05)).toBe('0:00.1');
		expect(formatTime(59.96)).toBe('0:60.0'); // the backend's own carry quirk, mirrored
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

	test('the runtime delta rounds as the backend does', () => {
		// 8.75 → 8.5 is -0.25 s: an exact tie, so "-0.2s" like `fmt_delta`, not "-0.3s".
		expect(diffHeadline(diff([entry('clip_removed')], 8.75, 8.5, 3, 2))).toBe('1 change · 0:08.8 → 0:08.5 (-0.2s) · 3 → 2 clips');
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
		expect(polarity('master_changed')).toBe('changed');
	});
});

describe('groupEntries', () => {
	test('buckets by what the entry touches and drops empty groups', () => {
		const groups = groupEntries([entry('clip_added'), entry('overlay_added'), entry('clip_moved')]);
		expect(groups.map((g) => g.label)).toEqual(['Clips', 'Text']);
		expect(groups[0].entries).toHaveLength(2);
	});

	test('a master-bus change is the mix, not the delivery frame', () => {
		const groups = groupEntries([entry('format_changed'), entry('master_changed'), entry('track_changed')]);
		expect(groups.map((g) => g.label)).toEqual(['Tracks', 'Mix', 'Delivery']);
	});
});
