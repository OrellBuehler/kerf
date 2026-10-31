<script lang="ts">
	// How a sound is shaped: its length (within what the kind allows), pitch, intensity, tail, whether it plays backwards and
	// which take of it (the seed). It edits a spec and nothing else — what a change *does* is the caller's: the Sound FX panel
	// writes it to its draft (nothing reaches the cut until Add), the Inspector's section renders the clip again. Sliders are
	// `SliderRow`s, so a drag is one change on release, Escape or a lost pointer drops it, and a double-click rests the value.
	import SliderRow from './SliderRow.svelte';
	import Switch from './Switch.svelte';
	import Btn from './Btn.svelte';
	import { GAIN_SPEC, INTENSITY_SPEC, PITCH_SPEC, TAIL_SPEC, lengthSpec, nextSeed } from '#lib/sfx-panel';
	import { resolvedDuration } from '#lib/sfx';
	import type { SfxPreset, SfxSpec } from '#lib/types';

	let {
		spec,
		preset,
		onchange,
		disabled = false,
		gain = undefined,
		ongain = undefined
	}: {
		spec: SfxSpec;
		preset: SfxPreset;
		/** A change to apply, as the fields it moves. True when it took, false when it was refused (a switch is put back). */
		onchange: (patch: Partial<SfxSpec>) => boolean | Promise<boolean>;
		disabled?: boolean;
		/** The clip's gain in dB, with a row for it, when the caller owns one (the panel does; a clip has its Volume section). */
		gain?: number;
		ongain?: (db: number) => unknown;
	} = $props();

	const length = $derived(resolvedDuration(spec));
	const lengthRow = $derived(lengthSpec(preset));
	const reverse = $derived(spec.reverse);

	async function flip(checked: boolean, box: HTMLInputElement) {
		if (!(await onchange({ reverse: checked }))) box.checked = !checked;
	}
</script>

<div data-testid="sfx-shape" role="group" aria-label="Shape" style="display:flex;flex-direction:column;gap:1px">
	<SliderRow label="Length" value={length} spec={lengthRow} {disabled} reserve={false} onCommit={(v) => onchange({ duration_s: v })} />
	<SliderRow label="Pitch" value={spec.pitch} spec={PITCH_SPEC} {disabled} reserve={false} onCommit={(v) => onchange({ pitch: v })} />
	<SliderRow label="Intensity" value={spec.intensity} spec={INTENSITY_SPEC} {disabled} reserve={false} onCommit={(v) => onchange({ intensity: v })} />
	<SliderRow label="Tail" value={spec.tail} spec={TAIL_SPEC} {disabled} reserve={false} onCommit={(v) => onchange({ tail: v })} />
	{#if gain !== undefined && ongain}
		<SliderRow label="Gain" value={gain} spec={GAIN_SPEC} {disabled} reserve={false} onCommit={(v) => ongain(v)} />
	{/if}
	<div style="display:flex;align-items:center;gap:8px;margin-top:4px;min-height:var(--control-sm)">
		<Switch checked={reverse} label="Reverse" text="Reverse" pending={disabled} onchange={flip} />
		<span style="flex:1"></span>
		<span data-testid="sfx-seed" style="font-family:var(--font-mono);font-size:var(--text-xs);color:var(--text-muted)" aria-label="Take {spec.seed}">#{spec.seed}</span>
		<Btn
			size="sm"
			icon="dices"
			pending={disabled}
			title="The same sound with different details — a new take"
			onclick={() => onchange({ seed: nextSeed(spec.seed) })}>Variation</Btn
		>
	</div>
</div>
