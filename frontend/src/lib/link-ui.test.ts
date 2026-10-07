import { describe, expect, test } from 'bun:test';
import { linkClips } from './link-groups';
import { detachAudio as detach } from './links';
import {
	ALT_HINT,
	clickSelectLinked,
	gestureReason,
	linkBadge,
	linkBadges,
	linkPlans,
	marqueeSelectLinked,
	onlyPartners,
	playsOwnSound,
	reasonOf
} from './link-ui';
import { NO_SELECTION } from './selection';
import type { Clip, StreamKind, Timeline, Track } from './types';

// `detachAudio` is links.ts's port of the backend's edit and `linkClips` link-groups.ts's:
// both build the shapes the chrome has to read.

const clip = (asset: string, id: string, at: number, len = 10): Clip => ({
	id,
	asset_id: asset,
	source_in: 0,
	source_out: len,
	timeline_start: at,
	volume: 1,
	fade_in: 0,
	fade_out: 0
});
const lane = (kind: StreamKind, name: string, clips: Clip[], locked = false): Track => ({
	id: name.toLowerCase(),
	kind,
	name,
	clips,
	locked
});
const names: Record<string, string> = { talk: 'interview.mp4', broll: 'broll.mp4', music: 'bed.wav' };
const assetName = (id: string) => names[id] ?? id;
const hasAudio = (id: string) => id !== 'broll';

/** V1: v1 [0,10) talk, v2 [10,20) broll          A1: a1 [0,10) talk (linked to v1)
 *  V2: w1 [0,5) talk                              A2: m1 [0,20) music */
function cut(): Timeline {
	const t: Timeline = {
		tracks: [
			lane('video', 'V1', [clip('talk', 'v1', 0), clip('broll', 'v2', 10)]),
			lane('video', 'V2', [clip('talk', 'w1', 0, 5)]),
			lane('audio', 'A1', [clip('talk', 'a1', 0)]),
			lane('audio', 'A2', [clip('music', 'm1', 0, 20)])
		]
	};
	linkClips(t, ['v1', 'a1']);
	return t;
}

