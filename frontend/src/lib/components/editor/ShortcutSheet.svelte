<script lang="ts">
	// Help › Keyboard shortcuts, and `?`: every shortcut in force, read-only. Changing one is the Keyboard
	// settings (`KeyboardSettings`), which this opens on request — the two used to be one dialog, so looking
	// a key up put a rebinding editor in front of you.
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import Kbd from './Kbd.svelte';
	import { settings } from '#lib/settings.svelte';
	import { shortcutSheet, unboundLine } from '#lib/shortcut-sheet';

	let { onClose }: { onClose: () => void } = $props();

	const sheet = $derived(shortcutSheet(settings.bindings, settings.platform));
	const more = $derived(unboundLine(sheet.unbound));

	function customize() {
		onClose();
		settings.openSection('keyboard');
	}
</script>

<Modal title="Keyboard shortcuts" icon="keyboard" width={760} rescue initial="[data-close]" {onClose}>
	<div data-shortcut-sheet style="flex:1;min-height:0;overflow-y:auto;padding:12px 18px 14px">
		<div style="column-width:300px;column-gap:28px">
			{#each sheet.groups as g (g.id)}
				<section style="break-inside:avoid;margin-bottom:14px">
					<h3 class="k-overline" style="margin:0 0 5px">{g.label}</h3>
					{#each g.rows as r (r.id)}
						<div style="display:flex;align-items:baseline;gap:10px;padding:2px 0;font-size:12px">
							<span style="flex:1;min-width:0;color:var(--text-secondary)">{r.label}</span>
							<span style="flex:none;display:inline-flex;gap:6px;color:var(--text-primary)">
								{#each r.keys as k (k)}<Kbd keys={k} />{/each}
							</span>
						</div>
					{/each}
				</section>
			{/each}
			<section style="break-inside:avoid;margin-bottom:14px">
				<h3 class="k-overline" style="margin:0 0 5px">Fixed keys</h3>
				{#each sheet.fixed as f (f.keys)}
					<div style="display:flex;align-items:baseline;gap:10px;padding:2px 0;font-size:12px">
						<span style="flex:1;min-width:0;color:var(--text-secondary)">{f.does}</span>
						<span style="flex:none;font-family:var(--font-mono);font-size:var(--text-xs);color:var(--text-primary)">{f.keys}</span>
					</div>
				{/each}
			</section>
		</div>
		{#if more}
			<p style="margin:2px 0 0;font-size:12px;color:var(--text-muted)">{more}</p>
		{/if}
	</div>
	{#snippet footer()}
		<div style="display:flex;align-items:center;gap:8px;padding:12px 16px">
			<Btn variant="ghost" size="sm" icon="settings" onclick={customize}>Customize…</Btn>
			<div style="flex:1"></div>
			<Btn variant="primary" size="md" data-close data-rescue onclick={onClose}>Close</Btn>
		</div>
	{/snippet}
</Modal>
