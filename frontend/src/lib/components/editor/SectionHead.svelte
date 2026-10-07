<script lang="ts">
	// An overline label with a rule running out to the edge — the heading the
	// export dialog and the Deliver panel put over each group of controls.
	let { label }: { label: string } = $props();
</script>

<div style="display:flex;align-items:center;gap:8px;margin:16px 0 9px">
	<span
		style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
		>{label}</span
	>
	<div style="flex:1;height:1px;background:var(--border-subtle)"></div>
</div>
