<script lang="ts">
	// The library's Motion tab: ready-made entrances, exits and emphases for the clips selected on the timeline.
	// A preset is written as ordinary keyframes on the clip's scale / position / rotation / opacity (relative to the
	// pose it already has), so the Inspector's keyframe rows, the dope sheet and the motion path show and tune what a
	// click wrote; nothing here owns a value. One click is one edit for every selected picture clip.
	import { onMount } from 'svelte';
	import Hint from './Hint.svelte';
	import Tile from './Tile.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import SliderRow from './SliderRow.svelte';
	import { editor } from '#lib/state.svelte';
	import { ui } from '#lib/editor-ui.svelte';
	import { listMotionPresets } from '#lib/api';
	import {
		MOTION_GROUPS,
		MOTION_MODES,
		lengthText,
		motionReason,
		motionTargets,
		parseMotionLength,
		presetTip
	} from '#lib/motion-ui';
	import { MAX_MOTION_INTENSITY, MIN_MOTION_INTENSITY, motionPresets } from '#lib/motion-presets';
	import { TIMES } from '#lib/slider-row';
	import { applyMotion, report } from '#lib/ops';
	import type { MotionMode, MotionPresetInfo } from '#lib/types';

	// The table the page ships until the backend's own answers (the two are the same list).
	let presets = $state<MotionPresetInfo[]>(motionPresets());
	onMount(() => {
		listMotionPresets().then(
			(list) => (presets = list),
			(e) => report(e)
		);
	});

	const selected = $derived(editor.selectedClips);
	const targets = $derived(motionTargets(editor.timeline, selected));
	/** Why the tiles are off, or null when they are on. */
	const reason = $derived(motionReason(editor.timeline, selected));
	const off = $derived(reason !== null);
	/** An edit is in flight, or the proposal is on screen: the tiles ignore a press but keep focus. */
	const hold = $derived(editor.locked);
	const what = $derived(targets.length === 1 ? 'the selected clip' : `the ${targets.length} selected clips`);
</script>

<div style="flex:1;min-height:0;overflow-y:auto;padding:12px">
	{#if reason}
		<div style="margin-bottom:14px"><Hint>{reason}</Hint></div>
	{:else}
		<div style="margin-bottom:12px;font-size:12px;color:var(--text-secondary)">
			Applies to <span style="color:var(--text-primary);font-weight:600">{what}</span>, as one edit.
		</div>
	{/if}

	<div class="k-overline" style="margin:0 0 7px">How</div>
	<div style="display:grid;grid-template-columns:64px 1fr;gap:8px 8px;align-items:center">
		<span style="font-size:12px;color:var(--text-muted)">Mode</span>
		<ChoiceChips
			label="Motion mode"
			options={MOTION_MODES}
			value={ui.motionMode}
			onpick={(m: MotionMode) => (ui.motionMode = m)}
			testid="motion-mode"
		/>
		<label for="motion-length" style="font-size:12px;color:var(--text-muted)">Length</label>
		<span style="display:inline-flex;align-items:center;gap:5px;color:var(--text-muted);font-size:12px">
			<input
				id="motion-length"
				type="text"
				inputmode="decimal"
				autocomplete="off"
				placeholder="default"
				aria-label="Motion length in seconds, blank for the preset’s own"
				value={lengthText(ui.motionLength)}
				onchange={(e) => {
					const v = parseMotionLength(e.currentTarget.value);
					// A number that is no entry goes back to what the field held: nothing was changed.
					if (v !== undefined) ui.motionLength = v;
					e.currentTarget.value = lengthText(ui.motionLength);
				}}
				style="width:96px;background:var(--surface-inset);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);color:var(--text-primary);font-family:var(--font-mono);font-size:12px;padding:4px 6px;text-align:right"
			/>
			<span>s</span>
		</span>
	</div>
	<div style="margin-top:4px">
		<SliderRow
			label="Strength"
			labelWidth={64}
			reserve={false}
			value={ui.motionIntensity}
			spec={{ min: MIN_MOTION_INTENSITY, max: MAX_MOTION_INTENSITY, step: 0.05, neutral: 1, ...TIMES }}
			onCommit={(v) => (ui.motionIntensity = v)}
		/>
	</div>

	{#each MOTION_GROUPS as g (g.kind)}
		<div class="k-overline" style="margin:16px 0 3px">{g.label}</div>
		<div style="font-size:12px;color:var(--text-muted);margin-bottom:7px">{g.hint}</div>
		<div
			role="group"
			aria-label="{g.label} presets"
			style="display:grid;grid-template-columns:repeat(auto-fill,minmax(104px,1fr));gap:6px"
		>
			{#each presets.filter((p) => p.kind === g.kind) as p (p.id)}
				<Tile
					disabled={off}
					pending={hold}
					title={presetTip(p, ui.motionLength, reason)}
					data-testid="motion-{p.id}"
					style="height:auto;min-height:var(--control-sm);padding-block:4px;white-space:normal;line-height:1.25"
					onclick={() => void applyMotion(p.id)}
				>
					{p.label}
				</Tile>
			{/each}
		</div>
	{/each}

	<div style="font-size:12px;color:var(--text-muted);line-height:1.45;margin-top:16px">
		A preset is relative to the clip’s own scale, position and opacity. Add keeps the rest of its animation; Replace
		starts the numbers a preset moves afresh. Tune the keys in the Inspector.
	</div>
</div>
