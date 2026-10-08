<script lang="ts">
	// The library: everything you pull *into* the cut, behind one icon rail —
	// Media, Titles, Effects, Transitions, Audio and the Transcript. It replaces
	// the old Media | Transcript tab group. Clicking the active icon folds the
	// content away and leaves the rail alone (the panel then gives its width
	// back); clicking any icon opens it again. Which tab and whether it is folded
	// are remembered (`settings.workspaces.library`), and shared by every
	// workspace — the rail is a tool, not part of any one arrangement.
	import { onMount, untrack } from 'svelte';
	import type { DockviewApi, DockviewPanelApi } from 'dockview';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import MediaBin from './MediaBin.svelte';
	import TitlesControls from './TitlesControls.svelte';
	import EffectsTab from './EffectsTab.svelte';
	import TransitionsTab from './TransitionsTab.svelte';
	import AudioTab from './AudioTab.svelte';
	import TranscriptPanel from './TranscriptPanel.svelte';
	import { settings } from '$lib/settings.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { LIBRARY_MIN_OPEN_WIDTH, LIBRARY_OPEN_WIDTH, LIBRARY_RAIL_WIDTH } from '$lib/layout';
	import { LIBRARY_TAB_SPECS, stepTab, type LibraryTab } from '$lib/workspaces';

	/** The dockview panel this is mounted in, and the dock around it; absent when
	 *  rendered on its own. */
	let { panelApi, dock }: { panelApi?: DockviewPanelApi; dock?: DockviewApi } = $props();

	const tab = $derived(settings.libraryTab);
	const spec = $derived(LIBRARY_TAB_SPECS.find((t) => t.id === tab) ?? LIBRARY_TAB_SPECS[0]);

	// The rail can only stand alone when the library has its group to itself — a
	// layout where the user tabbed another panel in beside it keeps the rail but
	// cannot give the group's width back.
	let solo = $state(true);
	const collapsed = $derived(settings.libraryCollapsed && solo);

	/** The roving-tabindex stop: arrows move focus along the rail, Enter / Space /
	 *  a click choose. */
	let focusTab = $state<LibraryTab>(untrack(() => settings.libraryTab));
	let rail = $state<HTMLElement | null>(null);

	function pick(id: LibraryTab, e?: MouseEvent) {
		focusTab = id;
		// A pointer click should not leave focus on the rail: a focused button
		// swallows Space, so the transport shortcut would stop working. Enter /
		// Space (whose click has `detail` 0) keep it, to be navigated on.
		if (e && e.detail > 0) (e.currentTarget as HTMLElement | null)?.blur();
		if (collapsed) {
			settings.setLibraryTab(id);
			settings.setLibraryCollapsed(false);
		} else if (id === tab) {
			if (solo) settings.setLibraryCollapsed(true);
		} else {
			settings.setLibraryTab(id);
		}
	}

	/** The header's collapse button unmounts with the content it sits in, which
	 *  would drop keyboard focus on the page; hand it to the rail's active tab,
	 *  which is what is left. (A click leaves it nowhere, as every click does.) */
	function foldFromHeader(e: MouseEvent) {
		settings.setLibraryCollapsed(true);
		if (e.detail === 0) rail?.querySelector<HTMLElement>(`[data-library-tab="${tab}"]`)?.focus();
	}

	function onRailKey(e: KeyboardEvent) {
		const by = e.key === 'ArrowDown' || e.key === 'ArrowRight' ? 1 : e.key === 'ArrowUp' || e.key === 'ArrowLeft' ? -1 : 0;
		const next =
			e.key === 'Home'
				? LIBRARY_TAB_SPECS[0].id
				: e.key === 'End'
					? LIBRARY_TAB_SPECS[LIBRARY_TAB_SPECS.length - 1].id
					: by
						? stepTab(focusTab, by)
						: null;
		if (!next) return;
		e.preventDefault();
		focusTab = next;
		rail?.querySelector<HTMLElement>(`[data-library-tab="${next}"]`)?.focus();
	}

	// ---- the panel's own width -----------------------------------------------
	//
	// Folded, the library is exactly its rail, and the freed width goes to the
	// panels beside it. Both bounds are pinned so the sash cannot be dragged
	// open onto an empty rail; open, the minimum comes back so the content has
	// room. The header (the tab strip) is only for an open library: a 40 px
	// column has nowhere to put a "Library" tab.
	let openWidth = LIBRARY_OPEN_WIDTH;
	let wasCollapsed: boolean | null = null;

	/** The group to the right of this one, side by side with it — the one the
	 *  width this group gives up (or takes) should come from (or go to). */
	function neighbour() {
		// A library in a window of its own has no row beside it to hand width to.
		if (panelApi?.group.api.location.type === 'popout') return null;
		const box = panelApi?.group.api.boundingBox;
		if (!box || !dock) return null;
		for (const g of dock.groups) {
			const b = g.api.boundingBox;
			if (!b || g === panelApi?.group) continue;
			const beside = Math.abs(b.left - (box.left + box.width)) < 3;
			const overlaps = b.top < box.top + box.height - 3 && b.top + b.height > box.top + 3;
			if (beside && overlaps) return g;
		}
		return null;
	}

	function applyGeometry(fold: boolean) {
		const group = panelApi?.group;
		if (!panelApi || !group) return;
		// Only a fold or an unfold *in front of the user* picks a width. At mount
		// the layout already carries one, and the constraints below clamp it.
		const moved = wasCollapsed !== null && wasCollapsed !== fold;
		wasCollapsed = fold;
		// Whatever the user changed before this is still on the workspace's debounce.
		if (moved) workspace.beforeLibraryMove();
		const next = neighbour();
		const nextWidth = next?.api.width ?? 0;
		const before = panelApi.width;
		if (fold) {
			if (before > LIBRARY_RAIL_WIDTH + 8) openWidth = before;
			group.header.hidden = true;
			group.api.setConstraints({ minimumWidth: LIBRARY_RAIL_WIDTH, maximumWidth: LIBRARY_RAIL_WIDTH });
			group.api.setSize({ width: LIBRARY_RAIL_WIDTH });
		} else {
			group.header.hidden = false;
			group.api.setConstraints({ minimumWidth: LIBRARY_MIN_OPEN_WIDTH, maximumWidth: Number.MAX_SAFE_INTEGER });
			// A layout saved while the rail was folded carries a rail's width; the new
			// minimum does not widen a group that is already narrower than it.
			if (moved || (before > 0 && before < LIBRARY_MIN_OPEN_WIDTH))
				group.api.setSize({ width: Math.max(openWidth, LIBRARY_MIN_OPEN_WIDTH) });
		}
		group.relayout();
		// dockview hands freed width to the last group in the row, and takes a
		// widened group's from there too; the panel next door is the one that
		// should move. (Also when a layout restored with the wrong width for the
		// rail's state — a workspace saved folded, opened unfolded — is corrected.)
		if (next) {
			const width = panelApi.width;
			const delta = before - width;
			if (delta !== 0) next.api.setSize({ width: nextWidth + delta });
		}
		// Folding is the `collapsed` setting at work, not the user rearranging the
		// workspace: do not let it be written down as one.
		if (moved) workspace.afterLibraryMove();
	}

	$effect(() => {
		const fold = collapsed;
		untrack(() => applyGeometry(fold));
	});

	onMount(() => {
		if (!panelApi) return;
		const subs: Array<{ dispose(): void }> = [];
		let watching: unknown = null;
		// A window of its own is its own width to fill: the rail cannot fold there, and a
		// workspace that remembers it folded must not show a 40 px rail in a big window.
		const alone = (group: DockviewPanelApi['group']) => group.panels.length <= 1 && group.api.location.type !== 'popout';
		const watch = () => {
			const group = panelApi.group;
			solo = alone(group);
			if (watching === group) return;
			watching = group;
			for (const s of subs.splice(0)) s.dispose();
			const recount = () => (solo = alone(group));
			subs.push(
				group.model.onDidAddPanel(recount),
				group.model.onDidRemovePanel(recount),
				group.api.onDidLocationChange(recount)
			);
		};
		watch();
		subs.push(panelApi.onDidGroupChange(() => watch()));
		return () => {
			for (const s of subs.splice(0)) s.dispose();
		};
	});
