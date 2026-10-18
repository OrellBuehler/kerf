<script lang="ts">
	import SettingRow from './SettingRow.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import Btn from './Btn.svelte';
	import { settings } from '$lib/settings.svelte';
	import { TIME_MODES, formatTime, type TimeMode } from '$lib/time-format';
	import { confirmAction } from '$lib/api';
	import { CONFIRM } from '$lib/confirm-copy';
	import { shortPath } from '$lib/media-info';
	import { fileName } from '$lib/failure-copy';
	import { tip } from '$lib/tip';

	// 4.4 s in a 30 fps cut: what each mode makes of the same moment.
	const example = (mode: TimeMode) => formatTime(4.4, 30, mode);

	async function clear() {
		const c = CONFIRM.clearRecent;
		if (await confirmAction(c.message, c.title, c)) await settings.clearRecent();
	}
</script>

<SettingRow id="time-display" label="Time display">
	{#snippet hint()}
		How every position and length reads: the timeline, the Inspector, the status bar, the export range and the
		platform checks. 4.4 s in a 30 fps cut is
		<span data-testid="time-example" style="font-family:var(--font-mono);color:var(--text-primary)">{example(settings.timeMode)}</span>.
	{/snippet}
	<ChoiceChips
		label="Time display"
		options={TIME_MODES.map((m) => ({ value: m.id, label: m.label, hint: m.hint }))}
		value={settings.timeMode}
		onpick={(m) => void settings.setTimeMode(m)}
		testid="time-display"
	/>
</SettingRow>

<SettingRow
	id="reopen-last"
	label="Reopen the last project on launch"
	hint="Off, Kerf starts empty. A project named on the command line, or opened by double-clicking a .kerf file, always wins."
>
	<input
		type="checkbox"
		role="switch"
		aria-label="Reopen the last project on launch"
		data-testid="reopen-last"
		checked={settings.reopenLast}
		onchange={(e) => void settings.setReopenLast(e.currentTarget.checked)}
		style="accent-color:var(--kerf-500);cursor:pointer;width:16px;height:16px"
	/>
</SettingRow>

<SettingRow id="recent-projects" label="Recent projects" stacked>
	{#snippet hint()}
		Opened or saved most recently, newest first, at most ten. File › Open Recent lists them; a file that has gone is dropped.
	{/snippet}
	{#if settings.recent.length === 0}
		<div style="font-size:12px;color:var(--text-muted)">No recent projects.</div>
	{:else}
		<ul data-testid="recent-list" style="margin:0 0 10px;padding:0;list-style:none;display:flex;flex-direction:column;gap:3px">
			{#each settings.recent as path (path)}
				<li
					use:tip={path}
					style="display:flex;gap:10px;align-items:baseline;font-size:12px;padding:4px 8px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle)"
				>
					<span style="color:var(--text-primary);white-space:nowrap">{fileName(path)}</span>
					<span style="color:var(--text-muted);min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:var(--font-mono);font-size:11px">{shortPath(path)}</span>
				</li>
			{/each}
		</ul>
		<Btn variant="secondary" size="sm" icon="trash" onclick={clear}>Clear recent projects</Btn>
	{/if}
</SettingRow>
