<script lang="ts">
	// The menu bar: File / Edit / View / Window / Help at the top left of the title
	// bar. What is in the menus is `menus.ts`; this is the widget — the ARIA menubar
	// pattern (a roving tabindex along the bar, `menu` panels, `menuitem` /
	// `menuitemcheckbox` / `menuitemradio` entries, submenus) with the behaviour of
	// the menu bar people already know:
	//
	//   - a click opens a menu, and once one is open, hovering another top-level
	//     title switches to it;
	//   - ← → move along the bar (and between open menus), ↓ ↑ move down a menu,
	//     → opens a submenu, ← closes it, Enter / Space run, Esc closes one level,
	//     a letter jumps to the next entry that starts with it;
	//   - Alt pressed and released on its own, or F10, focuses the bar (not when F10
	//     is bound to something, not while a field is being typed in, and not when
	//     the Alt was part of an Alt-drag).
	//
	// An entry runs the action it names (`onAction`) — the page's own key table, so
	// a menu and its shortcut are one piece of code — or a command (`onCommand`).
	// Too narrow for five titles (`compact`), the bar becomes one "Menu" button
	// whose entries are the five menus as submenus.
	import { tick, untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import { settings } from '$lib/settings.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { updater } from '$lib/updater.svelte';
	import { inTauri } from '$lib/api';
	import { presetFor } from '$lib/delivery-formats';
	import { uniformPreset } from '$lib/track-heights';
	import {
		buildMenus,
		collapseMenus,
		entryLabel,
		focusable as focusableIn,
		stepFocus,
		typeahead as typeaheadIn,
		type MenuCommand,
		type MenuEntry,
		type MenuState
	} from '$lib/menus';
	import type { ActionId } from '$lib/keymap';

	let {
		compact = false,
		onAction,
		onCommand
	}: {
		compact?: boolean;
		onAction: (id: ActionId) => void;
		onCommand: (command: MenuCommand) => void;
	} = $props();

	const menuState = $derived<MenuState>({
		saved: editor.saved,
		canUndo: editor.canUndo,
		canRedo: editor.canRedo,
		hasClips: editor.timeline.tracks.some((t) => t.clips.length > 0),
		hasSelection: editor.selectedClipIds.length > 0 || editor.selectedOverlayId !== null,
		rippleMode: editor.rippleMode,
		snap: ui.snap,
		minimap: ui.minimap,
		safeAreas: settings.safeAreas,
		hasFrame: !!editor.timeline.format,
		tool: ui.tool,
		delivery: presetFor(editor.timeline.format).id,
		allHeight: uniformPreset(
			ui.heights,
			editor.timeline.tracks.map((t) => t.id)
		),
		workspace: workspace.shown,
		openPanels: workspace.open,
		desktop: inTauri()
	});
	const full = $derived(buildMenus(menuState));
	const tops = $derived(compact ? collapseMenus(full) : full);

	// ---- what is open ------------------------------------------------------------

	/** Which top-level menu is open, and then which entry of each open menu holds
	 *  the next submenu open: `[2, 1]` is the third title's menu with its second
	 *  entry (a submenu) open. */
	let open = $state<number[]>([]);
	/** The title that holds the tab stop. */
	let active = $state(0);
	/** The entry the pointer or the focus is on, by path — what is drawn lit. */
	let hot = $state<string | null>(null);

	let bar = $state<HTMLElement | null>(null);
	/** Where focus was before the bar took it, to give back when it lets go. */
	let returnTo: HTMLElement | null = null;
	let hoverTimer: ReturnType<typeof setTimeout> | null = null;
	/** The title the pointer opened by arriving on it while another menu was open. A
	 *  click on it then is the same gesture finishing, not a request to close it. */
	let hoverOpened: number | null = null;

	const key = (path: readonly number[]) => path.join('.');
	const isOpen = (path: readonly number[]) => path.length <= open.length && path.every((v, i) => open[i] === v);

	function entriesAt(path: readonly number[]): MenuEntry[] {
		let items = tops[path[0]]?.items ?? [];
		for (const i of path.slice(1)) {
			const e = items[i];
			if (!e || e.kind !== 'submenu') return [];
			items = e.items;
		}
		return items;
	}

	/** The menu's entries that can take focus. */
	const focusable = (path: readonly number[]) => focusableIn(entriesAt(path));

	function el(path: readonly number[]): HTMLElement | null {
		return bar?.querySelector<HTMLElement>(`[data-path="${key(path)}"]`) ?? null;
	}

	async function focusPath(path: readonly number[]) {
		await tick();
		el(path)?.focus();
	}

	function closeAll(restore = false) {
		if (hoverTimer) clearTimeout(hoverTimer);
		hoverTimer = null;
		hoverOpened = null;
		const was = open.length > 0 || bar?.contains(document.activeElement);
		open = [];
		hot = null;
		if (restore && was) giveBackFocus();
	}

	function giveBackFocus() {
		const to = returnTo;
		returnTo = null;
		if (to && to.isConnected && !to.hasAttribute('inert')) to.focus();
		else if (bar?.contains(document.activeElement)) (document.activeElement as HTMLElement).blur();
	}

	/** Remember where focus was, the first time it comes into the bar. */
	function noteReturn() {
		const a = document.activeElement;
		if (a instanceof HTMLElement && !bar?.contains(a) && a !== document.body) returnTo = a;
		else if (!bar?.contains(a)) returnTo = null;
	}

	function openTop(i: number, focusFirst = false) {
		open = [i];
		active = i;
		if (focusFirst) {
			const first = focusable([i])[0];
			if (first !== undefined) void focusPath([i, first]);
		}
	}

	function openSub(path: readonly number[], focusFirst = false) {
		open = [...path];
		const first = focusable(path)[0];
		if (focusFirst && first !== undefined) void focusPath([...path, first]);
	}

	function run(e: MenuEntry) {
		if (e.kind === 'separator' || e.kind === 'submenu') return;
		if ('disabled' in e && e.disabled) return;
		closeAll(true);
		if (e.kind === 'action') onAction(e.id);
		else onCommand(e.command);
	}

	// ---- pointer -----------------------------------------------------------------

	function topClick(i: number) {
		if (hoverOpened === i) {
			hoverOpened = null;
			return;
		}
		if (isOpen([i]) && open.length === 1) closeAll(true);
		else openTop(i);
	}

	function topEnter(i: number) {
		hot = key([i]);
		// Once one menu is open the others follow the pointer; until then a title is
		// only a title.
		if (open.length === 0 || open[0] === i) return;
		openTop(i);
		hoverOpened = i;
		el([i])?.focus();
	}

	function entryEnter(path: number[], e: MenuEntry) {
		hot = key(path);
		if (hoverTimer) clearTimeout(hoverTimer);
		const parent = path.slice(0, -1);
		const sub = e.kind === 'submenu';
		// Land on an entry: its own submenu opens, anything deeper than its menu
		// closes — a beat late, so a pointer crossing the entries below on its way
		// into a submenu does not slam it shut.
		hoverTimer = setTimeout(() => {
			if (sub && !('disabled' in e && e.disabled)) open = [...path];
			else if (open.length > parent.length) open = [...parent];
			el(path)?.focus({ preventScroll: true });
		}, sub ? 80 : 120);
	}

	function entryClick(path: number[], e: MenuEntry) {
		if (e.kind === 'submenu') openSub(path);
		else run(e);
	}

	function onWindowPointerDown(ev: PointerEvent) {
		if (!bar || !(ev.target instanceof Node)) return;
		if (bar.contains(ev.target)) {
			noteReturn();
			return;
		}
		if (open.length > 0) closeAll();
	}

	// ---- keyboard ----------------------------------------------------------------

	function pathOf(target: EventTarget | null): number[] | null {
		const node = target instanceof Element ? target.closest<HTMLElement>('[data-path]') : null;
		const raw = node?.dataset.path;
		return raw ? raw.split('.').map(Number) : null;
	}

	function stepTop(from: number, by: 1 | -1): number {
		return (from + by + tops.length) % tops.length;
	}

	const stepIn = (path: readonly number[], at: number, by: 1 | -1) => stepFocus(entriesAt(path), at, by);
	const typeahead = (path: readonly number[], at: number, ch: string) => typeaheadIn(entriesAt(path), at, ch);

	function onBarKey(e: KeyboardEvent) {
		// A chord is not the menu's: close it and let the page have the key.
		if (e.ctrlKey || e.metaKey || e.altKey) {
			if (e.key !== 'Alt') closeAll();
			return;
		}
		const path = pathOf(e.target);
		if (!path) return;
		const handled = () => {
			e.preventDefault();
			e.stopPropagation();
		};
		const top = path.length === 1;
		const parent = path.slice(0, -1);
		const here = path[path.length - 1];
		const entry = top ? null : entriesAt(parent)[here];
		const k = e.key;

		if (k === 'Tab') {
			closeAll();
			return;
		}
		if (k === 'Escape') {
			handled();
			if (top) {
				if (open.length > 0) {
					closeAll();
					el(path)?.focus();
				} else giveBackFocus();
			} else if (parent.length > 1) {
				// Out of a submenu, onto the entry that opened it.
				open = parent;
				void focusPath(parent);
			} else {
				closeAll();
				void focusPath(parent);
			}
			return;
		}
		if (top) {
			const i = here;
			if (k === 'ArrowRight' || k === 'ArrowLeft') {
				handled();
				const next = stepTop(i, k === 'ArrowRight' ? 1 : -1);
				active = next;
				if (open.length > 0) openTop(next, true);
				else el([next])?.focus();
			} else if (k === 'Home' || k === 'End') {
				handled();
				const next = k === 'Home' ? 0 : tops.length - 1;
				active = next;
				if (open.length > 0) openTop(next, true);
				else el([next])?.focus();
			} else if (k === 'ArrowDown' || k === 'Enter' || k === ' ') {
				handled();
				openTop(i, true);
			} else if (k === 'ArrowUp') {
				handled();
				open = [i];
				active = i;
				const last = focusable([i]).at(-1);
				if (last !== undefined) void focusPath([i, last]);
			}
			return;
		}
		// Inside a menu.
		if (k === 'ArrowDown' || k === 'ArrowUp') {
			handled();
			void focusPath([...parent, stepIn(parent, here, k === 'ArrowDown' ? 1 : -1)]);
		} else if (k === 'Home' || k === 'End') {
			handled();
			const idx = focusable(parent);
			const to = k === 'Home' ? idx[0] : idx.at(-1);
			if (to !== undefined) void focusPath([...parent, to]);
		} else if (k === 'ArrowRight') {
			handled();
			if (entry?.kind === 'submenu') openSub(path, true);
			else {
				const next = stepTop(path[0], 1);
				active = next;
				openTop(next, true);
			}
		} else if (k === 'ArrowLeft') {
			handled();
			if (parent.length > 1) {
				open = parent;
				void focusPath(parent);
			} else {
				const next = stepTop(path[0], -1);
				active = next;
				openTop(next, true);
			}
		} else if (k === 'Enter' || k === ' ') {
			handled();
			if (!entry) return;
			if (entry.kind === 'submenu') openSub(path, true);
			else run(entry);
		} else if (k.length === 1 && /\S/.test(k)) {
			// A letter belongs to the menu while one is open: it jumps, or does nothing,
			// but is never the razor tool.
			handled();
			const to = typeahead(parent, here, k.toLowerCase());
			if (to !== null) void focusPath([...parent, to]);
		}
	}

	// ---- reaching the bar from anywhere -------------------------------------------

	let altArmed = false;

	/** Alt / F10: the bar's first title takes focus, as in every menu bar. */
	function focusBar() {
		noteReturn();
		active = 0;
		void focusPath([0]);
	}

	function editable(t: EventTarget | null): boolean {
		return t instanceof Element && !!t.closest('input:not([type="range"]), textarea, select, [contenteditable="true"]');
	}

	function onWindowKeyDown(e: KeyboardEvent) {
		if (e.key === 'Alt') {
			altArmed = !e.repeat && !e.ctrlKey && !e.metaKey && !e.shiftKey && !e.getModifierState?.('AltGraph');
			return;
		}
		altArmed = false;
		if (e.key === 'F10' && !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey && !e.defaultPrevented) {
			// Not if the user has put F10 to work.
			if (settings.actionFor(e) !== null || editable(e.target)) return;
			e.preventDefault();
			focusBar();
		}
	}

	function onWindowKeyUp(e: KeyboardEvent) {
		if (e.key !== 'Alt') return;
		const armed = altArmed;
		altArmed = false;
		if (!armed || editable(e.target) || e.defaultPrevented) return;
		e.preventDefault();
		focusBar();
	}

	// An Alt-drag, an Alt-click or an Alt-scroll is not "Alt on its own".
	function disarmAlt() {
		altArmed = false;
	}

	// ---- housekeeping --------------------------------------------------------------

	/** Put a panel where it opens and keep it inside the window. Panels are `fixed`,
	 *  placed from the rect of what opened them (its wrapper): a panel that scrolls
	 *  because it is taller than the room would clip the submenus nested in it if they
	 *  were positioned inside it. A menu opens below its title, a submenu beside its
	 *  entry; one that would leave through the right edge slides back (a submenu flips
	 *  to its parent's other side, and slides in over it if that is no better), and one
	 *  taller than the room scrolls. */
	function fit(node: HTMLElement, sub: boolean) {
		const anchor = node.parentElement?.getBoundingClientRect();
		if (!anchor) {
			node.style.visibility = 'visible';
			return;
		}
		const margin = 6;
		const vw = window.innerWidth;
		const vh = window.innerHeight;
		const w = node.offsetWidth;
		let x = sub ? anchor.right - 2 : anchor.left;
		let y = sub ? anchor.top - 5 : anchor.bottom + 3;
		if (x + w > vw - margin) x = sub ? anchor.left - w + 2 : vw - margin - w;
		x = Math.max(margin, Math.min(x, vw - margin - w));
		const h = node.offsetHeight;
		if (y + h > vh - margin) {
			if (sub) y = Math.max(margin, vh - margin - h);
			if (y + h > vh - margin) {
				node.style.maxHeight = `${Math.max(120, vh - margin - y)}px`;
				node.style.overflowY = 'auto';
			}
		}
		node.style.left = `${x}px`;
		node.style.top = `${y}px`;
		node.style.visibility = 'visible';
	}

	// Opening a dialog, or going to another window, puts the menus away; so does
	// the bar changing shape under an open menu.
	const modal = $derived(settings.open || ui.exportDialog || updater.dialogOpen || ui.voiceoverDialog !== null);
	$effect(() => {
		// `closeAll` reads what it writes; this must depend on the dialogs alone.
		if (modal) untrack(() => closeAll());
	});
	$effect(() => {
		void compact;
		open = [];
		active = 0;
	});
	$effect(() => {
		if (open.length === 0) hot = null;
	});

	const accel = (e: MenuEntry): string =>
		e.kind === 'action' ? settings.shortcut(e.id) : e.kind === 'command' ? (e.hint ?? '') : '';
</script>

<svelte:window
	onpointerdowncapture={onWindowPointerDown}
	onkeydown={onWindowKeyDown}
	onkeyup={onWindowKeyUp}
	onpointerdown={disarmAlt}
	onwheel={disarmAlt}
	onblur={() => closeAll()}
	onresize={() => closeAll()}
/>

{#snippet panel(items: MenuEntry[], path: number[], sub: boolean)}
	<div
		role="menu"
		aria-label={path.length === 1 ? tops[path[0]].label : undefined}
		aria-labelledby={path.length > 1 ? `kerf-menu-${key(path)}` : undefined}
		tabindex="-1"
		use:fit={sub}
		class="kerf-menu"
		style="position:fixed;left:0;top:0;visibility:hidden;z-index:950;width:max-content;min-width:{sub ? 190 : 214}px;max-width:min(420px, calc(100vw - 12px));padding:4px;border-radius:var(--radius-md);background:var(--surface-raised);border:var(--line-width) solid var(--border-strong);box-shadow:var(--shadow-lg);font-family:var(--font-sans);-webkit-app-region:no-drag"
	>
		{#each items as item, j (j)}
			{#if item.kind === 'separator'}
				<div role="separator" style="height:var(--line-width);margin:4px 6px;background:var(--border-default)"></div>
			{:else}
				{@const p = [...path, j]}
				{@const k = key(p)}
				{@const off = 'disabled' in item && !!item.disabled}
				{@const role = item.kind === 'submenu' ? 'menuitem' : (item.role ?? 'item')}
				<div role="none" style="position:relative">
					<button
						type="button"
						id="kerf-menu-{k}"
						data-path={k}
						role={role === 'check' ? 'menuitemcheckbox' : role === 'radio' ? 'menuitemradio' : 'menuitem'}
						aria-checked={role === 'item' ? undefined : !!(item.kind !== 'submenu' && item.checked)}
						aria-haspopup={item.kind === 'submenu' ? 'menu' : undefined}
						aria-expanded={item.kind === 'submenu' ? isOpen(p) : undefined}
						aria-disabled={off || undefined}
						title={off ? (item.kind !== 'submenu' ? item.reason : undefined) : undefined}
						tabindex="-1"
						class="kerf-menu-item"
						onclick={() => entryClick(p, item)}
						onpointerenter={() => entryEnter(p, item)}
						onpointerleave={() => {
							if (hot === k && !(item.kind === 'submenu' && isOpen(p))) hot = null;
						}}
						onfocus={() => (hot = k)}
						style="display:grid;grid-template-columns:16px minmax(0,1fr) auto 12px;align-items:center;column-gap:8px;width:100%;height:26px;padding:0 6px 0 8px;border:none;border-radius:var(--radius-sm);text-align:left;font-size:12.5px;white-space:nowrap;cursor:{off
							? 'default'
							: 'pointer'};background:{hot === k || (item.kind === 'submenu' && isOpen(p))
							? 'var(--surface-hover)'
							: 'transparent'};color:{off ? 'var(--text-disabled)' : 'var(--text-primary)'}"
					>
						<span style="display:grid;place-items:center;color:{off ? 'var(--text-disabled)' : 'var(--kerf-400)'}">
							{#if role !== 'item'}
								{#if item.kind !== 'submenu' && item.checked}<Icon n="check" s={13} color="currentColor" />{/if}
							{:else if item.icon}
								<Icon n={item.icon} s={13} color={off ? 'var(--text-disabled)' : 'var(--text-muted)'} />
							{/if}
						</span>
						<span style="overflow:hidden;text-overflow:ellipsis">{entryLabel(item)}</span>
						<span
							style="font-family:var(--font-mono);font-size:10.5px;padding-left:18px;color:{off
								? 'var(--text-disabled)'
								: 'var(--text-muted)'}">{accel(item)}</span
						>
						<span style="display:grid;place-items:center;color:{off ? 'var(--text-disabled)' : 'var(--text-muted)'}">
							{#if item.kind === 'submenu'}<Icon n="chevron-right" s={12} color="currentColor" />{/if}
						</span>
					</button>
					{#if item.kind === 'submenu' && isOpen(p)}
						{@render panel(item.items, p, true)}
					{/if}
				</div>
			{/if}
		{/each}
	</div>
{/snippet}

<!-- svelte-ignore a11y_interactive_supports_focus -->
<div
	bind:this={bar}
	role="menubar"
	aria-label="Application menu"
	tabindex="-1"
	onkeydown={onBarKey}
	onfocusout={() => {
		// Focus left the bar (Tab, or a click elsewhere): put the menus away. A beat
		// late, because moving between menus removes the entry that had focus and
		// puts it back a moment after.
		setTimeout(() => {
			if (open.length > 0 && !bar?.contains(document.activeElement)) closeAll();
		}, 0);
	}}
	style="display:flex;align-items:center;gap:1px;-webkit-app-region:no-drag"
>
	{#each tops as m, i (m.id)}
		<div role="none" style="position:relative">
			<button
				type="button"
				id="kerf-menu-{i}"
				data-path={String(i)}
				role="menuitem"
				aria-haspopup="menu"
				aria-expanded={open[0] === i}
				aria-label={compact ? 'Menu' : undefined}
				tabindex={active === i ? 0 : -1}
				class="kerf-menu-top"
				onclick={() => topClick(i)}
				onpointerenter={() => topEnter(i)}
				onpointerleave={() => {
					if (hot === key([i])) hot = null;
					if (hoverOpened === i) hoverOpened = null;
				}}
				onfocus={() => {
					active = i;
					noteReturn();
				}}
				style="display:inline-flex;align-items:center;gap:6px;height:24px;padding:0 {compact ? 7 : 9}px;border-radius:var(--radius-sm);cursor:pointer;font:var(--type-label);border:var(--line-width) solid {open[0] ===
				i
					? 'var(--border-strong)'
					: 'transparent'};background:{open[0] === i
					? 'var(--surface-active)'
					: hot === key([i])
						? 'var(--surface-hover)'
						: 'transparent'};color:{open[0] === i || hot === key([i]) ? 'var(--text-primary)' : 'var(--text-secondary)'}"
			>
				{#if compact}<Icon n="menu" s={14} color="currentColor" />{:else}{m.label}{/if}
			</button>
			{#if open[0] === i}
				{@render panel(m.items, [i], false)}
			{/if}
		</div>
	{/each}
</div>

<style>
	/* The lit state is drawn from `hot`, so the pointer and the keys agree; what the
	   stylesheet adds is the ring a keyboard user needs. */
	.kerf-menu-top:focus-visible,
	.kerf-menu-item:focus-visible {
		outline: var(--line-emphasis) solid var(--kerf-400);
		outline-offset: -1px;
	}
	.kerf-menu-item[aria-disabled='true']:hover {
		background: transparent;
	}
</style>
