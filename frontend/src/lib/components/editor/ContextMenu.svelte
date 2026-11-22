<script lang="ts">
	import { tick, untrack } from 'svelte';
	import Icon from './Icon.svelte';
	import Kbd from './Kbd.svelte';
	import { contextMenu } from '#lib/context-menu.svelte';
	import type { MenuItem } from '#lib/context-menu.svelte';
	import { contextRows } from '#lib/menu-items';
	import { menuKey, rowFor, startRow } from '#lib/menu-nav';
	import { bubbleFor } from '#lib/menu-bubble';

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
	/** The submenu that is open: an index into `contextMenu.items`, -1 while none is. One level, drawn as a panel of
	 *  its own beside the menu (a scrolling panel clips what is positioned inside it). */
	let sub = $state(-1);
	let subEl = $state<HTMLDivElement | null>(null);
	let subPos = $state({ x: 0, y: 0 });
	/** The row of the submenu that holds its tab stop, as `active` does for the menu. */
	let subActive = $state(-1);
	/** Whether focus is in the submenu (else in the menu itself). */
	let subFocus = $state(false);
	/** The reason an off row gives, shown beside its panel while it is lit. */
	let bubble = $state<{ text: string; side: 'right' | 'left'; at: number; top: number } | null>(null);
	let closeTimer: ReturnType<typeof setTimeout> | null = null;
	/** What had focus when the menu opened, to give it back when the menu closes without a
	 *  press elsewhere having moved it. */
	let returnTo: HTMLElement | null = null;
	const rows = $derived(contextRows(contextMenu.items));
	const subItems = $derived.by((): MenuItem[] => {
		const open = sub >= 0 ? contextMenu.items[sub] : undefined;
		return open?.type === 'submenu' ? open.items : [];
	});
	const subRows = $derived(contextRows(subItems));
	const subLabel = $derived.by((): string | undefined => {
		const open = sub >= 0 ? contextMenu.items[sub] : undefined;
		return open?.type === 'submenu' ? open.label : undefined;
	});

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

	// A submenu opens beside its row: to the right of the menu, or to the left when there is no room, and kept inside
	// the window like the menu is.
	$effect(() => {
		void sub;
		void subItems;
		if (!here || !el || !subEl || sub < 0) return;
		const row = rowEl(sub);
		if (!row) return;
		const margin = 6;
		const menu = el.getBoundingClientRect();
		const r = row.getBoundingClientRect();
		const w = subEl.offsetWidth;
		const h = subEl.offsetHeight;
		let x = menu.right - 2;
		if (x + w + margin > own.innerWidth) x = menu.left - w + 2;
		const y = Math.min(r.top - 5, own.innerHeight - h - margin);
		subPos = { x: Math.max(margin, x), y: Math.max(margin, y) };
	});

	// An off row says why, beside the panel it is in and only while it is lit (by the pointer or the keys: they are
	// the same row). It is not a row of the menu, so the menu stays as short as its rows.
	$effect(() => {
		void active;
		void subActive;
		void subFocus;
		void sub;
		void contextMenu.items;
		if (!here || !el) {
			bubble = null;
			return;
		}
		const items = subFocus ? subItems : contextMenu.items;
		const at = subFocus ? subActive : active;
		const item = at >= 0 ? items[at] : undefined;
		const row = subFocus ? subRowEl(at) : rowEl(at);
		const panel = subFocus ? subEl : el;
		if (!item || !row || !panel || (item.type !== undefined && item.type !== 'item' && item.type !== 'submenu') || !item.disabled || !item.reason) {
			bubble = null;
			return;
		}
		const p = panel.getBoundingClientRect();
		const r = row.getBoundingClientRect();
		// Away from the panel open beside this one (a submenu beside its menu, the menu beside its submenu), so it never
		// covers the row that opened it (`menu-bubble.ts`).
		const other = subFocus ? el : sub >= 0 ? subEl : null;
		const o = other?.getBoundingClientRect();
		const b = bubbleFor({ left: p.left, right: p.right }, o ? { left: o.left, right: o.right } : null, own.innerWidth);
		bubble = { text: item.reason, side: b.side, at: b.side === 'right' ? b.at : own.innerWidth - b.at, top: Math.max(6, r.top - 2) };
	});

	// Whatever closed the menu — a row, Escape, a press elsewhere, a scroll, a resize, the window losing
	// focus — what it remembered to give focus back to goes with it, so the next menu starts clean.
	$effect(() => {
		if (!here) returnTo = null;
	});

	function rowEl(i: number): HTMLElement | null {
		return el?.querySelector<HTMLElement>(`[data-row="${i}"]`) ?? null;
	}

	function subRowEl(i: number): HTMLElement | null {
		return subEl?.querySelector<HTMLElement>(`[data-sub-row="${i}"]`) ?? null;
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
			sub = -1;
			subActive = -1;
			subFocus = false;
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
			if (el && e.target instanceof Node && el.contains(e.target)) {
				// The menu itself scrolling moves its rows from under an open submenu.
				if (sub >= 0) sub = -1;
				return;
			}
			if (subEl && e.target instanceof Node && subEl.contains(e.target)) return;
			contextMenu.close();
		};
		const onResize = () => contextMenu.close();
		const onBlur = () => contextMenu.close();
		w.addEventListener('scroll', onScroll, true);
		w.addEventListener('pointerdown', onWindowPointerDown);
		w.addEventListener('keydown', onKey, true);
		w.addEventListener('resize', onResize);
		w.addEventListener('blur', onBlur);
		// The window going away takes its menu with it (a detached panel closed with the menu up fires no blur).
		w.addEventListener('pagehide', onBlur);
		return () => {
			w.removeEventListener('scroll', onScroll, true);
			w.removeEventListener('pointerdown', onWindowPointerDown);
			w.removeEventListener('keydown', onKey, true);
			w.removeEventListener('resize', onResize);
			w.removeEventListener('blur', onBlur);
			w.removeEventListener('pagehide', onBlur);
		};
	});

	function onWindowPointerDown(e: PointerEvent) {
		if (!contextMenu.visible) return;
		// A press inside the menu is handled by the item's onclick — don't close
		// here or the element would unmount before the click fires.
		if (el && e.target instanceof Node && el.contains(e.target)) return;
		if (subEl && e.target instanceof Node && subEl.contains(e.target)) return;
		contextMenu.close();
	}

	/** Close, and put focus back where it was. */
	function dismiss() {
		const back = returnTo;
		returnTo = null;
		contextMenu.close();
		if (back?.isConnected) back.focus({ preventScroll: true });
	}

	/** Make a row the lit one without moving focus to it: its reason shows. */
	function lightRow(level: 0 | 1, i: number) {
		if (level === 0) {
			subFocus = false;
			active = i;
		} else {
			subFocus = true;
			subActive = i;
		}
	}

	function focusRow(i: number) {
		subFocus = false;
		active = i;
		rowEl(i)?.focus();
	}

	function focusSub(i: number) {
		subFocus = true;
		subActive = i;
		subRowEl(i)?.focus();
	}

	function clearCloseTimer() {
		if (closeTimer !== null) clearTimeout(closeTimer);
		closeTimer = null;
	}

	/** Open the submenu of row `i`; with `focus`, take the keys into it (the keyboard and a click do; hovering
	 *  does not, so the pointer's row keeps its light). */
	async function openSub(i: number, focus: boolean) {
		clearCloseTimer();
		sub = i;
		if (!focus) return;
		await tick();
		const first = startRow(subRows);
		if (first >= 0) focusSub(first);
	}

	/** Close the submenu; with `back`, focus returns to the row that opened it. */
	function closeSub(back: boolean) {
		clearCloseTimer();
		const was = sub;
		sub = -1;
		subActive = -1;
		subFocus = false;
		if (back && was >= 0) focusRow(was);
	}

	function onKey(e: KeyboardEvent) {
		if (!contextMenu.visible || !here) return;
		const k = menuKey(e);
		if (!k) return;
		// A key that is typed while focus is elsewhere is not the menu's — except the one that
		// closes it.
		const inRoot = !!el && e.target instanceof Node && el.contains(e.target);
		const inSub = !!subEl && e.target instanceof Node && subEl.contains(e.target);
		if (!inRoot && !inSub && k.do !== 'close') return;
		if (k.do === 'close') {
			e.preventDefault();
			e.stopPropagation();
			// Escape closes one level: the submenu first, whichever of the two has the focus.
			if (inSub || sub >= 0) closeSub(true);
			else dismiss();
		} else if (k.do === 'leave') {
			// Tab goes on from where focus was, not from the menu that is about to go.
			dismiss();
		} else if (k.do === 'run') {
			e.preventDefault();
			e.stopPropagation();
			const item = inSub ? subItems[subActive] : contextMenu.items[active];
			if (item) run(item, inSub ? undefined : active);
		} else if (k.do === 'open') {
			e.preventDefault();
			e.stopPropagation();
			if (!inSub && contextMenu.items[active]?.type === 'submenu') void openSub(active, true);
		} else if (k.do === 'back') {
			e.preventDefault();
			e.stopPropagation();
			if (inSub) closeSub(true);
		} else {
			if (k.do !== 'type') e.preventDefault();
			e.stopPropagation();
			const to = rowFor(inSub ? subRows : rows, inSub ? subActive : active, k);
			if (to !== null) inSub ? focusSub(to) : focusRow(to);
		}
	}

	/** Run a row. A submenu row opens its menu instead (`at`: where it sits in the menu). */
	function run(item: MenuItem, at?: number) {
		if (item.type === 'submenu') {
			if (!item.disabled && at !== undefined) void openSub(at, true);
			return;
		}
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

	/** The pointer is on a row of the menu: it is lit, a submenu row opens its menu, any other row lets an open one
	 *  go after a moment — long enough to cross to it. */
	function onRowMove(e: PointerEvent, i: number, item: MenuItem) {
		const row = e.currentTarget as HTMLElement;
		if ((e.movementX !== 0 || e.movementY !== 0) && !row.matches(':focus')) row.focus({ preventScroll: true });
		if (item.type === 'submenu') {
			if (!item.disabled && sub !== i) void openSub(i, false);
			else clearCloseTimer();
		} else if (sub >= 0 && closeTimer === null) {
			closeTimer = setTimeout(() => {
				closeTimer = null;
				sub = -1;
				subActive = -1;
				subFocus = false;
			}, 260);
		}
	}
</script>

{#snippet panelRows(list: MenuItem[], level: 0 | 1)}
	{#each list as item, i (i)}
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
			{@const lit = level === 0 ? active === i : subActive === i}
			{@const off = !!item.disabled}
			<button
				type="button"
				role="menuitem"
				class="k-mrow"
				data-row={level === 0 ? i : undefined}
				data-sub-row={level === 1 ? i : undefined}
				data-danger={item.type !== 'submenu' && item.danger ? '' : undefined}
				aria-disabled={off || undefined}
				aria-haspopup={item.type === 'submenu' ? 'menu' : undefined}
				aria-expanded={item.type === 'submenu' ? sub === i : undefined}
				aria-describedby={lit && bubble && off ? 'k-menu-reason' : undefined}
				tabindex={lit ? 0 : -1}
				style={item.type === 'submenu' && sub === i ? 'background:var(--surface-hover)' : undefined}
				onclick={() => {
					// A tap or a click on a row that is off says why — a touch screen has no hover, and not every browser
					// focuses a button it was tapped on — by lighting it, which is what shows the reason.
					if (off && item.type !== 'submenu') lightRow(level, i);
					run(item, level === 0 ? i : undefined);
				}}
				onfocus={() => {
					if (level === 0) {
						active = i;
						subFocus = false;
					} else {
						subActive = i;
						subFocus = true;
					}
				}}
				onpointermove={(e) => {
					if (level === 0) onRowMove(e, i, item);
					else if ((e.movementX !== 0 || e.movementY !== 0) && !e.currentTarget.matches(':focus')) e.currentTarget.focus({ preventScroll: true });
				}}
			>
				<span style="width:15px;display:grid;place-items:center;flex:none;color:inherit">
					{#if item.icon}<Icon n={item.icon} s={14} color="currentColor" />{/if}
				</span>
				<span style="flex:1;min-width:0;white-space:nowrap">{item.label}</span>
				{#if item.type === 'submenu'}
					<span style="flex:none;display:grid;place-items:center;color:{off ? 'var(--text-disabled)' : 'var(--text-muted)'}"
						><Icon n="chevron-right" s={12} color="currentColor" /></span
					>
				{:else if item.shortcut}
					<span style="flex:none;color:{off ? 'var(--text-disabled)' : 'var(--text-muted)'}"><Kbd keys={item.shortcut} inherit /></span>
				{:else if item.meta}
					<span style="flex:none;font-variant-numeric:tabular-nums;color:{off ? 'var(--text-disabled)' : 'var(--text-muted)'}">{item.meta}</span>
				{/if}
			</button>
		{/if}
	{/each}
{/snippet}

{#if here}
	<div
		bind:this={el}
		role="menu"
		tabindex="-1"
		oncontextmenu={(e) => e.preventDefault()}
		style="position:fixed;left:{pos.x}px;top:{pos.y}px;z-index:1000;min-width:188px;max-height:{own.innerHeight - 12}px;overflow-y:auto;padding:4px;border-radius:var(--radius-md);background:var(--surface-raised);border:var(--line-width) solid var(--border-strong);box-shadow:var(--shadow-lg);font-family:var(--font-sans)"
	>
		{@render panelRows(contextMenu.items, 0)}
	</div>
	{#if sub >= 0 && subItems.length > 0}
		<div
			bind:this={subEl}
			role="menu"
			aria-label={subLabel}
			tabindex="-1"
			onpointerenter={clearCloseTimer}
			oncontextmenu={(e) => e.preventDefault()}
			style="position:fixed;left:{subPos.x}px;top:{subPos.y}px;z-index:1001;min-width:188px;max-height:{own.innerHeight - 12}px;overflow-y:auto;padding:4px;border-radius:var(--radius-md);background:var(--surface-raised);border:var(--line-width) solid var(--border-strong);box-shadow:var(--shadow-lg);font-family:var(--font-sans)"
		>
			{@render panelRows(subItems, 1)}
		</div>
	{/if}
	{#if bubble}
		<!-- Why the lit row is off: beside its panel, never over a row. -->
		<div
			id="k-menu-reason"
			role="tooltip"
			data-menu-reason
			style="position:fixed;{bubble.side === 'right' ? `left:${bubble.at}px` : `right:${bubble.at}px`};top:{bubble.top}px;z-index:1002;max-width:264px;padding:5px 9px;border-radius:var(--radius-sm);background:var(--surface-raised);border:var(--line-width) solid var(--border-strong);box-shadow:var(--shadow-lg);font-family:var(--font-sans);font-size:var(--text-xs);line-height:1.4;color:var(--text-secondary);pointer-events:none"
		>
			{bubble.text}
		</div>
	{/if}
{/if}
