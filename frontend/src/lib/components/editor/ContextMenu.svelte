<script lang="ts">
	import { untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import Kbd from './Kbd.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import type { MenuItem } from '#lib/context-menu.svelte';
	import { contextRows } from '#lib/menu-items';
	import { menuKey, rowFor, startRow } from '#lib/menu-nav';

	/** The window this instance draws in. One is mounted in the editor window and one in
	 *  each detached window (`popout.svelte.ts`): a menu belongs to the window it was
	 *  opened in, and draws there. */
	let { win }: { win?: Window } = $props();
	const own = $derived(win ?? window);
	const here = $derived(contextMenu.visible && (contextMenu.win ?? window) === own);

	let el = $state<HTMLDivElement | null>(null);
	let pos = $state({ x: 0, y: 0 });
	/** The row that holds the tab stop and, once the keys are in use, focus: an index into
	 *  `contextMenu.items`, -1 while none does. */
	let active = $state(-1);
	/** What had focus when the menu opened, to give it back when the menu closes without a
	 *  press elsewhere having moved it. */
	let returnTo: HTMLElement | null = null;
	const rows = $derived(contextRows(contextMenu.items));

	// Clamp the menu into the viewport once it's measured. Runs after the element
	// mounts (so offsetWidth/Height are real) and whenever the open position moves.
	$effect(() => {
		if (!here || !el) return;
		const margin = 6;
		const w = el.offsetWidth;
		const h = el.offsetHeight;
		let x = contextMenu.x;
		let y = contextMenu.y;
		if (x + w + margin > own.innerWidth) x = own.innerWidth - w - margin;
		if (y + h + margin > own.innerHeight) y = own.innerHeight - h - margin;
		pos = { x: Math.max(margin, x), y: Math.max(margin, y) };
	});

	// Whatever closed the menu — a row, Escape, a press elsewhere, a scroll, a resize, the window losing
	// focus — what it remembered to give focus back to goes with it, so the next menu starts clean.
	$effect(() => {
		if (!here) returnTo = null;
	});

	function rowEl(i: number): HTMLElement | null {
		return el?.querySelector<HTMLElement>(`[data-row="${i}"]`) ?? null;
	}

	// The menu takes focus when it opens, so the keys have somewhere to start. Opened with the
	// keyboard (the Menu key, Shift+F10, Enter on a button that opens one) the first row is lit and
	// ready; opened with the pointer it is the panel that has focus, so nothing is lit until a key
	// or the pointer picks a row.
	$effect(() => {
		void contextMenu.items;
		if (!here || !el) return;
		const menu = el;
		untrack(() => {
			const was = own.document.activeElement;
			if (was instanceof HTMLElement && was !== own.document.body && !menu.contains(was)) returnTo = was;
			const keys = !!returnTo?.matches(':focus-visible');
			active = keys ? startRow(rows) : -1;
			(keys && active >= 0 ? rowEl(active) : menu)?.focus({ preventScroll: true });
		});
	});

	// Dismiss on a press outside the menu, on scroll (any scroller, hence capture), on resize and
	// when the window loses focus — in the window it is open in. Keys are heard in the capture phase,
	// before the page's shortcuts: a menu is modal for them.
	$effect(() => {
		if (!here) return;
		const w = own;
		// The menu scrolls when it is taller than the window it is in; that is no reason to close it.
		const onScroll = (e: Event) => {
			if (el && e.target instanceof Node && el.contains(e.target)) return;
			contextMenu.close();
		};
		const onResize = () => contextMenu.close();
		const onBlur = () => contextMenu.close();
		w.addEventListener('scroll', onScroll, true);
		w.addEventListener('pointerdown', onWindowPointerDown);
		w.addEventListener('keydown', onKey, true);
		w.addEventListener('resize', onResize);
		w.addEventListener('blur', onBlur);
		return () => {
			w.removeEventListener('scroll', onScroll, true);
			w.removeEventListener('pointerdown', onWindowPointerDown);
			w.removeEventListener('keydown', onKey, true);
			w.removeEventListener('resize', onResize);
			w.removeEventListener('blur', onBlur);
		};
	});

	function onWindowPointerDown(e: PointerEvent) {
		if (!contextMenu.visible) return;
		// A press inside the menu is handled by the item's onclick — don't close
		// here or the element would unmount before the click fires.
		if (el && e.target instanceof Node && el.contains(e.target)) return;
		contextMenu.close();
	}

	/** Close, and put focus back where it was. */
	function dismiss() {
		const back = returnTo;
		returnTo = null;
		contextMenu.close();
		if (back?.isConnected) back.focus({ preventScroll: true });
	}

	function focusRow(i: number) {
		active = i;
		rowEl(i)?.focus();
	}

	function onKey(e: KeyboardEvent) {
		if (!contextMenu.visible || !here) return;
		const k = menuKey(e);
		if (!k) return;
		// A key that is typed while focus is elsewhere is not the menu's — except the one that
		// closes it.
		const inside = !!el && e.target instanceof Node && el.contains(e.target);
		if (!inside && k.do !== 'close') return;
		if (k.do === 'close') {
			e.preventDefault();
			e.stopPropagation();
			dismiss();
		} else if (k.do === 'leave') {
			// Tab goes on from where focus was, not from the menu that is about to go.
			dismiss();
		} else if (k.do === 'run') {
			e.preventDefault();
			e.stopPropagation();
			const item = contextMenu.items[active];
			if (item) run(item);
		} else {
			if (k.do !== 'type') e.preventDefault();
			e.stopPropagation();
			const to = rowFor(rows, active, k);
			if (to !== null) focusRow(to);
		}
	}

	function run(item: MenuItem) {
		if (item.type !== undefined && item.type !== 'item') return;
		if (item.disabled) return;
		const back = returnTo;
		returnTo = null;
		contextMenu.close();
		// Whatever the action opens (a dialog) then finds focus where the menu was opened from,
		// and gives it back there.
		if (back?.isConnected) back.focus({ preventScroll: true });
		item.action();
	}
</script>

{#if here}
	<div
		bind:this={el}
		role="menu"
		tabindex="-1"
		oncontextmenu={(e) => e.preventDefault()}
		style="position:fixed;left:{pos.x}px;top:{pos.y}px;z-index:1000;min-width:188px;max-height:{own.innerHeight - 12}px;overflow-y:auto;padding:4px;border-radius:var(--radius-md);background:var(--surface-raised);border:var(--line-width) solid var(--border-strong);box-shadow:var(--shadow-lg);font-family:var(--font-sans)"
	>
		{#each contextMenu.items as item, i (i)}
			{#if item.type === 'separator'}
				<div role="separator" style="height:var(--line-width);margin:4px 6px;background:var(--border-default)"></div>
			{:else if item.type === 'header'}
				<div style="padding:5px 8px 4px;min-width:0;max-width:300px">
					<div
						style="font-size:12px;font-weight:600;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
					>
						{item.label}
					</div>
					{#if item.sub}
						<div
							style="font-size:var(--text-xs);color:var(--text-muted);margin-top:2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
						>
							{item.sub}
						</div>
					{/if}
				</div>
			{:else if item.type === 'info'}
				<div style="padding:2px 8px;font-size:11px;line-height:16px;max-width:300px">
					<div style="display:flex;align-items:baseline;gap:12px">
						<span style="flex:none;color:var(--text-muted)">{item.label}</span>
						<span
							style="flex:1;min-width:0;text-align:right;font-family:var(--font-mono);font-size:var(--text-xs);color:var(--text-secondary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
							>{item.value}</span
						>
					</div>
					{#if item.title && item.title !== item.value}
						<!-- What a tooltip used to say over the next row's label: said under its own row instead. -->
						<div style="font-size:var(--text-xs);line-height:1.3;color:var(--text-muted);text-align:right">{item.title}</div>
					{/if}
				</div>
			{:else}
				<button
					type="button"
					role="menuitem"
					class="k-mrow"
					data-row={i}
					data-danger={item.danger || undefined}
					aria-disabled={item.disabled || undefined}
					tabindex={active === i ? 0 : -1}
					onclick={() => run(item)}
					onfocus={() => (active = i)}
					onpointermove={(e) => {
						if ((e.movementX !== 0 || e.movementY !== 0) && !e.currentTarget.matches(':focus')) e.currentTarget.focus({ preventScroll: true });
					}}
				>
					<span style="width:15px;display:grid;place-items:center;flex:none;color:inherit">
						{#if item.icon}<Icon n={item.icon} s={14} color="currentColor" />{/if}
					</span>
					<span style="flex:1;min-width:0">
						{item.label}
						{#if item.disabled && item.reason}
							<!-- A disabled line says why, in the words the backend would have refused with. -->
							<span
								data-menu-reason
								style="display:block;max-width:250px;margin-top:1px;font-size:var(--text-xs);line-height:1.3;font-weight:400;color:var(--text-muted)"
								>{item.reason}</span
							>
						{/if}
					</span>
					{#if item.shortcut}
						<span style="flex:none;color:{item.disabled ? 'var(--text-disabled)' : 'var(--text-muted)'}"
							><Kbd keys={item.shortcut} inherit /></span
						>
					{/if}
				</button>
			{/if}
		{/each}
	</div>
{/if}
