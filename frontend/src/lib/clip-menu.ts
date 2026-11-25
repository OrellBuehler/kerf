// The clip's right-click menu — pure. It used to be 23 rows in one list, five of them off with a second line each, so
// that the menu was 805 px tall and a 768 px window scrolled it. It is built here from plain facts (what the
// selection can do and why not, the keys the user has) and the callbacks that run each row, so its shape can be held
// by a test:
//
//  - the daily verbs are at the top level: split, add an edit to every track, copy, duplicate, disable, remove,
//    ripple delete;
//  - what is done less often sits one level down — trims, audio, link, keyframes, fades, "only this clip" — and a
//    group of one row is that row, not a submenu around it;
//  - a row that is off is one line, like the rest, and says why as its tooltip (`reason`);
//  - every key shown is the registry's (`key`), never written here.

import type { MenuItem } from './context-menu.svelte';
import type { ActionPlan } from './link-ui';
import { heldForPreview, removeRows } from './menu-items';

/** A feature a clip may offer (Fit to video, stems, compose, sync): whether it is offered at all, and why it is off. */
export interface Offer {
	show: boolean;
	reason: string | null;
}

export interface ClipMenuInput {
	/** The clips the menu acts on: the whole selection when the clicked clip is part of it. */
	count: number;
	kind: 'video' | 'audio';
	trackName: string;
	locked: boolean;
	/** The playhead is inside the clicked clip. */
	playheadIn: boolean;
	/** Whether a cut through every track at the playhead has anything to cut, and why not. */
	cutAll: { ok: boolean; reason?: string };
	/** The clip plays (is not disabled). */
	enabled: boolean;
	/** Why Duplicate cannot be done, or null. */
	dupWhy: string | null;
	/** The clip has linked partners, so "only this clip" is a real choice. */
	linked: boolean;
	detach: ActionPlan;
	reattach: ActionPlan;
	link: ActionPlan;
	unlink: ActionPlan;
	sync: Offer;
	fit: Offer;
	stems: Offer;
	compose: Offer;
	keyframes: { copy: { disabled: boolean; reason?: string }; paste: { disabled: boolean; reason?: string } };
	fadeIn: boolean;
	fadeOut: boolean;
	/** The agent's proposal is on the timeline, read-only: every row that edits the cut is off. */
	previewing: boolean;
	/** The key an action has in the registry (`settings.shortcut`); empty for none. */
	key: (actionId: string) => string;
	/** The analysis rows (what has been analysed, and the way to run more), built where the singletons are. */
	analysis: MenuItem[];
}

export interface ClipMenuActions {
	split(): void;
	cutAll(): void;
	trimStart(): void;
	trimEnd(): void;
	rippleTrimStart(): void;
	rippleTrimEnd(): void;
	copy(): void;
	duplicate(): void;
	toggleEnabled(): void;
	detach(): void;
	reattach(): void;
	link(): void;
	unlink(): void;
	sync(): void;
	fit(): void;
	stems(): void;
	compose(): void;
	motionPresets(): void;
	copyKeyframes(): void;
	pasteKeyframes(): void;
	toggleFadeIn(): void;
	toggleFadeOut(): void;
	remove(ripple: boolean): void;
	removeAlone(ripple: boolean): void;
}

const sub = (label: string, icon: string | undefined, items: MenuItem[]): MenuItem => ({ type: 'submenu', label, icon, items });

/** `items` as a submenu — or, when it is one row, that row: a menu around a single choice is a click for nothing. */
function group(label: string, icon: string | undefined, items: MenuItem[]): MenuItem[] {
	const rows = items.filter((i) => i.type === undefined || i.type === 'item' || i.type === 'submenu');
	if (rows.length === 0) return [];
	return rows.length === 1 ? rows : [sub(label, icon, items)];
}

