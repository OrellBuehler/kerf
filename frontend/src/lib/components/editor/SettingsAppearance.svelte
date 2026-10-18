<script lang="ts">
	import SettingRow from './SettingRow.svelte';
	import Btn from './Btn.svelte';
	import { settings } from '$lib/settings.svelte';
	import { useFilter } from '$lib/settings-filter';
	import { colorGroupId } from '$lib/settings-index';
	import { chip, OVERLINE } from '$lib/settings-ui';
	import { COLOR_GROUPS, PRESETS, PRESET_IDS, SHAPE_TOKENS, THUMB_STYLES } from '$lib/theme';
	import { tip } from '$lib/tip';

	const filter = useFilter();
	const themePreset = $derived(settings.themePreset);
</script>

<SettingRow id="theme" label="Theme" hint="Every color and line the editor draws with. Changing one makes the theme custom." stacked>
	<div role="group" aria-label="Theme presets" style="display:flex;flex-wrap:wrap;gap:6px;align-items:center">
		{#each PRESET_IDS as id (id)}
			<button type="button" onclick={() => settings.applyPreset(id)} aria-pressed={themePreset === id} style={chip(themePreset === id)}>
				{PRESETS[id].name}
			</button>
		{/each}
		{#if themePreset === 'custom'}
			<span style={chip(true)}>Custom</span>
		{/if}
	</div>
	<div style="margin-top:10px;display:flex;gap:10px;align-items:center">
		<input
			type="text"
			value={settings.theme.name}
			aria-label="Theme name"
			onchange={(e) => settings.setThemeName(e.currentTarget.value)}
			style="flex:1;min-width:0;height:28px;padding:0 8px;background:var(--surface-inset);color:var(--text-primary);border:var(--line-width) solid var(--border-default);border-radius:var(--radius-sm);font-size:12px"
		/>
		<button
			type="button"
			onclick={() => settings.setScheme('dark')}
			aria-pressed={settings.theme.scheme === 'dark'}
			style={chip(settings.theme.scheme === 'dark')}>Dark</button
		>
		<button
			type="button"
			onclick={() => settings.setScheme('light')}
			aria-pressed={settings.theme.scheme === 'light'}
			style={chip(settings.theme.scheme === 'light')}>Light</button
		>
	</div>
</SettingRow>

<SettingRow id="theme-file" label="Import and export a theme" hint="A theme is a JSON file you can share or bring back." stacked>
	<div style="display:flex;gap:8px;flex-wrap:wrap">
		<Btn variant="secondary" size="sm" icon="folder-open" onclick={() => settings.importTheme()}>Import…</Btn>
		<Btn variant="secondary" size="sm" icon="save" onclick={() => settings.exportTheme()}>Export…</Btn>
	</div>
</SettingRow>

{#if filter.shows('shapes')}
	<div data-setting="shapes" style="padding:12px 0 6px">
		<div style={OVERLINE}>Lines, shadows and sliders</div>
		<div style="margin-top:6px;display:flex;flex-direction:column;gap:2px">
			{#each SHAPE_TOKENS as t (t.name)}
				<label use:tip={t.hint} style="display:flex;align-items:center;gap:10px;height:28px;font-size:12px;color:var(--text-secondary)">
					<span style="width:130px;flex:none">{t.label}</span>
					<input
						type="range"
						min={t.min}
						max={t.max}
						step={t.step}
						value={settings.theme.shape[t.name]}
						aria-label={t.label}
						oninput={(e) => settings.setShape(t.name, Number(e.currentTarget.value))}
						style="flex:1;min-width:0"
					/>
					<span style="flex:none;width:44px;text-align:right;font-family:var(--font-mono);font-size:11px;color:var(--text-primary)"
						>{settings.theme.shape[t.name]}{t.unit}</span
					>
				</label>
			{/each}
			<div style="display:flex;align-items:center;gap:10px;height:32px;font-size:12px;color:var(--text-secondary)">
				<span style="width:130px;flex:none">Thumb style</span>
				{#each THUMB_STYLES as ts (ts.id)}
					<button
						type="button"
						onclick={() => settings.setThumbStyle(ts.id)}
						aria-pressed={settings.theme.shape['slider-thumb-style'] === ts.id}
						style={chip(settings.theme.shape['slider-thumb-style'] === ts.id)}>{ts.label}</button
					>
				{/each}
			</div>
		</div>
	</div>
{/if}

{#each COLOR_GROUPS as g (g.label)}
	{#if filter.shows(colorGroupId(g.label))}
		<div data-setting={colorGroupId(g.label)} style="padding:12px 0 6px">
			<div style={OVERLINE}>{g.label}</div>
			<div style="margin-top:6px;display:grid;grid-template-columns:1fr 1fr;gap:4px 16px">
				{#each g.tokens as t (t.name)}
					<label style="display:flex;align-items:center;gap:8px;height:28px;font-size:12px;color:var(--text-secondary);cursor:pointer">
						<input
							type="color"
							value={settings.theme.colors[t.name]}
							oninput={(e) => settings.setColor(t.name, e.currentTarget.value)}
							style="width:26px;height:20px;padding:0;border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-xs);background:none;cursor:pointer"
						/>
						<span style="flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{t.label}</span>
						<span style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{settings.theme.colors[t.name]}</span>
					</label>
				{/each}
			</div>
		</div>
	{/if}
{/each}
