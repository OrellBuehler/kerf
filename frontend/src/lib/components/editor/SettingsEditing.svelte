<script lang="ts">
	import SettingRow from './SettingRow.svelte';
	import Btn from './Btn.svelte';
	import { settings } from '$lib/settings.svelte';
	import { LENGTH_RANGE, clampSeconds, secondsLabel } from '$lib/editing-prefs';

	/** The field edits a number of seconds; a stepper either side moves it by half a second. */
	function commit(which: 'still_seconds' | 'title_seconds', raw: string, el?: HTMLInputElement) {
		const next = clampSeconds(raw, settings.editing[which]);
		if (el) el.value = String(next);
		void settings.setEditing({ [which]: next });
	}
</script>

{#snippet length(which: 'still_seconds' | 'title_seconds', label: string)}
	<div style="display:flex;align-items:center;gap:6px">
		<Btn
			size="sm"
			variant="secondary"
			aria-label="Shorter {label}"
			disabled={settings.editing[which] <= LENGTH_RANGE.min}
			onclick={() => commit(which, String(settings.editing[which] - LENGTH_RANGE.step))}>−</Btn
		>
		<input
			type="number"
			min={LENGTH_RANGE.min}
			max={LENGTH_RANGE.max}
			step={LENGTH_RANGE.step}
			value={settings.editing[which]}
			aria-label="{label} in seconds"
			data-testid="editing-{which}"
			onchange={(e) => commit(which, e.currentTarget.value, e.currentTarget)}
			style="width:64px;height:28px;padding:0 8px;text-align:right;background:var(--surface-inset);color:var(--text-primary);border:var(--line-width) solid var(--border-default);border-radius:var(--radius-sm);font-family:var(--font-mono);font-size:12px"
		/>
		<span style="font-size:12px;color:var(--text-muted);width:10px">s</span>
		<Btn
			size="sm"
			variant="secondary"
			aria-label="Longer {label}"
			disabled={settings.editing[which] >= LENGTH_RANGE.max}
			onclick={() => commit(which, String(settings.editing[which] + LENGTH_RANGE.step))}>+</Btn
		>
	</div>
{/snippet}

<SettingRow
	id="snap-default"
	label="Snapping on when Kerf starts"
	hint="Clips, the playhead, markers and bar lines pull a drag to them. The magnet in the timeline toolbar (and Edit › Snapping) still turns it on or off for the session — this is only where it starts."
>
	<input
		type="checkbox"
		role="switch"
		aria-label="Snapping on when Kerf starts"
		data-testid="editing-snap"
		checked={settings.editing.snap}
		onchange={(e) => void settings.setEditing({ snap: e.currentTarget.checked })}
		style="accent-color:var(--kerf-500);cursor:pointer;width:16px;height:16px"
	/>
</SettingRow>

<SettingRow
	id="still-length"
	label="Length of a still image"
	hint="How long a photo or graphic lasts when it is added to the timeline. A still has no footage to run out of, so it can be trimmed to any length afterwards."
>
	{@render length('still_seconds', 'still image length')}
</SettingRow>

<SettingRow
	id="title-length"
	label="Length of a new title"
	hint="How long a title lasts when it is added at the playhead ({secondsLabel(settings.editing.title_seconds)} now)."
>
	{@render length('title_seconds', 'title length')}
</SettingRow>