export function clipMenu(i: ClipMenuInput, a: ClipMenuActions): MenuItem[] {
	const n = i.count;
	const hold = (items: MenuItem[]) => heldForPreview(items, i.previewing);
	const lockedWhy = i.locked ? `Track ${i.trackName} is locked` : undefined;

	const trims = hold([
		{
			label: n > 1 ? 'Trim starts to playhead' : 'Trim start to playhead',
			shortcut: i.key('edit.trimStart'),
			disabled: !i.playheadIn || i.locked,
			reason: !i.playheadIn ? 'Move the playhead into this clip first' : lockedWhy,
			action: a.trimStart
		},
		{
			label: n > 1 ? 'Trim ends to playhead' : 'Trim end to playhead',
			shortcut: i.key('edit.trimEnd'),
			disabled: !i.playheadIn || i.locked,
			reason: !i.playheadIn ? 'Move the playhead into this clip first' : lockedWhy,
			action: a.trimEnd
		},
		{ type: 'separator' },
		{
			label: n > 1 ? 'Ripple trim starts to playhead' : 'Ripple trim start to playhead',
			shortcut: i.key('edit.rippleTrimStart'),
			disabled: !i.playheadIn || i.locked,
			reason: !i.playheadIn ? 'Move the playhead into this clip first' : lockedWhy,
			action: a.rippleTrimStart
		},
		{
			label: n > 1 ? 'Ripple trim ends to playhead' : 'Ripple trim end to playhead',
			shortcut: i.key('edit.rippleTrimEnd'),
			disabled: !i.playheadIn || i.locked,
			reason: !i.playheadIn ? 'Move the playhead into this clip first' : lockedWhy,
			action: a.rippleTrimEnd
		}
	]);

	const audio = hold([
		...(i.detach.show
			? [
					{
						label: i.detach.label,
						icon: 'volume-x',
						shortcut: i.key('edit.detachAudio'),
						disabled: i.detach.reason !== null,
						reason: i.detach.reason ?? undefined,
						action: a.detach
					} satisfies MenuItem
				]
			: []),
		...(i.reattach.show
			? [
					{
						label: i.reattach.label,
						icon: 'audio-waveform',
						shortcut: i.key('edit.reattachAudio'),
						disabled: i.reattach.reason !== null,
						reason: i.reattach.reason ?? undefined,
						action: a.reattach
					} satisfies MenuItem
				]
			: []),
		...(i.stems.show
			? [
					{
						label: 'Separate stems under this clip…',
						icon: 'layers',
						disabled: i.stems.reason !== null,
						reason: i.stems.reason ?? undefined,
						action: a.stems
					} satisfies MenuItem
				]
			: []),
		...(i.sync.show
			? [
					{
						label: 'Sync by audio',
						icon: 'audio-waveform',
						disabled: i.sync.reason !== null,
						reason: i.sync.reason ?? undefined,
						action: a.sync
					} satisfies MenuItem
				]
			: [])
	]);

	const link = hold([
		{
			label: i.link.label,
			icon: 'link',
			shortcut: i.key('edit.link'),
			disabled: i.link.reason !== null,
			reason: i.link.reason ?? undefined,
			action: a.link
		},
		{
			label: i.unlink.label,
			icon: 'unlink',
			shortcut: i.key('edit.unlink'),
			disabled: i.unlink.reason !== null,
			reason: i.unlink.reason ?? undefined,
			action: a.unlink
		}
	]);

	const keyframes = hold([
		{
			label: 'Motion presets…',
			icon: 'move',
			disabled: i.kind === 'audio',
			reason: i.kind === 'audio' ? 'A sound has no picture to move' : undefined,
			action: a.motionPresets
		},
		{ type: 'separator' },
		{ label: 'Copy keyframes', icon: 'diamond', disabled: i.keyframes.copy.disabled, reason: i.keyframes.copy.reason, action: a.copyKeyframes },
		{ label: 'Paste keyframes', icon: 'clipboard', disabled: i.keyframes.paste.disabled, reason: i.keyframes.paste.reason, action: a.pasteKeyframes }
	]);

	const fades = hold([
		{ label: i.fadeIn ? 'Remove fade-in' : 'Add fade-in', action: a.toggleFadeIn },
		{ label: i.fadeOut ? 'Remove fade-out' : 'Add fade-out', action: a.toggleFadeOut }
	]);

	const music = hold([
		...(i.fit.show
			? [
					{
						label: 'Fit to video…',
						icon: 'music',
						disabled: i.fit.reason !== null,
						reason: i.fit.reason ?? undefined,
						action: a.fit
					} satisfies MenuItem
				]
			: []),
		...(i.compose.show
			? [
					{
						label: 'Edit & re-compose…',
						icon: 'music',
						disabled: i.compose.reason !== null,
						reason: i.compose.reason ?? undefined,
						action: a.compose
					} satisfies MenuItem
				]
			: [])
	]);

	// Remove and Ripple delete are the daily rows; the two that leave the linked clips where they are go one level down.
	const removal = hold(
		removeRows({
			count: n,
			linked: i.linked,
			locked: i.locked,
			trackName: i.trackName,
			removeKey: i.key('edit.delete'),
			rippleKey: i.key('edit.rippleDelete'),
			remove: a.remove,
			removeAlone: a.removeAlone
		})
	);
	const daily = removal.filter((r) => !('label' in r && r.label.endsWith('only this clip')));
	const alone = removal.filter((r) => 'label' in r && r.label.endsWith('only this clip'));

	return [
		...hold([
			{
				label: n > 1 ? 'Split selected at playhead' : 'Split at playhead',
				icon: 'Scissors',
				shortcut: i.key('edit.split'),
				disabled: !i.playheadIn || i.locked,
				reason: !i.playheadIn ? 'Move the playhead into this clip first' : lockedWhy,
				// The playhead can be anywhere between frames; the cut is on one (`planPlayheadSplit`).
				action: a.split
			},
			{
				label: 'Add edit to all tracks',
				icon: 'scissors-line-dashed',
				shortcut: i.key('edit.splitAll'),
				disabled: !i.cutAll.ok,
				reason: i.cutAll.reason,
				action: a.cutAll
			}
		]),
		sub('Trim to playhead', undefined, trims),
		{ type: 'separator' },
		{
			label: n > 1 ? `Copy ${n} clips` : 'Copy',
			icon: 'copy',
			shortcut: i.key('edit.copy'),
			action: a.copy
		},
		...hold([
			{
				label: n > 1 ? `Duplicate ${n} clips` : 'Duplicate',
				shortcut: i.key('edit.duplicate'),
				disabled: !!i.dupWhy,
				reason: i.dupWhy ?? undefined,
				action: a.duplicate
			},
			{
				label: i.enabled ? 'Disable clip' : 'Enable clip',
				icon: i.enabled ? 'eye-off' : 'eye',
				disabled: i.locked,
				reason: lockedWhy,
				action: a.toggleEnabled
			}
		]),
		{ type: 'separator' },
		...group('Audio', 'audio-waveform', audio),
		sub('Link', 'link', link),
		sub('Keyframes', 'diamond', keyframes),
		...group('Fades', undefined, fades),
		...music,
		{ type: 'separator' },
		...i.analysis,
		{ type: 'separator' },
		...daily,
		...(alone.length > 0 ? [sub('Only this clip', undefined, alone)] : [])
	];
}

/** What a menu of rows takes up, in pixels: an upper bound from the row and rule heights the menu is drawn with, for a
 *  test that holds it under the window. A row is `--row-h` (26) plus its padding; an info row two lines at most. */
export const MENU_ROW_PX = 28;
export const MENU_RULE_PX = 9;
export const MENU_INFO_PX = 36;
export const MENU_PAD_PX = 10;

export function menuHeight(items: readonly MenuItem[]): number {
	return (
		MENU_PAD_PX +
		items.reduce((sum, item) => {
			if (item.type === 'separator') return sum + MENU_RULE_PX;
			if (item.type === 'info') return sum + MENU_INFO_PX;
			if (item.type === 'header') return sum + 40;
			return sum + MENU_ROW_PX;
		}, 0)
	);
}
