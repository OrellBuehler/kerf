<script lang="ts">
	// Kerf's preferences, as a section rail and a panel — and a search box that turns the panel into every setting
	// that answers it, grouped by section (`settings-index.ts` holds the words; each row hides itself unless the
	// search lets it through). The sections are components of their own, so the next preference is a row.
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import KeyboardSettings from './KeyboardSettings.svelte';
	import SettingsGeneral from './SettingsGeneral.svelte';
	import SettingsAppearance from './SettingsAppearance.svelte';
	import SettingsEditing from './SettingsEditing.svelte';
	import SettingsPreview from './SettingsPreview.svelte';
	import SettingsPerformance from './SettingsPerformance.svelte';
	import SettingsAnalysis from './SettingsAnalysis.svelte';
	import SettingsAgent from './SettingsAgent.svelte';
	import SettingsAdvanced from './SettingsAdvanced.svelte';
	import { settings } from '$lib/settings.svelte';
	import { filterSettings, SECTIONS, sectionOrFirst, type SectionId } from '$lib/settings-index';
	import { provideFilter } from '$lib/settings-filter';

	let { onClose }: { onClose: () => void } = $props();

	// Opened from a menu on one section (Help › Keyboard shortcuts), else the first.
	let section = $state<SectionId>(sectionOrFirst(untrack(() => settings.wantSection)));
	untrack(() => (settings.wantSection = null));

	let query = $state('');
	const filtered = $derived(filterSettings(query));
	const searching = $derived(filtered.query !== '');
	provideFilter({
		get query() {
			return filtered.query;
		},
		shows: (id) => filtered.ids.has(id)
	});

	const info = $derived(SECTIONS.find((s) => s.id === section)!);
	let panel = $state<HTMLElement | null>(null);

	function pick(id: SectionId) {
		if (!searching) {
			section = id;
			return;
		}
		// With a search on, every section with a match is on the page: go to it.
		if (filtered.counts[id] === 0) return;
		panel?.querySelector<HTMLElement>(`[data-section="${id}"]`)?.scrollIntoView({ block: 'start' });
	}

	function clear() {
		query = '';
	}

	function openKeyboard() {
		query = '';
		section = 'keyboard';
	}
</script>

{#snippet body(id: SectionId)}
	{#if id === 'general'}<SettingsGeneral />
	{:else if id === 'appearance'}<SettingsAppearance />
	{:else if id === 'editing'}<SettingsEditing />
	{:else if id === 'preview'}<SettingsPreview />
	{:else if id === 'performance'}<SettingsPerformance />
	{:else if id === 'analysis'}<SettingsAnalysis />
	{:else if id === 'agent'}<SettingsAgent />
	{:else if id === 'advanced'}<SettingsAdvanced />
	{/if}
{/snippet}

<Modal title="Settings" icon="settings" width={780} height={680} onClose={onClose}>
	<div style="flex:1;display:flex;min-height:0">
		<!-- section rail, search on top -->
		<nav
			aria-label="Settings sections"
			style="flex:none;width:172px;padding:10px 8px;border-right:var(--line-width) solid var(--border-default);display:flex;flex-direction:column;gap:2px;overflow-y:auto"
		>
			<div
				class="k-field"
				style="display:flex;align-items:center;gap:6px;height:28px;margin-bottom:8px;padding:0 6px 0 8px;background:var(--surface-inset);border:var(--line-width) solid var(--input);border-radius:var(--radius-sm)"
			>
				<Icon n="search" s={12} color="var(--text-muted)" />
				<input
					type="search"
					bind:value={query}
					placeholder="Search settings"
					aria-label="Search settings"
					data-testid="settings-search"
					spellcheck="false"
					autocomplete="off"
					onkeydown={(e) => {
						// Escape clears the search first and closes the dialog only when there is none.
						if (e.key === 'Escape' && query) {
							e.stopPropagation();
							clear();
						}
					}}
					style="flex:1;min-width:0;background:none;border:none;outline:none;color:var(--text-primary);font-size:12px"
				/>
			</div>
			{#each SECTIONS as s (s.id)}
				{@const here = !searching && section === s.id}
				{@const empty = searching && filtered.counts[s.id] === 0}
				<button
					type="button"
					onclick={() => pick(s.id)}
					aria-current={here ? 'page' : undefined}
					aria-disabled={empty ? 'true' : undefined}
					data-testid="settings-section-{s.id}"
					style="display:flex;align-items:center;gap:7px;padding:6px 9px;border-radius:var(--radius-sm);cursor:{empty
						? 'default'
						: 'pointer'};text-align:left;font-size:12px;border:var(--line-width) solid {here
						? 'var(--border-strong)'
						: 'transparent'};background:{here ? 'var(--surface-active)' : 'transparent'};color:{here
						? 'var(--text-primary)'
						: 'var(--text-secondary)'};opacity:{empty ? 0.4 : 1}"
				>
					<Icon n={s.icon} s={13} color="currentColor" />
					<span style="flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{s.label}</span>
					{#if searching && !empty}
						<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-muted)">{filtered.counts[s.id]}</span>
					{/if}
				</button>
			{/each}
		</nav>

		<!-- The keyboard list has its own search and scroll, so it gets the panel whole. -->
		<div
			bind:this={panel}
			data-testid="settings-panel"
			style="flex:1;min-width:0;{!searching && section === 'keyboard'
				? 'display:flex;flex-direction:column;overflow:hidden'
				: 'overflow-y:auto;padding:14px 18px 18px'}"
		>
			{#if searching}
				{#if filtered.sections.length === 0}
					<div data-testid="settings-none" style="padding:40px 12px;text-align:center;font-size:13px;color:var(--text-secondary)">
						No setting matches “{filtered.query}”.
						<div style="margin-top:10px"><Btn variant="secondary" size="sm" onclick={clear}>Clear the search</Btn></div>
					</div>
				{/if}
				{#each filtered.sections as id (id)}
					{@const s = SECTIONS.find((x) => x.id === id)!}
					<section data-section={id} aria-label={s.label} style="margin-bottom:18px">
						<h3
							style="margin:0 0 2px;display:flex;align-items:center;gap:7px;font:var(--type-label);color:var(--text-secondary);text-transform:uppercase;letter-spacing:.06em"
						>
							<Icon n={s.icon} s={13} color="currentColor" />{s.label}
						</h3>
						{#if id === 'keyboard'}
							<div style="padding:10px 0;font-size:12px;color:var(--text-secondary)">
								Every shortcut, and how to change it, has a page of its own.
								<Btn variant="secondary" size="sm" style="margin-left:8px" onclick={openKeyboard}>Open Keyboard</Btn>
							</div>
						{:else}
							{@render body(id)}
						{/if}
					</section>
				{/each}
			{:else if section === 'keyboard'}
				<KeyboardSettings />
			{:else}
				<h2 style="margin:0;font-size:16px;font-weight:600;color:var(--text-primary)">{info.label}</h2>
				<p style="margin:3px 0 4px;font-size:12px;line-height:1.5;color:var(--text-muted)">{info.blurb}</p>
				{@render body(section)}
			{/if}
		</div>
	</div>
</Modal>
