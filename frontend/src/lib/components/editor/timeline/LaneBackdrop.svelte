<script lang="ts">
	/* What a lane draws behind and around its clips: the empty-cut hint, the drag tint, what a proposal removes, a locked track's badge. */
	import Icon from '../Icon.svelte';
	import { NAME_CHIP_PCT } from '#lib/clip-label';
	import type { DiffView } from '#lib/staged-diff';
	import { tip } from '#lib/tip';
	import type { Track } from '#lib/types';
	import type { Drag } from './gesture';

	let {
		t,
		drawn,
		hasClips,
		drag,
		diff,
		pxPerSec
	}: {
		t: Track;
		drawn: Track[];
		hasClips: boolean;
		drag: Drag | null;
		diff: DiffView | null;
		pxPerSec: number;
	} = $props();
</script>

{#if !hasClips && t.id === drawn[0]?.id}
	<!-- In the first lane, where the clip will land — it used to be centred over the whole stack and sat on the line between
	     V1 and A1. Sticky, so it stays on screen when the timeline is scrolled. -->
	<span
		data-drop-hint
		style="position:sticky;left:calc(var(--track-header-w) + 12px);display:flex;align-items:center;width:max-content;max-width:calc(100% - var(--track-header-w) - 24px);height:100%;color:var(--text-muted);font-size:12px;pointer-events:none;white-space:nowrap"
		>Drop media here to start cutting.</span
	>
{/if}
{#if drag?.moved && drag.trackId === t.id}
	<!-- Where the dragged clip would land: the lane under the pointer takes a tint. -->
	<span
		aria-hidden="true"
		style="position:absolute;inset:0;background:var(--selection-fill);pointer-events:none"
	></span>
{:else if drag?.moved && t.kind !== drag.kind}
	<!-- A lane the clip cannot go to (a picture does not go to sound) is stood back. -->
	<span
		aria-hidden="true"
		style="position:absolute;inset:0;z-index:23;background:color-mix(in srgb,var(--surface-panel) 55%,transparent);pointer-events:none"
	></span>
{/if}
{#each (diff?.removed ?? []).filter((r) => r.trackId === t.id) as r (r.id)}
	<!-- What the proposal removes, where it stood: hatched under whatever now fills the space, with a
	     strip along the top that stays visible when something does. -->
	<span
		data-diff-removed
		style="position:absolute;left:{r.start * pxPerSec}px;top:5px;height:calc(100% - 10px);width:{Math.max(4, r.dur * pxPerSec)}px;box-sizing:border-box;border:1.5px dashed var(--diff-remove);border-radius:2px;background:repeating-linear-gradient(135deg,color-mix(in srgb,var(--diff-remove) 45%,transparent) 0 1px,var(--diff-remove-surface) 1px 6px);pointer-events:none"
	></span>
	<span
		aria-hidden="true"
		style="position:absolute;left:{r.start * pxPerSec}px;top:0;height:4px;width:{Math.max(4, r.dur * pxPerSec)}px;background:var(--diff-remove);opacity:.85;pointer-events:none;z-index:22"
	></span>
{/each}
{#if t.locked}
	<!-- A locked track refuses every edit, so it looks it: hatched lane, and a padlock that
	     stays at the left edge of the window as the lane scrolls. -->
	<span
		data-lock-badge
		use:tip={`${t.name} is locked — unlock it to edit`}
		style="position:sticky;left:calc(var(--track-header-w) + 6px);top:0;z-index:20;display:inline-flex;align-items:center;margin:2px 0 0 6px;padding:2px 4px;border-radius:3px;pointer-events:none;color:var(--text-on-video);background:color-mix(in srgb,var(--scrim) {NAME_CHIP_PCT}%,transparent)"
		><Icon n="lock" s={10} /></span
	>
{/if}
