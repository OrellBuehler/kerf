<script lang="ts">
	let { size = 18 }: { size?: number } = $props();
	const u = $derived(size / 42);
</script>

<span style="position:relative;width:{34 * u}px;height:{42 * u}px;display:inline-block">
	<i
		style="position:absolute;left:0;top:{4 *
			u}px;width:{13 * u}px;height:{34 * u}px;background:#eef1f5;border-radius:1px"
	></i>
	<i
		style="position:absolute;left:{17 * u}px;top:{8 *
			u}px;width:{13 * u}px;height:{34 * u}px;background:#aab3bf;border-radius:1px"
	></i>
	<i
		style="position:absolute;left:{14.5 * u}px;top:0;width:{2 *
			u}px;height:{42 * u}px;background:var(--kerf-500);border-radius:1px"
	></i>
</span>
