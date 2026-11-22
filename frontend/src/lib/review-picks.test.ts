import { describe, expect, test } from 'bun:test';
import { applyLabel, applyTip, comingAlong, groupState, keepPresent, pickLine, pickRows, subsetTone, toggled, toggledGroup } from './review-picks';
import type { Clip, DiffEntry, Timeline } from './types';

const clip = (id: string, extra: Partial<Clip> = {}): Clip => ({
	id,
	asset_id: 'a',
	source_in: 0,
	source_out: 10,
	timeline_start: 0,
	volume: 1,
	fade_in: 0,
	fade_out: 0,
	...extra
});
const cut = (v: Clip[], a: Clip[]): Timeline => ({
	tracks: [
		{ id: 'v1', kind: 'video', name: 'V1', clips: v },
		{ id: 'a1', kind: 'audio', name: 'A1', clips: a }
	]
});
const entry = (kind: DiffEntry['kind'], extra: Partial<DiffEntry> = {}): DiffEntry => ({ kind, summary: kind, ...extra });

describe('rows', () => {
	test('a change is named as the backend names it; the library changes follow', () => {
		const rows = pickRows(
			[entry('clip_retrimmed', { clip_id: 'c1' }), entry('overlay_added', { overlay_id: 't1', id: 'overlay_added:t1' })],
			[{ id: 'asset_projection:x', asset_id: 'x', projection: 'equirect', summary: 'Mark x as 360' }]
		);
		expect(rows.map((r) => r.id)).toEqual(['clip_retrimmed:c1', 'overlay_added:t1', 'asset_projection:x']);
		expect(rows[2].entry).toBeNull();
	});

	test('a tag change is a library row of its own: its summary is the line, never "mark as flat"', () => {
		const rows = pickRows(
			[],
			[
				{ id: 'asset_tags:x', asset_id: 'x', projection: null, tag_edit: { add: [{ key: 'location', value: 'Jeri' }] }, summary: 'Tag x.mp4: +location: Jeri' },
				{ id: 'asset_projection:x', asset_id: 'x', projection: null, summary: 'Mark x.mp4 as flat (not 360)' }
			]
		);
		expect(rows.map((r) => [r.id, r.summary])).toEqual([
			['asset_tags:x', 'Tag x.mp4: +location: Jeri'],
			['asset_projection:x', 'Mark x.mp4 as flat (not 360)']
		]);
		expect(rows.every((r) => r.entry === null)).toBe(true);
	});
});

describe('picking', () => {
	test('toggles one without touching the set it was given', () => {
		const base = new Set(['a']);
		const next = toggled(base, 'b');
		expect([...next]).toEqual(['a', 'b']);
		expect([...base]).toEqual(['a']);
		expect([...toggled(next, 'a')]).toEqual(['b']);
	});

	test('a group is none, some or all, and its box picks the lot or clears it', () => {
		const ids = ['x', 'y', 'z'];
		expect(groupState(ids, new Set())).toBe('none');
		expect(groupState(ids, new Set(['x']))).toBe('some');
		expect(groupState(ids, new Set(ids))).toBe('all');
		expect([...toggledGroup(new Set(['x']), ids)].sort()).toEqual(['x', 'y', 'z']);
		expect([...toggledGroup(new Set(ids), ids)]).toEqual([]);
		expect([...toggledGroup(new Set(['q']), ids)].sort()).toEqual(['q', 'x', 'y', 'z']);
	});

	test('a pick for a change that has gone is dropped, and an unchanged set is the same set', () => {
		const rows = [{ id: 'a' }, { id: 'b' }];
		const picks = new Set(['a', 'gone']);
		expect([...keepPresent(picks, rows)]).toEqual(['a']);
		const same = new Set(['a']);
		expect(keepPresent(same, rows)).toBe(same);
	});
});

