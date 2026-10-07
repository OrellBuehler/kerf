// Linked A/V as the editor's chrome sees it — pure. `link-groups.ts` / `links.ts` are
// the edits (the faithful port of kerf-core's `model/links.rs`); this is what the
// Timeline needs of them to *show* a link and to act on one:
//
//  - which clips a click selects (a clip and its partners; Alt for the one),
//  - what the clip menu and the keyboard offer — Detach audio, Reattach audio, Link,
//    Unlink — with the words for each and, when one cannot be done, the reason,
//  - what the link badge on a clip says.
//
// Nothing here edits: every answer is read off the timeline, and the checks are the
// backend's own (`planLink` / `planUnlink` are the validation halves of `linkClips` /
// `unlinkClips`), so a menu item is never offered that the backend would refuse for a
// reason this could have said first.

import { linkPartners, locateIndex, planLink, planUnlink, withLinkPartners } from './link-groups';
import { clickSelect, marqueeSelect, normalize, type MarqueeMode, type PickMode, type Selection } from './selection';
import type { Clip, StreamKind, Timeline } from './types';

/** What the tooltips say about Alt — one sentence, so the badge, the toolbar and the menu agree. */
export const ALT_HINT =
	'Alt-click selects just this clip; Alt-drag, Alt-trim and Alt-razor edit it on its own, leaving its linked clips where they are.';

const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
/** A backend refusal as a sentence for a menu line (`invalid argument: ` is the transport's, not the user's). */
export const reasonOf = (e: unknown): string =>
	capital((e instanceof Error ? e.message : String(e)).replace(/^invalid argument: /, ''));

/** A backend refusal worded for a *gesture*: where it says to edit with links off, the user
 *  holds Alt (`unlock it, or edit with links off` → `unlock it, or hold Alt to edit this clip on
 *  its own`). The refusals are the backend's own words, shared with the agent that has no Alt. */
export const gestureReason = (message: string): string =>
	message.replace(/edit with links off( to move one of them on its own)?/g, 'hold Alt to edit this clip on its own');

// ---- selection --------------------------------------------------------------------

/**
 * The selection after a click on `clipId`: `clickSelect`, but a click on a linked clip
 * takes its partners with it — pick the picture and its sound is picked too — unless
 * `alone` (Alt). A plain click replaces; Ctrl/Cmd toggles the clip *and* its partners
 * in or out together; Shift extends along the track and brings in the partners of
 * everything it added. The primary (what the Inspector edits) is always the clicked clip.
 */
export function clickSelectLinked(
	timeline: Timeline,
	sel: Selection,
	clipId: string,
	mode: PickMode,
	trackOrder: readonly string[] | null = null,
	alone = false
): Selection {
	if (alone) return clickSelect(sel, clipId, mode, trackOrder);
	if (mode === 'range') {
		const r = clickSelect(sel, clipId, 'range', trackOrder);
		return normalize(withLinkPartners(timeline, [...r.ids]), r.primary);
	}
	const partners = linkPartners(timeline, clipId);
	if (partners.length === 0) return clickSelect(sel, clipId, mode, trackOrder);
	if (mode === 'toggle') {
		if (sel.ids.includes(clipId)) {
			const drop = new Set([clipId, ...partners]);
			return normalize(
				sel.ids.filter((id) => !drop.has(id)),
				sel.primary !== null && drop.has(sel.primary) ? null : sel.primary
			);
		}
		return normalize([...sel.ids, clipId, ...partners], clipId);
	}
	return normalize([clipId, ...partners], clipId);
}

/**
 * What a marquee that touches `hits` leaves selected, given `base` (see `marqueeSelect`):
 * the clips it touched *and their partners* — sweeping a picture sweeps its sound — unless
 * `alone`. The primary (what the Inspector shows) is the clip the rectangle touched, never
 * a partner it only brought along; Ctrl/Cmd toggles a swept pair in or out together.
 */
export function marqueeSelectLinked(
	timeline: Timeline,
	base: Selection,
	hits: readonly string[],
	mode: MarqueeMode,
	alone: boolean
): Selection {
	const touched = marqueeSelect(base, hits, mode);
	if (alone) return touched;
	return normalize(marqueeSelect(base, withLinkPartners(timeline, [...hits]), mode).ids, touched.primary);
}

/** Whether `others` are all partners of `clipId` — a selection that is just the clip's own
 *  link group, which an Alt-press narrows to the clip instead of starting a group drag. */
export function onlyPartners(timeline: Timeline, clipId: string, others: readonly string[]): boolean {
	const partners = new Set(linkPartners(timeline, clipId));
	return others.every((id) => partners.has(id));
}

// ---- the badge ----------------------------------------------------------------------

