<script lang="ts">
	// The workspace switcher: Edit / Color / Audio / Motion / Deliver in the title
	// bar. Choosing one swaps the dock's arrangement and nothing else. A group of
	// toggle buttons rather than a tablist — there is no tabpanel to point at, the
	// panels are the whole window — with the arrow keys as a shortcut along it.
	import { workspace } from '$lib/workspace.svelte';
	import { WORKSPACE_SPECS, type WorkspaceId } from '$lib/workspaces';

	let list = $state<HTMLElement | null>(null);

	function onKey(e: KeyboardEvent) {
		const ids = WORKSPACE_SPECS.map((w) => w.id);
		const at = ids.indexOf(document.activeElement?.getAttribute('data-workspace') as WorkspaceId);
		if (at < 0) return;
		let next: number | null = null;
		if (e.key === 'ArrowRight' || e.key === 'ArrowDown') next = (at + 1) % ids.length;
		else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') next = (at - 1 + ids.length) % ids.length;
		else if (e.key === 'Home') next = 0;
		else if (e.key === 'End') next = ids.length - 1;
		if (next === null) return;
		e.preventDefault();
		list?.querySelector<HTMLElement>(`[data-workspace="${ids[next]}"]`)?.focus();
	}

	function choose(e: MouseEvent, id: WorkspaceId) {
		workspace.switchTo(id);
		// A click (as opposed to Enter / Space, whose `detail` is 0) must not leave
		// focus here: a focused button swallows Space, so the transport shortcut
		// would stop working until something else was clicked.
		if (e.detail > 0) (e.currentTarget as HTMLElement).blur();
	}
</script>

<div
	bind:this={list}
	role="group"
	aria-label="Workspace"
	style="-webkit-app-region:no-drag;display:inline-flex;align-items:center;gap:2px;padding:2px;border-radius:var(--radius-md);background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle)"
>
	{#each WORKSPACE_SPECS as w (w.id)}
		{@const on = workspace.shown === w.id}
		<button
			data-workspace={w.id}
			aria-pressed={on}
			title="{w.label} workspace — {w.hint}"
			onclick={(e) => choose(e, w.id)}
			onkeydown={onKey}
			style="height:22px;padding:0 11px;border-radius:var(--radius-sm);cursor:pointer;font:var(--type-label);border:var(--line-width) solid {on
				? 'var(--border-strong)'
				: 'transparent'};background:{on ? 'var(--surface-active)' : 'transparent'};color:{on
				? 'var(--text-primary)'
				: 'var(--text-muted)'};box-shadow:{on ? 'inset 0 -2px 0 var(--kerf-500)' : 'none'};transition:background var(--dur-fast) var(--ease-out),color var(--dur-fast) var(--ease-out)"
			>{w.label}</button
		>
	{/each}
</div>
