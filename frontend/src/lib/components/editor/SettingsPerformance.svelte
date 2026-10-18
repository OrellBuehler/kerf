<script lang="ts">
	import SettingRow from './SettingRow.svelte';
	import { settings, CPU_PRESETS } from '#lib/settings.svelte';
	import { chip } from '#lib/settings-ui';
	import { tip } from '#lib/tip';

	const cores = $derived(settings.cpuCores);
	const threads = $derived(settings.cpuThreads);
	const percent = $derived(settings.cpuPercent);
	const preset = $derived(settings.cpuPreset);

	// What the number actually buys, in the terms the complaint arrives in:
	// how much of the machine is left for everything that is not Kerf.
	const spare = $derived(Math.max(0, cores - threads));
</script>

<SettingRow id="cpu-limit" label="CPU limit" stacked>
	{#snippet hint()}
		Kerf runs one heavy job at a time — an analysis pass, a transcription, a proxy, an export — and this is how much of
		the machine that job may take. Left alone, FFmpeg takes all of it.
	{/snippet}
	<div role="group" aria-label="CPU budgets" style="display:flex;flex-wrap:wrap;gap:6px">
		{#each CPU_PRESETS as p (p.id)}
			<button
				type="button"
				use:tip={p.hint}
				aria-pressed={percent === p.percent}
				onclick={() => settings.setCpuPercent(p.percent)}
				style={chip(percent === p.percent)}
			>
				{p.label}
			</button>
		{/each}
	</div>

	<div style="margin-top:14px;display:flex;align-items:center;gap:10px">
		<input
			type="range"
			min={settings.cpuMinPercent}
			max="100"
			step="5"
			value={percent}
			aria-label="CPU limit"
			oninput={(e) => (settings.cpuPercent = Number(e.currentTarget.value))}
			onchange={(e) => settings.setCpuPercent(Number(e.currentTarget.value))}
			style="flex:1"
		/>
		<span style="flex:none;width:52px;text-align:right;font-family:var(--font-mono);font-size:13px;color:var(--text-primary)"
			>{percent}%</span
		>
	</div>

	<div
		data-selectable
		style="margin-top:12px;padding:9px 11px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);line-height:1.6"
	>
		{threads} of {cores}
		{cores === 1 ? 'core' : 'cores'} for Kerf · {spare === 0 ? 'nothing held back' : `${spare} left for everything else`}
	</div>

	<p style="margin:10px 0 0;font-size:12px;line-height:1.55;color:var(--text-secondary)">
		{#if preset}
			{preset.hint}
		{:else}
			Renders scale roughly with the share you allow; everything else on the machine gets the rest.
		{/if}
	</p>

	{#if percent < 100}
		<p style="margin:8px 0 0;font-size:12px;line-height:1.55;color:var(--text-muted)">
			Below 100%, background work also runs at lower scheduling priority, so the window you are looking at always wins a
			core when it wants one.
		</p>
	{/if}

	<p style="margin:12px 0 0;font-size:12px;line-height:1.55;color:var(--text-muted)">
		A job already running keeps the cores it started with — FFmpeg cannot be told otherwise mid-render. The next one picks
		this up.
	</p>
</SettingRow>
