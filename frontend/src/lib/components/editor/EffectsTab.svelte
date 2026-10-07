<script lang="ts">
	// The library's Effects tab: color looks and video effects, applied to the
	// clip selected on the timeline. Nothing here owns a value — a look is a
	// `Color` the Inspector's sliders then show, an effect is an entry in the
	// clip's chain the Inspector then tunes — so this is the quick way in, and
	// the Inspector stays the place to adjust.
	import Icon from './Icon.svelte';
	import { editor } from '$lib/state.svelte';
	import { COLOR_LOOKS, activeLook } from '$lib/style-presets';
	import { VIDEO_EFFECT_PRESETS, effectLabel, freshEffect } from '$lib/effect-presets';
	import { DEFAULT_COLOR } from '$lib/types';
	import { attempt } from '$lib/ops';
	import { chip } from '$lib/chip';

	const clip = $derived(editor.selectedClip);
	const asset = $derived(clip ? editor.assets.find((a) => a.id === clip.asset_id) : undefined);
	const hasPicture = $derived(asset?.streams.some((s) => s.kind === 'video') ?? false);
	const effects = $derived(clip?.effects ?? []);
	const color = $derived(clip?.color ?? DEFAULT_COLOR);

	/** Why the controls are off, or null when they are on. */
	const reason = $derived(
		!clip
			? 'Select a clip on the timeline to apply a look or an effect.'
			: !hasPicture
				? 'This clip has no picture — select a video clip.'
				: null
	);
	const off = $derived(reason !== null || editor.busy);

	const tile =
		'display:flex;flex-direction:column;align-items:flex-start;gap:2px;padding:8px 9px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-default);background:var(--surface-raised);color:var(--text-primary);font-size:12px;font-weight:500;text-align:left;cursor:pointer';
</script>

{#snippet head(label: string)}
	<div
		style="margin:14px 0 7px;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
	>
		{label}
	</div>
{/snippet}

<div style="flex:1;min-height:0;overflow-y:auto;padding:12px">
	{#if reason}
		<div
			role="status"
			style="display:flex;gap:7px;align-items:flex-start;padding:8px 10px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);font-size:12px;line-height:1.45;color:var(--text-muted)"
		>
			<span style="margin-top:1px"><Icon n="lightbulb" s={13} color="var(--text-muted)" /></span>
			<span>{reason}</span>
		</div>
	{/if}

	<div style="margin:{reason ? '14px' : '2px'} 0 7px;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)">
		Color looks
	</div>
	<div style="display:flex;gap:5px;flex-wrap:wrap">
		{#each COLOR_LOOKS as look (look.id)}
			<button
				style="{chip(!!clip && activeLook(color)?.id === look.id)};{off ? 'opacity:.5;cursor:default' : ''}"
				disabled={off}
				title={reason ?? `${look.label} — sets brightness, contrast, saturation, warmth and gamma`}
				onclick={() => clip && void attempt(() => editor.setColor(clip.id, look.color))}>{look.label}</button
			>
		{/each}
		<button
			style="{chip(false)};{off ? 'opacity:.5;cursor:default' : ''}"
			disabled={off}
			title={reason ?? 'Back to the ungraded picture'}
			onclick={() => clip && void attempt(() => editor.setColor(clip.id, DEFAULT_COLOR))}>Reset</button
		>
	</div>

	{@render head('Video effects')}
	<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(76px,1fr));gap:6px">
		{#each VIDEO_EFFECT_PRESETS as p (p.key)}
			<button
				style="{tile};{off ? 'opacity:.5;cursor:default' : ''}"
				disabled={off}
				title={reason ?? p.hint}
				onclick={() => clip && void attempt(() => editor.setVideoEffects(clip.id, [...effects, freshEffect(p)]))}
			>
				{p.label}
			</button>
		{/each}
	</div>

	{#if clip && hasPicture}
		{@render head(`On this clip · ${effects.length}`)}
		{#if effects.length === 0}
			<div style="font-size:12px;color:var(--text-muted);line-height:1.4">No effects yet.</div>
		{/if}
		{#each effects as e, i (i)}
			<div style="display:flex;align-items:center;gap:6px;padding:2px 0">
				<span style="flex:1;font-size:12px;color:var(--text-secondary)">{effectLabel(e)}</span>
				<button
					onclick={() => void attempt(() => editor.setVideoEffects(clip.id, effects.filter((_, j) => j !== i)))}
					disabled={editor.busy}
					title="Remove {effectLabel(e)}"
					aria-label="Remove {effectLabel(e)}"
					style="background:transparent;border:none;color:var(--text-muted);cursor:pointer;font-size:16px;line-height:1;min-width:28px;min-height:28px;padding:2px 5px"
					>×</button
				>
			</div>
		{/each}
		<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-top:6px">
			Tune an effect's values in the Inspector's Video effects section.
		</div>
	{/if}
</div>
