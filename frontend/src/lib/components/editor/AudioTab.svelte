<script lang="ts">
	// The library's Audio tab: audio effects for the clip selected on the
	// timeline, and the voiceover entry point. Like the Effects tab it is the
	// quick way in — the Inspector's Audio effects section tunes what lands here.
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import Tile from './Tile.svelte';
	import Hint from './Hint.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { AUDIO_EFFECT_PRESETS, effectLabel, freshEffect } from '#lib/effect-presets';
	import { attempt } from '#lib/ops';

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
</script>

<div style="flex:1;min-height:0;overflow-y:auto;padding:12px">
	{#if reason}
		<div style="margin-bottom:14px"><Hint>{reason}</Hint></div>
	{/if}

	<div class="k-overline" style="margin:0 0 7px">Audio effects</div>
	<div style="display:grid;grid-template-columns:repeat(auto-fill,minmax(96px,1fr));gap:6px">
		{#each AUDIO_EFFECT_PRESETS as p (p.key)}
			<Tile
				disabled={off}
				title={reason ?? p.hint}
				onclick={() => clip && void attempt(() => editor.setAudioEffects(clip.id, [...effects, freshEffect(p)]))}
			>
				{p.label}
			</Tile>
		{/each}
	</div>

	{#if clip && hasAudio}
		<div class="k-overline" style="margin:14px 0 7px">On this clip · {effects.length}</div>
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
					onclick={() => void attempt(() => editor.setAudioEffects(clip.id, effects.filter((_, j) => j !== i)))}
					><Icon n="x" s={14} /></IconBtn
				>
			</div>
		{/each}
		<div style="font-size:12px;color:var(--text-muted);line-height:1.4;margin-top:6px">
			Tune an effect's values in the Inspector's Audio effects section.
		</div>
	{/if}

	<div class="k-overline" style="margin:14px 0 7px">Voiceover</div>
	<div style="font-size:12px;color:var(--text-muted);line-height:1.45;margin-bottom:8px">
		Write a script and Kerf speaks it onto the VO track — it can caption the cut in the same step.
	</div>
	<Btn size="sm" variant="secondary" icon="mic" pending={editor.locked} onclick={() => ui.openVoiceover()}>Write a voiceover…</Btn>

	<div class="k-overline" style="margin:14px 0 7px">Music</div>
	<div style="font-size:12px;color:var(--text-muted);line-height:1.45;margin-bottom:8px">
		Write a chord progression and Kerf plays it onto the M track — it can fit the tempo to the cuts in the same step.
	</div>
	<Btn size="sm" variant="secondary" icon="music" pending={editor.locked} onclick={() => ui.openCompose()}>Compose music…</Btn>
</div>
