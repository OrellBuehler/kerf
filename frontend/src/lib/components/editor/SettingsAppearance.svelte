<script lang="ts">
	// Themes: pick one (it is in use at once), make your own from a built-in, rename, copy, export and delete them, follow the
	// system's light / dark setting, and open the editor. The built-ins are read-only; "Customize" copies one and opens the copy.
	import SettingRow from './SettingRow.svelte';
	import Switch from './Switch.svelte';
	import Btn from './Btn.svelte';
	import Badge from './Badge.svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import ThemeSwatch from './ThemeSwatch.svelte';
	import { settings } from '#lib/settings.svelte';
	import { useFilter } from '#lib/settings-filter';
	import { entriesFor } from '#lib/theme-library';
	import { tabStop } from '#lib/menu-nav';
	import { roving } from '#lib/roving';
	import { tip } from '#lib/tip';

	const filter = useFilter();
	const themes = settings.themes;
	const entries = $derived(themes.entries);
	const follow = $derived(themes.follow);
	const inForce = $derived(themes.inForceId);

	/** The one in use (while following the system, the slots are said in the badges). */
	const chosen = (id: string) => inForce === id;
	const stop = $derived(tabStop(entries.map((e) => ({ checked: chosen(e.id), enabled: true }))));

	let renaming = $state<{ id: string; text: string; error: string | null } | null>(null);

	function startRename(id: string, name: string) {
		renaming = { id, text: name, error: null };
	}

	function finishRename() {
		const r = renaming;
		if (!r) return;
		const out = themes.rename(r.id, r.text);
		if (out.ok) renaming = null;
		else renaming = { ...r, error: out.reason };
	}

	async function remove(id: string) {
		await themes.remove(id);
	}
</script>

<SettingRow
	id="theme"
	label="Theme"
	hint="Pick one to use it at once. The built-in themes can't be changed; Customize makes a copy of one that you can, and you can keep as many as you like."
	stacked
