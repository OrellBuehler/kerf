<script lang="ts">
	// The library's Audio tab: audio effects for the clip selected on the
	// timeline, and the voiceover entry point. Like the Effects tab it is the
	// quick way in — the Inspector's Audio effects section tunes what lands here.
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { AUDIO_EFFECT_PRESETS, effectLabel, freshEffect } from '$lib/effect-presets';
	import { attempt } from '$lib/ops';

	const clip = $derived(editor.selectedClip);
	const asset = $derived(clip ? editor.assets.find((a) => a.id === clip.asset_id) : undefined);
	const hasAudio = $derived(asset?.streams.some((s) => s.kind === 'audio') ?? false);
	const effects = $derived(clip?.audio ?? []);

	const reason = $derived(
		!clip
			? 'Select a clip on the timeline to apply an audio effect.'
			: !hasAudio
				? 'This clip has no sound — select a clip with audio.'
				: null
	);
	const off = $derived(reason !== null || editor.busy);

	const tile =
		'display:flex;flex-direction:column;align-items:flex-start;gap:2px;padding:8px 9px;border-radius:var(--radius-sm);border:var(--line-width) solid var(--border-default);background:var(--surface-raised);color:var(--text-primary);font-size:12px;font-weight:500;text-align:left;cursor:pointer';
	const head =
		'margin:14px 0 7px;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)';
</script>

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

	<div style="{head};margin-top:{reason ? '14px' : '2px'}">Audio effects</div>
	<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(76px,1fr));gap:6px">
		{#each AUDIO_EFFECT_PRESETS as p (p.key)}
			<button
				style="{tile};{off ? 'opacity:.5;cursor:default' : ''}"
				disabled={off}
				title={reason ?? p.hint}
				onclick={() => clip && void attempt(() => editor.setAudioEffects(clip.id, [...effects, freshEffect(p)]))}
			>
				{p.label}
			</button>
		{/each}
	</div>

	{#if clip && hasAudio}
		<div style={head}>On this clip · {effects.length}</div>
		{#if effects.length === 0}
			<div style="font-size:12px;color:var(--text-muted);line-height:1.4">No effects yet.</div>
		{/if}
		{#each effects as e, i (i)}
			<div style="display:flex;align-items:center;gap:6px;padding:2px 0">
				<span style="flex:1;font-size:12px;color:var(--text-secondary)">{effectLabel(e)}</span>
				<button
					onclick={() => void attempt(() => editor.setAudioEffects(clip.id, effects.filter((_, j) => j !== i)))}
					disabled={editor.busy}
					title="Remove {effectLabel(e)}"
					aria-label="Remove {effectLabel(e)}"
					style="background:transparent;border:none;color:var(--text-muted);cursor:pointer;font-size:16px;line-height:1;min-width:28px;min-height:28px;padding:2px 5px"
					>×</button
				>
			</div>
		{/each}
		<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-top:6px">
			Tune an effect's values in the Inspector's Audio effects section.
		</div>
	{/if}

	<div style={head}>Voiceover</div>
	<div style="font-size:12px;color:var(--text-muted);line-height:1.45;margin-bottom:8px">
		Write a script and Kerf speaks it onto the VO track — it can caption the cut in the same step.
	</div>
	<Btn size="sm" variant="secondary" icon="mic" disabled={editor.busy} onclick={() => ui.openVoiceover()}>Write a voiceover…</Btn>
</div>
