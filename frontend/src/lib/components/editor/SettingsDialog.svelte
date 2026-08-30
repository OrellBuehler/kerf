<script lang="ts">
	// Kerf's preferences. One section so far — how much of the machine the media
	// engine may take — but the shape (a section list on the left, panels on the
	// right) is what the next one slots into.
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { settings, CPU_PRESETS } from '$lib/settings.svelte';

	let { onClose }: { onClose: () => void } = $props();

	let dialogEl = $state<HTMLDivElement | null>(null);
	$effect(() => {
		dialogEl?.focus();
	});

	// Sections are a list rather than markup so adding one is a data change.
	const SECTIONS = [{ id: 'performance', label: 'Performance', icon: 'sliders-horizontal' }] as const;
	let section = $state<(typeof SECTIONS)[number]['id']>('performance');

	const cores = $derived(settings.cpuCores);
	const threads = $derived(settings.cpuThreads);
	const percent = $derived(settings.cpuPercent);
	const preset = $derived(settings.cpuPreset);

	// What the number actually buys, in the terms the complaint arrives in:
	// how much of the machine is left for everything that is not Kerf.
	const spare = $derived(Math.max(0, cores - threads));

	const chip = (active: boolean) =>
		`padding:5px 10px;border-radius:999px;font-size:12px;cursor:pointer;white-space:nowrap;border:1px solid ${
			active ? 'var(--kerf-500)' : 'var(--border-strong)'
		};background:${
			active ? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)' : 'var(--surface-inset)'
		};color:${active ? 'var(--text-primary)' : 'var(--text-secondary)'}`;
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	bind:this={dialogEl}
	role="dialog"
	aria-modal="true"
	aria-label="Settings"
	tabindex="-1"
	onclick={onClose}
	onkeydown={(e) => {
		if (e.key === 'Escape') onClose();
		e.stopPropagation();
	}}
	style="position:fixed;inset:0;z-index:50;background:rgba(0,0,0,.55);display:flex;align-items:center;justify-content:center;padding:24px"
>
	<div
		onclick={(e) => e.stopPropagation()}
		style="width:620px;max-width:100%;max-height:100%;display:flex;flex-direction:column;background:var(--surface-panel);border:1px solid var(--border-default);border-radius:var(--radius-md);box-shadow:var(--shadow-lg,0 24px 60px rgba(0,0,0,.5));overflow:hidden"
	>
		<div
			style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 14px;border-bottom:1px solid var(--border-default)"
		>
			<Icon n="settings" s={15} color="var(--text-secondary)" />
			<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary);flex:1">Settings</span>
			<Btn variant="ghost" size="sm" onclick={onClose}>✕</Btn>
		</div>

		<div style="flex:1;display:flex;min-height:0">
			<!-- section rail -->
			<div
				style="flex:none;width:150px;padding:10px 8px;border-right:1px solid var(--border-default);display:flex;flex-direction:column;gap:2px"
			>
				{#each SECTIONS as s (s.id)}
					<button
						onclick={() => (section = s.id)}
						style="display:flex;align-items:center;gap:7px;padding:6px 9px;border-radius:var(--radius-sm);cursor:pointer;text-align:left;font-size:12px;border:1px solid {section ===
						s.id
							? 'var(--border-strong)'
							: 'transparent'};background:{section === s.id
							? 'var(--surface-active)'
							: 'transparent'};color:{section === s.id ? 'var(--text-primary)' : 'var(--text-secondary)'}"
					>
						<Icon n={s.icon} s={13} color="currentColor" />
						{s.label}
					</button>
				{/each}
			</div>

			<div style="flex:1;overflow-y:auto;padding:14px 16px;min-width:0">
				{#if section === 'performance'}
					<div style="font:var(--type-label);color:var(--text-secondary);text-transform:uppercase;letter-spacing:.06em">
						CPU limit
					</div>
					<p style="margin:6px 0 12px;font-size:12px;line-height:1.55;color:var(--text-secondary)">
						Kerf runs one heavy job at a time — an analysis pass, a transcription, a proxy, an export — and
						this is how much of the machine that job may take. Left alone, FFmpeg takes all of it.
					</p>

					<div style="display:flex;flex-wrap:wrap;gap:6px">
						{#each CPU_PRESETS as p (p.id)}
							<button title={p.hint} onclick={() => settings.setCpuPercent(p.percent)} style={chip(percent === p.percent)}>
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
							style="flex:1;accent-color:var(--kerf-500);cursor:pointer"
						/>
						<span
							style="flex:none;width:52px;text-align:right;font-family:var(--font-mono);font-size:13px;color:var(--text-primary)"
							>{percent}%</span
						>
					</div>

					<div
						data-selectable
						style="margin-top:12px;padding:9px 11px;border-radius:var(--radius-sm);background:var(--surface-inset);border:1px solid var(--border-subtle);font-family:var(--font-mono);font-size:12px;color:var(--text-secondary);line-height:1.6"
					>
						{threads} of {cores}
						{cores === 1 ? 'core' : 'cores'} for Kerf · {spare === 0
							? 'nothing held back'
							: `${spare} left for everything else`}
					</div>

					<p style="margin:10px 0 0;font-size:12px;line-height:1.55;color:var(--text-secondary)">
						{#if preset}
							{preset.hint}
						{:else}
							Renders scale roughly with the share you allow; everything else on the machine gets the rest.
						{/if}
					</p>

					{#if percent < 100}
						<p style="margin:8px 0 0;font-size:12px;line-height:1.55;color:var(--text-disabled)">
							Below 100%, background work also runs at lower scheduling priority, so the window you are
							looking at always wins a core when it wants one.
						</p>
					{/if}

					<p style="margin:12px 0 0;font-size:12px;line-height:1.55;color:var(--text-disabled)">
						A job already running keeps the cores it started with — FFmpeg cannot be told otherwise
						mid-render. The next one picks this up.
					</p>
				{/if}
			</div>
		</div>
	</div>
</div>
