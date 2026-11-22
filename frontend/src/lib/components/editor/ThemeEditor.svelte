<script lang="ts">
	// The theme editor: a panel docked to the right edge with nothing dimmed behind it, so the page is seen changing as it is
	// edited. What is typed or picked is a draft — on the page at once (`settings.themes`), written only by Save; Cancel, and
	// Escape after being asked, put back what was there. The panel itself is drawn in the theme the sitting began with, not the draft, so
	// a theme that makes text unreadable does not make its own editor unreadable. Beside each colour, the pairs the UI draws
	// that the draft does not meet (`theme-contrast.ts`, the list the guard test holds the built-ins to): a warning, never a block.
	import { tick } from 'svelte';
	import { EDITOR_WIDTH, dockWidth } from '#lib/theme-dock';
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import ChoiceChips from './ChoiceChips.svelte';
	import ThemeColorRow from './ThemeColorRow.svelte';
	import ThemeShape from './ThemeShape.svelte';
	import { settings } from '#lib/settings.svelte';
	import { COLOR_GROUPS, baseFor, suggestScheme, themeStyle, PRESETS, type ColorToken } from '#lib/theme';
	import { describeFinding, findings as contrastFindings, findingsByToken, problems } from '#lib/theme-contrast';
	import { canRedo, canUndo, colorChanged, colorDefault, dirty } from '#lib/theme-session';
	import { OVERLINE } from '#lib/settings-ui';
	import { tip } from '#lib/tip';

	const themes = settings.themes;
	const session = $derived(themes.session);
	const draft = $derived(session?.draft);

	/** The panel is drawn in the theme the sitting began with. */
	const tokens = $derived(session ? themeStyle(session.saved) : undefined);

	// Docked: the app is made narrower by the editor's width (`theme-dock.ts`) while the window can spare it; narrower than that it
	// overlays the page. The dock relayouts to the new width through the workspace's own resize path, which writes nothing.
	let viewport = $state(typeof window === 'undefined' ? 0 : window.innerWidth);
	const dock = $derived(dockWidth(viewport));
	const docked = $derived(dock !== 0);
	$effect(() => {
		const root = document.documentElement;
		if (dock > 0) root.style.setProperty('--shell-right', `${dock}px`);
		return () => root.style.removeProperty('--shell-right');
	});

	const list = $derived(draft ? contrastFindings(draft.colors) : []);
	const byToken = $derived(findingsByToken(list));
	const summary = $derived(problems(list));
	const unreadable = $derived(draft ? suggestScheme(draft.colors) !== draft.scheme : false);

	let page = $state<'colors' | 'shape'>('colors');
	let showProblems = $state(false);
	let open = $state<Record<string, boolean>>({ Surfaces: true, Text: true });
	let scroller = $state<HTMLElement | null>(null);

	const warnings = (label: string) =>
		COLOR_GROUPS.find((g) => g.label === label)!.tokens.filter((t) => (byToken.get(t.name)?.length ?? 0) > 0).length;

	/** Take the user to a colour: its page and group open, the hex field focused. */
	async function jump(token: ColorToken) {
		page = 'colors';
		const group = COLOR_GROUPS.find((g) => g.tokens.some((t) => t.name === token));
		if (group) open[group.label] = true;
		await tick();
		const row = scroller?.querySelector<HTMLElement>(`[data-token="${token}"]`);
		row?.scrollIntoView({ block: 'center' });
		row?.querySelector<HTMLInputElement>('input[type="text"]')?.focus();
	}

	function onkeydown(e: KeyboardEvent) {
		// Typing in a field keeps the field's own undo.
		const t = e.target as HTMLElement | null;
		if (t && (t.tagName === 'TEXTAREA' || (t.tagName === 'INPUT' && (t as HTMLInputElement).type === 'text'))) return;
		const action = settings.actionFor(e);
		if (action === 'edit.undo') themes.undo();
		else if (action === 'edit.redo') themes.redo();
		else return;
		e.preventDefault();
	}

	const baseName = $derived(draft ? PRESETS[baseFor(draft)].name : '');
</script>

<svelte:window bind:innerWidth={viewport} />