describe('what comes along', () => {
	const picture = clip('pic', { link_id: 'g1' });
	const sound = clip('snd', { link_id: 'g1' });
	const live = cut([picture], [sound]);
	const entries = [entry('clip_retrimmed', { clip_id: 'pic' }), entry('clip_retrimmed', { clip_id: 'snd' }), entry('clip_retrimmed', { clip_id: 'other' })];

	test('nothing picked, nothing comes', () => {
		expect(comingAlong(entries, new Set(), { live }).size).toBe(0);
	});

	test('the sound of a picked picture comes with it, and nothing unrelated does', () => {
		const along = comingAlong(entries, new Set(['clip_retrimmed:pic']), { live });
		expect([...along.keys()]).toEqual(['clip_retrimmed:snd']);
		expect(along.get('clip_retrimmed:snd')).toContain('picture and its sound');
	});

	test('a link only the proposal knows is found too', () => {
		const bare = cut([clip('pic')], [clip('snd')]);
		const proposed = cut([picture], [sound]);
		expect([...comingAlong(entries, new Set(['clip_retrimmed:snd']), { live: bare, proposed }).keys()]).toEqual(['clip_retrimmed:pic']);
	});

	test('a link only the base knew is found too, as the backend finds it', () => {
		const bare = cut([clip('pic')], [clip('snd')]);
		expect([...comingAlong(entries, new Set(['clip_retrimmed:pic']), { live: bare, base: live, proposed: bare }).keys()]).toEqual([
			'clip_retrimmed:snd'
		]);
	});

	test('the new track a picked clip lands on comes along', () => {
		const rows = [entry('clip_added', { clip_id: 'n', track_id: 't9' }), entry('track_added', { track_id: 't9' })];
		const along = comingAlong(rows, new Set(['clip_added:n']), { live });
		expect([...along.keys()]).toEqual(['track_added:t9']);
	});

	test('the clips that leave a track picked for removal come with the removal', () => {
		const base = cut([clip('x'), clip('y')], [clip('z')]);
		const rows = [
			entry('track_removed', { track_id: 'v1' }),
			entry('clip_moved', { clip_id: 'x', track_id: 'a1' }),
			entry('clip_moved', { clip_id: 'z', track_id: 'a1' })
		];
		const along = comingAlong(rows, new Set(['track_removed:v1']), { live: base });
		expect([...along.keys()]).toEqual(['clip_moved:x']);
		expect(along.get('clip_moved:x')).toContain('lost with the track');
	});

	test('what the closure adds brings its own along', () => {
		const rows = [entry('clip_added', { clip_id: 'p2', track_id: 't9' }), entry('clip_added', { clip_id: 's2', track_id: 't9' }), entry('track_added', { track_id: 't9' })];
		const proposed = cut([clip('p2', { link_id: 'g2' })], [clip('s2', { link_id: 'g2' })]);
		const along = comingAlong(rows, new Set(['clip_added:p2']), { live, proposed });
		expect([...along.keys()].sort()).toEqual(['clip_added:s2', 'track_added:t9']);
	});
});

describe('the words', () => {
	test('Apply says how many when only some are picked', () => {
		expect(applyLabel(0, 5, false)).toBe('Apply');
		expect(applyLabel(5, 5, false)).toBe('Apply');
		expect(applyLabel(2, 5, false)).toBe('Apply 2 of 5');
		expect(applyLabel(2, 5, true)).toBe('Applying…');
	});

	test('the line over the list', () => {
		expect(pickLine(0, 0, 5)).toBe('Nothing changes until you apply. Tick changes to apply only those.');
		expect(pickLine(2, 0, 5)).toBe('2 of 5 picked · 3 stay in the proposal');
		expect(pickLine(2, 1, 5)).toBe('2 of 5 picked · 1 more comes along · 2 stay in the proposal');
		expect(pickLine(2, 3, 9)).toBe('2 of 9 picked · 3 more come along · 4 stay in the proposal');
		expect(pickLine(4, 0, 5)).toBe('4 of 5 picked · 1 stays in the proposal');
		// short enough for one line in a short panel
		expect(pickLine(0, 0, 5, true)).toBe('Nothing changes until you apply · tick to pick');
		expect(pickLine(2, 0, 5, true)).toBe(pickLine(2, 0, 5));
		// everything is covered: nothing is left to say about the rest
		expect(pickLine(2, 3, 5)).toBe('2 of 5 picked · 3 more come along');
		expect(pickLine(5, 0, 5)).toBe('5 of 5 picked');
	});

	test('the Apply tooltip says what happens to the rest before the press', () => {
		expect(applyTip(0, 0, 4)).toBe('Apply all 4 changes to your timeline as one edit. Undo takes them back.');
		expect(applyTip(0, 0, 1)).toBe('Apply the change to your timeline as one edit. Undo takes them back.');
		expect(applyTip(2, 0, 4)).toBe('Apply the 2 you picked as one edit. The other 2 stay here for you to review. Undo takes them back.');
		expect(applyTip(1, 1, 4)).toBe('Apply the 1 you picked and the 1 that comes along as one edit. The other 2 stay here for you to review. Undo takes them back.');
		expect(applyTip(3, 0, 4)).toContain('The other 1 stays here');
		// everything picked, or coming along, is the whole proposal
		expect(applyTip(2, 2, 4)).toBe('Apply all 4 changes to your timeline as one edit. Undo takes them back.');
	});

	test('a pick that no longer applied makes the toast a warning', () => {
		expect(subsetTone({ skipped: [] })).toBe('success');
		expect(subsetTone({ skipped: [{}] })).toBe('warning');
	});
});