export interface LinkedPartner {
	id: string;
	/** The partner's track name (`A1`). */
	track: string;
	kind: StreamKind;
	/** The partner's asset name. */
	asset: string;
}

export interface LinkBadge {
	partners: LinkedPartner[];
	/** The clip is a picture whose own sound was detached (it is silent). */
	detached: boolean;
	/** The tooltip. */
	title: string;
}

/** The badge for `clip` given its already-resolved partners (`null`: nothing to say). */
function describeBadge(clip: Clip, partners: LinkedPartner[]): LinkBadge | null {
	const detached = clip.source_audio === false;
	if (partners.length === 0 && !detached) return null;
	const who = partners.map((p) => `${p.track} (${p.asset})`).join(', ');
	const lines: string[] = [];
	if (partners.length > 0) lines.push(`Linked with ${who}. Edits to one carry to the others.`);
	if (detached) {
		const sound = partners.filter((p) => p.kind === 'audio').map((p) => p.track);
		lines.push(
			sound.length > 0
				? `Sound detached — it plays from ${sound.join(', ')}.`
				: 'Sound detached, and its audio clip is gone — this picture is silent. Reattach audio brings its own sound back.'
		);
	}
	if (partners.length > 0) lines.push(ALT_HINT);
	return { partners, detached, title: lines.join('\n') };
}

/**
 * What the chain glyph on a clip stands for, or `null` for a clip with nothing to say:
 * unlinked and playing its own sound. A picture whose sound was detached gets one even
 * when its audio clip is gone (it is silent, and **Reattach audio** is the way back).
 */
export function linkBadge(timeline: Timeline, clip: Clip, assetName: (assetId: string) => string): LinkBadge | null {
	const partners: LinkedPartner[] = linkPartners(timeline, clip.id).map((id) => {
		const at = locateIndex(timeline, id)!;
		const track = timeline.tracks[at[0]];
		return { id, track: track.name, kind: track.kind, asset: assetName(track.clips[at[1]].asset_id) };
	});
	return describeBadge(clip, partners);
}

/** `linkBadge` for every clip that has one, in a single pass over the cut — what the
 *  Timeline reads per clip on every render (a lookup per clip would make that quadratic). */
export function linkBadges(timeline: Timeline, assetName: (assetId: string) => string): Map<string, LinkBadge> {
	const groups = new Map<string, LinkedPartner[]>();
	for (const t of timeline.tracks) {
		for (const c of t.clips) {
			if (!c.link_id) continue;
			const g = groups.get(c.link_id) ?? [];
			g.push({ id: c.id, track: t.name, kind: t.kind, asset: assetName(c.asset_id) });
			groups.set(c.link_id, g);
		}
	}
	const out = new Map<string, LinkBadge>();
	for (const t of timeline.tracks) {
		for (const c of t.clips) {
			const partners = c.link_id ? (groups.get(c.link_id) ?? []).filter((p) => p.id !== c.id) : [];
			const badge = describeBadge(c, partners);
			if (badge) out.set(c.id, badge);
		}
	}
	return out;
}

/** Whether a clip plays the audio of its own asset: what a volume line and a track's mixer
 *  strip are for. A detached picture is silent however loud its (inert) volume reads. */
export const playsOwnSound = (clip: Pick<Clip, 'source_audio'>, assetHasAudio: boolean): boolean =>
	assetHasAudio && clip.source_audio !== false;

// ---- what the menu and the keyboard offer -------------------------------------------

export interface ActionPlan {
	/** The clips the action is sent: for Detach / Reattach each *picture* (one call each),
	 *  for Unlink the linked ones among the selection, for Link all of it. */
	ids: string[];
	/** The menu line, count included (`Detach audio from 3 clips`). */
	label: string;
	/** Why it cannot be done now, or `null` when it can. */
	reason: string | null;
	/** Whether the item belongs in the menu at all (Reattach appears only where something is detached). */
	show: boolean;
}

export interface LinkPlans {
	detach: ActionPlan;
	reattach: ActionPlan;
	link: ActionPlan;
	unlink: ActionPlan;
}

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

/** Whether detaching `clip`'s sound could work: why not, or `null`. Mirrors
 *  `Timeline::detach_audio`'s refusals that a menu can know of before asking. */
function detachProblem(timeline: Timeline, clip: Clip, hasAudio: (assetId: string) => boolean): string | null {
	const [ti] = locateIndex(timeline, clip.id)!;
	const track = timeline.tracks[ti];
	if (track.locked) return `Track ${track.name} is locked`;
	if (!hasAudio(clip.asset_id)) return 'This footage has no sound to detach';
	if (clip.source_audio === false) return 'Its sound is already detached';
	return null;
}

/**
 * Detach, Reattach, Link and Unlink for the selection `ids` — the clip menu's four lines
 * and the keymap's four actions, so the two cannot disagree about what a press does.
 * `hasAudio` says whether an asset carries an audio stream.
 */
