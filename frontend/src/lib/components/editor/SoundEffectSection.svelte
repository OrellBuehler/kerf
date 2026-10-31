<script lang="ts">
	// What a synthesized sound effect clip is, and the way to reshape it: the same controls as the Sound FX panel's Shape
	// section, but here a change is an edit — it renders the sound again and swaps the clip's audio, keeping its id, its place
	// by the *hit* (not the start), its fades, effects and level. One change, one `Edit sound effect` revision: a slider writes
	// on release (SliderRow), a switch or a new take when pressed. The level is the Volume section's, as for any clip.
	import Btn from './Btn.svelte';
	import SfxShape from './SfxShape.svelte';
	import SfxSketch from './SfxSketch.svelte';
	import { editor, PreviewReadOnly } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { sfxPanel } from '#lib/sfx-panel.svelte';
	import { hueOf, isSfxCancelled, patchSpec } from '#lib/sfx-panel';
	import { sfxPreset } from '#lib/sfx';
	import { toast } from '#lib/notifications.svelte';
	import { failure } from '#lib/failure-copy';
	import { report } from '#lib/ops';
	import type { Asset, Clip, SfxSpec } from '#lib/types';

	let { asset, clip }: { asset: Asset; clip: Clip } = $props();

	const spec = $derived(asset.sound_effect ?? null);
	const preset = $derived(spec ? sfxPreset(spec.kind) : null);
	const owner = $derived(`clip:${clip.id}` as const);
	const playing = $derived(sfxPanel.auditioning(owner));

	// A sound still playing is for a clip that is no longer looked at. (`mine` is read in the effect, not its cleanup: the
	// cleanup runs once the section is gone, when a derived is no longer to be read.)
	$effect(() => {
		const mine = owner;
		return () => {
			if (sfxPanel.owner === mine) sfxPanel.stop();
		};
	});

	async function reshape(patch: Partial<SfxSpec>): Promise<boolean> {
		if (!spec) return false;
		const next = patchSpec(spec, patch);
		const who = owner;
		try {
			await editor.updateSoundEffect(clip.id, next);
		} catch (e) {
			if (isSfxCancelled(e)) return false;
			if (e instanceof PreviewReadOnly) report(e);
			else toast.error(failure("Couldn't reshape the sound effect", e));
			return false;
		}
		// Hear what the change did, unless the cut is playing and it would be heard twice.
		if (!ui.playing) void sfxPanel.auditionSpec(next, who);
		return true;
	}
</script>

{#if spec && preset}
	<div data-testid="sound-effect-section">
		<div class="env"><SfxSketch {preset} hue={hueOf(preset.category)} reverse={spec.reverse} height={30} label="Shape of {preset.label} over its length; the dashed line is where its hit lands" /></div>
		<div style="margin-top:6px">
			<SfxShape {spec} {preset} disabled={editor.locked} onchange={reshape} />
		</div>
		<div style="margin-top:10px;display:flex;gap:8px;align-items:center">
			<Btn
				size="sm"
				variant="secondary"
				icon={playing ? 'circle-stop' : 'play'}
				title={playing ? 'Stop the sound' : 'Hear this sound'}
				data-testid="sound-effect-audition"
				onclick={() => sfxPanel.toggleAudition(spec, owner)}>{playing ? 'Stop' : 'Audition'}</Btn
			>
		</div>
		<p class="note">
			A change renders the sound again and swaps this clip's audio in place. Its hit, fades, effects and level stay where they are.
		</p>
		{#if sfxPanel.noteOf(owner)}
			<p class="note" role="status">{sfxPanel.noteOf(owner)}</p>
		{/if}
	</div>
{/if}

<style>
	.env {
		padding: 5px 7px;
		border-radius: var(--radius-sm);
		background: var(--surface-inset);
		border: var(--line-width) solid var(--border-subtle);
	}
	.note {
		margin: 8px 0 0;
		font-size: var(--text-xs);
		line-height: 1.5;
		color: var(--text-muted);
	}
</style>