{#if session && draft}
	<Modal title="Edit theme" icon="palette" width={EDITOR_WIDTH} side {docked} dismissOnScrim={false} initial="[data-theme-name]" {tokens} onClose={() => void themes.close()}>
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div {onkeydown} style="flex:1;min-height:0;display:flex;flex-direction:column">
			<div style="flex:none;padding:12px 14px 10px;border-bottom:var(--line-width) solid var(--border-default);display:flex;flex-direction:column;gap:10px">
				<div style="display:flex;gap:6px;align-items:center">
					<input
						data-theme-name
						type="text"
						value={draft.name}
						aria-label="Theme name"
						maxlength="48"
						spellcheck="false"
						oninput={(e) => themes.setName(e.currentTarget.value)}
						onchange={() => themes.endEdit()}
						style="flex:1;min-width:0;height:var(--control-sm);box-sizing:border-box;padding:0 8px;background:var(--surface-inset);color:var(--text-primary);border:var(--line-width) solid var(--border-strong);border-radius:var(--radius-sm);font-size:13px;font-weight:500"
					/>
					<IconBtn title={settings.tip('Undo', 'edit.undo')} size={28} pending={!canUndo(session)} onclick={() => themes.undo()}><Icon n="undo" s={14} /></IconBtn>
					<IconBtn title={settings.tip('Redo', 'edit.redo')} size={28} pending={!canRedo(session)} onclick={() => themes.redo()}><Icon n="redo" s={14} /></IconBtn>
				</div>

				<div style="display:flex;align-items:center;gap:10px;font-size:12px;color:var(--text-secondary)">
					<span use:tip={'What the browser draws itself — scrollbars, form controls, notifications — follows this'}>Light or dark</span>
					<ChoiceChips
						label="Base scheme"
						options={[
							{ value: 'dark', label: 'Dark' },
							{ value: 'light', label: 'Light' }
						]}
						value={draft.scheme}
						onpick={(v) => {
							themes.setScheme(v);
							themes.endEdit();
						}}
					/>
					<span style="flex:1"></span>
					<span use:tip={'What a color or value goes back to when you press its reset'} style="font-size:11px;color:var(--text-muted)">Resets go to {baseName}</span>
				</div>
				{#if unreadable}
					<div role="note" style="display:flex;align-items:center;gap:8px;font-size:11px;line-height:1.4;color:var(--text-secondary)">
						<Icon n="alert-triangle" s={12} color="var(--orange-400)" />
						<span style="flex:1">The panels look {suggestScheme(draft.colors)}, but this is marked {draft.scheme}: scrollbars and form controls will not match.</span>
						<Btn variant="ghost" size="sm" onclick={() => themes.setScheme(suggestScheme(draft.colors))}>Mark it {suggestScheme(draft.colors)}</Btn>
					</div>
				{/if}

				<div data-testid="contrast-summary" style="font-size:12px">
					{#if summary.length === 0}
						<div style="display:flex;align-items:center;gap:6px;color:var(--text-secondary)">
							<Icon n="circle-check" s={13} color="var(--green-400)" />
							Every pair Kerf checks is readable.
						</div>
					{:else}
						<button
							type="button"
							aria-expanded={showProblems}
							onclick={() => (showProblems = !showProblems)}
							style="display:flex;align-items:center;gap:6px;width:100%;padding:0;background:none;border:none;cursor:pointer;color:var(--orange-400);font-size:12px;text-align:left"
						>
							<Icon n="alert-triangle" s={13} />
							<span style="flex:1">{summary.length} {summary.length === 1 ? 'color pair is' : 'color pairs are'} hard to read</span>
							<Icon n={showProblems ? 'chevron-up' : 'chevron-down'} s={13} />
						</button>
						{#if showProblems}
							<ul style="margin:6px 0 0;padding:0;list-style:none;max-height:132px;overflow-y:auto;display:flex;flex-direction:column;gap:2px">
								{#each summary as p (p.fg + p.what)}
									<li>
										<button
											type="button"
											onclick={() => void jump(p.fg)}
											style="width:100%;display:flex;gap:8px;align-items:baseline;padding:3px 6px;background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);border-radius:var(--radius-xs);cursor:pointer;text-align:left;font-size:11px;color:var(--text-secondary)"
										>
											<span style="flex:1;min-width:0">{describeFinding(p.worst)}</span>
											{#if p.count > 1}<span style="color:var(--text-muted)">+{p.count - 1} more</span>{/if}
										</button>
									</li>
								{/each}
							</ul>
						{/if}
					{/if}
				</div>

				<ChoiceChips
					label="Editor page"
					options={[
						{ value: 'colors', label: 'Colors' },
						{ value: 'shape', label: 'Shape and type' }
					]}
					value={page}
					onpick={(v) => {
						page = v;
						if (scroller) scroller.scrollTop = 0;
					}}
				/>
			</div>

			<div bind:this={scroller} data-testid="theme-editor-scroll" style="flex:1;min-height:0;overflow-y:auto;padding:4px 14px 16px">
				{#if page === 'colors'}
					{#each COLOR_GROUPS as g (g.label)}
						{@const n = warnings(g.label)}
						<details bind:open={open[g.label]} data-group={g.label} style="border-bottom:var(--line-width) solid var(--border-subtle)">
							<summary style="display:flex;align-items:center;gap:8px;padding:10px 0;cursor:pointer;list-style:none">
								<Icon n={open[g.label] ? 'chevron-down' : 'chevron-right'} s={13} color="var(--text-muted)" />
								<span style="{OVERLINE};flex:1">{g.label}</span>
								{#if n > 0}
									<span style="display:inline-flex;align-items:center;gap:4px;font-size:11px;color:var(--orange-400)"><Icon n="alert-triangle" s={11} />{n}</span>
								{/if}
							</summary>
							<p style="margin:0 0 4px;font-size:11px;line-height:1.5;color:var(--text-muted)">{g.blurb}</p>
							<div style="padding-bottom:8px">
								{#each g.tokens as t (t.name)}
									<ThemeColorRow
										token={t.name}
										label={t.label}
										hint={t.hint}
										value={draft.colors[t.name]}
										original={colorDefault(draft, t.name)}
										changed={colorChanged(draft, t.name)}
										findings={byToken.get(t.name) ?? []}
										onpick={(hex) => themes.setColor(t.name, hex)}
										onend={() => themes.endEdit()}
										onreset={() => themes.resetColor(t.name)}
									/>
								{/each}
							</div>
						</details>
					{/each}
				{:else}
					<ThemeShape />
				{/if}
			</div>
		</div>

		{#snippet footer()}
			<div style="display:flex;align-items:center;gap:8px;padding:10px 14px">
				<Btn variant="ghost" size="sm" pending={!dirty(session)} onclick={() => themes.revertAll()} title="Put everything back to how it was when you opened the editor">Revert all</Btn>
				<span style="flex:1"></span>
				<Btn variant="secondary" onclick={() => void themes.close()}>Cancel</Btn>
				<Btn variant="primary" onclick={() => themes.save()}>Save theme</Btn>
			</div>
		{/snippet}
	</Modal>
{/if}