describe('clickSelectLinked', () => {
	test('a plain click on a linked clip selects it and its partners, the clicked one primary', () => {
		const sel = clickSelectLinked(cut(), NO_SELECTION, 'v1', 'replace');
		expect([...sel.ids].sort()).toEqual(['a1', 'v1']);
		expect(sel.primary).toBe('v1');
		const fromSound = clickSelectLinked(cut(), NO_SELECTION, 'a1', 'replace');
		expect(fromSound.primary).toBe('a1');
		expect([...fromSound.ids].sort()).toEqual(['a1', 'v1']);
	});

	test('Alt selects just the one, in every mode', () => {
		const t = cut();
		expect(clickSelectLinked(t, NO_SELECTION, 'v1', 'replace', null, true)).toEqual({ ids: ['v1'], primary: 'v1' });
		const base = { ids: ['m1'], primary: 'm1' };
		expect(clickSelectLinked(t, base, 'v1', 'toggle', null, true)).toEqual({ ids: ['m1', 'v1'], primary: 'v1' });
	});

	test('an unlinked clip is the plain click it always was', () => {
		expect(clickSelectLinked(cut(), NO_SELECTION, 'v2', 'replace')).toEqual({ ids: ['v2'], primary: 'v2' });
		expect(clickSelectLinked(cut(), { ids: ['m1'], primary: 'm1' }, 'v2', 'toggle')).toEqual({ ids: ['m1', 'v2'], primary: 'v2' });
	});

	test('ctrl-click toggles the clip and its partners in and out together', () => {
		const t = cut();
		const on = clickSelectLinked(t, { ids: ['m1'], primary: 'm1' }, 'a1', 'toggle');
		expect([...on.ids].sort()).toEqual(['a1', 'm1', 'v1']);
		expect(on.primary).toBe('a1');
		const off = clickSelectLinked(t, on, 'v1', 'toggle');
		expect(off.ids).toEqual(['m1']);
		expect(off.primary).toBe('m1');
	});

	test('toggling off the primary hands the Inspector what is left', () => {
		const t = cut();
		const sel = clickSelectLinked(t, { ids: ['m1', 'v1', 'a1'], primary: 'v1' }, 'v1', 'toggle');
		expect(sel).toEqual({ ids: ['m1'], primary: 'm1' });
	});

	test('shift extends along the track and brings in the partners of what it added', () => {
		const t = cut();
		const sel = clickSelectLinked(t, { ids: ['v1'], primary: 'v1' }, 'v2', 'range', ['v1', 'v2']);
		expect([...sel.ids].sort()).toEqual(['a1', 'v1', 'v2']);
		expect(sel.primary).toBe('v2');
	});

	test('a marquee selects what it touches and their partners; Alt, just what it touches', () => {
		const t = cut();
		const sel = marqueeSelectLinked(t, NO_SELECTION, ['v1', 'w1'], 'replace', false);
		expect([...sel.ids].sort()).toEqual(['a1', 'v1', 'w1']);
		// The Inspector shows a clip the rectangle touched, not the partner it brought along.
		expect(sel.primary).toBe('w1');
		const alone = marqueeSelectLinked(t, NO_SELECTION, ['v1', 'w1'], 'replace', true);
		expect([...alone.ids].sort()).toEqual(['v1', 'w1']);
	});

	test('a marquee that adds or toggles does so for a swept pair together', () => {
		const t = cut();
		const base = { ids: ['m1'], primary: 'm1' };
		const add = marqueeSelectLinked(t, base, ['a1'], 'add', false);
		expect([...add.ids].sort()).toEqual(['a1', 'm1', 'v1']);
		expect(add.primary).toBe('a1');
		const was = { ids: ['v1', 'a1', 'm1'], primary: 'a1' };
		const off = marqueeSelectLinked(t, was, ['v1'], 'toggle', false);
		expect(off.ids).toEqual(['m1']);
	});

	test('onlyPartners tells a press that is just the clip’s own group', () => {
		const t = cut();
		expect(onlyPartners(t, 'v1', ['a1'])).toBe(true);
		expect(onlyPartners(t, 'v1', [])).toBe(true);
		expect(onlyPartners(t, 'v1', ['a1', 'm1'])).toBe(false);
		expect(onlyPartners(t, 'v2', ['a1'])).toBe(false);
	});
});

describe('linkBadge', () => {
	test('an unlinked clip playing its own sound has none', () => {
		const t = cut();
		expect(linkBadge(t, t.tracks[0].clips[1], assetName)).toBeNull();
	});

	test('a linked pair names each other and says what Alt does', () => {
		const t = cut();
		const b = linkBadge(t, t.tracks[0].clips[0], assetName)!;
		expect(b.partners).toEqual([{ id: 'a1', track: 'A1', kind: 'audio', asset: 'interview.mp4' }]);
		expect(b.detached).toBe(false);
		expect(b.title).toContain('Linked with A1 (interview.mp4)');
		expect(b.title).toContain(ALT_HINT);
		const s = linkBadge(t, t.tracks[2].clips[0], assetName)!;
		expect(s.partners[0].track).toBe('V1');
	});

	test('a detached picture says where its sound plays, and the sound clip knows its picture', () => {
		const t = cut();
		const d = detach(t, 'w1', true);
		const b = linkBadge(t, t.tracks[1].clips[0], assetName)!;
		expect(b.detached).toBe(true);
		expect(b.title).toContain('Sound detached — it plays from');
		expect(b.partners[0].id).toBe(d.clip.id);
	});

	test('a detached picture whose audio clip is gone is still marked: it is silent', () => {
		const t = cut();
		t.tracks[0].clips[0].source_audio = false;
		t.tracks[2].clips = [];
		const b = linkBadge(t, t.tracks[0].clips[0], assetName)!;
		expect(b.partners).toEqual([]);
		expect(b.detached).toBe(true);
		expect(b.title).toContain('its audio clip is gone');
		expect(b.title).not.toContain(ALT_HINT);
	});

	test('linkBadges is linkBadge for every clip, in one pass', () => {
		const t = cut();
		detach(t, 'w1', true);
		const all = linkBadges(t, assetName);
		for (const c of t.tracks.flatMap((x) => x.clips)) expect(all.get(c.id) ?? null).toEqual(linkBadge(t, c, assetName));
		expect(all.has('v2')).toBe(false); // unlinked, own sound
		expect(all.has('m1')).toBe(false);
		expect(all.size).toBe(4); // v1 and a1, and w1 with the sound it was given
	});

	test('playsOwnSound: a detached picture is silent whatever its volume says', () => {
		expect(playsOwnSound({}, true)).toBe(true);
		expect(playsOwnSound({ source_audio: true }, true)).toBe(true);
		expect(playsOwnSound({ source_audio: false }, true)).toBe(false);
		expect(playsOwnSound({}, false)).toBe(false);
	});
});