</script>

<div style="flex:1;min-width:0;min-height:0;display:flex;background:var(--surface-panel);overflow:hidden">
	<div
		bind:this={rail}
		role="tablist"
		aria-orientation="vertical"
		aria-label="Library"
		tabindex="-1"
		onkeydown={onRailKey}
		style="width:{LIBRARY_RAIL_WIDTH}px;flex:none;display:flex;flex-direction:column;align-items:center;gap:2px;padding:6px 0;background:var(--surface-app);border-right:var(--line-width) solid var(--border-default);overflow-y:auto;overflow-x:hidden"
	>
		{#each LIBRARY_TAB_SPECS as t (t.id)}
			{@const open = !collapsed && t.id === tab}
			<button
				role="tab"
				id="library-tab-{t.id}"
				data-library-tab={t.id}
				aria-selected={open}
				aria-controls={open ? 'library-content' : undefined}
				aria-label={t.label}
				title={open && solo ? `${t.label} — click to collapse` : `${t.label} — ${t.hint}`}
				tabindex={t.id === focusTab ? 0 : -1}
				onclick={(e) => pick(t.id, e)}
				onfocus={() => (focusTab = t.id)}
				style="position:relative;width:36px;height:36px;flex:none;display:inline-flex;align-items:center;justify-content:center;border-radius:var(--radius-sm);cursor:pointer;border:var(--line-width) solid {open
					? 'var(--border-strong)'
					: 'transparent'};background:{open ? 'var(--surface-active)' : 'transparent'};color:{open
					? 'var(--kerf-400)'
					: t.id === tab
						? 'var(--text-secondary)'
						: 'var(--text-muted)'};transition:background var(--dur-fast) var(--ease-out),color var(--dur-fast) var(--ease-out)"
			>
				<Icon n={t.icon} s={18} />
			</button>
		{/each}
	</div>

	{#if !collapsed}
		<div
			id="library-content"
			role="tabpanel"
			aria-labelledby="library-tab-{tab}"
			style="flex:1;min-width:0;min-height:0;display:flex;flex-direction:column;overflow:hidden"
		>
			<div
				style="height:32px;flex:none;display:flex;align-items:center;gap:8px;padding:0 6px 0 12px;border-bottom:var(--line-width) solid var(--border-subtle)"
			>
				<span
					title={spec.hint}
					style="flex:1;min-width:0;font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
					>{spec.label}</span
				>
				{#if solo}
					<IconBtn title="Collapse the library to its rail" size={24} onclick={foldFromHeader}>
						<Icon n="chevron-left" s={14} />
					</IconBtn>
				{/if}
			</div>
			{#if tab === 'media'}
				<MediaBin />
			{:else if tab === 'titles'}
				<div style="flex:1;min-height:0;overflow-y:auto;padding:12px"><TitlesControls /></div>
			{:else if tab === 'effects'}
				<EffectsTab />
			{:else if tab === 'transitions'}
				<TransitionsTab />
			{:else if tab === 'audio'}
				<AudioTab />
			{:else}
				<TranscriptPanel />
			{/if}
		</div>
	{/if}
</div>