export function linkPlans(timeline: Timeline, hasAudio: (assetId: string) => boolean, ids: readonly string[]): LinkPlans {
	const here = ids.filter((id) => locateIndex(timeline, id) !== null);
	const clips = here.map((id) => {
		const [ti, ci] = locateIndex(timeline, id)!;
		return { clip: timeline.tracks[ti].clips[ci], track: timeline.tracks[ti] };
	});

	// ---- detach: the selected pictures still playing their own sound
	const pictures = clips.filter((c) => c.track.kind === 'video');
	const problems = pictures.map((p) => detachProblem(timeline, p.clip, hasAudio));
	const detachable = pictures.filter((_, i) => problems[i] === null).map((p) => p.clip.id);
	const detach: ActionPlan = {
		ids: detachable,
		label: detachable.length > 1 ? `Detach audio from ${detachable.length} clips` : 'Detach audio',
		reason: detachable.length > 0 ? null : (problems.find((p) => p !== null) ?? 'Select a picture clip to detach its sound'),
		show: pictures.length > 0
	};

	// ---- reattach: each detached picture, named by itself or by the sound that carries it
	const detachedPictures: string[] = [];
	for (const { clip, track } of clips) {
		let picture: string | null = null;
		if (track.kind === 'video') picture = clip.source_audio === false ? clip.id : null;
		else {
			picture =
				linkPartners(timeline, clip.id).find((p) => {
					const [pt, pc] = locateIndex(timeline, p)!;
					const c = timeline.tracks[pt].clips[pc];
					return timeline.tracks[pt].kind === 'video' && c.source_audio === false && c.asset_id === clip.asset_id;
				}) ?? null;
		}
		if (picture && !detachedPictures.includes(picture)) detachedPictures.push(picture);
	}
	let reattachReason: string | null = detachedPictures.length === 0 ? 'No selected clip has detached sound' : null;
	for (const id of detachedPictures) {
		const [pt, pc] = locateIndex(timeline, id)!;
		if (timeline.tracks[pt].locked) {
			reattachReason = `Track ${timeline.tracks[pt].name} is locked`;
			break;
		}
		const asset = timeline.tracks[pt].clips[pc].asset_id;
		const sound = linkPartners(timeline, id).find((p) => {
			const [t, c] = locateIndex(timeline, p)!;
			return timeline.tracks[t].kind === 'audio' && timeline.tracks[t].clips[c].asset_id === asset && timeline.tracks[t].locked;
		});
		if (sound) {
			reattachReason = `Track ${timeline.tracks[locateIndex(timeline, sound)![0]].name} is locked`;
			break;
		}
	}
	const reattach: ActionPlan = {
		ids: detachedPictures,
		label: detachedPictures.length > 1 ? `Reattach audio on ${detachedPictures.length} clips` : 'Reattach audio',
		reason: reattachReason,
		show: detachedPictures.length > 0
	};

	// ---- link: the validation is the backend's, said as a sentence
	let linkReason: string | null = null;
	if (here.length < 2) linkReason = 'Select two or more clips on different tracks to link them';
	else {
		try {
			planLink(timeline, here);
		} catch (e) {
			linkReason = reasonOf(e);
		}
	}
	const link: ActionPlan = {
		ids: [...here],
		label: here.length > 1 ? `Link ${here.length} clips` : 'Link',
		reason: linkReason,
		show: true
	};

	// ---- unlink: only the clips that are linked are sent (the backend refuses a locked one even unlinked)
	const linked = here.filter((id) => linkPartners(timeline, id).length > 0);
	let unlinkReason: string | null = null;
	if (linked.length === 0)
		unlinkReason =
			here.length === 0 ? 'Select a linked clip to unlink it' : here.length > 1 ? 'None of the selected clips is linked' : 'This clip is not linked';
	else {
		try {
			planUnlink(timeline, linked);
		} catch (e) {
			unlinkReason = reasonOf(e);
		}
	}
	const unlink: ActionPlan = {
		ids: linked,
		label: linked.length > 1 ? `Unlink ${linked.length} clips` : 'Unlink',
		reason: unlinkReason,
		show: true
	};

	return { detach, reattach, link, unlink };
}

/** What the toast says after an action ran on `n` clips. */
export const detachedNotice = (n: number) => `Audio detached from ${n} ${plural(n, 'clip', 'clips')}`;
export const reattachedNotice = (n: number) => `Audio reattached on ${n} ${plural(n, 'clip', 'clips')}`;
export const linkedNotice = (n: number) => `Linked ${n} clips`;
export const unlinkedNotice = (n: number) => `Unlinked ${n} ${plural(n, 'clip', 'clips')}`;
