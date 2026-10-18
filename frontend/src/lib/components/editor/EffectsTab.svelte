<script lang="ts">
	// The library's Effects tab: color looks and video effects, applied to the
	// clip selected on the timeline. Nothing here owns a value — a look is a
	// `Color` the Inspector's sliders then show, an effect is an entry in the
	// clip's chain the Inspector then tunes — so this is the quick way in, and
	// the Inspector stays the place to adjust.
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Chip from './Chip.svelte';
	import Tile from './Tile.svelte';
	import Hint from './Hint.svelte';
	import { editor } from '$lib/state.svelte';
	import { COLOR_LOOKS, activeLook } from '$lib/style-presets';
	import { VIDEO_EFFECT_PRESETS, effectLabel, freshEffect } from '$lib/effect-presets';
	import { DEFAULT_COLOR } from '$lib/types';
	import { attempt } from '$lib/ops';

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
	const off = $derived(reason !== null);
	/** An edit is in flight, or the proposal is on screen: the controls ignore a press but keep focus. */
	const hold = $derived(editor.locked);
</script>

{#snippet head(label: string)}
	<div class="k-overline" style="margin:14px 0 7px">{label}</div>
{/snippet}

<div style="flex:1;min-height:0;overflow-y:auto;padding:12px">
	{#if reason}
		<div style="margin-bottom:14px"><Hint>{reason}</Hint></div>
	{/if}

	<div style="display:flex;align-items:center;gap:8px;margin:0 0 7px">
		<span class="k-overline" style="flex:1">Color looks</span>
		<Btn
			variant="ghost"
			size="sm"
			disabled={off}
				pending={hold}
			title={reason ?? 'Back to the ungraded picture'}
			onclick={() => clip && void attempt(() => editor.setColor(clip.id, DEFAULT_COLOR))}>Reset</Btn
		>
	</div>
	<div style="display:flex;gap:5px;flex-wrap:wrap">
		{#each COLOR_LOOKS as look (look.id)}
			{@const on = !!clip && activeLook(color)?.id === look.id}
			<Chip
				pressed={on}
				disabled={off}
				pending={hold}
				title={reason ?? `${look.label} — sets brightness, contrast, saturation, warmth and gamma`}
				onclick={() => clip && void attempt(() => editor.setColor(clip.id, look.color))}>{look.label}</Chip
			>
		{/each}
	</div>

	{@render head('Video effects')}
	<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(96px,1fr));gap:6px">
		{#each VIDEO_EFFECT_PRESETS as p (p.key)}
			<Tile
				disabled={off}
				pending={hold}
				title={reason ?? p.hint}
				onclick={() => clip && void attempt(() => editor.setVideoEffects(clip.id, [...effects, freshEffect(p)]))}
			>
				{p.label}
			</Tile>
		{/each}
	</div>

	{#if clip && hasPicture}
		{@render head(`On this clip · ${effects.length}`)}
		{#if effects.length === 0}
			<div style="font-size:12px;color:var(--text-muted);line-height:1.4">No effects yet.</div>
		{/if}
		{#each effects as e, i (i)}
			<div style="display:flex;align-items:center;gap:6px;padding:2px 0">
				<span style="flex:1;min-width:0;font-size:12px;color:var(--text-secondary)">{effectLabel(e)}</span>
				<IconBtn
					size={28}
					data-tone="quiet"
					title={`Remove ${effectLabel(e)}`}
					pending={editor.locked}
					onclick={() => void attempt(() => editor.setVideoEffects(clip.id, effects.filter((_, j) => j !== i)))}
					><Icon n="x" s={14} /></IconBtn
				>
			</div>
		{/each}
		<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-top:6px">
			Tune an effect's values in the Inspector's Video effects section.
		</div>
	{/if}
</div>
