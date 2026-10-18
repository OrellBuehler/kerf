// Pure helpers for the timeline's context menus — what is easy to get wrong in a list
// built by hand on every right-click, and so worth a test.

import type { MenuItem } from './context-menu.svelte';
import type { Track } from './types';
import type { NavRow } from './menu-nav';
import { PREVIEW_READ_ONLY_REASON } from './proposal-ui';

/** A menu with its separators made tidy: none leading, none trailing, never two in a row.
 *  A menu is assembled from optional groups (the Analyze rows, the Fit / Stems offers), and
 *  a group that is not there used to leave its two separators side by side. */
export function tidyMenu(items: readonly MenuItem[]): MenuItem[] {
	const out: MenuItem[] = [];
	for (const item of items) {
		if (item.type === 'separator' && (out.length === 0 || out[out.length - 1].type === 'separator')) continue;
		out.push(item);
	}
	while (out.length > 0 && out[out.length - 1].type === 'separator') out.pop();
	return out;
}

/** The rows of a menu that edit the cut, while the proposal is previewed: each is off with the reason, because the
 *  cut on screen is the agent's and an edit would be refused (or, worse, planned against ids that are not the
 *  project's). Rows that only look — go to, copy, analyze — are not passed through this. */
export function heldForPreview(items: readonly MenuItem[], previewing: boolean): MenuItem[] {
	if (!previewing) return [...items];
	return items.map((item) =>
		item.type === undefined || item.type === 'item' ? { ...item, disabled: true, reason: PREVIEW_READ_ONLY_REASON } : item
	);
}

/** The track menu's mute / hide row: a picture track is hidden and shown (an eye), a sound
 *  track muted and unmuted (a speaker) — the words and the icon follow what the track header
 *  draws for the same state, and the icon is what the click does. */
export function muteItem(track: Pick<Track, 'kind' | 'muted'>): { label: string; icon: string } {
	if (track.kind === 'video') return track.muted ? { label: 'Show track', icon: 'eye' } : { label: 'Hide track', icon: 'eye-off' };
	return track.muted ? { label: 'Unmute track', icon: 'volume-2' } : { label: 'Mute track', icon: 'volume-x' };
}

export interface RemoveRows {
	/** Clips the menu acts on (the whole selection when the clicked clip is part of it). */
	count: number;
	/** The clip has link partners, so "only this clip" is a real choice. */
	linked: boolean;
	locked: boolean;
	trackName: string;
	removeKey?: string;
	rippleKey?: string;
	remove: (ripple: boolean) => void;
	removeAlone: (ripple: boolean) => void;
}

/** The delete rows of a clip's menu. Four red rows with one trash can between them said
 *  nothing about how they differ, so each kind has its own icon — the trash for removing, the
 *  ripple glyph for closing the gap — and the two that leave the linked clips where they are
 *  carry none. */
export function removeRows(o: RemoveRows): MenuItem[] {
	const n = o.count;
	const items: MenuItem[] = [
		{
			label: n > 1 ? `Remove ${n} clips` : 'Remove',
			icon: 'trash',
			shortcut: o.removeKey,
			danger: true,
			action: () => o.remove(false)
		},
		{
			label: n > 1 ? `Ripple delete ${n} clips` : 'Ripple delete',
			icon: 'between-horizontal-start',
			shortcut: o.rippleKey,
			danger: true,
			action: () => o.remove(true)
		}
	];
	if (!o.linked) return items;
	const reason = o.locked ? `Track ${o.trackName} is locked` : undefined;
	return [
		...items,
		{ label: 'Remove only this clip', danger: true, disabled: o.locked, reason, action: () => o.removeAlone(false) },
		{ label: 'Ripple delete only this clip', danger: true, disabled: o.locked, reason, action: () => o.removeAlone(true) }
	];
}

/** The context menu's rows as keyboard navigation sees them: only an action row takes focus (a
 *  header, a fact and a rule are not controls), and a disabled one still does so it can say why. */
export function contextRows(items: readonly MenuItem[]): NavRow[] {
	return items.map((i) =>
		i.type === undefined || i.type === 'item'
			? { focusable: true, label: i.label, enabled: !i.disabled }
			: { focusable: false, label: '' }
	);
}
