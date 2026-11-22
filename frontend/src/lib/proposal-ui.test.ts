import { describe, expect, test } from 'bun:test';
import {
	arrivalNotice,
	badgeTip,
	changeLine,
	changeWord,
	discardNeedsConfirm,
	headlineParts,
	placeStep,
	placesNote,
	proposalLine,
	respellTimes,
	reviewStepTip,
	reviewTarget,
	staleWords,
	touchedOnLive
} from './proposal-ui';
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

	test('the chip’s tooltip says who the changes are from, and where to review them', () => {
		expect(badgeTip(1)).toBe('The agent proposes 1 change — open the Agent panel to review them');
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

describe('the banner counter’s tooltip', () => {
	test('names the keys the user has bound, not the ones it shipped with', () => {
		expect(reviewStepTip(3, 4, ['PgUp', '['], [`PgDn`, ']'])).toBe('The proposal changes the cut at 3 places; 4 changes in all. PgUp and PgDn step between them.');
		expect(reviewStepTip(1, 1, ['F5'], ['F6'])).toBe('The proposal changes the cut at 1 place; 1 change in all. F5 and F6 step between them.');
	});

	test('says so when only one of them is bound, and nothing about keys when none is', () => {
		expect(reviewStepTip(2, 2, ['F5'], [])).toContain('F5 steps back between them.');
		expect(reviewStepTip(2, 2, [], ['F6'])).toContain('F6 steps forward between them.');
		expect(reviewStepTip(2, 2, [], [])).toBe('The proposal changes the cut at 2 places; 2 changes in all.');
	});
});

describe('a change reads in the editor’s times', () => {
	test('the backend’s m:ss.d becomes the timecode the ruler and the transport print', () => {
		expect(respellTimes('Trimmed clip on V1 at 0:12.5 — 12.5s → 9.0s (-3.5s)', 30, 'timecode')).toBe('Trimmed clip on V1 at 00:00:12:15 — 12.5s → 9.0s (-3.5s)');
		expect(respellTimes('Moved clip on V1 — 0:12.5 → 0:09.0', 30, 'timecode')).toBe('Moved clip on V1 — 00:00:12:15 → 00:00:09:00');
		expect(respellTimes('Added text at 0:01.0–0:04.0', 25, 'timecode')).toBe('Added text at 00:00:01:00–00:00:04:00');
	});

	test('past a minute and an hour', () => {
		expect(respellTimes('at 1:05.0', 30, 'timecode')).toBe('at 00:01:05:00');
		expect(respellTimes('at 75:03.0', 30, 'timecode')).toBe('at 01:15:03:00');
	});

	test('the seconds mode is what the backend wrote', () => {
		const s = 'Moved clip on V1 — 0:12.5 → 0:09.0';
		expect(respellTimes(s, 30, 'seconds')).toBe(s);
	});

	test('what is between curly quotes is the user’s own words and is left alone', () => {
		expect(respellTimes('Added text “meet at 3:45.0” at 0:01.0–0:04.0', 30, 'timecode')).toBe('Added text “meet at 3:45.0” at 00:00:01:00–00:00:04:00');
		expect(respellTimes('text “a 0:01.0” → “b 0:02.0”', 30, 'timecode')).toBe('text “a 0:01.0” → “b 0:02.0”');
	});

	test('only a whole m:ss.d is a time: a ratio, a duration, a decimal and a longer clock are not', () => {
		for (const s of ['ratio 16:9', 'speed 1.25× → 2.00×', 'trimmed 12.5s → 9.0s', 'level 80% → 100%', 'version 1.0:2.50']) expect(respellTimes(s, 30, 'timecode')).toBe(s);
		expect(respellTimes('at 0:12.5.3', 30, 'timecode')).toBe('at 0:12.5.3');
		expect(respellTimes('at 10:00:12.5', 30, 'timecode')).toBe('at 10:00:12.5');
		// a sentence’s own full stop after a time is not part of it
		expect(respellTimes('Cut at 0:12.5.', 30, 'timecode')).toBe('Cut at 00:00:12:15.');
	});

	test('a time the entry is about is shown at the frame, not at the tenth it was printed at', () => {
		// frame 139 at 30 fps (4.633 s) prints as 0:04.6 in the backend's sentence; the entry's `at` is exact.
		expect(respellTimes('Split at 0:04.6', 30, 'timecode', [139 / 30])).toBe('Split at 00:00:04:19');
		expect(respellTimes('Split at 0:04.6', 30, 'timecode')).toBe('Split at 00:00:04:18');
		// a different time in the same sentence is not pulled onto it
		expect(respellTimes('Moved 0:04.6 → 0:09.0', 30, 'timecode', [139 / 30])).toBe('Moved 00:00:04:19 → 00:00:09:00');
	});

	test('a change’s sentence and detail are both respelled with its own place', () => {
		const e = { summary: 'Retrimmed at 0:12.5', detail: 'was 0:10.0', at: 12.5 };
		expect(changeLine(e, 30, 'timecode')).toEqual({ summary: 'Retrimmed at 00:00:12:15', detail: 'was 00:00:10:00' });
		expect(changeLine({ summary: 'Added a marker', at: undefined }, 30, 'timecode')).toEqual({ summary: 'Added a marker', detail: null });
	});

	test('the headline keeps the backend’s arithmetic and its durations at the frame', () => {
		const diff = { entries: [{}, {}, {}, {}] as DiffEntry[], duration_before: 20.5, duration_after: 17, clips_before: 3, clips_after: 3 };
		expect(headlineParts(diff, 30, 'timecode')).toEqual(['4 changes', '00:00:20:15 → 00:00:17:00 (-3.5s)']);
		expect(headlineParts(diff, 30, 'seconds')).toEqual(['4 changes', '0:20.5 → 0:17.0 (-3.5s)']);
		expect(headlineParts({ ...diff, duration_after: 20.5, clips_after: 5 }, 30, 'timecode')).toEqual(['4 changes', '00:00:20:15', '3 → 5 clips']);
		expect(headlineParts({ ...diff, entries: [] }, 30, 'timecode')).toEqual(['No changes']);
	});
});

describe('the preview’s stepper says what a place is', () => {
	test('"place 1 of 3" and "4 changes" are both said, with how they relate', () => {
		expect(placeStep(3, 1)).toBe('Place 1 of 3');
		expect(placeStep(3, 0)).toBe('3 places');
		expect(placeStep(1, 0)).toBe('1 place');
		expect(placeStep(0, 0)).toBe('No places');
		expect(placesNote(3, 4)).toBe('4 changes in 3 places');
		expect(placesNote(1, 2)).toBe('2 changes in 1 place');
	});

	test('nothing to explain when every change is a place of its own, or there are none', () => {
		expect(placesNote(4, 4)).toBeNull();
		expect(placesNote(0, 2)).toBeNull();
	});
});

describe('a stale proposal says how far the cut has moved and the way out', () => {
	test('edits made since: applying replaces them, and going back is offered', () => {
		const w = staleWords(5, 8);
		expect(w.title).toBe('Your cut has changed since this was proposed');
		expect(w.body).toContain('You made 3 edits after the agent staged it');
		expect(w.body).toContain('Discard it, or go back');
		expect(w.back?.label).toBe('Go back to that cut');
		expect(w.back?.tip).toContain('Your edits stay in History until you make a new edit');
		expect(staleWords(7, 8).body).toContain('You made 1 edit after');
		expect(staleWords(7, 8).back?.tip).toContain('Your edit stays in History');
	});

	test('the user went back past the cut it started from: the way out is forward', () => {
		const w = staleWords(5, 3);
		expect(w.body).toContain('You went back 2 edits past the cut the agent started from');
		expect(w.back?.label).toBe('Return to that cut');
		expect(w.back?.tip).toContain('forward 2 edits');
	});

	test('with no head known there is no count to give and nothing to return to', () => {
		const w = staleWords(5, null);
		expect(w.back).toBeNull();
		expect(w.body).toContain('not the one the agent started from');
		expect(staleWords(5, 5).back).toBeNull();
	});
});

describe('the toast for an arriving proposal', () => {
	test('says how many and offers the one way in', () => {
		expect(arrivalNotice({ diff: { entries: [1, 2, 3, 4] } })).toEqual({ text: 'The agent proposed 4 changes', action: 'Review' });
		expect(arrivalNotice({ diff: { entries: [1] } }).text).toBe('The agent proposed 1 change');
	});

	test('carries the agent’s note, cut to a line', () => {
		expect(arrivalNotice({ diff: { entries: [1, 2] }, note: ' Tighten the intro ' }).text).toBe('The agent proposed 2 changes — Tighten the intro');
		const long = arrivalNotice({ diff: { entries: [1] }, note: 'x'.repeat(80) }).text;
		expect(long.endsWith('…')).toBe(true);
		expect(long.length).toBeLessThan(90);
	});

	test('an empty proposal is still one to look at', () => {
		expect(arrivalNotice({ diff: { entries: [] } }).text).toBe('The agent has a proposal for you');
	});
});
