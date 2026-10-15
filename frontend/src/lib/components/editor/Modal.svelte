<script lang="ts">
	// The shell every dialog is drawn in: the scrim, the panel, a header with a named close button,
	// and a footer. What differs between dialogs is data: how wide it is, when it cannot be closed
	// (`busy`), whether a stray click on the scrim may close it (`dismissOnScrim`), where focus
	// starts (`initial`) and whether it is kept in the dialog when what had it goes (`rescue`).
	// The body and the footer's contents are the dialog's own.
	import type { Snippet } from 'svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import { mayClose, scrimClick, trapFocus, type CloseVia } from '#lib/modal';

	let {
		title,
		icon,
		width = 480,
		height = undefined,
		onClose,
		busy = false,
		dismissOnScrim = true,
		initial = undefined,
		rescue = false,
		children,
		footer = undefined
	}: {
		title: string;
		icon: string;
		/** px; the panel is never wider than the window. */
		width?: number;
		/** px; a fixed height (Settings), else the panel is as tall as its content. */
		height?: number;
		onClose: () => void;
		/** A run is under way: Escape, the scrim and the close button are all off. */
		busy?: boolean;
		/** Off for a dialog holding typed input or chosen options: a stray click must not discard them. */
		dismissOnScrim?: boolean;
		/** A selector for the field that takes focus on open, else the dialog itself does. */
		initial?: string;
		/** Move focus to the `[data-rescue]` action when the control that had it is replaced or disabled. */
		rescue?: boolean;
		children: Snippet;
		footer?: Snippet;
	} = $props();

	const uid = $props.id();
	const titleId = `${uid}-title`;
	const rules = $derived({ busy, dismissOnScrim });
	let pressedOnScrim = false;

	function close(via: CloseVia) {
		if (mayClose(via, rules)) onClose();
	}
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	use:trapFocus={{ initial, rescue }}
	role="dialog"
	aria-modal="true"
	aria-labelledby={titleId}
	tabindex="-1"
	onpointerdown={(e) => (pressedOnScrim = e.target === e.currentTarget)}
	onclick={(e) => {
		const hit = scrimClick(pressedOnScrim, e.target, e.currentTarget);
		pressedOnScrim = false;
		if (hit) close('scrim');
	}}
	onkeydown={(e) => {
		if (e.key === 'Escape') close('escape');
		e.stopPropagation();
	}}
	style="position:fixed;inset:0;z-index:50;background:color-mix(in srgb,var(--scrim) 55%,transparent);display:flex;align-items:center;justify-content:center;padding:24px"
>
	<div
		style="width:min({width}px,100%);{height ? `height:min(${height}px,100%);` : ''}max-height:100%;display:flex;flex-direction:column;background:var(--surface-panel);border:var(--line-width) solid var(--border-default);border-radius:var(--radius-md);box-shadow:var(--shadow-lg);overflow:hidden"
	>
		<div
			style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 14px;border-bottom:var(--line-width) solid var(--border-default)"
		>
			<Icon n={icon} s={15} color="var(--text-secondary)" />
			<span id={titleId} style="font:var(--type-ui);font-weight:600;color:var(--text-primary);flex:1">{title}</span>
			{#if !busy}
				<IconBtn title="Close" size={28} onclick={() => close('button')}><Icon n="x" s={14} /></IconBtn>
			{/if}
		</div>

		{@render children()}

		{#if footer}
			<div style="flex:none;border-top:var(--line-width) solid var(--border-default)">
				{@render footer()}
			</div>
		{/if}
	</div>
</div>
