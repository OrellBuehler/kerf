<script lang="ts">
	// One template of the library: its poster (the engine's own picture of it at half its length, shown through an
	// <img> data URL, never inlined), its name, how long it runs, what it can change and the fonts it carries. A click
	// adds it at the playhead; the ⋯ button and a right-click offer Export… and Delete….
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import Tile from './Tile.svelte';
	import { contextMenu, type MenuItem } from '$lib/context-menu.svelte';
	import { editor } from '$lib/state.svelte';
	import { addTemplateHere, deleteTemplateFile, exportTemplateFile, posterOf } from '$lib/template-actions';
	import { controlCount, fontsLine, templateTooltip } from '$lib/template-ui';
	import { timeDisplay } from '$lib/time-display.svelte';
	import type { TemplateInfo } from '$lib/types';

	let { info, aspect }: { info: TemplateInfo; aspect: { width: number; height: number } } = $props();

	const id = $derived(info.id);
	let poster = $state<string | null>(null);

	$effect(() => {
		const wanted = id;
		let live = true;
		poster = null;
		posterOf(wanted).then(
			(url) => {
				if (live) poster = url;
			},
			() => {}
		);
		return () => {
			live = false;
		};
	});

	const fonts = $derived(fontsLine(info.fonts));
	const items = $derived<MenuItem[]>([
		{ label: 'Add at the playhead', icon: 'plus', disabled: editor.locked, action: () => void addTemplateHere(info) },
		{ type: 'separator' },
		{ label: 'Export…', icon: 'upload', action: () => void exportTemplateFile(info) },
		{ label: 'Delete…', icon: 'trash', danger: true, action: () => void deleteTemplateFile(info) }
	]);

	function openMenu(e: MouseEvent) {
		const button = e.currentTarget as HTMLElement;
		const r = button.getBoundingClientRect();
		contextMenu.show(new MouseEvent('contextmenu', { clientX: r.left, clientY: r.bottom + 2 }), items, button);
	}
</script>

<div
	class="card"
	role="group"
	aria-label={info.name}
	data-template={info.id}
	oncontextmenu={(e) => contextMenu.show(e, items)}
>
	<Tile
		title={templateTooltip(info)}
		aria-label="Add {info.name} at the playhead"
		pending={editor.locked}
		style="width:100%;height:auto;flex-direction:column;align-items:stretch;gap:5px;padding:5px"
		onclick={() => void addTemplateHere(info)}
	>
		<span
			style="position:relative;display:grid;place-items:center;aspect-ratio:{aspect.width} / {aspect.height};overflow:hidden;border-radius:var(--radius-sm);background:var(--surface-void);color:var(--text-muted);pointer-events:none"
		>
			{#if poster}
				<img
					src={poster}
					alt=""
					decoding="async"
					draggable="false"
					onerror={() => (poster = null)}
					style="width:100%;height:100%;object-fit:contain;display:block"
				/>
			{:else}
				<Icon n="layout-template" s={18} />
			{/if}
			<span
				data-duration
				style="position:absolute;right:4px;bottom:4px;padding:1px 4px;border-radius:var(--radius-sm);background:color-mix(in srgb,var(--surface-void) 72%,transparent);color:var(--text-on-video);font-family:var(--font-mono);font-size:10px;font-weight:400;line-height:1.4;font-variant-numeric:tabular-nums"
				>{timeDisplay.fmt(info.duration)}</span
			>
		</span>
		<span style="overflow:hidden;text-overflow:ellipsis">{info.name}</span>
		<span
			style="overflow:hidden;text-overflow:ellipsis;font-size:11px;font-weight:400;color:var(--text-muted);font-variant-numeric:tabular-nums"
			>{controlCount(info.controls.length)}</span
		>
		{#if fonts}
			<span style="overflow:hidden;text-overflow:ellipsis;font-size:11px;font-weight:400;color:var(--text-muted)">{fonts}</span>
		{/if}
	</Tile>
	<IconBtn
		class="more"
		size={24}
		title="More actions for {info.name}"
		aria-haspopup="menu"
		onclick={openMenu}
		style="position:absolute;top:8px;right:8px;background:color-mix(in srgb,var(--surface-void) 72%,transparent);color:var(--text-on-video)"
	>
		<Icon n="ellipsis" s={14} />
	</IconBtn>
</div>

<style>
	.card {
		position: relative;
		min-width: 0;
	}
	/* Out of the poster's way until the card is looked at; always there where there is no hover to look with. */
	.card :global(.more) {
		opacity: 0;
	}
	.card:hover :global(.more),
	.card:focus-within :global(.more) {
		opacity: 1;
	}
	@media (hover: none) {
		.card :global(.more) {
			opacity: 1;
		}
	}
</style>