describe('linkPlans — detach', () => {
	test('a picture playing its own sound can be detached', () => {
		const p = linkPlans(cut(), hasAudio, ['w1']).detach;
		expect(p).toEqual({ ids: ['w1'], label: 'Detach audio', reason: null, show: true });
	});

	test('several pictures: one call each, the label counts them', () => {
		const t = cut();
		const p = linkPlans(t, hasAudio, ['v1', 'w1']).detach;
		expect(p.ids).toEqual(['v1', 'w1']);
		expect(p.label).toBe('Detach audio from 2 clips');
	});

	test('what has no sound or has already given it up says so', () => {
		const t = cut();
		expect(linkPlans(t, hasAudio, ['v2']).detach.reason).toBe('This footage has no sound to detach');
		t.tracks[0].clips[0].source_audio = false;
		expect(linkPlans(t, hasAudio, ['v1']).detach.reason).toBe('Its sound is already detached');
		// …but the others in a mixed selection still go.
		expect(linkPlans(t, hasAudio, ['v1', 'v2', 'w1']).detach.ids).toEqual(['w1']);
	});

	test('a locked track is the reason, and an audio clip is not offered it at all', () => {
		const t = cut();
		t.tracks[1].locked = true;
		expect(linkPlans(t, hasAudio, ['w1']).detach.reason).toBe('Track V2 is locked');
		const p = linkPlans(cut(), hasAudio, ['m1']).detach;
		expect(p.show).toBe(false);
	});
});

describe('linkPlans — reattach', () => {
	function detached() {
		const t = cut();
		detach(t, 'w1', true);
		return t;
	}

	test('is offered only where something is detached, naming either clip of the pair', () => {
		const t = detached();
		expect(linkPlans(cut(), hasAudio, ['w1']).reattach.show).toBe(false);
		const byPicture = linkPlans(t, hasAudio, ['w1']).reattach;
		expect(byPicture).toMatchObject({ ids: ['w1'], label: 'Reattach audio', reason: null, show: true });
		const sound = t.tracks.flatMap((x) => x.clips).find((c) => c.id !== 'w1' && c.link_id === t.tracks[1].clips[0].link_id)!;
		const bySound = linkPlans(t, hasAudio, [sound.id]).reattach;
		expect(bySound.ids).toEqual(['w1']);
	});

	test('the picture and its sound selected together are one reattach, not two', () => {
		const t = detached();
		const sound = t.tracks.flatMap((x) => x.clips).find((c) => c.id !== 'w1' && c.link_id === t.tracks[1].clips[0].link_id)!;
		const p = linkPlans(t, hasAudio, ['w1', sound.id]).reattach;
		expect(p.ids).toEqual(['w1']);
		expect(p.label).toBe('Reattach audio');
	});

	test('several detached pairs are counted', () => {
		const t = detached();
		detach(t, 'v1', true);
		const p = linkPlans(t, hasAudio, ['w1', 'v1']).reattach;
		expect(p.ids.sort()).toEqual(['v1', 'w1']);
		expect(p.label).toBe('Reattach audio on 2 clips');
	});

	test('a locked picture track or a locked sound track is the reason', () => {
		const t = detached();
		t.tracks[1].locked = true;
		expect(linkPlans(t, hasAudio, ['w1']).reattach.reason).toBe('Track V2 is locked');
		t.tracks[1].locked = false;
		const at = t.tracks.findIndex((x) => x.clips.some((c) => c.id !== 'w1' && c.link_id === t.tracks[1].clips[0].link_id));
		t.tracks[at].locked = true;
		expect(linkPlans(t, hasAudio, ['w1']).reattach.reason).toBe(`Track ${t.tracks[at].name} is locked`);
	});
});

