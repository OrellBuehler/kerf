<script lang="ts">
	import Icon from './Icon.svelte';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';

	let el = $state<HTMLDivElement | null>(null);
	let pos = $state({ x: 0, y: 0 });

	// Clamp the menu into the viewport once it's measured. Runs after the element
	// mounts (so offsetWidth/Height are real) and whenever the open position moves.
	$effect(() => {
		if (!contextMenu.visible || !el) return;
		const margin = 6;
		const w = el.offsetWidth;
		const h = el.offsetHeight;
		let x = contextMenu.x;
		let y = contextMenu.y;
		if (x + w + margin > window.innerWidth) x = window.innerWidth - w - margin;
		if (y + h + margin > window.innerHeight) y = window.innerHeight - h - margin;
		pos = { x: Math.max(margin, x), y: Math.max(margin, y) };
	});

	// Dismiss on a press outside the menu, on scroll (any scroller, hence capture),
	// and on resize. Esc is handled below.
	$effect(() => {
		if (!contextMenu.visible) return;
		const onScroll = () => contextMenu.close();
		window.addEventListener('scroll', onScroll, true);
		return () => window.removeEventListener('scroll', onScroll, true);
	});

	function onWindowPointerDown(e: PointerEvent) {
		if (!contextMenu.visible) return;
		// A press inside the menu is handled by the item's onclick — don't close
		// here or the element would unmount before the click fires.
		if (el && e.target instanceof Node && el.contains(e.target)) return;
		contextMenu.close();
	}

	function onKey(e: KeyboardEvent) {
		if (contextMenu.visible && e.key === 'Escape') {
			e.preventDefault();
			contextMenu.close();
		}
	}

	function run(item: MenuItem) {
		if (item.type === 'separator' || item.disabled) return;
		contextMenu.close();
		item.action();
	}
</script>

<svelte:window
	onpointerdown={onWindowPointerDown}
	onkeydown={onKey}
	onresize={() => contextMenu.close()}
	onblur={() => contextMenu.close()}
/>

{#if contextMenu.visible}
	<div
		bind:this={el}
		role="menu"
		tabindex="-1"
		oncontextmenu={(e) => e.preventDefault()}
		style="position:fixed;left:{pos.x}px;top:{pos.y}px;z-index:1000;min-width:188px;padding:5px;border-radius:var(--radius-md);background:var(--surface-raised);border:1px solid var(--border-strong);box-shadow:var(--shadow-lg);font-family:var(--font-sans)"
	>
		{#each contextMenu.items as item, i (i)}
			{#if item.type === 'separator'}
				<div style="height:1px;margin:5px 4px;background:var(--border-subtle)"></div>
			{:else}
				<button
					role="menuitem"
					disabled={item.disabled}
					onclick={() => run(item)}
					style="display:flex;align-items:center;gap:9px;width:100%;padding:6px 8px;border:none;border-radius:var(--radius-sm);background:none;cursor:{item.disabled
						? 'default'
						: 'pointer'};text-align:left;font-size:12.5px;color:{item.disabled
						? 'var(--text-disabled)'
						: item.danger
							? 'var(--red-400, #f08a82)'
							: 'var(--text-primary)'};opacity:{item.disabled ? 0.55 : 1}"
					onpointerenter={(e) => {
						if (!item.disabled) (e.currentTarget as HTMLElement).style.background = 'var(--surface-hover)';
					}}
					onpointerleave={(e) => ((e.currentTarget as HTMLElement).style.background = 'none')}
				>
					<span style="width:15px;display:grid;place-items:center;flex:none;color:inherit">
						{#if item.icon}<Icon n={item.icon} s={14} color="currentColor" />{/if}
					</span>
					<span style="flex:1">{item.label}</span>
					{#if item.shortcut}
						<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled);flex:none"
							>{item.shortcut}</span
						>
					{/if}
				</button>
			{/if}
		{/each}
	</div>
{/if}
