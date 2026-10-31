<script lang="ts">
	// One sound in the Sound FX grid: its glyph and name over a sketch of how it behaves in time, in the colour of its
	// category. A `Tile` made taller — the grid is a listbox (`SoundFx.svelte` walks it with the arrow keys), so this is an
	// option: selected when it is the one being shaped. Click picks it, Enter or a double-click adds it, a drag takes it to a lane.
	import Tile from './Tile.svelte';
	import Icon from './Icon.svelte';
	import SfxSketch from './SfxSketch.svelte';
	import { glyphOf, hueOf } from '#lib/sfx-panel';
	import type { SfxPreset } from '#lib/types';

	let {
		preset,
		selected,
		tabbable,
		dragging = false,
		onselect,
		onadd,
		onpointerdown
	}: {
		preset: SfxPreset;
		selected: boolean;
		/** The one tile of the grid in the tab order (the selected one, or the first). */
		tabbable: boolean;
		/** This sound is being dragged to the timeline. */
		dragging?: boolean;
		onselect: () => void;
		onadd: () => void;
		onpointerdown: (e: PointerEvent) => void;
	} = $props();

	const hue = $derived(hueOf(preset.category));
</script>

<Tile
	class="sfx-tile"
	role="option"
	aria-selected={selected}
	tabindex={tabbable ? 0 : -1}
	data-kind={preset.kind}
	data-dragging={dragging ? '' : undefined}
	title={{ text: preset.label, detail: preset.description }}
	onclick={onselect}
	ondblclick={onadd}
	{onpointerdown}
>
	<span class="top">
		<Icon n={glyphOf(preset.kind)} s={13} color={hue} />
		<span class="name">{preset.label}</span>
	</span>
	<SfxSketch {preset} {hue} height={15} />
</Tile>

<style>
	:global(.k-tile.sfx-tile) {
		height: auto;
		flex-direction: column;
		align-items: stretch;
		justify-content: space-between;
		gap: 5px;
		padding: 7px 8px 6px;
		text-overflow: clip;
		user-select: none;
		touch-action: none;
	}
	:global(.k-tile.sfx-tile[aria-selected='true']),
	:global(.k-tile.sfx-tile[aria-selected='true']:hover:not(:disabled):not([aria-disabled='true'])) {
		border-color: var(--kerf-500);
		background: var(--selection-fill);
	}
	:global(.k-tile.sfx-tile[data-dragging]) {
		opacity: 0.55;
	}
	.top {
		display: flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
	}
	.name {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