describe('linkPlans — link and unlink', () => {
	test('one clip cannot be linked; two on different tracks can', () => {
		const t = cut();
		expect(linkPlans(t, hasAudio, ['w1']).link.reason).toBe('Select two or more clips on different tracks to link them');
		const p = linkPlans(t, hasAudio, ['w1', 'm1']).link;
		expect(p).toMatchObject({ ids: ['w1', 'm1'], label: 'Link 2 clips', reason: null, show: true });
	});

	test('two clips of one track are refused with the one-per-track rule, in the backend’s words', () => {
		const p = linkPlans(cut(), hasAudio, ['v1', 'v2']).link;
		expect(p.reason).toBe('Two of the clips are on track V1 — a link joins one clip per track (a picture and its sound)');
	});

	test('a locked track, and a pair that is already one group, say so', () => {
		const t = cut();
		t.tracks[3].locked = true;
		expect(linkPlans(t, hasAudio, ['w1', 'm1']).link.reason).toBe('Track A2 is locked');
		expect(linkPlans(cut(), hasAudio, ['v1', 'a1']).link.reason).toBe('Those clips are already linked');
	});

	test('unlink needs a linked clip, and sends only the linked ones', () => {
		const t = cut();
		expect(linkPlans(t, hasAudio, ['w1']).unlink.reason).toBe('This clip is not linked');
		expect(linkPlans(t, hasAudio, ['w1', 'm1']).unlink.reason).toBe('None of the selected clips is linked');
		const p = linkPlans(t, hasAudio, ['v1', 'a1', 'm1']).unlink;
		expect(p).toMatchObject({ ids: ['v1', 'a1'], label: 'Unlink 2 clips', reason: null });
		expect(linkPlans(t, hasAudio, ['v1']).unlink.label).toBe('Unlink');
	});

	test('a locked linked clip is the reason', () => {
		const t = cut();
		t.tracks[2].locked = true;
		expect(linkPlans(t, hasAudio, ['v1', 'a1']).unlink.reason).toBe('Track A1 is locked');
	});

	test('an empty or stale selection offers what it can and never throws', () => {
		const p = linkPlans(cut(), hasAudio, ['gone']);
		expect(p.detach.show).toBe(false);
		expect(p.reattach.show).toBe(false);
		expect(p.link.reason).toBe('Select two or more clips on different tracks to link them');
		expect(p.unlink.reason).toBe('Select a linked clip to unlink it');
		expect(p.detach.reason).toBe('Select a picture clip to detach its sound');
	});

	test('reasonOf strips the transport’s prefix', () => {
		expect(reasonOf(new Error('invalid argument: track V1 is locked'))).toBe('Track V1 is locked');
		expect(reasonOf('plain')).toBe('Plain');
	});
});

describe('gestureReason', () => {
	test('where the backend says to edit with links off, a gesture says to hold Alt', () => {
		expect(gestureReason('a linked clip is on locked track A1 — unlock it, or edit with links off')).toBe(
			'a linked clip is on locked track A1 — unlock it, or hold Alt to edit this clip on its own'
		);
		expect(gestureReason('that edit would put the linked clips on V1 and A1 out of step — edit with links off to move one of them on its own')).toBe(
			'that edit would put the linked clips on V1 and A1 out of step — hold Alt to edit this clip on its own'
		);
		expect(gestureReason('track V1 is locked')).toBe('track V1 is locked');
	});
});
