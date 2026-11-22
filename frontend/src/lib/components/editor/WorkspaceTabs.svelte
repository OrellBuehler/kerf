<script lang="ts">
	// The workspace switcher: Edit / Color / Audio / Motion / Deliver in the title
	// bar, and the ones the user saved beside them. Choosing one swaps the dock's
	// arrangement and nothing else. A group of toggle buttons rather than a tablist
	// — there is no tabpanel to point at, the panels are the whole window — with the
	// arrow keys as a shortcut along it.
	//
	// The + at the end keeps the arrangement on screen as a workspace of its own; a
	// button's menu (right-click, or the menu key on a focused one) renames, deletes
	// or resets it and moves it along; and a button is dragged to a new place (a
	// pointer drag, `tab-reorder.ts`: Escape, a lost window and `pointercancel`
	// leave it where it was). The order is the user's and is stored with the
	// workspaces.
	import Icon from './Icon.svelte';
	import { workspace } from '#lib/workspace.svelte';
	import { settings } from '#lib/settings.svelte';
	import { contextMenu, type MenuItem } from '#lib/context-menu.svelte';
	import type { ActionId } from '#lib/keymap';
	import { addBlocked, isBuiltinKey, workspaceEntries, workspaceLabel, type WorkspaceEntry, type WorkspaceKey } from '#lib/workspaces';
	import { dropIndex, markerX, pastThreshold } from '#lib/tab-reorder';
	import { deleteWorkspaceAsked } from '#lib/workspace-actions';
	import { toast } from '#lib/notifications.svelte';
	import { tip } from '#lib/tip';

	let list = $state<HTMLElement | null>(null);
	const entries = $derived(workspaceEntries(settings.workspaces));
	const full = $derived(addBlocked(settings.workspaces));

	function onKey(e: KeyboardEvent) {
		const ids = entries.map((w) => w.key);
		const at = ids.indexOf(document.activeElement?.getAttribute('data-workspace') ?? '');
		if (at < 0) return;
		let next: number | null = null;
		if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = (at + 1) % ids.length;
		else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') next = (at - 1 + ids.length) % ids.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = ids.length - 1;
		if (next === null) return;
		e.preventDefault();
		buttonOf(ids[next])?.focus();
	}

	/** A workspace's button. A saved one's key is an id of ours (`w-` and letters and digits), so it needs no escaping. */
	function buttonOf(key: WorkspaceKey | undefined): HTMLElement | null {
		return key ? (list?.querySelector<HTMLElement>(`[data-workspace="${key}"]`) ?? null) : null;
	}

	/** A drag has just ended on this button: the click it ends in is not a choice. */
	let afterDrag = false;

	function choose(e: MouseEvent, key: WorkspaceKey) {
		if (afterDrag) {
			afterDrag = false;
			return;
		}
		workspace.switchTo(key);
		// A click (as opposed to Enter / Space, whose `detail` is 0) must not leave
		// focus here: a focused button swallows Space, so the transport shortcut
		// would stop working until something else was clicked.
		if (e.detail > 0) (e.currentTarget as HTMLElement).blur();
	}

	// With more buttons than the bar has room for (the sides of the title bar are not given up for them) the group scrolls: the
	// wheel moves it, and the one on show is kept in view.
	function wheel(e: WheelEvent) {
		const el = e.currentTarget as HTMLElement;
		if (el.scrollWidth <= el.clientWidth || Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return;
		el.scrollLeft += e.deltaY;
		e.preventDefault();
	}
	$effect(() => {
		const key = workspace.shown;
		void entries.length;
		buttonOf(key)?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
	});

	// ---- moving a button ------------------------------------------------------------

	interface Drag {
		key: WorkspaceKey;
		pointer: number;
		x: number;
		y: number;
		/** Past the threshold: no longer a click. */
		moving: boolean;
		/** The place it takes among the buttons if let go now, and where the marker for it is drawn (px from the list's left). */
		at: number;
		marker: number | null;
	}
	let drag = $state<Drag | null>(null);

	/** The spans of the buttons that are not the one dragged, from the list's left edge. */
	function others(key: WorkspaceKey) {
		const origin = list?.getBoundingClientRect().left ?? 0;
		return [...(list?.querySelectorAll<HTMLElement>('[data-workspace]') ?? [])]
			.filter((b) => b.dataset.workspace !== key)
			.map((b) => {
				const r = b.getBoundingClientRect();
				return { left: r.left - origin, right: r.right - origin };
			});
	}

	function down(e: PointerEvent, key: WorkspaceKey) {
		// Only the main button, and only where there is something to put it before or after.
		if (e.button !== 0 || entries.length < 2) return;
		afterDrag = false;
		drag = { key, pointer: e.pointerId, x: e.clientX, y: e.clientY, moving: false, at: entries.findIndex((w) => w.key === key), marker: null };
		(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
	}

	function move(e: PointerEvent) {
		if (!drag || e.pointerId !== drag.pointer) return;
		if (!drag.moving && !pastThreshold(e.clientX - drag.x, e.clientY - drag.y)) return;
		const spans = others(drag.key);
		const at = dropIndex(spans, e.clientX - (list?.getBoundingClientRect().left ?? 0));
		drag = { ...drag, moving: true, at, marker: markerX(spans, at) };
	}

	function up(e: PointerEvent) {
		if (!drag || e.pointerId !== drag.pointer) return;
		const done = drag;
		drag = null;
		if (!done.moving) return;
		afterDrag = true;
		workspace.move(done.key, done.at);
		// The click that follows a drag is not a choice; a drag that ends where no click follows must not eat the next one.
		setTimeout(() => (afterDrag = false), 0);
	}

	/** Pointer cancelled, Escape, the window lost: it stays where it was. */
	function abandon() {
		if (!drag) return;
		const moved = drag.moving;
		drag = null;
		if (moved) {
			afterDrag = true;
			setTimeout(() => (afterDrag = false), 0);
		}
	}

	function onWindowKey(e: KeyboardEvent) {
		if (drag?.moving && e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			abandon();
		}
	}

	// ---- a button's menu ------------------------------------------------------------

	function menu(e: MouseEvent, w: WorkspaceEntry) {
		const at = entries.findIndex((x) => x.key === w.key);
		const items: MenuItem[] = [];
		if (w.builtin) {
			items.push({
				label: `Reset ${w.label} to its default`,
				icon: 'rotate-ccw',
				action: () => resetBuiltin(w.key)
			});
		} else {
			items.push(
				{ label: 'Rename…', action: () => workspace.askToRename(w.key) },
				{ label: 'Delete…', icon: 'trash', danger: true, action: () => void deleteWorkspaceAsked(w.key) }
			);
		}
		items.push(
			{ type: 'separator' },
			{
				label: 'Save the panels as a new workspace…',
				icon: 'plus',
				disabled: full !== null,
				reason: full ?? undefined,
				action: () => workspace.askToSave()
			},
			{ type: 'separator' },
			{
				label: 'Move left',
				icon: 'chevron-left',
				disabled: at <= 0,
				action: () => {
					workspace.move(w.key, at - 1);
					requestAnimationFrame(() => buttonOf(w.key)?.focus());
				}
			},
			{
				label: 'Move right',
				icon: 'chevron-right',
				disabled: at < 0 || at >= entries.length - 1,
				action: () => {
					workspace.move(w.key, at + 1);
					requestAnimationFrame(() => buttonOf(w.key)?.focus());
				}
			}
		);
		contextMenu.show(e, items, buttonOf(w.key));
	}

	/** Reset a built-in workspace: the one on screen through the dock, another straight in the settings (it is rebuilt when it is chosen). */
	function resetBuiltin(key: WorkspaceKey) {
		if (key === workspace.shown) workspace.reset();
		else if (isBuiltinKey(key)) {
			settings.resetWorkspace(key);
			toast.success(`${workspaceLabel(settings.workspaces, key)} workspace reset to its default arrangement`);
		}
	}
</script>

<svelte:window onkeydowncapture={onWindowKey} onblur={abandon} />

<div
	bind:this={list}
	role="group"
	aria-label="Workspace"
	onwheel={wheel}
	style="-webkit-app-region:no-drag;position:relative;display:inline-flex;align-items:center;gap:2px;padding:2px;border-radius:var(--radius-md);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);max-width:max(300px, calc(100vw - 640px));overflow-x:auto;scrollbar-width:none;overscroll-behavior:contain"
>
	{#each entries as w (w.key)}
		{@const on = workspace.shown === w.key}
		{@const lifted = drag?.moving && drag.key === w.key}
		<button
			class="k-tab"
			data-workspace={w.key}
			aria-pressed={on}
			use:tip={w.builtin ? settings.tip(`${w.label} workspace`, `workspace.${w.key}` as ActionId, w.hint) : { text: `${w.label} workspace`, detail: w.hint }}
			onclick={(e) => choose(e, w.key)}
			onkeydown={onKey}
			onpointerdown={(e) => down(e, w.key)}
			onpointermove={move}
			onpointerup={up}
			onpointercancel={abandon}
			oncontextmenu={(e) => menu(e, w)}
			style="white-space:nowrap;{lifted ? 'opacity:.55;cursor:grabbing' : ''}">{w.label}</button
		>
	{/each}
	<button
		class="k-tab"
		data-workspace-add
		aria-label="Save the panels as a new workspace"
		aria-disabled={full !== null}
		use:tip={full ?? settings.tip('Save the panels as a new workspace', 'window.saveWorkspace', 'Keeps this arrangement under a name of your own.')}
		onclick={(e) => {
			workspace.askToSave();
			if (e.detail > 0) (e.currentTarget as HTMLElement).blur();
		}}
		onkeydown={(e) => {
			// The arrows walk the workspaces; the + is one step past the last.
			if (e.key === 'ArrowLeft' || e.key === 'End') {
				e.preventDefault();
				buttonOf(entries[entries.length - 1]?.key)?.focus();
			}
		}}
		style="display:inline-flex;align-items:center;padding-inline:7px"
	>
		<Icon n="plus" s={13} />
	</button>
	{#if drag?.moving && drag.marker !== null}
		<span
			aria-hidden="true"
			style="position:absolute;top:3px;bottom:3px;left:{drag.marker - 1}px;width:2px;border-radius:var(--radius-full);background:var(--kerf-500);pointer-events:none"
		></span>
	{/if}
</div>
