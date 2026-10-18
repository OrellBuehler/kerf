import { describe, expect, test } from 'bun:test';
import { badgeLabel, badgeTip, changeWord, discardNeedsConfirm, proposalLine, reviewTarget, touchedOnLive } from './proposal-ui';
import { discardProposalCopy } from './confirm-copy';
import type { DiffEntry, Timeline } from './types';

const clip = (id: string) => ({ id, asset_id: 'a', source_in: 0, source_out: 5, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 });
const cut = (...ids: string[]): Timeline => ({ tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: ids.map(clip) }] });
const withTitle = (tl: Timeline, id: string): Timeline => ({
	...tl,
	overlays: [{ id, text: 't', start: 0, end: 1, pos_x: 0.5, pos_y: 0.5, size: 0.1, color: 'white', bold: false }]
});

describe('when a discard asks first', () => {
	test('more than three changes ask; a small proposal is one click', () => {
		expect(discardNeedsConfirm(0)).toBe(false);
		expect(discardNeedsConfirm(3)).toBe(false);
		expect(discardNeedsConfirm(4)).toBe(true);
		expect(discardNeedsConfirm(40)).toBe(true);
	});

	test('the question says how many and names both ends as verbs', () => {
		const copy = discardProposalCopy(4);
		expect(copy.message).toContain('4 proposed changes');
		expect(copy.ok).toBe('Discard');
		expect(copy.cancel).toBe('Keep reviewing');
		expect(discardProposalCopy(1).message).toContain('1 proposed change will');
	});
});

describe('naming a proposal', () => {
	test('counts are plural only when they should be', () => {
		expect(changeWord(1)).toBe('1 change');
		expect(changeWord(4)).toBe('4 changes');
		expect(proposalLine(4)).toBe('Agent proposes 4 changes');
		expect(proposalLine(1)).toBe('Agent proposes 1 change');
	});

	test('the badge says who the changes are from, and its tooltip says where to review them', () => {
		expect(badgeLabel(4)).toBe('4 agent changes');
		expect(badgeLabel(1)).toBe('1 agent change');
		expect(badgeTip(4)).toBe('The agent proposes 4 changes — open the Agent panel to review them');
	});
});

describe('what a proposal touches on the live cut', () => {
	const entries: DiffEntry[] = [
		{ kind: 'clip_retrimmed', summary: '', clip_id: 'c1', at: 0 },
		{ kind: 'clip_removed', summary: '', clip_id: 'c2', at: 5 },
		{ kind: 'clip_added', summary: '', clip_id: 'new', at: 9 },
		{ kind: 'overlay_added', summary: '', overlay_id: 'ov-new', at: 1 },
		{ kind: 'overlay_changed', summary: '', overlay_id: 'ov', at: 2 }
	];

	test('marks the moved, trimmed and removed clips and titles — not what only the proposal has', () => {
		const t = touchedOnLive(entries, withTitle(cut('c1', 'c2', 'c3'), 'ov'));
		expect([...t.clips].sort()).toEqual(['c1', 'c2']);
		expect([...t.titles]).toEqual(['ov']);
	});

	test('nothing on an empty proposal', () => {
		const t = touchedOnLive([], cut('c1'));
		expect(t.clips.size + t.titles.size).toBe(0);
	});
});

describe('clicking a row of the proposal', () => {
	const live = withTitle(cut('c1', 'c2'), 'ov');
	const proposal = cut('c1', 'new');

	test('a clip the live cut has is selected where it is, without opening the preview', () => {
		expect(reviewTarget({ at: 3, clip_id: 'c1' }, { previewing: false, live, shown: live })).toEqual({
			seek: 3,
			preview: false,
			target: { kind: 'clip', id: 'c1' }
		});
	});

	test('a clip only the proposal has opens the preview, then selects it there', () => {
		expect(reviewTarget({ at: 9, clip_id: 'new' }, { previewing: false, live, shown: live })).toEqual({
			seek: 9,
			preview: true,
			target: { kind: 'clip', id: 'new' }
		});
	});

	test('already previewing: no preview to open, and a removal (not on the proposed cut) is not selected', () => {
		const ctx = { previewing: true, live, shown: proposal };
		expect(reviewTarget({ at: 9, clip_id: 'new' }, ctx)).toEqual({ seek: 9, preview: false, target: { kind: 'clip', id: 'new' } });
		expect(reviewTarget({ at: 5, clip_id: 'c2' }, ctx)).toEqual({ seek: 5, preview: false, target: null });
	});

	test('titles go the same way', () => {
		expect(reviewTarget({ at: 1, overlay_id: 'ov' }, { previewing: false, live, shown: live }).target).toEqual({ kind: 'title', id: 'ov' });
		expect(reviewTarget({ at: 1, overlay_id: 'ov-new' }, { previewing: false, live, shown: live }).preview).toBe(true);
	});

	test('a row with no place and no subject goes nowhere', () => {
		expect(reviewTarget({}, { previewing: false, live, shown: live })).toEqual({ seek: null, preview: false, target: null });
		expect(reviewTarget({ at: null, clip_id: 'c1' }, { previewing: false, live, shown: live }).seek).toBeNull();
	});
});
