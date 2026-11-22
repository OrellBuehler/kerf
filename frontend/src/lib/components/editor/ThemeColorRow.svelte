<script lang="ts">
	// One colour of the theme being edited: a swatch that opens the picker, the name and where it shows, the hex it can be
	// typed as, a way back to the built-in it was copied from, and — when it is part of a pair the UI draws that is hard to read —
	// the pair and its ratio. The picker's many `input` events and the typed text go to the editor's session as they come; `change`
	// (the picker closed, Enter, focus away) ends the step.
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import { parseTypedHex, type ColorToken } from '#lib/theme';
	import { describeFinding, type Finding } from '#lib/theme-contrast';
	import { tip } from '#lib/tip';

	let {
		token,
		label,
		hint,
		value,
		original,
		changed,
		findings = [],
		onpick,
		onend,
		onreset
	}: {
		token: ColorToken;
		label: string;
		hint: string;
		value: string;
		/** What it goes back to, for the reset's tooltip. */
		original: string;
		changed: boolean;
		/** The pairs this colour is part of that the theme does not meet, worst first. */
		findings?: readonly Finding[];
		onpick: (hex: string) => void;
		onend: () => void;
		onreset: () => void;
	} = $props();

	let typed = $state<string | null>(null);
	const shown = $derived(typed ?? value);
	const invalid = $derived(typed !== null && parseTypedHex(typed) === null);
	const worst = $derived(findings[0]);
</script>

<div data-token={token} style="display:grid;grid-template-columns:30px minmax(0,1fr) 78px 24px;gap:4px 8px;align-items:center;padding:4px 0">
	<input
		type="color"
		{value}
		aria-label="{label} color"
		oninput={(e) => onpick(e.currentTarget.value)}
		onchange={onend}
		style="width:30px;height:24px;padding:0;border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-xs);background:none;cursor:pointer"
	/>
	<div style="min-width:0;line-height:1.3">
		<div style="font-size:12px;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{label}</div>
		<div use:tip={hint} style="font-size:11px;color:var(--text-muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{hint}</div>
	</div>
	<input
		type="text"
		value={shown}
		aria-label="{label} hex"
		aria-invalid={invalid ? 'true' : undefined}
		spellcheck="false"
		autocomplete="off"
		maxlength="7"
		oninput={(e) => {
			typed = e.currentTarget.value;
			const hex = parseTypedHex(typed);
			// A full colour is applied as the last digit lands; a part of one waits for Enter or focus away.
			if (hex !== null && typed.replace('#', '').length === 6) onpick(hex);
		}}
		onchange={(e) => {
			const hex = parseTypedHex(e.currentTarget.value);
			if (hex !== null) onpick(hex);
			typed = null;
			onend();
		}}
		onblur={() => (typed = null)}
		style="width:78px;height:24px;box-sizing:border-box;padding:0 6px;font:var(--type-mono);font-size:11px;background:var(--surface-inset);color:var(--text-primary);border:var(--line-width) solid {invalid
			? 'var(--red-400)'
			: 'var(--border-strong)'};border-radius:var(--radius-xs)"
	/>
	{#if changed}
		<IconBtn title="Put back {original}" size={24} onclick={onreset}><Icon n="rotate-ccw" s={12} /></IconBtn>
	{:else}
		<span></span>
	{/if}
	{#if worst}
		<div
			data-contrast
			use:tip={findings.slice(0, 6).map(describeFinding).join('\n')}
			style="grid-column:2 / 5;display:flex;align-items:center;gap:5px;font-size:11px;line-height:1.3;color:var(--orange-400)"
		>
			<Icon n="alert-triangle" s={11} />
			<span style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">
				{describeFinding(worst)}{findings.length > 1 ? ` · ${findings.length - 1} more` : ''}
			</span>
		</div>
	{/if}
</div>
