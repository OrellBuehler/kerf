<script lang="ts">
	// The project's frame, in the one place it is set: the shape the cut is made for, how footage of another shape
	// meets it, and the rate it plays and exports at. The Preview is this frame and so is an export, unless an export
	// overrides it for itself. The timeline's Frame button and View › Project frame run the same commands, so they
	// show what is set here and set the same thing (`menu-commands.ts`); this panel is where all three are in view.
	import ChoiceChips from './ChoiceChips.svelte';
	import FormRow from './FormRow.svelte';
	import { editor } from '#lib/state.svelte';
	import { DELIVERY_PRESETS, presetFor, rateOptions } from '#lib/delivery-formats';
	import { FITS } from '#lib/export-presets';
	import { setDeliveryFit, setDeliveryFps, setDeliveryPreset } from '#lib/menu-commands';
	import { PREVIEW_READ_ONLY_REASON } from '#lib/proposal-ui';
	import type { Fit } from '#lib/types';

	const format = $derived(editor.timeline.format);
	const shape = $derived(presetFor(format).id);
	const rates = $derived(rateOptions(format, editor.fps));
	const fit = $derived(FITS.find((f) => f.id === format?.fit));
	// A proposal being previewed is read-only, as the timeline's button is.
	const held = $derived(editor.previewingStaged);
</script>

<FormRow
	label="Shape"
	top
	hint={held
		? PREVIEW_READ_ONLY_REASON
		: 'What the Preview shows and an export renders. The timeline’s Frame button sets it too.'}
>
	{#snippet control()}
		<ChoiceChips
			label="Project frame shape"
			value={shape}
			options={DELIVERY_PRESETS.map((p) => ({ value: p.id, label: p.label, hint: p.hint, disabled: held }))}
			onpick={(id) => void setDeliveryPreset(id)}
		/>
	{/snippet}
</FormRow>
{#if format}
	<FormRow label="Fit" hint={fit?.hint}>
		{#snippet control(id, describedby)}
			<select
				{id}
				aria-describedby={describedby}
				class="k-select"
				style="width:100%;max-width:240px"
				disabled={held}
				value={format.fit}
				onchange={(e) => void setDeliveryFit(e.currentTarget.value as Fit)}
			>
				{#each FITS as f (f.id)}
					<option value={f.id}>{f.label}</option>
				{/each}
			</select>
		{/snippet}
	</FormRow>
{/if}
<FormRow
	label="Frame rate"
	hint={format ? 'Exports, playback and stills use it.' : 'Needs a shape: the rate belongs to the frame.'}
>
	{#snippet control(id, describedby)}
		{#if format}
			<select
				{id}
				aria-describedby={describedby}
				class="k-select"
				style="width:100%;max-width:240px"
				disabled={held}
				value={rates.value}
				onchange={(e) => void setDeliveryFps(rates.options.find((o) => o.value === e.currentTarget.value)?.fps ?? null)}
			>
				{#each rates.options as o (o.value)}
					<option value={o.value}>{o.label}</option>
				{/each}
			</select>
		{:else}
			<!-- no frame, no rate to choose: the footage's own is a fact, not a dead control -->
			<span
				aria-describedby={describedby}
				style="font-family:var(--font-mono);font-size:var(--text-sm);line-height:var(--control-sm);color:var(--text-primary)"
				>{rates.options[0].label}</span
			>
		{/if}
	{/snippet}
</FormRow>