>
	<div role="radiogroup" aria-label="Themes" use:roving style="display:flex;flex-direction:column;gap:5px">
		{#each entries as e, i (e.id)}
			{@const on = chosen(e.id)}
			<div
				data-theme={e.id}
				style="display:flex;align-items:center;gap:6px;padding:4px 6px 4px 4px;border-radius:var(--radius-md);border:var(--line-width) solid {on ? 'var(--kerf-500)' : 'var(--border-default)'};background:{on
					? 'color-mix(in srgb,var(--kerf-500) 14%,transparent)'
					: 'var(--surface-inset)'}"
			>
				{#if renaming?.id === e.id}
					<div style="flex:1;min-width:0;display:flex;align-items:center;gap:10px;padding:2px 6px 2px 2px">
						<ThemeSwatch theme={e.theme} />
						<span style="min-width:0;display:flex;flex-direction:column;gap:3px">
							<!-- svelte-ignore a11y_autofocus -->
							<input
								type="text"
								value={renaming.text}
								aria-label="Theme name"
								aria-invalid={renaming.error ? 'true' : undefined}
								maxlength="48"
								autofocus
								onkeydown={(ev) => {
									ev.stopPropagation();
									if (ev.key === 'Enter') finishRename();
									else if (ev.key === 'Escape') renaming = null;
								}}
								oninput={(ev) => renaming && (renaming = { ...renaming, text: ev.currentTarget.value, error: null })}
								onblur={() => renaming && finishRename()}
								style="height:24px;width:220px;box-sizing:border-box;padding:0 6px;background:var(--surface-panel);color:var(--text-primary);border:var(--line-width) solid {renaming.error
									? 'var(--red-400)'
									: 'var(--border-strong)'};border-radius:var(--radius-xs);font-size:13px"
							/>
							{#if renaming.error}<span role="alert" style="font-size:11px;color:var(--red-400)">{renaming.error}</span>{/if}
						</span>
					</div>
				{:else}
					<button
						type="button"
						role="radio"
						aria-checked={on}
						tabindex={i === stop ? 0 : -1}
						data-testid="theme-{e.id}"
						onclick={() => themes.select(e.id)}
						style="flex:1;min-width:0;display:flex;align-items:center;gap:10px;padding:2px 6px 2px 2px;background:none;border:none;border-radius:var(--radius-sm);cursor:pointer;text-align:left;color:var(--text-primary)"
					>
						<ThemeSwatch theme={e.theme} />
						<span style="min-width:0;display:flex;flex-direction:column;gap:3px">
							<span style="font-size:13px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{e.theme.name}</span>
							<span style="display:flex;gap:5px;flex-wrap:wrap">
								<Badge>{e.builtIn ? 'Built-in' : 'Yours'}</Badge>
								<Badge>{e.theme.scheme === 'light' ? 'Light' : 'Dark'}</Badge>
								{#if follow.on && follow.dark === e.id}<Badge tone="kerf">System dark</Badge>{/if}
								{#if follow.on && follow.light === e.id}<Badge tone="kerf">System light</Badge>{/if}
								{#if inForce === e.id}<Badge tone="success" dot>In use</Badge>{/if}
							</span>
						</span>
					</button>
				{/if}
				<IconBtn
					title={e.builtIn ? 'Customize…\nMakes a copy you can change, and opens it' : 'Edit…'}
					size={28}
					data-testid="theme-edit-{e.id}"
					onclick={() => themes.edit(e.id)}><Icon n="palette" s={14} /></IconBtn
				>
				<IconBtn title="Make a copy" size={28} data-testid="theme-copy-{e.id}" pending={!themes.canAdd} onclick={() => themes.duplicate(e.id)}><Icon n="copy" s={14} /></IconBtn>
				<IconBtn title="Export as a file…" size={28} data-testid="theme-export-{e.id}" onclick={() => void themes.exportFile(e.id)}><Icon n="download" s={14} /></IconBtn>
				{#if !e.builtIn}
					<IconBtn title="Rename" size={28} data-testid="theme-rename-{e.id}" onclick={() => startRename(e.id, e.theme.name)}><Icon n="pencil" s={14} /></IconBtn>
					<IconBtn title="Delete…" size={28} data-testid="theme-delete-{e.id}" onclick={() => void remove(e.id)}><Icon n="trash" s={14} /></IconBtn>
				{/if}
			</div>
		{/each}
	</div>
</SettingRow>

<SettingRow
	id="theme-follow"
	for="sw-theme-follow"
	label="Match the system's light and dark setting"
	hint="Kerf switches themes when the system does: one for light, one for dark. Choosing a theme above while this is on sets the one for its own scheme."
>
	<Switch
		id="sw-theme-follow"
		label="Match the system's light and dark setting"
		testid="theme-follow"
		checked={follow.on}
		onchange={(on) => themes.setFollow({ on })}
	/>
</SettingRow>
{#if follow.on && filter.shows('theme-follow')}
	<div data-setting-extra="theme-follow" style="display:flex;gap:16px;flex-wrap:wrap;padding:10px 0;border-bottom:var(--line-width) solid var(--border-subtle);font-size:12px;color:var(--text-secondary)">
		{#each [{ slot: 'light' as const, label: 'When the system is light' }, { slot: 'dark' as const, label: 'When the system is dark' }] as s (s.slot)}
			<label style="display:flex;align-items:center;gap:8px">
				<span>{s.label}</span>
				<select
					class="k-select"
					data-testid="theme-slot-{s.slot}"
					value={follow[s.slot]}
					onchange={(e) => themes.setFollow({ [s.slot]: e.currentTarget.value })}
				>
					{#each entriesFor(themes.book, s.slot) as t (t.id)}
						<option value={t.id}>{t.theme.name}</option>
					{/each}
				</select>
			</label>
		{/each}
		<span use:tip={'Following the system, the theme in use is the one for what the system says now.'} style="color:var(--text-muted)">
			Now: {themes.systemDark ? 'dark' : 'light'}
		</span>
	</div>
{/if}

<SettingRow
	id="theme-edit"
	label="Colors, corners, spacing and typefaces"
	hint="Opens the editor on the theme in use (a built-in is copied first). It sits beside the window so you can watch the page change; nothing is kept until you save, and the colors that are hard to read are marked as you go."
	stacked
>
	<Btn variant="secondary" size="sm" icon="palette" data-testid="theme-edit" onclick={() => themes.edit(inForce)}>Edit the theme in use…</Btn>
</SettingRow>

<SettingRow id="theme-file" label="Import and export a theme" hint="A theme is a JSON file you can share or bring back. An import is added to the list and used; nothing is replaced." stacked>
	<div style="display:flex;gap:8px;flex-wrap:wrap">
		<Btn variant="secondary" size="sm" icon="folder-open" pending={!themes.canAdd} onclick={() => void themes.importFile()}>Import…</Btn>
		<Btn variant="secondary" size="sm" icon="save" onclick={() => void themes.exportFile(inForce)}>Export the theme in use…</Btn>
	</div>
</SettingRow>
